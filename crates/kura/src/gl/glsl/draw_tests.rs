//! What a GLSL program's draw reads: the built-in attributes (gl_Vertex, gl_Color, gl_Normal,
//! gl_MultiTexCoord0/3/7) and a generic one, each from its array (types, sizes, strides,
//! normalization: colours and normals of integer types are normalized, GL 2.1 table 2.9) or
//! the current value, through glDrawArrays, glDrawElements and glBegin/glEnd; the model of
//! WebGPU must get, vertex for vertex, each input's four floats. And the built-in uniforms:
//! gl_TextureMatrix[n] of every unit, gl_NormalMatrix the inverse transpose of the model-view
//! matrix's upper 3x3.

use super::*;
use crate::d3d9::trace_tests::{alloc, attach_page, Rng, SERIAL};
use crate::render::model;

const VS: &str = "attribute vec4 extra;\nvarying vec4 v;\n\
void main() { gl_Position = gl_Vertex + extra;\n\
 v = gl_Color + vec4(gl_Normal, 0.0) + gl_MultiTexCoord0 + gl_MultiTexCoord3 + gl_MultiTexCoord7\n\
   + gl_TextureMatrix[3] * gl_MultiTexCoord3 + vec4(gl_NormalMatrix * gl_Normal, 0.0); }";
const FS: &str = "varying vec4 v;\nvoid main() { gl_FragColor = v; }";

fn guest(text: &str) -> *const *const c_char {
    unsafe {
        let p = alloc(text.len() as u32 + 1);
        std::ptr::copy_nonoverlapping(text.as_ptr(), p, text.len());
        *p.add(text.len()) = 0;
        let a = alloc(8) as *mut u32;
        *a = p as usize as u32;
        a as *const *const c_char
    }
}

/// GL 2.1 table 2.9 (normalized: (2c+1)/(2^b-1) signed, c/(2^b-1) unsigned)
fn comp(b: &[u8], ty: u32, norm: bool) -> f32 {
    (match ty {
        0x1400 => {
            let c = b[0] as i8 as f64;
            if norm {
                (2.0 * c + 1.0) / 255.0
            } else {
                c
            }
        }
        0x1401 => b[0] as f64 / if norm { 255.0 } else { 1.0 },
        0x1402 => {
            let c = i16::from_le_bytes([b[0], b[1]]) as f64;
            if norm {
                (2.0 * c + 1.0) / 65535.0
            } else {
                c
            }
        }
        _ => f32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
    }) as f32
}
fn tsize(ty: u32) -> usize {
    match ty {
        0x1400 | 0x1401 => 1,
        0x1402 => 2,
        _ => 4,
    }
}

/// (an array: size, type, stride, offset in the data, normalized)
#[derive(Clone, Copy, Debug)]
struct Arr(i32, u32, i32, usize, bool);

fn element(data: &[u8], a: &Arr, i: u32) -> [f32; 4] {
    let ts = tsize(a.1);
    let stride = if a.2 > 0 {
        a.2 as usize
    } else {
        ts * a.0 as usize
    };
    let mut v = [0.0, 0.0, 0.0, 1.0];
    for (k, x) in v.iter_mut().enumerate().take(a.0 as usize) {
        let at = a.3 + stride * i as usize + k * ts;
        *x = comp(&data[at..at + ts], a.1, a.4);
    }
    v
}

