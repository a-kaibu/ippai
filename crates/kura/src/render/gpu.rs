//! WebGPU through host/vngpu.js: objects are indexes of its table (0: none). Rare objects are
//! described in JSON (numbers under "module", "layout", "view", "buffer", "texture", "sampler"
//! name objects of the table); per-draw calls take plain arguments.

pub type Id = u32;

mod ffi {
    use super::Id;
    extern "C" {
        pub fn vngpu_attach(thread: usize) -> i32;
        pub fn vngpu_start();
        pub fn vngpu_canvas_format() -> u32;
        pub fn vngpu_canvas_size(w: u32, h: u32);
        pub fn vngpu_canvas_view() -> Id;
        pub fn vngpu_has_feature(p: *const u8, n: usize) -> i32;
        pub fn vngpu_drop(i: Id);
        pub fn vngpu_destroy(i: Id);
        pub fn vngpu_texture(p: *const u8, n: usize) -> Id;
        pub fn vngpu_view(tex: Id, p: *const u8, n: usize) -> Id;
        pub fn vngpu_buffer(size: u32, usage: u32) -> Id;
        pub fn vngpu_sampler(p: *const u8, n: usize) -> Id;
        pub fn vngpu_shader(p: *const u8, n: usize) -> Id;
        pub fn vngpu_bind_group_layout(p: *const u8, n: usize) -> Id;
        pub fn vngpu_pipeline_layout(p: *const u8, n: usize) -> Id;
        pub fn vngpu_render_pipeline(p: *const u8, n: usize) -> Id;
        pub fn vngpu_compute_pipeline(p: *const u8, n: usize) -> Id;
        pub fn vngpu_dispatch(pipe: Id, group: Id, x: u32, y: u32, z: u32);
        pub fn vngpu_pipeline_group_layout(p: Id, i: u32) -> Id;
        pub fn vngpu_bind_group(layout: Id, entries: *const u32, n: u32) -> Id;
        pub fn vngpu_write_buffer(buf: Id, at: u32, p: *const u8, n: usize);
        #[allow(clippy::too_many_arguments)]
        pub fn vngpu_write_texture(tex: Id, mip: u32, x: u32, y: u32, z: u32, p: *const u8, n: usize, bpr: u32, rows: u32, w: u32, h: u32, d: u32);
        pub fn vngpu_submit();
        pub fn vngpu_on_done(token: u32);
        pub fn vngpu_read_buffer(buf: Id, size: u32, dst: *mut u8, token: u32);
        #[allow(clippy::too_many_arguments)]
        pub fn vngpu_copy_texture(src: Id, smip: u32, sx: u32, sy: u32, sz: u32, dst: Id, dmip: u32, dx: u32, dy: u32, dz: u32, w: u32, h: u32, d: u32);
        #[allow(clippy::too_many_arguments)]
        pub fn vngpu_copy_to_buffer(tex: Id, mip: u32, x: u32, y: u32, buf: Id, bpr: u32, w: u32, h: u32);
        #[allow(clippy::too_many_arguments)]
        pub fn vngpu_copy_from_buffer(buf: Id, bpr: u32, tex: Id, mip: u32, x: u32, y: u32, w: u32, h: u32);
        pub fn vngpu_begin_pass(p: *const u8, n: usize);
        pub fn vngpu_end_pass();
        pub fn vngpu_set_pipeline(p: Id);
        pub fn vngpu_set_bind_group(i: u32, g: Id, offs: *const u32, n: u32);
        pub fn vngpu_set_vertex_buffer(slot: u32, buf: Id, off: u32);
        pub fn vngpu_set_index_buffer(buf: Id, u32_: u32, off: u32);
        pub fn vngpu_set_viewport(x: f32, y: f32, w: f32, h: f32, z0: f32, z1: f32);
        pub fn vngpu_set_scissor(x: u32, y: u32, w: u32, h: u32);
        pub fn vngpu_set_blend_constant(r: f64, g: f64, b: f64, a: f64);
        pub fn vngpu_set_stencil_ref(v: u32);
        pub fn vngpu_draw(n: u32, inst: u32, first: u32, first_inst: u32);
        pub fn vngpu_draw_indexed(n: u32, inst: u32, first: u32, base: i32, first_inst: u32);
    }
}

