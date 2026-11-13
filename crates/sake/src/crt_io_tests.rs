//! crt_io's rules checked three ways: against Windows' own answers (tests/winref/golden, read on
//! every cargo test), exhaustively over every character and byte, and by properties (proptest).

use super::*;
use proptest::prelude::*;

/// Windows' stdout for a winref probe (32- and 64-bit), or nothing when the golden is missing
fn golden(name: &str) -> Vec<(String, String)> {
    ["32", "64"]
        .iter()
        .filter_map(|b| {
            let p = format!("{}/../../tests/winref/golden/{name}-{b}/stdout.txt", env!("CARGO_MANIFEST_DIR"));
            std::fs::read_to_string(p).ok().map(|s| (b.to_string(), s))
        })
        .collect()
}

fn hex16(s: &str) -> u16 {
    u16::from_str_radix(s, 16).unwrap()
}

#[test]
fn strerror_messages_are_windows() {
    let g = golden("crt_io");
    assert!(!g.is_empty(), "crt_io golden missing");
    for (b, out) in &g {
        let mut n = 0;
        for l in out.lines() {
            // "strerror <errno> <message>" (strerror_s into a big buffer)
            let Some(rest) = l.strip_prefix("strerror ") else { continue };
            let Some((e, m)) = rest.split_once(' ') else { continue };
            let Ok(e) = e.parse::<u32>() else { continue };
            assert_eq!(message(e), m, "{b}-bit strerror({e})");
            n += 1;
        }
        assert_eq!(n, 44, "{b}-bit: all 44 messages checked");
    }
    // past the table: Windows says "Unknown error" (strerror(200) in the golden)
    assert_eq!(message(200), "Unknown error");
    assert_eq!(message(u32::MAX), "Unknown error");
}

/// _stat's name rule: the probe's paths whose answer the name alone decides
#[test]
fn stat_names_are_windows() {
    let cases = [("stat file", "io_a.bin"), ("stat dir", "."), ("stat dir slash", ".\\"), ("stat file slash", "io_a.bin\\"), ("stat wildcard", "io_*.bin"), ("stat parent slash", "..\\"), ("stat parent", "..")];
    for (b, out) in golden("crt_io") {
        // "stat root <path> <r> errno <e> dir <d>" (newer goldens)
        for l in out.lines().filter_map(|l| l.strip_prefix("stat root ")) {
            let (path, rest) = l.split_once(' ').unwrap();
            assert_eq!(stat_name_ok(path), rest.starts_with("0 "), "{b}-bit stat root {path}");
        }
        for (label, path) in cases {
            let Some(line) = out.lines().find(|l| l.strip_prefix(label).is_some_and(|r| r.starts_with(" -1") || r.starts_with(" mode"))) else {
                assert!(label.starts_with("stat parent"), "{b}-bit {label} missing");
                continue;
            };
            let windows_ok = !line[label.len()..].starts_with(" -1");
            assert_eq!(stat_name_ok(path), windows_ok, "{b}-bit {label} {path:?}");
        }
    }
    // roots keep their separator; anything else with one does not, and a bare drive is no
    // name (Windows' msvcrt: stat("C:") is ENOENT)
    for p in ["\\", "/", "C:\\", "c:/", "a", "dir\\x", "..\\..\\x.exe"] {
        assert!(stat_name_ok(p), "{p:?}");
    }
    for p in ["", ".\\", "./", "dir\\", "dir/", "C:\\dir\\", "a?", "*", "..\\", "C:", "z:"] {
        assert!(!stat_name_ok(p), "{p:?}");
    }
}

proptest! {
    // a trailing separator is accepted exactly when what precedes it is empty or a drive
    #[test]
    fn stat_trailing_separator(name in "[a-z.]{0,6}", drive in proptest::bool::ANY, sep in "[\\\\/]{1,3}") {
        let base = if drive { format!("C:{name}") } else { name.clone() };
        let root = name.is_empty();
        prop_assert_eq!(stat_name_ok(&format!("{base}{sep}")), root);
        // (a bare drive "C:" is no name)
        prop_assert_eq!(stat_name_ok(&base), !base.is_empty() && base != "C:");
    }
}

