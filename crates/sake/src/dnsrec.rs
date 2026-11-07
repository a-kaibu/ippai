//! dnsapi's DNS_RECORD lists: DnsExtractRecordsFromMessage (a wire-format message to records),
//! DnsRecordCopyEx / DnsRecordSetCopyEx (between character sets), DnsRecordCompare /
//! DnsRecordSetCompare, DnsRecordSetDetach. A record is read into `Rec` and written back as one
//! heap block (the record, then its strings), so DnsRecordListFree frees it whole.
//!
//! Behaviour follows Wine's dnsapi record tests (Windows' answers): the message header's counts
//! are in host byte order, a response with no answers is a bad packet, names keep their raw
//! label bytes (no escaping), compression loops and binary labels are bad packets.

use crate::mem::{self, ps, r16, r32, rp, w16, w32, wp};

pub const DNS_ERROR_BAD_PACKET: u32 = 9502;
pub const DNS_ERROR_RCODE_NOT_IMPLEMENTED: u32 = 9004;
const ERROR_INVALID_PARAMETER: u32 = 87;

pub const CHARSET_UNICODE: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    A([u8; 4]),
    Aaaa([u8; 16]),
    /// PTR, NS, CNAME, MB, MD, MF, MG, MR
    Name(String),
    /// MINFO, RP
    Two(String, String),
    /// MX, AFSDB, RT: preference, exchange
    Mx(u16, String),
    /// priority, weight, port, target
    Srv(u16, u16, u16, String),
    /// TEXT, HINFO, ISDN, X25
    Txt(Vec<String>),
    /// primary, administrator, serial, refresh, retry, expire, default TTL
    Soa(String, String, [u32; 5]),
    /// the data area as it is (KEY, NULL, OPT, other types)
    Raw(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rec {
    pub name: Option<String>,
    pub ty: u16,
    pub data_len: u16,
    pub flags: u32,
    pub ttl: u32,
    pub reserved: u32,
    pub data: Data,
}

const SECTION_ANSWER: u32 = 1;
const SECTION_ADDITIONAL: u32 = 3;

pub fn is_name_type(t: u16) -> bool {
    matches!(t, 2 | 3 | 4 | 5 | 7 | 8 | 9 | 12)
}
fn is_txt_type(t: u16) -> bool {
    matches!(t, 13 | 16 | 19 | 20)
}

// ---- wire format ----

/// A name at `pos` (labels joined with dots as they are; compression pointers followed): the
/// name and the position after it.
pub fn get_name(msg: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut out: Vec<u8> = Vec::new();
    let mut next = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos)? as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        match len & 0xc0 {
            0 => {
                let label = msg.get(pos..pos + len)?;
                if out.len() + len + 1 >= 256 {
                    return None;
                }
                if !out.is_empty() {
                    out.push(b'.');
                }
                out.extend_from_slice(label);
                pos += len;
            }
            0xc0 => {
                let lo = *msg.get(pos)? as usize;
                if next.is_none() {
                    next = Some(pos + 1);
                }
                jumps += 1;
                if jumps >= msg.len() {
                    return None;
                }
                pos = ((len & 0x3f) << 8) + lo;
            }
            _ => return None, // (binary labels: not supported, as on Windows)
        }
    }
    if out.is_empty() {
        out.push(b'.');
    }
    Some((String::from_utf8_lossy(&out).into_owned(), next.unwrap_or(pos)))
}

fn be16(m: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*m.get(p)?, *m.get(p + 1)?]))
}
fn be32(m: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_be_bytes(m.get(p..p + 4)?.try_into().ok()?))
}

