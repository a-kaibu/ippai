# SDRT 5.0.9.0 compatibility

Status: incomplete. Loading a runtime DLL or passing the host probes below is
not evidence that a licensed title can be played. Completion requires the
official runtime to authorize a purchased title and preserve its state across
browser restarts.

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
not an accepted production prerequisite. The user authorized an opaque WebSocket/TCP relay in `ippai serve`; its design
and tested protocol are documented in [tls-relay.md](tls-relay.md). Upstream TLS
must be verified in browser/Wasm, so the relay does not terminate the connection
or receive plaintext account credentials. The Wasm client is not integrated yet.

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
Windows and remote execution; it does not satisfy ippai's browser execution
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
SatX4 control display its DMM login dialog. This is not successful account
verification, activation or gameplay; those remain unverified.

Other observed gaps include Netbios, _mbsnbcpy_s, COM security, event logging,
and MFC42 ordinal 1200. Their presence in an import/trace does not establish
that each causes the current failure. The official x64 runtime and
cross-architecture interactions are untested.

## Open decisions and completion evidence

The provider currently stores private key material in the installation prefix.
Its protection and exclusion from portable library/save exports remain open. A
browser reload must preserve one installation's keys; a library export or a
cloned prefix must not silently become a second activated installation. Which
files may participate in portable saves and S3 backup remains undecided until
the vendor state can be identified. This blocks activation/storage integration,
but does not block independent API probes.

The user performs any required credential/serial entry in the runtime's UI;
never embed these values or signed game-download URLs in source or trace logs.
Once dependencies are implemented, verify on a fresh prefix: official runtime
installation, rejection without a license, purchased-license activation, game
startup, actual play, save/load, browser restart and continued authorized play.
Test expiration/revocation only with legitimate test licenses or vendor-provided
fixtures. Report unsupported conditions as such; never interpret a hang, a
fallback message loop or a visible first window as completed compatibility.