pub mod usage {
    pub const MAP_READ: u32 = 0x1;
    pub const COPY_SRC: u32 = 0x4;
    pub const COPY_DST: u32 = 0x8;
    pub const INDEX: u32 = 0x10;
    pub const VERTEX: u32 = 0x20;
    pub const UNIFORM: u32 = 0x40;
    // textures
    pub const T_COPY_SRC: u32 = 0x1;
    pub const T_COPY_DST: u32 = 0x2;
    pub const T_BINDING: u32 = 0x4;
    pub const T_STORAGE: u32 = 0x8;
    pub const T_RENDER: u32 = 0x10;
}

pub fn attach(thread: usize) -> bool {
    unsafe { ffi::vngpu_attach(thread) != 0 }
}
pub fn start() {
    unsafe { ffi::vngpu_start() }
}
/// The canvas's format: "bgra8unorm" or "rgba8unorm".
pub fn canvas_format() -> &'static str {
    if unsafe { ffi::vngpu_canvas_format() } == 1 { "rgba8unorm" } else { "bgra8unorm" }
}
pub fn canvas_size(w: u32, h: u32) {
    unsafe { ffi::vngpu_canvas_size(w, h) }
}
/// This frame's view of the canvas (dropped by the caller after the frame).
pub fn canvas_view() -> Id {
    unsafe { ffi::vngpu_canvas_view() }
}
pub fn has_feature(name: &str) -> bool {
    unsafe { ffi::vngpu_has_feature(name.as_ptr(), name.len()) != 0 }
}
/// Forgets object `i` (WebGPU frees it once unused).
pub fn drop(i: Id) {
    unsafe { ffi::vngpu_drop(i) }
}
/// destroy() of a texture or buffer, then forgets it.
pub fn destroy(i: Id) {
    if i != 0 {
        unsafe {
            ffi::vngpu_destroy(i);
            ffi::vngpu_drop(i);
        }
    }
}
pub fn texture(json: &str) -> Id {
    unsafe { ffi::vngpu_texture(json.as_ptr(), json.len()) }
}
/// A view of `tex` ("" : the default one).
pub fn view(tex: Id, json: &str) -> Id {
    unsafe { ffi::vngpu_view(tex, json.as_ptr(), json.len()) }
}
pub fn buffer(size: u32, usage: u32) -> Id {
    unsafe { ffi::vngpu_buffer(size, usage) }
}
pub fn sampler(json: &str) -> Id {
    unsafe { ffi::vngpu_sampler(json.as_ptr(), json.len()) }
}
pub fn shader(code: &str) -> Id {
    unsafe { ffi::vngpu_shader(code.as_ptr(), code.len()) }
}
pub fn bind_group_layout(json: &str) -> Id {
    unsafe { ffi::vngpu_bind_group_layout(json.as_ptr(), json.len()) }
}
pub fn pipeline_layout(json: &str) -> Id {
    unsafe { ffi::vngpu_pipeline_layout(json.as_ptr(), json.len()) }
}
pub fn render_pipeline(json: &str) -> Id {
    let t = std::time::Instant::now();
    let id = unsafe { ffi::vngpu_render_pipeline(json.as_ptr(), json.len()) };
    PIPELINES.lock().unwrap_or_else(|e| e.into_inner()).made(t.elapsed().as_micros() as u32);
    id
}

/// Pipelines the APIs' caches found (d3d9, d3d11, Direct2D, OpenGL) and the ones created here,
/// with their creation times: whether the caches work, and what a miss costs.
#[derive(Default)]
pub struct PipelineStats {
    pub made: u64,
    pub total_us: u64,
    pub max_us: u32,
    /// the last RECENT creation times (us), for p50/p95
    recent: std::collections::VecDeque<u32>,
}

const RECENT: usize = 512;
pub static PIPELINE_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PIPELINES: std::sync::Mutex<PipelineStats> =
    std::sync::Mutex::new(PipelineStats { made: 0, total_us: 0, max_us: 0, recent: std::collections::VecDeque::new() });

