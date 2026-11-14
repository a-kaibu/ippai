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

static NULL: Field = Field::Null;

/// The view's tables looked up once per execute.
struct Bound<'a> {
    db: &'a Database,
    /// per source: its table (None: a system table)
    tabs: Vec<Option<&'a Table>>,
}

/// An operand with its column resolved: (source, column) or a value.
enum COp {
    Col(usize, usize),
    Val(Field),
}

/// A WHERE expression resolved for one execute.
enum CExpr {
    And(Box<CExpr>, Box<CExpr>),
    Or(Box<CExpr>, Box<CExpr>),
    /// (operand, op, operand, compared as strings)
    Cmp(COp, Op, COp, bool),
    IsNull(usize, usize, bool),
}

fn conjuncts<'e>(e: &'e CExpr, out: &mut Vec<&'e CExpr>) {
    if let CExpr::And(a, b) = e {
        conjuncts(a, out);
        conjuncts(b, out);
    } else {
        out.push(e);
    }
}

#[derive(Default)]
struct Index {
    s: HashMap<String, Vec<usize>>,
    n: HashMap<Option<i64>, Vec<usize>>,
}

enum Plan {
    All(usize),
    List(Vec<usize>),
    /// the rows whose column equals (source, column) of the row already bound
    Index { ix: Index, other: (usize, usize), string: bool },
}

/// A field as the comparisons see it, borrowed from the table where it can be.
enum Fv<'a> {
    Int(i32),
    Str(&'a str),
    Ref(&'a Field),
    Owned(Field),
}

impl Fv<'_> {
    fn is_null(&self) -> bool {
        match self {
            Fv::Int(_) => false,
            Fv::Str(s) => s.is_empty(),
            Fv::Ref(f) => f.is_null(),
            Fv::Owned(f) => f.is_null(),
        }
    }
    fn text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Fv::Int(i) => i.to_string().into(),
            Fv::Str(s) => (*s).into(),
            Fv::Ref(Field::Str(s)) | Fv::Owned(Field::Str(s)) => s.as_str().into(),
            Fv::Ref(f) => f.text().into(),
            Fv::Owned(f) => f.text().into(),
        }
    }
    fn num(&self) -> Option<i64> {
        match self {
            Fv::Int(i) => num(&Field::Int(*i)),
            Fv::Str(s) => s.trim().parse().ok(),
            Fv::Ref(f) => num(f),
            Fv::Owned(f) => num(f),
        }
    }
    fn field(self) -> Field {
        match self {
            Fv::Int(i) => Field::Int(i),
            Fv::Str(s) => Field::Str(s.to_string()),
            Fv::Ref(f) => f.clone(),
            Fv::Owned(f) => f,
        }
    }
}

