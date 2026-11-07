//! Code pages: the ANSI code page is 932 (Shift_JIS, as WHATWG's = Windows-31J), the one
//! Japanese VNs are written for; SAKE_ACP=1252 for Western games.

use std::sync::OnceLock;

pub fn acp() -> u32 {
    static ACP: OnceLock<u32> = OnceLock::new();
    *ACP.get_or_init(|| std::env::var("SAKE_ACP").ok().and_then(|s| s.parse().ok()).unwrap_or(932))
}

fn enc(cp: u32) -> &'static encoding_rs::Encoding {
    match if cp == 0 || cp == 1 || cp == 3 { acp() } else { cp } {
        932 => encoding_rs::SHIFT_JIS,
        936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        65001 => encoding_rs::UTF_8,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        _ => encoding_rs::WINDOWS_1252,
    }
}

pub fn is_dbcs() -> bool {
    matches!(acp(), 932 | 936 | 949 | 950)
}

/// Lead byte of a double-byte character in the ANSI code page.
pub fn is_lead(b: u8) -> bool {
    is_lead_cp(0, b)
}

/// Lead byte of a double-byte character in code page `cp` (0 CP_ACP, 1 CP_OEMCP, 3
/// CP_THREAD_ACP: the ANSI one).
pub fn is_lead_cp(cp: u32, b: u8) -> bool {
    match if matches!(cp, 0 | 1 | 3) { acp() } else { cp } {
        932 => matches!(b, 0x81..=0x9f | 0xe0..=0xfc),
        936 | 949 | 950 => b >= 0x81 && b != 0xff,
        _ => false,
    }
}

pub fn mb2w(cp: u32, s: &[u8]) -> Vec<u16> {
    if cp == 65001 {
        return String::from_utf8_lossy(s).encode_utf16().collect();
    }
    let e = enc(cp);
    if e == encoding_rs::WINDOWS_1252 && s.is_ascii() {
        return s.iter().map(|&b| b as u16).collect();
    }
    let (cow, _) = e.decode_without_bom_handling(s);
    cow.encode_utf16().collect()
}

pub fn w2mb(cp: u32, s: &[u16]) -> Vec<u8> {
    let st = String::from_utf16_lossy(s);
    if cp == 65001 {
        return st.into_bytes();
    }
    let e = enc(cp);
    if st.is_ascii() {
        return st.into_bytes();
    }
    // (unmappable characters become '?', the default character, as on Windows; encoding_rs's
    // encode() would write HTML character references)
    let mut encoder = e.new_encoder();
    let src: Vec<u16> = st.encode_utf16().collect();
    let mut out = vec![0u8; src.len() * 4 + 16];
    let (mut read, mut written) = (0, 0);
    loop {
        let (r, n, w) = encoder.encode_from_utf16_without_replacement(&src[read..], &mut out[written..], true);
        read += n;
        written += w;
        match r {
            encoding_rs::EncoderResult::InputEmpty => break,
            encoding_rs::EncoderResult::Unmappable(_) => {
                out[written] = b'?';
                written += 1;
            }
            encoding_rs::EncoderResult::OutputFull => out.resize(out.len() * 2, 0),
        }
    }
    out.truncate(written);
    out
}

pub fn a2w(s: &[u8]) -> Vec<u16> {
    mb2w(0, s)
}

pub fn w2a(s: &[u16]) -> Vec<u8> {
    w2mb(0, s)
}

/// ANSI bytes as a Rust string (for paths, class names, logs).
pub fn a2s(s: &[u8]) -> String {
    if s.is_ascii() {
        return String::from_utf8_lossy(s).into_owned();
    }
    String::from_utf16_lossy(&a2w(s))
}

pub fn s2a(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        return s.as_bytes().to_vec();
    }
    w2a(&s.encode_utf16().collect::<Vec<_>>())
}

/// Windows' case tables (LCMapString, CharUpper, towupper outside "C"): Unicode's simple 1:1
/// mappings within the BMP; a character whose mapping is several (ß, ŉ) stays.
pub fn upper16(c: u16) -> u16 {
    if c < 0x80 {
        return if (0x61..=0x7a).contains(&c) { c - 32 } else { c };
    }
    case16(c, |ch| ch.to_uppercase())
}

pub fn lower16(c: u16) -> u16 {
    if c < 0x80 {
        return if (0x41..=0x5a).contains(&c) { c + 32 } else { c };
    }
    // (İ: its full lower case is i + a dot, its simple one i)
    if c == 0x130 {
        return 0x69;
    }
    case16(c, |ch| ch.to_lowercase())
}

fn case16<I: Iterator<Item = char>>(c: u16, f: fn(char) -> I) -> u16 {
    let Some(ch) = char::from_u32(c as u32) else { return c };
    let mut m = f(ch);
    match (m.next(), m.next()) {
        (Some(x), None) if (x as u32) < 0x10000 => x as u32 as u16,
        _ => c,
    }
}

