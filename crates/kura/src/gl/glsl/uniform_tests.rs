//! Uniform arrays and attribute locations against GL 2.1 section 2.15.3: random
//! glGetUniformLocation("a[k]") (inside the array, past its end, far past it) and glUniform*v
//! with counts that run past the end, checked against a model where each element holds the
//! last value written to it and writes past the end are dropped; what the program keeps never
//! grows past the declared array. And glBindAttribLocation with an index past
//! GL_MAX_VERTEX_ATTRIBS is GL_INVALID_VALUE and changes nothing.

use super::*;
use crate::d3d9::trace_tests::{alloc, SERIAL};
use proptest::prelude::*;

const VS: &str = "uniform vec4 a[3];\nuniform mat4 m[2];\nuniform float s;\nattribute vec4 extra;\n\
void main() { gl_Position = gl_Vertex + a[0] + a[1] + a[2] + m[0][0] + m[1][1] + vec4(s) + extra; }";
const FS: &str = "void main() { gl_FragColor = vec4(1.0); }";

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

fn program() -> u32 {
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ctx = vngl_context_create(0);
    vngl_make_current(ctx, 0, 0, 0);
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
    assert!(GL.lock().unwrap().as_ref().unwrap().programs[&p].linked.is_some(), "links");
    p
}

fn location(p: u32, name: &str) -> i32 {
    let c = std::ffi::CString::new(name).unwrap();
    unsafe { glGetUniformLocation(p, c.as_ptr()) }
}

/// (name, elements, words per element)
const UNIFORMS: [(&str, u32, usize); 3] = [("a", 3, 4), ("m", 2, 16), ("s", 1, 1)];

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn uniform_arrays_keep_what_fits(ops in prop::collection::vec((0usize..3, 0u32..8, any::<bool>(), 0i32..8, any::<u32>()), 1..40)) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let p = program();
        let mut model: Vec<Vec<u32>> = UNIFORMS.iter().map(|&(_, n, per)| vec![0; n as usize * per]).collect();
        let mut set: Vec<Vec<bool>> = UNIFORMS.iter().map(|&(_, n, _)| vec![false; n as usize]).collect();
        for (u, k, bracket, count, seed) in ops {
            let (name, n, per) = UNIFORMS[u];
            let loc = location(p, &if bracket || k > 0 { format!("{name}[{k}]") } else { name.to_string() });
            prop_assert_eq!(loc >= 0, k < n, "location of {}[{}]", name, k);
            let data: Vec<f32> = (0..count as usize * per).map(|i| f32::from_bits(seed.wrapping_add(i as u32 * 0x9e37) & 0x3fff_ffff)).collect();
            unsafe {
                match u {
                    0 => glUniform4fv(loc, count, data.as_ptr()),
                    1 => glUniformMatrix4fv(loc, count, 0, data.as_ptr()),
                    _ => glUniform1fv(loc, count, data.as_ptr()),
                }
            }
            if loc >= 0 {
                for e in k..(k + count as u32).min(n) {
                    let (e, d) = (e as usize, (e - k) as usize);
                    for w in 0..per {
                        model[u][e * per + w] = data[d * per + w].to_bits();
                    }
                    set[u][e] = true;
                }
            }
        }
        let g = GL.lock().unwrap();
        let x = &g.as_ref().unwrap().programs[&p];
        for (u, &(name, n, per)) in UNIFORMS.iter().enumerate() {
            let got = x.values.get(name).cloned().unwrap_or_default();
            prop_assert!(got.len() <= n as usize * per, "{} keeps {} words", name, got.len());
            for e in 0..n as usize {
                if set[u][e] {
                    prop_assert_eq!(&got[e * per..e * per + per], &model[u][e * per..e * per + per], "{}[{}]", name, e);
                }
            }
        }
    }
}

#[test]
fn far_elements_and_big_counts_allocate_nothing() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let p = program();
    for n in ["a[3]", "a[100000000]", "a[4294967295]", "s[1]", "m[2]"] {
        assert_eq!(location(p, n), -1, "{n}");
    }
    // (a count of a billion past a[2]: one element is written, the pointer is read for one)
    let one = [1.0f32, 2.0, 3.0, 4.0];
    unsafe { glUniform4fv(location(p, "a[2]"), 1_000_000_000, one.as_ptr()) };
    let g = GL.lock().unwrap();
    let v = &g.as_ref().unwrap().programs[&p].values["a"];
    assert_eq!(v.len(), 12);
    assert_eq!(&v[8..], &one.map(f32::to_bits));
}

#[test]
fn attribute_indices_past_the_limit_are_refused() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let p = program();
    while glGetError() != 0 {}
    let extra = std::ffi::CString::new("extra").unwrap();
    for idx in [16, 17, 1000, u32::MAX] {
        unsafe { glBindAttribLocation(p, idx, extra.as_ptr()) };
        assert_eq!(glGetError(), GL_INVALID_VALUE, "{idx}");
    }
    unsafe { glBindAttribLocation(p, 15, extra.as_ptr()) };
    assert_eq!(glGetError(), 0);
    glLinkProgram(p);
    let g = GL.lock().unwrap();
    let l = g.as_ref().unwrap().programs[&p].linked.as_ref().expect("links");
    assert_eq!(l.attribs.iter().find(|a| a.0 == "extra").map(|a| a.1), Some(15));
    assert!(l.attribs.iter().all(|a| a.1 < MAX_ATTRIBS), "{:?}", l.attribs);
}