/// The multibyte lines of crt_io, in the locale each section sets: "" is the runner's user locale
/// (1252), "932 " names .932, "C " is the "C" locale. What sake's helpers answer for the probe's
/// inputs must be Windows' line.
#[test]
fn multibyte_lines_are_windows() {
    let sj: &[u8] = b"\x82\xa0a\x83\x41";
    let mut checked = 0;
    for (b, out) in golden("crt_io") {
        for (tag, cp) in [("", 1252), ("932 ", 932), ("C ", 0)] {
            let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(&format!("{tag}{k} ")).map(str::to_string));
            let Some(m) = get("mbstowcs") else { continue }; // (a section the golden doesn't have yet)
            let ctx = format!("{b}-bit {tag:?}");
            let w = to_wide_in(cp, sj);
            let f: Vec<&str> = m.split(' ').collect();
            assert_eq!(f[0].parse::<usize>().unwrap(), w.len(), "{ctx} mbstowcs");
            for i in 0..3 {
                assert_eq!(hex16(f[1 + i]), w[i], "{ctx} mbstowcs [{i}]");
            }
            // mbrlen(sj, 2): 2 for a lead byte, else 1
            let ml = get("mbrlen").unwrap();
            assert_eq!(ml.split(' ').next().unwrap().parse::<u32>().unwrap(), if lead_in(cp, sj[0]) { 2 } else { 1 }, "{ctx} mbrlen");
            // mbtowc(sj + 3, 2)
            let mt = get("mbtowc").unwrap();
            let (n, u) = mt.split_once(' ').unwrap();
            let want: Vec<u16> = to_wide_in(cp, &sj[3..if lead_in(cp, sj[3]) { 5 } else { 4 }]);
            assert_eq!(n.parse::<usize>().unwrap(), if lead_in(cp, sj[3]) { 2 } else { 1 }, "{ctx} mbtowc");
            assert_eq!(hex16(u), want[0], "{ctx} mbtowc");
            // wcrtomb(0x3042): its bytes, or -1 when the code page lacks it
            let wr = get("wcrtomb").unwrap();
            let (n, bytes) = wr.split_once(' ').unwrap();
            match wc_bytes_in(cp, 0x3042) {
                Some(v) => {
                    assert_eq!(n.parse::<usize>().unwrap(), v.len(), "{ctx} wcrtomb");
                    assert_eq!(&bytes[..2 * v.len()], v.iter().map(|x| format!("{x:02x}")).collect::<String>(), "{ctx} wcrtomb");
                }
                None => assert_eq!(n, "-1", "{ctx} wcrtomb"),
            }
            // btowc / wctob (mingw-w64's convert in the ANSI code page in "C": not checked there)
            if cp != 0 {
                let bt: Vec<u16> = get("btowc").unwrap().split(' ').map(hex16).collect();
                for (i, byte) in [b'A', 0x82, 0xb1].into_iter().enumerate() {
                    let want = if lead_in(cp, byte) { 0xffff } else { to_wide_in(cp, &[byte])[0] };
                    assert_eq!(bt[i], want, "{ctx} btowc({byte:#x})");
                }
                let wb: Vec<i32> = get("wctob").unwrap().split(' ').map(|x| x.parse().unwrap()).collect();
                for (i, u) in [0x41u16, 0x3042, 0xff71].into_iter().enumerate() {
                    let want = match wc_bytes_in(cp, u) {
                        Some(v) if v.len() == 1 => v[0] as i8 as i32,
                        _ => -1,
                    };
                    // (wctob's byte is a char: msvcrt sign-extends it)
                    assert_eq!(wb[i], want, "{ctx} wctob({u:#x})");
                }
            }
            checked += 1;
        }
    }
    assert!(checked >= 2, "the crt_io golden has its multibyte lines");
}

