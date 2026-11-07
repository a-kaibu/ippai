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

    /// Ignoring a difference can only make strings equal, never reverse them
    #[test]
    fn ignoring_coarsens(a in text(), b in text(), f in flags()) {
        let fine = compare(&a, &b, 0);
        let coarse = compare(&a, &b, f & !SORT_STRINGSORT);
        if coarse != Ordering::Equal && f & NORM_IGNORESYMBOLS == 0 {
            prop_assert_eq!(coarse, fine);
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
