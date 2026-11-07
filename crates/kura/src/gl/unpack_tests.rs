//! Texture uploads against the GL 2.1 specification, written here a second time from its
//! text (3.6.4 unpacking, table 3.5/3.8 packed layouts, table 2.9 conversions, 3.8.1 base
//! internal formats): every source format and type with every base format, random unpack
//! state and data, byte for byte. The errors of glPixelStorei / glTexImage2D /
//! glTexSubImage2D. The fixed function's WGSL (texture environments, intensity included)
//! parses and validates. Windows' answers for the same: winref probe gltex.

use super::*;
use crate::d3d9::trace_tests::{Rng, SERIAL};

const FORMATS: [u32; 11] = [
    0x1903, 0x1904, 0x1905, 0x1906, 0x1907, 0x1908, 0x1909, 0x190A, 0x80E0, 0x80E1, 0x8227,
];
const TYPES: [u32; 8] = [
    0x1400, 0x1401, 0x1402, 0x1403, 0x1404, 0x1405, 0x1406, 0x140B,
];
/// packed types, their components' widths as the tables list them, and whether the first
/// component is the most significant (the _REV types list from the least)
const PACKED: [(u32, u32, &[u32], bool); 12] = [
    (0x8032, 8, &[3, 3, 2], true),
    (0x8362, 8, &[3, 3, 2], false),
    (0x8363, 16, &[5, 6, 5], true),
    (0x8364, 16, &[5, 6, 5], false),
    (0x8033, 16, &[4, 4, 4, 4], true),
    (0x8365, 16, &[4, 4, 4, 4], false),
    (0x8034, 16, &[5, 5, 5, 1], true),
    (0x8366, 16, &[5, 5, 5, 1], false),
    (0x8035, 32, &[8, 8, 8, 8], true),
    (0x8367, 32, &[8, 8, 8, 8], false),
    (0x8036, 32, &[10, 10, 10, 2], true),
    (0x8368, 32, &[10, 10, 10, 2], false),
];

/// (components of a format, in the order the format names them)
fn names(format: u32) -> &'static [char] {
    match format {
        0x1903 => &['R'],
        0x1904 => &['G'],
        0x1905 => &['B'],
        0x1906 => &['A'],
        0x1907 => &['R', 'G', 'B'],
        0x1908 => &['R', 'G', 'B', 'A'],
        0x1909 => &['L'],
        0x190A => &['L', 'A'],
        0x80E0 => &['B', 'G', 'R'],
        0x80E1 => &['B', 'G', 'R', 'A'],
        0x8227 => &['R', 'G'],
        _ => &['D'],
    }
}

fn half_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = (h >> 10 & 31) as i32;
    let m = (h & 1023) as f64;
    sign * match e {
        0 => m / 1024.0 * 2f64.powi(-14),
        31 if m == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        _ => (1.0 + m / 1024.0) * 2f64.powi(e - 15),
    }
}

/// The spec's texels for `data` (base: 0 RGBA 1 RGB 2 ALPHA 3 LUMINANCE 4 LUMINANCE_ALPHA 6 INTENSITY).
fn reference(
    data: &[u8],
    w: usize,
    h: usize,
    format: u32,
    ty: u32,
    base: u32,
    align: usize,
    row_length: usize,
    skip_rows: usize,
    skip_pixels: usize,
) -> Vec<u8> {
    let comps = names(format);
    let packed = PACKED.iter().find(|p| p.0 == ty);
    // n elements of size s per group (a packed group: one element of its size)
    let (n, s) = match packed {
        Some(p) => (1, p.1 as usize / 8),
        None => (
            comps.len(),
            match ty {
                0x1400 | 0x1401 => 1,
                0x1402 | 0x1403 | 0x140B => 2,
                _ => 4,
            },
        ),
    };
    let l = if row_length > 0 { row_length } else { w };
    // "k = nl if s >= a, else a/s * ceil(snl/a)" elements
    let k = if s >= align {
        n * l
    } else {
        align / s * (s * n * l).div_ceil(align)
    };
    let le = |at: usize, len: usize| -> u64 {
        (0..len).fold(0u64, |v, i| v | (data[at + i] as u64) << (8 * i))
    };
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let at = ((skip_rows + y) * k + (skip_pixels + x) * n) * s;
            // the components as [0, 1] values, in the format's order
            let vals: Vec<f64> = match packed {
                Some(&(_, bits, widths, msb_first)) => {
                    let v = le(at, bits as usize / 8);
                    let mut shift = if msb_first { bits } else { 0 };
                    widths
                        .iter()
                        .map(|&wd| {
                            let sh = if msb_first {
                                shift -= wd;
                                shift
                            } else {
                                shift += wd;
                                shift - wd
                            };
                            let max = (1u64 << wd) - 1;
                            ((v >> sh) & max) as f64 / max as f64
                        })
                        .collect()
                }
                None => (0..comps.len())
                    .map(|i| {
                        let p = at + i * s;
                        let c = le(p, s);
                        match ty {
                            0x1401 => c as f64 / 255.0,
                            0x1400 => (2.0 * (c as u8 as i8) as f64 + 1.0) / 255.0,
                            0x1403 => c as f64 / 65535.0,
                            0x1402 => (2.0 * (c as u16 as i16) as f64 + 1.0) / 65535.0,
                            0x1405 => c as f64 / 4294967295.0,
                            0x1404 => (2.0 * (c as u32 as i32) as f64 + 1.0) / 4294967295.0,
                            0x140B => half_to_f64(c as u16),
                            _ => f32::from_bits(c as u32) as f64,
                        }
                    })
                    .collect(),
            };
            // to RGBA: missing colour components 0, missing alpha 1; L to R, G and B
            let mut rgba = [0.0, 0.0, 0.0, 1.0];
            for (c, v) in comps.iter().zip(vals) {
                match c {
                    'R' => rgba[0] = v,
                    'G' => rgba[1] = v,
                    'B' => rgba[2] = v,
                    'A' => rgba[3] = v,
                    _ => rgba[..3].fill(v),
                }
            }
            // clamp, the base internal format's components (table 3.15), to 8 bits
            let q =
                |v: f64| (if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) } * 255.0).round() as u8;
            let [r, g, b, a] = rgba.map(q);
            out.extend_from_slice(&match base {
                1 => [r, g, b, 255],
                2 => [0, 0, 0, a],
                3 => [r, r, r, 255],
                4 => [r, r, r, a],
                6 => [r, r, r, r],
                _ => [r, g, b, a],
            });
        }
    }
    out
}