/// Every character of the BMP in "C", 932 and 1252: the bytes of a character read back as one
/// character with the same bytes, and the first is a lead byte exactly when it takes two
#[test]
fn every_character_round_trips() {
    for cp in [0, 932, 1252] {
        let (mut have, mut shared) = (0, Vec::new());
        for u in 0..=0xffffu16 {
            if (0xd800..0xe000).contains(&u) {
                continue;
            }
            let Some(b) = wc_bytes_in(cp, u) else { continue };
            assert!(b.len() == 1 || b.len() == 2, "cp {cp} {u:#x} {b:x?}");
            assert_eq!(lead_in(cp, b[0]), b.len() == 2, "cp {cp} {u:#x} {b:x?}");
            // the bytes read back as one character whose bytes they are (several characters can
            // share bytes: 932's U+2212 and U+FF0D are both 81 7C, which reads as U+FF0D)
            let back = to_wide_in(cp, &b);
            assert_eq!(back.len(), 1, "cp {cp} {u:#x} {b:x?}");
            assert_eq!(wc_bytes_in(cp, back[0]).as_deref(), Some(&b[..]), "cp {cp} {u:#x} {b:x?}");
            // (Windows' best fit: 932 é is 'e', as WideCharToMultiByte gives it; WorstFit)
            let fit = char::from_u32(u as u32).and_then(crate::text::best_fit);
            if fit.is_some_and(|f| b == [f]) && back[0] != u {
                continue;
            }
            have += 1;
            if back[0] != u {
                shared.push(u);
            }
        }
        // ("C" and 1252 are one to one: Windows decodes all 256 bytes of 1252, 81 as U+0081 in
        // nls' golden; 932 has these three that share bytes: U+00A5 5C and U+203E 7E as in nls'
        // golden for A5, U+2212 81 7C)
        assert_eq!(shared, if cp == 932 { vec![0xa5, 0x203e, 0x2212] } else { vec![] }, "cp {cp}");
        match cp {
            0 | 1252 => assert_eq!(have, 256, "cp {cp}"),
            _ => assert!(have > 7000, "{have}"),
        }
    }
}

/// Every byte: a single non-lead byte is one character in each code page ("C": its value)
#[test]
fn every_byte_is_one_character() {
    for cp in [0, 932, 1252] {
        for b in 0..=255u8 {
            if lead_in(cp, b) {
                continue;
            }
            let w = to_wide_in(cp, &[b]);
            assert_eq!(w.len(), 1, "cp {cp} {b:#x}");
            if cp == 0 {
                assert_eq!(w[0], b as u16);
            }
        }
        assert_eq!((0..=255u8).filter(|&b| lead_in(cp, b)).count(), if cp == 932 { 60 } else { 0 }, "cp {cp}");
    }
}

proptest! {
    // Converting a string is converting each character: the bytes are the characters' bytes in
    // turn (the default for those it lacks) and "used" says whether any was missing
    #[test]
    fn default_char_is_per_character(w in prop::collection::vec(prop_oneof![0u16..0x80, 0x80u16..0x3100, 0xff00u16..0xffff, any::<u16>()], 0..100),
                                     cp in prop_oneof![Just(932u32), Just(1252u32)], def in 0x20u8..0x7f) {
        // (lone surrogates are left out: a pair is one character)
        let w: Vec<u16> = w.into_iter().filter(|u| !(0xd800..0xe000).contains(u)).collect();
        let (all, used) = crate::text::w2mb_def(cp, &w, &[def]);
        let mut each = Vec::new();
        let mut any = false;
        for &u in &w {
            let (b, u_used) = crate::text::w2mb_def(cp, &[u], &[def]);
            prop_assert_eq!(u_used, wc_bytes_in(cp, u).is_none());
            if u_used {
                prop_assert_eq!(&b, &vec![def]);
            }
            any |= u_used;
            each.extend(b);
        }
        prop_assert_eq!(all, each);
        prop_assert_eq!(used, any);
        prop_assert_eq!(crate::text::w2mb(cp, &w), crate::text::w2mb_def(cp, &w, b"?").0);
    }
}

