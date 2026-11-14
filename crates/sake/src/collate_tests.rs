//! The ordering checked against Windows' own answers (tests/winref/golden/nls-*: the probe's
//! 22 strings against each other with CompareStringA 0x411 in three ways and lstrcmp(i)), and
//! by the properties of an order (proptest).

use super::*;
use proptest::prelude::*;

/// nls.c's `ws`, in code page 932 as the probe's manifest makes it
const WS: [&[u8]; 22] = [b"a", b"A", b"b", b"B", b"-a", b"a-b", b"ab", b"co-op", b"coop", b"a'b", b"_a", b"1", b"10", b"2", b"", b"\x82\xa0", b"\x83\x41", b"\xb1", b"z", b"Z", b"\xe9", b"\xc9"];

fn golden() -> Vec<String> {
    ["32", "64"].iter().filter_map(|b| std::fs::read_to_string(format!("{}/../../tests/winref/golden/nls-{b}/stdout.txt", env!("CARGO_MANIFEST_DIR"))).ok()).collect()
}

fn digit(o: Ordering) -> u8 {
    b'0' + (o as i32 + 1) as u8
}

#[test]
fn orders_as_windows() {
    let g = golden();
    assert!(!g.is_empty(), "nls golden missing");
    let w: Vec<Vec<u16>> = WS.iter().map(|s| crate::text::mb2w(932, s)).collect();
    for out in g {
        for i in 0..WS.len() {
            let row = out.lines().find(|l| l.starts_with(&format!("cmp {i}:"))).unwrap();
            let cells: Vec<&str> = row.split_whitespace().skip(2).collect();
            for j in 0..WS.len() {
                let (a, b) = (&w[i], &w[j]);
                // CompareString 0, NORM_IGNORECASE, SORT_STRINGSORT, lstrcmp, lstrcmpi
                let got: Vec<u8> = [0, NORM_IGNORECASE, SORT_STRINGSORT, 0, NORM_IGNORECASE].iter().map(|&f| digit(compare(a, b, f))).collect();
                assert_eq!(std::str::from_utf8(&got).unwrap(), cells[j], "{:?} vs {:?}", WS[i], WS[j]);
            }
        }
        // CompareStringW kana 2 width 2: あ = ア ignoring kana type, A = Ａ ignoring width
        assert_eq!(compare(&[0x3042], &[0x30a2], NORM_IGNOREKANATYPE), Ordering::Equal);
        assert_eq!(compare(&[0x41], &[0xff21], NORM_IGNOREWIDTH), Ordering::Equal);
        assert!(out.contains("CompareStringW kana 2 width 2"));
    }
}

/// The level that decides `a` against `b`: 0 primary, 1 diacritic, 2 tertiary, 3 hyphens and
/// apostrophes, 4 equal
fn level(a: &[u16], b: &[u16], flags: u32) -> usize {
    let (ka, kb) = (key(a, flags), key(b, flags));
    let sp = |k: &(Vec<u32>, Vec<u8>, Vec<u8>, Vec<(usize, u8)>)| k.3.iter().map(|x| x.1).collect::<Vec<u8>>();
    [ka.0 != kb.0, ka.1 != kb.1, ka.2 != kb.2, sp(&ka) != sp(&kb)].iter().position(|&d| d).unwrap_or(4)
}

/// Found by the proptest: a later level decides once a higher one is ignored
#[test]
fn ignored_level_lets_the_next_decide() {
    use Ordering::*;
    // è > Ｅ by the grave; ignoring marks, lower-case narrow e < upper-case wide Ｅ
    assert_eq!(compare(&[0xe8], &[0xff25], 0), Greater);
    assert_eq!(compare(&[0xe8], &[0xff25], NORM_IGNORECASE), Greater);
    assert_eq!(compare(&[0xe8], &[0xff25], NORM_IGNORENONSPACE), Less);
    assert_eq!(compare(&[0xe8], &[0xff25], NORM_IGNORENONSPACE | NORM_IGNORECASE | NORM_IGNOREWIDTH), Equal);
    // Z < ž by the caron; ignoring marks, Z > z by case
    assert_eq!(compare(&[0x5a], &[0x17e], 0), Less);
    assert_eq!(compare(&[0x5a], &[0x17e], NORM_IGNORENONSPACE), Greater);
    assert_eq!(compare(&[0x5a], &[0x17e], NORM_IGNORENONSPACE | NORM_IGNORECASE), Equal);
}

