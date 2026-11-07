//! What a fixed-function GL draw reads: random glDrawArrays, glDrawElements (client indices
//! and element buffers, ubyte/ushort/uint), glDrawRangeElements and glBegin/glEnd in all ten
//! primitive modes, with vertex, color and texture coordinate arrays (sizes, types,
//! normalization of colors, strides, BGRA colors, client memory and buffer objects with
//! offsets) or the current color and coordinates, smooth and flat shading. The page runs each
//! on the model of WebGPU (render/model.rs); the primitives it assembles must be GL 2.1's
//! (2.6.1, 2.8 and table 2.9 conversions, 2.14.7 the provoking vertex), vertex for vertex:
//! position, color, both units' coordinates as the four float vectors the fixed function gets.
//! Quads and polygons may split along either diagonal (GL leaves it open): triangles are
//! compared from their smallest vertex, which keeps their winding.

use super::*;
use crate::d3d9::trace_tests::{alloc, attach_page, Rng, SERIAL};
use crate::render::model;

#[derive(Clone, Copy)]
struct Arr {
    size: i32,
    ty: u32,
    stride: i32,
    /// byte offset of element 0 in `data`
    at: usize,
    vbo: bool,
}

/// GL 2.1 table 2.9: a component as a float (normalized: colors; others as they are).
fn comp(b: &[u8], ty: u32, norm: bool) -> f32 {
    let f = |v: f64, max: f64| if norm { v / max } else { v };
    (match ty {
        0x1400 => {
            let c = b[0] as i8 as f64;
            if norm {
                ((2.0 * c + 1.0) / 255.0).max(-1.0)
            } else {
                c
            }
        }
        0x1401 => f(b[0] as f64, 255.0),
        0x1402 => {
            let c = i16::from_le_bytes([b[0], b[1]]) as f64;
            if norm {
                ((2.0 * c + 1.0) / 65535.0).max(-1.0)
            } else {
                c
            }
        }
        0x1403 => f(u16::from_le_bytes([b[0], b[1]]) as f64, 65535.0),
        0x1404 => i32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
        0x1405 => u32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
        0x140A => f64::from_le_bytes(b[..8].try_into().unwrap()),
        _ => f32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
    }) as f32
}

fn tsize(ty: u32) -> usize {
    match ty {
        0x1400 | 0x1401 => 1,
        0x1402 | 0x1403 => 2,
        0x140A => 8,
        _ => 4,
    }
}

/// Element `i` of `a` over `data` as 4 floats (GL 2.8: missing components 0, 0, 0, 1).
fn element(data: &[u8], a: &Arr, i: u32, norm: bool) -> [f32; 4] {
    let n = if a.size == 0x80E1 { 4 } else { a.size as usize };
    let ts = tsize(a.ty);
    let stride = if a.stride > 0 {
        a.stride as usize
    } else {
        ts * n
    };
    let mut v = [0.0, 0.0, 0.0, 1.0];
    for (k, x) in v.iter_mut().enumerate().take(n) {
        let at = a.at + stride * i as usize + k * ts;
        *x = comp(&data[at..at + ts], a.ty, norm);
    }
    if a.size == 0x80E1 {
        v.swap(0, 2);
    }
    v
}

