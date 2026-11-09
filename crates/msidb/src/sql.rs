//! MSI SQL: the statements MsiDatabaseOpenView takes.
//!
//! SELECT [DISTINCT] cols|* FROM t[, t...] [WHERE expr] [ORDER BY cols];
//! INSERT INTO t (cols) VALUES (vals) [TEMPORARY]; UPDATE t SET c = v, ... [WHERE expr];
//! DELETE FROM t [WHERE expr]; CREATE TABLE t (c type [NOT NULL] [TEMPORARY|LOCALIZABLE], ...
//! PRIMARY KEY c, ...) [HOLD]; ALTER TABLE t HOLD|FREE|ADD c type [HOLD]; DROP TABLE t.
//! Names are bare or in backquotes, strings in single quotes, `?` a parameter from the record
//! MsiViewExecute takes. Keywords are case-insensitive, names are not.

use crate::db::*;
use crate::*;

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Id(String),
    /// a bare word (keyword or name)
    Word(String),
    Str(String),
    Int(i64),
    Param,
    Op(&'static str),
}

fn lex(s: &str) -> R<Vec<Tok>> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        match ch {
            '`' => {
                let st = i + 1;
                i = st;
                while i < c.len() && c[i] != '`' {
                    i += 1;
                }
                if i >= c.len() {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
                out.push(Tok::Id(c[st..i].iter().collect()));
                i += 1;
            }
            '\'' => {
                let st = i + 1;
                i = st;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
                if i >= c.len() {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
                out.push(Tok::Str(c[st..i].iter().collect()));
                i += 1;
            }
            '?' => {
                out.push(Tok::Param);
                i += 1;
            }
            '0'..='9' => {
                let st = i;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
                let t: String = c[st..i].iter().collect();
                // (a number running into letters is a name: `1a`)
                if i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                        i += 1;
                    }
                    out.push(Tok::Word(c[st..i].iter().collect()));
                } else {
                    out.push(Tok::Int(t.parse().map_err(|_| ERROR_BAD_QUERY_SYNTAX)?));
                }
            }
            '<' | '>' | '=' | '(' | ')' | ',' | '*' | '.' | '-' => {
                let two: String = c[i..(i + 2).min(c.len())].iter().collect();
                let op = match two.as_str() {
                    "<>" => "<>",
                    "<=" => "<=",
                    ">=" => ">=",
                    _ => match ch {
                        '<' => "<",
                        '>' => ">",
                        '=' => "=",
                        '(' => "(",
                        ')' => ")",
                        ',' => ",",
                        '*' => "*",
                        '.' => ".",
                        _ => "-",
                    },
                };
                i += op.len();
                out.push(Tok::Op(op));
            }
            c0 if c0.is_alphabetic() || c0 == '_' || c0 == '#' || c0 == '$' => {
                let st = i;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '#' || c[i] == '$') {
                    i += 1;
                }
                out.push(Tok::Word(c[st..i].iter().collect()));
            }
            _ => return Err(ERROR_BAD_QUERY_SYNTAX),
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Int(i32),
    Str(String),
    /// the n-th `?` of the statement (from 1)
    Param(usize),
    Null,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColRef {
    pub table: Option<String>,
    pub col: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Col(ColRef),
    Val(Val),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp(Operand, Op, Operand),
    IsNull(ColRef, bool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColDef {
    pub name: String,
    pub ty: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Select { distinct: bool, cols: Vec<ColRef>, tables: Vec<String>, cond: Option<Expr>, order: Vec<ColRef> },
    Insert { table: String, cols: Vec<String>, vals: Vec<Val>, temporary: bool },
    Update { table: String, sets: Vec<(String, Val)>, cond: Option<Expr> },
    Delete { table: String, cond: Option<Expr> },
    Create { table: String, cols: Vec<ColDef>, keys: Vec<String>, hold: bool },
    AlterHold { table: String, hold: bool },
    AlterAdd { table: String, col: ColDef, hold: bool },
    Drop { table: String },
}

struct P {
    t: Vec<Tok>,
    i: usize,
    np: usize,
}

const KEYWORDS: &[&str] = &[
    "ADD", "ALTER", "AND", "BY", "CHAR", "CHARACTER", "CREATE", "DELETE", "DISTINCT", "DROP", "FREE", "FROM", "HOLD", "INSERT", "INT", "INTEGER", "INTO", "IS", "KEY", "LIKE",
    "LOCALIZABLE", "LONG", "LONGCHAR", "NOT", "NULL", "OBJECT", "OR", "ORDER", "PRIMARY", "SELECT", "SET", "SHORT", "TABLE", "TEMPORARY", "UPDATE", "VALUES", "WHERE",
];

fn is_kw(w: &str) -> bool {
    KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(w))
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn kw(&mut self, k: &str) -> bool {
        if let Some(Tok::Word(w)) = self.peek() {
            if w.eq_ignore_ascii_case(k) {
                self.i += 1;
                return true;
            }
        }
        false
    }
    fn need_kw(&mut self, k: &str) -> R<()> {
        if self.kw(k) { Ok(()) } else { Err(ERROR_BAD_QUERY_SYNTAX) }
    }
    fn op(&mut self, o: &str) -> bool {
        if let Some(Tok::Op(x)) = self.peek() {
            if *x == o {
                self.i += 1;
                return true;
            }
        }
        false
    }
    fn need_op(&mut self, o: &str) -> R<()> {
        if self.op(o) { Ok(()) } else { Err(ERROR_BAD_QUERY_SYNTAX) }
    }
    fn name(&mut self) -> R<String> {
        match self.peek().cloned() {
            Some(Tok::Id(s)) => {
                self.i += 1;
                Ok(s)
            }
            Some(Tok::Word(w)) if !is_kw(&w) => {
                self.i += 1;
                Ok(w)
            }
            _ => Err(ERROR_BAD_QUERY_SYNTAX),
        }
    }
    fn colref(&mut self) -> R<ColRef> {
        let a = self.name()?;
        if self.op(".") {
            let b = self.name()?;
            return Ok(ColRef { table: Some(a), col: b });
        }
        Ok(ColRef { table: None, col: a })
    }
    fn names(&mut self) -> R<Vec<String>> {
        let mut v = vec![self.name()?];
        while self.op(",") {
            v.push(self.name()?);
        }
        Ok(v)
    }
    fn colrefs(&mut self) -> R<Vec<ColRef>> {
        let mut v = vec![self.colref()?];
        while self.op(",") {
            v.push(self.colref()?);
        }
        Ok(v)
    }
    fn val(&mut self) -> R<Val> {
        let neg = self.op("-");
        match self.peek().cloned() {
            Some(Tok::Int(n)) => {
                self.i += 1;
                let n = if neg { -n } else { n };
                Ok(Val::Int(i32::try_from(n).map_err(|_| ERROR_BAD_QUERY_SYNTAX)?))
            }
            Some(Tok::Str(s)) if !neg => {
                self.i += 1;
                Ok(Val::Str(s))
            }
            Some(Tok::Param) if !neg => {
                self.i += 1;
                self.np += 1;
                Ok(Val::Param(self.np))
            }
            Some(Tok::Word(w)) if !neg && w.eq_ignore_ascii_case("NULL") => {
                self.i += 1;
                Ok(Val::Null)
            }
            _ => Err(ERROR_BAD_QUERY_SYNTAX),
        }
    }
    fn operand(&mut self) -> R<Operand> {
        match self.peek() {
            Some(Tok::Int(_)) | Some(Tok::Str(_)) | Some(Tok::Param) | Some(Tok::Op("-")) => Ok(Operand::Val(self.val()?)),
            _ => Ok(Operand::Col(self.colref()?)),
        }
    }
    fn expr(&mut self) -> R<Expr> {
        let mut l = self.and_expr()?;
        while self.kw("OR") {
            let r = self.and_expr()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and_expr(&mut self) -> R<Expr> {
        let mut l = self.prim()?;
        while self.kw("AND") {
            let r = self.prim()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn prim(&mut self) -> R<Expr> {
        if self.op("(") {
            let e = self.expr()?;
            self.need_op(")")?;
            return Ok(e);
        }
        let a = self.operand()?;
        if self.kw("IS") {
            let not = self.kw("NOT");
            self.need_kw("NULL")?;
            let Operand::Col(c) = a else { return Err(ERROR_BAD_QUERY_SYNTAX) };
            return Ok(Expr::IsNull(c, !not));
        }
        let op = if self.op("=") {
            Op::Eq
        } else if self.op("<>") {
            Op::Ne
        } else if self.op("<=") {
            Op::Le
        } else if self.op(">=") {
            Op::Ge
        } else if self.op("<") {
            Op::Lt
        } else if self.op(">") {
            Op::Gt
        } else {
            return Err(ERROR_BAD_QUERY_SYNTAX);
        };
        let b = self.operand()?;
        // (a comparison needs a column on one side; `x = NULL` is a column IS NULL)
        match (&a, &b) {
            (Operand::Val(_), Operand::Val(_)) => return Err(ERROR_BAD_QUERY_SYNTAX),
            (Operand::Col(c), Operand::Val(Val::Null)) | (Operand::Val(Val::Null), Operand::Col(c)) => match op {
                Op::Eq => return Ok(Expr::IsNull(c.clone(), true)),
                Op::Ne => return Ok(Expr::IsNull(c.clone(), false)),
                _ => return Err(ERROR_BAD_QUERY_SYNTAX),
            },
            _ => {}
        }
        Ok(Expr::Cmp(a, op, b))
    }
    fn coltype(&mut self) -> R<u16> {
        let mut ty = if self.kw("CHAR") || self.kw("CHARACTER") {
            let mut t = T_STRING | T_NONBINARY;
            if self.op("(") {
                let Some(Tok::Int(n)) = self.peek().cloned() else { return Err(ERROR_BAD_QUERY_SYNTAX) };
                self.i += 1;
                self.need_op(")")?;
                if n > 255 {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
                t |= n as u16;
            }
            t
        } else if self.kw("LONGCHAR") {
            T_STRING | T_NONBINARY
        } else if self.kw("SHORT") || self.kw("INT") || self.kw("INTEGER") {
            2 | T_NONBINARY
        } else if self.kw("LONG") {
            4
        } else if self.kw("OBJECT") {
            T_STRING | T_VALID
        } else {
            return Err(ERROR_BAD_QUERY_SYNTAX);
        };
        if self.kw("NOT") {
            self.need_kw("NULL")?;
        } else {
            ty |= T_NULLABLE;
        }
        if self.kw("TEMPORARY") {
            ty |= T_TEMPORARY;
        } else if self.kw("LOCALIZABLE") {
            ty |= T_LOCALIZABLE;
        }
        Ok(ty | T_VALID)
    }
    fn stmt(&mut self) -> R<Stmt> {
        let s = if self.kw("SELECT") {
            let distinct = self.kw("DISTINCT");
            let cols = if self.op("*") { Vec::new() } else { self.colrefs()? };
            self.need_kw("FROM")?;
            let tables = self.names()?;
            let cond = if self.kw("WHERE") { Some(self.expr()?) } else { None };
            let order = if self.kw("ORDER") {
                self.need_kw("BY")?;
                self.colrefs()?
            } else {
                Vec::new()
            };
            Stmt::Select { distinct, cols, tables, cond, order }
        } else if self.kw("INSERT") {
            self.need_kw("INTO")?;
            let table = self.name()?;
            self.need_op("(")?;
            let cols = self.names()?;
            self.need_op(")")?;
            self.need_kw("VALUES")?;
            self.need_op("(")?;
            let mut vals = vec![self.val()?];
            while self.op(",") {
                vals.push(self.val()?);
            }
            self.need_op(")")?;
            if vals.len() != cols.len() {
                return Err(ERROR_BAD_QUERY_SYNTAX);
            }
            let temporary = self.kw("TEMPORARY");
            Stmt::Insert { table, cols, vals, temporary }
        } else if self.kw("UPDATE") {
            let table = self.name()?;
            self.need_kw("SET")?;
            let mut sets = Vec::new();
            loop {
                let c = self.colref()?;
                self.need_op("=")?;
                sets.push((c.col, self.val()?));
                if !self.op(",") {
                    break;
                }
            }
            let cond = if self.kw("WHERE") { Some(self.expr()?) } else { None };
            Stmt::Update { table, sets, cond }
        } else if self.kw("DELETE") {
            self.need_kw("FROM")?;
            let table = self.name()?;
            let cond = if self.kw("WHERE") { Some(self.expr()?) } else { None };
            Stmt::Delete { table, cond }
        } else if self.kw("CREATE") {
            self.need_kw("TABLE")?;
            let table = self.name()?;
            self.need_op("(")?;
            let mut cols = Vec::new();
            loop {
                if self.kw("PRIMARY") {
                    break;
                }
                let name = self.name()?;
                let ty = self.coltype()?;
                cols.push(ColDef { name, ty });
                if !self.op(",") && !matches!(self.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("PRIMARY")) {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
            }
            self.need_kw("KEY")?;
            let keys = self.names()?;
            self.need_op(")")?;
            let hold = self.kw("HOLD");
            Stmt::Create { table, cols, keys, hold }
        } else if self.kw("ALTER") {
            self.need_kw("TABLE")?;
            let table = self.name()?;
            if self.kw("HOLD") {
                Stmt::AlterHold { table, hold: true }
            } else if self.kw("FREE") {
                Stmt::AlterHold { table, hold: false }
            } else {
                self.need_kw("ADD")?;
                let name = self.name()?;
                let ty = self.coltype()?;
                let hold = self.kw("HOLD");
                Stmt::AlterAdd { table, col: ColDef { name, ty }, hold }
            }
        } else if self.kw("DROP") {
            self.need_kw("TABLE")?;
            Stmt::Drop { table: self.name()? }
        } else {
            return Err(ERROR_BAD_QUERY_SYNTAX);
        };
        if self.i != self.t.len() {
            return Err(ERROR_BAD_QUERY_SYNTAX);
        }
        Ok(s)
    }
}

pub fn parse(s: &str) -> R<Stmt> {
    let t = lex(s)?;
    P { t, i: 0, np: 0 }.stmt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn c(t: Option<&str>, n: &str) -> ColRef {
        ColRef { table: t.map(str::to_string), col: n.into() }
    }

    #[test]
    fn select() {
        let s = parse("SELECT `Action`, Sequence FROM `InstallExecuteSequence` WHERE `Sequence` > 0 AND Condition IS NULL ORDER BY `Sequence`").unwrap();
        let Stmt::Select { cols, tables, cond, order, distinct } = s else { panic!() };
        assert!(!distinct);
        assert_eq!(cols, [c(None, "Action"), c(None, "Sequence")]);
        assert_eq!(tables, ["InstallExecuteSequence"]);
        assert_eq!(order, [c(None, "Sequence")]);
        assert_eq!(
            cond,
            Some(Expr::And(Box::new(Expr::Cmp(Operand::Col(c(None, "Sequence")), Op::Gt, Operand::Val(Val::Int(0)))), Box::new(Expr::IsNull(c(None, "Condition"), true))))
        );
        assert!(parse("select * from Property where Property = 'X'").is_ok());
        assert!(parse("SELECT * FROM `File`, `Component` WHERE `File`.`Component_` = `Component`.`Component`").is_ok());
        assert_eq!(parse("SELECT * FROM"), Err(ERROR_BAD_QUERY_SYNTAX));
        assert_eq!(parse("SELECT * FROM T WHERE 1 = 1"), Err(ERROR_BAD_QUERY_SYNTAX));
        assert_eq!(parse("SELECT * FROM T junk"), Err(ERROR_BAD_QUERY_SYNTAX));
    }

    #[test]
    fn create_insert() {
        let s = parse("CREATE TABLE `phone` ( `id` INT, `name` CHAR(32) NOT NULL LOCALIZABLE, `data` OBJECT, `t` LONGCHAR TEMPORARY PRIMARY KEY `id`) HOLD").unwrap();
        let Stmt::Create { cols, keys, hold, .. } = s else { panic!() };
        assert!(hold);
        assert_eq!(keys, ["id"]);
        let tys: Vec<u16> = cols.iter().map(|c| c.ty).collect();
        assert_eq!(tys, [0x1502, 0x0f20, 0x1900, 0x5d00]);
        let s = parse("INSERT INTO `T` (`a`, `b`, `c`) VALUES (-1, 'x', ?) TEMPORARY").unwrap();
        assert_eq!(s, Stmt::Insert { table: "T".into(), cols: vec!["a".into(), "b".into(), "c".into()], vals: vec![Val::Int(-1), Val::Str("x".into()), Val::Param(1)], temporary: true });
        assert!(parse("INSERT INTO T (a) VALUES (1, 2)").is_err());
        assert!(parse("UPDATE T SET a = 1, b = 'z' WHERE c = ?").is_ok());
        assert!(parse("DELETE FROM T WHERE a <> 1 OR (b >= 2 AND c <= 3)").is_ok());
        assert!(parse("ALTER TABLE T ADD `x` SHORT HOLD").is_ok());
        assert!(parse("DROP TABLE T").is_ok());
    }

    proptest! {
        #[test]
        fn no_panic(s in "[ -~]{0,60}") {
            let _ = parse(&s);
        }
        #[test]
        fn names_round_trip(t in "[A-Za-z_][A-Za-z0-9_]{0,10}", v in "[a-z ]{0,10}") {
            prop_assume!(!is_kw(&t));
            let s = parse(&format!("SELECT * FROM `{t}` WHERE `{t}`.`c` = '{v}'")).unwrap();
            let Stmt::Select { tables, .. } = s else { panic!() };
            prop_assert_eq!(tables, vec![t]);
        }
    }
}
