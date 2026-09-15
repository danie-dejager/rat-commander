//! Recursive-descent parser for the template language.
//!
//! As in C, a statement can only be told apart from a declaration by knowing
//! which names are types, so every `typedef`, `struct`, `union` and `enum` is
//! registered the moment its name is read — which is also what lets a struct
//! refer to itself.

use super::ast::*;
use super::lex::{Diag, Pos, Tok, Token};
use super::preproc::Preprocessed;

/// Built-in struct types: the results of `FindAll` and `FindFiles`.
const PRELUDE: &str = "typedef struct { int count; int64 start[0]; int64 size[0]; } TFindResults;\n\
    typedef struct { int filecount; struct { string filename; } file[0]; \
    int dircount; struct { string dirname; } dir[0]; } TFileList;\n";

pub fn parse(pp: Preprocessed) -> Result<Program, Diag> {
    let prelude = super::lex::lex(PRELUDE.as_bytes(), u16::MAX)?;
    let mut p = Parser { toks: prelude, i: 0, prog: Program::default(), orphan_attrs: None };
    p.prog.attr_lists.push(Vec::new());
    p.register_prims();
    while p.i < p.toks.len() {
        p.stmt()?;
    }
    p.toks = pp.tokens;
    p.i = 0;
    p.prog.files = pp.files;
    p.prog.warnings = pp
        .warnings
        .iter()
        .map(|d| {
            format!(
                "{}:{}: {}",
                p.prog.files.get(d.pos.file as usize).map_or("?", String::as_str),
                d.pos.line,
                d.msg
            )
        })
        .collect();
    let mut body = Vec::new();
    p.stmts_until_end(&mut body, false)?;
    p.prog.body = body;
    Ok(p.prog)
}

struct Parser {
    toks: Vec<Token>,
    i: usize,
    prog: Program,
    /// Attributes written after a declaration's `;` (`int x; <format=hex>;`),
    /// which 010 Editor accepts: they belong to that declaration.
    orphan_attrs: Option<Vec<Attr>>,
}

/// Binary operators by token, with their precedence (higher binds tighter).
fn binop(t: &Tok) -> Option<(BinOp, u8)> {
    let Tok::Punct(p) = t else { return None };
    Some(match *p {
        "||" => (BinOp::Or, 1),
        "&&" => (BinOp::And, 2),
        "|" => (BinOp::BitOr, 3),
        "^" => (BinOp::BitXor, 4),
        "&" => (BinOp::BitAnd, 5),
        "==" => (BinOp::Eq, 6),
        "!=" => (BinOp::Ne, 6),
        "<" => (BinOp::Lt, 7),
        ">" => (BinOp::Gt, 7),
        "<=" => (BinOp::Le, 7),
        ">=" => (BinOp::Ge, 7),
        "<<" => (BinOp::Shl, 8),
        ">>" => (BinOp::Shr, 8),
        "+" => (BinOp::Add, 9),
        "-" => (BinOp::Sub, 9),
        "*" => (BinOp::Mul, 10),
        "/" => (BinOp::Div, 10),
        "%" => (BinOp::Rem, 10),
        _ => return None,
    })
}

fn assignop(t: &Tok) -> Option<Option<BinOp>> {
    let Tok::Punct(p) = t else { return None };
    Some(match *p {
        "=" => None,
        "+=" => Some(BinOp::Add),
        "-=" => Some(BinOp::Sub),
        "*=" => Some(BinOp::Mul),
        "/=" => Some(BinOp::Div),
        "%=" => Some(BinOp::Rem),
        "<<=" => Some(BinOp::Shl),
        ">>=" => Some(BinOp::Shr),
        "&=" => Some(BinOp::BitAnd),
        "|=" => Some(BinOp::BitOr),
        "^=" => Some(BinOp::BitXor),
        _ => return None,
    })
}

/// Words that start a type or a declaration.
const DECL_WORDS: &[&str] =
    &["struct", "union", "enum", "local", "const", "unsigned", "signed", "static"];

/// Words that are part of a C integer type name (`unsigned long long int`).
const INT_WORDS: &[&str] = &["char", "short", "int", "long", "double", "float"];

impl Parser {
    fn register_prims(&mut self) {
        let mut seen: Vec<Prim> = Vec::new();
        for (name, prim) in PRIM_NAMES {
            let sym = self.prog.syms.intern(name);
            let kind = if let Some(&canon) = self.prog.prim_ids.get(prim) {
                TypeKind::Alias { target: canon, dim: None }
            } else {
                TypeKind::Prim(*prim)
            };
            let id = self.add_type(sym, kind);
            if !seen.contains(prim) {
                seen.push(*prim);
                self.prog.prim_ids.insert(*prim, id);
            }
            self.prog.type_names.insert(sym, id);
        }
        let guid = self.prog.syms.intern("GUID");
        let uchar = self.prog.prim(Prim::UChar);
        let id = self.add_type(
            guid,
            TypeKind::Alias {
                target: uchar,
                dim: Some(Some(Box::new(Expr::Int(16, false, false)))),
            },
        );
        self.prog.type_names.insert(guid, id);
    }

