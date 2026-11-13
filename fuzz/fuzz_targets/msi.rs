//! msidb: an MSI database (a compound file) read and every field of every table looked at,
//! SQL statements and condition expressions on any text. No panic.
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
    match k % 3 {
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
        _ => {
            let _ = msidb::cond::evaluate(&String::from_utf8_lossy(rest), &Env);
        }
    }
});
