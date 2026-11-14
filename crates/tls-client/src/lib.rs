//! TLS stays in this crate; its Read/Write transport sees only TLS records.
//! HTTP status and body bytes are returned exactly, without licensing policy.
pub use rustls;
use std::{
    io::{self, Read, Write},
    sync::{Arc, OnceLock},
};
const BODY_MAX: usize = 4 * 1024 * 1024;
/// The only HTTPS hosts the browser transport reaches: (ippai serve's route, host), on 443. The
/// index is the transport's target number (host/tls-bridge.js has the routes in this order;
/// crates/ippai/src/tls_relay.rs the same routes, checked by its tests).
pub const DESTINATIONS: [(&str, &str); 3] = [
    ("soft-denchi", "charge1.soft-denchi.jp"),
    ("dmm-softdenchi", "dlsoft.dmm.co.jp"),
    ("dmm-doujin-softdenchi", "softdenchi-doujin-api.dmm.co.jp"),
];
/// The target number of `host` (lower case), if the transport reaches it.
pub fn target(host: &str) -> Option<u32> {
    DESTINATIONS.iter().position(|d| d.1 == host).map(|i| i as u32)
}
const HEADER_MAX: usize = 64 * 1024;
/// Interim (1xx) responses before the final one, at most: a peer that sends `100 Continue`
/// without end otherwise keeps the guest thread here as long as it likes.
const INTERIM_MAX: usize = 8;
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Tls(rustls::Error),
    InvalidRequest,
    InvalidResponse,
    TooLarge,
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
#[derive(Clone, Debug)]
pub struct Request {
    pub host: String,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
pub fn default_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            Arc::new(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .expect("TLS protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
            )
        })
        .clone()
}
fn token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
fn encode(r: &Request) -> Result<Vec<u8>, Error> {
    if !token(&r.method)
        || r.host.is_empty()
        || r.host
            .bytes()
            .any(|b| !b.is_ascii_alphanumeric() && b != b'.' && b != b'-')
        || !r.path.starts_with('/')
        || r.path.starts_with("//")
        || r.path.bytes().any(|b| b <= 0x20 || b >= 0x7f)
        || r.body.len() > BODY_MAX
    {
        return Err(Error::InvalidRequest);
    }
    let mut out = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n", r.method, r.path, r.host).into_bytes();
    // (as WinINet: a GET or HEAD without a body says nothing of its length)
    if !r.body.is_empty() || !matches!(r.method.as_str(), "GET" | "HEAD") {
        out.extend_from_slice(format!("Content-Length: {}\r\n", r.body.len()).as_bytes());
    }
    for (n, v) in &r.headers {
        if !token(n) || v.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n') {
            return Err(Error::InvalidRequest);
        }
        if [
            "host",
            "connection",
            "content-length",
            "transfer-encoding",
            "proxy-authorization",
            "accept-encoding",
        ]
        .iter()
        .any(|h| n.eq_ignore_ascii_case(h))
        {
            continue;
        }
        out.extend_from_slice(n.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Accept-Encoding: identity\r\n\r\n");
    if out.len() > HEADER_MAX {
        return Err(Error::TooLarge);
    }
    out.extend_from_slice(&r.body);
    Ok(out)
}
/// Checks the certificate chain and hostname before sending any HTTP bytes.
pub fn exchange<T: Read + Write>(
    mut io: T,
    r: &Request,
    config: Arc<rustls::ClientConfig>,
) -> Result<Response, Error> {
    let bytes = encode(r)?;
    let name = rustls::pki_types::ServerName::try_from(r.host.clone())
        .map_err(|_| Error::InvalidRequest)?;
    let mut tls = rustls::ClientConnection::new(config, name).map_err(Error::Tls)?;
    tls.set_buffer_limit(Some(64 * 1024));
    while tls.is_handshaking() {
        tls.complete_io(&mut io).map_err(|e| tls_error(e))?;
    }
    let mut stream = rustls::Stream::new(&mut tls, &mut io);
    stream.write_all(&bytes).map_err(tls_error)?;
    stream.flush().map_err(tls_error)?;
    read_response(&mut stream, &r.method)
}
fn tls_error(e: io::Error) -> Error {
    if let Some(error) = e.get_ref().and_then(|e| e.downcast_ref::<rustls::Error>()) {
        Error::Tls(error.clone())
    } else {
        Error::Io(e)
    }
}
fn read_response<R: Read>(io: &mut R, method: &str) -> Result<Response, Error> {
    let mut data = vec![];
    let mut temp = [0u8; 4096];
    let mut interim = 0;
    let (head, status, text, headers) = loop {
        if let Some(end) = data
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|n| n + 4)
        {
            if end > HEADER_MAX {
                return Err(Error::TooLarge);
            }
            let mut fields = [httparse::EMPTY_HEADER; 128];
            let mut parsed = httparse::Response::new(&mut fields);
            if !matches!(
                parsed.parse(&data[..end]),
                Ok(httparse::Status::Complete(_))
            ) {
                return Err(Error::InvalidResponse);
            }
            let status = parsed.code.ok_or(Error::InvalidResponse)?;
            if status < 200 && status != 101 {
                interim += 1;
                if interim > INTERIM_MAX {
                    return Err(Error::InvalidResponse);
                }
                data.drain(..end);
                continue;
            }
            if status == 101 {
                return Err(Error::InvalidResponse);
            }
            let headers = parsed
                .headers
                .iter()
                .map(|h| {
                    (
                        h.name.to_owned(),
                        String::from_utf8_lossy(h.value).into_owned(),
                    )
                })
                .collect::<Vec<_>>();
            break (end, status, parsed.reason.unwrap_or("").to_owned(), headers);
        }
        if data.len() >= HEADER_MAX {
            return Err(Error::TooLarge);
        }
        let n = io.read(&mut temp).map_err(tls_error)?;
        if n == 0 {
            return Err(Error::InvalidResponse);
        }
        data.extend_from_slice(&temp[..n]);
    };
    let mut pending = data[head..].to_vec();
    let values = |name: &str| {
        headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
    };
    let lengths = values("content-length");
    let transfers = values("transfer-encoding");
    if !lengths.is_empty() && !transfers.is_empty() {
        return Err(Error::InvalidResponse);
    }
    let mut body = vec![];
    if method == "HEAD" || matches!(status, 204 | 304) {
    } else if !transfers.is_empty() {
        if transfers.len() != 1 || !transfers[0].eq_ignore_ascii_case("chunked") {
            return Err(Error::InvalidResponse);
        }
        let mut source = Prefix {
            pending: std::mem::take(&mut pending),
            at: 0,
            io,
        };
        loop {
            let chunk_header = line(&mut source, 8192)?;
            let mut number = chunk_header
                .split(|b| *b == b';')
                .next()
                .ok_or(Error::InvalidResponse)?;
            // (RFC 9112 7.1.1: whitespace may come before the extension, `1a ;x` or `1a  `,
            // as older IIS and Apache send)
            while let [rest @ .., b' ' | b'\t'] = number {
                number = rest;
            }
            if number.is_empty() || number.len() > 16 || !number.iter().all(u8::is_ascii_hexdigit) {
                return Err(Error::InvalidResponse);
            }
            let n = usize::from_str_radix(
                std::str::from_utf8(number).map_err(|_| Error::InvalidResponse)?,
                16,
            )
            .map_err(|_| Error::InvalidResponse)?;
            if n > BODY_MAX - body.len() {
                return Err(Error::TooLarge);
            }
            if n == 0 {
                let mut trailer = 0;
                loop {
                    let l = line(&mut source, 8192)?;
                    trailer += l.len() + 2;
                    if trailer > HEADER_MAX {
                        return Err(Error::TooLarge);
                    }
                    if l.is_empty() {
                        break;
                    }
                }
                break;
            }
            let start = body.len();
            body.resize(start + n, 0);
            source.read_exact(&mut body[start..]).map_err(tls_error)?;
            let mut end = [0; 2];
            source.read_exact(&mut end).map_err(tls_error)?;
            if end != *b"\r\n" {
                return Err(Error::InvalidResponse);
            }
        }
    } else if !lengths.is_empty() {
        let mut size = None;
        // (RFC 9110 8.6: a list of one repeated value is that value)
        for value in lengths.iter().flat_map(|v| v.split(',')) {
            let n = value.trim().parse::<usize>().map_err(|_| Error::InvalidResponse)?;
            if n > BODY_MAX {
                return Err(Error::TooLarge);
            }
            if size.is_some_and(|old| old != n) {
                return Err(Error::InvalidResponse);
            }
            size = Some(n)
        }
        let n = size.unwrap();
        let mut source = Prefix { pending, at: 0, io };
        body.resize(n, 0);
        source.read_exact(&mut body).map_err(tls_error)?;
    } else {
        if pending.len() > BODY_MAX {
            return Err(Error::TooLarge);
        }
        body = pending;
        loop {
            let n = match io.read(&mut temp) {
                Ok(n) => n,
                // (a body that ends with the connection: many servers close without TLS's
                // close_notify, which rustls reports as an unexpected EOF; a truncation attack
                // needs a body that has no length, which is what this one is)
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => 0,
                Err(e) => return Err(tls_error(e)),
            };
            if n == 0 {
                break;
            }
            if n > BODY_MAX - body.len() {
                return Err(Error::TooLarge);
            }
            body.extend_from_slice(&temp[..n]);
        }
    }
    Ok(Response {
        status,
        text,
        headers,
        body,
    })
}
struct Prefix<'a, R> {
    pending: Vec<u8>,
    at: usize,
    io: &'a mut R,
}
impl<R: Read> Read for Prefix<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at < self.pending.len() {
            let n = out.len().min(self.pending.len() - self.at);
            out[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        } else {
            self.io.read(out)
        }
    }
}
fn line<R: Read>(io: &mut R, limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = vec![];
    loop {
        let mut byte = [0];
        io.read_exact(&mut byte).map_err(tls_error)?;
        out.push(byte[0]);
        if out.len() > limit {
            return Err(Error::TooLarge);
        }
        if out.ends_with(b"\r\n") {
            out.truncate(out.len() - 2);
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    /// A server's bytes, read in pieces of `step`, then a clean end or (rustls without the
    /// peer's close_notify) an unexpected EOF.
    struct Wire { data: Vec<u8>, at: usize, step: usize, abrupt: bool }
    impl Read for Wire {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.at == self.data.len() {
                return if self.abrupt { Err(io::ErrorKind::UnexpectedEof.into()) } else { Ok(0) };
            }
            let n = out.len().min(self.step).min(self.data.len() - self.at);
            out[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }
    fn parse(data: &[u8], step: usize, abrupt: bool, method: &str) -> Result<Response, Error> {
        read_response(&mut Wire { data: data.to_vec(), at: 0, step, abrupt }, method)
    }
    fn request(method: &str, body: &[u8]) -> String {
        let r = Request { host: "example.com".into(), method: method.into(), path: "/a".into(), headers: vec![("User-Agent".into(), "x".into()), ("Content-Length".into(), "9".into())], body: body.to_vec() };
        String::from_utf8(encode(&r).unwrap()).unwrap()
    }
    #[test]
    fn requests_say_their_length_unless_an_empty_get_or_head() {
        assert!(!request("GET", b"").contains("Content-Length"));
        assert!(!request("HEAD", b"").contains("Content-Length"));
        assert!(request("POST", b"").contains("\r\nContent-Length: 0\r\n"));
        assert!(request("POST", b"abc").ends_with("\r\nContent-Length: 3\r\nUser-Agent: x\r\nAccept-Encoding: identity\r\n\r\nabc"));
        // (the caller's own Content-Length is dropped: one, computed here)
        assert_eq!(request("POST", b"abc").matches("Content-Length").count(), 1);
    }
    #[test]
    fn bodies_by_length_chunks_or_the_connection_end() {
        let ok = |d: &[u8], abrupt| parse(d, 3, abrupt, "GET").unwrap().body;
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhiXX", false), b"hi");
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\nContent-Length: 2, 2\r\n\r\nhi", false), b"hi");
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;x=y\r\nhi\r\n1\r\n!\r\n0\r\nT: v\r\n\r\n", false), b"hi!");
        assert_eq!(ok(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\n\r\nto the end", false), b"to the end");
        // (the server closed TCP without TLS's close_notify)
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\n\r\nto the end", true), b"to the end");
        assert_eq!(parse(b"HTTP/1.1 204 No Content\r\n\r\n", 1, true, "GET").unwrap().status, 204);
        assert!(parse(b"HTTP/1.1 200 OK\r\n\r\n", 1, false, "HEAD").unwrap().body.is_empty());
        // (whitespace after a chunk's size, before its extension or alone)
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2 \t\r\nhi\r\n1 ;x\r\n!\r\n0  \r\n\r\n", false), b"hi!");
    }
    #[test]
    fn interim_responses_are_bounded() {
        let wire = |n: usize| {
            let mut w = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(n);
            w.extend(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
            w
        };
        assert_eq!(parse(&wire(INTERIM_MAX), 7, false, "GET").unwrap().body, b"hi");
        assert!(matches!(parse(&wire(INTERIM_MAX + 1), 7, false, "GET"), Err(Error::InvalidResponse)));
        // (endless ones stop at the bound, not at the end of the wire)
        let endless = b"HTTP/1.1 102 Processing\r\n\r\n".repeat(10_000);
        let mut w = Wire { data: endless, at: 0, step: 64, abrupt: false };
        assert!(read_response(&mut w, "GET").is_err());
        assert!(w.at < 64 * 1024);
    }
    #[test]
    fn ambiguous_or_truncated_responses_fail() {
        let bad = |d: &[u8], abrupt| parse(d, 4, abrupt, "GET").is_err();
        assert!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nabc", false));
        assert!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: 2, 3\r\n\r\nabc", false));
        assert!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n", false));
        assert!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n", false));
        // (a declared length or a chunk cut short is an error even when the server just closed)
        assert!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nab", true));
        assert!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab", true));
        assert!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", false));
        assert!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n \r\n", false));
        assert!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1 2\r\nab\r\n", false));
        assert!(bad(b"HTTP/1.1 101 Switching\r\n\r\n", false));
        assert!(bad(b"HTTP/1.1 200 OK\r\n", false));
        let big = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", BODY_MAX + 1);
        assert!(matches!(parse(big.as_bytes(), 64, false, "GET"), Err(Error::TooLarge)));
    }
    proptest! {
        // Any body, chunked any way and read in any piece size, comes back exactly.
        #[test]
        fn chunked_bodies_round_trip(body in proptest::collection::vec(any::<u8>(), 0..2000), cuts in proptest::collection::vec(1usize..300, 1..20), step in 1usize..64, ws in proptest::collection::vec(prop_oneof![Just(""), Just(" "), Just("\t "), Just(" ;e=1")], 1..8)) {
            let mut wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            let (mut at, mut i) = (0, 0);
            while at < body.len() {
                let n = cuts[i % cuts.len()].min(body.len() - at);
                // (any whitespace before the line's end or its extension, RFC 9112's BWS)
                wire.extend(format!("{n:x}{}\r\n", ws[i % ws.len()]).as_bytes()); wire.extend(&body[at..at + n]); wire.extend(b"\r\n");
                at += n; i += 1;
            }
            wire.extend(b"0\r\n\r\n");
            prop_assert_eq!(parse(&wire, step, true, "GET").unwrap().body, body);
        }
        // Arbitrary bytes never panic the parser.
        #[test]
        fn arbitrary_responses_never_panic(data in proptest::collection::vec(any::<u8>(), 0..600), step in 1usize..32, abrupt in any::<bool>()) {
            let mut wire = b"HTTP/1.1 200 OK\r\n".to_vec(); wire.extend(&data);
            let _ = parse(&wire, step, abrupt, "GET");
            let _ = parse(&data, step, abrupt, "GET");
        }
    }
}
