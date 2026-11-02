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
    match acp() {
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

pub fn upper16(c: u16) -> u16 {
    match c {
        0x61..=0x7a => c - 32,
        0xe0..=0xfe if c != 0xf7 => c - 32,
        0xff41..=0xff5a => c - 32,
        _ => c,
    }
}

pub fn lower16(c: u16) -> u16 {
    match c {
        0x41..=0x5a => c + 32,
        0xc0..=0xde if c != 0xd7 => c + 32,
        0xff21..=0xff3a => c + 32,
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