/// Every pair of the collate probe's strings under its eight flag sets, as Windows answered
/// (tests/winref/probes/collate.c; skipped until the winref workflow has recorded it)
#[test]
fn levels_as_windows() {
    const F: [u32; 8] = [0, NORM_IGNORECASE, NORM_IGNORENONSPACE, NORM_IGNOREWIDTH, NORM_IGNOREKANATYPE, NORM_IGNORECASE | NORM_IGNOREKANATYPE | NORM_IGNOREWIDTH, NORM_IGNORESYMBOLS, SORT_STRINGSORT];
    let outs: Vec<String> = ["32", "64"].iter().filter_map(|b| std::fs::read_to_string(format!("{}/../../tests/winref/golden/collate-{b}/stdout.txt", env!("CARGO_MANIFEST_DIR"))).ok()).collect();
    if outs.is_empty() {
        eprintln!("collate golden not recorded yet");
        return;
    }
    let mut bad = Vec::new();
    for out in outs {
        for table in ["one", "two"] {
            let strs: Vec<Vec<u16>> = out.lines().filter_map(|l| l.strip_prefix(&format!("{table} str "))).map(|l| l.split(':').nth(1).unwrap().split_whitespace().map(|h| u16::from_str_radix(h, 16).unwrap()).collect()).collect();
            for (i, row) in out.lines().filter_map(|l| l.strip_prefix(&format!("{table} row "))).enumerate() {
                for (j, cell) in row.split(':').nth(1).unwrap().split_whitespace().enumerate() {
                    let got: String = F.iter().map(|&f| digit(compare(&strs[i], &strs[j], f)) as char).collect();
                    if got != cell {
                        bad.push(format!("{:x?} vs {:x?}: windows {cell} sake {got}", strs[i], strs[j]));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad[..bad.len().min(60)].join("\n"));
}

fn text() -> impl Strategy<Value = Vec<u16>> {
    prop::collection::vec(
        prop_oneof![
            prop::sample::select(b"aAbBzZ09-'_ .!~".map(u16::from).to_vec()),
            0x3041u16..0x3097,
            0x30a1u16..0x30fb,
            0xff61u16..0xffa0,
            0xff01u16..0xff5f,
            0xc0u16..0x180,
            0x391u16..0x3ca,
            0x410u16..0x450,
            0x4e00u16..0x4e10,
        ],
        0..6,
    )
}

fn flags() -> impl Strategy<Value = u32> {
    prop::sample::select(vec![0, NORM_IGNORECASE, SORT_STRINGSORT, NORM_IGNOREKANATYPE, NORM_IGNOREWIDTH, NORM_IGNORENONSPACE, NORM_IGNORESYMBOLS, NORM_IGNORECASE | NORM_IGNOREKANATYPE | NORM_IGNOREWIDTH])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// A total order: a string equals itself, swapping the sides reverses the answer, and
    /// a <= b <= c gives a <= c
    #[test]
    fn is_an_order(a in text(), b in text(), c in text(), f in flags()) {
        prop_assert_eq!(compare(&a, &a, f), Ordering::Equal);
        prop_assert_eq!(compare(&a, &b, f), compare(&b, &a, f).reverse());
        if compare(&a, &b, f) != Ordering::Greater && compare(&b, &c, f) != Ordering::Greater {
            prop_assert_ne!(compare(&a, &c, f), Ordering::Greater);
        }
    }

    /// Ignoring a difference never touches the levels above it: an answer the primary weights
    /// decide stays under every flag, one the diacritics decide stays ignoring case, width or
    /// kana kind. (Below the ignored level a later difference may decide the other way, as on
    /// Windows: è > Ｅ by the mark, è < Ｅ ignoring it by case; tests/winref/golden/collate-*)
    #[test]
    fn ignoring_keeps_higher_levels(a in text(), b in text(), f in flags()) {
        let f = f & !SORT_STRINGSORT;
        let fine = compare(&a, &b, 0);
        let coarse = compare(&a, &b, f);
        let touched = if f & NORM_IGNORENONSPACE != 0 { 1 } else if f != 0 { 2 } else { 4 };
        if f & NORM_IGNORESYMBOLS == 0 && level(&a, &b, 0) < touched {
            prop_assert_eq!(coarse, fine);
        }
        // and what decides ignoring is never above what decided before
        if f & NORM_IGNORESYMBOLS == 0 {
            prop_assert!(level(&a, &b, f) >= level(&a, &b, 0));
        }
    }

    /// Case, width and kana kind: upper-casing ASCII, widening and switching kana kind are
    /// what the matching NORM_IGNORE* flag ignores
    #[test]
    fn ignore_flags_ignore(a in text()) {
        // (letters whose case pair maps back: ı upper-cases to I, a different letter)
        let up: Vec<u16> = a.iter().map(|&c| if crate::text::lower16(crate::text::upper16(c)) == c { crate::text::upper16(c) } else { c }).collect();
        prop_assert_eq!(compare(&a, &up, NORM_IGNORECASE), Ordering::Equal);
        let kata = crate::text::map_kana_width(&a, 0x200000);
        prop_assert_eq!(compare(&a, &kata, NORM_IGNOREKANATYPE), Ordering::Equal);
        let wide = crate::text::map_kana_width(&a, 0x800000);
        // (widening joins a half-width kana and its mark into one, which keeps the order)
        if !a.iter().any(|&c| c == 0xff9e || c == 0xff9f) {
            prop_assert_eq!(compare(&a, &wide, NORM_IGNOREWIDTH), Ordering::Equal);
        }
    }
}
