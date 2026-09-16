//! The parsed program: statements, expressions, and the type table.
//!
//! Types are compile-time in 010 Editor — a `typedef` or `struct` anywhere,
//! even inside a struct body, names a type for the whole template — so the
//! parser registers them as it goes and statements refer to them by
//! [`TypeId`].

use super::lex::Pos;
use std::collections::HashMap;

/// An interned identifier.
pub type Sym = u32;
pub type TypeId = u32;
pub type FuncId = u32;

#[derive(Default, Debug)]
pub struct Interner {
    names: Vec<Box<str>>,
    ids: HashMap<Box<str>, Sym>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = self.names.len() as Sym;
        self.names.push(s.into());
        self.ids.insert(s.into(), id);
        id
    }

    pub fn lookup(&self, s: &str) -> Option<Sym> {
        self.ids.get(s).copied()
    }

    pub fn name(&self, id: Sym) -> &str {
        self.names.get(id as usize).map(|s| &**s).unwrap_or("?")
    }
}

/// The built-in scalar types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prim {
    Char,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Int64,
    UInt64,
    HFloat,
    Float,
    Double,
    /// A NUL-terminated 8-bit string.
    Str,
    /// A NUL-terminated 16-bit string.
    WStr,
    WChar,
    DosDate,
    DosTime,
    FileTime,
    OleTime,
    TimeT,
    Time64T,
    Void,
    /// A one-byte disassembly unit (010 Editor disassembles; this doesn't).
    Opcode,
}

impl Prim {
    /// Bytes a value of this type occupies (0 for strings and void).
    pub fn size(self) -> u64 {
        use Prim::*;
        match self {
            Char | UChar | Opcode => 1,
            Short | UShort | WChar | HFloat | DosDate | DosTime => 2,
            Int | UInt | Float | TimeT => 4,
            Int64 | UInt64 | Double | FileTime | OleTime | Time64T => 8,
            Str | WStr | Void => 0,
        }
    }

    pub fn is_int(self) -> bool {
        use Prim::*;
        matches!(
            self,
            Char | UChar
                | Short
                | UShort
                | Int
                | UInt
                | Int64
                | UInt64
                | WChar
                | DosDate
                | DosTime
                | FileTime
                | TimeT
                | Time64T
                | Opcode
        )
    }

    pub fn is_float(self) -> bool {
        matches!(self, Prim::HFloat | Prim::Float | Prim::Double | Prim::OleTime)
    }

    pub fn is_signed(self) -> bool {
        use Prim::*;
        matches!(self, Char | Short | Int | Int64 | TimeT | Time64T)
    }

    /// The unsigned type of the same width.
    pub fn unsigned(self) -> Prim {
        use Prim::*;
        match self {
            Char => UChar,
            Short => UShort,
            Int => UInt,
            Int64 => UInt64,
            p => p,
        }
    }
}

/// Every built-in type name and its type.
pub const PRIM_NAMES: &[(&str, Prim)] = &[
    ("char", Prim::Char),
    ("byte", Prim::Char),
    ("CHAR", Prim::Char),
    ("BYTE", Prim::Char),
    ("int8", Prim::Char),
    ("INT8", Prim::Char),
    ("uchar", Prim::UChar),
    ("ubyte", Prim::UChar),
    ("UCHAR", Prim::UChar),
    ("UBYTE", Prim::UChar),
    ("uint8", Prim::UChar),
    ("UINT8", Prim::UChar),
    ("short", Prim::Short),
    ("int16", Prim::Short),
    ("SHORT", Prim::Short),
    ("INT16", Prim::Short),
    ("ushort", Prim::UShort),
    ("uint16", Prim::UShort),
    ("USHORT", Prim::UShort),
    ("UINT16", Prim::UShort),
    ("WORD", Prim::UShort),
    ("int", Prim::Int),
    ("int32", Prim::Int),
    ("long", Prim::Int),
    ("INT", Prim::Int),
    ("INT32", Prim::Int),
    ("LONG", Prim::Int),
    ("uint", Prim::UInt),
    ("uint32", Prim::UInt),
    ("ulong", Prim::UInt),
    ("UINT", Prim::UInt),
    ("UINT32", Prim::UInt),
    ("ULONG", Prim::UInt),
    ("DWORD", Prim::UInt),
    ("int64", Prim::Int64),
    ("quad", Prim::Int64),
    ("QUAD", Prim::Int64),
    ("INT64", Prim::Int64),
    ("__int64", Prim::Int64),
    ("uint64", Prim::UInt64),
    ("uquad", Prim::UInt64),
    ("UQUAD", Prim::UInt64),
    ("UINT64", Prim::UInt64),
    ("QWORD", Prim::UInt64),
    ("__uint64", Prim::UInt64),
    ("hfloat", Prim::HFloat),
    ("HFLOAT", Prim::HFloat),
    ("float", Prim::Float),
    ("FLOAT", Prim::Float),
    ("double", Prim::Double),
    ("DOUBLE", Prim::Double),
    ("string", Prim::Str),
    ("wstring", Prim::WStr),
    ("wchar_t", Prim::WChar),
    ("DOSDATE", Prim::DosDate),
    ("DOSTIME", Prim::DosTime),
    ("FILETIME", Prim::FileTime),
    ("OLETIME", Prim::OleTime),
    ("time_t", Prim::TimeT),
    ("time64_t", Prim::Time64T),
    ("void", Prim::Void),
    ("Opcode", Prim::Opcode),
];

#[derive(Debug)]
pub struct Param {
    pub name: Sym,
    pub ty: TypeId,
    pub by_ref: bool,
    pub array: bool,
}

