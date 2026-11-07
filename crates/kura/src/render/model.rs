//! A model of WebGPU behind host/vngpu.js, for native tests (cfg(test)): the `vngpu_*` entry
//! points of render/gpu.rs on an object table like the page's, with
//!
//! * the WebGPU validation rules of the calls kura makes (W3C WebGPU, "valid usage" of
//!   createTexture/createView/createBindGroup/createRenderPipeline, writeBuffer/writeTexture,
//!   copyTextureToTexture/copyTextureToBuffer, beginRenderPass, setViewport/setScissorRect,
//!   setBindGroup, draw/drawIndexed, submit, mapAsync), each broken rule an error in `errors`,
//!   and the page's own rule that a dropped table index is not used again;
//! * an abstract meaning of the commands: every texel holds a value (u64), queue writes take
//!   effect at once and encoded commands at submit, in order; a clear (loadOp or the clear
//!   shader of render/d3d.rs) makes the value of its color (in 8-bit UNORM formats: its bytes),
//!   a write the value of its bytes (the bytes themselves, up to 8: a readback gives back what
//!   was written, as on a GPU), a copy moves values, and any other draw gives every pixel it
//!   covers (viewport ∩ scissor ∩ the box `set_bounds` gave, the pass's attachment) a hash of everything it reads: pipeline,
//!   bound buffer bytes, sampled textures' values, vertex and index bytes, arguments, the
//!   pixel's position and its old color and depth. storeOp "discard" leaves zeros.
//!
//! Two runs that make the same observations (`Obs`) drew the same on any WebGPU, as far as
//! these meanings go: the model does not run shaders, so it cannot tell two different draws
//! that happen to give the same pixels, and it trusts the box a Direct3D 9 draw says it
//! covers.

#![allow(clippy::too_many_arguments)]

use std::cell::RefCell;
use std::collections::HashMap;

// ---- a small JSON reader (the descriptors render/ writes) ----

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn get(&self, k: &str) -> Option<&J> {
        match self {
            J::Obj(v) => v.iter().find(|e| e.0 == k).map(|e| &e.1),
            _ => None,
        }
    }
    fn num(&self, k: &str) -> Option<f64> {
        match self.get(k) {
            Some(J::Num(n)) => Some(*n),
            _ => None,
        }
    }
    fn u(&self, k: &str) -> Option<u32> {
        self.num(k).map(|n| n as u32)
    }
    fn s(&self, k: &str) -> Option<&str> {
        match self.get(k) {
            Some(J::Str(s)) => Some(s),
            _ => None,
        }
    }
    fn arr(&self, k: &str) -> &[J] {
        match self.get(k) {
            Some(J::Arr(a)) => a,
            _ => &[],
        }
    }
    fn b(&self, k: &str) -> Option<bool> {
        match self.get(k) {
            Some(J::Bool(b)) => Some(*b),
            _ => None,
        }
    }
}

pub fn parse_json(s: &str) -> Result<J, String> {
    let b = s.as_bytes();
    let mut i = 0;
    let v = value(b, &mut i)?;
    ws(b, &mut i);
    if i != b.len() {
        return Err(format!("JSON: trailing bytes at {i} of {s}"));
    }
    Ok(v)
}

fn ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && b[*i].is_ascii_whitespace() {
        *i += 1;
    }
}

fn value(b: &[u8], i: &mut usize) -> Result<J, String> {
    ws(b, i);
    let bad = |i: usize| format!("JSON: bad value at {i} of {}", String::from_utf8_lossy(b));
    match b.get(*i) {
        Some(b'{') => {
            *i += 1;
            let mut v = Vec::new();
            ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Ok(J::Obj(v));
            }
            loop {
                ws(b, i);
                let J::Str(k) = value(b, i)? else {
                    return Err(bad(*i));
                };
                ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(bad(*i));
                }
                *i += 1;
                let x = value(b, i)?;
                if v.iter().any(|e: &(String, J)| e.0 == k) {
                    return Err(format!("JSON: key {k} twice"));
                }
                v.push((k, x));
                ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => {
                        *i += 1;
                        return Ok(J::Obj(v));
                    }
                    _ => return Err(bad(*i)),
                }
            }
        }
        Some(b'[') => {
            *i += 1;
            let mut v = Vec::new();
            ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Ok(J::Arr(v));
            }
            loop {
                v.push(value(b, i)?);
                ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b']') => {
                        *i += 1;
                        return Ok(J::Arr(v));
                    }
                    _ => return Err(bad(*i)),
                }
            }
        }
        Some(b'"') => {
            *i += 1;
            let st = *i;
            while *i < b.len() && b[*i] != b'"' {
                if b[*i] == b'\\' {
                    return Err("JSON: escapes are not written by render/".into());
                }
                *i += 1;
            }
            if *i >= b.len() {
                return Err(bad(st));
            }
            *i += 1;
            Ok(J::Str(String::from_utf8_lossy(&b[st..*i - 1]).into_owned()))
        }
        Some(b't') if b[*i..].starts_with(b"true") => {
            *i += 4;
            Ok(J::Bool(true))
        }
        Some(b'f') if b[*i..].starts_with(b"false") => {
            *i += 5;
            Ok(J::Bool(false))
        }
        Some(b'n') if b[*i..].starts_with(b"null") => {
            *i += 4;
            Ok(J::Null)
        }
        Some(_) => {
            let st = *i;
            while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *i += 1;
            }
            let t = std::str::from_utf8(&b[st..*i]).unwrap_or("");
            // (JSON numbers: no NaN, no inf; what num() in render/ writes)
            t.parse::<f64>()
                .ok()
                .filter(|f| f.is_finite() && !t.is_empty())
                .map(J::Num)
                .ok_or_else(|| bad(st))
        }
        None => Err(bad(*i)),
    }
}

// ---- formats ----

/// (bytes per texel, renderable, blendable, filterable, sample kind: f float, u uint, s sint, d depth)
fn format_info(f: &str) -> Option<(u32, bool, bool, bool, char)> {
    Some(match f {
        "r8unorm" => (1, true, true, true, 'f'),
        "rg8unorm" => (2, true, true, true, 'f'),
        "rgba8unorm" | "rgba8unorm-srgb" | "bgra8unorm" | "bgra8unorm-srgb" | "rgb10a2unorm" => {
            (4, true, true, true, 'f')
        }
        "rgba8snorm" => (4, false, false, true, 'f'),
        "r16float" => (2, true, true, true, 'f'),
        "rg16float" => (4, true, true, true, 'f'),
        "rgba16float" => (8, true, true, true, 'f'),
        // (renderable with the rg11b10ufloat-renderable feature the page asks for)
        "rg11b10ufloat" => (4, true, true, true, 'f'),
        "r32float" => (4, true, false, false, 'f'),
        "rg32float" => (8, true, false, false, 'f'),
        "rgba32float" => (16, true, false, false, 'f'),
        "rgba8uint" | "r32uint" | "rgb10a2uint" | "rg16uint" => (4, true, false, false, 'u'),
        "rgba8sint" | "r32sint" | "rg16sint" => (4, true, false, false, 's'),
        "rgba16uint" => (8, true, false, false, 'u'),
        "rgba16sint" => (8, true, false, false, 's'),
        "rg8uint" | "r16uint" => (2, true, false, false, 'u'),
        "rg8sint" | "r16sint" => (2, true, false, false, 's'),
        "r8uint" => (1, true, false, false, 'u'),
        "r8sint" => (1, true, false, false, 's'),
        "rg32uint" => (8, true, false, false, 'u'),
        "rg32sint" => (8, true, false, false, 's'),
        "rgba32uint" => (16, true, false, false, 'u'),
        "rgba32sint" => (16, true, false, false, 's'),
        "r8snorm" => (1, false, false, true, 'f'),
        "rg8snorm" => (2, false, false, true, 'f'),
        "rgb9e5ufloat" => (4, false, false, true, 'f'),
        "depth16unorm" => (2, true, false, false, 'd'),
        "depth32float" => (4, true, false, false, 'd'),
        "depth24plus" | "depth24plus-stencil8" => (4, true, false, false, 'd'),
        _ => return None,
    })
}

fn has_stencil(f: &str) -> bool {
    f == "depth24plus-stencil8" || f == "stencil8" || f == "depth32float-stencil8"
}

fn vertex_format_size(f: &str) -> Option<u32> {
    Some(match f {
        "float32" | "uint32" | "sint32" | "unorm8x4" | "snorm8x4" | "uint8x4" | "sint8x4"
        | "unorm16x2" | "snorm16x2" | "uint16x2" | "sint16x2" | "float16x2" | "unorm10-10-10-2"
        | "unorm8x4-bgra" => 4,
        "float32x2" | "uint32x2" | "sint32x2" | "unorm16x4" | "snorm16x4" | "uint16x4"
        | "sint16x4" | "float16x4" => 8,
        "float32x3" | "uint32x3" | "sint32x3" => 12,
        "float32x4" | "uint32x4" | "sint32x4" => 16,
        "unorm8x2" | "uint8x2" | "snorm8x2" | "sint8x2" => 2,
        _ => return None,
    })
}

// ---- hashing ----

fn mix(h: u64, v: u64) -> u64 {
    let mut x = h ^ v
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(h << 6)
        .wrapping_add(h >> 2);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn hash_bytes(h: u64, b: &[u8]) -> u64 {
    let mut h = mix(h, b.len() as u64);
    for c in b.chunks(8) {
        let mut w = [0u8; 8];
        w[..c.len()].copy_from_slice(c);
        h = mix(h, u64::from_le_bytes(w));
    }
    h
}

fn hash_str(s: &str) -> u64 {
    hash_bytes(0x51, s.as_bytes())
}

/// A descriptor's meaning, hashed: not the table indexes of the objects it names (they differ
/// between runs that make other objects), which the caller hashes by what they hold.
fn hash_desc(j: &J) -> u64 {
    match j {
        J::Null => 1,
        J::Bool(b) => 2 + *b as u64,
        J::Num(n) => mix(4, n.to_bits()),
        J::Str(s) => mix(5, hash_str(s)),
        J::Arr(a) => a.iter().fold(6, |h, x| mix(h, hash_desc(x))),
        J::Obj(o) => o
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "module" | "layout"))
            .fold(7, |h, (k, v)| mix(mix(h, hash_str(k)), hash_desc(v))),
    }
}

/// The value a clear to `color` (and the clear shader drawing it) leaves.
fn clear_value(color: [f32; 4]) -> u64 {
    let mut h = 0xc1ea;
    for c in color {
        // (-0 and 0: the same clear)
        h = mix(h, if c == 0.0 { 0 } else { c.to_bits() as u64 });
    }
    h
}

/// What a clear leaves in a texel of `format`: 8-bit UNORM formats hold the bytes (as writes
/// do: a readback gives them back and uploading those bytes gives the same value), others the
/// value of the color.
pub(crate) fn clear_texel(format: &str, c: [f32; 4]) -> u64 {
    let u = |x: f32| {
        if x.is_nan() {
            0
        } else {
            (x.clamp(0.0, 1.0) * 255.0).round() as u64
        }
    };
    match format {
        "rgba8unorm" => u(c[0]) | u(c[1]) << 8 | u(c[2]) << 16 | u(c[3]) << 24,
        "bgra8unorm" => u(c[2]) | u(c[1]) << 8 | u(c[0]) << 16 | u(c[3]) << 24,
        _ => clear_value(c),
    }
}

/// What a write of a texel's bytes leaves: up to 8 bytes the bytes themselves (so a readback
/// gives them back, as on a GPU), more a hash of them.
fn write_texel(b: &[u8]) -> u64 {
    if b.len() <= 8 {
        let mut w = [0u8; 8];
        w[..b.len()].copy_from_slice(b);
        u64::from_le_bytes(w)
    } else {
        hash_bytes(0x77, b)
    }
}

fn depth_value(z: f32) -> u64 {
    mix(0xde97, z.clamp(0.0, 1.0).to_bits() as u64)
}

fn stencil_value(s: u32) -> u64 {
    mix(0x57e, (s & 0xff) as u64)
}

// ---- objects ----

#[derive(Clone)]
pub struct Texture {
    pub w: u32,
    pub h: u32,
    pub layers: u32,
    pub mips: u32,
    pub format: String,
    pub usage: u32,
    pub dim: String,
    pub destroyed: bool,
    /// per (mip, layer): w*h values (depth formats: the depth values)
    pub texels: Vec<Vec<u64>>,
    /// depth-stencil formats: the stencil values
    pub stencil: Vec<Vec<u64>>,
}

