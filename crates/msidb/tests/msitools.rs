//! Differential tests against msitools (libmsi, an independent reader/writer): packages
//! wixl builds read the same here, and databases written here read the same there.
//! Skipped when msitools/wixl are not installed (apt install msitools wixl).

use msidb::record::Field;
use msidb::{idt, suminfo, Database, View};
use std::path::{Path, PathBuf};
use std::process::Command;

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("msidb-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn msiinfo(args: &[&str]) -> Vec<u8> {
    let o = Command::new("msiinfo").args(args).output().unwrap();
    assert!(o.status.success(), "msiinfo {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

fn open(p: &Path) -> Database {
    Database::open(Box::new(std::fs::File::open(p).unwrap())).unwrap()
}

/// Our archive text (in the database's codepage) as text; msiinfo writes UTF-8.
fn text(db: &Database, b: &[u8]) -> String {
    msidb::pool::decode_cp(db.pool.codepage, b)
}

#[test]
fn wixl_package_reads_the_same() {
    if !have("wixl") || !have("msiinfo") {
        eprintln!("msitools not installed: skipped");
        return;
    }
    let d = tmp("wixl");
    std::fs::write(d.join("a.txt"), b"hello\n").unwrap();
    std::fs::write(d.join("b.dat"), vec![b'x'; 5000]).unwrap();
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/game.wxs"), d.join("t.wxs")).unwrap();
    let st = Command::new("wixl").current_dir(&d).args(["-o", "t.msi", "t.wxs"]).status().unwrap();
    assert!(st.success());
    let msi = d.join("t.msi");
    let mut db = open(&msi);
    let tables = String::from_utf8(msiinfo(&["tables", msi.to_str().unwrap()])).unwrap();
    let mut n = 0;
    for t in tables.lines().filter(|t| !t.starts_with('_')) {
        let theirs = msiinfo(&["export", msi.to_str().unwrap(), t]);
        let (ours, _) = idt::export(&mut db, t).unwrap();
        assert_eq!(text(&db, &ours), String::from_utf8(theirs).unwrap(), "table {t}");
        n += 1;
    }
    assert!(n > 20);
    // the embedded cabinet
    let streams = String::from_utf8(msiinfo(&["streams", msi.to_str().unwrap()])).unwrap();
    let mut v = View::new(&db, "SELECT `Name` FROM `_Streams`").unwrap();
    v.execute(&mut db, None).unwrap();
    let mut ours = Vec::new();
    while let Ok(r) = v.fetch(&db) {
        ours.push(r.get(1).text());
    }
    let mut theirs: Vec<String> = streams.lines().map(str::to_string).collect();
    theirs.sort();
    ours.sort();
    assert_eq!(ours, theirs);
    let cab = db.stream("data.cab").unwrap();
    let o = Command::new("msiinfo").args(["extract", msi.to_str().unwrap(), "data.cab"]).output().unwrap();
    assert_eq!(cab, o.stdout);
    // summary information
    let s = suminfo::SummaryInfo::parse(db.summary.as_ref().unwrap());
    // (wixl writes the strings as UTF-8 whatever the codepage says: only ASCII compared)
    assert_eq!(s.props.get(&suminfo::PID_KEYWORDS), Some(&suminfo::Prop::Str("Installer".into())));
    assert_eq!(s.props.get(&suminfo::PID_AUTHOR), Some(&suminfo::Prop::Str("masu".into())));
    assert_eq!(s.props.get(&suminfo::PID_WORDCOUNT), Some(&suminfo::Prop::I4(2)));
    assert_eq!(s.codepage(), 932);
    // (wixl drops non-ASCII text: Japanese is checked with our own databases below)
    let mut v = View::new(&db, "SELECT `Value` FROM `Property` WHERE `Property` = 'Manufacturer'").unwrap();
    v.execute(&mut db, None).unwrap();
    assert_eq!(v.fetch(&db).unwrap().get(1), &Field::Str("masu".into()));
}

#[test]
fn our_database_reads_the_same_in_msitools() {
    if !have("msiinfo") {
        eprintln!("msitools not installed: skipped");
        return;
    }
    let d = tmp("ours");
    let mut db = Database::new();
    db.pool.codepage = 932;
    for sql in [
        "CREATE TABLE `Property` (`Property` CHAR(72) NOT NULL, `Value` LONGCHAR NOT NULL LOCALIZABLE PRIMARY KEY `Property`)",
        "CREATE TABLE `Nums` (`A` INT NOT NULL, `B` LONG, `C` CHAR(10), `D` OBJECT PRIMARY KEY `A`)",
        "INSERT INTO `Property` (`Property`, `Value`) VALUES ('ProductName', '魔女のゲーム')",
        "INSERT INTO `Property` (`Property`, `Value`) VALUES ('ALLUSERS', '1')",
        "INSERT INTO `Nums` (`A`, `B`, `C`) VALUES (-32767, 2147483647, 'x')",
        "INSERT INTO `Nums` (`A`, `B`) VALUES (32767, -2147483647)",
        "INSERT INTO `Nums` (`A`) VALUES (0)",
    ] {
        let mut v = View::new(&db, sql).unwrap();
        v.execute(&mut db, None).unwrap();
    }
    let mut v = View::new(&db, "UPDATE `Nums` SET `C` = 'zero' WHERE `A` = 0").unwrap();
    v.execute(&mut db, None).unwrap();
    let mut s = suminfo::SummaryInfo::default();
    s.set(suminfo::PID_CODEPAGE, suminfo::Prop::I2(932)).unwrap();
    s.set(suminfo::PID_TITLE, suminfo::Prop::Str("タイトル".into())).unwrap();
    s.set(suminfo::PID_WORDCOUNT, suminfo::Prop::I4(2)).unwrap();
    db.summary = Some(s.serialize());
    let p = d.join("ours.msi");
    std::fs::write(&p, db.to_bytes().unwrap()).unwrap();
    let ps = p.to_str().unwrap();
    for t in ["Property", "Nums"] {
        let (ours, _) = idt::export(&mut db, t).unwrap();
        let theirs = msiinfo(&["export", ps, t]);
        // (msiinfo writes the binary column's file name as the key; ours too)
        assert_eq!(text(&db, &ours), String::from_utf8(theirs).unwrap(), "table {t}");
    }
    // (msiinfo prints the string's bytes as they are: Shift_JIS)
    let info = msiinfo(&["suminfo", ps]);
    let want = [b"Title: ".as_slice(), &msidb::pool::encode_cp(932, "タイトル")].concat();
    assert!(info.windows(want.len()).any(|w| w == want), "{}", String::from_utf8_lossy(&info));
}