/// kernel32's conversions as nls' golden has them: WideCharToMultiByte (bytes and
/// lpUsedDefaultChar), MultiByteToWideChar of 1252, IsDBCSLeadByteEx(932)
#[test]
fn nls_conversions_are_windows() {
    // Windows' best-fit mappings (é -> e, Ā -> A), which sake does not have: it writes the
    // default character for them
    const BEST_FIT: &[(u32, &str)] = &[(932, "00e9"), (1252, "20ac 0153 0100")];
    let g = golden("nls");
    assert!(!g.is_empty(), "nls golden missing");
    for (b, out) in &g {
        let mut n = 0;
        for l in out.lines() {
            // "w2m <cp> <flags> [ <units> ] -> <n> used <u> err <e>: <bytes>"
            let Some(rest) = l.strip_prefix("w2m ") else { continue };
            let Some((head, tail)) = rest.split_once(" ] -> ") else { continue };
            let (cpfl, units) = head.split_once(" [ ").unwrap();
            let (cp, fl) = cpfl.split_once(' ').unwrap();
            let cp: u32 = cp.parse().unwrap();
            if fl != "0" || !matches!(cp, 932 | 1252) || BEST_FIT.contains(&(cp, units)) {
                continue;
            }
            let w: Vec<u16> = units.split(' ').map(hex16).collect();
            let (_, after) = tail.split_once(" used ").unwrap();
            let (used, bytes) = after.split_once(" err 0:").unwrap();
            let want: Vec<u8> = bytes.split_whitespace().map(|x| u8::from_str_radix(x, 16).unwrap()).collect();
            let (got, got_used) = crate::text::w2mb_def(cp, &w, b"?");
            assert_eq!(got, want, "{b}-bit {l}");
            assert_eq!(got_used, used == "1", "{b}-bit {l}");
            n += 1;
        }
        assert!(n >= 12, "{b}-bit: {n} w2m lines checked");
        let l = out.lines().find(|l| l.starts_with("m2w 1252 0 ")).unwrap();
        let (src, dst) = l["m2w 1252 0 [ ".len()..].split_once(" ] -> ").unwrap();
        let bytes: Vec<u8> = src.split(' ').map(|x| u8::from_str_radix(x, 16).unwrap()).collect();
        let want: Vec<u16> = dst.split_once(':').unwrap().1.split_whitespace().map(hex16).collect();
        assert_eq!(to_wide_in(1252, &bytes), want, "{b}-bit {l}");
        let l = out.lines().find(|l| l.starts_with("IsDBCSLeadByteEx ")).unwrap();
        let want: Vec<bool> = l.split(' ').skip(1).map(|x| x == "1").collect();
        let got: Vec<bool> = (0x80..0x100).step_by(4).map(|c| crate::text::is_lead_cp(932, c as u8)).collect();
        assert_eq!(got, want, "{b}-bit IsDBCSLeadByteEx(932)");
        assert!((0..=255u8).all(|c| !crate::text::is_lead_cp(1252, c)));
    }
}

/// Lead bytes of every double-byte code page sake knows: exactly the bytes that, followed by a
/// valid trail byte (A1 is one in all four), read as one character
#[test]
fn lead_bytes_are_the_code_pages() {
    for cp in [932u32, 936, 949, 950] {
        for b in 0..=255u8 {
            // (characters, not units: Big5's 87 A1 is beyond the BMP)
            let one = String::from_utf16_lossy(&crate::text::mb2w(cp, &[b, 0xa1])).chars().count() == 1;
            assert_eq!(crate::text::is_lead_cp(cp, b), one, "cp {cp} {b:#x}");
        }
    }
    for b in 0..=255u8 {
        assert!(!crate::text::is_lead_cp(1252, b) && !crate::text::is_lead_cp(65001, b));
    }
}

#[test]
fn temporary_names_count_in_base_32() {
    assert_eq!(radix32(0), "0");
    assert_eq!(radix32(9), "9");
    assert_eq!(radix32(10), "a");
    assert_eq!(radix32(31), "v");
    assert_eq!(radix32(32), "10");
    assert_eq!(radix32(u32::MAX), "3vvvvvv");
}

#[test]
fn spawn_command_line_joins_as_is() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // (Ren'Py's launcher quotes its arguments itself)
    assert_eq!(spawn_command_line(&v(&["\"C:\\a b\\x.exe\"", "-EOO", "\"C:\\a b\\x.py\""])), ("\"C:\\a b\\x.exe\" -EOO \"C:\\a b\\x.py\"".into(), "-EOO \"C:\\a b\\x.py\"".into()));
    // (no quotes added: "a b" becomes two arguments, as on Windows)
    assert_eq!(spawn_command_line(&v(&["p", "a b"])), ("p a b".into(), "a b".into()));
    assert_eq!(spawn_command_line(&v(&["p"])), ("p".into(), "".into()));
    assert_eq!(spawn_command_line(&[]), ("".into(), "".into()));
}

proptest! {
    /// Arguments without spaces or quotes come back as they went in: the command line the
    /// C runtime builds splits (CreateProcess's rule, crate::process::split_command) into the
    /// program and the rest, which is the arguments joined by spaces.
    #[test]
    fn spawn_command_line_splits_back(argv in prop::collection::vec("[A-Za-z0-9_.:\\\\-]{1,12}", 1..6)) {
        let (cmd, args) = spawn_command_line(&argv);
        let (prog, rest) = crate::process::split_command(&cmd);
        prop_assert_eq!(prog, argv[0].clone());
        prop_assert_eq!(rest, args.clone());
        prop_assert_eq!(args.split(' ').filter(|s| !s.is_empty()).count(), argv.len() - 1);
    }
}
