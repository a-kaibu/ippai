//! The render optimizations of D3d (deferred clears into loadOps, stencil cleared and
//! discarded, batches the same as the last one not run, Siglus segments drawn again only where
//! they changed) against plain execution, on the model of WebGPU (render/model.rs): random
//! Direct3D 9 command streams run both ways must make the same observations (every presented
//! frame, every readback) and break no WebGPU rule.

use super::*;
use crate::render::model::{self, Obs};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // (xorshift64*)
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next() >> 33) as u32 % n.max(1)
    }
    fn chance(&mut self, pct: u32) -> bool {
        self.below(100) < pct
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u32) as usize]
    }
}

fn fnv(b: &[u8]) -> u32 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ x as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h as u32 ^ (h >> 32) as u32
}

const VS: &str = r#"
struct U { m: array<vec4f, 4> };
@group(0) @binding(0) var<uniform> u: U;
@vertex fn main(@location(0) p: vec4f) -> @builtin(position) vec4f { return p + u.m[0]; }
"#;
const PS_COLOR: &str = r#"
struct U { m: array<vec4f, 4> };
@group(0) @binding(1) var<uniform> u: U;
@fragment fn main() -> @location(0) vec4f { return u.m[0]; }
"#;
const PS_TEX: &str = r#"
struct U { m: array<vec4f, 4> };
@group(0) @binding(1) var<uniform> u: U;
@group(1) @binding(0) var t0: texture_2d<f32>;
@group(1) @binding(1) var s0: sampler;
@group(1) @binding(2) var t1: texture_2d<f32>;
@group(1) @binding(3) var s1: sampler;
@fragment fn main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  return textureSample(t0, s0, p.xy) * textureSample(t1, s1, p.yx) + u.m[0];
}
"#;

/// Textures of the scenario: (id, width, height, levels, format code)
const TEXTURES: [(u32, u32, u32, u32, u32); 6] = [
    (1, 16, 16, 1, 0), // back buffer
    (2, 16, 16, 1, 0), // a render target the frame samples
    (3, 8, 8, 2, 0),   // a picture
    (4, 16, 16, 1, 1), // depth stencil
    (5, 16, 16, 1, 4), // rgba8unorm target
    (6, 16, 16, 1, 0), // another layer target
];
const DS_ID: u32 = 4;

#[derive(Clone, Copy)]
struct PipeInfo {
    id: u32,
    fmt: u32,
    depth: bool,
}

/// One random scenario: its batches (commands, data, blob).
struct Gen {
    r: Rng,
    pipes: Vec<PipeInfo>,
    req: u32,
    /// the frames made, to send again as they were or a little changed
    frames: Vec<(Vec<u32>, Vec<u8>, Vec<u8>)>,
}

struct Batch {
    c: Vec<u32>,
    data: Vec<u8>,
    blob: Vec<u8>,
    target: [u32; 3],
}

impl Batch {
    fn new() -> Batch {
        Batch { c: Vec::new(), data: Vec::new(), blob: Vec::new(), target: [0; 3] }
    }
    fn data(&mut self, b: &[u8], align: usize) -> u32 {
        let at = self.data.len().div_ceil(align) * align;
        self.data.resize(at, 0);
        self.data.extend_from_slice(b);
        at as u32
    }
    fn blob(&mut self, b: &[u8]) -> u32 {
        let at = self.blob.len();
        self.blob.extend_from_slice(b);
        at as u32
    }
}

fn size_of_tex(id: u32) -> (u32, u32, u32, u32) {
    let t = TEXTURES.iter().find(|t| t.0 == id).unwrap();
    (t.1, t.2, t.3, t.4)
}

