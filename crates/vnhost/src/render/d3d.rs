//! Runs the Direct3D 9/11 (and OpenGL) commands the guest's threads collect in batches
//! (crates/vnhost/src/d3d9/gpu.rs, where the command words are listed) with WebGPU, on the
//! drawing thread, every frame.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;

use super::gpu::{self, q, usage, Id, Res};
use super::STATS;
use crate::d3d9::gpu::*;

/// buffer id of the batch's data block (DRAW2 vertex/index/uniform sources)
const DATA: u32 = 0xffff_ffff;

// (crates/vnhost/src/d3d11/format.rs P_*)
const TEX_FORMATS: [&str; 22] = [
    "bgra8unorm",
    "depth24plus-stencil8",
    "rgba16float",
    "rgba16float",
    "rgba8unorm",
    "r8unorm",
    "rg8unorm",
    "depth32float",
    "r16float",
    "rg16float",
    "rgb10a2unorm",
    "rg11b10ufloat",
    "rgba8unorm-srgb",
    "bgra8unorm-srgb",
    "depth16unorm",
    "r32float",
    "rg32float",
    "rgba8uint",
    "rgba8snorm",
    "rgba8sint",
    "r32uint",
    "r32sint",
];
fn tex_format(i: u32) -> &'static str {
    TEX_FORMATS.get(i as usize).copied().unwrap_or("bgra8unorm")
}
fn bpp(f: &str) -> u32 {
    match f {
        "r8unorm" => 1,
        "rg8unorm" | "r16float" | "depth16unorm" => 2,
        "rgba16float" | "rg32float" => 8,
        _ => 4,
    }
}
fn is_depth(f: &str) -> bool {
    f.starts_with("depth")
}
const DS: &str = "depth24plus-stencil8";

// (crates/vnhost/src/d3d11/format.rs vertex_format)
const VERTEX_FORMATS2: [&str; 32] = [
    "float32", "float32x2", "float32x3", "float32x4", "uint32", "uint32x2", "uint32x3", "uint32x4", "sint32", "sint32x2",
    "sint32x3", "sint32x4", "unorm8x4", "snorm8x4", "uint8x4", "sint8x4", "unorm16x2", "unorm16x4", "snorm16x2",
    "snorm16x4", "uint16x2", "uint16x4", "sint16x2", "sint16x4", "float16x2", "float16x4", "unorm8x2", "uint8x2",
    "unorm10-10-10-2", "unorm8x4-bgra", "snorm8x2", "sint8x2",
];
const BLEND2: [&str; 14] = [
    "zero",
    "one",
    "src",
    "one-minus-src",
    "src-alpha",
    "one-minus-src-alpha",
    "dst-alpha",
    "one-minus-dst-alpha",
    "dst",
    "one-minus-dst",
    "src-alpha-saturated",
    "constant",
    "one-minus-constant",
    "one",
];
const STENCIL_OP: [&str; 9] =
    ["keep", "keep", "zero", "replace", "increment-clamp", "decrement-clamp", "invert", "increment-wrap", "decrement-wrap"];
const ADDRESS2: [&str; 4] = ["repeat", "mirror-repeat", "clamp-to-edge", "clamp-to-edge"];
const VIEW_DIM: [&str; 6] = ["1d", "2d", "2d-array", "cube", "cube-array", "3d"];
const VERTEX_FORMATS: [&str; 14] = [
    "float32", "float32x2", "float32x3", "float32x4", "unorm8x4", "uint8x4", "sint16x2", "sint16x4", "snorm16x2",
    "snorm16x4", "unorm16x2", "unorm16x4", "float16x2", "float16x4",
];
const TOPOLOGY: [&str; 5] = ["point-list", "line-list", "line-strip", "triangle-list", "triangle-strip"];
const COMPARE: [&str; 9] = ["always", "never", "less", "equal", "less-equal", "greater", "not-equal", "greater-equal", "always"];
const BLEND: [&str; 16] = [
    "one",
    "zero",
    "one",
    "src",
    "one-minus-src",
    "src-alpha",
    "one-minus-src-alpha",
    "dst-alpha",
    "one-minus-dst-alpha",
    "dst",
    "one-minus-dst",
    "src-alpha-saturated",
    "src-alpha",
    "one-minus-src-alpha",
    "constant",
    "one-minus-constant",
];
const BLEND_ALPHA: [&str; 16] = [
    "one",
    "zero",
    "one",
    "src-alpha",
    "one-minus-src-alpha",
    "src-alpha",
    "one-minus-src-alpha",
    "dst-alpha",
    "one-minus-dst-alpha",
    "dst-alpha",
    "one-minus-dst-alpha",
    "one",
    "src-alpha",
    "one-minus-src-alpha",
    "constant",
    "one-minus-constant",
];
const BLEND_OP: [&str; 6] = ["add", "add", "subtract", "reverse-subtract", "min", "max"];
const ADDRESS: [&str; 6] = ["repeat", "repeat", "mirror-repeat", "clamp-to-edge", "clamp-to-edge", "mirror-repeat"];

fn pick<'a>(t: &[&'a str], i: u32, or: &'a str) -> &'a str {
    t.get(i as usize).copied().unwrap_or(or)
}
fn f32of(u: u32) -> f32 {
    f32::from_bits(u)
}
/// A JSON number (finite).
fn num(f: f32) -> String {
    if f.is_finite() { format!("{f}") } else { "0".into() }
}

const CLEAR_SHADER: &str = r#"
struct C { color: vec4f, z: vec4f };
@group(0) @binding(0) var<uniform> c: C;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4f(p * 2.0 - 1.0, c.z.x, 1.0);
}
@fragment fn fs() -> @location(0) vec4f { return c.color; }
"#;

const BLIT_SHADER: &str = r#"
struct Out { @builtin(position) pos: vec4f, @location(0) uv: vec2f };
struct R { src: vec4f };
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> r: R;
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  var o: Out;
  o.pos = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
  o.uv = r.src.xy + vec2f(p.x, 1.0 - p.y) * r.src.zw;
  return o;
}
@fragment fn fs(i: Out) -> @location(0) vec4f { return textureSample(tex, smp, i.uv); }
"#;

struct Tex {
    tex: Id,
    w: u32,
    h: u32,
    levels: u32,
    format: &'static str,
    view: Id,
    /// Direct3D 11: array layers (depth of 3D textures), dimension
    layers: u32,
    dim: &'static str,
    views: HashMap<(u8, u32, u32, u32, u32, bool), Id>,
    /// depth stand-ins of other sizes (see `depth_for`): size -> (texture, view)
    fits: HashMap<(u32, u32), (Id, Id)>,
    /// depth and stencil it was last cleared to
    zclear: Option<(f32, u32)>,
}

impl Tex {
    /// (the view the page shows stays until another frame replaces it: `keep`)
    fn free(&self, keep: Id) {
        gpu::destroy(self.tex);
        if self.view != keep {
            gpu::drop(self.view);
        }
        for &v in self.views.values() {
            gpu::drop(v);
        }
        for &(t, v) in self.fits.values() {
            gpu::destroy(t);
            gpu::drop(v);
        }
    }
}

/// Direct3D 11 pipeline: made at its first draw (the bind group layouts come from the draw)
struct Pipe2 {
    key: Vec<u32>,
    pipeline: Id,
    layouts: Vec<Id>,
    kinds: Vec<u32>,
}

struct Pipe {
    pipeline: Id,
}

/// What the page shows of a Direct3D window.
#[derive(Clone, Copy)]
pub struct Overlay {
    pub view: Id,
    /// client area on the screen: left, top, right, bottom
    pub rect: [i32; 4],
    /// bottom row first (OpenGL)
    pub flip: bool,
}

#[derive(Clone, Copy)]
struct Clear {
    flags: u32,
    color: [f32; 4],
    z: f32,
    stencil: u32,
}

struct Readback {
    req: u32,
    buf: Id,
    data: Vec<u8>,
    bpr: u32,
    row: u32,
    h: u32,
    dst: u32,
    pitch: u32,
}

