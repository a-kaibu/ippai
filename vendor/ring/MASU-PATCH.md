Upstream: ring 0.17.14, crates.io release (ISC license; see LICENSE).

The functional source change adds target_os = "emscripten" to SystemRandom's
getrandom-backed implementation in src/rand.rs. getrandom 0.2 already supports
Emscripten through getentropy, which uses Web Crypto in the browser. No
cryptographic algorithm, seed source, certificate policy or verification result
is changed. Rustls's ring provider otherwise fails to compile for Emscripten
because ring does not implement SecureRandom for SystemRandom on that target.

Remove this vendored patch once upstream ring supports Emscripten's existing
getentropy implementation; verify both Wasm TLS tests and masu server tests
before removing it. Native targets retain the upstream cfg and behavior.

The crate also allows upstream warnings, matching Cargo's registry-dependency
lint cap; sake builds its own code with -D warnings, and a path override otherwise
turns upstream platform-specific unused-code warnings into build errors.