/// GL's primitives of `mode` over `n` vertices, by vertex number, each with its provoking
/// vertex (2.14.7), in groups: a quad is one group of two triangles split along either
/// diagonal (GL leaves it open), as the alternatives; any other primitive a group of itself.
fn gl_prims(mode: u32, n: usize) -> Vec<(Vec<Vec<Vec<usize>>>, usize)> {
    let one = |p: Vec<usize>, pv: usize| (vec![vec![p]], pv);
    // a quad (a b c d), in its order around
    let quad = |a: usize, b: usize, c: usize, d: usize| {
        (
            vec![
                vec![vec![a, b, c], vec![a, c, d]],
                vec![vec![a, b, d], vec![b, c, d]],
            ],
            d,
        )
    };
    match mode {
        0 => (0..n).map(|i| one(vec![i], i)).collect(),
        1 => (0..n / 2)
            .map(|i| one(vec![2 * i, 2 * i + 1], 2 * i + 1))
            .collect(),
        2 if n >= 2 => (0..n)
            .map(|i| one(vec![i, (i + 1) % n], (i + 1) % n))
            .collect(),
        3 => (1..n).map(|i| one(vec![i - 1, i], i)).collect(),
        4 => (0..n / 3)
            .map(|t| one(vec![3 * t, 3 * t + 1, 3 * t + 2], 3 * t + 2))
            .collect(),
        5 => (0..n.saturating_sub(2))
            .map(|i| {
                one(
                    if i % 2 == 0 {
                        vec![i, i + 1, i + 2]
                    } else {
                        vec![i + 1, i, i + 2]
                    },
                    i + 2,
                )
            })
            .collect(),
        6 => (1..n.saturating_sub(1))
            .map(|i| one(vec![0, i, i + 1], i + 1))
            .collect(),
        7 => (0..n / 4)
            .map(|q| quad(4 * q, 4 * q + 1, 4 * q + 2, 4 * q + 3))
            .collect(),
        // quad strip quad q: (2q, 2q+1, 2q+3, 2q+2) around; its provoking vertex 2q+3
        8 => (0..n.saturating_sub(2) / 2)
            .map(|q| {
                let (alts, _) = quad(2 * q, 2 * q + 1, 2 * q + 3, 2 * q + 2);
                (alts, 2 * q + 3)
            })
            .collect(),
        9 if n >= 3 => (1..n - 1).map(|i| one(vec![0, i, i + 1], 0)).collect(),
        _ => Vec::new(),
    }
}