/// Lower-cases ASCII letters of ANSI text, leaving double-byte characters alone.
pub fn lower_a(s: &mut [u8]) {
    let mut i = 0;
    while i < s.len() {
        if is_lead(s[i]) && i + 1 < s.len() {
            i += 2;
            continue;
        }
        s[i] = s[i].to_ascii_lowercase();
        i += 1;
    }
}

pub fn upper_a(s: &mut [u8]) {
    let mut i = 0;
    while i < s.len() {
        if is_lead(s[i]) && i + 1 < s.len() {
            i += 2;
            continue;
        }
        s[i] = s[i].to_ascii_uppercase();
        i += 1;
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn case_tables() {
        // ASCII, Latin-1, Greek, Cyrillic, full-width, and the ones Windows keeps or maps simply
        let pairs = [(0x41, 0x61), (0xc0, 0xe0), (0xde, 0xfe), (0x178, 0xff), (0x391, 0x3b1), (0x410, 0x430), (0xff21, 0xff41), (0x100, 0x101)];
        for (u, l) in pairs {
            assert_eq!(lower16(u), l, "{u:x}");
            assert_eq!(upper16(l), u, "{l:x}");
        }
        assert_eq!(upper16(0xdf), 0xdf); // ß
        assert_eq!(lower16(0x130), 0x69); // İ
        assert_eq!(upper16(0x131), 0x49); // ı
        assert_eq!(lower16(0xd7), 0xd7); // ×
        assert_eq!(upper16(0xf7), 0xf7); // ÷
        assert_eq!(lower16(0x3042), 0x3042); // あ
        assert_eq!(lower16(0xd800), 0xd800); // (a lone surrogate)
    }

    /// Every BMP code point (not a sample): case mapping never leaves the BMP, ASCII stays
    /// ASCII, mapping twice is mapping once, and only letters change. No round trip through the
    /// other case: case pairs are not one to one (U+1C80 CYRILLIC SMALL LETTER ROUNDED VE
    /// upper-cases to U+0412, which lower-cases to U+0432; ſ, ς, µ, ı, ϐ the same; ϴ U+03F4
    /// lower-cases to θ, which upper-cases to Θ U+0398). Which letter maps to which is Windows'
    /// table, the winref probe casemap (tests/winref/probes/casemap.c), and case_tables.
    #[test]
    fn case_maps_every_code_point() {
        for c in 0..=0xffffu16 {
            let (l, u) = (lower16(c), upper16(c));
            if c < 0x80 {
                assert!(l < 0x80 && u < 0x80, "{c:x}");
            }
            assert_eq!(upper16(u), u, "upper twice {c:x}");
            assert_eq!(lower16(l), l, "lower twice {c:x}");
            // (only letters change)
            if u != c || l != c {
                assert!(char::from_u32(c as u32).is_some_and(char::is_alphabetic), "{c:x}");
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Text a Japanese game writes (ASCII, kana, kanji, full-width, half-width katakana; the
        /// wave dash is U+FF5E in Windows-31J, U+301C has no code; NEC/IBM extensions included)
        /// survives Shift_JIS both ways.
        #[test]
        fn cp932_round_trip(s in "[ -~ぁ-んァ-ヶ日本語漢字学校生活時間世界東京名前今年月火水木金土私君彼女愛夢恋剣魔法王姫髙﨑ａ-ｚ０-９｡-ﾟ「」、。・ー～①Ⅰ]{0,40}") {
            let w: Vec<u16> = s.encode_utf16().collect();
            let mb = w2mb(932, &w);
            prop_assert_eq!(mb2w(932, &mb), w);
        }

        /// Any bytes in any code page: decoding then encoding again is stable (a second trip
        /// changes nothing), never a panic.
        #[test]
        fn any_bytes_settle(b in proptest::collection::vec(any::<u8>(), 0..64), cp in prop::sample::select(vec![932u32, 936, 949, 950, 65001, 1250, 1251, 1252])) {
            let once = w2mb(cp, &mb2w(cp, &b));
            let w = mb2w(cp, &once);
            prop_assert_eq!(mb2w(cp, &w2mb(cp, &w)), w);
        }

        /// ASCII is itself in every code page.
        #[test]
        fn ascii_everywhere(s in "[\\x00-\\x7f]{0,40}", cp in prop::sample::select(vec![932u32, 936, 949, 950, 65001, 1250, 1251, 1252])) {
            let w: Vec<u16> = s.encode_utf16().collect();
            prop_assert_eq!(mb2w(cp, s.as_bytes()), w.clone());
            prop_assert_eq!(w2mb(cp, &w), s.as_bytes().to_vec());
        }

        /// Unmappable characters become '?' (one per character), as on Windows.
        #[test]
        fn unmappable_is_question_mark(n in 1usize..8) {
            let w: Vec<u16> = "😀".repeat(n).encode_utf16().collect();
            prop_assert_eq!(w2mb(932, &w), vec![b'?'; n]);
        }
    }
}
