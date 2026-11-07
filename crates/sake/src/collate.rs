//! CompareString / lstrcmp ordering: Windows' "word sort" by default (hyphens and apostrophes
//! only break ties), SORT_STRINGSORT making them symbols. A string's key is, per character, a
//! primary weight (symbols < digits < Latin < Greek < Cyrillic < kana < the rest), a diacritic
//! weight, and a tertiary one (case, width, kana kind); keys compare level by level, the
//! hyphens and apostrophes last. Measured on Windows Server 2025 (tests/winref/probes/nls.c,
//! 22 strings against each other in five ways): a < A < b, -a after a and before A, _a before
//! a, ' < - < _ < ・ < digits, あ > ア > ｱ (case-insensitive too), "10" < "2".

use std::cmp::Ordering;

pub const NORM_IGNORECASE: u32 = 1;
pub const NORM_IGNORENONSPACE: u32 = 2;
pub const NORM_IGNORESYMBOLS: u32 = 4;
pub const SORT_STRINGSORT: u32 = 0x1000;
pub const NORM_IGNOREKANATYPE: u32 = 0x10000;
pub const NORM_IGNOREWIDTH: u32 = 0x20000;

/// ASCII symbols in Windows' order (string sort puts ' and - first)
const SYMBOLS: &[u8] = b"'-\t\n\x0b\x0c\r !\"#$%&()*,./:;?@[\\]^_`{|}~+<=>";

const G_SYMBOL: u32 = 1;
const G_DIGIT: u32 = 2;
const G_LATIN: u32 = 3;
const G_GREEK: u32 = 4;
const G_CYRILLIC: u32 = 5;
const G_KANA: u32 = 6;
const G_OTHER: u32 = 7;

/// Tertiary bits
const UPPER: u8 = 1;
const WIDE: u8 = 2;
/// kana kind: a half-width katakana 0, a full-width one KATA, a hiragana KATA | HIRA
const KATA: u8 = 4;
const HIRA: u8 = 8;
const SMALL_NOT: u8 = 16;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Weight {
    /// (primary, diacritic, tertiary)
    Char(u32, u8, u8),
    /// a hyphen or apostrophe in word sort: its order among them
    Special(u8),
    Ignore,
}

/// The katakana of a kana (hiragana and half-width ones too), with its tertiary kind
fn kana(c: u16) -> Option<(u16, u8)> {
    match c {
        0x3041..=0x3096 | 0x309d | 0x309e => Some((c + 0x60, KATA | HIRA)),
        0x30a1..=0x30fa | 0x30fc..=0x30fe => Some((c, KATA)),
        0xff66..=0xff9d => Some((crate::text::half_kana_full(c), 0)),
        _ => None,
    }
}

/// A katakana's plain large form and its diacritic (1 voiced, 2 semi-voiced) and whether small
fn kana_base(k: u16) -> (u16, u8, bool) {
    if let Some((b, m)) = crate::text::kana_voiced(k) {
        return (b, if m == 0xff9e { 1 } else { 2 }, false);
    }
    match k {
        0x30a1 | 0x30a3 | 0x30a5 | 0x30a7 | 0x30a9 | 0x30c3 | 0x30e3 | 0x30e5 | 0x30e7 | 0x30ee => (k + 1, 0, true),
        0x30f5 => (0x30ab, 0, true),
        0x30f6 => (0x30b1, 0, true),
        _ => (k, 0, false),
    }
}

