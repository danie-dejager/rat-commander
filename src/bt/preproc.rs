//! The preprocessor: `#define` (object-like — 010 Editor has no macros with
//! arguments), `#undef`, `#ifdef` / `#ifndef` / `#else` / `#endif`, a constant
//! `#if` / `#elif`, `#include`, `#warning` and `#error`. It works on tokens,
//! so text inside strings and comments is never substituted.

use super::lex::{self, Diag, Pos, Tok, Token};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One source file handed to the preprocessor.
pub struct Source {
    /// A display name for messages.
    pub name: String,
    /// Where it was read from, if from disk: includes resolve relative to it.
    pub path: Option<PathBuf>,
    pub text: Vec<u8>,
}

/// Finds an included file: `(name, directory of the including file)`.
pub type Loader<'a> = dyn FnMut(&str, Option<&Path>) -> Option<Source> + 'a;

pub struct Preprocessed {
    pub tokens: Vec<Token>,
    /// Display names, indexed by `Pos::file`.
    pub files: Vec<String>,
    /// Paths of every file read, the root included.
    pub deps: Vec<PathBuf>,
    pub warnings: Vec<Diag>,
}

const MAX_INCLUDE_DEPTH: usize = 32;

pub fn preprocess(root: Source, loader: &mut Loader<'_>) -> Result<Preprocessed, Diag> {
    let mut pp = Pp {
        defines: HashMap::new(),
        out: Vec::new(),
        files: Vec::new(),
        deps: Vec::new(),
        warnings: Vec::new(),
        include_stack: Vec::new(),
    };
    for d in ["_010EDITOR", "_010_64BIT", "_RAT_COMMANDER"] {
        pp.defines.insert(d.to_string(), Vec::new());
    }
    let os = if cfg!(windows) {
        "_010_WIN"
    } else if cfg!(target_os = "macos") {
        "_010_MAC"
    } else {
        "_010_LINUX"
    };
    pp.defines.insert(os.to_string(), Vec::new());
    pp.file(root, loader)?;
    Ok(Preprocessed { tokens: pp.out, files: pp.files, deps: pp.deps, warnings: pp.warnings })
}

struct Pp {
    defines: HashMap<String, Vec<Token>>,
    out: Vec<Token>,
    files: Vec<String>,
    deps: Vec<PathBuf>,
    warnings: Vec<Diag>,
    include_stack: Vec<String>,
}

/// One level of `#if` nesting.
struct Cond {
    /// Whether the enclosing text is live.
    outer: bool,
    /// Whether the current branch is live.
    live: bool,
    /// Whether some branch of this `#if` has been taken already.
    taken: bool,
}

