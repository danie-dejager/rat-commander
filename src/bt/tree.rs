//! The result of running a template: a tree of variables mapped onto the file.
//!
//! Only positions and types are stored — a value is read from the file when it
//! is shown, so edits show at once and a tree of a huge file stays small. The
//! elements of an optimized array of structs aren't stored either: element `i`
//! is element 0's subtree seen `i * size` bytes further on (a [`NodeRef`] with
//! a `shift`).

use super::ast::{Prim, Sym, TypeId};
use std::collections::HashMap;

pub const ROOT: u32 = 0;
pub const NONE: u32 = u32::MAX;
/// 010 Editor's `cNone`.
pub const NO_COLOR: u32 = 0xFFFF_FFFF;

/// A node, possibly seen through an optimized array at `shift` bytes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct NodeRef {
    pub id: u32,
    pub shift: u64,
}

impl NodeRef {
    pub fn new(id: u32) -> Self {
        NodeRef { id, shift: 0 }
    }
}

/// Where a bitfield's bits are: the node's bytes read as one integer (little-
/// or big-endian), shifted right by `shift`, masked to `width` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitLoc {
    pub shift: u8,
    pub width: u8,
    pub le: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayKind {
    /// Scalars: elements are computed, never stored.
    Scalar,
    /// Structs, only element 0 stored (children[0]); the rest are shifts of it.
    Optimized,
    /// Every element stored as a child.
    Full,
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    Scalar {
        prim: Prim,
        bits: Option<BitLoc>,
    },
    /// A NUL-terminated string; `size` includes the terminator if present.
    Str {
        wide: bool,
    },
    Array {
        elem_ty: TypeId,
        elem_prim: Option<Prim>,
        count: u64,
        elem_size: u64,
        kind: ArrayKind,
    },
    Struct {
        pending: Option<Box<Pending>>,
    },
}

/// The deferred body of an on-demand (`size=`) struct.
#[derive(Debug, Clone)]
pub struct Pending {
    pub ty: TypeId,
    pub args: Vec<super::value::Value>,
    pub big_endian: bool,
}

/// Display formats (`format=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Decimal,
    Hex,
    Binary,
    Octal,
    DecimalHex,
}

pub const F_HIDDEN: u16 = 1;
pub const F_OPEN: u16 = 2;
pub const F_SUPPRESS: u16 = 4;
pub const F_BIG_ENDIAN: u16 = 8;
/// A bitfield of an enum, or an enum scalar: `enum_ty` names it.
pub const F_ENUM: u16 = 16;
/// A read callback without a write callback: shown, but not editable.
pub const F_READONLY: u16 = 32;

#[derive(Debug, Clone)]
pub struct Node {
    pub name: Sym,
    /// The type as declared (for labels, attributes and enum names).
    pub ty: TypeId,
    pub parent: u32,
    pub start: u64,
    pub size: u64,
    pub kind: NodeKind,
    pub children: Vec<u32>,
    pub members: Option<Box<Members>>,
    pub flags: u16,
    pub format: Format,
    pub fg: u32,
    pub bg: u32,
    pub style: u8,
    /// The enum type of an enum scalar.
    pub enum_ty: TypeId,
    /// Index of the node's lazy attributes in [`Tree::lazy`], or `NONE`.
    pub lazy: u32,
    /// Position in a duplicate array (`x[i]`), or `NONE`.
    pub dup: u32,
    /// Position within an array (element nodes), or `NONE`.
    pub index: u32,
}

/// Attributes evaluated only when a node is shown: each is `(attribute list,
/// index)` in the program.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Lazy {
    pub read: Option<(u32, u16)>,
    pub write: Option<(u32, u16)>,
    pub comment: Option<(u32, u16)>,
    pub name: Option<(u32, u16)>,
}

#[derive(Debug, Clone)]
pub enum Member {
    Single(u32),
    Dup(Vec<u32>),
}

/// A struct's members by name.
#[derive(Debug, Clone, Default)]
pub struct Members {
    list: Vec<(Sym, Member)>,
    index: Option<HashMap<Sym, usize>>,
}