impl Gen {
    fn new(seed: u64) -> Gen {
        Gen { r: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1), pipes: Vec::new(), req: 0x7000_0000 + (seed as u32 & 0xffff) * 0x1000, frames: Vec::new() }
    }

    fn setup(&mut self) -> Batch {
        let mut b = Batch::new();
        for &(id, w, h, levels, fmt) in &TEXTURES {
            b.c.extend_from_slice(&[CREATE_TEXTURE, id, w, h, levels, fmt, 1]);
        }
        for (id, code) in [(1, VS), (2, PS_COLOR), (3, PS_TEX)] {
            let off = b.blob(code.as_bytes());
            b.c.extend_from_slice(&[CREATE_SHADER, id, off, code.len() as u32]);
        }
        // pipelines: color format x depth x a few states
        let mut id = 1;
        for fmt in [0u32, 4] {
            for depth in [false, true] {
                for k in 0..4u32 {
                    let ps = if k & 1 != 0 { 3 } else { 2 };
                    let blend: [u32; 7] = if k & 2 != 0 { [1, 5, 6, 1, 5, 6, 1] } else { [0; 7] };
                    let topo = self.r.pick(&[3u32, 3, 4]);
                    let cull = self.r.below(3);
                    let mut key = vec![1, ps, topo, 0, cull, fmt, if depth { 1 } else { 0xff }, self.r.below(2), self.r.below(9)];
                    key.extend_from_slice(&blend);
                    key.push(0xf);
                    key.extend_from_slice(&[1, 16, 1, 3, 0, 0]);
                    b.c.extend_from_slice(&[CREATE_PIPELINE, id, key.len() as u32]);
                    b.c.extend_from_slice(&key);
                    self.pipes.push(PipeInfo { id, fmt, depth });
                    id += 1;
                }
            }
        }
        // the picture's pixels
        for level in 0..2 {
            let (w, h) = (8 >> level, 8 >> level);
            let px: Vec<u8> = (0..w * h * 4).map(|_| self.r.below(256) as u8).collect();
            let off = b.blob(&px);
            b.c.extend_from_slice(&[WRITE_TEXTURE, 3, level, 0, 0, w, h, w * 4, off]);
        }
        b
    }

    fn set_target(&mut self, b: &mut Batch, rt: u32, ds: bool) {
        let ds = if ds && size_of_tex(rt).0 == 16 { DS_ID } else { 0 };
        b.target = [rt, 0, ds];
        b.c.extend_from_slice(&[SET_TARGET, rt, 0, ds]);
    }

    fn clear(&mut self, b: &mut Batch, whole: bool) {
        let mut flags = 1;
        if b.target[2] != 0 {
            flags |= self.r.pick(&[0u32, 2, 4, 6, 6]);
            if self.r.chance(20) {
                flags &= !1;
            }
        }
        let color = [self.r.below(4) as f32 / 3.0, 0.5, self.r.below(2) as f32, 1.0];
        let (w, h, _, _) = size_of_tex(b.target[0]);
        let rect = if whole { [0, 0, 0, 0] } else { [self.r.below(w), self.r.below(h), 1 + self.r.below(w), 1 + self.r.below(h)] };
        b.c.extend_from_slice(&[CLEAR, flags, color[0].to_bits(), color[1].to_bits(), color[2].to_bits(), color[3].to_bits(), (self.r.below(3) as f32 / 2.0).to_bits(), self.r.below(256)]);
        b.c.extend_from_slice(&rect);
    }

    fn draw(&mut self, b: &mut Batch) {
        let [rt, _, ds] = b.target;
        if rt == 0 {
            return;
        }
        let (tw, th, _, fmt) = size_of_tex(rt);
        let cands: Vec<PipeInfo> = self.pipes.iter().copied().filter(|p| p.fmt == fmt && p.depth == (ds != 0)).collect();
        let p = self.r.pick(&cands);
        let r = &mut self.r;
        let vp = if r.chance(60) { [0.0, 0.0, tw as f32, th as f32] } else { [r.below(tw) as f32 - 2.0, r.below(th) as f32 - 2.0, 1.0 + r.below(tw + 4) as f32, 1.0 + r.below(th + 4) as f32] };
        let (minz, maxz) = if r.chance(80) { (0.0f32, 1.0f32) } else { (0.25, 0.75) };
        let sc = if r.chance(60) { [0, 0, tw, th] } else { [r.below(tw), r.below(th), r.below(tw + 1), r.below(th + 1)] };
        let vsu: Vec<u8> = (0..64).map(|_| r.below(4) as u8).collect();
        let psu: Vec<u8> = (0..64).map(|_| r.below(4) as u8).collect();
        let nv = r.pick(&[3u32, 4, 6]);
        let verts: Vec<u8> = (0..nv * 16).map(|_| r.below(3) as u8).collect();
        let indexed = r.chance(30);
        let idx: Vec<u8> = (0..nv).flat_map(|k| (k as u16).to_le_bytes()).collect();
        // textures sampled: the picture, other targets, sometimes the target itself
        let mut texs = Vec::new();
        for stage in 0..2u32 {
            if r.chance(70) {
                let t = r.pick(&[3u32, 3, 2, 6, 1, 5]);
                let key = r.pick(&[0x1bu32, 0x5, 0x3 | (3 << 3) | (2 << 6) | (2 << 8) | (2 << 10)]);
                texs.push([stage, t, key]);
            }
        }
        let bbox = if r.chance(15) {
            [u32::MAX; 4]
        } else {
            let (x0, y0) = (r.below(tw), r.below(th));
            [x0, y0, x0 + 1 + r.below(tw - x0), y0 + 1 + r.below(th - y0)]
        };
        let mut h = Vec::new();
        h.extend_from_slice(&verts);
        if indexed {
            h.extend_from_slice(&idx);
        }
        h.extend_from_slice(&vsu);
        h.extend_from_slice(&psu);
        let hash = fnv(&h);
        let vo = b.data(&verts, 4);
        let io = if indexed { b.data(&idx, 4) } else { 0 };
        let vso = b.data(&vsu, 256);
        let pso = b.data(&psu, 256);
        let mut c = vec![DRAW, p.id];
        for v in vp {
            c.push(v.to_bits());
        }
        c.extend_from_slice(&[minz.to_bits(), maxz.to_bits()]);
        c.extend_from_slice(&sc);
        c.extend_from_slice(&[vso, 64, pso, 64, 0xff80_4020, texs.len() as u32]);
        for t in &texs {
            c.extend_from_slice(t);
        }
        c.extend_from_slice(&[1, vo]);
        c.extend_from_slice(&[if indexed { 2 } else { 0 }, io, nv, 0]);
        c.push(hash);
        c.extend_from_slice(&bbox);
        b.c.extend_from_slice(&c);
    }

    fn misc(&mut self, b: &mut Batch, dst: usize) {
        let r = &mut self.r;
        match r.below(5) {
            4 => {
                // a layer target made again (Reset, a resized window)
                let (id, w, h, levels, fmt) = TEXTURES[5];
                b.c.extend_from_slice(&[DESTROY_TEXTURE, id]);
                b.c.extend_from_slice(&[CREATE_TEXTURE, id, w, h, levels, fmt, 1]);
            }
            0 => {
                // a picture update
                let level = r.below(2);
                let (w, h) = (1 + r.below(8 >> level), 1 + r.below(8 >> level));
                let px: Vec<u8> = (0..w * h * 4).map(|_| r.below(256) as u8).collect();
                let (x, y) = (r.below((8 >> level) - w + 1), r.below((8 >> level) - h + 1));
                let off = b.blob(&px);
                b.c.extend_from_slice(&[WRITE_TEXTURE, 3, level, x, y, w, h, w * 4, off]);
            }
            1 => {
                // StretchRect
                let st = r.pick(&[1u32, 2, 3, 5, 6]);
                let dt = r.pick(&[1u32, 2, 5, 6]);
                let (sw, sh, _, _) = size_of_tex(st);
                let (dw, dh, _, _) = size_of_tex(dt);
                let (w, h) = (1 + r.below(sw.min(dw)), 1 + r.below(sh.min(dh)));
                let (sx, sy, dx, dy) = (r.below(sw - w + 1), r.below(sh - h + 1), r.below(dw - w + 1), r.below(dh - h + 1));
                let (dw2, dh2) = if r.chance(50) { (w, h) } else { (1 + r.below(dw - dx), 1 + r.below(dh - dy)) };
                b.c.extend_from_slice(&[COPY, st, 0, sx, sy, w, h, dt, 0, dx, dy, dw2, dh2, r.below(2)]);
            }
            2 => {
                // ColorFill
                let t = r.pick(&[1u32, 2, 5, 6]);
                let (w, h, _, _) = size_of_tex(t);
                let (x, y) = (r.below(w), r.below(h));
                b.c.extend_from_slice(&[FILL, t, 0, x, y, 1 + r.below(w - x), 1 + r.below(h - y), r.next() as u32]);
            }
            _ => {
                // GetRenderTargetData
                let t = r.pick(&[1u32, 2, 3, 5, 6]);
                let (w, h, _, _) = size_of_tex(t);
                self.req += 1;
                b.c.extend_from_slice(&[READBACK, self.req, t, 0, 0, 0, w, h, dst as u32, w * 4]);
            }
        }
    }

    /// A frame: what Siglus does (one target cleared and drawn in layers, then shown) or
    /// anything.
    fn frame(&mut self, dst: usize) -> Batch {
        // the same frame again, or one with a draw changed / added / gone
        if !self.frames.is_empty() && self.r.chance(45) {
            let k = self.frames.len() - 1 - self.r.below(self.frames.len().min(3) as u32) as usize;
            let (c, data, blob) = self.frames[k].clone();
            let mut b = Batch { c, data, blob, target: [0; 3] };
            if self.r.chance(50) {
                self.mutate(&mut b);
            }
            self.frames.push((b.c.clone(), b.data.clone(), b.blob.clone()));
            return b;
        }
        let mut b = Batch::new();
        if self.r.chance(30) {
            // a layer drawn first, sampled by the frame
            let rt = self.r.pick(&[2u32, 6]);
            self.set_target(&mut b, rt, false);
            self.clear(&mut b, true);
            for _ in 0..1 + self.r.below(3) {
                self.draw(&mut b);
            }
        }
        let ds = self.r.chance(40);
        self.set_target(&mut b, 1, ds);
        self.clear(&mut b, true);
        if self.r.chance(15) {
            // (Siglus: color then depth at once)
            self.clear(&mut b, true);
        }
        let n = self.r.below(8);
        for _ in 0..n {
            match self.r.below(10) {
                0 => self.clear(&mut b, false),
                1 => {
                    let rt = self.r.pick(&[1u32, 2, 5, 6]);
                    let ds = self.r.chance(40);
                    self.set_target(&mut b, rt, ds);
                }
                2 => self.misc(&mut b, dst),
                3 if b.target[2] != 0 => {
                    let rt = b.target[0];
                    self.set_target(&mut b, rt, false);
                }
                _ => self.draw(&mut b),
            }
        }
        if self.r.chance(20) {
            self.misc(&mut b, dst);
        }
        let shown = self.r.pick(&[1u32, 1, 1, 2, 5]);
        b.c.extend_from_slice(&[PRESENT, shown, 0, 0, 16, 16, 0, 0, 0]);
        self.frames.push((b.c.clone(), b.data.clone(), b.blob.clone()));
        b
    }

    /// Changes one draw of a frame (its constants or its box), as the next letter of a message.
    fn mutate(&mut self, b: &mut Batch) {
        let mut draws = Vec::new();
        let mut i = 0;
        while i < b.c.len() {
            let n = match b.c[i] {
                CREATE_TEXTURE => 7,
                DESTROY_TEXTURE => 2,
                WRITE_TEXTURE | WRITE_TEXTURE_MEM => 9,
                SET_TARGET => 4,
                CLEAR => 12,
                PRESENT | PRESENT_FLIP => 9,
                COPY => 14,
                READBACK => 10,
                FILL => 8,
                DRAW => {
                    let ntex = b.c[i + 17] as usize;
                    let p = i + 18 + 3 * ntex;
                    draws.push(i);
                    p + 1 + b.c[p] as usize + 4 + 5 - i
                }
                _ => panic!("op {}", b.c[i]),
            };
            i += n;
        }
        let Some(&d) = draws.get(self.r.below(draws.len() as u32) as usize) else { return };
        let ntex = b.c[d + 17] as usize;
        let end = d + 18 + 3 * ntex + 1 + b.c[d + 18 + 3 * ntex] as usize + 4 + 5;
        // new constants (and so a new hash), at a new place in the data
        let pso = b.c[d + 14] as usize;
        let mut psu = b.data[pso..pso + 64].to_vec();
        psu[self.r.below(64) as usize] ^= 1 + self.r.below(255) as u8;
        let at = b.data(&psu, 256);
        b.c[d + 14] = at;
        b.c[end - 5] ^= 1 + self.r.below(1 << 20);
        if self.r.chance(30) && b.c[end - 4] != u32::MAX {
            b.c[end - 2] = (b.c[end - 2] + 1).min(16);
        }
    }
}

