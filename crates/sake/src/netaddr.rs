#![allow(non_snake_case)]
//! IP address text, both ways: ntdll's RtlIpv4/RtlIpv6 StringToAddress and AddressToString
//! (which ws2_32's inet_addr, inet_pton, inet_ntop, WSAStringToAddress and
//! WSAAddressToString build on). Windows' parsers have many quirks (ntdll's tests and
//! ws2_32's pin them down): a failed parse leaves what it got so far in the output, the last
//! IPv6 group may be a "0x" number of any length, and so on.

use crate::api::{implement, Ctx};
use crate::mem::{self, r32, w16, w32};

const STATUS_SUCCESS: u32 = 0;
const STATUS_INVALID_PARAMETER: u32 = 0xC000000D;

/// The character at `i` (0 past the end, like the C string's terminator)
fn at(s: &[u16], i: usize) -> u16 {
    s.get(i).copied().unwrap_or(0)
}

fn digit(c: u16) -> Option<u32> {
    char::from_u32(c as u32).and_then(|c| c.to_digit(16))
}

/// One IPv4 field: decimal, or (unless strict) octal "0..." or hex "0x..."; `i` moves past
/// what was read (past a "0x" or leading "0" even when it then fails)
fn v4_field(s: &[u16], i: &mut usize, strict: bool) -> Option<u32> {
    if at(s, *i) == b'.' as u16 {
        *i += 1;
        return None;
    }
    let mut base = 10;
    if at(s, *i) == b'0' as u16 {
        let n = at(s, *i + 1);
        if n == b'x' as u16 || n == b'X' as u16 {
            *i += 2;
            if strict {
                return None;
            }
            base = 16;
        } else if (b'0' as u16..=b'9' as u16).contains(&n) {
            *i += 1;
            if strict {
                return None;
            }
            base = 8;
        }
    }
    let mut v: u32 = 0;
    let mut any = false;
    while let Some(d) = digit(at(s, *i)).filter(|&d| d < base) {
        // (Windows' overflow check: only a wrapped value smaller than the last one fails, so
        // "0x123456789" reads as 0x23456789)
        let next = v.wrapping_mul(base).wrapping_add(d);
        if next < v {
            return None;
        }
        v = next;
        any = true;
        *i += 1;
    }
    any.then_some(v)
}

/// The result of a parse: the status, where it stopped, and the port (network order)
pub struct V4 {
    pub ok: bool,
    pub addr: Option<[u8; 4]>,
    pub end: usize,
    pub port: u16,
}

/// RtlIpv4StringToAddress(Ex): `want_end` for the plain form (the terminator is reported and
/// trailing text allowed); `want_port` for Ex (":port" allowed)
pub fn v4_parse(s: &[u16], strict: bool, want_end: bool, want_port: bool) -> V4 {
    let mut i = 0;
    let mut f = [0u32; 4];
    let mut n = 0;
    let fail = |i| V4 { ok: false, addr: None, end: i, port: 0 };
    loop {
        match v4_field(s, &mut i, strict) {
            Some(v) => f[n] = v,
            None => return fail(i),
        }
        n += 1;
        if at(s, i) != b'.' as u16 {
            break;
        }
        if n == 4 {
            return fail(i);
        }
        i += 1;
    }
    if strict && n < 4 {
        return fail(i);
    }
    let a = match n {
        4 if f.iter().all(|&v| v <= 0xff) => [f[0] as u8, f[1] as u8, f[2] as u8, f[3] as u8],
        3 if f[0] <= 0xff && f[1] <= 0xff && f[2] <= 0xffff => [f[0] as u8, f[1] as u8, (f[2] >> 8) as u8, f[2] as u8],
        2 if f[0] <= 0xff && f[1] <= 0xff_ffff => [f[0] as u8, (f[1] >> 16) as u8, (f[1] >> 8) as u8, f[1] as u8],
        1 => f[0].to_be_bytes(),
        _ => return fail(i),
    };
    let mut end = i;
    let mut port = 0;
    if at(s, i) == b':' as u16 {
        i += 1;
        match v4_field(s, &mut i, false) {
            Some(p) if p != 0 && p <= 0xffff && at(s, i) == 0 => {
                if want_port {
                    port = (p as u16).to_be();
                    if want_end {
                        end = i;
                    }
                }
            }
            _ => return V4 { ok: false, addr: Some(a), end: i, port: 0 },
        }
    }
    if !want_end && at(s, i) != 0 {
        return V4 { ok: false, addr: Some(a), end, port };
    }
    V4 { ok: true, addr: Some(a), end, port }
}

