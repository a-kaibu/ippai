//! Code pages: the ANSI code page is 932 (Shift_JIS, as WHATWG's = Windows-31J), the one
//! Japanese VNs are written for; SAKE_ACP=1252 for Western games.

use std::sync::OnceLock;

pub fn acp() -> u32 {
    static ACP: OnceLock<u32> = OnceLock::new();
    *ACP.get_or_init(|| {
        std::env::var("SAKE_ACP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(932)
    })
}

fn enc(cp: u32) -> &'static encoding_rs::Encoding {
    match if cp == 0 || cp == 1 || cp == 3 {
        acp()
    } else {
        cp
    } {
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
    mb2w_checked(cp, s).0
}

/// Code page 932's default Unicode character (KATAKANA MIDDLE DOT): what bytes that are no
/// character decode to
pub const DEFAULT_932: u16 = 0x30fb;

/// `s` decoded as Windows decodes it, and whether some bytes were no character (what
/// MB_ERR_INVALID_CHARS refuses). Measured on Windows (tests/winref/probes/nls.c):
/// - 932: a lead byte takes the next byte along whatever it is; a pair that is no character,
///   and a lead byte at the end, is U+30FB. 0x80 is U+0080, 0xA0 and 0xFD..0xFF are U+F8F0..3
///   (but invalid all the same).
/// - UTF-8: each maximal invalid subsequence is one U+FFFD, except that ED A0..BF (the start
///   of an encoded surrogate) is one.
pub fn mb2w_checked(cp: u32, s: &[u8]) -> (Vec<u16>, bool) {
    let real = if matches!(cp, 0 | 1 | 3) { acp() } else { cp };
    if real == 65001 {
        return utf8_to_w(s);
    }
    if real == 932 {
        return sjis_to_w(s);
    }
    let e = enc(cp);
    if e == encoding_rs::WINDOWS_1252 && s.is_ascii() {
        return (s.iter().map(|&b| b as u16).collect(), false);
    }
    let (cow, bad) = e.decode_without_bom_handling(s);
    (cow.encode_utf16().collect(), bad)
}

fn utf8_to_w(s: &[u8]) -> (Vec<u16>, bool) {
    let mut out = Vec::with_capacity(s.len());
    let mut bad = false;
    let mut rest = s;
    while let Some(ch) = rest.utf8_chunks().next() {
        out.extend(ch.valid().encode_utf16());
        let inv = ch.invalid();
        if inv.is_empty() {
            break;
        }
        bad = true;
        out.push(0xfffd);
        let mut used = ch.valid().len() + inv.len();
        if inv == [0xed] && rest.get(used).is_some_and(|b| (0xa0..=0xbf).contains(b)) {
            used += 1;
        }
        rest = &rest[used..];
    }
    (out, bad)
}

fn sjis_to_w(s: &[u8]) -> (Vec<u16>, bool) {
    // (the common case: valid text, where WHATWG's Shift_JIS is Windows')
    let (cow, err) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(s);
    if !err && !s.contains(&0x80) {
        return (cow.encode_utf16().collect(), false);
    }
    let mut out = Vec::with_capacity(s.len());
    let mut bad = false;
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        match b {
            0..=0x7f => out.push(b as u16),
            0xa1..=0xdf => out.push(0xff61 + (b - 0xa1) as u16),
            0x80 => {
                out.push(0x80);
                bad = true;
            }
            0xa0 | 0xfd..=0xff => {
                out.push(if b == 0xa0 {
                    0xf8f0
                } else {
                    0xf8f1 + (b - 0xfd) as u16
                });
                bad = true;
            }
            _ => {
                let Some(&t) = s.get(i + 1) else {
                    out.push(DEFAULT_932);
                    bad = true;
                    break;
                };
                let pair = [b, t];
                let (cow, err) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(&pair);
                let w: Vec<u16> = cow.encode_utf16().collect();
                if err || w.len() != 1 {
                    out.push(DEFAULT_932);
                    bad = true;
                } else {
                    out.push(w[0]);
                }
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    (out, bad)
}

pub fn w2mb(cp: u32, s: &[u16]) -> Vec<u8> {
    w2mb_def(cp, s, b"?").0
}

/// Windows' best-fit mappings for the Latin letters with marks (U+00C0..U+017F) that a code
/// page lacks: the letter without its marks (932 é -> e, 1252 Ā -> A; measured, winref nls).
/// '.' is none.
const BEST_FIT_LATIN: &[u8; 192] = b"AAAAAA.CEEEEIIII.NOOOOO..UUUUY..aaaaaa.ceeeeiiii.nooooo..uuuuy.yAaAaAaCcCcCcCcDd..EeEeEeEeEeGgGgGgGgHh..IiIiIiIiI...JjKk.LlLlLl....NnNnNn...OoOoOo..RrRrRrSsSsSsSsTtTt..UuUuUuUuUuUuWwYyYZzZzZz.";

pub fn best_fit(ch: char) -> Option<u8> {
    let c = ch as u32;
    let b = *BEST_FIT_LATIN.get(c.checked_sub(0xc0)? as usize)?;
    (b != b'.').then_some(b)
}

/// `s` in code page `cp`, a character the code page lacks as `def` (Windows' default character,
/// '?' unless lpDefaultChar names one): (bytes, whether `def` was used)
pub fn w2mb_def(cp: u32, s: &[u16], def: &[u8]) -> (Vec<u8>, bool) {
    w2mb_flags(cp, s, def, true)
}

/// w2mb_def; `best_fit` false is WC_NO_BEST_FIT_CHARS
pub fn w2mb_flags(cp: u32, s: &[u16], def: &[u8], best_fit: bool) -> (Vec<u8>, bool) {
    let st = String::from_utf16_lossy(s);
    if cp == 65001 {
        return (st.into_bytes(), false);
    }
    let e = enc(cp);
    if st.is_ascii() {
        return (st.into_bytes(), false);
    }
    // (encoding_rs's encode() would write HTML character references)
    let mut encoder = e.new_encoder();
    let src: Vec<u16> = st.encode_utf16().collect();
    // (in chunks of 64 bytes: OutputFull just means the next chunk)
    let (mut out, mut buf) = (Vec::with_capacity(src.len() * 2), [0u8; 64]);
    let (mut read, mut used) = (0, false);
    loop {
        let (r, n, w) = encoder.encode_from_utf16_without_replacement(&src[read..], &mut buf, true);
        read += n;
        out.extend_from_slice(&buf[..w]);
        match r {
            encoding_rs::EncoderResult::InputEmpty => break,
            // (932's user-defined area F040..F9FC is U+E000..U+E757 both ways on Windows; WHATWG
            // only decodes it)
            encoding_rs::EncoderResult::Unmappable(ch)
                if e == encoding_rs::SHIFT_JIS && ('\u{e000}'..='\u{e757}').contains(&ch) =>
            {
                let p = 8836 + (ch as u32 - 0xe000);
                let (lead, trail) = (p / 188, p % 188);
                out.push((lead + if lead < 0x1f { 0x81 } else { 0xc1 }) as u8);
                out.push((trail + if trail < 0x3f { 0x40 } else { 0x41 }) as u8);
            }
            encoding_rs::EncoderResult::Unmappable(ch) => {
                match best_fit.then(|| self::best_fit(ch)).flatten() {
                    Some(b) => out.push(b),
                    None => {
                        out.extend_from_slice(def);
                        used = true;
                    }
                }
            }
            encoding_rs::EncoderResult::OutputFull => {}
        }
    }
    (out, used)
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

/// Windows' case tables (LCMapString, CharUpper, towupper outside "C"): every BMP code point as
/// Windows Server 2025 maps it (tests/winref/golden/casemap, src/casemap_table.rs; its own
/// Unicode version: no Greek iota-subscript title pairs, µ ſ ı stay)
pub fn upper16(c: u16) -> u16 {
    if c < 0x80 {
        return if (0x61..=0x7a).contains(&c) {
            c - 32
        } else {
            c
        };
    }
    case_tables().0[c as usize]
}

pub fn lower16(c: u16) -> u16 {
    if c < 0x80 {
        return if (0x41..=0x5a).contains(&c) {
            c + 32
        } else {
            c
        };
    }
    case_tables().1[c as usize]
}

/// (upper, lower) for all 65536 code units, built once from the runs
fn case_tables() -> &'static (Vec<u16>, Vec<u16>) {
    static T: OnceLock<(Vec<u16>, Vec<u16>)> = OnceLock::new();
    T.get_or_init(|| {
        let build = |runs: &[(u16, u16, u8, i32)]| {
            let mut t: Vec<u16> = (0..=0xffffu16).collect();
            for &(a, b, step, d) in runs {
                for c in (a..=b).step_by(step as usize) {
                    t[c as usize] = (c as i32 + d) as u16;
                }
            }
            t
        };
        (
            build(&crate::casemap_table::UPPER),
            build(&crate::casemap_table::LOWER),
        )
    })
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

/// Case-maps ANSI text as user32's CharUpperA/CharLowerA and LCMapStringA do: through
/// Unicode, double-byte characters too (932 ａ 82 81 -> Ａ 82 60), where the mapped character
/// keeps its length.
pub fn case_a(s: &mut [u8], upper: bool) {
    let mut i = 0;
    while i < s.len() {
        let n = if is_lead(s[i]) && i + 1 < s.len() {
            2
        } else {
            1
        };
        if n == 1 && s[i] < 0x80 {
            s[i] = if upper {
                s[i].to_ascii_uppercase()
            } else {
                s[i].to_ascii_lowercase()
            };
        } else if let [w] = a2w(&s[i..i + n])[..] {
            let m = if upper { upper16(w) } else { lower16(w) };
            let b = w2a(&[m]);
            if m != w && b.len() == n {
                s[i..i + n].copy_from_slice(&b);
            }
        }
        i += n;
    }
}

/// The full-width forms of the half-width katakana U+FF61..U+FF9F (｡ ｢ ｣ ､ ･ ｦ ｧ ... ﾝ ﾞ ﾟ)
const HALF_KANA: [u16; 63] = [
    0x3002, 0x300c, 0x300d, 0x3001, 0x30fb, 0x30f2, 0x30a1, 0x30a3, 0x30a5, 0x30a7, 0x30a9, 0x30e3,
    0x30e5, 0x30e7, 0x30c3, 0x30fc, //
    0x30a2, 0x30a4, 0x30a6, 0x30a8, 0x30aa, 0x30ab, 0x30ad, 0x30af, 0x30b1, 0x30b3, 0x30b5, 0x30b7,
    0x30b9, 0x30bb, 0x30bd, 0x30bf, //
    0x30c1, 0x30c4, 0x30c6, 0x30c8, 0x30ca, 0x30cb, 0x30cc, 0x30cd, 0x30ce, 0x30cf, 0x30d2, 0x30d5,
    0x30d8, 0x30db, 0x30de, 0x30df, //
    0x30e0, 0x30e1, 0x30e2, 0x30e4, 0x30e6, 0x30e8, 0x30e9, 0x30ea, 0x30eb, 0x30ec, 0x30ed, 0x30ef,
    0x30f3, 0x309b, 0x309c,
];

/// A katakana with a (semi-)voiced mark as its plain kana and the mark: ガ -> (カ, ﾞ)
fn voiced(c: u16) -> Option<(u16, u16)> {
    match c {
        0x30ac..=0x30c2 if c % 2 == 0 => Some((c - 1, 0xff9e)),
        0x30c5 | 0x30c7 | 0x30c9 => Some((c - 1, 0xff9e)),
        0x30d0..=0x30dd => match (c - 0x30cf) % 3 {
            1 => Some((c - 1, 0xff9e)),
            2 => Some((c - 2, 0xff9f)),
            _ => None,
        },
        0x30f4 => Some((0x30a6, 0xff9e)),
        0x30f7 => Some((0x30ef, 0xff9e)),
        0x30fa => Some((0x30f2, 0xff9e)),
        _ => None,
    }
}

/// The full-width form of half-width kana `c` (U+FF61..U+FF9F)
pub fn half_kana_full(c: u16) -> u16 {
    HALF_KANA[(c - 0xff61) as usize]
}

pub fn kana_voiced(c: u16) -> Option<(u16, u16)> {
    voiced(c)
}

fn half_of(c: u16) -> Option<u16> {
    HALF_KANA
        .iter()
        .position(|&x| x == c)
        .map(|i| 0xff61 + i as u16)
}

/// LCMAP_HALFWIDTH: ASCII forms, the ideographic space, katakana (a voiced one as two)
fn to_half(w: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(w.len());
    for &c in w {
        match c {
            0xff01..=0xff5e => out.push(c - 0xfee0),
            0x3000 => out.push(0x20),
            // (the marks ゛ ゜ and the brackets keep their full forms: only kana narrow)
            0x30a1..=0x30fc => match (half_of(c), voiced(c)) {
                (Some(h), _) => out.push(h),
                (None, Some((b, m))) if half_of(b).is_some() => {
                    out.extend([half_of(b).unwrap(), m])
                }
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    out
}

/// LCMAP_FULLWIDTH: the other way, a half-width kana and its mark as one katakana
fn to_full(w: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(w.len());
    let mut i = 0;
    while i < w.len() {
        let c = w[i];
        match c {
            0x21..=0x7e => out.push(c + 0xfee0),
            0x20 => out.push(0x3000),
            0xff61..=0xff9f => {
                let f = HALF_KANA[(c - 0xff61) as usize];
                let next = w.get(i + 1).copied();
                let joined = (0x30a1..=0x30fa)
                    .flat_map(|v| voiced(v).map(|x| (v, x)))
                    .find(|&(_, (b, m))| b == f && Some(m) == next);
                match joined {
                    Some((v, _)) => {
                        out.push(v);
                        i += 1;
                    }
                    None => out.push(f),
                }
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// LCMapString's LCMAP_HIRAGANA (0x100000), LCMAP_KATAKANA (0x200000), LCMAP_HALFWIDTH
/// (0x400000), LCMAP_FULLWIDTH (0x800000), as Windows Server 2025 maps (winref nls): only
/// full-width kana change kind (ｶ stays under LCMAP_HIRAGANA); widening comes before the kana
/// kind, narrowing after it (ｶﾞ -> が for HIRAGANA | FULLWIDTH, が -> ｶﾞ for KATAKANA |
/// HALFWIDTH).
pub fn map_kana_width(w: &[u16], flags: u32) -> Vec<u16> {
    let kana = |w: Vec<u16>| -> Vec<u16> {
        w.into_iter()
            .map(|c| match c {
                0x30a1..=0x30f6 | 0x30fd | 0x30fe if flags & 0x100000 != 0 => c - 0x60,
                0x3041..=0x3096 | 0x309d | 0x309e if flags & 0x200000 != 0 => c + 0x60,
                _ => c,
            })
            .collect()
    };
    let mut v = w.to_vec();
    if flags & 0x800000 != 0 {
        v = to_full(&v);
    }
    v = kana(v);
    if flags & 0x400000 != 0 {
        v = to_half(&v);
    }
    v
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn nls_golden() -> Vec<String> {
        ["32", "64"]
            .iter()
            .filter_map(|b| {
                std::fs::read_to_string(format!(
                    "{}/../../tests/winref/golden/nls-{b}/stdout.txt",
                    env!("CARGO_MANIFEST_DIR")
                ))
                .ok()
            })
            .collect()
    }
    fn hex(s: &str) -> Vec<u32> {
        s.split_whitespace()
            .map(|x| u32::from_str_radix(x, 16).unwrap())
            .collect()
    }

    /// Windows' MultiByteToWideChar / WideCharToMultiByte / LCMapStringW answers in the nls
    /// golden, replayed: every line, both flags (MB_ERR_INVALID_CHARS is `bad`)
    #[test]
    fn converts_as_windows() {
        let g = nls_golden();
        assert!(!g.is_empty(), "nls golden missing");
        for out in g {
            for l in out.lines() {
                if let Some(r) = l.strip_prefix("m2w ") {
                    // m2w <cp> <flags> [ <bytes> ] -> <n> need <n> err <e>: <utf-16>
                    let (head, r) = r.split_once(" [").unwrap();
                    let (bytes, r) = r.split_once(" ] -> ").unwrap();
                    let (cp, fl) = head.split_once(' ').unwrap();
                    let (cp, fl) = (
                        cp.parse::<u32>().unwrap(),
                        u32::from_str_radix(fl, 16).unwrap(),
                    );
                    let b: Vec<u8> = hex(bytes).into_iter().map(|x| x as u8).collect();
                    let want = hex(r.split_once(": ").map_or("", |x| x.1));
                    let (w, bad) = mb2w_checked(cp, &b);
                    if fl & 8 != 0 && bad {
                        assert!(r.contains("err 1113"), "{l}");
                    } else {
                        assert_eq!(w.iter().map(|&x| x as u32).collect::<Vec<_>>(), want, "{l}");
                    }
                } else if let Some(r) = l.strip_prefix("w2m ").filter(|r| r.contains(" [")) {
                    // w2m <cp> <flags> [ <utf-16> ] -> <n> used <u> err <e>: <bytes>
                    let (head, r) = r.split_once(" [").unwrap();
                    let (units, r) = r.split_once(" ] -> ").unwrap();
                    let (cp, fl) = head.split_once(' ').unwrap();
                    let (cp, fl) = (
                        cp.parse::<u32>().unwrap(),
                        u32::from_str_radix(fl, 16).unwrap(),
                    );
                    let w: Vec<u16> = hex(units).into_iter().map(|x| x as u16).collect();
                    let want: Vec<u8> = hex(r.split_once(": ").map_or("", |x| x.1))
                        .into_iter()
                        .map(|x| x as u8)
                        .collect();
                    let (a, used) = w2mb_flags(cp, &w, b"?", fl & 0x400 == 0);
                    assert_eq!(a, want, "{l}");
                    if cp != 65001 {
                        assert!(r.contains(&format!("used {}", used as u8)), "{l}");
                    }
                } else if let Some(r) = l.strip_prefix("LCMapStringW ") {
                    let (fl, r) = r.split_once(" -> ").unwrap();
                    let fl = u32::from_str_radix(fl, 16).unwrap();
                    if fl & 0xf00000 == 0 {
                        continue;
                    }
                    let want = hex(r.split_once(':').unwrap().1);
                    let src = [
                        0x61u16, 0x5a, 0x3042, 0x30ac, 0x304c, 0xff76, 0xff9e, 0xff21, 0x3000,
                        0x20, 0x30fc, 0xff70, 0xe9, 0x430, 0,
                    ];
                    assert_eq!(
                        map_kana_width(&src, fl)
                            .iter()
                            .map(|&x| x as u32)
                            .collect::<Vec<_>>(),
                        want,
                        "{l}"
                    );
                }
            }
        }
    }

    /// Every BMP code point against Windows' answers (the casemap golden lists the ones any of
    /// CharUpperBuffW / CharLowerBuffW / LCMapStringW changes; the rest stay)
    #[test]
    fn case_maps_as_windows() {
        for b in ["32", "64"] {
            let p = format!(
                "{}/../../tests/winref/golden/casemap-{b}/stdout.txt",
                env!("CARGO_MANIFEST_DIR")
            );
            let out = std::fs::read_to_string(p).expect("casemap golden");
            let mut want: Vec<(u16, u16)> = (0..=0xffffu16).map(|c| (c, c)).collect();
            for f in out
                .lines()
                .map(|l| l.split_whitespace().collect::<Vec<_>>())
                .filter(|f| f.len() == 10)
            {
                let c = u16::from_str_radix(f[0], 16).unwrap() as usize;
                want[c] = (
                    u16::from_str_radix(f[2], 16).unwrap(),
                    u16::from_str_radix(f[4], 16).unwrap(),
                );
            }
            for c in 1..=0xffffu16 {
                assert_eq!((upper16(c), lower16(c)), want[c as usize], "{c:04x}");
            }
        }
    }

    /// 932 decoding against WHATWG's Shift_JIS for every byte pair: the same character where
    /// WHATWG has one, U+30FB (and invalid) where it has none, and the pair always taken
    /// together after a lead byte
    #[test]
    fn sjis_every_pair() {
        for b in 0x81..=0xfcu8 {
            if !is_lead_cp(932, b) {
                continue;
            }
            for t in 0..=0xffu8 {
                let (w, bad) = mb2w_checked(932, &[b, t]);
                assert_eq!(w.len(), 1, "{b:02x} {t:02x}");
                let pair = [b, t];
                let (cow, err) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(&pair);
                let ws: Vec<u16> = cow.encode_utf16().collect();
                if !err && ws.len() == 1 {
                    assert!(!bad && w == ws, "{b:02x} {t:02x}");
                } else {
                    assert!(bad && w == [DEFAULT_932], "{b:02x} {t:02x}");
                }
            }
        }
        for b in 0..=0xffu8 {
            let (w, bad) = mb2w_checked(932, &[b]);
            assert_eq!(w.len(), 1);
            assert_eq!(
                bad,
                matches!(b, 0x80 | 0xa0 | 0xfd..=0xff) || is_lead_cp(932, b),
                "{b:02x}"
            );
        }
    }

    /// UTF-8: the valid text is std's, and only invalid input is `bad`
    #[test]
    fn utf8_valid_is_std() {
        for s in ["", "abc", "あ亜", "😀x", "\u{10ffff}"] {
            assert_eq!(utf8_to_w(s.as_bytes()), (s.encode_utf16().collect(), false));
        }
    }

    #[test]
    fn case_tables() {
        // ASCII, Latin-1, Greek, Cyrillic, full-width, and the ones Windows keeps or maps simply
        let pairs = [
            (0x41, 0x61),
            (0xc0, 0xe0),
            (0xde, 0xfe),
            (0x178, 0xff),
            (0x391, 0x3b1),
            (0x410, 0x430),
            (0xff21, 0xff41),
            (0x100, 0x101),
        ];
        for (u, l) in pairs {
            assert_eq!(lower16(u), l, "{u:x}");
            assert_eq!(upper16(l), u, "{l:x}");
        }
        assert_eq!(upper16(0xdf), 0xdf); // ß
                                         // (Windows keeps the Turkish dotted/dotless i, µ and ſ: the casemap golden)
        assert_eq!(lower16(0x130), 0x130); // İ
        assert_eq!(upper16(0x131), 0x131); // ı
        assert_eq!(upper16(0xb5), 0xb5); // µ
        assert_eq!(upper16(0x17f), 0x17f); // ſ
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
                assert!(
                    char::from_u32(c as u32).is_some_and(char::is_alphabetic),
                    "{c:x}"
                );
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

        /// Any bytes: 932 and UTF-8 decoding never panic, flag exactly the input WHATWG / std
        /// reject (932: plus 0x80), and a lone surrogate's ED A0..BF is one U+FFFD.
        #[test]
        fn decoders_flag_invalid(b in proptest::collection::vec(any::<u8>(), 0..32)) {
            let (_, bad) = mb2w_checked(65001, &b);
            prop_assert_eq!(bad, std::str::from_utf8(&b).is_err());
            let (w, bad) = mb2w_checked(932, &b);
            let (_, err) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(&b);
            prop_assert!(bad || !err);
            // (valid text survives encoding again; NEC's row 89..92 come back as IBM's FA..FC
            // codes, as on Windows, so compare the characters)
            if !bad {
                prop_assert_eq!(mb2w(932, &w2mb(932, &w)), w);
            }
            let mut s = b.clone();
            s.extend([0xed, 0xa0, 0x80]);
            let (sw, _) = mb2w_checked(65001, &s);
            prop_assert_eq!(&sw[sw.len() - 2..], &[0xfffd, 0xfffd][..]);
        }

        /// Best fit only replaces what the code page lacks, with ASCII, and never in UTF-8
        #[test]
        fn best_fit_is_ascii(c in 0xc0u16..0x180, cp in prop::sample::select(vec![932u32, 1252, 1250, 1251])) {
            let (fit, used) = w2mb_flags(cp, &[c], b"?", true);
            let (plain, pused) = w2mb_flags(cp, &[c], b"?", false);
            if pused {
                prop_assert_eq!(used, best_fit(char::from_u32(c as u32).unwrap()).is_none());
                if !used {
                    prop_assert!(fit.len() == 1 && fit[0].is_ascii_alphabetic());
                }
            } else {
                prop_assert_eq!(fit, plain);
            }
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