impl Texture {
    fn size(&self, mip: u32) -> (u32, u32, u32) {
        let d = if self.dim == "3d" {
            (self.layers >> mip).max(1)
        } else {
            self.layers
        };
        ((self.w >> mip).max(1), (self.h >> mip).max(1), d)
    }
    fn sub(&self, mip: u32, layer: u32) -> usize {
        (mip * self.layers + layer) as usize
    }
}

#[derive(Clone)]
pub struct View {
    pub tex: usize,
    pub mip: u32,
    pub mips: u32,
    pub layer: u32,
    pub layers: u32,
    pub dim: String,
    pub aspect: String,
    pub format: String,
}

#[derive(Clone)]
pub struct Buffer {
    pub size: u32,
    pub usage: u32,
    pub data: Vec<u8>,
    pub destroyed: bool,
    pub mapped: bool,
}

#[derive(Clone, Debug)]
pub struct LayoutEntry {
    pub binding: u32,
    /// 'b' buffer (uniform), 't' texture, 's' sampler
    pub kind: char,
    pub dynamic: bool,
    /// texture: sample type ("float", "unfilterable-float", "depth", "uint", "sint"); sampler: type
    pub ty: String,
    pub view_dim: String,
}

#[derive(Clone)]
pub struct Pipeline {
    pub hash: u64,
    pub groups: Vec<usize>,
    /// format, write mask, whether the old color goes in (blending)
    pub targets: Vec<(String, u32, bool)>,
    pub depth: Option<(String, bool, String)>,
    pub buffers: Vec<u32>,
    /// per vertex buffer: its attributes (offset, size, shader location)
    pub attrs: Vec<Vec<(u32, u32, u32)>>,
    /// per vertex buffer: stepMode "instance"
    pub per_instance: Vec<bool>,
    pub topology: String,
    /// "uint16" / "uint32": an index of all ones restarts the strip
    pub strip_format: Option<String>,
    pub clear_shader: bool,
}

/// A vertex as the shader gets it: (location, the bytes of that attribute; empty when the
/// fetch was outside the buffer), by location.
pub type Vertex = Vec<(u32, Vec<u8>)>;

#[derive(Clone)]
pub enum Entry {
    Buffer(usize, u32, u32),
    View(usize),
    Sampler(usize),
}

#[derive(Clone)]
pub enum Obj {
    Texture(Texture),
    View(View),
    Buffer(Buffer),
    Sampler(u64),
    Shader(u64, Shader),
    Layout(Vec<LayoutEntry>),
    PipelineLayout(Vec<usize>),
    Pipeline(Pipeline),
    Group(usize, Vec<(u32, Entry)>),
}

/// What a shader module declares: its entry points, and per entry point the bindings it uses
/// (group, binding, kind, texture sample type / view dimension, dynamic unknown)
#[derive(Clone, Default)]
pub struct Shader {
    pub entries: Vec<(
        String,
        naga::ShaderStage,
        Vec<(u32, u32, char, String, String)>,
    )>,
    pub clear: bool,
}

#[derive(Clone)]
struct Attachment {
    view: usize,
    load_clear: Option<u64>,
    store: bool,
    stencil_clear: Option<u64>,
    stencil_store: bool,
}

#[derive(Clone, Default)]
struct DrawState {
    pipeline: Option<usize>,
    groups: Vec<Option<(usize, Vec<u32>)>>,
    vbufs: Vec<Option<(usize, u32)>>,
    ibuf: Option<(usize, bool, u32)>,
    viewport: [f32; 6],
    scissor: [u32; 4],
    blend: [f64; 4],
    stencil_ref: u32,
}

#[derive(Clone)]
struct Draw {
    st: DrawState,
    args: [u32; 5],
    indexed: bool,
    bounds: Option<[u32; 4]>,
}

#[derive(Clone)]
enum Cmd {
    Copy(usize, u32, [u32; 3], usize, u32, [u32; 3], [u32; 3]),
    ToBuffer(usize, u32, u32, u32, usize, u32, u32, u32),
    Pass(Vec<Attachment>, Option<Attachment>, Vec<Draw>),
}

/// What the page would show or hand back.
#[derive(Clone, Debug, PartialEq)]
pub enum Obs {
    /// a readback's bytes
    Read(Vec<u8>),
    /// values of a texture (by the test's name) at a moment the test picks
    Frame(String, Vec<u64>),
}

#[derive(Default)]
pub struct Model {
    table: Vec<Option<usize>>,
    free: Vec<u32>,
    pub objs: Vec<Obj>,
    pub errors: Vec<String>,
    enc: Vec<Cmd>,
    pass: Option<(
        Vec<Attachment>,
        Option<Attachment>,
        Vec<Draw>,
        DrawState,
        (u32, u32),
    )>,
    bounds: Option<[u32; 4]>,
    /// buffers to read, with where (as read_buffer gave them)
    pub pending_maps: Vec<(usize, u32, usize, u32)>,
    pub obs: Vec<Obs>,
    /// calls made, by name (for coverage)
    pub calls: HashMap<&'static str, u64>,
    /// Some: each draw submitted adds the primitives it assembles (its vertices in order),
    /// for tests of what draws read
    pub prims: Option<Vec<Vec<Vec<Vertex>>>>,
}

thread_local! {
    pub static M: RefCell<Model> = RefCell::new(Model { table: vec![None], ..Default::default() });
}

/// A fresh model for this thread.
pub fn reset() {
    M.with(|m| {
        *m.borrow_mut() = Model {
            table: vec![None],
            ..Default::default()
        }
    });
}

pub fn with<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    M.with(|m| f(&mut m.borrow_mut()))
}

/// The box (x0 y0 x1 y1) the next draws cover at most (render/d3d.rs: a Direct3D 9 draw's
/// vertices in pixels); None: all of the viewport.
pub fn set_bounds(b: Option<[u32; 4]>) {
    with(|m| m.bounds = b.filter(|b| b[0] != u32::MAX));
}

impl Model {
    fn err(&mut self, s: String) {
        self.errors.push(s);
    }

    fn put(&mut self, o: Obj) -> u32 {
        self.objs.push(o);
        let a = self.objs.len() - 1;
        match self.free.pop() {
            Some(i) => {
                self.table[i as usize] = Some(a);
                i
            }
            None => {
                self.table.push(Some(a));
                (self.table.len() - 1) as u32
            }
        }
    }

    /// The object behind table index `i` (an error when it was dropped or never made).
    pub fn at(&mut self, i: u32, what: &str) -> Option<usize> {
        match self.table.get(i as usize).copied().flatten() {
            Some(a) => Some(a),
            None => {
                self.err(format!(
                    "{what}: object {i} is not in the table (dropped or never made)"
                ));
                None
            }
        }
    }

    pub fn tex(&self, a: usize) -> &Texture {
        match &self.objs[a] {
            Obj::Texture(t) => t,
            _ => panic!("not a texture"),
        }
    }

    fn texture_of(&mut self, i: u32, what: &str) -> Option<usize> {
        let a = self.at(i, what)?;
        match &self.objs[a] {
            Obj::Texture(t) if t.destroyed => {
                self.err(format!("{what}: texture {i} is destroyed"));
                None
            }
            Obj::Texture(_) => Some(a),
            _ => {
                self.err(format!("{what}: object {i} is not a texture"));
                None
            }
        }
    }

    fn buffer_of(&mut self, i: u32, what: &str) -> Option<usize> {
        let a = self.at(i, what)?;
        match &self.objs[a] {
            Obj::Buffer(b) if b.destroyed => {
                self.err(format!("{what}: buffer {i} is destroyed"));
                None
            }
            Obj::Buffer(_) => Some(a),
            _ => {
                self.err(format!("{what}: object {i} is not a buffer"));
                None
            }
        }
    }

    fn json(&mut self, s: &str, what: &str) -> Option<J> {
        match parse_json(s) {
            Ok(j) => Some(j),
            Err(e) => {
                self.err(format!("{what}: {e}"));
                None
            }
        }
    }

    fn view_of(&self, a: usize) -> Option<&View> {
        match &self.objs[a] {
            Obj::View(v) => Some(v),
            _ => None,
        }
    }

    // ---- creation ----

    pub fn texture(&mut self, s: &str) -> u32 {
        let Some(j) = self.json(s, "createTexture") else {
            return self.put(Obj::Sampler(0));
        };
        let size: Vec<u32> = j
            .arr("size")
            .iter()
            .map(|n| if let J::Num(n) = n { *n as u32 } else { 0 })
            .collect();
        let (w, h, d) = (
            size.first().copied().unwrap_or(0),
            size.get(1).copied().unwrap_or(1),
            size.get(2).copied().unwrap_or(1),
        );
        let format = j.s("format").unwrap_or("").to_string();
        let usage = j.u("usage").unwrap_or(0);
        let mips = j.u("mipLevelCount").unwrap_or(1);
        let dim = j.s("dimension").unwrap_or("2d").to_string();
        let info = format_info(&format);
        if info.is_none() {
            self.err(format!("createTexture: format {format}"));
        }
        if w == 0 || h == 0 || d == 0 || w > 8192 || h > 8192 || d > 2048 {
            self.err(format!("createTexture: size {w}x{h}x{d}"));
        }
        if usage == 0 || usage & !0x1f != 0 {
            self.err(format!("createTexture: usage {usage:#x}"));
        }
        let max_dim = match dim.as_str() {
            "1d" => w,
            "3d" => w.max(h).max(d),
            _ => w.max(h),
        };
        if mips == 0 || mips > 32 - max_dim.max(1).leading_zeros() {
            self.err(format!("createTexture: {mips} mip levels for {w}x{h}x{d}"));
        }
        if let Some((_, renderable, ..)) = info {
            if usage & 0x10 != 0 && (!renderable || dim != "2d") {
                self.err(format!(
                    "createTexture: {format} {dim} as a render attachment"
                ));
            }
            if dim == "1d" && (h != 1 || mips != 1) {
                self.err("createTexture: a 1d texture with height or mips".into());
            }
        }
        let layers = d;
        let n = (w.min(4096) * h.min(4096)) as usize;
        let mut texels = Vec::new();
        for m in 0..mips.min(32) {
            for _ in 0..layers {
                texels.push(vec![
                    0u64;
                    (((w >> m).max(1)) * ((h >> m).max(1))).min(n as u32)
                        as usize
                ]);
            }
        }
        let stencil = if has_stencil(&format) {
            texels.clone()
        } else {
            Vec::new()
        };
        self.put(Obj::Texture(Texture {
            w,
            h,
            layers,
            mips,
            format,
            usage,
            dim,
            destroyed: false,
            texels,
            stencil,
        }))
    }

    pub fn view(&mut self, tex: u32, s: &str) -> u32 {
        let Some(t) = self.texture_of(tex, "createView") else {
            return self.put(Obj::Sampler(0));
        };
        let j = if s.is_empty() {
            J::Obj(Vec::new())
        } else {
            self.json(s, "createView").unwrap_or(J::Null)
        };
        let tx = self.tex(t).clone();
        let mip = j.u("baseMipLevel").unwrap_or(0);
        let mips = j.u("mipLevelCount").unwrap_or(tx.mips.saturating_sub(mip));
        let default_dim = if tx.dim == "2d" && tx.layers > 1 {
            "2d-array"
        } else {
            &tx.dim
        };
        let dim = j.s("dimension").unwrap_or(default_dim).to_string();
        let layer = j.u("baseArrayLayer").unwrap_or(0);
        let tl = if tx.dim == "3d" { 1 } else { tx.layers };
        let layers = j.u("arrayLayerCount").unwrap_or(match dim.as_str() {
            "2d" | "1d" | "3d" => 1,
            "cube" => 6,
            _ => tl.saturating_sub(layer),
        });
        let aspect = j.s("aspect").unwrap_or("all").to_string();
        let format = j.s("format").unwrap_or(&tx.format).to_string();
        if mips == 0 || mip + mips > tx.mips {
            self.err(format!("createView: mips {mip}+{mips} of {}", tx.mips));
        }
        if layers == 0 || layer + layers > tl {
            self.err(format!("createView: layers {layer}+{layers} of {tl}"));
        }
        let ok_dim = match tx.dim.as_str() {
            "1d" => dim == "1d",
            "3d" => dim == "3d",
            _ => matches!(dim.as_str(), "2d" | "2d-array" | "cube" | "cube-array"),
        };
        if !ok_dim {
            self.err(format!("createView: a {dim} view of a {} texture", tx.dim));
        }
        if (dim == "2d" && layers != 1)
            || (dim == "cube" && layers != 6)
            || (dim == "cube-array" && layers % 6 != 0)
        {
            self.err(format!("createView: {dim} with {layers} layers"));
        }
        if matches!(dim.as_str(), "cube" | "cube-array") && tx.w != tx.h {
            self.err("createView: a cube view of a texture that is not square".into());
        }
        let base = |f: &str| f.trim_end_matches("-srgb").to_string();
        if format != tx.format && base(&format) != base(&tx.format) {
            self.err(format!(
                "createView: format {format} of a {} texture",
                tx.format
            ));
        }
        if aspect != "all"
            && !(tx.format.starts_with("depth")
                && matches!(aspect.as_str(), "depth-only" | "stencil-only"))
        {
            self.err(format!("createView: aspect {aspect} of {}", tx.format));
        }
        self.put(Obj::View(View {
            tex: t,
            mip,
            mips,
            layer,
            layers,
            dim,
            aspect,
            format,
        }))
    }

