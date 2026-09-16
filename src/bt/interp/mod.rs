//! Running a template.
//!
//! Execution follows 010 Editor: statements run top to bottom, and declaring a
//! variable maps it onto the file at the current position and moves the
//! position on. Names resolve through the frames being executed — a struct
//! body sees its own members and those of the structs around it, a function
//! sees its locals and the globals — and a file variable's name is registered
//! before its body runs, so a struct can read its own earlier fields by path
//! while it is still being built.
//!
//! After a run the interpreter is kept: the template panel asks it for the
//! values of `read=` / `comment=` callbacks and the bodies of on-demand
//! structs when they are shown.

mod builtins;
mod decl;
pub mod display;
pub mod edit;
mod expr;
mod printf;
mod stmt;
pub(crate) mod time;

pub(crate) use builtins::guid_text;

use super::ast::{Prim, Program, Sym, TypeId, TypeKind};
use super::lex::Pos;
use super::source::ByteSource;
use super::tree::{
    ArrayKind, BitLoc, F_BIG_ENDIAN, F_ENUM, Format, Member, NO_COLOR, NodeKind, NodeRef, ROOT,
    Tree,
};
use super::value::{IntTy, LocalArray, Value, bytes_to_wide, until_nul, wide_to_bytes};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Why execution stopped early.
#[derive(Debug, Clone)]
pub enum Stop {
    Error(String, Pos),
    /// `Exit(code)`, or a top-level `return`.
    Exit,
    Cancelled,
}

pub type R<T> = Result<T, Stop>;

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_steps: u64,
    pub deadline: Option<Instant>,
    /// The longest string or local array a template may build.
    pub max_alloc: usize,
    pub max_output_lines: usize,
}

impl Limits {
    /// For a full run on a background thread.
    pub fn run() -> Limits {
        Limits {
            max_nodes: 2_000_000,
            max_depth: 500,
            max_steps: u64::MAX,
            deadline: Some(Instant::now() + std::time::Duration::from_secs(120)),
            max_alloc: 64 << 20,
            max_output_lines: 10_000,
        }
    }

    /// For a callback evaluated while drawing.
    pub fn display() -> Limits {
        Limits {
            max_nodes: 200_000,
            max_depth: 48,
            max_steps: 200_000,
            deadline: None,
            max_alloc: 16 << 20,
            max_output_lines: 10_000,
        }
    }
}

/// How the file variables' bitfield packing currently stands.
#[derive(Debug, Clone, Copy, Default)]
struct Bits {
    padding_off: bool,
    /// Forced direction: `Some(true)` left-to-right.
    ltr: Option<bool>,
    /// The padded unit being filled: start, bytes, bits used, endianness.
    unit: Option<(u64, u8, u32, bool)>,
    /// The unpadded bit cursor, valid while `pos` is where it left off.
    stream: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum Slot {
    /// A local of a declared type (assignments convert to it).
    Val(Value, TypeId),
    /// A by-reference parameter.
    Ref(Place),
    /// A file variable bound by name in a function.
    Node(NodeRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Global,
    Function,
    Struct(NodeRef),
    /// Building a local struct: declarations inside are locals.
    LocalStruct,
}

#[derive(Debug, Clone)]
struct Frame {
    kind: FrameKind,
    vars: Vec<(Sym, Slot)>,
    index: Option<HashMap<Sym, usize>>,
}

impl Frame {
    fn new(kind: FrameKind) -> Frame {
        let index = (kind == FrameKind::Global).then(HashMap::new);
        Frame { kind, vars: Vec::new(), index }
    }

    fn find(&self, s: Sym) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(&s).copied(),
            None => self.vars.iter().rposition(|(k, _)| *k == s),
        }
    }

    /// Declare `s` (a redeclaration replaces the old one, as 010 Editor's
    /// function-wide scoping allows).
    fn set(&mut self, s: Sym, slot: Slot) -> usize {
        if let Some(i) = self.find(s) {
            self.vars[i].1 = slot;
            return i;
        }
        self.vars.push((s, slot));
        let i = self.vars.len() - 1;
        if let Some(ix) = &mut self.index {
            ix.insert(s, i);
        }
        i
    }
}

/// Where a variable's frame is: on the frame stack, or the saved locals of a
/// struct node — a struct's locals outlive its body, reachable by path
/// (`header.count`) like its members.
#[derive(Debug, Clone, Copy)]
pub enum FrameRef {
    Stack(usize),
    Node(u32),
}

/// One step into a local value.
#[derive(Debug, Clone)]
pub enum Step {
    Index(usize),
    Field(Sym),
}

/// Something that can be read and assigned.
#[derive(Debug, Clone)]
pub enum Place {
    Var {
        frame: FrameRef,
        slot: usize,
        path: Vec<Step>,
    },
    Node(NodeRef),
    /// Element `i` of a scalar array, or character `i` of a string variable.
    NodeElem(NodeRef, u64),
}

