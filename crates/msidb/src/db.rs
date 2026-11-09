//! The database: tables of raw values over the string pool, streams and storages, loaded
//! from and saved to a compound file.

use crate::names;
use crate::pool::Pool;
use crate::record::Field;
use crate::*;
use std::collections::BTreeMap;
use std::io::{self, Read, Seek, Write};
use std::sync::Mutex;

pub const T_SIZE: u16 = 0x00ff;
pub const T_VALID: u16 = 0x0100;
pub const T_LOCALIZABLE: u16 = 0x0200;
pub const T_NONBINARY: u16 = 0x0400;
pub const T_STRING: u16 = 0x0800;
pub const T_NULLABLE: u16 = 0x1000;
pub const T_KEY: u16 = 0x2000;
pub const T_TEMPORARY: u16 = 0x4000;
pub const T_UNKNOWN: u16 = 0x8000;

/// Package, patch and transform storages' class ids.
pub const CLSID_PACKAGE: [u8; 16] = clsid(0x000C1084);
pub const CLSID_PATCH: [u8; 16] = clsid(0x000C1086);
pub const CLSID_TRANSFORM: [u8; 16] = clsid(0x000C1082);
pub const CLSID_PACKAGE_OLD: [u8; 16] = clsid(0x000C1080);

const fn clsid(d1: u32) -> [u8; 16] {
    let b = d1.to_le_bytes();
    [b[0], b[1], b[2], b[3], 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]
}

pub const SUMMARY: &str = "\u{5}SummaryInformation";

#[derive(Clone, Debug, PartialEq)]
pub struct Col {
    pub name: String,
    pub ty: u16,
}

impl Col {
    pub fn is_binary(&self) -> bool {
        self.ty & !(T_NULLABLE | T_TEMPORARY | T_KEY) & 0x0fff == T_STRING | T_VALID && self.ty & T_NONBINARY == 0 && self.ty & T_SIZE == 0
    }
    pub fn is_string(&self) -> bool {
        self.ty & T_STRING != 0 && !self.is_binary()
    }
    pub fn is_key(&self) -> bool {
        self.ty & T_KEY != 0
    }
    pub fn nullable(&self) -> bool {
        self.ty & T_NULLABLE != 0
    }
    pub fn temporary(&self) -> bool {
        self.ty & T_TEMPORARY != 0
    }
    /// Bytes in the file (string references: 2, or 3 with long references).
    pub fn width(&self, long: bool) -> usize {
        if self.is_binary() {
            2
        } else if self.ty & T_STRING != 0 {
            if long { 3 } else { 2 }
        } else if self.ty & T_SIZE == 4 {
            4
        } else {
            2
        }
    }
    pub fn int4(&self) -> bool {
        self.ty & T_STRING == 0 && self.ty & T_SIZE == 4
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub v: Vec<u32>,
    pub persistent: bool,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    pub cols: Vec<Col>,
    pub rows: Vec<Row>,
    pub persistent: bool,
    /// HOLD count (ALTER TABLE ... HOLD/FREE, CREATE ... HOLD)
    pub hold: i32,
}

impl Table {
    pub fn col(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|c| c.name == name)
    }
    pub fn keys(&self) -> impl Iterator<Item = usize> + '_ {
        self.cols.iter().enumerate().filter(|c| c.1.is_key()).map(|c| c.0)
    }
}

pub enum Stream {
    Mem(Vec<u8>),
    /// in the source file at this (encoded) path
    File(String),
}

pub trait Source: Read + Seek + Send {}
impl<T: Read + Seek + Send> Source for T {}

pub struct Database {
    pub pool: Pool,
    pub tables: Vec<Table>,
    pub streams: BTreeMap<String, Stream>,
    /// sub-storages (nested packages, transforms): their compound files
    pub storages: BTreeMap<String, Stream>,
    pub summary: Option<Vec<u8>>,
    pub clsid: [u8; 16],
    src: Option<Mutex<cfb::CompoundFile<Box<dyn Source>>>>,
    /// changed since load (MsiDatabaseCommit has something to write)
    pub dirty: bool,
}

pub fn ioerr(_: io::Error) -> u32 {
    ERROR_OPEN_FAILED
}

fn rd_ref(b: &[u8], long: bool) -> u32 {
    if long { b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16 } else { b[0] as u32 | (b[1] as u32) << 8 }
}

