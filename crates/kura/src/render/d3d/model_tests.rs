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
        // (no rectangle claims: the model hashes every sampled texel and the old color into
        // each pixel, so it cannot check them; render/d3d.rs tests `damage` itself)
        c.extend_from_slice(&[0; 5]);
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
                // (sometimes past the textures or their levels, as a bad StretchRect: no WebGPU error)
                let (dx, sl) = if r.chance(10) { (dx + r.below(20), r.below(3)) } else { (dx, 0) };
                b.c.extend_from_slice(&[COPY, st, sl, sx, sy, w, h, dt, 0, dx, dy, dw2, dh2, r.below(2)]);
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
                let (level, x) = if r.chance(10) { (r.below(3), r.below(4)) } else { (0, 0) };
                b.c.extend_from_slice(&[READBACK, self.req, t, level, x, 0, w, h, dst as u32, w * 4]);
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
                    p + 1 + b.c[p] as usize + 4 + 5 + 5 - i
                }
                _ => panic!("op {}", b.c[i]),
            };
            i += n;
        }
        let Some(&d) = draws.get(self.r.below(draws.len() as u32) as usize) else { return };
        let ntex = b.c[d + 17] as usize;
        // (where its box ends; rect_draw's 5 words come after it)
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
            CREATE_TEXTURE2 => ("tex2", 8),
            WRITE_TEXTURE2 => ("write2", 10),
            CREATE_BUFFER => ("buffer", 3),
            DESTROY_BUFFER => ("destroy buffer", 2),
            WRITE_BUFFER => ("write buffer", 5),
            CREATE_PIPELINE2 => ("pipe2", 3 + c[i + 2] as usize),
            SET_TARGETS2 => ("targets2", 2 + c[i + 1] as usize * 3 + 3),
            CLEAR_VIEW => ("clear view", 11),
            GEN_MIPS => ("mips", 2),
            RESOLVE => ("resolve", 6),
            DRAW2 => {
                let mut q = i + 2 + 6 + 5 + 4 + 1;
                q += 1 + 2 * c[q] as usize + 3;
                for _ in 0..2 {
                    q += 1 + 8 * c[q] as usize;
                }
                ("draw2", q + 7 - i)
            }
            CREATE_SHADER => ("shader", 4),
            CREATE_PIPELINE => ("pipe", 3 + c[i + 2] as usize),
            WRITE_TEXTURE => ("write", 9),
            SET_TARGET => ("target", 4),
            CLEAR => ("clear", 12),
            PRESENT | PRESENT_FLIP => ("present", 9),
            COPY => ("copy", 14),
            READBACK => ("readback", 10),
            FILL => ("fill", 8),
            DRAW => {
                let ntex = c[i + 17] as usize;
                let p = i + 18 + 3 * ntex;
                ("draw", p + 1 + c[p] as usize + 14 - i)
            }
            _ => ("?", c.len() - i),
        };
        let w = &c[i + 1..i + n];
        out.push(match c[i] {
            CREATE_SHADER | CREATE_PIPELINE | CREATE_PIPELINE2 => format!("{name} {}", w[0]),
            CLEAR => format!("clear flags {} color {:?} z {} s {} rect {:?}", w[0], [f32of(w[1]), f32of(w[2]), f32of(w[3]), f32of(w[4])], f32of(w[5]), w[6], &w[7..11]),
            DRAW => {
                let vp: Vec<f32> = w[1..7].iter().map(|&x| f32of(x)).collect();
                format!("draw pipe {} vp {vp:?} sc {:?} texs {:?} bbox {:?}", w[0], &w[7..11], &w[17..17 + 3 * w[16] as usize], &w[n - 10..n - 5])
            }
            _ => format!("{name} {:?}", w),
        });
        i += n;
    }
    out.join("\n  ")
}

/// Runs batches on a fresh model, optimized or not; returns what was observed.
fn run(seed: u64, batches: &[Batch], pass_opt: bool, seg: bool) -> (Vec<Obs>, Vec<String>, (u32, u32, u32)) {
    model::reset();
    let dump = std::env::var("IPPAI_MODEL_DUMP").ok().and_then(|s| s.parse::<u64>().ok()) == Some(seed);
    let mut d = D3d::new();
    d.no_pass_opt = !pass_opt;
    d.no_seg = !seg;
    d.skip_check = false;
    let mut obs = Vec::new();
    for (k, b) in batches.iter().enumerate() {
        if dump {
            eprintln!("batch {k}: {}", show(&b.c));
        }
        d.run(&b.c, &b.data, &b.blob);
        for token in model::with(|m| m.finish_maps()) {
            d.mapped(token, true);
        }
        if let Some(o) = &d.overlay {
            let v = model::with(|m| m.view_values(o.view));
            if dump {
                eprintln!("after batch {k} ({}): {:x} {:x} {:x}", if pass_opt { "opt" } else { "plain" }, v[0], v[128], v[255]);
            }
            obs.push(Obs::Frame(format!("after batch {k}"), v));
        }
    }
    obs.extend(model::with(|m| std::mem::take(&mut m.obs)));
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    let (a, b) = d.seg_stats.get();
    (obs, errors, (d.skipped.get(), a, b))
}