    pub fn buffer(&mut self, size: u32, usage: u32) -> u32 {
        if usage == 0 || (usage & 1 != 0 && usage & !(1 | 8) != 0) {
            self.err(format!(
                "createBuffer: usage {usage:#x} (MAP_READ goes only with COPY_DST)"
            ));
        }
        if size > 1 << 28 {
            self.err(format!("createBuffer: size {size}"));
        }
        self.put(Obj::Buffer(Buffer {
            size,
            usage,
            data: vec![0; size.min(1 << 28) as usize],
            destroyed: false,
            mapped: false,
        }))
    }

    pub fn sampler(&mut self, s: &str) -> u32 {
        let j = self.json(s, "createSampler").unwrap_or(J::Null);
        for k in ["addressModeU", "addressModeV", "addressModeW"] {
            if let Some(m) = j.s(k) {
                if !matches!(m, "repeat" | "mirror-repeat" | "clamp-to-edge") {
                    self.err(format!("createSampler: {k} {m}"));
                }
            }
        }
        for k in ["magFilter", "minFilter", "mipmapFilter"] {
            if let Some(m) = j.s(k) {
                if !matches!(m, "nearest" | "linear") {
                    self.err(format!("createSampler: {k} {m}"));
                }
            }
        }
        let (lo, hi) = (
            j.num("lodMinClamp").unwrap_or(0.0),
            j.num("lodMaxClamp").unwrap_or(32.0),
        );
        if lo < 0.0 || hi < lo {
            self.err(format!("createSampler: lod clamps {lo} {hi}"));
        }
        if let Some(a) = j.u("maxAnisotropy") {
            if a > 1
                && (j.s("magFilter") != Some("linear")
                    || j.s("minFilter") != Some("linear")
                    || j.s("mipmapFilter") != Some("linear"))
            {
                self.err("createSampler: anisotropy without linear filters".into());
            }
        }
        let h = hash_str(s);
        // (a comparison sampler binds as "comparison", a filtering one as "filtering")
        let kind = if j.get("compare").is_some() {
            2
        } else if j.s("magFilter") == Some("linear")
            || j.s("minFilter") == Some("linear")
            || j.s("mipmapFilter") == Some("linear")
        {
            1
        } else {
            0
        };
        self.put(Obj::Sampler(h << 2 | kind))
    }

    pub fn shader(&mut self, code: &str) -> u32 {
        let mut sh = Shader {
            clear: code == super::d3d::CLEAR_SHADER,
            ..Default::default()
        };
        match naga::front::wgsl::parse_str(code) {
            Err(e) => self.err(format!("createShaderModule: {}", e.emit_to_string(code))),
            Ok(m) => {
                match naga::valid::Validator::new(
                    naga::valid::ValidationFlags::all(),
                    naga::valid::Capabilities::empty(),
                )
                .validate(&m)
                {
                    Err(e) => self.err(format!("createShaderModule: {e:?}")),
                    Ok(_) => {
                        for ep in &m.entry_points {
                            let mut used = Vec::new();
                            for (h, g) in m.global_variables.iter() {
                                let Some(b) = &g.binding else { continue };
                                // (globals the entry point's function or what it calls touches)
                                if !uses_global(&m, &ep.function, h) {
                                    continue;
                                }
                                let (kind, ty, vd) = match &m.types[g.ty].inner {
                                    naga::TypeInner::Image {
                                        dim,
                                        arrayed,
                                        class,
                                    } => {
                                        let st = match class {
                                            naga::ImageClass::Depth { .. } => "depth",
                                            naga::ImageClass::Sampled {
                                                kind: naga::ScalarKind::Uint,
                                                ..
                                            } => "uint",
                                            naga::ImageClass::Sampled {
                                                kind: naga::ScalarKind::Sint,
                                                ..
                                            } => "sint",
                                            _ => "float",
                                        };
                                        let vd = match (dim, arrayed) {
                                            (naga::ImageDimension::D1, _) => "1d",
                                            (naga::ImageDimension::D2, false) => "2d",
                                            (naga::ImageDimension::D2, true) => "2d-array",
                                            (naga::ImageDimension::D3, _) => "3d",
                                            (naga::ImageDimension::Cube, false) => "cube",
                                            (naga::ImageDimension::Cube, true) => "cube-array",
                                        };
                                        ('t', st.to_string(), vd.to_string())
                                    }
                                    naga::TypeInner::Sampler { comparison } => (
                                        's',
                                        if *comparison {
                                            "comparison"
                                        } else {
                                            "filtering"
                                        }
                                        .to_string(),
                                        String::new(),
                                    ),
                                    _ => ('b', "uniform".to_string(), String::new()),
                                };
                                used.push((b.group, b.binding, kind, ty, vd));
                            }
                            sh.entries.push((ep.name.clone(), ep.stage, used));
                        }
                    }
                }
            }
        }
        self.put(Obj::Shader(hash_str(code), sh))
    }

    pub fn bind_group_layout(&mut self, s: &str) -> u32 {
        let j = self.json(s, "createBindGroupLayout").unwrap_or(J::Null);
        let mut v = Vec::new();
        for e in j.arr("entries") {
            let binding = e.u("binding").unwrap_or(u32::MAX);
            let (kind, dynamic, ty, view_dim) = if let Some(b) = e.get("buffer") {
                (
                    'b',
                    b.b("hasDynamicOffset").unwrap_or(false),
                    b.s("type").unwrap_or("uniform").to_string(),
                    String::new(),
                )
            } else if let Some(t) = e.get("texture") {
                (
                    't',
                    false,
                    t.s("sampleType").unwrap_or("float").to_string(),
                    t.s("viewDimension").unwrap_or("2d").to_string(),
                )
            } else if let Some(t) = e.get("sampler") {
                (
                    's',
                    false,
                    t.s("type").unwrap_or("filtering").to_string(),
                    String::new(),
                )
            } else {
                self.err(format!(
                    "createBindGroupLayout: entry {binding} binds nothing"
                ));
                ('?', false, String::new(), String::new())
            };
            if v.iter().any(|x: &LayoutEntry| x.binding == binding) {
                self.err(format!("createBindGroupLayout: binding {binding} twice"));
            }
            v.push(LayoutEntry {
                binding,
                kind,
                dynamic,
                ty,
                view_dim,
            });
        }
        self.put(Obj::Layout(v))
    }

    pub fn pipeline_layout(&mut self, s: &str) -> u32 {
        let j = self.json(s, "createPipelineLayout").unwrap_or(J::Null);
        let mut g = Vec::new();
        for l in j.arr("bindGroupLayouts").to_vec() {
            if let J::Num(n) = l {
                if let Some(a) = self.at(n as u32, "createPipelineLayout") {
                    if matches!(self.objs[a], Obj::Layout(_)) {
                        g.push(a);
                        continue;
                    }
                }
            }
            self.err("createPipelineLayout: not a bind group layout".into());
        }
        if g.len() > 4 {
            self.err("createPipelineLayout: more than 4 bind groups".into());
        }
        self.put(Obj::PipelineLayout(g))
    }

    fn module(
        &mut self,
        j: Option<&J>,
        stage: naga::ShaderStage,
        what: &str,
    ) -> Option<(u64, Shader, Vec<(u32, u32, char, String, String)>)> {
        let j = j?;
        let m = j.u("module")?;
        let a = self.at(m, what)?;
        let Obj::Shader(h, sh) = self.objs[a].clone() else {
            self.err(format!("{what}: not a shader module"));
            return None;
        };
        let ep = j.s("entryPoint").unwrap_or("");
        let e = sh.entries.iter().find(|e| e.0 == ep && e.1 == stage);
        let Some(e) = e else {
            self.err(format!("{what}: no {stage:?} entry point {ep}"));
            return None;
        };
        let used = e.2.clone();
        Some((h, sh, used))
    }