/// Whether two triangles are the same: their vertices from the smallest, in order.
fn canon(p: &[[f32; 16]]) -> Vec<Vec<u32>> {
    let v: Vec<Vec<u32>> = p
        .iter()
        .map(|x| x.iter().map(|f| f.to_bits()).collect())
        .collect();
    if v.len() != 3 {
        return v;
    }
    let k = (0..3).min_by_key(|&i| v[i].clone()).unwrap();
    (0..3).map(|i| v[(k + i) % 3].clone()).collect()
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 7);
    attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ctx = vngl_context_create(0);
    vngl_make_current(ctx, 0x1234, 32, 32);
    // vertex data: client memory, and the same bytes in a buffer object
    const N: usize = 4096;
    let data: Vec<u8> = (0..N / 4)
        .flat_map(|_| (((r.next() as u32) & 0x3fff_ffff) | 0x3000_0000).to_le_bytes())
        .collect();
    let client = unsafe { alloc(N as u32) as u32 };
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), client as *mut u8, N) };
    let mut vbo = 0u32;
    let mut ebo = 0u32;
    unsafe {
        glGenBuffers(1, &mut vbo);
        glGenBuffers(1, &mut ebo);
        glBindBuffer(0x8892, vbo);
        glBufferData(0x8892, N as isize, data.as_ptr(), 0x88E4);
        glBindBuffer(0x8892, 0);
    }
    let idx_mem = unsafe { alloc(1024) as u32 };
    let mut checked = 0;
    for step in 0..150 {
        let mode = r.below(10) as u32;
        let n = r.below(13);
        let flat = r.chance(30);
        glShadeModel(if flat { 0x1D00 } else { 0x1D01 });
        // the arrays (or the current values)
        let arr = |r: &mut Rng, sizes: &[i32], types: &[u32]| -> Option<Arr> {
            if !r.chance(75) {
                return None;
            }
            let size = sizes[r.below(sizes.len())];
            let ty = types[r.below(types.len())];
            let n = if size == 0x80E1 { 4 } else { size as usize };
            let stride = if r.chance(40) {
                0
            } else {
                (n * tsize(ty) + 4 * r.below(3)) as i32
            };
            Some(Arr {
                size,
                ty,
                stride,
                at: 8 * r.below(16),
                vbo: r.chance(50),
            })
        };
        let pos = arr(&mut r, &[2, 3, 4], &[0x1402, 0x1404, 0x1406, 0x140A]).unwrap_or(Arr {
            size: 3,
            ty: 0x1406,
            stride: 0,
            at: 0,
            vbo: false,
        });
        let col = arr(&mut r, &[3, 4, 0x80E1], &[0x1401, 0x1403, 0x1406, 0x1400])
            .filter(|a| a.size != 0x80E1 || a.ty == 0x1401);
        let tc = [
            arr(&mut r, &[1, 2, 3, 4], &[0x1402, 0x1406]),
            arr(&mut r, &[2, 4], &[0x1406]),
        ];
        let cur_col = [0.25, 0.5, 0.75, 1.0];
        let cur_tc = [[0.125, 0.375, 0.0, 1.0], [0.625, 0.875, 0.0, 1.0]];
        glColor4f(cur_col[0], cur_col[1], cur_col[2], cur_col[3]);
        glMultiTexCoord2f(GL_TEXTURE0, cur_tc[0][0], cur_tc[0][1]);
        glMultiTexCoord2f(GL_TEXTURE0 + 1, cur_tc[1][0], cur_tc[1][1]);
        let ptr = |a: &Arr| -> u32 {
            if a.vbo {
                glBindBuffer(0x8892, vbo);
                a.at as u32
            } else {
                glBindBuffer(0x8892, 0);
                client + a.at as u32
            }
        };
        glVertexPointer(pos.size, pos.ty, pos.stride, ptr(&pos));
        glEnableClientState(0x8074);
        match &col {
            Some(a) => {
                glColorPointer(a.size, a.ty, a.stride, ptr(a));
                glEnableClientState(0x8076);
            }
            None => glDisableClientState(0x8076),
        }
        for (u, t) in tc.iter().enumerate() {
            glClientActiveTexture(GL_TEXTURE0 + u as u32);
            match t {
                Some(a) => {
                    glTexCoordPointer(a.size, a.ty, a.stride, ptr(a));
                    glEnableClientState(0x8078);
                }
                None => glDisableClientState(0x8078),
            }
        }
        glBindBuffer(0x8892, 0);
        // which elements, by vertex: (array element numbers)
        let how = r.below(4);
        let elems: Vec<u32> = match how {
            0 | 3 => {
                let first = r.below(8) as u32;
                (first..first + n as u32).collect()
            }
            _ => {
                // (distinct: a vertex twice makes triangles whose turn cannot be told)
                let mut all: Vec<u32> = (0..40).collect();
                for i in (1..all.len()).rev() {
                    all.swap(i, r.below(i + 1));
                }
                all[..n].to_vec()
            }
        };
        // (every element inside the data)
        let last = |a: &Arr| {
            let k = if a.size == 0x80E1 { 4 } else { a.size as usize } * tsize(a.ty);
            let stride = if a.stride > 0 { a.stride as usize } else { k };
            a.at + stride * elems.iter().copied().max().unwrap_or(0) as usize + k
        };
        if [Some(pos), col, tc[0], tc[1]]
            .iter()
            .flatten()
            .any(|a| last(a) > N)
        {
            continue;
        }
        let vertex = |i: u32| -> [f32; 16] {
            let p = element(&data, &pos, i, false);
            let c = col.as_ref().map_or(cur_col, |a| element(&data, a, i, true));
            let t: [[f32; 4]; 2] = std::array::from_fn(|u| {
                tc[u]
                    .as_ref()
                    .map_or(cur_tc[u], |a| element(&data, a, i, false))
            });
            let mut v = [0f32; 16];
            v[..4].copy_from_slice(&p);
            v[4..8].copy_from_slice(&c);
            v[8..12].copy_from_slice(&t[0]);
            v[12..].copy_from_slice(&t[1]);
            v
        };
        let verts: Vec<[f32; 16]> = if how == 3 {
            // immediate mode: the array values passed as the current values, then the vertex
            glBegin(mode);
            let vs: Vec<[f32; 16]> = elems.iter().map(|&i| vertex(i)).collect();
            for v in &vs {
                glColor4f(v[4], v[5], v[6], v[7]);
                glMultiTexCoord2f(GL_TEXTURE0, v[8], v[9]);
                glMultiTexCoord2f(GL_TEXTURE0 + 1, v[12], v[13]);
                glVertex4f(v[0], v[1], v[2], v[3]);
            }
            glEnd();
            // (glTexCoord2 sets r = 0, q = 1)
            vs.into_iter()
                .map(|mut v| {
                    (v[10], v[11], v[14], v[15]) = (0.0, 1.0, 0.0, 1.0);
                    v
                })
                .collect()
        } else {
            match how {
                0 => glDrawArrays(mode, elems.first().copied().unwrap_or(0) as i32, n as i32),
                _ => {
                    let ty = [0x1401u32, 0x1403, 0x1405][r.below(3)];
                    let bytes: Vec<u8> = elems
                        .iter()
                        .flat_map(|&e| match ty {
                            0x1401 => vec![e as u8],
                            0x1403 => (e as u16).to_le_bytes().to_vec(),
                            _ => e.to_le_bytes().to_vec(),
                        })
                        .collect();
                    if r.chance(50) {
                        unsafe {
                            glBindBuffer(0x8893, ebo);
                            let mut all = vec![0u8; 8];
                            all.extend_from_slice(&bytes);
                            glBufferData(0x8893, all.len() as isize, all.as_ptr(), 0x88E4);
                        }
                        if how == 1 {
                            glDrawElements(mode, n as i32, ty, 8)
                        } else {
                            glDrawRangeElements(mode, 0, 40, n as i32, ty, 8)
                        }
                        glBindBuffer(0x8893, 0);
                    } else {
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                bytes.as_ptr(),
                                idx_mem as *mut u8,
                                bytes.len(),
                            )
                        };
                        if how == 1 {
                            glDrawElements(mode, n as i32, ty, idx_mem)
                        } else {
                            glDrawRangeElements(mode, 0, 40, n as i32, ty, idx_mem)
                        }
                    }
                }
            }
            elems.iter().map(|&i| vertex(i)).collect()
        };
        gpu::flush();
        gpu::pump();
        let want: Vec<Vec<Vec<Vec<Vec<u32>>>>> = gl_prims(mode, verts.len())
            .iter()
            .map(|(alts, pv)| {
                alts.iter()
                    .map(|alt| {
                        let mut tris: Vec<Vec<Vec<u32>>> = alt
                            .iter()
                            .map(|p| {
                                let vs: Vec<[f32; 16]> = p
                                    .iter()
                                    .map(|&i| {
                                        let mut v = verts[i];
                                        if flat {
                                            v[4..8].copy_from_slice(&verts[*pv][4..8]);
                                        }
                                        v
                                    })
                                    .collect();
                                canon(&vs)
                            })
                            .collect();
                        tris.sort();
                        tris
                    })
                    .collect()
            })
            .collect();
        let got = model::with(|m| m.prims.as_mut().unwrap().drain(..).collect::<Vec<_>>());
        let got: Vec<Vec<Vec<u32>>> = got
            .iter()
            .flatten()
            .map(|p| {
                let vs: Vec<[f32; 16]> = p
                    .iter()
                    .map(|v| {
                        let mut x = [0f32; 16];
                        for (k, (_, b)) in v.iter().enumerate().take(4) {
                            for j in 0..4 {
                                x[4 * k + j] = b.get(4 * j..4 * j + 4).map_or(f32::NAN, |w| {
                                    f32::from_le_bytes(w.try_into().unwrap())
                                });
                            }
                        }
                        x
                    })
                    .collect();
                canon(&vs)
            })
            .collect();
        let what = format!("seed {seed} step {step}: mode {mode} n {n} how {how} flat {flat} pos {}/{:#x}/{} col {:?} tc {:?}", pos.size, pos.ty, pos.stride, col.map(|a| (a.size, a.ty, a.stride)), tc.map(|t| t.map(|a| (a.size, a.ty, a.stride))));
        let total: usize = want.iter().map(|g| g[0].len()).sum();
        assert_eq!(got.len(), total, "{what}: primitives");
        let mut at = 0;
        let show = |v: &[Vec<Vec<u32>>]| {
            v.iter()
                .map(|p| {
                    p.iter()
                        .map(|v| v.iter().map(|b| f32::from_bits(*b)).collect::<Vec<_>>())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        for (k, g) in want.iter().enumerate() {
            let mut part: Vec<Vec<Vec<u32>>> = got[at..at + g[0].len()].to_vec();
            part.sort();
            at += g[0].len();
            assert!(
                g.contains(&part),
                "{what}: primitive {k} differs\n got  {:?}\n want {:?}",
                show(&part),
                show(&g[0])
            );
        }
        checked += 1;
    }
    assert!(checked > 80, "seed {seed}: {checked} draws checked");
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(
        errors.is_empty(),
        "seed {seed}: WebGPU errors {:?}",
        &errors[..errors.len().min(10)]
    );
    model::with(|m| m.prims = None);
    vngl_context_destroy(ctx);
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[test]
fn gl_draws_read_what_gl_reads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for seed in 0..6 {
        run(seed);
    }
}
