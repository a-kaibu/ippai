//! crypt32's CryptBinaryToStringA/W and CryptStringToBinaryA/W: base64 (bare or between
//! "-----BEGIN ...-----" / "-----END ...-----" lines), hex (spaced, 16 bytes a line) and raw hex.
//!
//! Behaviour follows Wine's crypt32 base64 tests (Windows' answers):
//! and Windows' own answers (tests/winref/probes/cryptstr.c):
//! - encoding: base64 lines of 64 characters, each ended by CR LF (LF with NOCR, nothing with
//!   NOCRLF); hex puts a space between bytes, two after the eighth of a line, a line break after
//!   the sixteenth. A NULL buffer asks for the length with the terminating NUL, a written string
//!   reports its length without it. No data fails. Too small a buffer is ERROR_MORE_DATA
//!   (too_small).
//! - decoding: white space anywhere is skipped; a group of four characters cut short by '='
//!   must still be four long, and ends the data; an unpadded last group decodes what it has. A
//!   string that is not standard base64 is tried as URL base64 ('-' '_', no '=': the format
//!   reported is CRYPT_STRING_BASE64URI). Hex skips white space and '-' ',' around and between
//!   bytes; the bytes before a bad character are written. The header formats take any
//!   "-----BEGIN <text>-----" (the skipped count is where it starts), CRYPT_STRING_ANY /
//!   BASE64_ANY try a header first, then bare base64 (then binary). A failure reports no
//!   skipped characters and no format. Binary from a NUL-terminated string into a buffer copies
//!   the whole buffer, as Windows does.

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem;

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_INVALID_DATA: u32 = 13;

pub const BASE64HEADER: u32 = 0;
pub const BASE64: u32 = 1;
pub const BINARY: u32 = 2;
pub const BASE64REQUESTHEADER: u32 = 3;
pub const HEX: u32 = 4;
pub const BASE64_ANY: u32 = 6;
pub const ANY: u32 = 7;
pub const HEX_ANY: u32 = 8;
pub const BASE64X509CRLHEADER: u32 = 9;
pub const HEXRAW: u32 = 0xc;
pub const BASE64URI: u32 = 0xd;
pub const NOCRLF: u32 = 0x4000_0000;
pub const NOCR: u32 = 0x8000_0000;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn eol(flags: u32) -> &'static str {
    if flags & NOCRLF != 0 {
        ""
    } else if flags & NOCR != 0 {
        "\n"
    } else {
        "\r\n"
    }
}

fn base64_lines(data: &[u8], eol: &str, out: &mut String) {
    let mut line = 0;
    for chunk in data.chunks(3) {
        let v = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                B64[(v >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
        line += 4;
        if line == 64 {
            out.push_str(eol);
            line = 0;
        }
    }
    if line != 0 {
        out.push_str(eol);
    }
}

fn label(format: u32) -> Option<&'static str> {
    match format {
        BASE64HEADER => Some("CERTIFICATE"),
        BASE64REQUESTHEADER => Some("NEW CERTIFICATE REQUEST"),
        BASE64X509CRLHEADER => Some("X509 CRL"),
        _ => None,
    }
}

/// The string for `data` (without the NUL), or the error for an unknown format.
pub fn encode(data: &[u8], fmt: u32) -> Result<String, u32> {
    let (format, nl) = (fmt & 0xffff, eol(fmt));
    let mut s = String::new();
    match format {
        BASE64 => base64_lines(data, nl, &mut s),
        BASE64HEADER | BASE64REQUESTHEADER | BASE64X509CRLHEADER => {
            let l = label(format).unwrap();
            s.push_str(&format!("-----BEGIN {l}-----{nl}"));
            base64_lines(data, nl, &mut s);
            s.push_str(&format!("-----END {l}-----{nl}"));
        }
        HEXRAW => {
            for b in data {
                s.push_str(&format!("{b:02x}"));
            }
            s.push_str(nl);
        }
        HEX => {
            let breaks = fmt & NOCRLF == 0;
            for (j, b) in data.iter().enumerate() {
                s.push_str(&format!("{b:02x}"));
                if j + 1 == data.len() {
                    break;
                }
                if breaks && (j + 1) % 16 == 0 {
                    s.push_str(nl);
                } else {
                    if breaks && (j + 1) % 8 == 0 {
                        s.push(' ');
                    }
                    s.push(' ');
                }
            }
            s.push_str(nl);
        }
        _ => return Err(ERROR_INVALID_PARAMETER),
    }
    Ok(s)
}

fn is_space(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0d | 0x0a)
}

