# Browser TLS relay

The client requirement is an Android browser without an installed companion.
The user authorized adding encrypted transport to `ippai serve`. The original
vendor runtime remains responsible for authentication and licensing.

## Decision

Expose `GET /net/tls/soft-denchi` as a WebSocket on the authenticated app listener.
The client must select `ippai.tls.v1`. Binary messages carry a byte stream;
message boundaries have no meaning to TLS. The server connects only to
one of the fixed official destinations listed below, on port 443, and forwards bytes in both directions. DNS results
are checked before connecting to those exact resolved addresses. No HTTP proxy,
HTTP header rewriting, upstream TLS termination or licensing responses live in
the server. It cannot decrypt application data from a correctly verifying client.
Hostnames, traffic sizes and timings remain visible to the relay operator.

The browser/Wasm TLS client must independently verify the official hostname and
certificate chain. The server tunnel alone is not browser authentication support.
Neither a local companion nor server-side decryption is an accepted prerequisite.

A JSON HTTPS proxy was prototyped earlier but would expose account credentials
to its operator; it is retained only as a local development fixture. Direct Fetch
cannot read the current endpoint without vendor CORS support. A raw WebSocket
transport preserves browser-only use at the cost of implementing TLS in Wasm.

## Bounds and integration

Reuse ippai's existing auth guard. Reject foreign Origin and Sec-Fetch-Site even
though the upgrade uses GET. Add no route on the isolated web-game listener.
Allow at most eight active/pending tunnels per server, 64 KiB WebSocket messages,
16 MiB total traffic per direction, 60 seconds without progress, and five minutes
per tunnel. DNS/connect timeouts are bounded. Connections drop rather than retry
or replay authentication writes. The server does not persist or log payloads.

The fixed destinations are now:

| Route target | Official host (port 443) |
| --- | --- |
| `soft-denchi` | `charge1.soft-denchi.jp` |
| `dmm-softdenchi` | `dlsoft.dmm.co.jp` |
| `dmm-doujin-softdenchi` | `softdenchi-doujin-api.dmm.co.jp` |

The two DMM destinations are present in the original SDRT dialog modules (UCOpgDlg.dll and SatX4.ocx). The actual
DMM login uses a different destination from the Soft Denchi controller, so
the former single-destination transport rejected it before sending anything.
Wasm selects an integer target from this allowlist and verifies TLS for that
exact hostname. No browser-supplied hostname, port or URL is accepted by the server.

## Verification and remaining work

Nine `cargo test -p ippai tls_relay` tests cover origin and auth rejection, fixed
destination selection, public address checks, actual bidirectional WebSocket/TCP
bytes, close propagation, traffic/time limits and admission-slot release. A real
rustls client through the tunnel verifies a trusted test certificate, rejects an
untrusted chain or incorrect hostname, and sends a test secret that is absent from the recorded tunnel
bytes. This is a native client test, not evidence of Android/Wasm integration.
Browser/Wasm integration and actual licensed play remain separate completion
requirements.

## Client protocol

Open a WebSocket to `/net/tls/soft-denchi` on the app origin, using `wss:` when
the app uses HTTPS, and select subprotocol `ippai.tls.v1`. The normal ippai session
cookie authenticates the upgrade. Each binary message contains the next bytes
of the TLS stream; combine received messages before parsing TLS records. Text
messages are rejected. A successful upgrade means the upstream TCP connection
has opened. It does not mean a TLS handshake or authentication has succeeded.

The TLS client uses server name `charge1.soft-denchi.jp`, checks the certificate
chain and hostname, and constructs its own HTTP request inside TLS. Do not send
HTTP headers, account credentials or an HTTP request as a WebSocket text/JSON
message. A close, failed upgrade or timeout is an error, and must not replay a
POST automatically. The endpoint is not available on the separate web-game
origin. Deployments behind a reverse proxy must forward WebSocket upgrades.

## Wasm implementation checkpoint

`crates/tls-client` now supplies rustls certificate/hostname verification and
bounded HTTP/1.1 parsing. `host/tls-worker.js` / `host/tls-bridge.js` carry only
TLS records over WebSocket while the guest pthread waits. The app origin is
passed to each pthread by `host/pre.js`; WinINet uses this transport for the
fixed official HTTPS destination. The earlier plaintext JSON relay is no
longer linked into sake. ring's Emscripten RNG cfg fix is documented in
`vendor/ring/IPPAI-PATCH.md`.

This section described an earlier work-in-progress checkpoint. Licensed play
through this transport has since been verified in Chromium (docs/sdrt.md).
HTTP framing edge-case coverage and real Android devices remain unverified.

## Browser transport verification after the checkpoint

A real WinINet HEAD probe now completes in Chromium/Wasm through the ippai
WebSocket route, with rustls verification, and receives HTTP 200 from the
official controller. Node/Wasm does too. Creating a nested WebSocket worker
and immediately blocking its parent pthread prevented Chromium from starting
the nested worker. The page now creates a broker worker before guest threads
block; each pthread receives a broker MessagePort and each TLS exchange its
own channel. The broker still handles only ciphertext.

This probe verifies communication, not an account or a license. Actual title
authentication and subsequent play must be reported separately.

## Account integration verification

The actual MAJOKOI_DL title takes input saved in the React settings dialog,
starts its real service/helper processes in Chromium, and sends authentication
through the original runtime to the DMM endpoint. The earlier charge error 1 was
traced to a compatibility bug in ANSI-to-wide buffer handling. After the API,
formatter, Netbios and path corrections, the vendor runtime completes charging
exchanges with the official server and starts the real game executable.

The browser's encrypted installation state survives page reload: a test observed
one DMM connection on first activation and none on reload while game
initialization still proceeded. This verifies transport and installation
continuity. Correct visible gameplay and Android testing remain incomplete; see
[sdrt.md](sdrt.md) for current evidence and limitations. Credentials and response
bodies are not recorded in application logs or source.
