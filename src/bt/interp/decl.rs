//! Declarations: mapping variables onto the file, and locals.

use super::stmt::Flow;
use super::{Frame, FrameKind, Interp, R, Slot, Stop};
use crate::bt::ast::{Attr, Declarator, Expr, Prim, Stmt, Sym, TypeId, TypeKind, VarDecl};
use crate::bt::tree::{
    ArrayKind, BitLoc, F_BIG_ENDIAN, F_ENUM, F_HIDDEN, F_OPEN, F_READONLY, F_SUPPRESS, Format,
    Lazy, NO_COLOR, NONE, Node, NodeKind, NodeRef, Pending,
};
use crate::bt::value::{IntTy, LocalArray, Record, Value};

/// Arrays of variable-size structs up to this many elements are parsed in
/// full rather than assuming every element is the size of the first.
const FULL_ARRAY_LIMIT: u64 = 1024;

/// The styles `style=` can name, in the order of their ids (0 is none).
pub const STYLES: &[&str] = &[
    "sNone",
    "sHeading1",
    "sHeading1Accent",
    "sHeading2",
    "sHeading2Accent",
    "sHeading3",
    "sHeading3Accent",
    "sHeading4",
    "sHeading4Accent",
    "sSection1",
    "sSection1Accent",
    "sSection2",
    "sSection2Accent",
    "sSection3",
    "sSection3Accent",
    "sSection4",
    "sSection4Accent",
    "sMarker",
    "sMarkerAccent",
    "sData",
    "sDataAccent",
];

/// Whether an expression statement can move the file position (or declare
/// variables, through a user function).
fn moves_position(prog: &crate::bt::ast::Program, e: &Expr) -> bool {
    match e {
        Expr::Call(name, args, _) => {
            matches!(prog.name(*name), "FSeek" | "FSkip")
                || prog.func_names.contains_key(name)
                || args.iter().any(|a| moves_position(prog, a))
        }
        Expr::Unary(_, x) | Expr::Cast(_, x) | Expr::SizeofValue(x) => moves_position(prog, x),
        Expr::IncDec { e, .. } => moves_position(prog, e),
        Expr::Binary(_, a, b) | Expr::Assign(_, a, b) | Expr::Comma(a, b) | Expr::Index(a, b) => {
            moves_position(prog, a) || moves_position(prog, b)
        }
        Expr::Cond(c, a, b) => {
            moves_position(prog, c) || moves_position(prog, a) || moves_position(prog, b)
        }
        _ => false,
    }
}

impl Interp {
    pub(crate) fn exec_decl(&mut self, d: &VarDecl) -> R<()> {
        let local = d.local
            || self.in_callback > 0
            || matches!(self.frames.last().map(|f| f.kind), Some(FrameKind::LocalStruct))
            || self.cur_parent().is_none();
        for v in &d.vars {
            self.cur_pos = v.pos;
            // A variable with an initializer can't be mapped onto the file:
            // templates that forget `local` on one mean a local.
            if local || v.init.is_some() {
                self.declare_local(d.ty, v)?;
            } else {
                self.declare_file(d.ty, v)?;
            }
        }
        Ok(())
    }

    // ---- locals -------------------------------------------------------------

    fn declare_local(&mut self, ty: TypeId, v: &Declarator) -> R<()> {
        let Some(name) = v.name else { return Ok(()) };
        let prog = self.prog.clone();
        let rid = prog.resolve(ty);
        // An array: from the declarator, or a typedef'd array type.
        let (elem_ty, dim) = match (&v.dim, &prog.ty(rid).kind) {
            (Some(d), _) => (ty, Some(d.as_ref())),
            (None, TypeKind::Alias { target, dim: Some(d) }) => (*target, Some(d.as_deref())),
            _ => (ty, None),
        };
        let value = if let Some(dim) = dim {
            let count = match dim {
                Some(e) => {
                    let n = self.eval(e)?;
                    self.int_of(&n)?
                }
                None => 0,
            };
            if count < 0 || count as usize > self.limits.max_alloc {
                return self.err(format!("bad array size {count}"));
            }
            let zero = self.zero(elem_ty)?;
            let mut items = vec![zero; count as usize];
            match &v.init {
                Some(Expr::InitList(list)) => {
                    for (i, e) in list.iter().enumerate() {
                        let x = self.eval(e)?;
                        let x = self.convert(x, elem_ty)?;
                        if i < items.len() {
                            items[i] = x;
                        } else {
                            items.push(x);
                        }
                    }
                }
                Some(e) => {
                    let x = self.eval(e)?;
                    let arr = self.local_array_of(x, elem_ty)?;
                    if arr.items.len() > items.len() {
                        items = arr.items;
                    } else {
                        items[..arr.items.len()].clone_from_slice(&arr.items);
                        // A string shorter than the array ends at its NUL.
                        if let Some(next) = items.get_mut(arr.items.len())
                            && let Value::Int(b, _) = next
                        {
                            *b = 0;
                        }
                    }
                }
                None => {}
            }
            Value::Array(Box::new(LocalArray { elem: elem_ty, items }))
        } else if let TypeKind::Struct { .. } = &prog.ty(rid).kind {
            match &v.init {
                Some(e) => {
                    let x = self.eval(e)?;
                    match x {
                        x @ (Value::Record(_) | Value::Node(_)) => x,
                        _ => return self.err("a struct needs a struct value"),
                    }
                }
                None => self.local_struct(rid)?,
            }
        } else {
            match &v.init {
                Some(e) => {
                    let x = self.eval(e)?;
                    self.convert(x, ty)?
                }
                None => self.zero(ty)?,
            }
        };
        let frame = self.frames.last_mut().expect("a frame");
        frame.set(name, Slot::Val(value, elem_ty));
        Ok(())
    }

