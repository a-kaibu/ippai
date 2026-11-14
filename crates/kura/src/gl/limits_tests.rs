//! The GLSL limits glGetIntegerv answers: present (EntisGLS, zombie2_trial.zip, builds
//! `#define MAX_BONE_PALETTE n` from GL_MAX_VERTEX_UNIFORM_COMPONENTS and crashed on the 0 an
//! unknown query gave), at least the GL 2.1 / GLES 2 minimums, consistent between their
//! component and vector forms, and true: shaders using random amounts up to them translate,
//! with a uniform block inside WebGPU's 64 KiB binding and inter-stage locations inside its 16.

use super::*;
use crate::d3d9::trace_tests::{Rng, SERIAL};
use std::collections::HashMap;

fn get(p: u32) -> i32 {
    let mut v = [-7i32; 4];
    unsafe { glGetIntegerv(p, v.as_mut_ptr()) };
    v[0]
}

fn fresh() {
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ctx = vngl_context_create(0);
    vngl_make_current(ctx, 0, 0, 0);
}

#[test]
fn glsl_limits_are_answered_and_consistent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fresh();
    // (pname, GL 2.1 / GLES 2 minimum)
    for (p, min) in [(0x8B4A, 512), (0x8B49, 64), (0x8B4B, 32), (0x8DFB, 128), (0x8DFD, 16), (0x8DFC, 8), (0x8869, 16)] {
        assert!(get(p) >= min, "{p:#x}: {} < {min}", get(p));
    }
    assert_eq!(get(0x8B4A), 4 * get(0x8DFB));
    assert_eq!(get(0x8B49), 4 * get(0x8DFD));
    assert_eq!(get(0x8B4B), 4 * get(0x8DFC));
    let mut f = [0f32; 4];
    unsafe { glGetFloatv(0x8B4A, f.as_mut_ptr()) };
    assert_eq!(f[0] as i32, get(0x8B4A));
}

/// Random uniform arrays (vec4 / mat4 / mat3, the bone palettes games declare) and varying
/// counts within the advertised limits link, and what they need fits WebGPU's defaults.
#[test]
fn shaders_within_the_limits_translate() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fresh();
    let (uv, fv, vary) = (get(0x8DFB) as usize, get(0x8DFD) as usize, get(0x8DFC) as usize);
    let mut r = Rng(0x2026_1113);
    for case in 0..48 {
        // a bone palette: mat3 + vec4 per bone (4 vectors), as EntisGLS declares it
        let bones = 1 + r.below(uv / 4 - 2);
        let extra = r.below(uv - bones * 4);
        let pix = 1 + r.below(fv - 1);
        let nv = 1 + r.below(vary);
        let vs = format!(
            "#define MAX_BONE_PALETTE {bones}\nattribute vec4 p;\nuniform mat3 rot[MAX_BONE_PALETTE];\nuniform vec4 tr[MAX_BONE_PALETTE];\nuniform vec4 more[{n}];\n{vs_vary}void main() {{ int k = int(p.w); gl_Position = vec4(rot[k] * p.xyz + tr[k].xyz, 1.0) + more[k]; {vs_set} }}",
            n = extra.max(1),
            vs_vary = (0..nv).map(|i| format!("varying vec4 v{i};\n")).collect::<String>(),
            vs_set = (0..nv).map(|i| format!("v{i} = p;")).collect::<String>(),
        );
        let ps = format!(
            "uniform vec4 c[{pix}];\n{ps_vary}void main() {{ gl_FragColor = c[0]{ps_sum}; }}",
            ps_vary = (0..nv).map(|i| format!("varying vec4 v{i};\n")).collect::<String>(),
            ps_sum = (0..nv).map(|i| format!(" + v{i}")).collect::<String>(),
        );
        let prog = glwgsl::link(&vs, &ps, &HashMap::new()).unwrap_or_else(|e| panic!("case {case}: bones {bones} extra {extra} pix {pix} varyings {nv}: {e}"));
        for b in [&prog.vs.block, &prog.ps.block].into_iter().flatten() {
            assert!(b.size <= 65536, "case {case}: block of {} bytes", b.size);
        }
        let locs = prog.vs.wgsl.matches("@location(").count().saturating_sub(prog.attribs.len());
        assert!(locs <= 16, "case {case}: {locs} inter-stage locations\n{}", prog.vs.wgsl);
    }
}