/// An IPv6 component: like wcstoul (with the value capped at 0x7fffffff), but it must start
/// with a hex digit, and a "0x" with nothing after counts as just the "0"
fn v6_field(s: &[u16], i: &mut usize, base: u32) -> Option<u32> {
    digit(at(s, *i))?;
    let mut j = *i;
    if base == 16 && at(s, j) == b'0' as u16 && (at(s, j + 1) | 0x20) == b'x' as u16 && digit(at(s, j + 2)).is_some() {
        j += 2;
    }
    let start = j;
    let mut v: u64 = 0;
    while let Some(d) = digit(at(s, j)).filter(|&d| d < base) {
        v = (v * base as u64 + d as u64).min(u32::MAX as u64 + 1);
        j += 1;
    }
    if j == start {
        // no digits: "0x" with nothing valid after reads as "0" (else nothing was read)
        if at(s, *i) == b'0' as u16 {
            *i += 1;
            return Some(0);
        }
        return None;
    }
    *i = j;
    Some(v.min(u32::MAX as u64).min(0x7fff_ffff) as u32)
}

pub struct V6 {
    pub ok: bool,
    pub end: usize,
    pub scope: u32,
    pub port: u16,
}

/// RtlIpv6StringToAddress(Ex) into `a` (which keeps what was parsed on failure)
pub fn v6_parse(s: &[u16], a: &mut [u8; 16], ex: bool, want_end: bool) -> V6 {
    let mut i = 0;
    let mut want_port = false;
    let (mut nb, mut n4, mut gap): (usize, usize, Option<usize>) = (0, 0, None);
    let (mut has_0x, mut too_big) = (false, false);
    let fail = |i| V6 { ok: false, end: i, scope: 0, port: 0 };
    let c = |s: &[u16], i: usize, ch: u8| at(s, i) == ch as u16;
    if c(s, 0, b'[') {
        if !ex {
            return fail(0);
        }
        want_port = true;
        i += 1;
    }
    if c(s, i, b':') {
        if !c(s, i + 1, b':') {
            return fail(i);
        }
        i += 1;
        a[0] = 0;
        a[1] = 0;
    }
    let (mut filled, mut term0x) = (false, 0);
    loop {
        let mut prev = i;
        if n4 == 0 && c(s, i, b':') {
            // "::"
            if gap.is_some() {
                return fail(i);
            }
            i += 1;
            prev = i;
            gap = Some(nb);
            let mut t = i;
            if nb == 14 || v6_field(s, &mut t, 16).is_none() {
                break;
            }
        }
        if n4 == 0 && nb <= if gap.is_some() { 10 } else { 12 } {
            let mut t = i;
            if v6_field(s, &mut t, 10).is_some() && c(s, t, b'.') {
                n4 = 1;
            }
        }
        if n4 != 0 {
            let Some(v) = v6_field(s, &mut i, 10) else { return fail(i) };
            if i - prev > 3 || v > 255 {
                too_big = true;
            } else {
                if !c(s, i, b'.') && (n4 < 4 || (nb < 15 && gap.is_none())) {
                    return fail(i);
                }
                a[nb] = v as u8;
                nb += 1;
            }
            if n4 == 4 || !c(s, i, b'.') {
                break;
            }
            n4 += 1;
        } else {
            let Some(v) = v6_field(s, &mut i, 16) else { return fail(i) };
            if c(s, prev, b'0') && (at(s, prev + 1) | 0x20) == b'x' as u16 {
                // the last group may be "0x" and any length; the "x" counts as the terminator
                if nb < 14 && gap.is_none() {
                    return V6 { ok: false, end: prev + 1, scope: 0, port: 0 };
                }
                a[nb..nb + 2].copy_from_slice(&(v as u16).to_be_bytes());
                nb += 2;
                has_0x = true;
                term0x = prev + 1;
                filled = true;
                break;
            }
            if !c(s, i, b':') && nb < 14 && gap.is_none() {
                return fail(i);
            }
            if i - prev > 4 {
                too_big = true;
            } else {
                a[nb..nb + 2].copy_from_slice(&(v as u16).to_be_bytes());
            }
            nb += 2;
            if !c(s, i, b':') || (gap.is_some() && c(s, i + 1, b':')) {
                break;
            }
        }
        if nb == if gap.is_some() { 14 } else { 16 } {
            break;
        }
        if too_big {
            return fail(i);
        }
        i += 1;
    }
    let end = if filled { term0x } else { i };
    if !filled && too_big {
        return fail(i);
    }
    match gap {
        None if nb < 16 => return fail(i),
        None => {}
        Some(g) => {
            let n = nb - g;
            a.copy_within(g..nb, 16 - n);
            a[g..16 - n].fill(0);
        }
    }
    let (mut scope, mut port) = (0, 0);
    if ex {
        if has_0x {
            return fail(end);
        }
        if c(s, i, b'%') {
            i += 1;
            match v4_field(s, &mut i, true) {
                Some(v) => scope = v,
                None => return fail(i),
            }
        }
        if want_port {
            if !c(s, i, b']') {
                return fail(i);
            }
            i += 1;
            if c(s, i, b':') {
                i += 1;
                match v4_field(s, &mut i, false) {
                    Some(p) if p != 0 && p <= 0xffff && at(s, i) == 0 => port = (p as u16).to_be(),
                    _ => return fail(i),
                }
            }
        }
    }
    if !want_end && at(s, i) != 0 {
        return V6 { ok: false, end, scope: 0, port: 0 };
    }
    V6 { ok: true, end, scope, port }
}