    /// A local struct: its body run with every declaration made local.
    fn local_struct(&mut self, rid: TypeId) -> R<Value> {
        let prog = self.prog.clone();
        let TypeKind::Struct { body: Some(body), .. } = &prog.ty(rid).kind else {
            return Ok(Value::Record(Box::default()));
        };
        if self.depth >= self.limits.max_depth {
            return self.err("structs nested too deeply");
        }
        self.depth += 1;
        self.frames.push(Frame::new(FrameKind::LocalStruct));
        let mut result = Ok(());
        for s in body {
            match self.exec(s) {
                Ok(Flow::Normal) => {}
                Ok(_) => break,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        let frame = self.frames.pop().expect("pushed");
        self.depth -= 1;
        result?;
        let fields = frame
            .vars
            .into_iter()
            .map(|(k, s)| match s {
                Slot::Val(v, _) => (k, v),
                Slot::Node(r) => (k, Value::Node(r)),
                Slot::Ref(_) => (k, Value::Void),
            })
            .collect();
        Ok(Value::Record(Box::new(Record { fields })))
    }

    /// The initial value of a local of type `ty`.
    pub(crate) fn zero(&mut self, ty: TypeId) -> R<Value> {
        let prog = self.prog.clone();
        let rid = prog.resolve(ty);
        Ok(match &prog.ty(rid).kind {
            TypeKind::Prim(Prim::Str) => Value::Str(Vec::new()),
            TypeKind::Prim(Prim::WStr) => Value::WStr(Vec::new()),
            TypeKind::Prim(Prim::Void) => Value::Void,
            TypeKind::Prim(p) if p.is_float() => Value::Float(0.0, *p == Prim::Float),
            TypeKind::Prim(p) => Value::Int(0, IntTy::of(*p)),
            TypeKind::Enum { base, .. } => {
                let mut it = IntTy::of(prog.prim_of(*base).unwrap_or(Prim::Int));
                it.enum_ty = rid;
                Value::Int(0, it)
            }
            TypeKind::Alias { target, dim: Some(d) } => {
                let n = match d {
                    Some(e) => {
                        let v = self.eval(e)?;
                        self.int_of(&v)?.max(0) as usize
                    }
                    None => 0,
                };
                if n > self.limits.max_alloc {
                    return self.err("array too large");
                }
                let z = self.zero(*target)?;
                Value::Array(Box::new(LocalArray { elem: *target, items: vec![z; n] }))
            }
            TypeKind::Struct { .. } => self.local_struct(rid)?,
            TypeKind::Alias { .. } => Value::Void,
        })
    }

    /// `x` as the items of a local array of `elem`.
    pub(crate) fn local_array_of(&mut self, x: Value, elem: TypeId) -> R<LocalArray> {
        Ok(match x {
            Value::Array(a) => {
                let mut items = Vec::with_capacity(a.items.len());
                for it in a.items {
                    items.push(self.convert(it, elem)?);
                }
                LocalArray { elem, items }
            }
            Value::Node(r) => {
                let n = self.tree.node(r.id);
                match n.kind.clone() {
                    NodeKind::Array { kind: ArrayKind::Scalar, count, .. } => {
                        let count = count.min(self.limits.max_alloc as u64);
                        let mut items = Vec::with_capacity(count as usize);
                        for i in 0..count {
                            let v = self.node_elem(r, i)?;
                            items.push(self.convert(v, elem)?);
                        }
                        LocalArray { elem, items }
                    }
                    _ => {
                        let v = self.node_value(r)?;
                        if matches!(v, Value::Node(_)) {
                            return self.err("can't copy a struct into an array");
                        }
                        return self.local_array_of(v, elem);
                    }
                }
            }
            Value::WStr(w) => LocalArray {
                elem,
                items: w.into_iter().map(|c| Value::Int(c as u64, IntTy::new(2, false))).collect(),
            },
            other => {
                let bytes = self.bytes_of(&other)?;
                let mut items = Vec::with_capacity(bytes.len());
                for b in bytes {
                    items.push(self.convert(Value::Int(b as i8 as u64, IntTy::I8), elem)?);
                }
                LocalArray { elem, items }
            }
        })
    }

    // ---- file variables -------------------------------------------------------

    /// The attribute lists that apply to a declaration of `ty`: its own, then
    /// each typedef's on the way to the underlying type.
    fn attr_lists(&self, own: u32, ty: TypeId) -> Vec<u32> {
        let mut lists = vec![own];
        let mut t = ty;
        for _ in 0..64 {
            let td = self.prog.ty(t);
            lists.push(td.attrs);
            match &td.kind {
                TypeKind::Alias { target, .. } => t = *target,
                _ => break,
            }
        }
        lists
    }

    /// The attribute lists of an array typedef itself: its own and the
    /// typedefs down to the one with the dimension.
    fn array_lists(&self, own: u32, ty: TypeId) -> Vec<u32> {
        let mut lists = vec![own];
        let mut t = ty;
        for _ in 0..64 {
            let td = self.prog.ty(t);
            lists.push(td.attrs);
            match &td.kind {
                TypeKind::Alias { dim: Some(_), .. } => break,
                TypeKind::Alias { target, .. } => t = *target,
                _ => break,
            }
        }
        lists
    }

    fn struct_array(&self, id: u32) -> bool {
        matches!(
            self.tree.node(id).kind,
            NodeKind::Array { kind: ArrayKind::Optimized | ArrayKind::Full, .. }
        )
    }

    fn find_attr(&self, lists: &[u32], name: &str) -> Option<(u32, u16)> {
        let sym = self.prog.syms.lookup(name)?;
        for &l in lists {
            if let Some(i) = self.prog.attrs(l).iter().position(|a| a.name == sym) {
                return Some((l, i as u16));
            }
        }
        None
    }

    fn attr(&self, at: (u32, u16)) -> &Attr {
        &self.prog.attr_lists[at.0 as usize][at.1 as usize]
    }

    /// A bare-word attribute value (`hex`, `true`, `sHeading1`).
    fn attr_word(&self, at: (u32, u16)) -> Option<String> {
        match &self.attr(at).value {
            Expr::Ident(s, _) => Some(self.prog.name(*s).to_string()),
            _ => None,
        }
    }

    /// Evaluate an attribute for node `r`: a function name is called with the
    /// node; anything else is evaluated with `this` bound to it.
    pub(crate) fn attr_value(
        &mut self,
        at: (u32, u16),
        r: NodeRef,
        extra: Option<Value>,
    ) -> R<Value> {
        let prog = self.prog.clone();
        let attr = &prog.attr_lists[at.0 as usize][at.1 as usize];
        if let Expr::Ident(s, _) = &attr.value
            && let Some(&fid) = prog.func_names.get(s)
            && prog.funcs[fid as usize].body.is_some()
        {
            return self.call_with_node(fid, r, extra);
        }
        // An inline expression sees the variable's struct (or the struct
        // holding it) as its scope, and `value` when writing.
        let scope = match self.tree.node(r.id).kind {
            NodeKind::Struct { .. } => r,
            _ => {
                let p = self.tree.node(r.id).parent;
                if p == NONE { r } else { NodeRef { id: p, shift: r.shift } }
            }
        };
        let mut frame = Frame::new(FrameKind::Struct(scope));
        if let Some(x) = extra {
            let value = self.prog.syms.lookup("value");
            if let Some(vs) = value {
                let sty = self.prog.prim(Prim::Str);
                frame.set(vs, Slot::Val(x, sty));
            }
        }
        self.frames.push(frame);
        self.this_stack.push(r);
        let v = self.eval(&attr.value);
        self.this_stack.pop();
        self.frames.pop();
        v
    }

    fn declare_file(&mut self, ty: TypeId, v: &Declarator) -> R<()> {
        let prog = self.prog.clone();
        let parent = self.cur_parent().expect("checked by exec_decl");
        if let Some(&(u, ustart, _)) = self.unions.last()
            && u == parent.id
        {
            self.pos = ustart;
        }
        let lists = self.attr_lists(v.attrs, ty);

        // `pos=` places the variable without moving on; `localpos=` places it
        // relative to its struct.
        let mut restore = None;
        if let Some(at) = self.find_attr(&lists, "pos") {
            let pv = self.attr_value(at, parent, None)?;
            restore = Some(self.pos);
            self.pos = self.int_of(&pv)?.max(0) as u64;
        } else if let Some(at) = self.find_attr(&lists, "localpos") {
            let pv = self.attr_value(at, parent, None)?;
            restore = Some(self.pos);
            self.pos = self.tree.node(parent.id).start + self.int_of(&pv)?.max(0) as u64;
        }

        let made = if let Some(bits) = &v.bits {
            self.declare_bitfield(v.name, ty, bits, parent.id)?.map(|id| (id, lists.clone()))
        } else {
            self.bits.unit = None;
            let Some(name) = v.name else { return self.err("a variable needs a name") };
            let rid = prog.resolve(ty);
            match (&v.dim, &prog.ty(rid).kind) {
                (Some(d), _) => self
                    .declare_array(name, ty, ty, d.as_ref(), parent.id, &lists, v.args.as_deref())?
                    // An array of structs takes only its own attributes; the
                    // element type's go to each element.
                    .map(|id| {
                        (id, if self.struct_array(id) { vec![v.attrs] } else { lists.clone() })
                    }),
                (None, TypeKind::Alias { target, dim: Some(d) }) => self
                    .declare_array(
                        name,
                        ty,
                        *target,
                        d.as_deref(),
                        parent.id,
                        &lists,
                        v.args.as_deref(),
                    )?
                    .map(|id| {
                        (
                            id,
                            if self.struct_array(id) {
                                self.array_lists(v.attrs, ty)
                            } else {
                                lists.clone()
                            },
                        )
                    }),
                _ => Some((
                    self.declare_one(name, ty, parent.id, &lists, v.args.as_deref(), NONE)?,
                    lists.clone(),
                )),
            }
        };
        if let Some((id, lists)) = made {
            self.apply_attrs(id, &lists, !self.struct_array(id))?;
        }
        if let Some(p) = restore {
            self.pos = p;
        }
        if let Some(u) = self.unions.last_mut()
            && u.0 == parent.id
        {
            u.2 = u.2.max(self.pos);
        }
        Ok(())
    }

    /// Attributes that take effect as soon as the variable exists. `lazy`:
    /// whether its `read=` / `write=` / `comment=` / `name=` are its own (an
    /// array of structs passes them to its elements instead).
    fn apply_attrs(&mut self, id: u32, lists: &[u32], lazy_attrs: bool) -> R<()> {
        let r = NodeRef::new(id);
        let mut lazy = Lazy::default();
        for (name, slot) in [("read", 0), ("write", 1), ("comment", 2), ("name", 3)] {
            if !lazy_attrs {
                break;
            }
            if let Some(at) = self.find_attr(lists, name) {
                match slot {
                    0 => lazy.read = Some(at),
                    1 => lazy.write = Some(at),
                    2 => lazy.comment = Some(at),
                    _ => lazy.name = Some(at),
                }
            }
        }
        let lazy_id = self.tree.intern_lazy(lazy);
        let mut flags = 0u16;
        if lazy.read.is_some() && lazy.write.is_none() {
            flags |= F_READONLY;
        }
        let mut format = None;
        if let Some(at) = self.find_attr(lists, "format") {
            format = match self.attr_word(at).as_deref() {
                Some("hex") => Some(Format::Hex),
                Some("binary") => Some(Format::Binary),
                Some("octal") => Some(Format::Octal),
                Some("decimalhex") => Some(Format::DecimalHex),
                Some("decimal") => Some(Format::Decimal),
                _ => None,
            };
        }
        let mut fg = None;
        let mut bg = None;
        for (name, out) in [("fgcolor", &mut fg), ("bgcolor", &mut bg)] {
            if let Some(at) = self.find_attr(lists, name) {
                match self.attr_value(at, r, None) {
                    Ok(v) => *out = Some(self.int_of(&v)? as u32),
                    Err(Stop::Error(msg, _)) => self.warn(&format!("{name}: {msg}")),
                    Err(e) => return Err(e),
                }
            }
        }
        let mut style = None;
        if let Some(at) = self.find_attr(lists, "style")
            && let Some(w) = self.attr_word(at)
        {
            style = Some(STYLES.iter().position(|s| *s == w).unwrap_or(0) as u8);
        }
        if let Some(at) = self.find_attr(lists, "hidden") {
            let hide = match self.attr_word(at).as_deref() {
                Some("true") => true,
                Some("false") => false,
                _ => match self.attr_value(at, r, None) {
                    Ok(v) => self.truthy(&v)?,
                    Err(Stop::Error(..)) => false,
                    Err(e) => return Err(e),
                },
            };
            if hide {
                flags |= F_HIDDEN;
            }
        }
        if let Some(at) = self.find_attr(lists, "open") {
            match self.attr_word(at).as_deref() {
                Some("true") => flags |= F_OPEN,
                Some("suppress") => flags |= F_SUPPRESS,
                _ => {}
            }
        }
        let n = self.tree.node_mut(id);
        n.lazy = lazy_id;
        n.flags |= flags;
        if let Some(f) = format {
            n.format = f;
        }
        if let Some(c) = fg {
            n.fg = c;
        }
        if let Some(c) = bg {
            n.bg = c;
        }
        if let Some(s) = style {
            n.style = s;
        }
        Ok(())
    }

    fn new_node(
        &mut self,
        name: Sym,
        ty: TypeId,
        kind: NodeKind,
        size: u64,
        parent: u32,
        index: u32,
    ) -> R<u32> {
        if self.tree.nodes.len() >= self.limits.max_nodes {
            return self.err(format!("too many variables (over {})", self.limits.max_nodes));
        }
        let is_struct = matches!(kind, NodeKind::Struct { .. });
        let node = Node {
            name,
            ty,
            parent,
            start: self.pos,
            size,
            kind,
            children: Vec::new(),
            members: is_struct.then(Box::default),
            flags: if self.big_endian { F_BIG_ENDIAN } else { 0 },
            format: self.format,
            fg: self.fg,
            bg: self.bg,
            style: self.style,
            enum_ty: NONE,
            lazy: NONE,
            dup: NONE,
            index,
        };
        let id = self.tree.push(node);
        let p = self.tree.node_mut(parent);
        p.children.push(id);
        if index == NONE
            && let Some(m) = p.members.as_mut()
            && let Some(dup) = m.add(name, id)
        {
            if dup == 1
                && let Some(crate::bt::tree::Member::Dup(ids)) = m.get(name)
            {
                let first = ids[0];
                self.tree.node_mut(first).dup = 0;
            }
            self.tree.node_mut(id).dup = dup;
        }
        Ok(id)
    }

    fn check_room(&self, size: u64) -> R<()> {
        let len = self.file_len();
        if self.pos > len || size > len - self.pos {
            return self.err(format!(
                "the variable at 0x{:X} ({size} bytes) runs past the end of the file",
                self.pos
            ));
        }
        Ok(())
    }

    /// Declare one (non-array) variable of type `ty`.
    fn declare_one(
        &mut self,
        name: Sym,
        ty: TypeId,
        parent: u32,
        lists: &[u32],
        args: Option<&[Expr]>,
        index: u32,
    ) -> R<u32> {
        let prog = self.prog.clone();
        let rid = prog.resolve(ty);
        match &prog.ty(rid).kind {
            TypeKind::Prim(Prim::Void) => self.err("a variable can't be void"),
            TypeKind::Prim(p @ (Prim::Str | Prim::WStr)) => {
                self.declare_string(name, ty, *p == Prim::WStr, parent, index)
            }
            TypeKind::Prim(p) => {
                let size = p.size();
                self.check_room(size)?;
                let id = self.new_node(
                    name,
                    ty,
                    NodeKind::Scalar { prim: *p, bits: None },
                    size,
                    parent,
                    index,
                )?;
                self.pos += size;
                Ok(id)
            }
            TypeKind::Enum { base, .. } => {
                let p = prog.prim_of(*base).unwrap_or(Prim::Int);
                let size = p.size();
                self.check_room(size)?;
                let id = self.new_node(
                    name,
                    ty,
                    NodeKind::Scalar { prim: p, bits: None },
                    size,
                    parent,
                    index,
                )?;
                let n = self.tree.node_mut(id);
                n.flags |= F_ENUM;
                n.enum_ty = rid;
                self.pos += size;
                Ok(id)
            }
            TypeKind::Struct { params, .. } => {
                let mut vals = Vec::new();
                if let Some(args) = args {
                    for (i, a) in args.iter().enumerate() {
                        let v = match params.get(i) {
                            Some(p) if p.by_ref || p.array => match self.target(a)? {
                                super::Target::Place(super::Place::Node(r)) => Value::Node(r),
                                super::Target::Dup(ids) => {
                                    Value::Node(NodeRef::new(*ids.last().expect("dup")))
                                }
                                t => self.load(t)?,
                            },
                            Some(p) => {
                                let v = self.eval(a)?;
                                self.convert(v, p.ty)?
                            }
                            None => self.eval(a)?,
                        };
                        vals.push(v);
                    }
                }
                if vals.len() < params.len() {
                    return self.err(format!(
                        "struct '{}' takes {} arguments, {} given",
                        prog.name(prog.ty(rid).name),
                        params.len(),
                        vals.len()
                    ));
                }
                let id =
                    self.new_node(name, ty, NodeKind::Struct { pending: None }, 0, parent, index)?;
                if let Some(at) = self.find_attr(lists, "size") {
                    let sv = self.attr_value(at, NodeRef::new(id), None)?;
                    let size = self.int_of(&sv)?.max(0) as u64;
                    let n = self.tree.node_mut(id);
                    n.size = size;
                    n.kind = NodeKind::Struct {
                        pending: Some(Box::new(Pending {
                            ty: rid,
                            args: vals,
                            big_endian: self.big_endian,
                        })),
                    };
                    self.pos = self.pos.saturating_add(size);
                    return Ok(id);
                }
                self.run_struct(id, rid, vals)?;
                Ok(id)
            }
            TypeKind::Alias { target, dim: Some(d) } => {
                // An array type used as one element of something.
                let (target, d) = (*target, d.as_deref());
                let made =
                    self.declare_array_node(name, ty, target, d, parent, lists, args, index)?;
                match made {
                    Some(id) => Ok(id),
                    None => {
                        // Zero elements: an empty node keeps the indexing of
                        // the enclosing array intact.
                        self.new_node(
                            name,
                            ty,
                            NodeKind::Struct { pending: None },
                            0,
                            parent,
                            index,
                        )
                    }
                }
            }
            TypeKind::Alias { .. } => self.err("bad type"),
        }
    }

    fn declare_string(
        &mut self,
        name: Sym,
        ty: TypeId,
        wide: bool,
        parent: u32,
        index: u32,
    ) -> R<u32> {
        let len = self.file_len();
        if self.pos >= len {
            return self
                .err(format!("the string at 0x{:X} starts past the end of the file", self.pos));
        }
        let unit = if wide { 2 } else { 1 };
        let mut size = 0u64;
        let mut at = self.pos;
        let cap = self.limits.max_alloc as u64;
        'scan: while at < len && size < cap {
            let chunk = self.read_bytes(at, 4096);
            if chunk.is_empty() {
                break;
            }
            if wide {
                for c in chunk.as_chunks::<2>().0 {
                    size += 2;
                    if *c == [0, 0] {
                        break 'scan;
                    }
                }
                if chunk.len() % 2 == 1 {
                    // An odd trailing byte: stop at the end of the file.
                    size += 1;
                    break;
                }
            } else if let Some(i) = chunk.iter().position(|&b| b == 0) {
                size += i as u64 + 1;
                break;
            } else {
                size += chunk.len() as u64;
            }
            at = self.pos + size;
        }
        let _ = unit;
        let id = self.new_node(name, ty, NodeKind::Str { wide }, size, parent, index)?;
        self.pos += size;
        Ok(id)
    }

    /// Run struct `rid`'s body for node `id`.
    pub(crate) fn run_struct(&mut self, id: u32, rid: TypeId, args: Vec<Value>) -> R<()> {
        let prog = self.prog.clone();
        let TypeKind::Struct { union, params, body } = &prog.ty(rid).kind else {
            return self.err("not a struct");
        };
        let Some(body) = body else {
            return self.err(format!(
                "struct '{}' is declared but never defined",
                prog.name(prog.ty(rid).name)
            ));
        };
        if self.depth >= self.limits.max_depth {
            return self.err("structs nested too deeply (runaway recursion?)");
        }
        let start = self.tree.node(id).start;
        let mut frame = Frame::new(FrameKind::Struct(NodeRef::new(id)));
        for (p, a) in params.iter().zip(args) {
            let slot = match a {
                Value::Node(r) if p.by_ref || p.array => Slot::Node(r),
                v => Slot::Val(v, p.ty),
            };
            frame.set(p.name, slot);
        }
        self.depth += 1;
        self.frames.push(frame);
        self.this_stack.push(NodeRef::new(id));
        if *union {
            self.unions.push((id, start, start));
        }
        self.bits.unit = None;
        let mut result = Ok(());
        for s in body {
            match self.exec(s) {
                Ok(Flow::Normal) => {}
                Ok(_) => break,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        self.bits.unit = None;
        if *union && let Some((_, ustart, uend)) = self.unions.pop() {
            self.pos = uend.max(ustart);
        }
        self.this_stack.pop();
        let frame = self.frames.pop().expect("pushed");
        if !frame.vars.is_empty() {
            self.node_locals.insert(id, frame);
        }
        self.depth -= 1;
        // An on-demand struct keeps its declared size.
        if !self.expanding.contains(&id) {
            let size = self.pos.saturating_sub(start);
            self.tree.node_mut(id).size = size;
        }
        result
    }

    // ---- arrays -----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn declare_array(
        &mut self,
        name: Sym,
        decl_ty: TypeId,
        elem_ty: TypeId,
        dim: Option<&Expr>,
        parent: u32,
        lists: &[u32],
        args: Option<&[Expr]>,
    ) -> R<Option<u32>> {
        self.declare_array_node(name, decl_ty, elem_ty, dim, parent, lists, args, NONE)
    }

    #[allow(clippy::too_many_arguments)]
    fn declare_array_node(
        &mut self,
        name: Sym,
        decl_ty: TypeId,
        elem_ty: TypeId,
        dim: Option<&Expr>,
        parent: u32,
        lists: &[u32],
        args: Option<&[Expr]>,
        index: u32,
    ) -> R<Option<u32>> {
        let prog = self.prog.clone();
        let erid = prog.resolve(elem_ty);
        let ekind = &prog.ty(erid).kind;
        // `char s[]` / `wchar_t s[]`: a NUL-terminated string.
        let Some(dim) = dim else {
            return match ekind {
                TypeKind::Prim(Prim::Char | Prim::UChar) => {
                    Ok(Some(self.declare_string(name, decl_ty, false, parent, index)?))
                }
                TypeKind::Prim(Prim::WChar | Prim::UShort) => {
                    Ok(Some(self.declare_string(name, decl_ty, true, parent, index)?))
                }
                _ => self.err("an array needs a size"),
            };
        };
        let cv = self.eval(dim)?;
        let count = self.int_of(&cv)?;
        if count < 0 {
            return self.err(format!("negative array size {count}"));
        }
        let count = count as u64;
        if count == 0 {
            return Ok(None);
        }
        match ekind {
            TypeKind::Prim(p) if !matches!(p, Prim::Str | Prim::WStr | Prim::Void) => {
                let esize = p.size();
                let Some(size) = esize.checked_mul(count) else {
                    return self.err("array too large");
                };
                self.check_room(size)?;
                let kind = NodeKind::Array {
                    elem_ty,
                    elem_prim: Some(*p),
                    count,
                    elem_size: esize,
                    kind: ArrayKind::Scalar,
                };
                let id = self.new_node(name, decl_ty, kind, size, parent, index)?;
                self.pos += size;
                Ok(Some(id))
            }
            TypeKind::Enum { base, .. } => {
                let p = prog.prim_of(*base).unwrap_or(Prim::Int);
                let esize = p.size();
                let Some(size) = esize.checked_mul(count) else {
                    return self.err("array too large");
                };
                self.check_room(size)?;
                let kind = NodeKind::Array {
                    elem_ty,
                    elem_prim: Some(p),
                    count,
                    elem_size: esize,
                    kind: ArrayKind::Scalar,
                };
                let id = self.new_node(name, decl_ty, kind, size, parent, index)?;
                let n = self.tree.node_mut(id);
                n.flags |= F_ENUM;
                n.enum_ty = erid;
                self.pos += size;
                Ok(Some(id))
            }
            _ => {
                let optimize =
                    match self.find_attr(lists, "optimize").and_then(|at| self.attr_word(at)) {
                        Some(w) => w == "true",
                        None => {
                            matches!(ekind, TypeKind::Struct { .. })
                                && (count > FULL_ARRAY_LIMIT || self.simple_size(erid).is_some())
                        }
                    };
                let optimize = optimize && matches!(ekind, TypeKind::Struct { .. });
                let start = self.pos;
                let kind = NodeKind::Array {
                    elem_ty,
                    elem_prim: None,
                    count,
                    elem_size: 0,
                    kind: if optimize { ArrayKind::Optimized } else { ArrayKind::Full },
                };
                let id = self.new_node(name, decl_ty, kind, 0, parent, index)?;
                // Each element takes the element type's attributes — and, for
                // `T x[n] <read=…>`, the declaration's: its callbacks are
                // written for one element.
                let elem_lists =
                    if elem_ty == decl_ty { lists.to_vec() } else { self.attr_lists(0, elem_ty) };
                let mut result = Ok(());
                if optimize {
                    match self.declare_one(name, elem_ty, id, &elem_lists, args, 0) {
                        Ok(e0) => {
                            let _ = self.apply_attrs(e0, &elem_lists, true);
                            let esize = self.pos.saturating_sub(start);
                            let total = esize.saturating_mul(count);
                            if count > 1 && esize > 0 {
                                self.pos = start;
                                if let Err(e) = self.check_room(total) {
                                    result = Err(e);
                                } else {
                                    self.pos = start + total;
                                }
                            }
                            if let NodeKind::Array { elem_size, .. } =
                                &mut self.tree.node_mut(id).kind
                            {
                                *elem_size = esize;
                            }
                        }
                        Err(e) => result = Err(e),
                    }
                } else {
                    for i in 0..count {
                        match self.declare_one(
                            name,
                            elem_ty,
                            id,
                            &elem_lists,
                            args,
                            i.min(u32::MAX as u64 - 1) as u32,
                        ) {
                            Ok(e) => {
                                if let Err(err) = self.apply_attrs(e, &elem_lists, true) {
                                    result = Err(err);
                                    break;
                                }
                            }
                            Err(e) => {
                                result = Err(e);
                                break;
                            }
                        }
                    }
                    let made = self.tree.node(id).children.len() as u64;
                    if let NodeKind::Array { count: c, .. } = &mut self.tree.node_mut(id).kind {
                        *c = made;
                    }
                }
                let size = self.pos.saturating_sub(start);
                self.tree.node_mut(id).size = size;
                result.map(|_| Some(id))
            }
        }
    }

    /// The size of a struct whose layout never varies (no control flow, fixed
    /// array sizes), or `None`. Cached per type.
    pub(crate) fn simple_size(&mut self, rid: TypeId) -> Option<u64> {
        if let Some(c) = self.simple_cache.get(&rid) {
            return *c;
        }
        self.simple_cache.insert(rid, None);
        let r = self.static_size(rid, 0).ok();
        self.simple_cache.insert(rid, r);
        r
    }

    /// `sizeof(type)`.
    pub(crate) fn sizeof_type(&mut self, ty: TypeId) -> R<u64> {
        self.static_size(ty, 0)
    }

    fn static_size(&mut self, ty: TypeId, depth: usize) -> R<u64> {
        if depth > 32 {
            return self.err("sizeof: type nested too deeply");
        }
        let prog = self.prog.clone();
        let rid = prog.resolve(ty);
        match &prog.ty(rid).kind {
            TypeKind::Prim(Prim::Str | Prim::WStr) => {
                self.err("sizeof: a string has no fixed size")
            }
            TypeKind::Prim(p) => Ok(p.size()),
            TypeKind::Enum { base, .. } => Ok(prog.prim_of(*base).unwrap_or(Prim::Int).size()),
            TypeKind::Alias { target, dim: Some(Some(e)) } => {
                let n = self.const_int(e)?;
                Ok(self.static_size(*target, depth + 1)?.saturating_mul(n.max(0) as u64))
            }
            TypeKind::Alias { .. } => self.err("sizeof: an open array has no fixed size"),
            TypeKind::Struct { union, params, body } => {
                if !params.is_empty() {
                    return self.err("sizeof: a struct with arguments has no fixed size");
                }
                let Some(body) = body else { return self.err("sizeof: struct is not defined") };
                let mut total = 0u64;
                let mut unit: Option<(u64, u32)> = None; // (bytes, bits used)
                for s in body {
                    match s {
                        Stmt::Decl(d) if d.local => {}
                        Stmt::Decl(d) => {
                            for v in &d.vars {
                                if v.args.is_some()
                                    || prog.attrs(v.attrs).iter().any(|a| {
                                        matches!(prog.name(a.name), "size" | "pos" | "localpos")
                                    })
                                {
                                    return self.err("sizeof: variable-size struct");
                                }
                                if let Some(b) = &v.bits {
                                    let width = self.const_int(b)?.max(0) as u32;
                                    let bytes = self.static_size(d.ty, depth + 1)?;
                                    let fits = unit.is_some_and(|(ub, used)| {
                                        ub == bytes && used + width <= (ub * 8) as u32
                                    });
                                    if !fits || width == 0 {
                                        if let Some((ub, _)) = unit.take() {
                                            total = if *union { total.max(ub) } else { total + ub };
                                        }
                                        if width > 0 {
                                            unit = Some((bytes, 0));
                                        }
                                    }
                                    if let Some(u) = &mut unit {
                                        u.1 += width;
                                    }
                                    continue;
                                }
                                if let Some((ub, _)) = unit.take() {
                                    total = if *union { total.max(ub) } else { total + ub };
                                }
                                let one = match &v.dim {
                                    Some(Some(e)) => {
                                        let n = self.const_int(e)?.max(0) as u64;
                                        self.static_size(d.ty, depth + 1)?.saturating_mul(n)
                                    }
                                    Some(None) => return self.err("sizeof: variable-size struct"),
                                    None => self.static_size(d.ty, depth + 1)?,
                                };
                                total = if *union { total.max(one) } else { total + one };
                            }
                        }
                        // Expressions — BigEndian(), SetBackColor(), counters —
                        // don't change the layout; moving the position does.
                        Stmt::Expr(_, e) if !moves_position(&prog, e) => {}
                        Stmt::Empty => {}
                        _ => return self.err("sizeof: variable-size struct"),
                    }
                }
                if let Some((ub, _)) = unit {
                    total = if *union { total.max(ub) } else { total + ub };
                }
                Ok(total)
            }
        }
    }

    /// An expression that must be a constant integer (for `sizeof`): evaluated
    /// where only globals are visible.
    fn const_int(&mut self, e: &Expr) -> R<i64> {
        self.frames.push(Frame::new(FrameKind::Function));
        let v = self.eval(e);
        self.frames.pop();
        match v.and_then(|v| self.int_of(&v)) {
            Ok(n) => Ok(n),
            Err(Stop::Error(..)) => self.err("sizeof: variable-size struct"),
            Err(e) => Err(e),
        }
    }

    /// `sizeof(expr)`.
    pub(crate) fn sizeof_expr(&mut self, e: &Expr) -> R<u64> {
        let t = self.target(e)?;
        let v = match t {
            super::Target::Place(super::Place::Node(r)) | super::Target::Value(Value::Node(r)) => {
                return Ok(self.tree.node(r.id).size);
            }
            super::Target::Dup(ids) => return Ok(self.tree.node(*ids.last().expect("dup")).size),
            super::Target::Place(super::Place::NodeElem(r, _)) => {
                return Ok(match &self.tree.node(r.id).kind {
                    NodeKind::Array { elem_size, .. } => *elem_size,
                    NodeKind::Str { wide: true } => 2,
                    _ => 1,
                });
            }
            t => self.load(t)?,
        };
        Ok(self.value_size(&v))
    }

    fn value_size(&self, v: &Value) -> u64 {
        match v {
            Value::Int(_, t) => t.bytes as u64,
            Value::Float(_, f32) => {
                if *f32 {
                    4
                } else {
                    8
                }
            }
            Value::Str(s) => s.len() as u64,
            Value::WStr(w) => w.len() as u64 * 2,
            Value::Array(a) => a.items.iter().map(|x| self.value_size(x)).sum(),
            Value::Record(r) => r.fields.iter().map(|(_, x)| self.value_size(x)).sum(),
            Value::Node(r) => self.tree.node(r.id).size,
            Value::Void => 0,
        }
    }

    // ---- bitfields ----------------------------------------------------------

    fn declare_bitfield(
        &mut self,
        name: Option<Sym>,
        ty: TypeId,
        bits: &Expr,
        parent: u32,
    ) -> R<Option<u32>> {
        let prog = self.prog.clone();
        let rid = prog.resolve(ty);
        let is_enum = matches!(prog.ty(rid).kind, TypeKind::Enum { .. });
        let Some(prim) = prog.prim_of(rid).filter(|p| p.is_int()) else {
            return self.err("a bitfield needs an integer type");
        };
        let wv = self.eval(bits)?;
        let width = self.int_of(&wv)?;
        let unit_bytes = prim.size() as u8;
        let unit_bits = unit_bytes as u32 * 8;
        if width < 0 || width as u32 > unit_bits {
            return self.err(format!("bad bitfield width {width}"));
        }
        let width = width as u32;
        let (start, size, loc) = if !self.bits.padding_off {
            if width == 0 {
                self.bits.unit = None;
                return Ok(None);
            }
            let big = self.big_endian;
            let ltr = self.bits.ltr.unwrap_or(big);
            let fits = self.bits.unit.is_some_and(|(ustart, ub, used, ubig)| {
                ub == unit_bytes
                    && used + width <= unit_bits
                    && ubig == big
                    && self.pos == ustart + ub as u64
            });
            if !fits {
                self.check_room(unit_bytes as u64)?;
                self.bits.unit = Some((self.pos, unit_bytes, 0, big));
                self.pos += unit_bytes as u64;
            }
            let (ustart, ub, used, _) = self.bits.unit.expect("set above");
            let shift = if ltr { unit_bits - used - width } else { used };
            if let Some(u) = &mut self.bits.unit {
                u.2 += width;
            }
            (ustart, ub as u64, BitLoc { shift: shift as u8, width: width as u8, le: !big })
        } else {
            let ltr = self.bits.ltr.unwrap_or(self.big_endian);
            let bitpos = match self.bits.stream {
                Some(b) if b.div_ceil(8) == self.pos => b,
                _ => self.pos * 8,
            };
            let start = bitpos / 8;
            let off = (bitpos % 8) as u32;
            let size = (off + width).div_ceil(8) as u64;
            let len = self.file_len();
            if start + size > len {
                return self
                    .err(format!("the bitfield at 0x{start:X} runs past the end of the file"));
            }
            let shift = if ltr { size as u32 * 8 - off - width } else { off };
            self.bits.stream = Some(bitpos + width as u64);
            self.pos = (bitpos + width as u64).div_ceil(8);
            (start, size, BitLoc { shift: shift as u8, width: width as u8, le: !ltr })
        };
        let Some(name) = name else { return Ok(None) };
        let saved = self.pos;
        self.pos = start;
        let id =
            self.new_node(name, ty, NodeKind::Scalar { prim, bits: Some(loc) }, size, parent, NONE);
        self.pos = saved;
        let id = id?;
        let n = self.tree.node_mut(id);
        if !loc.le {
            n.flags |= F_BIG_ENDIAN;
        } else {
            n.flags &= !F_BIG_ENDIAN;
        }
        if is_enum {
            n.flags |= F_ENUM;
            n.enum_ty = rid;
        }
        Ok(Some(id))
    }

    // ---- on-demand structs --------------------------------------------------

    /// Run the deferred body of on-demand struct `id`, if it has one.
    pub(crate) fn expand(&mut self, id: u32) -> R<()> {
        let pending = match &mut self.tree.node_mut(id).kind {
            NodeKind::Struct { pending, .. } => pending.take(),
            _ => None,
        };
        let Some(pending) = pending else { return Ok(()) };
        let start = self.tree.node(id).start;
        let saved =
            (self.pos, self.big_endian, self.fg, self.bg, self.style, self.format, self.bits);
        let saved_frames = self.frames.split_off(1);
        let saved_this = std::mem::take(&mut self.this_stack);
        let saved_unions = std::mem::take(&mut self.unions);
        // The struct's ancestors are in scope, as they were when it was declared.
        let mut chain = Vec::new();
        let mut p = self.tree.node(id).parent;
        while p != NONE && p != crate::bt::tree::ROOT {
            if matches!(self.tree.node(p).kind, NodeKind::Struct { .. }) {
                chain.push(p);
            }
            p = self.tree.node(p).parent;
        }
        for &a in chain.iter().rev() {
            self.frames.push(Frame::new(FrameKind::Struct(NodeRef::new(a))));
        }
        self.pos = start;
        self.big_endian = pending.big_endian;
        self.fg = NO_COLOR;
        self.bg = NO_COLOR;
        self.style = 0;
        self.bits = super::Bits::default();
        let depth = self.depth;
        self.expanding.push(id);
        let r = self.run_struct(id, pending.ty, pending.args);
        self.expanding.pop();
        self.depth = depth;
        self.frames.truncate(1);
        self.frames.extend(saved_frames);
        self.this_stack = saved_this;
        self.unions = saved_unions;
        (self.pos, self.big_endian, self.fg, self.bg, self.style, self.format, self.bits) = saved;
        r
    }
}