#[test]
fn unpack_matches_the_spec() {
    let mut r = Rng(0x5eed_1234_9876_0001);
    let mut cases = 0;
    let all: Vec<(u32, u32)> = FORMATS
        .iter()
        .flat_map(|&f| TYPES.iter().map(move |&t| (f, t)))
        .chain(PACKED.iter().flat_map(|p| {
            let f: &[u32] = if p.2.len() == 3 {
                &[0x1907, 0x80E0]
            } else {
                &[0x1908, 0x80E1]
            };
            f.iter().map(move |&f| (f, p.0))
        }))
        .collect();
    for &(format, ty) in &all {
        for base in [0, 1, 2, 3, 4, 6] {
            for _ in 0..12 {
                let (w, h) = (1 + r.below(5), 1 + r.below(4));
                let align = [1, 2, 4, 8][r.below(4)];
                let row_length = if r.chance(40) { w + r.below(4) } else { 0 };
                let (skip_rows, skip_pixels) = (r.below(3), r.below(3));
                let mut data = vec![0u8; 4096];
                for b in data.iter_mut() {
                    *b = r.next() as u8;
                }
                if ty == 0x1406 || ty == 0x140B {
                    // floats near [0, 1] most of the time (and NaN, infinities and the rest)
                    for i in (0..data.len()).step_by(if ty == 0x1406 { 4 } else { 2 }) {
                        if r.chance(80) {
                            let v = [
                                0.0f32,
                                1.0,
                                0.5,
                                -0.5,
                                2.0,
                                0.25,
                                1.0 / 255.0,
                                0.5 / 255.0,
                                0.999,
                            ][r.below(9)]
                                + (r.below(1000) as f32 / 1e6);
                            if ty == 0x1406 {
                                data[i..i + 4].copy_from_slice(&v.to_le_bytes());
                            } else {
                                data[i..i + 2].copy_from_slice(
                                    &crate::d3d11::format::f16_bits(v).to_le_bytes(),
                                );
                            }
                        }
                    }
                }
                let got = unsafe {
                    to_rgba(
                        data.as_ptr(),
                        w as u32,
                        h as u32,
                        format,
                        ty,
                        base,
                        [
                            align as i32,
                            row_length as i32,
                            skip_rows as i32,
                            skip_pixels as i32,
                        ],
                    )
                };
                let want = reference(
                    &data,
                    w,
                    h,
                    format,
                    ty,
                    base,
                    align,
                    row_length,
                    skip_rows,
                    skip_pixels,
                );
                assert_eq!(got, want, "format {format:#x} type {ty:#x} base {base} {w}x{h} align {align} row {row_length} skip {skip_rows}/{skip_pixels}");
                cases += 1;
            }
        }
    }
    assert!(cases > 5000);
}

fn err() -> u32 {
    glGetError()
}

