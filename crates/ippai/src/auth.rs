//! Who may use `ippai serve`.
//!
//! - everyone needs the server's token, `~/.config/ippai/token` (made at the first start;
//!   `ippai token` prints it and the URL to sign in with), once per browser: the sign-in page
//!   (`/login`, the token typed in or in the URL's fragment, `/login#<token>`, which no request,
//!   history entry of the server's or proxy log carries; the page posts it) sets a session
//!   cookie (HttpOnly, SameSite=Lax, ten years): a session id and its HMAC under the token, not
//!   the token. Scripts send `Authorization: Bearer <token>`. Without either: 401, the page.
//! - the server's own headless Chromium (pre-runs) signs in with a ticket (`/login?ticket=`:
//!   an expiry and its HMAC, good for a few minutes) instead of the token on its command line
//! - with `--trust-loopback` (or `trust_loopback = true` in config.toml), a request from a
//!   loopback address for a loopback name (`Host: localhost:8656`, `127.0.0.1`, `[::1]`; not a
//!   name that only resolves there: DNS rebinding) with no proxy's forwarding headers needs
//!   nothing: for a machine nothing proxies to. Off by default: a reverse proxy on this
//!   machine connects from loopback, and one that sets `Host: 127.0.0.1` and adds no
//!   forwarding header (nginx's defaults do both) is not told apart from a local browser.
//! - on top of that, writes only from this origin's own pages (see `same_origin_write`)
//!
//! `ippai token --new` makes another (the server reads it at its start: restart it); it ends
//! every session made with the old one.

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use ring::hmac;
use std::{net::SocketAddr, path::Path, sync::Arc};

const COOKIE: &str = "ippai_session";
/// How long a pre-run's ticket is good for, at most (seconds).
const TICKET_SECS: u64 = 300;

pub struct Auth {
    token: String,
    key: hmac::Key,
    trust_loopback: bool,
}

/// The token in `path`, made (random, 128 bits, readable by this user only) when there is none
/// or `new`.
pub fn token(path: &Path, new: bool) -> Result<String> {
    if !new {
        if let Ok(t) = std::fs::read_to_string(path) {
            let t = t.trim().to_string();
            if t.len() >= 16 && t.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Ok(t);
            }
        }
    }
    let t = hex(&random::<16>()?);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).with_context(|| d.display().to_string())?;
    }
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).with_context(|| tmp.display().to_string())?;
    writeln!(f, "{t}")?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(t)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).map_err(|_| anyhow::anyhow!("no random numbers"))?;
    Ok(b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `a == b`, every byte looked at (no early out an attacker could time).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |x, (p, q)| x | (p ^ q)) == 0
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Auth {
    pub fn load(path: &Path, trust_loopback: bool) -> Result<Arc<Auth>> {
        Ok(Self::with(&token(path, false)?, trust_loopback))
    }

    fn with(token: &str, trust_loopback: bool) -> Arc<Auth> {
        Arc::new(Auth { token: token.into(), key: hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes()), trust_loopback })
    }

    fn matches(&self, given: &str) -> bool {
        same(self.token.as_bytes(), given.trim().as_bytes())
    }

    /// `<what>.<HMAC of kind:what>` (128 bits of it).
    fn signed(&self, kind: &str, what: &str) -> String {
        format!("{what}.{}", hex(&hmac::sign(&self.key, format!("{kind}:{what}").as_bytes()).as_ref()[..16]))
    }

    /// The `what` of a value [`signed`](Self::signed) made, if it is one.
    fn verified<'a>(&self, kind: &str, v: &'a str) -> Option<&'a str> {
        let (what, _) = v.split_once('.')?;
        same(self.signed(kind, what).as_bytes(), v.as_bytes()).then_some(what)
    }

    fn new_session(&self) -> String {
        self.signed("session", &hex(&random::<16>().unwrap_or_default()))
    }

    /// A ticket that signs a browser in once, for `secs` from now (at most TICKET_SECS).
    pub fn ticket(&self, secs: u64) -> String {
        self.signed("ticket", &(now() + secs.min(TICKET_SECS)).to_string())
    }

    fn ticket_ok(&self, t: &str) -> bool {
        self.verified("ticket", t).and_then(|e| e.parse::<u64>().ok()).is_some_and(|e| e >= now() && e <= now() + TICKET_SECS)
    }

    /// Whether `h` carries a session of this token's, or the token itself (a bearer header).
    fn signed_in(&self, h: &HeaderMap) -> bool {
        let bearer = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
        if bearer.is_some_and(|t| self.matches(t)) {
            return true;
        }
        h.get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|c| c.trim().strip_prefix(COOKIE).and_then(|r| r.strip_prefix('=')))
            .any(|v| v.len() == 32 + 1 + 32 && self.verified("session", v).is_some())
    }
}

