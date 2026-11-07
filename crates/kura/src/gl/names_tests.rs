//! Random glGen*, glDelete*, glBind*, glIs*, framebuffer attachments, binding queries and
//! glGetError on one context, recorded as a trace for TLC to check against
//! formal/tla/GlNames.tla (formal/trace.sh): what each call gave back.
//!
//! `IPPAI_TRACE_DIR=dir` writes `dir/gl-<seed>/`; `IPPAI_TRACE_SEEDS=n` (default 20).

use super::*;
use crate::d3d9::trace_tests::{seeds, write_module, Rng, SERIAL};
use std::fmt::Write as _;

/// Kinds: 0 textures, 1 buffers, 2 framebuffers, 3 renderbuffers.
const KINDS: [&str; 4] = ["tex", "buf", "fbo", "rb"];

#[derive(Default)]
struct Step {
    op: &'static str,
    kind: u32,
    /// names passed (Delete) or given back (Gen)
    names: Vec<i64>,
    /// a name; a count (Gen, Delete); a target; a unit
    v: i64,
    t: &'static str,
    got: i64,
}

unsafe fn gen(kind: u32, n: i32, out: *mut u32) {
    match kind {
        0 => glGenTextures(n, out),
        1 => glGenBuffers(n, out),
        2 => glGenFramebuffers(n, out),
        _ => glGenRenderbuffers(n, out),
    }
}

unsafe fn delete(kind: u32, n: i32, p: *const u32) {
    match kind {
        0 => glDeleteTextures(n, p),
        1 => glDeleteBuffers(n, p),
        2 => glDeleteFramebuffers(n, p),
        _ => glDeleteRenderbuffers(n, p),
    }
}