/// "a.b.c.d[:port]" (port in network order, 0 for none)
pub fn v4_text(a: [u8; 4], port: u16) -> String {
    let mut s = format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]);
    if port != 0 {
        s += &format!(":{}", u16::from_be(port));
    }
    s
}

/// Whether the last 32 bits read as an IPv4 address: ISATAP (::0:5efe:a.b.c.d, ::200:5efe:...),
/// IPv4-compatible (::a.b.c.d, not ::0.0.x.x) and IPv4-mapped (::ffff:a.b.c.d)
fn v4_inside(w: &[u16; 8]) -> bool {
    if w[5] == 0x5efe && w[4] & !0x0200 == 0 {
        return true;
    }
    if w[..4].iter().any(|&x| x != 0) {
        return false;
    }
    match (w[4], w[5]) {
        (0, 0) | (0, 0xffff) | (0xffff, 0) => w[6] != 0,
        _ => false,
    }
}

/// RFC 5952-style text the way Windows writes it: the longest run of two or more zero groups
/// (the first of equal ones) becomes "::"; "[...]%scope:port" when a port is given
pub fn v6_text(a: &[u8; 16], scope: u32, port: u16) -> String {
    let w: [u16; 8] = std::array::from_fn(|i| u16::from_be_bytes([a[2 * i], a[2 * i + 1]]));
    let end = if v4_inside(&w) { 6 } else { 8 };
    let (mut gap, mut gap_len) = (usize::MAX, 1);
    let mut i = 0;
    while i < end {
        let st = i;
        while i < end && w[i] == 0 {
            i += 1;
        }
        if i - st > gap_len {
            gap = st;
            gap_len = i - st;
        }
        i += 1;
    }
    let mut s = String::new();
    if port != 0 {
        s.push('[');
    }
    let mut i = 0;
    while i < end {
        if i == gap {
            s.push(':');
            i += gap_len;
            if i == end {
                s.push(':');
            }
            continue;
        }
        if i > 0 {
            s.push(':');
        }
        s += &format!("{:x}", w[i]);
        i += 1;
    }
    if end == 6 {
        if !s.ends_with(':') {
            s.push(':');
        }
        s += &v4_text([a[12], a[13], a[14], a[15]], 0);
    }
    if scope != 0 {
        s += &format!("%{scope}");
    }
    if port != 0 {
        s += &format!("]:{}", u16::from_be(port));
    }
    s
}

fn wide(p: u32) -> Vec<u16> {
    mem::wstr(p)
}
fn narrow(p: u32) -> Vec<u16> {
    mem::cstr(p).iter().map(|&b| b as u16).collect()
}

fn v4_s2a(c: &mut Ctx, w: bool, ex: bool) -> u32 {
    // plain: (str, strict, &terminator, &addr); Ex: (str, strict, &addr, &port)
    let (p, strict) = (c.a(0), c.a(1) & 0xff != 0);
    if p == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let s = if w { wide(p) } else { narrow(p) };
    let r = v4_parse(&s, strict, !ex, ex);
    let (term, out, portp) = if ex { (0, c.a(2), c.a(3)) } else { (c.a(2), c.a(3), 0) };
    if let Some(a) = r.addr {
        if out != 0 {
            mem::write(out, &a);
        }
    }
    if term != 0 {
        mem::wp(term, p + r.end as u32 * if w { 2 } else { 1 });
    }
    if r.ok && portp != 0 {
        w16(portp, r.port);
    }
    if r.ok { STATUS_SUCCESS } else { STATUS_INVALID_PARAMETER }
}

