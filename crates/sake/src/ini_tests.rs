//! The profile rules checked against Windows' own answers (tests/winref/golden/ini-*, from
//! tests/winref/probes/ini.c: the same file and the same calls replayed on `Ini`), and by
//! properties (proptest) for files the probe does not cover.

use super::*;
use proptest::prelude::*;

/// Windows' stdout for the ini probe (32- and 64-bit), or nothing when the golden is missing
fn golden() -> Vec<String> {
    ["32", "64"].iter().filter_map(|b| std::fs::read_to_string(format!("{}/../../tests/winref/golden/ini-{b}/stdout.txt", env!("CARGO_MANIFEST_DIR"))).ok()).collect()
}

/// The probe's file (ini.c's `text`)
const TEXT: &str = "; comment line\r\ntop=before any section\r\n[Main]\r\nName = Alice  \r\nquoted=\"  spaced  \"\r\nsingle='x'\r\nhalf=\"open\r\nnum=42\r\nneg=-7\r\nhex=0x1F\r\nbig=99999999999\r\nlead=  12abc\r\nempty=\r\nnoequals\r\ndup=first\r\nDUP=second\r\n  indented key = v  \r\nsemi=a;b\r\ntab=\tt\t\r\n[ Spaced ]\r\nk=spaced section\r\n[main]\r\nextra=second main\r\n[Unclosed\r\nu=1\r\n[Empty]\r\n[Bin]\r\ndata=0102030405D2\r\n[Last]\r\nnonl=end";

/// The probe's dump of a file after "file <n> bytes:" (number `which`), as the bytes
fn dumped(out: &str, which: usize) -> String {
    let mut parts = out.split("bytes:\n").skip(1 + which);
    let body = parts.next().unwrap();
    let body = &body[..body.find("|\n").unwrap()];
    body.replace("\\n\n", "\n").replace("\\r", "\r")
}

/// The list a "get" line printed: '|' for each NUL, up to what the probe shows
fn list_of(v: &[String]) -> String {
    v.iter().map(|s| format!("{s}|")).collect()
}

#[test]
fn reads_as_windows() {
    let g = golden();
    assert!(!g.is_empty(), "ini golden missing");
    let ini = Ini::parse(TEXT);
    for out in &g {
        for l in out.lines() {
            // get [sec] key def d cap 64: n err e [value|#]
            let Some(r) = l.strip_prefix("get [") else { continue };
            let (sec, r) = r.split_once("] ").unwrap();
            let (key, r) = r.split_once(" def ").unwrap();
            let (_, r) = r.rsplit_once(" cap ").unwrap();
            let (cap, r) = r.split_once(": ").unwrap();
            let (n, r) = r.split_once(" err ").unwrap();
            let (err, shown) = r.split_once(" [").unwrap();
            let shown = shown.strip_suffix(']').unwrap();
            if cap != "64" && cap != "256" {
                continue;
            }
            let n: usize = n.parse().unwrap();
            if sec == "(null)" {
                assert_eq!(list_of(&ini.names()), shown[..n].to_string(), "{l}");
            } else if key == "(null)" {
                assert_eq!(list_of(&ini.keys(sec).unwrap_or_default()), shown[..n].to_string(), "{l}");
            } else {
                match ini.value(sec, key) {
                    Some(v) => assert!(err == "0" && v == shown[..n], "{l}: sake {v:?}"),
                    None => assert_eq!(err, "2", "{l}"),
                }
            }
        }
        // section Main 170 [...]
        let l = out.lines().find(|l| l.starts_with("section Main ") && !l.contains("cap")).unwrap();
        let shown = l.split_once(" [").unwrap().1;
        assert_eq!(list_of(&ini.section("Main").unwrap()) + "|]", shown, "{l}");
        // int <key> = <n> (default 5)
        for l in out.lines().filter_map(|l| l.strip_prefix("int ")) {
            let (k, n) = l.split_once(" = ").unwrap();
            let got = ini.value("Main", k).and_then(|v| int_of(&v)).unwrap_or(5) as i32;
            assert_eq!(got.to_string(), n, "int {k}");
        }
    }
}

