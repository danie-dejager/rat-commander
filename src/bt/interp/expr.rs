//! Expressions: C operators with 010 Editor's string semantics, names and
//! member paths resolved to places, and calls.

use super::stmt::Flow;
use super::{Interp, Place, R, Slot, Step, Stop, Target};
use crate::bt::ast::{BinOp, Expr, FuncId, Prim, Sym, TypeKind, UnOp};
use crate::bt::tree::{ArrayKind, NodeKind, NodeRef, ROOT};
use crate::bt::value::{IntTy, LocalArray, Value, bytes_to_wide};

impl Interp {
    pub(crate) fn eval(&mut self, e: &Expr) -> R<Value> {
        self.tick()?;
        match e {
            Expr::Int(v, u, l) => {
                let ty = if *l || *v > u32::MAX as u64 {
                    if *u { IntTy::U64 } else { IntTy::I64 }
                } else if *u || *v > i32::MAX as u64 {
                    IntTy::U32
                } else {
                    IntTy::I32
                };
                Ok(Value::Int(ty.norm(*v), ty))
            }
            Expr::Float(f, is32) => Ok(Value::Float(*f, *is32)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::WStr(s) => Ok(Value::WStr(bytes_to_wide(s))),
            Expr::Ident(..)
            | Expr::Member(..)
            | Expr::Index(..)
            | Expr::This
            | Expr::Parentof(_) => {
                let t = self.target(e)?;
                self.load(t)
            }
            Expr::Unary(op, x) => {
                let v = self.eval(x)?;
                self.unary(*op, v)
            }
            Expr::IncDec { pre, inc, e: inner } => {
                let t = self.target(inner)?;
                let Target::Place(p) = t.clone() else { return self.err("can't increment this") };
                let old = self.load(t)?;
                let op = if *inc { BinOp::Add } else { BinOp::Sub };
                let new = self.binop(op, old.clone(), Value::int(1))?;
                self.store(&p, new)?;
                if *pre { self.load(Target::Place(p)) } else { Ok(old) }
            }
            Expr::Binary(BinOp::And, a, b) => {
                let va = self.eval(a)?;
                if !self.truthy(&va)? {
                    return Ok(Value::bool(false));
                }
                let vb = self.eval(b)?;
                Ok(Value::bool(self.truthy(&vb)?))
            }
            Expr::Binary(BinOp::Or, a, b) => {
                let va = self.eval(a)?;
                if self.truthy(&va)? {
                    return Ok(Value::bool(true));
                }
                let vb = self.eval(b)?;
                Ok(Value::bool(self.truthy(&vb)?))
            }
            Expr::Binary(op, a, b) => {
                let va = self.eval(a)?;
                let vb = self.eval(b)?;
                self.binop(*op, va, vb)
            }
            Expr::Assign(op, lhs, rhs) => {
                let t = self.target(lhs)?;
                let mut v = self.eval(rhs)?;
                if let Some(op) = op {
                    let old = self.load(t.clone())?;
                    v = self.binop(*op, old, v)?;
                }
                match t {
                    Target::Place(p) => {
                        self.store(&p, v)?;
                        self.load(Target::Place(p))
                    }
                    _ => self.err("this can't be assigned to"),
                }
            }
            Expr::Cond(c, a, b) => {
                let vc = self.eval(c)?;
                if self.truthy(&vc)? { self.eval(a) } else { self.eval(b) }
            }
            Expr::Call(name, args, pos) => {
                let saved = self.cur_pos;
                if pos.line != 0 {
                    self.cur_pos = *pos;
                }
                let r = self.call(*name, args);
                self.cur_pos = saved;
                r
            }
            Expr::Cast(ty, x) => {
                let v = self.eval(x)?;
                self.convert(v, *ty)
            }
            Expr::SizeofType(ty) => Ok(Value::int64(self.sizeof_type(*ty)? as i64)),
            Expr::SizeofValue(x) => Ok(Value::int64(self.sizeof_expr(x)? as i64)),
            Expr::Startof(x) => {
                let t = self.target(x)?;
                let start = match t {
                    Target::Place(Place::Node(r)) | Target::Value(Value::Node(r)) => {
                        self.tree.node(r.id).start + r.shift
                    }
                    Target::Dup(ids) => self.tree.node(*ids.last().expect("dup")).start,
                    Target::Place(Place::NodeElem(r, i)) => {
                        let n = self.tree.node(r.id);
                        let es = match &n.kind {
                            NodeKind::Array { elem_size, .. } => *elem_size,
                            NodeKind::Str { wide: true } => 2,
                            _ => 1,
                        };
                        n.start + r.shift + i * es
                    }
                    _ => return self.err("startof needs a template variable"),
                };
                Ok(Value::int64(start as i64))
            }
            Expr::Exists(x) => match self.target(x) {
                Ok(Target::Place(Place::Var { frame, slot, path })) => Ok(Value::bool(
                    self.load(Target::Place(Place::Var { frame, slot, path })).is_ok(),
                )),
                Ok(Target::Place(Place::NodeElem(r, i))) => {
                    Ok(Value::bool(self.node_elem(r, i).is_ok()))
                }
                Ok(_) => Ok(Value::bool(true)),
                Err(Stop::Error(..)) => Ok(Value::bool(false)),
                Err(other) => Err(other),
            },
            Expr::FunctionExists(f) => Ok(Value::bool(self.function_exists(*f))),
            Expr::InitList(items) => {
                let mut out = Vec::with_capacity(items.len());
                for i in items {
                    out.push(self.eval(i)?);
                }
                let elem = self.prog.prim(Prim::Int);
                Ok(Value::Array(Box::new(LocalArray { elem, items: out })))
            }
            Expr::Comma(a, b) => {
                self.eval(a)?;
                self.eval(b)
            }
        }
    }

    pub(crate) fn function_exists(&self, f: Sym) -> bool {
        self.prog.func_names.get(&f).is_some_and(|&id| self.prog.funcs[id as usize].body.is_some())
            || self.builtin_ids.contains_key(&f)
    }

    /// What `e` names: a variable, a file variable, a duplicate array, or (for
    /// anything else) its value.
    pub(crate) fn target(&mut self, e: &Expr) -> R<Target> {
        match e {
            Expr::Ident(s, _) => self.lookup(*s),
            Expr::This => match self.this_stack.last() {
                Some(r) => Ok(Target::Place(Place::Node(*r))),
                None => self.err("'this' is only defined inside a struct"),
            },
            Expr::Parentof(x) => {
                let r = self.node_of(x)?;
                let mut id = self.tree.node(r.id).parent;
                let mut shift = r.shift;
                let mut child = r.id;
                // Arrays aren't structs: skip to the struct holding the array.
                while id != crate::bt::tree::NONE {
                    let n = self.tree.node(id);
                    match &n.kind {
                        NodeKind::Array { kind, elem_size, .. } => {
                            if *kind == ArrayKind::Optimized
                                && *elem_size > 0
                                && n.children.first() == Some(&child)
                            {
                                shift %= elem_size;
                            }
                            child = id;
                            id = n.parent;
                        }
                        _ => break,
                    }
                }
                if id == crate::bt::tree::NONE {
                    return self.err("the variable has no parent");
                }
                Ok(Target::Place(Place::Node(NodeRef { id, shift })))
            }
            Expr::Member(base, m) => {
                let t = self.target(base)?;
                self.member_of(t, *m)
            }
            Expr::Index(base, idx) => {
                let t = self.target(base)?;
                let iv = self.eval(idx)?;
                let i = self.int_of(&iv)?;
                self.index_of(t, i)
            }
            _ => Ok(Target::Value(self.eval(e)?)),
        }
    }

    /// The file variable `e` names.
    fn node_of(&mut self, e: &Expr) -> R<NodeRef> {
        match self.target(e)? {
            Target::Place(Place::Node(r)) | Target::Value(Value::Node(r)) => Ok(r),
            Target::Dup(ids) => Ok(NodeRef::new(*ids.last().expect("dup"))),
            Target::Place(Place::Var { frame, slot, path }) if path.is_empty() => {
                match &self.frame_at(frame).vars[slot].1 {
                    Slot::Val(Value::Node(r), _) => Ok(*r),
                    _ => self.err("not a template variable"),
                }
            }
            _ => self.err("not a template variable"),
        }
    }

    fn member_of(&mut self, t: Target, m: Sym) -> R<Target> {
        let r = match t {
            Target::Place(Place::Node(r)) | Target::Value(Value::Node(r)) => r,
            Target::Dup(ids) => NodeRef::new(*ids.last().expect("dup")),
            Target::Place(Place::Var { frame, slot, mut path }) => {
                // A local struct's field — or a local holding a node.
                let inner = match &self.frame_at(frame).vars[slot].1 {
                    Slot::Val(v, _) => super::walk(v, &path).cloned(),
                    _ => None,
                };
                match inner {
                    Some(Value::Node(r)) => r,
                    Some(Value::Record(rec)) => {
                        if rec.field(m).is_none() {
                            return self.err(format!("no member '{}'", self.prog.name(m)));
                        }
                        path.push(Step::Field(m));
                        return Ok(Target::Place(Place::Var { frame, slot, path }));
                    }
                    _ => return self.err(format!("'.{}' needs a struct", self.prog.name(m))),
                }
            }
            Target::Value(Value::Record(rec)) => {
                return match rec.field(m) {
                    Some(v) => Ok(Target::Value(v.clone())),
                    None => self.err(format!("no member '{}'", self.prog.name(m))),
                };
            }
            _ => return self.err(format!("'.{}' needs a struct", self.prog.name(m))),
        };
        if !matches!(self.tree.node(r.id).kind, NodeKind::Struct { .. }) {
            return self.err(format!("'.{}' needs a struct", self.prog.name(m)));
        }
        match self.member_target(r, m)? {
            Some(t) => Ok(t),
            None => {
                let owner = if r.id == ROOT {
                    "the file".to_string()
                } else {
                    format!("'{}'", self.prog.name(self.tree.node(r.id).name))
                };
                self.err(format!("{owner} has no member '{}'", self.prog.name(m)))
            }
        }
    }

    fn index_of(&mut self, t: Target, i: i64) -> R<Target> {
        if i < 0 {
            return self.err(format!("negative index {i}"));
        }
        let i = i as u64;
        match t {
            Target::Dup(ids) => match ids.get(i as usize) {
                Some(&id) => Ok(Target::Place(Place::Node(NodeRef::new(id)))),
                None => self.err(format!("index {i} out of bounds ({} declared)", ids.len())),
            },
            Target::Place(Place::Node(r)) | Target::Value(Value::Node(r)) => {
                let n = self.tree.node(r.id);
                match &n.kind {
                    NodeKind::Array { kind: ArrayKind::Scalar, .. } | NodeKind::Str { .. } => {
                        Ok(Target::Place(Place::NodeElem(r, i)))
                    }
                    NodeKind::Array { count, .. } => match self.tree.element(r, i) {
                        Some(e) => Ok(Target::Place(Place::Node(e))),
                        None => self.err(format!("index {i} out of bounds (array of {count})")),
                    },
                    NodeKind::Scalar { .. } | NodeKind::Struct { .. } => {
                        // `x[0]` on a single variable is the variable itself.
                        if i == 0 {
                            Ok(Target::Place(Place::Node(r)))
                        } else {
                            self.err("not an array")
                        }
                    }
                }
            }
            Target::Place(Place::Var { frame, slot, mut path }) => {
                let inner = match &self.frame_at(frame).vars[slot].1 {
                    Slot::Val(v, _) => super::walk(v, &path).cloned(),
                    _ => None,
                };
                match inner {
                    Some(Value::Node(r)) => self.index_of(Target::Place(Place::Node(r)), i as i64),
                    Some(Value::Str(s)) => {
                        // Characters of a local string: read-only view here;
                        // assignment goes through the path below.
                        if (i as usize) <= s.len() {
                            path.push(Step::Index(i as usize));
                            Ok(Target::Place(Place::Var { frame, slot, path }))
                        } else {
                            self.err(format!("index {i} out of bounds (string of {})", s.len()))
                        }
                    }
                    Some(Value::WStr(s)) => {
                        if (i as usize) <= s.len() {
                            path.push(Step::Index(i as usize));
                            Ok(Target::Place(Place::Var { frame, slot, path }))
                        } else {
                            self.err(format!("index {i} out of bounds (string of {})", s.len()))
                        }
                    }
                    Some(Value::Array(a)) => {
                        if (i as usize) <= a.items.len() {
                            path.push(Step::Index(i as usize));
                            Ok(Target::Place(Place::Var { frame, slot, path }))
                        } else {
                            self.err(format!(
                                "index {i} out of bounds (array of {})",
                                a.items.len()
                            ))
                        }
                    }
                    Some(Value::Int(..) | Value::Float(..)) if i == 0 => {
                        Ok(Target::Place(Place::Var { frame, slot, path }))
                    }
                    _ => self.err("not an array"),
                }
            }
            Target::Value(Value::Str(s)) => match s.get(i as usize) {
                Some(&c) => Ok(Target::Value(Value::Int(c as i8 as u64, IntTy::I8))),
                None if i as usize == s.len() => Ok(Target::Value(Value::Int(0, IntTy::I8))),
                None => self.err("index out of bounds"),
            },
            Target::Value(Value::WStr(s)) => match s.get(i as usize) {
                Some(&c) => Ok(Target::Value(Value::Int(c as u64, IntTy::new(2, false)))),
                None => self.err("index out of bounds"),
            },
            Target::Value(Value::Array(a)) => match a.items.get(i as usize) {
                Some(v) => Ok(Target::Value(v.clone())),
                None => self.err("index out of bounds"),
            },
            _ => self.err("not an array"),
        }
    }

    fn unary(&mut self, op: UnOp, v: Value) -> R<Value> {
        let v = match v {
            Value::Node(r) => self.node_value(r)?,
            v => v,
        };
        match (op, v) {
            (UnOp::Not, v) => Ok(Value::bool(!self.truthy(&v)?)),
            (UnOp::Neg, Value::Float(f, s)) => Ok(Value::Float(-f, s)),
            (UnOp::Plus, v @ Value::Float(..)) => Ok(v),
            (UnOp::Neg, Value::Int(b, t)) => {
                let t = IntTy::new(t.promoted().bytes, t.promoted().signed);
                Ok(Value::Int(t.norm(b.wrapping_neg()), t))
            }
            (UnOp::Plus, Value::Int(b, t)) => {
                let t = IntTy::new(t.promoted().bytes, t.promoted().signed);
                Ok(Value::Int(t.norm(b), t))
            }
            (UnOp::BitNot, Value::Int(b, t)) => {
                let t = IntTy::new(t.promoted().bytes, t.promoted().signed);
                Ok(Value::Int(t.norm(!b), t))
            }
            (UnOp::BitNot, Value::Float(f, _)) => Ok(Value::Int(!(f as i64) as u64, IntTy::I64)),
            _ => self.err("bad operand for a unary operator"),
        }
    }

    fn is_stringish(v: &Value) -> bool {
        matches!(v, Value::Str(_) | Value::WStr(_))
    }

    pub(crate) fn binop(&mut self, op: BinOp, a: Value, b: Value) -> R<Value> {
        let a = match a {
            Value::Node(r) => self.node_value(r)?,
            Value::Array(arr) if self.char_array(&arr) => {
                Value::Str(self.bytes_of(&Value::Array(arr))?)
            }
            v => v,
        };
        let b = match b {
            Value::Node(r) => self.node_value(r)?,
            Value::Array(arr) if self.char_array(&arr) => {
                Value::Str(self.bytes_of(&Value::Array(arr))?)
            }
            v => v,
        };
        if Self::is_stringish(&a) || Self::is_stringish(&b) {
            return self.string_op(op, a, b);
        }
        if let (Value::Node(_), _) | (_, Value::Node(_)) = (&a, &b) {
            return match op {
                BinOp::Eq | BinOp::Ne => {
                    let same = matches!((&a, &b), (Value::Node(x), Value::Node(y)) if x == y);
                    Ok(Value::bool(same == (op == BinOp::Eq)))
                }
                _ => self.err("a struct or array can't be used in arithmetic"),
            };
        }
        if let (Value::Float(..), _) | (_, Value::Float(..)) = (&a, &b) {
            let is32 = matches!((&a, &b), (Value::Float(_, true), Value::Float(_, true)))
                || matches!(
                    (&a, &b),
                    (Value::Float(_, true), Value::Int(..))
                        | (Value::Int(..), Value::Float(_, true))
                );
            let x = self.float_of(&a)?;
            let y = self.float_of(&b)?;
            return Ok(match op {
                BinOp::Add => Value::Float(x + y, is32),
                BinOp::Sub => Value::Float(x - y, is32),
                BinOp::Mul => Value::Float(x * y, is32),
                BinOp::Div => Value::Float(x / y, is32),
                BinOp::Rem => Value::Float(x % y, is32),
                BinOp::Eq => Value::bool(x == y),
                BinOp::Ne => Value::bool(x != y),
                BinOp::Lt => Value::bool(x < y),
                BinOp::Gt => Value::bool(x > y),
                BinOp::Le => Value::bool(x <= y),
                BinOp::Ge => Value::bool(x >= y),
                BinOp::And => Value::bool(x != 0.0 && y != 0.0),
                BinOp::Or => Value::bool(x != 0.0 || y != 0.0),
                _ => {
                    let ai = Value::Int(x as i64 as u64, IntTy::I64);
                    let bi = Value::Int(y as i64 as u64, IntTy::I64);
                    return self.binop(op, ai, bi);
                }
            });
        }
        let (Value::Int(xa, ta), Value::Int(yb, tb)) = (&a, &b) else {
            return self.err("bad operands for a binary operator");
        };
        let ty = IntTy::common(*ta, *tb);
        let (x, y) = (ty.norm(*xa), ty.norm(*yb));
        let cmp = |ord: std::cmp::Ordering| -> std::cmp::Ordering { ord };
        let order = if ty.signed { cmp((x as i64).cmp(&(y as i64))) } else { x.cmp(&y) };
        use std::cmp::Ordering::*;
        Ok(match op {
            BinOp::Add => Value::Int(ty.norm(x.wrapping_add(y)), ty),
            BinOp::Sub => Value::Int(ty.norm(x.wrapping_sub(y)), ty),
            BinOp::Mul => Value::Int(ty.norm(x.wrapping_mul(y)), ty),
            BinOp::Div | BinOp::Rem => {
                if y == 0 {
                    return self.err("division by zero");
                }
                let r = match (op, ty.signed) {
                    (BinOp::Div, true) => (x as i64).wrapping_div(y as i64) as u64,
                    (BinOp::Div, false) => x / y,
                    (_, true) => (x as i64).wrapping_rem(y as i64) as u64,
                    (_, false) => x % y,
                };
                Value::Int(ty.norm(r), ty)
            }
            BinOp::Shl | BinOp::Shr => {
                let lt = ta.promoted();
                let lt = IntTy::new(lt.bytes, lt.signed);
                let lx = lt.norm(*xa);
                let n = (tb.norm(*yb) & 63) as u32;
                let r = if op == BinOp::Shl {
                    lx.wrapping_shl(n)
                } else if lt.signed {
                    ((lx as i64) >> n) as u64
                } else {
                    lx >> n
                };
                Value::Int(lt.norm(r), lt)
            }
            BinOp::BitAnd => Value::Int(ty.norm(x & y), ty),
            BinOp::BitOr => Value::Int(ty.norm(x | y), ty),
            BinOp::BitXor => Value::Int(ty.norm(x ^ y), ty),
            BinOp::Eq => Value::bool(order == Equal),
            BinOp::Ne => Value::bool(order != Equal),
            BinOp::Lt => Value::bool(order == Less),
            BinOp::Gt => Value::bool(order == Greater),
            BinOp::Le => Value::bool(order != Greater),
            BinOp::Ge => Value::bool(order != Less),
            BinOp::And => Value::bool(x != 0 && y != 0),
            BinOp::Or => Value::bool(x != 0 || y != 0),
        })
    }

    fn char_array(&self, a: &LocalArray) -> bool {
        matches!(self.prog.prim_of(a.elem), Some(Prim::Char | Prim::UChar | Prim::WChar))
    }

    fn string_op(&mut self, op: BinOp, a: Value, b: Value) -> R<Value> {
        let wide = matches!(a, Value::WStr(_)) || matches!(b, Value::WStr(_));
        if wide {
            let to_w = |s: &mut Self, v: &Value| -> R<Vec<u16>> {
                Ok(match v {
                    Value::WStr(w) => w.clone(),
                    Value::Int(c, _) => vec![*c as u16],
                    other => bytes_to_wide(&s.bytes_of(other)?),
                })
            };
            let x = to_w(self, &a)?;
            let y = to_w(self, &b)?;
            return Ok(match op {
                BinOp::Add => Value::WStr([x, y].concat()),
                BinOp::Eq => Value::bool(x == y),
                BinOp::Ne => Value::bool(x != y),
                BinOp::Lt => Value::bool(x < y),
                BinOp::Gt => Value::bool(x > y),
                BinOp::Le => Value::bool(x <= y),
                BinOp::Ge => Value::bool(x >= y),
                _ => return self.err("bad operator for strings"),
            });
        }
        // A number next to a string: `s + 'x'` appends a character; comparing
        // a string with a number compares the number with the first char.
        let x = match &a {
            Value::Int(c, _) if op == BinOp::Add => vec![*c as u8],
            Value::Int(..) | Value::Float(..) => {
                let n = self.int_of(&a)?;
                let first = match &b {
                    Value::Str(s) => s.first().copied().unwrap_or(0) as i8 as i64,
                    _ => 0,
                };
                return self.binop(op, Value::int64(n), Value::int64(first));
            }
            other => self.bytes_of(other)?,
        };
        let y = match &b {
            Value::Int(c, _) if op == BinOp::Add => vec![*c as u8],
            Value::Int(..) | Value::Float(..) => {
                let n = self.int_of(&b)?;
                let first = x.first().copied().unwrap_or(0) as i8 as i64;
                return self.binop(op, Value::int64(first), Value::int64(n));
            }
            other => self.bytes_of(other)?,
        };
        Ok(match op {
            BinOp::Add => {
                let mut s = x;
                if s.len() + y.len() > self.limits.max_alloc {
                    return self.err("string too long");
                }
                s.extend_from_slice(&y);
                Value::Str(s)
            }
            BinOp::Eq => Value::bool(x == y),
            BinOp::Ne => Value::bool(x != y),
            BinOp::Lt => Value::bool(x < y),
            BinOp::Gt => Value::bool(x > y),
            BinOp::Le => Value::bool(x <= y),
            BinOp::Ge => Value::bool(x >= y),
            _ => return self.err("bad operator for strings"),
        })
    }

    fn call(&mut self, name: Sym, args: &[Expr]) -> R<Value> {
        if let Some(&fid) = self.prog.func_names.get(&name)
            && self.prog.funcs[fid as usize].body.is_some()
        {
            let slots = self.bind_args(fid, args)?;
            return self.call_user(fid, slots);
        }
        if let Some(&id) = self.builtin_ids.get(&name) {
            return (super::builtins::TABLE[id].1)(self, args);
        }
        if self.prog.func_names.contains_key(&name) {
            return self.err(format!(
                "function '{}' has no body (external functions aren't supported)",
                self.prog.name(name)
            ));
        }
        self.err(format!("unknown function '{}'", self.prog.name(name)))
    }

    /// Evaluate the arguments of a call to `fid` in the caller's scope.
    fn bind_args(&mut self, fid: FuncId, args: &[Expr]) -> R<Vec<Slot>> {
        let prog = self.prog.clone();
        let f = &prog.funcs[fid as usize];
        if args.len() < f.params.len() {
            return self.err(format!(
                "'{}' takes {} arguments, {} given",
                prog.name(f.name),
                f.params.len(),
                args.len()
            ));
        }
        let mut slots = Vec::with_capacity(f.params.len());
        for (p, a) in f.params.iter().zip(args) {
            let is_struct = matches!(prog.ty(prog.resolve(p.ty)).kind, TypeKind::Struct { .. });
            let slot = if p.by_ref || p.array || is_struct {
                match self.target(a)? {
                    Target::Place(Place::Node(r)) | Target::Value(Value::Node(r)) => Slot::Node(r),
                    Target::Dup(ids) => Slot::Node(NodeRef::new(*ids.last().expect("dup"))),
                    Target::Place(Place::Var { frame, slot, path }) if p.by_ref || p.array => {
                        // A local holding a node passes the node.
                        match &self.frame_at(frame).vars[slot].1 {
                            Slot::Val(Value::Node(r), _) if path.is_empty() => Slot::Node(*r),
                            _ => Slot::Ref(Place::Var { frame, slot, path }),
                        }
                    }
                    Target::Place(p2 @ Place::NodeElem(..)) if p.by_ref => Slot::Ref(p2),
                    t => {
                        let v = self.load(t)?;
                        let v = match v {
                            v @ (Value::Array(_) | Value::Record(_) | Value::Node(_)) => v,
                            v if p.array => v,
                            v => self.convert(v, p.ty)?,
                        };
                        Slot::Val(v, p.ty)
                    }
                }
            } else {
                let v = self.eval(a)?;
                let v = self.convert(v, p.ty)?;
                Slot::Val(v, p.ty)
            };
            slots.push(slot);
        }
        Ok(slots)
    }

    /// Run user function `fid` with its parameters already bound.
    pub(crate) fn call_user(&mut self, fid: FuncId, slots: Vec<Slot>) -> R<Value> {
        let prog = self.prog.clone();
        let f = &prog.funcs[fid as usize];
        let Some(body) = &f.body else {
            return self.err(format!("function '{}' has no body", prog.name(f.name)));
        };
        if self.depth >= self.limits.max_depth {
            return self.err("functions nested too deeply (runaway recursion?)");
        }
        let mut frame = super::Frame::new(super::FrameKind::Function);
        for (p, s) in f.params.iter().zip(slots) {
            frame.set(p.name, s);
        }
        self.frames.push(frame);
        self.depth += 1;
        let saved_pos = self.cur_pos;
        let mut result = Ok(Value::Void);
        for s in body {
            match self.exec(s) {
                Ok(Flow::Normal) => {}
                Ok(Flow::Return(v)) => {
                    result = Ok(v);
                    break;
                }
                Ok(Flow::Break | Flow::Continue) => break,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        self.depth -= 1;
        self.frames.pop();
        self.cur_pos = saved_pos;
        let v = result?;
        // Return by the declared type; a string function may return a char
        // array, a struct function a node.
        match &prog.ty(prog.resolve(f.ret)).kind {
            TypeKind::Prim(Prim::Void) => Ok(Value::Void),
            _ if matches!(v, Value::Void) => Ok(v),
            TypeKind::Prim(Prim::Str | Prim::WStr) | TypeKind::Prim(_) | TypeKind::Enum { .. } => {
                match v {
                    Value::Array(_) | Value::Node(_)
                        if matches!(prog.prim_of(f.ret), Some(Prim::Char | Prim::UChar)) =>
                    {
                        Ok(Value::Str(self.bytes_of(&v)?))
                    }
                    Value::Str(_) | Value::WStr(_)
                        if prog.prim_of(f.ret).is_some_and(|p| p.is_int()) =>
                    {
                        // `char[] f()` parses as returning `char`.
                        Ok(v)
                    }
                    v => self.convert(v, f.ret),
                }
            }
            _ => Ok(v),
        }
    }

    /// Call user function `fid` with a file variable as its first argument
    /// (attribute callbacks), and optionally a string as its second.
    pub(crate) fn call_with_node(
        &mut self,
        fid: FuncId,
        r: NodeRef,
        extra: Option<Value>,
    ) -> R<Value> {
        let prog = self.prog.clone();
        let f = &prog.funcs[fid as usize];
        let mut slots = Vec::new();
        if let Some(p) = f.params.first() {
            let is_node = p.by_ref
                || p.array
                || matches!(
                    prog.ty(prog.resolve(p.ty)).kind,
                    TypeKind::Struct { .. } | TypeKind::Alias { dim: Some(_), .. }
                );
            if is_node {
                slots.push(Slot::Node(r));
            } else {
                let v = self.node_value(r)?;
                let v = match v {
                    v @ Value::Node(_) => v,
                    v => self.convert(v, p.ty)?,
                };
                slots.push(Slot::Val(v, p.ty));
            }
        }
        if let (Some(p), Some(x)) = (f.params.get(1), extra) {
            let v = self.convert(x, p.ty)?;
            slots.push(Slot::Val(v, p.ty));
        }
        self.call_user(fid, slots)
    }

    /// A string as wide or narrow text, whichever `like` is.
    pub(crate) fn text_like(like: &Value, s: Vec<u8>) -> Value {
        match like {
            Value::WStr(_) => Value::WStr(bytes_to_wide(&s)),
            _ => Value::Str(s),
        }
    }

    pub(crate) fn wide_text(&mut self, v: &Value) -> R<Vec<u16>> {
        Ok(match v {
            Value::WStr(w) => w.clone(),
            Value::Node(r) => match self.node_value(*r)? {
                Value::WStr(w) => w,
                other => bytes_to_wide(&self.bytes_of(&other)?),
            },
            other => bytes_to_wide(&self.bytes_of(other)?),
        })
    }
}