fn weight(c: u16, flags: u32) -> Weight {
    let p = |g: u32, w: u32| (g << 24) | w;
    // full-width ASCII forms: the ASCII character, wide
    let (c, wide) = match c {
        0xff01..=0xff5e => (c - 0xfee0, WIDE),
        0x3000 => (0x20, WIDE),
        // (the half-width ｡｢｣､･ are the full-width 。「」、・ narrow)
        0xff61..=0xff65 => (crate::text::half_kana_full(c), 0),
        0x3001 | 0x3002 | 0x300c | 0x300d | 0x30fb => (c, WIDE),
        _ => (c, 0),
    };
    if flags & SORT_STRINGSORT == 0 && matches!(c, 0x27 | 0x2d | 0xad | 0x2010 | 0x2011) {
        return Weight::Special(if c == 0x27 { 0 } else { 1 });
    }
    if let Some(i) = SYMBOLS.iter().position(|&s| s as u16 == c) {
        return Weight::Char(p(G_SYMBOL, i as u32), 0, wide);
    }
    match c {
        0..=0x1f | 0x7f..=0x9f => Weight::Ignore,
        0x30..=0x39 => Weight::Char(p(G_DIGIT, (c - 0x30) as u32), 0, wide),
        0x41..=0x5a => Weight::Char(p(G_LATIN, (c - 0x41) as u32), 0, wide | UPPER),
        0x61..=0x7a => Weight::Char(p(G_LATIN, (c - 0x61) as u32), 0, wide),
        _ => {
            let ch = char::from_u32(c as u32);
            if let Some(b) = ch.and_then(crate::text::best_fit) {
                // (a Latin letter with marks: its letter, and the mark as the diacritic)
                let up = b.is_ascii_uppercase();
                return Weight::Char(p(G_LATIN, (b.to_ascii_lowercase() - b'a') as u32), (crate::text::lower16(c) & 0xff) as u8 | 1, if up { UPPER } else { 0 });
            }
            if let Some((k, kind)) = kana(c) {
                let (base, dia, small) = kana_base(k);
                return Weight::Char(p(G_KANA, base as u32), dia, kind | if small { 0 } else { SMALL_NOT });
            }
            let lower = crate::text::lower16(c);
            let up = if lower != c { UPPER } else { 0 };
            let is_alnum = ch.is_some_and(char::is_alphanumeric);
            match c {
                0x370..=0x3ff => Weight::Char(p(G_GREEK, lower as u32), 0, up),
                0x400..=0x4ff => Weight::Char(p(G_CYRILLIC, lower as u32), 0, up),
                0xc0..=0x24f => Weight::Char(p(G_LATIN, 26 + lower as u32), 0, up),
                _ if !is_alnum => Weight::Char(p(G_SYMBOL, 0x100 + c as u32), 0, 0),
                _ => Weight::Char(p(G_OTHER, lower as u32), 0, up),
            }
        }
    }
}

/// The levels of a string's key: primaries, diacritics, tertiaries, specials (position, kind)
fn key(s: &[u16], flags: u32) -> (Vec<u32>, Vec<u8>, Vec<u8>, Vec<(usize, u8)>) {
    let (mut p, mut d, mut t, mut sp) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, &c) in s.iter().enumerate() {
        match weight(c, flags) {
            Weight::Ignore => {}
            Weight::Special(k) => {
                if flags & NORM_IGNORESYMBOLS == 0 {
                    sp.push((i, k));
                }
            }
            Weight::Char(pr, di, mut te) => {
                if flags & NORM_IGNORESYMBOLS != 0 && pr >> 24 == G_SYMBOL {
                    continue;
                }
                if flags & NORM_IGNORECASE != 0 {
                    te &= !UPPER;
                }
                if flags & NORM_IGNOREWIDTH != 0 {
                    te &= !WIDE;
                    if te & KATA == 0 && pr >> 24 == G_KANA {
                        te |= KATA;
                    }
                }
                if flags & NORM_IGNOREKANATYPE != 0 {
                    te &= !HIRA;
                }
                p.push(pr);
                d.push(if flags & NORM_IGNORENONSPACE != 0 { 0 } else { di });
                t.push(te);
            }
        }
    }
    (p, d, t, sp)
}

/// CompareString's answer for UTF-16 strings `a` and `b` under `flags`
pub fn compare(a: &[u16], b: &[u16], flags: u32) -> Ordering {
    let (ka, kb) = (key(a, flags), key(b, flags));
    ka.0.cmp(&kb.0).then_with(|| ka.1.cmp(&kb.1)).then_with(|| ka.2.cmp(&kb.2)).then_with(|| ka.3.iter().map(|x| x.1).cmp(kb.3.iter().map(|x| x.1)))
}

#[cfg(test)]
#[path = "collate_tests.rs"]
mod tests;
