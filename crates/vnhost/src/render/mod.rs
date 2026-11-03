//! What the page shows, drawn by one thread of ours: the page hands its canvas over (an
//! OffscreenCanvas, host/vngpu.js) and this thread draws into it with WebGPU every animation
//! frame: the screen (display.rs) letterboxed, a Direct3D window's frames (`d3d`, the batches
//! d3d9/gpu.rs collects) over it, and the windows above that one.
//!
//! The page only passes its canvas size and pointer events (`vnhost_render_resize`,
//! `vnhost_render_input`) and reads what tests and stats want.

pub mod d3d;
pub mod gpu;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

use gpu::{q, usage, Id, Res};

const SHADER: &str = r#"
struct Out { @builtin(position) position: vec4f, @location(0) uv: vec2f };

@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
  let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
  var out: Out;
  out.position = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
  out.uv = mix(uv.xy, uv.zw, vec2f(p.x, 1.0 - p.y));
  return out;
}

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
/** the part of the texture drawn: u0 v0 u1 v1 */
@group(0) @binding(2) var<uniform> uv: vec4f;

@fragment fn fs(in: Out) -> @location(0) vec4f {
  return vec4f(textureSample(frame, smp, in.uv).rgb, 1.0);
}
"#;

/// The canvas size in device pixels (the page's, `vnhost_render_resize`): w << 16 | h.
static CANVAS: AtomicU32 = AtomicU32::new(0);
/// Where the shown part of the screen is on the canvas, for pointer events: x, y, w, h, and
/// the view l t r b, screen w h.
static LAYOUT: Mutex<[i32; 10]> = Mutex::new([0, 0, 1, 1, 0, 0, 1, 1, 1, 1]);
static STARTED: AtomicBool = AtomicBool::new(false);
/// A test wants the picture (`vnhost_render_shot`): its serial asked for, and done.
static SHOT_WANT: AtomicU32 = AtomicU32::new(0);
static SHOT: Mutex<(u32, usize, u32, u32)> = Mutex::new((0, 0, 0, 0));
/// the screen's latest frame: serial, its pixels, row length, rows (for the page: vnvm-frame)
static FRAME: Mutex<[u32; 4]> = Mutex::new([0; 4]);
/// counters: Direct3D frames, batch time (µs), passes, bytes uploaded, and the bytes passes
/// would move between tile memory and memory on a tile-based GPU (loaded, stored)
pub(crate) static STATS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
/// time spent in frames (µs)
static FRAME_US: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static R: RefCell<Option<Render>> = const { RefCell::new(None) };
}

/// The page, once its module runs: starts the thread that draws into `Module.vnvmCanvas`.
#[no_mangle]
pub extern "C" fn vnhost_render_start() {
    if STARTED.swap(true, Relaxed) {
        return;
    }
    extern "C" fn main(_: *mut libc::c_void) -> *mut libc::c_void {
        extern "C" {
            fn emscripten_exit_with_live_runtime() -> !;
        }
        // (its time is between animation frames: not the game's CPU)
        crate::wrap::load_exclude();
        if !gpu::attach(unsafe { libc::pthread_self() } as usize) {
            eprintln!("render: no canvas");
            return std::ptr::null_mut();
        }
        gpu::start();
        // (back to the worker's event loop: the frames come from there)
        unsafe { emscripten_exit_with_live_runtime() }
    }
    let mut t: libc::pthread_t = unsafe { std::mem::zeroed() };
    unsafe { libc::pthread_create(&mut t, std::ptr::null(), main, std::ptr::null_mut()) };
}

/// The page's canvas is `w`×`h` device pixels.
#[no_mangle]
pub extern "C" fn vnhost_render_resize(w: u32, h: u32) {
    CANVAS.store(w.clamp(1, 16384) << 16 | h.clamp(1, 16383), Relaxed);
}

/// A pointer event at canvas pixel x, y (device pixels): pushed as input at the screen pixel
/// under it (kinds as `vnhost_input_push`; key events take no position).
#[no_mangle]
pub extern "C" fn vnhost_render_input(kind: u32, x: i32, y: i32, data: u32) -> i32 {
    let [rx, ry, rw, rh, l, t, r, b, w, h] = *LAYOUT.lock().unwrap_or_else(|e| e.into_inner());
    let fx = l + ((x - rx) as i64 * (r - l) as i64 / rw.max(1) as i64) as i32;
    let fy = t + ((y - ry) as i64 * (b - t) as i64 / rh.max(1) as i64) as i32;
    crate::display::vnhost_input_push(kind, fx.clamp(0, (w - 1).max(0)), fy.clamp(0, (h - 1).max(0)), data)
}

