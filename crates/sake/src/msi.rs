#![allow(non_snake_case)]
//! msi.dll: Windows Installer. The database, its SQL views, records, summary information,
//! MsiFormatRecord and conditions are crates/msidb; this is the handle table and the A/W
//! entry points over guest memory, and (msi_pkg.rs) packages and installing.
//!
//! Handles are small numbers per process, as on Windows (MSIHANDLE). A string result goes
//! to (buffer, *size in characters): the size is set to the length without the NUL; a
//! buffer that is too small gets what fits and ERROR_MORE_DATA.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use msidb::record::{Field, Record};
use msidb::suminfo::{Prop, SummaryInfo};
use msidb::*;

use crate::api::{implement, Ctx};
use crate::k32::arg_str;
use crate::mem::{self, r32, w32};

pub type Db = Arc<Mutex<DbObj>>;

pub struct DbObj {
    pub db: Database,
    /// the file it was opened from / is committed to (Windows path)
    pub path: String,
    pub out: String,
    pub readonly: bool,
    /// MSIDBSTATE_WRITE: opened for writing
    pub writable: bool,
    /// MSIDBOPEN_DIRECT / CREATEDIRECT: changes go to the file without MsiDatabaseCommit
    /// (written when the last handle closes)
    pub direct: bool,
    /// the output file was made at open (MSIDBOPEN_CREATE, a separate output file) and goes
    /// when the database closes without MsiDatabaseCommit
    pub temp_out: bool,
}

impl Drop for DbObj {
    fn drop(&mut self) {
        if self.direct && self.db.dirty {
            let _ = commit(self);
        } else if self.temp_out {
            if let Some(h) = crate::files::resolve(&self.out) {
                let _ = kura::fs::remove_file(h);
            }
        }
    }
}

pub enum Obj {
    Db(Db),
    View(Db, Arc<Mutex<View>>),
    Rec(Record),
    Sum { db: Option<Db>, info: SummaryInfo, max: u32, path: String, changed: bool },
    Pkg(Arc<Mutex<crate::msi_pkg::Package>>),
}

struct Table {
    next: u32,
    objs: BTreeMap<u32, Arc<Mutex<Obj>>>,
}

static HANDLES: Mutex<Table> = Mutex::new(Table { next: 1, objs: BTreeMap::new() });

pub fn new_handle(o: Obj) -> u32 {
    let mut t = HANDLES.lock().unwrap();
    let h = t.next;
    t.next += 1;
    t.objs.insert(h, Arc::new(Mutex::new(o)));
    h
}

pub fn get(h: u32) -> Option<Arc<Mutex<Obj>>> {
    HANDLES.lock().unwrap().objs.get(&h).cloned()
}

pub fn close(h: u32) -> bool {
    HANDLES.lock().unwrap().objs.remove(&h).is_some()
}

/// Runs `f` on a record handle's record.
fn with_rec<T>(h: u32, f: impl FnOnce(&mut Record) -> T) -> Option<T> {
    let o = get(h)?;
    let mut o = o.lock().unwrap();
    match &mut *o {
        Obj::Rec(r) => Some(f(r)),
        _ => None,
    }
}

pub fn rec_of(h: u32) -> Option<Record> {
    with_rec(h, |r| r.clone())
}

pub fn db_of(h: u32) -> Option<Db> {
    let o = get(h)?;
    let o = o.lock().unwrap();
    match &*o {
        Obj::Db(d) => Some(d.clone()),
        Obj::Pkg(p) => Some(p.lock().unwrap().db.clone()),
        _ => None,
    }
}

/// A string result into (buf, *pcch) in characters (ANSI: bytes).
pub fn out_str(buf: u32, pcch: u32, s: &str, wide: bool) -> u32 {
    if pcch == 0 {
        return if buf != 0 { ERROR_INVALID_PARAMETER } else { ERROR_SUCCESS };
    }
    let cap = r32(pcch);
    let len = if wide {
        let v: Vec<u16> = s.encode_utf16().collect();
        if buf != 0 && cap > 0 {
            let n = v.len().min(cap as usize - 1);
            for (i, &c) in v[..n].iter().enumerate() {
                mem::w16(buf + 2 * i as u32, c);
            }
            mem::w16(buf + 2 * n as u32, 0);
        }
        v.len()
    } else {
        let v = crate::text::s2a(s);
        if buf != 0 && cap > 0 {
            let n = v.len().min(cap as usize - 1);
            mem::write(buf, &v[..n]);
            mem::w8(buf + n as u32, 0);
        }
        v.len()
    };
    w32(pcch, len as u32);
    if buf != 0 && len as u32 >= cap { ERROR_MORE_DATA } else { ERROR_SUCCESS }
}

