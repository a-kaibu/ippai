//! Synchronous WinINet HTTP with TLS verified inside Wasm.
//! Response status and bytes are real server data; no licensing logic lives here.
#![allow(non_snake_case)]
use crate::api::{self, Ctx};
use crate::mem::{self, ps, r32, rp, w32};
use std::collections::HashMap;
use std::sync::{
    Mutex,
    atomic::{AtomicU32, Ordering},
};
const LIMIT: usize = 4 * 1024 * 1024;
#[derive(Clone)]
struct Session {
    agent: String,
    timeout: u32,
    callback: u32,
}
#[derive(Clone)]
struct Connection {
    session: Session,
    host: String,
    port: u16,
}
#[derive(Clone, Default)]
struct Response {
    status: u32,
    text: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    offset: usize,
}
#[derive(Clone)]
struct Request {
    connection: Connection,
    method: String,
    path: String,
    flags: u32,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    response: Option<Response>,
}
#[derive(Clone)]
enum Handle {
    Session(Session),
    Connection(Connection),
    Request(Request),
}
static HANDLES: Mutex<Option<HashMap<u32, Handle>>> = Mutex::new(None);
static NEXT: AtomicU32 = AtomicU32::new(0x4c000000);
fn add(h: Handle) -> u32 {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    HANDLES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(id, h);
    id
}
fn get(id: u32) -> Option<Handle> {
    HANDLES.lock().unwrap().as_ref()?.get(&id).cloned()
}
fn edit<T>(id: u32, f: impl FnOnce(&mut Handle) -> T) -> Option<T> {
    HANDLES.lock().unwrap().as_mut()?.get_mut(&id).map(f)
}
fn fail(error: u32) -> u32 {
    if crate::env_on!("SAKE_TRACE_WININET") { eprintln!("sake: WinINet error {error}"); }
    crate::k32::last_error(error);
    0
}
// WinINet browser-mode canonicalization (Microsoft InternetCanonicalizeUrl).
// Query/fragment bytes are preserved; only the path is normalized/escaped.
fn canonical_url(input: &[u8], flags: u32) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(input.len());
    let hex = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
    let mut i = 0;
    while i < input.len() {
        if flags & 0x1000_0000 != 0 && input[i] == b'%' && i + 2 < input.len() {
            if let (Some(a), Some(b)) = (hex(input[i + 1]), hex(input[i + 2])) {
                decoded.push(a * 16 + b); i += 3; continue;
            }
        }
        decoded.push(input[i]); i += 1;
    }
    let tail = decoded.iter().position(|b| matches!(b, b'?' | b'#')).unwrap_or(decoded.len());
    let (path, suffix) = decoded.split_at(tail);
    let mut path = path.to_vec();
    if path.starts_with(b"http:") || path.starts_with(b"https:") {
        for b in &mut path { if *b == b'\\' { *b = b'/'; } }
    }
    if flags & 0x0800_0000 == 0 {
        let start = path.windows(3).position(|s| s == b"://")
            .and_then(|n| path[n + 3..].iter().position(|b| *b == b'/').map(|p| n + 3 + p))
            .unwrap_or(0);
        let prefix = path[..start].to_vec();
        let mut segments: Vec<&[u8]> = Vec::new();
        let trailing = path.ends_with(b"/.") || path.ends_with(b"/..");
        for part in path[start..].split(|b| *b == b'/') {
            match part {
                b"." => (),
                b".." => { if segments.last().is_some_and(|s| !s.is_empty()) { segments.pop(); } },
                _ => segments.push(part),
            }
        }
        let mut normal = prefix;
        for (n, part) in segments.iter().enumerate() { if n != 0 { normal.push(b'/'); } normal.extend_from_slice(part); }
        if trailing && !normal.ends_with(b"/") { normal.push(b'/'); }
        path = normal;
    }
    let mut out = Vec::new();
    for b in path {
        let escape = flags & 0x2000_0000 == 0 && if flags & 0x0400_0000 != 0 {
            b == b' '
        } else {
            b <= 0x20 || b >= 0x7f || b"<>\"{}|^`".contains(&b) || b == b'%' && flags & 0x1000 != 0
        };
        if escape { out.extend_from_slice(format!("%{b:02X}").as_bytes()); } else { out.push(b); }
    }
    out.extend_from_slice(suffix);
    out
}
fn canonicalize(c: &mut Ctx, wide: bool) -> u32 {
    let (src, dst, len, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if src == 0 || len == 0 || flags & !0xfe00_1000 != 0 { return fail(87); }
    let input = if wide { string(src, true).into_bytes() } else { mem::cstr(src) };
    if input.len() > 32768 { return fail(12005); }
    let result = canonical_url(&input, flags);
    if wide {
        let Ok(text) = String::from_utf8(result) else { return fail(12005) };
        let words: Vec<u16> = text.encode_utf16().collect();
        if dst == 0 || r32(len) as usize <= words.len() { w32(len, words.len() as u32 + 1); return fail(122); }
        mem::put_wstr(dst, words.len() as u32 + 1, &words); w32(len, words.len() as u32);
    } else {
        if dst == 0 || r32(len) as usize <= result.len() { w32(len, result.len() as u32 + 1); return fail(122); }
        mem::put_cstr(dst, result.len() as u32 + 1, &result); w32(len, result.len() as u32);
    }
    1
}
fn InternetCanonicalizeUrlA(c: &mut Ctx) -> u32 { canonicalize(c, false) }
fn InternetCanonicalizeUrlW(c: &mut Ctx) -> u32 { canonicalize(c, true) }
fn crack_url(c: &mut Ctx, wide: bool) -> u32 {
    let (src, n, flags, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if src == 0 || out == 0 || n > 32768 || flags & !0x9000_0000 != 0 { return fail(87); }
    let x64 = mem::x64();
    if r32(out) != if x64 { 104 } else { 60 } { return fail(87); }
    let units: Vec<u16> = if wide {
        if n == 0 { mem::wstr(src) } else { mem::wstrn(src, n) }
    } else {
        let b = if n == 0 { mem::cstr(src) } else { mem::cstrn(src, n) };
        b.into_iter().map(u16::from).collect()
    };
    let len = units.len();
    let Some(colon) = units.iter().position(|u| *u == b':' as u16) else { return fail(12005) };
    let scheme = String::from_utf16_lossy(&units[..colon]).to_ascii_lowercase();
    let (kind, default_port) = match scheme.as_str() {
        "http" => (3, 80), "https" => (4, 443), "ftp" => (1, 21), "file" => (5, 0), _ => (u32::MAX, 0),
    };
    let mut host = colon + 1;
    let authority = units.get(host..host + 2) == Some(&[47, 47]);
    if authority { host += 2; }
    let end = if authority { (host..len).find(|&i| matches!(units[i], 47 | 63 | 35)).unwrap_or(len) } else { host };
    let mut user = (host, host);
    let mut password = user;
    if let Some(at) = (host..end).rev().find(|&i| units[i] == 64) {
        let sep = (host..at).find(|&i| units[i] == 58).unwrap_or(at);
        user = (host, sep); password = (if sep < at { sep + 1 } else { at }, at); host = at + 1;
    }
    let mut host_end = end;
    let mut port = default_port;
    if let Some(sep) = (host..end).rev().find(|&i| units[i] == 58) {
        let bracket = units.get(host) == Some(&91);
        if !bracket || units.get(sep.wrapping_sub(1)) == Some(&93) {
            let Ok(value) = String::from_utf16_lossy(&units[sep + 1..end]).parse::<u16>() else { return fail(12005) };
            host_end = sep; port = value;
        }
    }
    let extra = (end..len).find(|&i| matches!(units[i], 63 | 35)).unwrap_or(len);
    let offsets = if x64 { [8, 24, 40, 56, 72, 88] } else { [4, 16, 28, 36, 44, 52] };
    let fields = [(0, colon), (host, host_end), user, password, (end, extra), (extra, len)];
    let mut copies = Vec::new();
    for (offset, (start, end)) in offsets.into_iter().zip(fields) {
        let (ptr, capacity) = (rp(out + offset), r32(out + offset + ps()));
        if capacity == 0 { continue; }
        let mut value = units[start..end].to_vec();
        if flags != 0 {
            if ptr == 0 { return fail(87); }
            let mut decoded = Vec::new(); let mut i = 0;
            while i < value.len() {
                if value[i] == 37 && i + 2 < value.len() {
                    if let (Some(a), Some(b)) = (char::from_u32(value[i + 1] as u32).and_then(|c| c.to_digit(16)), char::from_u32(value[i + 2] as u32).and_then(|c| c.to_digit(16))) {
                        decoded.push((a * 16 + b) as u16); i += 3; continue;
                    }
                }
                decoded.push(value[i]); i += 1;
            }
            value = decoded;
        }
        if ptr != 0 && capacity as usize <= value.len() { w32(out + offset + ps(), value.len() as u32 + 1); return fail(122); }
        copies.push((offset, ptr, start, value));
    }
    for (offset, ptr, start, value) in copies {
        if ptr == 0 { mem::wp(out + offset, if value.is_empty() { 0 } else { src + start as u32 * if wide { 2 } else { 1 } }); }
        else if wide { mem::put_wstr(ptr, value.len() as u32 + 1, &value); }
        else { mem::put_cstr(ptr, value.len() as u32 + 1, &value.iter().map(|u| *u as u8).collect::<Vec<_>>()); }
        w32(out + offset + ps(), value.len() as u32);
    }
    w32(out + if x64 { 20 } else { 12 }, kind);
    mem::w16(out + if x64 { 36 } else { 24 }, port);
    1
}
fn InternetCrackUrlA(c: &mut Ctx) -> u32 { crack_url(c, false) }
fn InternetCrackUrlW(c: &mut Ctx) -> u32 { crack_url(c, true) }
fn string(p: u32, wide: bool) -> String {
    if p == 0 {
        String::new()
    } else {
        crate::k32::arg_str(p, wide)
    }
}
fn headers(p: u32, n: u32, wide: bool) -> Result<Vec<(String, String)>, u32> {
    if p == 0 {
        return if n == 0 { Ok(vec![]) } else { Err(87) };
    }
    let s = if n == u32::MAX {
        string(p, wide)
    } else {
        if n as usize > LIMIT {
            return Err(87);
        }
        if wide {
            String::from_utf16_lossy(&mem::wstrn(p, n))
        } else {
            crate::text::a2s(&mem::cstrn(p, n))
        }
    };
    let mut out = vec![];
    for line in s.split("\r\n").filter(|s| !s.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(87);
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.contains(['\r', '\n', '\0'])
        {
            return Err(87);
        }
        out.push((name.into(), value.trim().into()));
    }
    Ok(out)
}
fn open(c: &mut Ctx, wide: bool) -> u32 {
    if c.a(4) & 0x10000000 != 0 {
        return fail(50);
    }
    if !matches!(c.a(1), 0 | 1) {
        return fail(50);
    }
    add(Handle::Session(Session {
        agent: string(c.a(0), wide),
        timeout: 30000,
        callback: 0,
    }))
}
fn InternetOpenA(c: &mut Ctx) -> u32 {
    open(c, false)
}
fn InternetOpenW(c: &mut Ctx) -> u32 {
    open(c, true)
}
fn connect(c: &mut Ctx, wide: bool) -> u32 {
    let Some(Handle::Session(session)) = get(c.a(0)) else {
        return fail(6);
    };
    if c.a(5) != 3 {
        return fail(50);
    }
    if c.a(3) != 0 || c.a(4) != 0 {
        return fail(50);
    }
    let host = string(c.a(1), wide);
    if host.is_empty() || host.contains(['/', ':', '@', '?', '#', '\r', '\n']) {
        return fail(87);
    }
    add(Handle::Connection(Connection {
        session,
        host,
        port: c.a(2) as u16,
    }))
}
fn InternetConnectA(c: &mut Ctx) -> u32 {
    connect(c, false)
}
fn InternetConnectW(c: &mut Ctx) -> u32 {
    connect(c, true)
}
fn request(c: &mut Ctx, wide: bool) -> u32 {
    let Some(Handle::Connection(connection)) = get(c.a(0)) else {
        return fail(6);
    };
    let method = if c.a(1) == 0 {
        "GET".into()
    } else {
        string(c.a(1), wide)
    };
    let mut path = string(c.a(2), wide);
    if path.is_empty() {
        path = "/".into()
    }
    if !path.starts_with('/') || path.starts_with("//") || path.contains(['\r', '\n', '\0']) {
        return fail(87);
    }
    if c.a(6) & 0x3000 != 0 {
        return fail(50);
    } // certificate-name/date bypass flags unsupported
    let mut accept = vec![];
    if c.a(5) != 0 {
        for i in 0..128 {
            let p = rp(c.a(5) + i * ps());
            if p == 0 {
                break;
            }
            accept.push(string(p, wide));
        }
    }
    let headers = if accept.is_empty() {
        vec![]
    } else {
        vec![("Accept".into(), accept.join(", "))]
    };
    add(Handle::Request(Request {
        connection,
        method,
        path,
        flags: c.a(6),
        headers,
        body: vec![],
        response: None,
    }))
}
fn HttpOpenRequestA(c: &mut Ctx) -> u32 {
    request(c, false)
}
fn HttpOpenRequestW(c: &mut Ctx) -> u32 {
    request(c, true)
}
fn add_headers(c: &mut Ctx, wide: bool) -> u32 {
    let values = match headers(c.a(1), c.a(2), wide) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let flags = c.a(3);
    let result = edit(c.a(0), |h| {
        let Handle::Request(r) = h else { return Err(6) };
        for (name, value) in values {
            let i = r
                .headers
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case(&name));
            if flags & 0x80000000 != 0 {
                r.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
                if !value.is_empty() {
                    r.headers.push((name, value));
                }
            } else if let Some(i) = i {
                if flags & 0x10000000 != 0 {
                    return Err(12155);
                }
                if flags & 0x01000000 != 0 {
                    r.headers[i].1.push_str(", ");
                    r.headers[i].1.push_str(&value)
                } else if flags & 0x02000000 != 0 {
                    r.headers[i].1.push_str("; ");
                    r.headers[i].1.push_str(&value)
                } else {
                    r.headers.push((name, value));
                }
            } else {
                r.headers.push((name, value));
            }
        }
        Ok(())
    });
    match result {
        Some(Ok(())) => 1,
        Some(Err(e)) => fail(e),
        None => fail(6),
    }
}
fn HttpAddRequestHeadersA(c: &mut Ctx) -> u32 {
    add_headers(c, false)
}
fn HttpAddRequestHeadersW(c: &mut Ctx) -> u32 {
    add_headers(c, true)
}
fn transport(r: &Request) -> Result<Response, u32> {
    let connection = &r.connection;
    let host = connection.host.to_ascii_lowercase();
    if crate::env_on!("SAKE_TRACE_WININET") {
        eprintln!("sake: WinINet target allowed={} port={} secure={}", ippai_tls::target(&host).is_some(), connection.port, r.flags & 0x800000 != 0);
    }
    if r.flags & 0x800000 == 0 || !ippai_tls::target(&host).is_some() || !matches!(connection.port, 0 | 443) {
        return Err(12029);
    }
    let mut headers = r.headers.clone();
    if !connection.session.agent.is_empty() && !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("user-agent")) {
        headers.push(("User-Agent".into(), connection.session.agent.clone()));
    }
    let request = ippai_tls::Request {
        host: host.clone(),
        method: r.method.clone(),
        path: r.path.clone(),
        headers,
        body: r.body.clone(),
    };
    let io = crate::tls_socket::Socket::open(&host, connection.session.timeout).map_err(|_| 12029u32)?;
    let response =
        ippai_tls::exchange(io, &request, ippai_tls::default_config()).map_err(|error| -> u32 {
            use ippai_tls::{
                Error,
                rustls::{CertificateError, Error as TlsError},
            };
            match error {
                Error::Tls(TlsError::InvalidCertificate(
                    CertificateError::NotValidForName
                    | CertificateError::NotValidForNameContext { .. },
                )) => 12038,
                Error::Tls(TlsError::InvalidCertificate(
                    CertificateError::Expired
                    | CertificateError::ExpiredContext { .. }
                    | CertificateError::NotValidYet
                    | CertificateError::NotValidYetContext { .. },
                )) => 12037,
                Error::Tls(TlsError::InvalidCertificate(_)) => 12045,
                Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => 12002,
                Error::InvalidRequest => 87,
                _ => 12152,
            }
        })?;
    if crate::env_on!("SAKE_TRACE_WININET") {
        eprintln!("sake: WinINet HTTP {} body bytes {}", response.status, response.body.len());
    }
    Ok(Response {
        status: response.status as u32,
        text: response.text,
        headers: response.headers,
        body: response.body,
        offset: 0,
    })
}
fn send(id: u32) -> u32 {
    let Some(Handle::Request(r)) = get(id) else {
        return fail(6);
    };
    let response = match transport(&r) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    if edit(id, |h| {
        if let Handle::Request(r) = h {
            r.response = Some(response);
            true
        } else {
            false
        }
    }) != Some(true)
    {
        return fail(6);
    }
    1
}
fn send_request(c: &mut Ctx, wide: bool) -> u32 {
    let values = match headers(c.a(1), c.a(2), wide) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let n = c.a(4) as usize;
    if n > LIMIT || n != 0 && c.a(3) == 0 {
        return fail(87);
    }
    let body = if n == 0 {
        vec![]
    } else {
        mem::slice(c.a(3), n as u32).to_vec()
    };
    if edit(c.a(0), |h| {
        if let Handle::Request(r) = h {
            r.headers.extend(values);
            r.body = body;
            r.response = None;
            true
        } else {
            false
        }
    }) != Some(true)
    {
        return fail(6);
    }
    send(c.a(0))
}
fn HttpSendRequestA(c: &mut Ctx) -> u32 {
    send_request(c, false)
}
fn HttpSendRequestW(c: &mut Ctx) -> u32 {
    send_request(c, true)
}
fn HttpEndRequestA(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 || c.a(2) != 0 {
        return fail(50);
    }
    send(c.a(0))
}
fn HttpEndRequestW(c: &mut Ctx) -> u32 {
    HttpEndRequestA(c)
}
fn send_ex(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(1);
    if p == 0 || c.a(2) != 0 {
        return fail(87);
    }
    let (hp, hn, bp, bn) = if mem::x64() {
        (rp(p + 16), r32(p + 24), rp(p + 32), r32(p + 40))
    } else {
        (rp(p + 8), r32(p + 12), rp(p + 20), r32(p + 24))
    };
    let values = match headers(hp, hn, wide) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    if bn as usize > LIMIT || bn != 0 && bp == 0 {
        return fail(87);
    }
    let body = if bn == 0 {
        vec![]
    } else {
        mem::slice(bp, bn).to_vec()
    };
    if edit(c.a(0), |h| {
        if let Handle::Request(r) = h {
            r.headers.extend(values);
            r.body = body;
            r.response = None;
            true
        } else {
            false
        }
    }) != Some(true)
    {
        return fail(6);
    }
    1
}
fn HttpSendRequestExA(c: &mut Ctx) -> u32 {
    send_ex(c, false)
}
fn HttpSendRequestExW(c: &mut Ctx) -> u32 {
    send_ex(c, true)
}
fn InternetReadFile(c: &mut Ctx) -> u32 {
    let (id, out, n, done) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if done == 0 || n != 0 && out == 0 {
        return fail(87);
    }
    w32(done, 0);
    let result = edit(id, |h| {
        let Handle::Request(r) = h else { return Err(6) };
        let Some(response) = r.response.as_mut() else {
            return Err(12019);
        };
        let n = (n as usize).min(response.body.len() - response.offset);
        mem::write(out, &response.body[response.offset..response.offset + n]);
        response.offset += n;
        Ok(n)
    });
    match result {
        Some(Ok(n)) => {
            w32(done, n as u32);
            1
        }
        Some(Err(e)) => fail(e),
        None => fail(6),
    }
}
fn InternetWriteFile(c: &mut Ctx) -> u32 {
    let (id, p, n, done) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if done == 0 || n != 0 && p == 0 {
        return fail(87);
    }
    w32(done, 0);
    let result = edit(id, |h| {
        let Handle::Request(r) = h else { return Err(6) };
        if r.body.len().saturating_add(n as usize) > LIMIT {
            return Err(8);
        }
        if n != 0 {
            r.body.extend_from_slice(mem::slice(p, n));
        }
        Ok(())
    });
    match result {
        Some(Ok(())) => {
            w32(done, n);
            1
        }
        Some(Err(e)) => fail(e),
        None => fail(6),
    }
}
fn InternetQueryDataAvailable(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return fail(87);
    }
    let Some(Handle::Request(r)) = get(c.a(0)) else {
        return fail(6);
    };
    let Some(response) = r.response else {
        return fail(12019);
    };
    w32(c.a(1), (response.body.len() - response.offset) as u32);
    1
}
fn output(text: &str, out: u32, len: u32, wide: bool) -> u32 {
    if len == 0 {
        return fail(87);
    }
    let bytes: Vec<u8> = if wide {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    } else {
        crate::text::s2a(text)
    };
    let cap = r32(len);
    let terminator = if wide { 2 } else { 1 };
    if out == 0 || cap < bytes.len() as u32 + terminator {
        w32(len, bytes.len() as u32 + terminator);
        return fail(122);
    }
    mem::write(out, &bytes);
    mem::write(out + bytes.len() as u32, &vec![0; terminator as usize]);
    w32(len, bytes.len() as u32);
    1
}
/// The HTTP_QUERY_* header indices of wininet.h (empty: not a header line; 18-22, 34 and 45
/// are answered above).
const HEADER_NAMES: [&str; 70] = [
    "mime-version", "content-type", "content-transfer-encoding", "content-id", "content-description",
    "content-length", "content-language", "allow", "public", "date", "expires", "last-modified",
    "message-id", "uri", "derived-from", "cost", "link", "pragma", "", "", "", "", "", "connection",
    "accept", "accept-charset", "accept-encoding", "accept-language", "authorization", "content-encoding",
    "forwarded", "from", "if-modified-since", "location", "", "referer", "retry-after", "server",
    "title", "user-agent", "www-authenticate", "proxy-authenticate", "accept-ranges", "set-cookie",
    "cookie", "", "refresh", "content-disposition", "age", "cache-control", "content-base",
    "content-location", "content-md5", "content-range", "etag", "host", "if-match", "if-none-match",
    "if-range", "if-unmodified-since", "max-forwards", "proxy-authorization", "range",
    "transfer-encoding", "upgrade", "vary", "via", "warning", "expect", "proxy-connection",
];
fn query(c: &mut Ctx, wide: bool) -> u32 {
    let (id, info, out, len, index) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if len == 0 {
        return fail(87);
    }
    let Some(Handle::Request(r)) = get(id) else {
        return fail(6);
    };
    let kind = info & 0xffff;
    // HTTP_QUERY_FLAG_REQUEST_HEADERS: the request's own headers, as this side sends them
    // (before it is sent too); the response's only once there is one
    let request_side = info & 0x8000_0000 != 0;
    let response = match r.response.clone() {
        Some(v) => v,
        None if request_side => Response::default(),
        None => return fail(12019),
    };
    let fields: Vec<(String, String)> = if request_side {
        let mut h = r.headers.clone();
        if !r.connection.session.agent.is_empty() && !h.iter().any(|(n, _)| n.eq_ignore_ascii_case("user-agent")) {
            h.push(("User-Agent".into(), r.connection.session.agent.clone()));
        }
        h
    } else {
        response.headers.clone()
    };
    let values = match kind {
        19 if !request_side => vec![response.status.to_string()],
        20 if !request_side => vec![response.text],
        18 => vec!["HTTP/1.1".into()],
        45 if request_side => vec![r.method.clone()],
        // RAW_HEADERS (each line ends in a NUL, the list in one more) and RAW_HEADERS_CRLF
        21 | 22 => {
            let (end, last) = if kind == 21 { ("\0", "\0") } else { ("\r\n", "\r\n") };
            let mut s = if request_side { format!("{} {} HTTP/1.1{end}", r.method, r.path) } else { format!("HTTP/1.1 {} {}{end}", response.status, response.text) };
            for (n, v) in &fields {
                s += &format!("{n}: {v}{end}");
            }
            s += last;
            vec![s]
        }
        _ => {
            let name = if kind == 65535 {
                string(out, wide)
            } else {
                match HEADER_NAMES.get(kind as usize).copied() {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => return fail(12150),
                }
            };
            fields
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case(&name))
                .map(|(_, v)| v.clone())
                .collect()
        }
    };
    let at = if index == 0 { 0 } else { r32(index) as usize };
    let Some(value) = values.get(at) else {
        return fail(12150);
    };
    let result = if info & 0x20000000 != 0 {
        let Ok(n) = value.parse::<u32>() else {
            return fail(12150);
        };
        if out == 0 || r32(len) < 4 {
            w32(len, 4);
            return fail(122);
        }
        w32(out, n);
        w32(len, 4);
        1
    } else {
        output(value, out, len, wide)
    };
    if result != 0 && index != 0 {
        w32(index, (at + 1) as u32)
    }
    result
}
fn HttpQueryInfoA(c: &mut Ctx) -> u32 {
    query(c, false)
}
fn HttpQueryInfoW(c: &mut Ctx) -> u32 {
    query(c, true)
}
fn InternetCloseHandle(c: &mut Ctx) -> u32 {
    if HANDLES
        .lock()
        .unwrap()
        .as_mut()
        .and_then(|m| m.remove(&c.a(0)))
        .is_some()
    {
        1
    } else {
        fail(6)
    }
}
fn session(h: &mut Handle) -> &mut Session {
    match h {
        Handle::Session(s) => s,
        Handle::Connection(s) => &mut s.session,
        Handle::Request(s) => &mut s.connection.session,
    }
}
fn set_option(c: &mut Ctx) -> u32 {
    let (id, option, p, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if p == 0 || n != 4 {
        return fail(87);
    }
    let value = r32(p);
    match option {
        2 | 5 | 6 => {
            if edit(id, |h| session(h).timeout = value).is_some() {
                1
            } else {
                fail(6)
            }
        }
        31 => {
            if value == 0 && get(id).is_some() {
                1
            } else {
                fail(50)
            }
        }
        _ => fail(12009),
    }
}
fn InternetSetOptionA(c: &mut Ctx) -> u32 {
    set_option(c)
}
fn InternetSetOptionW(c: &mut Ctx) -> u32 {
    set_option(c)
}
fn set_callback(c: &mut Ctx) -> u32 {
    let value = c.a(1);
    edit(c.a(0), |h| {
        std::mem::replace(&mut session(h).callback, value)
    })
    .unwrap_or(u32::MAX)
}
fn InternetSetStatusCallback(c: &mut Ctx) -> u32 {
    set_callback(c)
}
fn InternetSetStatusCallbackA(c: &mut Ctx) -> u32 {
    set_callback(c)
}
fn InternetSetStatusCallbackW(c: &mut Ctx) -> u32 {
    set_callback(c)
}
pub fn init() {
    macro_rules! reg{($($name:ident),*)=>{$(api::implement("wininet",stringify!($name),$name);)*}}
    reg!(
        InternetCanonicalizeUrlA,
        InternetCanonicalizeUrlW,
        InternetCrackUrlA,
        InternetCrackUrlW,
        InternetOpenA,
        InternetOpenW,
        InternetConnectA,
        InternetConnectW,
        HttpOpenRequestA,
        HttpOpenRequestW,
        HttpAddRequestHeadersA,
        HttpAddRequestHeadersW,
        HttpSendRequestA,
        HttpSendRequestW,
        HttpSendRequestExA,
        HttpSendRequestExW,
        HttpEndRequestA,
        HttpEndRequestW,
        InternetReadFile,
        InternetWriteFile,
        InternetQueryDataAvailable,
        HttpQueryInfoA,
        HttpQueryInfoW,
        InternetCloseHandle,
        InternetSetOptionA,
        InternetSetOptionW,
        InternetSetStatusCallback,
        InternetSetStatusCallbackA,
        InternetSetStatusCallbackW
    );
}