/// The commands of a batch, readable.
fn show(c: &[u32]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let (name, n) = match c[i] {
            CREATE_TEXTURE => ("tex", 7),
            DESTROY_TEXTURE => ("destroy", 2),
            CREATE_SHADER => ("shader", 4),
            CREATE_PIPELINE => ("pipe", 3 + c[i + 2] as usize),
            WRITE_TEXTURE => ("write", 9),
            SET_TARGET => ("target", 4),
            CLEAR => ("clear", 12),
            PRESENT => ("present", 9),
            COPY => ("copy", 14),
            READBACK => ("readback", 10),
            FILL => ("fill", 8),
            DRAW => {
                let ntex = c[i + 17] as usize;
                let p = i + 18 + 3 * ntex;
                ("draw", p + 1 + c[p] as usize + 9 - i)
            }
            _ => ("?", c.len() - i),
        };
        let w = &c[i + 1..i + n];
        out.push(match c[i] {
            CREATE_SHADER | CREATE_PIPELINE => format!("{name} {}", w[0]),
            CLEAR => format!("clear flags {} color {:?} z {} s {} rect {:?}", w[0], [f32of(w[1]), f32of(w[2]), f32of(w[3]), f32of(w[4])], f32of(w[5]), w[6], &w[7..11]),
            DRAW => {
                let vp: Vec<f32> = w[1..7].iter().map(|&x| f32of(x)).collect();
                format!("draw pipe {} vp {vp:?} sc {:?} texs {:?} bbox {:?}", w[0], &w[7..11], &w[17..17 + 3 * w[16] as usize], &w[n - 5..])
            }
            _ => format!("{name} {:?}", w),
        });
        i += n;
    }
    out.join("\n  ")
}