fn rdata(msg: &[u8], pos: usize, len: usize, ty: u16) -> Result<Data, u32> {
    let end = pos + len;
    let bad = DNS_ERROR_BAD_PACKET;
    let rd = msg.get(pos..end).ok_or(bad)?;
    let name = |p: usize| get_name(msg, p).ok_or(bad);
    Ok(match ty {
        1 => Data::A(rd.get(..4).ok_or(bad)?.try_into().unwrap()),
        28 => Data::Aaaa(rd.get(..16).ok_or(bad)?.try_into().unwrap()),
        t if is_name_type(t) => Data::Name(name(pos)?.0),
        14 | 17 => {
            let (a, p) = name(pos)?;
            Data::Two(a, name(p)?.0)
        }
        15 | 18 | 21 => {
            let pref = be16(rd, 0).ok_or(bad)?;
            Data::Mx(pref, name(pos + 2)?.0)
        }
        33 => {
            if len < 6 {
                return Err(bad);
            }
            Data::Srv(be16(rd, 0).unwrap(), be16(rd, 2).unwrap(), be16(rd, 4).unwrap(), name(pos + 6)?.0)
        }
        t if is_txt_type(t) => {
            let mut v = Vec::new();
            let mut p = 0;
            while p < rd.len() {
                let n = rd[p] as usize;
                let s = rd.get(p + 1..p + 1 + n).ok_or(bad)?;
                v.push(String::from_utf8_lossy(s).into_owned());
                p += n + 1;
            }
            Data::Txt(v)
        }
        6 => {
            let (a, p) = name(pos)?;
            let (b, p) = name(p)?;
            if p + 20 > end {
                return Err(bad);
            }
            let mut d = [0u32; 5];
            for (i, v) in d.iter_mut().enumerate() {
                *v = be32(msg, p + 4 * i).ok_or(bad)?;
            }
            Data::Soa(a, b, d)
        }
        25 => {
            // KEY: flags, protocol, algorithm, then the key; laid out as DNS_KEY_DATA
            if len < 4 {
                return Err(bad);
            }
            let key = &rd[4..];
            let mut b = Vec::with_capacity(8 + key.len());
            b.extend_from_slice(&be16(rd, 0).unwrap().to_le_bytes());
            b.extend_from_slice(&rd[2..4]);
            b.extend_from_slice(&(key.len() as u16).to_le_bytes());
            b.extend_from_slice(&[0, 0]);
            b.extend_from_slice(key);
            Data::Raw(b)
        }
        10 => {
            // NULL: dwByteCount, Data
            let mut b = (len as u32).to_le_bytes().to_vec();
            b.extend_from_slice(rd);
            Data::Raw(b)
        }
        41 => {
            // OPT: wDataLength, wPad, Data
            let mut b = (len as u16).to_le_bytes().to_vec();
            b.extend_from_slice(&[0, 0]);
            b.extend_from_slice(rd);
            Data::Raw(b)
        }
        _ => return Err(DNS_ERROR_RCODE_NOT_IMPLEMENTED),
    })
}

fn skip_name(msg: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *msg.get(pos)? as usize;
        pos += 1;
        match len & 0xc0 {
            _ if len == 0 => return Some(pos),
            0 => pos += len,
            0xc0 => return (pos < msg.len()).then_some(pos + 1),
            _ => return None,
        }
        if pos > msg.len() {
            return None;
        }
    }
}

/// DnsExtractRecordsFromMessage: the answer and additional records of a message (its header's
/// counts in host byte order), with `charset` (1 UTF-16, 2 UTF-8) for their strings.
pub fn extract(msg: &[u8], charset: u32) -> Result<Vec<Rec>, u32> {
    if msg.len() < 13 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let le = |p: usize| u16::from_le_bytes([msg[p], msg[p + 1]]) as usize;
    let (response, qd, an, ns, ar) = (msg[2] & 0x80 != 0, le(4), le(6), le(8), le(10));
    let bad = DNS_ERROR_BAD_PACKET;
    if response && an == 0 {
        return Err(bad);
    }
    let mut pos = 12;
    for _ in 0..qd {
        pos = skip_name(msg, pos).ok_or(bad)? + 4;
        if pos > msg.len() {
            return Err(bad);
        }
    }
    let mut out = Vec::new();
    let record = |pos: &mut usize, section: u32, out: &mut Vec<Rec>| -> Result<(), u32> {
        let (name, p) = get_name(msg, *pos).ok_or(bad)?;
        if p + 10 > msg.len() {
            return Err(bad);
        }
        let ty = be16(msg, p).unwrap();
        let ttl = be32(msg, p + 4).unwrap();
        let len = be16(msg, p + 8).unwrap() as usize;
        let p = p + 10;
        if p + len > msg.len() {
            return Err(bad);
        }
        *pos = p + len;
        let data = rdata(msg, p, len, ty)?;
        let mut r = Rec { name: Some(name), ty, data_len: 0, flags: section | charset << 3, ttl, reserved: 0, data };
        r.data_len = data_len(&r, charset, ps());
        out.push(r);
        Ok(())
    };
    for _ in 0..an {
        record(&mut pos, SECTION_ANSWER, &mut out)?;
    }
    for _ in 0..ns {
        let p = skip_name(msg, pos).ok_or(bad)?;
        let len = be16(msg, p + 8).ok_or(bad)? as usize;
        pos = p + 10 + len;
        if pos > msg.len() {
            return Err(bad);
        }
    }
    for _ in 0..ar {
        record(&mut pos, SECTION_ADDITIONAL, &mut out)?;
    }
    if pos != msg.len() {
        return Err(bad);
    }
    Ok(out)
}

