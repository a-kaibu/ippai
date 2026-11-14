Upstream: naga 30.0.1, crates.io release (MIT OR Apache-2.0; see LICENSE.MIT, LICENSE.APACHE).

Why vendored: sake is built with `-C panic=abort` (crates/sake/build.sh), so a panic in naga's
GLSL front end on a game's shader ends the game; `catch_unwind` in glwgsl could never catch it.
The panics that game shaders can reach are fixed here instead, found by fuzzing glwgsl
(fuzz/fuzz_targets/glsl.rs); each fix has a regression test in crates/glwgsl
(`tests::malformed_sources_are_errors`) and its input under fuzz/regressions/glsl/.

Changes (search for "masu:"):
- src/front/glsl/context.rs, `lower_inner` / `HirExprKind::Access`: in a constant expression
  (an array size such as `float a[2[2]]`) the index expression lives in the module's global
  expressions, but its constant value was looked up in the function's arena: index out of
  bounds. It now reads the arena the expression was added to.
- src/lib.rs: `#![allow(warnings)]`, matching Cargo's lint cap for registry dependencies (a path
  override otherwise turns upstream warnings into errors under sake's `-D warnings`).

To drop this copy: check the fix is upstream, remove `naga` from [patch.crates-io] in the root
Cargo.toml and fuzz/Cargo.toml, and run `cargo test -p glwgsl` and the glsl fuzz target.