/// `host` (a Host header: name, then maybe a port) names this machine itself.
fn loopback_name(host: &str) -> bool {
    let name = if let Some(r) = host.strip_prefix('[') {
        r.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |x| x.0)
    };
    let name = name.to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// A request that looks like this machine's own (see the module's notes: a proxy can look so).
fn local(peer: SocketAddr, h: &HeaderMap) -> bool {
    const FORWARDED: [&str; 5] = ["forwarded", "x-forwarded-for", "x-forwarded-host", "x-real-ip", "tailscale-user-login"];
    peer.ip().is_loopback()
        && h.get(header::HOST).and_then(|v| v.to_str().ok()).is_some_and(loopback_name)
        && FORWARDED.iter().all(|f| !h.contains_key(*f))
}

/// Where to go after signing in: a path of this server's (not `//host`, which is another one).
fn next_path(next: Option<String>) -> String {
    next.filter(|n| n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\") && !n.contains(['\r', '\n'])).unwrap_or_else(|| "/".into())
}

fn page(status: StatusCode, note: &str, next: &str) -> Response {
    let esc = |s: &str| s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    // (the fragment's token posted at once, and taken off the address bar)
    let body = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>ippai</title>\
         <style>body{{font:16px system-ui;max-width:28em;margin:3em auto;padding:0 16px}}input{{width:100%;font:inherit;padding:.4em;box-sizing:border-box}}</style>\
         <h1>ippai</h1><p>{note}</p><p>サーバーのマシンで <code>ippai token</code> を実行すると、このブラウザで開くURLとトークンが出ます。</p>\
         <form id=f method=post action=/login><input type=hidden name=next value=\"{}\"><input name=token type=password autocomplete=off placeholder=トークン autofocus><p><button>サインイン</button></form>\
         <script>const t=location.hash.slice(1);if(t){{history.replaceState(null,'',location.pathname+location.search);f.token.value=decodeURIComponent(t);f.submit()}}</script>",
        esc(next)
    );
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store"), (header::REFERRER_POLICY, "no-referrer")], body).into_response()
}

fn signed_in_to(auth: &Auth, h: &HeaderMap, next: &str) -> Response {
    let https = h.get("x-forwarded-proto").is_some_and(|v| v == "https");
    let cookie = format!("{COOKIE}={}; Path=/; Max-Age=315360000; HttpOnly; SameSite=Lax{}", auth.new_session(), if https { "; Secure" } else { "" });
    let mut res = (StatusCode::SEE_OTHER, [(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap())]).into_response();
    if let Ok(l) = HeaderValue::from_str(next) {
        res.headers_mut().insert(header::LOCATION, l);
    }
    res
}

/// `/login`: GET the page (or with a pre-run's ticket), POST the token.
async fn login(auth: &Auth, req: Request) -> Response {
    let query = |q: &str| form_urlencoded::parse(q.as_bytes()).into_owned().collect::<Vec<(String, String)>>();
    let q = query(req.uri().query().unwrap_or(""));
    let get = |q: &[(String, String)], k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let next = next_path(get(&q, "next"));
    if req.method() == Method::POST {
        let h = req.headers().clone();
        let Ok(body) = axum::body::to_bytes(req.into_body(), 4096).await else {
            return page(StatusCode::PAYLOAD_TOO_LARGE, "長すぎます。", &next);
        };
        let f = query(std::str::from_utf8(&body).unwrap_or(""));
        let next = next_path(get(&f, "next"));
        return if get(&f, "token").is_some_and(|t| auth.matches(&t)) {
            signed_in_to(auth, &h, &next)
        } else {
            page(StatusCode::UNAUTHORIZED, "トークンが違います。", &next)
        };
    }
    if get(&q, "ticket").is_some_and(|t| auth.ticket_ok(&t)) {
        return signed_in_to(auth, req.headers(), &next);
    }
    page(StatusCode::OK, "サインインしてください。", &next)
}