/// An API's pipeline cache had the pipeline.
pub fn pipeline_hit() {
    PIPELINE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

impl PipelineStats {
    fn made(&mut self, us: u32) {
        self.made += 1;
        self.total_us += us as u64;
        self.max_us = self.max_us.max(us);
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(us);
    }
    /// The `q`th quantile (0..1) of the recent creation times (us; 0 with none).
    pub fn quantile(&self, q: f64) -> u32 {
        let mut v: Vec<u32> = self.recent.iter().copied().collect();
        if v.is_empty() {
            return 0;
        }
        v.sort_unstable();
        v[((v.len() - 1) as f64 * q).round() as usize]
    }
}

/// The pipeline counters into `out` (f64 each): cache hits, pipelines created, their creation
/// time in all, the longest, p50 and p95 of the recent ones (ms).
#[no_mangle]
pub unsafe extern "C" fn vnhost_gpu_pipelines(out: *mut f64) {
    let s = PIPELINES.lock().unwrap_or_else(|e| e.into_inner());
    let v = [
        PIPELINE_HITS.load(std::sync::atomic::Ordering::Relaxed) as f64,
        s.made as f64,
        s.total_us as f64 / 1000.0,
        s.max_us as f64 / 1000.0,
        s.quantile(0.5) as f64 / 1000.0,
        s.quantile(0.95) as f64 / 1000.0,
    ];
    std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len());
}

#[cfg(test)]
mod pipeline_stats_tests {
    use super::*;

    #[test]
    fn quantiles_of_the_recent() {
        let mut s = PipelineStats::default();
        assert_eq!(s.quantile(0.5), 0);
        for us in 1..=100 {
            s.made(us);
        }
        assert_eq!((s.quantile(0.0), s.quantile(0.5), s.quantile(0.95), s.quantile(1.0)), (1, 51, 95, 100));
        for _ in 0..RECENT {
            s.made(7);
        }
        assert_eq!((s.quantile(0.5), s.quantile(1.0), s.max_us, s.made), (7, 7, 100, 100 + RECENT as u64));
        assert_eq!(s.total_us, 5050 + 7 * RECENT as u64);
    }
}
pub fn compute_pipeline(json: &str) -> Id {
    let t = std::time::Instant::now();
    let id = unsafe { ffi::vngpu_compute_pipeline(json.as_ptr(), json.len()) };
    PIPELINES.lock().unwrap_or_else(|e| e.into_inner()).made(t.elapsed().as_micros() as u32);
    id
}
/// A compute pass of its own: `pipe` with bind group 0 `group`, x*y*z workgroups.
pub fn dispatch(pipe: Id, group: Id, x: u32, y: u32, z: u32) {
    unsafe { ffi::vngpu_dispatch(pipe, group, x, y, z) }
}
pub fn pipeline_group_layout(p: Id, i: u32) -> Id {
    unsafe { ffi::vngpu_pipeline_group_layout(p, i) }
}

/// A bind group entry.
#[derive(Clone, Copy)]
pub enum Res {
    Buffer(Id, u32, u32),
    /// a texture view or a sampler
    Obj(Id),
}

pub fn bind_group(layout: Id, entries: &[(u32, Res)]) -> Id {
    let mut w = Vec::with_capacity(entries.len() * 5);
    for &(binding, r) in entries {
        match r {
            Res::Buffer(b, off, size) => w.extend_from_slice(&[binding, 0, b, off, size]),
            Res::Obj(o) => w.extend_from_slice(&[binding, 1, o, 0, 0]),
        }
    }
    unsafe { ffi::vngpu_bind_group(layout, w.as_ptr(), entries.len() as u32) }
}