#[test]
fn upload_errors() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let ctx = vngl_context_create(0);
        vngl_make_current(ctx, 0, 0, 0);
        let px = [0u8; 256];
        let p = px.as_ptr();
        for (v, e) in [
            (0, GL_INVALID_VALUE),
            (3, GL_INVALID_VALUE),
            (16, GL_INVALID_VALUE),
            (-1, GL_INVALID_VALUE),
            (2, 0),
            (8, 0),
        ] {
            glPixelStorei(0x0CF5, v);
            assert_eq!(err(), e, "alignment {v}");
        }
        let mut got = 0i32;
        glGetIntegerv(0x0CF5, &mut got);
        assert_eq!(got, 8, "a refused alignment leaves the last one");
        glPixelStorei(0x0CF2, -1);
        assert_eq!(err(), GL_INVALID_VALUE);
        glPixelStorei(0x1234, 1);
        assert_eq!(err(), GL_INVALID_ENUM);
        glPixelStorei(0x0CF5, 4);
        // texture 0 is a texture too
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            0x1908,
            4,
            4,
            0,
            0x1908,
            GL_UNSIGNED_BYTE,
            p,
        );
        assert_eq!(err(), 0, "default texture");
        let mut t = 0u32;
        glGenTextures(1, &mut t);
        glBindTexture(GL_TEXTURE_2D, t);
        for (what, e, f) in [
            (
                "target",
                GL_INVALID_ENUM,
                (0x1234u32, 0, 0x1908, 4, 4, 0, 0x1908u32, GL_UNSIGNED_BYTE),
            ),
            (
                "width -1",
                GL_INVALID_VALUE,
                (GL_TEXTURE_2D, 0, 0x1908, -1, 4, 0, 0x1908, GL_UNSIGNED_BYTE),
            ),
            (
                "level -1",
                GL_INVALID_VALUE,
                (GL_TEXTURE_2D, -1, 0x1908, 4, 4, 0, 0x1908, GL_UNSIGNED_BYTE),
            ),
            (
                "border 2",
                GL_INVALID_VALUE,
                (GL_TEXTURE_2D, 0, 0x1908, 4, 4, 2, 0x1908, GL_UNSIGNED_BYTE),
            ),
            (
                "internal 5",
                GL_INVALID_VALUE,
                (GL_TEXTURE_2D, 0, 5, 4, 4, 0, 0x1908, GL_UNSIGNED_BYTE),
            ),
            (
                "format",
                GL_INVALID_ENUM,
                (GL_TEXTURE_2D, 0, 0x1908, 4, 4, 0, 0x1234, GL_UNSIGNED_BYTE),
            ),
            (
                "type",
                GL_INVALID_ENUM,
                (GL_TEXTURE_2D, 0, 0x1908, 4, 4, 0, 0x1908, 0x1234),
            ),
            (
                "565 rgba",
                GL_INVALID_OPERATION,
                (GL_TEXTURE_2D, 0, 0x1908, 4, 4, 0, 0x1908, 0x8363),
            ),
            (
                "4444 rgb",
                GL_INVALID_OPERATION,
                (GL_TEXTURE_2D, 0, 0x1907, 4, 4, 0, 0x1907, 0x8033),
            ),
            (
                "ok 3x2",
                0,
                (GL_TEXTURE_2D, 0, 0x1908, 3, 2, 0, 0x1908, GL_UNSIGNED_BYTE),
            ),
            (
                "ok 8x8 null",
                0,
                (GL_TEXTURE_2D, 0, 0x1908, 8, 8, 0, 0x1908, GL_UNSIGNED_BYTE),
            ),
        ] {
            glTexImage2D(
                f.0,
                f.1,
                f.2,
                f.3,
                f.4,
                f.5,
                f.6,
                f.7,
                if what.contains("null") {
                    std::ptr::null()
                } else {
                    p
                },
            );
            assert_eq!(err(), e, "TexImage2D {what}");
        }
        for (what, e, f) in [
            (
                "past the edge",
                GL_INVALID_VALUE,
                (0, 6, 6, 4, 1, 0x1908u32),
            ),
            ("x -1", GL_INVALID_VALUE, (0, -1, 0, 1, 1, 0x1908)),
            (
                "undefined level",
                GL_INVALID_OPERATION,
                (1, 0, 0, 1, 1, 0x1908),
            ),
            ("format", GL_INVALID_ENUM, (0, 0, 0, 1, 1, 0x1234)),
            ("empty", 0, (0, 8, 8, 0, 0, 0x1908)),
            ("all of it", 0, (0, 0, 0, 8, 8, 0x1908)),
            ("luminance into rgba", 0, (0, 1, 1, 2, 2, 0x1909)),
        ] {
            glTexSubImage2D(
                GL_TEXTURE_2D,
                f.0,
                f.1,
                f.2,
                f.3,
                f.4,
                f.5,
                GL_UNSIGNED_BYTE,
                p,
            );
            assert_eq!(err(), e, "TexSubImage2D {what}");
        }
        glDeleteTextures(1, &t);
        vngl_context_destroy(ctx);
        *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[test]
fn fixed_function_wgsl_validates() {
    for (what, code) in [("vertex", FF_VS), ("fragment", FF_PS)] {
        let m = naga::front::wgsl::parse_str(code)
            .unwrap_or_else(|e| panic!("{what}: {}", e.emit_to_string(code)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&m)
        .unwrap_or_else(|e| panic!("{what}: {e:?}"));
    }
}
