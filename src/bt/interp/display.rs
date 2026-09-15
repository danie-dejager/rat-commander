//! How template variables are shown: names with their indices, values in
//! their display format, type labels, and the results of `read=`, `name=` and
//! `comment=` callbacks — which run here, when a row is drawn, and are cached
//! until the data changes.

use super::{Interp, Stop};
use crate::bt::ast::{Prim, TypeKind};
#[cfg(test)]
use crate::bt::tree::{ArrayKind, ROOT};
use crate::bt::tree::{F_ENUM, F_HIDDEN, Format, NONE, NodeKind, NodeRef};
use crate::bt::value::Value;
use std::collections::HashMap;

/// Which callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LazyKind {
    Read,
    Comment,
    Name,
}

/// Cached callback results, keyed by node and callback.
#[derive(Default)]
pub struct LazyCache(HashMap<(NodeRef, LazyKind), Option<String>>);

impl LazyCache {
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl Interp {
    /// Evaluate a node's callback, with the display limits, never failing.
    fn lazy_text(&mut self, cache: &mut LazyCache, r: NodeRef, kind: LazyKind) -> Option<String> {
        if let Some(c) = cache.0.get(&(r, kind)) {
            return c.clone();
        }
        let lazy = self.tree.lazy_of(r.id);
        let at = match kind {
            LazyKind::Read => lazy.read,
            LazyKind::Comment => lazy.comment,
            LazyKind::Name => lazy.name,
        };
        let text = at.map(|at| {
            self.steps = 0;
            let saved_pos = self.pos;
            self.in_callback += 1;
            let v = self.attr_value(at, r, None);
            self.in_callback -= 1;
            self.pos = saved_pos;
            match v {
                Ok(v) => self.display_value(&v),
                Err(Stop::Error(msg, _)) => format!("(error: {msg})"),
                Err(_) => "(error)".to_string(),
            }
        });
        cache.0.insert((r, kind), text.clone());
        text
    }

    /// The name shown for node `r`: its `name=` text or declared name, with the
    /// index of an array element or duplicate.
    pub fn row_name(
        &mut self,
        cache: &mut LazyCache,
        r: NodeRef,
        array_index: Option<u64>,
    ) -> String {
        let n = self.tree.node(r.id);
        let (name, dup, index) = (n.name, n.dup, n.index);
        let base = self
            .lazy_text(cache, r, LazyKind::Name)
            .unwrap_or_else(|| self.prog.name(name).to_string());
        match (array_index, dup, index) {
            (Some(i), ..) => format!("{base}[{i}]"),
            (None, d, _) if d != NONE => format!("{base}[{d}]"),
            (None, _, i) if i != NONE => format!("{base}[{i}]"),
            _ => base,
        }
    }

    /// The comment shown for node `r`.
    pub fn row_comment(&mut self, cache: &mut LazyCache, r: NodeRef) -> String {
        self.lazy_text(cache, r, LazyKind::Comment).unwrap_or_default()
    }

    /// The value shown for node `r`: its `read=` text, or its own value.
    pub fn row_value(&mut self, cache: &mut LazyCache, r: NodeRef) -> String {
        if let Some(t) = self.lazy_text(cache, r, LazyKind::Read) {
            return t;
        }
        self.steps = 0;
        let n = self.tree.node(r.id).clone();
        match &n.kind {
            NodeKind::Struct { .. } => String::new(),
            NodeKind::Array { elem_ty, .. }
                if super::builtins::is_guid(self, n.ty)
                    || super::builtins::is_guid(self, *elem_ty) =>
            {
                let b = self.read_bytes(n.start + r.shift, 16);
                if b.len() == 16 { super::builtins::guid_text(&b) } else { String::new() }
            }
            NodeKind::Array { elem_prim: Some(p @ (Prim::Char | Prim::UChar)), count, .. } => {
                // Text shows as a string, other bytes as hex: `uchar pad[6]`
                // is six zeros, not an empty string.
                let raw = self.read_bytes(n.start + r.shift, (*count as usize).min(256));
                let text = match raw.iter().rposition(|&b| b != 0) {
                    Some(last) => &raw[..=last],
                    None => &raw[..0],
                };
                let textual = std::str::from_utf8(text).is_ok_and(|t| {
                    t.chars().all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
                });
                if *p == Prim::UChar && (!textual || text.is_empty()) {
                    let shown: Vec<String> =
                        raw.iter().take(32).map(|b| format!("{b:02X}")).collect();
                    let more = if *count > 32 { " …" } else { "" };
                    format!("{}{more}", shown.join(" "))
                } else {
                    quote_bytes(crate::bt::value::until_nul(&raw), 256)
                }
            }
            NodeKind::Array { elem_prim: Some(Prim::WChar), .. } | NodeKind::Str { .. } => {
                match self.node_value(r) {
                    Ok(Value::Str(s)) => quote_bytes(&s, 256),
                    Ok(Value::WStr(w)) => quote_bytes(String::from_utf16_lossy(&w).as_bytes(), 256),
                    Ok(_) => String::new(),
                    Err(Stop::Error(m, _)) => format!("(error: {m})"),
                    Err(_) => String::new(),
                }
            }
            NodeKind::Array { .. } => String::new(),
            NodeKind::Scalar { prim, .. } => match self.node_value(r) {
                Ok(v) => self.format_scalar(*prim, &v, n.format, n.flags & F_ENUM != 0),
                Err(Stop::Error(m, _)) => format!("(error: {m})"),
                Err(_) => String::new(),
            },
        }
    }

    /// The value of element `i` of scalar array `r`.
    pub fn element_value(&mut self, r: NodeRef, i: u64) -> String {
        self.steps = 0;
        let n = self.tree.node(r.id).clone();
        let NodeKind::Array { elem_prim: Some(p), .. } = n.kind else { return String::new() };
        match self.node_elem(r, i) {
            Ok(v) => self.format_scalar(p, &v, n.format, n.flags & F_ENUM != 0),
            Err(Stop::Error(m, _)) => format!("(error: {m})"),
            Err(_) => String::new(),
        }
    }

    /// A scalar in display format `f`.
    pub fn format_scalar(&mut self, prim: Prim, v: &Value, f: Format, is_enum: bool) -> String {
        use crate::bt::interp::time;
        match v {
            Value::Float(x, _) => {
                if prim == Prim::OleTime {
                    return time::format(&time::from_oletime(*x), "MM/dd/yyyy hh:mm:ss");
                }
                let s = format!("{x}");
                if s.len() > 16 { format!("{x:e}") } else { s }
            }
            Value::Int(bits, t) => {
                let raw = t.norm(*bits);
                let shown_bits = t.bytes as u32 * 8;
                let unsigned =
                    if shown_bits >= 64 { raw } else { raw & ((1u64 << shown_bits) - 1) };
                match prim {
                    Prim::DosDate => {
                        return time::format(&time::from_dosdate(raw as u16), "MM/dd/yyyy");
                    }
                    Prim::DosTime => {
                        return time::format(&time::from_dostime(raw as u16), "hh:mm:ss");
                    }
                    Prim::FileTime => {
                        return time::format(&time::from_filetime(raw), "MM/dd/yyyy hh:mm:ss");
                    }
                    Prim::TimeT | Prim::Time64T => {
                        return time::format(
                            &time::from_unix(raw as i64, 0),
                            "MM/dd/yyyy hh:mm:ss",
                        );
                    }
                    _ => {}
                }
                let number = match f {
                    Format::Hex => format!("0x{unsigned:X}"),
                    Format::Binary => format!("{unsigned:b}b"),
                    Format::Octal => format!("0{unsigned:o}"),
                    Format::DecimalHex => {
                        if t.signed {
                            format!("{} (0x{unsigned:X})", raw as i64)
                        } else {
                            format!("{raw} (0x{unsigned:X})")
                        }
                    }
                    Format::Decimal => {
                        if t.signed {
                            (raw as i64).to_string()
                        } else {
                            raw.to_string()
                        }
                    }
                };
                if is_enum {
                    return match self.enum_text(v) {
                        Some(name) => format!("{name} ({number})"),
                        None => number,
                    };
                }
                if matches!(prim, Prim::Char | Prim::UChar | Prim::WChar) {
                    let c = unsigned as u32;
                    if let Some(ch) = char::from_u32(c).filter(|ch| !ch.is_control() && c >= 0x20) {
                        return format!("{number} '{ch}'");
                    }
                }
                number
            }
            _ => String::new(),
        }
    }

    /// The type shown for node `r` (`uint`, `char[4]`, `struct HEADER`).
    pub fn type_label(&self, r: NodeRef) -> String {
        let n = self.tree.node(r.id);
        let td = self.prog.ty(n.ty);
        let mut name = self.prog.name(td.name).to_string();
        let resolved = self.prog.ty(self.prog.resolve(n.ty));
        match &resolved.kind {
            TypeKind::Struct { union, .. }
                if name.is_empty() || name == self.prog.name(resolved.name) =>
            {
                let kw = if *union { "union" } else { "struct" };
                name = if name.is_empty() { kw.to_string() } else { format!("{kw} {name}") };
            }
            TypeKind::Enum { .. } if name.is_empty() => name = "enum".into(),
            _ => {}
        }
        match &n.kind {
            // A declarator's dimension: the declared type is the element's.
            NodeKind::Array { count, .. }
                if !matches!(td.kind, TypeKind::Alias { dim: Some(_), .. }) =>
            {
                format!("{name}[{count}]")
            }
            NodeKind::Str { .. }
                if !matches!(resolved.kind, TypeKind::Prim(Prim::Str | Prim::WStr)) =>
            {
                format!("{name}[]")
            }
            NodeKind::Scalar { bits: Some(b), .. } => format!("{name} :{}", b.width),
            _ => name,
        }
    }

    /// Whether node `r` has children to open.
    pub fn has_children(&self, r: NodeRef) -> bool {
        let n = self.tree.node(r.id);
        match &n.kind {
            NodeKind::Struct { pending: Some(_), .. } => true,
            NodeKind::Struct { .. } => {
                n.children.iter().any(|&c| self.tree.node(c).flags & F_HIDDEN == 0)
            }
            NodeKind::Array { count, .. } => *count > 0,
            _ => false,
        }
    }

    /// The byte range node `r` covers.
    pub fn range_of(&self, r: NodeRef) -> (u64, u64) {
        let n = self.tree.node(r.id);
        (n.start + r.shift, n.size)
    }

    /// Run on-demand struct `r`'s body now (the panel opened it). Errors go to
    /// the output.
    pub fn open_node(&mut self, r: NodeRef) {
        self.steps = 0;
        if let Err(Stop::Error(msg, pos)) = self.expand(r.id) {
            let at = self.prog.pos_text(pos);
            self.print(&format!("Error ({at}): {msg}\n"));
            self.flush_output();
        }
    }
}

/// Bytes as a quoted, escaped string, at most `max` characters.
pub fn quote_bytes(b: &[u8], max: usize) -> String {
    let text = String::from_utf8_lossy(b);
    let mut out = String::with_capacity(b.len().min(max) + 2);
    out.push('"');
    for (i, ch) in text.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            c if c.is_control() => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A text dump of the first `rows` variables (debugging and tests).
#[cfg(test)]
pub fn dump(it: &mut Interp, rows: usize) -> Vec<String> {
    let mut cache = LazyCache::default();
    let mut out = Vec::new();
    let mut stack: Vec<(NodeRef, usize, Option<u64>)> =
        it.tree.child_refs(NodeRef::new(ROOT)).into_iter().rev().map(|r| (r, 0, None)).collect();
    while let Some((r, depth, idx)) = stack.pop() {
        if out.len() >= rows {
            break;
        }
        let n = it.tree.node(r.id).clone();
        if n.flags & F_HIDDEN != 0 {
            continue;
        }
        let name = it.row_name(&mut cache, r, idx);
        let value = it.row_value(&mut cache, r);
        let comment = it.row_comment(&mut cache, r);
        let ty = it.type_label(r);
        out.push(format!(
            "{:indent$}{name} = {value}  [{ty} @0x{:X} +{}]{}",
            "",
            n.start + r.shift,
            n.size,
            if comment.is_empty() { String::new() } else { format!("  // {comment}") },
            indent = depth * 2
        ));
        match &n.kind {
            NodeKind::Struct { .. } => {
                it.open_node(r);
                for c in it.tree.child_refs(r).into_iter().rev() {
                    stack.push((c, depth + 1, None));
                }
            }
            NodeKind::Array { kind: ArrayKind::Optimized, count, .. } => {
                for i in (0..(*count).min(3)).rev() {
                    if let Some(e) = it.tree.element(r, i) {
                        stack.push((e, depth + 1, Some(i)));
                    }
                }
            }
            NodeKind::Array { kind: ArrayKind::Full, .. } => {
                for c in it.tree.child_refs(r).into_iter().take(3).rev() {
                    stack.push((c, depth + 1, None));
                }
            }
            _ => {}
        }
    }
    out
}