#[derive(Debug)]
pub enum TypeKind {
    Prim(Prim),
    Enum {
        base: TypeId,
        consts: Vec<(Sym, Option<Expr>)>,
    },
    Struct {
        union: bool,
        params: Vec<Param>,
        /// `None` while only forward-declared.
        body: Option<Vec<Stmt>>,
    },
    /// A typedef: another type, optionally as an array (`typedef char s[4]`,
    /// or `[]` for an open one).
    Alias {
        target: TypeId,
        dim: Option<Option<Box<Expr>>>,
    },
}

#[derive(Debug)]
pub struct TypeDef {
    pub name: Sym,
    pub kind: TypeKind,
    /// Attributes given where the type was defined (`typedef … T <read=…>`),
    /// as an index into [`Program::attr_lists`].
    pub attrs: u32,
    pub pos: Pos,
}

#[derive(Debug)]
pub struct Attr {
    pub name: Sym,
    pub value: Expr,
}

#[derive(Debug)]
pub struct Declarator {
    /// `None` for an unnamed bitfield, which only skips bits.
    pub name: Option<Sym>,
    /// `[expr]`, or `[]` (`Some(None)`).
    pub dim: Option<Option<Expr>>,
    pub bits: Option<Expr>,
    /// Arguments of a struct with parameters.
    pub args: Option<Vec<Expr>>,
    /// An index into [`Program::attr_lists`] (0: none).
    pub attrs: u32,
    pub init: Option<Expr>,
    pub pos: Pos,
}

#[derive(Debug)]
pub struct VarDecl {
    pub pos: Pos,
    pub local: bool,
    pub ty: TypeId,
    pub vars: Vec<Declarator>,
}

#[derive(Debug)]
pub enum Stmt {
    Expr(Pos, Expr),
    Decl(Box<VarDecl>),
    Block(Vec<Stmt>),
    /// `if … else if … else`: the arms in order, then the final `else`.
    If(Pos, Vec<(Expr, Stmt)>, Option<Box<Stmt>>),
    While(Pos, Expr, Box<Stmt>),
    DoWhile(Pos, Box<Stmt>, Expr),
    For(Pos, Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    /// The body holds the `case` / `default` labels among its statements.
    Switch(Pos, Expr, Vec<Stmt>),
    Case(Pos, Expr),
    Default,
    Break,
    Continue,
    Return(Pos, Option<Expr>),
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

#[derive(Debug)]
pub enum Expr {
    /// An integer literal: value, unsigned, 64-bit.
    Int(u64, bool, bool),
    Float(f64, bool),
    Str(Vec<u8>),
    WStr(Vec<u8>),
    Ident(Sym, Pos),
    This,
    Unary(UnOp, Box<Expr>),
    IncDec {
        pre: bool,
        inc: bool,
        e: Box<Expr>,
    },
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Sym, Vec<Expr>, Pos),
    Index(Box<Expr>, Box<Expr>),
    Member(Box<Expr>, Sym),
    Cast(TypeId, Box<Expr>),
    SizeofType(TypeId),
    SizeofValue(Box<Expr>),
    Startof(Box<Expr>),
    Exists(Box<Expr>),
    FunctionExists(Sym),
    Parentof(Box<Expr>),
    InitList(Vec<Expr>),
    Comma(Box<Expr>, Box<Expr>),
}

#[derive(Debug)]
pub struct Func {
    pub name: Sym,
    pub ret: TypeId,
    pub params: Vec<Param>,
    /// `None` for a prototype never defined (or an external DLL function).
    pub body: Option<Vec<Stmt>>,
    pub pos: Pos,
}

#[derive(Debug, Default)]
pub struct Program {
    pub syms: Interner,
    pub types: Vec<TypeDef>,
    pub type_names: HashMap<Sym, TypeId>,
    pub funcs: Vec<Func>,
    pub func_names: HashMap<Sym, FuncId>,
    /// Every enum constant: its enum and index in that enum's list.
    pub enum_consts: HashMap<Sym, (TypeId, usize)>,
    /// Every enum type, in definition order (their constants are evaluated
    /// in this order before the template runs).
    pub enums: Vec<TypeId>,
    pub body: Vec<Stmt>,
    /// Display names of the source files, indexed by `Pos::file`.
    pub files: Vec<String>,
    pub prim_ids: HashMap<Prim, TypeId>,
    /// Every attribute list (`<…>`); list 0 is empty.
    pub attr_lists: Vec<Vec<Attr>>,
    /// `#warning` messages, shown in the output when the template runs.
    pub warnings: Vec<String>,
}

impl Program {
    pub fn ty(&self, id: TypeId) -> &TypeDef {
        &self.types[id as usize]
    }

    /// The type behind any chain of array-less typedefs.
    pub fn resolve(&self, mut id: TypeId) -> TypeId {
        for _ in 0..64 {
            match &self.ty(id).kind {
                TypeKind::Alias { target, dim: None } => id = *target,
                _ => break,
            }
        }
        id
    }

    pub fn prim(&self, p: Prim) -> TypeId {
        self.prim_ids[&p]
    }

    /// The scalar type behind `id`, if it is one (enums give their base).
    pub fn prim_of(&self, id: TypeId) -> Option<Prim> {
        match &self.ty(self.resolve(id)).kind {
            TypeKind::Prim(p) => Some(*p),
            TypeKind::Enum { base, .. } => self.prim_of(*base),
            _ => None,
        }
    }

    pub fn attrs(&self, list: u32) -> &[Attr] {
        self.attr_lists.get(list as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn name(&self, s: Sym) -> &str {
        self.syms.name(s)
    }

    pub fn pos_text(&self, pos: Pos) -> String {
        let file = self.files.get(pos.file as usize).map(String::as_str).unwrap_or("?");
        format!("{file}:{}", pos.line)
    }
}
