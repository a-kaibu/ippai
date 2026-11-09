//! Archive files (.idt): MsiDatabaseExport / MsiDatabaseImport's tab-separated tables.
//!
//! Line 1 the column names, line 2 their types (s72, S0, L64, i2, I4, v0...), line 3 the
//! table name and its keys, then one line per row; CR LF in a value is \x11\x19, LF \x19.
//! Binary fields name a file in the folder "<Table>/<key>.ibd".

use crate::db::*;
use crate::record::Field;
use crate::view::View;
use crate::*;

/// MSICOLINFO_TYPES' text for a column type.
pub fn type_text(ty: u16) -> String {
    let c = Col { name: String::new(), ty };
    let n = c.nullable();
    let (ch, size) = if c.is_binary() {
        ('v', 0)
    } else if ty & T_STRING != 0 {
        (if ty & T_LOCALIZABLE != 0 { 'l' } else { 's' }, ty & T_SIZE)
    } else {
        ('i', if ty & T_SIZE == 4 { 4 } else { 2 })
    };
    format!("{}{size}", if n { ch.to_ascii_uppercase() } else { ch })
}

/// The type bits for an archive type ("s72"); None: not a type.
pub fn parse_type(t: &str) -> Option<u16> {
    let mut it = t.chars();
    let ch = it.next()?;
    let size: u16 = it.as_str().parse().ok()?;
    let nullable = if ch.is_ascii_uppercase() { T_NULLABLE } else { 0 };
    let base = match ch.to_ascii_lowercase() {
        's' => T_STRING | T_NONBINARY | (size & 0xff),
        'l' => T_STRING | T_NONBINARY | T_LOCALIZABLE | (size & 0xff),
        'i' => match size {
            2 | 1 => 2 | T_NONBINARY,
            4 => 4,
            _ => return None,
        },
        'v' => T_STRING,
        _ => return None,
    };
    Some(base | nullable | T_VALID)
}

fn escape(s: &str) -> String {
    s.replace("\r\n", "\u{11}\u{19}").replace('\n', "\u{19}")
}

/// Exports `table` as archive text (in the database's codepage); binary fields as files
/// returned alongside: (path relative to the folder, data).
pub fn export(db: &mut Database, table: &str) -> R<(Vec<u8>, Vec<(String, Vec<u8>)>)> {
    let cp = db.pool.codepage;
    if table == "_ForceCodepage" {
        return Ok((format!("\r\n\r\n{cp}\t_ForceCodepage\r\n").into_bytes(), Vec::new()));
    }
    let mut v = View::new(db, &format!("SELECT * FROM `{table}`"))?;
    v.execute(db, None)?;
    let cols = v.columns(db);
    let mut out = String::new();
    out.push_str(&cols.iter().map(|c| c.0.clone()).collect::<Vec<_>>().join("\t"));
    out.push_str("\r\n");
    out.push_str(&cols.iter().map(|c| type_text(c.1)).collect::<Vec<_>>().join("\t"));
    out.push_str("\r\n");
    let keys: Vec<String> = cols.iter().filter(|c| c.1 & T_KEY != 0).map(|c| c.0.clone()).collect();
    let mut l3 = vec![table.to_string()];
    l3.extend(keys);
    out.push_str(&l3.join("\t"));
    out.push_str("\r\n");
    let mut files = Vec::new();
    while let Ok(r) = v.fetch(db) {
        let mut fs = Vec::new();
        for i in 1..=r.count() {
            match r.get(i) {
                Field::Stream(d) => {
                    let name = format!("{}.ibd", r.get(1).text());
                    files.push((format!("{table}/{name}"), d.clone()));
                    fs.push(name);
                }
                f => fs.push(escape(&f.text())),
            }
        }
        out.push_str(&fs.join("\t"));
        out.push_str("\r\n");
    }
    Ok((crate::pool::encode_cp(cp, &out), files))
}