pub struct D3d {
    textures: HashMap<u32, Tex>,
    shaders: HashMap<u32, Id>,
    pipelines: HashMap<u32, Pipe>,
    samplers: HashMap<u32, Id>,
    groups1: HashMap<Vec<u32>, Id>,
    groups0: HashMap<(u32, u32), Id>,
    /// Direct3D 11 bind groups by pipeline, group and binding words (cleared when a texture or
    /// buffer comes or goes): layout, group, textures it uses
    groups2: HashMap<Vec<u32>, (Id, Id, Vec<u32>)>,
    layout0: Id,
    layout1: Id,
    layout: Id,
    data_buf: Id,
    data_size: u32,
    white: Id,
    clear_pipes: HashMap<(&'static str, bool, u32), Id>,
    clear_uniform: Id,
    clear_slot: u32,
    blit_pipes: HashMap<&'static str, Id>,
    pub overlay: Option<Overlay>,
    /// changes when a texture goes or the overlay's view changes (view indexes are reused)
    pub tex_gen: u64,
    pub frames: u64,
    inflight: u32,
    next_token: u32,
    pub gpu_ms: Cell<(f64, f64)>,
    pub submits: Cell<u32>,
    pub created: Cell<u32>,
    readbacks: HashMap<u32, Readback>,

    // per batch
    open: bool,
    pass: bool,
    target: [u32; 3],
    pending: Option<Clear>,
    /// a depth/stencil-only clear of this texture not done yet: the next pass using it clears
    pending_z: Option<(u32, Clear)>,
    used: Vec<u32>,
    // Direct3D 11
    buffers: HashMap<u32, (Id, u32)>,
    doomed: Vec<Id>,
    pipelines2: HashMap<u32, Pipe2>,
    targets2: Option<Vec<u32>>,
    dummies: HashMap<(u32, u32), Id>,
    zero_buf: Id,
    samplers2: HashMap<(u32, u32, u32), Id>,
    no_ps: Id,
    clear_module: Id,
    blit_module: Id,
}

impl D3d {
    pub fn new() -> D3d {
        let layout0 = gpu::bind_group_layout(
            r#"{"entries":[{"binding":0,"visibility":3,"buffer":{"type":"uniform","hasDynamicOffset":true}},{"binding":1,"visibility":3,"buffer":{"type":"uniform","hasDynamicOffset":true}}]}"#,
        );
        let mut e1 = Vec::new();
        for s in 0..8 {
            e1.push(format!(r#"{{"binding":{},"visibility":2,"texture":{{"sampleType":"float"}}}}"#, s * 2));
            e1.push(format!(r#"{{"binding":{},"visibility":2,"sampler":{{"type":"filtering"}}}}"#, s * 2 + 1));
        }
        let layout1 = gpu::bind_group_layout(&format!(r#"{{"entries":[{}]}}"#, e1.join(",")));
        let layout = gpu::pipeline_layout(&format!(r#"{{"bindGroupLayouts":[{layout0},{layout1}]}}"#));
        let white = gpu::texture(&format!(r#"{{"size":[1,1],"format":"bgra8unorm","usage":{}}}"#, usage::T_BINDING | usage::T_COPY_DST));
        let px = [255u8; 4];
        gpu::write_texture(white, 0, [0; 3], px.as_ptr(), 4, 4, 1, [1, 1, 1]);
        D3d {
            textures: HashMap::new(),
            shaders: HashMap::new(),
            pipelines: HashMap::new(),
            samplers: HashMap::new(),
            groups1: HashMap::new(),
            groups0: HashMap::new(),
            groups2: HashMap::new(),
            layout0,
            layout1,
            layout,
            data_buf: 0,
            data_size: 0,
            white: gpu::view(white, ""),
            clear_pipes: HashMap::new(),
            clear_uniform: gpu::buffer(256 * 64, usage::UNIFORM | usage::COPY_DST),
            clear_slot: 0,
            blit_pipes: HashMap::new(),
            overlay: None,
            tex_gen: 0,
            frames: 0,
            inflight: 0,
            next_token: 1,
            gpu_ms: Cell::new((0.0, 0.0)),
            submits: Cell::new(0),
            created: Cell::new(0),
            readbacks: HashMap::new(),
            open: false,
            pass: false,
            target: [0; 3],
            pending: None,
            pending_z: None,
            used: Vec::new(),
            buffers: HashMap::new(),
            doomed: Vec::new(),
            pipelines2: HashMap::new(),
            targets2: None,
            dummies: HashMap::new(),
            zero_buf: 0,
            samplers2: HashMap::new(),
            no_ps: 0,
            clear_module: 0,
            blit_module: 0,
        }
    }

    /// Runs what the guest sent since the last call.
    pub fn poll(&mut self) {
        let mut io = [0u32; 6];
        for _ in 0..64 {
            // (the GPU at most two frames behind: the game then waits in Present)
            if self.inflight >= 2 {
                break;
            }
            if unsafe { vnhost_d3d9_poll(io.as_mut_ptr()) } == 0 {
                break;
            }
            let t0 = now();
            unsafe {
                let c = std::slice::from_raw_parts(io[0] as usize as *const u32, io[1] as usize);
                let data = std::slice::from_raw_parts(io[2] as usize as *const u8, io[3] as usize);
                let blob = std::slice::from_raw_parts(io[4] as usize as *const u8, io[5] as usize);
                self.run(c, data, blob);
            }
            // (guest memory the batch read may change now)
            vnhost_d3d9_done();
            STATS[1].fetch_add(((now() - t0) * 1000.0) as u64, Relaxed);
        }
    }

    pub fn gpu_done(&mut self, _token: u32, ms: f64) {
        self.inflight = self.inflight.saturating_sub(1);
        let (sum, max) = self.gpu_ms.get();
        self.gpu_ms.set((sum + ms, max.max(ms)));
    }

    pub fn mapped(&mut self, token: u32, ok: bool) {
        let Some(r) = self.readbacks.remove(&token) else { return };
        // (not after the guest stopped waiting: it may have freed dst)
        if ok && vnhost_d3d9_readback_begin(r.req) != 0 {
            for y in 0..r.h as usize {
                let src = &r.data[y * r.bpr as usize..y * r.bpr as usize + r.row as usize];
                unsafe {
                    std::ptr::copy_nonoverlapping(src.as_ptr(), (r.dst as usize + y * r.pitch as usize) as *mut u8, r.row as usize);
                }
            }
        }
        gpu::destroy(r.buf);
        vnhost_d3d9_readback_done(r.req);
    }

    fn run(&mut self, c: &[u32], data: &[u8], blob: &[u8]) {
        if !data.is_empty() {
            let size = (data.len().div_ceil(4) * 4 + 65536) as u32;
            if self.data_buf == 0 || self.data_size < size {
                gpu::destroy(self.data_buf);
                self.data_size = size.max(self.data_size * 2).max(1 << 20);
                self.data_buf = gpu::buffer(self.data_size, usage::VERTEX | usage::INDEX | usage::UNIFORM | usage::COPY_DST);
                drop_all(&mut self.groups0);
                self.clear_groups2();
            }
            // (rounded up to 4 bytes: what follows the data in memory goes along, unused)
            let n = data.len().div_ceil(4) * 4;
            gpu::write_buffer(self.data_buf, 0, unsafe { std::slice::from_raw_parts(data.as_ptr(), n) });
        }
        self.target = [0; 3];
        self.pending = None;
        let mut i = 0;
        while i < c.len() {
            let op = c[i];
            let a = |k: usize| c[i + k];
            if !matches!(op, SET_TARGET | CLEAR | DRAW | SET_TARGETS2 | DRAW2) {
                self.flush_z();
            }
            match op {
                CREATE_TEXTURE => {
                    let (id, w, h, levels, format, rt) = (a(1), a(2), a(3), a(4), a(5), a(6));
                    let fmt = tex_format(format);
                    let depth = fmt == DS;
                    // (StretchRect/ColorFill can draw into any color texture)
                    let _ = rt;
                    let u = usage::T_BINDING | usage::T_COPY_DST | usage::T_COPY_SRC | usage::T_RENDER;
                    let levels = if depth { 1 } else { levels.max(1) };
                    let tex = gpu::texture(&format!(r#"{{"size":[{w},{h}],"format":{},"mipLevelCount":{levels},"usage":{u}}}"#, q(fmt)));
                    if let Some(old) = self.textures.remove(&id) {
                        old.free(self.shown());
                        self.tex_gen += 1;
                    }
                    self.clear_groups2();
                    self.created.set(self.created.get() + 1);
                    let view = gpu::view(tex, "");
                    self.textures.insert(id, Tex { tex, w, h, levels, format: fmt, view, layers: 1, dim: "2d", views: HashMap::new(), fits: HashMap::new(), zclear: None });
                    i += 7;
                }
                DESTROY_TEXTURE => {
                    let id = a(1);
                    if self.textures.contains_key(&id) {
                        self.flush_if_used(id);
                        self.textures.remove(&id).unwrap().free(self.shown());
                        self.tex_gen += 1;
                        drop_all(&mut self.groups1);
                        self.clear_groups2();
                    }
                    i += 2;
                }
                WRITE_TEXTURE => {
                    let (id, level, x, y, w, h, bpr, off) = (a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8));
                    if let Some(t) = self.textures.get(&id).filter(|t| t.format != DS) {
                        let (fmt, tw, th, tex) = (t.format, t.w, t.h, t.tex);
                        self.flush_if_used(id);
                        let (lw, lh) = ((tw >> level).max(1), (th >> level).max(1));
                        let (cw, ch) = (w.min(lw.saturating_sub(x)), h.min(lh.saturating_sub(y)));
                        let len = (bpr * h) as usize;
                        if cw > 0 && ch > 0 && (fmt == "bgra8unorm" || bpr == w * 8) && off as usize + len <= blob.len() {
                            STATS[3].fetch_add(len as u64, Relaxed);
                            gpu::write_texture(tex, level, [x, y, 0], blob[off as usize..].as_ptr(), len, bpr, ch, [cw, ch, 1]);
                        }
                    }
                    i += 9;
                }
                WRITE_TEXTURE_MEM => {
                    // rows straight from guest memory (the guest leaves them alone until this batch ran)
                    let (id, level, x, y, w, h, pitch, ptr) = (a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8));
                    if let Some(t) = self.textures.get(&id).filter(|t| t.format != DS) {
                        let (fmt, tw, th, tex) = (t.format, t.w, t.h, t.tex);
                        self.flush_if_used(id);
                        let (lw, lh) = ((tw >> level).max(1), (th >> level).max(1));
                        let (cw, ch) = (w.min(lw.saturating_sub(x)), h.min(lh.saturating_sub(y)));
                        let bpp = if fmt == "bgra8unorm" { 4 } else { 8 };
                        if cw > 0 && ch > 0 {
                            let len = (pitch * (ch - 1) + cw * bpp) as usize;
                            STATS[3].fetch_add(len as u64, Relaxed);
                            gpu::write_texture(tex, level, [x, y, 0], ptr as usize as *const u8, len, pitch, ch, [cw, ch, 1]);
                        }
                    }
                    i += 9;
                }
                CREATE_SHADER => {
                    let (id, off, len) = (a(1) as usize, a(2) as usize, a(3) as usize);
                    let code = String::from_utf8_lossy(&blob[off..off + len]);
                    if let Some(old) = self.shaders.insert(id as u32, gpu::shader(&code)) {
                        gpu::drop(old);
                    }
                    i += 4;
                }
                CREATE_PIPELINE => {
                    let (id, n) = (a(1), a(2) as usize);
                    let p = self.create_pipeline(&c[i + 3..i + 3 + n]);
                    if let Some(old) = self.pipelines.insert(id, p) {
                        gpu::drop(old.pipeline);
                    }
                    i += 3 + n;
                }
                SET_TARGET => {
                    let t = [a(1), a(2), a(3)];
                    if self.targets2.is_some() {
                        self.end_pass();
                        self.targets2 = None;
                    }
                    if t != self.target {
                        self.end_pass();
                        self.target = t;
                        self.pending = None;
                    }
                    i += 4;
                }
                CLEAR => {
                    let flags = a(1);
                    let color = [f32of(a(2)), f32of(a(3)), f32of(a(4)), f32of(a(5))];
                    let z = f32of(a(6));
                    let stencil = a(7);
                    let (x, y, w, h) = (a(8), a(9), a(10), a(11));
                    if w == 0 {
                        // whole target: the next pass clears
                        self.clear_whole(Clear { flags, color, z, stencil });
                    } else {
                        self.clear_rect(flags, color, z, x, y, w, h);
                    }
                    i += 12;
                }
                DRAW => i = self.draw(c, i),
                PRESENT | PRESENT_FLIP => {
                    let (tex, rect) = (a(1), [a(2) as i32, a(3) as i32, a(4) as i32, a(5) as i32]);
                    self.end_pass();
                    self.submit();
                    if let Some(t) = self.textures.get(&tex) {
                        let view = t.view;
                        let old = self.shown();
                        // (a view kept for the screen after its texture went: now unused)
                        if old != view && old != 0 && !self.textures.values().any(|t| t.view == old) {
                            gpu::drop(old);
                        }
                        if old != view {
                            self.tex_gen += 1;
                        }
                        self.overlay = Some(Overlay { view, rect, flip: op == PRESENT_FLIP });
                    }
                    self.frames += 1;
                    STATS[0].fetch_add(1, Relaxed);
                    self.inflight += 1;
                    gpu::on_done(self.next_token);
                    self.next_token = self.next_token.wrapping_add(1).max(1);
                    i += 10;
                }
                COPY => {
                    let v: [u32; 13] = c[i + 1..i + 14].try_into().unwrap();
                    self.copy_rect(v);
                    i += 14;
                }
                READBACK => {
                    let v: [u32; 9] = c[i + 1..i + 10].try_into().unwrap();
                    self.readback(v);
                    i += 10;
                }
                FILL => {
                    let (tex, level, x, y, w, h, color) = (a(1), a(2), a(3), a(4), a(5), a(6), a(7));
                    let saved = self.target;
                    let saved2 = self.targets2.take();
                    self.end_pass();
                    self.target = [tex, level, 0];
                    let col = [((color >> 16) & 255) as f32 / 255.0, ((color >> 8) & 255) as f32 / 255.0, (color & 255) as f32 / 255.0, (color >> 24) as f32 / 255.0];
                    self.clear_rect(1, col, 0.0, x, y, w, h);
                    self.end_pass();
                    self.target = saved;
                    self.targets2 = saved2;
                    i += 8;
                }
                CREATE_TEXTURE2 => {
                    self.clear_groups2();
                    let v: [u32; 7] = c[i + 1..i + 8].try_into().unwrap();
                    self.create_texture2(v);
                    i += 8;
                }
                WRITE_TEXTURE2 => {
                    let (id, level, layer, x, y, w, h, bpr, off) = (a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8), a(9));
                    if let Some(t) = self.textures.get(&id).filter(|t| !is_depth(t.format)) {
                        let (tw, th, levels, tex) = (t.w, t.h, t.levels, t.tex);
                        self.flush_if_used(id);
                        let (lw, lh) = ((tw >> level).max(1), (th >> level).max(1));
                        let (cw, ch) = (w.min(lw.saturating_sub(x)), h.min(lh.saturating_sub(y)));
                        let len = (bpr * h) as usize;
                        if cw > 0 && ch > 0 && level < levels && off as usize + len <= blob.len() {
                            STATS[3].fetch_add(len as u64, Relaxed);
                            gpu::write_texture(tex, level, [x, y, layer], blob[off as usize..].as_ptr(), len, bpr, h, [cw, ch, 1]);
                        }
                    }
                    i += 10;
                }
                CREATE_BUFFER => {
                    let (id, size) = (a(1), a(2).div_ceil(4) * 4);
                    self.clear_groups2();
                    let b = gpu::buffer(size.max(4), usage::VERTEX | usage::INDEX | usage::UNIFORM | usage::COPY_DST | usage::COPY_SRC);
                    if let Some((old, _)) = self.buffers.insert(id, (b, size.max(4))) {
                        self.doomed.push(old);
                    }
                    i += 3;
                }
                DESTROY_BUFFER => {
                    if let Some((b, _)) = self.buffers.remove(&a(1)) {
                        self.doomed.push(b);
                        self.clear_groups2();
                    }
                    i += 2;
                }
                WRITE_BUFFER => {
                    let (id, at, off, len) = (a(1), a(2), a(3) as usize, a(4));
                    if let Some(&(b, size)) = self.buffers.get(&id) {
                        let n = (len.div_ceil(4) * 4).min(size.saturating_sub(at)) as usize;
                        if n > 0 && off + n <= blob.len().div_ceil(4) * 4 {
                            gpu::write_buffer(b, at, unsafe { std::slice::from_raw_parts(blob.as_ptr().add(off), n) });
                        }
                    }
                    i += 5;
                }
                CREATE_PIPELINE2 => {
                    let (id, n) = (a(1), a(2) as usize);
                    if let Some(old) = self.pipelines2.insert(id, Pipe2 { key: c[i + 3..i + 3 + n].to_vec(), pipeline: 0, layouts: Vec::new(), kinds: Vec::new() }) {
                        gpu::drop(old.pipeline);
                        old.layouts.iter().for_each(|&l| gpu::drop(l));
                    }
                    i += 3 + n;
                }
                SET_TARGETS2 => {
                    let n = a(1) as usize;
                    let mut t = vec![n as u32];
                    t.extend_from_slice(&c[i + 2..i + 2 + n * 3 + 3]);
                    if self.targets2.as_ref() != Some(&t) {
                        self.end_pass();
                        self.target = [0; 3];
                        self.pending = None;
                        self.targets2 = Some(t);
                    }
                    i += 2 + n * 3 + 3;
                }
                DRAW2 => i = self.draw2(c, i),
                CLEAR_VIEW => {
                    let (tex, level, layer, flags) = (a(1), a(2), a(3), a(4));
                    let color = [f32of(a(5)), f32of(a(6)), f32of(a(7)), f32of(a(8))];
                    self.clear_view(tex, level, layer, flags, color, f32of(a(9)), a(10));
                    i += 11;
                }
                GEN_MIPS => {
                    self.gen_mips(a(1));
                    i += 2;
                }
                _ => {
                    eprintln!("d3d9: unknown command {op} at {i}");
                    break;
                }
            }
        }
        self.flush_z();
        self.end_pass();
        self.submit();
    }

    fn shown(&self) -> Id {
        self.overlay.map_or(0, |o| o.view)
    }

    fn clear_groups2(&mut self) {
        for (_, (_, g, _)) in self.groups2.drain() {
            gpu::drop(g);
        }
    }

    fn submit(&mut self) {
        if self.open {
            self.submits.set(self.submits.get() + 1);
            gpu::submit();
            self.open = false;
            self.used.clear();
        }
        // (after the commands that use them are submitted)
        for x in self.doomed.drain(..) {
            gpu::destroy(x);
        }
    }

    /// Before writing a texture the open commands use: submits them first (queue order).
    fn flush_if_used(&mut self, id: u32) {
        if self.used.contains(&id) {
            self.end_pass();
            self.submit();
        }
    }

    fn use_tex(&mut self, id: u32) {
        if !self.used.contains(&id) {
            self.used.push(id);
        }
    }

    /// A whole-target clear: merged into one not done yet (Siglus clears color+z+stencil, then z,
    /// then stencil, each a pass of its own otherwise).
    fn clear_whole(&mut self, c: Clear) {
        if self.pass {
            self.end_pass();
        }
        self.pending = Some(match self.pending {
            Some(p) => Clear {
                flags: p.flags | c.flags,
                color: if c.flags & 1 != 0 { c.color } else { p.color },
                z: if c.flags & 2 != 0 { c.z } else { p.z },
                stencil: if c.flags & 4 != 0 { c.stencil } else { p.stencil },
            },
            None => c,
        });
    }

    fn end_pass(&mut self) {
        if self.pass {
            gpu::end_pass();
            self.pass = false;
        }
        // a whole-target clear nobody drew after still has to happen
        if let Some(p) = self.pending.take() {
            if self.target[0] != 0 {
                if p.flags & 1 == 0 && self.target[2] != 0 && p.flags & 6 != 0 {
                    // depth/stencil only: done by the next pass that uses that depth buffer
                    self.flush_z();
                    self.pending_z = Some((self.target[2], p));
                } else {
                    // (no pass when the target is gone: the clear goes with it)
                    self.pending = Some(p);
                    if self.begin_pass() {
                        gpu::end_pass();
                        self.pass = false;
                    }
                    self.pending = None;
                }
            }
        }
    }

    /// The depth-only clear waiting for a pass, as a pass of its own.
    fn flush_z(&mut self) {
        let Some((z, c)) = self.pending_z.take() else { return };
        let Some(t) = self.textures.get(&z) else { return };
        let s = t.format == DS;
        let view = t.view;
        self.open = true;
        self.use_tex(z);
        gpu::begin_pass(&format!(
            r#"{{"colorAttachments":[],"depthStencilAttachment":{{"view":{view},"depthLoadOp":{},"depthClearValue":{},"depthStoreOp":"store"{}}}}}"#,
            q(if c.flags & 2 != 0 { "clear" } else { "load" }),
            num(c.z),
            if s { format!(r#","stencilLoadOp":{},"stencilClearValue":{},"stencilStoreOp":"store""#, q(if c.flags & 4 != 0 { "clear" } else { "load" }), c.stencil) } else { String::new() }
        ));
        gpu::end_pass();
        STATS[2].fetch_add(1, Relaxed);
    }

    fn begin_pass(&mut self) -> bool {
        if self.pass {
            return true;
        }
        if let Some(t) = self.targets2.clone() {
            return self.begin_pass2(&t);
        }
        let Some(t) = self.textures.get(&self.target[0]) else { return false };
        let level = self.target[1].min(t.levels - 1);
        let (tw, th, tex, format) = ((t.w >> level).max(1), (t.h >> level).max(1), t.tex, t.format);
        let z = if self.target[2] != 0 { self.depth_for(self.target[2], tw, th) } else { None };
        let mut clear = self.pending.take();
        // a depth clear waiting for this depth buffer
        if let Some((zt, c)) = self.pending_z {
            if z.is_some() && zt == self.target[2] {
                self.pending_z = None;
                let mut m = clear.unwrap_or(Clear { flags: 0, color: [0.0; 4], z: 1.0, stencil: 0 });
                if m.flags & 2 == 0 && c.flags & 2 != 0 {
                    m.z = c.z;
                }
                if m.flags & 4 == 0 && c.flags & 4 != 0 {
                    m.stencil = c.stencil;
                }
                m.flags |= c.flags & 6;
                clear = Some(m);
            } else {
                self.flush_z();
            }
        }
        self.use_tex(self.target[0]);
        if z.is_some() {
            self.use_tex(self.target[2]);
        }
        let cl = clear.unwrap_or(Clear { flags: 0, color: [0.0; 4], z: 1.0, stencil: 0 });
        let view = gpu::view(tex, &format!(r#"{{"baseMipLevel":{level},"mipLevelCount":1}}"#));
        let color = format!(
            r#"{{"view":{view},"loadOp":{},"clearValue":[{},{},{},{}],"storeOp":"store"}}"#,
            q(if cl.flags & 1 != 0 { "clear" } else { "load" }),
            num(cl.color[0]),
            num(cl.color[1]),
            num(cl.color[2]),
            num(cl.color[3])
        );
        let depth = match z {
            Some(zv) => format!(
                r#","depthStencilAttachment":{{"view":{zv},"depthLoadOp":{},"depthClearValue":{},"depthStoreOp":"store","stencilLoadOp":{},"stencilClearValue":{},"stencilStoreOp":"store"}}"#,
                q(if cl.flags & 2 != 0 { "clear" } else { "load" }),
                num(cl.z),
                q(if cl.flags & 4 != 0 { "clear" } else { "load" }),
                cl.stencil
            ),
            None => String::new(),
        };
        let _ = format;
        self.open = true;
        gpu::begin_pass(&format!(r#"{{"colorAttachments":[{color}]{depth}}}"#));
        gpu::drop(view);
        self.pass = true;
        STATS[2].fetch_add(1, Relaxed);
        true
    }

    /// Direct3D 9 lets a depth buffer be larger than the color target; WebGPU wants equal
    /// sizes, so such passes get a depth texture of the target's size (its contents are not
    /// shared with the real one). Returns the view to use.
    fn depth_for(&mut self, id: u32, w: u32, h: u32) -> Option<Id> {
        let z = self.textures.get(&id)?;
        if z.w == w && z.h == h {
            return Some(z.view);
        }
        if let Some(&(_, v)) = z.fits.get(&(w, h)) {
            return Some(v);
        }
        let format = z.format;
        let (depth, stencil) = z.zclear.unwrap_or((1.0, 0));
        let tex = gpu::texture(&format!(r#"{{"size":[{w},{h}],"format":{},"usage":{}}}"#, q(format), usage::T_RENDER));
        let view = gpu::view(tex, "");
        self.textures.get_mut(&id)?.fits.insert((w, h), (tex, view));
        // starts as the depth buffer was last cleared (not 0, which fails every LESS test)
        let was = self.pass;
        if was {
            gpu::end_pass();
            self.pass = false;
        }
        self.open = true;
        gpu::begin_pass(&format!(
            r#"{{"colorAttachments":[],"depthStencilAttachment":{{"view":{view},"depthLoadOp":"clear","depthClearValue":{},"depthStoreOp":"store"{}}}}}"#,
            num(depth),
            if format == DS { format!(r#","stencilLoadOp":"clear","stencilClearValue":{stencil},"stencilStoreOp":"store""#) } else { String::new() }
        ));
        gpu::end_pass();
        Some(view)
    }

    fn target_size(&self) -> (u32, u32) {
        match self.textures.get(&self.target[0]) {
            Some(t) => ((t.w >> self.target[1]).max(1), (t.h >> self.target[1]).max(1)),
            None => (0, 0),
        }
    }

    fn uniform_slot(&mut self, v: &[f32]) -> u32 {
        self.clear_slot = (self.clear_slot + 1) % 64;
        let slot = self.clear_slot;
        let b: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        gpu::write_buffer(self.clear_uniform, slot * 256, &b);
        slot
    }

    #[allow(clippy::too_many_arguments)]
    fn clear_rect(&mut self, flags: u32, color: [f32; 4], z: f32, x: u32, y: u32, w: u32, h: u32) {
        let (tw, th) = self.target_size();
        let (x0, y0) = (x.min(tw), y.min(th));
        let (x1, y1) = (x.saturating_add(w).min(tw), y.saturating_add(h).min(th));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        if x0 == 0 && y0 == 0 && x1 == tw && y1 == th {
            self.clear_whole(Clear { flags, color, z, stencil: 0 });
            return;
        }
        if !self.begin_pass() {
            return;
        }
        let fmt = self.textures[&self.target[0]].format;
        let depth = self.target[2] != 0;
        let key = (fmt, depth, flags & 3);
        let pipe = match self.clear_pipes.get(&key) {
            Some(&p) => p,
            None => {
                if self.clear_module == 0 {
                    self.clear_module = gpu::shader(CLEAR_SHADER);
                }
                let m = self.clear_module;
                let ds = if depth {
                    format!(r#","depthStencil":{{"format":"depth24plus-stencil8","depthWriteEnabled":{},"depthCompare":"always"}}"#, flags & 2 != 0)
                } else {
                    String::new()
                };
                let p = gpu::render_pipeline(&format!(
                    r#"{{"layout":"auto","vertex":{{"module":{m},"entryPoint":"vs"}},"fragment":{{"module":{m},"entryPoint":"fs","targets":[{{"format":{},"writeMask":{}}}]}},"primitive":{{"topology":"triangle-list"}}{ds}}}"#,
                    q(fmt),
                    if flags & 1 != 0 { 15 } else { 0 }
                ));
                self.clear_pipes.insert(key, p);
                p
            }
        };
        let slot = self.uniform_slot(&[color[0], color[1], color[2], color[3], z, 0.0, 0.0, 0.0]);
        let layout = gpu::pipeline_group_layout(pipe, 0);
        let group = gpu::bind_group(layout, &[(0, Res::Buffer(self.clear_uniform, slot * 256, 32))]);
        gpu::drop(layout);
        gpu::set_pipeline(pipe);
        gpu::set_bind_group(0, group, &[]);
        gpu::drop(group);
        gpu::set_viewport(0.0, 0.0, tw as f32, th as f32, 0.0, 1.0);
        gpu::set_scissor(x0, y0, x1 - x0, y1 - y0);
        gpu::draw(3, 1, 0, 0);
        // (the clear uniform slots are written now, at queue time: no more than 64 per batch)
        if slot == 63 {
            self.end_pass();
            self.submit();
        }
    }

    fn create_pipeline(&mut self, k: &[u32]) -> Pipe {
        let (vs, ps, topo, strip, cull, color_fmt, depth_fmt, depth_write, depth_cmp) = (k[0], k[1], k[2], k[3], k[4], k[5], k[6], k[7], k[8]);
        let blend = &k[9..16];
        let wmask = k[16];
        let mut p = 17;
        let nbuf = k[p];
        p += 1;
        let mut buffers = Vec::new();
        for _ in 0..nbuf {
            let stride = k[p];
            let n = k[p + 1];
            p += 2;
            let mut attrs = Vec::new();
            for _ in 0..n {
                attrs.push(format!(r#"{{"format":{},"offset":{},"shaderLocation":{}}}"#, q(pick(&VERTEX_FORMATS, k[p], "float32x4")), k[p + 1], k[p + 2]));
                p += 3;
            }
            buffers.push(format!(r#"{{"arrayStride":{},"attributes":[{}]}}"#, stride.div_ceil(4) * 4, attrs.join(",")));
        }
        let format = tex_format(color_fmt);
        let depth = depth_fmt != 0xff;
        let blend_state = if blend[0] != 0 {
            let comp = |s: &str, d: &str, o: &str| {
                if o == "min" || o == "max" {
                    format!(r#"{{"srcFactor":"one","dstFactor":"one","operation":{}}}"#, q(o))
                } else {
                    format!(r#"{{"srcFactor":{},"dstFactor":{},"operation":{}}}"#, q(s), q(d), q(o))
                }
            };
            format!(
                r#","blend":{{"color":{},"alpha":{}}}"#,
                comp(pick(&BLEND, blend[1], "one"), pick(&BLEND, blend[2], "zero"), pick(&BLEND_OP, blend[3], "add")),
                comp(pick(&BLEND_ALPHA, blend[4], "one"), pick(&BLEND_ALPHA, blend[5], "zero"), pick(&BLEND_OP, blend[6], "add"))
            )
        } else {
            String::new()
        };
        let strip_fmt = match strip {
            2 => r#","stripIndexFormat":"uint16""#,
            4 => r#","stripIndexFormat":"uint32""#,
            _ => "",
        };
        let cull = match cull {
            1 => "front",
            2 => "back",
            _ => "none",
        };
        let ds = if depth {
            format!(r#","depthStencil":{{"format":"depth24plus-stencil8","depthWriteEnabled":{},"depthCompare":{}}}"#, depth_write != 0, q(pick(&COMPARE, depth_cmp, "always")))
        } else {
            String::new()
        };
        let (Some(&vm), Some(&pm)) = (self.shaders.get(&vs), self.shaders.get(&ps)) else { return Pipe { pipeline: 0 } };
        let pipeline = gpu::render_pipeline(&format!(
            r#"{{"layout":{},"vertex":{{"module":{vm},"entryPoint":"main","buffers":[{}]}},"fragment":{{"module":{pm},"entryPoint":"main","targets":[{{"format":{}{blend_state},"writeMask":{wmask}}}]}},"primitive":{{"topology":{}{strip_fmt},"cullMode":{},"frontFace":"ccw"}}{ds}}}"#,
            self.layout,
            buffers.join(","),
            q(format),
            q(pick(&TOPOLOGY, topo, "triangle-list")),
            q(cull)
        ));
        Pipe { pipeline }
    }

    fn sampler(&mut self, key: u32) -> Id {
        if let Some(&s) = self.samplers.get(&key) {
            return s;
        }
        let filter = |f: u32| if f >= 2 { "linear" } else { "nearest" };
        let mip = (key >> 10) & 3;
        let s = gpu::sampler(&format!(
            r#"{{"addressModeU":{},"addressModeV":{},"magFilter":{},"minFilter":{},"mipmapFilter":{},"lodMaxClamp":{}}}"#,
            q(pick(&ADDRESS, key & 7, "repeat")),
            q(pick(&ADDRESS, (key >> 3) & 7, "repeat")),
            q(filter((key >> 6) & 3)),
            q(filter((key >> 8) & 3)),
            q(if mip >= 2 { "linear" } else { "nearest" }),
            if mip == 0 { 0 } else { 32 }
        ));
        self.samplers.insert(key, s);
        s
    }

    fn draw(&mut self, c: &[u32], i: usize) -> usize {
        let pipe = self.pipelines.get(&c[i + 1]).map(|p| p.pipeline).unwrap_or(0);
        let [vx, vy, vw, vh, minz, maxz] = std::array::from_fn(|k| f32of(c[i + 2 + k]));
        let (sx, sy, sw, sh) = (c[i + 8], c[i + 9], c[i + 10], c[i + 11]);
        let (vso, vss, pso, pss) = (c[i + 12], c[i + 13], c[i + 14], c[i + 15]);
        let blend_factor = c[i + 16];
        let ntex = c[i + 17] as usize;
        let mut p = i + 18;
        let texs: Vec<[u32; 3]> = (0..ntex).map(|t| [c[p + 3 * t], c[p + 3 * t + 1], c[p + 3 * t + 2]]).collect();
        p += 3 * ntex;
        let nbuf = c[p] as usize;
        p += 1;
        let offs = c[p..p + nbuf].to_vec();
        p += nbuf;
        let (ifmt, ioff, count, base) = (c[p], c[p + 1], c[p + 2], c[p + 3] as i32);
        p += 4;
        if pipe == 0 || self.data_buf == 0 || count == 0 {
            return p;
        }
        let (tw, th) = self.target_size();
        // viewport and scissor inside the target
        let (x0, y0) = (sx.min(tw), sy.min(th));
        let (x1, y1) = (x0.max(sx.saturating_add(sw).min(tw)), y0.max(sy.saturating_add(sh).min(th)));
        if x1 <= x0 || y1 <= y0 {
            return p;
        }
        let (vpx, vpy) = (vx.max(-32768.0), vy.max(-32768.0));
        let (vpw, vph) = (vw.min(32768.0).max(1.0), vh.min(32768.0).max(1.0));
        if !self.begin_pass() {
            return p;
        }
        for t in &texs {
            if t[1] != 0 {
                self.use_tex(t[1]);
            }
        }
        gpu::set_pipeline(pipe);
        // WebGPU wants the viewport inside the target
        let (cx0, cy0) = (vpx.max(0.0), vpy.max(0.0));
        let (cx1, cy1) = ((tw as f32).min(vpx + vpw), (th as f32).min(vpy + vph));
        if cx1 <= cx0 || cy1 <= cy0 {
            return p;
        }
        gpu::set_viewport(cx0, cy0, cx1 - cx0, cy1 - cy0, minz.min(maxz), minz.max(maxz));
        let (sx0, sy0) = (x0.max(cx0.floor() as u32), y0.max(cy0.floor() as u32));
        let (sx1, sy1) = (x1.min(cx1.ceil() as u32), y1.min(cy1.ceil() as u32));
        if sx1 <= sx0 || sy1 <= sy0 {
            gpu::set_scissor(sx0.min(tw), sy0.min(th), 0, 0);
            return p;
        }
        gpu::set_scissor(sx0, sy0, sx1 - sx0, sy1 - sy0);
        let g0 = match self.groups0.get(&(vss, pss)) {
            Some(&g) => g,
            None => {
                let g = gpu::bind_group(self.layout0, &[(0, Res::Buffer(self.data_buf, 0, vss)), (1, Res::Buffer(self.data_buf, 0, pss))]);
                self.groups0.insert((vss, pss), g);
                g
            }
        };
        gpu::set_bind_group(0, g0, &[vso, pso]);
        // (sampling the texture being drawn to is not allowed in WebGPU: it reads as white)
        let rt = if self.targets2.is_some() { 0 } else { self.target[0] };
        let mut k1: Vec<u32> = texs.iter().flatten().copied().collect();
        if rt != 0 && texs.iter().any(|t| t[1] == rt) {
            k1.push(u32::MAX);
            k1.push(rt);
        }
        let g1 = match self.groups1.get(&k1) {
            Some(&g) => g,
            None => {
                let mut entries = Vec::new();
                for s in 0..8u32 {
                    let t = texs.iter().find(|x| x[0] == s);
                    let view = t
                        .and_then(|t| self.textures.get(&t[1]).filter(|tex| tex.format != DS && !(rt != 0 && t[1] == rt)))
                        .map_or(self.white, |tex| tex.view);
                    let smp = self.sampler(t.map_or(0x5, |t| t[2]));
                    entries.push((s * 2, Res::Obj(view)));
                    entries.push((s * 2 + 1, Res::Obj(smp)));
                }
                let g = gpu::bind_group(self.layout1, &entries);
                if self.groups1.len() > 4096 {
                    drop_all(&mut self.groups1);
                }
                self.groups1.insert(k1, g);
                g
            }
        };
        gpu::set_bind_group(1, g1, &[]);
        for (b, &o) in offs.iter().enumerate() {
            gpu::set_vertex_buffer(b as u32, self.data_buf, o);
        }
        gpu::set_blend_constant(color_of(blend_factor));
        if ifmt != 0 {
            gpu::set_index_buffer(self.data_buf, ifmt == 4, ioff);
            gpu::draw_indexed(count, 1, 0, base, 0);
        } else {
            gpu::draw(count, 1, 0, 0);
        }
        p
    }

    fn blit_pipe(&mut self, format: &'static str) -> Id {
        if let Some(&p) = self.blit_pipes.get(format) {
            return p;
        }
        if self.blit_module == 0 {
            self.blit_module = gpu::shader(BLIT_SHADER);
        }
        let m = self.blit_module;
        let p = gpu::render_pipeline(&format!(
            r#"{{"layout":"auto","vertex":{{"module":{m},"entryPoint":"vs"}},"fragment":{{"module":{m},"entryPoint":"fs","targets":[{{"format":{}}}]}},"primitive":{{"topology":"triangle-list"}}}}"#,
            q(format)
        ));
        self.blit_pipes.insert(format, p);
        p
    }

    fn copy_rect(&mut self, a: [u32; 13]) {
        let [st, sl, sx, sy, sw, sh, dt, dl, dx, dy, dw, dh, filter] = a;
        let (Some(s), Some(t)) = (self.textures.get(&st), self.textures.get(&dt)) else { return };
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 || sw > 1 << 15 || dw > 1 << 15 {
            return;
        }
        let (s_tex, s_fmt, s_w, s_h) = (s.tex, s.format, s.w, s.h);
        let (t_tex, t_fmt) = (t.tex, t.format);
        self.end_pass();
        if sw == dw && sh == dh && s_fmt == t_fmt && st != dt {
            self.open = true;
            gpu::copy_texture(s_tex, sl, [sx, sy, 0], t_tex, dl, [dx, dy, 0], [sw, sh, 1]);
            self.use_tex(st);
            self.use_tex(dt);
            return;
        }
        if st == dt || t_fmt == DS {
            return;
        }
        let pipe = self.blit_pipe(t_fmt);
        let (lw, lh) = ((s_w >> sl).max(1) as f32, (s_h >> sl).max(1) as f32);
        let slot = self.uniform_slot(&[sx as f32 / lw, sy as f32 / lh, sw as f32 / lw, sh as f32 / lh]);
        let smp = self.sampler(if filter != 0 { 0x3 | (3 << 3) | (2 << 6) | (2 << 8) } else { 0x1b });
        let view = gpu::view(s_tex, &format!(r#"{{"baseMipLevel":{sl},"mipLevelCount":1}}"#));
        let layout = gpu::pipeline_group_layout(pipe, 0);
        let group = gpu::bind_group(layout, &[(0, Res::Obj(view)), (1, Res::Obj(smp)), (2, Res::Buffer(self.clear_uniform, slot * 256, 16))]);
        let saved = self.target;
        let saved2 = self.targets2.take();
        self.target = [dt, dl, 0];
        if self.begin_pass() {
            gpu::set_pipeline(pipe);
            gpu::set_bind_group(0, group, &[]);
            gpu::set_viewport(dx as f32, dy as f32, dw as f32, dh as f32, 0.0, 1.0);
            gpu::draw(3, 1, 0, 0);
            self.use_tex(st);
            self.end_pass();
        }
        gpu::drop(group);
        gpu::drop(layout);
        gpu::drop(view);
        self.target = saved;
        self.targets2 = saved2;
        if slot == 63 {
            self.submit();
        }
    }

    fn readback(&mut self, a: [u32; 9]) {
        let [req, id, level, x, y, w, h, dst, pitch] = a;
        let Some(t) = self.textures.get(&id).filter(|t| !is_depth(t.format) && w > 0 && h > 0) else {
            vnhost_d3d9_readback_done(req);
            return;
        };
        let (tex, format) = (t.tex, t.format);
        self.end_pass();
        let bpp = bpp(format);
        let bpr = (w * bpp).div_ceil(256) * 256;
        let buf = gpu::buffer(bpr * h, usage::COPY_DST | usage::MAP_READ);
        self.open = true;
        gpu::copy_to_buffer(tex, level, x, y, buf, bpr, w, h);
        self.submit();
        let mut data = vec![0u8; (bpr * h) as usize];
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        gpu::read_buffer(buf, bpr * h, data.as_mut_ptr(), token);
        self.readbacks.insert(token, Readback { req, buf, data, bpr, row: w * bpp, h, dst, pitch });
    }

    // ---- Direct3D 11 ----

    fn create_texture2(&mut self, a: [u32; 7]) {
        let [id, w, h, layers, mips, format, dim_code] = a;
        let fmt = tex_format(format);
        let dim = match dim_code {
            1 => "3d",
            2 => "1d",
            _ => "2d",
        };
        let depth = is_depth(fmt);
        let renderable = dim == "2d" && !fmt.ends_with("snorm") && (fmt != "rg11b10ufloat" || gpu::has_feature("rg11b10ufloat-renderable"));
        let levels = if dim == "1d" { 1 } else { mips.max(1) };
        let u = usage::T_BINDING | usage::T_COPY_DST | usage::T_COPY_SRC | if renderable || depth { usage::T_RENDER } else { 0 };
        let tex = gpu::texture(&format!(
            r#"{{"size":[{w},{},{}],"dimension":{},"format":{},"mipLevelCount":{levels},"usage":{u}}}"#,
            if dim == "1d" { 1 } else { h },
            layers.max(1),
            q(dim),
            q(fmt)
        ));
        if let Some(old) = self.textures.remove(&id) {
            self.doomed.push(old.tex);
            self.tex_gen += 1;
            if old.view != self.shown() {
                gpu::drop(old.view);
            }
            old.views.values().for_each(|&v| gpu::drop(v));
            for &(t, v) in old.fits.values() {
                self.doomed.push(t);
                gpu::drop(v);
            }
        }
        self.created.set(self.created.get() + 1);
        let view = gpu::view(
            tex,
            &format!(r#"{{"dimension":{}{},"aspect":{}}}"#, q(dim), if dim == "2d" { r#","arrayLayerCount":1"# } else { "" }, q(if depth { "depth-only" } else { "all" })),
        );
        self.textures.insert(id, Tex { tex, w, h, levels, format: fmt, view, layers: layers.max(1), dim, views: HashMap::new(), fits: HashMap::new(), zclear: None });
    }

    /// A view of texture `id` for binding or rendering (cached on the texture).
    #[allow(clippy::too_many_arguments)]
    fn view2(&mut self, id: u32, dim: u8, mip: u32, mips: u32, layer: u32, layers: u32, depth_only: bool) -> Id {
        let t = self.textures.get_mut(&id).unwrap();
        let k = (dim, mip, mips, layer, layers, depth_only);
        if let Some(&v) = t.views.get(&k) {
            return v;
        }
        let dname = VIEW_DIM.get(dim as usize).copied().unwrap_or("2d");
        let all = t.layers;
        let base = layer.min(all - 1);
        let mut count = if matches!(dname, "2d" | "1d" | "3d") { 1 } else { layers.min(all - base).max(1) };
        if dname == "cube" {
            count = 6;
        }
        if dname == "cube-array" {
            count = (count / 6 * 6).max(6);
        }
        let m = mip.min(t.levels - 1);
        let v = gpu::view(
            t.tex,
            &format!(
                r#"{{"dimension":{},"baseMipLevel":{m},"mipLevelCount":{},"baseArrayLayer":{},"arrayLayerCount":{},"aspect":{}}}"#,
                q(dname),
                mips.min(t.levels - m).max(1),
                if dname == "3d" { 0 } else { base },
                if dname == "3d" { 1 } else { count },
                q(if depth_only { "depth-only" } else { "all" })
            ),
        );
        t.views.insert(k, v);
        v
    }

    fn begin_pass2(&mut self, tg: &[u32]) -> bool {
        let n = tg[0] as usize;
        let mut colors = Vec::new();
        for k in 0..n {
            let id = tg[1 + k * 3];
            if !self.textures.contains_key(&id) {
                return false;
            }
            self.use_tex(id);
            let v = self.view2(id, 1, tg[2 + k * 3], 1, tg[3 + k * 3], 1, false);
            colors.push(format!(r#"{{"view":{v},"loadOp":"load","storeOp":"store"}}"#));
        }
        let zi = 1 + n * 3;
        let zid = tg[zi];
        let z = if zid != 0 && self.textures.contains_key(&zid) { Some(zid) } else { None };
        if colors.is_empty() && z.is_none() {
            return false;
        }
        let mut depth = String::new();
        if let Some(zid) = z {
            self.use_tex(zid);
            let zt = &self.textures[&zid];
            let stencil = zt.format == DS;
            let (zw, zh) = (zt.w, zt.h);
            // a depth buffer of another size than the targets (Eushully makes a 0x0 one) is
            // invalid in WebGPU: a stand-in of the targets' size
            let mut zview = None;
            if n > 0 {
                let t0 = &self.textures[&tg[1]];
                let (w, h) = ((t0.w >> tg[2]).max(1), (t0.h >> tg[2]).max(1));
                let zl = tg[zi + 1];
                if (zw >> zl).max(1) != w || (zh >> zl).max(1) != h {
                    zview = self.depth_for(zid, w, h);
                }
            }
            let v = match zview {
                Some(v) => v,
                None => self.view2(zid, 1, tg[zi + 1], 1, tg[zi + 2], 1, false),
            };
            depth = format!(
                r#","depthStencilAttachment":{{"view":{v},"depthLoadOp":"load","depthStoreOp":"store"{}}}"#,
                if stencil { r#","stencilLoadOp":"load","stencilStoreOp":"store""# } else { "" }
            );
        }
        self.open = true;
        gpu::begin_pass(&format!(r#"{{"colorAttachments":[{}]{depth}}}"#, colors.join(",")));
        self.pass = true;
        STATS[2].fetch_add(1, Relaxed);
        true
    }

    /// The size of the Direct3D 11 targets' first level.
    fn target_size2(&self) -> (u32, u32) {
        let Some(tg) = &self.targets2 else { return (0, 0) };
        let n = tg[0] as usize;
        let (id, level) = if n > 0 { (tg[1], tg[2]) } else { (tg[1 + n * 3], tg[2 + n * 3]) };
        match self.textures.get(&id) {
            Some(t) => ((t.w >> level).max(1), (t.h >> level).max(1)),
            None => (0, 0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn clear_view(&mut self, tex: u32, level: u32, layer: u32, flags: u32, color: [f32; 4], depth: f32, stencil: u32) {
        if !self.textures.contains_key(&tex) {
            return;
        }
        self.end_pass();
        self.use_tex(tex);
        let view = self.view2(tex, 1, level, 1, layer, 1, false);
        let t = self.textures.get_mut(&tex).unwrap();
        self.open = true;
        if is_depth(t.format) {
            let s = t.format == DS;
            let d = depth.clamp(0.0, 1.0);
            // (and the stand-ins of other sizes, see depth_for)
            if flags & 2 != 0 {
                t.zclear = Some((d, t.zclear.map_or(0, |z| z.1)));
            }
            if flags & 4 != 0 {
                t.zclear = Some((t.zclear.map_or(1.0, |z| z.0), stencil));
            }
            let views: Vec<Id> = std::iter::once(view).chain(t.fits.values().map(|f| f.1)).collect();
            for v in views {
                gpu::begin_pass(&format!(
                    r#"{{"colorAttachments":[],"depthStencilAttachment":{{"view":{v},"depthLoadOp":{},"depthClearValue":{},"depthStoreOp":"store"{}}}}}"#,
                    q(if flags & 2 != 0 { "clear" } else { "load" }),
                    num(d),
                    if s { format!(r#","stencilLoadOp":{},"stencilClearValue":{stencil},"stencilStoreOp":"store""#, q(if flags & 4 != 0 { "clear" } else { "load" })) } else { String::new() }
                ));
                gpu::end_pass();
            }
        } else if flags & 1 != 0 {
            gpu::begin_pass(&format!(
                r#"{{"colorAttachments":[{{"view":{view},"loadOp":"clear","clearValue":[{},{},{},{}],"storeOp":"store"}}]}}"#,
                num(color[0]),
                num(color[1]),
                num(color[2]),
                num(color[3])
            ));
            gpu::end_pass();
        }
        STATS[2].fetch_add(1, Relaxed);
    }

    fn gen_mips(&mut self, tex: u32) {
        let Some(t) = self.textures.get(&tex).filter(|t| t.levels >= 2 && t.dim == "2d" && !is_depth(t.format)) else { return };
        let (format, layers, levels) = (t.format, t.layers, t.levels);
        self.end_pass();
        let pipe = self.blit_pipe(format);
        let slot = self.uniform_slot(&[0.0, 0.0, 1.0, 1.0]);
        let smp = self.sampler2((1 << 9) | (1 << 10) | 2 | (2 << 3) | (2 << 6), 0, 0);
        let layout = gpu::pipeline_group_layout(pipe, 0);
        self.open = true;
        for layer in 0..layers {
            for l in 1..levels {
                let src = self.view2(tex, 1, l - 1, 1, layer, 1, false);
                let dst = self.view2(tex, 1, l, 1, layer, 1, false);
                let group = gpu::bind_group(layout, &[(0, Res::Obj(src)), (1, Res::Obj(smp)), (2, Res::Buffer(self.clear_uniform, slot * 256, 16))]);
                gpu::begin_pass(&format!(r#"{{"colorAttachments":[{{"view":{dst},"loadOp":"clear","storeOp":"store"}}]}}"#));
                gpu::set_pipeline(pipe);
                gpu::set_bind_group(0, group, &[]);
                gpu::draw(3, 1, 0, 0);
                gpu::end_pass();
                gpu::drop(group);
            }
        }
        gpu::drop(layout);
        self.use_tex(tex);
        if slot == 63 {
            self.submit();
        }
    }

    /// sampler key: crates/vnhost/src/d3d11 sampler_key; LOD clamps as f32 bits
    fn sampler2(&mut self, key: u32, min_lod: u32, max_lod: u32) -> Id {
        if let Some(&s) = self.samplers2.get(&(key, min_lod, max_lod)) {
            return s;
        }
        let aniso = (key >> 12) & 1 != 0;
        let lin = |b: u32| if aniso || (key >> b) & 1 != 0 { "linear" } else { "nearest" };
        let func = (key >> 13) & 15;
        let finite = |f: f32| if f.is_nan() { 0.0 } else { f };
        let lo = finite(f32of(min_lod)).clamp(0.0, 32.0);
        let hi = finite(f32of(max_lod)).clamp(lo, 32.0);
        let s = gpu::sampler(&format!(
            r#"{{"addressModeU":{},"addressModeV":{},"addressModeW":{},"magFilter":{},"minFilter":{},"mipmapFilter":{},"lodMinClamp":{},"lodMaxClamp":{},"maxAnisotropy":{}{}}}"#,
            q(pick(&ADDRESS2, key & 7, "repeat")),
            q(pick(&ADDRESS2, (key >> 3) & 7, "repeat")),
            q(pick(&ADDRESS2, (key >> 6) & 7, "repeat")),
            q(lin(9)),
            q(lin(10)),
            q(if aniso || (key >> 11) & 1 != 0 { "linear" } else { "nearest" }),
            num(lo),
            num(hi),
            if aniso { ((key >> 17) & 31).clamp(1, 16) } else { 1 },
            if func != 0 { format!(r#","compare":{}"#, q(pick(&COMPARE, func, "always"))) } else { String::new() }
        ));
        self.samplers2.insert((key, min_lod, max_lod), s);
        s
    }

    /// A 1x1 texture of zeros for unbound slots: view dimension, kind (1 float, 4 uint, 5 sint, 6 depth).
    fn dummy(&mut self, vdim: u32, kind: u32) -> Id {
        if let Some(&v) = self.dummies.get(&(vdim, kind)) {
            return v;
        }
        let dim = pick(&VIEW_DIM, vdim, "2d");
        let format = match kind {
            4 => "rgba8uint",
            5 => "rgba8sint",
            6 => "depth32float",
            _ => "rgba8unorm",
        };
        let layers = if dim == "cube" || dim == "cube-array" { 6 } else { 1 };
        let tdim = match dim {
            "1d" => "1d",
            "3d" => "3d",
            _ => "2d",
        };
        let tex = gpu::texture(&format!(
            r#"{{"size":[1,1,{layers}],"dimension":{},"format":{},"usage":{}}}"#,
            q(tdim),
            q(format),
            usage::T_BINDING | if kind == 6 { usage::T_RENDER } else { usage::T_COPY_DST }
        ));
        let v = gpu::view(tex, &format!(r#"{{"dimension":{}}}"#, q(dim)));
        self.dummies.insert((vdim, kind), v);
        v
    }

    fn zero(&mut self) -> Id {
        if self.zero_buf == 0 {
            self.zero_buf = gpu::buffer(65536, usage::UNIFORM | usage::VERTEX | usage::INDEX);
        }
        self.zero_buf
    }

    /// The pipeline of `p` with bind group layouts for `groups` (entries of 8 words).
    fn pipeline2(&mut self, id: u32, groups: &[&[u32]]) -> bool {
        let mut kinds = Vec::new();
        for g in groups {
            for e in g.chunks(8) {
                kinds.extend_from_slice(&[e[0], e[1], if e[1] == 1 || e[1] >= 4 { e[3] } else { u32::MAX }]);
            }
            kinds.push(u32::MAX - 1);
        }
        let Some(p) = self.pipelines2.get(&id) else { return false };
        if p.pipeline != 0 && p.kinds == kinds {
            return true;
        }
        let k = p.key.clone();
        let mut layouts = Vec::new();
        for (stage, g) in groups.iter().enumerate() {
            let visibility = if stage == 0 { 1 } else { 2 };
            let mut entries = Vec::new();
            for e in g.chunks(8) {
                let (binding, kind, vdim) = (e[0], e[1], e[3]);
                let vd = q(pick(&VIEW_DIM, vdim, "2d"));
                entries.push(match kind {
                    0 => format!(r#"{{"binding":{binding},"visibility":{visibility},"buffer":{{"type":"uniform"}}}}"#),
                    2 => format!(r#"{{"binding":{binding},"visibility":{visibility},"sampler":{{"type":"filtering"}}}}"#),
                    3 => format!(r#"{{"binding":{binding},"visibility":{visibility},"sampler":{{"type":"comparison"}}}}"#),
                    _ => {
                        let st = match kind {
                            4 => "uint",
                            5 => "sint",
                            6 => "depth",
                            _ => "float",
                        };
                        format!(r#"{{"binding":{binding},"visibility":{visibility},"texture":{{"sampleType":{},"viewDimension":{vd}}}}}"#, q(st))
                    }
                });
            }
            layouts.push(gpu::bind_group_layout(&format!(r#"{{"entries":[{}]}}"#, entries.join(","))));
        }
        // key: crates/vnhost/src/d3d11 draw()
        let mut q_ = 0;
        let mut next = || {
            q_ += 1;
            k.get(q_ - 1).copied().unwrap_or(0)
        };
        let (vs, ps, topo, strip) = (next(), next(), next(), next());
        let (cull, ccw, bias, slope, clamp) = (next(), next(), next() as i32, f32of(next()), f32of(next()));
        let mut qi = 9;
        let mut ds = String::new();
        if k.get(qi) == Some(&0xffff) {
            qi += 1;
        } else {
            let (fmt, en, write, func, sen, masks) = (k[qi], k[qi + 1], k[qi + 2], k[qi + 3], k[qi + 4], k[qi + 5]);
            let face = |o: usize| {
                if sen != 0 {
                    format!(
                        r#"{{"failOp":{},"depthFailOp":{},"passOp":{},"compare":{}}}"#,
                        q(pick(&STENCIL_OP, k[o], "keep")),
                        q(pick(&STENCIL_OP, k[o + 1], "keep")),
                        q(pick(&STENCIL_OP, k[o + 2], "keep")),
                        q(pick(&COMPARE, k[o + 3], "always"))
                    )
                } else {
                    "{}".into()
                }
            };
            let format = TEX_FORMATS.get(fmt as usize).copied().unwrap_or(DS);
            let tri = topo >= 3;
            let stencil = if format == DS {
                format!(
                    r#","stencilFront":{},"stencilBack":{},"stencilReadMask":{},"stencilWriteMask":{}"#,
                    face(qi + 6),
                    face(qi + 10),
                    if sen != 0 { masks & 0xff } else { 0xff },
                    if sen != 0 { (masks >> 8) & 0xff } else { 0 }
                )
            } else {
                String::new()
            };
            ds = format!(
                r#","depthStencil":{{"format":{},"depthWriteEnabled":{},"depthCompare":{}{stencil},"depthBias":{},"depthBiasSlopeScale":{},"depthBiasClamp":{}}}"#,
                q(format),
                en != 0 && write != 0,
                q(if en != 0 { pick(&COMPARE, func, "less") } else { "always" }),
                if tri { bias } else { 0 },
                num(if tri { slope } else { 0.0 }),
                num(if tri { clamp } else { 0.0 })
            );
            qi += 14;
        }
        let mut buffers = Vec::new();
        let nvb = k[qi];
        qi += 1;
        for _ in 0..nvb {
            let (stride, step, n) = (k[qi], k[qi + 1], k[qi + 2]);
            qi += 3;
            let mut attrs = Vec::new();
            for _ in 0..n {
                attrs.push(format!(r#"{{"format":{},"offset":{},"shaderLocation":{}}}"#, q(pick(&VERTEX_FORMATS2, k[qi], "float32x4")), k[qi + 1], k[qi + 2]));
                qi += 3;
            }
            buffers.push(format!(
                r#"{{"arrayStride":{},"stepMode":{},"attributes":[{}]}}"#,
                stride.div_ceil(4) * 4,
                q(if step != 0 { "instance" } else { "vertex" }),
                attrs.join(",")
            ));
        }
        let mut targets = Vec::new();
        let nt = k[qi];
        qi += 1;
        for _ in 0..nt {
            let t = &k[qi..qi + 9];
            qi += 9;
            let format = tex_format(t[0]);
            let comp = |s: u32, d: u32, o: u32| {
                let op = pick(&BLEND_OP, o, "add");
                if op == "min" || op == "max" {
                    format!(r#"{{"operation":{},"srcFactor":"one","dstFactor":"one"}}"#, q(op))
                } else {
                    format!(r#"{{"operation":{},"srcFactor":{},"dstFactor":{}}}"#, q(op), q(pick(&BLEND2, s, "one")), q(pick(&BLEND2, d, "zero")))
                }
            };
            let blendable = !format.ends_with("uint") && !format.ends_with("sint") && format != "r32float" && format != "rg32float";
            let blend = if t[1] != 0 && blendable { format!(r#","blend":{{"color":{},"alpha":{}}}"#, comp(t[2], t[3], t[4]), comp(t[5], t[6], t[7])) } else { String::new() };
            targets.push(format!(r#"{{"format":{}{blend},"writeMask":{}}}"#, q(format), if ps != 0 { t[8] } else { 0 }));
        }
        let fragment = if ps != 0 {
            self.shaders.get(&ps).copied()
        } else {
            if self.no_ps == 0 {
                self.no_ps = gpu::shader("@fragment fn main() {}");
            }
            Some(self.no_ps)
        };
        let (Some(vm), Some(fm)) = (self.shaders.get(&vs).copied(), fragment) else {
            layouts.iter().for_each(|&l| gpu::drop(l));
            return false;
        };
        let pl = gpu::pipeline_layout(&format!(r#"{{"bindGroupLayouts":[{}]}}"#, layouts.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(",")));
        let strip_fmt = match strip {
            1 => r#","stripIndexFormat":"uint16""#,
            2 => r#","stripIndexFormat":"uint32""#,
            _ => "",
        };
        let pipeline = gpu::render_pipeline(&format!(
            r#"{{"layout":{pl},"vertex":{{"module":{vm},"entryPoint":"main","buffers":[{}]}},"fragment":{{"module":{fm},"entryPoint":"main","targets":[{}]}},"primitive":{{"topology":{}{strip_fmt},"cullMode":{},"frontFace":{}}}{ds}}}"#,
            buffers.join(","),
            targets.join(","),
            q(pick(&TOPOLOGY, topo, "triangle-list")),
            q(match cull {
                2 => "front",
                3 => "back",
                _ => "none",
            }),
            q(if ccw != 0 { "ccw" } else { "cw" })
        ));
        gpu::drop(pl);
        let p = self.pipelines2.get_mut(&id).unwrap();
        gpu::drop(p.pipeline);
        p.layouts.iter().for_each(|&l| gpu::drop(l));
        p.pipeline = pipeline;
        p.layouts = layouts;
        p.kinds = kinds;
        true
    }

    fn draw2(&mut self, c: &[u32], i: usize) -> usize {
        let mut q_ = i + 1;
        let pid = c[q_];
        q_ += 1;
        let [vx, vy, vw, vh, minz, maxz] = std::array::from_fn(|k| f32of(c[q_ + k]));
        q_ += 6;
        let (sx, sy, sw, sh, s_on) = (c[q_], c[q_ + 1], c[q_ + 2], c[q_ + 3], c[q_ + 4]);
        q_ += 5;
        let blend: [f64; 4] = std::array::from_fn(|k| f32of(c[q_ + k]) as f64);
        q_ += 4;
        let stencil_ref = c[q_];
        q_ += 1;
        let nvb = c[q_] as usize;
        q_ += 1;
        let vbs: Vec<(u32, u32)> = (0..nvb).map(|b| (c[q_ + 2 * b], c[q_ + 2 * b + 1])).collect();
        q_ += 2 * nvb;
        let (ib, ioff, ifmt) = (c[q_], c[q_ + 1], c[q_ + 2]);
        q_ += 3;
        let mut groups: Vec<&[u32]> = Vec::new();
        for _ in 0..2 {
            let n = c[q_] as usize;
            q_ += 1;
            groups.push(&c[q_..q_ + n * 8]);
            q_ += n * 8;
        }
        let (indexed, count, instances, start, base, first) = (c[q_], c[q_ + 1], c[q_ + 2], c[q_ + 3], c[q_ + 4] as i32, c[q_ + 5]);
        q_ += 6;
        if !self.pipelines2.contains_key(&pid) || count == 0 || instances == 0 {
            return q_;
        }
        if !self.pipeline2(pid, &groups) {
            return q_;
        }
        if !self.begin_pass() {
            return q_;
        }
        let (tw, th) = self.target_size2();
        let (tw, th) = (tw as f32, th as f32);
        // viewport inside the target (WebGPU wants it there)
        let (mut x0, mut y0, mut x1, mut y1) = if vw > 0.0 && vh > 0.0 { (vx, vy, vx + vw, vy + vh) } else { (0.0, 0.0, tw, th) };
        x0 = x0.max(0.0);
        y0 = y0.max(0.0);
        x1 = x1.min(tw);
        y1 = y1.min(th);
        if x1 <= x0 || y1 <= y0 {
            return q_;
        }
        let (z0, z1) = (minz.min(maxz).clamp(0.0, 1.0), minz.max(maxz).clamp(0.0, 1.0));
        let (pipeline, layouts) = {
            let p = &self.pipelines2[&pid];
            (p.pipeline, p.layouts.clone())
        };
        gpu::set_pipeline(pipeline);
        gpu::set_viewport(x0, y0, x1 - x0, y1 - y0, z0, z1);
        let (mut rx0, mut ry0, mut rx1, mut ry1) = if s_on != 0 { (sx as i64, sy as i64, sx as i64 + sw as i64, sy as i64 + sh as i64) } else { (0, 0, tw as i64, th as i64) };
        rx0 = rx0.max(x0.floor() as i64);
        ry0 = ry0.max(y0.floor() as i64);
        rx1 = rx1.min(x1.ceil() as i64).min(tw as i64);
        ry1 = ry1.min(y1.ceil() as i64).min(th as i64);
        if rx1 <= rx0 || ry1 <= ry0 {
            return q_;
        }
        gpu::set_scissor(rx0 as u32, ry0 as u32, (rx1 - rx0) as u32, (ry1 - ry0) as u32);
        gpu::set_blend_constant(blend);
        gpu::set_stencil_ref(stencil_ref);
        for (gi, g) in groups.iter().enumerate() {
            let layout = layouts[gi];
            // (the same resources as an earlier draw: its bind group; not when it binds the
            // batch's data block, whose offsets change every draw)
            let has_data = g.chunks(8).any(|e| e[1] == 0 && e[2] == DATA);
            let mut key = Vec::new();
            if !has_data {
                key.reserve(g.len() + 2);
                key.push(pid);
                key.push(gi as u32);
                key.extend_from_slice(g);
                if let Some((l, grp, used)) = self.groups2.get(&key) {
                    if *l == layout {
                        let (grp, used) = (*grp, used.clone());
                        for t in used {
                            self.use_tex(t);
                        }
                        gpu::set_bind_group(gi as u32, grp, &[]);
                        continue;
                    }
                }
            }
            let mut used = Vec::new();
            let mut entries = Vec::new();
            for e in g.chunks(8) {
                let (binding, kind, a, b, cc, dd, ee, ff) = (e[0], e[1], e[2], e[3], e[4], e[5], e[6], e[7]);
                if kind == 0 && a == DATA {
                    entries.push((binding, Res::Buffer(self.data_buf, b, cc.max(16))));
                } else if kind == 0 {
                    let want = (dd.div_ceil(16) * 16).max(16);
                    match self.buffers.get(&a).copied() {
                        Some((buf, size)) if size >= want => entries.push((binding, Res::Buffer(buf, 0, size.min(65536)))),
                        _ => {
                            let z = self.zero();
                            entries.push((binding, Res::Buffer(z, 0, want.min(65536))));
                        }
                    }
                } else if kind == 2 || kind == 3 {
                    let s = self.sampler2(a, b, cc);
                    entries.push((binding, Res::Obj(s)));
                } else {
                    let ok = self.textures.get(&a).filter(|t| {
                        let depth = is_depth(t.format);
                        depth == (kind == 6)
                            && (kind != 1 || !t.format.ends_with("int"))
                            && (kind != 4 || t.format.ends_with("uint"))
                            && (kind != 5 || t.format.ends_with("sint"))
                    });
                    let v = match ok {
                        Some(t) => {
                            let depth = is_depth(t.format);
                            self.use_tex(a);
                            used.push(a);
                            self.view2(a, b.min(5) as u8, cc, dd, ee, ff, depth)
                        }
                        None => self.dummy(b, kind),
                    };
                    entries.push((binding, Res::Obj(v)));
                }
            }
            let group = gpu::bind_group(layout, &entries);
            if !key.is_empty() {
                if self.groups2.len() > 4096 {
                    self.clear_groups2();
                }
                self.groups2.insert(key, (layout, group, used));
            }
            gpu::set_bind_group(gi as u32, group, &[]);
            if !has_data {
                // (kept in the cache)
            } else {
                gpu::drop(group);
            }
        }
        for (k, &(b, off)) in vbs.iter().enumerate() {
            let buf = if b == DATA { Some((self.data_buf, self.data_size)) } else { self.buffers.get(&b).copied() };
            match buf {
                Some((buf, size)) => gpu::set_vertex_buffer(k as u32, buf, off.min(size)),
                None => {
                    let z = self.zero();
                    gpu::set_vertex_buffer(k as u32, z, 0);
                }
            }
        }
        if indexed != 0 {
            let buf = if ib == DATA { Some((self.data_buf, self.data_size)) } else { self.buffers.get(&ib).copied() };
            let Some((buf, size)) = buf else { return q_ };
            gpu::set_index_buffer(buf, ifmt == 2, ioff.min(size));
            gpu::draw_indexed(count, instances, start, base, first);
        } else {
            gpu::draw(count, instances, start, first);
        }
        q_
    }
}

fn color_of(c: u32) -> [f64; 4] {
    [((c >> 16) & 255) as f64 / 255.0, ((c >> 8) & 255) as f64 / 255.0, (c & 255) as f64 / 255.0, (c >> 24) as f64 / 255.0]
}

fn drop_all<K>(m: &mut HashMap<K, Id>) {
    for (_, g) in m.drain() {
        gpu::drop(g);
    }
}

pub(crate) fn now() -> f64 {
    extern "C" {
        fn emscripten_get_now() -> f64;
    }
    unsafe { emscripten_get_now() }
}