/// Runs a scenario's batches on a fresh model, optimized or not; returns what was observed.
fn run(seed: u64, frames: usize, pass_opt: bool, seg: bool) -> (Vec<Obs>, Vec<String>, (u32, u32, u32)) {
    model::reset();
    // (readbacks write to a guest address: 32 bits)
    let dst = unsafe { libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT, -1, 0) };
    assert!(dst != libc::MAP_FAILED);
    let mut g = Gen::new(seed);
    let mut d = D3d::new();
    d.no_pass_opt = !pass_opt;
    d.no_seg = !seg;
    d.skip_check = false;
    let mut obs = Vec::new();
    let setup = g.setup();
    let batches = std::iter::once(setup).chain((0..frames).map(|_| g.frame(dst as usize)).collect::<Vec<_>>());
    for (k, b) in batches.enumerate() {
        if std::env::var("IPPAI_MODEL_DUMP").ok().and_then(|s| s.parse::<u64>().ok()) == Some(seed) {
            eprintln!("batch {k}: {}", show(&b.c));
        }
        d.run(&b.c, &b.data, &b.blob);
        for token in model::with(|m| m.finish_maps()) {
            d.mapped(token, true);
        }
        if let Some(o) = &d.overlay {
            let v = model::with(|m| m.view_values(o.view));
            if std::env::var("IPPAI_MODEL_DUMP").ok().and_then(|s| s.parse::<u64>().ok()) == Some(seed) {
                eprintln!("after batch {k} ({}): {:x} {:x} {:x}", if pass_opt { "opt" } else { "plain" }, v[0], v[128], v[255]);
            }
            obs.push(Obs::Frame(format!("after batch {k}"), v));
        }
    }
    obs.extend(model::with(|m| std::mem::take(&mut m.obs)));
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    let (a, b) = d.seg_stats.get();
    unsafe { libc::munmap(dst, 4096) };
    (obs, errors, (d.skipped.get(), a, b))
}

