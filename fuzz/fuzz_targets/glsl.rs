//! glwgsl (the OpenGL path's GLSL -> WGSL, preprocessor first) on any text: no panic, whatever
//! the directives, macros and declarations. The input is split at its first NUL into a vertex
//! and a fragment shader; each is also checked on its own. When linking succeeds, the WGSL of
//! both stages declares a uniform block only together with its members.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let (vs, ps) = s.split_once('\0').unwrap_or((&s, "void main() { gl_FragColor = vec4(1.0); }"));
    let _ = glwgsl::check(vs, false);
    let _ = glwgsl::check(ps, true);
    if let Ok(p) = glwgsl::link(vs, ps, &HashMap::new()) {
        for st in [&p.vs, &p.ps] {
            if let Some(b) = &st.block {
                assert!(!b.members.is_empty() && b.size >= b.members.iter().map(|m| m.offset).max().unwrap_or(0));
            }
        }
    }
});