pub fn write_buffer(buf: Id, at: u32, bytes: &[u8]) {
    unsafe { ffi::vngpu_write_buffer(buf, at, bytes.as_ptr(), bytes.len()) }
}
/// `bytes` at `p` (any memory: guest memory too).
#[allow(clippy::too_many_arguments)]
pub fn write_texture(tex: Id, mip: u32, origin: [u32; 3], p: *const u8, n: usize, bpr: u32, rows: u32, size: [u32; 3]) {
    unsafe { ffi::vngpu_write_texture(tex, mip, origin[0], origin[1], origin[2], p, n, bpr, rows, size[0], size[1], size[2]) }
}
pub fn submit() {
    unsafe { ffi::vngpu_submit() }
}
/// `vnhost_render_done(token, ms)` once the GPU has done what was submitted.
pub fn on_done(token: u32) {
    unsafe { ffi::vngpu_on_done(token) }
}
/// Maps `buf`, copies its first `size` bytes to `dst`, then `vnhost_render_mapped(token, ok)`.
pub fn read_buffer(buf: Id, size: u32, dst: *mut u8, token: u32) {
    unsafe { ffi::vngpu_read_buffer(buf, size, dst, token) }
}
#[allow(clippy::too_many_arguments)]
pub fn copy_texture(src: Id, smip: u32, so: [u32; 3], dst: Id, dmip: u32, d: [u32; 3], size: [u32; 3]) {
    unsafe { ffi::vngpu_copy_texture(src, smip, so[0], so[1], so[2], dst, dmip, d[0], d[1], d[2], size[0], size[1], size[2]) }
}
#[allow(clippy::too_many_arguments)]
pub fn copy_to_buffer(tex: Id, mip: u32, x: u32, y: u32, buf: Id, bpr: u32, w: u32, h: u32) {
    unsafe { ffi::vngpu_copy_to_buffer(tex, mip, x, y, buf, bpr, w, h) }
}

/// copyBufferToTexture: rows of `bpr` bytes from the start of `buf` into a box of mip `mip`.
#[allow(clippy::too_many_arguments)]
pub fn copy_from_buffer(buf: Id, bpr: u32, tex: Id, mip: u32, x: u32, y: u32, w: u32, h: u32) {
    unsafe { ffi::vngpu_copy_from_buffer(buf, bpr, tex, mip, x, y, w, h) }
}

// the render pass (one at a time)
pub fn begin_pass(json: &str) {
    unsafe { ffi::vngpu_begin_pass(json.as_ptr(), json.len()) }
}
pub fn end_pass() {
    unsafe { ffi::vngpu_end_pass() }
}
pub fn set_pipeline(p: Id) {
    unsafe { ffi::vngpu_set_pipeline(p) }
}
pub fn set_bind_group(i: u32, g: Id, offsets: &[u32]) {
    unsafe { ffi::vngpu_set_bind_group(i, g, offsets.as_ptr(), offsets.len() as u32) }
}
pub fn set_vertex_buffer(slot: u32, buf: Id, off: u32) {
    unsafe { ffi::vngpu_set_vertex_buffer(slot, buf, off) }
}
pub fn set_index_buffer(buf: Id, u32_: bool, off: u32) {
    unsafe { ffi::vngpu_set_index_buffer(buf, u32_ as u32, off) }
}
pub fn set_viewport(x: f32, y: f32, w: f32, h: f32, z0: f32, z1: f32) {
    unsafe { ffi::vngpu_set_viewport(x, y, w, h, z0, z1) }
}
pub fn set_scissor(x: u32, y: u32, w: u32, h: u32) {
    unsafe { ffi::vngpu_set_scissor(x, y, w, h) }
}
pub fn set_blend_constant(c: [f64; 4]) {
    unsafe { ffi::vngpu_set_blend_constant(c[0], c[1], c[2], c[3]) }
}
pub fn set_stencil_ref(v: u32) {
    unsafe { ffi::vngpu_set_stencil_ref(v) }
}
pub fn draw(n: u32, inst: u32, first: u32, first_inst: u32) {
    unsafe { ffi::vngpu_draw(n, inst, first, first_inst) }
}
pub fn draw_indexed(n: u32, inst: u32, first: u32, base: i32, first_inst: u32) {
    unsafe { ffi::vngpu_draw_indexed(n, inst, first, base, first_inst) }
}

/// A JSON string literal.
pub fn q(s: &str) -> String {
    format!("\"{s}\"")
}
