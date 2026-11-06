//! OpenGL for sake's opengl32 (crates/sake/src/gl.rs): the GL functions it calls, drawn with the page's WebGPU commands (the ones Direct3D
//! 11 uses, crates/kura/src/d3d9/gpu.rs).
//!
//! What there is: GL 1.x fixed function without lighting or fog (immediate mode, client arrays,
//! matrices, two texture units with MODULATE/REPLACE/DECAL/ADD/BLEND, alpha test, blending, depth
//! and stencil), buffer objects, framebuffer objects with texture and renderbuffer attachments,
//! glReadPixels, glCopyTex(Sub)Image2D. Every draw is turned into a non-indexed list of float
//! vertices in the batch's data block (slow, simple; optimizing is for later).
//!
//! Frames are stored bottom row first, as OpenGL has them (vertex shaders negate y): viewports,
//! scissors, glReadPixels and texture copies use GL coordinates unchanged; the page flips the
//! window's frame when it shows it (PRESENT_FLIP).

#![allow(non_snake_case, clippy::missing_safety_doc, clippy::too_many_arguments)]

use crate::d3d9::gpu;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;


const DATA: u32 = 0xffff_ffff;
const P_RGBA8: u32 = 4;
const P_D24S8: u32 = 1;

// enums
const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x0500;
const GL_INVALID_VALUE: u32 = 0x0501;
const GL_INVALID_OPERATION: u32 = 0x0502;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_BLEND: u32 = 0x0BE2;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_ALPHA_TEST: u32 = 0x0BC0;
const GL_CULL_FACE: u32 = 0x0B44;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_STENCIL_TEST: u32 = 0x0B90;
const GL_VERTEX_ARRAY: u32 = 0x8074;
const GL_COLOR_ARRAY: u32 = 0x8076;
const GL_TEXTURE_COORD_ARRAY: u32 = 0x8078;
const GL_MODELVIEW: u32 = 0x1700;
const GL_PROJECTION: u32 = 0x1701;
const GL_TEXTURE: u32 = 0x1702;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_FRONT: u32 = 0x0404;
const GL_BACK: u32 = 0x0405;
const GL_CW: u32 = 0x0900;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const GL_PIXEL_PACK_BUFFER: u32 = 0x88EB;
const GL_PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_DEPTH_ATTACHMENT: u32 = 0x8D00;
const GL_STENCIL_ATTACHMENT: u32 = 0x8D20;
const GL_DEPTH_STENCIL_ATTACHMENT: u32 = 0x821A;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_UNSIGNED_SHORT: u32 = 0x1403;
const GL_FLOAT: u32 = 0x1406;

// ---- WGSL of the fixed function ----

const FF_VS: &str = r#"
struct VU { mvp: mat4x4f, tm0: mat4x4f, tm1: mat4x4f, misc: vec4f };
@group(0) @binding(0) var<uniform> vu: VU;
struct In { @location(0) pos: vec4f, @location(1) col: vec4f, @location(2) t0: vec4f, @location(3) t1: vec4f };
struct Out { @builtin(position) p: vec4f, @location(0) col: vec4f, @location(1) t0: vec4f, @location(2) t1: vec4f };
@vertex fn main(i: In) -> Out {
  var o: Out;
  var p = vu.mvp * i.pos;
  p.y = -p.y;
  p.z = (p.z + p.w) * 0.5;
  o.p = p;
  o.col = i.col;
  o.t0 = vu.tm0 * i.t0;
  o.t1 = vu.tm1 * i.t1;
  return o;
}
"#;

const FF_PS: &str = r#"
diagnostic(off, derivative_uniformity);
struct FU { env: vec4u, aref: vec4f, ec0: vec4f, ec1: vec4f, base: vec4u };
@group(1) @binding(0) var<uniform> fu: FU;
@group(1) @binding(16) var t0: texture_2d<f32>;
@group(1) @binding(17) var t1: texture_2d<f32>;
@group(1) @binding(160) var s0: sampler;
@group(1) @binding(161) var s1: sampler;
struct In { @builtin(position) p: vec4f, @location(0) col: vec4f, @location(1) t0: vec4f, @location(2) t1: vec4f };
// mode: 0 off 1 modulate 2 replace 3 decal 4 add 5 blend; base: 0 rgba 1 rgb 2 alpha 3 luminance 4 luminance alpha
fn env(mode: u32, base: u32, c: vec4f, t: vec4f, ec: vec4f) -> vec4f {
  let rgb = base != 2u;
  let a = base == 0u || base == 2u || base == 4u;
  switch mode {
    case 1u: { return vec4f(select(c.rgb, c.rgb * t.rgb, rgb), select(c.a, c.a * t.a, a)); }
    case 2u: { return vec4f(select(c.rgb, t.rgb, rgb), select(c.a, t.a, a)); }
    case 3u: { return vec4f(mix(c.rgb, t.rgb, select(1.0, t.a, a)), c.a); }
    case 4u: { return vec4f(select(c.rgb, c.rgb + t.rgb, rgb), select(c.a, c.a * t.a, a)); }
    case 5u: { return vec4f(select(c.rgb, mix(c.rgb, ec.rgb, t.rgb), rgb), select(c.a, c.a * t.a, a)); }
    default: { return c; }
  }
}
@fragment fn main(i: In) -> @location(0) vec4f {
  let a = textureSample(t0, s0, i.t0.xy / i.t0.w);
  let b = textureSample(t1, s1, i.t1.xy / i.t1.w);
  var c = clamp(env(fu.env.x, fu.base.x, i.col, a, fu.ec0), vec4f(0.0), vec4f(1.0));
  c = clamp(env(fu.env.y, fu.base.y, c, b, fu.ec1), vec4f(0.0), vec4f(1.0));
  // alpha test: GL func - GL_NEVER (0 never ... 7 always)
  let f = fu.env.z;
  let r = fu.aref.x;
  if (f == 0u) || (f == 1u && !(c.a < r)) || (f == 2u && c.a != r) || (f == 3u && !(c.a <= r)) ||
     (f == 4u && !(c.a > r)) || (f == 5u && c.a == r) || (f == 6u && !(c.a >= r)) { discard; }
  return c;
}
"#;

// ---- objects ----

#[derive(Default)]
pub(crate) struct Texture {
    gpu: u32,
    w: u32,
    h: u32,
    levels: u32,
    /// 0 rgba 1 rgb 2 alpha 3 luminance 4 luminance alpha, 5 depth
    base: u32,
    defined: u32,
    min: u32,
    mag: u32,
    wrap_s: u32,
    wrap_t: u32,
    gen_mips: bool,
}

#[derive(Default)]
pub(crate) struct Buffer {
    data: Vec<u8>,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) enum Att {
    #[default]
    None,
    Tex(u32, u32),
    Rb(u32),
}

#[derive(Default)]
pub(crate) struct Fbo {
    color: Att,
    depth: Att,
}

#[derive(Default)]
pub(crate) struct Renderbuffer {
    gpu: u32,
    w: u32,
    h: u32,
    depth: bool,
}

pub(crate) struct Drawable {
    color: u32,
    depth: u32,
    w: u32,
    h: u32,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Array {
    on: bool,
    size: i32,
    ty: u32,
    stride: i32,
    ptr: u32,
    buffer: u32,
    normalized: bool,
}

pub(crate) struct Unit {
    tex2d: bool,
    bound: u32,
    env: u32,
    env_color: [f32; 4],
    matrices: Vec<[f32; 16]>,
    coords: [f32; 4],
    array: Array,
}

impl Default for Unit {
    fn default() -> Self {
        Unit { tex2d: false, bound: 0, env: 0x2100, env_color: [0.0; 4], matrices: vec![IDENTITY], coords: [0.0, 0.0, 0.0, 1.0], array: Array::default() }
    }
}

pub(crate) struct Ctx {
    error: u32,
    draw: u32,
    w: u32,
    h: u32,
    fbo_draw: u32,
    fbo_read: u32,
    rb: u32,
    caps: HashSet<u32>,
    viewport: [i32; 4],
    scissor: [i32; 4],
    /// glShadeModel(GL_FLAT)
    flat: bool,
    depth_range: [f32; 2],
    clear_color: [f32; 4],
    clear_depth: f32,
    clear_stencil: i32,
    color_mask: u32,
    depth_mask: bool,
    depth_func: u32,
    blend: [u32; 6],
    blend_color: [f32; 4],
    alpha_func: u32,
    alpha_ref: f32,
    cull: u32,
    front: u32,
    stencil: [u32; 3],
    stencil_ops: [u32; 3],
    stencil_mask: u32,
    matrix_mode: u32,
    modelview: Vec<[f32; 16]>,
    projection: Vec<[f32; 16]>,
    active: usize,
    client_active: usize,
    units: [Unit; 8],
    color: [f32; 4],
    vertex: Array,
    colors: Array,
    array_buffer: u32,
    element_buffer: u32,
    unpack: [i32; 4],
    pack: [i32; 4],
    begin: Option<u32>,
    verts: Vec<[f32; 16]>,
    // GLSL
    pub(crate) program: u32,
    pub(crate) attribs: [Array; 16],
    pub(crate) attrib_values: [[f32; 4]; 16],
}

const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

impl Ctx {
    fn new() -> Ctx {
        Ctx {
            error: 0,
            draw: 0,
            w: 0,
            h: 0,
            fbo_draw: 0,
            fbo_read: 0,
            rb: 0,
            caps: HashSet::new(),
            viewport: [0; 4],
            scissor: [0; 4],
            flat: false,
            depth_range: [0.0, 1.0],
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            color_mask: 0xf,
            depth_mask: true,
            depth_func: 0x201,
            // src, dst, srcA, dstA, op, opA
            blend: [1, 0, 1, 0, 0x8006, 0x8006],
            blend_color: [0.0; 4],
            alpha_func: 0x207,
            alpha_ref: 0.0,
            cull: GL_BACK,
            front: 0x0901,
            stencil: [0x207, 0, 0xff],
            stencil_ops: [0x1E00; 3],
            stencil_mask: 0xff,
            matrix_mode: GL_MODELVIEW,
            modelview: vec![IDENTITY],
            projection: vec![IDENTITY],
            active: 0,
            client_active: 0,
            units: Default::default(),
            color: [1.0; 4],
            vertex: Array::default(),
            colors: Array::default(),
            array_buffer: 0,
            element_buffer: 0,
            unpack: [4, 0, 0, 0],
            pack: [4, 0, 0, 0],
            begin: None,
            verts: Vec::new(),
            program: 0,
            attribs: [Array::default(); 16],
            attrib_values: [[0.0, 0.0, 0.0, 1.0]; 16],
        }
    }

    fn err(&mut self, e: u32) {
        if self.error == 0 {
            self.error = e;
        }
    }

    fn stack(&mut self) -> &mut Vec<[f32; 16]> {
        match self.matrix_mode {
            GL_PROJECTION => &mut self.projection,
            GL_TEXTURE => &mut self.units[self.active].matrices,
            _ => &mut self.modelview,
        }
    }