    pub fn render_pipeline(&mut self, s: &str) -> u32 {
        let what = "createRenderPipeline";
        let Some(j) = self.json(s, what) else {
            return self.put(Obj::Sampler(0));
        };
        let vs = self.module(j.get("vertex"), naga::ShaderStage::Vertex, what);
        let fs = self.module(j.get("fragment"), naga::ShaderStage::Fragment, what);
        let mut used = Vec::new();
        let mut h = hash_desc(&j);
        let mut clear = false;
        if let Some((mh, sh, u)) = &vs {
            h = mix(h, *mh);
            clear = sh.clear;
            used.extend(u.iter().cloned());
        }
        if let Some((mh, _, u)) = &fs {
            h = mix(h, *mh);
            used.extend(u.iter().cloned());
        }
        // the layout: explicit, or what the shaders use ("auto")
        let groups: Vec<usize> = match j.get("layout") {
            Some(J::Str(a)) if a == "auto" => {
                let mut gs = Vec::new();
                let max = used.iter().map(|u| u.0 + 1).max().unwrap_or(0);
                for g in 0..max {
                    let mut es: Vec<LayoutEntry> = Vec::new();
                    for u in used.iter().filter(|u| u.0 == g) {
                        if es.iter().any(|e| e.binding == u.1) {
                            continue;
                        }
                        es.push(LayoutEntry {
                            binding: u.1,
                            kind: u.2,
                            dynamic: false,
                            ty: if u.2 == 't' && u.3 == "float" {
                                "float".into()
                            } else {
                                u.3.clone()
                            },
                            view_dim: u.4.clone(),
                        });
                    }
                    self.objs.push(Obj::Layout(es));
                    gs.push(self.objs.len() - 1);
                }
                gs
            }
            Some(J::Num(n)) => {
                let n = *n as u32;
                match self.at(n, what).map(|a| self.objs[a].clone()) {
                    Some(Obj::PipelineLayout(g)) => g,
                    _ => {
                        self.err(format!("{what}: layout {n} is not a pipeline layout"));
                        Vec::new()
                    }
                }
            }
            _ => {
                self.err(format!("{what}: no layout"));
                Vec::new()
            }
        };
        // every binding the shaders use is in the layout, of its kind
        for u in &used {
            let e = groups.get(u.0 as usize).and_then(|&g| match &self.objs[g] {
                Obj::Layout(es) => es.iter().find(|e| e.binding == u.1).cloned(),
                _ => None,
            });
            match e {
                None => self.err(format!(
                    "{what}: the shaders use @group({}) @binding({}), the layout has no such entry",
                    u.0, u.1
                )),
                Some(e) => {
                    let ok = e.kind == u.2
                        && match u.2 {
                            't' => {
                                (e.ty == u.3
                                    || (u.3 == "float" && e.ty == "unfilterable-float")
                                    || (u.3 == "depth" && e.ty == "depth"))
                                    && e.view_dim == u.4
                            }
                            's' => (u.3 == "comparison") == (e.ty == "comparison"),
                            _ => true,
                        };
                    if !ok {
                        self.err(format!("{what}: @group({}) @binding({}) is a {} {} {} in the shader, {} {} {} in the layout", u.0, u.1, u.2, u.3, u.4, e.kind, e.ty, e.view_dim));
                    }
                }
            }
        }
        // vertex buffers
        let mut buffers = Vec::new();
        let mut attrs = Vec::new();
        let mut per_instance = Vec::new();
        let mut locs = Vec::new();
        if let Some(v) = j.get("vertex") {
            for b in v.arr("buffers") {
                let mut list = Vec::new();
                let stride = b.u("arrayStride").unwrap_or(0);
                if stride % 4 != 0 || stride > 2048 {
                    self.err(format!("{what}: arrayStride {stride}"));
                }
                for a in b.arr("attributes") {
                    let f = a.s("format").unwrap_or("");
                    let off = a.u("offset").unwrap_or(0);
                    let loc = a.u("shaderLocation").unwrap_or(99);
                    match vertex_format_size(f) {
                        None => self.err(format!("{what}: vertex format {f}")),
                        Some(sz) => {
                            if off % sz.min(4) != 0
                                || (stride != 0 && off + sz > stride)
                                || (stride == 0 && off + sz > 2048)
                            {
                                self.err(format!(
                                    "{what}: attribute {f} at {off} in a stride of {stride}"
                                ));
                            }
                        }
                    }
                    if loc >= 16 || locs.contains(&loc) {
                        self.err(format!("{what}: shader location {loc}"));
                    }
                    locs.push(loc);
                    list.push((off, vertex_format_size(f).unwrap_or(0), loc));
                }
                buffers.push(stride);
                attrs.push(list);
                per_instance.push(b.s("stepMode") == Some("instance"));
            }
            if buffers.len() > 8 {
                self.err(format!("{what}: {} vertex buffers", buffers.len()));
            }
        }
        // targets
        let mut targets = Vec::new();
        if let Some(f) = j.get("fragment") {
            for t in f.arr("targets") {
                let format = t.s("format").unwrap_or("").to_string();
                let mask = t.u("writeMask").unwrap_or(15);
                match format_info(&format) {
                    Some((_, true, blendable, _, k)) if k != 'd' => {
                        if t.get("blend").is_some() && !blendable {
                            self.err(format!("{what}: blending into {format}"));
                        }
                    }
                    _ => self.err(format!("{what}: color target {format}")),
                }
                if mask > 15 {
                    self.err(format!("{what}: writeMask {mask}"));
                }
                if let Some(b) = t.get("blend") {
                    for c in ["color", "alpha"] {
                        let Some(x) = b.get(c) else {
                            self.err(format!("{what}: blend without {c}"));
                            continue;
                        };
                        let op = x.s("operation").unwrap_or("add");
                        let (sf, df) = (
                            x.s("srcFactor").unwrap_or("one"),
                            x.s("dstFactor").unwrap_or("zero"),
                        );
                        if (op == "min" || op == "max") && (sf != "one" || df != "one") {
                            self.err(format!("{what}: blend {op} with factors {sf} {df}"));
                        }
                        for f in [sf, df] {
                            if !matches!(
                                f,
                                "zero"
                                    | "one"
                                    | "src"
                                    | "one-minus-src"
                                    | "src-alpha"
                                    | "one-minus-src-alpha"
                                    | "dst"
                                    | "one-minus-dst"
                                    | "dst-alpha"
                                    | "one-minus-dst-alpha"
                                    | "src-alpha-saturated"
                                    | "constant"
                                    | "one-minus-constant"
                            ) {
                                self.err(format!("{what}: blend factor {f}"));
                            }
                        }
                        if !matches!(op, "add" | "subtract" | "reverse-subtract" | "min" | "max") {
                            self.err(format!("{what}: blend operation {op}"));
                        }
                    }
                }
                // (one, zero, add: the old color does not count)
                let reads = t.get("blend").is_some_and(|b| {
                    ["color", "alpha"].iter().any(|c| {
                        b.get(c).is_some_and(|x| {
                            (
                                x.s("srcFactor").unwrap_or("one"),
                                x.s("dstFactor").unwrap_or("zero"),
                                x.s("operation").unwrap_or("add"),
                            ) != ("one", "zero", "add")
                        })
                    })
                });
                targets.push((format, mask, reads));
            }
        }
        let prim = j.get("primitive");
        let topo = prim
            .and_then(|p| p.s("topology"))
            .unwrap_or("triangle-list");
        if !matches!(
            topo,
            "point-list" | "line-list" | "line-strip" | "triangle-list" | "triangle-strip"
        ) {
            self.err(format!("{what}: topology {topo}"));
        }
        if let Some(sf) = prim.and_then(|p| p.s("stripIndexFormat")) {
            if !topo.ends_with("strip") || !matches!(sf, "uint16" | "uint32") {
                self.err(format!("{what}: stripIndexFormat {sf} with {topo}"));
            }
        }
        if let Some(c) = prim.and_then(|p| p.s("cullMode")) {
            if !matches!(c, "none" | "front" | "back") {
                self.err(format!("{what}: cullMode {c}"));
            }
        }
        let cmp_ok = |c: &str| {
            matches!(
                c,
                "never"
                    | "less"
                    | "equal"
                    | "less-equal"
                    | "greater"
                    | "not-equal"
                    | "greater-equal"
                    | "always"
            )
        };
        let depth = j.get("depthStencil").map(|d| {
            let f = d.s("format").unwrap_or("").to_string();
            (
                f,
                d.b("depthWriteEnabled").unwrap_or(false),
                d.s("depthCompare").unwrap_or("always").to_string(),
                d.clone(),
            )
        });
        if let Some((f, _, c, d)) = &depth {
            if format_info(f).map(|i| i.4) != Some('d') {
                self.err(format!("{what}: depthStencil format {f}"));
            }
            if !cmp_ok(c) {
                self.err(format!("{what}: depthCompare {c}"));
            }
            for face in ["stencilFront", "stencilBack"] {
                if let Some(s) = d.get(face) {
                    if !has_stencil(f) {
                        // (allowed when it is the default: "always"/"keep")
                        let defaults = s.s("compare").unwrap_or("always") == "always"
                            && ["failOp", "depthFailOp", "passOp"]
                                .iter()
                                .all(|k| s.s(k).unwrap_or("keep") == "keep");
                        if !defaults {
                            self.err(format!("{what}: stencil state with {f}"));
                        }
                    }
                    if !cmp_ok(s.s("compare").unwrap_or("always")) {
                        self.err(format!("{what}: stencil compare"));
                    }
                }
            }
            if !f.starts_with("depth") {
                // (nothing)
            }
            if (d.num("depthBias").unwrap_or(0.0) != 0.0
                || d.num("depthBiasSlopeScale").unwrap_or(0.0) != 0.0)
                && topo != "triangle-list"
                && topo != "triangle-strip"
            {
                self.err(format!("{what}: depth bias on {topo}"));
            }
        }
        if targets.is_empty() && depth.is_none() {
            self.err(format!("{what}: no color target and no depth"));
        }
        if fs.is_none() && j.get("fragment").is_some() {
            // (the error was given)
        }
        let strip_format = prim
            .and_then(|p| p.s("stripIndexFormat"))
            .map(str::to_string);
        let p = Pipeline {
            hash: h,
            groups,
            targets,
            depth: depth.map(|d| (d.0, d.1, d.2)),
            buffers,
            attrs,
            per_instance,
            topology: topo.to_string(),
            strip_format,
            clear_shader: clear,
        };
        self.put(Obj::Pipeline(p))
    }

    pub fn pipeline_group_layout(&mut self, p: u32, i: u32) -> u32 {
        let l = match self
            .at(p, "getBindGroupLayout")
            .map(|a| self.objs[a].clone())
        {
            Some(Obj::Pipeline(p)) => p.groups.get(i as usize).map(|&g| self.objs[g].clone()),
            _ => None,
        };
        match l {
            Some(o) => self.put(o),
            None => {
                self.err(format!("getBindGroupLayout({i}): no such group"));
                self.put(Obj::Layout(Vec::new()))
            }
        }
    }

    pub fn bind_group(&mut self, layout: u32, words: &[u32]) -> u32 {
        let what = "createBindGroup";
        let Some(la) = self.at(layout, what) else {
            return self.put(Obj::Group(0, Vec::new()));
        };
        let Obj::Layout(es) = self.objs[la].clone() else {
            self.err(format!("{what}: not a layout"));
            return self.put(Obj::Group(la, Vec::new()));
        };
        let mut entries = Vec::new();
        for w in words.chunks(5) {
            let (binding, kind, o, off, size) = (w[0], w[1], w[2], w[3], w[4]);
            let Some(e) = es.iter().find(|e| e.binding == binding) else {
                self.err(format!("{what}: binding {binding} is not in the layout"));
                continue;
            };
            if entries.iter().any(|x: &(u32, Entry)| x.0 == binding) {
                self.err(format!("{what}: binding {binding} twice"));
            }
            if kind == 0 {
                let Some(b) = self.buffer_of(o, what) else {
                    continue;
                };
                let Obj::Buffer(buf) = &self.objs[b] else {
                    continue;
                };
                let (bs, bu) = (buf.size, buf.usage);
                if e.kind != 'b' {
                    self.err(format!(
                        "{what}: a buffer at binding {binding}, a {} in the layout",
                        e.kind
                    ));
                }
                if bu & 0x40 == 0 {
                    self.err(format!(
                        "{what}: buffer without UNIFORM usage at binding {binding}"
                    ));
                }
                if off % 256 != 0
                    || size == 0
                    || off as u64 + size as u64 > bs as u64
                    || size > 65536
                {
                    self.err(format!(
                        "{what}: binding {binding}: {size} bytes at {off} of a {bs}-byte buffer"
                    ));
                }
                entries.push((binding, Entry::Buffer(b, off, size)));
            } else {
                let Some(a) = self.at(o, what) else { continue };
                match self.objs[a].clone() {
                    Obj::View(v) => {
                        let t = self.tex(v.tex).clone();
                        if e.kind != 't' {
                            self.err(format!(
                                "{what}: a texture view at binding {binding}, a {} in the layout",
                                e.kind
                            ));
                        }
                        if t.usage & 0x4 == 0 {
                            self.err(format!(
                                "{what}: binding {binding}: texture without TEXTURE_BINDING usage"
                            ));
                        }
                        let (_, _, _, filterable, k) =
                            format_info(&v.format).unwrap_or((4, false, false, false, '?'));
                        let kind = if v.aspect == "depth-only" || k == 'd' {
                            'd'
                        } else {
                            k
                        };
                        let ok = match e.ty.as_str() {
                            "float" => kind == 'f' && filterable,
                            "unfilterable-float" => kind == 'f' || kind == 'd',
                            "depth" => kind == 'd',
                            "uint" => kind == 'u',
                            "sint" => kind == 's',
                            _ => false,
                        };
                        if !ok {
                            self.err(format!("{what}: binding {binding}: a {} view ({}) where the layout wants {}", v.format, v.aspect, e.ty));
                        }
                        if has_stencil(&v.format) && v.aspect == "all" {
                            self.err(format!(
                                "{what}: binding {binding}: a depth-stencil view with both aspects"
                            ));
                        }
                        if v.dim != e.view_dim {
                            self.err(format!(
                                "{what}: binding {binding}: a {} view where the layout wants {}",
                                v.dim, e.view_dim
                            ));
                        }
                        entries.push((binding, Entry::View(a)));
                    }
                    Obj::Sampler(s) => {
                        if e.kind != 's' {
                            self.err(format!(
                                "{what}: a sampler at binding {binding}, a {} in the layout",
                                e.kind
                            ));
                        }
                        let ok = match e.ty.as_str() {
                            "filtering" => s & 3 != 2,
                            "non-filtering" => s & 3 == 0,
                            "comparison" => s & 3 == 2,
                            _ => false,
                        };
                        if !ok {
                            self.err(format!(
                                "{what}: binding {binding}: sampler kind {} for {}",
                                s & 3,
                                e.ty
                            ));
                        }
                        entries.push((binding, Entry::Sampler(a)));
                    }
                    _ => self.err(format!("{what}: binding {binding}: not a view or sampler")),
                }
            }
        }
        for e in &es {
            if !entries.iter().any(|x| x.0 == e.binding) {
                self.err(format!("{what}: layout binding {} has no entry", e.binding));
            }
        }
        // a view sampled through a "float" binding with a filtering sampler: checked at draw
        self.put(Obj::Group(la, entries))
    }

    // ---- the queue ----