fn opt_str(p: u32, wide: bool) -> Option<String> {
    (p != 0).then(|| arg_str(p, wide))
}

fn status(r: R<()>) -> u32 {
    match r {
        Ok(()) => ERROR_SUCCESS,
        Err(e) => e,
    }
}

// ---- handles

fn MsiCloseHandle(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h == 0 || close(h) { ERROR_SUCCESS } else { ERROR_INVALID_HANDLE }
}

fn MsiCloseAllHandles(_c: &mut Ctx) -> u32 {
    let mut t = HANDLES.lock().unwrap();
    let n = t.objs.len() as u32;
    t.objs.clear();
    n
}

// ---- records

fn MsiCreateRecord(c: &mut Ctx) -> u32 {
    let n = c.a(0);
    if n > 65535 {
        return 0;
    }
    new_handle(Obj::Rec(Record::new(n as usize)))
}

fn MsiRecordGetFieldCount(c: &mut Ctx) -> u32 {
    with_rec(c.a(0), |r| r.count() as u32).unwrap_or(u32::MAX)
}

fn MsiRecordGetInteger(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    with_rec(c.a(0), |r| if i > r.count() { NULL_INT } else { r.get(i).int() }).unwrap_or(NULL_INT) as u32
}

fn MsiRecordSetInteger(c: &mut Ctx) -> u32 {
    let (i, v) = (c.a(1) as usize, c.i(2));
    with_rec(c.a(0), |r| status(r.set(i, if v == NULL_INT { Field::Null } else { Field::Int(v) }))).unwrap_or(ERROR_INVALID_HANDLE)
}

fn record_set_string(c: &mut Ctx, wide: bool) -> u32 {
    let i = c.a(1) as usize;
    let s = opt_str(c.a(2), wide).unwrap_or_default();
    with_rec(c.a(0), |r| status(r.set(i, Field::Str(s)))).unwrap_or(ERROR_INVALID_HANDLE)
}
fn MsiRecordSetStringA(c: &mut Ctx) -> u32 {
    record_set_string(c, false)
}
fn MsiRecordSetStringW(c: &mut Ctx) -> u32 {
    record_set_string(c, true)
}

fn record_get_string(c: &mut Ctx, wide: bool) -> u32 {
    let i = c.a(1) as usize;
    let Some(f) = with_rec(c.a(0), |r| r.get(i).clone()) else { return ERROR_INVALID_HANDLE };
    if let Field::Stream(_) = f {
        return ERROR_INVALID_DATATYPE;
    }
    out_str(c.a(2), c.a(3), &f.text(), wide)
}
fn MsiRecordGetStringA(c: &mut Ctx) -> u32 {
    record_get_string(c, false)
}
fn MsiRecordGetStringW(c: &mut Ctx) -> u32 {
    record_get_string(c, true)
}

fn MsiRecordIsNull(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    with_rec(c.a(0), |r| (i > r.count() || r.get(i).is_null()) as u32).unwrap_or(0)
}

fn MsiRecordDataSize(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    with_rec(c.a(0), |r| r.data_size(i)).unwrap_or(0)
}

fn MsiRecordClearData(c: &mut Ctx) -> u32 {
    with_rec(c.a(0), |r| {
        r.clear();
        ERROR_SUCCESS
    })
    .unwrap_or(ERROR_INVALID_HANDLE)
}