/// Random glMatrixMode / glPushMatrix / glPopMatrix / glLoadMatrixf against a model of the three
/// stacks: a push at the advertised depth is GL_STACK_OVERFLOW and changes nothing, a pop of
/// the last matrix GL_STACK_UNDERFLOW, and the top is always the model's.
#[test]
fn matrix_stacks_follow_their_depths() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for seed in 1..=40u64 {
        fresh();
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 3);
        let depths = [get(0x0D36) as usize, get(0x0D38) as usize, get(0x0D39) as usize];
        assert!(depths[0] >= 32 && depths[1] >= 2 && depths[2] >= 2, "{depths:?}");
        // (the model: each stack's matrices by their first element; identity is 1)
        let mut model: [Vec<f32>; 3] = [vec![1.0], vec![1.0], vec![1.0]];
        let mut mode = 0usize;
        let mut mark = 2.0f32;
        for step in 0..600 {
            let want_err = match r.below(10) {
                0 => {
                    mode = r.below(3);
                    glMatrixMode([GL_MODELVIEW, GL_PROJECTION, GL_TEXTURE][mode]);
                    0
                }
                // (pushes more often than pops: the depth is reached)
                1..=5 => {
                    glPushMatrix();
                    if model[mode].len() >= depths[mode] {
                        0x0503
                    } else {
                        let t = *model[mode].last().unwrap();
                        model[mode].push(t);
                        0
                    }
                }
                6 | 7 => {
                    glPopMatrix();
                    if model[mode].len() > 1 {
                        model[mode].pop();
                        0
                    } else {
                        0x0504
                    }
                }
                _ => {
                    let mut m = [0f32; 16];
                    m[0] = mark;
                    unsafe { glLoadMatrixf(m.as_ptr()) };
                    *model[mode].last_mut().unwrap() = mark;
                    mark += 1.0;
                    0
                }
            };
            assert_eq!(glGetError(), want_err, "seed {seed} step {step}");
            let mut m = [0f32; 16];
            unsafe { glGetFloatv([0x0BA6, 0x0BA7, 0x0BA8][mode], m.as_mut_ptr()) };
            assert_eq!(m[0], *model[mode].last().unwrap(), "seed {seed} step {step}");
        }
    }
}

/// Sizes past GL_MAX_TEXTURE_SIZE are GL_INVALID_VALUE (their byte counts wrapped 32 bits and
/// the upload wrote outside its buffer), for glTexImage2D, glCopyTexImage2D and glReadPixels;
/// random sizes around the limit and around 2^16 and 2^31.
#[test]
fn sizes_past_the_limit_are_refused() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fresh();
    let max = get(0x0D33);
    assert_eq!(max, 8192);
    let mut r = Rng(0x51ce_5eed);
    let mut tex = 0u32;
    unsafe {
        glGenTextures(1, &mut tex);
        glBindTexture(GL_TEXTURE_2D, tex);
    }
    let px = [0u8; 16];
    let mut out = [0u8; 16];
    for k in 0..3000 {
        let pick = |r: &mut Rng| -> i32 {
            match r.below(6) {
                0 => max + 1 + r.below(8) as i32,
                1 => 65536 - 2 + r.below(4) as i32,
                2 => i32::MAX - r.below(3) as i32,
                3 => 46341 + r.below(3) as i32,
                _ => 1 + r.below(2) as i32,
            }
        };
        let (w, h) = (pick(&mut r), pick(&mut r));
        let big = w > max || h > max;
        unsafe {
            // (no pixels for the sizes that are taken: only the refusal is under test)
            glTexImage2D(GL_TEXTURE_2D, 0, 0x1908, w, h, 0, 0x1908, 0x1401, if big { px.as_ptr() } else { std::ptr::null() });
            assert_eq!(glGetError(), if big { GL_INVALID_VALUE } else { 0 }, "glTexImage2D {w}x{h} ({k})");
            if big {
                glCopyTexImage2D(GL_TEXTURE_2D, 0, 0x1908, 0, 0, w, h, 0);
                assert_eq!(glGetError(), GL_INVALID_VALUE, "glCopyTexImage2D {w}x{h}");
                glReadPixels(0, 0, w, h, 0x1908, 0x1401, out.as_mut_ptr());
                assert_eq!(glGetError(), GL_INVALID_VALUE, "glReadPixels {w}x{h}");
            }
        }
    }
}