impl Pp {
    fn file(&mut self, src: Source, loader: &mut Loader<'_>) -> Result<(), Diag> {
        if self.include_stack.len() >= MAX_INCLUDE_DEPTH {
            return Err(Diag { pos: Pos::default(), msg: "includes nested too deeply".into() });
        }
        let key = src.path.as_ref().map(|p| p.display().to_string()).unwrap_or(src.name.clone());
        if self.include_stack.contains(&key) {
            return Ok(());
        }
        let id = self.files.len() as u16;
        self.files.push(src.name.clone());
        if let Some(p) = &src.path {
            self.deps.push(p.clone());
        }
        self.include_stack.push(key);
        let dir = src.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        let toks = lex::lex(&src.text, id)?;
        let mut conds: Vec<Cond> = Vec::new();
        let live = |conds: &[Cond]| conds.last().is_none_or(|c| c.live);
        let mut i = 0;
        while i < toks.len() {
            let t = &toks[i];
            let Tok::Directive(name) = &t.tok else {
                if live(&conds) {
                    self.expand(t, &mut Vec::new())?;
                }
                i += 1;
                continue;
            };
            let line = t.lline;
            let pos = t.pos;
            let mut j = i + 1;
            while j < toks.len()
                && toks[j].lline == line
                && !matches!(toks[j].tok, Tok::Directive(_))
            {
                j += 1;
            }
            let args = &toks[i + 1..j];
            i = j;
            let err = |msg: String| Diag { pos, msg };
            match &**name {
                "ifdef" | "ifndef" => {
                    let defined = match args.first().map(|a| &a.tok) {
                        Some(Tok::Ident(n)) => self.defines.contains_key(&**n),
                        _ => return Err(err(format!("#{name} needs a name"))),
                    };
                    let outer = live(&conds);
                    let yes = outer && (defined == (&**name == "ifdef"));
                    conds.push(Cond { outer, live: yes, taken: yes });
                }
                "if" => {
                    let outer = live(&conds);
                    let yes = outer && self.eval_if(args)? != 0;
                    conds.push(Cond { outer, live: yes, taken: yes });
                }
                "elif" => {
                    let Some(top) = conds.last() else {
                        return Err(err("#elif without #if".into()));
                    };
                    let (outer, taken) = (top.outer, top.taken);
                    let yes = outer && !taken && self.eval_if(args)? != 0;
                    let top = conds.last_mut().expect("checked");
                    top.live = yes;
                    top.taken |= yes;
                }
                "else" => {
                    let Some(top) = conds.last_mut() else {
                        return Err(err("#else without #if".into()));
                    };
                    top.live = top.outer && !top.taken;
                    top.taken = true;
                }
                "endif" => {
                    if conds.pop().is_none() {
                        return Err(err("#endif without #if".into()));
                    }
                }
                _ if !live(&conds) => {}
                "define" => {
                    let Some(Tok::Ident(n)) = args.first().map(|a| &a.tok) else {
                        return Err(err("#define needs a name".into()));
                    };
                    self.defines.insert(n.to_string(), args[1..].to_vec());
                }
                "undef" => {
                    if let Some(Tok::Ident(n)) = args.first().map(|a| &a.tok) {
                        self.defines.remove(&**n);
                    }
                }
                "include" => {
                    let raw = match args.first().map(|a| &a.tok) {
                        Some(Tok::Raw(r)) => r.trim(),
                        _ => "",
                    };
                    let target = raw
                        .strip_prefix('"')
                        .and_then(|r| r.split('"').next())
                        .or_else(|| raw.strip_prefix('<').and_then(|r| r.split('>').next()))
                        .unwrap_or("")
                        .trim();
                    if target.is_empty() {
                        return Err(err("#include needs a file name".into()));
                    }
                    let Some(inc) = loader(target, dir.as_deref()) else {
                        return Err(err(format!("include file '{target}' not found")));
                    };
                    self.file(inc, loader)?;
                }
                "warning" | "error" => {
                    let msg = match args.first().map(|a| &a.tok) {
                        Some(Tok::Raw(r)) => r.trim().trim_matches('"').to_string(),
                        _ => String::new(),
                    };
                    if &**name == "error" {
                        return Err(err(msg));
                    }
                    self.warnings.push(err(msg));
                }
                // #pragma, #link/#endlink (external DLL functions, whose
                // prototypes then parse as ordinary ones), and anything else.
                _ => {}
            }
        }
        if !conds.is_empty() {
            return Err(Diag {
                pos: toks.last().map(|t| t.pos).unwrap_or_default(),
                msg: "#if without #endif".into(),
            });
        }
        self.include_stack.pop();
        Ok(())
    }

    /// Push `t`, substituting a defined name (recursively, but never a name
    /// inside its own expansion).
    fn expand(&mut self, t: &Token, active: &mut Vec<String>) -> Result<(), Diag> {
        if let Tok::Ident(n) = &t.tok
            && !active.iter().any(|a| **a == **n)
            && let Some(body) = self.defines.get(&**n).cloned()
        {
            if active.len() > 64 {
                return Err(Diag { pos: t.pos, msg: format!("'{n}' expands too deeply") });
            }
            active.push(n.to_string());
            for b in &body {
                let mut b = b.clone();
                b.pos = t.pos;
                self.expand(&b, active)?;
            }
            active.pop();
            return Ok(());
        }
        self.out.push(t.clone());
        Ok(())
    }

    /// Evaluate an `#if` condition: integers, `defined(NAME)`, and C operators.
    fn eval_if(&mut self, args: &[Token]) -> Result<i64, Diag> {
        let pos = args.first().map(|a| a.pos).unwrap_or_default();
        let mut toks = Vec::new();
        let mut k = 0;
        while k < args.len() {
            if let Tok::Ident(n) = &args[k].tok
                && &**n == "defined"
            {
                let (name, skip) =
                    match (args.get(k + 1).map(|a| &a.tok), args.get(k + 2).map(|a| &a.tok)) {
                        (Some(Tok::Punct("(")), Some(Tok::Ident(x))) => (x.to_string(), 4),
                        (Some(Tok::Ident(x)), _) => (x.to_string(), 2),
                        _ => return Err(Diag { pos, msg: "bad defined()".into() }),
                    };
                toks.push(Tok::Int(self.defines.contains_key(&name) as u64, false, false));
                k += skip;
                continue;
            }
            let saved = std::mem::take(&mut self.out);
            self.expand(&args[k], &mut Vec::new())?;
            let expanded = std::mem::replace(&mut self.out, saved);
            toks.extend(expanded.into_iter().map(|t| t.tok));
            k += 1;
        }
        let mut p = 0;
        let v = cexpr(&toks, &mut p, 0).ok_or(Diag { pos, msg: "bad #if expression".into() })?;
        Ok(v)
    }
}

