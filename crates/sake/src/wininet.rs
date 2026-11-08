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
    crate::k32::last_error(error);
    0
}
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
    if r.flags & 0x800000 == 0
        || !connection
            .host
            .eq_ignore_ascii_case("charge1.soft-denchi.jp")
        || !matches!(connection.port, 0 | 443)
    {
        return Err(12029);
    }
    let mut headers = r.headers.clone();
    if !connection.session.agent.is_empty() {
        headers.push(("User-Agent".into(), connection.session.agent.clone()));
    }
    let request = ippai_tls::Request {
        host: "charge1.soft-denchi.jp".into(),
        method: r.method.clone(),
        path: r.path.clone(),
        headers,
        body: r.body.clone(),
    };
    let io = crate::tls_socket::Socket::open(connection.session.timeout).map_err(|_| 12029u32)?;
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
fn query(c: &mut Ctx, wide: bool) -> u32 {
    let (id, info, out, len, index) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if len == 0 {
        return fail(87);
    }
    let Some(Handle::Request(r)) = get(id) else {
        return fail(6);
    };
    let Some(response) = r.response else {
        return fail(12019);
    };
    let kind = info & 0xffff;
    let values = match kind {
        19 => vec![response.status.to_string()],
        20 => vec![response.text],
        18 => vec!["HTTP/1.1".into()],
        22 => {
            let mut s = format!("HTTP/1.1 {} {}\r\n", response.status, response.text);
            for (n, v) in &response.headers {
                s += &format!("{n}: {v}\r\n")
            }
            s += "\r\n";
            vec![s]
        }
        _ => {
            let name = match kind {
                1 => "content-type".into(),
                5 => "content-length".into(),
                21 => "connection".into(),
                23 => "user-agent".into(),
                28 => "location".into(),
                43 => "set-cookie".into(),
                65535 => string(out, wide),
                _ => return fail(12150),
            };
            response
                .headers
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
