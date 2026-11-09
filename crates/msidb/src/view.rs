//! Views: a statement over the database, executed with a record of parameters, then
//! fetched (SELECT) or done at once (the rest); MsiViewModify on fetched records.

use crate::db::*;
use crate::record::{Field, Record};
use crate::sql::*;
use crate::*;
use std::sync::atomic::{AtomicU64, Ordering};

static IDS: AtomicU64 = AtomicU64::new(1);

/// The system tables that are not stored as tables.
pub const VIRTUAL: &[&str] = &["_Tables", "_Columns", "_Streams", "_Storages"];
const SUMMARY: &str = "\u{5}SummaryInformation";

enum Src {
    Db(String),
    Virt { name: String, cols: Vec<Col>, rows: Vec<Vec<Field>> },
}

pub struct View {
    pub id: u64,
    pub stmt: Stmt,
    srcs: Vec<Src>,
    /// (source, column) of each result column
    pub cols: Vec<(usize, usize)>,
    /// per result row: the row index in each source
    result: Vec<Vec<usize>>,
    pos: usize,
    pub executed: bool,
    /// the column MsiViewGetError reports and its error
    pub error: Option<(String, i32)>,
}

pub const MSIMODIFY_SEEK: i32 = -1;
pub const MSIMODIFY_REFRESH: i32 = 0;
pub const MSIMODIFY_INSERT: i32 = 1;
pub const MSIMODIFY_UPDATE: i32 = 2;
pub const MSIMODIFY_ASSIGN: i32 = 3;
pub const MSIMODIFY_REPLACE: i32 = 4;
pub const MSIMODIFY_MERGE: i32 = 5;
pub const MSIMODIFY_DELETE: i32 = 6;
pub const MSIMODIFY_INSERT_TEMPORARY: i32 = 7;
pub const MSIMODIFY_VALIDATE: i32 = 8;
pub const MSIMODIFY_VALIDATE_NEW: i32 = 9;
pub const MSIMODIFY_VALIDATE_FIELD: i32 = 10;
pub const MSIMODIFY_VALIDATE_DELETE: i32 = 11;

fn vcol(name: &str, ty: u16) -> Col {
    Col { name: name.into(), ty }
}

fn virt(db: &Database, name: &str) -> Option<Src> {
    let (cols, rows): (Vec<Col>, Vec<Vec<Field>>) = match name {
        "_Tables" => (vec![vcol("Name", 0x2d40)], db.table_order().into_iter().map(|i| vec![Field::Str(db.tables[i].name.clone())]).collect()),
        "_Columns" => {
            let mut rows = Vec::new();
            for i in db.table_order() {
                let t = &db.tables[i];
                for (n, c) in t.cols.iter().enumerate() {
                    rows.push(vec![Field::Str(t.name.clone()), Field::Int(n as i32 + 1), Field::Str(c.name.clone()), Field::Int(c.ty as i32)]);
                }
            }
            (vec![vcol("Table", 0x2d40), vcol("Number", 0x2502), vcol("Name", 0x0d40), vcol("Type", 0x0502)], rows)
        }
        // (the summary information is a stream too)
        "_Streams" => (
            vec![vcol("Name", 0x2d3e), vcol("Data", 0x1900)],
            db.streams.keys().cloned().chain(db.summary.is_some().then(|| SUMMARY.to_string())).map(|k| vec![Field::Str(k), Field::Null]).collect(),
        ),
        "_Storages" => (vec![vcol("Name", 0x2d3e), vcol("Data", 0x1900)], db.storages.keys().map(|k| vec![Field::Str(k.clone()), Field::Null]).collect()),
        _ => return None,
    };
    Some(Src::Virt { name: name.into(), cols, rows })
}

fn src_of(db: &Database, name: &str) -> R<Src> {
    if let Some(v) = virt(db, name) {
        return Ok(v);
    }
    db.table(name).ok_or(ERROR_BAD_QUERY_SYNTAX)?;
    Ok(Src::Db(name.into()))
}

