//! Random glCreateShader/glCreateProgram, glCompileShader, glAttachShader/glDetachShader,
//! glLinkProgram, glUseProgram, glDeleteShader/glDeleteProgram (also of names in use, current
//! or attached, of the other kind, of nothing), glIs*, glGet*iv and glGetError on one
//! context, recorded as a trace for TLC to check against formal/tla/GlPrograms.tla
//! (formal/trace.sh).
//!
//! `IPPAI_TRACE_DIR=dir` writes `dir/glp-<seed>/`; `IPPAI_TRACE_SEEDS=n` (default 20).

use super::*;
use crate::d3d9::trace_tests::{alloc, seeds, write_module, Rng, SERIAL};
use std::fmt::Write as _;

#[derive(Default)]
struct Step {
    op: &'static str,
    a: i64,
    b: i64,
    got: i64,
}

const VS: &str = "void main() { gl_Position = gl_Vertex; }";
const FS: &str = "void main() { gl_FragColor = vec4(1.0, 0.5, 0.25, 1.0); }";

fn iv(f: unsafe extern "C" fn(u32, u32, *mut i32), n: u32, p: u32) -> i64 {
    let mut v = -1;
    unsafe { f(n, p, &mut v) };
    v as i64
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 7);
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ctx = vngl_context_create(0);
    vngl_make_current(ctx, 0, 0, 0);
    // (sources and the array of their addresses in guest memory: 32-bit addresses)
    let guest = |text: &str| unsafe {
        let p = alloc(text.len() as u32 + 1);
        std::ptr::copy_nonoverlapping(text.as_ptr(), p, text.len());
        *p.add(text.len()) = 0;
        let a = alloc(8) as *mut u32;
        *a = p as usize as u32;
        a as *const *const c_char
    };
    let (vs, fs) = (guest(VS), guest(FS));
    let mut steps = Vec::new();
    let mut names: Vec<u32> = Vec::new();
    for _ in 0..250 {
        let pick = |r: &mut Rng, names: &[u32]| -> u32 {
            match r.below(12) {
                0 => 0,
                1 => 1 + r.below(30) as u32,
                _ if !names.is_empty() => names[r.below(names.len())],
                _ => 1,
            }
        };
        let (a, b) = (pick(&mut r, &names), pick(&mut r, &names));
        let mut t = Step { a: a as i64, b: b as i64, ..Default::default() };
        match r.below(20) {
            0 | 1 => {
                let pixel = r.chance(50);
                let n = glCreateShader(if pixel { 0x8B30 } else { GL_VERTEX_SHADER });
                // (the source is set at once; a failed glShaderSource has its own errors)
                unsafe { glShaderSource(n, 1, if pixel { fs } else { vs }, std::ptr::null()) };
                names.push(n);
                (t.op, t.a, t.got) = ("CreateShader", pixel as i64, n as i64);
            }
            2 => {
                let n = glCreateProgram();
                names.push(n);
                (t.op, t.got) = ("CreateProgram", n as i64);
                // (half the time a whole program: two shaders made, compiled, attached, linked)
                if r.chance(60) {
                    steps.push(t);
                    for pixel in [false, true] {
                        let x = glCreateShader(if pixel { 0x8B30 } else { GL_VERTEX_SHADER });
                        unsafe { glShaderSource(x, 1, if pixel { fs } else { vs }, std::ptr::null()) };
                        names.push(x);
                        steps.push(Step { op: "CreateShader", a: pixel as i64, got: x as i64, ..Default::default() });
                        glCompileShader(x);
                        steps.push(Step { op: "Compile", a: x as i64, ..Default::default() });
                        glAttachShader(n, x);
                        steps.push(Step { op: "Attach", a: n as i64, b: x as i64, ..Default::default() });
                    }
                    glLinkProgram(n);
                    t = Step { op: "Link", a: n as i64, ..Default::default() };
                    if r.chance(70) {
                        steps.push(t);
                        glUseProgram(n);
                        t = Step { op: "Use", a: n as i64, ..Default::default() };
                    }
                }
            }
            3 | 4 => {
                glCompileShader(a);
                t.op = "Compile";
            }
            5 => {
                glDeleteShader(a);
                t.op = "DeleteShader";
            }
            6 => {
                // (often the program in use: flagged, it lives on)
                let a = if r.chance(50) { with(|_, c| c.program) } else { a };
                glDeleteProgram(a);
                (t.op, t.a) = ("DeleteProgram", a as i64);
            }
            7 | 8 | 9 => {
                glAttachShader(a, b);
                t.op = "Attach";
            }
            10 => {
                glDetachShader(a, b);
                t.op = "Detach";
            }
            11 | 12 => {
                glLinkProgram(a);
                t.op = "Link";
            }
            13 => {
                let p = if r.chance(20) { 0 } else { a };
                glUseProgram(p);
                (t.op, t.a) = ("Use", p as i64);
            }
            14 => (t.op, t.got) = (["IsShader", "IsProgram"][r.below(2)], 0),
            15 => {
                let k = r.below(5);
                let a = if r.chance(40) { with(|_, c| c.program) } else { a };
                t.a = a as i64;
                t.op = ["ShaderDeleted", "Compiled", "ProgramDeleted", "Linked", "Attached"][k];
                t.got = match k {
                    0 => iv(glGetShaderiv, a, 0x8B80),
                    1 => iv(glGetShaderiv, a, 0x8B81),
                    2 => iv(glGetProgramiv, a, 0x8B80),
                    3 => iv(glGetProgramiv, a, 0x8B82),
                    _ => iv(glGetProgramiv, a, 0x8B85),
                };
            }
            16 | 17 => {
                let mut v = [0i32; 4];
                unsafe { glGetIntegerv(0x8B8D, v.as_mut_ptr()) };
                (t.op, t.got) = ("Current", v[0] as i64);
            }
            _ => (t.op, t.got) = ("GetError", glGetError() as i64),
        }
        if t.op.starts_with("Is") {
            t.got = if t.op == "IsShader" { glIsShader(a) } else { glIsProgram(a) } as i64;
        }
        steps.push(t);
    }
    vngl_make_current(0, 0, 0, 0);
    vngl_context_destroy(ctx);
    let mut s = String::from("---- MODULE GlpData ----\nEXTENDS Integers\nTrace == <<\n");
    for (i, t) in steps.iter().enumerate() {
        let _ = writeln!(s, "  [op |-> \"{}\", a |-> {}, b |-> {}, got |-> {}]{}", t.op, t.a, t.b, t.got, if i + 1 < steps.len() { "," } else { "" });
    }
    s.push_str(">>\n====\n");
    write_module(&format!("glp-{seed}"), "GlPrograms", "GlpData", &s);
    check(&steps, seed);
}

