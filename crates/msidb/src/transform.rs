//! Transforms (.mst; MsiDatabaseApplyTransform, the TRANSFORMS property): a compound file
//! with its own string pool and, per table changed, a stream of row operations in the
//! table's encoded name. Each operation is a mask, then values (string references into the
//! transform's pool, integers offset as in tables, binary columns as a flag whose data is the
//! transform's stream "Table.key"):
//!
//! - mask with bit 0 set: a row with every column (inserted, or replacing the row with its keys)
//! - mask 0: a deletion, the keys follow
//! - otherwise an update: the keys, then the columns whose bits (bit n: column n) are set
//!
//! The masks are 16 bits (32 for tables of more than 16 columns). `_Tables` operations add or
//! drop tables, `_Columns` ones add columns (Number NULL: the next one), before the rest.

use std::io::{Cursor, Read, Seek};

use crate::db::*;
use crate::pool::Pool;
use crate::record::Field;
use crate::*;

pub const ERROR_INSTALL_TRANSFORM_FAILURE: u32 = 1624;

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<u32> {
        let s = self.b.get(self.at..self.at + n)?;
        self.at += n;
        Some(s.iter().rev().fold(0, |v, &x| v << 8 | x as u32))
    }
    fn done(&self) -> bool {
        self.at >= self.b.len()
    }
}

/// A value of `col` from the transform: (field; for a binary column, whether data is there).
fn value(r: &mut Reader, col: &Col, pool: &Pool, long: bool) -> Option<Field> {
    let v = r.take(col.width(long))?;
    Some(if v == 0 {
        Field::Null
    } else if col.is_binary() {
        // (filled in from the transform's stream by the caller)
        Field::Stream(Vec::new())
    } else if col.ty & T_STRING != 0 {
        Field::Str(pool.get(v).unwrap_or("").to_string())
    } else {
        Field::Int(int_of(col, v))
    })
}

enum Op {
    Row(Vec<Field>),
    Delete(Vec<Field>),
    Update(Vec<Field>, Vec<(usize, Field)>),
}

fn ops(data: &[u8], cols: &[Col], pool: &Pool, long: bool) -> Option<Vec<Op>> {
    let mut r = Reader { b: data, at: 0 };
    let mw = if cols.len() > 16 { 4 } else { 2 };
    let keys: Vec<usize> = (0..cols.len()).filter(|&i| cols[i].is_key()).collect();
    let mut out = Vec::new();
    while !r.done() {
        let mask = r.take(mw)?;
        if mask & 1 != 0 {
            let mut row = Vec::new();
            for c in cols {
                row.push(value(&mut r, c, pool, long)?);
            }
            out.push(Op::Row(row));
            continue;
        }
        let mut k = Vec::new();
        for &i in &keys {
            k.push(value(&mut r, &cols[i], pool, long)?);
        }
        if mask == 0 {
            out.push(Op::Delete(k));
            continue;
        }
        let mut set = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            if !c.is_key() && i < 32 && mask & 1 << i != 0 {
                set.push((i, value(&mut r, c, pool, long)?));
            }
        }
        out.push(Op::Update(k, set));
    }
    Some(out)
}

fn read<F: Read + Seek>(cf: &mut cfb::CompoundFile<F>, p: &str) -> Option<Vec<u8>> {
    if !cf.is_stream(p) {
        return None;
    }
    let mut v = Vec::new();
    cf.open_stream(p).ok()?.read_to_end(&mut v).ok()?;
    Some(v)
}