/// Where the shown part of the screen is on the canvas: `out` = x y w h (canvas pixels), the
/// view l t r b, the screen's w h.
#[no_mangle]
pub unsafe extern "C" fn vnhost_render_layout(out: *mut i32) {
    std::slice::from_raw_parts_mut(out, 10).copy_from_slice(&*LAYOUT.lock().unwrap_or_else(|e| e.into_inner()));
}

/// Tests: asks for the picture shown (at the screen's pixels) and gives the last one taken:
/// `out` = pixels (BGRA rows), width, height; returns its serial (0: none yet).
#[no_mangle]
pub unsafe extern "C" fn vnhost_render_shot(out: *mut u32) -> u32 {
    SHOT_WANT.fetch_add(1, Relaxed);
    let s = *SHOT.lock().unwrap_or_else(|e| e.into_inner());
    *out = s.1 as u32;
    *out.add(1) = s.2;
    *out.add(2) = s.3;
    s.0
}

/// The page: the screen's latest frame (serial, pixels, row length, rows) and counters
/// (Direct3D frames, batch ms, passes, MB uploaded) into `out` (8 words).
#[no_mangle]
pub unsafe extern "C" fn vnhost_render_info(out: *mut u32) {
    let f = *FRAME.lock().unwrap_or_else(|e| e.into_inner());
    let out = std::slice::from_raw_parts_mut(out, 8);
    out[..4].copy_from_slice(&f);
    out[4] = STATS[0].load(Relaxed) as u32;
    out[5] = (STATS[1].load(Relaxed) / 1000) as u32;
    out[6] = STATS[2].load(Relaxed) as u32;
    out[7] = (STATS[3].load(Relaxed) >> 20) as u32;
}

/// host/vngpu.js: every animation frame (of the drawing thread's worker).
#[no_mangle]
pub extern "C" fn vnhost_render_frame(t: f64) {
    let t0 = d3d::now();
    R.with(|r| {
        let mut r = r.borrow_mut();
        r.get_or_insert_with(Render::new).frame(t);
    });
    FRAME_US.fetch_add(((d3d::now() - t0) * 1000.0) as u64, Relaxed);
}

/// host/vngpu.js: the GPU finished the work submitted before `on_done(token)`.
#[no_mangle]
pub extern "C" fn vnhost_render_done(token: u32, ms: f64) {
    R.with(|r| {
        if let Some(r) = r.borrow_mut().as_mut() {
            r.d3d.gpu_done(token, ms);
        }
    })
}

/// host/vngpu.js: a buffer `read_buffer` asked for was copied (ok) or not.
#[no_mangle]
pub extern "C" fn vnhost_render_mapped(token: u32, ok: u32) {
    R.with(|r| {
        if let Some(r) = r.borrow_mut().as_mut() {
            if token == SHOT_TOKEN {
                r.shot_mapped(ok != 0);
            } else {
                r.d3d.mapped(token, ok != 0);
            }
        }
    })
}

const SHOT_TOKEN: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Where a frame of `width`×`height` goes on a canvas of `cw`×`ch`.
fn letterbox(cw: u32, ch: u32, width: i32, height: i32) -> Rect {
    let (width, height) = (width.max(1) as f64, height.max(1) as f64);
    let mut scale = (cw as f64 / width).min(ch as f64 / height);
    let integer = scale.floor();
    if integer >= 1.0 && integer / scale > 0.92 {
        scale = integer;
    }
    let w = (width * scale).round();
    let h = (height * scale).round();
    Rect { x: ((cw as f64 - w) / 2.0).floor() as f32, y: ((ch as f64 - h) / 2.0).floor() as f32, w: w as f32, h: h as f32 }
}

struct Pipe {
    pipeline: Id,
    layout: Id,
}