fn read_stream<F: Read + Seek>(cf: &mut cfb::CompoundFile<F>, path: &str) -> R<Option<Vec<u8>>> {
    if !cf.is_stream(path) {
        return Ok(None);
    }
    let mut v = Vec::new();
    cf.open_stream(path).map_err(ioerr)?.read_to_end(&mut v).map_err(ioerr)?;
    Ok(Some(v))
}

/// A stream's name in the compound file: encoded, except the property sets ("\x05...").
pub fn stream_path(name: &str, table: bool) -> String {
    if name.starts_with('\u{5}') { name.to_string() } else { names::encode(name, table) }
}

fn tpath(name: &str) -> String {
    format!("/{}", names::encode(name, true))
}

impl Database {
    pub fn new() -> Database {
        let mut d = Database {
            pool: Pool::default(),
            tables: Vec::new(),
            streams: BTreeMap::new(),
            storages: BTreeMap::new(),
            summary: None,
            clsid: CLSID_PACKAGE,
            src: None,
            dirty: true,
        };
        d.pool.codepage = 0;
        d
    }

    /// Reads the tables of a compound file (streams are read when asked for).
    pub fn open(f: Box<dyn Source>) -> R<Database> {
        let mut cf = cfb::CompoundFile::open(f).map_err(|_| ERROR_INSTALL_PACKAGE_INVALID)?;
        let sp = read_stream(&mut cf, &tpath("_StringPool"))?.unwrap_or_default();
        let sd = read_stream(&mut cf, &tpath("_StringData"))?.unwrap_or_default();
        let mut pool = Pool::load(&sp, &sd)?;
        let long = pool.long_refs;
        let rw = if long { 3 } else { 2 };
        let mut tables = Vec::new();
        let tnames = read_stream(&mut cf, &tpath("_Tables"))?.unwrap_or_default();
        let cols = read_stream(&mut cf, &tpath("_Columns"))?.unwrap_or_default();
        // _Columns: Table, Number, Name, Type, column-major
        let ncols = cols.len() / (2 * rw + 4);
        let mut colrows: Vec<(u32, u16, u32, u16)> = (0..ncols)
            .map(|i| {
                let t = rd_ref(&cols[i * rw..], long);
                let o = ncols * rw;
                let num = u16::from_le_bytes([cols[o + 2 * i], cols[o + 2 * i + 1]]) ^ 0x8000;
                let o2 = o + ncols * 2;
                let name = rd_ref(&cols[o2 + i * rw..], long);
                let o3 = o2 + ncols * rw;
                let ty = u16::from_le_bytes([cols[o3 + 2 * i], cols[o3 + 2 * i + 1]]) ^ 0x8000;
                (t, num, name, ty)
            })
            .collect();
        colrows.sort_by_key(|c| (c.0, c.1));
        for i in 0..tnames.len() / rw {
            let id = rd_ref(&tnames[i * rw..], long);
            let name = pool.get(id).unwrap_or("").to_string();
            pool.addref(id, true);
            let mut tcols = Vec::new();
            for c in colrows.iter().filter(|c| c.0 == id) {
                pool.addref(c.0, true);
                pool.addref(c.2, true);
                tcols.push(Col { name: pool.get(c.2).unwrap_or("").to_string(), ty: c.3 });
            }
            let data = read_stream(&mut cf, &tpath(&name))?.unwrap_or_default();
            let rowsize: usize = tcols.iter().map(|c| c.width(long)).sum();
            let n = if rowsize == 0 { 0 } else { data.len() / rowsize };
            let mut rows = vec![Row { v: Vec::with_capacity(tcols.len()), persistent: true }; n];
            let mut at = 0;
            for c in &tcols {
                let w = c.width(long);
                for r in rows.iter_mut() {
                    let b = &data[at..at + w];
                    let v = match w {
                        4 => u32::from_le_bytes(b.try_into().unwrap()),
                        3 => rd_ref(b, true),
                        _ => rd_ref(b, false),
                    };
                    if c.is_string() && v != 0 {
                        pool.addref(v, true);
                    }
                    r.v.push(v);
                    at += w;
                }
            }
            tables.push(Table { name, cols: tcols, rows, persistent: true, hold: 0 });
        }
        pool.prune();
        let mut streams = BTreeMap::new();
        let mut storages = BTreeMap::new();
        let mut summary = None;
        let entries: Vec<(String, bool)> = cf.read_root_storage().map(|e| (e.name().to_string(), e.is_stream())).collect();
        for (raw, stream) in entries {
            let (name, table) = names::decode(&raw);
            if table {
                continue;
            }
            let p = format!("/{raw}");
            if !stream {
                storages.insert(name, Stream::File(p));
            } else if name == SUMMARY {
                summary = read_stream(&mut cf, &p)?;
            } else if !name.starts_with('\u{5}') {
                streams.insert(name, Stream::File(p));
            }
        }
        let mut clsid = [0u8; 16];
        clsid.copy_from_slice(cf.root_entry().clsid().as_bytes());
        let clsid = guid_bytes(&clsid);
        Ok(Database { pool, tables, streams, storages, summary, clsid, src: Some(Mutex::new(cf)), dirty: false })
    }

    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }
    pub fn table_index(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|t| t.name == name)
    }

    /// A stream's bytes.
    pub fn stream(&self, name: &str) -> Option<Vec<u8>> {
        match self.streams.get(name)? {
            Stream::Mem(v) => Some(v.clone()),
            Stream::File(p) => self.read_src(p),
        }
    }

    /// Reads a stream of the source file by its encoded path.
    pub fn read_src(&self, p: &str) -> Option<Vec<u8>> {
        let mut cf = self.src.as_ref()?.lock().unwrap();
        read_stream(&mut cf, p).ok().flatten()
    }

    /// Copies a stream to `w` without holding it all (cabinets of a few GB).
    pub fn copy_stream(&self, name: &str, w: &mut dyn Write) -> R<u64> {
        match self.streams.get(name).ok_or(ERROR_NOT_FOUND)? {
            Stream::Mem(v) => {
                w.write_all(v).map_err(ioerr)?;
                Ok(v.len() as u64)
            }
            Stream::File(p) => {
                let mut cf = self.src.as_ref().ok_or(ERROR_NOT_FOUND)?.lock().unwrap();
                let mut s = cf.open_stream(p).map_err(ioerr)?;
                io::copy(&mut s, w).map_err(ioerr)
            }
        }
    }

    pub fn stream_len(&self, name: &str) -> Option<u64> {
        match self.streams.get(name)? {
            Stream::Mem(v) => Some(v.len() as u64),
            Stream::File(p) => self.src.as_ref()?.lock().unwrap().entry(p).ok().map(|e| e.len()),
        }
    }

    /// A sub-storage as a compound file of its own.
    pub fn storage(&self, name: &str) -> Option<Vec<u8>> {
        match self.storages.get(name)? {
            Stream::Mem(v) => Some(v.clone()),
            Stream::File(p) => {
                let mut cf = self.src.as_ref()?.lock().unwrap();
                let entries: Vec<(String, bool, Vec<u8>)> = cf.walk_storage(p).ok()?.filter(|e| !e.is_root() && e.path() != std::path::Path::new(p)).map(|e| (e.path().to_string_lossy().into_owned(), e.is_stream(), e.clsid().as_bytes().to_vec())).collect();
                let root_clsid = cf.entry(p).ok()?.clsid().as_bytes().to_vec();
                let mut out = cfb::CompoundFile::create(io::Cursor::new(Vec::new())).ok()?;
                let _ = out.set_storage_clsid("/", uuid_from(&root_clsid));
                for (path, stream, cl) in entries {
                    let rel = &path[p.len()..];
                    if stream {
                        let mut data = Vec::new();
                        cf.open_stream(&path).ok()?.read_to_end(&mut data).ok()?;
                        out.create_stream(rel).ok()?.write_all(&data).ok()?;
                    } else {
                        out.create_storage(rel).ok()?;
                        let _ = out.set_storage_clsid(rel, uuid_from(&cl));
                    }
                }
                out.flush().ok()?;
                Some(out.into_inner().into_inner())
            }
        }
    }

    // ---- values

    /// A raw value as a record field (binary columns: the stream).
    pub fn field(&self, t: &Table, row: &Row, c: usize) -> Field {
        let col = &t.cols[c];
        let v = row.v[c];
        if v == 0 {
            return Field::Null;
        }
        if col.is_binary() {
            let name = self.stream_name(t, row);
            return match self.stream(&name) {
                Some(d) => Field::Stream(d),
                None => Field::Null,
            };
        }
        if col.ty & T_STRING != 0 {
            return match self.pool.get(v) {
                Some(s) => Field::Str(s.to_string()),
                None => Field::Null,
            };
        }
        Field::Int(int_of(col, v))
    }

    /// The raw value as a string or integer, for comparing (binary: whether set).
    pub fn value_str(&self, t: &Table, row: &Row, c: usize) -> Option<String> {
        let col = &t.cols[c];
        let v = row.v[c];
        if v == 0 {
            return None;
        }
        if col.ty & T_STRING != 0 && !col.is_binary() {
            return self.pool.get(v).map(str::to_string);
        }
        Some(int_of(col, v).to_string())
    }

    /// The stream a binary column of a row refers to: "Table.key1.key2".
    pub fn stream_name(&self, t: &Table, row: &Row) -> String {
        let mut s = t.name.clone();
        for k in t.keys() {
            s.push('.');
            s.push_str(&self.value_str(t, row, k).unwrap_or_default());
        }
        s
    }

    /// The raw value for a field (string ids are added with a reference).
    pub fn encode(&mut self, col: &Col, f: &Field, persistent: bool) -> R<u32> {
        Ok(match f {
            Field::Null => 0,
            _ if col.is_binary() => 1,
            Field::Str(s) if col.ty & T_STRING != 0 => self.pool.add(s, persistent),
            Field::Int(i) if col.ty & T_STRING != 0 => self.pool.add(&i.to_string(), persistent),
            Field::Int(i) => raw_int(col, *i),
            Field::Str(s) => match s.trim().parse::<i32>() {
                Ok(i) => raw_int(col, i),
                Err(_) if s.is_empty() => 0,
                Err(_) => return Err(ERROR_DATATYPE_MISMATCH),
            },
            Field::Stream(_) => return Err(ERROR_DATATYPE_MISMATCH),
        })
    }

    /// The raw value a field would have without adding it (None: a string not in the pool,
    /// so no row can match it).
    pub fn lookup(&self, col: &Col, f: &Field) -> Option<u32> {
        Some(match f {
            Field::Null => 0,
            _ if col.is_binary() => 1,
            Field::Str(s) if col.ty & T_STRING != 0 => {
                if s.is_empty() {
                    0
                } else {
                    self.pool.find(s)?
                }
            }
            Field::Int(i) if col.ty & T_STRING != 0 => self.pool.find(&i.to_string())?,
            Field::Int(i) => raw_int(col, *i),
            Field::Str(s) => raw_int(col, s.trim().parse().ok()?),
            Field::Stream(_) => return None,
        })
    }

    fn release_row(&mut self, ti: usize, row: &Row) {
        let t = &self.tables[ti];
        let ids: Vec<u32> = t.cols.iter().zip(&row.v).filter(|(c, &v)| c.is_string() && v != 0).map(|(_, &v)| v).collect();
        for id in ids {
            self.pool.release(id, row.persistent);
        }
    }

    // ---- rows

    /// Where a row with these key values goes (Ok: insert there; Err: a row has these keys).
    pub fn find_key(&self, ti: usize, v: &[u32]) -> Result<usize, usize> {
        let t = &self.tables[ti];
        let keys: Vec<usize> = t.keys().collect();
        t.rows.binary_search_by(|r| {
            for &k in &keys {
                match r.v[k].cmp(&v[k]) {
                    std::cmp::Ordering::Equal => continue,
                    o => return o,
                }
            }
            std::cmp::Ordering::Equal
        }).map_or_else(Ok, Err)
    }

    /// Inserts a row (fields 1..=columns of `rec`); fails on a row with the same keys.
    pub fn insert(&mut self, ti: usize, fields: &[Field], temporary: bool) -> R<usize> {
        let persistent = !temporary && self.tables[ti].persistent;
        let cols = self.tables[ti].cols.clone();
        // (find the place first: a duplicate must not leave string references behind)
        let mut probe = Vec::with_capacity(cols.len());
        for (c, f) in cols.iter().zip(fields) {
            probe.push(if c.is_key() { self.lookup(c, f).unwrap_or(u32::MAX) } else { 0 });
        }
        if !self.tables[ti].keys().any(|_| true) {
            // (a table without keys keeps rows in insert order)
        } else if probe.iter().all(|&p| p != u32::MAX) && self.find_key(ti, &probe).is_err() {
            return Err(ERROR_FUNCTION_FAILED);
        }
        let mut v = Vec::with_capacity(cols.len());
        for (c, f) in cols.iter().zip(fields) {
            let p = persistent && !c.temporary();
            v.push(self.encode(c, f, p)?);
        }
        let at = if self.tables[ti].keys().any(|_| true) {
            match self.find_key(ti, &v) {
                Ok(i) => i,
                Err(_) => {
                    // (cannot happen: checked above; undo the references)
                    let row = Row { v, persistent };
                    self.release_row(ti, &row);
                    return Err(ERROR_FUNCTION_FAILED);
                }
            }
        } else {
            self.tables[ti].rows.len()
        };
        for (c, f) in cols.iter().zip(fields) {
            if let (true, Field::Stream(d)) = (c.is_binary(), f) {
                let row = Row { v: v.clone(), persistent };
                let name = self.stream_name(&self.tables[ti], &row);
                self.streams.insert(name, Stream::Mem(d.clone()));
            }
        }
        self.tables[ti].rows.insert(at, Row { v, persistent });
        if persistent {
            self.dirty = true;
        }
        Ok(at)
    }

    pub fn delete(&mut self, ti: usize, ri: usize) {
        let row = self.tables[ti].rows.remove(ri);
        if self.tables[ti].cols.iter().any(|c| c.is_binary()) {
            let name = self.stream_name(&self.tables[ti], &row);
            self.streams.remove(&name);
        }
        self.release_row(ti, &row);
        if row.persistent {
            self.dirty = true;
        }
    }

    /// Sets column `c` of a row (keys may change: the row moves to its new place).
    pub fn set(&mut self, ti: usize, ri: usize, c: usize, f: &Field) -> R<usize> {
        let col = self.tables[ti].cols[c].clone();
        let persistent = self.tables[ti].rows[ri].persistent && !col.temporary();
        if col.is_binary() {
            let row = self.tables[ti].rows[ri].clone();
            let name = self.stream_name(&self.tables[ti], &row);
            match f {
                Field::Stream(d) => {
                    self.streams.insert(name, Stream::Mem(d.clone()));
                    self.tables[ti].rows[ri].v[c] = 1;
                }
                _ => {
                    self.streams.remove(&name);
                    self.tables[ti].rows[ri].v[c] = 0;
                }
            }
            self.dirty |= persistent;
            return Ok(ri);
        }
        let v = self.encode(&col, f, persistent)?;
        let old = std::mem::replace(&mut self.tables[ti].rows[ri].v[c], v);
        if col.is_string() && old != 0 {
            self.pool.release(old, persistent);
        }
        self.dirty |= persistent;
        if col.is_key() {
            let row = self.tables[ti].rows.remove(ri);
            let at = match self.find_key(ti, &row.v) {
                Ok(i) | Err(i) => i,
            };
            self.tables[ti].rows.insert(at, row);
            return Ok(at);
        }
        Ok(ri)
    }

    pub fn create_table(&mut self, name: &str, cols: Vec<Col>, persistent: bool) -> R<()> {
        if self.table(name).is_some() {
            return Err(ERROR_BAD_QUERY_SYNTAX);
        }
        self.pool.add(name, persistent);
        for c in &cols {
            self.pool.add(name, persistent);
            self.pool.add(&c.name, persistent);
        }
        self.tables.push(Table { name: name.to_string(), cols, rows: Vec::new(), persistent, hold: 0 });
        self.dirty |= persistent;
        Ok(())
    }

    pub fn drop_table(&mut self, ti: usize) {
        while !self.tables[ti].rows.is_empty() {
            self.delete(ti, 0);
        }
        let t = self.tables.remove(ti);
        self.dirty |= t.persistent;
    }

    /// The tables in _Tables' order (by the name's string id).
    pub fn table_order(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.tables.len()).collect();
        v.sort_by_key(|&i| self.pool.find(&self.tables[i].name).unwrap_or(u32::MAX));
        v
    }

    // ---- saving

    /// Writes the persistent tables, streams, storages and summary information as a new
    /// compound file.
    pub fn save<W: Read + Write + Seek>(&self, w: W) -> R<W> {
        let mut out = cfb::CompoundFile::create(w).map_err(|_| ERROR_CREATE_FAILED)?;
        out.set_storage_clsid("/", uuid_from(&self.clsid)).map_err(ioerr)?;
        // (a pool with only the persistent references: rebuilt so the counts are exact)
        let mut pool = self.pool.clone();
        for e in pool.v.iter_mut() {
            e.persistent = 0;
        }
        let tables: Vec<&Table> = self.table_order().into_iter().map(|i| &self.tables[i]).filter(|t| t.persistent).collect();
        for t in &tables {
            let id = pool.find(&t.name).unwrap();
            pool.addref(id, true);
            for c in t.cols.iter().filter(|c| !c.temporary()) {
                pool.addref(id, true);
                pool.addref(pool.find(&c.name).unwrap(), true);
            }
            for r in t.rows.iter().filter(|r| r.persistent) {
                for (c, &v) in t.cols.iter().zip(&r.v) {
                    if c.is_string() && !c.temporary() && v != 0 {
                        pool.addref(v, true);
                    }
                }
            }
        }
        let long = pool.wide_refs();
        let rw = if long { 3 } else { 2 };
        let put_ref = |b: &mut Vec<u8>, v: u32| {
            b.extend(&v.to_le_bytes()[..rw]);
        };
        let mut wr = |name: &str, table: bool, data: &[u8]| -> R<()> {
            let p = format!("/{}", stream_path(name, table));
            out.create_stream(&p).map_err(ioerr)?.write_all(data).map_err(ioerr)
        };
        let (sp, sd) = pool.save();
        wr("_StringPool", true, &sp)?;
        wr("_StringData", true, &sd)?;
        let mut tb = Vec::new();
        let mut cl: Vec<(u32, u16, u32, u16)> = Vec::new();
        for t in &tables {
            let id = pool.find(&t.name).unwrap();
            put_ref(&mut tb, id);
            for (i, c) in t.cols.iter().filter(|c| !c.temporary()).enumerate() {
                cl.push((id, i as u16 + 1, pool.find(&c.name).unwrap(), c.ty));
            }
        }
        wr("_Tables", true, &tb)?;
        cl.sort_by_key(|c| (c.0, c.1));
        let mut cb = Vec::new();
        cl.iter().for_each(|c| put_ref(&mut cb, c.0));
        cl.iter().for_each(|c| cb.extend((c.1 ^ 0x8000).to_le_bytes()));
        cl.iter().for_each(|c| put_ref(&mut cb, c.2));
        cl.iter().for_each(|c| cb.extend((c.3 ^ 0x8000).to_le_bytes()));
        wr("_Columns", true, &cb)?;
        for t in &tables {
            let rows: Vec<&Row> = t.rows.iter().filter(|r| r.persistent).collect();
            let mut b = Vec::new();
            for (c, col) in t.cols.iter().enumerate().filter(|c| !c.1.temporary()) {
                let w = col.width(long);
                for r in &rows {
                    let v = r.v[c];
                    b.extend(&v.to_le_bytes()[..w]);
                }
            }
            if !b.is_empty() {
                wr(&t.name, true, &b)?;
            }
        }
        if let Some(s) = &self.summary {
            wr(SUMMARY, false, s)?;
        }
        for (name, s) in &self.streams {
            let p = format!("/{}", stream_path(name, false));
            let mut st = out.create_stream(&p).map_err(ioerr)?;
            match s {
                Stream::Mem(v) => st.write_all(v).map_err(ioerr)?,
                Stream::File(sp) => {
                    let mut cf = self.src.as_ref().ok_or(ERROR_FUNCTION_FAILED)?.lock().unwrap();
                    let mut from = cf.open_stream(sp).map_err(ioerr)?;
                    io::copy(&mut from, &mut st).map_err(ioerr)?;
                }
            }
        }
        for name in self.storages.keys() {
            let data = self.storage(name).ok_or(ERROR_FUNCTION_FAILED)?;
            let mut sub = cfb::CompoundFile::open(io::Cursor::new(data)).map_err(ioerr)?;
            let base = format!("/{}", names::encode(name, false));
            out.create_storage(&base).map_err(ioerr)?;
            let _ = out.set_storage_clsid(&base, *sub.root_entry().clsid());
            let entries: Vec<(String, bool)> = sub.walk().filter(|e| !e.is_root()).map(|e| (e.path().to_string_lossy().into_owned(), e.is_stream())).collect();
            for (p, stream) in entries {
                let dst = format!("{base}{p}");
                if stream {
                    let mut d = Vec::new();
                    sub.open_stream(&p).map_err(ioerr)?.read_to_end(&mut d).map_err(ioerr)?;
                    out.create_stream(&dst).map_err(ioerr)?.write_all(&d).map_err(ioerr)?;
                } else {
                    out.create_storage(&dst).map_err(ioerr)?;
                }
            }
        }
        out.flush().map_err(ioerr)?;
        Ok(out.into_inner())
    }

    /// The whole database as bytes (tests, small databases).
    pub fn to_bytes(&self) -> R<Vec<u8>> {
        Ok(self.save(io::Cursor::new(Vec::new()))?.into_inner())
    }
}