/// Scenarios `make(seed, readback address)` run plain and optimized: the same observations, no
/// WebGPU error. Returns (batches skipped, segments not drawn, segments drawn in part).
fn check(seeds: std::ops::Range<u64>, make: impl Fn(u64, u32) -> Vec<Batch>) -> (u32, u32, u32) {
    // (readbacks write to a guest address: 32 bits)
    let dst = unsafe { libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT, -1, 0) };
    assert!(dst != libc::MAP_FAILED);
    let mut stats = (0, 0, 0);
    for seed in seeds {
        let batches = make(seed, dst as usize as u32);
        let (want, e1, _) = run(seed, &batches, false, false);
        assert!(e1.is_empty(), "seed {seed}, plain: WebGPU errors {e1:?}");
        let (pass_opt, seg) = match std::env::var("IPPAI_MODEL_OPT").as_deref() {
            Ok("pass") => (true, false),
            _ => (true, true),
        };
        let (got, e2, s) = run(seed, &batches, pass_opt, seg);
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
    unsafe { libc::munmap(dst, 4096) };
    eprintln!("skipped batches {}, segments not drawn {}, drawn in part {}", stats.0, stats.1, stats.2);
    stats
}

/// (IPPAI_MODEL_FIRST: the first seed, to rerun a failing one)
fn first() -> u64 {
    std::env::var("IPPAI_MODEL_FIRST").ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn seeds() -> u64 {
    std::env::var("IPPAI_MODEL_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(200)
}

#[test]
fn optimizations_draw_the_same_d3d9() {
    let s = check(first()..seeds(), |seed, dst| {
        let mut g = Gen::new(seed);
        let mut v = vec![g.setup()];
        for _ in 0..12 {
            v.push(g.frame(dst as usize));
        }
        v
    });
    // (the optimizations ran)
    assert!(s.0 > 0 && s.1 > 0 && s.2 > 0, "{s:?}");
}

// ---- Direct3D 11 (crates/kura/src/d3d11 draw(): the commands as it makes them) ----

const VS11: &str = r#"
struct U { m: array<vec4f, 4> };
@group(0) @binding(0) var<uniform> cb: U;
@vertex fn main(@location(0) p: vec4f) -> @builtin(position) vec4f { return p * cb.m[0]; }
"#;

fn ps11(targets: u32, tex: bool) -> String {
    let mut s = String::from("struct U { m: array<vec4f, 4> };\n@group(1) @binding(0) var<uniform> cb: U;\n");
    if tex {
        s += "@group(1) @binding(16) var t0: texture_2d<f32>;\n@group(1) @binding(160) var s0: sampler;\n";
    }
    let outs: Vec<String> = (0..targets).map(|k| format!("@location({k}) o{k}: vec4f")).collect();
    s += &format!("struct O {{ {} }};\n", outs.join(", "));
    let v = if tex { "textureSample(t0, s0, p.xy) + cb.m[0]" } else { "cb.m[0]" };
    let set: Vec<String> = (0..targets).map(|k| format!("o.o{k} = {v} * {}.0;", k + 1)).collect();
    s += &format!("@fragment fn main(@builtin(position) p: vec4f) -> O {{ var o: O; {} return o; }}\n", set.join(" "));
    s
}

/// Textures: (id, w, h, layers, mips, format code, dimension word: 0x200 multisampled)
const TEXTURES11: [(u32, u32, u32, u32, u32, u32, u32); 8] = [
    (21, 16, 16, 1, 1, 0, 0),     // back buffer
    (22, 16, 16, 1, 1, 0, 0),     // a layer
    (23, 8, 8, 1, 2, 0, 0),       // a picture with mips
    (24, 16, 16, 1, 1, 1, 0),     // depth stencil
    (25, 16, 16, 2, 1, 0, 0),     // an array: two targets in one texture
    (26, 16, 16, 1, 1, 4, 0),     // rgba8unorm
    (27, 16, 16, 1, 1, 0, 0x200), // a 4x target, resolved into 22 or 25, sampled (Texture2DMS)
    (28, 16, 16, 1, 1, 1, 0x200), // its 4x depth stencil
];

#[derive(Clone, Copy)]
struct Pipe11 {
    id: u32,
    /// color targets
    n: u32,
    /// 0 no depth, 1 depth, 2 depth and stencil
    z: u32,
    tex: bool,
    fmt: u32,
    /// its pixel shader reads the samples of texture 27
    ms: bool,
}

/// A pixel shader that reads the samples of a 4x texture (Texture2DMS.Load, sample count).
const PS11_MS: &str = "struct U { m: array<vec4f, 4> };
@group(1) @binding(0) var<uniform> cb: U;
@group(1) @binding(16) var t0: texture_multisampled_2d<f32>;
@fragment fn main(@builtin(position) p: vec4f) -> @location(0) vec4f {
  return textureLoad(t0, vec2<i32>(p.xy), i32(textureNumSamples(t0)) - 1) + cb.m[0];
}
";

struct Gen11 {
    r: Rng,
    pipes: Vec<Pipe11>,
    /// buffer ids (the guest's: never the same twice)
    next_buf: u32,
    req: u32,
    /// the contents of the constant buffers (ids 1 and 2)
    cbs: [Vec<u8>; 2],
    /// the vertex buffer of the last frame, destroyed in the next
    old_vb: u32,
    /// the frames made (their seeds), to make again
    frames: Vec<u64>,
}

fn fnv32(h: &mut u32, b: &[u8]) {
    for &x in b {
        *h = (*h ^ x as u32).wrapping_mul(0x0100_0193);
    }
}

impl Gen11 {
    fn new(seed: u64) -> Gen11 {
        Gen11 { r: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x1111 | 1), pipes: Vec::new(), next_buf: 100, req: 0x7800_0000 + (seed as u32 & 0xffff) * 0x1000, cbs: [vec![0; 64], vec![0; 64]], old_vb: 0, frames: Vec::new() }
    }

    fn setup(&mut self) -> Batch {
        let mut b = Batch::new();
        for &(id, w, h, layers, mips, fmt, dim) in &TEXTURES11 {
            b.c.extend_from_slice(&[CREATE_TEXTURE2, id, w, h, layers, mips, fmt, dim]);
        }
        let mut shaders = vec![(11u32, VS11.to_string())];
        for n in 1..=2 {
            for tex in [false, true] {
                shaders.push((12 + (n - 1) * 2 + tex as u32, ps11(n, tex)));
            }
        }
        shaders.push((16, PS11_MS.to_string()));
        for (id, code) in &shaders {
            let off = b.blob(code.as_bytes());
            b.c.extend_from_slice(&[CREATE_SHADER, *id, off, code.len() as u32]);
        }
        for (id, size) in [(1, 64), (2, 64)] {
            b.c.extend_from_slice(&[CREATE_BUFFER, id, size]);
        }
        let mut id = 50;
        for fmt in [0u32, 4] {
            for n in 1..=2u32 {
                if fmt == 4 && n == 2 {
                    continue;
                }
                for z in 0..3u32 {
                    for tex in [false, true] {
                        let r = &mut self.r;
                        let ps = 12 + (n - 1) * 2 + tex as u32;
                        let mut key = vec![11, ps, r.pick(&[3u32, 3, 4]), 0, r.below(3), r.below(2), 0, 0, 0];
                        if z == 0 {
                            key.push(0xffff);
                        } else {
                            let (en, write, func) = (r.below(2) | (z == 2) as u32, r.below(2), r.below(9));
                            let sen = (z == 2) as u32;
                            key.extend_from_slice(&[1, en, write, func, sen, 0xffff]);
                            for _ in 0..2 {
                                key.extend_from_slice(&[r.below(9), r.below(9), r.below(9), r.below(9)]);
                            }
                        }
                        key.extend_from_slice(&[1, 16, 0, 1, 3, 0, 0]);
                        key.push(n);
                        for _ in 0..n {
                            let blend = r.chance(40) as u32;
                            key.extend_from_slice(&[fmt, blend, r.below(14), r.below(14), 1 + r.below(5), r.below(14), r.below(14), 1 + r.below(5), r.pick(&[15u32, 15, 7, 0])]);
                        }
                        b.c.extend_from_slice(&[CREATE_PIPELINE2, id, key.len() as u32]);
                        b.c.extend_from_slice(&key);
                        self.pipes.push(Pipe11 { id, n, z, tex, fmt, ms: false });
                        id += 1;
                    }
                }
            }
        }
        // pixel shaders that read the 4x texture's samples (one target of format 0)
        for z in 0..3u32 {
            let r = &mut self.r;
            let mut key = vec![11, 16, 3, 0, r.below(3), r.below(2), 0, 0, 0];
            if z == 0 {
                key.push(0xffff);
            } else {
                key.extend_from_slice(&[1, 1, r.below(2), r.below(9), (z == 2) as u32, 0xffff]);
                for _ in 0..2 {
                    key.extend_from_slice(&[r.below(9), r.below(9), r.below(9), r.below(9)]);
                }
            }
            key.extend_from_slice(&[1, 16, 0, 1, 3, 0, 0, 1, 0, 0, 2, 1, 1, 2, 1, 1, 15]);
            // (alpha to coverage, a sample mask)
            key.extend_from_slice(&[r.below(2), r.pick(&[u32::MAX, 5, 0])]);
            b.c.extend_from_slice(&[CREATE_PIPELINE2, id, key.len() as u32]);
            b.c.extend_from_slice(&key);
            self.pipes.push(Pipe11 { id, n: 1, z, tex: false, fmt: 0, ms: true });
            id += 1;
        }
        self.picture(&mut b, 0);
        self.picture(&mut b, 1);
        b
    }

    fn picture(&mut self, b: &mut Batch, level: u32) {
        let (w, h) = (8 >> level, 8 >> level);
        let px: Vec<u8> = (0..w * h * 4).map(|_| self.r.below(256) as u8).collect();
        let off = b.blob(&px);
        b.c.extend_from_slice(&[WRITE_TEXTURE2, 23, level, 0, 0, 0, w, h, w * 4, off]);
    }

    fn write_cb(&mut self, b: &mut Batch, k: usize) {
        let bytes: Vec<u8> = (0..64).map(|_| self.r.below(3) as u8).collect();
        self.cbs[k] = bytes.clone();
        let off = b.blob(&bytes);
        b.c.extend_from_slice(&[WRITE_BUFFER, k as u32 + 1, 0, off, 64]);
    }

    /// One frame from its seed (made again from the same seed, it draws the same, from new
    /// buffers); `change`: one constant differs.
    fn frame(&mut self, fseed: u64, change: bool, dst: u32) -> Batch {
        let saved = std::mem::replace(&mut self.r, Rng(fseed | 1));
        let mut b = Batch::new();
        // this frame's vertices, in a buffer of its own (a Map with DISCARD renames it)
        let vb = self.next_buf;
        self.next_buf += 1;
        let ib = self.next_buf;
        self.next_buf += 1;
        let verts: Vec<u8> = (0..64 * 16).map(|_| self.r.below(3) as u8).collect();
        let idx: Vec<u8> = (0..64u16).flat_map(|k| (k % 16).to_le_bytes()).collect();
        b.c.extend_from_slice(&[CREATE_BUFFER, vb, verts.len() as u32]);
        let off = b.blob(&verts);
        b.c.extend_from_slice(&[WRITE_BUFFER, vb, 0, off, verts.len() as u32]);
        b.c.extend_from_slice(&[CREATE_BUFFER, ib, idx.len() as u32]);
        let off = b.blob(&idx);
        b.c.extend_from_slice(&[WRITE_BUFFER, ib, 0, off, idx.len() as u32]);
        if self.old_vb != 0 && self.r.chance(70) {
            b.c.extend_from_slice(&[DESTROY_BUFFER, self.old_vb, DESTROY_BUFFER, self.old_vb + 1]);
        }
        self.old_vb = vb;
        if self.r.chance(10) {
            self.picture(&mut b, 0);
            if self.r.chance(50) {
                b.c.extend_from_slice(&[GEN_MIPS, 23]);
            }
        }
        // a 4x pass, resolved into a layer (sampled after) or a layer of the array
        let mut ms_shown = false;
        if self.r.chance(30) {
            let z = self.r.chance(50);
            self.pass(&mut b, &[(27, 0)], z, vb, ib, &verts, &idx, change);
            let (dt, layer) = self.r.pick(&[(22u32, 0u32), (22, 0), (25, 1)]);
            b.c.extend_from_slice(&[RESOLVE, dt, 0, layer, 27, 0]);
            ms_shown = self.r.chance(20);
        }
        // a layer first
        if self.r.chance(40) {
            let (t, layer) = self.r.pick(&[(22u32, 0u32), (25, 0), (25, 1)]);
            self.pass(&mut b, &[(t, layer)], false, vb, ib, &verts, &idx, change);
        }
        let main: &[(u32, u32)] = if self.r.chance(25) { &[(21, 0), (25, 1)] } else if self.r.chance(10) { &[(26, 0)] } else { &[(21, 0)] };
        let z = self.r.chance(50);
        self.pass(&mut b, main, z, vb, ib, &verts, &idx, change);
        if self.r.chance(15) {
            let (st, dt) = self.r.pick(&[(21u32, 22u32), (22, 21), (23, 22), (25, 21)]);
            b.c.extend_from_slice(&[COPY, st, 0, 0, 0, 8, 8, dt, 0, self.r.below(8), self.r.below(8), 8, 8, 0]);
        }
        if self.r.chance(10) {
            self.req += 1;
            b.c.extend_from_slice(&[READBACK, self.req, 21, 0, 0, 0, 16, 16, dst, 64]);
        }
        let shown = if ms_shown { 27 } else if main[0].0 == 26 { 26 } else { 21 };
        // (OpenGL presents bottom row first)
        let op = if self.r.chance(20) { PRESENT_FLIP } else { PRESENT };
        b.c.extend_from_slice(&[op, shown, 0, 0, 16, 16, 0, 0, 0]);
        self.r = saved;
        b
    }

    #[allow(clippy::too_many_arguments)]
    fn pass(&mut self, b: &mut Batch, targets: &[(u32, u32)], z: bool, vb: u32, ib: u32, verts: &[u8], idx: &[u8], change: bool) {
        let mut c = vec![SET_TARGETS2, targets.len() as u32];
        for &(t, l) in targets {
            c.extend_from_slice(&[t, 0, l]);
        }
        let zt = if targets[0].0 == 27 { 28 } else { 24 };
        c.extend_from_slice(&if z { [zt, 0, 0] } else { [0, 0, 0] });
        b.c.extend_from_slice(&c);
        for &(t, l) in targets {
            if self.r.chance(90) {
                let col = [self.r.below(3) as f32 / 2.0, 0.25, 1.0, 1.0];
                b.c.extend_from_slice(&[CLEAR_VIEW, t, 0, l, 1, col[0].to_bits(), col[1].to_bits(), col[2].to_bits(), col[3].to_bits(), 0, 0]);
            }
        }
        if z && self.r.chance(85) {
            let flags = self.r.pick(&[2u32, 6, 6, 4]);
            b.c.extend_from_slice(&[CLEAR_VIEW, zt, 0, 0, flags, 0, 0, 0, 0, (self.r.below(3) as f32 / 2.0).to_bits(), self.r.below(256)]);
        }
        let fmt = if targets[0].0 == 26 { 4 } else { 0 };
        let sampled: Vec<(u32, u32, u32, u32)> = [(23u32, 0u32, 2u32, 0u32), (23, 1, 1, 0), (22, 0, 1, 0), (25, 0, 1, 0), (25, 0, 1, 1), (26, 0, 1, 0)]
            .into_iter()
            .filter(|s| !targets.iter().any(|t| t.0 == s.0) && !(s.0 == 21))
            .collect();
        for _ in 0..1 + self.r.below(5) {
            if self.r.chance(15) {
                let k = self.r.below(2) as usize;
                self.write_cb(b, k);
            }
            let ms_ok = targets[0].0 != 27;
            let cands: Vec<Pipe11> = self.pipes.iter().copied().filter(|p| p.n == targets.len() as u32 && p.fmt == fmt && (p.z != 0) == z && (!p.ms || ms_ok)).collect();
            let p = self.r.pick(&cands);
            let r = &mut self.r;
            let vp = if r.chance(70) { [0.0f32, 0.0, 16.0, 16.0] } else { [r.below(10) as f32, r.below(10) as f32, 1.0 + r.below(16) as f32, 1.0 + r.below(16) as f32] };
            let mut w = vec![DRAW2, p.id];
            w.extend(vp.iter().map(|f| f.to_bits()));
            w.extend_from_slice(&[0f32.to_bits(), 1f32.to_bits()]);
            if r.chance(30) {
                w.extend_from_slice(&[r.below(16), r.below(16), r.below(17), r.below(17), 1]);
            } else {
                w.extend_from_slice(&[0, 0, 0, 0, 0]);
            }
            w.extend([0.5f32, 0.25, 1.0, 1.0].iter().map(|f| f.to_bits()));
            w.push(r.below(256));
            // (OpenGL and Direct2D: vertices and constants in the batch's data block, no hash)
            let gl = r.chance(25);
            let voff = r.below(16) * 16;
            if gl {
                let at = b.data(&verts[voff as usize..], 16);
                w.extend_from_slice(&[1, DATA, at]);
            } else {
                w.extend_from_slice(&[1, vb, voff]);
            }
            let indexed = r.chance(30);
            let ioff = r.below(16) * 2;
            if indexed {
                w.extend_from_slice(&[ib, ioff, 1]);
            } else {
                w.extend_from_slice(&[0, 0, 0]);
            }
            let r = &mut self.r;
            let cb = |b: &mut Batch, k: usize, cbs: &[Vec<u8>; 2]| if gl { [DATA, b.data(&cbs[k], 256), 64, 64] } else { [k as u32 + 1, 0, 64, 64] };
            let c0 = cb(b, 0, &self.cbs);
            let c1 = cb(b, 1, &self.cbs);
            w.extend_from_slice(&[1, 0, 0, c0[0], c0[1], c0[2], c0[3], 0, 0]);
            if p.ms {
                w.extend_from_slice(&[2, 0, 0, c1[0], c1[1], c1[2], c1[3], 0, 0, 16, 1, 27, 6, 0, 1, 0, 1]);
            } else if p.tex {
                let s = r.pick(&sampled);
                let key = 2 | 2 << 3 | 2 << 6 | r.below(2) << 9 | r.below(2) << 10 | r.below(2) << 11 | 1 << 17;
                w.extend_from_slice(&[3, 0, 0, c1[0], c1[1], c1[2], c1[3], 0, 0, 16, 1, s.0, 1, s.1, s.2, s.3, 1, 160, 2, key, 0, 32f32.to_bits(), 0, 0, 0]);
            } else {
                w.extend_from_slice(&[1, 0, 0, c1[0], c1[1], c1[2], c1[3], 0, 0]);
            }
            let count = r.pick(&[3u32, 4, 6]);
            let start = r.below(4);
            w.extend_from_slice(&[indexed as u32, count, 1, start, 0, 0]);
            if change && r.chance(30) {
                // (another constant: written before this draw)
                let k = 1;
                self.cbs[k][self.r.below(64) as usize] ^= 1;
                let bytes = self.cbs[k].clone();
                let off = b.blob(&bytes);
                b.c.extend_from_slice(&[WRITE_BUFFER, k as u32 + 1, 0, off, 64]);
            }
            // what it reads from its buffers (as d3d11 draw() hashes it)
            let mut h = 0x811c_9dc5u32;
            let vs: Vec<u32> = if indexed {
                (0..count).map(|k| u16::from_le_bytes([idx[(ioff + (start + k) * 2) as usize], idx[(ioff + (start + k) * 2 + 1) as usize]]) as u32).collect()
            } else {
                (start..start + count).collect()
            };
            for v in vs {
                let at = (voff + v * 16) as usize;
                fnv32(&mut h, &verts[at..at + 16]);
            }
            fnv32(&mut h, &self.cbs[0]);
            fnv32(&mut h, &self.cbs[1]);
            w.push(if gl { 0 } else { h.max(1) });
            b.c.extend_from_slice(&w);
        }
    }
}

#[test]
fn optimizations_draw_the_same_d3d11() {
    let s = check(0..seeds(), |seed, dst| {
        let mut g = Gen11::new(seed);
        let mut v = vec![g.setup()];
        for _ in 0..12 {
            // the same frame again (a still screen), a little changed, or a new one
            let (fs, change) = match g.r.below(4) {
                0 | 1 if !g.frames.is_empty() => (*g.frames.last().unwrap(), g.r.chance(30)),
                _ => (g.r.next(), false),
            };
            g.frames.push(fs);
            v.push(g.frame(fs, change, dst));
        }
        v
    });
    assert!(s.0 > 0, "{s:?}");
}

// ---- state mappings: whatever state a game sets, the descriptors kura makes are valid WebGPU ----

/// Samplers: every Direct3D 9 sampler key, and Direct3D 11 keys with any LOD clamps (NaN,
/// negative, reversed).
#[test]
fn every_sampler_state_is_valid_webgpu() {
    model::reset();
    let mut d = D3d::new();
    for key in 0..1u32 << 12 {
        d.sampler(key);
    }
    let lods = [0f32, 1.0, 13.5, -1.0, 1e30, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 3.0];
    let mut r = Rng(7);
    for k in 0..40000u32 {
        let key = if k < 1 << 13 { k << 9 | (k & 0x1ff) } else { r.next() as u32 };
        let (lo, hi) = (r.pick(&lods), r.pick(&lods));
        d.sampler2(key, lo.to_bits(), hi.to_bits());
    }
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(errors.is_empty(), "{:?}", &errors[..errors.len().min(10)]);
}

/// Pipelines: random Direct3D 9 and Direct3D 11 / OpenGL keys (every table index and past the
/// tables, any formats, blends, depth and stencil states, topologies) on the shaders of the
/// scenarios above.
#[test]
fn every_pipeline_state_is_valid_webgpu() {
    let n: u32 = std::env::var("IPPAI_MODEL_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(200) * 50;
    model::reset();
    let mut d = D3d::new();
    let mut g = Gen::new(1);
    let mut g11 = Gen11::new(1);
    let (s9, s11) = (g.setup(), g11.setup());
    d.run(&s9.c, &s9.data, &s9.blob);
    d.run(&s11.c, &s11.data, &s11.blob);
    model::with(|m| m.errors.clear());
    let mut r = Rng(11);
    let mut last = 0;
    for k in 0..n {
        // Direct3D 9: vs ps topo strip cull color_fmt depth_fmt depth_write depth_cmp blend[7] wmask, one buffer
        let mut key = vec![1, r.pick(&[2u32, 3]), r.below(7), r.below(6), r.below(4), r.pick(&[0u32, 2, 4, 5, 8, 9, 10, 15, 16, 99]), r.pick(&[0xffu32, 1]), r.below(2), r.below(12)];
        for _ in 0..7 {
            key.push(r.below(20));
        }
        key.push(r.below(17));
        key.extend_from_slice(&[1, 16, 1, r.below(16), 0, 0]);
        let p = d.create_pipeline(&key);
        if p.pipeline != 0 {
            gpu::drop(p.pipeline);
        }
        // Direct3D 11 / OpenGL
        let n_t = 1 + r.below(2);
        let mut key = vec![11, 12 + (n_t - 1) * 2 + r.below(2), r.below(7), r.below(4), r.below(5), r.below(2), r.next() as u32 % 100, r.pick(&[0f32, 1.5, -2.0, f32::NAN]).to_bits(), r.pick(&[0f32, 0.5, f32::INFINITY]).to_bits()];
        if r.chance(30) {
            key.push(0xffff);
        } else {
            key.extend_from_slice(&[r.pick(&[1u32, 7, 14]), r.below(2), r.below(2), r.below(12), r.below(2), r.next() as u32]);
            for _ in 0..8 {
                key.push(r.below(12));
            }
        }
        key.extend_from_slice(&[1, 16, r.below(2), 1, r.below(40), 0, 0]);
        key.push(n_t);
        for _ in 0..n_t {
            // (the float formats: the test's shaders write floats)
            key.extend_from_slice(&[r.pick(&[0u32, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 15, 16, 18]), r.below(2), r.below(20), r.below(20), r.below(8), r.below(20), r.below(20), r.below(8), r.below(17)]);
        }
        // alpha to coverage, sample mask (or a key that ends before them)
        if r.chance(80) {
            key.extend_from_slice(&[r.below(3), r.next() as u32]);
        }
        let id = 9000 + k;
        let mut c = vec![CREATE_PIPELINE2, id, key.len() as u32];
        c.extend_from_slice(&key);
        d.run(&c, &[], &[]);
        let groups: [&[u32]; 2] = [&[0, 0, 1, 0, 64, 64, 0, 0], if key[1] % 2 == 1 { &[0, 0, 2, 0, 64, 64, 0, 0, 16, 1, 23, 1, 0, 1, 0, 1, 160, 2, 0, 0, 0, 0, 0, 0] } else { &[0, 0, 2, 0, 64, 64, 0, 0] }];
        d.pipeline2(id, &groups, r.pick(&[1, 4]));
        let errors = model::with(|m| std::mem::take(&mut m.errors));
        assert!(errors.is_empty(), "pipeline {k}: {errors:?}\nkey {key:?}");
        last = k;
    }
    assert_eq!(last + 1, n);
}

/// Textures of every format the page knows (and past the table), every dimension, odd sizes and
/// mip counts: valid createTexture / createView calls.
#[test]
fn every_texture_kind_is_valid_webgpu() {
    model::reset();
    let mut d = D3d::new();
    let mut c = Vec::new();
    let mut id = 1;
    for fmt in 0..24u32 {
        for (w, h, levels) in [(1u32, 1u32, 1u32), (16, 8, 5), (7, 3, 3), (256, 1, 9), (5, 5, 20)] {
            c.extend_from_slice(&[CREATE_TEXTURE, id, w, h, levels, fmt, 1]);
            id += 1;
            for dim in 0..3u32 {
                for layers in [1u32, 6] {
                    c.extend_from_slice(&[CREATE_TEXTURE2, id, w, h, layers, levels, fmt, dim]);
                    id += 1;
                }
            }
        }
    }
    // multisampled (bit 9), with UAV binding too (bit 8): every page format, 1 or more layers
    // and levels, every dimension; then resolved, cleared, copied whole, presented, bound
    for fmt in 0..TEX_FORMATS.len() as u32 {
        for (w, h, layers, levels) in [(16u32, 8u32, 1u32, 1u32), (7, 3, 1, 1), (16, 16, 2, 1), (8, 8, 1, 3)] {
            for dim in [0x200u32, 0x300, 0x201, 0x202] {
                c.extend_from_slice(&[CREATE_TEXTURE2, id, w, h, layers, levels, fmt, dim]);
                // a single-sample twin, resolved into and copied to
                c.extend_from_slice(&[CREATE_TEXTURE2, id + 1, w, h, layers, levels, fmt, dim & 0xff]);
                c.extend_from_slice(&[CLEAR_VIEW, id, 0, 0, 7, 0, 0, 0, 0, 0, 0]);
                c.extend_from_slice(&[RESOLVE, id + 1, 0, 0, id, 0]);
                c.extend_from_slice(&[RESOLVE, id + 1, 0, layers - 1, id, layers - 1]);
                c.extend_from_slice(&[COPY, id, 0, 0, 0, w, h, id + 2, 0, 0, 0, w, h, 0]);
                c.extend_from_slice(&[PRESENT, id, 0, 0, 16, 16, 0, 0, 0]);
                id += 3;
            }
        }
    }
    d.run(&c, &[], &[]);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    let mut seen: Vec<&String> = errors.iter().collect();
    seen.dedup();
    assert!(errors.is_empty(), "{} errors: {:?}", errors.len(), &seen[..seen.len().min(12)]);
}

// ---- save states (d3d/snap.rs): the GPU read back after any batch and made again ----

/// Runs `batches`, with a state after batch `cut` restored into a fresh model and D3d (Some), or
/// straight through (None); returns what each batch after `cut` was observed to do.
fn run_cut(batches: &[Batch], cut: usize, snap: bool) -> Vec<Vec<Obs>> {
    model::reset();
    let mut d = D3d::new();
    d.skip_check = false;
    let mut out = Vec::new();
    for (k, b) in batches.iter().enumerate() {
        d.run(&b.c, &b.data, &b.blob);
        for token in model::with(|m| m.finish_maps()) {
            d.mapped(token, true);
        }
        let mut obs = model::with(|m| std::mem::take(&mut m.obs));
        if let Some(o) = &d.overlay {
            obs.push(Obs::Frame(format!("after batch {k}"), model::with(|m| m.view_values(o.view))));
        }
        if k > cut {
            out.push(obs);
        }
        if k == cut && !snap {
            // (what a state leaves out: depth and stencil, as last cleared)
            let mut c = Vec::new();
            for (&id, t) in d.textures.iter().filter(|(_, t)| is_depth(t.format)) {
                c.extend(snap::depth_clears(id, t));
            }
            d.run_batch(&c, &[], &[]);
            model::with(|m| m.obs.clear());
            // (what the page shows right then)
            out.push(d.overlay.iter().map(|o| Obs::Frame("restored".into(), model::with(|m| m.view_values(o.view)))).collect());
        }
        if k == cut && snap {
            d.snap_begin();
            assert!(d.snap_ready().is_none() || d.textures.is_empty() && d.buffers.is_empty());
            for token in model::with(|m| m.finish_maps()) {
                d.mapped(token, true);
            }
            assert_eq!(d.snap_ready(), Some(true));
            let saved = d.snap_take();
            let targets2 = d.targets2.clone();
            let errors = model::with(|m| std::mem::take(&mut m.errors));
            assert!(errors.is_empty(), "reading back: WebGPU errors {errors:?}");
            // (a new page: nothing of the old GPU is left)
            model::reset();
            d = D3d::new();
            d.skip_check = false;
            d.snap_restore(&saved);
            let errors = model::with(|m| std::mem::take(&mut m.errors));
            assert!(errors.is_empty(), "making again: WebGPU errors {errors:?}");
            model::with(|m| m.obs.clear());
            assert_eq!(d.targets2, targets2, "Direct3D 11 targets");
            out.push(d.overlay.iter().map(|o| Obs::Frame("restored".into(), model::with(|m| m.view_values(o.view)))).collect());
        }
    }
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(errors.is_empty(), "WebGPU errors {errors:?}");
    out
}

fn check_cuts(make: impl Fn(u64, u32) -> Vec<Batch>) {
    let dst = unsafe { libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT, -1, 0) };
    assert!(dst != libc::MAP_FAILED);
    for seed in 0..seeds() / 4 {
        let batches = make(seed, dst as usize as u32);
        let cut = (seed as usize * 7) % (batches.len() - 1);
        let want = run_cut(&batches, cut, false);
        let got = run_cut(&batches, cut, true);
        for (k, (w, g)) in want.iter().zip(&got).enumerate() {
            if w != g {
                let what: Vec<String> = w
                    .iter()
                    .zip(g)
                    .map(|(a, b)| match (a, b) {
                        (Obs::Frame(n, a), Obs::Frame(_, b)) => format!("{n}: {} of {} differ (first {:?})", a.iter().zip(b).filter(|(x, y)| x != y).count(), a.len(), a.iter().zip(b).position(|(x, y)| x != y)),
                        (Obs::Read(a), Obs::Read(b)) => format!("read: {} of {} differ", a.iter().zip(b).filter(|(x, y)| x != y).count(), a.len()),
                        _ => "kinds".into(),
                    })
                    .collect();
                panic!("seed {seed}, state after batch {cut}: batch {} (0: right after the state) observed differently ({} vs {} observations): {what:?}", k, w.len(), g.len());
            }
        }
    }
    unsafe { libc::munmap(dst, 4096) };
}

#[test]
fn a_state_draws_the_same_d3d9() {
    check_cuts(|seed, dst| {
        let mut g = Gen::new(seed);
        let mut v = vec![g.setup()];
        for _ in 0..12 {
            v.push(g.frame(dst as usize));
        }
        v
    });
}

#[test]
fn a_state_draws_the_same_d3d11() {
    check_cuts(|seed, dst| {
        let mut g = Gen11::new(seed);
        let mut v = vec![g.setup()];
        for _ in 0..12 {
            let fs = g.r.next();
            g.frames.push(fs);
            v.push(g.frame(fs, false, dst));
        }
        v
    });
}
