# SDRT 5.0.9.0 compatibility

Status: playable in Chromium with a purchased title (MAJOKOI_DL). The official
runtime authorizes the title against DMM, keeps that state across browser
restarts, and the game's own save/load works across a page reload. Real Android
devices, the official x64 runtime and fresh-prefix rejection without a license
are still unverified (see "Open decisions and completion evidence").

## Verified inputs

The [official download page](https://www.paltio.co.jp/home_contents/paltiosoft/userpages/basicforuse/download/)
links to `https://acf.paltio.co.jp/sdrt/sdrt5090_win32.exe`.
The installer inspected on 2026-11-08 has SHA-256
`f44b9fb5dba10db4cd97187834ee8bf5fc6b21e3c51051324d489d83154299f0`.
Its InstallShield payload contains `sdrt50_win32.msi`, SHA-256
`42dfde15738989b0798cdba15abf82446ff754faa8ebea399282553c815dcaf7`.
Keep these proprietary files outside Git; acquire them from the vendor.

The MSI installs UCUtil32/38/41/50.dll in SystemFolder, and UCManSvc.exe,
UCManSvcps.dll, UCX.ocx, SatX4.ocx and SdMeter.exe under SoftDenchi.
Its ServiceInstall table registers UCManSvc as type 272 (0x110), automatic start.
The Registry table associates AppID
`{EEF76D48-0BA0-4FF1-B619-297A89F1E776}` with LocalService `UCManSvc`, and
ServiceParameters `-Service`. Thus copying its DLLs alone is insufficient.
The installer also bundles the official x86 and x64 VC 2019 redistributables.
UCUtil50.dll imports MFC140; use the corresponding official DLL.

## Approach and alternatives

The required user experience is an Android browser with no locally installed
companion program. The earlier localhost Node relay is a development fixture,
not an accepted production prerequisite. The user authorized an opaque WebSocket/TCP relay in `masu serve`; its design
and tested protocol are documented in [tls-relay.md](tls-relay.md). Upstream TLS
must be verified in browser/Wasm, so the relay does not terminate the connection
or receive plaintext account credentials. The Wasm client uses rustls and the opaque relay; official HTTPS exchanges have been verified in Chromium.

The official satxControler endpoint answered a credential-free HEAD request on
2026-11-08 without Access-Control-Allow-Origin. Ordinary cross-origin browser
fetch cannot expose its response to the runtime. `no-cors` and service workers
do not grant access to opaque response bodies. Therefore direct browser
transport is not established. Vendor CORS support or an agreed server-side
transport is necessary; this cannot be fixed by returning an invented success.


Run the original game, vendor DLLs and service as guest PE code. Implement their
Windows API dependencies in sake. Authorization decisions, signature contents,
license expiration and battery accounting remain in the vendor code.

A native Windows host running the official runtime is an alternative, but needs
Windows and remote execution; it does not satisfy masu's browser execution
model. Replacing the vendor's authorization results would not satisfy the
requested legitimate-runtime behavior and is not part of this implementation.

The selected approach preserves that execution model at the cost of implementing
service management, COM activation/marshalling and persistent cryptographic
providers. Implement those as general Windows facilities, not title-specific
success responses. Unsupported operations must not produce invented keys,
certificates, licenses or successful verification results.

## Implemented prerequisites

* Load third-party DLLs installed in system32 / SysWOW64, including absolute
  paths. Known system DLLs in those directories continue using sake's HLE.
* Read version resources from unloaded on-disk PE32 / PE32+ files without
  loading their DLLs or running initialization code.
* CryptoAPI RSA PUBLICKEYBLOB import/export and key parameters.
* CryptVerifySignatureA/W using real PKCS#1 v1.5 verification, CryptoAPI's
  little-endian signature ordering and DigestInfo checking. CRYPT_NOHASHOID
  explicitly selects unprefixed verification; other flags fail.
* Hash finalization when verifying; malformed blobs, altered data, truncated
  or modified signatures, and destroyed keys are rejected.

`host/tests/cryptoapi.c` uses a fixed OpenSSL-generated public key and signature,
independent of sake's signature generation. `host/tests/dllsearch.c` loads a
separately compiled DLL installed in a fresh test prefix. Both are in the default
`tests/host-tests.sh` set, with x64 builds as well. `sysdlls` checks that existing
system DLL behavior remains intact.

## Service, COM and provider foundations

SCM records and state live in the shared guest kernel. StartService launches the
configured PE, waits for dispatcher connection, and runs the real ServiceMain.
Status changes and controls are delivered to that service. LocalService COM
activation uses its registered factory and the official proxy DLL's MIDL
metadata; supported arguments cross process boundaries and vendor code supplies
the result. The broker currently supports scalar values, fixed structures,
BSTR (including binary and null values), and correlated arrays. Unsupported
metadata fails explicitly. This is a subset of Windows COM/RPC.

CryptoAPI contexts distinguish provider, user/machine container, create/open/
delete and ephemeral use. RSA keys are generated, persisted atomically, reopened
by another guest process, and used for actual signatures. Export permission is
enforced. Generation tokens prevent stale handles writing into a recreated
container. Registry updates are shared-kernel transactions with atomic file
replacement, so service/helper processes do not overwrite one another's state.

ImageHlp reads the original certificate records from the PE security directory.
ImageGetDigestStream supports digest level 6, providing normalized headers and
section bytes to the application's real hash callback; other digest levels
currently fail with ERROR_NOT_SUPPORTED. This API supplies bytes, not a trust
or authorization result. Certificate-chain trust and additional CryptoAPI
operations remain incomplete.

The regression probes cover service callbacks, automatic COM activation,
parallel calls, BSTR preservation, error results, actual RSA signing and
cross-process key reopening, certificate access, and concurrent registry writes.
Run with `tests/host-tests.sh registrymulti imagecert capiprovider cryptoapi
comrpc services dllsearch recalloc sysdlls` using a freshly built Node target.
The runner exercises x86 JIT/interpreter and x64 JIT.

## Actual title and outstanding verification

The user-provided `spal_0079.zip` was downloaded completely (2,884,863,005 bytes)
and extracted under Git-ignored `target/sdrt-fixtures/`. Its executable is
`MAJOKOI_DL/MAJOKOI_.exe`. The version-resource check now succeeds. The game
starts the official UCManSvc process and calls its real IUCBat UM_Initialize40
method (slot 40) through CLSID `{973558CA-B388-11D3-AEF6-0040264002C6}` / IID
`{973558C7-B388-11D3-AEF6-0040264002C6}`. The level-6 image digest implementation now verifies the original vendor
signature and rejects a separately copied executable with one modified code
byte. Real in-process factory loading and OLE font support let the original
SatX4 control display its DMM login dialog. In the browser, account settings
reach the original form unchanged (checked by in-memory comparisons, without
logging values). The official DMM endpoint returns HTTP 200 with a 22-byte
response beginning `0:`; the remainder is sensitive and must not be logged.
The charging stage originally failed with error 1 because the MFC conversion
path supplied empty BSTR arguments. Its ANSI-to-wide conversion requests one
fewer character than the queried size including NUL, then supplies its own
terminator. MultiByteToWideChar now preserves the converted prefix on an
insufficient buffer while returning zero and ERROR_INSUFFICIENT_BUFFER. The
runtime's `%Ts`/`%Tc` formatter, Netbios adapter queries, and `_makepath_s` paths
are implemented and covered by regression tests. Authorization decisions stay
inside the original vendor code.

The original runtime now completes its charging exchanges with the official
server. Reopening the protected launcher without account input reaches the
actual game executable and Vorbis initialization. Browser child processes share
the live VFS, including the executable created by the official launcher; no
pre-decrypted executable is distributed or added to the streamed library.

The browser stores installation changes in IndexedDB `masu-installation`,
AES-GCM encrypted with a non-extractable CryptoKey. Nothing in it is specific to
SDRT: it is this device's machine state, what the prefix keeps outside the user
folders (C:\ProgramData, C:\windows, C:\Program Files) plus the CryptoAPI key
containers (also those under the user folder), with executables, DLLs, OCXs and
temporary folders left out. Everything else under C:\users, the registry file
included, stays each game's own save as before 8ab1b50, so the two never hold one
path and neither can override the other (web/src/file-records.ts, tested by
file-records.test.ts). A registry that 8ab1b50-83ea93a kept only in the
installation record is kept aside and given to any game whose saves have none.
One tab writes the installation at a time; the lease is taken when a tab first has
something to write, so games that never write machine state run in any number of
tabs. This is same-origin browser storage, not protection against compromised
same-origin scripts.

A run becomes sensitive when the guest takes the account input or opens a
persisted CryptoAPI key container (host/account-bridge.js `sake_mark_sensitive`);
from then on it exports no crash logs and no memory states. Memory states are also
off once a second process runs (a state holds one process). Saving an account in
the settings or having key containers installed does not by itself affect other
games. When the DMM dialog's layout matches but the runtime files are not the
verified 5.0.9.0 build, nothing is filled and the page says to type the account in.

A Chromium test confirmed an encrypted installation record with a
non-extractable key. First launch opened one DMM authentication connection;
reloading the page reached the game initialization with zero new DMM connections.
This verifies local installation continuity, not that the game can be played.

Windows comparison isolated another startup blocker: a captioned hidden window
receives posted WM_DWMNCRENDERINGCHANGED (enabled), which satisfies the input part
of the game's wait for its audio initialization event. The same public Win32
probe ran on Windows and sake; see
[Windows reference run](https://github.com/a-kaibu/masu/actions/runs/42493996944).
The window creation notification and fractional-millisecond wait rounding now
match that regression fixture. The game subsequently creates and shows its own
window and sends Direct3D frames.

Verified in Chromium on 2026-11-09 with the purchased MAJOKOI_DL: the title
screen renders correctly; the prologue and the first voiced story scenes play
with Japanese text, voice and scene changes; the in-game System/Save/Load screens
work; a slot saved in the story survives a page reload and loads back to the same
line, and the story continues from there. A phone-sized touch viewport also
reached the title and prologue. Input sent while a loaded scene is still
streaming its voice is ignored by the game itself; later input advances normally.

The game's shipped `save/config.dat` places its 1280x720 window at (160,90), so
on kura's default 1366x768 screen the window extends past the right and bottom
edges, as it would on Windows. Add the title with
`--env MASU_SCREEN=1920x1080` (any screen of at least about 1460x850 fits the
window and its frame) to see the whole window.

Other observed gaps include _mbsnbcpy_s, COM security, event logging,
and MFC42 ordinal 1200. Their presence in an import/trace does not establish
that each causes the current rendering failure. The official x64 runtime and
cross-architecture interactions are untested.

## Open decisions and completion evidence

Provider keys and registry changes belong to the browser installation, with
local encryption and portable-export exclusions described above. A library
export or S3 save backup contains game saves rather than an activated copy of
the installation. Cross-device restoration of licenses is not implemented.

The library settings now supply account input to the original DMM dialog.
Email is stored in a separate browser IndexedDB database; passwords stay in
page memory unless the user explicitly selects encrypted persistence (AES-GCM
with a non-exportable CryptoKey in the same browser). This does not isolate
credentials from scripts of the app origin. Nothing is included in portable
game-save exports or sent to a server for storage.

An atomic, one-shot SharedArrayBuffer carries input to the guest UI thread.
The receiver checks the DMM dialog layout and SHA-256 of the original SDRT
5.0.9.0 UCOpgDlgRes.dll, UCOpgDlg.dll and SatX4.ocx before taking it. It uses
WM_SETTEXT and queues the original Submit button; the vendor remember checkbox
is left off. Shared and temporary input buffers are erased after use. ES_PASSWORD
controls now draw masked characters. No licensing decision is replaced.

Sensitive runs disable crash-log export and memory save states because the
original runtime can retain plaintext in its guest heap/stack. Ordinary game
saves are separate. Shared VFS visibility, encrypted browser restart
persistence, visible story playback and normal game save/load across a page
reload have been verified with the actual protected launcher.

Never embed credentials, serials or signed game-download URLs in source or trace logs.
Once dependencies are implemented, verify on a fresh prefix: official runtime
installation, rejection without a license, purchased-license activation, game
startup, actual play, save/load, browser restart and continued authorized play.
Test expiration/revocation only with legitimate test licenses or vendor-provided
fixtures. Report unsupported conditions as such; never interpret a hang, a
fallback message loop or a visible first window as completed compatibility.