/// What an expression names.
#[derive(Debug, Clone)]
pub enum Target {
    Place(Place),
    /// A duplicate array: the ids of its elements.
    Dup(Vec<u32>),
    Value(Value),
}

pub struct Interp {
    pub prog: Arc<Program>,
    pub tree: Tree,
    pub src: Box<dyn ByteSource>,
    pub file_name: String,
    pub output: Vec<String>,
    partial: String,
    frames: Vec<Frame>,
    this_stack: Vec<NodeRef>,
    /// Unions being built: node, start, furthest end.
    unions: Vec<(u32, u64, u64)>,
    pub pos: u64,
    big_endian: bool,
    fg: u32,
    bg: u32,
    style: u8,
    format: Format,
    bits: Bits,
    /// Enum constants' values by symbol, and each enum's constants.
    enum_values: HashMap<Sym, (TypeId, i64)>,
    enum_lists: HashMap<TypeId, Vec<(Sym, i64)>>,
    steps: u64,
    depth: usize,
    pub limits: Limits,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Arc<AtomicU64>>,
    pub cur_pos: Pos,
    /// In write mode (running a `write=` callback), the writes made.
    pub writes: Option<Vec<(u64, Vec<u8>)>>,
    warned_write: bool,
    /// The last `FindFirst` search, for `FindNext`.
    find: Option<(Vec<u8>, bool, u64)>,
    builtin_ids: HashMap<Sym, usize>,
    /// On-demand structs whose bodies are running (they keep their size).
    expanding: Vec<u32>,
    /// Which struct types have a fixed layout, and its size.
    simple_cache: HashMap<TypeId, Option<u64>>,
    /// The locals of struct bodies that have finished.
    node_locals: HashMap<u32, Frame>,
    /// Inside a display-time callback, where declarations are always local.
    in_callback: usize,
    rng: u64,
}

impl Interp {
    pub fn new(
        prog: Arc<Program>,
        src: Box<dyn ByteSource>,
        file_name: &str,
        limits: Limits,
    ) -> Interp {
        let len = src.len();
        let root_ty = prog.prim(Prim::Void);
        let builtin_ids = builtins::TABLE
            .iter()
            .enumerate()
            .filter_map(|(i, (name, _))| prog.syms.lookup(name).map(|s| (s, i)))
            .collect();
        Interp {
            tree: Tree::new(len, root_ty),
            prog,
            src,
            file_name: file_name.to_string(),
            output: Vec::new(),
            partial: String::new(),
            frames: vec![Frame::new(FrameKind::Global)],
            this_stack: Vec::new(),
            unions: Vec::new(),
            pos: 0,
            big_endian: false,
            fg: NO_COLOR,
            bg: NO_COLOR,
            style: 0,
            format: Format::Decimal,
            bits: Bits::default(),
            enum_values: HashMap::new(),
            enum_lists: HashMap::new(),
            steps: 0,
            depth: 0,
            limits,
            cancel: None,
            progress: None,
            cur_pos: Pos::default(),
            writes: None,
            warned_write: false,
            find: None,
            builtin_ids,
            expanding: Vec::new(),
            simple_cache: HashMap::new(),
            node_locals: HashMap::new(),
            in_callback: 0,
            rng: 0x2545_f491_4f6c_dd1d,
        }
    }

    pub fn set_cancel(&mut self, cancel: Arc<AtomicBool>, progress: Arc<AtomicU64>) {
        self.cancel = Some(cancel);
        self.progress = Some(progress);
    }

    /// Run the template. Returns the error that stopped it, if any; the tree
    /// built up to that point stays.
    pub fn run(&mut self) -> Option<(String, Pos)> {
        let prog = self.prog.clone();
        for w in &prog.warnings {
            self.print(&format!("Warning: {w}\n"));
        }
        let result = self.eval_enums().and_then(|_| {
            for s in &prog.body {
                match self.exec(s)? {
                    stmt::Flow::Normal => {}
                    stmt::Flow::Return(v) => {
                        if !matches!(v, Value::Void) {
                            let text = self.display_value(&v);
                            self.print(&format!("Template returned: {text}\n"));
                        }
                        break;
                    }
                    stmt::Flow::Break | stmt::Flow::Continue => break,
                }
            }
            Ok(())
        });
        self.flush_output();
        self.finish_tree();
        match result {
            Ok(()) | Err(Stop::Exit) => None,
            Err(Stop::Cancelled) => Some(("cancelled".into(), self.cur_pos)),
            Err(Stop::Error(msg, pos)) => Some((msg, pos)),
        }
    }

    /// The root covers the whole file; unions and structs are sized already.
    fn finish_tree(&mut self) {
        let len = self.src.len();
        self.tree.node_mut(ROOT).size = len;
    }