fn get(p: u32) -> i64 {
    let mut v = [0i32; 4];
    unsafe { glGetIntegerv(p, v.as_mut_ptr()) };
    v[0] as i64
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 5);
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ctx = vngl_context_create(0);
    vngl_make_current(ctx, 0, 0, 0);
    let mut steps = Vec::new();
    // names seen so far, of every kind (what the random calls pick from)
    let mut seen: Vec<u32> = Vec::new();
    for _ in 0..200 {
        let kind = r.below(4) as u32;
        let name = |r: &mut Rng, seen: &[u32]| -> u32 {
            match r.below(10) {
                0 => 0,
                1 => 1 + r.below(40) as u32,
                _ if !seen.is_empty() => seen[r.below(seen.len())],
                _ => 1,
            }
        };
        let mut t = Step {
            kind,
            ..Default::default()
        };
        match r.below(11) {
            0 | 1 => {
                let n = if r.chance(5) { -1 } else { r.below(3) as i32 };
                let mut out = [0u32; 4];
                unsafe { gen(kind, n, out.as_mut_ptr()) };
                let got = &out[..n.max(0) as usize];
                seen.extend(got);
                (t.op, t.v, t.names) = ("Gen", n as i64, got.iter().map(|&x| x as i64).collect());
            }
            2 => {
                let n = if r.chance(5) { -1 } else { r.below(3) as i32 };
                let ids: Vec<u32> = (0..n.max(0)).map(|_| name(&mut r, &seen)).collect();
                unsafe { delete(kind, n, ids.as_ptr()) };
                (t.op, t.v, t.names) =
                    ("Delete", n as i64, ids.iter().map(|&x| x as i64).collect());
            }
            3 | 4 => {
                let id = name(&mut r, &seen);
                if !seen.contains(&id) && id != 0 {
                    seen.push(id);
                }
                t.t = match kind {
                    0 => {
                        glBindTexture(GL_TEXTURE_2D, id);
                        "tex"
                    }
                    1 => {
                        let (tg, n) = if r.chance(50) {
                            (GL_ARRAY_BUFFER, "array")
                        } else {
                            (GL_ELEMENT_ARRAY_BUFFER, "element")
                        };
                        glBindBuffer(tg, id);
                        n
                    }
                    2 => {
                        let (tg, n) = [
                            (GL_FRAMEBUFFER, "both"),
                            (GL_DRAW_FRAMEBUFFER, "draw"),
                            (GL_READ_FRAMEBUFFER, "read"),
                        ][r.below(3)];
                        glBindFramebuffer(tg, id);
                        n
                    }
                    _ => {
                        glBindRenderbuffer(0x8D41, id);
                        "rb"
                    }
                };
                (t.op, t.v) = ("Bind", id as i64);
            }
            5 => {
                let u = r.below(2) as u32;
                glActiveTexture(GL_TEXTURE0 + u);
                (t.op, t.v) = ("Active", u as i64);
            }
            6 => {
                // color: a texture, depth: a renderbuffer, on the draw framebuffer
                let id = name(&mut r, &seen);
                if r.chance(50) {
                    glFramebufferTexture2D(
                        GL_DRAW_FRAMEBUFFER,
                        GL_COLOR_ATTACHMENT0,
                        GL_TEXTURE_2D,
                        id,
                        0,
                    );
                    (t.op, t.t, t.v) = ("Attach", "color", id as i64);
                } else {
                    glFramebufferRenderbuffer(GL_DRAW_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, 0x8D41, id);
                    (t.op, t.t, t.v) = ("Attach", "depth", id as i64);
                }
            }
            7 => {
                let id = name(&mut r, &seen);
                let got = match kind {
                    0 => glIsTexture(id),
                    1 => glIsBuffer(id),
                    2 => glIsFramebuffer(id),
                    _ => glIsRenderbuffer(id),
                };
                (t.op, t.v, t.got) = ("Is", id as i64, got as i64);
            }
            8 => {
                let (p, n) = [
                    (0x8069, "tex"),
                    (0x8894, "array"),
                    (0x8895, "element"),
                    (0x8CA6, "draw"),
                    (0x8CAA, "read"),
                    (0x8CA7, "rb"),
                    (0x84E0, "active"),
                ][r.below(7)];
                let mut v = get(p);
                if n == "active" {
                    v -= GL_TEXTURE0 as i64;
                }
                (t.op, t.t, t.got) = ("Get", n, v);
            }
            9 => {
                if get(0x8CA6) == 0 {
                    continue;
                }
                let color = r.chance(50);
                let mut v = 0;
                unsafe {
                    glGetFramebufferAttachmentParameteriv(
                        GL_DRAW_FRAMEBUFFER,
                        if color {
                            GL_COLOR_ATTACHMENT0
                        } else {
                            GL_DEPTH_ATTACHMENT
                        },
                        0x8CD1,
                        &mut v,
                    )
                };
                (t.op, t.t, t.got) = ("GetAttach", if color { "color" } else { "depth" }, v as i64);
            }
            _ => {
                (t.op, t.got) = ("GetError", glGetError() as i64);
            }
        }
        steps.push(t);
    }
    vngl_make_current(0, 0, 0, 0);
    vngl_context_destroy(ctx);
    let mut s = String::from("---- MODULE GlData ----\nEXTENDS Integers\nTrace == <<\n");
    let seq = |v: &[i64]| {
        v.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    for (i, t) in steps.iter().enumerate() {
        let _ = write!(
            s,
            "  [op |-> \"{}\", kind |-> \"{}\", names |-> <<{}>>, v |-> {}, t |-> \"{}\", got |-> {}]{}\n",
            t.op, KINDS[t.kind as usize], seq(&t.names), t.v, t.t, t.got,
            if i + 1 < steps.len() { "," } else { "" }
        );
    }
    s.push_str(">>\n====\n");
    write_module(&format!("gl-{seed}"), "GlNames", "GlData", &s);
    check(&steps, seed);
}