/// Imports archive text into the database (`read`: a binary field's file, by its table and
/// name; it is in a folder beside the archive named after the archive file, or the table).
pub fn import(db: &mut Database, data: &[u8], read: &dyn Fn(&str, &str) -> Option<Vec<u8>>) -> R<()> {
    // (the codepage line comes first when there is one; the rest is in that codepage)
    let first = String::from_utf8_lossy(data);
    let lines0: Vec<&str> = first.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    if lines0.len() >= 3 && lines0[0].is_empty() && lines0[1].is_empty() {
        let mut it = lines0[2].split('\t');
        let cp: u32 = it.next().unwrap_or("").parse().map_err(|_| ERROR_FUNCTION_FAILED)?;
        if it.next() == Some("_ForceCodepage") {
            db.pool.codepage = cp;
            db.dirty = true;
            return Ok(());
        }
    }
    let text = crate::pool::decode_cp(db.pool.codepage, data);
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    if lines.len() < 3 {
        return Err(ERROR_FUNCTION_FAILED);
    }
    let names: Vec<&str> = lines[0].split('\t').collect();
    let types: Vec<&str> = lines[1].split('\t').collect();
    let l3: Vec<&str> = lines[2].split('\t').collect();
    if names.len() != types.len() || l3.is_empty() {
        return Err(ERROR_FUNCTION_FAILED);
    }
    let table = l3[0];
    if table == "_SummaryInformation" {
        return Ok(());
    }
    let keys = &l3[1..];
    let mut cols = Vec::new();
    for (n, t) in names.iter().zip(&types) {
        let mut ty = parse_type(t).ok_or(ERROR_FUNCTION_FAILED)?;
        if keys.contains(n) {
            ty |= T_KEY;
        }
        cols.push(Col { name: n.to_string(), ty });
    }
    let ti = match db.table_index(table) {
        Some(ti) => {
            if db.tables[ti].cols.iter().map(|c| &c.name).ne(cols.iter().map(|c| &c.name)) {
                return Err(ERROR_FUNCTION_FAILED);
            }
            ti
        }
        None => {
            db.create_table(table, cols.clone(), true)?;
            db.table_index(table).unwrap()
        }
    };
    for l in &lines[3..] {
        if l.is_empty() {
            continue;
        }
        let vals: Vec<&str> = l.split('\t').collect();
        let mut row = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            // (a CR on its own is dropped)
            let s = vals.get(i).copied().unwrap_or("").replace('\r', "").replace("\u{11}\u{19}", "\r\n").replace('\u{19}', "\n");
            row.push(if s.is_empty() {
                Field::Null
            } else if c.is_binary() {
                Field::Stream(read(table, &s).ok_or(ERROR_FUNCTION_FAILED)?)
            } else if c.ty & T_STRING != 0 {
                Field::Str(s)
            } else {
                Field::Int(crate::record::parse_int(&s).ok_or(ERROR_FUNCTION_FAILED)?)
            });
        }
        // (an existing row with the same keys is replaced)
        let probe: Vec<u32> = cols.iter().zip(&row).map(|(c, f)| if c.is_key() { db.lookup(c, f).unwrap_or(u32::MAX) } else { 0 }).collect();
        if keys.is_empty() || probe.contains(&u32::MAX) {
            db.insert(ti, &row, false)?;
        } else {
            match db.find_key(ti, &probe) {
                Err(ri) => {
                    for (c, f) in row.iter().enumerate() {
                        if !cols[c].is_key() {
                            db.set(ti, ri, c, f)?;
                        }
                    }
                }
                Ok(_) => {
                    db.insert(ti, &row, false)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types() {
        for t in ["s72", "S255", "l0", "L64", "i2", "I2", "i4", "I4", "v0", "V0"] {
            assert_eq!(type_text(parse_type(t).unwrap()), t);
        }
        assert_eq!(type_text(0x1d48), "S72");
        assert_eq!(type_text(0x2d48 & !T_KEY), "s72");
        assert_eq!(parse_type("x2"), None);
    }

    #[test]
    fn export_import() {
        let mut db = Database::new();
        db.pool.codepage = 932;
        let mut v = View::new(&db, "CREATE TABLE `T` (`K` CHAR(16) NOT NULL, `N` INT, `S` LONGCHAR LOCALIZABLE, `B` OBJECT PRIMARY KEY `K`)").unwrap();
        v.execute(&mut db, None).unwrap();
        let ti = db.table_index("T").unwrap();
        db.insert(ti, &[Field::Str("a".into()), Field::Int(-3), Field::Str("改行\r\nあり".into()), Field::Stream(b"bin".to_vec())], false).unwrap();
        db.insert(ti, &[Field::Str("b".into()), Field::Null, Field::Null, Field::Null], false).unwrap();
        let (txt, files) = export(&mut db, "T").unwrap();
        let s = crate::pool::decode_cp(932, &txt);
        assert_eq!(s, "K\tN\tS\tB\r\ns16\tI2\tL0\tV0\r\nT\tK\r\na\t-3\t改行\u{11}\u{19}あり\ta.ibd\r\nb\t\t\t\r\n");
        assert_eq!(files, [("T/a.ibd".to_string(), b"bin".to_vec())]);
        let mut e = Database::new();
        import(&mut e, b"\r\n\r\n932\t_ForceCodepage\r\n", &|_, _| None).unwrap();
        import(&mut e, &txt, &|t, n| files.iter().find(|f| f.0 == format!("{t}/{n}")).map(|f| f.1.clone())).unwrap();
        let (txt2, _) = export(&mut e, "T").unwrap();
        assert_eq!(txt2, txt);
    }
}