    fn eval_enums(&mut self) -> R<()> {
        let prog = self.prog.clone();
        for &id in &prog.enums {
            let TypeKind::Enum { consts, .. } = &prog.ty(id).kind else { continue };
            let mut next: i64 = 0;
            let mut list = Vec::with_capacity(consts.len());
            for (sym, e) in consts {
                let v = match e {
                    Some(e) => {
                        self.cur_pos = prog.ty(id).pos;
                        let v = self.eval(e)?;
                        self.int_of(&v)?
                    }
                    None => next,
                };
                list.push((*sym, v));
                self.enum_values.insert(*sym, (id, v));
                next = v.wrapping_add(1);
            }
            self.enum_lists.insert(id, list);
        }
        Ok(())
    }

    // ---- errors, limits, output -------------------------------------------

    pub(crate) fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(Stop::Error(msg.into(), self.cur_pos))
    }

    fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.limits.max_steps {
            return self.err("the template ran too long");
        }
        if self.steps & 0xfff == 0 {
            if self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(Stop::Cancelled);
            }
            if let Some(p) = &self.progress {
                p.store(self.pos, Ordering::Relaxed);
            }
            if self.limits.deadline.is_some_and(|d| Instant::now() > d) {
                return self.err("the template ran too long");
            }
        }
        Ok(())
    }

    pub(crate) fn print(&mut self, text: &str) {
        self.partial.push_str(text);
        while let Some(i) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=i).collect();
            if self.output.len() < self.limits.max_output_lines {
                self.output.push(line.trim_end_matches(['\n', '\r']).to_string());
            }
        }
    }

    fn flush_output(&mut self) {
        if !self.partial.is_empty() {
            let rest = std::mem::take(&mut self.partial);
            self.output.push(rest);
        }
    }

    pub(crate) fn warn(&mut self, msg: &str) {
        let at = self.prog.pos_text(self.cur_pos);
        self.print(&format!("Warning ({at}): {msg}\n"));
    }

    // ---- name lookup ------------------------------------------------------

    /// The node being built that new file variables go into.
    fn cur_parent(&self) -> Option<NodeRef> {
        for f in self.frames.iter().rev() {
            match f.kind {
                FrameKind::Struct(r) => return Some(r),
                FrameKind::LocalStruct => return None,
                FrameKind::Global => return Some(NodeRef::new(ROOT)),
                FrameKind::Function => {}
            }
        }
        Some(NodeRef::new(ROOT))
    }

    fn frame_at(&self, f: FrameRef) -> &Frame {
        match f {
            FrameRef::Stack(i) => &self.frames[i],
            FrameRef::Node(id) => &self.node_locals[&id],
        }
    }

    fn frame_at_mut(&mut self, f: FrameRef) -> &mut Frame {
        match f {
            FrameRef::Stack(i) => &mut self.frames[i],
            FrameRef::Node(id) => self.node_locals.get_mut(&id).expect("node locals exist"),
        }
    }

    /// A local of struct `id`'s body, while it runs or after.
    fn struct_local(&self, id: u32, s: Sym) -> Option<Target> {
        let (frame, slot) = match self
            .frames
            .iter()
            .rposition(|f| matches!(f.kind, FrameKind::Struct(r) if r.id == id))
        {
            Some(i) if self.frames[i].find(s).is_some() => {
                (FrameRef::Stack(i), self.frames[i].find(s)?)
            }
            _ => (FrameRef::Node(id), self.node_locals.get(&id)?.find(s)?),
        };
        Some(match &self.frame_at(frame).vars[slot].1 {
            Slot::Ref(p) => Target::Place(p.clone()),
            Slot::Node(r) => Target::Place(Place::Node(*r)),
            Slot::Val(..) => Target::Place(Place::Var { frame, slot, path: Vec::new() }),
        })
    }

    fn member_target(&mut self, parent: NodeRef, s: Sym) -> R<Option<Target>> {
        self.expand(parent.id)?;
        if let Some(t) = self.struct_local(parent.id, s) {
            return Ok(Some(t));
        }
        Ok(match self.tree.member(parent.id, s) {
            Some(Member::Single(id)) => {
                Some(Target::Place(Place::Node(NodeRef { id: *id, shift: parent.shift })))
            }
            Some(Member::Dup(ids)) => {
                if parent.shift == 0 {
                    Some(Target::Dup(ids.clone()))
                } else {
                    // Duplicates inside an optimized element: shift each.
                    let last = *ids.last().expect("dup has elements");
                    Some(Target::Place(Place::Node(NodeRef { id: last, shift: parent.shift })))
                }
            }
            None => None,
        })
    }

    fn lookup(&mut self, s: Sym) -> R<Target> {
        let mut i = self.frames.len() - 1;
        loop {
            if let Some(slot) = self.frames[i].find(s) {
                return Ok(match &self.frames[i].vars[slot].1 {
                    Slot::Ref(p) => Target::Place(p.clone()),
                    Slot::Node(r) => Target::Place(Place::Node(*r)),
                    Slot::Val(..) => Target::Place(Place::Var {
                        frame: FrameRef::Stack(i),
                        slot,
                        path: Vec::new(),
                    }),
                });
            }
            match self.frames[i].kind {
                FrameKind::Struct(r) => {
                    if let Some(t) = self.member_target(r, s)? {
                        return Ok(t);
                    }
                }
                FrameKind::Function => {
                    i = 0;
                    continue;
                }
                FrameKind::Global => {
                    if let Some(t) = self.member_target(NodeRef::new(ROOT), s)? {
                        return Ok(t);
                    }
                    break;
                }
                FrameKind::LocalStruct => {}
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        // A function reaching into the struct it was called from builds.
        let structs: Vec<NodeRef> = self
            .frames
            .iter()
            .rev()
            .filter_map(|f| if let FrameKind::Struct(r) = f.kind { Some(r) } else { None })
            .collect();
        for r in structs {
            if let Some(t) = self.member_target(r, s)? {
                return Ok(t);
            }
        }
        if let Some(&(ty, v)) = self.enum_values.get(&s) {
            let base = match &self.prog.ty(ty).kind {
                TypeKind::Enum { base, .. } => self.prog.prim_of(*base).unwrap_or(Prim::Int),
                _ => Prim::Int,
            };
            let mut it = IntTy::of(base).promoted();
            it.enum_ty = ty;
            return Ok(Target::Value(Value::Int(it.norm(v as u64), it)));
        }
        if let Some(v) = builtins::constant(self.prog.name(s)) {
            return Ok(Target::Value(v));
        }
        self.err(format!("'{}' is not defined", self.prog.name(s)))
    }

    // ---- reading the file -------------------------------------------------

    pub(crate) fn file_len(&self) -> u64 {
        self.src.len()
    }

    pub(crate) fn read_bytes(&mut self, at: u64, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let got = self.src.read_at(at, &mut buf);
        buf.truncate(got);
        buf
    }

    /// An unsigned integer of `size` bytes at `at`.
    pub(crate) fn read_uint(&mut self, at: u64, size: usize, big: bool) -> R<u64> {
        let mut buf = [0u8; 8];
        let size = size.min(8);
        if self.src.read_at(at, &mut buf[..size]) < size {
            return self.err(format!("read past the end of the file at 0x{at:X}"));
        }
        let mut v = 0u64;
        for k in 0..size {
            let b = if big { buf[k] } else { buf[size - 1 - k] };
            v = (v << 8) | b as u64;
        }
        Ok(v)
    }

    fn read_bits(&mut self, at: u64, size: usize, bits: BitLoc) -> R<u64> {
        let mut buf = [0u8; 16];
        let size = size.min(16);
        if self.src.read_at(at, &mut buf[..size]) < size {
            return self.err(format!("read past the end of the file at 0x{at:X}"));
        }
        let mut v: u128 = 0;
        for k in 0..size {
            let b = if bits.le { buf[size - 1 - k] } else { buf[k] };
            v = (v << 8) | b as u128;
        }
        let mask: u128 =
            if bits.width >= 64 { u64::MAX as u128 } else { (1u128 << bits.width) - 1 };
        Ok(((v >> bits.shift) & mask) as u64)
    }

    /// A scalar of type `prim` at `at` in the given byte order.
    pub(crate) fn read_scalar(&mut self, prim: Prim, at: u64, big: bool) -> R<Value> {
        let size = prim.size() as usize;
        let raw = self.read_uint(at, size, big)?;
        Ok(scalar_from_raw(prim, raw))
    }

    /// The value of file variable `r`: scalars and strings are read; a char
    /// array reads as a string; structs and other arrays stay references.
    pub(crate) fn node_value(&mut self, r: NodeRef) -> R<Value> {
        let n = self.tree.node(r.id);
        let at = n.start + r.shift;
        let big = n.flags & F_BIG_ENDIAN != 0;
        let enum_ty = if n.flags & F_ENUM != 0 { n.enum_ty } else { super::tree::NONE };
        match n.kind.clone() {
            NodeKind::Scalar { prim, bits } => {
                let mut v = match bits {
                    Some(b) => {
                        // Bitfields read as unsigned, as 010 Editor does
                        // (a 2-bit `char` field holding 2 is 2, not -2).
                        let raw = self.read_bits(at, n.size as usize, b)?;
                        let it = IntTy::of(prim);
                        Value::Int(it.norm(raw), it)
                    }
                    None => self.read_scalar(prim, at, big)?,
                };
                if let Value::Int(_, it) = &mut v {
                    it.enum_ty = enum_ty;
                }
                Ok(v)
            }
            NodeKind::Str { wide } => {
                let size = n.size as usize;
                let data = self.read_bytes(at, size.min(self.limits.max_alloc));
                Ok(if wide {
                    let w = decode_wide(&data, big);
                    Value::WStr(w.into_iter().take_while(|&c| c != 0).collect())
                } else {
                    Value::Str(until_nul(&data).to_vec())
                })
            }
            NodeKind::Array { elem_prim: Some(p), count, elem_size, .. }
                if matches!(p, Prim::Char | Prim::UChar) && elem_size == 1 =>
            {
                let data = self.read_bytes(at, (count as usize).min(self.limits.max_alloc));
                Ok(Value::Str(until_nul(&data).to_vec()))
            }
            NodeKind::Array { elem_prim: Some(Prim::WChar), count, .. } => {
                let data = self.read_bytes(at, ((count * 2) as usize).min(self.limits.max_alloc));
                let w = decode_wide(&data, big);
                Ok(Value::WStr(w.into_iter().take_while(|&c| c != 0).collect()))
            }
            NodeKind::Struct { pending: Some(_), .. } => {
                self.expand(r.id)?;
                Ok(Value::Node(r))
            }
            _ => Ok(Value::Node(r)),
        }
    }

    /// Element `i` of scalar array or string variable `r`.
    fn node_elem(&mut self, r: NodeRef, i: u64) -> R<Value> {
        let n = self.tree.node(r.id);
        let at = n.start + r.shift;
        let big = n.flags & F_BIG_ENDIAN != 0;
        let enum_ty = if n.flags & F_ENUM != 0 { n.enum_ty } else { super::tree::NONE };
        match n.kind.clone() {
            NodeKind::Array {
                elem_prim: Some(p),
                count,
                elem_size,
                kind: ArrayKind::Scalar,
                ..
            } => {
                if i >= count {
                    return self.err(format!("index {i} out of bounds (array of {count})"));
                }
                let mut v = self.read_scalar(p, at + i * elem_size, big)?;
                if let Value::Int(_, it) = &mut v {
                    it.enum_ty = enum_ty;
                }
                Ok(v)
            }
            NodeKind::Str { wide } => {
                let w = if wide { 2 } else { 1 };
                if i * w >= n.size {
                    return self.err(format!("index {i} out of bounds (string of {})", n.size / w));
                }
                let p = if wide { Prim::WChar } else { Prim::Char };
                self.read_scalar(p, at + i * w, big)
            }
            _ => self.err("not an array"),
        }
    }

    // ---- places -----------------------------------------------------------

    pub(crate) fn load(&mut self, t: Target) -> R<Value> {
        match t {
            Target::Value(Value::Node(r)) | Target::Place(Place::Node(r)) => self.node_value(r),
            Target::Value(v) => Ok(v),
            Target::Dup(ids) => {
                self.node_value(NodeRef::new(*ids.last().expect("dup has elements")))
            }
            Target::Place(Place::NodeElem(r, i)) => self.node_elem(r, i),
            Target::Place(Place::Var { frame, slot, path }) => {
                let Slot::Val(root, _) = &self.frame_at(frame).vars[slot].1 else {
                    return self.err("bad variable reference");
                };
                // A character of a local string.
                if let Some((Step::Index(i), prefix)) = path.split_last()
                    && let Some(parent @ (Value::Str(_) | Value::WStr(_))) = walk(root, prefix)
                {
                    return Ok(match parent {
                        Value::Str(s) => {
                            Value::Int(s.get(*i).copied().unwrap_or(0) as i8 as u64, IntTy::I8)
                        }
                        Value::WStr(w) => {
                            Value::Int(w.get(*i).copied().unwrap_or(0) as u64, IntTy::new(2, false))
                        }
                        _ => unreachable!(),
                    });
                }
                let v = walk(root, &path)
                    .ok_or_else(|| Stop::Error("index out of bounds".into(), self.cur_pos))?;
                match v {
                    Value::Node(r) => {
                        let r = *r;
                        self.node_value(r)
                    }
                    v => Ok(v.clone()),
                }
            }
        }
    }

    /// Store `v` into a place. File variables are only written in write mode
    /// (a `write=` callback); a template run never changes the file.
    pub(crate) fn store(&mut self, p: &Place, v: Value) -> R<()> {
        match p {
            Place::Var { frame, slot, path } => {
                let prog = self.prog.clone();
                let (frame, slot) = (*frame, *slot);
                let ty = match &self.frame_at(frame).vars[slot].1 {
                    Slot::Val(_, ty) => *ty,
                    _ => return self.err("bad variable reference"),
                };
                let current_kind = match &self.frame_at(frame).vars[slot].1 {
                    Slot::Val(root, _) => match walk(root, path) {
                        Some(Value::Array(a)) => Some(Some(a.elem)),
                        Some(Value::Record(_)) => Some(None),
                        _ => None,
                    },
                    _ => None,
                };
                let limit = self.limits.max_alloc;
                let pos = self.cur_pos;
                // A character of a local string.
                if let Some((Step::Index(i), prefix)) = path.split_last() {
                    let is_str = match &self.frame_at(frame).vars[slot].1 {
                        Slot::Val(root, _) => {
                            matches!(walk(root, prefix), Some(Value::Str(_) | Value::WStr(_)))
                        }
                        _ => false,
                    };
                    if is_str {
                        let c = self.int_of(&v)?;
                        let i = *i;
                        let Slot::Val(root, _) = &mut self.frame_at_mut(frame).vars[slot].1 else {
                            unreachable!()
                        };
                        match walk_mut(root, prefix, limit) {
                            Some(Value::Str(s)) => {
                                if i >= s.len() {
                                    s.resize(i + 1, 0);
                                }
                                s[i] = c as u8;
                            }
                            Some(Value::WStr(w)) => {
                                if i >= w.len() {
                                    w.resize(i + 1, 0);
                                }
                                w[i] = c as u16;
                            }
                            _ => {}
                        }
                        return Ok(());
                    }
                }
                let conv = match current_kind {
                    // A whole local array: a string or another array copied in.
                    Some(Some(elem)) => {
                        let is_text = matches!(v, Value::Str(_) | Value::WStr(_));
                        let mut arr = self.local_array_of(v, elem)?;
                        if is_text {
                            let zero = self.zero(elem)?;
                            arr.items.push(zero);
                        }
                        Value::Array(Box::new(arr))
                    }
                    Some(None) => v,
                    None if path.is_empty() => self.convert(v, ty)?,
                    None => match self.path_type(&prog, frame, slot, path) {
                        Some(t) => self.convert(v, t)?,
                        None => v,
                    },
                };
                let Slot::Val(root, _) = &mut self.frame_at_mut(frame).vars[slot].1 else {
                    unreachable!()
                };
                let target = walk_mut(root, path, limit)
                    .ok_or_else(|| Stop::Error("index out of bounds".into(), pos))?;
                *target = conv;
                Ok(())
            }
            Place::Node(r) => {
                let r = *r;
                self.write_node(r, None, v)
            }
            Place::NodeElem(r, i) => {
                let (r, i) = (*r, *i);
                self.write_node(r, Some(i), v)
            }
        }
    }

    /// The element type an array element at `path` must be stored as.
    fn path_type(
        &self,
        _prog: &Program,
        frame: FrameRef,
        slot: usize,
        path: &[Step],
    ) -> Option<TypeId> {
        let Slot::Val(root, _) = &self.frame_at(frame).vars[slot].1 else { return None };
        let (Step::Index(_), prefix) = path.split_last()? else { return None };
        match walk(root, prefix)? {
            Value::Array(a) => Some(a.elem),
            _ => None,
        }
    }

    fn write_node(&mut self, r: NodeRef, elem: Option<u64>, v: Value) -> R<()> {
        if self.writes.is_none() {
            if !self.warned_write {
                self.warned_write = true;
                self.warn("assignments to file variables are ignored (the file is never changed by a run)");
            }
            return Ok(());
        }
        let n = self.tree.node(r.id).clone();
        let at = n.start + r.shift;
        let big = n.flags & F_BIG_ENDIAN != 0;
        let bytes: Vec<u8>;
        let mut write_at = at;
        match (&n.kind, elem) {
            (NodeKind::Scalar { prim, bits: None }, None) => {
                bytes = encode_scalar(*prim, &self.load(Target::Value(v))?, big);
            }
            (NodeKind::Scalar { bits: Some(b), .. }, None) => {
                let new = self.int_of(&v)? as u64;
                let size = n.size as usize;
                let old = self.read_bytes(at, size);
                let mut acc: u128 = 0;
                for k in 0..old.len() {
                    let byte = if b.le { old[old.len() - 1 - k] } else { old[k] };
                    acc = (acc << 8) | byte as u128;
                }
                let mask: u128 =
                    if b.width >= 64 { u64::MAX as u128 } else { (1u128 << b.width) - 1 };
                acc = (acc & !(mask << b.shift)) | (((new as u128) & mask) << b.shift);
                let mut out = vec![0u8; size];
                for k in 0..size {
                    let byte = (acc >> (8 * k)) as u8;
                    if b.le {
                        out[k] = byte;
                    } else {
                        out[size - 1 - k] = byte;
                    }
                }
                bytes = out;
            }
            (NodeKind::Array { elem_prim: Some(p), elem_size, count, .. }, Some(i)) => {
                if i >= *count {
                    return self.err("index out of bounds");
                }
                write_at = at + i * elem_size;
                bytes = encode_scalar(*p, &self.load(Target::Value(v))?, big);
            }
            (NodeKind::Array { elem_prim: Some(p), count, .. }, None)
                if matches!(p, Prim::Char | Prim::UChar | Prim::WChar) =>
            {
                let wide = *p == Prim::WChar;
                let s = self.bytes_of(&v)?;
                let cap = *count as usize;
                let mut out = if wide {
                    bytes_to_wide(&s)
                        .into_iter()
                        .flat_map(|c| if big { c.to_be_bytes() } else { c.to_le_bytes() })
                        .collect()
                } else {
                    s
                };
                let unit = if wide { 2 } else { 1 };
                out.resize(cap * unit, 0);
                bytes = out;
            }
            (NodeKind::Str { wide }, None) => {
                let s = self.bytes_of(&v)?;
                let mut out = if *wide {
                    bytes_to_wide(&s)
                        .into_iter()
                        .flat_map(|c| if big { c.to_be_bytes() } else { c.to_le_bytes() })
                        .collect()
                } else {
                    s
                };
                out.resize(n.size as usize, 0);
                bytes = out;
            }
            (NodeKind::Str { wide }, Some(i)) => {
                let unit = if *wide { 2 } else { 1 };
                write_at = at + i * unit;
                bytes = encode_scalar(if *wide { Prim::WChar } else { Prim::UChar }, &v, big);
            }
            _ => return self.err("this variable can't be assigned"),
        }
        if let Some(w) = &mut self.writes {
            w.push((write_at, bytes));
        }
        Ok(())
    }

    // ---- conversions ------------------------------------------------------

    pub(crate) fn int_of(&mut self, v: &Value) -> R<i64> {
        match v {
            Value::Int(b, _) => Ok(*b as i64),
            Value::Float(f, _) => Ok(*f as i64),
            Value::Node(r) => {
                let r = *r;
                let v = self.node_value(r)?;
                if matches!(v, Value::Node(_)) {
                    return self.err("a struct or array is not a number");
                }
                self.int_of(&v)
            }
            Value::Str(s) if s.len() == 1 => Ok(s[0] as i8 as i64),
            _ => self.err("not a number"),
        }
    }

    pub(crate) fn float_of(&mut self, v: &Value) -> R<f64> {
        match v {
            Value::Node(r) => {
                let r = *r;
                let v = self.node_value(r)?;
                if matches!(v, Value::Node(_)) {
                    return self.err("a struct or array is not a number");
                }
                self.float_of(&v)
            }
            v => v.as_f64_lossy().map_or_else(|| self.err("not a number"), Ok),
        }
    }

    /// A value as string bytes (char arrays and strings; a number is its
    /// decimal text).
    pub(crate) fn bytes_of(&mut self, v: &Value) -> R<Vec<u8>> {
        match v {
            Value::Str(s) => Ok(until_nul(s).to_vec()),
            Value::WStr(w) => Ok(wide_to_bytes(w)),
            Value::Node(r) => {
                let r = *r;
                match self.node_value(r)? {
                    Value::Node(_) => self.err("a struct is not a string"),
                    v => self.bytes_of(&v),
                }
            }
            Value::Array(a) => Ok(until_nul(
                &a.items.iter().map(|x| x.as_i64_lossy().unwrap_or(0) as u8).collect::<Vec<_>>(),
            )
            .to_vec()),
            Value::Int(c, _) => Ok(vec![*c as u8]),
            Value::Float(f, _) => Ok(format!("{f}").into_bytes()),
            _ => self.err("not a string"),
        }
    }

    pub(crate) fn truthy(&mut self, v: &Value) -> R<bool> {
        match v {
            Value::Int(b, _) => Ok(*b != 0),
            Value::Float(f, _) => Ok(*f != 0.0),
            Value::Str(s) => Ok(!s.is_empty()),
            Value::WStr(s) => Ok(!s.is_empty()),
            Value::Node(r) => {
                let r = *r;
                match self.node_value(r)? {
                    Value::Node(_) => Ok(true),
                    v => self.truthy(&v),
                }
            }
            Value::Void => Ok(false),
            _ => Ok(true),
        }
    }

    /// Convert `v` for storage in a local of type `ty`.
    pub(crate) fn convert(&mut self, v: Value, ty: TypeId) -> R<Value> {
        let prog = self.prog.clone();
        let rid = prog.resolve(ty);
        match &prog.ty(rid).kind {
            TypeKind::Prim(Prim::Str) => Ok(Value::Str(self.bytes_of(&v)?)),
            TypeKind::Prim(Prim::WStr) => Ok(match v {
                Value::WStr(w) => Value::WStr(w),
                other => Value::WStr(bytes_to_wide(&self.bytes_of(&other)?)),
            }),
            TypeKind::Prim(Prim::Void) => Ok(Value::Void),
            TypeKind::Prim(p) if p.is_float() => {
                let f = self.float_of(&v)?;
                Ok(Value::Float(
                    if *p == Prim::Float { f as f32 as f64 } else { f },
                    *p == Prim::Float,
                ))
            }
            TypeKind::Prim(p) => {
                let it = IntTy::of(*p);
                let raw = match &v {
                    Value::Float(f, _) => {
                        if it.signed {
                            *f as i64 as u64
                        } else {
                            *f as u64
                        }
                    }
                    _ => self.int_of(&v)? as u64,
                };
                Ok(Value::Int(it.norm(raw), it))
            }
            TypeKind::Enum { base, .. } => {
                let p = prog.prim_of(*base).unwrap_or(Prim::Int);
                let mut it = IntTy::of(p);
                it.enum_ty = rid;
                let raw = self.int_of(&v)? as u64;
                Ok(Value::Int(it.norm(raw), it))
            }
            TypeKind::Alias { target, dim: Some(_) } => {
                // A local array type: strings go into char arrays.
                match v {
                    Value::Array(a) => Ok(Value::Array(a)),
                    Value::Node(r) => Ok(Value::Node(r)),
                    other => {
                        let bytes = self.bytes_of(&other)?;
                        let elem = *target;
                        let items = bytes
                            .into_iter()
                            .map(|b| Value::Int(b as i8 as u64, IntTy::I8))
                            .collect();
                        Ok(Value::Array(Box::new(LocalArray { elem, items })))
                    }
                }
            }
            TypeKind::Alias { .. } | TypeKind::Struct { .. } => match v {
                v @ (Value::Record(_) | Value::Node(_)) => Ok(v),
                _ => self.err("can't convert to a struct"),
            },
        }
    }

    /// A short text form of a value, for `return` output and `%s`.
    pub(crate) fn display_value(&mut self, v: &Value) -> String {
        match v {
            Value::Int(b, t) => {
                if t.signed {
                    (*b as i64).to_string()
                } else {
                    b.to_string()
                }
            }
            Value::Float(f, _) => format!("{f}"),
            Value::Str(s) => String::from_utf8_lossy(s).into_owned(),
            Value::WStr(w) => String::from_utf16_lossy(w),
            Value::Node(r) => match self.node_value(*r) {
                Ok(Value::Node(_)) | Err(_) => String::new(),
                Ok(v) => self.display_value(&v),
            },
            Value::Void => String::new(),
            Value::Array(_) | Value::Record(_) => String::new(),
        }
    }
}

/// A value from `size` raw bytes already read as an unsigned integer.
pub(crate) fn scalar_from_raw(prim: Prim, raw: u64) -> Value {
    match prim {
        Prim::Float => Value::Float(f32::from_bits(raw as u32) as f64, true),
        Prim::Double | Prim::OleTime => Value::Float(f64::from_bits(raw), false),
        Prim::HFloat => Value::Float(half_to_f64(raw as u16), true),
        p => {
            let it = IntTy::of(p);
            Value::Int(it.norm(raw), it)
        }
    }
}

pub(crate) fn half_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f64;
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 => {
            if frac == 0.0 {
                f64::INFINITY
            } else {
                f64::NAN
            }
        }
        e => (1.0 + frac / 1024.0) * 2f64.powi(e - 15),
    }
}

