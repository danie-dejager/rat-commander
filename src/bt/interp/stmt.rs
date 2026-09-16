//! Statements and control flow.

use super::{Interp, R};
use crate::bt::ast::{BinOp, Stmt};
use crate::bt::value::Value;

/// How a statement finished.
#[derive(Debug)]
pub(crate) enum Flow {
    Normal,
    Break,
    Continue,
    Return(Value),
}

impl Interp {
    pub(crate) fn exec(&mut self, s: &Stmt) -> R<Flow> {
        self.tick()?;
        match s {
            Stmt::Expr(pos, e) => {
                self.cur_pos = *pos;
                self.eval(e)?;
                Ok(Flow::Normal)
            }
            Stmt::Decl(d) => {
                self.cur_pos = d.pos;
                self.exec_decl(d)?;
                Ok(Flow::Normal)
            }
            Stmt::Block(list) => {
                for s in list {
                    match self.exec(s)? {
                        Flow::Normal => {}
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::If(pos, arms, els) => {
                for (c, body) in arms {
                    self.cur_pos = *pos;
                    let v = self.eval(c)?;
                    if self.truthy(&v)? {
                        return self.exec(body);
                    }
                }
                match els {
                    Some(e) => self.exec(e),
                    None => Ok(Flow::Normal),
                }
            }
            Stmt::While(pos, c, body) => {
                loop {
                    self.cur_pos = *pos;
                    let v = self.eval(c)?;
                    if !self.truthy(&v)? {
                        break;
                    }
                    match self.exec(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::DoWhile(pos, body, c) => {
                loop {
                    match self.exec(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                    self.cur_pos = *pos;
                    let v = self.eval(c)?;
                    if !self.truthy(&v)? {
                        break;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For(pos, init, cond, step, body) => {
                if let Some(i) = init {
                    self.exec(i)?;
                }
                loop {
                    self.cur_pos = *pos;
                    if let Some(c) = cond {
                        let v = self.eval(c)?;
                        if !self.truthy(&v)? {
                            break;
                        }
                    }
                    match self.exec(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                    if let Some(st) = step {
                        self.cur_pos = *pos;
                        self.eval(st)?;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Switch(pos, c, body) => {
                self.cur_pos = *pos;
                let v = self.eval(c)?;
                let v = match v {
                    Value::Node(r) => self.node_value(r)?,
                    v => v,
                };
                let mut start = None;
                let mut default = None;
                for (i, s) in body.iter().enumerate() {
                    match s {
                        Stmt::Case(p, e) => {
                            self.cur_pos = *p;
                            let cv = self.eval(e)?;
                            let eq = self.binop(BinOp::Eq, v.clone(), cv)?;
                            if self.truthy(&eq)? {
                                start = Some(i);
                                break;
                            }
                        }
                        Stmt::Default if default.is_none() => default = Some(i),
                        _ => {}
                    }
                }
                let Some(from) = start.or(default) else { return Ok(Flow::Normal) };
                for s in &body[from..] {
                    match self.exec(s)? {
                        Flow::Normal => {}
                        Flow::Break => return Ok(Flow::Normal),
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Case(..) | Stmt::Default | Stmt::Empty => Ok(Flow::Normal),
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Return(pos, e) => {
                self.cur_pos = *pos;
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => Value::Void,
                };
                Ok(Flow::Return(v))
            }
        }
    }
}
