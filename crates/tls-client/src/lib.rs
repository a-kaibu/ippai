//! TLS stays in this crate; its Read/Write transport sees only TLS records.
//! HTTP status and body bytes are returned exactly, without licensing policy.
pub use rustls;
use std::{
    io::{self, Read, Write},
    sync::{Arc, OnceLock},
};
const BODY_MAX: usize = 4 * 1024 * 1024;
const HEADER_MAX: usize = 64 * 1024;
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
    let mut out = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        r.method,
        r.path,
        r.host,
        r.body.len()
    )
    .into_bytes();
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
            let number = chunk_header
                .split(|b| *b == b';')
                .next()
                .ok_or(Error::InvalidResponse)?;
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
        for value in lengths {
            let n = value.parse::<usize>().map_err(|_| Error::InvalidResponse)?;
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
            let n = io.read(&mut temp).map_err(tls_error)?;
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