fn MsiRecordReadStream(c: &mut Ctx) -> u32 {
    let (i, buf, pcb) = (c.a(1) as usize, c.a(2), c.a(3));
    if pcb == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    with_rec(c.a(0), |r| {
        if i > r.count() {
            return ERROR_INVALID_PARAMETER;
        }
        let Field::Stream(d) = r.get(i) else {
            return if r.get(i).is_null() {
                w32(pcb, 0);
                ERROR_SUCCESS
            } else {
                ERROR_INVALID_DATATYPE
            };
        };
        if buf == 0 {
            w32(pcb, d.len() as u32);
            return ERROR_SUCCESS;
        }
        let got = r.read_stream(i, r32(pcb) as usize).unwrap_or_default();
        mem::write(buf, &got);
        w32(pcb, got.len() as u32);
        ERROR_SUCCESS
    })
    .unwrap_or(ERROR_INVALID_HANDLE)
}

fn record_set_stream(c: &mut Ctx, wide: bool) -> u32 {
    let i = c.a(1) as usize;
    let data = match opt_str(c.a(2), wide) {
        Some(p) => match crate::files::resolve(&p).and_then(|h| kura::fs::read(h).ok()) {
            Some(d) => Some(d),
            None => return ERROR_FILE_NOT_FOUND,
        },
        None => None,
    };
    with_rec(c.a(0), |r| {
        if i == 0 || i > r.count() {
            return ERROR_INVALID_PARAMETER;
        }
        match data {
            Some(d) => status(r.set(i, Field::Stream(d))),
            None => {
                // (NULL path: rewind the stream already there)
                if !matches!(r.get(i), Field::Stream(_)) {
                    return ERROR_INVALID_PARAMETER;
                }
                r.pos[i] = 0;
                ERROR_SUCCESS
            }
        }
    })
    .unwrap_or(ERROR_INVALID_HANDLE)
}
fn MsiRecordSetStreamA(c: &mut Ctx) -> u32 {
    record_set_stream(c, false)
}
fn MsiRecordSetStreamW(c: &mut Ctx) -> u32 {
    record_set_stream(c, true)
}

fn format_record(c: &mut Ctx, wide: bool) -> u32 {
    let (hinst, hrec, buf, pcch) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(rec) = rec_of(hrec) else { return ERROR_INVALID_HANDLE };
    if buf != 0 && pcch == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let s = match crate::msi_pkg::pkg_of(hinst) {
        Some(p) => p.lock().unwrap().format_record(&rec),
        None if hinst != 0 => return ERROR_INVALID_HANDLE,
        None => format::format_record(&rec, &format::NoPackage),
    };
    out_str(buf, pcch, &s, wide)
}
fn MsiFormatRecordA(c: &mut Ctx) -> u32 {
    format_record(c, false)
}
fn MsiFormatRecordW(c: &mut Ctx) -> u32 {
    format_record(c, true)
}

// ---- databases

pub const MSIDBOPEN_READONLY: u32 = 0;
pub const MSIDBOPEN_TRANSACT: u32 = 1;
pub const MSIDBOPEN_DIRECT: u32 = 2;
pub const MSIDBOPEN_CREATE: u32 = 3;
pub const MSIDBOPEN_CREATEDIRECT: u32 = 4;
pub const MSIDBOPEN_PATCHFILE: u32 = 32;

/// Opens a database file (Windows path) the way MsiOpenDatabase does.
pub fn open_db(path: &str, mode: u32, out: Option<String>) -> R<DbObj> {
    let m = mode & !MSIDBOPEN_PATCHFILE;
    if m == MSIDBOPEN_CREATE || m == MSIDBOPEN_CREATEDIRECT {
        let mut db = Database::new();
        if mode & MSIDBOPEN_PATCHFILE != 0 {
            db.clsid = msidb::db::CLSID_PATCH;
        }
        let o = DbObj { db, path: path.to_string(), out: path.to_string(), readonly: false, writable: true, direct: m == MSIDBOPEN_CREATEDIRECT, temp_out: m == MSIDBOPEN_CREATE };
        // (the file exists from the start, as on Windows)
        commit(&o)?;
        return Ok(o);
    }
    if m > MSIDBOPEN_CREATEDIRECT && out.is_none() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let h = crate::files::resolve(path).ok_or(ERROR_OPEN_FAILED)?;
    if kura::fs::metadata(&h).map(|m| m.is_dir()).unwrap_or(true) {
        return Err(ERROR_OPEN_FAILED);
    }
    let f = kura::fs::File::open(&h).map_err(|_| ERROR_OPEN_FAILED)?;
    let db = Database::open(Box::new(f)).map_err(|e| if e == ERROR_INSTALL_PACKAGE_INVALID { ERROR_OPEN_FAILED } else { e })?;
    if mode & MSIDBOPEN_PATCHFILE == 0 && db.clsid == msidb::db::CLSID_PATCH {
        return Err(ERROR_OPEN_FAILED);
    }
    let readonly = m == MSIDBOPEN_READONLY && out.is_none();
    let temp_out = out.is_some();
    let o = DbObj { db, path: path.to_string(), out: out.unwrap_or_else(|| path.to_string()), readonly, writable: !readonly, direct: m == MSIDBOPEN_DIRECT, temp_out };
    // (a separate output file exists from the start, as on Windows)
    if temp_out {
        commit(&o)?;
    }
    Ok(o)
}

