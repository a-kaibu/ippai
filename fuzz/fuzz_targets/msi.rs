//! msidb: an MSI database (a compound file) read and every field of every table looked at,
//! SQL statements and condition expressions on any text; statements (split at ';') all
//! opened as views first, then executed, fetched, their columns read and the database saved
//! (views a later DROP or ALTER left stale; joins, ORDER BY, DISTINCT); MsiFormatRecord
//! templates with and without a package. No panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

struct Env;
impl msidb::cond::Env for Env {
    fn property(&self, name: &str) -> String {
        if name.len() % 2 == 0 { name.to_string() } else { String::new() }
    }
    fn env(&self, name: &str) -> String {
        name.chars().rev().collect()
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else { return };
    match k % 5 {
        0 => {
            let Ok(db) = msidb::db::Database::open(Box::new(std::io::Cursor::new(rest.to_vec()))) else { return };
            for t in &db.tables {
                for row in t.rows.iter().take(1000) {
                    for c in 0..t.cols.len() {
                        let _ = db.value_str(t, row, c);
                    }
                }
            }
            for name in db.streams.keys().take(100) {
                let _ = db.stream(name);
            }
        }
        1 => {
            let _ = msidb::sql::parse(&String::from_utf8_lossy(rest));
        }
        3 => {
            use msidb::view::View;
            let mut db = msidb::db::Database::new();
            for q in [
                "CREATE TABLE `A` (`k` CHAR(8) NOT NULL, `v` INT, `s` CHAR(8) PRIMARY KEY `k`)",
                "CREATE TABLE `B` (`k` INT NOT NULL, `a` CHAR(8), `d` OBJECT PRIMARY KEY `k`)",
                "INSERT INTO `A` (`k`, `v`, `s`) VALUES ('x', 1, 'p')",
                "INSERT INTO `A` (`k`, `v`, `s`) VALUES ('y', 2, 'x')",
                "INSERT INTO `B` (`k`, `a`) VALUES (1, 'x')",
                "INSERT INTO `B` (`k`, `a`) VALUES (2, 'y')",
            ] {
                let mut v = View::new(&db, q).unwrap();
                v.execute(&mut db, None).unwrap();
            }
            let text = String::from_utf8_lossy(rest);
            let mut views: Vec<View> = text.split(';').take(8).filter_map(|q| View::new(&db, q).ok()).collect();
            let mut p = msidb::record::Record::new(3);
            let _ = p.set(1, msidb::record::Field::Int(1));
            let _ = p.set(2, msidb::record::Field::Str("x".into()));
            for v in views.iter_mut() {
                let _ = v.columns(&db);
                if v.execute(&mut db, Some(&p)).is_ok() {
                    for _ in 0..64 {
                        let Ok(mut r) = v.fetch(&db) else { break };
                        let _ = v.modify(&mut db, msidb::view::MSIMODIFY_REFRESH, &mut r);
                    }
                }
                let _ = v.columns(&db);
            }
            let _ = db.to_bytes();
        }
        4 => {
            struct Pkg;
            impl msidb::format::Env for Pkg {
                fn has_package(&self) -> bool {
                    true
                }
                fn property(&self, name: &str) -> Option<String> {
                    (name.len() % 3 != 0).then(|| name.chars().rev().collect())
                }
            }
            let text = String::from_utf8_lossy(rest);
            let mut r = msidb::record::Record::new(4);
            let _ = r.set(1, msidb::record::Field::Str("[2]".into()));
            let _ = r.set(2, msidb::record::Field::Str("{[1]}".into()));
            let _ = msidb::format::format_str(&text, &r, &Pkg);
            let _ = msidb::format::format_str(&text, &r, &msidb::format::NoPackage);
        }
        _ => {
            let _ = msidb::cond::evaluate(&String::from_utf8_lossy(rest), &Env);
        }
    }
});