/// A tiny precedence-climbing evaluator for `#if`.
fn cexpr(t: &[Tok], p: &mut usize, min: u8) -> Option<i64> {
    let mut lhs = match t.get(*p)? {
        Tok::Int(v, ..) | Tok::Char(v) => {
            *p += 1;
            *v as i64
        }
        Tok::Ident(_) => {
            *p += 1;
            0
        }
        Tok::Punct("(") => {
            *p += 1;
            let v = cexpr(t, p, 0)?;
            if t.get(*p) != Some(&Tok::Punct(")")) {
                return None;
            }
            *p += 1;
            v
        }
        Tok::Punct("!") => {
            *p += 1;
            (cexpr(t, p, 11)? == 0) as i64
        }
        Tok::Punct("-") => {
            *p += 1;
            cexpr(t, p, 11)?.wrapping_neg()
        }
        Tok::Punct("~") => {
            *p += 1;
            !cexpr(t, p, 11)?
        }
        _ => return None,
    };
    while let Some(Tok::Punct(op)) = t.get(*p) {
        let prec = match *op {
            "||" => 1,
            "&&" => 2,
            "|" => 3,
            "^" => 4,
            "&" => 5,
            "==" | "!=" => 6,
            "<" | ">" | "<=" | ">=" => 7,
            "<<" | ">>" => 8,
            "+" | "-" => 9,
            "*" | "/" | "%" => 10,
            "?" if min == 0 => {
                *p += 1;
                let a = cexpr(t, p, 0)?;
                if t.get(*p) != Some(&Tok::Punct(":")) {
                    return None;
                }
                *p += 1;
                let b = cexpr(t, p, 0)?;
                lhs = if lhs != 0 { a } else { b };
                continue;
            }
            _ => break,
        };
        if prec <= min {
            break;
        }
        *p += 1;
        let rhs = cexpr(t, p, prec)?;
        lhs = match *op {
            "||" => (lhs != 0 || rhs != 0) as i64,
            "&&" => (lhs != 0 && rhs != 0) as i64,
            "|" => lhs | rhs,
            "^" => lhs ^ rhs,
            "&" => lhs & rhs,
            "==" => (lhs == rhs) as i64,
            "!=" => (lhs != rhs) as i64,
            "<" => (lhs < rhs) as i64,
            ">" => (lhs > rhs) as i64,
            "<=" => (lhs <= rhs) as i64,
            ">=" => (lhs >= rhs) as i64,
            "<<" => lhs.wrapping_shl(rhs as u32),
            ">>" => lhs.wrapping_shr(rhs as u32),
            "+" => lhs.wrapping_add(rhs),
            "-" => lhs.wrapping_sub(rhs),
            "*" => lhs.wrapping_mul(rhs),
            "/" => lhs.checked_div(rhs)?,
            _ => lhs.checked_rem(rhs)?,
        };
    }
    Some(lhs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, files: &[(&str, &str)]) -> Result<Vec<Tok>, Diag> {
        let files: Vec<(String, String)> =
            files.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let mut loader = |name: &str, _: Option<&Path>| {
            files.iter().find(|(n, _)| n == name).map(|(n, t)| Source {
                name: n.clone(),
                path: None,
                text: t.as_bytes().to_vec(),
            })
        };
        let root = Source { name: "root".into(), path: None, text: src.as_bytes().to_vec() };
        preprocess(root, &mut loader).map(|p| p.tokens.into_iter().map(|t| t.tok).collect())
    }

    fn idents(toks: &[Tok]) -> Vec<String> {
        toks.iter()
            .map(|t| match t {
                Tok::Ident(s) => s.to_string(),
                Tok::Int(v, ..) => v.to_string(),
                other => other.to_string(),
            })
            .collect()
    }

    #[test]
    fn defines_substitute_recursively_but_not_in_strings() {
        let t = run("#define A 1\n#define B (A+A)\nB \"A\"\n#undef A\nA", &[]).unwrap();
        assert_eq!(idents(&t), vec!["'('", "1", "'+'", "1", "')'", "\"A\"", "A"]);
        // A self-referential define stops expanding.
        assert_eq!(idents(&run("#define X X+1\nX", &[]).unwrap()), vec!["X", "'+'", "1"]);
    }

    #[test]
    fn conditionals_nest_and_else() {
        let src = "#define ON\n#ifdef ON\na\n#ifndef ON\nb\n#else\nc\n#endif\n#else\nd\n#endif\n\
                   #if defined(ON) && 2 > 1\ne\n#elif 1\nf\n#endif\n#ifdef _010EDITOR\ng\n#endif";
        assert_eq!(idents(&run(src, &[]).unwrap()), vec!["a", "c", "e", "g"]);
        assert!(run("#ifdef A\n", &[]).is_err());
        assert!(run("#endif\n", &[]).is_err());
    }

    #[test]
    fn includes_and_errors() {
        let t = run("#include \"inc.bt\"\nb", &[("inc.bt", "#define V 5\na V")]).unwrap();
        assert_eq!(idents(&t), vec!["a", "5", "b"]);
        let e = run("#include <missing.bt>", &[]).unwrap_err();
        assert!(e.msg.contains("missing.bt"));
        assert_eq!(run("#error \"stop here\"", &[]).unwrap_err().msg, "stop here");
        assert!(run("#ifdef NOPE\n#error \"no\"\n#endif\nx", &[]).is_ok());
    }
}
