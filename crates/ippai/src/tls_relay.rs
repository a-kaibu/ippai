//! Authenticated, same-origin WebSocket-to-TCP transport. TLS is implemented
//! and verified by the browser/Wasm client, not terminated by this server.
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
#[cfg(test)]
use std::net::SocketAddr;
use std::{net::IpAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{timeout, Instant},
};

const PROTOCOL: &str = "ippai.tls.v1";
const MESSAGE_MAX: usize = 64 * 1024;
const TRAFFIC_MAX: usize = 16 * 1024 * 1024;
const IDLE: Duration = Duration::from_secs(60);
const LIFETIME: Duration = Duration::from_secs(300);
#[derive(Clone, Copy)]
struct Limits {
    idle: Duration,
    lifetime: Duration,
    traffic: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            idle: IDLE,
            lifetime: LIFETIME,
            traffic: TRAFFIC_MAX,
        }
    }
}
#[derive(Clone)]
struct Relay {
    slots: Arc<Semaphore>,
    limits: Limits,
    #[cfg(test)]
    upstream: Option<SocketAddr>,
}
impl Default for Relay {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(8)),
            limits: Limits::default(),
            #[cfg(test)]
            upstream: None,
        }
    }
}
pub fn router() -> Router {
    routes(Relay::default())
}
fn routes(relay: Relay) -> Router {
    Router::new()
        .route("/net/tls/{target}", get(upgrade))
        .with_state(relay)
}
fn destination(target: &str) -> Option<(&'static str, u16)> {
    match target {
        "soft-denchi" => Some(("charge1.soft-denchi.jp", 443)),
        "dmm-softdenchi" => Some(("dlsoft.dmm.co.jp", 443)),
        "dmm-doujin-softdenchi" => Some(("softdenchi-doujin-api.dmm.co.jp", 443)),
        _ => None,
    }
}
/// Browser WebSockets carry cookies on GET; the ordinary write guard is not
/// enough. Check the Origin even for locally trusted ippai users. The Origin may match the Host
/// or, behind a reverse proxy, X-Forwarded-Host: a page cannot set either header on a
/// WebSocket, and a non-browser client could forge the Origin anyway (the auth guard is what
/// stops those).
fn same_origin(headers: &HeaderMap) -> bool {
    if !headers
        .get("sec-fetch-site")
        .is_none_or(|v| v == "same-origin" || v == "none")
    {
        return false;
    }
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<Uri>().ok())
    else {
        return false;
    };
    let Some(scheme @ ("http" | "https")) = origin.scheme_str() else {
        return false;
    };
    if origin.query().is_some() || origin.path() != "/" && !origin.path().is_empty() {
        return false;
    }
    let Some(authority) = origin.authority() else {
        return false;
    };
    let normalize = |s: &str| -> Option<(String, u16)> {
        let a = s.parse::<axum::http::uri::Authority>().ok()?;
        if a.as_str().contains('@') || a.port().is_some() && a.port_u16().is_none() {
            return None;
        }
        Some((
            a.host().to_ascii_lowercase(),
            a.port_u16()
                .unwrap_or(if scheme == "https" { 443 } else { 80 }),
        ))
    };
    let Some(from) = normalize(authority.as_str()) else {
        return false;
    };
    ["host", "x-forwarded-host"].iter().any(|key| {
        headers
            .get(*key)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| normalize(s.split(',').next()?.trim()))
            .is_some_and(|to| to == from)
    })
}
/// Reject local, reserved and transition addresses. Resolve once and connect
/// to the checked SocketAddr; TcpStream must not perform a second DNS lookup.
fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || a == 100 && (64..=127).contains(&b)
                || a == 169 && b == 254
                || a == 172 && (16..=31).contains(&b)
                || a == 192 && (b == 168 || b == 0 && (c == 0 || c == 2) || b == 88 && c == 99)
                || a == 198 && (b == 18 || b == 19 || b == 51 && c == 100)
                || a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Only ordinary global unicast; exclude mapped/NAT64, Teredo,
            // 6to4 and IETF special/documentation ranges.
            s[0] & 0xe000 == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