fn check(seeds: std::ops::Range<u64>, frames: usize) {
    let mut stats = (0, 0, 0);
    for seed in seeds {
        let (want, e1, _) = run(seed, frames, false, false);
        assert!(e1.is_empty(), "seed {seed}, plain: WebGPU errors {e1:?}");
        let (pass_opt, seg) = match std::env::var("IPPAI_MODEL_OPT").as_deref() {
            Ok("pass") => (true, false),
            Ok("seg") => (false, true),
            _ => (true, true),
        };
        let (got, e2, s) = run(seed, frames, pass_opt, seg);
        assert!(e2.is_empty(), "seed {seed}, optimized: WebGPU errors {e2:?}");
        stats = (stats.0 + s.0, stats.1 + s.1, stats.2 + s.2);
        assert_eq!(want.len(), got.len(), "seed {seed}: observations");
        for (k, (w, g)) in want.iter().zip(&got).enumerate() {
            if w != g {
                let what = match (w, g) {
                    (Obs::Frame(n, a), Obs::Frame(_, b)) => format!("frame {n}: {} of {} pixels differ (first at {:?})", a.iter().zip(b).filter(|(x, y)| x != y).count(), a.len(), a.iter().zip(b).position(|(x, y)| x != y)),
                    (Obs::Read(a), Obs::Read(b)) => format!("readback: {} of {} bytes differ", a.iter().zip(b).filter(|(x, y)| x != y).count(), a.len()),
                    _ => "different kinds".into(),
                };
                panic!("seed {seed}: observation {k}: {what}");
            }
        }
    }
    eprintln!("skipped batches {}, segments not drawn {}, drawn in part {}", stats.0, stats.1, stats.2);
}

#[test]
fn optimizations_draw_the_same_d3d9() {
    let n: u64 = std::env::var("IPPAI_MODEL_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(200);
    check(0..n, 12);
}