fn b64_value(c: u16) -> Option<u32> {
    B64.iter().position(|&b| b as u16 == c).map(|i| i as u32)
}

/// The URL alphabet: '-' and '_' for '+' and '/', no padding
fn b64url_value(c: u16) -> Option<u32> {
    match c {
        0x2d => Some(62),
        0x5f => Some(63),
        0x2b | 0x2f | 0x3d => None,
        _ => b64_value(c),
    }
}

/// Base64 as CRYPT_STRING_BASE64 reads it: the standard alphabet, else the URL one
/// (CRYPT_STRING_BASE64URI, 13): (bytes, the format it was)
pub fn decode_base64_any(s: &[u16]) -> Option<(Vec<u8>, u32)> {
    decode_base64(s)
        .map(|d| (d, BASE64))
        .or_else(|| decode_base64url(s).map(|d| (d, BASE64URI)))
}

/// URL base64 (white space skipped): no padding; an unfinished last group decodes what it has
pub fn decode_base64url(s: &[u16]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut group, mut n) = (0u32, 0usize);
    for &c in s.iter().filter(|&&c| !is_space(c)) {
        group = group << 6 | b64url_value(c)?;
        n += 1;
        if n == 4 {
            flush(&mut out, group, 4);
            (group, n) = (0, 0);
        }
    }
    if n != 0 {
        flush(&mut out, group << (6 * (4 - n) as u32), n);
    }
    Some(out)
}

/// Bare base64 (white space skipped), or None when it is malformed.
pub fn decode_base64(s: &[u16]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut group, mut n) = (0u32, 0usize);
    let mut pad: Option<usize> = None;
    for &c in s {
        if is_space(c) {
            continue;
        }
        if c == '=' as u16 && n == 0 && pad.is_none() {
            // padding after a whole group ends the data
            return Some(out);
        }
        if c == '=' as u16 {
            pad.get_or_insert(n);
            n += 1;
        } else if pad.is_some() {
            return None;
        } else {
            group = group << 6 | b64_value(c)?;
            n += 1;
        }
        if n == 4 {
            if let Some(p) = pad {
                flush(&mut out, group << (6 * (4 - p) as u32), p);
                return Some(out);
            }
            flush(&mut out, group, 4);
            (group, n) = (0, 0);
        }
    }
    if pad.is_some() {
        return None;
    }
    if n != 0 {
        flush(&mut out, group << (6 * (4 - n) as u32), n);
    }
    Some(out)
}

/// The bytes of a group of `n` characters (one or two characters make a byte, three make two).
fn flush(out: &mut Vec<u8>, v: u32, n: usize) {
    let bytes = match n {
        0 => 0,
        1 | 2 => 1,
        3 => 2,
        _ => 3,
    };
    for i in 0..bytes {
        out.push((v >> (16 - 8 * i)) as u8);
    }
}

fn find(s: &[u16], pat: &str, from: usize) -> Option<usize> {
    let p: Vec<u16> = pat.encode_utf16().collect();
    (from..=s.len().checked_sub(p.len())?).find(|&i| s[i..i + p.len()] == p[..])
}

/// "-----BEGIN <text>-----" body "-----END <text>-----": where the header starts, the body.
pub fn decode_header(s: &[u16]) -> Option<(u32, Vec<u8>)> {
    let begin = find(s, "-----BEGIN ", 0)?;
    let body = find(s, "-----", begin + 11)? + 5;
    let end = find(s, "-----END ", body)?;
    find(s, "-----", end + 9)?;
    Some((begin as u32, decode_base64(&s[body..end])?))
}

#[derive(Debug, PartialEq)]
pub struct Hex {
    /// the bytes read before the end or the error
    pub bytes: Vec<u8>,
    pub error: Option<u32>,
}

fn hex_digit(c: u16) -> Option<u8> {
    char::from_u32(c as u32)?.to_digit(16).map(|d| d as u8)
}

fn hex_separator(c: u16) -> bool {
    matches!(c, 0x2d | 0x2c | 0x20 | 0x09 | 0x0d | 0x0a)
}

