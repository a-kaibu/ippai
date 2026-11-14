//! sake's lines into a Trace: the Schemas are the catalog's, the Trace passes shirusu's
//! conformance checker, and shirusu's compare sees through handle values but not through a
//! byte read differently (the differential sake vs Windows rests on these).
use proptest::prelude::*;
use sake_shirusu::{convert, def_of, parse, Line, Options};
use shirusu_compare::{compare_traces, CandidateKind};
use shirusu_trace::reader::read_trace;

fn opt(arch: &str) -> Options {
    Options { arch: arch.into(), created_by: "sake test".into(), target: "test".into() }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// a line as crates/sake/src/shirusu.rs writes it
fn fmt(l: &Line) -> String {
    match l {
        Line::Enter { tid, call, ms, api, args, s, d } => {
            let a: Vec<String> = args.iter().map(|v| format!("{v:x}")).collect();
            let a = if a.is_empty() { "-".to_string() } else { a.join(",") };
            let mut t = format!("shirusu E {tid:x} {call:x} {ms} {api} {a}");
            if let Some(s) = s { t += &format!(" s={}", hex(s)); }
            if let Some(d) = d { t += &format!(" d={}", hex(d)); }
            t
        }
        Line::Leave { tid, call, ms, ret, err, o, d } => {
            let mut t = format!("shirusu L {tid:x} {call:x} {ms} {ret:x} {err:x}");
            if let Some(o) = o { t += &format!(" o={o:x}"); }
            if let Some(d) = d { t += &format!(" d={}", hex(d)); }
            t
        }
    }
}

fn w(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// a game writing a save, reading it back, a registry key it does not find, and a call that
/// never returns; `h` its file handle, `byte` the first byte ReadFile gives back
fn session(h: u32, byte: u8) -> String {
    let e = |tid, call, api: &str, args: &[u64], s: Option<Vec<u8>>, d: Option<Vec<u8>>| fmt(&Line::Enter { tid, call, ms: call * 10, api: api.into(), args: args.to_vec(), s, d });
    let l = |tid, call, ret, err, o, d: Option<Vec<u8>>| fmt(&Line::Leave { tid, call, ms: call * 10 + 1, ret, err, o, d });
    let path = 0x0040_1000;
    [
        "   0.5 [log] sake: starting".to_string(),
        e(0x20, 1, "kernel32!CreateFileW", &[path, 0x4000_0000, 0, 0, 1, 0x80, 0], Some(w("save\\a.dat")), None),
        l(0x20, 1, h as u64, 0, None, None),
        e(0x20, 2, "kernel32!WriteFile", &[h as u64, 0x50_0000, 4, 0x6f_f000, 0], None, Some(vec![1, 2, 3, 4])),
        l(0x20, 2, 1, 0, Some(4), None),
        e(0x20, 3, "kernel32!CloseHandle", &[h as u64], None, None),
        l(0x20, 3, 1, 0, None, None),
        e(0x20, 4, "kernel32!CreateFileW", &[path, 0x8000_0000, 1, 0, 3, 0x80, 0], Some(w("save\\a.dat")), None),
        l(0x20, 4, (h + 4) as u64, 0, None, None),
        e(0x20, 5, "kernel32!ReadFile", &[(h + 4) as u64, 0x50_0000, 16, 0x6f_f000, 0], None, None),
        l(0x20, 5, 1, 0, Some(4), Some(vec![byte, 2, 3, 4])),
        e(0x20, 6, "kernel32!CloseHandle", &[(h + 4) as u64], None, None),
        l(0x20, 6, 1, 0, None, None),
        e(0x20, 7, "advapi32!RegOpenKeyExA", &[0x8000_0001, 0x40_2000, 0, 0x2001_9, 0x6f_f010], None, None),
        l(0x20, 7, 2, 2, None, None),
        e(0x24, 1, "kernel32!GetFileAttributesA", &[0x40_3000], None, None),
        l(0x24, 1, 0xffff_ffff, 2, None, None),
        e(0x24, 2, "user32!CreateWindowExW", &[0, 0x40_4000, 0x40_4000, 0x10cf_0000, 0x8000_0000, 0x8000_0000, 800, 600, 0, 0, 0x40_0000, 0], None, None),
    ]
    .join("\n")
}

#[test]
fn schemas_are_the_catalogs() {
    let (bytes, st) = convert(&session(0x40, 1), &opt("x86")).unwrap();
    assert_eq!((st.calls, st.unfinished, st.bad), (9, 1, 0));
    let t = read_trace(&bytes).unwrap();
    let schemas: Vec<_> = t.schemas.iter().collect();
    assert!(!schemas.is_empty());
    for s in schemas {
        let (api, side) = s.name.rsplit_once('/').unwrap();
        let def = def_of(api).unwrap();
        let want = if side == "enter" { def.enter_schema() } else { def.leave_schema() };
        assert_eq!(s.content_hash, want.content_hash, "{}", s.name);
    }
}

#[test]
fn conforms() {
    for arch in ["x86", "x86_64"] {
        let (bytes, _) = convert(&session(0x40, 1), &opt(arch)).unwrap();
        let r = shirusu_conformance::check_trace(&read_trace(&bytes).unwrap());
        assert!(r.conformant(), "{arch}: {:#?}", r.findings);
    }
}

#[test]
fn compare_sees_through_handles_not_bytes() {
    let a = read_trace(&convert(&session(0x40, 1), &opt("x86")).unwrap().0).unwrap();
    let b = read_trace(&convert(&session(0x1c8, 1), &opt("x86")).unwrap().0).unwrap();
    let same = compare_traces(&a, &b);
    assert!(same.candidates.iter().all(|c| c.severity.name() == "info"), "{:#?}", same.candidates);
    let c = read_trace(&convert(&session(0x1c8, 9), &opt("x86")).unwrap().0).unwrap();
    let diff = compare_traces(&a, &c);
    assert!(
        diff.candidates.iter().any(|c| c.api.as_deref().is_some_and(|a| a.contains("ReadFile")) && matches!(c.kind, CandidateKind::ArgumentDifference | CandidateKind::PayloadDifference)),
        "{:#?}",
        diff.candidates
    );
}

/// an API the catalog does not have is refused, not written under a made-up Schema
#[test]
fn unknown_api() {
    let l = fmt(&Line::Enter { tid: 1, call: 1, ms: 0, api: "kernel32!NoSuchApi".into(), args: vec![], s: None, d: None });
    assert!(convert(&l, &opt("x86")).is_err());
}

fn line() -> impl Strategy<Value = Line> {
    let bytes = proptest::option::of(proptest::collection::vec(any::<u8>(), 0..40));
    prop_oneof![
        (any::<u32>(), 1..u64::MAX, any::<u32>(), "[a-z0-9]{1,8}![A-Za-z0-9]{1,20}", proptest::collection::vec(any::<u64>(), 0..12), bytes.clone(), bytes.clone())
            .prop_map(|(tid, call, ms, api, args, s, d)| Line::Enter { tid: tid as u64, call, ms: ms as u64, api, args, s, d }),
        (any::<u32>(), 1..u64::MAX, any::<u32>(), any::<u64>(), any::<u32>(), proptest::option::of(any::<u32>()), bytes)
            .prop_map(|(tid, call, ms, ret, err, o, d)| Line::Leave { tid: tid as u64, call, ms: ms as u64, ret, err, o, d }),
    ]
}

proptest! {
    /// what sake writes reads back as it was, after any prefix a log puts before it
    #[test]
    fn roundtrip(l in line(), pre in "[ -~]{0,20}") {
        prop_assert_eq!(parse(&format!("{pre}{}", fmt(&l))), Some(Some(l)));
    }

    /// any text: no panic, and a log of it converts or is refused (never a panic)
    #[test]
    fn garbage(t in "(shirusu [EL] )?[ -~]{0,80}", n in 0..4usize) {
        let log = vec![t.clone(); n].join("\n");
        let _ = parse(&t);
        let _ = convert(&log, &opt("x86"));
    }
}

/// the lines crates/sake/src/shirusu.rs's test `lines` makes (the same strings): the contract
/// between sake and this converter
#[test]
fn sake_lines() {
    let e = parse("   1.5 [error] shirusu E 20 1 1500 kernel32!CreateFileW 401000,80000000,1,0,3,80,0 s=61002e006400610074000000").unwrap().unwrap();
    assert_eq!(e, Line::Enter { tid: 0x20, call: 1, ms: 1500, api: "kernel32!CreateFileW".into(), args: vec![0x401000, 0x8000_0000, 1, 0, 3, 0x80, 0], s: Some(w("a.dat")), d: None });
    let e = parse("shirusu E 24 2 7 kernel32!GetLogicalDrives -").unwrap().unwrap();
    assert_eq!(e, Line::Enter { tid: 0x24, call: 2, ms: 7, api: "kernel32!GetLogicalDrives".into(), args: vec![], s: None, d: None });
    assert_eq!(parse("shirusu L 20 1 1501 ffffffff 2").unwrap().unwrap(), Line::Leave { tid: 0x20, call: 1, ms: 1501, ret: 0xffff_ffff, err: 2, o: None, d: None });
    assert_eq!(parse("shirusu L 20 3 9 1 0 o=4 d=0102ab04").unwrap().unwrap(), Line::Leave { tid: 0x20, call: 3, ms: 9, ret: 1, err: 0, o: Some(4), d: Some(vec![1, 2, 0xab, 4]) });
    assert_eq!(parse("sake: nothing of ours"), None);
    assert_eq!(parse("shirusu E zz"), Some(None));
}