fn v6_s2a(c: &mut Ctx, w: bool, ex: bool) -> u32 {
    // plain: (str, &terminator, &addr); Ex: (str, &addr, &scope, &port)
    let p = c.a(0);
    let (term, out, scopep, portp) = if ex { (0, c.a(1), c.a(2), c.a(3)) } else { (c.a(1), c.a(2), 0, 0) };
    if p == 0 || out == 0 || (ex && (scopep == 0 || portp == 0)) {
        return STATUS_INVALID_PARAMETER;
    }
    let s = if w { wide(p) } else { narrow(p) };
    let mut a: [u8; 16] = mem::slice(out, 16).try_into().unwrap();
    let r = v6_parse(&s, &mut a, ex, !ex);
    mem::write(out, &a);
    if term != 0 {
        mem::wp(term, p + r.end as u32 * if w { 2 } else { 1 });
    }
    if r.ok && ex {
        w32(scopep, r.scope);
        w16(portp, r.port);
    }
    if r.ok { STATUS_SUCCESS } else { STATUS_INVALID_PARAMETER }
}

/// Writes `s` (with its NUL) if `*size` holds it; `*size` becomes the length with the NUL.
/// `strict_gt`: IPv4's form needs the size to be greater than the length
fn put_text(s: &str, out: u32, sizep: u32, w: bool, strict_gt: bool) -> u32 {
    let need = s.len() as u32 + 1;
    let have = r32(sizep);
    w32(sizep, need);
    let fits = if strict_gt { have > need - 1 } else { have >= need };
    if !fits {
        return STATUS_INVALID_PARAMETER;
    }
    put_raw(s, out, w);
    STATUS_SUCCESS
}

/// `s` and its NUL, as bytes or UTF-16
pub fn put_raw(s: &str, out: u32, w: bool) {
    if w {
        for (i, b) in s.bytes().chain([0]).enumerate() {
            w16(out + 2 * i as u32, b as u16);
        }
    } else {
        mem::write(out, s.as_bytes());
        mem::w8(out + s.len() as u32, 0);
    }
}

fn v4_a2s_ex(c: &mut Ctx, w: bool) -> u32 {
    let (a, port, out, sizep) = (c.a(0), c.a(1) as u16, c.a(2), c.a(3));
    if a == 0 || out == 0 || sizep == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let s = v4_text(mem::slice(a, 4).try_into().unwrap(), port);
    put_text(&s, out, sizep, w, true)
}
fn v4_a2s(c: &mut Ctx, w: bool) -> u32 {
    let (a, out) = (c.a(0), c.a(1));
    let s = v4_text(mem::slice(a, 4).try_into().unwrap(), 0);
    let ch = if w { 2 } else { 1 };
    put_raw(&s, out, w);
    out + s.len() as u32 * ch
}
fn v6_a2s_ex(c: &mut Ctx, w: bool) -> u32 {
    let (a, scope, port, out, sizep) = (c.a(0), c.a(1), c.a(2) as u16, c.a(3), c.a(4));
    if a == 0 || out == 0 || sizep == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let s = v6_text(mem::slice(a, 16).try_into().unwrap(), scope, port);
    put_text(&s, out, sizep, w, false)
}
fn v6_a2s(c: &mut Ctx, w: bool) -> u32 {
    let (a, out) = (c.a(0), c.a(1));
    let ch = if w { 2 } else { 1 };
    if a == 0 || out == 0 {
        return if w { out } else { out.wrapping_sub(1) };
    }
    // the 46th character is set even though the text is always shorter
    if w {
        w16(out + 90, 0);
    } else {
        mem::w8(out + 45, 0);
    }
    let s = v6_text(mem::slice(a, 16).try_into().unwrap(), 0, 0);
    put_raw(&s, out, w);
    out + s.len() as u32 * ch
}