/// Random contexts made, made current on windows and destroyed: every window a context draws
/// to has its buffers, and after a context is destroyed no other window has (they were kept
/// for every window ever drawn to).
#[test]
fn window_buffers_go_with_their_last_context() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for seed in 1..=60u64 {
        *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 5);
        // (the model: each live context's window, 0 for none yet)
        let mut model: HashMap<u32, u32> = HashMap::new();
        for step in 0..120 {
            let live: Vec<u32> = model.keys().copied().collect();
            let mut destroyed = false;
            match r.below(5) {
                0 | 1 => {
                    model.insert(vngl_context_create(0), 0);
                }
                2 | 3 if !live.is_empty() => {
                    let (c, hwnd) = (live[r.below(live.len())], 100 + r.below(4) as u32);
                    assert_eq!(vngl_make_current(c, hwnd, 64, 32), 1);
                    model.insert(c, hwnd);
                }
                _ if !live.is_empty() => {
                    let c = live[r.below(live.len())];
                    vngl_context_destroy(c);
                    model.remove(&c);
                    destroyed = true;
                }
                _ => {}
            }
            let mut want: Vec<u32> = model.values().copied().filter(|&h| h != 0).collect();
            want.sort();
            want.dedup();
            let mut got: Vec<u32> = GL.lock().unwrap().as_ref().map(|g| g.drawables.keys().copied().collect()).unwrap_or_default();
            got.sort();
            assert!(want.iter().all(|h| got.contains(h)), "seed {seed} step {step}: {got:?} {want:?}");
            if destroyed {
                assert_eq!(got, want, "seed {seed} step {step}");
            }
        }
        vngl_make_current(0, 0, 0, 0);
    }
}

/// glMapBuffer's blocks, as the test's guest allocator hands them out: live ones by address.
static BLOCKS: std::sync::Mutex<Option<HashMap<usize, usize>>> = std::sync::Mutex::new(None);
unsafe extern "C" fn block_alloc(n: u32) -> *mut u8 {
    let p = libc::malloc(n as usize) as *mut u8;
    BLOCKS.lock().unwrap().get_or_insert_with(HashMap::new).insert(p as usize, n as usize);
    p
}
unsafe extern "C" fn block_free(p: *mut u8) {
    let n = BLOCKS.lock().unwrap().as_mut().unwrap().remove(&(p as usize)).expect("freed twice, or never given");
    // (whoever still reads it finds this)
    std::ptr::write_bytes(p, 0xdd, n);
    libc::free(p as *mut libc::c_void);
}