#[test]
fn glsl_draws_read_what_gl_reads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ctx = vngl_context_create(0);
    vngl_make_current(ctx, 0x1234, 32, 32);
    let mut r = Rng(0x5eed_9157);
    let (vs, fs) = (glCreateShader(GL_VERTEX_SHADER), glCreateShader(0x8B30));
    unsafe {
        glShaderSource(vs, 1, guest(VS), std::ptr::null());
        glShaderSource(fs, 1, guest(FS), std::ptr::null());
    }
    glCompileShader(vs);
    glCompileShader(fs);
    let p = glCreateProgram();
    glAttachShader(p, vs);
    glAttachShader(p, fs);
    glLinkProgram(p);
    glUseProgram(p);
    // which input is which (location -> name)
    let attribs: Vec<(String, u32)> = GL.lock().unwrap().as_ref().unwrap().programs[&p]
        .linked
        .as_ref()
        .expect("linked")
        .attribs
        .clone();
    for n in [
        "vn_Vertex",
        "vn_Color",
        "vn_Normal",
        "vn_MultiTexCoord0",
        "vn_MultiTexCoord3",
        "vn_MultiTexCoord7",
        "extra",
    ] {
        assert!(
            attribs.iter().any(|a| a.0 == n),
            "no input {n}: {attribs:?}"
        );
    }
    let extra = attribs.iter().find(|a| a.0 == "extra").unwrap().1;
    const N: usize = 4096;
    let data: Vec<u8> = (0..N / 4)
        .flat_map(|_| (((r.next() as u32) & 0x3fff_ffff) | 0x3000_0000).to_le_bytes())
        .collect();
    let client = unsafe { alloc(N as u32) as u32 };
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), client as *mut u8, N) };
    let idx = unsafe { alloc(256) as u32 };
    let mut checked = 0;
    for step in 0..200 {
        let pick = |r: &mut Rng, sizes: &[i32], types: &[u32], norm: bool| -> Option<Arr> {
            if r.chance(30) {
                return None;
            }
            let (size, ty) = (sizes[r.below(sizes.len())], types[r.below(types.len())]);
            let stride = if r.chance(40) {
                0
            } else {
                (size as usize * tsize(ty) + 4 * r.below(3)) as i32
            };
            Some(Arr(size, ty, stride, 8 * r.below(16), norm && ty != 0x1406))
        };
        let pos = pick(&mut r, &[2, 3, 4], &[0x1406, 0x1402], false)
            .unwrap_or(Arr(3, 0x1406, 0, 0, false));
        let col = pick(&mut r, &[3, 4], &[0x1401, 0x1406], true);
        let nor = pick(&mut r, &[3], &[0x1400, 0x1402, 0x1406], true);
        let tcs = [
            pick(&mut r, &[1, 2, 4], &[0x1406], false),
            pick(&mut r, &[2, 3, 4], &[0x1406, 0x1402], false),
            pick(&mut r, &[2], &[0x1406], false),
        ];
        let ext_norm = r.chance(50);
        let ext = pick(&mut r, &[1, 2, 3, 4], &[0x1406, 0x1401], ext_norm);
        // the current values
        let cur_col = [0.25, 0.5, 0.75, 1.0];
        let cur_nor = [r.below(5) as f32 - 2.0, 0.5, -0.25];
        let cur_tc = [
            [0.125, 0.375, 0.0, 1.0],
            [0.625, 0.875, 0.0, 1.0],
            [3.0, -1.0, 0.0, 1.0],
        ];
        let cur_ext = [1.5, 2.5, 3.5, 4.5];
        glColor4f(cur_col[0], cur_col[1], cur_col[2], cur_col[3]);
        glNormal3f(cur_nor[0], cur_nor[1], cur_nor[2]);
        for (k, u) in [0u32, 3, 7].into_iter().enumerate() {
            glMultiTexCoord2f(GL_TEXTURE0 + u, cur_tc[k][0], cur_tc[k][1]);
        }
        glVertexAttrib4f(extra, cur_ext[0], cur_ext[1], cur_ext[2], cur_ext[3]);
        glVertexPointer(pos.0, pos.1, pos.2, client + pos.3 as u32);
        glEnableClientState(0x8074);
        let on = |cap: u32, a: &Option<Arr>, set: &dyn Fn(&Arr)| match a {
            Some(a) => {
                set(a);
                glEnableClientState(cap);
            }
            None => glDisableClientState(cap),
        };
        on(0x8076, &col, &|a| {
            glColorPointer(a.0, a.1, a.2, client + a.3 as u32)
        });
        on(0x8075, &nor, &|a| {
            glNormalPointer(a.1, a.2, client + a.3 as u32)
        });
        for (k, u) in [0u32, 3, 7].into_iter().enumerate() {
            glClientActiveTexture(GL_TEXTURE0 + u);
            on(0x8078, &tcs[k], &|a| {
                glTexCoordPointer(a.0, a.1, a.2, client + a.3 as u32)
            });
        }
        match &ext {
            Some(a) => {
                glVertexAttribPointer(extra, a.0, a.1, a.4 as u8, a.2, client + a.3 as u32);
                glEnableVertexAttribArray(extra);
            }
            None => glDisableVertexAttribArray(extra),
        }
        let n = 1 + r.below(12);
        let how = r.below(3);
        let elems: Vec<u32> = if how == 1 {
            (0..n).map(|_| r.below(40) as u32).collect()
        } else {
            let f = r.below(8) as u32;
            (f..f + n as u32).collect()
        };
        // (every element inside the data)
        let last = |a: &Arr| {
            a.3 + (if a.2 > 0 {
                a.2 as usize
            } else {
                a.0 as usize * tsize(a.1)
            }) * (*elems.iter().max().unwrap() as usize + 1)
        };
        if [Some(pos), col, nor, tcs[0], tcs[1], tcs[2], ext]
            .iter()
            .flatten()
            .any(|a| last(a) > N)
        {
            continue;
        }
        // what each input reads for element i
        let want = |i: u32| -> Vec<(String, [f32; 4])> {
            let mut v = vec![
                ("vn_Vertex".to_string(), element(&data, &pos, i)),
                (
                    "vn_Color".into(),
                    col.map_or(cur_col, |a| element(&data, &a, i)),
                ),
                (
                    "vn_Normal".into(),
                    nor.map_or([cur_nor[0], cur_nor[1], cur_nor[2], 1.0], |a| {
                        element(&data, &a, i)
                    }),
                ),
                (
                    "extra".into(),
                    ext.map_or(cur_ext, |a| element(&data, &a, i)),
                ),
            ];
            for (k, u) in [0, 3, 7].into_iter().enumerate() {
                v.push((
                    format!("vn_MultiTexCoord{u}"),
                    tcs[k].map_or(cur_tc[k], |a| element(&data, &a, i)),
                ));
            }
            v
        };
        let mut wanted: Vec<Vec<(String, [f32; 4])>> = elems.iter().map(|&i| want(i)).collect();
        match how {
            0 => glDrawArrays(0, elems[0] as i32, n as i32),
            1 => {
                for (k, &e) in elems.iter().enumerate() {
                    unsafe { *((idx + 2 * k as u32) as *mut u16) = e as u16 };
                }
                glDrawElements(0, n as i32, 0x1403, idx);
            }
            _ => {
                // immediate mode: the values passed as current values, then the vertex (the
                // normal's w and glMultiTexCoord2f's r, q: 1, 0, 1)
                glBegin(0);
                for v in wanted.iter_mut() {
                    let g = |k: &str| v.iter().find(|x| x.0 == k).unwrap().1;
                    let (c, nn) = (g("vn_Color"), g("vn_Normal"));
                    glColor4f(c[0], c[1], c[2], c[3]);
                    glNormal3f(nn[0], nn[1], nn[2]);
                    for u in [0u32, 3, 7] {
                        let t = g(&format!("vn_MultiTexCoord{u}"));
                        glMultiTexCoord2f(GL_TEXTURE0 + u, t[0], t[1]);
                    }
                    let p = g("vn_Vertex");
                    glVertex4f(p[0], p[1], p[2], p[3]);
                    for x in v.iter_mut() {
                        match x.0.as_str() {
                            "vn_Normal" => x.1[3] = 1.0,
                            "extra" => x.1 = cur_ext,
                            s if s.starts_with("vn_MultiTexCoord") => (x.1[2], x.1[3]) = (0.0, 1.0),
                            _ => {}
                        }
                    }
                }
                glEnd();
            }
        }
        gpu::flush();
        gpu::pump();
        let got = model::with(|m| m.prims.as_mut().unwrap().drain(..).collect::<Vec<_>>());
        let got: Vec<Vec<(String, [f32; 4])>> = got
            .iter()
            .flatten()
            .map(|p| {
                let mut v: Vec<(String, [f32; 4])> = p[0]
                    .iter()
                    .map(|(loc, b)| {
                        let name = attribs
                            .iter()
                            .find(|a| a.1 == *loc)
                            .map_or(format!("@{loc}"), |a| a.0.clone());
                        (
                            name,
                            std::array::from_fn(|j| {
                                b.get(4 * j..4 * j + 4)
                                    .map_or(f32::NAN, |w| f32::from_le_bytes(w.try_into().unwrap()))
                            }),
                        )
                    })
                    .collect();
                v.sort_by(|a, b| a.0.cmp(&b.0));
                v
            })
            .collect();
        for w in wanted.iter_mut() {
            w.sort_by(|a, b| a.0.cmp(&b.0));
        }
        let what = format!("step {step}: how {how} pos {pos:?} col {col:?} normal {nor:?} tc {tcs:?} extra {ext:?}");
        assert_eq!(got.len(), wanted.len(), "{what}: points");
        for (k, (g, w)) in got.iter().zip(&wanted).enumerate() {
            assert_eq!(g, w, "{what}: vertex {k}");
        }
        checked += 1;
    }
    assert!(checked > 100, "{checked} draws checked");
    // the built-in uniforms
    for _ in 0..50 {
        let m: [f32; 16] = std::array::from_fn(|_| r.below(17) as f32 / 4.0 - 2.0);
        glMatrixMode(0x1700);
        unsafe { glLoadMatrixf(m.as_ptr()) };
        let unit = r.below(8) as u32;
        let t: [f32; 16] = std::array::from_fn(|_| r.below(9) as f32);
        glActiveTexture(GL_TEXTURE0 + unit);
        glMatrixMode(0x1702);
        unsafe { glLoadMatrixf(t.as_ptr()) };
        let g = GL.lock().unwrap();
        let c = g.as_ref().unwrap().ctxs.get(&ctx).unwrap();
        assert_eq!(
            builtin_uniform(c, &format!("vn_TM{unit}")).unwrap(),
            t.to_vec(),
            "gl_TextureMatrix[{unit}]"
        );
        assert_eq!(builtin_uniform(c, "vn_MV").unwrap(), m.to_vec());
        // gl_NormalMatrix^T * mat3(MV) = I (when MV's 3x3 is invertible)
        let a = |c: usize, r: usize| m[c * 4 + r] as f64;
        let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(2, 1) * a(1, 2))
            - a(1, 0) * (a(0, 1) * a(2, 2) - a(2, 1) * a(0, 2))
            + a(2, 0) * (a(0, 1) * a(1, 2) - a(1, 1) * a(0, 2));
        if det.abs() < 1e-3 {
            continue;
        }
        let nm = builtin_uniform(c, "vn_NM").unwrap();
        let n = |c: usize, r: usize| nm[c * 3 + r] as f64;
        for i in 0..3 {
            for j in 0..3 {
                // (N^T M)[i][j] = sum_k N[k][i] M[k][j]
                let s: f64 = (0..3).map(|k| n(i, k) * a(j, k)).sum();
                let e = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (s - e).abs() < 1e-3,
                    "gl_NormalMatrix {nm:?} of {m:?}: (N^T M)[{i}][{j}] = {s}"
                );
            }
        }
    }
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(
        errors.is_empty(),
        "WebGPU errors {:?}",
        &errors[..errors.len().min(10)]
    );
    model::with(|m| m.prims = None);
    vngl_context_destroy(ctx);
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