/// CRYPT_STRING_HEX: pairs of digits, separators (white space, '-', ',') around and between
/// them, not inside one. An embedded NUL is a bad character.
pub fn decode_hex(s: &[u16]) -> Hex {
    let mut it = s.iter().map(|&c| if c == 0 { 0xffff } else { c });
    let mut c = it.next();
    while c.is_some_and(hex_separator) {
        c = it.next();
    }
    let mut bytes = Vec::new();
    while let Some(d1) = c.and_then(hex_digit) {
        let Some(d2) = it.next().and_then(hex_digit) else {
            return Hex {
                bytes,
                error: Some(ERROR_INVALID_DATA),
            };
        };
        bytes.push(d1 << 4 | d2);
        c = it.next();
        while c.is_some_and(hex_separator) {
            c = it.next();
        }
    }
    while let Some(ch) = c {
        if !hex_separator(ch) {
            return Hex {
                bytes,
                error: Some(ERROR_INVALID_DATA),
            };
        }
        c = it.next();
    }
    Hex { bytes, error: None }
}

/// Base64 and binary decoding: (data, skipped characters, the format used).
pub fn decode(s: &[u16], raw: &[u8], fmt: u32) -> Result<(Vec<u8>, u32, u32), u32> {
    let header = || decode_header(s).map(|(skip, d)| (d, skip, BASE64HEADER));
    let bare = || decode_base64_any(s).map(|(d, f)| (d, 0, f));
    let r = match fmt {
        BASE64HEADER | BASE64REQUESTHEADER | BASE64X509CRLHEADER => {
            decode_header(s).map(|(skip, d)| (d, skip, fmt))
        }
        BASE64 => bare(),
        BINARY => Some((raw.to_vec(), 0, BINARY)),
        BASE64_ANY => header().or_else(bare),
        ANY => header().or_else(bare).or(Some((raw.to_vec(), 0, BINARY))),
        _ => None,
    };
    r.ok_or(ERROR_INVALID_DATA)
}

// ---------------------------------------------------------------------------------------------
// the exports