#[test]
fn writes_as_windows() {
    for out in golden() {
        let mut ini = Ini::parse(TEXT);
        // [Bin] data=0102030405D2: the checksum is wrong; 4 or 6 bytes are the wrong length
        assert_eq!(struct_bytes(&ini.value("Bin", "data").unwrap(), 5), Err(13));
        assert_eq!(struct_bytes(&ini.value("Bin", "data").unwrap(), 6), Err(24));
        assert_eq!(struct_bytes(&ini.value("Bin", "data").unwrap(), 4), Err(24));
        ini.set("Bin", "out", Some(&struct_hex(&[0x10, 0x20, 0xff])));
        ini.set("Main", "num", Some("43"));
        ini.set("main", "NewKey", Some("  new value  "));
        ini.set("Main", "Name", None);
        ini.set("Fresh", "a", Some("1"));
        ini.set("Fresh", "b", Some("\"q\""));
        ini.set("Unclosed", "u", Some("2"));
        ini.set("Last", "added", Some("x"));
        ini.delete_section("Spaced");
        ini.set_section("Sec", vec!["k1=v1".into(), "k2=v2".into()]);
        ini.set_section("Main", vec!["only=this".into()]);
        assert_eq!(ini.text(), dumped(&out, 0));
        // a new file: one key, the key deleted, the section deleted
        let mut n = Ini::default();
        n.set("S", "k", Some("v"));
        assert_eq!(n.text(), dumped(&out, 1));
        n.set("S", "k", None);
        assert_eq!(n.text(), dumped(&out, 2));
        n.delete_section("S");
        assert_eq!(n.text(), dumped(&out, 3));
    }
}

fn word() -> impl Strategy<Value = String> {
    "[A-Za-z][A-Za-z0-9_]{0,6}"
}
fn line() -> impl Strategy<Value = String> {
    prop_oneof![
        "[ \t]{0,2}[A-Za-z0-9_ ]{1,8}[ \t]{0,2}=[ \t]{0,2}[A-Za-z0-9_;\"' ]{0,8}",
        "; ?[a-z ]{0,8}",
        "[ \t]{0,2}",
        "[A-Za-z]{1,6}",
        "\\[[ ]?[A-Za-z]{1,6}[ ]?\\]?",
    ]
}

proptest! {
    /// Reading a file and writing it back changes nothing (but a missing last line end)
    #[test]
    fn parse_text_roundtrip(lines in prop::collection::vec(line(), 0..20)) {
        let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
        prop_assert_eq!(Ini::parse(&text).text(), text.clone());
        if lines.last().is_some_and(|l| !l.is_empty()) {
            prop_assert_eq!(Ini::parse(text.strip_suffix("\r\n").unwrap()).text(), text);
        }
    }

    /// A value set reads back (trimmed, unquoted), other sections' lines stay as they were, and
    /// a deleted key reads as missing
    #[test]
    fn set_reads_back(lines in prop::collection::vec(line(), 0..20), sec in word(), key in word(), val in "[A-Za-z0-9_]{0,8}") {
        let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
        let before = Ini::parse(&text);
        let mut ini = before.clone();
        ini.set(&sec, &key, Some(&val));
        prop_assert_eq!(ini.value(&sec, &key), Some(val.clone()));
        let i = ini.sec(&sec).unwrap();
        for (j, s) in ini.secs.iter().enumerate() {
            if j != i {
                prop_assert!(before.secs.contains(s));
            }
        }
        prop_assert_eq!(&ini.pre, &before.pre);
        // the change survives the file
        prop_assert_eq!(Ini::parse(&ini.text()).value(&sec, &key), Some(val));
        ini.set(&sec, &key, None);
        // (a duplicate key of the same name can still be there)
        let left = before.sec(&sec).map_or(0, |b| before.secs[b].lines.iter().filter(|l| entry(l).is_some_and(|e| e.0.eq_ignore_ascii_case(&key))).count());
        if left <= 1 {
            prop_assert_eq!(ini.value(&sec, &key), None);
        }
    }

    /// A struct written reads back; any other length is ERROR_BAD_LENGTH, a changed digit
    /// ERROR_INVALID_DATA
    #[test]
    fn struct_roundtrip(b in prop::collection::vec(any::<u8>(), 0..32), other in 0usize..40, flip in any::<prop::sample::Index>()) {
        let h = struct_hex(&b);
        prop_assert_eq!(struct_bytes(&h, b.len()), Ok(b.clone()));
        prop_assert_eq!(struct_bytes(&h.to_lowercase(), b.len()), Ok(b.clone()));
        if other != b.len() {
            prop_assert_eq!(struct_bytes(&h, other), Err(24));
        }
        let mut bad: Vec<u8> = h.into_bytes();
        let at = flip.index(bad.len());
        bad[at] = if bad[at] == b'0' { b'1' } else { b'0' };
        prop_assert_eq!(struct_bytes(std::str::from_utf8(&bad).unwrap(), b.len()), Err(13));
    }

    /// GetPrivateProfileInt reads what %d / 0x%x wrote, wrapped to 32 bits
    #[test]
    fn int_roundtrip(n in any::<i32>(), x in any::<u32>()) {
        prop_assert_eq!(int_of(&n.to_string()), Some(n as u32));
        prop_assert_eq!(int_of(&format!("0x{x:X}")), Some(x));
        prop_assert_eq!(int_of(&format!("  {n}abc")), Some(n as u32));
    }
}