impl Src {
    fn name(&self) -> &str {
        match self {
            Src::Db(n) => n,
            Src::Virt { name, .. } => name,
        }
    }
    fn cols<'a>(&'a self, db: &'a Database) -> &'a [Col] {
        match self {
            Src::Db(n) => db.table(n).map_or(&[], |t| &t.cols[..]),
            Src::Virt { cols, .. } => cols,
        }
    }
    fn len(&self, db: &Database) -> usize {
        match self {
            Src::Db(n) => db.table(n).map_or(0, |t| t.rows.len()),
            Src::Virt { rows, .. } => rows.len(),
        }
    }
    fn field(&self, db: &Database, r: usize, c: usize) -> Field {
        match self {
            Src::Db(n) => match db.table(n) {
                Some(t) if r < t.rows.len() => db.field(t, &t.rows[r], c),
                _ => Field::Null,
            },
            Src::Virt { name, rows, .. } => {
                let f = rows.get(r).and_then(|row| row.get(c)).cloned().unwrap_or_default();
                if c == 1 && (name == "_Streams" || name == "_Storages") {
                    let key = rows[r][0].text();
                    let d = if name == "_Streams" && key == SUMMARY {
                        db.summary.clone()
                    } else if name == "_Streams" {
                        db.stream(&key)
                    } else {
                        db.storage(&key)
                    };
                    return d.map_or(Field::Null, Field::Stream);
                }
                f
            }
        }
    }
    /// The raw value (for ORDER BY: string ids, as Windows sorts) or the field's text.
    fn raw(&self, db: &Database, r: usize, c: usize) -> (u32, String) {
        match self {
            Src::Db(n) => match db.table(n) {
                Some(t) if r < t.rows.len() => (t.rows[r].v[c], String::new()),
                _ => (0, String::new()),
            },
            Src::Virt { rows, .. } => match &rows[r][c] {
                Field::Int(i) => ((*i as u32) ^ 0x8000_0000, String::new()),
                f => (0, f.text()),
            },
        }
    }
}

fn param(p: &Option<&Record>, n: usize) -> Field {
    p.map_or(Field::Null, |r| r.get(n).clone())
}

fn val_field(v: &Val, p: &Option<&Record>) -> Field {
    match v {
        Val::Int(i) => Field::Int(*i),
        Val::Str(s) => Field::Str(s.clone()),
        Val::Param(n) => param(p, *n),
        Val::Null => Field::Null,
    }
}