    fn top(&mut self) -> &mut [f32; 16] {
        self.stack().last_mut().unwrap()
    }
}

pub(crate) struct Gl {
    ctxs: HashMap<u32, Ctx>,
    next_ctx: u32,
    textures: HashMap<u32, Texture>,
    buffers: HashMap<u32, Buffer>,
    fbos: HashMap<u32, Fbo>,
    rbs: HashMap<u32, Renderbuffer>,
    next_name: u32,
    drawables: HashMap<u32, Drawable>,
    ff: Option<(u32, u32)>,
    pipelines: HashMap<Vec<u32>, u32>,
    pub(crate) shaders: HashMap<u32, glsl::Shader>,
    pub(crate) programs: HashMap<u32, glsl::Program>,
}

static GL: Mutex<Option<Gl>> = Mutex::new(None);

thread_local! {
    static CURRENT: Cell<u32> = const { Cell::new(0) };
}

fn page_id() -> u32 {
    crate::d3d11::page_id()
}

fn lock() -> std::sync::MutexGuard<'static, Option<Gl>> {
    let mut g = GL.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(Gl {
            ctxs: HashMap::new(),
            next_ctx: 1,
            textures: HashMap::new(),
            buffers: HashMap::new(),
            fbos: HashMap::new(),
            rbs: HashMap::new(),
            next_name: 1,
            drawables: HashMap::new(),
            ff: None,
            pipelines: HashMap::new(),
            shaders: HashMap::new(),
            programs: HashMap::new(),
        });
    }
    g
}

/// Runs `f` with the current context (nothing without one).
fn with<R: Default>(f: impl FnOnce(&mut Gl, &mut Ctx) -> R) -> R {
    let id = CURRENT.with(|c| c.get());
    if id == 0 {
        return R::default();
    }
    let mut g = lock();
    let g = g.as_mut().unwrap();
    let Some(mut c) = g.ctxs.remove(&id) else { return R::default() };
    let r = f(g, &mut c);
    g.ctxs.insert(id, c);
    r
}

fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut r = [0f32; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

fn bytes<T>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

// ---- the WGL entries (contexts, current context, swap) ----

#[no_mangle]
pub extern "C" fn vngl_context_create(_share: u32) -> u32 {
    let mut g = lock();
    let g = g.as_mut().unwrap();
    let id = g.next_ctx;
    g.next_ctx += 1;
    g.ctxs.insert(id, Ctx::new());
    id
}

#[no_mangle]
pub extern "C" fn vngl_context_destroy(id: u32) {
    let mut g = lock();
    g.as_mut().unwrap().ctxs.remove(&id);
    CURRENT.with(|c| {
        if c.get() == id {
            c.set(0)
        }
    });
}

/// Makes `ctx` current on this thread, drawing to window `hwnd` (`w` x `h`); 0: none.
#[no_mangle]
pub extern "C" fn vngl_make_current(ctx: u32, hwnd: u32, w: u32, h: u32) -> i32 {
    CURRENT.with(|c| c.set(ctx));
    if ctx == 0 {
        return 1;
    }
    let mut g = lock();
    let g = g.as_mut().unwrap();
    if hwnd != 0 {
        drawable(g, hwnd, w.max(1), h.max(1));
    }
    let Some(c) = g.ctxs.get_mut(&ctx) else { return 0 };
    if c.draw == 0 && hwnd != 0 {
        c.viewport = [0, 0, w as i32, h as i32];
        c.scissor = [0, 0, w as i32, h as i32];
    }
    c.draw = hwnd;
    c.w = w;
    c.h = h;
    1
}

fn drawable(g: &mut Gl, hwnd: u32, w: u32, h: u32) -> &Drawable {
    let stale = g.drawables.get(&hwnd).is_some_and(|d| d.w != w || d.h != h);
    if stale {
        let d = g.drawables.remove(&hwnd).unwrap();
        gpu::record(|q| {
            q.cmd(&[gpu::DESTROY_TEXTURE, d.color]);
            q.cmd(&[gpu::DESTROY_TEXTURE, d.depth]);
        });
    }
    g.drawables.entry(hwnd).or_insert_with(|| {
        let (color, depth) = (page_id(), page_id());
        gpu::record(|q| {
            q.cmd(&[gpu::CREATE_TEXTURE2, color, w, h, 1, 1, P_RGBA8, 0]);
            q.cmd(&[gpu::CREATE_TEXTURE2, depth, w, h, 1, 1, P_D24S8, 0]);
            q.cmd(&[gpu::CLEAR_VIEW, color, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
            q.cmd(&[gpu::CLEAR_VIEW, depth, 0, 0, 6, 0, 0, 0, 0, 1f32.to_bits(), 0]);
        });
        Drawable { color, depth, w, h }
    })
}

/// SwapBuffers of window `hwnd`; `rect`: its client area on the screen.
#[no_mangle]
pub unsafe extern "C" fn vngl_swap(hwnd: u32, rect: *const i32) {
    let tex = {
        let g = lock();
        match g.as_ref().unwrap().drawables.get(&hwnd) {
            Some(d) => (d.color, d.w, d.h),
            None => return,
        }
    };
    let r = std::slice::from_raw_parts(rect, 4);
    gpu::record(|q| q.cmd(&[gpu::PRESENT_FLIP, tex.0, r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32, tex.1, tex.2, hwnd]));
    gpu::present();
}

/// A GL function by name (null: not ours; the driver has a do-nothing one).
#[no_mangle]
pub unsafe extern "C" fn vngl_proc(name: *const c_char) -> *const c_void {
    let Ok(n) = CStr::from_ptr(name).to_str() else { return std::ptr::null() };
    // (ARB/EXT names of the core functions)
    let base = n.strip_suffix("ARB").or_else(|| n.strip_suffix("EXT")).unwrap_or(n);
    proc_of(n).or_else(|| proc_of(base)).or_else(|| glsl::proc_of(n)).or_else(|| glsl::proc_of(base)).unwrap_or(std::ptr::null())
}

macro_rules! procs {
    ($n:expr, $($f:ident),* $(,)?) => {
        match $n { $(stringify!($f) => Some($f as *const c_void),)* _ => None }
    };
}

fn proc_of(n: &str) -> Option<*const c_void> {
    procs!(n,
        glGetError, glGetString, glGetStringi, glGetIntegerv, glGetFloatv, glGetBooleanv, glGetDoublev,
        glEnable, glDisable, glIsEnabled, glEnableClientState, glDisableClientState, glViewport, glScissor,
        glDepthRange, glClearColor, glClearDepth, glClearDepthf, glClearStencil, glClear, glColorMask, glDepthMask,
        glDepthFunc, glBlendFunc, glBlendFuncSeparate, glBlendEquation, glBlendEquationSeparate, glBlendColor,
        glAlphaFunc, glCullFace, glFrontFace, glStencilFunc, glStencilOp, glStencilMask, glMatrixMode,
        glLoadIdentity, glLoadMatrixf, glLoadMatrixd, glMultMatrixf, glMultMatrixd, glPushMatrix, glPopMatrix,
        glOrtho, glFrustum, glTranslatef, glTranslated, glRotatef, glRotated, glScalef, glScaled, glBegin, glEnd,
        glVertex2f, glVertex2i, glVertex2d, glVertex2s, glVertex3f, glVertex3d, glVertex3i, glVertex4f, glVertex2fv,
        glVertex3fv, glVertex2iv, glColor3f, glColor4f, glColor3d, glColor4d, glColor3ub, glColor4ub, glColor3fv,
        glColor4fv, glColor3ubv, glColor4ubv, glTexCoord2f, glTexCoord2d, glTexCoord2i, glTexCoord2fv,
        glMultiTexCoord2f, glMultiTexCoord2fv, glNormal3f, glVertexPointer, glColorPointer, glTexCoordPointer,
        glNormalPointer, glClientActiveTexture, glActiveTexture, glDrawArrays, glDrawElements, glDrawRangeElements,
        glGenTextures, glDeleteTextures, glBindTexture, glIsTexture, glTexImage2D, glTexSubImage2D,
        glTexParameteri, glTexParameterf, glTexParameteriv, glTexParameterfv, glTexEnvi, glTexEnvf, glTexEnviv,
        glTexEnvfv, glPixelStorei, glCopyTexSubImage2D, glCopyTexImage2D, glGenerateMipmap, glReadPixels,
        glReadBuffer, glDrawBuffer, glFlush, glFinish, glHint, glShadeModel, glPolygonMode, glLineWidth,
        glPointSize, glPolygonOffset, glGenBuffers, glDeleteBuffers, glBindBuffer, glIsBuffer, glBufferData,
        glBufferSubData, glMapBuffer, glUnmapBuffer, glGenFramebuffers, glDeleteFramebuffers, glBindFramebuffer,
        glIsFramebuffer, glFramebufferTexture2D, glFramebufferTexture, glCheckFramebufferStatus, glGenRenderbuffers,
        glDeleteRenderbuffers, glBindRenderbuffer, glRenderbufferStorage, glRenderbufferStorageMultisample,
        glFramebufferRenderbuffer, glGetFramebufferAttachmentParameteriv, glGetTexLevelParameteriv,
        glGetTexParameteriv,
    )
}

// ---- state ----

extern "C" fn glGetError() -> u32 {
    with(|_, c| std::mem::replace(&mut c.error, GL_NO_ERROR))
}

const EXTENSIONS: &str = "GL_ARB_multitexture GL_EXT_bgra GL_ARB_texture_non_power_of_two GL_EXT_framebuffer_object \
GL_ARB_framebuffer_object GL_ARB_vertex_buffer_object GL_EXT_blend_func_separate GL_EXT_blend_minmax \
GL_EXT_blend_subtract GL_EXT_blend_color GL_ARB_texture_env_add GL_EXT_texture_env_add GL_EXT_texture_edge_clamp \
GL_SGIS_texture_edge_clamp GL_ARB_texture_mirrored_repeat GL_SGIS_generate_mipmap GL_EXT_packed_pixels \
GL_ARB_shader_objects GL_ARB_vertex_program GL_ARB_vertex_shader GL_ARB_fragment_shader GL_ARB_shading_language_100";

extern "C" fn glGetString(name: u32) -> *const u8 {
    let s: &'static [u8] = match name {
        0x1F00 => b"vnvm\0",
        0x1F01 => b"WebGPU\0",
        0x1F02 => b"2.1 vnvm\0",
        0x8B8C => b"1.20\0",
        0x1F03 => concat!("GL_ARB_multitexture GL_EXT_bgra GL_ARB_texture_non_power_of_two GL_EXT_framebuffer_object ",
            "GL_ARB_framebuffer_object GL_ARB_vertex_buffer_object GL_EXT_blend_func_separate GL_EXT_blend_minmax ",
            "GL_EXT_blend_subtract GL_EXT_blend_color GL_ARB_texture_env_add GL_EXT_texture_env_add GL_EXT_texture_edge_clamp ",
            "GL_SGIS_texture_edge_clamp GL_ARB_texture_mirrored_repeat GL_SGIS_generate_mipmap GL_EXT_packed_pixels ",
            "GL_ARB_shader_objects GL_ARB_vertex_program GL_ARB_vertex_shader GL_ARB_fragment_shader GL_ARB_shading_language_100\0").as_bytes(),
        _ => return std::ptr::null(),
    };
    s.as_ptr()
}

extern "C" fn glGetStringi(name: u32, i: u32) -> *const u8 {
    static NAMES: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
    if name != 0x1F03 {
        return std::ptr::null();
    }
    let mut v = NAMES.lock().unwrap();
    if v.is_empty() {
        *v = EXTENSIONS.split(' ').map(|s| format!("{s}\0").into_bytes()).collect();
    }
    v.get(i as usize).map_or(std::ptr::null(), |s| s.as_ptr())
}

/// glGet values as f64 (None: unknown)
fn get(g: &Gl, c: &Ctx, pname: u32) -> Option<Vec<f64>> {
    let f = |v: &[f32]| v.iter().map(|&x| x as f64).collect::<Vec<_>>();
    let i = |v: &[i32]| v.iter().map(|&x| x as f64).collect::<Vec<_>>();
    Some(match pname {
        0x0BA2 => i(&c.viewport),
        0x0C10 => i(&c.scissor),
        0x0D33 => vec![8192.0],
        0x84E2 => vec![2.0],
        0x8872 | 0x8B4D | 0x8B4C => vec![8.0],
        0x8869 => vec![16.0],
        0x0BA6 => f(c.modelview.last().unwrap()),
        0x0BA7 => f(c.projection.last().unwrap()),
        0x0BA8 => f(c.units[c.active].matrices.last().unwrap()),
        0x8069 => vec![c.units[c.active].bound as f64],
        0x0BA0 => vec![c.matrix_mode as f64],
        0x0CF5 => vec![c.unpack[0] as f64],
        0x0D05 => vec![c.pack[0] as f64],
        0x0D3A => vec![8192.0, 8192.0],
        0x0D56 => vec![24.0],
        0x0D57 => vec![8.0],
        0x0D52..=0x0D55 => vec![8.0],
        0x821D => vec![EXTENSIONS.split(' ').count() as f64],
        0x821B => vec![2.0],
        0x821C => vec![1.0],
        0x8CA6 => vec![c.fbo_draw as f64],
        0x8CAA => vec![c.fbo_read as f64],
        0x8CA7 => vec![c.rb as f64],
        0x8894 => vec![c.array_buffer as f64],
        0x8895 => vec![c.element_buffer as f64],
        0x0B00 => f(&c.color),
        0x84E0 => vec![(GL_TEXTURE0 + c.active as u32) as f64],
        0x84E1 => vec![(GL_TEXTURE0 + c.client_active as u32) as f64],
        0x0BE1 => vec![c.blend[0] as f64],
        0x0BE0 => vec![c.blend[1] as f64],
        0x0C22 => f(&c.clear_color),
        0x0B73 => vec![c.clear_depth as f64],
        0x0B72 => vec![c.depth_mask as u32 as f64],
        0x0B74 => vec![c.depth_func as f64],
        0x0C23 => (0..4).map(|k| ((c.color_mask >> k) & 1) as f64).collect(),
        0x9126 => vec![2.0],
        0x821E => vec![0.0],
        0x80E8 | 0x80E9 => vec![65536.0],
        0x0D32 | 0x0D36 => vec![32.0],
        0x0B8D => vec![c.program as f64],
        _ if c.caps.contains(&pname) => vec![1.0],
        0x0BE2 | 0x0B71 | 0x0BC0 | 0x0B44 | 0x0C11 | 0x0B90 | 0x0DE1 => vec![0.0],
        _ => {
            let _ = g;
            return None;
        }
    })
}

unsafe fn get_into<T: Copy>(pname: u32, out: *mut T, conv: impl Fn(f64) -> T) {
    let v = with(|g, c| get(g, c, pname).unwrap_or_else(|| vec![0.0]));
    if out.is_null() {
        return;
    }
    for (k, x) in v.iter().enumerate() {
        *out.add(k) = conv(*x);
    }
}

unsafe extern "C" fn glGetIntegerv(p: u32, out: *mut i32) {
    get_into(p, out, |x| x as i32)
}
unsafe extern "C" fn glGetFloatv(p: u32, out: *mut f32) {
    get_into(p, out, |x| x as f32)
}
unsafe extern "C" fn glGetDoublev(p: u32, out: *mut f64) {
    get_into(p, out, |x| x)
}
unsafe extern "C" fn glGetBooleanv(p: u32, out: *mut u8) {
    get_into(p, out, |x| (x != 0.0) as u8)
}

fn set_cap(cap: u32, on: bool) {
    with(|_, c| {
        if cap == GL_TEXTURE_2D {
            c.units[c.active].tex2d = on;
        } else if on {
            c.caps.insert(cap);
        } else {
            c.caps.remove(&cap);
        }
    })
}
extern "C" fn glEnable(cap: u32) {
    set_cap(cap, true)
}
extern "C" fn glDisable(cap: u32) {
    set_cap(cap, false)
}
extern "C" fn glIsEnabled(cap: u32) -> u8 {
    with(|_, c| if cap == GL_TEXTURE_2D { c.units[c.active].tex2d } else { c.caps.contains(&cap) } as u8)
}

fn client_state(cap: u32, on: bool) {
    with(|_, c| match cap {
        GL_VERTEX_ARRAY => c.vertex.on = on,
        GL_COLOR_ARRAY => c.colors.on = on,
        GL_TEXTURE_COORD_ARRAY => c.units[c.client_active].array.on = on,
        _ => {}
    })
}
extern "C" fn glEnableClientState(cap: u32) {
    client_state(cap, true)
}
extern "C" fn glDisableClientState(cap: u32) {
    client_state(cap, false)
}

extern "C" fn glViewport(x: i32, y: i32, w: i32, h: i32) {
    with(|_, c| c.viewport = [x, y, w, h])
}
extern "C" fn glScissor(x: i32, y: i32, w: i32, h: i32) {
    with(|_, c| c.scissor = [x, y, w, h])
}
extern "C" fn glDepthRange(n: f64, f: f64) {
    with(|_, c| c.depth_range = [n.clamp(0.0, 1.0) as f32, f.clamp(0.0, 1.0) as f32])
}
extern "C" fn glClearColor(r: f32, g: f32, b: f32, a: f32) {
    with(|_, c| c.clear_color = [r, g, b, a])
}
extern "C" fn glClearDepth(d: f64) {
    with(|_, c| c.clear_depth = d.clamp(0.0, 1.0) as f32)
}
extern "C" fn glClearDepthf(d: f32) {
    glClearDepth(d as f64)
}
extern "C" fn glClearStencil(s: i32) {
    with(|_, c| c.clear_stencil = s)
}
extern "C" fn glColorMask(r: u8, g: u8, b: u8, a: u8) {
    with(|_, c| c.color_mask = (r != 0) as u32 | ((g != 0) as u32) << 1 | ((b != 0) as u32) << 2 | ((a != 0) as u32) << 3)
}
extern "C" fn glDepthMask(m: u8) {
    with(|_, c| c.depth_mask = m != 0)
}
extern "C" fn glDepthFunc(f: u32) {
    with(|_, c| c.depth_func = f)
}
extern "C" fn glBlendFunc(s: u32, d: u32) {
    with(|_, c| {
        c.blend[0] = s;
        c.blend[1] = d;
        c.blend[2] = s;
        c.blend[3] = d;
    })
}
extern "C" fn glBlendFuncSeparate(s: u32, d: u32, sa: u32, da: u32) {
    with(|_, c| {
        c.blend[..4].copy_from_slice(&[s, d, sa, da]);
    })
}
extern "C" fn glBlendEquation(m: u32) {
    with(|_, c| {
        c.blend[4] = m;
        c.blend[5] = m;
    })
}
extern "C" fn glBlendEquationSeparate(m: u32, ma: u32) {
    with(|_, c| {
        c.blend[4] = m;
        c.blend[5] = ma;
    })
}
extern "C" fn glBlendColor(r: f32, g: f32, b: f32, a: f32) {
    with(|_, c| c.blend_color = [r, g, b, a])
}
extern "C" fn glAlphaFunc(f: u32, r: f32) {
    with(|_, c| {
        c.alpha_func = f;
        c.alpha_ref = r.clamp(0.0, 1.0);
    })
}
extern "C" fn glCullFace(m: u32) {
    with(|_, c| c.cull = m)
}
extern "C" fn glFrontFace(m: u32) {
    with(|_, c| c.front = m)
}
extern "C" fn glStencilFunc(f: u32, r: i32, m: u32) {
    with(|_, c| c.stencil = [f, r as u32, m])
}
extern "C" fn glStencilOp(a: u32, b: u32, d: u32) {
    with(|_, c| c.stencil_ops = [a, b, d])
}
extern "C" fn glStencilMask(m: u32) {
    with(|_, c| c.stencil_mask = m)
}
extern "C" fn glHint(_: u32, _: u32) {}
extern "C" fn glShadeModel(m: u32) {
    with(|_, c| c.flat = m == 0x1D00)
}
extern "C" fn glPolygonMode(_: u32, _: u32) {}
extern "C" fn glLineWidth(_: f32) {}
extern "C" fn glPointSize(_: f32) {}
extern "C" fn glPolygonOffset(_: f32, _: f32) {}
extern "C" fn glReadBuffer(_: u32) {}
extern "C" fn glDrawBuffer(_: u32) {}
extern "C" fn glFlush() {
    gpu::flush();
}
extern "C" fn glFinish() {
    let gen = gpu::batch_gen();
    gpu::flush();
    gpu::wait_done(gen);
}
extern "C" fn glPixelStorei(p: u32, v: i32) {
    with(|_, c| match p {
        0x0CF5 => c.unpack[0] = v,
        0x0CF2 => c.unpack[1] = v,
        0x0CF3 => c.unpack[2] = v,
        0x0CF4 => c.unpack[3] = v,
        0x0D05 => c.pack[0] = v,
        0x0D02 => c.pack[1] = v,
        0x0D03 => c.pack[2] = v,
        0x0D04 => c.pack[3] = v,
        _ => {}
    })
}

// ---- matrices ----

extern "C" fn glMatrixMode(m: u32) {
    with(|_, c| c.matrix_mode = m)
}
extern "C" fn glLoadIdentity() {
    with(|_, c| *c.top() = IDENTITY)
}
unsafe extern "C" fn glLoadMatrixf(m: *const f32) {
    let v: [f32; 16] = std::ptr::read_unaligned(m as *const [f32; 16]);
    with(|_, c| *c.top() = v)
}
unsafe extern "C" fn glLoadMatrixd(m: *const f64) {
    let v: [f32; 16] = std::array::from_fn(|k| *m.add(k) as f32);
    with(|_, c| *c.top() = v)
}
fn mult(m: &[f32; 16]) {
    with(|_, c| {
        let t = c.top();
        *t = mul(t, m);
    })
}
unsafe extern "C" fn glMultMatrixf(m: *const f32) {
    mult(&std::ptr::read_unaligned(m as *const [f32; 16]))
}
unsafe extern "C" fn glMultMatrixd(m: *const f64) {
    mult(&std::array::from_fn(|k| *m.add(k) as f32))
}
extern "C" fn glPushMatrix() {
    with(|_, c| {
        let t = *c.top();
        c.stack().push(t);
    })
}
extern "C" fn glPopMatrix() {
    with(|_, c| {
        if c.stack().len() > 1 {
            c.stack().pop();
        } else {
            c.err(0x0504);
        }
    })
}
extern "C" fn glOrtho(l: f64, r: f64, b: f64, t: f64, n: f64, f: f64) {
    let (l, r, b, t, n, f) = (l as f32, r as f32, b as f32, t as f32, n as f32, f as f32);
    mult(&[2.0 / (r - l), 0.0, 0.0, 0.0, 0.0, 2.0 / (t - b), 0.0, 0.0, 0.0, 0.0, -2.0 / (f - n), 0.0, -(r + l) / (r - l), -(t + b) / (t - b), -(f + n) / (f - n), 1.0])
}
extern "C" fn glFrustum(l: f64, r: f64, b: f64, t: f64, n: f64, f: f64) {
    let (l, r, b, t, n, f) = (l as f32, r as f32, b as f32, t as f32, n as f32, f as f32);
    mult(&[2.0 * n / (r - l), 0.0, 0.0, 0.0, 0.0, 2.0 * n / (t - b), 0.0, 0.0, (r + l) / (r - l), (t + b) / (t - b), -(f + n) / (f - n), -1.0, 0.0, 0.0, -2.0 * f * n / (f - n), 0.0])
}
extern "C" fn glTranslatef(x: f32, y: f32, z: f32) {
    mult(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, x, y, z, 1.0])
}
extern "C" fn glTranslated(x: f64, y: f64, z: f64) {
    glTranslatef(x as f32, y as f32, z as f32)
}
extern "C" fn glScalef(x: f32, y: f32, z: f32) {
    mult(&[x, 0.0, 0.0, 0.0, 0.0, y, 0.0, 0.0, 0.0, 0.0, z, 0.0, 0.0, 0.0, 0.0, 1.0])
}
extern "C" fn glScaled(x: f64, y: f64, z: f64) {
    glScalef(x as f32, y as f32, z as f32)
}
extern "C" fn glRotatef(a: f32, x: f32, y: f32, z: f32) {
    let l = (x * x + y * y + z * z).sqrt();
    if l == 0.0 {
        return;
    }
    let (x, y, z) = (x / l, y / l, z / l);
    let (s, c) = a.to_radians().sin_cos();
    let k = 1.0 - c;
    mult(&[
        x * x * k + c, y * x * k + z * s, x * z * k - y * s, 0.0,
        x * y * k - z * s, y * y * k + c, y * z * k + x * s, 0.0,
        x * z * k + y * s, y * z * k - x * s, z * z * k + c, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ])
}
extern "C" fn glRotated(a: f64, x: f64, y: f64, z: f64) {
    glRotatef(a as f32, x as f32, y as f32, z as f32)
}

// ---- immediate mode ----

extern "C" fn glBegin(mode: u32) {
    with(|_, c| {
        c.begin = Some(mode);
        c.verts.clear();
    })
}
extern "C" fn glEnd() {
    with(|g, c| {
        let Some(mode) = c.begin.take() else { return };
        let v = std::mem::take(&mut c.verts);
        if c.program != 0 {
            return glsl::draw_from(g, c, mode, glsl::Src::Immediate(&v));
        }
        draw(g, c, mode, v);
    })
}
fn vertex(x: f32, y: f32, z: f32, w: f32) {
    with(|_, c| {
        if c.begin.is_none() {
            return;
        }
        let (t0, t1) = (c.units[0].coords, c.units[1].coords);
        let col = c.color;
        c.verts.push([x, y, z, w, col[0], col[1], col[2], col[3], t0[0], t0[1], t0[2], t0[3], t1[0], t1[1], t1[2], t1[3]]);
    })
}
extern "C" fn glVertex2f(x: f32, y: f32) {
    vertex(x, y, 0.0, 1.0)
}
extern "C" fn glVertex2i(x: i32, y: i32) {
    vertex(x as f32, y as f32, 0.0, 1.0)
}
extern "C" fn glVertex2s(x: i32, y: i32) {
    vertex(x as i16 as f32, y as i16 as f32, 0.0, 1.0)
}
extern "C" fn glVertex2d(x: f64, y: f64) {
    vertex(x as f32, y as f32, 0.0, 1.0)
}
extern "C" fn glVertex3f(x: f32, y: f32, z: f32) {
    vertex(x, y, z, 1.0)
}
extern "C" fn glVertex3d(x: f64, y: f64, z: f64) {
    vertex(x as f32, y as f32, z as f32, 1.0)
}
extern "C" fn glVertex3i(x: i32, y: i32, z: i32) {
    vertex(x as f32, y as f32, z as f32, 1.0)
}
extern "C" fn glVertex4f(x: f32, y: f32, z: f32, w: f32) {
    vertex(x, y, z, w)
}
unsafe extern "C" fn glVertex2fv(v: *const f32) {
    vertex(*v, *v.add(1), 0.0, 1.0)
}
unsafe extern "C" fn glVertex3fv(v: *const f32) {
    vertex(*v, *v.add(1), *v.add(2), 1.0)
}
unsafe extern "C" fn glVertex2iv(v: *const i32) {
    vertex(*v as f32, *v.add(1) as f32, 0.0, 1.0)
}
fn color(r: f32, g: f32, b: f32, a: f32) {
    with(|_, c| c.color = [r, g, b, a])
}
extern "C" fn glColor3f(r: f32, g: f32, b: f32) {
    color(r, g, b, 1.0)
}
extern "C" fn glColor4f(r: f32, g: f32, b: f32, a: f32) {
    color(r, g, b, a)
}
extern "C" fn glColor3d(r: f64, g: f64, b: f64) {
    color(r as f32, g as f32, b as f32, 1.0)
}
extern "C" fn glColor4d(r: f64, g: f64, b: f64, a: f64) {
    color(r as f32, g as f32, b as f32, a as f32)
}
extern "C" fn glColor3ub(r: u8, g: u8, b: u8) {
    color(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}
extern "C" fn glColor4ub(r: u8, g: u8, b: u8, a: u8) {
    color(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0)
}
unsafe extern "C" fn glColor3fv(v: *const f32) {
    color(*v, *v.add(1), *v.add(2), 1.0)
}
unsafe extern "C" fn glColor4fv(v: *const f32) {
    color(*v, *v.add(1), *v.add(2), *v.add(3))
}
unsafe extern "C" fn glColor3ubv(v: *const u8) {
    glColor3ub(*v, *v.add(1), *v.add(2))
}
unsafe extern "C" fn glColor4ubv(v: *const u8) {
    glColor4ub(*v, *v.add(1), *v.add(2), *v.add(3))
}
fn texcoord(unit: usize, s: f32, t: f32) {
    with(|_, c| c.units[unit.min(7)].coords = [s, t, 0.0, 1.0])
}
extern "C" fn glTexCoord2f(s: f32, t: f32) {
    texcoord(0, s, t)
}
extern "C" fn glTexCoord2d(s: f64, t: f64) {
    texcoord(0, s as f32, t as f32)
}
extern "C" fn glTexCoord2i(s: i32, t: i32) {
    texcoord(0, s as f32, t as f32)
}
unsafe extern "C" fn glTexCoord2fv(v: *const f32) {
    texcoord(0, *v, *v.add(1))
}
extern "C" fn glMultiTexCoord2f(u: u32, s: f32, t: f32) {
    texcoord(u.wrapping_sub(GL_TEXTURE0) as usize, s, t)
}
unsafe extern "C" fn glMultiTexCoord2fv(u: u32, v: *const f32) {
    texcoord(u.wrapping_sub(GL_TEXTURE0) as usize, *v, *v.add(1))
}
extern "C" fn glNormal3f(_: f32, _: f32, _: f32) {}

// ---- arrays ----

fn pointer(which: u32, size: i32, ty: u32, stride: i32, ptr: u32) {
    with(|_, c| {
        let a = Array { on: false, size, ty, stride, ptr, buffer: c.array_buffer, normalized: which == 1 };
        let slot = match which {
            0 => &mut c.vertex,
            1 => &mut c.colors,
            _ => &mut c.units[c.client_active].array,
        };
        *slot = Array { on: slot.on, ..a };
    })
}
extern "C" fn glVertexPointer(size: i32, ty: u32, stride: i32, ptr: u32) {
    pointer(0, size, ty, stride, ptr)
}
extern "C" fn glColorPointer(size: i32, ty: u32, stride: i32, ptr: u32) {
    pointer(1, size, ty, stride, ptr)
}
extern "C" fn glTexCoordPointer(size: i32, ty: u32, stride: i32, ptr: u32) {
    pointer(2, size, ty, stride, ptr)
}
extern "C" fn glNormalPointer(_: u32, _: i32, _: u32) {}
extern "C" fn glClientActiveTexture(u: u32) {
    with(|_, c| c.client_active = (u.wrapping_sub(GL_TEXTURE0) as usize).min(7))
}
extern "C" fn glActiveTexture(u: u32) {
    with(|_, c| c.active = (u.wrapping_sub(GL_TEXTURE0) as usize).min(7))
}

fn type_size(ty: u32) -> usize {
    match ty {
        0x1400 | 0x1401 => 1,
        0x1402 | 0x1403 | 0x140B => 2,
        0x140A => 8,
        _ => 4,
    }
}

/// Element `i` of array `a` as 4 floats (missing ones from `def`).
pub(crate) fn fetch(g: &Gl, a: &Array, i: u32, def: [f32; 4]) -> [f32; 4] {
    let ts = type_size(a.ty);
    let stride = if a.stride > 0 { a.stride as usize } else { ts * a.size.max(1) as usize };
    let off = a.ptr as usize + stride * i as usize;
    let base: *const u8 = if a.buffer != 0 {
        match g.buffers.get(&a.buffer) {
            Some(b) if off + ts * a.size as usize <= b.data.len() => unsafe { b.data.as_ptr().add(off) },
            _ => return def,
        }
    } else {
        if a.ptr == 0 {
            return def;
        }
        off as *const u8
    };
    let mut v = def;
    let n = if a.size == 0x80E1 { 4 } else { a.size.clamp(1, 4) as usize };
    for k in 0..n {
        let p = unsafe { base.add(k * ts) };
        v[k] = unsafe {
            match a.ty {
                0x1400 => {
                    let x = *(p as *const i8) as f32;
                    if a.normalized { (x / 127.0).max(-1.0) } else { x }
                }
                0x1401 => {
                    let x = *p as f32;
                    if a.normalized { x / 255.0 } else { x }
                }
                0x1402 => {
                    let x = std::ptr::read_unaligned(p as *const i16) as f32;
                    if a.normalized { (x / 32767.0).max(-1.0) } else { x }
                }
                0x1403 => {
                    let x = std::ptr::read_unaligned(p as *const u16) as f32;
                    if a.normalized { x / 65535.0 } else { x }
                }
                0x1404 => std::ptr::read_unaligned(p as *const i32) as f32,
                0x1405 => std::ptr::read_unaligned(p as *const u32) as f32,
                0x140A => std::ptr::read_unaligned(p as *const f64) as f32,
                _ => std::ptr::read_unaligned(p as *const f32),
            }
        };
    }
    if a.size == 0x80E1 {
        v.swap(0, 2);
    }
    v
}

fn array_vertex(g: &Gl, c: &Ctx, i: u32) -> [f32; 16] {
    let p = if c.vertex.on { fetch(g, &c.vertex, i, [0.0, 0.0, 0.0, 1.0]) } else { [0.0, 0.0, 0.0, 1.0] };
    let col = if c.colors.on { fetch(g, &c.colors, i, [0.0, 0.0, 0.0, 1.0]) } else { c.color };
    let t: [[f32; 4]; 2] = std::array::from_fn(|u| {
        let un = &c.units[u];
        if un.array.on { fetch(g, &un.array, i, [0.0, 0.0, 0.0, 1.0]) } else { un.coords }
    });
    [p[0], p[1], p[2], p[3], col[0], col[1], col[2], col[3], t[0][0], t[0][1], t[0][2], t[0][3], t[1][0], t[1][1], t[1][2], t[1][3]]
}

/// The indices of glDrawElements (from the element buffer or client memory).
pub(crate) fn indices(g: &Gl, c: &Ctx, count: i32, ty: u32, ptr: u32) -> Vec<u32> {
    let n = count.max(0) as usize;
    let ts = type_size(ty);
    let src: *const u8 = if c.element_buffer != 0 {
        match g.buffers.get(&c.element_buffer) {
            Some(b) if ptr as usize + n * ts <= b.data.len() => unsafe { b.data.as_ptr().add(ptr as usize) },
            _ => return Vec::new(),
        }
    } else {
        ptr as *const u8
    };
    if src.is_null() {
        return Vec::new();
    }
    (0..n)
        .map(|k| unsafe {
            match ty {
                GL_UNSIGNED_BYTE => *src.add(k) as u32,
                GL_UNSIGNED_SHORT => std::ptr::read_unaligned((src as *const u16).add(k)) as u32,
                _ => std::ptr::read_unaligned((src as *const u32).add(k)),
            }
        })
        .collect()
}

extern "C" fn glDrawArrays(mode: u32, first: i32, count: i32) {
    with(|g, c| {
        if c.program != 0 {
            let idx: Vec<u32> = (first.max(0) as u32..(first + count).max(0) as u32).collect();
            return glsl::draw(g, c, mode, &idx);
        }
        let v = (first.max(0)..(first + count).max(0)).map(|i| array_vertex(g, c, i as u32)).collect();
        draw(g, c, mode, v);
    })
}
extern "C" fn glDrawElements(mode: u32, count: i32, ty: u32, ptr: u32) {
    with(|g, c| {
        let idx = indices(g, c, count, ty, ptr);
        if c.program != 0 {
            return glsl::draw(g, c, mode, &idx);
        }
        let v = idx.iter().map(|&i| array_vertex(g, c, i)).collect();
        draw(g, c, mode, v);
    })
}
extern "C" fn glDrawRangeElements(mode: u32, _start: u32, _end: u32, count: i32, ty: u32, ptr: u32) {
    glDrawElements(mode, count, ty, ptr)
}

/// GL primitives as a WebGPU topology (0 points, 1 lines, 2 line strip, 3 triangles, 4 triangle
/// strip) and the order of the vertices for it.
pub(crate) fn assemble(mode: u32, n: usize) -> (u32, Vec<usize>) {
    match mode {
        0 => (0, (0..n).collect()),
        1 => (1, (0..n / 2 * 2).collect()),
        2 => {
            let mut v: Vec<usize> = (0..n).collect();
            if n > 1 {
                v.push(0);
            }
            (2, v)
        }
        3 => (2, (0..n).collect()),
        4 => (3, (0..n / 3 * 3).collect()),
        5 => (4, (0..n).collect()),
        6 | 9 => (3, (1..n.saturating_sub(1)).flat_map(|i| [0, i, i + 1]).collect()),
        7 => (3, (0..n / 4).flat_map(|q| [4 * q, 4 * q + 1, 4 * q + 2, 4 * q, 4 * q + 2, 4 * q + 3]).collect()),
        8 => (4, (0..n / 2 * 2).collect()),
        _ => (3, Vec::new()),
    }
}

// ---- drawing ----

/// The current draw framebuffer: (color texture, level, depth texture, width, height)
pub(crate) fn target(g: &mut Gl, c: &Ctx) -> Option<(u32, u32, u32, u32, u32)> {
    if c.fbo_draw == 0 {
        if c.draw == 0 {
            return None;
        }
        let d = drawable(g, c.draw, c.w.max(1), c.h.max(1));
        return Some((d.color, 0, d.depth, d.w, d.h));
    }
    let f = g.fbos.get(&c.fbo_draw)?;
    let (color, depth) = (f.color, f.depth);
    let att = |g: &Gl, a: Att| -> Option<(u32, u32, u32, u32)> {
        match a {
            Att::Tex(t, l) => g.textures.get(&t).filter(|t| t.gpu != 0).map(|t| (t.gpu, l, (t.w >> l).max(1), (t.h >> l).max(1))),
            Att::Rb(r) => g.rbs.get(&r).filter(|r| r.gpu != 0).map(|r| (r.gpu, 0, r.w, r.h)),
            Att::None => None,
        }
    };
    let c0 = att(g, color);
    let d0 = att(g, depth);
    let (w, h) = c0.or(d0).map(|x| (x.2, x.3))?;
    Some((c0.map_or(0, |x| x.0), c0.map_or(0, |x| x.1), d0.map_or(0, |x| x.0), w, h))
}

fn blend_factor(f: u32) -> u32 {
    match f {
        0 => 0,
        1 => 1,
        0x300 => 2,
        0x301 => 3,
        0x302 => 4,
        0x303 => 5,
        0x304 => 6,
        0x305 => 7,
        0x306 => 8,
        0x307 => 9,
        0x308 => 10,
        0x8001 | 0x8003 => 11,
        0x8002 | 0x8004 => 12,
        _ => 1,
    }
}

fn blend_op(m: u32) -> u32 {
    match m {
        0x800A => 2,
        0x800B => 3,
        0x8007 => 4,
        0x8008 => 5,
        _ => 1,
    }
}

fn stencil_op(o: u32) -> u32 {
    match o {
        0 => 2,
        0x1E01 => 3,
        0x1E02 => 4,
        0x1E03 => 5,
        0x150A => 6,
        0x8507 => 7,
        0x8508 => 8,
        _ => 1,
    }
}

fn compare(f: u32) -> u32 {
    if (0x200..=0x207).contains(&f) { f - 0x200 + 1 } else { 8 }
}

/// The pipeline key's states after the shaders (topology on): see d3d11::draw.
pub(crate) fn pipeline_key(c: &Ctx, vs: u32, ps: u32, topo: u32, depth: bool, vertex: &[u32]) -> Vec<u32> {
    let mut k = vec![vs, ps, topo, 0];
    let cull = if !c.caps.contains(&GL_CULL_FACE) {
        1
    } else {
        match c.cull {
            GL_FRONT => 2,
            GL_BACK => 3,
            _ => 1,
        }
    };
    // (y is negated: GL's counter-clockwise is clockwise on the page)
    k.extend_from_slice(&[cull, (c.front == GL_CW) as u32, 0, 0, 0]);
    if depth {
        let dt = c.caps.contains(&GL_DEPTH_TEST);
        let st = c.caps.contains(&GL_STENCIL_TEST);
        let face = [stencil_op(c.stencil_ops[0]), stencil_op(c.stencil_ops[1]), stencil_op(c.stencil_ops[2]), compare(c.stencil[0])];
        k.extend_from_slice(&[P_D24S8, dt as u32, c.depth_mask as u32, compare(c.depth_func), st as u32, (c.stencil[2] & 0xff) | (c.stencil_mask & 0xff) << 8]);
        k.extend_from_slice(&face);
        k.extend_from_slice(&face);
    } else {
        k.push(0xffff);
    }
    k.extend_from_slice(vertex);
    let b = &c.blend;
    let on = c.caps.contains(&GL_BLEND) as u32;
    k.extend_from_slice(&[1, P_RGBA8, on, blend_factor(b[0]), blend_factor(b[1]), blend_op(b[4]), blend_factor(b[2]), blend_factor(b[3]), blend_op(b[5]), c.color_mask]);
    k
}

pub(crate) fn pipeline(g: &mut Gl, key: Vec<u32>) -> u32 {
    if let Some(&p) = g.pipelines.get(&key) {
        crate::render::gpu::pipeline_hit();
        return p;
    }
    let id = page_id();
    let mut cmd = vec![gpu::CREATE_PIPELINE2, id, key.len() as u32];
    cmd.extend_from_slice(&key);
    gpu::record(|q| q.cmd(&cmd));
    g.pipelines.insert(key, id);
    id
}

pub(crate) fn shader(code: &str) -> u32 {
    let id = page_id();
    gpu::record(|q| {
        let off = q.blob(code.as_bytes());
        q.cmd(&[gpu::CREATE_SHADER, id, off, code.len() as u32]);
    });
    id
}

/// D3D11-style sampler key of texture `t` (see d3d11::sampler_key) and its max LOD.
pub(crate) fn sampler(t: Option<&Texture>) -> (u32, u32) {
    let Some(t) = t else { return (2 | 2 << 3 | 2 << 6 | 1 << 17, 0) };
    let wrap = |w: u32| match w {
        0x8370 => 1,
        0x2900 | 0x812F | 0x812D => 2,
        _ => 0,
    };
    let mip_filter = matches!(t.min, 0x2700..=0x2703);
    let mipmapped = mip_filter && (t.defined.count_ones() > 1 || t.gen_mips);
    let min = matches!(t.min, 0x2601 | 0x2701 | 0x2703) as u32;
    let mip = matches!(t.min, 0x2702 | 0x2703) as u32;
    let mag = (t.mag == 0x2601) as u32;
    let key = wrap(t.wrap_s) | wrap(t.wrap_t) << 3 | 2 << 6 | mag << 9 | min << 10 | mip << 11 | 1 << 17;
    (key, if mipmapped { 1000f32.to_bits() } else { 0 })
}

/// Viewport, scissor, blend constant and stencil reference words of DRAW2.
pub(crate) fn draw_state(c: &Ctx, w: u32, h: u32) -> Vec<u32> {
    let v = c.viewport;
    let mut out: Vec<u32> = [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32, c.depth_range[0], c.depth_range[1]].iter().map(|f| f.to_bits()).collect();
    if c.caps.contains(&GL_SCISSOR_TEST) {
        let s = c.scissor;
        out.extend_from_slice(&[s[0].max(0) as u32, s[1].max(0) as u32, s[2].max(0) as u32, s[3].max(0) as u32, 1]);
    } else {
        out.extend_from_slice(&[0, 0, w, h, 0]);
    }
    out.extend(c.blend_color.iter().map(|f| f.to_bits()));
    out.push(c.stencil[1]);
    out
}

/// Flat shading: triangles as a list, each vertex with the color of the triangle's provoking
/// vertex (its last one; a quad's or quad strip quad's last one).
fn flat_order(mode: u32, n: usize) -> Option<Vec<(usize, usize)>> {
    let tris: Vec<([usize; 3], usize)> = match mode {
        4 => (0..n / 3).map(|t| ([3 * t, 3 * t + 1, 3 * t + 2], 3 * t + 2)).collect(),
        5 => (0..n.saturating_sub(2)).map(|i| (if i % 2 == 0 { [i, i + 1, i + 2] } else { [i + 1, i, i + 2] }, i + 2)).collect(),
        6 => (1..n.saturating_sub(1)).map(|i| ([0, i, i + 1], i + 1)).collect(),
        7 => (0..n / 4).flat_map(|q| [([4 * q, 4 * q + 1, 4 * q + 2], 4 * q + 3), ([4 * q, 4 * q + 2, 4 * q + 3], 4 * q + 3)]).collect(),
        8 => (0..n.saturating_sub(2) / 2).flat_map(|q| [([2 * q, 2 * q + 1, 2 * q + 3], 2 * q + 3), ([2 * q, 2 * q + 3, 2 * q + 2], 2 * q + 3)]).collect(),
        // (a polygon takes the first vertex's color)
        9 => (1..n.saturating_sub(1)).map(|i| ([0, i, i + 1], 0)).collect(),
        _ => return None,
    };
    Some(tris.iter().flat_map(|&(t, p)| t.map(|i| (i, p))).collect())
}

fn draw(g: &mut Gl, c: &mut Ctx, mode: u32, v: Vec<[f32; 16]>) {
    if c.flat {
        if let Some(fo) = flat_order(mode, v.len()) {
            let nv: Vec<[f32; 16]> = fo
                .iter()
                .map(|&(i, p)| {
                    let mut x = v[i];
                    x[4..8].copy_from_slice(&v[p][4..8]);
                    x
                })
                .collect();
            c.flat = false;
            draw(g, c, 4, nv);
            c.flat = true;
            return;
        }
    }
    let (topo, order) = assemble(mode, v.len());
    if order.is_empty() {
        return;
    }
    let Some((color, level, depth, w, h)) = target(g, c) else { return };
    let (vs, ps) = *g.ff.get_or_insert_with(|| (shader(FF_VS), shader(FF_PS)));
    let vertex = [1, 64, 0, 4, 3, 0, 0, 3, 16, 1, 3, 32, 2, 3, 48, 3];
    let pipe = pipeline(g, pipeline_key(c, vs, ps, topo, depth != 0, &vertex));
    let verts: Vec<[f32; 16]> = order.iter().map(|&i| v[i]).collect();
    // uniforms
    let mvp = mul(c.projection.last().unwrap(), c.modelview.last().unwrap());
    let mut vu = Vec::with_capacity(52);
    vu.extend_from_slice(&mvp);
    vu.extend_from_slice(c.units[0].matrices.last().unwrap());
    vu.extend_from_slice(c.units[1].matrices.last().unwrap());
    vu.extend_from_slice(&[0.0; 4]);
    let mut texs = [(0u32, 1u32, (0u32, 0u32)); 2];
    let mut fu = [0u32; 20];
    for u in 0..2 {
        let un = &c.units[u];
        let t = if un.tex2d { g.textures.get(&un.bound).filter(|t| t.gpu != 0 && t.base != 5) } else { None };
        if let Some(t) = t {
            fu[u] = match un.env {
                0x2101 => 3,
                0x1E01 => 2,
                0x0104 => 4,
                0x0BE2 => 5,
                _ => 1,
            };
            fu[16 + u] = t.base;
            texs[u] = (t.gpu, t.levels, sampler(Some(t)));
        } else {
            texs[u].2 = sampler(None);
        }
        for k in 0..4 {
            fu[8 + u * 4 + k] = un.env_color[k].to_bits();
        }
    }
    fu[2] = if c.caps.contains(&GL_ALPHA_TEST) { c.alpha_func.wrapping_sub(0x200).min(7) } else { 7 };
    fu[4] = c.alpha_ref.to_bits();
    let state = draw_state(c, w, h);
    gpu::record(|q| {
        q.cmd(&[gpu::SET_TARGETS2, (color != 0) as u32]);
        if color != 0 {
            q.cmd(&[color, level, 0]);
        }
        q.cmd(&[depth, 0, 0]);
        let vo = q.data(bytes(&verts), 16);
        let vuo = q.data(bytes(&vu), 256);
        let fuo = q.data(bytes(&fu), 256);
        let mut w = vec![gpu::DRAW2, pipe];
        w.extend_from_slice(&state);
        w.extend_from_slice(&[1, DATA, vo, 0, 0, 0]);
        w.extend_from_slice(&[1, 0, 0, DATA, vuo, 208, 208, 0, 0]);
        w.extend_from_slice(&[5, 0, 0, DATA, fuo, 80, 80, 0, 0]);
        for (u, t) in texs.iter().enumerate() {
            w.extend_from_slice(&[16 + u as u32, 1, t.0, 1, 0, t.1, 0, 1]);
        }
        for (u, t) in texs.iter().enumerate() {
            w.extend_from_slice(&[160 + u as u32, 2, t.2 .0, 0, t.2 .1, 0, 0, 0]);
        }
        w.extend_from_slice(&[0, verts.len() as u32, 1, 0, 0, 0, 0]);
        q.cmd(&w);
    });
}

extern "C" fn glClear(mask: u32) {
    with(|g, c| {
        let Some((color, level, depth, _, _)) = target(g, c) else { return };
        let cc = c.clear_color.map(|f| f.to_bits());
        let (cd, cs) = (c.clear_depth.to_bits(), c.clear_stencil as u32 & 0xff);
        // (with the scissor test on: only its box, as a rectangle clear of the targets)
        if c.caps.contains(&GL_SCISSOR_TEST) && color != 0 {
            let sc = c.scissor;
            let f = (mask & 0x4000 != 0 && c.color_mask != 0) as u32 | (mask & 0x100 != 0 && depth != 0) as u32 * 2 | (mask & 0x400 != 0 && depth != 0) as u32 * 4;
            let (x, y) = (sc[0].max(0) as u32, sc[1].max(0) as u32);
            let (w, h) = ((sc[0] + sc[2]).max(0) as u32 - x, (sc[1] + sc[3]).max(0) as u32 - y);
            if f != 0 && w > 0 && h > 0 {
                gpu::record(|q| {
                    q.cmd(&[gpu::SET_TARGETS2, 1, color, level, 0, depth, 0, 0]);
                    q.cmd(&[gpu::CLEAR, f, cc[0], cc[1], cc[2], cc[3], cd, cs, x, y, w, h]);
                });
            }
            return;
        }
        gpu::record(|q| {
            if mask & 0x4000 != 0 && color != 0 && c.color_mask != 0 {
                q.cmd(&[gpu::CLEAR_VIEW, color, level, 0, 1, cc[0], cc[1], cc[2], cc[3], 0, 0]);
            }
            let f = (mask & 0x100 != 0) as u32 * 2 | (mask & 0x400 != 0) as u32 * 4;
            if f != 0 && depth != 0 {
                q.cmd(&[gpu::CLEAR_VIEW, depth, 0, 0, f, 0, 0, 0, 0, cd, cs]);
            }
        });
    })
}

// ---- textures ----

unsafe extern "C" fn glGenTextures(n: i32, out: *mut u32) {
    gen(n, out, |g, id| {
        g.textures.insert(id, Texture { min: 0x2702, mag: 0x2601, wrap_s: 0x2901, wrap_t: 0x2901, ..Default::default() });
    })
}

pub(crate) unsafe fn gen(n: i32, out: *mut u32, f: impl Fn(&mut Gl, u32)) {
    let mut g = lock();
    let g = g.as_mut().unwrap();
    for k in 0..n.max(0) as usize {
        let id = g.next_name;
        g.next_name += 1;
        f(g, id);
        *out.add(k) = id;
    }
}

unsafe extern "C" fn glDeleteTextures(n: i32, names: *const u32) {
    let mut g = lock();
    let g = g.as_mut().unwrap();
    for k in 0..n.max(0) as usize {
        if let Some(t) = g.textures.remove(&*names.add(k)) {
            if t.gpu != 0 {
                gpu::record(|q| q.cmd(&[gpu::DESTROY_TEXTURE, t.gpu]));
            }
        }
    }
}

extern "C" fn glBindTexture(target: u32, t: u32) {
    if target != GL_TEXTURE_2D {
        return;
    }
    with(|g, c| {
        if t != 0 && !g.textures.contains_key(&t) {
            g.textures.insert(t, Texture { min: 0x2702, mag: 0x2601, wrap_s: 0x2901, wrap_t: 0x2901, ..Default::default() });
        }
        c.units[c.active].bound = t;
    })
}

extern "C" fn glIsTexture(t: u32) -> u8 {
    lock().as_ref().unwrap().textures.contains_key(&t) as u8
}

fn base_format(internal: i32) -> u32 {
    match internal as u32 {
        0x1906 | 0x803B..=0x803E => 2,
        1 | 0x1909 | 0x803F..=0x8042 => 3,
        2 | 0x190A | 0x8043..=0x8048 => 4,
        3 | 0x1907 | 0x8050..=0x8054 | 0x8D62 | 0x8C40 | 0x8C41 | 0x2A10 => 1,
        0x1902 | 0x81A5..=0x81A7 | 0x84F9 | 0x88F0 | 0x8CAC | 0x8CAD => 5,
        _ => 0,
    }
}

/// Pixels in GL format/type as RGBA8 rows (w x h), with the unpack state's alignment and row length.
pub(crate) unsafe fn to_rgba(src: *const u8, w: u32, h: u32, format: u32, ty: u32, base: u32, unpack: [i32; 4]) -> Vec<u8> {
    let comps = match format {
        0x1906 | 0x1909 | 0x1903 | 0x1904 | 0x1905 => 1,
        0x190A | 0x8227 => 2,
        0x1907 | 0x80E0 => 3,
        _ => 4,
    };
    let packed = matches!(ty, 0x8363 | 0x8364 | 0x8033 | 0x8365 | 0x8034 | 0x8366);
    let bpp = if packed {
        2
    } else if matches!(ty, 0x8035 | 0x8367) {
        4
    } else {
        comps * type_size(ty)
    };
    let row_px = if unpack[1] > 0 { unpack[1] as usize } else { w as usize };
    let align = unpack[0].max(1) as usize;
    let pitch = (row_px * bpp).next_multiple_of(align);
    let src = src.add(unpack[2].max(0) as usize * pitch + unpack[3].max(0) as usize * bpp);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        let row = src.add(y * pitch);
        for x in 0..w as usize {
            let p = row.add(x * bpp);
            let ch = |k: usize| -> u8 {
                match ty {
                    GL_FLOAT => (std::ptr::read_unaligned((p as *const f32).add(k)).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                    GL_UNSIGNED_SHORT => (std::ptr::read_unaligned((p as *const u16).add(k)) >> 8) as u8,
                    _ => *p.add(k),
                }
            };
            let [r, g, b, a]: [u8; 4] = if packed {
                let v = std::ptr::read_unaligned(p as *const u16) as u32;
                let e = |v: u32, bits: u32| ((v * 255 + ((1 << bits) - 1) / 2) / ((1 << bits) - 1)) as u8;
                let rgba = match ty {
                    0x8363 => [e(v >> 11, 5), e((v >> 5) & 63, 6), e(v & 31, 5), 255],
                    0x8364 => [e(v & 31, 5), e((v >> 5) & 63, 6), e(v >> 11, 5), 255],
                    0x8033 => [e(v >> 12, 4), e((v >> 8) & 15, 4), e((v >> 4) & 15, 4), e(v & 15, 4)],
                    0x8365 => [e(v & 15, 4), e((v >> 4) & 15, 4), e((v >> 8) & 15, 4), e(v >> 12, 4)],
                    0x8034 => [e(v >> 11, 5), e((v >> 6) & 31, 5), e((v >> 1) & 31, 5), e(v & 1, 1)],
                    _ => [e(v & 31, 5), e((v >> 5) & 31, 5), e((v >> 10) & 31, 5), e(v >> 15, 1)],
                };
                // (BGRA with packed types: components in that order)
                if format == 0x80E1 { [rgba[2], rgba[1], rgba[0], rgba[3]] } else { rgba }
            } else if ty == 0x8035 {
                let b4 = [*p, *p.add(1), *p.add(2), *p.add(3)];
                let v = [b4[3], b4[2], b4[1], b4[0]];
                if format == 0x80E1 { [v[2], v[1], v[0], v[3]] } else { v }
            } else {
                match format {
                    0x1906 => [0, 0, 0, ch(0)],
                    0x1909 => [ch(0), ch(0), ch(0), 255],
                    0x190A => [ch(0), ch(0), ch(0), ch(1)],
                    0x1903 => [ch(0), 0, 0, 255],
                    0x8227 => [ch(0), ch(1), 0, 255],
                    0x1907 => [ch(0), ch(1), ch(2), 255],
                    0x80E0 => [ch(2), ch(1), ch(0), 255],
                    0x80E1 => [ch(2), ch(1), ch(0), ch(3)],
                    _ => [ch(0), ch(1), ch(2), ch(3)],
                }
            };
            let px = match base {
                1 => [r, g, b, 255],
                2 => [0, 0, 0, if format == 0x1906 { a } else { a }],
                3 => [r, r, r, 255],
                4 => [r, r, r, a],
                _ => [r, g, b, a],
            };
            out[(y * w as usize + x) * 4..][..4].copy_from_slice(&px);
        }
    }
    out
}

fn bound_texture(c: &Ctx) -> u32 {
    c.units[c.active].bound
}

/// (Re)makes `t`'s page texture for a level 0 of w x h.
fn alloc_texture(t: &mut Texture, w: u32, h: u32, base: u32) {
    if t.gpu != 0 && t.w == w && t.h == h && (t.base == 5) == (base == 5) {
        t.base = base;
        return;
    }
    let old = t.gpu;
    t.gpu = page_id();
    t.w = w.max(1);
    t.h = h.max(1);
    t.base = base;
    t.levels = if base == 5 { 1 } else { 32 - t.w.max(t.h).leading_zeros() };
    t.defined = 0;
    let (gpu_id, levels, fmt) = (t.gpu, t.levels, if base == 5 { P_D24S8 } else { P_RGBA8 });
    gpu::record(|q| {
        if old != 0 {
            q.cmd(&[gpu::DESTROY_TEXTURE, old]);
        }
        q.cmd(&[gpu::CREATE_TEXTURE2, gpu_id, w.max(1), h.max(1), 1, levels, fmt, 0]);
    });
}

unsafe extern "C" fn glTexImage2D(target: u32, level: i32, internal: i32, w: i32, h: i32, _border: i32, format: u32, ty: u32, pixels: *const u8) {
    if target != GL_TEXTURE_2D || w < 0 || h < 0 {
        return;
    }
    with(|g, c| {
        let name = bound_texture(c);
        let src = unpack_ptr(g, c, pixels);
        let Some(t) = g.textures.get_mut(&name) else { return c.err(GL_INVALID_OPERATION) };
        let base = base_format(internal);
        if level == 0 {
            alloc_texture(t, w as u32, h as u32, base);
        } else if t.gpu == 0 || level as u32 >= t.levels {
            return;
        }
        t.defined |= 1 << level;
        if !src.is_null() && base != 5 && w > 0 && h > 0 {
            upload(t, level as u32, 0, 0, w as u32, h as u32, src, format, ty, c.unpack);
        }
    })
}

fn unpack_ptr(g: &Gl, c: &Ctx, p: *const u8) -> *const u8 {
    let _ = (g, c);
    p
}

unsafe fn upload(t: &mut Texture, level: u32, x: u32, y: u32, w: u32, h: u32, src: *const u8, format: u32, ty: u32, unpack: [i32; 4]) {
    let rgba = to_rgba(src, w, h, format, ty, t.base, unpack);
    let (id, gen) = (t.gpu, t.gen_mips && level == 0);
    gpu::record(|q| {
        let off = q.blob(&rgba);
        q.cmd(&[gpu::WRITE_TEXTURE2, id, level, 0, x, y, w, h, w * 4, off]);
        if gen {
            q.cmd(&[gpu::GEN_MIPS, id]);
        }
    });
}

unsafe extern "C" fn glTexSubImage2D(target: u32, level: i32, x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, pixels: *const u8) {
    if target != GL_TEXTURE_2D || w <= 0 || h <= 0 || pixels.is_null() {
        return;
    }
    with(|g, c| {
        let name = bound_texture(c);
        let Some(t) = g.textures.get_mut(&name) else { return };
        if t.gpu == 0 || t.base == 5 {
            return;
        }
        upload(t, level as u32, x.max(0) as u32, y.max(0) as u32, w as u32, h as u32, pixels, format, ty, c.unpack);
    })
}

fn tex_param(target: u32, p: u32, v: f32) {
    if target != GL_TEXTURE_2D {
        return;
    }
    with(|g, c| {
        let Some(t) = g.textures.get_mut(&bound_texture(c)) else { return };
        let v = v as u32;
        match p {
            0x2801 => t.min = v,
            0x2800 => t.mag = v,
            0x2802 => t.wrap_s = v,
            0x2803 => t.wrap_t = v,
            0x8191 => t.gen_mips = v != 0,
            _ => {}
        }
    })
}
extern "C" fn glTexParameteri(t: u32, p: u32, v: i32) {
    tex_param(t, p, v as f32)
}
extern "C" fn glTexParameterf(t: u32, p: u32, v: f32) {
    tex_param(t, p, v)
}
unsafe extern "C" fn glTexParameteriv(t: u32, p: u32, v: *const i32) {
    tex_param(t, p, *v as f32)
}
unsafe extern "C" fn glTexParameterfv(t: u32, p: u32, v: *const f32) {
    tex_param(t, p, *v)
}
unsafe extern "C" fn glGetTexParameteriv(_t: u32, p: u32, out: *mut i32) {
    *out = with(|g, c| {
        let Some(t) = g.textures.get(&bound_texture(c)) else { return 0 };
        (match p {
            0x2801 => t.min,
            0x2800 => t.mag,
            0x2802 => t.wrap_s,
            0x2803 => t.wrap_t,
            _ => 0,
        }) as i32
    })
}
unsafe extern "C" fn glGetTexLevelParameteriv(_t: u32, level: i32, p: u32, out: *mut i32) {
    *out = with(|g, c| {
        let Some(t) = g.textures.get(&bound_texture(c)) else { return 0 };
        match p {
            0x1000 => (t.w >> level).max(1) as i32,
            0x1001 => (t.h >> level).max(1) as i32,
            0x1003 => 0x8058,
            _ => 0,
        }
    })
}

fn tex_env(p: u32, v: &[f32]) {
    with(|_, c| match p {
        0x2200 => c.units[c.active].env = v[0] as u32,
        0x2201 if v.len() >= 4 => c.units[c.active].env_color.copy_from_slice(&v[..4]),
        _ => {}
    })
}
extern "C" fn glTexEnvi(_t: u32, p: u32, v: i32) {
    tex_env(p, &[v as f32])
}
extern "C" fn glTexEnvf(_t: u32, p: u32, v: f32) {
    tex_env(p, &[v])
}
unsafe extern "C" fn glTexEnviv(_t: u32, p: u32, v: *const i32) {
    if p == 0x2201 {
        let f: Vec<f32> = (0..4).map(|k| *v.add(k) as f32 / i32::MAX as f32).collect();
        tex_env(p, &f)
    } else {
        tex_env(p, &[*v as f32])
    }
}
unsafe extern "C" fn glTexEnvfv(_t: u32, p: u32, v: *const f32) {
    let n = if p == 0x2201 { 4 } else { 1 };
    tex_env(p, std::slice::from_raw_parts(v, n))
}

extern "C" fn glGenerateMipmap(target: u32) {
    if target != GL_TEXTURE_2D {
        return;
    }
    with(|g, c| {
        if let Some(t) = g.textures.get_mut(&bound_texture(c)) {
            if t.gpu != 0 && t.base != 5 {
                t.defined = (1u32 << t.levels.min(31)) - 1;
                let id = t.gpu;
                gpu::record(|q| q.cmd(&[gpu::GEN_MIPS, id]));
            }
        }
    })
}

/// The read framebuffer's color texture and level.
fn read_target(g: &mut Gl, c: &Ctx) -> Option<(u32, u32, u32, u32)> {
    let saved = c.fbo_draw;
    let mut tmp = Ctx::new();
    tmp.fbo_draw = c.fbo_read;
    tmp.draw = c.draw;
    tmp.w = c.w;
    tmp.h = c.h;
    let _ = saved;
    target(g, &tmp).map(|t| (t.0, t.1, t.3, t.4))
}

extern "C" fn glCopyTexSubImage2D(target: u32, level: i32, xo: i32, yo: i32, x: i32, y: i32, w: i32, h: i32) {
    if target != GL_TEXTURE_2D || w <= 0 || h <= 0 {
        return;
    }
    with(|g, c| {
        let Some((src, sl, sw, sh)) = read_target(g, c) else { return };
        let Some(t) = g.textures.get(&bound_texture(c)) else { return };
        if t.gpu == 0 || src == 0 {
            return;
        }
        let (x0, y0) = (x.max(0) as u32, y.max(0) as u32);
        let (w, h) = ((w as u32).min(sw.saturating_sub(x0)), (h as u32).min(sh.saturating_sub(y0)));
        let id = t.gpu;
        gpu::record(|q| q.cmd(&[gpu::COPY, src, sl, x0, y0, w, h, id, level as u32, xo.max(0) as u32, yo.max(0) as u32, w, h, 0]));
    })
}

extern "C" fn glCopyTexImage2D(target: u32, level: i32, internal: u32, x: i32, y: i32, w: i32, h: i32, _border: i32) {
    if target != GL_TEXTURE_2D || w <= 0 || h <= 0 {
        return;
    }
    with(|g, c| {
        if let Some(t) = g.textures.get_mut(&bound_texture(c)) {
            if level == 0 {
                alloc_texture(t, w as u32, h as u32, base_format(internal as i32));
            }
            t.defined |= 1 << level;
        }
    });
    glCopyTexSubImage2D(target, level, 0, 0, x, y, w, h)
}

unsafe extern "C" fn glReadPixels(x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, out: *mut u8) {
    if w <= 0 || h <= 0 || out.is_null() {
        return;
    }
    let req = with(|g, c| {
        let (src, sl, sw, sh) = read_target(g, c)?;
        if src == 0 {
            return None;
        }
        let (x0, y0) = (x.max(0) as u32, y.max(0) as u32);
        let (rw, rh) = ((w as u32).min(sw.saturating_sub(x0)), (h as u32).min(sh.saturating_sub(y0)));
        let buf = vec![0u8; (w * h * 4) as usize];
        let req = page_id();
        let p = buf.as_ptr() as u32;
        gpu::record(|q| q.cmd(&[gpu::READBACK, req, src, sl, x0, y0, rw, rh, p, w as u32 * 4]));
        Some((req, buf, c.pack))
    });
    let Some((req, buf, pack)) = req else { return };
    gpu::wait_readback(req);
    // RGBA8 rows (bottom first, as GL) to the caller's format
    let comps = match format {
        0x1906 | 0x1909 | 0x1903 | 0x1902 => 1,
        0x1907 | 0x80E0 => 3,
        _ => 4,
    };
    let bpp = if ty == GL_FLOAT { comps * 4 } else { comps };
    let pitch = (w as usize * bpp).next_multiple_of(pack[0].max(1) as usize);
    for row in 0..h as usize {
        for col in 0..w as usize {
            let s = &buf[(row * w as usize + col) * 4..][..4];
            let px: [u8; 4] = match format {
                0x80E1 | 0x80E0 => [s[2], s[1], s[0], s[3]],
                0x1906 => [s[3], 0, 0, 0],
                _ => [s[0], s[1], s[2], s[3]],
            };
            let d = out.add(row * pitch + col * bpp);
            for k in 0..comps {
                if ty == GL_FLOAT {
                    std::ptr::write_unaligned((d as *mut f32).add(k), px[k] as f32 / 255.0);
                } else {
                    *d.add(k) = px[k];
                }
            }
        }
    }
}

// ---- buffers ----

unsafe extern "C" fn glGenBuffers(n: i32, out: *mut u32) {
    gen(n, out, |g, id| {
        g.buffers.insert(id, Buffer::default());
    })
}
unsafe extern "C" fn glDeleteBuffers(n: i32, names: *const u32) {
    let mut g = lock();
    let g = g.as_mut().unwrap();
    for k in 0..n.max(0) as usize {
        g.buffers.remove(&*names.add(k));
    }
}
extern "C" fn glBindBuffer(target: u32, b: u32) {
    with(|g, c| {
        if b != 0 {
            g.buffers.entry(b).or_default();
        }
        match target {
            GL_ARRAY_BUFFER => c.array_buffer = b,
            GL_ELEMENT_ARRAY_BUFFER => c.element_buffer = b,
            GL_PIXEL_PACK_BUFFER | GL_PIXEL_UNPACK_BUFFER => {}
            _ => {}
        }
    })
}
extern "C" fn glIsBuffer(b: u32) -> u8 {
    lock().as_ref().unwrap().buffers.contains_key(&b) as u8
}
fn bound_buffer(c: &Ctx, target: u32) -> u32 {
    match target {
        GL_ARRAY_BUFFER => c.array_buffer,
        GL_ELEMENT_ARRAY_BUFFER => c.element_buffer,
        _ => 0,
    }
}
unsafe extern "C" fn glBufferData(target: u32, size: isize, data: *const u8, _usage: u32) {
    with(|g, c| {
        let Some(b) = g.buffers.get_mut(&bound_buffer(c, target)) else { return c.err(GL_INVALID_OPERATION) };
        b.data = if data.is_null() { vec![0; size.max(0) as usize] } else { std::slice::from_raw_parts(data, size.max(0) as usize).to_vec() };
    })
}
unsafe extern "C" fn glBufferSubData(target: u32, off: isize, size: isize, data: *const u8) {
    with(|g, c| {
        let Some(b) = g.buffers.get_mut(&bound_buffer(c, target)) else { return };
        let (o, n) = (off.max(0) as usize, size.max(0) as usize);
        if !data.is_null() && o + n <= b.data.len() {
            b.data[o..o + n].copy_from_slice(std::slice::from_raw_parts(data, n));
        } else {
            c.err(GL_INVALID_VALUE);
        }
    })
}
/// The buffer's own memory: draws read it as they are made, after the unmap.
extern "C" fn glMapBuffer(target: u32, _access: u32) -> *mut u8 {
    with(|g, c| g.buffers.get_mut(&bound_buffer(c, target)).map_or(0, |b| b.data.as_mut_ptr() as usize)) as *mut u8
}
extern "C" fn glUnmapBuffer(_target: u32) -> u8 {
    1
}

// ---- framebuffers ----

unsafe extern "C" fn glGenFramebuffers(n: i32, out: *mut u32) {
    gen(n, out, |g, id| {
        g.fbos.insert(id, Fbo::default());
    })
}
unsafe extern "C" fn glDeleteFramebuffers(n: i32, names: *const u32) {
    let ids: Vec<u32> = (0..n.max(0) as usize).map(|k| *names.add(k)).collect();
    with(|g, c| {
        for id in &ids {
            g.fbos.remove(id);
            if c.fbo_draw == *id {
                c.fbo_draw = 0;
            }
            if c.fbo_read == *id {
                c.fbo_read = 0;
            }
        }
    })
}
extern "C" fn glBindFramebuffer(target: u32, f: u32) {
    with(|g, c| {
        if f != 0 {
            g.fbos.entry(f).or_default();
        }
        if target == GL_FRAMEBUFFER || target == GL_DRAW_FRAMEBUFFER {
            c.fbo_draw = f;
        }
        if target == GL_FRAMEBUFFER || target == GL_READ_FRAMEBUFFER {
            c.fbo_read = f;
        }
    })
}
extern "C" fn glIsFramebuffer(f: u32) -> u8 {
    lock().as_ref().unwrap().fbos.contains_key(&f) as u8
}
fn attach(target: u32, point: u32, a: Att) {
    with(|g, c| {
        let id = if target == GL_READ_FRAMEBUFFER { c.fbo_read } else { c.fbo_draw };
        let Some(f) = g.fbos.get_mut(&id) else { return c.err(GL_INVALID_OPERATION) };
        match point {
            GL_COLOR_ATTACHMENT0 => f.color = a,
            GL_DEPTH_ATTACHMENT | GL_STENCIL_ATTACHMENT | GL_DEPTH_STENCIL_ATTACHMENT => f.depth = a,
            _ => {}
        }
    })
}
extern "C" fn glFramebufferTexture2D(target: u32, point: u32, _textarget: u32, t: u32, level: i32) {
    attach(target, point, if t == 0 { Att::None } else { Att::Tex(t, level.max(0) as u32) })
}
extern "C" fn glFramebufferTexture(target: u32, point: u32, t: u32, level: i32) {
    glFramebufferTexture2D(target, point, GL_TEXTURE_2D, t, level)
}
extern "C" fn glFramebufferRenderbuffer(target: u32, point: u32, _rbtarget: u32, rb: u32) {
    attach(target, point, if rb == 0 { Att::None } else { Att::Rb(rb) })
}
extern "C" fn glCheckFramebufferStatus(_target: u32) -> u32 {
    GL_FRAMEBUFFER_COMPLETE
}
unsafe extern "C" fn glGetFramebufferAttachmentParameteriv(target: u32, point: u32, p: u32, out: *mut i32) {
    *out = with(|g, c| {
        let id = if target == GL_READ_FRAMEBUFFER { c.fbo_read } else { c.fbo_draw };
        let Some(f) = g.fbos.get(&id) else { return 0 };
        let a = if point == GL_COLOR_ATTACHMENT0 { f.color } else { f.depth };
        match (p, a) {
            (0x8CD0, Att::Tex(..)) => 0x1702,
            (0x8CD0, Att::Rb(_)) => 0x8D41,
            (0x8CD1, Att::Tex(t, _)) | (0x8CD1, Att::Rb(t)) => t as i32,
            _ => 0,
        }
    })
}
unsafe extern "C" fn glGenRenderbuffers(n: i32, out: *mut u32) {
    gen(n, out, |g, id| {
        g.rbs.insert(id, Renderbuffer::default());
    })
}
unsafe extern "C" fn glDeleteRenderbuffers(n: i32, names: *const u32) {
    let mut g = lock();
    let g = g.as_mut().unwrap();
    for k in 0..n.max(0) as usize {
        if let Some(r) = g.rbs.remove(&*names.add(k)) {
            if r.gpu != 0 {
                gpu::record(|q| q.cmd(&[gpu::DESTROY_TEXTURE, r.gpu]));
            }
        }
    }
}
extern "C" fn glBindRenderbuffer(_target: u32, rb: u32) {
    with(|g, c| {
        if rb != 0 {
            g.rbs.entry(rb).or_default();
        }
        c.rb = rb;
    })
}
extern "C" fn glRenderbufferStorage(_target: u32, internal: u32, w: i32, h: i32) {
    with(|g, c| {
        let Some(r) = g.rbs.get_mut(&c.rb) else { return c.err(GL_INVALID_OPERATION) };
        let depth = base_format(internal as i32) == 5 || matches!(internal, 0x8D48 | 0x8D46 | 0x8D47 | 0x1901);
        let old = r.gpu;
        r.gpu = page_id();
        r.w = w.max(1) as u32;
        r.h = h.max(1) as u32;
        r.depth = depth;
        let (id, rw, rh) = (r.gpu, r.w, r.h);
        gpu::record(|q| {
            if old != 0 {
                q.cmd(&[gpu::DESTROY_TEXTURE, old]);
            }
            q.cmd(&[gpu::CREATE_TEXTURE2, id, rw, rh, 1, 1, if depth { P_D24S8 } else { P_RGBA8 }, 0]);
        });
    })
}
extern "C" fn glRenderbufferStorageMultisample(target: u32, _samples: i32, internal: u32, w: i32, h: i32) {
    glRenderbufferStorage(target, internal, w, h)
}

#[allow(dead_code)]
fn unused() -> u32 {
    GL_INVALID_ENUM
}

mod glsl;