/// The server's front door (both listeners): `/login`, then everything only signed in (or, with
/// --trust-loopback, from this machine).
pub async fn guard(State(auth): State<Arc<Auth>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request<Body>, next: Next) -> Response {
    if req.uri().path() == "/login" {
        return login(&auth, req).await;
    }
    let h = req.headers();
    if (auth.trust_loopback && local(peer, h)) || auth.signed_in(h) {
        return next.run(req).await;
    }
    let back = req.uri().path_and_query().map_or("/", |p| p.as_str()).to_string();
    page(StatusCode::UNAUTHORIZED, "このサーバーはトークンが要ります。", &next_path(Some(back)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use tower::ServiceExt;

    const T: &str = "0123456789abcdef0123456789abcdef";

    fn app(auth: Arc<Auth>) -> Router {
        Router::new().route("/x", get(|| async { "ok" }).put(|| async { "ok" })).layer(axum::middleware::from_fn_with_state(auth, guard))
    }

    async fn send(auth: Arc<Auth>, peer: &str, req: axum::http::request::Builder, body: &str) -> (StatusCode, HeaderMap, String) {
        let mut r = req.body(Body::from(body.to_string())).unwrap();
        r.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        let res = app(auth).oneshot(r).await.unwrap();
        let (p, b) = res.into_parts();
        (p.status, p.headers, String::from_utf8_lossy(&axum::body::to_bytes(b, 1 << 20).await.unwrap()).into_owned())
    }

    async fn status_with(trust: bool, peer: &str, req: axum::http::request::Builder) -> StatusCode {
        send(Auth::with(T, trust), peer, req, "").await.0
    }

    async fn status(peer: &str, req: axum::http::request::Builder) -> StatusCode {
        status_with(false, peer, req).await
    }

    fn get_(host: &str, uri: &str) -> axum::http::request::Builder {
        Request::builder().uri(uri).header(header::HOST, host)
    }

    fn post_login(host: &str) -> axum::http::request::Builder {
        Request::builder().method("POST").uri("/login").header(header::HOST, host).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    }

    /// The session cookie a sign-in answer sets, and where it sends the browser.
    fn session(h: &HeaderMap) -> (String, String) {
        let c = h[header::SET_COOKIE].to_str().unwrap();
        assert!(c.contains("HttpOnly") && c.contains("SameSite=Lax"), "{c}");
        (c.split(';').next().unwrap().to_string(), h[header::LOCATION].to_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn loopback_needs_the_token_unless_trusted() {
        // a reverse proxy on this machine that sets Host: 127.0.0.1 and no forwarding header
        // (nginx's proxy_pass defaults) looks exactly like a browser here: so by default
        // nothing goes without the token, from loopback either
        for host in ["localhost:8656", "127.0.0.1:8656", "[::1]:8656", "app.localhost", "127.0.0.1"] {
            assert_eq!(status("127.0.0.1:5000", get_(host, "/x")).await, StatusCode::UNAUTHORIZED, "{host}");
            assert_eq!(status_with(true, "127.0.0.1:5000", get_(host, "/x")).await, StatusCode::OK, "{host} trusted");
        }
        assert_eq!(status("[::1]:5000", get_("[::1]:8656", "/x")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status_with(true, "[::1]:5000", get_("[::1]:8656", "/x")).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn trusted_loopback_still_tells_proxies_and_others() {
        let t = |peer, req| status_with(true, peer, req);
        // another device
        assert_eq!(t("100.64.0.2:5000", get_("100.64.0.1:8656", "/x")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(t("100.64.0.2:5000", get_("127.0.0.1:8656", "/x")).await, StatusCode::UNAUTHORIZED, "a Host of loopback from elsewhere");
        // a proxy that says it forwards (tailscale serve, a tunnel)
        for fwd in ["x-forwarded-for", "forwarded", "x-real-ip", "x-forwarded-host", "tailscale-user-login"] {
            let r = get_("127.0.0.1:8656", "/x").header(fwd, "100.64.0.2");
            assert_eq!(t("127.0.0.1:5000", r).await, StatusCode::UNAUTHORIZED, "{fwd}");
        }
        // DNS rebinding: a name of an attacker's resolving to 127.0.0.1
        assert_eq!(t("127.0.0.1:5000", get_("evil.example:8656", "/x")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(t("127.0.0.1:5000", get_("localhost.evil.example", "/x")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(t("127.0.0.1:5000", Request::builder().uri("/x")).await, StatusCode::UNAUTHORIZED, "no Host");
    }

    #[tokio::test]
    async fn bearer_token_for_scripts() {
        let dev = || get_("box.example.ts.net", "/x").header("x-forwarded-for", "100.64.0.2");
        assert_eq!(status("127.0.0.1:5000", dev().header(header::AUTHORIZATION, format!("Bearer {T}"))).await, StatusCode::OK);
        assert_eq!(status("127.0.0.1:5000", dev().method("PUT").header(header::AUTHORIZATION, format!("Bearer {T}"))).await, StatusCode::OK);
        for bad in ["0123456789abcdef0123456789abcdee", "0123456789abcdef", ""] {
            assert_eq!(status("127.0.0.1:5000", dev().header(header::AUTHORIZATION, format!("Bearer {bad}"))).await, StatusCode::UNAUTHORIZED, "{bad}");
        }
        // the token itself is no cookie (a cookie is a session)
        assert_eq!(status("127.0.0.1:5000", dev().header(header::COOKIE, format!("{COOKIE}={T}"))).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status("127.0.0.1:5000", dev().header(header::COOKIE, format!("ippai_auth={T}"))).await, StatusCode::UNAUTHORIZED, "the old cookie");
    }

    #[tokio::test]
    async fn sign_in_posts_the_token_for_a_session() {
        let auth = Auth::with(T, false);
        let host = "box.example.ts.net";
        let r = post_login(host).header("x-forwarded-for", "1.2.3.4").header("x-forwarded-proto", "https");
        let (s, h, _) = send(auth.clone(), "127.0.0.1:5000", r, &format!("token={T}&next=%2Fplay.html%3Fgame%3Dg")).await;
        assert_eq!(s, StatusCode::SEE_OTHER);
        let (cookie, to) = session(&h);
        assert!(h[header::SET_COOKIE].to_str().unwrap().contains("Secure"));
        assert_eq!(to, "/play.html?game=g");
        assert!(!cookie.contains(T), "the cookie is not the token: {cookie}");
        let with = |c: &str| get_(host, "/x").header("x-forwarded-for", "1.2.3.4").header(header::COOKIE, format!("a=b; {c}"));
        assert_eq!(send(auth.clone(), "127.0.0.1:5000", with(&cookie), "").await.0, StatusCode::OK);
        // each sign-in its own session
        let (_, h2, _) = send(auth.clone(), "127.0.0.1:5000", post_login(host), &format!("token={T}")).await;
        let (cookie2, to2) = session(&h2);
        assert_ne!(cookie, cookie2);
        assert_eq!(to2, "/");
        // a session of another token's (`ippai token --new` ends them), a forged or cut one
        assert_eq!(send(Auth::with("fedcba9876543210fedcba9876543210", false), "127.0.0.1:5000", with(&cookie), "").await.0, StatusCode::UNAUTHORIZED);
        let v = cookie.split_once('=').unwrap().1;
        let (id, tag) = v.split_once('.').unwrap();
        let flipped = format!("{COOKIE}={id}.{}{}", if tag.starts_with('0') { '1' } else { '0' }, &tag[1..]);
        for bad in [flipped, format!("{COOKIE}={id}."), format!("{COOKIE}={id}"), format!("{COOKIE}=.{tag}"), format!("x{cookie}")] {
            assert_eq!(send(auth.clone(), "127.0.0.1:5000", with(&bad), "").await.0, StatusCode::UNAUTHORIZED, "{bad}");
        }
        // a wrong token: no cookie
        let (s, h, _) = send(auth.clone(), "100.64.0.2:5000", post_login("100.64.0.1:8656"), "token=nope").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(!h.contains_key(header::SET_COOKIE));
        // the token in the query (history, a proxy's access log): not taken any more
        let (s, h, _) = send(auth.clone(), "100.64.0.2:5000", get_("100.64.0.1:8656", &format!("/login?token={T}")), "").await;
        assert_eq!(s, StatusCode::OK);
        assert!(!h.contains_key(header::SET_COOKIE));
    }

    #[tokio::test]
    async fn sign_in_goes_back_to_this_server_only() {
        let auth = Auth::with(T, false);
        for (next, to) in [("%2F%2Fevil.example%2F", "/"), ("https%3A%2F%2Fevil.example", "/"), ("%2F%5Cevil.example", "/"), ("%2Fa%0D%0ASet-Cookie%3A%20x", "/"), ("%2Fplay.html", "/play.html")] {
            let (_, h, _) = send(auth.clone(), "100.64.0.2:5000", post_login("h"), &format!("token={T}&next={next}")).await;
            assert_eq!(session(&h).1, to, "{next}");
        }
        // the page sent instead of a file names it, escaped
        let (s, _, body) = send(auth.clone(), "100.64.0.2:5000", get_("h", "/x?a=%22%3E%3Cscript%3E"), "").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(body.contains("name=next value=\"/x?a=%22%3E%3Cscript%3E\""), "{body}");
        let (_, _, body) = send(auth, "100.64.0.2:5000", get_("h", "/login?next=%2F%22%3E%3Cb%3E"), "").await;
        assert!(body.contains("value=\"/&quot;>&lt;b>\"") && !body.contains("<b>"), "{body}");
    }

    #[tokio::test]
    async fn pre_runs_sign_in_with_a_ticket() {
        let auth = Auth::with(T, false);
        let t = auth.ticket(60);
        assert!(!t.contains(T));
        let (s, h, _) = send(auth.clone(), "127.0.0.1:5000", get_("127.0.0.1:8656", &format!("/login?ticket={t}&next=%2Fplay.html%3Fgame%3Dg%26autostart")), "").await;
        assert_eq!(s, StatusCode::SEE_OTHER);
        assert_eq!(session(&h).1, "/play.html?game=g&autostart");
        // expired, too far ahead, forged, another token's
        let old = auth.signed("ticket", &(now() - 1).to_string());
        let far = auth.signed("ticket", &(now() + TICKET_SECS + 60).to_string());
        let forged = format!("{}.{}", now() + 60, "0".repeat(32));
        let other = Auth::with("fedcba9876543210fedcba9876543210", false).ticket(60);
        let session_as_ticket = auth.new_session();
        for bad in [old, far, forged, other, session_as_ticket] {
            let (s, h, _) = send(auth.clone(), "127.0.0.1:5000", get_("127.0.0.1:8656", &format!("/login?ticket={bad}")), "").await;
            assert_eq!(s, StatusCode::OK, "{bad}");
            assert!(!h.contains_key(header::SET_COOKIE), "{bad}");
        }
        // nor is a ticket a session
        let c = format!("{COOKIE}={t}");
        assert_eq!(send(auth, "127.0.0.1:5000", get_("127.0.0.1:8656", "/x").header(header::COOKIE, c), "").await.0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn token_is_made_once_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("ippai-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("cfg/token");
        let a = token(&p, false).unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(token(&p, false).unwrap(), a);
        let b = token(&p, true).unwrap();
        assert_ne!(a, b);
        assert_eq!(token(&p, false).unwrap(), b);
        // a damaged file: a new one
        std::fs::write(&p, "x\n").unwrap();
        assert_ne!(token(&p, false).unwrap(), "x");
        let _ = std::fs::remove_dir_all(&d);
    }
}