fn str_bytes(s: &str, charset: u32) -> usize {
    if charset == CHARSET_UNICODE {
        2 * (s.encode_utf16().count() + 1)
    } else {
        s.len() + 1
    }
}

/// The size of the data area of `d` with pointers of `k` bytes (no strings).
pub fn data_size(d: &Data, k: u32) -> u32 {
    match d {
        Data::A(_) => 4,
        Data::Aaaa(_) => 16,
        Data::Name(_) => k,
        Data::Two(..) => 2 * k,
        Data::Mx(..) => 2 * k, // pNameExchange, wPreference, Pad (rounded to the pointer size)
        Data::Srv(..) => k + 8, // pNameTarget, wPriority, wWeight, wPort, Pad
        Data::Txt(v) => k + k * v.len() as u32,
        Data::Soa(..) => 2 * k + 20,
        Data::Raw(b) => b.len() as u32,
    }
}

/// wDataLength as DnsExtractRecordsFromMessage sets it: the structure, plus the name's bytes
/// for the types whose structure ends in a name (PTR, MX, SRV).
pub fn data_len(r: &Rec, charset: u32, k: u32) -> u16 {
    let n = match &r.data {
        Data::Name(s) | Data::Mx(_, s) | Data::Srv(.., s) => str_bytes(s, charset) as u32,
        _ => 0,
    };
    (data_size(&r.data, k) + n) as u16
}

// ---- guest records ----

fn read_str(p: u32, wide: bool) -> Option<String> {
    if p == 0 {
        None
    } else if wide {
        Some(String::from_utf16_lossy(&mem::wstr(p)))
    } else {
        Some(String::from_utf8_lossy(&mem::cstr(p)).into_owned())
    }
}

/// The record at `p`, its strings read as UTF-16 when `wide`.
pub fn read(p: u32, wide: bool) -> Rec {
    let k = ps();
    let ty = r16(p + 2 * k);
    let data_len = r16(p + 2 * k + 2);
    let d = p + 2 * k + 16;
    let s = |q: u32| read_str(rp(q), wide).unwrap_or_default();
    let data = match ty {
        1 => Data::A(r32(d).to_le_bytes()),
        28 => Data::Aaaa(mem::slice(d, 16).try_into().unwrap()),
        t if is_name_type(t) => Data::Name(s(d)),
        14 | 17 => Data::Two(s(d), s(d + k)),
        15 | 18 | 21 => Data::Mx(r16(d + k), s(d)),
        33 => Data::Srv(r16(d + k), r16(d + k + 2), r16(d + k + 4), s(d)),
        t if is_txt_type(t) => {
            let n = r32(d).min(1024);
            Data::Txt((0..n).map(|i| s(d + k + k * i)).collect())
        }
        6 => Data::Soa(s(d), s(d + k), std::array::from_fn(|i| r32(d + 2 * k + 4 * i as u32))),
        _ => Data::Raw(mem::slice(d, data_len as u32).to_vec()),
    };
    Rec { name: read_str(rp(p + k), wide), ty, data_len, flags: r32(p + 2 * k + 4), ttl: r32(p + 2 * k + 8), reserved: r32(p + 2 * k + 12), data }
}