fn binary_to_string(c: &mut Ctx, wide: bool) -> u32 {
    let (p, n, fmt, out, plen) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if p == 0 || plen == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let data = mem::slice(p, n).to_vec();
    let cap = mem::r32(plen);
    // (no data: nothing to write, for every format)
    if n == 0 {
        mem::w32(plen, 0);
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    if fmt & 0xffff == BINARY {
        if out != 0 {
            if cap < n {
                last_error(ERROR_MORE_DATA);
                return 0;
            }
            mem::write(out, &data);
        }
        mem::w32(plen, n);
        return 1;
    }
    let s = match encode(&data, fmt) {
        Ok(s) => s,
        Err(e) => {
            last_error(e);
            return 0;
        }
    };
    let len = s.len() as u32;
    if out == 0 {
        mem::w32(plen, len + 1);
        return 1;
    }
    if cap > len {
        if wide {
            for (i, u) in s.encode_utf16().chain([0]).enumerate() {
                mem::w16(out + 2 * i as u32, u);
            }
        } else {
            mem::write(out, s.as_bytes());
            mem::w8(out + len, 0);
        }
        mem::w32(plen, len);
        return 1;
    }
    let (written, need) = too_small(&data, fmt, &s, cap);
    if !wide {
        if let Some(w) = written {
            mem::write(out, &w);
        }
    }
    mem::w32(plen, need);
    last_error(if wide && cap == 0 {
        ERROR_INVALID_PARAMETER
    } else {
        ERROR_MORE_DATA
    });
    0
}

/// Too small a buffer (cap <= the length): what the A function writes there and the length it
/// reports (Windows' answers, winref cryptstr): base64 writes what fits, without a NUL, and
/// reports cap (the needed length when only the NUL is missing); hex writes whole bytes and a
/// NUL (nothing when not even one fits) and reports cap. The W functions write nothing.
pub fn too_small(data: &[u8], fmt: u32, s: &str, cap: u32) -> (Option<Vec<u8>>, u32) {
    let len = s.len() as u32;
    if cap == 0 {
        return (None, 0);
    }
    if !matches!(fmt & 0xffff, HEX | HEXRAW) {
        let w = s.as_bytes()[..cap.min(len) as usize].to_vec();
        return (Some(w), if cap == len { len + 1 } else { cap });
    }
    let nl = eol(fmt).len();
    let mut best = None;
    for k in 1..=data.len() {
        let mut t = encode(&data[..k], fmt).unwrap_or_default();
        t.truncate(t.len() - nl);
        if t.len() as u32 + 1 > cap {
            break;
        }
        t.push('\0');
        best = Some(t.into_bytes());
    }
    (best, cap)
}

fn string_to_binary(c: &mut Ctx, wide: bool) -> u32 {
    let (p, n, fmt, out, plen, pskip, pflags) =
        (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    if p == 0 || plen == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let (s, raw): (Vec<u16>, Vec<u8>) = if wide {
        let s = if n == 0 {
            mem::wstr(p)
        } else {
            (0..n).map(|i| mem::r16(p + 2 * i)).collect()
        };
        let raw = s.iter().flat_map(|u| u.to_le_bytes()).collect();
        (s, raw)
    } else {
        let raw = if n == 0 {
            mem::cstr(p)
        } else {
            mem::slice(p, n).to_vec()
        };
        (raw.iter().map(|&b| b as u16).collect(), raw)
    };
    let cap = mem::r32(plen);
    let set = |skip: u32, flags: u32| {
        if pskip != 0 {
            mem::w32(pskip, skip);
        }
        if pflags != 0 {
            mem::w32(pflags, flags);
        }
    };
    // a failure: no skipped characters, no format, and no length when it was asked for
    let failed = |e: u32| {
        set(0, 0);
        if out == 0 {
            mem::w32(plen, 0);
        }
        last_error(e);
        0
    };
    if matches!(fmt, HEX | HEXRAW | HEX_ANY) {
        if wide && s.is_empty() {
            last_error(ERROR_INVALID_PARAMETER);
            return 0;
        }
        set(0, 0);
        let h = decode_hex(&s);
        if out != 0 {
            let k = h.bytes.len().min(cap as usize);
            mem::write(out, &h.bytes[..k]);
        }
        if let Some(e) = h.error {
            if out == 0 {
                mem::w32(plen, 0);
            }
            last_error(e);
            return 0;
        }
        if out != 0 && h.bytes.len() > cap as usize {
            last_error(ERROR_MORE_DATA);
            return 0;
        }
        set(0, if fmt == HEXRAW { HEXRAW } else { HEX });
        mem::w32(plen, h.bytes.len() as u32);
        return 1;
    }
    match decode(&s, &raw, fmt) {
        // binary with a NUL-terminated string and a buffer: Windows copies the whole buffer
        Ok((_, skip, BINARY)) if n == 0 && out != 0 => {
            mem::copy(out, p, cap);
            mem::w32(plen, cap);
            set(skip, BINARY);
            1
        }
        Ok((data, skip, used)) => {
            if out != 0 {
                if (cap as usize) < data.len() {
                    return failed(ERROR_MORE_DATA);
                }
                mem::write(out, &data);
            }
            mem::w32(plen, data.len() as u32);
            set(skip, used);
            1
        }
        Err(e) => failed(e),
    }
}

pub fn init() {
    implement("crypt32", "CryptBinaryToStringA", |c| {
        binary_to_string(c, false)
    });
    implement("crypt32", "CryptBinaryToStringW", |c| {
        binary_to_string(c, true)
    });
    implement("crypt32", "CryptStringToBinaryA", |c| {
        string_to_binary(c, false)
    });
    implement("crypt32", "CryptStringToBinaryW", |c| {
        string_to_binary(c, true)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn base64_lines_and_padding() {
        assert_eq!(encode(&[0], BASE64).unwrap(), "AA==\r\n");
        assert_eq!(encode(&[1, 2], BASE64 | NOCR).unwrap(), "AQI=\n");
        assert_eq!(encode(&[1, 2], BASE64 | NOCRLF).unwrap(), "AQI=");
        let s = encode(&[b'a'; 49], BASE64).unwrap();
        let lines: Vec<&str> = s.split("\r\n").collect();
        assert_eq!(lines[0].len(), 64);
        assert_eq!(lines[1], "YQ==");
        // exactly 48 bytes: one full line, no empty one after it
        assert_eq!(encode(&[0; 48], BASE64).unwrap().matches("\r\n").count(), 1);
        assert_eq!(
            encode(&[0], BASE64X509CRLHEADER | NOCR).unwrap(),
            "-----BEGIN X509 CRL-----\nAA==\n-----END X509 CRL-----\n"
        );
    }

    /// Windows' lengths for spaced hex (the formula Wine's tests check against).
    fn hex_len(n: usize, flags: u32) -> usize {
        let mut l = n * 3;
        if flags & NOCR != 0 {
            l += (n + 7) / 16 + 1;
        } else if flags & NOCRLF == 0 {
            l += (n + 7) / 16 + n / 16 + 1;
            if n % 16 != 0 {
                l += 1;
            }
        }
        l
    }

    #[test]
    fn hex_layout() {
        let d: Vec<u8> = (0..17).collect();
        assert_eq!(
            encode(&d, HEX).unwrap(),
            "00 01 02 03 04 05 06 07  08 09 0a 0b 0c 0d 0e 0f\r\n10\r\n"
        );
        assert_eq!(encode(&d[..3], HEX | NOCRLF).unwrap(), "00 01 02");
        assert_eq!(encode(&[0xab, 0xcd], HEXRAW | NOCR).unwrap(), "abcd\n");
        for n in [3, 4, 7, 8, 12, 15, 16, 17, 256] {
            for f in [0, NOCR, NOCRLF] {
                assert_eq!(
                    encode(&vec![0; n], HEX | f).unwrap().len() + 1,
                    hex_len(n, f),
                    "n {n} flags {f:#x}"
                );
            }
        }
    }

    #[test]
    fn lenient_base64() {
        let d = |s: &str| decode_base64(&w(s));
        for bad in ["V=", "VV=", "V=="] {
            assert_eq!(d(bad), None, "{bad}");
        }
        assert_eq!(d("V").unwrap(), b"T");
        assert_eq!(d("VV").unwrap(), b"U");
        assert_eq!(d("VVV").unwrap(), b"UU");
        assert_eq!(d("V===").unwrap(), b"T");
        assert_eq!(d("V========").unwrap(), b"T");
        assert_eq!(d("VV    VV").unwrap(), b"UUU");
        assert_eq!(d("VVV\nV").unwrap(), b"UUU");
        assert_eq!(d("V===VVVV").unwrap(), b"T");
        assert_eq!(d("VVV=VVVV").unwrap(), b"UU");
        assert_eq!(d("VVVV=VVVV").unwrap(), b"UUU");
        assert_eq!(d("VV-V"), None);
        // not standard base64: the URL alphabet, without padding
        let a = |s: &str| decode_base64_any(&w(s));
        assert_eq!(a("VV-V"), Some((vec![0x55, 0x5f, 0x95], BASE64URI)));
        assert_eq!(a("V_"), Some((vec![0x57], BASE64URI)));
        assert_eq!(a("VVVV").map(|x| x.1), Some(BASE64));
        assert_eq!(
            a("-----BEGIN-----VVVV-----END-----").map(|x| x.0.len()),
            Some(24)
        );
        assert_eq!(a("-----BEGIN X509 CRL-----\r\nAA==\r\n"), None);
        assert_eq!(
            a("junk-----BEGIN foo-----V-----END foo-----").map(|x| (x.0.len(), x.1)),
            Some((29, BASE64URI))
        );
    }

    /// Windows' answers for 3 bytes and small buffers (winref cryptstr, A functions)
    #[test]
    fn small_buffers() {
        let d = [1u8, 8, 15];
        let run = |fmt: u32, cap: u32| {
            let s = encode(&d, fmt).unwrap();
            too_small(&d, fmt, &s, cap)
        };
        assert_eq!(run(BASE64, 0), (None, 0));
        assert_eq!(run(BASE64, 1), (Some(b"A".to_vec()), 1));
        assert_eq!(run(BASE64, 5), (Some(b"AQgP\r".to_vec()), 5));
        assert_eq!(run(BASE64, 6), (Some(b"AQgP\r\n".to_vec()), 7));
        for (cap, want) in [
            (1, None),
            (2, None),
            (3, Some(&b"01\0"[..])),
            (5, Some(b"01\0")),
            (6, Some(b"01 08\0")),
            (7, Some(b"01 08\0")),
        ] {
            assert_eq!(run(HEX, cap), (want.map(|w| w.to_vec()), cap), "hex {cap}");
        }
        for (cap, want) in [
            (2, None),
            (3, Some(&b"01\0"[..])),
            (4, Some(b"01\0")),
            (5, Some(b"0108\0")),
            (7, Some(b"01080f\0")),
        ] {
            assert_eq!(
                run(HEXRAW, cap),
                (want.map(|w| w.to_vec()), cap),
                "hexraw {cap}"
            );
        }
    }

    #[test]
    fn headers() {
        let h = |s: &str| decode_header(&w(s)).map(|(k, d)| (k, String::from_utf8(d).unwrap()));
        assert_eq!(h("-----BEGIN-----VVVV-----END-----"), None);
        assert_eq!(h("-----BEGIN-----VVVV-----END -----"), None);
        assert_eq!(h("-----BEGIN -----VVVV-----END-----"), None);
        assert_eq!(
            h("-----BEGIN -----VVVV-----END -----"),
            Some((0, "UUU".into()))
        );
        assert_eq!(
            h("junk-----BEGIN foo-----V-----END foo-----"),
            Some((4, "T".into()))
        );
        assert_eq!(h("-----BEGIN X509 CRL-----\r\nAA==\r\n"), None);
        // the any formats: a header first, then bare base64
        let s =
            w("garbage\r\n-----BEGIN CERTIFICATE-----\r\nAA==\r\n-----END CERTIFICATE-----\r\n");
        assert_eq!(decode(&s, &[], ANY), Ok((vec![0], 9, BASE64HEADER)));
        assert_eq!(
            decode(&w("AA=="), &[], BASE64_ANY),
            Ok((vec![0], 0, BASE64))
        );
        assert_eq!(
            decode(&w("garbage\r\nAA==\r\n"), &[], BASE64),
            Err(ERROR_INVALID_DATA)
        );
    }

    #[test]
    fn hex_decoding() {
        let h = |s: &str| decode_hex(&w(s));
        assert_eq!(
            h(""),
            Hex {
                bytes: vec![],
                error: None
            }
        );
        assert_eq!(
            h(",-"),
            Hex {
                bytes: vec![],
                error: None
            }
        );
        assert_eq!(h("0").error, Some(ERROR_INVALID_DATA));
        assert_eq!(
            h("000"),
            Hex {
                bytes: vec![0],
                error: Some(ERROR_INVALID_DATA)
            }
        );
        assert_eq!(
            h(" aE\t\n\r\n"),
            Hex {
                bytes: vec![0xae],
                error: None
            }
        );
        assert_eq!(
            h("aa,,-BB---ff,-,"),
            Hex {
                bytes: vec![0xaa, 0xbb, 0xff],
                error: None
            }
        );
        // a separator only goes between bytes, not inside one
        assert_eq!(h("1-2").error, Some(ERROR_INVALID_DATA));
        assert_eq!(
            h("aa,BBff,-,"),
            Hex {
                bytes: vec![0xaa, 0xbb, 0xff],
                error: None
            }
        );
        assert_eq!(
            h("0102q"),
            Hex {
                bytes: vec![1, 2],
                error: Some(ERROR_INVALID_DATA)
            }
        );
        assert_eq!(
            h("01 02"),
            Hex {
                bytes: vec![1, 2],
                error: None
            }
        );
        assert_eq!(h("00 01 02 03 04 05 06 07  08 09\r\n").bytes.len(), 10);
        assert_eq!(
            decode_hex(&[0x30, 0x31, 0, 0x20, 0x30, 0x32]),
            Hex {
                bytes: vec![1],
                error: Some(ERROR_INVALID_DATA)
            }
        );
    }

    proptest! {
        #[test]
        fn base64_round_trips(data in proptest::collection::vec(any::<u8>(), 0..300), f in prop_oneof![Just(0), Just(NOCR), Just(NOCRLF)],
                              fmt in prop_oneof![Just(BASE64), Just(BASE64HEADER), Just(BASE64REQUESTHEADER), Just(BASE64X509CRLHEADER)]) {
            let s = w(&encode(&data, fmt | f).unwrap());
            let (back, skip, used) = decode(&s, &[], fmt).unwrap();
            prop_assert_eq!(back, data.clone());
            prop_assert_eq!((skip, used), (0, fmt));
            if fmt != BASE64 {
                prop_assert_eq!(decode(&s, &[], ANY).unwrap().0, data);
            }
        }

        #[test]
        fn hex_round_trips(data in proptest::collection::vec(any::<u8>(), 1..300), f in prop_oneof![Just(0), Just(NOCR), Just(NOCRLF)]) {
            let s = encode(&data, HEX | f).unwrap();
            prop_assert_eq!(s.len() + 1, hex_len(data.len(), f));
            // HEXRAW is one run of digits: it decodes as hex
            let raw = encode(&data, HEXRAW | f).unwrap();
            prop_assert_eq!(raw.len(), 2 * data.len() + eol(f).len());
            prop_assert_eq!(decode_hex(&w(&raw)), Hex { bytes: data.clone(), error: None });
            // spaced hex decodes back too (Windows: "01 02" is two bytes)
            prop_assert_eq!(decode_hex(&w(&s)), Hex { bytes: data.clone(), error: None });
        }

        #[test]
        fn base64_decoder_never_panics(s in "[A-Za-z0-9+/= \r\n-]{0,40}") {
            let _ = decode_base64(&w(&s));
            let _ = decode_header(&w(&s));
            let _ = decode_hex(&w(&s));
        }
    }
}
