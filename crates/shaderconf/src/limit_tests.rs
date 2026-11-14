//! review b00: dxwgsl's limits on what hlslc's DXBC (as a game's D3DCompile would hand it)
//! can ask of the GPU, and signatures a damaged file gives.

/// two indexable arrays of 3000 vectors: each within one x#'s 4096, together past the
/// D3D11.3 total of 4096 vectors of temps; refused instead of 6000 vectors of private storage
/// per invocation. One array of 3000 still translates.
#[test]
fn indexable_temps_count_together() {
    let one = "float4 ps(float4 p : SV_Position, uint i : TEXCOORD0) : SV_Target { float4 a[3000]; for (uint k = 0; k < 3000; k++) a[k] = k; return a[i % 3000]; }";
    let two = "float4 ps(float4 p : SV_Position, uint i : TEXCOORD0) : SV_Target { float4 a[3000]; float4 b[3000]; for (uint k = 0; k < 3000; k++) { a[k] = k; b[k] = -k; } return a[i % 3000] + b[(i * 7) % 3000]; }";
    let code = |s: &str| hlslc::compile(s, &[], &mut |_, _| None, "ps", "ps_5_0", 0).unwrap();
    let w = dxwgsl::translate(&code(one), true, None).unwrap();
    crate::fuzz::valid(&w.code).unwrap();
    let Err(e) = dxwgsl::translate(&code(two), true, None) else { panic!("6000 vectors of temps translated") };
    assert!(e.contains("4096"), "{e}");
}

/// a point-size output whose OSGN mask a damaged container left at 0: no panic, the sprite
/// gets the default size
#[test]
fn point_size_with_an_empty_mask() {
    let vs = "struct O { float4 p : SV_Position; float s : PSIZE; }; O vs(float4 p : POSITION) { O o; o.p = p; o.s = 4; return o; }";
    let mut code = hlslc::compile(vs, &[], &mut |_, _| None, "vs", "vs_5_0", 0).unwrap();
    assert!(dxwgsl::translate_point_sprite(&code, None, 64.0).is_ok());
    // OSGN: the element named PSIZE, its mask byte (offset 20 of the 24-byte element) to 0
    let at = code.windows(4).position(|w| w == b"OSGN").unwrap();
    let chunk = at + 8;
    let n = u32::from_le_bytes(code[chunk..chunk + 4].try_into().unwrap()) as usize;
    let mut cleared = 0;
    for e in 0..n {
        let el = chunk + 8 + 24 * e;
        let name = u32::from_le_bytes(code[el..el + 4].try_into().unwrap()) as usize;
        if code[chunk + name..].starts_with(b"PSIZE") {
            code[el + 20] = 0;
            cleared += 1;
        }
    }
    assert_eq!(cleared, 1);
    let _ = dxwgsl::translate_point_sprite(&code, None, 64.0);
}