async fn connect(host: &str, port: u16) -> Result<TcpStream, StatusCode> {
    let addresses: Vec<_> = timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
    .map_err(|_| StatusCode::BAD_GATEWAY)?
    .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !public_address(a.ip())) {
        return Err(StatusCode::BAD_GATEWAY);
    }
    timeout(Duration::from_secs(10), async {
        for address in addresses {
            if let Ok(tcp) = TcpStream::connect(address).await {
                return Ok(tcp);
            }
        }
        Err(StatusCode::BAD_GATEWAY)
    })
    .await
    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
}
async fn upgrade(
    State(relay): State<Relay>,
    Path(target): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some((host, port)) = destination(&target) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let protocols = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !protocols.split(',').any(|p| p.trim() == PROTOCOL) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(permit) = relay.slots.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    #[cfg(test)]
    let connected = match relay.upstream {
        Some(address) => TcpStream::connect(address)
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY),
        None => connect(host, port).await,
    };
    #[cfg(not(test))]
    let connected = connect(host, port).await;
    let tcp = match connected {
        Ok(tcp) => tcp,
        Err(status) => return status.into_response(),
    };
    let _ = tcp.set_nodelay(true);
    let mut response = ws
        .protocols([PROTOCOL])
        .max_message_size(MESSAGE_MAX)
        .max_frame_size(MESSAGE_MAX)
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * MESSAGE_MAX)
        .on_upgrade(move |ws| {
            tunnel(
                ws,
                tcp,
                permit,
                relay.limits.idle,
                relay.limits.lifetime,
                relay.limits.traffic,
            )
        });
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn tunnel(
    mut ws: WebSocket,
    tcp: TcpStream,
    _permit: OwnedSemaphorePermit,
    idle: Duration,
    lifetime: Duration,
    traffic_max: usize,
) {
    let (mut read, mut write) = tcp.into_split();
    let mut buffer = vec![0; 16 * 1024];
    let (mut uploaded, mut downloaded) = (0usize, 0usize);
    let deadline = Instant::now() + lifetime;
    let mut last_progress = Instant::now();
    loop {
        let until = deadline.min(last_progress + idle);
        enum Event {
            Client(Option<Result<Message, axum::Error>>),
            Upstream(std::io::Result<usize>),
        }
        let event = timeout(until.saturating_duration_since(Instant::now()), async {
            tokio::select! { message=ws.recv()=>Event::Client(message), n=read.read(&mut buffer)=>Event::Upstream(n) }
        }).await;
        let remaining = until.saturating_duration_since(Instant::now());
        match event {
            Ok(Event::Client(Some(Ok(Message::Binary(bytes))))) => {
                if bytes.is_empty() {
                    continue;
                }
                if bytes.len() > traffic_max.saturating_sub(uploaded) {
                    break;
                }
                if !matches!(
                    timeout(remaining, write.write_all(&bytes)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
                uploaded += bytes.len();
                last_progress = Instant::now();
            }
            Ok(Event::Client(Some(Ok(Message::Ping(_) | Message::Pong(_))))) => {}
            Ok(Event::Upstream(Ok(n))) if n > 0 => {
                if n > traffic_max.saturating_sub(downloaded) {
                    break;
                }
                if !matches!(
                    timeout(
                        remaining,
                        ws.send(Message::Binary(buffer[..n].to_vec().into()))
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
                downloaded += n;
                last_progress = Instant::now();
            }
            _ => break,
        }
    }
    // Dropping both halves closes TCP; do not wait for a peer's close handshake
    // or retry/replay any of the bytes already forwarded.
    let _ = timeout(Duration::from_secs(1), ws.send(Message::Close(None))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{self, client::IntoClientRequest},
    };
    type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;
    struct Running {
        address: SocketAddr,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Running {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn run(app: Router) -> Running {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap()
        });
        Running { address, task }
    }
    fn request(
        address: SocketAddr,
        path: &str,
        origin: Option<&str>,
        protocol: bool,
    ) -> axum::http::Request<()> {
        let mut r = format!("ws://{address}{path}")
            .into_client_request()
            .unwrap();
        if let Some(origin) = origin {
            r.headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
        }
        if protocol {
            r.headers_mut()
                .insert(header::SEC_WEBSOCKET_PROTOCOL, PROTOCOL.parse().unwrap());
        }
        r
    }
    async fn client(server: &Running) -> Client {
        let origin = format!("http://{}", server.address);
        let (client, response) = connect_async(request(
            server.address,
            "/net/tls/soft-denchi",
            Some(&origin),
            true,
        ))
        .await
        .unwrap();
        assert_eq!(response.headers()[header::SEC_WEBSOCKET_PROTOCOL], PROTOCOL);
        client
    }
    async fn rejected(r: axum::http::Request<()>, status: StatusCode) {
        match connect_async(r).await {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), status),
            other => panic!("unexpected upgrade result: {other:?}"),
        }
    }
    #[test]
    fn routes_match_the_wasm_transport_and_the_bridge() {
        // (sake picks a target number from ippai_tls::DESTINATIONS; host/tls-bridge.js turns it
        // into this route; each must reach the host the TLS client verifies)
        for (route, host) in ippai_tls::DESTINATIONS {
            assert_eq!(destination(route), Some((host, 443)));
        }
        let bridge = include_str!("../../../host/tls-bridge.js");
        let names: Vec<String> = ippai_tls::DESTINATIONS.iter().map(|d| format!("'{}'", d.0)).collect();
        assert!(bridge.contains(&format!("[{}][target]", names.join(","))), "host/tls-bridge.js routes differ from ippai_tls::DESTINATIONS");
    }
    #[test]
    fn destination_and_public_addresses_are_bounded() {
        assert_eq!(
            destination("soft-denchi"),
            Some(("charge1.soft-denchi.jp", 443))
        );
        assert_eq!(destination("dmm-softdenchi"), Some(("dlsoft.dmm.co.jp", 443)));
        assert_eq!(destination("dmm-doujin-softdenchi"), Some(("softdenchi-doujin-api.dmm.co.jp", 443)));
        for name in [
            "example.com",
            "127.0.0.1:443",
            "soft-denchi/../x",
            "SOFT-DENCHI",
            "",
        ] {
            assert!(destination(name).is_none());
        }
        for ip in [
            "0.0.0.0",
            "10.1.1.1",
            "127.0.0.1",
            "100.64.1.1",
            "169.254.169.254",
            "172.31.0.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.19.1.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "3fff::1",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_address(ip.parse().unwrap()), "{ip}");
        }
    }
    #[test]
    fn origins_require_the_app_authority_and_fetch_site() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "app.example:443".parse().unwrap());
        h.insert(header::ORIGIN, "https://app.example".parse().unwrap());
        assert!(same_origin(&h));
        h.insert("sec-fetch-site", "same-site".parse().unwrap());
        assert!(!same_origin(&h));
        h.remove("sec-fetch-site");
        for value in [
            "null",
            "https://game.example",
            "https://app.example:444",
            "https://app.example/path",
            "https://app.example/?query=x",
            "file://app.example",
            "https://evil@app.example",
        ] {
            h.insert(header::ORIGIN, value.parse().unwrap());
            assert!(!same_origin(&h), "{value}");
        }
        h.remove(header::ORIGIN);
        assert!(!same_origin(&h));
    }
    #[tokio::test]
    async fn upgrades_reject_foreign_origin_protocol_and_destination_before_connecting() {
        let server = run(router()).await;
        let origin = format!("http://{}", server.address);
        rejected(
            request(
                server.address,
                "/net/tls/soft-denchi",
                Some("https://foreign.invalid"),
                true,
            ),
            StatusCode::FORBIDDEN,
        )
        .await;
        rejected(
            request(server.address, "/net/tls/soft-denchi", None, true),
            StatusCode::FORBIDDEN,
        )
        .await;
        rejected(
            request(
                server.address,
                "/net/tls/arbitrary-host",
                Some(&origin),
                true,
            ),
            StatusCode::NOT_FOUND,
        )
        .await;
        rejected(
            request(server.address, "/net/tls/soft-denchi", Some(&origin), false),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    #[tokio::test]
    async fn existing_auth_guard_protects_the_upgrade() {
        let dir = std::env::temp_dir().join(format!("ippai-relay-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = crate::auth::Auth::load(&dir.join("token"), true).unwrap();
        let app = router().layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::guard,
        ));
        let server = run(app).await;
        rejected(
            request(
                server.address,
                "/net/tls/soft-denchi",
                Some(&format!("http://{}", server.address)),
                true,
            ),
            StatusCode::UNAUTHORIZED,
        )
        .await;
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn tunnel_preserves_binary_stream_both_ways_and_releases_slot_on_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = listener.local_addr().unwrap();
        let relay = Relay {
            slots: Arc::new(Semaphore::new(1)),
            upstream: Some(upstream),
            ..Default::default()
        };
        let slots = relay.slots.clone();
        let server = run(routes(relay)).await;
        let mut ws = client(&server).await;
        let (mut tcp, _) = listener.accept().await.unwrap();
        assert_eq!(slots.available_permits(), 0);
        let origin = format!("http://{}", server.address);
        rejected(
            request(server.address, "/net/tls/soft-denchi", Some(&origin), true),
            StatusCode::TOO_MANY_REQUESTS,
        )
        .await;
        ws.send(tungstenite::Message::Binary(vec![0, 255, 1].into()))
            .await
            .unwrap();
        ws.send(tungstenite::Message::Binary(vec![2, 3].into()))
            .await
            .unwrap();
        let mut bytes = [0; 5];
        tcp.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, [0, 255, 1, 2, 3]);
        tcp.write_all(&[255, 0, 128, 9]).await.unwrap();
        let mut got = vec![];
        while got.len() < 4 {
            if let Some(Ok(tungstenite::Message::Binary(b))) = ws.next().await {
                got.extend(b)
            } else {
                panic!("closed early")
            }
        }
        assert_eq!(got, [255, 0, 128, 9]);
        ws.close(None).await.unwrap();
        let n = timeout(Duration::from_secs(2), tcp.read(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 0);
        timeout(Duration::from_secs(2), async {
            while slots.available_permits() == 0 {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn upstream_eof_and_text_frames_close_the_tunnel() {
        for text in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let relay = Relay {
                upstream: Some(listener.local_addr().unwrap()),
                ..Default::default()
            };
            let server = run(routes(relay)).await;
            let mut ws = client(&server).await;
            let (tcp, _) = listener.accept().await.unwrap();
            if text {
                ws.send(tungstenite::Message::Text("not binary".into()))
                    .await
                    .unwrap();
            } else {
                drop(tcp);
            }
            assert!(matches!(
                timeout(Duration::from_secs(2), ws.next()).await.unwrap(),
                Some(Ok(tungstenite::Message::Close(_))) | None
            ));
        }
    }

    #[tokio::test]
    async fn idle_lifetime_and_traffic_limits_close_connections_and_release_slots() {
        for mode in 0..4 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let limits = match mode {
                0 => Limits {
                    idle: Duration::from_millis(100),
                    ..Default::default()
                },
                1 => Limits {
                    lifetime: Duration::from_millis(100),
                    ..Default::default()
                },
                _ => Limits {
                    traffic: 32,
                    ..Default::default()
                },
            };
            let relay = Relay {
                slots: Arc::new(Semaphore::new(1)),
                upstream: Some(listener.local_addr().unwrap()),
                limits,
            };
            let slots = relay.slots.clone();
            let server = run(routes(relay)).await;
            let mut ws = client(&server).await;
            let (mut tcp, _) = listener.accept().await.unwrap();
            if mode == 2 {
                ws.send(tungstenite::Message::Binary(vec![7; 33].into()))
                    .await
                    .unwrap();
                let mut data = vec![];
                timeout(Duration::from_secs(2), tcp.read_to_end(&mut data))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(data.is_empty());
            } else if mode == 3 {
                tcp.write_all(&[7; 33]).await.unwrap();
            }
            let mut received = 0;
            timeout(Duration::from_secs(2), async {
                loop {
                    match ws.next().await {
                        Some(Ok(tungstenite::Message::Binary(b))) => received += b.len(),
                        Some(Ok(tungstenite::Message::Close(_))) | None => break,
                        Some(Err(_)) => break,
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            assert!(received <= 32);
            timeout(Duration::from_secs(2), async {
                while slots.available_permits() == 0 {
                    tokio::task::yield_now().await
                }
            })
            .await
            .unwrap();
        }
    }
    #[tokio::test]
    async fn failed_connect_returns_admission_slot() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let relay = Relay {
            slots: Arc::new(Semaphore::new(1)),
            upstream: Some(address),
            ..Default::default()
        };
        let slots = relay.slots.clone();
        let server = run(routes(relay)).await;
        rejected(
            request(
                server.address,
                "/net/tls/soft-denchi",
                Some(&format!("http://{}", server.address)),
                true,
            ),
            StatusCode::BAD_GATEWAY,
        )
        .await;
        assert_eq!(slots.available_permits(), 1);
    }
    async fn tls_upstream() -> (SocketAddr, tokio::task::JoinHandle<bool>) {
        use tokio_rustls::rustls::{
            self,
            pki_types::{CertificateDer, PrivatePkcs8KeyDer},
        };
        let provider = rustls::crypto::ring::default_provider();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(
                    include_bytes!("../testdata/relay-cert.der").to_vec(),
                )],
                PrivatePkcs8KeyDer::from(include_bytes!("../testdata/relay-key.der").to_vec())
                    .into(),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let Ok(mut tls) = acceptor.accept(tcp).await else {
                return false;
            };
            let mut message = [0; 21];
            if tls.read_exact(&mut message).await.is_err() {
                return false;
            }
            if &message != b"private-login-fixture" {
                return false;
            }
            tls.write_all(b"official-response-fixture").await.unwrap();
            let _ = tls.shutdown().await;
            true
        });
        (address, task)
    }
    async fn websocket_stream(
        mut ws: Client,
    ) -> (
        tokio::io::DuplexStream,
        Arc<std::sync::Mutex<Vec<u8>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client, mut bridge) = tokio::io::duplex(64 * 1024);
        let recorded = Arc::new(std::sync::Mutex::new(vec![]));
        let capture = recorded.clone();
        let task = tokio::spawn(async move {
            let mut buf = [0; 16384];
            loop {
                tokio::select! {
                    n=bridge.read(&mut buf)=>{let Ok(n)=n else{break};if n==0{break}capture.lock().unwrap().extend_from_slice(&buf[..n]);if ws.send(tungstenite::Message::Binary(buf[..n].to_vec().into())).await.is_err(){break}},
                    m=ws.next()=>{match m{Some(Ok(tungstenite::Message::Binary(b)))=>{if bridge.write_all(&b).await.is_err(){break}},Some(Ok(tungstenite::Message::Ping(_)|tungstenite::Message::Pong(_)))=>{},_=>break}}
                }
            }
        });
        (client, recorded, task)
    }
    #[tokio::test]
    async fn tls_stays_end_to_end_and_client_rejects_untrusted_certificates() {
        use tokio_rustls::rustls::{
            self,
            pki_types::{CertificateDer, ServerName},
        };
        for (trusted, name) in [
            (true, "localhost"),
            (false, "localhost"),
            (true, "wrong.invalid"),
        ] {
            let (upstream, task) = tls_upstream().await;
            let server = run(routes(Relay {
                upstream: Some(upstream),
                ..Default::default()
            }))
            .await;
            let (stream, recorded, bridge) = websocket_stream(client(&server).await).await;
            let mut roots = rustls::RootCertStore::empty();
            if trusted {
                roots
                    .add(CertificateDer::from(
                        include_bytes!("../testdata/relay-cert.der").to_vec(),
                    ))
                    .unwrap();
            }
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            let connected = timeout(
                Duration::from_secs(5),
                tokio_rustls::TlsConnector::from(Arc::new(config))
                    .connect(ServerName::try_from(name).unwrap(), stream),
            )
            .await
            .unwrap();
            if trusted && name == "localhost" {
                let mut tls = connected.unwrap();
                tls.write_all(b"private-login-fixture").await.unwrap();
                let mut response = [0; 25];
                tls.read_exact(&mut response).await.unwrap();
                assert_eq!(&response, b"official-response-fixture");
                assert!(task.await.unwrap());
                let ciphertext = recorded.lock().unwrap();
                assert!(ciphertext.len() > 21);
                assert!(!ciphertext
                    .windows(21)
                    .any(|w| w == b"private-login-fixture"));
            } else {
                assert!(connected.is_err());
                assert!(!task.await.unwrap());
            }
            bridge.abort();
        }
    }
}