/// GlNames.tla in Rust (the test fails here too, with the step, when TLC is not run).
fn check(steps: &[Step], seed: u64) {
    use std::collections::{HashMap, HashSet};
    // per kind: names in use (reserved by a Gen or with an object), names with an object
    let mut used: [HashSet<i64>; 4] = Default::default();
    let mut obj: [HashSet<i64>; 4] = Default::default();
    let mut b: HashMap<&str, i64> = HashMap::new();
    let mut units = [0i64; 2];
    let mut active = 0usize;
    let mut att: HashMap<i64, (i64, i64)> = HashMap::new();
    let mut error = 0i64;
    let bind = |b: &HashMap<&str, i64>, k: &str| *b.get(k).unwrap_or(&0);
    for (n, t) in steps.iter().enumerate() {
        let k = t.kind as usize;
        let mut set_err = |e: i64| {
            if error == 0 {
                error = e;
            }
        };
        let fail = |what: &str| panic!("seed {seed} step {n}: {} {}: {what}", t.op, KINDS[k]);
        match t.op {
            "Gen" => {
                if t.v < 0 {
                    set_err(0x501);
                } else {
                    let fresh: HashSet<i64> = t.names.iter().copied().collect();
                    if fresh.len() != t.names.len()
                        || t.names.iter().any(|x| *x == 0 || used[k].contains(x))
                    {
                        fail(&format!("gave {:?}, in use {:?}", t.names, used[k]));
                    }
                    used[k].extend(fresh);
                }
            }
            "Delete" => {
                if t.v < 0 {
                    set_err(0x501);
                }
                for &id in &t.names {
                    if id == 0 || !used[k].contains(&id) {
                        continue;
                    }
                    used[k].remove(&id);
                    if obj[k].remove(&id) {
                        match k {
                            0 => {
                                for u in units.iter_mut().filter(|u| **u == id) {
                                    *u = 0;
                                }
                                for a in att.values_mut().filter(|a| a.0 == id) {
                                    a.0 = 0;
                                }
                            }
                            1 => {
                                for tg in ["array", "element"] {
                                    if bind(&b, tg) == id {
                                        b.insert(tg, 0);
                                    }
                                }
                            }
                            2 => {
                                att.remove(&id);
                                for tg in ["draw", "read"] {
                                    if bind(&b, tg) == id {
                                        b.insert(tg, 0);
                                    }
                                }
                            }
                            _ => {
                                if bind(&b, "rb") == id {
                                    b.insert("rb", 0);
                                }
                                for a in att.values_mut().filter(|a| a.1 == id) {
                                    a.1 = 0;
                                }
                            }
                        }
                    }
                }
            }
            "Bind" => {
                if t.v != 0 {
                    used[k].insert(t.v);
                    obj[k].insert(t.v);
                    if k == 2 {
                        att.entry(t.v).or_insert((0, 0));
                    }
                }
                match t.t {
                    "tex" => units[active] = t.v,
                    "both" => {
                        b.insert("draw", t.v);
                        b.insert("read", t.v);
                    }
                    x => {
                        b.insert(x, t.v);
                    }
                }
            }
            "Active" => active = t.v as usize,
            "Attach" => {
                let fb = bind(&b, "draw");
                let (kk, slot) = if t.t == "color" { (0, 0) } else { (3, 1) };
                if fb == 0 || (t.v != 0 && !obj[kk].contains(&t.v)) {
                    set_err(0x502);
                } else {
                    let a = att.get_mut(&fb).unwrap();
                    if slot == 0 {
                        a.0 = t.v;
                    } else {
                        a.1 = t.v;
                    }
                }
            }
            "Is" => {
                if t.got != obj[k].contains(&t.v) as i64 {
                    fail(&format!("{} gave {}", t.v, t.got));
                }
            }
            "Get" => {
                let want = match t.t {
                    "tex" => units[active],
                    "active" => active as i64,
                    x => bind(&b, x),
                };
                if t.got != want {
                    fail(&format!("{} is {}, want {want}", t.t, t.got));
                }
            }
            "GetAttach" => {
                let a = att[&bind(&b, "draw")];
                let want = if t.t == "color" { a.0 } else { a.1 };
                if t.got != want {
                    fail(&format!("{} is {}, want {want}", t.t, t.got));
                }
            }
            "GetError" => {
                if t.got != error {
                    fail(&format!("{:#x}, want {error:#x}", t.got));
                }
                error = 0;
            }
            _ => {}
        }
    }
}

#[test]
fn gl_names_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for seed in 0..seeds() {
        run(seed);
    }
}