    pub fn write_buffer(&mut self, buf: u32, at: u32, bytes: &[u8]) {
        let what = "writeBuffer";
        let Some(b) = self.buffer_of(buf, what) else {
            return;
        };
        let Obj::Buffer(bf) = &mut self.objs[b] else {
            return;
        };
        let (size, usage, mapped) = (bf.size, bf.usage, bf.mapped);
        if usage & 8 == 0
            || mapped
            || at % 4 != 0
            || bytes.len() % 4 != 0
            || at as u64 + bytes.len() as u64 > size as u64
        {
            self.err(format!("{what}: {} bytes at {at} of a {size}-byte buffer (usage {usage:#x}, mapped {mapped})", bytes.len()));
            return;
        }
        let Obj::Buffer(bf) = &mut self.objs[b] else {
            return;
        };
        bf.data[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
    }

    pub fn write_texture(
        &mut self,
        tex: u32,
        mip: u32,
        o: [u32; 3],
        p: *const u8,
        n: usize,
        bpr: u32,
        rows: u32,
        size: [u32; 3],
    ) {
        let what = "writeTexture";
        let Some(t) = self.texture_of(tex, what) else {
            return;
        };
        let tx = self.tex(t).clone();
        let Some((bpp, ..)) = format_info(&tx.format) else {
            return;
        };
        if tx.usage & 2 == 0 {
            self.err(format!("{what}: texture without COPY_DST"));
        }
        if matches!(
            tx.format.as_str(),
            "depth24plus" | "depth24plus-stencil8" | "depth32float"
        ) {
            self.err(format!("{what}: into {}", tx.format));
            return;
        }
        if mip >= tx.mips {
            self.err(format!("{what}: mip {mip} of {}", tx.mips));
            return;
        }
        let (lw, lh, ld) = tx.size(mip);
        if o[0] + size[0] > lw || o[1] + size[1] > lh || o[2] + size[2] > ld {
            self.err(format!(
                "{what}: box {o:?}+{size:?} outside mip {mip} ({lw}x{lh}x{ld})"
            ));
            return;
        }
        let row = size[0] * bpp;
        if size[1] * size[2] > 1 && bpr < row {
            self.err(format!("{what}: bytesPerRow {bpr} under a row of {row}"));
            return;
        }
        if size[2] > 1 && rows < size[1] {
            self.err(format!("{what}: rowsPerImage {rows} under {}", size[1]));
            return;
        }
        let need = if size[0] * size[1] * size[2] == 0 {
            0
        } else {
            (bpr as u64) * (rows as u64) * (size[2] as u64 - 1)
                + bpr as u64 * (size[1] as u64 - 1)
                + row as u64
        };
        if (n as u64) < need {
            self.err(format!("{what}: {n} bytes for a box that needs {need}"));
            return;
        }
        let data = unsafe { std::slice::from_raw_parts(p, n) };
        let Obj::Texture(tm) = &mut self.objs[t] else {
            return;
        };
        for z in 0..size[2] {
            let s = tm.sub(mip, o[2] + z);
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let at = (z * rows * bpr + y * bpr + x * bpp) as usize;
                    tm.texels[s][((o[1] + y) * lw + o[0] + x) as usize] =
                        write_texel(&data[at..at + bpp as usize]);
                }
            }
        }
    }

    pub fn destroy(&mut self, i: u32) {
        let Some(a) = self.at(i, "destroy") else {
            return;
        };
        match &mut self.objs[a] {
            Obj::Texture(t) => t.destroyed = true,
            Obj::Buffer(b) => b.destroyed = true,
            _ => {}
        }
    }

    pub fn drop_obj(&mut self, i: u32) {
        if i == 0 {
            return;
        }
        match self.table.get(i as usize).copied().flatten() {
            Some(_) => {
                self.table[i as usize] = None;
                self.free.push(i);
            }
            None => self.err(format!("drop: object {i} is not in the table")),
        }
    }

    // ---- the encoder ----

    fn no_pass(&mut self, what: &str) {
        if self.pass.is_some() {
            self.err(format!("{what} while a render pass is open"));
        }
    }

    pub fn copy_texture(
        &mut self,
        src: u32,
        smip: u32,
        so: [u32; 3],
        dst: u32,
        dmip: u32,
        d: [u32; 3],
        size: [u32; 3],
    ) {
        let what = "copyTextureToTexture";
        self.no_pass(what);
        let (Some(s), Some(t)) = (self.texture_of(src, what), self.texture_of(dst, what)) else {
            return;
        };
        let (st, dt) = (self.tex(s).clone(), self.tex(t).clone());
        if st.usage & 1 == 0 || dt.usage & 2 == 0 {
            self.err(format!(
                "{what}: usage (source COPY_SRC, destination COPY_DST)"
            ));
        }
        let base = |f: &str| f.trim_end_matches("-srgb").to_string();
        if base(&st.format) != base(&dt.format) {
            self.err(format!("{what}: {} to {}", st.format, dt.format));
        }
        if smip >= st.mips || dmip >= dt.mips {
            self.err(format!(
                "{what}: mip {smip} of {} / {dmip} of {}",
                st.mips, dt.mips
            ));
            return;
        }
        let (sw, sh, sd) = st.size(smip);
        let (dw, dh, dd) = dt.size(dmip);
        if so[0] + size[0] > sw
            || so[1] + size[1] > sh
            || so[2] + size[2] > sd
            || d[0] + size[0] > dw
            || d[1] + size[1] > dh
            || d[2] + size[2] > dd
        {
            self.err(format!(
                "{what}: {so:?}+{size:?} of {sw}x{sh}x{sd} to {d:?} of {dw}x{dh}x{dd}"
            ));
            return;
        }
        if st.format.starts_with("depth")
            && (st.format == "depth24plus"
                || st.format == "depth24plus-stencil8"
                || size[0] != sw
                || size[1] != sh)
        {
            self.err(format!(
                "{what}: {} (depth formats: whole subresources of copyable ones)",
                st.format
            ));
        }
        if s == t && smip == dmip && so[2] < d[2] + size[2] && d[2] < so[2] + size[2] {
            self.err(format!(
                "{what}: source and destination are the same subresource"
            ));
        }
        self.enc.push(Cmd::Copy(s, smip, so, t, dmip, d, size));
    }

    pub fn copy_to_buffer(
        &mut self,
        tex: u32,
        mip: u32,
        x: u32,
        y: u32,
        buf: u32,
        bpr: u32,
        w: u32,
        h: u32,
    ) {
        let what = "copyTextureToBuffer";
        self.no_pass(what);
        let (Some(t), Some(b)) = (self.texture_of(tex, what), self.buffer_of(buf, what)) else {
            return;
        };
        let tx = self.tex(t).clone();
        let Obj::Buffer(bf) = &self.objs[b] else {
            return;
        };
        let (bs, bu) = (bf.size, bf.usage);
        let Some((bpp, ..)) = format_info(&tx.format) else {
            return;
        };
        if tx.usage & 1 == 0 || bu & 8 == 0 {
            self.err(format!("{what}: usage"));
        }
        if matches!(tx.format.as_str(), "depth24plus" | "depth24plus-stencil8") {
            self.err(format!("{what}: from {}", tx.format));
            return;
        }
        if mip >= tx.mips {
            self.err(format!("{what}: mip {mip}"));
            return;
        }
        let (lw, lh, _) = tx.size(mip);
        if x + w > lw || y + h > lh {
            self.err(format!("{what}: {x},{y}+{w}x{h} outside {lw}x{lh}"));
            return;
        }
        if bpr % 256 != 0 || (h > 1 && bpr < w * bpp) {
            self.err(format!("{what}: bytesPerRow {bpr}"));
            return;
        }
        if h > 0 && w > 0 && (bpr as u64 * (h as u64 - 1) + (w * bpp) as u64) > bs as u64 {
            self.err(format!("{what}: {w}x{h} into a {bs}-byte buffer"));
            return;
        }
        self.enc.push(Cmd::ToBuffer(t, mip, x, y, b, bpr, w, h));
    }

    pub fn begin_pass(&mut self, s: &str) {
        let what = "beginRenderPass";
        self.no_pass(what);
        let Some(j) = self.json(s, what) else { return };
        let mut size: Option<(u32, u32)> = None;
        let mut colors = Vec::new();
        let mut subs: Vec<(usize, u32, u32)> = Vec::new();
        let mut check_view = |m: &mut Model, v: u32, color: bool| -> Option<usize> {
            let a = m.at(v, what)?;
            let Some(vw) = m.view_of(a).cloned() else {
                m.err(format!("{what}: attachment {v} is not a view"));
                return None;
            };
            let t = m.tex(vw.tex).clone();
            if t.destroyed {
                m.err(format!("{what}: attachment of a destroyed texture"));
            }
            if t.usage & 0x10 == 0 {
                m.err(format!("{what}: texture without RENDER_ATTACHMENT"));
            }
            if vw.mips != 1 || vw.layers != 1 || !matches!(vw.dim.as_str(), "2d" | "2d-array") {
                m.err(format!(
                    "{what}: attachment view of {} mips {} layers ({})",
                    vw.mips, vw.layers, vw.dim
                ));
            }
            let k = format_info(&vw.format).map(|i| i.4);
            if color != (k != Some('d')) {
                m.err(format!(
                    "{what}: {} as a {} attachment",
                    vw.format,
                    if color { "color" } else { "depth" }
                ));
            }
            let (w, h, _) = t.size(vw.mip);
            match size {
                None => size = Some((w, h)),
                Some(s) if s != (w, h) => {
                    m.err(format!("{what}: attachments of {s:?} and {w}x{h}"))
                }
                _ => {}
            }
            if subs.contains(&(vw.tex, vw.mip, vw.layer)) {
                m.err(format!("{what}: one subresource attached twice"));
            }
            subs.push((vw.tex, vw.mip, vw.layer));
            Some(a)
        };
        let carr = j.arr("colorAttachments").to_vec();
        if carr.len() > 8 {
            self.err(format!("{what}: {} color attachments", carr.len()));
        }
        for c in &carr {
            if *c == J::Null {
                continue;
            }
            let Some(v) = c.u("view") else {
                self.err(format!("{what}: color attachment without a view"));
                continue;
            };
            let Some(a) = check_view(self, v, true) else {
                continue;
            };
            let load = c.s("loadOp").unwrap_or("");
            let store = c.s("storeOp").unwrap_or("");
            if !matches!(load, "load" | "clear") || !matches!(store, "store" | "discard") {
                self.err(format!("{what}: loadOp {load} storeOp {store}"));
            }
            let cv: Vec<f32> = c
                .arr("clearValue")
                .iter()
                .map(|x| if let J::Num(n) = x { *n as f32 } else { 0.0 })
                .collect();
            let color = [
                cv.first().copied().unwrap_or(0.0),
                cv.get(1).copied().unwrap_or(0.0),
                cv.get(2).copied().unwrap_or(0.0),
                cv.get(3).copied().unwrap_or(0.0),
            ];
            let format = self
                .view_of(a)
                .map(|v| self.tex(v.tex).format.clone())
                .unwrap_or_default();
            colors.push(Attachment {
                view: a,
                load_clear: (load == "clear").then(|| clear_texel(&format, color)),
                store: store == "store",
                stencil_clear: None,
                stencil_store: true,
            });
        }
        let mut depth = None;
        if let Some(d) = j.get("depthStencilAttachment") {
            if let Some(a) = d.u("view").and_then(|v| check_view(self, v, false)) {
                let vw = self.view_of(a).cloned().unwrap();
                let ro = d.b("depthReadOnly").unwrap_or(false);
                let (load, store) = (d.s("depthLoadOp"), d.s("depthStoreOp"));
                if !ro
                    && (!matches!(load, Some("load" | "clear"))
                        || !matches!(store, Some("store" | "discard")))
                {
                    self.err(format!(
                        "{what}: depthLoadOp {load:?} depthStoreOp {store:?}"
                    ));
                }
                let z = d.num("depthClearValue");
                if load == Some("clear") && !z.is_some_and(|z| (0.0..=1.0).contains(&z)) {
                    self.err(format!("{what}: depthClearValue {z:?}"));
                }
                let (sl, ss) = (d.s("stencilLoadOp"), d.s("stencilStoreOp"));
                let sro = d.b("stencilReadOnly").unwrap_or(false);
                if has_stencil(&vw.format) {
                    if !sro
                        && (!matches!(sl, Some("load" | "clear"))
                            || !matches!(ss, Some("store" | "discard")))
                    {
                        self.err(format!("{what}: {} needs stencilLoadOp/StoreOp", vw.format));
                    }
                } else if sl.is_some() || ss.is_some() {
                    self.err(format!("{what}: stencil ops for {}", vw.format));
                }
                depth = Some(Attachment {
                    view: a,
                    load_clear: (load == Some("clear"))
                        .then(|| depth_value(z.unwrap_or(0.0) as f32)),
                    store: store != Some("discard"),
                    stencil_clear: (sl == Some("clear"))
                        .then(|| stencil_value(d.u("stencilClearValue").unwrap_or(0))),
                    stencil_store: ss != Some("discard"),
                });
            }
        }
        let Some(sz) = size else {
            self.err(format!("{what}: no attachments"));
            return;
        };
        let st = DrawState {
            viewport: [0.0, 0.0, sz.0 as f32, sz.1 as f32, 0.0, 1.0],
            scissor: [0, 0, sz.0, sz.1],
            ..Default::default()
        };
        self.pass = Some((colors, depth, Vec::new(), st, sz));
    }

    pub fn end_pass(&mut self) {
        match self.pass.take() {
            Some((c, d, draws, _, _)) => self.enc.push(Cmd::Pass(c, d, draws)),
            None => self.err("end(): no render pass".into()),
        }
    }

    fn in_pass(&mut self, what: &str) -> bool {
        if self.pass.is_none() {
            self.err(format!("{what} outside a render pass"));
            return false;
        }
        true
    }

    pub fn set_pipeline(&mut self, p: u32) {
        if !self.in_pass("setPipeline") {
            return;
        }
        let Some(a) = self.at(p, "setPipeline") else {
            return;
        };
        if !matches!(self.objs[a], Obj::Pipeline(_)) {
            self.err("setPipeline: not a pipeline".into());
            return;
        }
        self.pass.as_mut().unwrap().3.pipeline = Some(a);
    }

    pub fn set_bind_group(&mut self, i: u32, g: u32, offs: &[u32]) {
        let what = "setBindGroup";
        if !self.in_pass(what) {
            return;
        }
        if i >= 4 {
            self.err(format!("{what}: index {i}"));
            return;
        }
        let Some(a) = self.at(g, what) else { return };
        let Obj::Group(la, entries) = self.objs[a].clone() else {
            self.err(format!("{what}: not a bind group"));
            return;
        };
        let Obj::Layout(es) = self.objs[la].clone() else {
            return;
        };
        // dynamic offsets: one per dynamic buffer entry, in binding order
        let mut dynamic: Vec<&LayoutEntry> = es.iter().filter(|e| e.dynamic).collect();
        dynamic.sort_by_key(|e| e.binding);
        if dynamic.len() != offs.len() {
            self.err(format!(
                "{what}: {} dynamic offsets for {} dynamic bindings",
                offs.len(),
                dynamic.len()
            ));
        }
        for (e, &o) in dynamic.iter().zip(offs) {
            if o % 256 != 0 {
                self.err(format!("{what}: dynamic offset {o}"));
            }
            if let Some((_, Entry::Buffer(b, off, size))) =
                entries.iter().find(|x| x.0 == e.binding)
            {
                if let Obj::Buffer(bf) = &self.objs[*b] {
                    if *off as u64 + o as u64 + *size as u64 > bf.size as u64 {
                        self.err(format!(
                            "{what}: binding {}: {size} bytes at {off}+{o} of a {}-byte buffer",
                            e.binding, bf.size
                        ));
                    }
                }
            }
        }
        let st = &mut self.pass.as_mut().unwrap().3;
        if st.groups.len() <= i as usize {
            st.groups.resize(i as usize + 1, None);
        }
        st.groups[i as usize] = Some((a, offs.to_vec()));
    }

    pub fn set_vertex_buffer(&mut self, slot: u32, buf: u32, off: u32) {
        let what = "setVertexBuffer";
        if !self.in_pass(what) {
            return;
        }
        let Some(b) = self.buffer_of(buf, what) else {
            return;
        };
        let Obj::Buffer(bf) = &self.objs[b] else {
            return;
        };
        let (u, s) = (bf.usage, bf.size);
        if u & 0x20 == 0 || off % 4 != 0 || off > s || slot >= 8 {
            self.err(format!(
                "{what}: slot {slot} offset {off} of {s} (usage {u:#x})"
            ));
        }
        let st = &mut self.pass.as_mut().unwrap().3;
        if st.vbufs.len() <= slot as usize {
            st.vbufs.resize(slot as usize + 1, None);
        }
        st.vbufs[slot as usize] = Some((b, off));
    }

    pub fn set_index_buffer(&mut self, buf: u32, u32_: bool, off: u32) {
        let what = "setIndexBuffer";
        if !self.in_pass(what) {
            return;
        }
        let Some(b) = self.buffer_of(buf, what) else {
            return;
        };
        let Obj::Buffer(bf) = &self.objs[b] else {
            return;
        };
        let (u, s) = (bf.usage, bf.size);
        if u & 0x10 == 0 || off % if u32_ { 4 } else { 2 } != 0 || off > s {
            self.err(format!("{what}: offset {off} of {s} (usage {u:#x})"));
        }
        self.pass.as_mut().unwrap().3.ibuf = Some((b, u32_, off));
    }

    pub fn set_viewport(&mut self, v: [f32; 6]) {
        let what = "setViewport";
        if !self.in_pass(what) {
            return;
        }
        let (w, h) = self.pass.as_ref().unwrap().4;
        let [x, y, vw, vh, z0, z1] = v;
        if vw < 0.0
            || vh < 0.0
            || x < 0.0
            || y < 0.0
            || x + vw > w as f32
            || y + vh > h as f32
            || !(0.0..=1.0).contains(&z0)
            || !(0.0..=1.0).contains(&z1)
            || z0 > z1
        {
            self.err(format!("{what}: {v:?} in a {w}x{h} pass"));
        }
        self.pass.as_mut().unwrap().3.viewport = v;
    }

    pub fn set_scissor(&mut self, s: [u32; 4]) {
        let what = "setScissorRect";
        if !self.in_pass(what) {
            return;
        }
        let (w, h) = self.pass.as_ref().unwrap().4;
        if s[0] as u64 + s[2] as u64 > w as u64 || s[1] as u64 + s[3] as u64 > h as u64 {
            self.err(format!("{what}: {s:?} in a {w}x{h} pass"));
        }
        self.pass.as_mut().unwrap().3.scissor = s;
    }

    pub fn set_blend(&mut self, c: [f64; 4]) {
        if self.in_pass("setBlendConstant") {
            self.pass.as_mut().unwrap().3.blend = c;
        }
    }

    pub fn set_stencil_ref(&mut self, v: u32) {
        if self.in_pass("setStencilReference") {
            self.pass.as_mut().unwrap().3.stencil_ref = v;
        }
    }

    pub fn draw(&mut self, args: [u32; 5], indexed: bool) {
        let what = if indexed { "drawIndexed" } else { "draw" };
        if !self.in_pass(what) {
            return;
        }
        let (colors, depth, _, st, _) = self.pass.clone().unwrap();
        let Some(pa) = st.pipeline else {
            self.err(format!("{what}: no pipeline"));
            return;
        };
        let Obj::Pipeline(p) = self.objs[pa].clone() else {
            return;
        };
        // the pipeline's attachment formats are the pass's
        let pass_colors: Vec<String> = colors
            .iter()
            .map(|c| self.view_of(c.view).unwrap().format.clone())
            .collect();
        let pipe_colors: Vec<String> = p.targets.iter().map(|t| t.0.clone()).collect();
        if pass_colors != pipe_colors {
            self.err(format!(
                "{what}: pipeline targets {pipe_colors:?}, pass {pass_colors:?}"
            ));
        }
        let pass_depth = depth
            .as_ref()
            .map(|d| self.view_of(d.view).unwrap().format.clone());
        if pass_depth != p.depth.as_ref().map(|d| d.0.clone()) {
            self.err(format!(
                "{what}: pipeline depth {:?}, pass {pass_depth:?}",
                p.depth.as_ref().map(|d| &d.0)
            ));
        }
        // bind groups: one compatible with each of the layout's
        for (gi, &lg) in p.groups.iter().enumerate() {
            match st.groups.get(gi).cloned().flatten() {
                None => self.err(format!("{what}: bind group {gi} not set")),
                Some((g, _)) => {
                    let Obj::Group(la, entries) = self.objs[g].clone() else {
                        continue;
                    };
                    let (Obj::Layout(a), Obj::Layout(b)) = (&self.objs[la], &self.objs[lg]) else {
                        continue;
                    };
                    let same = la == lg || format!("{a:?}") == format!("{b:?}");
                    if !same {
                        self.err(format!("{what}: bind group {gi} of another layout"));
                    }
                    // a texture both sampled and attached in this pass
                    for (_, e) in &entries {
                        if let Entry::View(v) = e {
                            let vw = self.view_of(*v).unwrap().clone();
                            for att in colors.iter().chain(depth.iter()) {
                                let av = self.view_of(att.view).unwrap().clone();
                                if av.tex == vw.tex
                                    && av.mip >= vw.mip
                                    && av.mip < vw.mip + vw.mips
                                    && av.layer >= vw.layer
                                    && av.layer < vw.layer + vw.layers
                                {
                                    self.err(format!(
                                        "{what}: a texture sampled and attached in the same pass"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        // vertex and index buffers
        for (k, &stride) in p.buffers.iter().enumerate() {
            match st.vbufs.get(k).cloned().flatten() {
                None => self.err(format!("{what}: vertex buffer {k} not set")),
                Some((b, off)) => {
                    if !indexed {
                        let need = (args[2] as u64 + args[0] as u64) * stride as u64;
                        let Obj::Buffer(bf) = &self.objs[b] else {
                            continue;
                        };
                        if args[0] > 0 && stride > 0 && off as u64 + need > bf.size as u64 {
                            self.err(format!(
                                "{what}: vertex buffer {k}: {need} bytes at {off} of {}",
                                bf.size
                            ));
                        }
                    }
                }
            }
        }
        if indexed {
            match st.ibuf {
                None => self.err(format!("{what}: no index buffer")),
                Some((b, u32_, off)) => {
                    let Obj::Buffer(bf) = &self.objs[b] else {
                        return;
                    };
                    let need = (args[2] as u64 + args[0] as u64) * if u32_ { 4 } else { 2 };
                    if off as u64 + need > bf.size as u64 {
                        self.err(format!(
                            "{what}: {} indices at {off} of a {}-byte buffer",
                            args[0], bf.size
                        ));
                    }
                }
            }
        }
        let bounds = self.bounds;
        self.pass.as_mut().unwrap().2.push(Draw {
            st,
            args,
            indexed,
            bounds,
        });
    }

    /// The values of a texture's subresources a view sees, hashed.
    fn view_hash(&self, v: &View) -> u64 {
        let t = self.tex(v.tex);
        let mut h = mix(0x71e, (v.mip as u64) << 32 | v.layer as u64);
        for m in v.mip..v.mip + v.mips {
            for l in v.layer..v.layer + v.layers {
                for &x in t.texels.get(t.sub(m, l)).map(|s| &s[..]).unwrap_or(&[]) {
                    h = mix(h, x);
                }
            }
        }
        mix(h, hash_str(&v.format))
    }

    /// The primitives a draw assembles (WebGPU's vertex fetch and primitive assembly: index
    /// buffer, baseVertex, topology, strip restart), instance after instance.
    fn assemble(&self, d: &Draw) -> Vec<Vec<Vertex>> {
        (d.args[4]..d.args[4].saturating_add(d.args[1]))
            .flat_map(|i| self.assemble1(d, i))
            .collect()
    }

    fn assemble1(&self, d: &Draw, instance: u32) -> Vec<Vec<Vertex>> {
        let Obj::Pipeline(p) = &self.objs[d.st.pipeline.unwrap()] else {
            return Vec::new();
        };
        let (n, first, base) = (d.args[0], d.args[2], d.args[3] as i32);
        // the vertex index of each invocation (None: a strip restart)
        let ids: Vec<Option<i64>> = if d.indexed {
            let Some((b, u32_, off)) = d.st.ibuf else {
                return Vec::new();
            };
            let Obj::Buffer(bf) = &self.objs[b] else {
                return Vec::new();
            };
            let sz = if u32_ { 4 } else { 2 };
            (0..n as usize)
                .map(|i| {
                    let at = off as usize + (first as usize + i) * sz;
                    let ix = bf.data.get(at..at + sz).map(|c| {
                        if sz == 4 {
                            u32::from_le_bytes(c.try_into().unwrap())
                        } else {
                            u16::from_le_bytes(c.try_into().unwrap()) as u32
                        }
                    });
                    let ix = ix.unwrap_or(0);
                    let restart =
                        p.strip_format.is_some() && ix == if u32_ { u32::MAX } else { 0xffff };
                    (!restart).then_some(ix as i64 + base as i64)
                })
                .collect()
        } else {
            (0..n).map(|i| Some(first as i64 + i as i64)).collect()
        };
        let vertex =
            |v: i64| -> Vertex {
                let mut out = Vec::new();
                for (k, list) in p.attrs.iter().enumerate() {
                    let stride = p.buffers[k] as i64;
                    let v = if p.per_instance[k] {
                        instance as i64
                    } else {
                        v
                    };
                    let src =
                        d.st.vbufs.get(k).copied().flatten().and_then(|(b, off)| {
                            match &self.objs[b] {
                                Obj::Buffer(bf) => Some((bf, off)),
                                _ => None,
                            }
                        });
                    for &(aoff, size, loc) in list {
                        let bytes = src
                            .and_then(|(bf, off)| {
                                let at = off as i64 + v * stride + aoff as i64;
                                (at >= 0)
                                    .then(|| bf.data.get(at as usize..at as usize + size as usize))
                                    .flatten()
                            })
                            .map(|b| b.to_vec())
                            .unwrap_or_default();
                        out.push((loc, bytes));
                    }
                }
                out.sort_by_key(|x| x.0);
                out
            };
        let mut prims = Vec::new();
        // (runs between restarts)
        for run in ids.split(|x| x.is_none()) {
            let vs: Vec<Vertex> = run.iter().map(|x| vertex(x.unwrap())).collect();
            match p.topology.as_str() {
                "point-list" => prims.extend(vs.into_iter().map(|v| vec![v])),
                "line-list" => prims.extend(vs.chunks_exact(2).map(|c| c.to_vec())),
                "line-strip" => prims.extend(vs.windows(2).map(|c| c.to_vec())),
                "triangle-list" => prims.extend(vs.chunks_exact(3).map(|c| c.to_vec())),
                _ => {
                    for (i, w) in vs.windows(3).enumerate() {
                        prims.push(if i % 2 == 0 {
                            w.to_vec()
                        } else {
                            vec![w[1].clone(), w[0].clone(), w[2].clone()]
                        });
                    }
                }
            }
        }
        prims
    }

    /// What a draw reads, hashed (its "signature"), and for the clear shader: the color and
    /// depth it writes.
    fn draw_sig(&mut self, d: &Draw) -> (u64, Option<([f32; 4], f32)>) {
        let p = match &self.objs[d.st.pipeline.unwrap()] {
            Obj::Pipeline(p) => p.clone(),
            _ => unreachable!(),
        };
        let mut h = mix(0xd7a, p.hash);
        let mut clear = None;
        for (gi, g) in d.st.groups.iter().enumerate().take(p.groups.len()) {
            let Some((g, offs)) = g else { continue };
            let Obj::Group(la, entries) = self.objs[*g].clone() else {
                continue;
            };
            let Obj::Layout(es) = self.objs[la].clone() else {
                continue;
            };
            let mut dynamic: Vec<u32> =
                es.iter().filter(|e| e.dynamic).map(|e| e.binding).collect();
            dynamic.sort();
            for (binding, e) in &entries {
                h = mix(h, (gi as u64) << 32 | *binding as u64);
                match e {
                    Entry::Buffer(b, off, size) => {
                        let dynoff = dynamic
                            .iter()
                            .position(|x| x == binding)
                            .and_then(|k| offs.get(k))
                            .copied()
                            .unwrap_or(0);
                        let Obj::Buffer(bf) = &self.objs[*b] else {
                            continue;
                        };
                        let lo = (*off + dynoff) as usize;
                        let hi = (lo + *size as usize).min(bf.data.len());
                        let bytes = &bf.data[lo.min(hi)..hi];
                        h = hash_bytes(h, bytes);
                        if p.clear_shader && bytes.len() >= 32 {
                            let f = |k: usize| {
                                f32::from_le_bytes(bytes[k * 4..k * 4 + 4].try_into().unwrap())
                            };
                            clear = Some(([f(0), f(1), f(2), f(3)], f(4)));
                        }
                    }
                    Entry::View(v) => {
                        let vw = self.view_of(*v).unwrap().clone();
                        if std::env::var_os("IPPAI_MODEL_SIGS").is_some() {
                            {
                                let t = self.tex(vw.tex);
                                eprintln!("    view {gi}/{binding}: tex {} {}x{} mips {} (view mip {} n {}) {:x}", vw.tex, t.w, t.h, t.mips, vw.mip, vw.mips, self.view_hash(&vw));
                            }
                        }
                        h = mix(h, self.view_hash(&vw));
                    }
                    Entry::Sampler(s) => {
                        if let Obj::Sampler(x) = self.objs[*s] {
                            h = mix(h, x);
                        }
                    }
                }
            }
        }
        // vertices and indices: the bytes it reads
        let (n, inst, first, base, first_inst) =
            (d.args[0], d.args[1], d.args[2], d.args[3] as i32, d.args[4]);
        let mut max_vertex = first as u64 + n as u64;
        if d.indexed {
            if let Some((b, u32_, off)) = d.st.ibuf {
                let Obj::Buffer(bf) = &self.objs[b] else {
                    unreachable!()
                };
                let sz = if u32_ { 4 } else { 2 };
                let lo = off as usize + first as usize * sz;
                let hi = (lo + n as usize * sz).min(bf.data.len());
                let ix = &bf.data[lo.min(hi)..hi];
                h = hash_bytes(h, ix);
                max_vertex = ix.chunks(sz).map(|c| if sz == 4 { u32::from_le_bytes(c.try_into().unwrap()) } else { u16::from_le_bytes(c.try_into().unwrap()) as u32 } as i64 + base as i64).max().unwrap_or(0).max(0) as u64 + 1;
            }
        }
        for (k, &stride) in p.buffers.iter().enumerate() {
            if let Some((b, off)) = d.st.vbufs.get(k).copied().flatten() {
                let Obj::Buffer(bf) = &self.objs[b] else {
                    continue;
                };
                let hi =
                    (off as u64 + max_vertex * stride as u64).min(bf.data.len() as u64) as usize;
                h = hash_bytes(h, &bf.data[(off as usize).min(hi)..hi]);
            }
        }
        h = mix(h, (n as u64) << 32 | inst as u64);
        h = mix(h, (first as u64) << 32 | (base as u32 as u64));
        h = mix(h, first_inst as u64);
        for v in d.st.viewport {
            h = mix(h, v.to_bits() as u64);
        }
        for c in d.st.blend {
            h = mix(h, c.to_bits());
        }
        h = mix(h, d.st.stencil_ref as u64);
        if std::env::var_os("IPPAI_MODEL_SIGS").is_some() {
            eprintln!(
                "    sig parts: pipe {:x} after groups/verts/args {:x}",
                p.hash, h
            );
        }
        (h, clear)
    }

    fn run_pass(&mut self, colors: &[Attachment], depth: &Option<Attachment>, draws: &[Draw]) {
        // loads
        let sub = |m: &Model, a: &Attachment| {
            let v = m.view_of(a.view).unwrap().clone();
            let t = m.tex(v.tex);
            (v.tex, t.sub(v.mip, v.layer), t.size(v.mip))
        };
        if std::env::var_os("IPPAI_MODEL_SIGS").is_some() {
            eprintln!(
                "  model pass {:?}",
                colors
                    .iter()
                    .chain(depth.iter())
                    .map(|a| (
                        self.view_of(a.view).map(|v| (v.tex, v.mip)),
                        a.load_clear,
                        a.store
                    ))
                    .collect::<Vec<_>>()
            );
        }
        for a in colors.iter().chain(depth.iter()) {
            let (t, s, _) = sub(self, a);
            let Obj::Texture(tx) = &mut self.objs[t] else {
                continue;
            };
            if let Some(v) = a.load_clear {
                tx.texels[s].fill(v);
            }
            if let Some(v) = a.stencil_clear {
                if let Some(st) = tx.stencil.get_mut(s) {
                    st.fill(v);
                }
            }
        }
        for d in draws {
            if self.prims.is_some() {
                let p = self.assemble(d);
                self.prims.as_mut().unwrap().push(p);
            }
            let (sig, clear) = self.draw_sig(d);
            if std::env::var_os("IPPAI_MODEL_SIGS").is_some() {
                eprintln!("  model draw into {:?} {sig:x} vp {:?} sc {:?} bounds {:?} load {:?} zload {:?}", colors.iter().chain(depth.iter()).map(|a| self.view_of(a.view).map(|v| v.tex)).collect::<Vec<_>>(), d.st.viewport, d.st.scissor, d.bounds, colors.iter().map(|a| a.load_clear).collect::<Vec<_>>(), depth.as_ref().map(|a| a.load_clear));
            }
            let Obj::Pipeline(p) = self.objs[d.st.pipeline.unwrap()].clone() else {
                continue;
            };
            let [vx, vy, vw, vh, _, _] = d.st.viewport;
            let s = d.st.scissor;
            let (cx0, cy0) = (
                (vx - 0.5).ceil().max(0.0) as u32,
                (vy - 0.5).ceil().max(0.0) as u32,
            );
            let (cx1, cy1) = (
                (vx + vw - 0.5).ceil().max(0.0) as u32,
                (vy + vh - 0.5).ceil().max(0.0) as u32,
            );
            let (mut x0, mut y0, mut x1, mut y1) = (
                cx0.max(s[0]),
                cy0.max(s[1]),
                cx1.min(s[0] + s[2]),
                cy1.min(s[1] + s[3]),
            );
            if let Some(b) = d.bounds {
                (x0, y0, x1, y1) = (x0.max(b[0]), y0.max(b[1]), x1.min(b[2]), y1.min(b[3]));
            }
            let zatt = depth.as_ref().map(|a| sub(self, a));
            for (ci, a) in colors.iter().enumerate() {
                let (t, si, (w, h, _)) = sub(self, a);
                let mask = p.targets.get(ci).map_or(15, |t| t.1);
                // (a depth test may keep the old color too)
                let ztest = zatt.is_some() && p.depth.as_ref().is_some_and(|d| d.2 != "always");
                let blends = p.targets.get(ci).is_some_and(|t| t.2) || ztest;
                for y in y0..y1.min(h) {
                    for x in x0..x1.min(w) {
                        let at = (y * w + x) as usize;
                        let old = self.tex(t).texels[si][at];
                        let oz = zatt.map_or(0, |(zt, zs, _)| self.tex(zt).texels[zs][at]);
                        let new = match clear {
                            Some((c, _)) if p.clear_shader => clear_texel(&self.tex(t).format, c),
                            _ => mix(
                                mix(
                                    mix(sig, (x as u64) << 32 | y as u64),
                                    if blends { old } else { 0 },
                                ),
                                if p.depth.as_ref().is_some_and(|d| d.2 != "always") {
                                    oz
                                } else {
                                    0
                                },
                            ),
                        };
                        if mask != 0 {
                            let Obj::Texture(tx) = &mut self.objs[t] else {
                                continue;
                            };
                            // (a partial write mask keeps part of the old value)
                            tx.texels[si][at] = if mask == 15 {
                                new
                            } else {
                                mix(new, old ^ mask as u64)
                            };
                        }
                    }
                }
            }
            if let (Some((zt, zs, (w, h, _))), Some(pd)) = (zatt, &p.depth) {
                if pd.1 {
                    for y in y0..y1.min(h) {
                        for x in x0..x1.min(w) {
                            let at = (y * w + x) as usize;
                            let oz = self.tex(zt).texels[zs][at];
                            let nz = match clear {
                                Some((_, z)) if p.clear_shader => depth_value(z),
                                _ => mix(
                                    mix(mix(sig ^ 0xde, (x as u64) << 32 | y as u64), oz),
                                    if pd.2 != "always" { 1 } else { 0 },
                                ),
                            };
                            let Obj::Texture(tx) = &mut self.objs[zt] else {
                                continue;
                            };
                            tx.texels[zs][at] = nz;
                        }
                    }
                }
            }
        }
        // stores
        for a in colors.iter().chain(depth.iter()) {
            let (t, s, _) = sub(self, a);
            let Obj::Texture(tx) = &mut self.objs[t] else {
                continue;
            };
            if !a.store {
                tx.texels[s].fill(0);
            }
            if !a.stencil_store {
                if let Some(st) = tx.stencil.get_mut(s) {
                    st.fill(0);
                }
            }
        }
    }

    pub fn submit(&mut self) {
        self.no_pass("submit");
        let cmds = std::mem::take(&mut self.enc);
        // (destroyed or mapped resources in the commands: the whole submit fails)
        let mut bad = Vec::new();
        for c in &cmds {
            let mut texs = Vec::new();
            let mut bufs = Vec::new();
            match c {
                Cmd::Copy(s, _, _, t, ..) => texs.extend([*s, *t]),
                Cmd::ToBuffer(t, _, _, _, b, ..) => {
                    texs.push(*t);
                    bufs.push(*b);
                }
                Cmd::Pass(cs, d, draws) => {
                    for a in cs.iter().chain(d.iter()) {
                        texs.push(self.view_of(a.view).unwrap().tex);
                    }
                    for dr in draws {
                        for (g, _) in dr.st.groups.iter().flatten() {
                            if let Obj::Group(_, es) = &self.objs[*g] {
                                for (_, e) in es {
                                    match e {
                                        Entry::Buffer(b, ..) => bufs.push(*b),
                                        Entry::View(v) => texs.push(self.view_of(*v).unwrap().tex),
                                        _ => {}
                                    }
                                }
                            }
                        }
                        bufs.extend(dr.st.vbufs.iter().flatten().map(|v| v.0));
                        bufs.extend(dr.st.ibuf.iter().map(|v| v.0));
                    }
                }
            }
            for t in texs {
                if self.tex(t).destroyed {
                    bad.push("a destroyed texture".to_string());
                }
            }
            for b in bufs {
                if let Obj::Buffer(bf) = &self.objs[b] {
                    if bf.destroyed || bf.mapped {
                        bad.push(format!(
                            "a {} buffer",
                            if bf.destroyed { "destroyed" } else { "mapped" }
                        ));
                    }
                }
            }
        }
        if !bad.is_empty() {
            self.err(format!("submit: commands use {}", bad.join(", ")));
            return;
        }
        for c in cmds {
            match c {
                Cmd::Copy(s, smip, so, t, dmip, d, size) => {
                    let st = self.tex(s).clone();
                    let (sw, ..) = st.size(smip);
                    let (dw, ..) = self.tex(t).size(dmip);
                    for z in 0..size[2] {
                        for y in 0..size[1] {
                            for x in 0..size[0] {
                                let v = st.texels[st.sub(smip, so[2] + z)]
                                    [((so[1] + y) * sw + so[0] + x) as usize];
                                let Obj::Texture(tx) = &mut self.objs[t] else {
                                    continue;
                                };
                                let si = tx.sub(dmip, d[2] + z);
                                tx.texels[si][((d[1] + y) * dw + d[0] + x) as usize] = v;
                            }
                        }
                    }
                }
                Cmd::ToBuffer(t, mip, x, y, b, bpr, w, h) => {
                    let tx = self.tex(t).clone();
                    let (bpp, ..) = format_info(&tx.format).unwrap();
                    let (lw, ..) = tx.size(mip);
                    let s = tx.sub(mip, 0);
                    let Obj::Buffer(bf) = &mut self.objs[b] else {
                        continue;
                    };
                    for r in 0..h {
                        for c in 0..w {
                            let v = tx.texels[s][((y + r) * lw + x + c) as usize].to_le_bytes();
                            let at = (r * bpr + c * bpp) as usize;
                            for k in 0..bpp as usize {
                                bf.data[at + k] = v[k % 8];
                            }
                        }
                    }
                }
                Cmd::Pass(cs, d, draws) => self.run_pass(&cs, &d, &draws),
            }
        }
    }

    pub fn read_buffer(&mut self, buf: u32, size: u32, dst: *mut u8, token: u32) {
        let what = "mapAsync";
        let Some(b) = self.buffer_of(buf, what) else {
            return;
        };
        let Obj::Buffer(bf) = &mut self.objs[b] else {
            return;
        };
        if bf.usage & 1 == 0 || bf.mapped || size > bf.size {
            let (u, m, s) = (bf.usage, bf.mapped, bf.size);
            self.err(format!(
                "{what}: usage {u:#x} mapped {m} size {size} of {s}"
            ));
            return;
        }
        bf.mapped = true;
        self.pending_maps.push((b, size, dst as usize, token));
    }

    /// Finishes the maps asked for (as the page does once the GPU is done): copies, unmaps,
    /// records what was read; returns the tokens.
    pub fn finish_maps(&mut self) -> Vec<u32> {
        let mut tokens = Vec::new();
        for (b, size, dst, token) in std::mem::take(&mut self.pending_maps) {
            let Obj::Buffer(bf) = &mut self.objs[b] else {
                continue;
            };
            let bytes = bf.data[..size as usize].to_vec();
            bf.mapped = false;
            if dst != 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len())
                };
            }
            self.obs.push(Obs::Read(bytes));
            tokens.push(token);
        }
        tokens
    }

    /// The values a view sees (all its subresources, in order).
    pub fn view_values(&mut self, view: u32) -> Vec<u64> {
        let Some(a) = self.at(view, "(test) view_values") else {
            return Vec::new();
        };
        let Some(v) = self.view_of(a).cloned() else {
            return Vec::new();
        };
        let t = self.tex(v.tex);
        let mut out = Vec::new();
        for m in v.mip..v.mip + v.mips {
            for l in v.layer..v.layer + v.layers {
                out.extend_from_slice(&t.texels[t.sub(m, l)]);
            }
        }
        out
    }
}

/// Whether function `f` (or a function it calls) uses global `g`.
fn uses_global(
    m: &naga::Module,
    f: &naga::Function,
    g: naga::Handle<naga::GlobalVariable>,
) -> bool {
    let mut seen = Vec::new();
    uses_global_in(m, f, g, &mut seen)
}

fn uses_global_in(
    m: &naga::Module,
    f: &naga::Function,
    g: naga::Handle<naga::GlobalVariable>,
    seen: &mut Vec<naga::Handle<naga::Function>>,
) -> bool {
    for (_, e) in f.expressions.iter() {
        if let naga::Expression::GlobalVariable(h) = e {
            if *h == g {
                return true;
            }
        }
    }
    // calls
    fn calls(b: &naga::Block, out: &mut Vec<naga::Handle<naga::Function>>) {
        for s in b.iter() {
            match s {
                naga::Statement::Call { function, .. } => out.push(*function),
                naga::Statement::Block(b) => calls(b, out),
                naga::Statement::If { accept, reject, .. } => {
                    calls(accept, out);
                    calls(reject, out);
                }
                naga::Statement::Loop {
                    body, continuing, ..
                } => {
                    calls(body, out);
                    calls(continuing, out);
                }
                naga::Statement::Switch { cases, .. } => {
                    for c in cases {
                        calls(&c.body, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut cs = Vec::new();
    calls(&f.body, &mut cs);
    for c in cs {
        if seen.contains(&c) {
            continue;
        }
        seen.push(c);
        if uses_global_in(m, &m.functions[c], g, seen) {
            return true;
        }
    }
    false
}

// ---- the entry points render/gpu.rs calls (host/vngpu.js on the page) ----

fn s<'a>(p: *const u8, n: usize) -> &'a str {
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(p, n) }).unwrap_or("")
}

fn count(name: &'static str) {
    with(|m| *m.calls.entry(name).or_insert(0) += 1);
}

#[no_mangle]
pub extern "C" fn vngpu_attach(_t: usize) -> i32 {
    1
}
#[no_mangle]
pub extern "C" fn vngpu_start() {}
#[no_mangle]
pub extern "C" fn vngpu_canvas_format() -> u32 {
    0
}
#[no_mangle]
pub extern "C" fn vngpu_canvas_size(_w: u32, _h: u32) {}
#[no_mangle]
pub extern "C" fn vngpu_canvas_view() -> u32 {
    with(|m| {
        let t = m.texture(r#"{"size":[64,64],"format":"bgra8unorm","usage":16}"#);
        m.view(t, "")
    })
}
#[no_mangle]
pub extern "C" fn vngpu_has_feature(_p: *const u8, _n: usize) -> i32 {
    0
}
#[no_mangle]
pub extern "C" fn vngpu_drop(i: u32) {
    count("drop");
    with(|m| m.drop_obj(i))
}
#[no_mangle]
pub extern "C" fn vngpu_destroy(i: u32) {
    count("destroy");
    with(|m| m.destroy(i))
}
#[no_mangle]
pub extern "C" fn vngpu_texture(p: *const u8, n: usize) -> u32 {
    count("texture");
    with(|m| m.texture(s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_view(t: u32, p: *const u8, n: usize) -> u32 {
    count("view");
    with(|m| m.view(t, s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_buffer(size: u32, usage: u32) -> u32 {
    count("buffer");
    with(|m| m.buffer(size, usage))
}
#[no_mangle]
pub extern "C" fn vngpu_sampler(p: *const u8, n: usize) -> u32 {
    count("sampler");
    with(|m| m.sampler(s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_shader(p: *const u8, n: usize) -> u32 {
    count("shader");
    with(|m| m.shader(s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_bind_group_layout(p: *const u8, n: usize) -> u32 {
    count("bind_group_layout");
    with(|m| m.bind_group_layout(s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_pipeline_layout(p: *const u8, n: usize) -> u32 {
    count("pipeline_layout");
    with(|m| m.pipeline_layout(s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_render_pipeline(p: *const u8, n: usize) -> u32 {
    count("render_pipeline");
    with(|m| m.render_pipeline(s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_pipeline_group_layout(p: u32, i: u32) -> u32 {
    count("pipeline_group_layout");
    with(|m| m.pipeline_group_layout(p, i))
}
#[no_mangle]
pub extern "C" fn vngpu_bind_group(layout: u32, entries: *const u32, n: u32) -> u32 {
    count("bind_group");
    let w = unsafe { std::slice::from_raw_parts(entries, n as usize * 5) };
    with(|m| m.bind_group(layout, w))
}
#[no_mangle]
pub extern "C" fn vngpu_write_buffer(buf: u32, at: u32, p: *const u8, n: usize) {
    count("write_buffer");
    let b = unsafe { std::slice::from_raw_parts(p, n) };
    with(|m| m.write_buffer(buf, at, b))
}
#[no_mangle]
pub extern "C" fn vngpu_write_texture(
    tex: u32,
    mip: u32,
    x: u32,
    y: u32,
    z: u32,
    p: *const u8,
    n: usize,
    bpr: u32,
    rows: u32,
    w: u32,
    h: u32,
    d: u32,
) {
    count("write_texture");
    with(|m| m.write_texture(tex, mip, [x, y, z], p, n, bpr, rows, [w, h, d]))
}
#[no_mangle]
pub extern "C" fn vngpu_submit() {
    count("submit");
    with(|m| m.submit())
}
#[no_mangle]
pub extern "C" fn vngpu_on_done(_token: u32) {}
#[no_mangle]
pub extern "C" fn vngpu_read_buffer(buf: u32, size: u32, dst: *mut u8, token: u32) {
    count("read_buffer");
    with(|m| m.read_buffer(buf, size, dst, token))
}
#[no_mangle]
pub extern "C" fn vngpu_copy_texture(
    src: u32,
    smip: u32,
    sx: u32,
    sy: u32,
    sz: u32,
    dst: u32,
    dmip: u32,
    dx: u32,
    dy: u32,
    dz: u32,
    w: u32,
    h: u32,
    d: u32,
) {
    count("copy_texture");
    with(|m| m.copy_texture(src, smip, [sx, sy, sz], dst, dmip, [dx, dy, dz], [w, h, d]))
}
#[no_mangle]
pub extern "C" fn vngpu_copy_to_buffer(
    tex: u32,
    mip: u32,
    x: u32,
    y: u32,
    buf: u32,
    bpr: u32,
    w: u32,
    h: u32,
) {
    count("copy_to_buffer");
    with(|m| m.copy_to_buffer(tex, mip, x, y, buf, bpr, w, h))
}
#[no_mangle]
pub extern "C" fn vngpu_begin_pass(p: *const u8, n: usize) {
    count("begin_pass");
    with(|m| m.begin_pass(s(p, n)))
}
#[no_mangle]
pub extern "C" fn vngpu_end_pass() {
    count("end_pass");
    with(|m| m.end_pass())
}
#[no_mangle]
pub extern "C" fn vngpu_set_pipeline(p: u32) {
    with(|m| m.set_pipeline(p))
}
#[no_mangle]
pub extern "C" fn vngpu_set_bind_group(i: u32, g: u32, offs: *const u32, n: u32) {
    let o = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(offs, n as usize) }
    };
    with(|m| m.set_bind_group(i, g, o))
}
#[no_mangle]
pub extern "C" fn vngpu_set_vertex_buffer(slot: u32, buf: u32, off: u32) {
    with(|m| m.set_vertex_buffer(slot, buf, off))
}
#[no_mangle]
pub extern "C" fn vngpu_set_index_buffer(buf: u32, u32_: u32, off: u32) {
    with(|m| m.set_index_buffer(buf, u32_ != 0, off))
}
#[no_mangle]
pub extern "C" fn vngpu_set_viewport(x: f32, y: f32, w: f32, h: f32, z0: f32, z1: f32) {
    with(|m| m.set_viewport([x, y, w, h, z0, z1]))
}
#[no_mangle]
pub extern "C" fn vngpu_set_scissor(x: u32, y: u32, w: u32, h: u32) {
    with(|m| m.set_scissor([x, y, w, h]))
}
#[no_mangle]
pub extern "C" fn vngpu_set_blend_constant(r: f64, g: f64, b: f64, a: f64) {
    with(|m| m.set_blend([r, g, b, a]))
}
#[no_mangle]
pub extern "C" fn vngpu_set_stencil_ref(v: u32) {
    with(|m| m.set_stencil_ref(v))
}
#[no_mangle]
pub extern "C" fn vngpu_draw(n: u32, inst: u32, first: u32, first_inst: u32) {
    count("draw");
    with(|m| m.draw([n, inst, first, 0, first_inst], false))
}
#[no_mangle]
pub extern "C" fn vngpu_draw_indexed(n: u32, inst: u32, first: u32, base: i32, first_inst: u32) {
    count("draw_indexed");
    with(|m| m.draw([n, inst, first, base as u32, first_inst], true))
}
#[no_mangle]
pub extern "C" fn emscripten_get_now() -> f64 {
    0.0
}
