//! Who may use `ippai serve`.
//!
//! - this machine's own browser (and the pre-runs, the tests, curl): a request from a loopback
//!   address, for a loopback name (`Host: localhost:8656`, `127.0.0.1`, `[::1]`: not a name that
//!   only resolves there, DNS rebinding), with no proxy's forwarding headers, needs nothing
//! - everyone else (another device on `--listen 0.0.0.0`, or through `tailscale serve`, a
//!   reverse proxy, a tunnel: those connect from loopback but say who they forward) needs the
//!   server's token, `~/.config/ippai/token` (made at the first start, `ippai token` prints the
//!   URL to sign in with): `GET /login?token=...` once per browser (a cookie, HttpOnly, for ten
//!   years), or `Authorization: Bearer <token>` (scripts). Without it: 401, a page to paste it in
//! - on top of that, writes only from this origin's own pages (see `same_origin_write`)
//!
//! `ippai token --new` makes another (the server reads it at its start: restart it).

use anyhow::{Context, Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{net::SocketAddr, path::Path, sync::Arc};

const COOKIE: &str = "ippai_auth";

pub struct Auth {
    token: String,
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
    let mut b = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).map_err(|_| anyhow::anyhow!("no random numbers"))?;
    let t: String = b.iter().map(|x| format!("{x:02x}")).collect();
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

impl Auth {
    pub fn load(path: &Path) -> Result<Arc<Auth>> {
        Ok(Arc::new(Auth { token: token(path, false)? }))
    }

    #[cfg(test)]
    fn with(token: &str) -> Arc<Auth> {
        Arc::new(Auth { token: token.into() })
    }

    fn matches(&self, given: &str) -> bool {
        // (every byte looked at: no early out an attacker could time)
        let (a, b) = (self.token.as_bytes(), given.trim().as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0, |x, (p, q)| x | (p ^ q)) == 0
    }

    /// Whether `h` carries the token (the cookie or a bearer header).
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
            .any(|t| self.matches(t))
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

/// A request from this machine itself, not forwarded by a proxy on it.
fn local(peer: SocketAddr, h: &HeaderMap) -> bool {
    const FORWARDED: [&str; 5] = ["forwarded", "x-forwarded-for", "x-forwarded-host", "x-real-ip", "tailscale-user-login"];
    peer.ip().is_loopback()
        && h.get(header::HOST).and_then(|v| v.to_str().ok()).is_some_and(loopback_name)
        && FORWARDED.iter().all(|f| !h.contains_key(*f))
}

fn page(status: StatusCode, note: &str) -> Response {
    let body = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>ippai</title>\
         <style>body{{font:16px system-ui;max-width:28em;margin:3em auto;padding:0 16px}}input{{width:100%;font:inherit;padding:.4em;box-sizing:border-box}}</style>\
         <h1>ippai</h1><p>{note}</p><p>サーバーのマシンで <code>ippai token</code> を実行すると、このブラウザで開くURLとトークンが出ます。</p>\
         <form action=/login><input name=token autocomplete=off placeholder=トークン autofocus><p><button>サインイン</button></form>"
    );
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], body).into_response()
}