/// `r` as a new heap block (the record, then its strings in `charset`); pNext is 0.
pub fn write(r: &Rec, charset: u32) -> u32 {
    let k = ps();
    let hdr = 2 * k + 16;
    let fixed = hdr + data_size(&r.data, k).max(4);
    let strs: Vec<&str> = r
        .name
        .as_deref()
        .into_iter()
        .chain(match &r.data {
            Data::Name(s) | Data::Mx(_, s) | Data::Srv(.., s) => vec![s.as_str()],
            Data::Two(a, b) | Data::Soa(a, b, _) => vec![a.as_str(), b.as_str()],
            Data::Txt(v) => v.iter().map(|s| s.as_str()).collect(),
            _ => vec![],
        })
        .collect();
    let total = fixed.next_multiple_of(2) + strs.iter().map(|s| str_bytes(s, charset) as u32).sum::<u32>();
    let p = mem::heap_alloc(total, true);
    let mut at = p + fixed.next_multiple_of(2);
    let mut put = |s: &str| -> u32 {
        let q = at;
        if charset == CHARSET_UNICODE {
            let w: Vec<u16> = s.encode_utf16().collect();
            mem::put_wstr(q, w.len() as u32 + 1, &w);
        } else {
            mem::write(q, s.as_bytes());
            mem::w8(q + s.len() as u32, 0);
        }
        at += str_bytes(s, charset) as u32;
        q
    };
    if let Some(n) = &r.name {
        wp(p + k, put(n));
    }
    w16(p + 2 * k, r.ty);
    w16(p + 2 * k + 2, r.data_len);
    w32(p + 2 * k + 4, (r.flags & !0x18) | (charset & 3) << 3);
    w32(p + 2 * k + 8, r.ttl);
    w32(p + 2 * k + 12, r.reserved);
    let d = p + hdr;
    match &r.data {
        Data::A(a) => mem::write(d, a),
        Data::Aaaa(a) => mem::write(d, a),
        Data::Name(s) => wp(d, put(s)),
        Data::Two(a, b) => {
            wp(d, put(a));
            wp(d + k, put(b));
        }
        Data::Mx(pref, s) => {
            wp(d, put(s));
            w16(d + k, *pref);
        }
        Data::Srv(pr, w, port, s) => {
            wp(d, put(s));
            w16(d + k, *pr);
            w16(d + k + 2, *w);
            w16(d + k + 4, *port);
        }
        Data::Txt(v) => {
            w32(d, v.len() as u32);
            for (i, s) in v.iter().enumerate() {
                wp(d + k + k * i as u32, put(s));
            }
        }
        Data::Soa(a, b, n) => {
            wp(d, put(a));
            wp(d + k, put(b));
            for (i, v) in n.iter().enumerate() {
                w32(d + 2 * k + 4 * i as u32, *v);
            }
        }
        Data::Raw(b) => mem::write(d, b),
    }
    p
}

/// Records linked into a list (pNext), its first record (0 for none).
pub fn link(recs: &[u32]) -> u32 {
    for w in recs.windows(2) {
        wp(w[0], w[1]);
    }
    recs.first().copied().unwrap_or(0)
}