struct Render {
    d3d: d3d::D3d,
    headless: bool,
    /// per target format
    pipes: Vec<(&'static str, Pipe)>,
    nearest: Id,
    linear: Id,
    /// the screen's texture and its view, size
    tex: Id,
    tex_view: Id,
    tex_size: (i32, i32),
    /// u0 v0 u1 v1 of the screen, the Direct3D window and up to 8 windows above it
    uv: Vec<Id>,
    /// bind groups: the screen's (nearest, linear), the overlay's (and its view)
    group_main: [Id; 2],
    group_overlay: (Id, u64, Id),
    width: i32,
    height: i32,
    /// view l t r b shown
    shown: [i32; 4],
    canvas: (u32, u32),
    rect: Rect,
    /// serial of the screen frame uploaded (None: upload all)
    serial: Option<u32>,
    above: Vec<[i32; 4]>,
    d3d_frames: u64,
    stats: Option<Stats>,
    /// a shot being read: its buffer, texture, size, pixels
    shot: Option<(Id, Id, u32, u32, Vec<u8>, u32)>,
    shot_done: u32,
}

struct Stats {
    t0: f64,
    last: f64,
    frames: u32,
    slow: u32,
    at: [u64; 6],
    d3d_at: u64,
}

impl Render {
    fn new() -> Render {
        let headless = crate::env_on!("VNVM_HEADLESS");
        let uv = (0..10).map(|_| gpu::buffer(16, usage::UNIFORM | usage::COPY_DST)).collect();
        Render {
            d3d: d3d::D3d::new(),
            headless,
            pipes: Vec::new(),
            nearest: gpu::sampler(r#"{"magFilter":"nearest","minFilter":"nearest"}"#),
            linear: gpu::sampler(r#"{"magFilter":"linear","minFilter":"linear"}"#),
            tex: 0,
            tex_view: 0,
            tex_size: (0, 0),
            uv,
            group_main: [0, 0],
            group_overlay: (0, 0, 0),
            width: 1,
            height: 1,
            shown: [0, 0, 1, 1],
            canvas: (0, 0),
            rect: Rect::default(),
            serial: None,
            above: Vec::new(),
            d3d_frames: 0,
            stats: crate::env_on!("VNVM_D3DSTATS").then_some(Stats { t0: 0.0, last: 0.0, frames: 0, slow: 0, at: [0; 6], d3d_at: 0 }),
            shot: None,
            shot_done: 0,
        }
    }

    fn pipe(&mut self, format: &'static str) -> &Pipe {
        if let Some(i) = self.pipes.iter().position(|p| p.0 == format) {
            return &self.pipes[i].1;
        }
        thread_local! {
            static MODULE: Id = gpu::shader(SHADER);
        }
        let module = MODULE.with(|m| *m);
        let pipeline = gpu::render_pipeline(&format!(
            r#"{{"layout":"auto","vertex":{{"module":{module},"entryPoint":"vs"}},"fragment":{{"module":{module},"entryPoint":"fs","targets":[{{"format":{}}}]}},"primitive":{{"topology":"triangle-list"}}}}"#,
            q(format)
        ));
        let layout = gpu::pipeline_group_layout(pipeline, 0);
        self.pipes.push((format, Pipe { pipeline, layout }));
        &self.pipes.last().unwrap().1
    }

    fn frame(&mut self, t: f64) {
        self.stats(t);
        let (mut w, mut h) = (0, 0);
        unsafe { crate::display::vnhost_display_size(&mut w, &mut h) };
        if (w, h) != (self.width, self.height) {
            self.serial = None;
        }
        let mut v = [0i32; 4];
        unsafe { crate::display::vnhost_display_view(v.as_mut_ptr()) };
        let v = if v[2] > v[0] && v[3] > v[1] { v } else { [0, 0, w, h] };
        let c = CANVAS.load(Relaxed);
        let canvas = ((c >> 16).max(1), (c & 0xffff).max(1));
        let mut redraw = false;
        if v != self.shown || (w, h) != (self.width, self.height) || canvas != self.canvas {
            self.width = w;
            self.height = h;
            self.shown = v;
            self.canvas = canvas;
            gpu::canvas_size(canvas.0, canvas.1);
            self.rect = letterbox(canvas.0, canvas.1, v[2] - v[0], v[3] - v[1]);
            let r = self.rect;
            *LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = [r.x as i32, r.y as i32, r.w as i32, r.h as i32, v[0], v[1], v[2], v[3], w, h];
            let (fw, fh) = (w.max(1) as f32, h.max(1) as f32);
            write_f32(self.uv[0], &[v[0] as f32 / fw, v[1] as f32 / fh, v[2] as f32 / fw, v[3] as f32 / fh]);
            // (the sampler: nearest when the screen is scaled by a whole number)
            self.group_main = [0, 0];
            redraw = true;
        }
        self.d3d.poll();
        if self.d3d.frames != self.d3d_frames {
            self.d3d_frames = self.d3d.frames;
            redraw = true;
        }
        // windows over the Direct3D one (a dialog the game opens) are drawn from the screen on top
        let mut above = Vec::new();
        if let Some(o) = self.d3d.overlay {
            let mut buf = [0i32; 32];
            let n = unsafe { crate::display::vnhost_display_windows(buf.as_mut_ptr(), 8) } as usize;
            let rs: Vec<[i32; 4]> = (0..n).map(|k| [buf[4 * k], buf[4 * k + 1], buf[4 * k + 2], buf[4 * k + 3]]).collect();
            let [ol, ot, or, ob] = o.rect;
            if let Some(i) = rs.iter().position(|&[l, t, r, b]| l <= ol && t <= ot && r >= or && b >= ob) {
                above = rs[..i].to_vec();
            }
        }
        if above != self.above {
            self.above = above;
            redraw = true;
        }
        if self.upload_screen() {
            redraw = true;
        }
        if redraw && !self.headless && self.tex != 0 {
            let view = gpu::canvas_view();
            let (cw, ch) = self.canvas;
            let format = gpu::canvas_format();
            self.draw(view, format, self.rect, cw, ch);
            gpu::drop(view);
        }
        let want = SHOT_WANT.load(Relaxed);
        if want != self.shot_done && self.shot.is_none() && self.tex != 0 {
            self.shot_done = want;
            self.take_shot();
        }
    }

    /// Uploads what changed of the screen; whether anything did.
    fn upload_screen(&mut self) -> bool {
        let (width, height) = (self.width, self.height);
        if width <= 0 || height <= 0 {
            return false;
        }
        let mut io = [0u32; 10];
        io[0] = self.serial.unwrap_or(u32::MAX);
        // (the changed rows straight from the game window's surface when only it changed)
        io[3] = self.serial.is_some() as u32;
        let bits = unsafe { crate::display::vnhost_display_frame(io.as_mut_ptr()) };
        if Some(io[0]) == self.serial {
            return false;
        }
        let new_tex = self.tex == 0 || self.tex_size != (width, height);
        if new_tex {
            gpu::destroy(self.tex);
            gpu::drop(self.tex_view);
            self.tex = gpu::texture(&format!(r#"{{"size":[{width},{height}],"format":"bgra8unorm","usage":{}}}"#, usage::T_BINDING | usage::T_COPY_DST));
            self.tex_view = gpu::view(self.tex, "");
            self.tex_size = (width, height);
            self.group_main = [0, 0];
            if io[3] == 1 {
                // (the texture is new: everything, from the screen)
                self.serial = None;
                return self.upload_screen();
            }
        }
        let (mut y0, mut y1) = (io[1] as i32, io[2] as i32);
        if new_tex {
            (y0, y1) = (0, height);
        }
        let mut frame = [io[0], bits as u32, width as u32, height as u32];
        if io[3] == 1 {
            let (ptr, stride) = (io[4] as usize, io[5] as i32);
            let [l, t, r, b] = [io[6] as i32, io[7] as i32, io[8] as i32, io[9] as i32];
            let (x0, x1) = (l.max(0), r.min(width));
            let (t2, b2) = (y0.max(t).max(0), y1.min(b).min(height));
            if x1 > x0 && b2 > t2 {
                let off = ptr + (((t2 - t) * stride + (x0 - l)) * 4) as usize;
                let bpr = (stride * 4) as u32;
                let n = ((b2 - t2 - 1) * stride * 4 + (x1 - x0) * 4) as usize;
                gpu::write_texture(self.tex, 0, [x0 as u32, t2 as u32, 0], off as *const u8, n, bpr, (b2 - t2) as u32, [(x1 - x0) as u32, (b2 - t2) as u32, 1]);
            }
            frame = [io[0], ptr as u32, stride as u32, (b - t) as u32];
        } else if y1 > y0 {
            let stride = width * 4;
            let p = bits as usize + (y0 * stride) as usize;
            gpu::write_texture(self.tex, 0, [0, y0 as u32, 0], p as *const u8, ((y1 - y0) * stride) as usize, stride as u32, (y1 - y0) as u32, [width as u32, (y1 - y0) as u32, 1]);
        }
        self.serial = Some(io[0]);
        *FRAME.lock().unwrap_or_else(|e| e.into_inner()) = frame;
        true
    }

    /// Draws the shown part of the screen, the Direct3D window and the windows above it into
    /// `target` (`format`, `tw`×`th`) at `r`.
    fn draw(&mut self, target: Id, format: &'static str, r: Rect, tw: u32, th: u32) {
        let pipe = self.pipe(format);
        let (pipeline, layout) = (pipe.pipeline, pipe.layout);
        let [vl, vt, vr, vb] = self.shown;
        let nearest = (r.w as i32) % (vr - vl).max(1) == 0 && (r.h as i32) % (vb - vt).max(1) == 0;
        let gi = nearest as usize;
        if self.group_main[gi] == 0 {
            let smp = if nearest { self.nearest } else { self.linear };
            self.group_main[gi] = gpu::bind_group(layout, &[(0, Res::Obj(self.tex_view)), (1, Res::Obj(smp)), (2, Res::Buffer(self.uv[0], 0, 16))]);
        }
        gpu::begin_pass(&format!(r#"{{"colorAttachments":[{{"view":{target},"loadOp":"clear","storeOp":"store","clearValue":[0,0,0,1]}}]}}"#));
        gpu::set_viewport(r.x, r.y, r.w, r.h, 0.0, 1.0);
        gpu::set_pipeline(pipeline);
        gpu::set_bind_group(0, self.group_main[gi], &[]);
        gpu::draw(3, 1, 0, 0);
        if let Some(o) = self.d3d.overlay {
            if (self.group_overlay.0, self.group_overlay.1) != (o.view, self.d3d.tex_gen) {
                gpu::drop(self.group_overlay.2);
                self.group_overlay = (o.view, self.d3d.tex_gen, gpu::bind_group(layout, &[(0, Res::Obj(o.view)), (1, Res::Obj(self.linear)), (2, Res::Buffer(self.uv[1], 0, 16))]));
            }
            let [ol, ot, or, ob] = o.rect;
            let (l, t, rr, b) = (ol.max(vl), ot.max(vt), or.min(vr), ob.min(vb));
            let sx = r.w / (vr - vl) as f32;
            let sy = r.h / (vb - vt) as f32;
            let x0 = r.x + (l - vl) as f32 * sx;
            let y0 = r.y + (t - vt) as f32 * sy;
            let x1 = (tw as f32).min(r.x + (rr - vl) as f32 * sx);
            let y1 = (th as f32).min(r.y + (b - vt) as f32 * sy);
            if x1 > x0 && y1 > y0 && or > ol && ob > ot {
                let (ow, oh) = ((or - ol) as f32, (ob - ot) as f32);
                let mut uv = [(l - ol) as f32 / ow, (t - ot) as f32 / oh, (rr - ol) as f32 / ow, (b - ot) as f32 / oh];
                // (OpenGL frames are stored bottom row first)
                if o.flip {
                    uv[1] = 1.0 - uv[1];
                    uv[3] = 1.0 - uv[3];
                }
                write_f32(self.uv[1], &uv);
                gpu::set_viewport(x0, y0, x1 - x0, y1 - y0, 0.0, 1.0);
                gpu::set_bind_group(0, self.group_overlay.2, &[]);
                gpu::draw(3, 1, 0, 0);
            }
            // dialogs and other windows above it
            let (fw, fh) = (self.width as f32, self.height as f32);
            for (k, &[al, at, ar, ab]) in self.above.iter().take(8).enumerate() {
                let (l, t, rr, b) = (al.max(vl), at.max(vt), ar.min(vr), ab.min(vb));
                if rr <= l || b <= t {
                    continue;
                }
                let x0 = r.x + (l - vl) as f32 * sx;
                let y0 = r.y + (t - vt) as f32 * sy;
                let x1 = (tw as f32).min(r.x + (rr - vl) as f32 * sx);
                let y1 = (th as f32).min(r.y + (b - vt) as f32 * sy);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let buf = self.uv[2 + k];
                write_f32(buf, &[l as f32 / fw, t as f32 / fh, rr as f32 / fw, b as f32 / fh]);
                let g = gpu::bind_group(layout, &[(0, Res::Obj(self.tex_view)), (1, Res::Obj(self.nearest)), (2, Res::Buffer(buf, 0, 16))]);
                gpu::set_viewport(x0, y0, x1 - x0, y1 - y0, 0.0, 1.0);
                gpu::set_bind_group(0, g, &[]);
                gpu::draw(3, 1, 0, 0);
                gpu::drop(g);
            }
        }
        gpu::end_pass();
        gpu::submit();
    }

    /// Draws what is shown at the screen's pixels into a texture and reads it.
    fn take_shot(&mut self) {
        let [vl, vt, vr, vb] = self.shown;
        let (l, t) = (vl.max(0), vt.max(0));
        let (r, b) = (vr.min(self.width), vb.min(self.height));
        if r <= l || b <= t {
            return;
        }
        let (w, h) = ((vr - vl) as u32, (vb - vt) as u32);
        let tex = gpu::texture(&format!(r#"{{"size":[{w},{h}],"format":"bgra8unorm","usage":{}}}"#, usage::T_RENDER | usage::T_COPY_SRC));
        let view = gpu::view(tex, "");
        self.draw(view, "bgra8unorm", Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 }, w, h);
        gpu::drop(view);
        let bpr = (w * 4).div_ceil(256) * 256;
        let buf = gpu::buffer(bpr * h, usage::COPY_DST | usage::MAP_READ);
        gpu::copy_to_buffer(tex, 0, 0, 0, buf, bpr, w, h);
        gpu::submit();
        let mut px = vec![0u8; (bpr * h) as usize];
        gpu::read_buffer(buf, bpr * h, px.as_mut_ptr(), SHOT_TOKEN);
        self.shot = Some((buf, tex, w, h, px, bpr));
    }

    fn shot_mapped(&mut self, ok: bool) {
        let Some((buf, tex, w, h, px, bpr)) = self.shot.take() else { return };
        gpu::destroy(buf);
        gpu::destroy(tex);
        if !ok {
            return;
        }
        let mut out = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            out[y * w as usize * 4..(y + 1) * w as usize * 4].copy_from_slice(&px[y * bpr as usize..y * bpr as usize + w as usize * 4]);
        }
        let mut s = SHOT.lock().unwrap_or_else(|e| e.into_inner());
        if s.1 != 0 {
            unsafe { drop(Vec::from_raw_parts(s.1 as *mut u8, (s.2 * s.3 * 4) as usize, (s.2 * s.3 * 4) as usize)) };
        }
        let mut out = std::mem::ManuallyDrop::new(out.into_boxed_slice());
        *s = (s.0 + 1, out.as_mut_ptr() as usize, w, h);
    }

    /// &d3dstats: every 5 s, frames, slow ones (>34 ms apart), Direct3D frames and batch time.
    fn stats(&mut self, t: f64) {
        let Some(s) = &mut self.stats else { return };
        if s.t0 == 0.0 {
            s.t0 = t;
        }
        if s.last != 0.0 && t - s.last > 34.0 {
            s.slow += 1;
        }
        s.last = t;
        s.frames += 1;
        if t - s.t0 >= 5000.0 {
            let secs = (t - s.t0) / 1000.0;
            let now: [u64; 6] = std::array::from_fn(|i| STATS[i].load(Relaxed));
            let d = |i: usize| now[i] - s.at[i];
            let g = self.d3d.gpu_ms.take();
            eprintln!(
                "page stats/{secs:.1}s: rAF {:.1}/s, slow {}, render {}ms, d3d frames {:.1}/s, batches {}ms, gpu {:.0}ms (max {:.0}), passes {}, submits {}, textures {}, upload {:.1}MB, tile load {}MB store {}MB, same frames skipped {}",
                s.frames as f64 / secs,
                s.slow,
                FRAME_US.swap(0, Relaxed) / 1000,
                (self.d3d.frames - s.d3d_at) as f64 / secs,
                d(1) / 1000,
                g.0,
                g.1,
                d(2),
                self.d3d.submits.take(),
                self.d3d.created.take(),
                d(3) as f64 / 1e6,
                d(4) >> 20,
                d(5) >> 20,
                self.d3d.skipped.take()
            );
            *s = Stats { t0: t, last: t, frames: 0, slow: 0, at: now, d3d_at: self.d3d.frames };
        }
    }
}

fn write_f32(buf: Id, v: &[f32]) {
    let b: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
    gpu::write_buffer(buf, 0, &b);
}