impl Database {
    /// Applies a transform (its compound file's bytes).
    pub fn apply_transform(&mut self, mst: &[u8]) -> R<()> {
        let mut cf = cfb::CompoundFile::open(Cursor::new(mst)).map_err(|_| ERROR_INSTALL_TRANSFORM_FAILURE)?;
        let tp = |n: &str| format!("/{}", names::encode(n, true));
        let sp = read(&mut cf, &tp("_StringPool")).unwrap_or_default();
        let sd = read(&mut cf, &tp("_StringData")).unwrap_or_default();
        let pool = Pool::load(&sp, &sd).map_err(|_| ERROR_INSTALL_TRANSFORM_FAILURE)?;
        let long = pool.long_refs;
        let bad = ERROR_INSTALL_TRANSFORM_FAILURE;
        let tables_cols = [Col { name: "Name".into(), ty: 0x2d40 }];
        let columns_cols = [
            Col { name: "Table".into(), ty: 0x2d40 },
            Col { name: "Number".into(), ty: 0x2502 | T_NULLABLE },
            Col { name: "Name".into(), ty: 0x0d40 },
            Col { name: "Type".into(), ty: 0x0502 },
        ];
        // new tables (their columns come in _Columns), dropped tables
        let mut new_tables: Vec<(String, Vec<Col>)> = Vec::new();
        if let Some(d) = read(&mut cf, &tp("_Tables")) {
            for op in ops(&d, &tables_cols, &pool, long).ok_or(bad)? {
                match op {
                    Op::Row(f) => new_tables.push((f[0].text(), Vec::new())),
                    Op::Delete(k) => {
                        if let Some(ti) = self.table_index(&k[0].text()) {
                            self.drop_table(ti);
                        }
                    }
                    Op::Update(..) => {}
                }
            }
        }
        if let Some(d) = read(&mut cf, &tp("_Columns")) {
            for op in ops(&d, &columns_cols, &pool, long).ok_or(bad)? {
                let Op::Row(f) = op else { continue };
                let (t, name) = (f[0].text(), f[2].text());
                let ty = match f[3] {
                    Field::Int(v) => v as u16,
                    _ => return Err(bad),
                };
                let col = Col { name, ty };
                if let Some(nt) = new_tables.iter_mut().find(|x| x.0 == t) {
                    nt.1.push(col);
                } else if let Some(ti) = self.table_index(&t) {
                    if self.tables[ti].col(&col.name).is_none() {
                        self.add_column(ti, col);
                    }
                }
            }
        }
        for (name, cols) in new_tables {
            if self.table_index(&name).is_none() && !cols.is_empty() {
                self.create_table(&name, cols, true)?;
            }
        }
        // the rows
        let entries: Vec<String> = cf.read_root_storage().filter(|e| e.is_stream()).map(|e| e.name().to_string()).collect();
        for raw in entries {
            let (name, table) = names::decode(&raw);
            if !table || matches!(name.as_str(), "_StringPool" | "_StringData" | "_Tables" | "_Columns") {
                continue;
            }
            let Some(ti) = self.table_index(&name) else { continue };
            let d = read(&mut cf, &format!("/{raw}")).unwrap_or_default();
            let cols = self.tables[ti].cols.clone();
            let keys: Vec<usize> = (0..cols.len()).filter(|&i| cols[i].is_key()).collect();
            for op in ops(&d, &cols, &pool, long).ok_or(bad)? {
                // (a binary column's data: the transform's stream "Table.key1\tkey2...")
                let fill = |cf: &mut cfb::CompoundFile<Cursor<&[u8]>>, kv: &[Field], f: Field| -> Field {
                    match f {
                        Field::Stream(_) => {
                            let key: Vec<String> = kv.iter().map(|k| k.text()).collect();
                            let sname = format!("{name}.{}", key.join("\t"));
                            Field::Stream(read(cf, &format!("/{}", names::encode(&sname, false))).unwrap_or_default())
                        }
                        f => f,
                    }
                };
                match op {
                    Op::Row(row) => {
                        let kv: Vec<Field> = keys.iter().map(|&i| row[i].clone()).collect();
                        let row: Vec<Field> = row.into_iter().map(|f| fill(&mut cf, &kv, f)).collect();
                        match self.row_by_keys(ti, &keys, &kv) {
                            Some(ri) => {
                                let mut ri = ri;
                                for (c, f) in row.iter().enumerate() {
                                    if !cols[c].is_key() {
                                        ri = self.set(ti, ri, c, f)?;
                                    }
                                }
                            }
                            None => {
                                self.insert(ti, &row, false)?;
                            }
                        }
                    }
                    Op::Delete(kv) => {
                        if let Some(ri) = self.row_by_keys(ti, &keys, &kv) {
                            self.delete(ti, ri);
                        }
                    }
                    Op::Update(kv, set) => {
                        if let Some(mut ri) = self.row_by_keys(ti, &keys, &kv) {
                            for (c, f) in set {
                                let f = fill(&mut cf, &kv, f);
                                ri = self.set(ti, ri, c, &f)?;
                            }
                        }
                    }
                }
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// The row with these key values.
    pub fn row_by_keys(&self, ti: usize, keys: &[usize], kv: &[Field]) -> Option<usize> {
        let cols = &self.tables[ti].cols;
        let mut probe = vec![0u32; cols.len()];
        for (&i, f) in keys.iter().zip(kv) {
            probe[i] = self.lookup(&cols[i], f)?;
        }
        self.find_key(ti, &probe).err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::View;
    use std::io::Write;

    fn run(db: &mut Database, q: &str) -> Vec<String> {
        let mut v = View::new(db, q).unwrap();
        v.execute(db, None).unwrap();
        let mut out = Vec::new();
        while let Ok(r) = v.fetch(db) {
            out.push((1..=r.count()).map(|i| match r.get(i) {
                Field::Stream(d) => String::from_utf8_lossy(d).into_owned(),
                f => f.text(),
            }).collect::<Vec<_>>().join("|"));
        }
        out
    }

    fn words(w: &[u16]) -> Vec<u8> {
        w.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// The transform of Wine's db tests (test_try_transform's generate_transform_manual).
    fn wine_transform() -> Vec<u8> {
        let mut cf = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        let mut put = |name: &str, table: bool, d: &[u8]| {
            cf.create_stream(format!("/{}", names::encode(name, table))).unwrap().write_all(d).unwrap();
        };
        put("AAR", true, &words(&[0x0201, 0x0008, 0x8001, 0x0201, 0x0009, 0x8002]));
        put(
            "_Columns",
            true,
            &words(&[
                0x0401, 0x0001, 0x8003, 0x0002, 0x9502, 0x0401, 0x0001, 0x8004, 0x0003, 0x9502, 0x0401, 0x0005, 0x0000, 0x0006, 0xbdff, 0x0401, 0x0005,
                0x0000, 0x0007, 0x8502, 0x0401, 0x000a, 0x0000, 0x000a, 0xad48, 0x0401, 0x000a, 0x0000, 0x000b, 0x9d00,
            ]),
        );
        put("_Tables", true, &words(&[0x0101, 0x0005, 0x0101, 0x000a]));
        put("_StringData", true, b"MOOCOWPIGcAARCARBARvwbmwPropertyValuepropval");
        put("_StringPool", true, &words(&[0, 0, 3, 2, 3, 1, 3, 1, 1, 1, 3, 3, 3, 1, 3, 1, 2, 1, 3, 1, 8, 4, 5, 1, 4, 1, 3, 1]));
        put("MOO", true, &words(&[0x000a, 0x8001, 0x0004, 0x8005, 0x0000, 0x8003]));
        put("BINARY", true, &words(&[0x0201, 0x8001, 0x0001]));
        put("BINARY.1", false, b"naengmyon");
        put("Property", true, &words(&[0x0201, 0x000c, 0x000d]));
        cf.flush().unwrap();
        cf.into_inner().into_inner()
    }

    #[test]
    fn wine_try_transform() {
        let mut db = Database::new();
        for q in [
            "CREATE TABLE `MOO` ( `NOO` SHORT NOT NULL, `OOO` CHAR(255) PRIMARY KEY `NOO`)",
            "INSERT INTO `MOO` ( `NOO`, `OOO` ) VALUES ( 1, 'a' )",
            "INSERT INTO `MOO` ( `NOO`, `OOO` ) VALUES ( 2, 'b' )",
            "INSERT INTO `MOO` ( `NOO`, `OOO` ) VALUES ( 3, 'c' )",
            "CREATE TABLE `BINARY` ( `ID` SHORT NOT NULL, `BLOB` OBJECT PRIMARY KEY `ID`)",
        ] {
            run(&mut db, q);
        }
        db.apply_transform(&wine_transform()).unwrap();
        assert_eq!(run(&mut db, "SELECT `BAR`, `CAR` FROM `AAR`"), ["1|vw", "2|bmw"]);
        assert_eq!(run(&mut db, "SELECT * FROM `MOO`"), ["1|c||5", "2|b||"]);
        assert_eq!(run(&mut db, "SELECT `Name` FROM `_Columns` WHERE `Table` = 'MOO'"), ["NOO", "OOO", "COW", "PIG"]);
        assert_eq!(run(&mut db, "SELECT `BLOB` FROM `BINARY` WHERE `ID` = 1"), ["naengmyon"]);
        assert_eq!(run(&mut db, "SELECT * FROM `Property`"), ["prop|val"]);
        // (and it survives a commit)
        let mut e = Database::open(Box::new(Cursor::new(db.to_bytes().unwrap()))).unwrap();
        assert_eq!(run(&mut e, "SELECT * FROM `MOO`"), ["1|c||5", "2|b||"]);
        assert_eq!(db.apply_transform(b"not a transform"), Err(ERROR_INSTALL_TRANSFORM_FAILURE));
    }
}