    fn add_type(&mut self, name: Sym, kind: TypeKind) -> TypeId {
        let pos = self.pos();
        self.prog.types.push(TypeDef { name, kind, attrs: 0, pos });
        (self.prog.types.len() - 1) as TypeId
    }

    // ---- token access ----------------------------------------------------

    fn peek(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.i + k).map(|t| &t.tok)
    }

    fn pos(&self) -> Pos {
        self.toks.get(self.i).or_else(|| self.toks.last()).map(|t| t.pos).unwrap_or_default()
    }

    fn is_punct(&self, k: usize, p: &str) -> bool {
        matches!(self.peek(k), Some(Tok::Punct(q)) if *q == p)
    }

    fn is_word(&self, k: usize, w: &str) -> bool {
        matches!(self.peek(k), Some(Tok::Ident(q)) if &**q == w)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(0, p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(0, w) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, Diag> {
        Err(Diag { pos: self.pos(), msg: msg.into() })
    }

    fn found(&self) -> String {
        match self.peek(0) {
            Some(t) => t.to_string(),
            None => "end of file".into(),
        }
    }

    fn expect(&mut self, p: &str) -> Result<(), Diag> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            self.err(format!("expected '{p}', found {}", self.found()))
        }
    }

    fn ident(&mut self) -> Result<Sym, Diag> {
        match self.peek(0) {
            Some(Tok::Ident(s)) => {
                let s = s.clone();
                self.i += 1;
                Ok(self.prog.syms.intern(&s))
            }
            _ => self.err(format!("expected a name, found {}", self.found())),
        }
    }

    fn type_named(&self, name: &str) -> Option<TypeId> {
        self.prog.syms.lookup(name).and_then(|s| self.prog.type_names.get(&s).copied())
    }

    /// Whether token `k` is a type name (not a keyword).
    fn is_type_name(&self, k: usize) -> bool {
        matches!(self.peek(k), Some(Tok::Ident(n)) if self.type_named(n).is_some())
    }

    /// How many tokens a type spelled from token `k` takes, without parsing it
    /// (for telling casts and `sizeof(type)` from parenthesised expressions).
    fn type_len(&self, k: usize) -> Option<usize> {
        let Some(Tok::Ident(w)) = self.peek(k) else { return None };
        match &**w {
            "struct" | "union" | "enum" => {
                matches!(self.peek(k + 1), Some(Tok::Ident(_))).then_some(2)
            }
            "unsigned" | "signed" | "long" | "short" => {
                let mut n = 1;
                while let Some(Tok::Ident(x)) = self.peek(k + n) {
                    if INT_WORDS.contains(&&**x) || self.type_named(x).is_some() {
                        n += 1;
                    } else {
                        break;
                    }
                }
                Some(n)
            }
            "const" => self.type_len(k + 1).map(|n| n + 1),
            _ if self.type_named(w).is_some() => Some(1),
            _ => None,
        }
    }

    // ---- statements ------------------------------------------------------

    fn stmt(&mut self) -> Result<Stmt, Diag> {
        let pos = self.pos();
        match self.peek(0) {
            None => self.err("unexpected end of file"),
            Some(Tok::Punct("{")) => {
                self.i += 1;
                Ok(Stmt::Block(self.block_rest()?))
            }
            Some(Tok::Punct(";")) => {
                self.i += 1;
                Ok(Stmt::Empty)
            }
            Some(Tok::Punct("<"))
                if matches!(self.peek(1), Some(Tok::Ident(_))) && self.is_punct(2, "=") =>
            {
                let attrs = self.attrs()?;
                self.eat_punct(";");
                self.orphan_attrs = Some(attrs);
                Ok(Stmt::Empty)
            }
            Some(Tok::Ident(w)) => {
                let w = w.clone();
                match &*w {
                    "if" => {
                        // An `else if` chain is kept flat: templates have
                        // chains hundreds long, which nesting would turn into
                        // as deep a recursion.
                        let mut arms = Vec::new();
                        let mut els = None;
                        loop {
                            self.i += 1;
                            self.expect("(")?;
                            let c = self.expr()?;
                            self.expect(")")?;
                            arms.push((c, self.stmt()?));
                            if !self.eat_word("else") {
                                break;
                            }
                            if !self.is_word(0, "if") {
                                els = Some(Box::new(self.stmt()?));
                                break;
                            }
                        }
                        Ok(Stmt::If(pos, arms, els))
                    }
                    "while" => {
                        self.i += 1;
                        self.expect("(")?;
                        let c = self.expr()?;
                        self.expect(")")?;
                        Ok(Stmt::While(pos, c, Box::new(self.stmt()?)))
                    }
                    "do" => {
                        self.i += 1;
                        let body = Box::new(self.stmt()?);
                        if !self.eat_word("while") {
                            return self.err("expected 'while' after 'do' body");
                        }
                        self.expect("(")?;
                        let c = self.expr()?;
                        self.expect(")")?;
                        self.eat_punct(";");
                        Ok(Stmt::DoWhile(pos, body, c))
                    }
                    "for" => {
                        self.i += 1;
                        self.expect("(")?;
                        let init = if self.eat_punct(";") {
                            None
                        } else if self.decl_start() {
                            // A declaration consumes its own ';'.
                            Some(Box::new(self.decl_stmt()?))
                        } else {
                            let e = self.expr()?;
                            self.expect(";")?;
                            Some(Box::new(Stmt::Expr(pos, e)))
                        };
                        let cond = if self.is_punct(0, ";") { None } else { Some(self.expr()?) };
                        self.expect(";")?;
                        let step = if self.is_punct(0, ")") { None } else { Some(self.expr()?) };
                        self.expect(")")?;
                        Ok(Stmt::For(pos, init, cond, step, Box::new(self.stmt()?)))
                    }
                    "switch" => {
                        self.i += 1;
                        self.expect("(")?;
                        let c = self.expr()?;
                        self.expect(")")?;
                        self.expect("{")?;
                        Ok(Stmt::Switch(pos, c, self.block_rest()?))
                    }
                    "case" => {
                        self.i += 1;
                        let e = self.cond()?;
                        self.expect(":")?;
                        Ok(Stmt::Case(pos, e))
                    }
                    "default" if self.is_punct(1, ":") => {
                        self.i += 2;
                        Ok(Stmt::Default)
                    }
                    "break" => {
                        self.i += 1;
                        self.expect(";")?;
                        Ok(Stmt::Break)
                    }
                    "continue" => {
                        self.i += 1;
                        self.expect(";")?;
                        Ok(Stmt::Continue)
                    }
                    "return" => {
                        self.i += 1;
                        let e = if self.is_punct(0, ";") { None } else { Some(self.expr()?) };
                        self.expect(";")?;
                        Ok(Stmt::Return(pos, e))
                    }
                    "typedef" => {
                        self.i += 1;
                        self.typedef()?;
                        Ok(Stmt::Empty)
                    }
                    _ if self.decl_start() => self.decl_stmt(),
                    _ => self.expr_stmt(),
                }
            }
            _ => self.expr_stmt(),
        }
    }

    fn expr_stmt(&mut self) -> Result<Stmt, Diag> {
        let pos = self.pos();
        let e = self.expr()?;
        self.expect(";")?;
        Ok(Stmt::Expr(pos, e))
    }

    /// Statements up to the closing `}` (the `{` already taken).
    fn block_rest(&mut self) -> Result<Vec<Stmt>, Diag> {
        let mut out = Vec::new();
        self.stmts_until_end(&mut out, true)?;
        Ok(out)
    }

    /// Statements up to a `}` (taken) when `braced`, else to the end.
    fn stmts_until_end(&mut self, out: &mut Vec<Stmt>, braced: bool) -> Result<(), Diag> {
        loop {
            if braced && self.eat_punct("}") {
                return Ok(());
            }
            if self.i >= self.toks.len() {
                return if braced { self.err("expected '}', found end of file") } else { Ok(()) };
            }
            let s = self.stmt()?;
            if let Some(attrs) = self.orphan_attrs.take()
                && let Some(Stmt::Decl(d)) = out.last_mut()
                && let Some(v) = d.vars.last_mut()
            {
                if v.attrs == 0 {
                    v.attrs = self.attr_list(attrs);
                } else {
                    self.prog.attr_lists[v.attrs as usize].extend(attrs);
                }
            }
            if !matches!(s, Stmt::Empty) {
                out.push(s);
            }
        }
    }

    /// Whether a declaration starts here.
    fn decl_start(&self) -> bool {
        let Some(Tok::Ident(w)) = self.peek(0) else { return false };
        if DECL_WORDS.contains(&&**w) {
            return true;
        }
        if matches!(&**w, "long" | "short") && self.type_len(0).is_some_and(|n| n > 1) {
            return true;
        }
        if self.type_named(w).is_none() {
            return false;
        }
        // `T name`, `T : bits` (an unnamed bitfield), `T[] f(`.
        matches!(self.peek(1), Some(Tok::Ident(_)))
            || self.is_punct(1, ":")
            || (self.is_punct(1, "[")
                && self.is_punct(2, "]")
                && matches!(self.peek(3), Some(Tok::Ident(_))))
    }

    fn decl_stmt(&mut self) -> Result<Stmt, Diag> {
        let pos = self.pos();
        let (mut local, mut konst) = (false, false);
        loop {
            if self.eat_word("local") {
                local = true;
            } else if self.eat_word("const") {
                konst = true;
            } else if !self.eat_word("static") {
                break;
            }
        }
        let ty = self.type_spec()?;
        while self.eat_word("const") || self.eat_word("local") {}
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        // `char[] name(…)`: a function returning an array.
        if self.is_punct(0, "[")
            && self.is_punct(1, "]")
            && matches!(self.peek(2), Some(Tok::Ident(_)))
            && self.is_punct(3, "(")
        {
            self.i += 2;
            self.function(ty)?;
            return Ok(Stmt::Empty);
        }
        if matches!(self.peek(0), Some(Tok::Ident(_)))
            && self.is_punct(1, "(")
            && self.is_function(ty)
        {
            self.function(ty)?;
            return Ok(Stmt::Empty);
        }
        let vars = self.declarators()?;
        Ok(Stmt::Decl(Box::new(VarDecl { pos, local: local || konst, ty, vars })))
    }

    /// At `name (`: is this a function (definition or prototype) rather than
    /// a struct declared with arguments?
    fn is_function(&self, ty: TypeId) -> bool {
        let mut depth = 0usize;
        let mut k = 1;
        while let Some(t) = self.peek(k) {
            match t {
                Tok::Punct("(") => depth += 1,
                Tok::Punct(")") => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        if self.is_punct(k + 1, "{") {
            return true;
        }
        // `name();` is a prototype, unless the type is a struct taking
        // parameters. Otherwise parameters look like `type name`; arguments
        // are expressions.
        if self.is_punct(2, ")") {
            let takes_args = matches!(
                &self.prog.ty(self.prog.resolve(ty)).kind,
                TypeKind::Struct { params, .. } if !params.is_empty()
            );
            return !takes_args;
        }
        if self.is_word(2, "void") && self.is_punct(3, ")") {
            return true;
        }
        match self.peek(2) {
            Some(Tok::Ident(w))
                if matches!(
                    &**w,
                    "local" | "const" | "struct" | "union" | "enum" | "unsigned" | "signed"
                ) =>
            {
                true
            }
            Some(Tok::Ident(_)) if self.is_type_name(2) => {
                let n = self.type_len(2).unwrap_or(1);
                matches!(self.peek(2 + n), Some(Tok::Ident(_)))
                    || self.is_punct(2 + n, "&")
                    || self.is_punct(2 + n, ",")
                    || self.is_punct(2 + n, ")")
                    || self.is_punct(2 + n, "[")
            }
            _ => false,
        }
    }

    fn function(&mut self, ret: TypeId) -> Result<(), Diag> {
        let pos = self.pos();
        let name = self.ident()?;
        self.expect("(")?;
        let mut params = Vec::new();
        if !(self.is_word(0, "void") && self.is_punct(1, ")")) && !self.is_punct(0, ")") {
            loop {
                while self.eat_word("local") || self.eat_word("const") {}
                let ty = self.type_spec()?;
                while self.eat_word("const") {}
                let by_ref = self.eat_punct("&");
                let pname = if matches!(self.peek(0), Some(Tok::Ident(_))) {
                    self.ident()?
                } else {
                    self.prog.syms.intern("")
                };
                let mut array = false;
                if self.eat_punct("[") {
                    array = true;
                    while !self.eat_punct("]") {
                        if self.i >= self.toks.len() {
                            return self.err("expected ']'");
                        }
                        self.i += 1;
                    }
                }
                params.push(Param { name: pname, ty, by_ref, array });
                if !self.eat_punct(",") {
                    break;
                }
            }
        } else if self.is_word(0, "void") {
            self.i += 1;
        }
        self.expect(")")?;
        let body = if self.eat_punct(";") {
            None
        } else {
            self.expect("{")?;
            Some(self.block_rest()?)
        };
        match self.prog.func_names.get(&name).copied() {
            Some(id) => {
                let f = &mut self.prog.funcs[id as usize];
                if body.is_some() {
                    f.body = body;
                    f.params = params;
                    f.ret = ret;
                    f.pos = pos;
                }
            }
            None => {
                self.prog.funcs.push(Func { name, ret, params, body, pos });
                self.prog.func_names.insert(name, (self.prog.funcs.len() - 1) as FuncId);
            }
        }
        Ok(())
    }

    fn declarators(&mut self) -> Result<Vec<Declarator>, Diag> {
        let mut out = Vec::new();
        loop {
            let pos = self.pos();
            let name = if self.is_punct(0, ":") { None } else { Some(self.ident()?) };
            let mut dim = self.dims()?;
            let args = if self.is_punct(0, "(") { Some(self.call_args()?) } else { None };
            if dim.is_none() {
                dim = self.dims()?;
            }
            let bits = if self.eat_punct(":") { Some(self.binary(7)?) } else { None };
            let mut attrs = if self.is_punct(0, "<") { self.attrs()? } else { Vec::new() };
            let init = if self.eat_punct("=") {
                Some(if self.is_punct(0, "{") { self.init_list()? } else { self.assign()? })
            } else {
                None
            };
            if self.is_punct(0, "<") {
                attrs.extend(self.attrs()?);
            }
            let attrs = self.attr_list(attrs);
            out.push(Declarator { name, dim, bits, args, attrs, init, pos });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(out)
    }

    /// Array dimensions: `[n]`, `[]`, or several (`[a][b]`, taken as one
    /// array of `a * b` elements).
    fn dims(&mut self) -> Result<Option<Option<Expr>>, Diag> {
        let mut dim: Option<Option<Expr>> = None;
        while self.eat_punct("[") {
            if self.eat_punct("]") {
                dim = Some(None);
                continue;
            }
            let e = self.expr()?;
            self.expect("]")?;
            dim = Some(Some(match dim {
                Some(Some(prev)) => Expr::Binary(BinOp::Mul, Box::new(prev), Box::new(e)),
                _ => e,
            }));
        }
        Ok(dim)
    }

    fn init_list(&mut self) -> Result<Expr, Diag> {
        self.expect("{")?;
        let mut items = Vec::new();
        while !self.eat_punct("}") {
            items.push(if self.is_punct(0, "{") { self.init_list()? } else { self.assign()? });
            if !self.eat_punct(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok(Expr::InitList(items))
    }

    fn attr_list(&mut self, attrs: Vec<Attr>) -> u32 {
        if attrs.is_empty() {
            return 0;
        }
        self.prog.attr_lists.push(attrs);
        (self.prog.attr_lists.len() - 1) as u32
    }

    fn attrs(&mut self) -> Result<Vec<Attr>, Diag> {
        self.expect("<")?;
        let mut out = Vec::new();
        loop {
            let name = self.ident()?;
            self.expect("=")?;
            let value = self.unary()?;
            out.push(Attr { name, value });
            if !self.eat_punct(",") {
                break;
            }
        }
        // `>>` closes both a nested template-ish value and the list; rare.
        if !self.eat_punct(">") {
            return self
                .err(format!("expected '>' to close the attributes, found {}", self.found()));
        }
        Ok(out)
    }

    fn typedef(&mut self) -> Result<(), Diag> {
        while self.eat_word("local") || self.eat_word("const") {}
        let target = self.type_spec()?;
        while self.eat_word("const") {}
        // `typedef struct { … };` with no name just defines the struct.
        if self.eat_punct(";") {
            return Ok(());
        }
        loop {
            let pos = self.pos();
            let name = self.ident()?;
            let dim = self.dims()?.map(|d| d.map(Box::new));
            let attrs = if self.is_punct(0, "<") { self.attrs()? } else { Vec::new() };
            let attrs = self.attr_list(attrs);
            let forward = self.prog.type_names.get(&name).copied().filter(|&old| {
                old != target
                    && matches!(self.prog.ty(old).kind, TypeKind::Struct { body: None, .. })
            });
            // `typedef struct X X;` names the struct again: nothing to add.
            if dim.is_none() && attrs == 0 && self.prog.ty(target).name == name {
                self.prog.type_names.insert(name, target);
            } else if let Some(old) = forward {
                // `struct X` was used before this typedef defined X: the
                // forward declaration becomes the typedef.
                let t = &mut self.prog.types[old as usize];
                t.kind = TypeKind::Alias { target, dim };
                t.attrs = attrs;
                t.pos = pos;
            } else {
                self.prog.types.push(TypeDef {
                    name,
                    kind: TypeKind::Alias { target, dim },
                    attrs,
                    pos,
                });
                let id = (self.prog.types.len() - 1) as TypeId;
                self.prog.type_names.insert(name, id);
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect(";")
    }

    fn anon_name(&mut self) -> Sym {
        self.prog.syms.intern("")
    }

    fn type_spec(&mut self) -> Result<TypeId, Diag> {
        while self.eat_word("const") {}
        let Some(Tok::Ident(w)) = self.peek(0) else {
            return self.err(format!("expected a type, found {}", self.found()));
        };
        let w = w.clone();
        match &*w {
            "struct" | "union" => self.struct_spec(),
            "enum" => self.enum_spec(),
            "unsigned" | "signed" | "long" | "short" => {
                let unsigned = &*w == "unsigned";
                if matches!(&*w, "unsigned" | "signed") {
                    self.i += 1;
                }
                let mut words: Vec<String> = Vec::new();
                while let Some(Tok::Ident(x)) = self.peek(0) {
                    if INT_WORDS.contains(&&**x) {
                        words.push(x.to_string());
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                let prim = if words.is_empty() {
                    // `unsigned int64`, `unsigned DWORD`: a named integer type.
                    match self.peek(0) {
                        Some(Tok::Ident(x)) if self.type_named(x).is_some() => {
                            let id = self.type_named(x).expect("checked");
                            self.i += 1;
                            self.prog.prim_of(id).unwrap_or(Prim::Int)
                        }
                        _ => Prim::Int,
                    }
                } else {
                    let longs = words.iter().filter(|x| *x == "long").count();
                    if words.iter().any(|x| x == "char") {
                        Prim::Char
                    } else if words.iter().any(|x| x == "short") {
                        Prim::Short
                    } else if words.iter().any(|x| x == "double" || x == "float") {
                        return Ok(self.prog.prim(Prim::Double));
                    } else if longs >= 2 {
                        Prim::Int64
                    } else {
                        Prim::Int
                    }
                };
                let prim = if unsigned {
                    prim.unsigned()
                } else if &*w == "signed" {
                    match prim {
                        Prim::UChar => Prim::Char,
                        Prim::UShort => Prim::Short,
                        Prim::UInt => Prim::Int,
                        Prim::UInt64 => Prim::Int64,
                        p => p,
                    }
                } else {
                    prim
                };
                Ok(self.prog.prim(prim))
            }
            _ => match self.type_named(&w) {
                Some(id) => {
                    self.i += 1;
                    Ok(id)
                }
                None => self.err(format!("unknown type {}", self.found())),
            },
        }
    }

    fn struct_spec(&mut self) -> Result<TypeId, Diag> {
        let union = self.is_word(0, "union");
        self.i += 1;
        let pos = self.pos();
        let name =
            if matches!(self.peek(0), Some(Tok::Ident(_))) { Some(self.ident()?) } else { None };
        let has_params = self.is_punct(0, "(");
        let defines = has_params || self.is_punct(0, "{");
        if !defines {
            let Some(name) = name else { return self.err("expected a struct name or body") };
            if let Some(&id) = self.prog.type_names.get(&name) {
                return Ok(id);
            }
            let id =
                self.add_type(name, TypeKind::Struct { union, params: Vec::new(), body: None });
            self.prog.type_names.insert(name, id);
            return Ok(id);
        }
        let sym = match name {
            Some(n) => n,
            None => self.anon_name(),
        };
        let id = match name.and_then(|n| self.prog.type_names.get(&n).copied()) {
            Some(id) if matches!(self.prog.ty(id).kind, TypeKind::Struct { .. }) => id,
            _ => {
                let id =
                    self.add_type(sym, TypeKind::Struct { union, params: Vec::new(), body: None });
                if let Some(n) = name {
                    self.prog.type_names.insert(n, id);
                }
                id
            }
        };
        self.prog.types[id as usize].pos = pos;
        let mut params = Vec::new();
        if has_params {
            self.i += 1;
            if !self.is_punct(0, ")") && !(self.is_word(0, "void") && self.is_punct(1, ")")) {
                loop {
                    while self.eat_word("local") || self.eat_word("const") {}
                    let ty = self.type_spec()?;
                    let by_ref = self.eat_punct("&");
                    let pname = self.ident()?;
                    let array = self.eat_punct("[");
                    if array {
                        self.expect("]")?;
                    }
                    params.push(Param { name: pname, ty, by_ref, array });
                    if !self.eat_punct(",") {
                        break;
                    }
                }
            } else if self.is_word(0, "void") {
                self.i += 1;
            }
            self.expect(")")?;
        }
        if has_params && !self.is_punct(0, "{") {
            // A forward declaration with parameters.
            if let TypeKind::Struct { params: p, .. } = &mut self.prog.types[id as usize].kind {
                *p = params;
            }
            return Ok(id);
        }
        self.expect("{")?;
        let body = self.block_rest()?;
        self.prog.types[id as usize].kind = TypeKind::Struct { union, params, body: Some(body) };
        Ok(id)
    }

    fn enum_spec(&mut self) -> Result<TypeId, Diag> {
        self.i += 1;
        let pos = self.pos();
        let base = if self.eat_punct("<") {
            let b = self.type_spec()?;
            self.expect(">")?;
            b
        } else {
            self.prog.prim(Prim::Int)
        };
        let name =
            if matches!(self.peek(0), Some(Tok::Ident(_))) { Some(self.ident()?) } else { None };
        if !self.is_punct(0, "{") {
            let Some(name) = name else { return self.err("expected an enum name or body") };
            if let Some(&id) = self.prog.type_names.get(&name) {
                return Ok(id);
            }
            let id = self.add_type(name, TypeKind::Enum { base, consts: Vec::new() });
            self.prog.type_names.insert(name, id);
            return Ok(id);
        }
        self.i += 1;
        let sym = match name {
            Some(n) => n,
            None => self.anon_name(),
        };
        let id = self.add_type(sym, TypeKind::Enum { base, consts: Vec::new() });
        self.prog.types[id as usize].pos = pos;
        if let Some(n) = name {
            self.prog.type_names.insert(n, id);
        }
        let mut consts = Vec::new();
        while !self.eat_punct("}") {
            let c = self.ident()?;
            let v = if self.eat_punct("=") { Some(self.cond()?) } else { None };
            self.prog.enum_consts.insert(c, (id, consts.len()));
            consts.push((c, v));
            if !self.eat_punct(",") {
                self.expect("}")?;
                break;
            }
        }
        self.prog.types[id as usize].kind = TypeKind::Enum { base, consts };
        self.prog.enums.push(id);
        Ok(id)
    }

    // ---- expressions -----------------------------------------------------

    fn expr(&mut self) -> Result<Expr, Diag> {
        let mut e = self.assign()?;
        while self.eat_punct(",") {
            let r = self.assign()?;
            e = Expr::Comma(Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn assign(&mut self) -> Result<Expr, Diag> {
        let lhs = self.cond()?;
        if let Some(op) = self.peek(0).and_then(assignop) {
            self.i += 1;
            let rhs = if self.is_punct(0, "{") { self.init_list()? } else { self.assign()? };
            return Ok(Expr::Assign(op, Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn cond(&mut self) -> Result<Expr, Diag> {
        let c = self.binary(0)?;
        if self.eat_punct("?") {
            let a = self.expr()?;
            self.expect(":")?;
            let b = self.cond()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    /// Binary operators binding tighter than `min`.
    fn binary(&mut self, min: u8) -> Result<Expr, Diag> {
        let mut lhs = self.unary()?;
        while let Some((op, prec)) = self.peek(0).and_then(binop) {
            if prec <= min {
                break;
            }
            self.i += 1;
            let rhs = self.binary(prec)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn paren_expr(&mut self) -> Result<Expr, Diag> {
        self.expect("(")?;
        let e = self.expr()?;
        self.expect(")")?;
        Ok(e)
    }

    fn unary(&mut self) -> Result<Expr, Diag> {
        let op = match self.peek(0) {
            Some(Tok::Punct("-")) => Some(UnOp::Neg),
            Some(Tok::Punct("+")) => Some(UnOp::Plus),
            Some(Tok::Punct("!")) => Some(UnOp::Not),
            Some(Tok::Punct("~")) => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.i += 1;
            return Ok(Expr::Unary(op, Box::new(self.unary()?)));
        }
        if self.is_punct(0, "++") || self.is_punct(0, "--") {
            let inc = self.is_punct(0, "++");
            self.i += 1;
            return Ok(Expr::IncDec { pre: true, inc, e: Box::new(self.unary()?) });
        }
        if self.is_punct(0, "(")
            && let Some(n) = self.type_len(1)
            && self.is_punct(1 + n, ")")
        {
            self.i += 1;
            let ty = self.type_spec()?;
            self.expect(")")?;
            return Ok(Expr::Cast(ty, Box::new(self.unary()?)));
        }
        if self.is_word(0, "sizeof") {
            self.i += 1;
            if self.is_punct(0, "(")
                && let Some(n) = self.type_len(1)
                && self.is_punct(1 + n, ")")
            {
                self.i += 1;
                let ty = self.type_spec()?;
                self.expect(")")?;
                return Ok(Expr::SizeofType(ty));
            }
            return Ok(Expr::SizeofValue(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn call_args(&mut self) -> Result<Vec<Expr>, Diag> {
        self.expect("(")?;
        let mut args = Vec::new();
        if !self.eat_punct(")") {
            loop {
                args.push(self.assign()?);
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect(")")?;
        }
        Ok(args)
    }

    fn postfix(&mut self) -> Result<Expr, Diag> {
        let mut e = self.primary()?;
        loop {
            if self.eat_punct("[") {
                let idx = self.expr()?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(idx));
            } else if self.eat_punct(".") {
                let m = self.ident()?;
                e = Expr::Member(Box::new(e), m);
            } else if self.is_punct(0, "(") {
                let Expr::Ident(name, pos) = e else {
                    return self.err("only a named function can be called");
                };
                let args = self.call_args()?;
                e = Expr::Call(name, args, pos);
            } else if self.is_punct(0, "++") || self.is_punct(0, "--") {
                let inc = self.is_punct(0, "++");
                self.i += 1;
                e = Expr::IncDec { pre: false, inc, e: Box::new(e) };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, Diag> {
        let pos = self.pos();
        let Some(t) = self.peek(0).cloned() else { return self.err("unexpected end of file") };
        self.i += 1;
        Ok(match t {
            Tok::Int(v, u, l) => Expr::Int(v, u, l || v > u32::MAX as u64),
            Tok::Float(v, f) => Expr::Float(v, f),
            Tok::Char(v) | Tok::WChar(v) => Expr::Int(v, false, v > u32::MAX as u64),
            Tok::Str(mut s) => {
                while let Some(Tok::Str(more)) = self.peek(0) {
                    s.extend_from_slice(more);
                    self.i += 1;
                }
                Expr::Str(s)
            }
            Tok::WStr(mut s) => {
                while let Some(Tok::WStr(more) | Tok::Str(more)) = self.peek(0) {
                    s.extend_from_slice(more);
                    self.i += 1;
                }
                Expr::WStr(s)
            }
            Tok::Ident(w) if &*w == "this" => Expr::This,
            Tok::Ident(w) if &*w == "startof" && self.is_punct(0, "(") => {
                Expr::Startof(Box::new(self.paren_expr()?))
            }
            Tok::Ident(w) if &*w == "exists" && self.is_punct(0, "(") => {
                Expr::Exists(Box::new(self.paren_expr()?))
            }
            Tok::Ident(w) if &*w == "parentof" && self.is_punct(0, "(") => {
                Expr::Parentof(Box::new(self.paren_expr()?))
            }
            Tok::Ident(w) if &*w == "function_exists" && self.is_punct(0, "(") => {
                self.expect("(")?;
                let f = self.ident()?;
                self.expect(")")?;
                Expr::FunctionExists(f)
            }
            Tok::Ident(w) => Expr::Ident(self.prog.syms.intern(&w), pos),
            Tok::Punct("(") => {
                let e = self.expr()?;
                self.expect(")")?;
                e
            }
            Tok::Punct("{") => {
                self.i -= 1;
                self.init_list()?
            }
            other => {
                self.i -= 1;
                return self.err(format!("unexpected {other}"));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::preproc::{Source, preprocess};
    use super::*;

    pub(crate) fn parse_str(src: &str) -> Result<Program, Diag> {
        let root = Source { name: "t.bt".into(), path: None, text: src.as_bytes().to_vec() };
        parse(preprocess(root, &mut |_, _| None)?)
    }

    #[test]
    fn declarations_and_expressions_are_told_apart() {
        let p = parse_str(
            "typedef uint DW; DW a; DW : 4; a = 5; local int b = a * 2; \
             struct S (int n) { uchar d[n]; }; S s(3); \
             void f(int &x, char s[]) { x = 1; } f(b, \"x\"); \
             int g(); S t;",
        )
        .unwrap();
        let kinds: Vec<&str> = p
            .body
            .iter()
            .map(|s| match s {
                Stmt::Decl(_) => "decl",
                Stmt::Expr(..) => "expr",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["decl", "decl", "expr", "decl", "decl", "expr", "decl"]);
        assert!(p.func_names.contains_key(&p.syms.lookup("f").unwrap()));
        assert!(p.func_names.contains_key(&p.syms.lookup("g").unwrap()));
    }

    #[test]
    fn casts_sizeof_attributes_and_enums() {
        let p = parse_str(
            "enum <ushort> E { A, B = 5, C } e : 4 <format=hex, comment=\"x\">; \
             local int x = (int)sizeof(ushort) + sizeof(e) + (A); \
             typedef struct { int a; } T <read=Str(\"%d\", this.a), size=(4)>; \
             struct NODE; struct NODE { int n; if (n) NODE child; } root; \
             typedef enum <uchar> { X, Y } TE; TE te <bgcolor=cRed>;",
        )
        .unwrap();
        assert_eq!(p.enums.len(), 2);
        let t = p.type_names[&p.syms.lookup("T").unwrap()];
        assert_eq!(p.attrs(p.ty(t).attrs).len(), 2);
        let node = p.type_names[&p.syms.lookup("NODE").unwrap()];
        assert!(matches!(&p.ty(node).kind, TypeKind::Struct { body: Some(b), .. } if b.len() == 2));
    }

    #[test]
    fn unsigned_combinations_and_control_flow() {
        let p = parse_str(
            "unsigned long long a; unsigned short int b; signed char c; unsigned d; long double e; \
             local int i; for (i = 0, a = 1; i < 3; i++) { continue; } \
             for (local int j = 0; j < 2; j++) ; \
             switch (i) { case 1: case 'A': break; default: i = 2; } \
             do { i--; } while (i > 0); \
             while (!FEof()) { if (i) break; else i = i ? 1 : 0; }",
        )
        .unwrap();
        let Stmt::Decl(d) = &p.body[0] else { panic!() };
        assert_eq!(p.prim_of(d.ty), Some(Prim::UInt64));
        let Stmt::Decl(d) = &p.body[1] else { panic!() };
        assert_eq!(p.prim_of(d.ty), Some(Prim::UShort));
        let Stmt::Decl(d) = &p.body[3] else { panic!() };
        assert_eq!(p.prim_of(d.ty), Some(Prim::UInt));
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = parse_str("int a\nint b;").unwrap_err();
        assert_eq!(e.pos.line, 2);
        assert!(parse_str("foo bar;").is_err());
    }
}