impl Members {
    pub fn get(&self, s: Sym) -> Option<&Member> {
        match &self.index {
            Some(ix) => ix.get(&s).map(|&i| &self.list[i].1),
            None => self.list.iter().find(|(k, _)| *k == s).map(|(_, m)| m),
        }
    }

    /// Register `id` under `s`; a second declaration of the name makes a
    /// duplicate array. Returns the node's position in it, or `None`.
    pub fn add(&mut self, s: Sym, id: u32) -> Option<u32> {
        let slot = match &self.index {
            Some(ix) => ix.get(&s).copied(),
            None => self.list.iter().position(|(k, _)| *k == s),
        };
        match slot {
            Some(i) => {
                let m = &mut self.list[i].1;
                match m {
                    Member::Single(first) => {
                        let first = *first;
                        *m = Member::Dup(vec![first, id]);
                        Some(1)
                    }
                    Member::Dup(v) => {
                        v.push(id);
                        Some(v.len() as u32 - 1)
                    }
                }
            }
            None => {
                self.list.push((s, Member::Single(id)));
                if let Some(ix) = &mut self.index {
                    ix.insert(s, self.list.len() - 1);
                } else if self.list.len() > 24 {
                    self.index =
                        Some(self.list.iter().enumerate().map(|(i, (k, _))| (*k, i)).collect());
                }
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub lazy: Vec<Lazy>,
    lazy_ids: HashMap<Lazy, u32>,
}

impl Tree {
    pub fn new(file_len: u64, root_ty: TypeId) -> Tree {
        let root = Node {
            name: 0,
            ty: root_ty,
            parent: NONE,
            start: 0,
            size: file_len,
            kind: NodeKind::Struct { pending: None },
            children: Vec::new(),
            members: Some(Box::default()),
            flags: F_OPEN,
            format: Format::Decimal,
            fg: NO_COLOR,
            bg: NO_COLOR,
            style: 0,
            enum_ty: NONE,
            lazy: NONE,
            dup: NONE,
            index: NONE,
        };
        Tree { nodes: vec![root], lazy: Vec::new(), lazy_ids: HashMap::new() }
    }

    pub fn node(&self, id: u32) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn node_mut(&mut self, id: u32) -> &mut Node {
        &mut self.nodes[id as usize]
    }

    pub fn push(&mut self, n: Node) -> u32 {
        self.nodes.push(n);
        (self.nodes.len() - 1) as u32
    }

    pub fn intern_lazy(&mut self, l: Lazy) -> u32 {
        if l == Lazy::default() {
            return NONE;
        }
        if let Some(&id) = self.lazy_ids.get(&l) {
            return id;
        }
        self.lazy.push(l);
        let id = (self.lazy.len() - 1) as u32;
        self.lazy_ids.insert(l, id);
        id
    }

    pub fn lazy_of(&self, id: u32) -> Lazy {
        let n = self.node(id);
        if n.lazy == NONE { Lazy::default() } else { self.lazy[n.lazy as usize] }
    }

    /// A struct's member named `s`.
    pub fn member(&self, parent: u32, s: Sym) -> Option<&Member> {
        self.node(parent).members.as_ref()?.get(s)
    }

    /// The array element `i` of array node `r`, for struct arrays.
    pub fn element(&self, r: NodeRef, i: u64) -> Option<NodeRef> {
        let n = self.node(r.id);
        let NodeKind::Array { count, elem_size, kind, .. } = &n.kind else { return None };
        if i >= *count {
            return None;
        }
        match kind {
            ArrayKind::Scalar => None,
            ArrayKind::Optimized => {
                let first = *n.children.first()?;
                Some(NodeRef { id: first, shift: r.shift + i * elem_size })
            }
            ArrayKind::Full => n.children.get(i as usize).map(|&id| NodeRef { id, shift: r.shift }),
        }
    }

    /// The children of `r` as refs, in display order, for struct and full or
    /// optimized arrays (not scalar arrays, whose elements are computed).
    pub fn child_refs(&self, r: NodeRef) -> Vec<NodeRef> {
        let n = self.node(r.id);
        match &n.kind {
            NodeKind::Array { count, kind: ArrayKind::Optimized, .. } => {
                (0..*count).filter_map(|i| self.element(r, i)).collect()
            }
            _ => n.children.iter().map(|&id| NodeRef { id, shift: r.shift }).collect(),
        }
    }

    /// The nodes whose bytes cover `off`, outermost first (not the root): each
    /// is inside the one before. An optimized array's element appears as a
    /// shifted ref after its array.
    pub fn path_at(&self, off: u64) -> Vec<NodeRef> {
        let mut cur = NodeRef::new(ROOT);
        let mut path = Vec::new();
        'down: for _ in 0..256 {
            let n = self.node(cur.id);
            if let NodeKind::Array { count, elem_size, kind: ArrayKind::Optimized, .. } = &n.kind {
                let start = n.start + cur.shift;
                if *elem_size == 0 || off < start {
                    break;
                }
                let i = (off - start) / elem_size;
                if i >= *count {
                    break;
                }
                let Some(e) = self.element(cur, i) else { break };
                path.push(e);
                cur = e;
                continue 'down;
            }
            // Children in reverse: a later declaration at the same bytes (a
            // union member, a re-read) wins.
            for &c in n.children.iter().rev() {
                let ch = self.node(c);
                let s = ch.start + cur.shift;
                if ch.size > 0 && off >= s && off < s + ch.size && ch.flags & F_HIDDEN == 0 {
                    let r = NodeRef { id: c, shift: cur.shift };
                    path.push(r);
                    cur = r;
                    continue 'down;
                }
            }
            break;
        }
        path
    }

    /// The colours (`fg`, `bg`) of the deepest coloured node covering each
    /// byte of `start..start + len`, as 010 `0xBBGGRR` values or [`NO_COLOR`],
    /// plus a style id.
    pub fn colors_in(&self, start: u64, len: usize) -> Vec<(u32, u32, u8)> {
        let mut out = vec![(NO_COLOR, NO_COLOR, 0u8); len];
        if len == 0 {
            return out;
        }
        let end = start + len as u64;
        self.paint(NodeRef::new(ROOT), start, end, &mut out, 0);
        out
    }

    fn paint(&self, r: NodeRef, start: u64, end: u64, out: &mut [(u32, u32, u8)], depth: usize) {
        if depth > 128 {
            return;
        }
        let n = self.node(r.id);
        let s = n.start + r.shift;
        let e = s.saturating_add(n.size);
        if r.id != ROOT && (e <= start || s >= end) {
            return;
        }
        if r.id != ROOT && (n.fg != NO_COLOR || n.bg != NO_COLOR || n.style != 0) {
            let a = s.max(start);
            let b = e.min(end);
            for slot in &mut out[(a - start) as usize..(b - start) as usize] {
                *slot = (n.fg, n.bg, n.style);
            }
        }
        match &n.kind {
            NodeKind::Array { count, elem_size, kind: ArrayKind::Optimized, .. }
                if *elem_size > 0 =>
            {
                // Only the elements overlapping the window.
                let first = start.saturating_sub(s) / elem_size;
                let last = ((end.saturating_sub(s)).div_ceil(*elem_size)).min(*count);
                for i in first..last {
                    if let Some(el) = self.element(r, i) {
                        self.paint(el, start, end, out, depth + 1);
                    }
                }
            }
            _ => {
                for &c in &n.children {
                    self.paint(NodeRef { id: c, shift: r.shift }, start, end, out, depth + 1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_turn_into_duplicate_arrays_and_index_when_many() {
        let mut m = Members::default();
        assert_eq!(m.add(1, 10), None);
        assert_eq!(m.add(1, 11), Some(1));
        assert_eq!(m.add(1, 12), Some(2));
        assert!(matches!(m.get(1), Some(Member::Dup(v)) if v == &[10, 11, 12]));
        for s in 2..40 {
            m.add(s, s * 100);
        }
        assert!(matches!(m.get(39), Some(Member::Single(3900))));
        assert!(matches!(m.get(1), Some(Member::Dup(_))));
    }
}