impl Default for Database {
    fn default() -> Self {
        Database::new()
    }
}

pub fn int_of(col: &Col, v: u32) -> i32 {
    if col.int4() { (v ^ 0x8000_0000) as i32 } else { (v as i32) - 0x8000 }
}

pub fn raw_int(col: &Col, i: i32) -> u32 {
    if i == NULL_INT {
        return 0;
    }
    if col.int4() { (i as u32) ^ 0x8000_0000 } else { ((i as u32).wrapping_add(0x8000)) & 0xffff }
}

/// cfb's Uuid bytes are the GUID's text order; the CLSID constants here are in memory order.
fn guid_bytes(b: &[u8; 16]) -> [u8; 16] {
    [b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]
}

fn uuid_from(mem: &[u8]) -> uuid::Uuid {
    let m: [u8; 16] = mem.try_into().unwrap();
    uuid::Uuid::from_bytes(guid_bytes(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(n: &str, ty: u16) -> Col {
        Col { name: n.into(), ty }
    }

    #[test]
    fn save_and_load() {
        let mut d = Database::new();
        d.pool.codepage = 932;
        d.create_table("Property", vec![col("Property", 0x2d48), col("Value", 0x0d00)], true).unwrap();
        let t = d.table_index("Property").unwrap();
        d.insert(t, &[Field::Str("ProductName".into()), Field::Str("魔女".into())], false).unwrap();
        d.insert(t, &[Field::Str("ALLUSERS".into()), Field::Str("1".into())], false).unwrap();
        assert_eq!(d.insert(t, &[Field::Str("ALLUSERS".into()), Field::Str("2".into())], false), Err(ERROR_FUNCTION_FAILED));
        d.create_table("N", vec![col("K", 0x2104), col("S", 0x1502), col("B", 0x1900)], true).unwrap();
        let n = d.table_index("N").unwrap();
        d.insert(n, &[Field::Int(-5), Field::Int(7), Field::Stream(b"data".to_vec())], false).unwrap();
        d.insert(n, &[Field::Int(3), Field::Null, Field::Null], false).unwrap();
        d.summary = Some(vec![1, 2, 3]);
        let bytes = d.to_bytes().unwrap();
        let e = Database::open(Box::new(io::Cursor::new(bytes))).unwrap();
        let p = e.table("Property").unwrap();
        // (rows in key order: by the strings' ids, ProductName came first)
        let vals: Vec<Field> = p.rows.iter().map(|r| e.field(p, r, 1)).collect();
        assert_eq!(vals, [Field::Str("魔女".into()), Field::Str("1".into())]);
        let n = e.table("N").unwrap();
        assert_eq!(e.field(n, &n.rows[0], 0), Field::Int(-5));
        assert_eq!(e.field(n, &n.rows[0], 1), Field::Int(7));
        assert_eq!(e.field(n, &n.rows[0], 2), Field::Stream(b"data".to_vec()));
        assert_eq!(e.field(n, &n.rows[1], 1), Field::Null);
        assert_eq!(e.summary, Some(vec![1, 2, 3]));
        assert_eq!(e.clsid, CLSID_PACKAGE);
        assert_eq!(e.pool.codepage, 932);
    }

    #[test]
    fn int_raw() {
        let c2 = col("a", 0x0502);
        let c4 = col("a", 0x0104);
        for i in [-32767, -1, 0, 1, 32767] {
            assert_eq!(int_of(&c2, raw_int(&c2, i)), i);
        }
        for i in [i32::MIN + 1, -1, 0, 1, i32::MAX] {
            assert_eq!(int_of(&c4, raw_int(&c4, i)), i);
        }
        assert_eq!(raw_int(&c4, NULL_INT), 0);
    }
}