/// GlPrograms.tla in Rust (the test fails here too, with the step, when TLC is not run).
fn check(steps: &[Step], seed: u64) {
    use std::collections::{BTreeSet, HashMap};
    struct Sh {
        pixel: bool,
        compiled: bool,
        deleted: bool,
    }
    struct Pr {
        att: BTreeSet<i64>,
        linked: bool,
        deleted: bool,
    }
    let (mut sh, mut pr) = (HashMap::<i64, Sh>::new(), HashMap::<i64, Pr>::new());
    let (mut cur, mut error) = (0i64, 0i64);
    for (n, t) in steps.iter().enumerate() {
        let kind = |sh: &HashMap<i64, Sh>, pr: &HashMap<i64, Pr>, x: i64, shader: bool| -> i64 {
            if shader && sh.contains_key(&x) || !shader && pr.contains_key(&x) {
                0
            } else if sh.contains_key(&x) || pr.contains_key(&x) {
                0x502
            } else {
                0x501
            }
        };
        let collect = |sh: &mut HashMap<i64, Sh>, pr: &mut HashMap<i64, Pr>, cur: i64| {
            pr.retain(|id, p| !p.deleted || *id == cur);
            let att: BTreeSet<i64> = pr.values().flat_map(|p| p.att.iter().copied()).collect();
            sh.retain(|id, s| !s.deleted || att.contains(id));
        };
        let mut e = 0;
        let mut want: Option<i64> = None;
        let q = |e: &mut i64, k: i64, v: i64| -> Option<i64> {
            if k != 0 {
                *e = k;
                Some(-1)
            } else {
                Some(v)
            }
        };
        match t.op {
            "CreateShader" | "CreateProgram" => {
                if t.got == 0 || sh.contains_key(&t.got) || pr.contains_key(&t.got) {
                    panic!("seed {seed} step {n}: {} gave {}, in use", t.op, t.got);
                }
                if t.op == "CreateShader" {
                    sh.insert(t.got, Sh { pixel: t.a == 1, compiled: false, deleted: false });
                } else {
                    pr.insert(t.got, Pr { att: BTreeSet::new(), linked: false, deleted: false });
                }
            }
            "Compile" => match kind(&sh, &pr, t.a, true) {
                0 => sh.get_mut(&t.a).unwrap().compiled = true,
                k => e = k,
            },
            "DeleteShader" | "DeleteProgram" if t.a != 0 => {
                let s = t.op == "DeleteShader";
                match kind(&sh, &pr, t.a, s) {
                    0 if s => sh.get_mut(&t.a).unwrap().deleted = true,
                    0 => pr.get_mut(&t.a).unwrap().deleted = true,
                    k => e = k,
                }
                collect(&mut sh, &mut pr, cur);
            }
            "Attach" | "Detach" => {
                e = kind(&sh, &pr, t.a, false);
                if e == 0 {
                    e = kind(&sh, &pr, t.b, true);
                }
                if e == 0 {
                    let att = &mut pr.get_mut(&t.a).unwrap().att;
                    if (t.op == "Attach") == att.contains(&t.b) {
                        e = 0x502;
                    } else if t.op == "Attach" {
                        att.insert(t.b);
                    } else {
                        att.remove(&t.b);
                        collect(&mut sh, &mut pr, cur);
                    }
                }
            }
            "Link" => match kind(&sh, &pr, t.a, false) {
                0 => {
                    let ok = [false, true].iter().all(|&px| {
                        let of: Vec<&i64> = pr[&t.a].att.iter().filter(|x| sh.get(x).is_some_and(|s| s.pixel == px)).collect();
                        of.len() == 1 && sh[of[0]].compiled
                    });
                    pr.get_mut(&t.a).unwrap().linked = ok;
                }
                k => e = k,
            },
            "Use" => {
                if t.a == 0 {
                    cur = 0;
                    collect(&mut sh, &mut pr, cur);
                } else {
                    e = kind(&sh, &pr, t.a, false);
                    if e == 0 && !pr[&t.a].linked {
                        e = 0x502;
                    }
                    if e == 0 {
                        cur = t.a;
                        collect(&mut sh, &mut pr, cur);
                    }
                }
            }
            "IsShader" => want = Some(sh.contains_key(&t.a) as i64),
            "IsProgram" => want = Some(pr.contains_key(&t.a) as i64),
            "ShaderDeleted" | "Compiled" => {
                let v = sh.get(&t.a).map_or(0, |s| if t.op == "Compiled" { s.compiled } else { s.deleted } as i64);
                want = q(&mut e, kind(&sh, &pr, t.a, true), v);
            }
            "ProgramDeleted" | "Linked" | "Attached" => {
                let v = pr.get(&t.a).map_or(0, |p| match t.op {
                    "Linked" => p.linked as i64,
                    "Attached" => p.att.len() as i64,
                    _ => p.deleted as i64,
                });
                want = q(&mut e, kind(&sh, &pr, t.a, false), v);
            }
            "Current" => want = Some(cur),
            "GetError" => {
                want = Some(error);
                error = 0;
            }
            _ => {}
        }
        if error == 0 {
            error = e;
        }
        if let Some(w) = want {
            assert_eq!(t.got, w, "seed {seed} step {n}: {} {} {}", t.op, t.a, t.b);
        }
    }
}

#[test]
fn gl_programs_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for seed in 0..seeds() {
        run(seed);
    }
}