/// Writes the database to its output file (via a temporary file beside it).
pub fn commit(o: &DbObj) -> R<()> {
    let target = crate::files::resolve_new(&o.out);
    let tmp = format!("{target}.sake-tmp");
    let f = kura::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&tmp).map_err(|_| ERROR_OPEN_FAILED)?;
    let r = o.db.save(f);
    match r {
        Ok(_) => kura::fs::rename(&tmp, &target).map_err(|_| ERROR_FUNCTION_FAILED),
        Err(e) => {
            let _ = kura::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn open_database(c: &mut Ctx, wide: bool) -> u32 {
    let (p, persist, out) = (c.a(0), c.a(1), c.a(2));
    if p == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let path = arg_str(p, wide);
    // (a pointer as the mode: the file to commit to)
    let (mode, to) = if persist > 0xffff { (MSIDBOPEN_TRANSACT, Some(arg_str(persist, wide))) } else { (persist, None) };
    match open_db(&path, mode, to) {
        Ok(o) => {
            w32(out, new_handle(Obj::Db(Arc::new(Mutex::new(o)))));
            ERROR_SUCCESS
        }
        Err(e) => e,
    }
}
fn MsiOpenDatabaseA(c: &mut Ctx) -> u32 {
    open_database(c, false)
}
fn MsiOpenDatabaseW(c: &mut Ctx) -> u32 {
    open_database(c, true)
}

fn MsiDatabaseCommit(c: &mut Ctx) -> u32 {
    let Some(d) = db_of(c.a(0)) else { return ERROR_INVALID_HANDLE };
    let mut o = d.lock().unwrap();
    if o.readonly {
        return ERROR_SUCCESS;
    }
    let r = commit(&o);
    if r.is_ok() {
        o.temp_out = false;
        o.db.dirty = false;
    }
    status(r)
}

fn MsiGetDatabaseState(c: &mut Ctx) -> u32 {
    match db_of(c.a(0)) {
        Some(d) => d.lock().unwrap().writable as u32,
        None => u32::MAX, // MSIDBSTATE_ERROR
    }
}

fn open_view(c: &mut Ctx, wide: bool) -> u32 {
    let (h, q, out) = (c.a(0), c.a(1), c.a(2));
    let Some(d) = db_of(h) else { return ERROR_INVALID_HANDLE };
    if q == 0 || out == 0 {
        return ERROR_BAD_QUERY_SYNTAX;
    }
    let sql = arg_str(q, wide);
    let v = View::new(&d.lock().unwrap().db, &sql);
    match v {
        Ok(v) => {
            w32(out, new_handle(Obj::View(d, Arc::new(Mutex::new(v)))));
            ERROR_SUCCESS
        }
        Err(e) => e,
    }
}
fn MsiDatabaseOpenViewA(c: &mut Ctx) -> u32 {
    open_view(c, false)
}
fn MsiDatabaseOpenViewW(c: &mut Ctx) -> u32 {
    open_view(c, true)
}

fn view_of(h: u32) -> Option<(Db, Arc<Mutex<View>>)> {
    let o = get(h)?;
    let o = o.lock().unwrap();
    match &*o {
        Obj::View(d, v) => Some((d.clone(), v.clone())),
        _ => None,
    }
}

fn MsiViewExecute(c: &mut Ctx) -> u32 {
    let Some((d, v)) = view_of(c.a(0)) else { return ERROR_INVALID_HANDLE };
    let p = match c.a(1) {
        0 => None,
        h => match rec_of(h) {
            Some(r) => Some(r),
            None => return ERROR_INVALID_HANDLE,
        },
    };
    let mut o = d.lock().unwrap();
    let readonly = o.readonly;
    let mut v = v.lock().unwrap();
    if readonly && !matches!(v.stmt, msidb::sql::Stmt::Select { .. }) {
        return ERROR_FUNCTION_FAILED;
    }
    status(v.execute(&mut o.db, p.as_ref()))
}

fn MsiViewFetch(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some((d, v)) = view_of(h) else { return ERROR_INVALID_HANDLE };
    let o = d.lock().unwrap();
    let r = v.lock().unwrap().fetch(&o.db);
    match r {
        Ok(mut rec) => {
            // (field 0 of a fetched record: the view it came from, as on Windows)
            rec.f[0] = Field::Int(h as i32 | 0x10000);
            w32(out, new_handle(Obj::Rec(rec)));
            ERROR_SUCCESS
        }
        Err(e) => {
            w32(out, 0);
            e
        }
    }
}

fn MsiViewClose(c: &mut Ctx) -> u32 {
    let Some((_, v)) = view_of(c.a(0)) else { return ERROR_INVALID_HANDLE };
    v.lock().unwrap().close();
    ERROR_SUCCESS
}

fn MsiViewModify(c: &mut Ctx) -> u32 {
    let (h, mode, hrec) = (c.a(0), c.i(1), c.a(2));
    let Some((d, v)) = view_of(h) else { return ERROR_INVALID_HANDLE };
    let Some(ro) = get(hrec) else { return ERROR_INVALID_HANDLE };
    let mut ro = ro.lock().unwrap();
    let Obj::Rec(rec) = &mut *ro else { return ERROR_INVALID_HANDLE };
    let mut o = d.lock().unwrap();
    if o.readonly && !matches!(mode, view::MSIMODIFY_SEEK | view::MSIMODIFY_REFRESH | view::MSIMODIFY_VALIDATE..=view::MSIMODIFY_VALIDATE_DELETE) {
        return ERROR_ACCESS_DENIED;
    }
    let r = v.lock().unwrap().modify(&mut o.db, mode, rec);
    status(r)
}

fn MsiViewGetColumnInfo(c: &mut Ctx) -> u32 {
    let (h, kind, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    if kind > 1 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some((d, v)) = view_of(h) else { return ERROR_INVALID_HANDLE };
    let o = d.lock().unwrap();
    let cols = match v.lock().unwrap().columns(&o.db) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut r = Record::new(cols.len());
    for (i, (name, ty)) in cols.iter().enumerate() {
        r.f[i + 1] = Field::Str(if kind == 0 { name.clone() } else { idt::type_text(*ty) });
    }
    w32(out, new_handle(Obj::Rec(r)));
    ERROR_SUCCESS
}

fn view_get_error(c: &mut Ctx, wide: bool) -> u32 {
    let (h, buf, pcch) = (c.a(0), c.a(1), c.a(2));
    if pcch == 0 {
        return u32::MAX - 1; // MSIDBERROR_INVALIDARG
    }
    let Some((_, v)) = view_of(h) else { return u32::MAX - 1 };
    let e = v.lock().unwrap().error.clone();
    match e {
        Some((col, code)) => {
            if out_str(buf, pcch, &col, wide) == ERROR_MORE_DATA {
                return u32::MAX - 0; // MSIDBERROR_MOREDATA (-2 is invalid arg)
            }
            code as u32
        }
        None => {
            out_str(buf, pcch, "", wide);
            0 // MSIDBERROR_NOERROR
        }
    }
}
fn MsiViewGetErrorA(c: &mut Ctx) -> u32 {
    view_get_error(c, false)
}
fn MsiViewGetErrorW(c: &mut Ctx) -> u32 {
    view_get_error(c, true)
}

fn primary_keys(c: &mut Ctx, wide: bool) -> u32 {
    let (h, t, out) = (c.a(0), c.a(1), c.a(2));
    let Some(d) = db_of(h) else { return ERROR_INVALID_HANDLE };
    if t == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let name = arg_str(t, wide);
    let o = d.lock().unwrap();
    let Some(tb) = o.db.table(&name) else { return ERROR_INVALID_TABLE };
    let keys: Vec<String> = tb.keys().map(|k| tb.cols[k].name.clone()).collect();
    let mut r = Record::new(keys.len());
    r.f[0] = Field::Str(name);
    for (i, k) in keys.into_iter().enumerate() {
        r.f[i + 1] = Field::Str(k);
    }
    w32(out, new_handle(Obj::Rec(r)));
    ERROR_SUCCESS
}
fn MsiDatabaseGetPrimaryKeysA(c: &mut Ctx) -> u32 {
    primary_keys(c, false)
}
fn MsiDatabaseGetPrimaryKeysW(c: &mut Ctx) -> u32 {
    primary_keys(c, true)
}

fn table_persistent(c: &mut Ctx, wide: bool) -> u32 {
    // MSICONDITION_FALSE 0, TRUE 1, NONE 2, ERROR 3
    let (h, t) = (c.a(0), c.a(1));
    let Some(d) = db_of(h) else { return 3 };
    if t == 0 {
        return 3;
    }
    let name = arg_str(t, wide);
    let o = d.lock().unwrap();
    match o.db.table(&name) {
        Some(tb) => tb.persistent as u32,
        None if view::VIRTUAL.contains(&name.as_str()) => 1,
        None => 2,
    }
}
fn MsiDatabaseIsTablePersistentA(c: &mut Ctx) -> u32 {
    table_persistent(c, false)
}
fn MsiDatabaseIsTablePersistentW(c: &mut Ctx) -> u32 {
    table_persistent(c, true)
}

fn join(folder: &str, name: &str) -> String {
    if folder.ends_with('\\') { format!("{folder}{name}") } else { format!("{folder}\\{name}") }
}

fn db_export(c: &mut Ctx, wide: bool) -> u32 {
    let (h, t, folder, file) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(d) = db_of(h) else { return ERROR_INVALID_HANDLE };
    if t == 0 || folder == 0 || file == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let (t, folder, file) = (arg_str(t, wide), arg_str(folder, wide), arg_str(file, wide));
    let mut o = d.lock().unwrap();
    let (text, files) = if t == "_SummaryInformation" {
        (Vec::new(), Vec::new())
    } else {
        match idt::export(&mut o.db, &t) {
            Ok(x) => x,
            Err(_) => return ERROR_FUNCTION_FAILED,
        }
    };
    let h = crate::files::resolve_new(&join(&folder, &file));
    if kura::fs::write(&h, text).is_err() {
        return ERROR_FUNCTION_FAILED;
    }
    for (rel, data) in files {
        let p = join(&folder, &rel.replace('/', "\\"));
        let dir = p.rsplit_once('\\').unwrap().0.to_string();
        let _ = kura::fs::create_dir_all(crate::files::resolve_new(&dir));
        let _ = kura::fs::write(crate::files::resolve_new(&p), data);
    }
    ERROR_SUCCESS
}
fn MsiDatabaseExportA(c: &mut Ctx) -> u32 {
    db_export(c, false)
}
fn MsiDatabaseExportW(c: &mut Ctx) -> u32 {
    db_export(c, true)
}

fn db_import(c: &mut Ctx, wide: bool) -> u32 {
    let (h, folder, file) = (c.a(0), c.a(1), c.a(2));
    let Some(d) = db_of(h) else { return ERROR_INVALID_HANDLE };
    if folder == 0 || file == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let (folder, file) = (arg_str(folder, wide), arg_str(file, wide));
    let Some(data) = crate::files::resolve(&join(&folder, &file)).and_then(|h| kura::fs::read(h).ok()) else { return ERROR_FUNCTION_FAILED };
    let mut o = d.lock().unwrap();
    // (binary fields: files in a folder named after the archive file, or after the table)
    let stem = file.rsplit(['\\', '/']).next().unwrap_or(&file).rsplit_once('.').map_or(file.as_str(), |x| x.0).to_string();
    let read = |table: &str, name: &str| {
        [&stem, table].iter().find_map(|d| crate::files::resolve(&join(&folder, &format!("{d}\\{name}"))).and_then(|h| kura::fs::read(h).ok()))
    };
    match idt::import(&mut o.db, &data, &read) {
        Ok(()) => ERROR_SUCCESS,
        Err(_) => ERROR_FUNCTION_FAILED,
    }
}
fn MsiDatabaseImportA(c: &mut Ctx) -> u32 {
    db_import(c, false)
}
fn MsiDatabaseImportW(c: &mut Ctx) -> u32 {
    db_import(c, true)
}

// ---- summary information

fn get_summary(c: &mut Ctx, wide: bool) -> u32 {
    let (h, path, max, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let (db, path) = if h != 0 {
        match db_of(h) {
            Some(d) => (Some(d), String::new()),
            None => return ERROR_INVALID_HANDLE,
        }
    } else {
        let Some(p) = opt_str(path, wide) else { return ERROR_INVALID_PARAMETER };
        (None, p)
    };
    let info = match &db {
        Some(d) => d.lock().unwrap().db.summary.as_deref().map(SummaryInfo::parse).unwrap_or_default(),
        None => match open_db(&path, MSIDBOPEN_READONLY, None) {
            Ok(o) => o.db.summary.as_deref().map(SummaryInfo::parse).unwrap_or_default(),
            Err(ERROR_OPEN_FAILED) => return ERROR_INSTALL_PACKAGE_OPEN_FAILED,
            Err(e) => return e,
        },
    };
    w32(out, new_handle(Obj::Sum { db, info, max, path, changed: false }));
    ERROR_SUCCESS
}
fn MsiGetSummaryInformationA(c: &mut Ctx) -> u32 {
    get_summary(c, false)
}
fn MsiGetSummaryInformationW(c: &mut Ctx) -> u32 {
    get_summary(c, true)
}

fn MsiSummaryInfoGetPropertyCount(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    let Some(o) = get(h) else { return ERROR_INVALID_HANDLE };
    let o = o.lock().unwrap();
    let Obj::Sum { info, .. } = &*o else { return ERROR_INVALID_HANDLE };
    if out != 0 {
        w32(out, info.props.len() as u32);
    }
    ERROR_SUCCESS
}

fn sum_get(c: &mut Ctx, wide: bool) -> u32 {
    let (h, pid, pty, pint, pft, buf, pcch) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let Some(o) = get(h) else { return ERROR_INVALID_HANDLE };
    let o = o.lock().unwrap();
    let Obj::Sum { info, .. } = &*o else { return ERROR_INVALID_HANDLE };
    if pid > suminfo::MAX_PID {
        return ERROR_UNKNOWN_PROPERTY;
    }
    let p = info.props.get(&pid);
    if pty != 0 {
        w32(pty, p.map_or(suminfo::VT_EMPTY, |p| p.vt()));
    }
    match p {
        Some(Prop::I2(v)) => {
            if pint != 0 {
                w32(pint, *v as i32 as u32);
            }
        }
        Some(Prop::I4(v)) => {
            if pint != 0 {
                w32(pint, *v as u32);
            }
        }
        Some(Prop::Time(t)) => {
            if pft != 0 {
                mem::w64(pft, *t);
            }
        }
        Some(Prop::Str(s)) => return out_str(buf, pcch, s, wide),
        None => {}
    }
    ERROR_SUCCESS
}
fn MsiSummaryInfoGetPropertyA(c: &mut Ctx) -> u32 {
    sum_get(c, false)
}
fn MsiSummaryInfoGetPropertyW(c: &mut Ctx) -> u32 {
    sum_get(c, true)
}

fn sum_set(c: &mut Ctx, wide: bool) -> u32 {
    let (h, pid, ty, iv, pft, s) = (c.a(0), c.a(1), c.a(2), c.i(3), c.a(4), c.a(5));
    let Some(o) = get(h) else { return ERROR_INVALID_HANDLE };
    let mut o = o.lock().unwrap();
    let Obj::Sum { info, max, changed, .. } = &mut *o else { return ERROR_INVALID_HANDLE };
    let Some(want) = suminfo::type_of(pid) else { return ERROR_UNKNOWN_PROPERTY };
    if ty == suminfo::VT_EMPTY {
        return ERROR_DATATYPE_MISMATCH;
    }
    if ty != want {
        return ERROR_DATATYPE_MISMATCH;
    }
    let p = match ty {
        suminfo::VT_I2 => Prop::I2(iv as i16),
        suminfo::VT_I4 => Prop::I4(iv),
        suminfo::VT_FILETIME => {
            if pft == 0 {
                return ERROR_INVALID_PARAMETER;
            }
            Prop::Time(mem::r64(pft))
        }
        _ => {
            if s == 0 {
                return ERROR_INVALID_PARAMETER;
            }
            Prop::Str(arg_str(s, wide))
        }
    };
    // (an update count of 0: read only)
    if *max == 0 {
        return ERROR_FUNCTION_FAILED;
    }
    let r = info.set(pid, p);
    *changed = true;
    status(r)
}
fn MsiSummaryInfoSetPropertyA(c: &mut Ctx) -> u32 {
    sum_set(c, false)
}
fn MsiSummaryInfoSetPropertyW(c: &mut Ctx) -> u32 {
    sum_set(c, true)
}

fn MsiSummaryInfoPersist(c: &mut Ctx) -> u32 {
    let Some(o) = get(c.a(0)) else { return ERROR_INVALID_HANDLE };
    let o = o.lock().unwrap();
    let Obj::Sum { db, info, path, max, .. } = &*o else { return ERROR_INVALID_HANDLE };
    if *max == 0 {
        return ERROR_FUNCTION_FAILED;
    }
    match db {
        Some(d) => {
            let mut d = d.lock().unwrap();
            if d.readonly {
                return ERROR_FUNCTION_FAILED;
            }
            d.db.summary = Some(info.serialize());
            d.db.dirty = true;
            ERROR_SUCCESS
        }
        None => match open_db(path, MSIDBOPEN_DIRECT, None) {
            Ok(mut d) => {
                d.db.summary = Some(info.serialize());
                status(commit(&d))
            }
            Err(e) => e,
        },
    }
}

pub fn init() {
    macro_rules! reg { ($($f:ident),* $(,)?) => { $( implement("msi", stringify!($f), $f); )* }; }
    reg!(
        MsiCloseHandle, MsiCloseAllHandles, MsiCreateRecord, MsiRecordGetFieldCount, MsiRecordGetInteger, MsiRecordSetInteger,
        MsiRecordSetStringA, MsiRecordSetStringW, MsiRecordGetStringA, MsiRecordGetStringW, MsiRecordIsNull, MsiRecordDataSize,
        MsiRecordClearData, MsiRecordReadStream, MsiRecordSetStreamA, MsiRecordSetStreamW, MsiFormatRecordA, MsiFormatRecordW,
        MsiOpenDatabaseA, MsiOpenDatabaseW, MsiDatabaseCommit, MsiGetDatabaseState, MsiDatabaseOpenViewA, MsiDatabaseOpenViewW,
        MsiViewExecute, MsiViewFetch, MsiViewClose, MsiViewModify, MsiViewGetColumnInfo, MsiViewGetErrorA, MsiViewGetErrorW,
        MsiDatabaseGetPrimaryKeysA, MsiDatabaseGetPrimaryKeysW, MsiDatabaseIsTablePersistentA, MsiDatabaseIsTablePersistentW,
        MsiDatabaseExportA, MsiDatabaseExportW, MsiDatabaseImportA, MsiDatabaseImportW,
        MsiGetSummaryInformationA, MsiGetSummaryInformationW, MsiSummaryInfoGetPropertyCount, MsiSummaryInfoGetPropertyA,
        MsiSummaryInfoGetPropertyW, MsiSummaryInfoSetPropertyA, MsiSummaryInfoSetPropertyW, MsiSummaryInfoPersist,
    );
    crate::msi_pkg::init();
}