fn f64_to_half(f: f64) -> u16 {
    let bits = (f as f32).to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let frac = bits & 0x7f_ffff;
    if exp >= 31 {
        sign | 0x7c00
    } else if exp <= 0 {
        sign
    } else {
        sign | ((exp as u16) << 10) | ((frac >> 13) as u16)
    }
}

/// The bytes of a scalar `v` stored as `prim`.
pub(crate) fn encode_scalar(prim: Prim, v: &Value, big: bool) -> Vec<u8> {
    let size = prim.size() as usize;
    let raw: u64 = match prim {
        Prim::Float => (v.as_f64_lossy().unwrap_or(0.0) as f32).to_bits() as u64,
        Prim::Double | Prim::OleTime => v.as_f64_lossy().unwrap_or(0.0).to_bits(),
        Prim::HFloat => f64_to_half(v.as_f64_lossy().unwrap_or(0.0)) as u64,
        _ => match v {
            Value::Float(f, _) => *f as i64 as u64,
            other => other.as_i64_lossy().unwrap_or(0) as u64,
        },
    };
    let le = raw.to_le_bytes();
    let mut out = le[..size].to_vec();
    if big {
        out.reverse();
    }
    out
}

pub(crate) fn decode_wide(data: &[u8], big: bool) -> Vec<u16> {
    data.as_chunks::<2>()
        .0
        .iter()
        .map(|c| if big { u16::from_be_bytes(*c) } else { u16::from_le_bytes(*c) })
        .collect()
}