pub fn init() {
    let d = "ntdll";
    implement(d, "RtlIpv4StringToAddressA", |c| v4_s2a(c, false, false));
    implement(d, "RtlIpv4StringToAddressW", |c| v4_s2a(c, true, false));
    implement(d, "RtlIpv4StringToAddressExA", |c| v4_s2a(c, false, true));
    implement(d, "RtlIpv4StringToAddressExW", |c| v4_s2a(c, true, true));
    implement(d, "RtlIpv6StringToAddressA", |c| v6_s2a(c, false, false));
    implement(d, "RtlIpv6StringToAddressW", |c| v6_s2a(c, true, false));
    implement(d, "RtlIpv6StringToAddressExA", |c| v6_s2a(c, false, true));
    implement(d, "RtlIpv6StringToAddressExW", |c| v6_s2a(c, true, true));
    implement(d, "RtlIpv4AddressToStringA", |c| v4_a2s(c, false));
    implement(d, "RtlIpv4AddressToStringW", |c| v4_a2s(c, true));
    implement(d, "RtlIpv4AddressToStringExA", |c| v4_a2s_ex(c, false));
    implement(d, "RtlIpv4AddressToStringExW", |c| v4_a2s_ex(c, true));
    implement(d, "RtlIpv6AddressToStringA", |c| v6_a2s(c, false));
    implement(d, "RtlIpv6AddressToStringW", |c| v6_a2s(c, true));
    implement(d, "RtlIpv6AddressToStringExA", |c| v6_a2s_ex(c, false));
    implement(d, "RtlIpv6AddressToStringExW", |c| v6_a2s_ex(c, true));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    fn inet_addr(s: &str) -> Option<[u8; 4]> {
        let r = v4_parse(&u(s), false, true, false);
        (r.ok && matches!(at(&u(s), r.end), 0 | 0x20)).then(|| r.addr.unwrap())
    }
    #[test]
    fn v4() {
        assert_eq!(inet_addr("1.2.3.4"), Some([1, 2, 3, 4]));
        assert_eq!(inet_addr("0777"), Some([0, 0, 1, 0xff]));
        assert_eq!(inet_addr("0x2134"), Some([0, 0, 0x21, 0x34]));
        assert_eq!(inet_addr("1.2.3.0a"), None);
        assert_eq!(inet_addr("1.1.1.0x"), None);
        assert_eq!(inet_addr("127.127.127.256"), None);
        let r = v4_parse(&u("1.2.3.4:80"), true, false, true);
        assert!(r.ok && r.port == 80u16.to_be());
        let r = v4_parse(&u("0x123456789"), false, false, true);
        assert!(r.ok && r.addr == Some([0x23, 0x45, 0x67, 0x89]), "{:?}", (r.ok, r.addr));
        let r = v4_parse(&u("0x123456789"), false, true, false);
        assert!(r.ok && r.addr == Some([0x23, 0x45, 0x67, 0x89]), "{:?}", (r.ok, r.addr));
    }
    fn p6(s: &str) -> Option<[u16; 8]> {
        let mut a = [0xab; 16];
        let r = v6_parse(&u(s), &mut a, false, true);
        (r.ok && at(&u(s), r.end) == 0).then(|| std::array::from_fn(|i| u16::from_le_bytes([a[2 * i], a[2 * i + 1]])))
    }
    #[test]
    fn v6() {
        assert_eq!(p6("::1"), Some([0, 0, 0, 0, 0, 0, 0, 0x100]));
        assert_eq!(p6("1111:2222:3333:4444:5555:6666:123.123.123.123"), Some([0x1111, 0x2222, 0x3333, 0x4444, 0x5555, 0x6666, 0x7b7b, 0x7b7b]));
        assert_eq!(p6("1::001.2.3.4"), Some([0x100, 0, 0, 0, 0, 0, 0x201, 0x403]));
        assert_eq!(p6("1::0001.2.3.4"), None);
        assert_eq!(p6("1111:2222:3333:4444:5555::0x88"), None);
        assert_eq!(p6("::0:a:b:c:d:e:f"), Some([0, 0, 0xa00, 0xb00, 0xc00, 0xd00, 0xe00, 0xf00]));
        let mut a = [0; 16];
        let r = v6_parse(&u("[::1%5]:80"), &mut a, true, false);
        assert!(r.ok && r.scope == 5 && r.port == 80u16.to_be());
    }
    #[test]
    fn text() {
        let mut a = [0u8; 16];
        a[15] = 1;
        assert_eq!(v6_text(&a, 0, 0), "::1");
        a[10] = 0xff;
        a[11] = 0xff;
        a[12] = 1;
        assert_eq!(v6_text(&a, 0, 0), "::ffff:1.0.0.1");
        assert_eq!(v6_text(&[0; 16], 3, 80u16.to_be()), "[::%3]:80");
        let b = [0x20, 1, 0xd, 0xb8, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1];
        assert_eq!(v6_text(&b, 0, 0), "2001:db8::1:0:0:1");
        assert_eq!(v4_text([1, 2, 3, 4], 0x5000), "1.2.3.4:80");
    }
}