/// A WHERE comparison (strings compare for equality only; NULLs are equal numbers).
fn compare(fa: &Fv, op: Op, fb: &Fv, string: bool) -> bool {
    if string {
        let (x, y) = (fa.text(), fb.text());
        match op {
            Op::Eq => x == y,
            Op::Ne => x != y,
            _ => false,
        }
    } else {
        match (fa.num(), fb.num()) {
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
    /// (ERROR_FUNCTION_FAILED: a table of the view was dropped or altered since.)
    pub fn columns(&self, db: &Database) -> R<Vec<(String, u16)>> {
        self.cols.iter().map(|&(s, c)| {
            let col = self.srcs[s].cols(db).get(c).ok_or(ERROR_FUNCTION_FAILED)?;
            Ok((col.name.clone(), col.ty))
        }).collect()
    }

    pub fn column_tables(&self) -> Vec<String> {
        self.cols.iter().map(|&(s, _)| self.srcs[s].name().to_string()).collect()
    }

    /// The tables of the sources as they are now, with every column the view names checked
    /// against them (a table dropped or altered by another view since `new`).
    fn bind<'a>(&self, db: &'a Database) -> R<Bound<'a>> {
        let mut tabs = Vec::with_capacity(self.srcs.len());
        for s in &self.srcs {
            tabs.push(match s {
                Src::Db(n) => Some(db.table(n).ok_or(ERROR_FUNCTION_FAILED)?),
                Src::Virt { .. } => None,
            });
        }
        let b = Bound { db, tabs };
        for &(s, c) in &self.cols {
            if c >= self.srcs[s].cols(db).len() {
                return Err(ERROR_FUNCTION_FAILED);
            }
        }
        Ok(b)
    }

    /// The expression with its columns resolved and its parameters filled in.
    fn compile(&self, db: &Database, e: &Expr, p: &Option<&Record>) -> R<CExpr> {
        Ok(match e {
            Expr::And(a, b) => CExpr::And(Box::new(self.compile(db, a, p)?), Box::new(self.compile(db, b, p)?)),
            Expr::Or(a, b) => CExpr::Or(Box::new(self.compile(db, a, p)?), Box::new(self.compile(db, b, p)?)),
            Expr::IsNull(c, null) => {
                let (s, ci) = self.resolve(db, c).map_err(|_| ERROR_FUNCTION_FAILED)?;
                CExpr::IsNull(s, ci, *null)
            }
            Expr::Cmp(a, op, b) => {
                let mut kinds = [None, None];
                let mut ops = Vec::with_capacity(2);
                for (k, o) in [a, b].into_iter().enumerate() {
                    ops.push(match o {
                        Operand::Val(v) => COp::Val(val_field(v, p)),
                        Operand::Col(c) => {
                            let (s, ci) = self.resolve(db, c).map_err(|_| ERROR_FUNCTION_FAILED)?;
                            kinds[k] = Some(self.srcs[s].cols(db)[ci].ty & T_STRING != 0);
                            COp::Col(s, ci)
                        }
                    });
                }
                let [sa, sb] = kinds;
                let string = sa.or(sb).unwrap_or(false) && sa != Some(false) && sb != Some(false);
                let b = ops.pop().unwrap();
                let a = ops.pop().unwrap();
                CExpr::Cmp(a, *op, b, string)
            }
        })
    }

    fn operand<'a>(&'a self, bd: &Bound<'a>, o: &'a COp, rows: &[usize]) -> Option<Fv<'a>> {
        match o {
            COp::Val(f) => Some(Fv::Ref(f)),
            COp::Col(s, c) => Some(self.value(bd, *s, *rows.get(*s)?, *c)),
        }
    }

    /// A field of a source row, borrowed where it can be (no String per string column).
    fn value<'a>(&'a self, bd: &Bound<'a>, s: usize, r: usize, c: usize) -> Fv<'a> {
        match (&self.srcs[s], bd.tabs[s]) {
            (Src::Db(_), Some(t)) => {
                let Some(row) = t.rows.get(r) else { return Fv::Ref(&NULL) };
                let col = &t.cols[c];
                let v = row.v[c];
                if v == 0 {
                    Fv::Ref(&NULL)
                } else if col.is_binary() {
                    Fv::Owned(bd.db.field(t, row, c))
                } else if col.ty & T_STRING != 0 {
                    bd.db.pool.get(v).map_or(Fv::Ref(&NULL), Fv::Str)
                } else {
                    Fv::Int(int_of(col, v))
                }
            }
            (Src::Virt { name, rows, .. }, _) => {
                if c == 1 && (name == "_Streams" || name == "_Storages") {
                    return Fv::Owned(self.srcs[s].field(bd.db, r, c));
                }
                rows.get(r).and_then(|row| row.get(c)).map_or(Fv::Ref(&NULL), Fv::Ref)
            }
            (Src::Db(_), None) => Fv::Ref(&NULL),
        }
    }

    /// The expression's value for bound rows (None: a table it needs is not bound yet).
    fn eval(&self, bd: &Bound, e: &CExpr, rows: &[usize]) -> Option<bool> {
        match e {
            CExpr::And(a, b) => match (self.eval(bd, a, rows), self.eval(bd, b, rows)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            CExpr::Or(a, b) => match (self.eval(bd, a, rows), self.eval(bd, b, rows)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            CExpr::IsNull(s, c, null) => {
                let r = *rows.get(*s)?;
                Some(self.value(bd, *s, r, *c).is_null() == *null)
            }
            CExpr::Cmp(a, op, b, string) => {
                let fa = self.operand(bd, a, rows)?;
                let fb = self.operand(bd, b, rows)?;
                Some(compare(&fa, *op, &fb, *string))
            }
        }
    }

    /// How each source's rows are met in the join: all of them, the ones a constant
    /// equality lets through, or (an equality with an earlier source's column) the rows of a
    /// hash index on the value. The rows keep their table order, so the result is in the
    /// same order as the nested loop over every row.
    fn plan(&self, bd: &Bound, cond: &Option<CExpr>) -> Vec<Plan> {
        let mut conj = Vec::new();
        if let Some(e) = cond {
            conjuncts(e, &mut conj);
        }
        (0..self.srcs.len()).map(|k| {
            let n = self.srcs[k].len(bd.db);
            let mut konst = None;
            for e in &conj {
                let CExpr::Cmp(a, Op::Eq, b, string) = e else { continue };
                match (a, b) {
                    (COp::Col(sa, ca), COp::Col(sb, cb)) if (*sa == k && *sb < k) || (*sb == k && *sa < k) => {
                        let ((_, mine), other) = if *sa == k { ((sa, *ca), (*sb, *cb)) } else { ((sb, *cb), (*sa, *ca)) };
                        let mut ix = Index::default();
                        for r in 0..n {
                            let v = self.value(bd, k, r, mine);
                            if *string {
                                ix.s.entry(v.text().into_owned()).or_default().push(r);
                            } else {
                                ix.n.entry(v.num()).or_default().push(r);
                            }
                        }
                        return Plan::Index { ix, other, string: *string };
                    }
                    (COp::Col(s, c), COp::Val(f)) | (COp::Val(f), COp::Col(s, c)) if *s == k && konst.is_none() => {
                        let want = Fv::Ref(f);
                        konst = Some((0..n).filter(|&r| compare(&self.value(bd, k, r, *c), Op::Eq, &want, *string)).collect());
                    }
                    _ => {}
                }
            }
            konst.map_or(Plan::All(n), Plan::List)
        }).collect()
    }

    fn matching(&self, db: &Database, cond: &Option<Expr>, p: &Option<&Record>) -> R<Vec<Vec<usize>>> {
        let bd = self.bind(db)?;
        let cond = cond.as_ref().map(|e| self.compile(db, e, p)).transpose()?;
        let plan = self.plan(&bd, &cond);
        let mut out = Vec::new();
        let mut cur = Vec::new();
        self.join(&bd, &cond, &plan, &mut cur, &mut out);
        Ok(out)
    }

    fn join(&self, bd: &Bound, cond: &Option<CExpr>, plan: &[Plan], cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        let k = cur.len();
        if k == self.srcs.len() {
            if cond.as_ref().is_none_or(|e| self.eval(bd, e, cur) == Some(true)) {
                out.push(cur.clone());
            }
            return;
        }
        let mut each = |r: usize, cur: &mut Vec<usize>| {
            cur.push(r);
            if cond.as_ref().is_none_or(|e| self.eval(bd, e, cur) != Some(false)) {
                self.join(bd, cond, plan, cur, out);
            }
            cur.pop();
        };
        match &plan[k] {
            Plan::All(n) => (0..*n).for_each(|r| each(r, cur)),
            Plan::List(v) => v.iter().for_each(|&r| each(r, cur)),
            Plan::Index { ix, other, string } => {
                let v = self.value(bd, other.0, cur[other.0], other.1);
                let hits = if *string { ix.s.get(v.text().as_ref()) } else { ix.n.get(&v.num()) };
                if let Some(hits) = hits {
                    hits.iter().for_each(|&r| each(r, cur));
                }
            }
        }
    }

    /// ORDER BY's sort key of a source row: the raw value (string ids, as Windows sorts) or
    /// the field's text.
    fn raw(&self, bd: &Bound, s: usize, r: usize, c: usize) -> (u32, String) {
        match (&self.srcs[s], bd.tabs[s]) {
            (Src::Db(_), Some(t)) => (t.rows.get(r).map_or(0, |row| row.v[c]), String::new()),
            (Src::Db(_), None) => (0, String::new()),
            (src @ Src::Virt { .. }, _) => src.raw(bd.db, r, c),
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
                let mut res = self.matching(db, &cond, &p)?;
                let bd = self.bind(db)?;
                if !order.is_empty() {
                    let mut keys = Vec::with_capacity(order.len());
                    for c in &order {
                        keys.push(self.resolve(db, c).map_err(|_| ERROR_FUNCTION_FAILED)?);
                    }
                    // (each row's key once; a stable sort keeps the join's order for ties)
                    let mut keyed: Vec<(Vec<(u32, String)>, Vec<usize>)> = res.into_iter().map(|r| (keys.iter().map(|&(s, c)| self.raw(&bd, s, r[s], c)).collect(), r)).collect();
                    keyed.sort_by(|a, b| a.0.cmp(&b.0));
                    res = keyed.into_iter().map(|k| k.1).collect();
                }
                if distinct {
                    let mut seen = std::collections::HashSet::<Vec<Field>>::new();
                    res.retain(|r| seen.insert(self.cols.iter().map(|&(s, c)| self.value(&bd, s, r[s], c).field()).collect()));
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
                let rows = self.matching(db, &cond, &p)?;
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
                let rows = self.matching(db, &cond, &p)?;
                let vals: Vec<Field> = sets.iter().map(|(_, v)| val_field(v, &p)).collect();
                // (a key changing moves rows: update by key values, last first)
                let mut keys = Vec::new();
                for r in &rows {
                    keys.push(db.tables[ti].rows[r[0]].v.clone());
                }
                let keyed = db.tables[ti].keys().next().is_some();
                for k in keys {
                    // (keys are unique and the rows sorted by them: a binary search finds the row)
                    let found = if keyed { db.find_key(ti, &k).err().filter(|&i| db.tables[ti].rows[i].v == k) } else { None };
                    let found = found.or_else(|| db.tables[ti].rows.iter().position(|r| r.v == k));
                    let Some(ri) = found else { continue };
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
                let rows = self.matching(db, &cond, &p)?;
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
                db.add_column(ti, Col { name: col.name.clone(), ty: col.ty });
                if hold {
                    db.tables[ti].hold += 1;
                }
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
            if self.cols.iter().any(|&(s, c)| c >= self.srcs[s].cols(db).len()) {
                return Err(ERROR_FUNCTION_FAILED);
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
        if self.cols.iter().any(|&(_, c)| c >= db.tables[ti].cols.len()) {
            return Err(ERROR_FUNCTION_FAILED);
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
            // (an empty string is a NULL)
            if let Field::Str(s) = f {
                if !s.is_empty() && crate::record::parse_int(s).is_none() {
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

    /// '' into an integer column is a NULL; other text is a type mismatch (Wine's package
    /// tests insert Signature rows with '' sizes and dates).
    #[test]
    fn empty_string_in_int_column() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `S` (`k` CHAR(10) NOT NULL, `n` LONG, `m` SHORT PRIMARY KEY `k`)").unwrap();
        run(&mut db, "INSERT INTO `S` (`k`, `n`, `m`) VALUES ('a', '', '')").unwrap();
        run(&mut db, "INSERT INTO `S` (`k`, `n`, `m`) VALUES ('b', '12', 3)").unwrap();
        assert_eq!(run(&mut db, "INSERT INTO `S` (`k`, `n`) VALUES ('c', 'x')"), Err(ERROR_DATATYPE_MISMATCH));
        assert_eq!(texts(&run(&mut db, "SELECT * FROM `S`").unwrap()), ["a||", "b|12|3"]);
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

    /// A table of a view dropped or altered by another view before the first executes
    /// (MsiDatabaseOpenView, then DROP TABLE through a second view) is ERROR_FUNCTION_FAILED
    /// from MsiViewExecute / MsiViewGetColumnInfo / MsiViewFetch, not a panic (review b04).
    #[test]
    fn stale_views_fail() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `T` (`k` INT NOT NULL, `s` CHAR(0), `x` CHAR(0) PRIMARY KEY `k`)").unwrap();
        run(&mut db, "INSERT INTO `T` (`k`, `s`, `x`) VALUES (1, 'a', 'b')").unwrap();
        let mut sel = View::new(&db, "SELECT `k`, `x` FROM `T` WHERE `s` = 'a' ORDER BY `x`").unwrap();
        let mut fetched = View::new(&db, "SELECT `x` FROM `T`").unwrap();
        fetched.execute(&mut db, None).unwrap();
        run(&mut db, "DROP TABLE `T`").unwrap();
        assert_eq!(sel.columns(&db), Err(ERROR_FUNCTION_FAILED));
        assert_eq!(sel.execute(&mut db, None), Err(ERROR_FUNCTION_FAILED));
        // (the same name again, with fewer columns)
        run(&mut db, "CREATE TABLE `T` (`k` INT NOT NULL PRIMARY KEY `k`)").unwrap();
        run(&mut db, "INSERT INTO `T` (`k`) VALUES (1)").unwrap();
        assert_eq!(sel.execute(&mut db, None), Err(ERROR_FUNCTION_FAILED));
        assert_eq!(fetched.fetch(&db).err(), Some(ERROR_FUNCTION_FAILED));
        let mut upd = View::new(&db, "UPDATE `T` SET `k` = 2 WHERE `k` = 1").unwrap();
        run(&mut db, "DROP TABLE `T`").unwrap();
        assert!(upd.execute(&mut db, None).is_err());
    }

    /// A File x Component x Directory style join of 20000 rows each: the equalities go
    /// through hash indexes (the nested loop was 8e12 row combinations).
    #[test]
    fn large_equality_join() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `F` (`File` CHAR(72) NOT NULL, `Component_` CHAR(72) NOT NULL, `Size` LONG PRIMARY KEY `File`)").unwrap();
        run(&mut db, "CREATE TABLE `C` (`Component` CHAR(72) NOT NULL, `Directory_` CHAR(72) NOT NULL PRIMARY KEY `Component`)").unwrap();
        run(&mut db, "CREATE TABLE `D` (`Directory` CHAR(72) NOT NULL, `Seq` LONG PRIMARY KEY `Directory`)").unwrap();
        let n = 20000;
        let ins = |db: &mut Database, sql: &str, f: &[Field]| {
            let mut v = View::new(db, sql).unwrap();
            let mut r = Record::new(f.len());
            for (i, x) in f.iter().enumerate() {
                r.set(i + 1, x.clone()).unwrap();
            }
            v.execute(db, Some(&r)).unwrap();
        };
        for i in 0..n {
            ins(&mut db, "INSERT INTO `F` (`File`, `Component_`, `Size`) VALUES (?, ?, ?)", &[Field::Str(format!("f{i}")), Field::Str(format!("c{}", i / 2)), Field::Int(i)]);
            ins(&mut db, "INSERT INTO `C` (`Component`, `Directory_`) VALUES (?, ?)", &[Field::Str(format!("c{i}")), Field::Str(format!("d{}", i % 100))]);
            ins(&mut db, "INSERT INTO `D` (`Directory`, `Seq`) VALUES (?, ?)", &[Field::Str(format!("d{i}")), Field::Int(i % 7)]);
        }
        let t = std::time::Instant::now();
        let r = run(&mut db, "SELECT `File`, `Directory`, `Seq` FROM `F`, `C`, `D` WHERE `F`.`Component_` = `C`.`Component` AND `C`.`Directory_` = `D`.`Directory` AND `Size` >= 10 ORDER BY `Seq`").unwrap();
        assert_eq!(r.len(), n as usize - 10);
        assert!(r.windows(2).all(|w| w[0].get(3).int() <= w[1].get(3).int()));
        assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
        // UPDATE over all rows by key (was a linear search per row)
        run(&mut db, "UPDATE `F` SET `Size` = 0 WHERE `Size` > 0").unwrap();
        assert_eq!(run(&mut db, "SELECT `File` FROM `F` WHERE `Size` = 0").unwrap().len(), n as usize);
    }

    mod model {
        use super::*;
        use proptest::prelude::*;

        /// A value of one generated column: strings from a small set (with '' = NULL), or
        /// integers (None = NULL).
        #[derive(Clone, Debug)]
        enum V {
            S(&'static str),
            N(Option<i32>),
        }
        const STRS: &[&str] = &["", "a", "b", "c", "ab"];

        /// The columns of each table: 0 the key (a string), 1 a string, 2 an integer.
        fn table() -> impl Strategy<Value = Vec<(u8, V, V)>> {
            prop::collection::vec((0u8..12, prop::sample::select(STRS).prop_map(V::S), prop::option::of(-2i32..3).prop_map(V::N)), 0..7)
        }

        #[derive(Clone, Debug)]
        enum E {
            And(Box<E>, Box<E>),
            Or(Box<E>, Box<E>),
            /// (table, column, op, table, column)
            ColCol(usize, usize, Op, usize, usize),
            /// (table, column, op, the constant: an index into STRS or an integer)
            ColVal(usize, usize, Op, i32),
            IsNull(usize, usize, bool),
        }

        fn op(string: bool) -> BoxedStrategy<Op> {
            if string {
                prop::sample::select(vec![Op::Eq, Op::Ne]).boxed()
            } else {
                prop::sample::select(vec![Op::Eq, Op::Ne, Op::Lt, Op::Gt, Op::Le, Op::Ge]).boxed()
            }
        }

        fn leaf(nt: usize) -> BoxedStrategy<E> {
            let col = (0..nt, 0usize..3);
            prop_oneof![
                (col.clone(), 0..nt, any::<bool>()).prop_flat_map(|((t, c), u, x)| {
                    // (columns of the same kind: 0 and 1 strings, 2 integers)
                    let string = c < 2;
                    let d = if string { if x { 0 } else { 1 } } else { 2 };
                    op(string).prop_map(move |o| E::ColCol(t, c, o, u, d))
                }),
                (col.clone(), -2i32..5).prop_flat_map(|((t, c), v)| op(c < 2).prop_map(move |o| E::ColVal(t, c, o, if c < 2 { v.rem_euclid(STRS.len() as i32) } else { v }))),
                (col, any::<bool>()).prop_map(|((t, c), n)| E::IsNull(t, c, n)),
            ]
            .boxed()
        }

        fn expr(nt: usize) -> impl Strategy<Value = E> {
            leaf(nt).prop_recursive(3, 12, 2, |inner| prop_oneof![(inner.clone(), inner.clone()).prop_map(|(a, b)| E::And(Box::new(a), Box::new(b))), (inner.clone(), inner).prop_map(|(a, b)| E::Or(Box::new(a), Box::new(b)))])
        }

        const COLS: [&str; 3] = ["k", "s", "n"];
        fn name(t: usize) -> String {
            format!("T{t}")
        }
        fn opsql(o: Op) -> &'static str {
            match o {
                Op::Eq => "=",
                Op::Ne => "<>",
                Op::Lt => "<",
                Op::Gt => ">",
                Op::Le => "<=",
                Op::Ge => ">=",
            }
        }
        fn sql(e: &E) -> String {
            match e {
                E::And(a, b) => format!("({} AND {})", sql(a), sql(b)),
                E::Or(a, b) => format!("({} OR {})", sql(a), sql(b)),
                E::ColCol(t, c, o, u, d) => format!("`{}`.`{}` {} `{}`.`{}`", name(*t), COLS[*c], opsql(*o), name(*u), COLS[*d]),
                E::ColVal(t, c, o, v) if *c < 2 => format!("`{}`.`{}` {} '{}'", name(*t), COLS[*c], opsql(*o), STRS[*v as usize]),
                E::ColVal(t, c, o, v) => format!("`{}`.`{}` {} {}", name(*t), COLS[*c], opsql(*o), v),
                E::IsNull(t, c, n) => format!("`{}`.`{}` IS {}NULL", name(*t), COLS[*c], if *n { "" } else { "NOT " }),
            }
        }

        fn cmp_model(a: &V, o: Op, b: &V) -> bool {
            match (a, b) {
                (V::S(x), V::S(y)) => match o {
                    Op::Eq => x == y,
                    Op::Ne => x != y,
                    _ => false,
                },
                (V::N(x), V::N(y)) => match (x, y) {
                    (Some(x), Some(y)) => match o {
                        Op::Eq => x == y,
                        Op::Ne => x != y,
                        Op::Lt => x < y,
                        Op::Gt => x > y,
                        Op::Le => x <= y,
                        Op::Ge => x >= y,
                    },
                    (None, None) => matches!(o, Op::Eq | Op::Le | Op::Ge),
                    _ => o == Op::Ne,
                },
                _ => unreachable!(),
            }
        }

        fn eval_model(e: &E, row: &[(String, V, V)]) -> bool {
            let get = |t: usize, c: usize| -> V {
                let r = &row[t];
                match c {
                    0 => V::S(STRS.iter().copied().find(|s| *s == r.0).unwrap_or_else(|| Box::leak(r.0.clone().into_boxed_str()))),
                    1 => r.1.clone(),
                    _ => r.2.clone(),
                }
            };
            match e {
                E::And(a, b) => eval_model(a, row) && eval_model(b, row),
                E::Or(a, b) => eval_model(a, row) || eval_model(b, row),
                E::ColCol(t, c, o, u, d) => cmp_model(&get(*t, *c), *o, &get(*u, *d)),
                E::ColVal(t, c, o, v) => cmp_model(&get(*t, *c), *o, &if *c < 2 { V::S(STRS[*v as usize]) } else { V::N(Some(*v)) }),
                E::IsNull(t, c, n) => {
                    let null = match get(*t, *c) {
                        V::S(s) => s.is_empty(),
                        V::N(x) => x.is_none(),
                    };
                    null == *n
                }
            }
        }

        fn show(v: &V) -> String {
            match v {
                V::S(s) => s.to_string(),
                V::N(Some(i)) => i.to_string(),
                V::N(None) => String::new(),
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(400))]
            /// SELECT over 1-3 tables with any WHERE: the same rows as a model that tries every
            /// combination of rows, and in the order of the nested loop over the tables in
            /// FROM order (each table in key order); the hash-indexed join and the filtered
            /// candidate lists may not drop, add or reorder a row.
            #[test]
            fn select_matches_the_model(tabs in prop::collection::vec(table(), 1..4).prop_flat_map(|t| { let n = t.len(); (Just(t), prop::option::of(expr(n))) })) {
                let (tabs, cond) = tabs;
                let mut db = Database::new();
                let mut rows: Vec<Vec<(String, V, V)>> = Vec::new();
                for (t, rs) in tabs.iter().enumerate() {
                    run(&mut db, &format!("CREATE TABLE `{}` (`k` CHAR(8) NOT NULL, `s` CHAR(8), `n` INT PRIMARY KEY `k`)", name(t))).unwrap();
                    let mut kept = Vec::new();
                    for (k, s, n) in rs {
                        let k = format!("k{k}");
                        let (V::S(sv), V::N(nv)) = (s, n) else { unreachable!() };
                        let nsql = nv.map_or("NULL".to_string(), |i| i.to_string());
                        if run(&mut db, &format!("INSERT INTO `{}` (`k`, `s`, `n`) VALUES ('{k}', '{sv}', {nsql})", name(t))).is_ok() {
                            kept.push((k, s.clone(), n.clone()));
                        }
                    }
                    rows.push(kept);
                }
                // (each table's rows in the order the table keeps them: by key)
                let order: Vec<Vec<usize>> = (0..tabs.len()).map(|t| {
                    let tb = db.table(&name(t)).unwrap();
                    tb.rows.iter().map(|r| rows[t].iter().position(|x| db.pool.get(r.v[0]) == Some(x.0.as_str())).unwrap()).collect()
                }).collect();
                let mut want = Vec::new();
                let mut idx = vec![0usize; tabs.len()];
                'all: loop {
                    if order.iter().all(|o| !o.is_empty()) {
                        let row: Vec<(String, V, V)> = (0..tabs.len()).map(|t| rows[t][order[t][idx[t]]].clone()).collect();
                        if cond.as_ref().is_none_or(|e| eval_model(e, &row)) {
                            want.push(row.iter().map(|r| format!("{}|{}|{}", r.0, show(&r.1), show(&r.2))).collect::<Vec<_>>().join("|"));
                        }
                    } else {
                        break;
                    }
                    for t in (0..tabs.len()).rev() {
                        idx[t] += 1;
                        if idx[t] < order[t].len() {
                            continue 'all;
                        }
                        idx[t] = 0;
                    }
                    break;
                }
                let from = (0..tabs.len()).map(|t| format!("`{}`", name(t))).collect::<Vec<_>>().join(", ");
                let q = format!("SELECT * FROM {from}{}", cond.as_ref().map_or(String::new(), |e| format!(" WHERE {}", sql(e))));
                let got = texts(&run(&mut db, &q).unwrap());
                prop_assert_eq!(got, want, "{}", q);
            }
        }
    }

    /// A table or column named "" (SQL CREATE TABLE ``, or a damaged file's string id past
    /// the pool) makes MsiDatabaseCommit fail, not panic (review b04).
    #[test]
    fn empty_names_do_not_save() {
        let mut db = Database::new();
        if run(&mut db, "CREATE TABLE `` (`k` INT NOT NULL PRIMARY KEY `k`)").is_ok() {
            assert_eq!(db.to_bytes().err(), Some(ERROR_FUNCTION_FAILED));
        }
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `T` (`k` INT NOT NULL PRIMARY KEY `k`)").unwrap();
        db.tables[0].name.clear();
        assert_eq!(db.to_bytes().err(), Some(ERROR_FUNCTION_FAILED));
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `T` (`k` INT NOT NULL PRIMARY KEY `k`)").unwrap();
        db.tables[0].cols[0].name.clear();
        assert_eq!(db.to_bytes().err(), Some(ERROR_FUNCTION_FAILED));
    }

    /// A file whose rows are not in key order (written by another tool): it opens with its
    /// rows sorted, so keyed lookups, duplicate checks and UPDATE find them.
    #[test]
    fn unsorted_rows_are_sorted_on_open() {
        let mut db = Database::new();
        run(&mut db, "CREATE TABLE `T` (`k` INT NOT NULL, `s` CHAR(0) PRIMARY KEY `k`)").unwrap();
        for i in 0..50 {
            run(&mut db, &format!("INSERT INTO `T` (`k`, `s`) VALUES ({i}, 'v{i}')")).unwrap();
        }
        db.tables[0].rows.reverse();
        let mut e = Database::open(Box::new(std::io::Cursor::new(db.to_bytes().unwrap()))).unwrap();
        for i in 0..50 {
            assert_eq!(run(&mut e, &format!("INSERT INTO `T` (`k`, `s`) VALUES ({i}, 'dup')")), Err(ERROR_FUNCTION_FAILED));
        }
        run(&mut e, "UPDATE `T` SET `s` = 'x' WHERE `k` >= 0").unwrap();
        assert_eq!(texts(&run(&mut e, "SELECT `k` FROM `T` WHERE `s` = 'x'").unwrap()).len(), 50);
        let mut v = View::new(&e, "SELECT `k`, `s` FROM `T`").unwrap();
        v.execute(&mut e, None).unwrap();
        let mut r = v.fetch(&e).unwrap();
        v.modify(&mut e, MSIMODIFY_SEEK, &mut r).unwrap();
    }
}