impl View {
    pub fn new(db: &Database, sql: &str) -> R<View> {
        let stmt = parse(sql)?;
        let mut v = View { id: IDS.fetch_add(1, Ordering::Relaxed), stmt, srcs: Vec::new(), cols: Vec::new(), result: Vec::new(), pos: 0, executed: false, error: None };
        match &v.stmt {
            Stmt::Select { cols, tables, order, .. } => {
                for t in tables {
                    if v.srcs.iter().any(|s| s.name() == t) {
                        return Err(ERROR_BAD_QUERY_SYNTAX);
                    }
                    v.srcs.push(src_of(db, t)?);
                }
                if cols.is_empty() {
                    for (si, s) in v.srcs.iter().enumerate() {
                        for ci in 0..s.cols(db).len() {
                            v.cols.push((si, ci));
                        }
                    }
                } else {
                    for c in cols.clone() {
                        let r = v.resolve(db, &c)?;
                        v.cols.push(r);
                    }
                }
                for c in order.clone() {
                    v.resolve(db, &c)?;
                }
                if let Stmt::Select { cond: Some(e), .. } = &v.stmt {
                    let e = e.clone();
                    v.check_expr(db, &e)?;
                }
            }
            Stmt::Insert { table, cols, .. } => {
                let s = src_of(db, table)?;
                for c in cols {
                    let ci = s.cols(db).iter().position(|x| &x.name == c).ok_or(ERROR_BAD_QUERY_SYNTAX)?;
                    v.cols.push((0, ci));
                }
                v.srcs.push(s);
            }
            Stmt::Update { table, sets, cond } => {
                let s = src_of(db, table)?;
                if matches!(s, Src::Virt { .. }) && table != "_Streams" && table != "_Storages" {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
                for (c, _) in sets {
                    let ci = s.cols(db).iter().position(|x| &x.name == c).ok_or(ERROR_BAD_QUERY_SYNTAX)?;
                    v.cols.push((0, ci));
                }
                v.srcs.push(s);
                if let Some(e) = cond.clone() {
                    v.check_expr(db, &e)?;
                }
            }
            Stmt::Delete { table, cond } => {
                v.srcs.push(src_of(db, table)?);
                if let Some(e) = cond.clone() {
                    v.check_expr(db, &e)?;
                }
            }
            Stmt::Create { table, .. } => {
                if db.table(table).is_some() || VIRTUAL.contains(&table.as_str()) {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
            }
            Stmt::AlterHold { table, .. } | Stmt::AlterAdd { table, .. } | Stmt::Drop { table } => {
                if db.table(table).is_none() {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
            }
        }
        Ok(v)
    }

    fn resolve(&self, db: &Database, c: &ColRef) -> R<(usize, usize)> {
        for (si, s) in self.srcs.iter().enumerate() {
            if c.table.as_ref().is_some_and(|t| t != s.name()) {
                continue;
            }
            if let Some(ci) = s.cols(db).iter().position(|x| x.name == c.col) {
                return Ok((si, ci));
            }
        }
        Err(ERROR_BAD_QUERY_SYNTAX)
    }

    fn check_expr(&self, db: &Database, e: &Expr) -> R<()> {
        match e {
            Expr::And(a, b) | Expr::Or(a, b) => {
                self.check_expr(db, a)?;
                self.check_expr(db, b)
            }
            Expr::IsNull(c, _) => self.resolve(db, c).map(|_| ()),
            Expr::Cmp(a, op, b) => {
                let mut string = false;
                for o in [a, b] {
                    match o {
                        Operand::Col(c) => {
                            let (si, ci) = self.resolve(db, c)?;
                            string |= self.srcs[si].cols(db)[ci].ty & T_STRING != 0;
                        }
                        Operand::Val(Val::Str(_)) => string = true,
                        _ => {}
                    }
                }
                // (strings compare only for equality)
                if string && !matches!(op, Op::Eq | Op::Ne) {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
                Ok(())
            }
        }
    }

    /// Columns of the result: (name, type) as MsiViewGetColumnInfo gives them.
    pub fn columns(&self, db: &Database) -> Vec<(String, u16)> {
        self.cols.iter().map(|&(s, c)| {
            let col = &self.srcs[s].cols(db)[c];
            (col.name.clone(), col.ty)
        }).collect()
    }

    pub fn column_tables(&self) -> Vec<String> {
        self.cols.iter().map(|&(s, _)| self.srcs[s].name().to_string()).collect()
    }

    /// The expression's value for bound rows (None: a table it needs is not bound yet).
    fn eval(&self, db: &Database, e: &Expr, rows: &[usize], p: &Option<&Record>) -> Option<bool> {
        match e {
            Expr::And(a, b) => match (self.eval(db, a, rows, p), self.eval(db, b, rows, p)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            Expr::Or(a, b) => match (self.eval(db, a, rows, p), self.eval(db, b, rows, p)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            Expr::IsNull(c, null) => {
                let (s, ci) = self.resolve(db, c).ok()?;
                let r = *rows.get(s)?;
                Some(self.srcs[s].field(db, r, ci).is_null() == *null)
            }
            Expr::Cmp(a, op, b) => {
                let get = |o: &Operand| -> Option<(Field, Option<bool>)> {
                    Some(match o {
                        Operand::Val(v) => (val_field(v, p), None),
                        Operand::Col(c) => {
                            let (s, ci) = self.resolve(db, c).ok()?;
                            let r = *rows.get(s)?;
                            let col = &self.srcs[s].cols(db)[ci];
                            (self.srcs[s].field(db, r, ci), Some(col.ty & T_STRING != 0))
                        }
                    })
                };
                let (fa, sa) = get(a)?;
                let (fb, sb) = get(b)?;
                let string = sa.or(sb).unwrap_or(false) && sa != Some(false) && sb != Some(false);
                Some(if string {
                    let (x, y) = (fa.text(), fb.text());
                    match op {
                        Op::Eq => x == y,
                        Op::Ne => x != y,
                        _ => false,
                    }
                } else {
                    let (x, y) = (num(&fa), num(&fb));
                    match (x, y) {
                        (Some(x), Some(y)) => match op {
                            Op::Eq => x == y,
                            Op::Ne => x != y,
                            Op::Lt => x < y,
                            Op::Gt => x > y,
                            Op::Le => x <= y,
                            Op::Ge => x >= y,
                        },
                        (None, None) => matches!(op, Op::Eq | Op::Le | Op::Ge),
                        _ => matches!(op, Op::Ne),
                    }
                })
            }
        }
    }

    fn matching(&self, db: &Database, cond: &Option<Expr>, p: &Option<&Record>) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        let mut cur = Vec::new();
        self.join(db, cond, p, &mut cur, &mut out);
        out
    }

    fn join(&self, db: &Database, cond: &Option<Expr>, p: &Option<&Record>, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        let k = cur.len();
        if k == self.srcs.len() {
            if cond.as_ref().is_none_or(|e| self.eval(db, e, cur, p) == Some(true)) {
                out.push(cur.clone());
            }
            return;
        }
        for r in 0..self.srcs[k].len(db) {
            cur.push(r);
            let ok = cond.as_ref().is_none_or(|e| self.eval(db, e, cur, p) != Some(false));
            if ok {
                self.join(db, cond, p, cur, out);
            }
            cur.pop();
        }
    }

    /// MsiViewExecute.
    pub fn execute(&mut self, db: &mut Database, p: Option<&Record>) -> R<()> {
        self.pos = 0;
        self.result.clear();
        // (the system tables' contents as they are now)
        for s in self.srcs.iter_mut() {
            if let Src::Virt { name, .. } = s {
                *s = virt(db, &name.clone()).unwrap();
            }
        }
        let stmt = self.stmt.clone();
        match stmt {
            Stmt::Select { cond, order, distinct, .. } => {
                let mut res = self.matching(db, &cond, &p);
                if !order.is_empty() {
                    let keys: Vec<(usize, usize)> = order.iter().map(|c| self.resolve(db, c).unwrap()).collect();
                    res.sort_by(|a, b| {
                        for &(s, c) in &keys {
                            let o = self.srcs[s].raw(db, a[s], c).cmp(&self.srcs[s].raw(db, b[s], c));
                            if o != std::cmp::Ordering::Equal {
                                return o;
                            }
                        }
                        std::cmp::Ordering::Equal
                    });
                }
                if distinct {
                    let mut seen = Vec::<Vec<Field>>::new();
                    res.retain(|r| {
                        let f: Vec<Field> = self.cols.iter().map(|&(s, c)| self.srcs[s].field(db, r[s], c)).collect();
                        if seen.contains(&f) {
                            false
                        } else {
                            seen.push(f);
                            true
                        }
                    });
                }
                self.result = res;
            }
            Stmt::Insert { table, vals, temporary, .. } => {
                let fields: Vec<Field> = vals.iter().map(|v| val_field(v, &p)).collect();
                if let Some(ti) = db.table_index(&table) {
                    let n = db.tables[ti].cols.len();
                    let mut row = vec![Field::Null; n];
                    for (&(_, ci), f) in self.cols.iter().zip(fields) {
                        row[ci] = f;
                    }
                    check_row(&db.tables[ti], &row)?;
                    db.insert(ti, &row, temporary)?;
                } else if table == "_Streams" || table == "_Storages" {
                    let mut name = None;
                    let mut data = None;
                    for (&(_, ci), f) in self.cols.iter().zip(fields) {
                        if ci == 0 {
                            name = Some(f.text());
                        } else if let Field::Stream(d) = f {
                            data = Some(d);
                        }
                    }
                    let name = name.filter(|n| !n.is_empty()).ok_or(ERROR_FUNCTION_FAILED)?;
                    let map = if table == "_Streams" { &mut db.streams } else { &mut db.storages };
                    if map.contains_key(&name) {
                        return Err(ERROR_FUNCTION_FAILED);
                    }
                    map.insert(name, Stream::Mem(data.unwrap_or_default()));
                    db.dirty = true;
                } else {
                    return Err(ERROR_FUNCTION_FAILED);
                }
            }
            Stmt::Update { table, sets, cond } if db.table_index(&table).is_none() => {
                // (_Streams / _Storages: a stream's data)
                let rows = self.matching(db, &cond, &p);
                let vals: Vec<Field> = sets.iter().map(|(_, v)| val_field(v, &p)).collect();
                let names: Vec<String> = rows.iter().map(|r| self.srcs[0].field(db, r[0], 0).text()).collect();
                for n in names {
                    for (&(_, ci), f) in self.cols.iter().zip(&vals) {
                        let Field::Stream(d) = f else { return Err(ERROR_FUNCTION_FAILED) };
                        if ci != 1 {
                            return Err(ERROR_FUNCTION_FAILED);
                        }
                        if table == "_Streams" && n == SUMMARY {
                            db.summary = Some(d.clone());
                        } else {
                            let map = if table == "_Streams" { &mut db.streams } else { &mut db.storages };
                            map.insert(n.clone(), Stream::Mem(d.clone()));
                        }
                        db.dirty = true;
                    }
                }
            }
            Stmt::Update { table, sets, cond } => {
                let ti = db.table_index(&table).ok_or(ERROR_FUNCTION_FAILED)?;
                let rows = self.matching(db, &cond, &p);
                let vals: Vec<Field> = sets.iter().map(|(_, v)| val_field(v, &p)).collect();
                // (a key changing moves rows: update by key values, last first)
                let mut keys = Vec::new();
                for r in &rows {
                    keys.push(db.tables[ti].rows[r[0]].v.clone());
                }
                for k in keys {
                    let Some(ri) = db.tables[ti].rows.iter().position(|r| r.v == k) else { continue };
                    let mut ri = ri;
                    for (&(_, ci), f) in self.cols.iter().zip(&vals) {
                        let col = &db.tables[ti].cols[ci];
                        if f.is_null() && !col.nullable() {
                            return Err(ERROR_FUNCTION_FAILED);
                        }
                        ri = db.set(ti, ri, ci, f)?;
                    }
                }
            }
            Stmt::Delete { table, cond } => {
                let rows = self.matching(db, &cond, &p);
                if let Some(ti) = db.table_index(&table) {
                    for r in rows.iter().rev() {
                        db.delete(ti, r[0]);
                    }
                } else {
                    let names: Vec<String> = rows.iter().map(|r| self.srcs[0].field(db, r[0], 0).text()).collect();
                    for n in names {
                        if table == "_Streams" {
                            db.streams.remove(&n);
                        } else {
                            db.storages.remove(&n);
                        }
                        db.dirty = true;
                    }
                }
            }
            Stmt::Create { table, cols, keys, hold } => {
                let mut names = std::collections::HashSet::new();
                if cols.iter().any(|c| !names.insert(c.name.clone())) || keys.iter().any(|k| !names.contains(k)) {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
                let cs: Vec<Col> = cols.iter().map(|c| Col { name: c.name.clone(), ty: c.ty | if keys.contains(&c.name) { T_KEY } else { 0 } }).collect();
                let temp = cs.iter().all(|c| c.temporary());
                if !temp && cs.iter().any(|c| c.temporary() && c.is_key()) {
                    return Err(ERROR_FUNCTION_FAILED);
                }
                if temp && !hold {
                    return Ok(());
                }
                db.create_table(&table, cs, !temp)?;
                if hold {
                    let ti = db.table_index(&table).unwrap();
                    db.tables[ti].hold += 1;
                }
            }
            Stmt::AlterHold { table, hold } => {
                let ti = db.table_index(&table).ok_or(ERROR_BAD_QUERY_SYNTAX)?;
                if hold {
                    db.tables[ti].hold += 1;
                } else {
                    db.tables[ti].hold -= 1;
                    if db.tables[ti].hold <= 0 && !db.tables[ti].persistent {
                        db.drop_table(ti);
                    }
                }
            }
            Stmt::AlterAdd { table, col, hold } => {
                let ti = db.table_index(&table).ok_or(ERROR_BAD_QUERY_SYNTAX)?;
                if db.tables[ti].cols.iter().any(|c| c.name == col.name) {
                    return Err(ERROR_BAD_QUERY_SYNTAX);
                }
                let persistent = db.tables[ti].persistent;
                db.pool.add(&col.name, persistent);
                db.pool.add(&table, persistent);
                db.tables[ti].cols.push(Col { name: col.name.clone(), ty: col.ty });
                for r in db.tables[ti].rows.iter_mut() {
                    r.v.push(0);
                }
                if hold {
                    db.tables[ti].hold += 1;
                }
                db.dirty |= persistent;
            }
            Stmt::Drop { table } => {
                let ti = db.table_index(&table).ok_or(ERROR_BAD_QUERY_SYNTAX)?;
                db.drop_table(ti);
            }
        }
        self.executed = true;
        Ok(())
    }

    /// MsiViewFetch: the next result row (rows deleted since are skipped).
    pub fn fetch(&mut self, db: &Database) -> R<Record> {
        if !self.executed {
            return Err(ERROR_FUNCTION_FAILED);
        }
        if !matches!(self.stmt, Stmt::Select { .. }) {
            return Err(ERROR_FUNCTION_FAILED);
        }
        while self.pos < self.result.len() {
            let i = self.pos;
            self.pos += 1;
            let r = &self.result[i];
            if r.iter().enumerate().any(|(s, &ri)| ri >= self.srcs[s].len(db)) {
                continue;
            }
            let mut rec = Record::new(self.cols.len());
            for (k, &(s, c)) in self.cols.iter().enumerate() {
                rec.f[k + 1] = self.srcs[s].field(db, r[s], c);
            }
            rec.origin = Some((self.id, i));
            return Ok(rec);
        }
        Err(ERROR_NO_MORE_ITEMS)
    }

    pub fn close(&mut self) {
        self.executed = false;
        self.result.clear();
        self.pos = 0;
    }

    /// The table row a fetched record came from (single-table views).
    fn origin_row(&self, db: &Database, rec: &Record) -> Option<usize> {
        let (id, i) = rec.origin?;
        if id != self.id {
            return None;
        }
        let r = self.result.get(i)?;
        (r[0] < self.srcs[0].len(db)).then_some(r[0])
    }

    /// A full table row from a record of this view's columns.
    fn row_fields(&self, db: &Database, rec: &Record) -> R<(usize, Vec<Field>)> {
        let Src::Db(name) = &self.srcs[0] else { return Err(ERROR_FUNCTION_FAILED) };
        let ti = db.table_index(name).ok_or(ERROR_FUNCTION_FAILED)?;
        if self.srcs.len() != 1 {
            return Err(ERROR_FUNCTION_FAILED);
        }
        if rec.count() < self.cols.len() {
            return Err(ERROR_INVALID_DATA);
        }
        let mut row = vec![Field::Null; db.tables[ti].cols.len()];
        for (k, &(_, c)) in self.cols.iter().enumerate() {
            row[c] = rec.get(k + 1).clone();
        }
        Ok((ti, row))
    }

    fn find_by_keys(db: &Database, ti: usize, row: &[Field]) -> Option<usize> {
        let t = &db.tables[ti];
        let mut probe = vec![0; t.cols.len()];
        for k in t.keys().collect::<Vec<_>>() {
            probe[k] = db.lookup(&t.cols[k], &row[k])?;
        }
        if t.keys().next().is_none() {
            return None;
        }
        db.find_key(ti, &probe).err()
    }

    /// MsiViewModify.
    pub fn modify(&mut self, db: &mut Database, mode: i32, rec: &mut Record) -> R<()> {
        if !self.executed && !matches!(mode, MSIMODIFY_INSERT | MSIMODIFY_INSERT_TEMPORARY | MSIMODIFY_VALIDATE_NEW) {
            return Err(ERROR_FUNCTION_FAILED);
        }
        match mode {
            MSIMODIFY_SEEK | MSIMODIFY_REFRESH => {
                let (ti, row) = self.row_fields(db, rec)?;
                let ri = if mode == MSIMODIFY_REFRESH { self.origin_row(db, rec) } else { None }.or_else(|| Self::find_by_keys(db, ti, &row)).ok_or(ERROR_FUNCTION_FAILED)?;
                for (k, &(_, c)) in self.cols.iter().enumerate() {
                    let t = &db.tables[ti];
                    rec.f[k + 1] = db.field(t, &t.rows[ri], c);
                }
                Ok(())
            }
            MSIMODIFY_INSERT | MSIMODIFY_INSERT_TEMPORARY => {
                let (ti, row) = self.row_fields(db, rec)?;
                check_row(&db.tables[ti], &row)?;
                db.insert(ti, &row, mode == MSIMODIFY_INSERT_TEMPORARY).map(|_| ())
            }
            MSIMODIFY_UPDATE => {
                let (ti, row) = self.row_fields(db, rec)?;
                let ri = self.origin_row(db, rec).ok_or(ERROR_FUNCTION_FAILED)?;
                // (keys may not change through UPDATE)
                for k in db.tables[ti].keys().collect::<Vec<_>>() {
                    if !self.cols.iter().any(|&(_, c)| c == k) {
                        continue;
                    }
                    let t = &db.tables[ti];
                    if db.field(t, &t.rows[ri], k) != row[k] {
                        return Err(ERROR_FUNCTION_FAILED);
                    }
                }
                for &(_, c) in &self.cols.clone() {
                    if !db.tables[ti].cols[c].is_key() {
                        db.set(ti, ri, c, &row[c])?;
                    }
                }
                Ok(())
            }
            MSIMODIFY_ASSIGN | MSIMODIFY_REPLACE | MSIMODIFY_MERGE => {
                let (ti, row) = self.row_fields(db, rec)?;
                match Self::find_by_keys(db, ti, &row) {
                    None => {
                        check_row(&db.tables[ti], &row)?;
                        db.insert(ti, &row, false).map(|_| ())
                    }
                    Some(ri) => {
                        if mode == MSIMODIFY_MERGE {
                            let t = &db.tables[ti];
                            let same = (0..t.cols.len()).all(|c| !self.cols.iter().any(|&(_, x)| x == c) || db.field(t, &t.rows[ri], c) == row[c]);
                            return if same { Ok(()) } else { Err(ERROR_FUNCTION_FAILED) };
                        }
                        let cols: Vec<usize> = self.cols.iter().map(|&(_, c)| c).collect();
                        for c in cols {
                            if !db.tables[ti].cols[c].is_key() {
                                db.set(ti, ri, c, &row[c])?;
                            }
                        }
                        Ok(())
                    }
                }
            }
            MSIMODIFY_DELETE => {
                let (ti, row) = self.row_fields(db, rec)?;
                let ri = Self::find_by_keys(db, ti, &row).or_else(|| self.origin_row(db, rec)).ok_or(ERROR_FUNCTION_FAILED)?;
                db.delete(ti, ri);
                Ok(())
            }
            MSIMODIFY_VALIDATE | MSIMODIFY_VALIDATE_NEW | MSIMODIFY_VALIDATE_FIELD | MSIMODIFY_VALIDATE_DELETE => {
                let (ti, row) = self.row_fields(db, rec)?;
                let t = &db.tables[ti];
                for (k, &(_, c)) in self.cols.iter().enumerate() {
                    if row[c].is_null() && !t.cols[c].nullable() {
                        self.error = Some((t.cols[c].name.clone(), 3)); // MSIDBERROR_REQUIRED
                        let _ = k;
                        return Err(ERROR_INVALID_DATA);
                    }
                }
                if mode == MSIMODIFY_VALIDATE_NEW && Self::find_by_keys(db, ti, &row).is_some() {
                    let k = t.keys().next().unwrap();
                    self.error = Some((t.cols[k].name.clone(), 1)); // MSIDBERROR_DUPLICATEKEY
                    return Err(ERROR_INVALID_DATA);
                }
                Ok(())
            }
            _ => Err(ERROR_FUNCTION_FAILED),
        }
    }
}

fn num(f: &Field) -> Option<i64> {
    match f {
        Field::Int(i) if *i != NULL_INT => Some(*i as i64),
        Field::Str(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Checks a row before insert: key columns must be set, values must fit the column type.
fn check_row(t: &Table, row: &[Field]) -> R<()> {
    for (c, f) in t.cols.iter().zip(row) {
        if c.is_key() && f.is_null() && t.cols.iter().filter(|x| x.is_key()).count() == 1 && !c.nullable() && c.ty & T_STRING != 0 {
            // (Windows allows a NULL key; inserting a second one then fails as a duplicate)
        }
        if c.ty & T_STRING == 0 && !c.is_binary() {
            if let Field::Str(s) = f {
                if crate::record::parse_int(s).is_none() {
                    return Err(ERROR_DATATYPE_MISMATCH);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(db: &mut Database, sql: &str) -> R<Vec<Record>> {
        let mut v = View::new(db, sql)?;
        v.execute(db, None)?;
        let mut out = Vec::new();
        if matches!(v.stmt, Stmt::Select { .. }) {
            while let Ok(r) = v.fetch(db) {
                out.push(r);
            }
        }
        Ok(out)
    }

    fn texts(rs: &[Record]) -> Vec<String> {
        rs.iter().map(|r| (1..=r.count()).map(|i| r.get(i).text()).collect::<Vec<_>>().join("|")).collect()
    }

    #[test]
    fn crud() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `phone` ( `id` INT, `name` CHAR(32), `number` CHAR(32) PRIMARY KEY `id`)").unwrap();
        run(&mut db, "INSERT INTO `phone` ( `id`, `name`, `number` ) VALUES ( 3, 'Cindy', '2937550' )").unwrap();
        run(&mut db, "INSERT INTO `phone` ( `id`, `name`, `number` ) VALUES ( 1, 'Abe', '8675309' )").unwrap();
        run(&mut db, "INSERT INTO `phone` ( `id`, `name`, `number` ) VALUES ( 2, 'Bob', '1234567' )").unwrap();
        assert_eq!(run(&mut db, "INSERT INTO `phone` ( `id`, `name` ) VALUES ( 2, 'Dup' )"), Err(ERROR_FUNCTION_FAILED));
        assert_eq!(texts(&run(&mut db, "SELECT `name` FROM `phone`").unwrap()), ["Abe", "Bob", "Cindy"]);
        assert_eq!(texts(&run(&mut db, "SELECT `name`, `id` FROM `phone` WHERE `id` > 1 AND `name` <> 'Bob'").unwrap()), ["Cindy|3"]);
        run(&mut db, "UPDATE `phone` SET `number` = '555' WHERE `name` = 'Abe'").unwrap();
        assert_eq!(texts(&run(&mut db, "SELECT `number` FROM `phone` WHERE `id` = 1").unwrap()), ["555"]);
        run(&mut db, "DELETE FROM `phone` WHERE `id` = 2").unwrap();
        assert_eq!(texts(&run(&mut db, "SELECT * FROM `phone`").unwrap()), ["1|Abe|555", "3|Cindy|2937550"]);
        assert_eq!(texts(&run(&mut db, "SELECT `Name` FROM `_Tables`").unwrap()), ["phone"]);
        assert_eq!(texts(&run(&mut db, "SELECT `Name`, `Type` FROM `_Columns`").unwrap()), ["id|13570", "name|7456", "number|7456"]);
        assert_eq!(View::new(&db, "SELECT * FROM `nope`").err(), Some(ERROR_BAD_QUERY_SYNTAX));
        assert_eq!(View::new(&db, "SELECT `nope` FROM `phone`").err(), Some(ERROR_BAD_QUERY_SYNTAX));
    }

    #[test]
    fn join_and_order() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `A` (`k` CHAR(10) NOT NULL, `v` INT PRIMARY KEY `k`)").unwrap();
        run(&mut db, "CREATE TABLE `B` (`k` CHAR(10) NOT NULL, `a` CHAR(10) PRIMARY KEY `k`)").unwrap();
        for (k, v) in [("x", 3), ("y", 1), ("z", 2)] {
            run(&mut db, &format!("INSERT INTO `A` (`k`, `v`) VALUES ('{k}', {v})")).unwrap();
        }
        for (k, a) in [("b1", "z"), ("b2", "x"), ("b3", "q")] {
            run(&mut db, &format!("INSERT INTO `B` (`k`, `a`) VALUES ('{k}', '{a}')")).unwrap();
        }
        let r = run(&mut db, "SELECT `B`.`k`, `A`.`v` FROM `A`, `B` WHERE `A`.`k` = `B`.`a` ORDER BY `v`").unwrap();
        assert_eq!(texts(&r), ["b1|2", "b2|3"]);
        let r = run(&mut db, "SELECT DISTINCT `a` FROM `B` WHERE `a` <> 'q'").unwrap();
        assert_eq!(texts(&r), ["z", "x"]);
    }

    #[test]
    fn params_and_modify() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `T` (`k` INT NOT NULL, `s` CHAR(0), `b` OBJECT PRIMARY KEY `k`)").unwrap();
        let mut v = View::new(&db, "INSERT INTO `T` (`k`, `s`, `b`) VALUES (?, ?, ?)").unwrap();
        let mut p = Record::new(3);
        p.set(1, Field::Int(7)).unwrap();
        p.set(2, Field::Str("seven".into())).unwrap();
        p.set(3, Field::Stream(b"\x01\x02".to_vec())).unwrap();
        v.execute(&mut db, Some(&p)).unwrap();
        let mut v = View::new(&db, "SELECT `k`, `s`, `b` FROM `T`").unwrap();
        v.execute(&mut db, None).unwrap();
        let mut r = v.fetch(&db).unwrap();
        assert_eq!(r.get(3), &Field::Stream(vec![1, 2]));
        r.set(2, Field::Str("SEVEN".into())).unwrap();
        v.modify(&mut db, MSIMODIFY_UPDATE, &mut r).unwrap();
        r.set(1, Field::Int(8)).unwrap();
        v.modify(&mut db, MSIMODIFY_INSERT, &mut r).unwrap();
        assert_eq!(v.modify(&mut db, MSIMODIFY_INSERT, &mut r), Err(ERROR_FUNCTION_FAILED));
        let rs = run(&mut db, "SELECT `k`, `s` FROM `T`").unwrap();
        assert_eq!(texts(&rs), ["7|SEVEN", "8|SEVEN"]);
        // saved and loaded again
        let bytes = db.to_bytes().unwrap();
        let mut e = Database::open(Box::new(std::io::Cursor::new(bytes))).unwrap();
        let rs = run(&mut e, "SELECT `k`, `s`, `b` FROM `T` WHERE `k` = 8").unwrap();
        assert_eq!(rs[0].get(3), &Field::Stream(vec![1, 2]));
        assert_eq!(texts(&run(&mut e, "SELECT `Name` FROM `_Streams`").unwrap()), ["T.7", "T.8"]);
    }

    #[test]
    fn temporary() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `T` (`k` INT NOT NULL TEMPORARY, `s` CHAR(0) TEMPORARY PRIMARY KEY `k`) HOLD").unwrap();
        run(&mut db, "INSERT INTO `T` (`k`, `s`) VALUES (1, 'gone')").unwrap();
        run(&mut db, "CREATE TABLE `P` (`k` INT NOT NULL, `s` CHAR(0) PRIMARY KEY `k`)").unwrap();
        run(&mut db, "INSERT INTO `P` (`k`, `s`) VALUES (1, 'kept')").unwrap();
        run(&mut db, "INSERT INTO `P` (`k`, `s`) VALUES (2, 'temp') TEMPORARY").unwrap();
        let mut e = Database::open(Box::new(std::io::Cursor::new(db.to_bytes().unwrap()))).unwrap();
        assert!(e.table("T").is_none());
        assert_eq!(texts(&run(&mut e, "SELECT `s` FROM `P`").unwrap()), ["kept"]);
        assert!(e.pool.find("temp").is_none());
        run(&mut db, "ALTER TABLE `T` FREE").unwrap();
        assert!(db.table("T").is_none());
    }
}