/// Random glBufferData / glBufferSubData / glMapBuffer / writes through the mapping /
/// glUnmapBuffer / draws / glDeleteBuffers on a few buffers against a model: a mapping is a block of the
/// guest allocator's (never this side's Vec), as long as the buffer; what is written through it
/// is the buffer's contents at the unmap and at a draw before it; new storage, a delete and
/// the unmap give the block back exactly once, and at the end none is left.
#[test]
fn mapped_buffers_live_in_guest_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    vngl_set_alloc(block_alloc, block_free);
    for seed in 1..=60u64 {
        fresh();
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 9);
        const T: u32 = 0x8892; // GL_ARRAY_BUFFER
        // (the model: a buffer's contents, and its mapping's address when mapped)
        let mut model: HashMap<u32, (Vec<u8>, usize)> = HashMap::new();
        for step in 0..300 {
            let ids: Vec<u32> = model.keys().copied().collect();
            let at = format!("seed {seed} step {step}");
            if ids.is_empty() || r.below(12) == 0 {
                let mut id = 0;
                unsafe { glGenBuffers(1, &mut id) };
                glBindBuffer(T, id);
                model.insert(id, (Vec::new(), 0));
                continue;
            }
            let id = ids[r.below(ids.len())];
            glBindBuffer(T, id);
            let (data, mapped) = model.get_mut(&id).unwrap();
            match r.below(9) {
                0 => {
                    let n = r.below(40);
                    let src: Vec<u8> = (0..n).map(|_| r.next() as u8).collect();
                    unsafe { glBufferData(T, n as isize, if r.chance(30) { std::ptr::null() } else { src.as_ptr() }, 0x88E4) };
                    // (glBufferData(NULL) is zeros here; the mapping, if any, is gone)
                    *data = GL.lock().unwrap().as_ref().unwrap().buffers[&id].data.clone();
                    assert_eq!(data.len(), n, "{at}");
                    *mapped = 0;
                }
                1 if !data.is_empty() => {
                    let o = r.below(data.len());
                    let n = r.below(data.len() - o + 1);
                    let src: Vec<u8> = (0..n).map(|_| r.next() as u8).collect();
                    unsafe { glBufferSubData(T, o as isize, n as isize, src.as_ptr()) };
                    data[o..o + n].copy_from_slice(&src);
                }
                2 | 3 => {
                    let p = glMapBuffer(T, 0x88BA) as usize;
                    if *mapped != 0 {
                        assert_eq!((p, glGetError()), (0, GL_INVALID_OPERATION), "{at}: mapped twice");
                    } else {
                        let blocks = BLOCKS.lock().unwrap();
                        assert!(blocks.as_ref().unwrap().get(&p).is_some_and(|&n| n >= data.len()), "{at}: not a guest block of the buffer's size");
                        assert_eq!(unsafe { std::slice::from_raw_parts(p as *const u8, data.len()) }, &data[..], "{at}");
                        *mapped = p;
                    }
                }
                4 | 5 if *mapped != 0 && !data.is_empty() => {
                    // the program writes through the pointer
                    let o = r.below(data.len());
                    let v = r.next() as u8;
                    unsafe { *((*mapped + o) as *mut u8) = v };
                    data[o] = v;
                }
                6 => {
                    assert_eq!(glUnmapBuffer(T), 1);
                    *mapped = 0;
                    assert_eq!(GL.lock().unwrap().as_ref().unwrap().buffers[&id].data, *data, "{at}: after the unmap");
                }
                7 => {
                    // (a draw of nothing: buffers still mapped are read as they are now)
                    glDrawArrays(4, 0, 0);
                    assert_eq!(GL.lock().unwrap().as_ref().unwrap().buffers[&id].data, *data, "{at}: at a draw");
                }
                8 if r.chance(40) => {
                    unsafe { glDeleteBuffers(1, &id) };
                    model.remove(&id);
                }
                _ => {}
            }
            // the live blocks are the mappings, no more and no fewer
            let mut want: Vec<usize> = model.values().map(|m| m.1).filter(|&p| p != 0).collect();
            let mut got: Vec<usize> = BLOCKS.lock().unwrap().as_ref().map(|b| b.keys().copied().collect()).unwrap_or_default();
            want.sort();
            got.sort();
            assert_eq!(got, want, "{at}");
        }
        for id in model.keys() {
            unsafe { glDeleteBuffers(1, id) };
        }
        assert!(BLOCKS.lock().unwrap().as_ref().is_none_or(|b| b.is_empty()), "seed {seed}: blocks left");
    }
    *GUEST.lock().unwrap() = None;
}

/// Client arrays whose element would end past the program's memory (a pointer near 2 GB, a
/// stride that wraps 32 bits) read the default instead of anywhere; random pointers, strides,
/// sizes and vertex numbers around the end, and no read at all past it (the reads would fault
/// in this test: nothing is mapped there).
#[test]
fn client_arrays_stay_in_guest_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fresh();
    let g = GL.lock().unwrap();
    let g = g.as_ref().unwrap();
    let mut r = Rng(0xa11a_57e5);
    let def = [9.0, 8.0, 7.0, 6.0];
    for _ in 0..20000 {
        let a = Array {
            on: true,
            size: [1, 2, 3, 4, 0x80E1][r.below(5)],
            ty: [0x1400, 0x1401, 0x1402, 0x1403, 0x1404, 0x1405, 0x1406][r.below(7)],
            stride: [0, 4, 16, 0x1000, i32::MAX][r.below(5)],
            ptr: (1u32 << 31) - r.below(64) as u32,
            ..Array::default()
        };
        let i = [0, 1, 1000, u32::MAX][r.below(4)];
        // (those that end inside are not this test's: nothing is mapped there to read)
        let ts = [1, 1, 2, 2, 4, 4, 4][[0x1400, 0x1401, 0x1402, 0x1403, 0x1404, 0x1405, 0x1406].iter().position(|&t| t == a.ty).unwrap()];
        let n = if a.size == 0x80E1 { 4 } else { a.size as u64 };
        let stride = if a.stride > 0 { a.stride as u64 } else { ts * n };
        if a.ptr as u64 + stride * i as u64 + ts * n <= 1 << 31 {
            continue;
        }
        assert_eq!(fetch(g, &a, i, def), def, "{:?} {} {:#x} vertex {i}", a.size, a.stride, a.ptr);
    }
}