/// Follow `path` into a local value.
fn walk<'a>(v: &'a Value, path: &[Step]) -> Option<&'a Value> {
    let mut cur = v;
    for step in path {
        cur = match (cur, step) {
            (Value::Array(a), Step::Index(i)) => a.items.get(*i)?,
            (Value::Record(r), Step::Field(f)) => r.field(*f)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn walk_mut<'a>(v: &'a mut Value, path: &[Step], limit: usize) -> Option<&'a mut Value> {
    let mut cur = v;
    for step in path {
        cur = match (cur, step) {
            (Value::Array(a), Step::Index(i)) => {
                // Writing just past the end grows a local array (as strings do).
                if *i == a.items.len() && *i < limit {
                    let fill = a.items.last().cloned().map(zero_like).unwrap_or(Value::int(0));
                    a.items.push(fill);
                }
                a.items.get_mut(*i)?
            }
            (Value::Record(r), Step::Field(f)) => r.field_mut(*f)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn zero_like(v: Value) -> Value {
    match v {
        Value::Int(_, t) => Value::Int(0, t),
        Value::Float(_, f) => Value::Float(0.0, f),
        Value::Str(_) => Value::Str(Vec::new()),
        Value::WStr(_) => Value::WStr(Vec::new()),
        other => other,
    }
}

#[cfg(test)]
mod tests;