/// The server's front door (both listeners): `/login`, then everything only from this machine
/// or with the token.
pub async fn guard(State(auth): State<Arc<Auth>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    let h = req.headers();
    if req.uri().path() == "/login" {
        let given = req.uri().query().unwrap_or("").split('&').find_map(|p| p.strip_prefix("token="));
        if !given.is_some_and(|t| auth.matches(t)) {
            return page(StatusCode::UNAUTHORIZED, "トークンが違います。");
        }
        let https = h.get("x-forwarded-proto").is_some_and(|v| v == "https");
        let cookie = format!("{COOKIE}={}; Path=/; Max-Age=315360000; HttpOnly; SameSite=Lax{}", auth.token, if https { "; Secure" } else { "" });
        return (
            StatusCode::SEE_OTHER,
            [(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap()), (header::LOCATION, HeaderValue::from_static("/"))],
        )
            .into_response();
    }
    if local(peer, h) || auth.signed_in(h) {
        return next.run(req).await;
    }
    page(StatusCode::UNAUTHORIZED, "このサーバーは、ほかの端末からはトークンが要ります。")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use tower::ServiceExt;

    fn app(auth: Arc<Auth>) -> Router {
        Router::new().route("/x", get(|| async { "ok" }).put(|| async { "ok" })).layer(axum::middleware::from_fn_with_state(auth, guard))
    }

    async fn status(peer: &str, req: axum::http::request::Builder) -> (StatusCode, HeaderMap) {
        let mut r = req.body(Body::empty()).unwrap();
        r.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        let res = app(Auth::with("0123456789abcdef0123456789abcdef")).oneshot(r).await.unwrap();
        (res.status(), res.headers().clone())
    }

    fn get_(host: &str, uri: &str) -> axum::http::request::Builder {
        Request::builder().uri(uri).header(header::HOST, host)
    }

    #[tokio::test]
    async fn this_machine_needs_nothing() {
        for host in ["localhost:8656", "127.0.0.1:8656", "[::1]:8656", "app.localhost", "127.0.0.1"] {
            assert_eq!(status("127.0.0.1:5000", get_(host, "/x")).await.0, StatusCode::OK, "{host}");
        }
        assert_eq!(status("[::1]:5000", get_("[::1]:8656", "/x")).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn others_need_the_token() {
        const T: &str = "0123456789abcdef0123456789abcdef";
        // another device
        assert_eq!(status("100.64.0.2:5000", get_("100.64.0.1:8656", "/x")).await.0, StatusCode::UNAUTHORIZED);
        // a proxy on this machine (tailscale serve, nginx, a tunnel)
        for fwd in ["x-forwarded-for", "forwarded", "x-real-ip", "x-forwarded-host", "tailscale-user-login"] {
            let r = get_("127.0.0.1:8656", "/x").header(fwd, "100.64.0.2");
            assert_eq!(status("127.0.0.1:5000", r).await.0, StatusCode::UNAUTHORIZED, "{fwd}");
        }
        // DNS rebinding: a name of an attacker's resolving to 127.0.0.1
        assert_eq!(status("127.0.0.1:5000", get_("evil.example:8656", "/x")).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(status("127.0.0.1:5000", get_("localhost.evil.example", "/x")).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(status("127.0.0.1:5000", Request::builder().uri("/x")).await.0, StatusCode::UNAUTHORIZED, "no Host");
        // with it
        let dev = || get_("box.example.ts.net", "/x").header("x-forwarded-for", "100.64.0.2");
        assert_eq!(status("127.0.0.1:5000", dev().header(header::COOKIE, format!("a=b; {COOKIE}={T}"))).await.0, StatusCode::OK);
        assert_eq!(status("127.0.0.1:5000", dev().header(header::AUTHORIZATION, format!("Bearer {T}"))).await.0, StatusCode::OK);
        assert_eq!(status("127.0.0.1:5000", dev().method("PUT").header(header::AUTHORIZATION, format!("Bearer {T}"))).await.0, StatusCode::OK);
        // a wrong one, a prefix of it, an empty one
        for bad in ["0123456789abcdef0123456789abcdee", "0123456789abcdef", ""] {
            assert_eq!(status("127.0.0.1:5000", dev().header(header::COOKIE, format!("{COOKIE}={bad}"))).await.0, StatusCode::UNAUTHORIZED, "{bad}");
            assert_eq!(status("127.0.0.1:5000", dev().header(header::AUTHORIZATION, format!("Bearer {bad}"))).await.0, StatusCode::UNAUTHORIZED);
        }
        // a cookie of another name with the token's value as part of it
        assert_eq!(status("127.0.0.1:5000", dev().header(header::COOKIE, format!("x{COOKIE}={T}"))).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_sets_the_cookie() {
        const T: &str = "0123456789abcdef0123456789abcdef";
        let r = get_("box.example.ts.net", &format!("/login?token={T}")).header("x-forwarded-for", "1.2.3.4").header("x-forwarded-proto", "https");
        let (s, h) = status("127.0.0.1:5000", r).await;
        assert_eq!(s, StatusCode::SEE_OTHER);
        let c = h[header::SET_COOKIE].to_str().unwrap();
        assert!(c.starts_with(&format!("{COOKIE}={T};")) && c.contains("HttpOnly") && c.contains("SameSite=Lax") && c.contains("Secure"), "{c}");
        let (s, h) = status("100.64.0.2:5000", get_("100.64.0.1:8656", "/login?token=nope")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(!h.contains_key(header::SET_COOKIE));
        assert_eq!(status("100.64.0.2:5000", get_("100.64.0.1:8656", "/login")).await.0, StatusCode::UNAUTHORIZED);
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
