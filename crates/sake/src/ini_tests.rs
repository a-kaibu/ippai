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

// ---------------------------------------------------------------------------------------------
// the cache of parsed files (read_host / write_host)

thread_local! {
    /// whole-file reads read_host made on this thread
    pub(super) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn reads() -> usize {
    READS.with(|n| n.get())
}

fn temp_file(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("sake-ini-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("game.ini").to_string_lossy().into_owned()
}

/// Sets the file's time `secs` back (a change the stamp may trust: kura::fs::RECENT)
fn age(p: &str, secs: u64) {
    let f = std::fs::File::options().write(true).open(p).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs)).unwrap();
}

/// A value as GetPrivateProfileString reads it (`load` on a host path)
fn value(host: &str, sec: &str, key: &str) -> Option<String> {
    let _r = PROFILE.read().unwrap_or_else(|e| e.into_inner());
    read_host(host)?.value(sec, key)
}

/// Reading 500 keys reads the file once; a write by anything else (same size, another time)
/// is seen at once; the profile's own write is kept without a read; a file changed just now
/// is read every time (its stamp can't tell a same-size rewrite in the same tick)
#[test]
fn cache_reads_once_and_sees_changes() {
    let f = temp_file("once");
    std::fs::write(&f, "[Main]\r\nvolume=50\r\n").unwrap();
    age(&f, 100);
    let r0 = reads();
    for _ in 0..500 {
        assert_eq!(value(&f, "Main", "volume").as_deref(), Some("50"));
    }
    assert_eq!(reads() - r0, 1);
    std::fs::write(&f, "[Main]\r\nvolume=60\r\n").unwrap();
    age(&f, 90);
    assert_eq!(value(&f, "Main", "volume").as_deref(), Some("60"));
    assert_eq!(reads() - r0, 2);
    assert!(modify_host(&f, |i| i.set("Main", "volume", Some("70"))));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "[Main]\r\nvolume=70\r\n");
    // (just written: not trusted, read again)
    assert_eq!(value(&f, "Main", "volume").as_deref(), Some("70"));
    std::fs::write(&f, "[Main]\r\nvolume=80\r\n").unwrap();
    assert_eq!(value(&f, "Main", "volume").as_deref(), Some("80"));
    assert_eq!(value(&f, "Main", "volume").as_deref(), Some("80"));
    std::fs::remove_file(&f).unwrap();
    assert!(read_host(&f).is_none());
    assert!(modify_host(&f, |i| i.set("New", "k", Some("v"))));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "[New]\r\nk=v\r\n");
}

/// A UTF-16 file stays UTF-16 through the cache, and a file over the cache's size is read
/// every time (never kept)
#[test]
fn cache_keeps_encoding_and_skips_big_files() {
    let f = temp_file("wide");
    let wide: Vec<u8> = [0xff, 0xfe].into_iter().chain("[S]\r\nk=あ\r\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
    std::fs::write(&f, &wide).unwrap();
    age(&f, 100);
    assert_eq!(value(&f, "S", "k").as_deref(), Some("あ"));
    assert!(modify_host(&f, |i| i.set("S", "k", Some("い"))));
    let b = std::fs::read(&f).unwrap();
    assert_eq!(&b[..2], &[0xff, 0xfe]);
    assert_eq!(decode(&b).value("S", "k").as_deref(), Some("い"));
    let big = temp_file("big");
    let text = format!("[S]\r\nk=1\r\npad={}\r\n", "x".repeat(CACHE_BYTES));
    std::fs::write(&big, text).unwrap();
    age(&big, 100);
    let r0 = reads();
    for _ in 0..3 {
        assert_eq!(value(&big, "S", "k").as_deref(), Some("1"));
    }
    assert_eq!(reads() - r0, 3);
}

/// Threads writing keys of one file at once all keep theirs
#[test]
fn cache_writes_from_threads_all_kept() {
    let f = temp_file("threads");
    let ts: Vec<_> = (0..8)
        .map(|t| {
            let f = f.clone();
            std::thread::spawn(move || {
                for k in 0..20 {
                    assert!(modify_host(&f, |i| i.set("S", &format!("t{t}k{k}"), Some(&format!("{t}.{k}")))));
                    assert_eq!(value(&f, "S", &format!("t{t}k{k}")), Some(format!("{t}.{k}")));
                }
            })
        })
        .collect();
    for t in ts {
        t.join().unwrap();
    }
    let ini = decode(&std::fs::read(&f).unwrap());
    for t in 0..8 {
        for k in 0..20 {
            assert_eq!(ini.value("S", &format!("t{t}k{k}")), Some(format!("{t}.{k}")), "t{t}k{k}");
        }
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// WritePrivateProfileString
    Set(u8, u8),
    /// WritePrivateProfileString(key, NULL)
    Unset(u8),
    /// the game (or another process) writes the file itself; `old`: its time far enough back
    /// for the stamp to trust it
    Outside(Vec<(u8, u8)>, bool),
    Remove,
    Get(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..4, 0u8..10).prop_map(|(k, v)| Op::Set(k, v)),
        (0u8..4).prop_map(Op::Unset),
        (prop::collection::vec((0u8..4, 0u8..10), 0..4), any::<bool>()).prop_map(|(e, old)| Op::Outside(e, old)),
        Just(Op::Remove),
        (0u8..4).prop_map(Op::Get),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Differential: any mix of profile writes, outside writes and removals reads exactly what
    /// a fresh read of the file says (the cache never answers from an old file)
    #[test]
    fn cache_matches_fresh_reads(ops in prop::collection::vec(op(), 1..30)) {
        let f = temp_file("pbt");
        // (each outside write a different time back: what a real later change looks like)
        let mut back = 1000;
        for o in ops {
            match o {
                Op::Set(k, v) => {
                    let ok = modify_host(&f, |i| i.set("S", &format!("k{k}"), Some(&v.to_string())));
                    prop_assert!(ok);
                }
                Op::Unset(k) => {
                    let ok = modify_host(&f, |i| i.set("S", &format!("k{k}"), None));
                    prop_assert!(ok);
                }
                Op::Outside(e, old) => {
                    let text: String = std::iter::once("[S]\r\n".to_string()).chain(e.iter().map(|(k, v)| format!("k{k}={v}\r\n"))).collect();
                    std::fs::write(&f, text).unwrap();
                    if old {
                        back -= 1;
                        age(&f, back);
                    }
                }
                Op::Remove => {
                    let _ = std::fs::remove_file(&f);
                }
                Op::Get(k) => {
                    let fresh = std::fs::read(&f).ok().map(|b| decode(&b));
                    let key = format!("k{k}");
                    prop_assert_eq!(value(&f, "S", &key), fresh.and_then(|i| i.value("S", &key)));
                }
            }
        }
    }
}