fn ieq(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// DnsRecordCompare on two read records: header fields (not the TTL or character set), the
/// names case-insensitively, then the data.
pub fn compare(a: &Rec, b: &Rec) -> bool {
    // Section (bits 0-1), Delete (2), Unused (5-7), Reserved (8-31): everything but CharSet
    let m = !0x18u32;
    if a.ty != b.ty || a.data_len != b.data_len || a.flags & m != b.flags & m || a.reserved != b.reserved {
        return false;
    }
    if !ieq(a.name.as_deref().unwrap_or(""), b.name.as_deref().unwrap_or("")) {
        return false;
    }
    match (&a.data, &b.data) {
        (Data::Name(x), Data::Name(y)) => ieq(x, y),
        (Data::Two(x1, x2), Data::Two(y1, y2)) => ieq(x1, y1) && ieq(x2, y2),
        (Data::Mx(p, x), Data::Mx(q, y)) => p == q && ieq(x, y),
        (Data::Srv(a1, a2, a3, x), Data::Srv(b1, b2, b3, y)) => (a1, a2, a3) == (b1, b2, b3) && ieq(x, y),
        (Data::Txt(x), Data::Txt(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| ieq(x, y)),
        (Data::Soa(x1, x2, n), Data::Soa(y1, y2, m)) => n == m && ieq(x1, y1) && ieq(x2, y2),
        (x, y) => x == y,
    }
}

/// The character set a record's strings are in: its own when it names one (UTF-8 2, ANSI 3,
/// UTF-16 1), otherwise `fallback`.
pub fn charset_of(p: u32, fallback: u32) -> u32 {
    match r32(p + 2 * ps() + 4) >> 3 & 3 {
        c @ 1..=3 => c,
        _ => fallback,
    }
}

pub fn wide(charset: u32) -> bool {
    charset == CHARSET_UNICODE || charset == 0
}

/// DnsRecordCopyEx(record, in, out)
pub fn copy(p: u32, cin: u32, cout: u32) -> u32 {
    let cin = charset_of(p, cin);
    write(&read(p, wide(cin)), cout)
}

pub fn list(mut p: u32) -> Vec<u32> {
    let mut v = Vec::new();
    while p != 0 && v.len() < 100_000 {
        v.push(p);
        p = rp(p);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    // messages from Wine's dnsapi record tests
    const WINEHQ: &[u8] = &[
        0x12, 0x34, 0x81, 0x80, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, b'w', b'i', b'n', b'e', b'h', b'q', 0x03, b'o', b'r', b'g', 0x00, 0x00, 0x01, 0x00, 0x01, 0xc0, 0x0c,
        0x00, 0x01, 0x00, 0x01, 0x04, 0x05, 0x06, 0x07, 0x00, 0x04, 0x04, 0x04, 0x51, 0x7c,
    ];

    #[test]
    fn winehq_answer() {
        let r = extract(WINEHQ, 2).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name.as_deref(), Some("winehq.org"));
        assert_eq!(r[0].ty, 1);
        assert_eq!(r[0].ttl, 0x04050607);
        assert_eq!(r[0].flags, 1 | 2 << 3);
        assert_eq!(r[0].data, Data::A([4, 4, 0x51, 0x7c]));
        assert_eq!(r[0].data_len, 4);
    }

    #[test]
    fn bad_packets() {
        let empty = [0x12, 0x34, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0];
        assert_eq!(extract(&empty[..12], 2), Err(87));
        assert_eq!(extract(&empty, 2), Err(DNS_ERROR_BAD_PACKET));
        let empty_answer = [0x12, 0x34, 0x80, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1];
        assert_eq!(extract(&empty_answer, 2), Err(DNS_ERROR_BAD_PACKET));
        let question = [0x12, 0x34, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1];
        assert_eq!(extract(&question, 2), Ok(vec![]));
        let label = [0x12, 0x34, 0x81, 0x80, 1, 0, 1, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c', 0x41, 1, 0x34, 0, 0, 1, 0, 1, 0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 4, 4, 0x51, 0x7c];
        assert_eq!(extract(&label, 2), Err(DNS_ERROR_BAD_PACKET));
        let lp = [0x12, 0x34, 0x81, 0x80, 1, 0, 1, 0, 0, 0, 0, 0, 0xc0, 0x0c, 0, 1, 0, 1, 0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 4, 4, 0x51, 0x7c];
        assert_eq!(extract(&lp, 2), Err(DNS_ERROR_BAD_PACKET));
    }

    #[test]
    fn raw_names() {
        let inv = [
            0x12, 0x34, 0x81, 0x80, 1, 0, 1, 0, 0, 0, 0, 0, 2, b'a', b'$', 2, b'b', b'\\', 2, b'c', b'.', 2, 9, b'd', 0, 0, 1, 0, 1, 0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 4, 4, 0x51, 0x7c,
        ];
        assert_eq!(extract(&inv, 2).unwrap()[0].name.as_deref(), Some("a$.b\\.c..\td"));
        let root = [0x12, 0x34, 0x81, 0x80, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 4, 4, 0x51, 0x7c];
        assert_eq!(extract(&root, 2).unwrap()[0].name.as_deref(), Some("."));
    }

    #[test]
    fn record_types() {
        let mut m = vec![0x12, 0x34, 0x81, 0x80, 0x01, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 6];
        m.extend_from_slice(b"winehq\x03org\x00\x00\x01\x00\x01");
        m.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x05, 0x00, 0x01, 4, 5, 6, 7, 0x00, 0x05, 1, b'a', 1, b'b', 0]);
        m.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x0f, 0x00, 0x01, 4, 5, 6, 7, 0x00, 0x07, 3, 4, 1, b'c', 1, b'd', 0]);
        m.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x1c, 0x00, 0x01, 4, 5, 6, 7, 0x00, 0x10]);
        m.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0]);
        m.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x19, 0x00, 0x01, 4, 5, 6, 7, 0x00, 0x06, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        m.extend_from_slice(&[1, b't', 1, b'x', 0, 0x00, 0x10, 0x00, 0x01, 4, 5, 6, 7, 0x00, 0x09, 2, b'z', b'y', 0, 4, b'X', b'Y', 0xc3, 0xa9]);
        let r = extract(&m, 2).unwrap();
        assert_eq!(r.len(), 5);
        assert_eq!(r[0].data, Data::Name("a.b".into()));
        assert_eq!(r[0].data_len as u32, ps() + 4);
        assert_eq!(r[1].data, Data::Mx(0x0304, "c.d".into()));
        assert_eq!(r[1].data_len as u32, 2 * ps() + 4);
        assert_eq!(r[2].data, Data::Aaaa([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0]));
        assert_eq!(r[3].data, Data::Raw(vec![0x22, 0x11, 0x33, 0x44, 2, 0, 0, 0, 0x55, 0x66]));
        assert_eq!(r[3].data_len, 10);
        assert_eq!(r[4].name.as_deref(), Some("t.x"));
        assert_eq!(r[4].data, Data::Txt(vec!["zy".into(), "".into(), "XY\u{e9}".into()]));
        assert_eq!(r[4].data_len as u32, ps() + 3 * ps());
    }

    #[test]
    fn srv_message() {
        let mut m = vec![0x12, 0x34, 0x81, 0x80, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
        m.extend_from_slice(b"\x05_ldap\x04_tcp\x02dc\x06_msdcs\x06winehq\x03org\x00\x00\x21\x00\x01");
        m.extend_from_slice(&[0xc0, 0x0c, 0x00, 0x21, 0x00, 0x01, 4, 5, 6, 7, 0x00, 0x15, 0, 0, 0, 0, 1, 0x85]);
        m.extend_from_slice(b"\x02dc\x06winehq\x03org\x00");
        let r = extract(&m, 2).unwrap();
        assert_eq!(r[0].name.as_deref(), Some("_ldap._tcp.dc._msdcs.winehq.org"));
        assert_eq!(r[0].data, Data::Srv(0, 0, 0x185, "dc.winehq.org".into()));
        assert_eq!(r[0].data_len as u32, ps() + 8 + 14);
        assert_eq!(extract(&m, 1).unwrap()[0].data_len as u32, ps() + 8 + 28);
    }

    #[test]
    fn compare_rules() {
        let a = Rec { name: Some("localhost".into()), ty: 1, data_len: 4, flags: 0x2019, ttl: 1200, reserved: 0, data: Data::A([255; 4]) };
        let mut b = a.clone();
        b.name = Some("LOCALHOST".into());
        b.ttl = 0;
        b.flags = 0x2009; // another character set
        assert!(compare(&a, &b));
        b.data = Data::A([0; 4]);
        assert!(!compare(&a, &b));
        let mut c = a.clone();
        c.flags = 0x201a; // another section
        assert!(!compare(&a, &c));
    }

    #[test]
    fn fuzz_never_panics() {
        // random and mutated messages: extract only ever returns, and every name it gives back
        // is at most 255 bytes
        let mut s = 0x1234_5678u32;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        for i in 0..20_000 {
            let mut m = WINEHQ.to_vec();
            let n = rnd() as usize % 8;
            for _ in 0..n {
                let at = rnd() as usize % m.len();
                m[at] = rnd() as u8;
            }
            if i % 3 == 0 {
                m.truncate(rnd() as usize % (m.len() + 1));
            }
            if let Ok(r) = extract(&m, 2) {
                assert!(r.iter().all(|r| r.name.as_ref().unwrap().len() <= 255));
            }
        }
    }
}
