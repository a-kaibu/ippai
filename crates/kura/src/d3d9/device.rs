//! IDirect3D9, the device and its resources.

use super::format as fmt;
use super::gpu::{self, Batch};
use dxwgsl::d3d9::{self as shader, Shader};
use dxwgsl::d3d9ff as ff;
use super::{After, D3d, Data, D3DERR_INVALIDCALL, D3DERR_NOTAVAILABLE, E_NOTIMPL, S_OK};
use super::stateblock as sb;
use super::{IF_CUBE, IF_D3D9, IF_DECL, IF_DEVICE, IF_IB, IF_PS, IF_QUERY, IF_STATEBLOCK, IF_SURFACE, IF_SWAPCHAIN};
use super::{IF_TEXTURE, IF_VB, IF_VOLUME, IF_VOLUMETEX, IF_VS};
use std::collections::HashMap;

const POOL_DEFAULT: u32 = 0;
const POOL_SYSTEMMEM: u32 = 2;
const POOL_SCRATCH: u32 = 3;
const USAGE_RENDERTARGET: u32 = 1;
const USAGE_DEPTHSTENCIL: u32 = 2;
const LOCK_READONLY: u32 = 0x10;
const LOCK_DISCARD: u32 = 0x2000;

const RT_SURFACE: u32 = 1;
const RT_VOLUME: u32 = 2;
const RT_TEXTURE: u32 = 3;
const RT_VOLUMETEX: u32 = 4;
const RT_CUBETEX: u32 = 5;
const RT_VB: u32 = 6;
const RT_IB: u32 = 7;

pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub fmt: u32,
    pub pool: u32,
    pub usage: u32,
    /// page texture (0: none, system memory) and mip level
    pub gpu: u32,
    pub level: u32,
    pub own_gpu: bool,
    /// guest copy of the pixels (0: not made yet)
    pub mem: u32,
    pub pitch: u32,
    /// the page has newer pixels than `mem` (rendered to)
    pub gpu_newer: bool,
    pub lock: Option<[u32; 4]>,
    pub dc_bitmap: u32,
    pub dc: u32,
    pub container: u32,
    pub multisample: u32,
    /// an upload the page reads from `mem` (batch generation, rect): `mem` must not change
    /// until that batch is done
    pub pending: Option<(u64, [u32; 4])>,
    /// whole-surface locks so far, and from the second on, the pixels as last uploaded: an
    /// unlock then uploads only the box that changed (see changed_box)
    pub full_locks: u32,
    pub shadow: Option<Box<[u8]>>,
    /// a cube texture's face (D3DCUBEMAP_FACES): its layer of the cube's page texture, which
    /// gets the pixels with gpu::WRITE_TEXTURE2
    pub face: Option<u32>,
    /// a P8 surface: the device's `pal_gen` its page texels were made with (see refresh_p8)
    pub pal_gen: u64,
}

pub struct Texture {
    pub w: u32,
    pub h: u32,
    pub fmt: u32,
    pub pool: u32,
    pub usage: u32,
    pub gpu: u32,
    pub levels: Vec<u32>,
    pub lod: u32,
    /// a volume texture's depth (its levels are [`Volume`]s; 0: a 2D texture of surfaces)
    pub depth: u32,
    /// a cube texture's faces (6; 0: not a cube): `levels` are the surfaces face by face, each
    /// face's levels together (face * levels per face + level)
    pub faces: u32,
}

impl Texture {
    /// Mip levels (of each face, for a cube).
    pub fn level_count(&self) -> u32 {
        self.levels.len() as u32 / self.faces.max(1)
    }
}

/// A level of a volume texture: its texels in guest memory (made on first lock), slices of
/// `rows` rows `pitch` bytes apart, `slice` bytes apart; uploaded on unlock (the page texture
/// is 3D, one layer per slice: gpu::CREATE_TEXTURE2 dimension 1).
pub struct Volume {
    pub w: u32,
    pub h: u32,
    pub depth: u32,
    pub fmt: u32,
    pub pool: u32,
    pub usage: u32,
    pub gpu: u32,
    pub level: u32,
    pub container: u32,
    pub mem: u32,
    pub pitch: u32,
    pub slice: u32,
    /// the locked box (left, top, front, right, bottom, back; all 0: read only)
    pub lock: Option<[u32; 6]>,
}

pub struct Buffer {
    pub len: u32,
    pub usage: u32,
    /// FVF (vertex buffers) / index format
    pub fmt: u32,
    pub pool: u32,
    pub mem: u32,
    /// (#119) Its copy on the page (a page buffer, 0: none yet), for a buffer that is not
    /// D3DUSAGE_DYNAMIC: draws read that instead of their own copy of the bytes. `gen` counts
    /// its writable locks and their unlocks, `sent` is the `gen` the page has, `dirty` the
    /// bytes locked for writing since.
    pub gpu: u32,
    pub gen: u64,
    pub sent: u64,
    pub dirty: Option<(u32, u32)>,
    pub locked: bool,
    /// what the page copy holds, and a count of its changes (draws hash that: a buffer locked
    /// and written with the same bytes again leaves a still frame the same)
    pub shadow: Vec<u8>,
    pub cgen: u64,
}

/// A buffer whose page copy a draw may read (`Buffer::gpu`): not dynamic (the game writes those
/// every frame, often past what it locks); IPPAI_NORESIDENT=1: none.
fn resident(b: &Buffer) -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    b.usage & 0x200 == 0 && b.len >= 4 && !*OFF.get_or_init(|| std::env::var_os("IPPAI_NORESIDENT").is_some_and(|v| v != "0"))
}

/// The page copy of buffer `obj`, made or brought up to date with what was locked for writing
/// since it was sent (`Buffer::dirty`: the words of it that differ from what the page has):
/// its page buffer and the count of its changes.
fn page_buffer(objs: &mut HashMap<u32, super::Obj, super::FastHash>, obj: u32) -> (u32, u64) {
    let Some(Data::Buffer(b)) = objs.get_mut(&obj).map(|o| &mut o.data) else { return (0, 0) };
    let size = b.len.next_multiple_of(4);
    if b.gpu == 0 {
        b.gpu = crate::d3d11::page_id();
        b.dirty = Some((0, b.len));
        b.shadow = vec![0; size as usize];
        gpu::record(|q| q.cmd(&[gpu::CREATE_BUFFER, b.gpu, size]));
        b.cgen += 1;
        // (all of it once, zeros too: the page's buffer starts as zeros, so sent as they are)
        let bytes = unsafe { std::slice::from_raw_parts(b.mem as *const u8, b.len as usize) };
        b.shadow[..bytes.len()].copy_from_slice(bytes);
        gpu::record(|q| {
            let off = q.blob(&b.shadow);
            q.cmd(&[gpu::WRITE_BUFFER, b.gpu, 0, off, size]);
        });
        RESIDENT_SENT.fetch_add(size as u64, std::sync::atomic::Ordering::Relaxed);
        b.dirty = None;
        b.sent = b.gen;
    }
    if let Some((lo, hi)) = b.dirty.take() {
        // (whole words; only from the first to the last that changed)
        let (lo, hi) = ((lo & !3) as usize, (hi.min(b.len) as usize).next_multiple_of(4).min(size as usize));
        if hi > lo {
            let now = unsafe { std::slice::from_raw_parts((b.mem as usize + lo) as *const u8, (hi - lo).min(b.len as usize - lo)) };
            let old = &b.shadow[lo..lo + now.len()];
            if let (Some(a), Some(z)) = (now.iter().zip(old).position(|(x, y)| x != y), now.iter().zip(old).rposition(|(x, y)| x != y)) {
                let (a, z) = (a & !3, (z + 1).next_multiple_of(4).min(now.len()));
                b.shadow[lo + a..lo + z].copy_from_slice(&now[a..z]);
                let words = &b.shadow[lo + a..(lo + z).next_multiple_of(4)];
                gpu::record(|q| {
                    let off = q.blob(words);
                    q.cmd(&[gpu::WRITE_BUFFER, b.gpu, (lo + a) as u32, off, words.len() as u32]);
                });
                RESIDENT_SENT.fetch_add(words.len() as u64, std::sync::atomic::Ordering::Relaxed);
                b.cgen += 1;
            }
        }
    }
    b.sent = b.gen;
    (b.gpu, b.cgen)
}

/// In a DRAW's (buffer, offset) pairs: the data block
pub const DATA: u32 = 0xffff_ffff;
/// a pair's buffer marked so: the indices' (the page buffers' numbers are below it)
pub const INDEX_PAIR: u32 = 0x8000_0000;

/// Bytes sent to page copies of buffers, and bytes draws read from them instead of copying
/// (#119; stats)
pub static RESIDENT_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RESIDENT_READ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Elem {
    pub stream: u32,
    pub offset: u32,
    pub ty: u32,
    pub usage: u32,
    pub index: u32,
}

pub struct Decl {
    pub elems: Vec<Elem>,
    pub raw: Vec<[u32; 2]>,
    pub fvf: u32,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ModKey {
    Vs(u32, Vec<(u32, u32, u32)>),
    Ps(u32, u32, Option<u32>, Vec<dxwgsl::addr::Addr>, u32),
    FfVs(ff::VsKey),
    FfPs(ff::PsKey),
}

pub struct ShaderObj {
    pub code: Vec<u32>,
    pub parsed: Option<Shader>,
    pub serial: u32,
}

#[derive(Clone)]
pub struct State {
    pub rs: Vec<u32>,
    pub tss: [[u32; 33]; 8],
    /// (samplers and textures by `sampler_slot`: 0-15, the displacement map, vertex textures 0-3)
    pub ss: [[u32; 14]; SAMPLERS],
    pub textures: [u32; SAMPLERS],
    pub streams: [(u32, u32, u32); 16],
    pub freq: [u32; 16],
    pub indices: u32,
    pub fvf: u32,
    pub decl: u32,
    pub vs: u32,
    pub ps: u32,
    pub vsf: Vec<[f32; 4]>,
    pub psf: Vec<[f32; 4]>,
    pub vsi: [[i32; 4]; 16],
    pub psi: [[i32; 4]; 16],
    pub vsb: u32,
    pub psb: u32,
    pub transforms: HashMap<u32, [f32; 16]>,
    pub viewport: [u32; 6],
    pub scissor: [i32; 4],
    pub material: [f32; 17],
    pub lights: HashMap<u32, (Vec<u8>, bool)>,
    pub clip: [[f32; 4]; 6],
    /// SetNPatchMode's segments (f32 bits)
    pub npatch: u32,
    /// a software vertex processing device's integer and boolean vertex shader constants past
    /// the first 16 (2048 of each there)
    pub vsi_more: Vec<[i32; 4]>,
    pub vsb_more: Vec<u32>,
}

pub const SAMPLERS: usize = 21;

/// A sampler's (or texture stage's) slot: 0-15, D3DDMAPSAMPLER (256), D3DVERTEXTEXTURESAMPLER0-3
/// (257-260)
pub fn sampler_slot(s: u32) -> Option<usize> {
    match s {
        0..=15 => Some(s as usize),
        256..=260 => Some(s as usize - 240),
        _ => None,
    }
}

pub type Snapshot = State;

pub struct Device {
    pub obj: u32,
    pub d3d: u32,
    pub hwnd: u32,
    pub pp: [u32; 14],
    pub behavior: u32,
    pub backbuffer: u32,
    pub swapchain: u32,
    pub auto_depth: u32,
    pub rt: [u32; 4],
    pub ds: u32,
    pub st: State,
    pub recording: Option<Box<super::stateblock::Recording>>,
    /// draw state the page has (per batch)
    batch_target: Option<(u32, u32, u32)>,
    pipelines: HashMap<Vec<u32>, u32, super::FastHash>,
    shaders: HashMap<String, u32>,
    /// shader modules by what their WGSL is made from (no WGSL generated for a draw that has one)
    modules: HashMap<ModKey, u32, super::FastHash>,
    next_gpu: u32,
    serial: u32,
    readbacks: u32,
    pub dummy: u32,
    /// the last uniform blocks written in this batch (bytes, offset)
    last_vs: (Vec<u8>, u32),
    last_ps: (Vec<u8>, u32),
    batch_gen: u64,
    /// the batch `last_vs` / `last_ps` were written to
    uniform_gen: u64,
    pub in_scene: bool,
    pub cursor: bool,
    pub gamma: Vec<u8>,
    pub frames: u64,
    /// the declarations GetVertexDeclaration hands out for FVFs (fvf -> object id)
    pub fvf_decls: HashMap<u32, u32>,
    /// SetPaletteEntries' palettes (PALETTEENTRY red, green, blue, flags: the alpha) by number,
    /// SetCurrentTexturePalette's, and a count that changes whenever the palette P8 textures
    /// read does (their page texels are made again at the next draw: refresh_p8)
    pub palettes: HashMap<u32, Box<[u8; 1024]>>,
    pub cur_palette: u32,
    pub pal_gen: u64,
    /// Counts the calls that may change what a draw prepares from the device's state (its
    /// layout, shaders, pipeline: `Prep`) and the calls that may change its uniform blocks;
    /// `stale` says which (#118).
    pub prep_gen: u64,
    pub uni_gen: u64,
    /// the last draw's preparation, and its uniform blocks
    prep: Option<Box<Prep>>,
    uni: Option<Box<Uni>>,
}

/// The render states Direct3D 9 has (GetRenderState of the others: D3DERR_INVALIDCALL), as
/// Windows answers (winref d3d9state)
pub const RS_VALID: [u64; 4] = [0xfff5_857c_3fd9_c381, 0xffff_ffff_0000_0000, 0xffff_ffef_fdfe_ffff, 0xffff_ffff_ffff_ffcf];
pub fn rs_valid(i: u32) -> bool {
    i < 256 && RS_VALID[i as usize / 64] >> (i % 64) & 1 != 0
}
/// (what Windows has in the states no one sets: legacy and unused ones)
const UNSET: u32 = 0xbaad_cafe;

fn default_state() -> State {
    let mut rs = vec![0u32; 256];
    let f1 = 1.0f32.to_bits();
    for i in [0, 42, 47, 50, 61, 62, 63, 138, 149, 150, 165, 169, 177].into_iter().chain(96..128).chain(210..256) {
        rs[i] = UNSET;
    }
    // (Windows' defaults, winref d3d9state; ZENABLE as the auto depth buffer: set at creation)
    for (k, v) in [
        (7, 1),    // ZENABLE
        (8, 3),    // FILLMODE solid
        (9, 2),    // SHADEMODE gouraud
        (14, 1),   // ZWRITEENABLE
        (16, 1),   // LASTPIXEL
        (19, 2),   // SRCBLEND one
        (20, 1),   // DESTBLEND zero
        (22, 3),   // CULLMODE ccw
        (23, 4),   // ZFUNC lessequal
        (25, 8),   // ALPHAFUNC always
        (37, f1),  // FOGEND
        (38, f1),  // FOGDENSITY
        (53, 1),   // STENCILFAIL keep
        (54, 1),
        (55, 1),
        (56, 8),   // STENCILFUNC always
        (58, 0xffffffff),
        (59, 0xffffffff),
        (60, 0xffffffff), // TEXTUREFACTOR
        (136, 1),  // CLIPPING
        (137, 1),  // LIGHTING
        (141, 1),  // COLORVERTEX
        (142, 1),  // LOCALVIEWER
        (145, 1),  // DIFFUSEMATERIALSOURCE color1
        (146, 2),  // SPECULARMATERIALSOURCE color2
        (154, f1), // POINTSIZE
        (155, f1), // POINTSIZE_MIN
        (158, f1), // POINTSCALE_A
        (161, 1),  // MULTISAMPLEANTIALIAS
        (162, 0xffffffff),
        (166, 8192f32.to_bits()), // POINTSIZE_MAX (software and hardware devices alike)
        (168, 0xf), // COLORWRITEENABLE
        (171, 1),  // BLENDOP add
        (172, 3),  // POSITIONDEGREE cubic
        (173, 1),  // NORMALDEGREE linear
        (178, f1), // MINTESSELLATIONLEVEL
        (179, f1),
        (182, f1), // ADAPTIVETESS_Z
        (186, 1),  // CCW_STENCILFAIL keep
        (187, 1),
        (188, 1),
        (189, 8),  // CCW_STENCILFUNC always
        (190, 0xf),
        (191, 0xf),
        (192, 0xf),
        (193, 0xffffffff), // BLENDFACTOR
        (207, 2),  // SRCBLENDALPHA one
        (208, 1),
        (209, 1),
    ] {
        rs[k] = v;
    }
    let mut tss = [[0u32; 33]; 8];
    for (i, s) in tss.iter_mut().enumerate() {
        s[1] = if i == 0 { 4 } else { 1 }; // COLOROP
        s[2] = 2; // COLORARG1 texture
        s[3] = 1; // COLORARG2 current
        s[4] = if i == 0 { 2 } else { 1 }; // ALPHAOP
        s[5] = 2;
        s[6] = 1;
        s[11] = i as u32; // TEXCOORDINDEX
        s[26] = 1;
        s[27] = 1;
        s[28] = 1; // RESULTARG current
        for t in [0, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 25, 29, 30, 31] {
            s[t] = UNSET;
        }
    }
    let mut ss = [[0u32; 14]; SAMPLERS];
    for s in ss.iter_mut() {
        s[1] = 1;
        s[2] = 1;
        s[3] = 1;
        s[5] = 1; // MAGFILTER point
        s[6] = 1;
        s[7] = 0;
        s[10] = 1;
        s[0] = UNSET;
    }
    State {
        rs,
        tss,
        ss,
        textures: [0; SAMPLERS],
        streams: [(0, 0, 0); 16],
        freq: [1; 16],
        indices: 0,
        fvf: 0,
        decl: 0,
        vs: 0,
        ps: 0,
        vsf: vec![[0.0; 4]; 256],
        psf: vec![[0.0; 4]; 224],
        vsi: [[0; 4]; 16],
        psi: [[0; 4]; 16],
        vsb: 0,
        psb: 0,
        transforms: HashMap::new(),
        viewport: [0, 0, 0, 0, 0, 1f32.to_bits()],
        scissor: [0; 4],
        material: [0.0; 17],
        lights: HashMap::new(),
        clip: [[0.0; 4]; 6],
        npatch: 0,
        vsi_more: Vec::new(),
        vsb_more: Vec::new(),
    }
}

impl Device {
    pub fn release_all(self: Box<Self>, d: &mut D3d) {
        let st = &self.st;
        let mut held: Vec<u32> = st.textures.iter().copied().filter(|&t| t != 0).collect();
        held.extend(st.streams.iter().map(|s| s.0).filter(|&s| s != 0));
        held.extend([st.indices, st.decl, st.vs, st.ps].into_iter().filter(|&x| x != 0));
        held.extend(self.rt.iter().copied().filter(|&x| x != 0));
        if self.ds != 0 {
            held.push(self.ds);
        }
        for h in held {
            d.unbind(h);
        }
        for s in [self.backbuffer, self.auto_depth, self.swapchain] {
            if s != 0 {
                d.unbind(s);
            }
        }
        if self.dummy != 0 {
            d.unbind(self.dummy);
            d.release(self.dummy);
        }
        // (the reference CreateDevice took on the IDirect3D9)
        d.release(self.d3d);
    }
}

unsafe fn rd(p: u32, i: usize) -> u32 {
    *((p as *const u32).add(i))
}

unsafe fn wr(p: u32, i: usize, v: u32) {
    if p != 0 {
        *((p as *mut u32).add(i)) = v;
    }
}

/// Size of the guest's pointers (and HWNDs).
pub(crate) static PTR_SIZE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(4);

/// Size of the guest's pointers (4, or 8 for 64-bit programs: set by the embedder).
#[inline(always)]
pub(crate) fn ptr() -> u32 {
    PTR_SIZE.load(std::sync::atomic::Ordering::Relaxed)
}

/// A pointer-sized output (interfaces, mapped memory): 4 or 8 bytes.
pub(crate) unsafe fn wrp(p: u32, i: usize, v: u32) {
    wr(p, i * (ptr() / 4) as usize, v);
    if ptr() == 8 {
        wr(p, i * 2 + 1, 0);
    }
}

/// D3DPRESENT_PARAMETERS as 14 dwords in its 32-bit layout (hDeviceWindow at 7; 64-bit: 8-byte
/// aligned at dword 8, the rest 2 dwords later)
fn pp_index(i: usize) -> usize {
    if ptr() == 8 && i >= 7 { i + 1 + (i > 7) as usize } else { i }
}
unsafe fn read_pp(pp: u32) -> [u32; 14] {
    let mut p = [0u32; 14];
    for (i, v) in p.iter_mut().enumerate() {
        *v = rd(pp, pp_index(i));
    }
    p
}
unsafe fn write_pp(pp: u32, p: &[u32; 14]) {
    for (i, v) in p.iter().enumerate() {
        wr(pp, pp_index(i), *v);
    }
    if ptr() == 8 {
        wr(pp, 9, 0);
    }
}

fn f2u(v: f32) -> u32 {
    v.to_bits()
}

/// Whether a call may change what draws prepare from the device's state (`Prep`: the vertex
/// layout, shaders, pipeline, samplers) and their uniform blocks (`Uni`), before it runs: the
/// calls named here change neither (a value set again, the uniforms' own states, getters,
/// locks); every other call may (#118). The first is the second too.
unsafe fn stale(d: &D3d, iface: u32, m: u32, a: &dyn Fn(usize) -> u32) -> (bool, bool) {
    const BOTH: (bool, bool) = (true, true);
    const UNI: (bool, bool) = (false, true);
    const NONE: (bool, bool) = (false, false);
    match iface {
        // (their contents: the draws read those again, and textures' and surfaces' page
        // textures and formats come from the objects at each draw)
        IF_VB | IF_IB | IF_TEXTURE | IF_SURFACE => return NONE,
        IF_DEVICE => {}
        _ => return BOTH,
    }
    let Some(dev) = d.dev.as_ref() else { return BOTH };
    // (a state block being recorded takes the Set* calls: conservative)
    if dev.recording.is_some() {
        return BOTH;
    }
    let st = &dev.st;
    match m {
        // BeginScene, EndScene, Clear, SetScissorRect, draws (the scissor is each draw's own)
        41 | 42 | 43 | 75 | 81..=84 => NONE,
        // getters
        45 | 48 | 50 | 52 | 54 | 56 | 58 | 64 | 66 | 68 | 76 | 78 | 80 | 88 | 90 | 93 | 95 | 97 | 99 | 101 | 103 | 105 | 108 | 110 | 112 | 114 => NONE,
        // SetTransform, MultiplyTransform, SetViewport, SetMaterial, SetLight (not which are on),
        // the shader constants
        44 | 46 | 47 | 49 | 51 | 94 | 96 | 98 | 109 | 111 | 113 => UNI,
        57 => match st.rs.get(a(1) as usize) {
            Some(&v) if v == a(2) => NONE,
            // ALPHAREF, TEXTUREFACTOR: uniforms; BLENDFACTOR: each draw's own
            Some(_) if a(1) == 24 || a(1) == 60 => UNI,
            Some(_) if a(1) == 193 => NONE,
            _ => BOTH,
        },
        67 => match st.tss.get(a(1) as usize).and_then(|s| s.get(a(2) as usize)) {
            Some(&v) if v == a(3) => NONE,
            // D3DTSS_CONSTANT: a uniform
            Some(_) if a(2) == 32 => UNI,
            _ => BOTH,
        },
        69 => match sampler_slot(a(1)).and_then(|s| st.ss[s].get(a(2) as usize).map(|&v| (s, v))) {
            Some((_, v)) if v == a(3) => NONE,
            // (the filters, ADDRESSW, LOD and the rest: each draw's sampler; ADDRESSU/V, BORDERCOLOR
            // and MAGFILTER only when they change the addressing the pixel shader does)
            Some((s, _)) => {
                let mut ss = st.ss[s];
                let addr = |ss: &[u32; 14]| {
                    let f = dxwgsl::addr::flags(ss[1], ss[2], ss[5] >= 2);
                    (f, if f != 0 { ss[4] } else { 0 })
                };
                let before = addr(&ss);
                ss[a(2) as usize] = a(3);
                if addr(&ss) == before { NONE } else { BOTH }
            }
            None => BOTH,
        },
        // SetTexture: another texture of the same kind (its page texture and format are each
        // draw's own)
        65 => {
            let kind = |t: u32| d.objs.get(&t).map(|o| o.iface);
            match sampler_slot(a(1)) {
                Some(s) if kind(st.textures[s]) == kind(d.id_of(a(2))) => NONE,
                _ => BOTH,
            }
        }
        // SetFVF (0: ignored; the one in use: no declaration either)
        89 if a(1) == 0 || st.fvf == a(1) && st.decl == 0 => NONE,
        // SetVertexDeclaration (the one in use, with its FVF)
        87 if st.decl == d.id_of(a(1)) && matches!(d.objs.get(&st.decl).map(|o| &o.data), Some(Data::Decl(dc)) if dc.fvf == st.fvf) => NONE,
        92 if st.vs == d.id_of(a(1)) => NONE,
        107 if st.ps == d.id_of(a(1)) => NONE,
        // SetStreamSource: another buffer or offset (each draw's own), the same stride
        100 => match st.streams.get(a(1) as usize) {
            Some(&(_, _, stride)) if stride == a(4) => NONE,
            _ => BOTH,
        },
        102 if st.freq.get(a(1) as usize) == Some(&a(2)) => NONE,
        // SetIndices: the index format is each draw's own
        104 => NONE,
        _ => BOTH,
    }
}

pub unsafe fn call(d: &mut D3d, iface: u32, m: u32, this: u32, args: *const u32, after: &mut After) -> u32 {
    let a = |i: usize| *args.add(i);
    let (p, u) = stale(d, iface, m, &a);
    if let Some(dev) = d.dev.as_mut() {
        dev.prep_gen += p as u64;
        dev.uni_gen += u as u64;
    }
    match iface {
        IF_D3D9 => d3d9_call(d, m, this, &a),
        IF_DEVICE => device_call(d, m, &a, after),
        IF_SWAPCHAIN => match m {
            3 => present(d, after, a(1), a(2), a(3)),
            4 => D3DERR_INVALIDCALL,
            5 => get_backbuffer(d, a(3)),
            6 => raster_status(d, a(1)),
            7 => display_mode(a(1)),
            8 => device_out(d, a(1)),
            9 => {
                let Some(dev) = &d.dev else { return D3DERR_INVALIDCALL };
                write_pp(a(1), &dev.pp);
                S_OK
            }
            _ => E_NOTIMPL,
        },
        IF_SURFACE => surface_call(d, m, this, &a, after),
        IF_TEXTURE | IF_CUBE | IF_VOLUMETEX => texture_call(d, iface, m, this, &a, after),
        IF_VB | IF_IB => buffer_call(d, iface, m, this, &a),
        IF_DECL => match m {
            3 => device_out(d, a(1)),
            4 => {
                let Some(Data::Decl(decl)) = d.objs.get(&this).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
                if a(2) == 0 {
                    return D3DERR_INVALIDCALL;
                }
                if a(1) != 0 {
                    std::ptr::copy_nonoverlapping(decl.raw.as_ptr() as *const u32, a(1) as *mut u32, decl.raw.len() * 2);
                }
                wr(a(2), 0, decl.raw.len() as u32);
                S_OK
            }
            _ => E_NOTIMPL,
        },
        IF_VS | IF_PS => match m {
            3 => device_out(d, a(1)),
            4 => {
                let Some(Data::Shader(s)) = d.objs.get(&this).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
                // (Windows does not read *size: a buffer with a size of 0 gets the code too,
                // replay_s2020_wonder_fool_maokatsu; NULL data asks for the size)
                if a(2) == 0 {
                    return D3DERR_INVALIDCALL;
                }
                let size = s.code.len() as u32 * 4;
                if a(1) != 0 {
                    std::ptr::copy_nonoverlapping(s.code.as_ptr(), a(1) as *mut u32, s.code.len());
                }
                wr(a(2), 0, size);
                S_OK
            }
            _ => E_NOTIMPL,
        },
        IF_STATEBLOCK => match m {
            3 => device_out(d, a(1)),
            4 => {
                // Capture
                let Some(dev) = &d.dev else { return D3DERR_INVALIDCALL };
                if let Some(Data::StateBlock(b)) = d.objs.get_mut(&this).map(|o| &mut o.data) {
                    let old = state_binds(&b.st);
                    sb::copy_masked(&mut b.st, &dev.st, &b.mask);
                    let new = state_binds(&b.st);
                    bind_all(d, &new);
                    unbind_all(d, &old);
                }
                S_OK
            }
            5 => {
                if !matches!(d.objs.get(&this).map(|o| &o.data), Some(Data::StateBlock(_))) {
                    return D3DERR_INVALIDCALL;
                }
                apply_state(d, this);
                S_OK
            }
            _ => E_NOTIMPL,
        },
        IF_QUERY => match m {
            3 => device_out(d, a(1)),
            4 => match d.objs.get(&this).map(|o| &o.data) {
                Some(Data::Query(t)) => *t,
                _ => 0,
            },
            5 => query_size(match d.objs.get(&this).map(|o| &o.data) {
                Some(Data::Query(t)) => *t,
                _ => 0,
            }),
            6 => S_OK,
            7 => {
                let t = match d.objs.get(&this).map(|o| &o.data) {
                    Some(Data::Query(t)) => *t,
                    _ => 0,
                };
                let n = query_size(t).min(a(2));
                if a(1) != 0 && n > 0 {
                    std::ptr::write_bytes(a(1) as *mut u8, 0, n as usize);
                    if t == 8 || t == 9 {
                        // event: done; occlusion: something drawn
                        wr(a(1), 0, 1);
                    }
                    if t == 9 {
                        wr(a(1), 0, 1000);
                    }
                }
                S_OK
            }
            _ => E_NOTIMPL,
        },
        IF_VOLUME => volume_call(d, m, this, &a),
        _ => E_NOTIMPL,
    }
}

fn query_size(t: u32) -> u32 {
    match t {
        8 => 4,     // EVENT: BOOL
        9 => 4,     // OCCLUSION: DWORD
        10 => 8,    // TIMESTAMP
        11 => 4,    // TIMESTAMPDISJOINT
        12 => 8,    // TIMESTAMPFREQ
        _ => 4,
    }
}

unsafe fn device_out(d: &mut D3d, out: u32) -> u32 {
    let Some(dev) = &d.dev else { return D3DERR_INVALIDCALL };
    let obj = dev.obj;
    d.add_ref(obj);
    wrp(out, 0, d.addr_of(obj));
    S_OK
}

/// The modes the adapter lists: the usual ones that fit the screen, and the screen's own (games
/// pick a window size from these; AdvHD's settings dialog wants more than one).
/// GetRasterStatus: a 60 Hz beam from the clock (games pace frames on it: BGI waits for the
/// scanline to pass mid-screen, and a beam that never moves hangs it).
unsafe fn raster_status(d: &D3d, out: u32) -> u32 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    if out == 0 {
        return D3DERR_INVALIDCALL;
    }
    let h = d.dev.as_ref().map_or(768, |dev| dev.pp[1].max(1));
    let total = h + h / 20 + 1; // ~5% of the lines in vertical blank
    let phase = (T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 60.0).fract();
    let line = (phase * total as f64) as u32;
    let vblank = line >= h;
    wr(out, 0, vblank as u32);
    wr(out, 1, if vblank { 0 } else { line });
    S_OK
}

unsafe fn adapter_modes() -> Vec<(u32, u32)> {
    let (mut w, mut h) = (0, 0);
    crate::display::vnhost_display_size(&mut w, &mut h);
    let (w, h) = (w.max(640) as u32, h.max(480) as u32);
    let mut v: Vec<(u32, u32)> = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 768), (1280, 800), (1280, 960),
        (1280, 1024), (1366, 768), (1600, 900), (1920, 1080)]
        .into_iter()
        .filter(|&(x, y)| x <= w && y <= h)
        .collect();
    if !v.contains(&(w, h)) {
        v.push((w, h));
    }
    v.sort_unstable();
    v
}

unsafe fn display_mode(out: u32) -> u32 {
    let (mut w, mut h) = (0, 0);
    crate::display::vnhost_display_size(&mut w, &mut h);
    wr(out, 0, w as u32);
    wr(out, 1, h as u32);
    wr(out, 2, 60);
    wr(out, 3, fmt::X8R8G8B8);
    S_OK
}

// ---------------------------------------------------------------------------------------------
// IDirect3D9

const CAPS: [u32; 76] = {
    let mut c = [0u32; 76];
    c[0] = 1; // HAL
    c[2] = 0x0002_0000; // Caps: READ_SCANLINE
    c[3] = 0x0002_0000 | 0x2000_0000 | 0x4000_0000; // Caps2: CANAUTOGENMIPMAP, FULLSCREENGAMMA, DYNAMICTEXTURES
    c[4] = 0x20 | 0x80 | 0x100 | 0x200; // Caps3: ALPHA_FULLSCREEN_FLIP_OR_DISCARD, COPY_TO_VIDMEM, COPY_TO_SYSTEMMEM, ...
    c[5] = 0x8000_0000 | 1; // PresentationIntervals: IMMEDIATE | ONE
    c[6] = 1; // CursorCaps: COLOR
    c[7] = 0x10 | 0x40 | 0x80 | 0x200 | 0x400 | 0x2000 | 0x4000 | 0x8_0000 | 0x10_0000 | 0x20_0000; // DevCaps
    // PrimitiveMiscCaps (not 0x1000, NULLREFERENCE: the D3D sample framework then refuses the device).
    // TSSARGTEMP (0x400): the fixed-function stages have D3DTA_TEMP and RESULTARG (dxwgsl d3d9ff);
    // a game that checks it quits without it (ダンジョンズ＆シネステジア: 「テクスチャステージで
    // テンポラリレジスタが使用できません」)
    c[8] = 0x2 | 0x4 | 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x400 | 0x800 | 0x2_0000 | 0x4_0000;
    // RasterCaps (SCISSORTEST: games check it before they use the scissor rect)
    c[9] = 0x1 | 0x10 | 0x80 | 0x100 | 0x2000 | 0x8_0000 | 0x20_0000 | 0x40_0000 | 0x100_0000 | 0x200_0000 | 0x400_0000;
    c[10] = 0xff; // ZCmpCaps
    // Src/DestBlendCaps: through BLENDFACTOR (0x2000; no dual-source SRCCOLOR2)
    c[11] = 0x3fff;
    c[12] = 0x3fff;
    c[13] = 0xff; // AlphaCmpCaps
    c[14] = 0x8 | 0x80 | 0x200 | 0x4000 | 0x8_0000; // ShadeCaps
    // TextureCaps: PERSPECTIVE, ALPHA, TEXREPEATNOTSCALEDBYSIZE, CUBEMAP, VOLUMEMAP, MIPMAP, MIPVOLUMEMAP;
    // no POW2, CUBEMAP_POW2 or VOLUMEMAP_POW2: WebGPU textures have any size (AdvHD refuses to
    // start without that)
    c[15] = 0x1 | 0x4 | 0x40 | 0x800 | 0x2000 | 0x4000 | 0x8000;
    c[16] = 0x0300_0000 | 0x0001_0000 | 0x0002_0000 | 0x0100 | 0x0200 | 0x0400; // TextureFilterCaps
    c[17] = c[16];
    c[18] = c[16];
    c[19] = 0x1f | 0x20; // TextureAddressCaps
    c[20] = 0x3f;
    c[21] = 0x1f; // LineCaps
    c[22] = 8192;
    c[23] = 8192;
    c[24] = 2048;
    c[25] = 8192;
    c[26] = 8192;
    c[27] = 16;
    c[28] = 0x4b18_9680; // MaxVertexW 1e7
    c[29] = 0xc6c0_0000; // guard band -24576
    c[30] = 0xc6c0_0000;
    c[31] = 0x46c0_0000;
    c[32] = 0x46c0_0000;
    c[34] = 0x1ff; // StencilCaps
    c[35] = 8 | 0x8_0000; // FVFCaps: 8 texcoords, PSIZE
    c[36] = 0x03ff_ffff; // TextureOpCaps
    c[37] = 8;
    c[38] = 8;
    c[39] = 0x1 | 0x2 | 0x8 | 0x10 | 0x20 | 0x100; // VertexProcessingCaps
    c[40] = 8;
    c[41] = 6;
    c[42] = 4;
    c[43] = 0;
    c[44] = 0x4200_0000; // MaxPointSize 32
    c[45] = 0xffffff;
    c[46] = 0xffffff;
    c[47] = 16;
    c[48] = 1024;
    c[49] = 0xfffe0300; // vs 3.0
    c[50] = 256;
    c[51] = 0xffff0300; // ps 3.0
    c[52] = 0x4100_0000; // PixelShader1xMaxValue 8
    c[53] = 0x1 | 0x10 | 0x20 | 0x40; // DevCaps2: STREAMOFFSET, VERTEXELEMENTSCANSHARESTREAMOFFSET, CAN_STRETCHRECT_FROM_TEXTURES
    c[58] = 1;
    c[59] = 0x3ff; // DeclTypes
    c[60] = 1; // NumSimultaneousRTs
    c[61] = 0x0300_0000 | 0x0001_0000 | 0x0002_0000 | 0x0100 | 0x0200;
    c[62] = 1; // VS20Caps.Caps: PREDICATION
    c[63] = 24;
    c[64] = 32;
    c[65] = 4;
    c[66] = 0x1f; // PS20Caps.Caps
    c[67] = 24;
    c[68] = 32;
    c[69] = 4;
    c[70] = 512;
    c[71] = 0x0300_0000 | 0x0100 | 0x0200;
    c[72] = 65535;
    c[73] = 65535;
    c[74] = 32768;
    c[75] = 32768;
    c
};

unsafe fn d3d9_call(d: &mut D3d, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    match m {
        3 => S_OK,
        4 => 1, // GetAdapterCount
        5 => {
            // GetAdapterIdentifier
            let p = a(3) as *mut u8;
            if p.is_null() {
                return D3DERR_INVALIDCALL;
            }
            std::ptr::write_bytes(p, 0, 1100);
            let put = |off: usize, s: &[u8]| std::ptr::copy_nonoverlapping(s.as_ptr(), p.add(off), s.len());
            put(0, b"ippai.dll");
            put(512, b"ippai WebGPU");
            put(1024, b"\\\\.\\DISPLAY1");
            // DriverVersion (1056), VendorId 1064, DeviceId 1068
            *(p.add(1056) as *mut u32) = 0x0001_0000;
            *(p.add(1060) as *mut u32) = 0x0006_000e;
            *(p.add(1064) as *mut u32) = 0x10de;
            *(p.add(1068) as *mut u32) = 0x1c82;
            *(p.add(1088) as *mut u32) = 0x5642_4e56;
            S_OK
        }
        6 => {
            if a(2) == fmt::X8R8G8B8 || a(2) == fmt::A8R8G8B8 { adapter_modes().len() as u32 } else { 0 }
        }
        7 => {
            let modes = adapter_modes();
            let Some(&(w, h)) = modes.get(a(3) as usize) else { return D3DERR_INVALIDCALL };
            if a(2) != fmt::X8R8G8B8 && a(2) != fmt::A8R8G8B8 {
                return D3DERR_INVALIDCALL;
            }
            wr(a(4), 0, w);
            wr(a(4), 1, h);
            wr(a(4), 2, 60);
            wr(a(4), 3, a(2));
            S_OK
        }
        8 => display_mode(a(2)),
        9 => {
            // CheckDeviceType(adapter, type, adapter fmt, backbuffer fmt, windowed)
            if a(3) == fmt::X8R8G8B8 || a(3) == fmt::A8R8G8B8 || a(3) == 0 { S_OK } else { D3DERR_NOTAVAILABLE }
        }
        10 => {
            // CheckDeviceFormat(adapter, type, adapter fmt, usage, rtype, fmt)
            let (usage, rtype, f) = (a(4), a(5), a(6));
            // (as Windows answers: no D32 depth buffers and no R8G8B8 textures, which GPUs
            // have not had for long; creating them still works)
            let ok = if usage & USAGE_DEPTHSTENCIL != 0 {
                fmt::is_depth(f) && f != fmt::D32
            } else if usage & USAGE_RENDERTARGET != 0 {
                fmt::renderable(f)
            } else if rtype == RT_VB || rtype == RT_IB {
                true
            } else {
                fmt::supported(f) && !fmt::is_depth(f) && f != fmt::R8G8B8 || (fmt::is_depth(f) && rtype == RT_SURFACE)
            };
            if ok { S_OK } else { D3DERR_NOTAVAILABLE }
        }
        11 => {
            if a(6) != 0 {
                wr(a(6), 0, 1);
            }
            if a(5) == 0 { S_OK } else { D3DERR_NOTAVAILABLE }
        }
        12 => S_OK,
        13 => S_OK,
        14 => {
            let mut c = CAPS;
            c[0] = a(2).max(1);
            std::ptr::copy_nonoverlapping(c.as_ptr(), a(3) as *mut u32, c.len());
            S_OK
        }
        // GetAdapterMonitor: win32u numbers monitors from 1 (sysparams.c), the only one is 1
        15 => 1,
        16 => create_device(d, this, a(2), a(3), a(4), a(5), a(6)),
        _ => E_NOTIMPL,
    }
}

impl Device {
    /// Whatever draws prepared from the state is to be prepared again (`stale`).
    pub fn stale_all(&mut self) {
        self.prep_gen += 1;
        self.uni_gen += 1;
    }

    fn new_gpu(&mut self) -> u32 {
        self.next_gpu += 1;
        self.next_gpu
    }
}

static GPU_IDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn gpu_id() -> u32 {
    GPU_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A page texture: id
fn gpu_texture(w: u32, h: u32, levels: u32, f: u32, usage: u32) -> u32 {
    let id = gpu_id();
    let rt = usage & (USAGE_RENDERTARGET | USAGE_DEPTHSTENCIL) != 0 || fmt::is_depth(f);
    gpu::record(|b| b.cmd(&[gpu::CREATE_TEXTURE, id, w.max(1), h.max(1), levels.max(1), fmt::page_format(f), rt as u32]));
    id
}

fn new_surface(d: &mut D3d, w: u32, h: u32, f: u32, pool: u32, usage: u32, container: u32, gpu: u32, level: u32) -> u32 {
    let (own_gpu, gpu) = if gpu != 0 {
        (false, gpu)
    } else if pool == POOL_SYSTEMMEM || pool == POOL_SCRATCH {
        (false, 0)
    } else {
        (true, gpu_texture(w, h, 1, f, usage))
    };
    let (pitch, _) = fmt::pitch_rows(f, w, h);
    let s = Surface {
        w,
        h,
        fmt: f,
        pool,
        usage,
        gpu,
        level,
        own_gpu,
        mem: 0,
        pitch,
        gpu_newer: false,
        lock: None,
        dc_bitmap: 0,
        dc: 0,
        container,
        multisample: 0,
        pending: None,
        full_locks: 0,
        shadow: None,
        face: None,
        pal_gen: 0,
    };
    d.new_obj(IF_SURFACE, container, Data::Surface(s))
}

unsafe fn create_device(d: &mut D3d, d3d: u32, ty: u32, focus: u32, behavior: u32, pp: u32, out: u32) -> u32 {
    let _ = ty;
    if pp == 0 || out == 0 {
        return D3DERR_INVALIDCALL;
    }
    let mut p = read_pp(pp);
    let hwnd = if p[7] != 0 { p[7] } else { focus };
    let mut rect = [0i32; 4];
    (d.calls().client_rect)(hwnd, rect.as_mut_ptr());
    if p[0] == 0 {
        p[0] = (rect[2] - rect[0]).max(1) as u32;
    }
    if p[1] == 0 {
        p[1] = (rect[3] - rect[1]).max(1) as u32;
    }
    if p[2] == 0 {
        p[2] = fmt::X8R8G8B8;
    }
    p[3] = p[3].max(1);
    // (no multisampling)
    p[4] = 0;
    p[5] = 0;
    write_pp(pp, &p);
    if d.trace {
        eprintln!("d3d9: device {}x{} format {:#x} window {:?} windowed {}", p[0], p[1], p[2], rect, p[8]);
    }
    let obj_addr = d.new_obj(IF_DEVICE, 0, Data::Device);
    let obj = d.id_of(obj_addr);
    d.add_ref(d3d);
    let mut dev = Box::new(Device {
        obj,
        d3d,
        hwnd,
        pp: p,
        behavior,
        backbuffer: 0,
        swapchain: 0,
        auto_depth: 0,
        rt: [0; 4],
        ds: 0,
        st: default_state(),
        recording: None,
        batch_target: None,
        pipelines: HashMap::default(),
        shaders: HashMap::new(),
        modules: HashMap::default(),
        next_gpu: 0,
        serial: 0,
        readbacks: 0,
        dummy: 0,
        last_vs: (Vec::new(), 0),
        last_ps: (Vec::new(), 0),
        batch_gen: u64::MAX,
        uniform_gen: u64::MAX,
        in_scene: false,
        cursor: true,
        gamma: Vec::new(),
        frames: 0,
        fvf_decls: HashMap::new(),
        palettes: HashMap::new(),
        cur_palette: 0,
        pal_gen: 0,
        prep_gen: 0,
        uni_gen: 0,
        prep: None,
        uni: None,
    });
    let _ = dev.new_gpu();
    // (another device of the program waits: its calls switch back to it, `D3d::activate`)
    if let Some(cur) = d.dev.replace(dev) {
        d.parked.push(cur);
    }
    reset_buffers(d);
    // (always: in every run's log, a game's picture starts here)
    eprintln!("sake: d3d9 CreateDevice hwnd {hwnd:#x} {}x{} fmt {} depth {}/{} windowed {} behavior {behavior:#x}", p[0], p[1], p[2], p[9], p[10], p[8]);
    wrp(out, 0, obj_addr);
    S_OK
}

/// (Re)creates the swap chain's back buffer and the automatic depth buffer.
fn reset_buffers(d: &mut D3d) {
    let dev = d.dev.as_ref().unwrap();
    let p = dev.pp;
    let (old_bb, old_ds, old_sc) = (dev.backbuffer, dev.auto_depth, dev.swapchain);
    // (the device holds them by binding only: an application's Release of what GetBackBuffer
    // gave it returns 0, as on Windows; QLIE checks)
    for s in [old_bb, old_ds, old_sc] {
        if s != 0 {
            d.unbind(s);
        }
    }
    let bb = new_surface(d, p[0], p[1], p[2], POOL_DEFAULT, USAGE_RENDERTARGET, 0, 0, 0);
    let bb = d.id_of(bb);
    d.bind(bb);
    d.release(bb);
    let ds = if p[9] != 0 {
        let s = new_surface(d, p[0], p[1], p[10], POOL_DEFAULT, USAGE_DEPTHSTENCIL, 0, 0, 0);
        let s = d.id_of(s);
        d.bind(s);
        d.release(s);
        s
    } else {
        0
    };
    let sc = d.new_obj(IF_SWAPCHAIN, 0, Data::SwapChain);
    let sc = d.id_of(sc);
    d.bind(sc);
    d.release(sc);
    let dev = d.dev.as_mut().unwrap();
    for r in dev.rt {
        if r != 0 && r != old_bb {
            // (released below)
        }
    }
    let old_rt = dev.rt;
    let old_dsv = dev.ds;
    dev.backbuffer = bb;
    dev.auto_depth = ds;
    dev.swapchain = sc;
    dev.rt = [bb, 0, 0, 0];
    dev.ds = ds;
    dev.st = default_state();
    dev.st.rs[7] = (ds != 0) as u32;
    // (software vertex processing: 8192 float, 2048 integer and boolean vertex shader
    // constants; POINTSIZE_MAX is 8192.0 on either kind of device, winref d3d9state)
    if dev.behavior & 0x20 != 0 {
        dev.st.vsf.resize(8192, [0.0; 4]);
        dev.st.vsi_more = vec![[0; 4]; 2048 - 16];
        dev.st.vsb_more = vec![0; 2048 - 16];
    }
    dev.st.viewport = [0, 0, p[0], p[1], 0, 1f32.to_bits()];
    dev.st.scissor = [0, 0, p[0] as i32, p[1] as i32];
    d.bind(bb);
    if ds != 0 {
        d.bind(ds);
    }
    for r in old_rt {
        if r != 0 {
            d.unbind(r);
        }
    }
    if old_dsv != 0 {
        d.unbind(old_dsv);
    }
}

unsafe fn get_backbuffer(d: &mut D3d, out: u32) -> u32 {
    let Some(dev) = &d.dev else { return D3DERR_INVALIDCALL };
    let bb = dev.backbuffer;
    d.add_ref(bb);
    wrp(out, 0, d.addr_of(bb));
    S_OK
}

// ---------------------------------------------------------------------------------------------
// the device

macro_rules! dev {
    ($d:expr) => {
        match $d.dev.as_mut() {
            Some(dev) => dev,
            None => return D3DERR_INVALIDCALL,
        }
    };
}

unsafe fn set_bound(d: &mut D3d, new: u32, old: u32) {
    if new != 0 {
        d.bind(new);
    }
    if old != 0 {
        d.unbind(old);
    }
}

unsafe fn device_call(d: &mut D3d, m: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    // (while recording a state block, the Set* calls change the recording, not the device)
    if sb::records(m) {
        if let Some(rec) = d.dev.as_mut().and_then(|dev| dev.recording.as_mut()) {
            rec.mask.record(m, a);
            let swap = |d: &mut D3d| {
                let dev = d.dev.as_mut().unwrap();
                std::mem::swap(&mut dev.st, &mut dev.recording.as_mut().unwrap().st);
            };
            swap(d);
            let r = device_method(d, m, a, after);
            swap(d);
            return r;
        }
    }
    device_method(d, m, a, after)
}

unsafe fn device_method(d: &mut D3d, m: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    match m {
        3 => S_OK, // TestCooperativeLevel
        4 => 1024 * 1024 * 1024,
        5 => S_OK,
        6 => {
            let dev = dev!(d);
            let d3d = dev.d3d;
            d.add_ref(d3d);
            wrp(a(1), 0, d.addr_of(d3d));
            S_OK
        }
        7 => {
            std::ptr::copy_nonoverlapping(CAPS.as_ptr(), a(1) as *mut u32, CAPS.len());
            S_OK
        }
        8 => display_mode(a(2)),
        9 => {
            let dev = dev!(d);
            // D3DDEVICE_CREATION_PARAMETERS (64-bit: the HWND 8-byte aligned)
            wr(a(1), 0, 0);
            wr(a(1), 1, 1);
            wrp(a(1) + 8, 0, dev.hwnd);
            wr(a(1) + 8 + ptr(), 0, dev.behavior);
            S_OK
        }
        10 => S_OK,
        11 => S_OK,
        12 => {
            let dev = dev!(d);
            let was = dev.cursor;
            dev.cursor = a(1) != 0;
            was as u32
        }
        13 => D3DERR_NOTAVAILABLE,
        14 => {
            if a(1) != 0 {
                return D3DERR_INVALIDCALL;
            }
            let dev = dev!(d);
            let sc = dev.swapchain;
            d.add_ref(sc);
            wrp(a(2), 0, d.addr_of(sc));
            S_OK
        }
        15 => 1,
        16 => {
            // Reset
            let dev = dev!(d);
            let mut p = read_pp(a(1));
            if p[7] != 0 {
                dev.hwnd = p[7];
            }
            let hwnd = dev.hwnd;
            let mut rect = [0i32; 4];
            (d.calls().client_rect)(hwnd, rect.as_mut_ptr());
            if p[0] == 0 {
                p[0] = (rect[2] - rect[0]).max(1) as u32;
            }
            if p[1] == 0 {
                p[1] = (rect[3] - rect[1]).max(1) as u32;
            }
            if p[2] == 0 {
                p[2] = fmt::X8R8G8B8;
            }
            p[4] = 0;
            p[5] = 0;
            write_pp(a(1), &p);
            let dev = d.dev.as_mut().unwrap();
            dev.pp = p;
            // unbind everything the old state held
            let st = std::mem::replace(&mut dev.st, default_state());
            let mut held: Vec<u32> = st.textures.iter().copied().filter(|&t| t != 0).collect();
            held.extend(st.streams.iter().map(|s| s.0).filter(|&s| s != 0));
            held.extend([st.indices, st.decl, st.vs, st.ps].into_iter().filter(|&x| x != 0));
            for h in held {
                d.unbind(h);
            }
            reset_buffers(d);
            S_OK
        }
        17 => present(d, after, a(1), a(2), a(3)),
        18 => get_backbuffer(d, a(4)),
        19 => raster_status(d, a(2)),
        20 => S_OK,
        21 => S_OK,
        22 => {
            // GetGammaRamp: identity
            let p = a(3) as *mut u16;
            if !p.is_null() {
                for c in 0..3 {
                    for i in 0..256 {
                        *p.add(c * 256 + i) = (i * 257) as u16;
                    }
                }
            }
            S_OK
        }
        23 => create_texture(d, a(1), a(2), a(3), a(4), a(5), a(6), a(7)),
        24 => create_volume_texture(d, [a(1), a(2), a(3)], a(4), a(5), a(6), a(7), a(8)),
        25 => create_cube_texture(d, a(1), a(2), a(3), a(4), a(5), a(6)),
        26 | 27 => {
            let (len, usage, f, pool, out) = (a(1), a(2), a(3), a(4), a(5));
            let mem = d.guest_alloc(len.max(16));
            if mem == 0 {
                return super::E_OUTOFMEMORY;
            }
            let b = Buffer { len, usage, fmt: f, pool, mem, gpu: 0, gen: 0, sent: 0, dirty: None, locked: false, shadow: Vec::new(), cgen: 0 };
            let o = d.new_obj(if m == 26 { IF_VB } else { IF_IB }, 0, Data::Buffer(b));
            wrp(out, 0, o);
            S_OK
        }
        28 | 29 => {
            // CreateRenderTarget / CreateDepthStencilSurface(w, h, fmt, ms, quality, lockable/discard, out)
            let usage = if m == 28 { USAGE_RENDERTARGET } else { USAGE_DEPTHSTENCIL };
            if !fmt::supported(a(3)) {
                return D3DERR_INVALIDCALL;
            }
            let s = new_surface(d, a(1), a(2), a(3), POOL_DEFAULT, usage, 0, 0, 0);
            wrp(a(7), 0, s);
            S_OK
        }
        30 => update_surface(d, a(1), a(2), a(3), a(4)),
        31 => update_texture(d, a(1), a(2)),
        32 => get_rt_data(d, a(1), a(2), after),
        33 => D3DERR_INVALIDCALL,
        34 => stretch_rect(d, a(1), a(2), a(3), a(4), a(5)),
        35 => color_fill(d, a(1), a(2), a(3)),
        36 => {
            // CreateOffscreenPlainSurface(w, h, fmt, pool, out, shared)
            if !fmt::supported(a(3)) {
                return D3DERR_INVALIDCALL;
            }
            let s = new_surface(d, a(1), a(2), a(3), a(4), 0, 0, 0, 0);
            wrp(a(5), 0, s);
            S_OK
        }
        37 => {
            let (i, s) = (a(1) as usize, d.id_of(a(2)));
            if i >= 4 || (i == 0 && s == 0) {
                return D3DERR_INVALIDCALL;
            }
            let dev = dev!(d);
            let old = dev.rt[i];
            dev.rt[i] = s;
            if i == 0 {
                // the viewport follows the new target
                if let Some(Data::Surface(sf)) = d.objs.get(&s).map(|o| &o.data) {
                    let (w, h) = (sf.w, sf.h);
                    let dev = d.dev.as_mut().unwrap();
                    dev.st.viewport = [0, 0, w, h, 0, 1f32.to_bits()];
                    dev.st.scissor = [0, 0, w as i32, h as i32];
                }
            }
            set_bound(d, s, old);
            S_OK
        }
        38 => {
            let i = a(1) as usize;
            let dev = dev!(d);
            let s = if i < 4 { dev.rt[i] } else { 0 };
            if s == 0 {
                wr(a(2), 0, 0);
                return D3DERR_NOTFOUND;
            }
            d.add_ref(s);
            wrp(a(2), 0, d.addr_of(s));
            S_OK
        }
        39 => {
            let s = d.id_of(a(1));
            let dev = dev!(d);
            let old = dev.ds;
            dev.ds = s;
            set_bound(d, s, old);
            S_OK
        }
        40 => {
            let dev = dev!(d);
            let s = dev.ds;
            if s == 0 {
                wr(a(1), 0, 0);
                return D3DERR_NOTFOUND;
            }
            d.add_ref(s);
            wrp(a(1), 0, d.addr_of(s));
            S_OK
        }
        // (a BeginScene inside a scene, an EndScene outside one: D3DERR_INVALIDCALL, as
        // Windows; replay_b2011_anim_team_mm begins twice)
        41 => {
            let dev = dev!(d);
            if dev.in_scene {
                return D3DERR_INVALIDCALL;
            }
            dev.in_scene = true;
            S_OK
        }
        42 => {
            let dev = dev!(d);
            if !dev.in_scene {
                return D3DERR_INVALIDCALL;
            }
            dev.in_scene = false;
            S_OK
        }
        43 => clear(d, a(1), a(2), a(3), a(4), f32::from_bits(a(5)), a(6)),
        44 | 46 => {
            let (state, p) = (a(1), a(2));
            if p == 0 {
                return D3DERR_INVALIDCALL;
            }
            let mut mtx = [0f32; 16];
            std::ptr::copy_nonoverlapping(p as *const f32, mtx.as_mut_ptr(), 16);
            let dev = dev!(d);
            if m == 46 {
                let cur = dev.st.transforms.get(&state).copied().unwrap_or(IDENT);
                // MultiplyTransform: current * supplied matrix.
                mtx = mul(&cur, &mtx);
            }
            dev.st.transforms.insert(state, mtx);
            S_OK
        }
        45 => {
            // (the transforms: VIEW, PROJECTION, TEXTURE0-7, WORLDMATRIX(0-255))
            if !matches!(a(1), 2 | 3 | 16..=23 | 256..=511) {
                return D3DERR_INVALIDCALL;
            }
            let dev = dev!(d);
            let mtx = dev.st.transforms.get(&a(1)).copied().unwrap_or(IDENT);
            std::ptr::copy_nonoverlapping(mtx.as_ptr(), a(2) as *mut f32, 16);
            S_OK
        }
        47 => {
            let dev = dev!(d);
            std::ptr::copy_nonoverlapping(a(1) as *const u32, dev.st.viewport.as_mut_ptr(), 6);
            S_OK
        }
        48 => {
            let dev = dev!(d);
            std::ptr::copy_nonoverlapping(dev.st.viewport.as_ptr(), a(1) as *mut u32, 6);
            S_OK
        }
        49 => {
            let dev = dev!(d);
            std::ptr::copy_nonoverlapping(a(1) as *const f32, dev.st.material.as_mut_ptr(), 17);
            S_OK
        }
        50 => {
            let dev = dev!(d);
            std::ptr::copy_nonoverlapping(dev.st.material.as_ptr(), a(1) as *mut f32, 17);
            S_OK
        }
        51 => {
            let dev = dev!(d);
            let l = std::slice::from_raw_parts(a(2) as *const u8, 104).to_vec();
            let on = dev.st.lights.get(&a(1)).is_some_and(|x| x.1);
            dev.st.lights.insert(a(1), (l, on));
            S_OK
        }
        52 => {
            let dev = dev!(d);
            let Some((l, _)) = dev.st.lights.get(&a(1)) else { return D3DERR_INVALIDCALL };
            std::ptr::copy_nonoverlapping(l.as_ptr(), a(2) as *mut u8, 104);
            S_OK
        }
        53 => {
            let dev = dev!(d);
            let e = dev.st.lights.entry(a(1)).or_insert_with(|| (vec![0; 104], false));
            e.1 = a(2) != 0;
            S_OK
        }
        54 => {
            let dev = dev!(d);
            let Some((_, on)) = dev.st.lights.get(&a(1)) else { return D3DERR_INVALIDCALL };
            wr(a(2), 0, if *on { 128 } else { 0 });
            S_OK
        }
        55 => {
            let dev = dev!(d);
            if let Some(c) = dev.st.clip.get_mut(a(1) as usize) {
                std::ptr::copy_nonoverlapping(a(2) as *const f32, c.as_mut_ptr(), 4);
            }
            S_OK
        }
        56 => {
            let dev = dev!(d);
            // (past the 6 planes: zeros, as Windows)
            let c = dev.st.clip.get(a(1) as usize).copied().unwrap_or([0.0; 4]);
            std::ptr::copy_nonoverlapping(c.as_ptr(), a(2) as *mut f32, 4);
            S_OK
        }
        57 => {
            let dev = dev!(d);
            if let Some(r) = dev.st.rs.get_mut(a(1) as usize) {
                *r = a(2);
            }
            S_OK
        }
        58 => {
            if !rs_valid(a(1)) {
                return D3DERR_INVALIDCALL;
            }
            let dev = dev!(d);
            wr(a(2), 0, dev.st.rs[a(1) as usize]);
            S_OK
        }
        59 => {
            // CreateStateBlock(type, out)
            let dev = dev!(d);
            if dev.recording.is_some() || a(2) == 0 {
                return D3DERR_INVALIDCALL;
            }
            let mask = match a(1) {
                1 => sb::Mask::all(&dev.st),
                2 => sb::Mask::pixel(&dev.st),
                3 => sb::Mask::vertex(&dev.st),
                // (Windows takes 0: a block of nothing)
                0 => sb::Mask::default(),
                _ => return D3DERR_INVALIDCALL,
            };
            let st = sb::masked_copy(&dev.st, &mask);
            bind_all(d, &state_binds(&st));
            let o = d.new_obj(IF_STATEBLOCK, 0, Data::StateBlock(Box::new(sb::Block { st, mask })));
            wrp(a(2), 0, o);
            S_OK
        }
        60 => {
            // BeginStateBlock: the Set* calls go to a copy of the state until EndStateBlock
            let dev = dev!(d);
            if dev.recording.is_some() {
                return D3DERR_INVALIDCALL;
            }
            let st = dev.st.clone();
            bind_all(d, &state_binds(&st));
            d.dev.as_mut().unwrap().recording = Some(Box::new(sb::Recording { st, mask: sb::Mask::default() }));
            S_OK
        }
        61 => {
            // EndStateBlock(out): a block of what was recorded
            let dev = dev!(d);
            if a(1) == 0 || dev.recording.is_none() {
                return D3DERR_INVALIDCALL;
            }
            let rec = dev.recording.take().unwrap();
            let st = sb::masked_copy(&rec.st, &rec.mask);
            bind_all(d, &state_binds(&st));
            unbind_all(d, &state_binds(&rec.st));
            let o = d.new_obj(IF_STATEBLOCK, 0, Data::StateBlock(Box::new(sb::Block { st, mask: rec.mask })));
            wrp(a(1), 0, o);
            S_OK
        }
        62 | 63 => S_OK,
        64 => {
            let dev = dev!(d);
            let t = sampler_slot(a(1)).map_or(0, |s| dev.st.textures[s]);
            if t != 0 {
                d.add_ref(t);
            }
            wrp(a(2), 0, d.addr_of(t));
            S_OK
        }
        65 => {
            let stage = a(1) as usize;
            let t = d.id_of(a(2));
            let dev = dev!(d);
            let Some(slot) = sampler_slot(stage as u32).map(|s| &mut dev.st.textures[s]) else { return S_OK };
            let old = *slot;
            *slot = t;
            set_bound(d, t, old);
            S_OK
        }
        66 => {
            let dev = dev!(d);
            let v = dev.st.tss.get(a(1) as usize).and_then(|s| s.get(a(2) as usize)).copied().unwrap_or(0);
            wr(a(3), 0, v);
            S_OK
        }
        67 => {
            let dev = dev!(d);
            if let Some(v) = dev.st.tss.get_mut(a(1) as usize).and_then(|s| s.get_mut(a(2) as usize)) {
                *v = a(3);
            }
            S_OK
        }
        68 => {
            let dev = dev!(d);
            let v = sampler_slot(a(1)).and_then(|s| dev.st.ss[s].get(a(2) as usize)).copied().unwrap_or(0);
            wr(a(3), 0, v);
            S_OK
        }
        69 => {
            let dev = dev!(d);
            if let Some(v) = sampler_slot(a(1)).and_then(|s| dev.st.ss[s].get_mut(a(2) as usize)) {
                *v = a(3);
            }
            S_OK
        }
        70 => {
            wr(a(1), 0, 1);
            S_OK
        }
        71 => {
            // SetPaletteEntries(number, 256 PALETTEENTRY)
            if a(2) == 0 || a(1) > 0xffff {
                return D3DERR_INVALIDCALL;
            }
            let dev = dev!(d);
            let mut p = Box::new([0u8; 1024]);
            std::ptr::copy_nonoverlapping(a(2) as *const u8, p.as_mut_ptr(), 1024);
            if dev.palettes.get(&a(1)) != Some(&p) {
                if a(1) == dev.cur_palette {
                    dev.pal_gen += 1;
                }
                dev.palettes.insert(a(1), p);
            }
            S_OK
        }
        72 => {
            // GetPaletteEntries(number, out): only a palette that was set
            let dev = dev!(d);
            match dev.palettes.get(&a(1)) {
                Some(p) if a(2) != 0 => {
                    std::ptr::copy_nonoverlapping(p.as_ptr(), a(2) as *mut u8, 1024);
                    S_OK
                }
                _ => D3DERR_INVALIDCALL,
            }
        }
        73 => {
            // SetCurrentTexturePalette(number)
            let dev = dev!(d);
            if a(1) > 0xffff {
                return D3DERR_INVALIDCALL;
            }
            if a(1) != dev.cur_palette {
                dev.cur_palette = a(1);
                dev.pal_gen += 1;
            }
            S_OK
        }
        74 => {
            let dev = dev!(d);
            let p = dev.cur_palette;
            wr(a(1), 0, p);
            S_OK
        }
        75 => {
            let dev = dev!(d);
            std::ptr::copy_nonoverlapping(a(1) as *const i32, dev.st.scissor.as_mut_ptr(), 4);
            S_OK
        }
        76 => {
            let dev = dev!(d);
            std::ptr::copy_nonoverlapping(dev.st.scissor.as_ptr(), a(1) as *mut i32, 4);
            S_OK
        }
        77 => S_OK,
        78 => 0,
        79 => {
            let dev = dev!(d);
            dev.st.npatch = a(1);
            S_OK
        }
        80 => dev!(d).st.npatch,
        81 => draw(d, a(1), Src::Streams(a(2)), a(3), None),
        82 => {
            // DrawIndexedPrimitive(type, base vertex, min index, num vertices, start index, prims)
            draw(d, a(1), Src::Indexed { base: a(2) as i32, min: a(3), num: a(4), start: a(5) }, a(6), None)
        }
        83 => draw(d, a(1), Src::Up { data: a(3), stride: a(4) }, a(2), None),
        84 => {
            // DrawIndexedPrimitiveUP(type, min index, num vertices, prims, indices, index fmt, data, stride)
            let isize = if a(6) == fmt::INDEX32 { 4 } else { 2 };
            draw(d, a(1), Src::UpIndexed { min: a(2), num: a(3), idx: a(5), isize, data: a(7), stride: a(8) }, a(4), None)
        }
        85 => D3DERR_INVALIDCALL,
        86 => create_decl(d, a(1), a(2)),
        87 => {
            let decl = d.id_of(a(1));
            let dev = dev!(d);
            let old = dev.st.decl;
            dev.st.decl = decl;
            if let Some(Data::Decl(dc)) = d.objs.get(&decl).map(|o| &o.data) {
                let f = dc.fvf;
                d.dev.as_mut().unwrap().st.fvf = f;
            }
            set_bound(d, decl, old);
            S_OK
        }
        88 => {
            let dev = dev!(d);
            let (mut decl, fvf) = (dev.st.decl, dev.st.fvf);
            // with an FVF set: a declaration of it, as Direct3D makes one (made once a device)
            if decl == 0 && fvf != 0 {
                decl = match dev.fvf_decls.get(&fvf) {
                    Some(&id) => id,
                    None => {
                        let elems = fvf_elems(fvf).0;
                        let mut raw: Vec<[u32; 2]> = elems.iter().map(|e| [e.stream | e.offset << 16, e.ty | e.usage << 16 | e.index << 24]).collect();
                        // (blend weights, and indices in the last beta: in the declaration only)
                        let pos = fvf & 0x400e;
                        if (6..=0xe).contains(&pos) {
                            let n = (pos - 6) / 2 + 1;
                            let idx = if fvf & 0x1000 != 0 { Some(5) } else if fvf & 0x8000 != 0 { Some(4) } else { None };
                            let w = n - idx.is_some() as u32;
                            let mut at = 1;
                            if w > 0 {
                                raw.insert(at, [12 << 16, (w - 1) | 1 << 16]);
                                at += 1;
                            }
                            if let Some(t) = idx {
                                raw.insert(at, [(12 + 4 * w) << 16, t | 2 << 16]);
                            }
                        }
                        raw.push([0xff, 17]);
                        let addr = d.new_obj(IF_DECL, 0, Data::Decl(Decl { elems, raw, fvf }));
                        let id = d.id_of(addr);
                        d.dev.as_mut().unwrap().fvf_decls.insert(fvf, id);
                        id
                    }
                };
            }
            if decl != 0 {
                d.add_ref(decl);
            }
            wrp(a(1), 0, d.addr_of(decl));
            S_OK
        }
        89 => {
            let f = a(1);
            let dev = dev!(d);
            if f == 0 {
                return S_OK;
            }
            dev.st.fvf = f;
            let old = dev.st.decl;
            dev.st.decl = 0;
            set_bound(d, 0, old);
            S_OK
        }
        90 => {
            let dev = dev!(d);
            wr(a(1), 0, dev.st.fvf);
            S_OK
        }
        91 | 106 => create_shader(d, m == 106, a(1), a(2)),
        92 | 107 => {
            let s = d.id_of(a(1));
            let dev = dev!(d);
            let slot = if m == 92 { &mut dev.st.vs } else { &mut dev.st.ps };
            let old = *slot;
            *slot = s;
            set_bound(d, s, old);
            S_OK
        }
        93 | 108 => {
            let dev = dev!(d);
            let s = if m == 93 { dev.st.vs } else { dev.st.ps };
            if s != 0 {
                d.add_ref(s);
            }
            wrp(a(1), 0, d.addr_of(s));
            S_OK
        }
        94 | 95 | 109 | 110 => {
            let dev = dev!(d);
            let regs = if m < 100 { &mut dev.st.vsf } else { &mut dev.st.psf };
            let (start, n) = (a(1) as usize, a(3) as usize);
            if start + n > regs.len() {
                return D3DERR_INVALIDCALL;
            }
            let p = a(2) as *mut [f32; 4];
            if m == 94 || m == 109 {
                std::ptr::copy_nonoverlapping(p, regs[start..].as_mut_ptr(), n);
            } else {
                std::ptr::copy_nonoverlapping(regs[start..].as_ptr(), p, n);
            }
            S_OK
        }
        96 | 97 | 111 | 112 => {
            let dev = dev!(d);
            let st = &mut dev.st;
            let more = if m < 100 { st.vsi_more.len() } else { 0 };
            let (start, n) = (a(1) as usize, a(3) as usize);
            if start.saturating_add(n) > 16 + more {
                return D3DERR_INVALIDCALL;
            }
            let p = a(2) as *mut [i32; 4];
            for i in 0..n {
                let r = start + i;
                let reg = match (m < 100, r < 16) {
                    (true, true) => &mut st.vsi[r],
                    (true, false) => &mut st.vsi_more[r - 16],
                    _ => &mut st.psi[r],
                };
                if m == 96 || m == 111 {
                    *reg = *p.add(i);
                } else {
                    *p.add(i) = *reg;
                }
            }
            S_OK
        }
        98 | 99 | 113 | 114 => {
            let dev = dev!(d);
            let st = &mut dev.st;
            let more = if m < 100 { st.vsb_more.len() } else { 0 };
            let (start, n) = (a(1), a(3));
            if start.saturating_add(n) > 16 + more as u32 {
                return D3DERR_INVALIDCALL;
            }
            let p = a(2) as *mut u32;
            for i in 0..n {
                let r = start + i;
                let set = m == 98 || m == 113;
                let v = *p.add(i as usize) != 0;
                if r >= 16 {
                    let x = &mut st.vsb_more[r as usize - 16];
                    if set {
                        *x = v as u32;
                    } else {
                        *p.add(i as usize) = *x;
                    }
                    continue;
                }
                let bits = if m < 100 { &mut st.vsb } else { &mut st.psb };
                let b = 1 << r;
                if set {
                    if v {
                        *bits |= b;
                    } else {
                        *bits &= !b;
                    }
                } else {
                    *p.add(i as usize) = (*bits & b != 0) as u32;
                }
            }
            S_OK
        }
        100 => {
            let (n, vb, off, stride) = (a(1) as usize, d.id_of(a(2)), a(3), a(4));
            let dev = dev!(d);
            // (Windows takes streams past 15 and keeps nothing)
            let Some(slot) = dev.st.streams.get_mut(n) else { return S_OK };
            let old = slot.0;
            *slot = (vb, off, stride);
            set_bound(d, vb, old);
            S_OK
        }
        101 => {
            let dev = dev!(d);
            let Some(&(vb, off, stride)) = dev.st.streams.get(a(1) as usize) else { return D3DERR_INVALIDCALL };
            if vb != 0 {
                d.add_ref(vb);
            }
            wrp(a(2), 0, d.addr_of(vb));
            wr(a(3), 0, off);
            wr(a(4), 0, stride);
            S_OK
        }
        102 => {
            // (a divider of 0: D3DERR_INVALIDCALL)
            if a(2) & 0x3fff_ffff == 0 {
                return D3DERR_INVALIDCALL;
            }
            let dev = dev!(d);
            if let Some(f) = dev.st.freq.get_mut(a(1) as usize) {
                *f = a(2);
            }
            S_OK
        }
        103 => {
            let dev = dev!(d);
            wr(a(2), 0, dev.st.freq.get(a(1) as usize).copied().unwrap_or(1));
            S_OK
        }
        104 => {
            let ib = d.id_of(a(1));
            let dev = dev!(d);
            let old = dev.st.indices;
            dev.st.indices = ib;
            set_bound(d, ib, old);
            S_OK
        }
        105 => {
            let dev = dev!(d);
            let ib = dev.st.indices;
            if ib != 0 {
                d.add_ref(ib);
            }
            wrp(a(1), 0, d.addr_of(ib));
            S_OK
        }
        115..=117 => E_NOTIMPL,
        118 => {
            // CreateQuery(type, out): out NULL asks whether it's supported
            let t = a(1);
            if !matches!(t, 8 | 9 | 10 | 11 | 12) {
                return D3DERR_NOTAVAILABLE;
            }
            if a(2) == 0 {
                return S_OK;
            }
            let o = d.new_obj(IF_QUERY, 0, Data::Query(t));
            wrp(a(2), 0, o);
            S_OK
        }
        _ => {
            if d.trace {
                eprintln!("d3d9: {} not implemented", super::name(IF_DEVICE, m));
            }
            E_NOTIMPL
        }
    }
}

const D3DERR_NOTFOUND: u32 = super::D3DERR_NOTFOUND;

const IDENT: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// a * b (row-major, D3D's order: v * a * b)
fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut r = [0f32; 16];
    for i in 0..4 {
        for j in 0..4 {
            r[i * 4 + j] = (0..4).map(|k| a[i * 4 + k] * b[k * 4 + j]).sum();
        }
    }
    r
}

/// the objects a state holds a binding on
pub(crate) fn state_binds(st: &State) -> [u32; SAMPLERS + 20] {
    let mut b = [0u32; SAMPLERS + 20];
    b[..SAMPLERS].copy_from_slice(&st.textures);
    for (i, s) in st.streams.iter().enumerate() {
        b[SAMPLERS + i] = s.0;
    }
    b[SAMPLERS + 16..].copy_from_slice(&[st.indices, st.decl, st.vs, st.ps]);
    b
}

/// (state blocks hold what they captured, as the device's state does)
pub(crate) fn bind_all(d: &mut D3d, ids: &[u32]) {
    for &b in ids {
        if b != 0 {
            d.bind(b);
        }
    }
}

pub(crate) fn unbind_all(d: &mut D3d, ids: &[u32]) {
    for &b in ids {
        if b != 0 {
            d.unbind(b);
        }
    }
}

/// the device state = state block `sb`'s
fn apply_state(d: &mut D3d, sb: u32) {
    let (Some(dev), Some(Data::StateBlock(b))) = (d.dev.as_mut(), d.objs.get(&sb).map(|o| &o.data)) else { return };
    let unbind = state_binds(&dev.st);
    super::stateblock::copy_masked(&mut dev.st, &b.st, &b.mask);
    let bind = state_binds(&dev.st);
    // (objects released since the capture are gone: drop them from the state)
    for b in bind {
        if b != 0 && d.objs.contains_key(&b) {
            d.bind(b);
        }
    }
    let dev = d.dev.as_mut().unwrap();
    for t in dev.st.textures.iter_mut() {
        if *t != 0 && !d.objs.contains_key(t) {
            *t = 0;
        }
    }
    for u in unbind {
        if u != 0 {
            d.unbind(u);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// resources

unsafe fn create_texture(d: &mut D3d, w: u32, h: u32, levels: u32, usage: u32, f: u32, pool: u32, out: u32) -> u32 {
    // (a render target texture of a format that cannot be one: refused, as Windows, winref
    // d3d9rtfmt's X8B8G8R8)
    if out == 0 || w == 0 || h == 0 || !fmt::supported(f) || usage & USAGE_RENDERTARGET != 0 && !fmt::renderable(f) {
        if d.trace {
            eprintln!("d3d9: CreateTexture {w}x{h} format {f:#x}: not supported");
        }
        return D3DERR_INVALIDCALL;
    }
    let max = 32 - w.max(h).leading_zeros();
    let levels = if usage & 0x400 != 0 { 1 } else if levels == 0 { max } else { levels.min(max) };
    let gpu = if pool == POOL_SYSTEMMEM || pool == POOL_SCRATCH { 0 } else { gpu_texture(w, h, levels, f, usage) };
    let tex = Texture { w, h, fmt: f, pool, usage, gpu, levels: Vec::new(), lod: 0, depth: 0, faces: 0 };
    let addr = d.new_obj(IF_TEXTURE, 0, Data::Texture(tex));
    let id = d.id_of(addr);
    let mut lv = Vec::new();
    for l in 0..levels {
        let (lw, lh) = ((w >> l).max(1), (h >> l).max(1));
        let s = new_surface(d, lw, lh, f, pool, usage, id, if gpu == 0 { u32::MAX } else { gpu }, l);
        let sid = d.id_of(s);
        if let Some(Data::Surface(sf)) = d.objs.get_mut(&sid).map(|o| &mut o.data) {
            if gpu == 0 {
                sf.gpu = 0;
            }
        }
        // (a level's references are the texture's)
        if let Some(o) = d.objs.get_mut(&sid) {
            o.refs = 0;
        }
        lv.push(sid);
    }
    if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
        t.levels = lv;
    }
    wrp(out, 0, addr);
    S_OK
}

/// CreateVolumeTexture(w, h, depth, levels, usage, format, pool, out): a texture of `depth`
/// slices, its levels [`Volume`]s (Windows: no render target or depth-stencil volumes, no
/// depth formats; levels down to 1x1x1 of the largest of the three).
unsafe fn create_volume_texture(d: &mut D3d, size: [u32; 3], levels: u32, usage: u32, f: u32, pool: u32, out: u32) -> u32 {
    let [w, h, depth] = size;
    if out == 0 || w == 0 || h == 0 || depth == 0 || !fmt::supported(f) || fmt::is_depth(f) || usage & (USAGE_RENDERTARGET | USAGE_DEPTHSTENCIL) != 0 {
        if d.trace {
            eprintln!("d3d9: CreateVolumeTexture {w}x{h}x{depth} format {f:#x} usage {usage:#x}: not supported");
        }
        if out != 0 {
            wrp(out, 0, 0);
        }
        return D3DERR_INVALIDCALL;
    }
    let max = 32 - w.max(h).max(depth).leading_zeros();
    let levels = if levels == 0 { max } else { levels.min(max) };
    let gpu = if pool == POOL_SYSTEMMEM || pool == POOL_SCRATCH {
        0
    } else {
        let id = gpu_id();
        gpu::record(|b| b.cmd(&[gpu::CREATE_TEXTURE2, id, w, h, depth, levels, fmt::page_format(f), 1]));
        id
    };
    let tex = Texture { w, h, fmt: f, pool, usage, gpu, levels: Vec::new(), lod: 0, depth, faces: 0 };
    let addr = d.new_obj(IF_VOLUMETEX, 0, Data::Texture(tex));
    let id = d.id_of(addr);
    let mut lv = Vec::new();
    for l in 0..levels {
        let (lw, lh, ld) = ((w >> l).max(1), (h >> l).max(1), (depth >> l).max(1));
        let (pitch, rows) = fmt::pitch_rows(f, lw, lh);
        let v = Volume { w: lw, h: lh, depth: ld, fmt: f, pool, usage, gpu, level: l, container: id, mem: 0, pitch, slice: pitch * rows, lock: None };
        let va = d.new_obj(IF_VOLUME, id, Data::Volume(v));
        let vid = d.id_of(va);
        // (a level's references are the texture's)
        if let Some(o) = d.objs.get_mut(&vid) {
            o.refs = 0;
        }
        lv.push(vid);
    }
    if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
        t.levels = lv;
    }
    wrp(out, 0, addr);
    S_OK
}

/// CreateCubeTexture(edge, levels, usage, format, pool, out): six faces of `edge` x `edge`, each
/// a chain of surfaces (IDirect3DSurface9) whose container is the cube. The page texture is a
/// 2D texture of six layers, one per face in D3DCUBEMAP_FACES order (+X -X +Y -Y +Z -Z), which
/// is WebGPU's cube layer order: a cube sampler reads it through a cube view (render::d3d
/// slot_view). No render target or depth-stencil cubes yet (refused, so a game sees an error,
/// not a NULL it would call).
unsafe fn create_cube_texture(d: &mut D3d, edge: u32, levels: u32, usage: u32, f: u32, pool: u32, out: u32) -> u32 {
    if out == 0 || edge == 0 || !fmt::supported(f) || fmt::is_depth(f) || usage & (USAGE_RENDERTARGET | USAGE_DEPTHSTENCIL) != 0 {
        if d.trace {
            eprintln!("d3d9: CreateCubeTexture {edge} format {f:#x} usage {usage:#x}: not supported");
        }
        if out != 0 {
            wrp(out, 0, 0);
        }
        return if usage & USAGE_RENDERTARGET != 0 && fmt::supported(f) { D3DERR_NOTAVAILABLE } else { D3DERR_INVALIDCALL };
    }
    let max = 32 - edge.leading_zeros();
    let levels = if usage & 0x400 != 0 { 1 } else if levels == 0 { max } else { levels.min(max) };
    let gpu = if pool == POOL_SYSTEMMEM || pool == POOL_SCRATCH {
        0
    } else {
        let id = gpu_id();
        gpu::record(|b| b.cmd(&[gpu::CREATE_TEXTURE2, id, edge, edge, 6, levels, fmt::page_format(f), 0]));
        id
    };
    let tex = Texture { w: edge, h: edge, fmt: f, pool, usage, gpu, levels: Vec::new(), lod: 0, depth: 0, faces: 6 };
    let addr = d.new_obj(IF_CUBE, 0, Data::Texture(tex));
    let id = d.id_of(addr);
    let mut lv = Vec::new();
    for face in 0..6 {
        for l in 0..levels {
            let e = (edge >> l).max(1);
            let s = new_surface(d, e, e, f, pool, usage, id, if gpu == 0 { u32::MAX } else { gpu }, l);
            let sid = d.id_of(s);
            if let Some(o) = d.objs.get_mut(&sid) {
                // (a level's references are the texture's)
                o.refs = 0;
                if let Data::Surface(sf) = &mut o.data {
                    sf.face = Some(face);
                    if gpu == 0 {
                        sf.gpu = 0;
                    }
                }
            }
            lv.push(sid);
        }
    }
    if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
        t.levels = lv;
    }
    wrp(out, 0, addr);
    S_OK
}

/// IDirect3DVolume9: GetDevice, private data, GetContainer(riid, out), GetDesc(desc),
/// LockBox(locked, box, flags), UnlockBox.
unsafe fn volume_call(d: &mut D3d, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    let Some(Data::Volume(v)) = d.objs.get(&this).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
    match m {
        3 => device_out(d, a(1)),
        4..=6 => private_data(d, m, this, a),
        7 => {
            let c = v.container;
            if a(2) == 0 {
                return D3DERR_INVALIDCALL;
            }
            d.add_ref(c);
            wrp(a(2), 0, d.addr_of(c));
            S_OK
        }
        8 => {
            // D3DVOLUME_DESC: Format, Type, Usage, Pool, Width, Height, Depth
            if a(1) == 0 {
                return D3DERR_INVALIDCALL;
            }
            let desc = [v.fmt, RT_VOLUME, v.usage, v.pool, v.w, v.h, v.depth];
            std::ptr::copy_nonoverlapping(desc.as_ptr(), a(1) as *mut u32, 7);
            S_OK
        }
        9 => lock_volume(d, this, a(1), a(2), a(3)),
        10 => unlock_volume(d, this),
        _ => E_NOTIMPL,
    }
}

/// The volume's guest copy, made on first use.
fn volume_mem(d: &mut D3d, v: u32) -> Option<u32> {
    let Some(Data::Volume(vl)) = d.objs.get(&v).map(|o| &o.data) else { return None };
    if vl.mem != 0 {
        return Some(vl.mem);
    }
    let mem = d.guest_alloc((vl.slice * vl.depth).max(16));
    if mem == 0 {
        return None;
    }
    // (zeros, as a new texture reads)
    unsafe { std::ptr::write_bytes(mem as *mut u8, 0, (vl.slice * vl.depth) as usize) };
    if let Some(Data::Volume(vl)) = d.objs.get_mut(&v).map(|o| &mut o.data) {
        vl.mem = mem;
    }
    Some(mem)
}

/// LockBox(D3DLOCKED_BOX { RowPitch, SlicePitch, pBits }, box or NULL, flags).
unsafe fn lock_volume(d: &mut D3d, v: u32, out: u32, bx: u32, flags: u32) -> u32 {
    if out == 0 {
        return D3DERR_INVALIDCALL;
    }
    let Some(mem) = volume_mem(d, v) else { return D3DERR_INVALIDCALL };
    let Some(Data::Volume(vl)) = d.objs.get_mut(&v).map(|o| &mut o.data) else { return D3DERR_INVALIDCALL };
    if vl.lock.is_some() {
        return D3DERR_INVALIDCALL;
    }
    // D3DBOX: Left, Top, Right, Bottom, Front, Back
    let b = if bx != 0 { [rd(bx, 0), rd(bx, 1), rd(bx, 4), rd(bx, 2), rd(bx, 3), rd(bx, 5)] } else { [0, 0, 0, vl.w, vl.h, vl.depth] };
    let (bpb, bs) = fmt::layout(vl.fmt).unwrap_or((4, 1));
    if b[0] >= b[3] || b[1] >= b[4] || b[2] >= b[5] || b[3] > vl.w || b[4] > vl.h || b[5] > vl.depth || (bs > 1 && (b[0] % bs != 0 || b[1] % bs != 0)) {
        return D3DERR_INVALIDCALL;
    }
    vl.lock = Some(if flags & LOCK_READONLY != 0 { [0; 6] } else { b });
    let bits = mem + b[2] * vl.slice + (b[1] / bs) * vl.pitch + (b[0] / bs) * bpb;
    wr(out, 0, vl.pitch);
    wr(out, 1, vl.slice);
    wrp(out + 8, 0, bits);
    S_OK
}

unsafe fn unlock_volume(d: &mut D3d, v: u32) -> u32 {
    let Some(Data::Volume(vl)) = d.objs.get_mut(&v).map(|o| &mut o.data) else { return D3DERR_INVALIDCALL };
    let Some(b) = vl.lock.take() else { return D3DERR_INVALIDCALL };
    if b[3] > b[0] {
        upload_volume(d, v, b);
    }
    S_OK
}

/// Uploads box `b` (left, top, front, right, bottom, back) of a volume's guest copy to its page
/// texture, a slice at a time (the page's layer z), converted to the page's texels.
fn upload_volume(d: &mut D3d, v: u32, b: [u32; 6]) {
    let Some(Data::Volume(vl)) = d.objs.get(&v).map(|o| &o.data) else { return };
    if vl.gpu == 0 || vl.mem == 0 {
        return;
    }
    let (bpb, bs) = fmt::layout(vl.fmt).unwrap_or((4, 1));
    let (x0, y0) = (b[0] / bs * bs, b[1] / bs * bs);
    let (x1, y1) = (b[3].min(vl.w), b[4].min(vl.h));
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let bpp = fmt::page_bpp(vl.fmt);
    let (gpu, level, f, pitch, slice, mem) = (vl.gpu, vl.level, vl.fmt, vl.pitch, vl.slice, vl.mem);
    for z in b[2]..b[5].min(vl.depth) {
        let src = mem + z * slice + (y0 / bs) * pitch + (x0 / bs) * bpb;
        gpu::record(|bt: &mut Batch| {
            let at = bt.blob.len().next_multiple_of(4);
            bt.blob.resize(at, 0);
            unsafe { fmt::convert(f, src as *const u8, pitch as usize, w, h, &mut bt.blob) };
            bt.cmd(&[gpu::WRITE_TEXTURE2, gpu, level, z, x0, y0, w, h, w * bpp, at as u32]);
        });
    }
}

/// The surface's guest copy, made on first use (and read back from the page if it rendered).
unsafe fn surface_mem(d: &mut D3d, s: u32) -> Option<u32> {
    let Some(Data::Surface(sf)) = d.objs.get(&s).map(|o| &o.data) else { return None };
    if sf.mem != 0 {
        return Some(sf.mem);
    }
    let (_, rows) = fmt::pitch_rows(sf.fmt, sf.w, sf.h);
    let size = sf.pitch * rows;
    let mem = d.guest_alloc(size.max(16));
    if let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) {
        sf.mem = mem;
    }
    (mem != 0).then_some(mem)
}

/// The box (x0, y0, x1, y1 in pixels) where `rows` (h rows of `row` bytes, `pitch` apart)
/// differ from `shadow` (packed rows), which takes the new rows. None: all the same.
fn changed_box(rows: &[u8], pitch: usize, row: usize, h: usize, bpp: usize, shadow: &mut [u8]) -> Option<[u32; 4]> {
    let mut b: Option<[usize; 4]> = None;
    for y in 0..h {
        let (a, s) = (&rows[y * pitch..y * pitch + row], &mut shadow[y * row..(y + 1) * row]);
        // 8 bytes at a time from each end (the tail bytes by themselves)
        let words = row / 8;
        let w = |x: &[u8], i: usize| u64::from_ne_bytes(x[i * 8..i * 8 + 8].try_into().unwrap());
        let l = (0..words).find(|&i| w(a, i) != w(s, i)).map_or(words * 8, |i| i * 8);
        let Some(l) = (l..row).find(|&i| a[i] != s[i]) else { continue };
        let r = match (words * 8..row).rev().find(|&i| a[i] != s[i]) {
            Some(i) => i + 1,
            None => {
                let wi = (0..words).rev().find(|&i| w(a, i) != w(s, i)).unwrap();
                (wi * 8..wi * 8 + 8).rev().find(|&i| a[i] != s[i]).unwrap() + 1
            }
        };
        s[l..r].copy_from_slice(&a[l..r]);
        b = Some(match b {
            None => [l, y, r, y + 1],
            Some(o) => [o[0].min(l), o[1], o[2].max(r), y + 1],
        });
    }
    b.map(|b| [(b[0] / bpp) as u32, b[1] as u32, b[2].div_ceil(bpp) as u32, b[3] as u32])
}

/// After a whole-surface lock: uploads only what changed since the last upload (a movie frame
/// written into the corner of a bigger texture: ANEMOI's 1920x1080 frames in 2048x2048, every
/// frame). From the second such lock on (the first makes the copy); big surfaces only.
unsafe fn upload_changed(d: &mut D3d, s: u32) {
    let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) else { return };
    let (bpb, bs) = fmt::layout(sf.fmt).unwrap_or((4, 1));
    let (w, h) = (sf.w, sf.h);
    let row = (w * bpb) as usize;
    if bs != 1 || sf.mem == 0 || sf.gpu == 0 || row * (h as usize) < 1 << 18 || fmt::is_depth(sf.fmt) {
        return upload(d, s, [0, 0, w, h]);
    }
    sf.full_locks += 1;
    let rows = std::slice::from_raw_parts(sf.mem as *const u8, sf.pitch as usize * (h as usize - 1) + row);
    let r = match sf.shadow.as_mut() {
        Some(sh) => changed_box(rows, sf.pitch as usize, row, h as usize, bpb as usize, sh),
        None => {
            if sf.full_locks >= 2 {
                let mut sh = vec![0u8; row * h as usize].into_boxed_slice();
                for y in 0..h as usize {
                    sh[y * row..(y + 1) * row].copy_from_slice(&rows[y * sf.pitch as usize..][..row]);
                }
                sf.shadow = Some(sh);
            }
            Some([0, 0, w, h])
        }
    };
    if super::stats::on() {
        eprintln!("d3d9: whole lock of {w}x{h} surface {s}: changed {r:?}");
    }
    if let Some(r) = r {
        upload_keep(d, s, r);
    }
}

/// Uploads rect `r` of a surface's guest copy to its page texture.
unsafe fn upload(d: &mut D3d, s: u32, r: [u32; 4]) {
    if let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) {
        // (the texture now has pixels the copy may not: see upload_changed)
        sf.shadow = None;
    }
    upload_keep(d, s, r);
}

unsafe fn upload_keep(d: &mut D3d, s: u32, r: [u32; 4]) {
    let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) else { return };
    if sf.gpu == 0 || sf.mem == 0 || fmt::is_depth(sf.fmt) {
        return;
    }
    let (bpb, bs) = fmt::layout(sf.fmt).unwrap_or((4, 1));
    // whole blocks
    let x0 = r[0] / bs * bs;
    let y0 = r[1] / bs * bs;
    let x1 = r[2].min(sf.w);
    let y1 = r[3].min(sf.h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let src = sf.mem + (y0 / bs) * sf.pitch + (x0 / bs) * bpb;
    let (w, h) = (x1 - x0, y1 - y0);
    let bpp = fmt::page_bpp(sf.fmt);
    let (gpu, level, f, pitch) = (sf.gpu, sf.level, sf.fmt, sf.pitch);
    // (P8: through the current palette, when there is one)
    let pal = if f == fmt::P8 {
        sf.pal_gen = d.dev.as_ref().map_or(0, |dev| dev.pal_gen);
        d.dev.as_ref().and_then(|dev| dev.palettes.get(&dev.cur_palette)).map(|p| **p)
    } else {
        None
    };
    if let Some(layer) = sf.face {
        // (a cube's face: its layer of the page texture)
        gpu::record(|b: &mut Batch| {
            let at = b.blob.len().next_multiple_of(4);
            b.blob.resize(at, 0);
            match &pal {
                Some(p) => fmt::convert_p8(src as *const u8, pitch as usize, w, h, p, &mut b.blob),
                None => fmt::convert(f, src as *const u8, pitch as usize, w, h, &mut b.blob),
            }
            b.cmd(&[gpu::WRITE_TEXTURE2, gpu, level, layer, x0, y0, w, h, w * bpp, at as u32]);
        });
        return;
    }
    if super::stats::on() && w * h >= 1 << 20 {
        eprintln!("d3d9: upload {w}x{h} at {x0},{y0} of {}x{} format {f:#x} level {level} (page texture {gpu}, surface {s})", sf.w, sf.h);
    }
    if fmt::same_on_page(f) {
        // the page reads the rows from guest memory itself (no copy here); `mem` stays as it
        // is until the page has run the batch (see lock_surface)
        let r = [x0, y0, x1, y1];
        if let Some((g, p)) = sf.pending {
            if g == gpu::batch_gen() && p[0] <= x0 && p[1] <= y0 && p[2] >= x1 && p[3] >= y1 {
                return; // already in this batch (it reads the newest pixels)
            }
        }
        let ((), g) = gpu::record_gen(|b: &mut Batch| b.cmd_mem(&[gpu::WRITE_TEXTURE_MEM, gpu, level, x0, y0, w, h, pitch, src]));
        sf.pending = Some(match sf.pending {
            Some((pg, p)) if pg == g => (g, [p[0].min(x0), p[1].min(y0), p[2].max(x1), p[3].max(y1)]),
            _ => (g, r),
        });
        return;
    }
    gpu::record(|b: &mut Batch| {
        let at = b.blob.len().next_multiple_of(4);
        b.blob.resize(at, 0);
        match &pal {
            Some(p) => fmt::convert_p8(src as *const u8, pitch as usize, w, h, p, &mut b.blob),
            None => fmt::convert(f, src as *const u8, pitch as usize, w, h, &mut b.blob),
        }
        b.cmd(&[gpu::WRITE_TEXTURE, gpu, level, x0, y0, w, h, w * bpp, at as u32]);
    });
}

/// The P8 textures a draw reads whose page texels were made with another palette than the
/// current one (SetPaletteEntries / SetCurrentTexturePalette since): made again from their
/// guest copies, so a draw shows the palette current at the draw, as Direct3D 9 does.
unsafe fn refresh_p8(d: &mut D3d) {
    let Some(dev) = d.dev.as_ref() else { return };
    let gen = dev.pal_gen;
    let mut stale = Vec::new();
    for &t in dev.st.textures.iter().filter(|&&t| t != 0) {
        let Some(Data::Texture(tx)) = d.objs.get(&t).map(|o| &o.data) else { continue };
        if tx.fmt != fmt::P8 || tx.depth != 0 {
            continue;
        }
        for &s in &tx.levels {
            if let Some(Data::Surface(sf)) = d.objs.get(&s).map(|o| &o.data) {
                if sf.pal_gen != gen && sf.mem != 0 && sf.gpu != 0 {
                    stale.push((s, sf.w, sf.h));
                }
            }
        }
    }
    for (s, w, h) in stale {
        upload(d, s, [0, 0, w, h]);
    }
}

/// Before the guest copy of a surface changes: the uploads the page has not read yet take a
/// copy of its rows (waiting only if the page is reading them right now).
fn settle(d: &mut D3d, s: u32) {
    if let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) {
        if let Some((g, _)) = sf.pending.take() {
            if !gpu::is_done(g) {
                let lo = sf.mem;
                let hi = sf.mem.saturating_add(sf.pitch.saturating_mul(sf.h));
                if let Some(r) = gpu::detach_mem(lo, hi) {
                    gpu::wait_done(r);
                }
            }
        }
    }
}

unsafe fn lock_surface(d: &mut D3d, s: u32, out: u32, rect: u32, flags: u32, after: &mut After) -> u32 {
    let Some(mem) = surface_mem(d, s) else { return D3DERR_INVALIDCALL };
    if flags & LOCK_READONLY == 0 {
        settle(d, s);
    }
    let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) else { return D3DERR_INVALIDCALL };
    if sf.lock.is_some() {
        return D3DERR_INVALIDCALL;
    }
    let r = if rect != 0 {
        [rd(rect, 0), rd(rect, 1), rd(rect, 2), rd(rect, 3)]
    } else {
        [0, 0, sf.w, sf.h]
    };
    let (bpb, bs) = fmt::layout(sf.fmt).unwrap_or((4, 1));
    sf.lock = Some(if flags & LOCK_READONLY != 0 { [0, 0, 0, 0] } else { r });
    let bits = mem + (r[1] / bs) * sf.pitch + (r[0] / bs) * bpb;
    wr(out, 0, sf.pitch);
    wrp(out + ptr(), 0, bits);
    if sf.gpu_newer {
        sf.shadow = None;
    }
    if sf.gpu_newer && flags & LOCK_DISCARD == 0 && fmt::layout(sf.fmt).is_some_and(|l| l.1 == 1) {
        // the page has the pixels: read them back first
        sf.gpu_newer = false;
        let (gpu, level, w, h, pitch, f) = (sf.gpu, sf.level, sf.w, sf.h, sf.pitch, sf.fmt);
        *after = readback_as(d, gpu, level, w, h, f, mem, pitch);
    }
    S_OK
}

/// A readback of a page texture of `f`'s texels to `mem` (rows `pitch` apart): straight when
/// the page has them as they are, else through a copy of the page's texels (pack).
fn readback_as(d: &mut D3d, gpu: u32, level: u32, w: u32, h: u32, f: u32, mem: u32, pitch: u32) -> After {
    if fmt::same_on_page(f) || f == fmt::A32B32G32R32F {
        return After::Readback(readback(d, gpu, level, w, h, mem, pitch));
    }
    let page = fmt::page_bpp(f);
    let staging = d.guest_alloc(w * h * page);
    if staging == 0 {
        return After::None;
    }
    let req = readback(d, gpu, level, w, h, staging, w * page);
    After::ReadbackAs { req, staging, fmt: f, w, h, dst: mem, pitch }
}

fn readback(d: &mut D3d, gpu: u32, level: u32, w: u32, h: u32, mem: u32, pitch: u32) -> u32 {
    let dev = d.dev.as_mut().unwrap();
    dev.readbacks += 1;
    let req = dev.readbacks;
    gpu::record(|b| b.cmd(&[gpu::READBACK, req, gpu, level, 0, 0, w, h, mem, pitch]));
    req
}

unsafe fn unlock_surface(d: &mut D3d, s: u32) -> u32 {
    let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) else { return D3DERR_INVALIDCALL };
    let Some(r) = sf.lock.take() else { return D3DERR_INVALIDCALL };
    if d.trace && sf.mem != 0 {
        let n = (sf.pitch * sf.h) as usize;
        let nz = std::slice::from_raw_parts(sf.mem as *const u8, n).iter().filter(|&&b| b != 0).count();
        eprintln!("d3d9: unlock surface {s} {}x{} fmt {} pool {} rect {r:?}: {nz} of {n} bytes non-zero", sf.w, sf.h, sf.fmt, sf.pool);
    }
    if r == [0, 0, sf.w, sf.h] {
        upload_changed(d, s);
    } else if r[2] > r[0] {
        upload(d, s, r);
    }
    S_OK
}

unsafe fn surface_call(d: &mut D3d, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    match m {
        3 => device_out(d, a(1)),
        4..=6 => private_data(d, m, this, a),
        7 | 8 => 0,
        9 => 0,
        10 => RT_SURFACE,
        11 => {
            // GetContainer(riid, out)
            let Some(Data::Surface(sf)) = d.objs.get(&this).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
            let c = if sf.container != 0 { sf.container } else { d.dev.as_ref().map_or(0, |x| x.obj) };
            if c == 0 {
                return super::E_NOINTERFACE;
            }
            d.add_ref(c);
            wrp(a(2), 0, d.addr_of(c));
            S_OK
        }
        12 => {
            let Some(Data::Surface(sf)) = d.objs.get(&this).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
            let desc = [sf.fmt, RT_SURFACE, sf.usage, sf.pool, 0, 0, sf.w, sf.h];
            std::ptr::copy_nonoverlapping(desc.as_ptr(), a(1) as *mut u32, 8);
            S_OK
        }
        13 => lock_surface(d, this, a(1), a(2), a(3), after),
        14 => unlock_surface(d, this),
        0xf0 => {
            // (PE side) GetDC: remember (dc, bitmap); ReleaseDC: hand the bitmap back
            let s = d.id_of(a(0));
            let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) else { return D3DERR_INVALIDCALL };
            if a(1) != 0 {
                sf.dc = a(1);
                sf.dc_bitmap = a(2);
            } else {
                wrp(a(2), 0, sf.dc_bitmap);
                sf.dc = 0;
                sf.dc_bitmap = 0;
            }
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

unsafe fn private_data(d: &mut D3d, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    let mut guid = [0u8; 16];
    if a(1) == 0 {
        return D3DERR_INVALIDCALL;
    }
    std::ptr::copy_nonoverlapping(a(1) as *const u8, guid.as_mut_ptr(), 16);
    let Some(o) = d.objs.get_mut(&this) else { return D3DERR_INVALIDCALL };
    match m {
        4 => {
            // SetPrivateData(guid, data, size, flags): (IUnknown flag: keeps the pointer only)
            let data = std::slice::from_raw_parts(a(2) as *const u8, a(3) as usize).to_vec();
            o.private.retain(|p| p.0 != guid);
            o.private.push((guid, data));
            S_OK
        }
        5 => {
            let Some(p) = o.private.iter().find(|p| p.0 == guid) else { return D3DERR_NOTFOUND };
            let size = a(3);
            if size == 0 {
                return D3DERR_INVALIDCALL;
            }
            let have = rd(size, 0) as usize;
            wr(size, 0, p.1.len() as u32);
            if a(2) == 0 {
                return S_OK;
            }
            if have < p.1.len() {
                return 0x8876_0868; // D3DERR_MOREDATA
            }
            std::ptr::copy_nonoverlapping(p.1.as_ptr(), a(2) as *mut u8, p.1.len());
            S_OK
        }
        _ => {
            o.private.retain(|p| p.0 != guid);
            S_OK
        }
    }
}

unsafe fn texture_call(d: &mut D3d, iface: u32, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let Some(Data::Texture(t)) = d.objs.get_mut(&this).map(|o| &mut o.data) else { return D3DERR_INVALIDCALL };
    let level = |t: &Texture, l: u32| t.levels.get(l as usize).copied();
    if iface == IF_CUBE && m >= 17 {
        // GetLevelDesc(level, desc), GetCubeMapSurface(face, level, out), LockRect(face,
        // level, locked, rect, flags), UnlockRect(face, level), AddDirtyRect(face, rect): a
        // face's level is the 2D texture's level with the face in front
        let n = t.level_count();
        if m == 17 {
            if a(1) >= n {
                return D3DERR_INVALIDCALL;
            }
            let Some(s) = level(t, a(1)) else { return D3DERR_INVALIDCALL };
            return surface_call(d, 12, s, &|i| if i == 1 { a(2) } else { 0 }, after);
        }
        if a(1) >= 6 {
            return D3DERR_INVALIDCALL;
        }
        if m == 21 {
            return S_OK;
        }
        if a(2) >= n {
            return D3DERR_INVALIDCALL;
        }
        let Some(s) = level(t, a(1) * n + a(2)) else { return D3DERR_INVALIDCALL };
        return match m {
            18 => {
                if a(3) == 0 {
                    return D3DERR_INVALIDCALL;
                }
                d.add_ref(s);
                wrp(a(3), 0, d.addr_of(s));
                S_OK
            }
            19 => lock_surface(d, s, a(3), a(4), a(5), after),
            _ => unlock_surface(d, s),
        };
    }
    if iface == IF_VOLUMETEX && m >= 17 {
        // GetLevelDesc(level, desc), GetVolumeLevel(level, out), LockBox(level, locked, box,
        // flags), UnlockBox(level), AddDirtyBox(box)
        if m == 21 {
            return S_OK;
        }
        let Some(v) = level(t, a(1)) else { return D3DERR_INVALIDCALL };
        return match m {
            17 => volume_call(d, 8, v, &|i| if i == 1 { a(2) } else { 0 }),
            18 => {
                if a(2) == 0 {
                    return D3DERR_INVALIDCALL;
                }
                d.add_ref(v);
                wrp(a(2), 0, d.addr_of(v));
                S_OK
            }
            19 => lock_volume(d, v, a(2), a(3), a(4)),
            _ => unlock_volume(d, v),
        };
    }
    match m {
        3 => device_out(d, a(1)),
        4..=6 => private_data(d, m, this, a),
        7 | 8 => 0,
        9 => 0,
        10 => match iface {
            IF_VOLUMETEX => RT_VOLUMETEX,
            IF_CUBE => RT_CUBETEX,
            _ => RT_TEXTURE,
        },
        11 => {
            let old = t.lod;
            t.lod = a(1);
            old
        }
        12 => t.lod,
        13 => t.level_count(),
        14 => S_OK,
        15 => 2,
        16 => 0,
        17 => {
            let Some(s) = level(t, a(1)) else { return D3DERR_INVALIDCALL };
            surface_call(d, 12, s, &|i| if i == 1 { a(2) } else { 0 }, after)
        }
        18 => {
            let Some(s) = level(t, a(1)) else { return D3DERR_INVALIDCALL };
            d.add_ref(s);
            wrp(a(2), 0, d.addr_of(s));
            S_OK
        }
        19 => {
            let Some(s) = level(t, a(1)) else { return D3DERR_INVALIDCALL };
            lock_surface(d, s, a(2), a(3), a(4), after)
        }
        20 => {
            let Some(s) = level(t, a(1)) else { return D3DERR_INVALIDCALL };
            unlock_surface(d, s)
        }
        21 => S_OK,
        _ => E_NOTIMPL,
    }
}

unsafe fn buffer_call(d: &mut D3d, iface: u32, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    match m {
        3 => device_out(d, a(1)),
        4..=6 => private_data(d, m, this, a),
        7 | 8 => 0,
        9 => 0,
        10 => if iface == IF_VB { RT_VB } else { RT_IB },
        11 => {
            // Lock(offset, size (0: to the end), out, flags)
            let Some(Data::Buffer(b)) = d.objs.get_mut(&this).map(|o| &mut o.data) else { return D3DERR_INVALIDCALL };
            let off = a(1).min(b.len);
            wrp(a(3), 0, b.mem + off);
            // (not D3DLOCK_READONLY: its page copy is to be sent again, `page_buffer`)
            if a(4) & 0x10 == 0 {
                let end = if a(2) == 0 { b.len } else { off.saturating_add(a(2)).min(b.len) };
                b.dirty = Some(b.dirty.map_or((off, end), |(lo, hi)| (lo.min(off), hi.max(end))));
                b.gen += 1;
                b.locked = true;
            }
            S_OK
        }
        12 => {
            // (what it wrote while locked: sent at the next draw)
            if let Some(Data::Buffer(b)) = d.objs.get_mut(&this).map(|o| &mut o.data) {
                if std::mem::take(&mut b.locked) {
                    b.gen += 1;
                }
            }
            S_OK
        }
        13 => {
            let Some(Data::Buffer(b)) = d.objs.get(&this).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
            let desc = if iface == IF_VB {
                [fmt::A8R8G8B8.min(100), RT_VB, b.usage, b.pool, b.len, b.fmt]
            } else {
                [b.fmt, RT_IB, b.usage, b.pool, b.len, 0]
            };
            let desc = if iface == IF_VB { [100, desc[1], desc[2], desc[3], desc[4], desc[5]] } else { desc };
            let n = if iface == IF_VB { 6 } else { 5 };
            std::ptr::copy_nonoverlapping(desc.as_ptr(), a(1) as *mut u32, n);
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

unsafe fn update_surface(d: &mut D3d, src: u32, srect: u32, dst: u32, dpoint: u32) -> u32 {
    let (s, t) = (d.id_of(src), d.id_of(dst));
    let (Some(sm), Some(tm)) = (surface_mem(d, s), surface_mem(d, t)) else { return D3DERR_INVALIDCALL };
    settle(d, t);
    let (Some(Data::Surface(a)), Some(Data::Surface(b))) = (d.objs.get(&s).map(|o| &o.data), d.objs.get(&t).map(|o| &o.data))
    else {
        return D3DERR_INVALIDCALL;
    };
    if a.fmt != b.fmt {
        return D3DERR_INVALIDCALL;
    }
    let r = if srect != 0 { [rd(srect, 0), rd(srect, 1), rd(srect, 2), rd(srect, 3)] } else { [0, 0, a.w, a.h] };
    let (dx, dy) = if dpoint != 0 { (rd(dpoint, 0), rd(dpoint, 1)) } else { (0, 0) };
    let (bpb, bs) = fmt::layout(a.fmt).unwrap_or((4, 1));
    let w = (r[2] - r[0]).min(b.w.saturating_sub(dx));
    let h = (r[3] - r[1]).min(b.h.saturating_sub(dy));
    let row = w.div_ceil(bs) * bpb;
    for y in 0..h.div_ceil(bs) {
        let sp = sm + (r[1] / bs + y) * a.pitch + (r[0] / bs) * bpb;
        let dp = tm + (dy / bs + y) * b.pitch + (dx / bs) * bpb;
        std::ptr::copy(sp as *const u8, dp as *mut u8, row as usize);
    }
    upload(d, t, [dx, dy, dx + w, dy + h]);
    S_OK
}

unsafe fn update_texture(d: &mut D3d, src: u32, dst: u32) -> u32 {
    let (s, t) = (d.id_of(src), d.id_of(dst));
    let (Some(Data::Texture(a)), Some(Data::Texture(b))) = (d.objs.get(&s).map(|o| &o.data), d.objs.get(&t).map(|o| &o.data))
    else {
        return D3DERR_INVALIDCALL;
    };
    if (a.depth == 0) != (b.depth == 0) || a.faces != b.faces {
        return D3DERR_INVALIDCALL;
    }
    if a.faces != 0 {
        // (a cube: each face's levels from the top, as 2D textures pair them; the source may
        // have more levels than the target)
        let (na, nb) = (a.level_count(), b.level_count());
        if a.fmt != b.fmt || na < nb || a.w != b.w {
            return D3DERR_INVALIDCALL;
        }
        let pairs: Vec<(u32, u32)> = (0..6).flat_map(|f| (0..nb).map(move |l| (f, l))).map(|(f, l)| (a.levels[(f * na + l) as usize], b.levels[(f * nb + l) as usize])).collect();
        for (x, y) in pairs {
            let (sa, sb) = (d.addr_of(x), d.addr_of(y));
            update_surface(d, sa, 0, sb, 0);
        }
        return S_OK;
    }
    if a.depth != 0 {
        // (Windows: a system memory texture into a default pool one, the same format and size)
        if a.pool != POOL_SYSTEMMEM || b.pool != POOL_DEFAULT || a.fmt != b.fmt || (a.w, a.h, a.depth) != (b.w, b.h, b.depth) || a.levels.len() < b.levels.len() {
            return D3DERR_INVALIDCALL;
        }
        let pairs: Vec<(u32, u32)> = a.levels.iter().copied().zip(b.levels.iter().copied()).collect();
        for (x, y) in pairs {
            let (Some(xm), Some(ym)) = (volume_mem(d, x), volume_mem(d, y)) else { return D3DERR_INVALIDCALL };
            let Some(Data::Volume(v)) = d.objs.get(&y).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
            let (len, b) = (v.slice * v.depth, [0, 0, 0, v.w, v.h, v.depth]);
            std::ptr::copy(xm as *const u8, ym as *mut u8, len as usize);
            upload_volume(d, y, b);
        }
        return S_OK;
    }
    let pairs: Vec<(u32, u32)> = a.levels.iter().copied().zip(b.levels.iter().copied()).collect();
    for (x, y) in pairs {
        let sa = d.addr_of(x);
        let sb = d.addr_of(y);
        update_surface(d, sa, 0, sb, 0);
    }
    S_OK
}

unsafe fn get_rt_data(d: &mut D3d, rt: u32, dst: u32, after: &mut After) -> u32 {
    let (s, t) = (d.id_of(rt), d.id_of(dst));
    let Some(Data::Surface(a)) = d.objs.get(&s).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
    let (gpu, level, w, h, page, af) = (a.gpu, a.level, a.w, a.h, fmt::page_bpp(a.fmt), a.fmt);
    let Some(mem) = surface_mem(d, t) else { return D3DERR_INVALIDCALL };
    let Some(Data::Surface(b)) = d.objs.get(&t).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
    // (the target's format, or one the page has as it is for one it has as it is: A8R8G8B8
    // and X8R8G8B8)
    let same = b.fmt == af || fmt::same_on_page(b.fmt) && fmt::same_on_page(af) && fmt::layout(b.fmt) == Some((page, 1));
    if b.w != w || b.h != h || gpu == 0 || !same || fmt::layout(b.fmt).map_or(true, |l| l.1 != 1) {
        return D3DERR_INVALIDCALL;
    }
    let (pitch, f) = (b.pitch, b.fmt);
    *after = readback_as(d, gpu, level, w, h, f, mem, pitch);
    S_OK
}

/// (texture, level, w, h) of a surface on the page
fn surface_gpu(d: &D3d, s: u32) -> Option<(u32, u32, u32, u32, u32)> {
    match d.objs.get(&s).map(|o| &o.data) {
        Some(Data::Surface(sf)) if sf.gpu != 0 => Some((sf.gpu, sf.level, sf.w, sf.h, sf.fmt)),
        _ => None,
    }
}

fn mark_rendered(d: &mut D3d, s: u32) {
    if let Some(Data::Surface(sf)) = d.objs.get_mut(&s).map(|o| &mut o.data) {
        sf.gpu_newer = true;
        sf.shadow = None;
    }
}

unsafe fn stretch_rect(d: &mut D3d, src: u32, srect: u32, dst: u32, drect: u32, filter: u32) -> u32 {
    let (s, t) = (d.id_of(src), d.id_of(dst));
    let (Some(a), Some(b)) = (surface_gpu(d, s), surface_gpu(d, t)) else {
        // system memory source: upload through the destination's copy
        if let (Some(Data::Surface(_)), Some(_)) = (d.objs.get(&s).map(|o| &o.data), surface_gpu(d, t)) {
            return update_surface(d, src, srect, dst, if drect != 0 { drect } else { 0 });
        }
        return D3DERR_INVALIDCALL;
    };
    let sr = if srect != 0 { [rd(srect, 0), rd(srect, 1), rd(srect, 2), rd(srect, 3)] } else { [0, 0, a.2, a.3] };
    let dr = if drect != 0 { [rd(drect, 0), rd(drect, 1), rd(drect, 2), rd(drect, 3)] } else { [0, 0, b.2, b.3] };
    gpu::record(|bt| {
        bt.cmd(&[
            gpu::COPY,
            a.0,
            a.1,
            sr[0],
            sr[1],
            sr[2] - sr[0],
            sr[3] - sr[1],
            b.0,
            b.1,
            dr[0],
            dr[1],
            dr[2] - dr[0],
            dr[3] - dr[1],
            (filter >= 2) as u32,
        ])
    });
    if let Some(dev) = d.dev.as_mut() {
        dev.batch_target = None;
    }
    mark_rendered(d, t);
    S_OK
}

unsafe fn color_fill(d: &mut D3d, s: u32, rect: u32, color: u32) -> u32 {
    let s = d.id_of(s);
    // (D3DPOOL_DEFAULT surfaces only: a managed texture's level is not filled on Windows)
    if !matches!(d.objs.get(&s).map(|o| &o.data), Some(Data::Surface(sf)) if sf.pool == POOL_DEFAULT) {
        return D3DERR_INVALIDCALL;
    }
    let Some(g) = surface_gpu(d, s) else { return D3DERR_INVALIDCALL };
    let r = if rect != 0 { [rd(rect, 0), rd(rect, 1), rd(rect, 2), rd(rect, 3)] } else { [0, 0, g.2, g.3] };
    gpu::record(|b| b.cmd(&[gpu::FILL, g.0, g.1, r[0], r[1], r[2] - r[0], r[3] - r[1], fmt::target_color(g.4, color)]));
    if let Some(dev) = d.dev.as_mut() {
        dev.batch_target = None;
    }
    mark_rendered(d, s);
    S_OK
}

unsafe fn create_decl(d: &mut D3d, elems: u32, out: u32) -> u32 {
    let mut raw = Vec::new();
    let mut el = Vec::new();
    let mut p = elems;
    loop {
        let w0 = rd(p, 0);
        let w1 = rd(p, 1);
        raw.push([w0, w1]);
        let stream = w0 & 0xffff;
        if stream == 0xff {
            break;
        }
        // { WORD stream, offset; BYTE type, method, usage, usage index }
        el.push(Elem { stream, offset: w0 >> 16, ty: w1 & 0xff, usage: (w1 >> 16) & 0xff, index: w1 >> 24 });
        p += 8;
        if raw.len() > 64 {
            return D3DERR_INVALIDCALL;
        }
    }
    let o = d.new_obj(IF_DECL, 0, Data::Decl(Decl { elems: el, raw, fvf: 0 }));
    wrp(out, 0, o);
    S_OK
}

/// The vertex declaration of an FVF.
pub fn fvf_elems(fvf: u32) -> (Vec<Elem>, u32) {
    let mut e = Vec::new();
    let mut off = 0;
    let mut push = |ty: u32, usage: u32, index: u32, size: u32, off: &mut u32| {
        e.push(Elem { stream: 0, offset: *off, ty, usage, index });
        *off += size;
    };
    let pos = fvf & 0x400e;
    match pos {
        0x2 => push(2, shader::USAGE_POSITION, 0, 12, &mut off),
        0x4 => push(3, shader::USAGE_POSITIONT, 0, 16, &mut off),
        0x6 | 0x8 | 0xa | 0xc | 0xe => {
            push(2, shader::USAGE_POSITION, 0, 12, &mut off);
            let n = (pos - 0x6) / 2 + 1; // blend weights
            off += n * 4;
        }
        0x4002 => {
            push(3, shader::USAGE_POSITION, 0, 16, &mut off);
        }
        _ => {}
    }
    if fvf & 0x10 != 0 {
        push(2, shader::USAGE_NORMAL, 0, 12, &mut off);
    }
    if fvf & 0x20 != 0 {
        push(0, shader::USAGE_PSIZE, 0, 4, &mut off);
    }
    if fvf & 0x40 != 0 {
        push(4, shader::USAGE_COLOR, 0, 4, &mut off);
    }
    if fvf & 0x80 != 0 {
        push(4, shader::USAGE_COLOR, 1, 4, &mut off);
    }
    let ntex = (fvf >> 8) & 0xf;
    for t in 0..ntex.min(8) {
        let size = match (fvf >> (16 + t * 2)) & 3 {
            0 => 2,
            1 => 3,
            2 => 4,
            _ => 1,
        };
        push(size - 1, shader::USAGE_TEXCOORD, t, size * 4, &mut off);
    }
    (e, off)
}

unsafe fn create_shader(d: &mut D3d, pixel: bool, code: u32, out: u32) -> u32 {
    if code == 0 || out == 0 {
        return D3DERR_INVALIDCALL;
    }
    let mut n = 1;
    let p = code as *const u32;
    let ver = *p;
    loop {
        let t = *p.add(n);
        n += 1;
        if t == 0xffff {
            break;
        }
        if t & 0xffff == 0xfffe {
            n += ((t >> 16) & 0x7fff) as usize;
        }
        if n > 1 << 20 {
            return D3DERR_INVALIDCALL;
        }
    }
    let code = std::slice::from_raw_parts(p, n).to_vec();
    let parsed = Shader::parse(&code);
    if d.trace {
        eprintln!(
            "d3d9: Create{}Shader {}.{} ({} tokens){}",
            if pixel { "Pixel" } else { "Vertex" },
            (ver >> 8) & 0xff,
            ver & 0xff,
            n,
            if parsed.is_none() { ": can't parse" } else { "" }
        );
    }
    let dev = dev!(d);
    dev.serial += 1;
    let serial = dev.serial;
    // (IPPAI_D3D_TRACE_CODE=1 with the trace: the tokens too, to translate them again by hand)
    let dump = d.trace && std::env::var_os("IPPAI_D3D_TRACE_CODE").is_some();
    let words = if dump { code.iter().map(|t| format!("{t:08x}")).collect::<Vec<_>>().join(" ") } else { String::new() };
    let o = d.new_obj(if pixel { IF_PS } else { IF_VS }, 0, Data::Shader(ShaderObj { code, parsed, serial }));
    if dump {
        eprintln!("d3d9:   shader {o} serial {serial}: {words}");
    }
    wrp(out, 0, o);
    S_OK
}

unsafe fn clear(d: &mut D3d, count: u32, rects: u32, flags: u32, color: u32, z: f32, stencil: u32) -> u32 {
    let Some(dev) = d.dev.as_ref() else { return D3DERR_INVALIDCALL };
    let rt = dev.rt[0];
    let ds = if dev.ds != 0 && dev.st.rs.get(7).copied().unwrap_or(0) != 0 || dev.ds != 0 { dev.ds } else { 0 };
    let vp = dev.st.viewport;
    let sc = dev.st.scissor;
    let scissor = dev.st.rs[174] != 0;
    // (as Windows: a list with a count of 0 clears nothing, a count without a list clears it
    // all; no target, depth or stencil flag, depth or stencil without a depth buffer and stencil
    // without stencil bits are refused)
    if count == 0 && rects != 0 {
        return S_OK;
    }
    if flags & 7 == 0 || flags & 6 != 0 && ds == 0 {
        return D3DERR_INVALIDCALL;
    }
    if flags & 4 != 0 && !d.objs.get(&ds).is_some_and(|o| matches!(&o.data, Data::Surface(z) if fmt::has_stencil(z.fmt))) {
        return D3DERR_INVALIDCALL;
    }
    // (stencil values past the 8 bits saturate, as Windows)
    let stencil = stencil.min(0xff);
    let (Some(target), dsg) = (surface_gpu(d, rt), surface_gpu(d, ds)) else { return S_OK };
    ensure_target(d, rt, ds);
    let mut area = [vp[0] as i32, vp[1] as i32, (vp[0] + vp[2]) as i32, (vp[1] + vp[3]) as i32];
    if scissor {
        area = [area[0].max(sc[0]), area[1].max(sc[1]), area[2].min(sc[2]), area[3].min(sc[3])];
    }
    let mut list = Vec::new();
    if rects == 0 {
        list.push(area);
    } else {
        for i in 0..count as usize {
            let r = [rd(rects, i * 4) as i32, rd(rects, i * 4 + 1) as i32, rd(rects, i * 4 + 2) as i32, rd(rects, i * 4 + 3) as i32];
            list.push([r[0].max(area[0]), r[1].max(area[1]), r[2].min(area[2]), r[3].min(area[3])]);
        }
    }
    let flags = if dsg.is_none() { flags & 1 } else { flags };
    // render targets 1-3 are cleared too (in a row from 1, of the size of 0)
    let more: Vec<(u32, (u32, u32, u32, u32, u32))> = if flags & 1 != 0 {
        d.dev.as_ref().unwrap().rt[1..]
            .iter()
            .map_while(|&r| surface_gpu(d, r).filter(|g| (g.2, g.3) == (target.2, target.3)).map(|g| (r, g)))
            .collect()
    } else {
        Vec::new()
    };
    // (a target of fewer channels: what it does not keep as it reads, format::target_color)
    let c = fmt::target_clear(target.4, color).map(f2u);
    for r in list {
        if r[2] <= r[0] || r[3] <= r[1] {
            continue;
        }
        let full = r == [0, 0, target.2 as i32, target.3 as i32];
        gpu::record(|b| {
            b.cmd(&[gpu::CLEAR, flags, c[0], c[1], c[2], c[3], f2u(z), stencil]);
            if full {
                b.cmd(&[0, 0, 0, 0]);
            } else {
                b.cmd(&[r[0] as u32, r[1] as u32, (r[2] - r[0]) as u32, (r[3] - r[1]) as u32]);
            }
            for (_, g) in &more {
                b.cmd(&[gpu::FILL, g.0, g.1, r[0] as u32, r[1] as u32, (r[2] - r[0]) as u32, (r[3] - r[1]) as u32, fmt::target_color(g.4, color)]);
            }
        });
    }
    mark_rendered(d, rt);
    for (m, _) in more {
        mark_rendered(d, m);
    }
    S_OK
}

/// A draw's scissor within the depth buffer of size `z`, bound whether the depth test is on or
/// not: Windows' WARP draws nothing past a depth buffer smaller than the target (winref
/// replay_d3d9clear r483..r515, a D16 of 8x8 under a 16x16 target, #63)
fn depth_clip(sc: [i32; 4], z: Option<(u32, u32)>) -> [i32; 4] {
    match z {
        Some((w, h)) => [sc[0], sc[1], sc[2].min(w as i32), sc[3].min(h as i32)],
        None => sc,
    }
}
#[cfg(test)]
mod depth_clip_tests {
    use super::depth_clip;
    #[test]
    fn as_windows() {
        assert_eq!(depth_clip([0, 0, 16, 16], Some((8, 8))), [0, 0, 8, 8]);
        assert_eq!(depth_clip([0, 0, 16, 16], Some((32, 32))), [0, 0, 16, 16]);
        assert_eq!(depth_clip([0, 0, 16, 16], None), [0, 0, 16, 16]);
        assert_eq!(depth_clip([4, 4, 12, 12], Some((32, 8))), [4, 4, 12, 8]);
        // (a scissor past the depth buffer: empty, nothing drawn)
        let c = depth_clip([10, 10, 16, 16], Some((8, 8)));
        assert!(c[2] <= c[0] && c[3] <= c[1]);
    }
    /// whatever the sizes, what is left is inside the scissor and the depth buffer
    #[test]
    fn inside_both() {
        let mut x = 0x2545_f491u32;
        for _ in 0..50_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let sc = [(x % 40) as i32, ((x >> 6) % 40) as i32, ((x >> 12) % 80) as i32, ((x >> 19) % 80) as i32];
            let (zw, zh) = (1 + (x >> 3) % 64, 1 + (x >> 9) % 64);
            let c = depth_clip(sc, Some((zw, zh)));
            assert_eq!((c[0], c[1]), (sc[0], sc[1]));
            assert!(c[2] <= sc[2] && c[3] <= sc[3] && c[2] <= zw as i32 && c[3] <= zh as i32);
        }
    }
}

/// Makes the page draw into render target 0 and `more` (1-3) at once.
fn ensure_targets(d: &mut D3d, rt: u32, more: &[u32], ds: u32) {
    if more.is_empty() {
        return ensure_target(d, rt, ds);
    }
    let Some(t) = surface_gpu(d, rt) else { return };
    // (a depth buffer of another size than the target: the page draws with one of the target's
    // size, render/d3d.rs depth_for)
    let z = surface_gpu(d, ds);
    let mut c = vec![gpu::SET_TARGETS2, 1 + more.len() as u32, t.0, t.1, 0];
    for &m in more {
        let g = surface_gpu(d, m).unwrap();
        c.extend_from_slice(&[g.0, g.1, 0]);
    }
    c.extend_from_slice(&[z.map_or(0, |z| z.0), 0, 0]);
    gpu::record(|b| b.cmd(&c));
    // (the next single target goes out again)
    d.dev.as_mut().unwrap().batch_target = None;
}

/// Makes the page draw into the device's targets.
fn ensure_target(d: &mut D3d, rt: u32, ds: u32) {
    let t = surface_gpu(d, rt);
    let z = surface_gpu(d, ds);
    let Some(t) = t else { return };
    // (a depth buffer of another size than the target: the page draws with one of the
    // target's size, render/d3d.rs depth_for; Windows uses a smaller one too)
    let key = (t.0, t.1, z.map_or(0, |z| z.0));
    let gen = gpu::batch_gen();
    let dev = d.dev.as_mut().unwrap();
    if dev.batch_target != Some(key) || dev.batch_gen != gen {
        gpu::record(|b| b.cmd(&[gpu::SET_TARGET, key.0, key.1, key.2]));
        dev.batch_target = Some(key);
        dev.batch_gen = gen;
    }
}

/// The rectangle at `p` (RECT: l t r b), None for NULL.
unsafe fn rect_at(p: u32) -> Option<[i32; 4]> {
    (p != 0).then(|| [rd(p, 0) as i32, rd(p, 1) as i32, rd(p, 2) as i32, rd(p, 3) as i32])
}

/// Where a windowed Present shows its frame: `dst` (Present's pDestRect, in the window's client
/// coordinates) of the client area `client` (on the screen), all of it without one.
pub fn present_dest(client: [i32; 4], dst: Option<[i32; 4]>) -> [i32; 4] {
    match dst {
        Some([l, t, r, b]) if r > l && b > t => [client[0] + l, client[1] + t, client[0] + r, client[1] + b],
        _ => client,
    }
}

/// Present's pSourceRect as PRESENT's source words (render/d3d.rs `present_src`): of a `w`x`h`
/// back buffer, clipped to it; all of it (`w`, `h`) without one.
pub fn present_src_words(src: Option<[i32; 4]>, w: u32, h: u32) -> (u32, u32) {
    let clip = |v: i32, m: u32| v.clamp(0, m.min(0xffff) as i32) as u32;
    match src {
        Some([l, t, r, b]) if r > l && b > t && clip(r, w) > clip(l, w) && clip(b, h) > clip(t, h) && clip(b, h) > 0 => {
            (clip(l, w) | clip(t, h) << 16, clip(r, w) | clip(b, h) << 16)
        }
        _ => (w, h),
    }
}

/// Present (IDirect3DDevice9 and IDirect3DSwapChain9): `src`, `dst` its pSourceRect and
/// pDestRect (windowed: the part of the back buffer shown, and where in the client area: 俺の姿が､
/// 透明に! presents (0,0)-(1920,1080) of a bigger back buffer), `over` hDestWindowOverride.
unsafe fn present(d: &mut D3d, after: &mut After, src: u32, dst: u32, over: u32) -> u32 {
    // IPPAI_D3D_TRACE_EVERY=n: trace one whole frame in n
    static EVERY: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let every = *EVERY.get_or_init(|| std::env::var("IPPAI_D3D_TRACE_EVERY").ok().and_then(|v| v.parse().ok()));
    if let (Some(n), Some(dev)) = (every, d.dev.as_ref()) {
        d.trace = (dev.frames + 1) % n == 0;
    }
    let Some(dev) = d.dev.as_mut() else { return D3DERR_INVALIDCALL };
    let windowed = dev.pp[8] != 0;
    let (bb, hwnd) = (dev.backbuffer, if over != 0 && windowed { over } else { dev.hwnd });
    dev.frames += 1;
    if d.trace && dev.frames % 100 == 1 {
        eprintln!("d3d9: present #{} backbuffer {bb} hwnd {hwnd:#x}", dev.frames);
    }
    let frames = dev.frames;
    let mut client = [0i32; 4];
    (d.calls().client_rect)(hwnd, client.as_mut_ptr());
    // (the rectangles count only windowed: Windows refuses them otherwise)
    let (src, dst) = if windowed { (rect_at(src), rect_at(dst)) } else { (None, None) };
    let rect = present_dest(client, dst);
    // (always, the 1st, 60th and 600th: where the frames go, in every run's log)
    if matches!(frames, 1 | 60 | 600) {
        let size = surface_gpu(d, bb).map(|g| (g.2, g.3));
        eprintln!("sake: d3d9 present {frames} of {size:?} source {src:?} to window {hwnd:#x} client {client:?} at {rect:?}");
    }
    if let Some(g) = surface_gpu(d, bb) {
        let (s6, s7) = present_src_words(src, g.2, g.3);
        gpu::record(|b| b.cmd(&[gpu::PRESENT, g.0, rect[0] as u32, rect[1] as u32, rect[2] as u32, rect[3] as u32, s6, s7, hwnd]));
    }
    let dev = d.dev.as_mut().unwrap();
    dev.batch_target = None;
    dev.last_vs = (Vec::new(), 0);
    dev.last_ps = (Vec::new(), 0);
    *after = After::Present;
    S_OK
}

// ---------------------------------------------------------------------------------------------
// draws

pub enum Src {
    /// DrawPrimitive: from this vertex of the streams
    Streams(u32),
    Indexed { base: i32, min: u32, num: u32, start: u32 },
    Up { data: u32, stride: u32 },
    UpIndexed { min: u32, num: u32, idx: u32, isize: u32, data: u32, stride: u32 },
}

fn vertex_count(prim: u32, n: u32) -> u32 {
    match prim {
        1 => n,
        2 => n * 2,
        3 => n + 1,
        4 => n * 3,
        5 | 6 => n + 2,
        _ => 0,
    }
}

/// WebGPU vertex format for a D3DDECLTYPE: (format code for the page, shader kind)
fn vertex_format(ty: u32) -> Option<(u32, u32)> {
    // page codes: index into d3d9.ts VERTEX_FORMATS
    Some(match ty {
        0 => (0, 0),  // float32
        1 => (1, 0),  // float32x2
        2 => (2, 0),  // float32x3
        3 => (3, 0),  // float32x4
        4 => (4, 3),  // D3DCOLOR: unorm8x4, swizzled
        5 => (5, 1),  // UBYTE4: uint8x4
        6 => (6, 2),  // SHORT2: sint16x2
        7 => (7, 2),  // SHORT4
        8 => (4, 0),  // UBYTE4N: unorm8x4
        9 => (8, 0),  // SHORT2N: snorm16x2
        10 => (9, 0), // SHORT4N
        11 => (10, 0),
        12 => (11, 0),
        15 => (12, 0), // FLOAT16_2
        16 => (13, 0), // FLOAT16_4
        _ => return None,
    })
}

fn type_size(ty: u32) -> u32 {
    match ty {
        0 => 4,
        1 => 8,
        2 => 12,
        3 => 16,
        4 | 5 | 6 | 8 | 9 | 11 | 13 | 14 | 15 => 4,
        7 | 10 | 12 | 16 => 8,
        _ => 4,
    }
}

const PRIM_TOPOLOGY: [u32; 7] = [3, 0, 1, 2, 3, 4, 3];

/// What the page may assume of a draw of one screen-aligned rectangle in pixels (XYZRHW, not
/// indexed, fixed function), for redrawing only what changed (render/d3d.rs, segments):
/// `[flags, sx, ox, sy, oy]`, flags 1: it covers its rectangle opaque (nothing it draws over
/// shows through: no blending, alpha test, depth, stencil or scissor, all of the color written,
/// no culling); 2: its pixels depend on nothing but stage 0's texture near texel
/// (sx * x + ox, sy * y + oy) for pixel (x, y) (the f32 bits of sx ox sy oy), and what else
/// the draw reads (its key). All zero: neither.
#[allow(clippy::too_many_arguments)]
unsafe fn rect_draw(d: &D3d, elems: &[Elem], streams: &[(u32, u32, u32)], first: u32, count: u32, prim: u32, ff: bool, samplers: &[u32], wmask: u32, ds: u32) -> [u32; 5] {
    let none = [0u32; 5];
    let Some(dev) = d.dev.as_ref() else { return none };
    let st = &dev.st;
    // (four corners as a strip or a fan)
    if !ff || count != 4 || !(prim == 5 || prim == 6) {
        return none;
    }
    let Some(pe) = elems.iter().find(|e| e.usage == shader::USAGE_POSITIONT && e.stream == 0) else { return none };
    let Some(&(_, ptr, stride)) = streams.first() else { return none };
    let vtx = |v: u32, off: u32, k: usize| *((ptr + (first + v) * stride + off) as *const f32).add(k);
    let pos: Vec<(f32, f32)> = (0..4).map(|v| (vtx(v, pe.offset, 0), vtx(v, pe.offset, 1))).collect();
    let (x0, x1) = pos.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (y0, y1) = pos.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    // (an axis-aligned rectangle: every corner once)
    let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
    if !(x1 > x0 && y1 > y0 && x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite()) || !corners.iter().all(|c| pos.iter().filter(|p| **p == *c).count() == 1) {
        return none;
    }
    let rs = &st.rs;
    let mut flags = 0;
    if rs[27] == 0 && rs[15] == 0 && ds == 0 && rs[52] == 0 && rs[174] == 0 && rs[22] == 1 && wmask == 0xf {
        flags |= 1;
    }
    let mut m = [0f32; 4];
    // stage 0 alone, a 2D texture of one level, its coordinates the vertices' texcoord 0 as they are
    let tex = st.textures[0];
    let tsize = match d.objs.get(&tex).map(|o| &o.data) {
        Some(Data::Texture(t)) if t.levels.len() == 1 && t.depth == 0 => Some((t.w as f32, t.h as f32)),
        _ => None,
    };
    let tc = elems.iter().find(|e| e.usage == shader::USAGE_TEXCOORD && e.index == 0 && e.stream == 0 && e.ty == 1);
    if let (Some((tw, th)), Some(tc), [0]) = (tsize, tc, samplers) {
        let one_stage = st.tss[1][1] == 1 && st.tss[0][11] & 0xffff == 0 && st.tss[0][24] == 0;
        let uv: Vec<(f32, f32)> = (0..4).map(|v| (vtx(v, tc.offset, 0) * tw, vtx(v, tc.offset, 1) * th)).collect();
        // texel = s * position + o, from two corners, true for all four; inside the texture
        // (no wrapping), at most 4 times larger or smaller
        let at = |c: (f32, f32)| uv[pos.iter().position(|p| *p == c).unwrap()];
        let (a, b) = (at((x0, y0)), at((x1, y1)));
        let (sx, sy) = ((b.0 - a.0) / (x1 - x0), (b.1 - a.1) / (y1 - y0));
        let (ox, oy) = (a.0 - sx * x0, a.1 - sy * y0);
        let fits = (0..4).all(|v| (sx * pos[v].0 + ox - uv[v].0).abs() < 0.01 && (sy * pos[v].1 + oy - uv[v].1).abs() < 0.01);
        let inside = uv.iter().all(|t| t.0 >= -0.01 && t.1 >= -0.01 && t.0 <= tw + 0.01 && t.1 <= th + 0.01);
        let scale = |s: f32| (0.25..=4.0).contains(&s.abs());
        if one_stage && fits && inside && scale(sx) && scale(sy) {
            flags |= 2;
            m = [sx, ox, sy, oy];
        }
    }
    if flags == 0 {
        return none;
    }
    [flags, f2u(m[0]), f2u(m[1]), f2u(m[2]), f2u(m[3])]
}

unsafe fn draw(d: &mut D3d, prim: u32, src: Src, prims: u32, _x: Option<()>) -> u32 {
    if prims == 0 || !(1..=6).contains(&prim) {
        return S_OK;
    }
    refresh_p8(d);
    let Some(dev) = d.dev.as_ref() else { return D3DERR_INVALIDCALL };
    let st = &dev.st;
    let rt = dev.rt[0];
    // (the depth buffer for the depth test or the stencil test)
    let ds = if st.rs[7] != 0 || st.rs[52] != 0 { dev.ds } else { 0 };
    let rts = dev.rt;
    // (the depth buffer's size, whether its tests are on or not: see depth_clip)
    let zsize = surface_gpu(d, dev.ds).map(|z| (z.2, z.3));
    let nverts = vertex_count(prim, prims);
    // where the vertices come from: (stream, data pointer of the first used vertex, stride, count)
    // (the vertices the indices name: Direct3D's hardware path reads BaseVertexIndex + index
    // whatever MinVertexIndex and NumVertices say, and games pass those wrong)
    let range = |iptr: u32, isize: u32| -> (u32, u32) {
        let (mut lo, mut hi) = (u32::MAX, 0);
        for k in 0..nverts {
            let v = if isize == 4 { *((iptr + 4 * k) as *const u32) } else { *((iptr + 2 * k) as *const u16) as u32 };
            lo = lo.min(v);
            hi = hi.max(v);
        }
        (lo.min(hi), hi.saturating_sub(lo.min(hi)) + 1)
    };
    let (first, count, index): (u32, u32, Option<(u32, u32, u32)>) = match &src {
        Src::Streams(start) => (*start, nverts, None),
        Src::Indexed { base, start, .. } => {
            let Some(Data::Buffer(ib)) = d.objs.get(&st.indices).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
            let isize = if ib.fmt == fmt::INDEX32 { 4 } else { 2 };
            let iptr = ib.mem + start * isize;
            if (start + nverts) * isize > ib.len {
                return D3DERR_INVALIDCALL;
            }
            let (lo, n) = range(iptr, isize);
            if *base + (lo as i32) < 0 {
                return D3DERR_INVALIDCALL;
            }
            ((*base + lo as i32) as u32, n, Some((iptr, isize, lo)))
        }
        Src::Up { .. } => (0, nverts, None),
        Src::UpIndexed { idx, isize, .. } => {
            let (lo, n) = range(*idx, *isize);
            (lo, n, Some((*idx, *isize, lo)))
        }
    };
    // geometry instancing (SetStreamSourceFreq): stream 0 INDEXEDDATA | instances, the streams
    // of INSTANCEDATA | divider step once every `divider` instances; indexed draws only
    let instances = if index.is_some() && st.freq[0] & 0x4000_0000 != 0 { (st.freq[0] & 0x3fff_ffff).max(1) } else { 1 };
    let freq = if index.is_some() && st.freq[0] & 0x4000_0000 != 0 { st.freq } else { [1; 16] };
    let divider = move |s: u32| -> Option<u32> { Some(freq[s as usize & 15]).filter(|f| f & 0x8000_0000 != 0).map(|f| (f & 0x3fff_ffff).max(1)) };
    // the targets
    let target = surface_gpu(d, rt);
    // (one of another size too: the page draws with one of the target's size, render/d3d.rs depth_for)
    let dsg = surface_gpu(d, ds);
    // render targets 1-3 (in a row from 1, of the size of 0)
    let more: Vec<(u32, (u32, u32, u32, u32, u32))> = match target {
        Some(target) => rts[1..].iter().map_while(|&r| surface_gpu(d, r).filter(|g| (g.2, g.3) == (target.2, target.3)).map(|g| (r, g))).collect(),
        None => Vec::new(),
    };
    let topo = PRIM_TOPOLOGY[prim as usize];
    // (WebGPU restarts an indexed strip at an index of all ones, Direct3D 9 never does: 16-bit
    // strips with a 0xffff go out as 32-bit indices)
    let widen = matches!(index, Some((iptr, 2, _)) if (topo == 2 || topo == 4) && (0..nverts).any(|k| *((iptr + 2 * k) as *const u16) == 0xffff));
    // What the draw prepares from the state (#118): the last draw's, when no call since may have
    // changed it (`stale`) and this draw is of the same kind
    let mut more_fmts = [u32::MAX; 3];
    for (m, (_, g)) in more_fmts.iter_mut().zip(&more) {
        *m = g.4;
    }
    let pkey = PrepKey {
        gen: dev.prep_gen,
        prim,
        kind: match src {
            Src::Streams(_) => 0,
            Src::Indexed { .. } => 1,
            Src::Up { .. } => 2,
            Src::UpIndexed { .. } => 3,
        },
        up_stride: match src {
            Src::Up { stride, .. } | Src::UpIndexed { stride, .. } => stride,
            _ => 0,
        },
        isize: index.map_or(0, |i| i.1),
        widen,
        target: target.map_or(u32::MAX, |t| t.4),
        ds: dsg.is_some(),
        more: more_fmts,
    };
    let trace = d.trace || no_prep_cache();
    let hit = d.dev.as_mut().unwrap().prep.take().filter(|p| p.key == pkey && !trace);
    let fresh = hit.is_none();
    // (IPPAI_PREP_CHECK: a draw that reuses also prepares from scratch, and says what differs)
    let check = !fresh && prep_check();
    let st = &d.dev.as_ref().unwrap().st;
    let layout = |d: &D3d, st: &State| -> Option<(Vec<Elem>, u32, Vec<u32>)> {
        // the vertex layout
        let (elems, fvf_stride) = if st.decl != 0 {
            match d.objs.get(&st.decl).map(|o| &o.data) {
                Some(Data::Decl(dc)) => (dc.elems.clone(), 0),
                _ => return None,
            }
        } else {
            fvf_elems(st.fvf)
        };
        let mut used: Vec<u32> = elems.iter().map(|e| e.stream).collect();
        used.sort();
        used.dedup();
        Some((elems, fvf_stride, used))
    };
    let mut prep = match hit {
        Some(p) => {
            if check {
                let now = layout(d, st);
                if now.as_ref() != Some(&(p.elems.clone(), p.fvf_stride, p.used.clone())) {
                    prep_differs("layout", &format!("{:?} kept, {now:?} now", (&p.elems, p.fvf_stride, &p.used)));
                }
            }
            p
        }
        None => {
            let Some((elems, fvf_stride, used)) = layout(d, st) else { return D3DERR_INVALIDCALL };
            Box::new(Prep { key: pkey, elems, fvf_stride, used, ..Prep::default() })
        }
    };
    let fvf_stride = prep.fvf_stride;
    let mut streams: Vec<(u32, u32, u32)> = Vec::with_capacity(prep.used.len()); // (stream, ptr, stride)
    for &s in &prep.used {
        let (ptr, stride) = match &src {
            Src::Up { data, stride } | Src::UpIndexed { data, stride, .. } => {
                if s != 0 {
                    return D3DERR_INVALIDCALL;
                }
                (*data, *stride)
            }
            _ => {
                let (vb, off, stride) = st.streams[s as usize];
                let Some(Data::Buffer(b)) = d.objs.get(&vb).map(|o| &o.data) else {
                    if d.trace {
                        eprintln!("d3d9: draw without stream {s}");
                    }
                    return D3DERR_INVALIDCALL;
                };
                let stride = if stride == 0 { fvf_stride } else { stride };
                let _ = b;
                (b.mem + off, stride)
            }
        };
        streams.push((s, ptr, stride));
    }
    let elems = &prep.elems;
    let used = &prep.used;
    if d.trace {
        let tex0 = st.textures[0];
        eprintln!("d3d9: draw prim {prim} x{prims} first {first} count {count} fvf {:#x} decl {} ps {} tex0 {tex0} blend {} {} {} tss0 {:?}", st.fvf, st.decl, st.ps, st.rs[27], st.rs[19], st.rs[20], &st.tss[0][1..7]);
        if st.fvf & 0xe == 2 {
            for k in [256u32, 2, 3] {
                eprintln!("d3d9:   transform {k} {:?}", st.transforms.get(&k));
            }
            eprintln!("d3d9:   viewport {:?}", st.viewport);
        }
        eprintln!("d3d9:   rt {rt} ds {ds} viewport {:?} cull {} z {} {}", st.viewport, st.rs[22], st.rs[7], st.rs[14]);
        if st.rs[137] != 0 && st.vs == 0 {
            let on: Vec<_> = st.lights.iter().filter(|l| l.1 .1).map(|(i, l)| (*i, ff::Light::from_bytes(&l.0))).collect();
            eprintln!("d3d9:   lit: ambient {:#x} material {:?} colorvertex {} sources {} {} {} {} lights {:?}", st.rs[139], st.material, st.rs[141], st.rs[145], st.rs[146], st.rs[147], st.rs[148], on);
        }
        if let Some((iptr, isize, _)) = index {
            let ix: Vec<u32> = (0..nverts.min(12)).map(|k| if isize == 4 { *((iptr + 4 * k) as *const u32) } else { *((iptr + 2 * k) as *const u16) as u32 }).collect();
            eprintln!("d3d9:   indices {ix:?}");
        }
        if st.vs != 0 {
            eprintln!("d3d9:   vs c0..13 {:?}", &st.vsf[..st.vsf.len().min(13)]);
        }
        if st.ps != 0 {
            eprintln!("d3d9:   ps c0..3 {:?}", &st.psf[..st.psf.len().min(3)]);
        }
        for &(s, ptr, stride) in streams.iter().skip(1) {
            let w: Vec<String> = (0..stride).map(|k| format!("{:08x}", *((ptr + 4 * k) as *const u32))).collect();
            eprintln!("d3d9:   stream {s} {ptr:#x} stride {stride}: {}", w.join(" "));
        }
        if let Some(&(_, ptr, stride)) = streams.first() {
            eprintln!("d3d9:   data {ptr:#x} stride {stride}");
            let max = std::env::var("IPPAI_D3D_TRACE_VERTS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
            for v in 0..count.min(max) {
                let p = ptr + (first + v) * stride;
                let w: Vec<String> = (0..stride / 4).map(|k| format!("{:08x}", *((p + 4 * k) as *const u32))).collect();
                eprintln!("d3d9:   v{v} {}", w.join(" "));
            }
        }
    }
    if fresh || check {
    let (vs, ps, pretransformed, samplers, vs_consts, ps_consts, pipe, blend, wmask) = {
    // the shaders (pretransformed vertices skip the vertex shader, as on Windows)
    let pretransformed = elems.iter().any(|e| e.usage == shader::USAGE_POSITIONT);
    let vs = if st.vs != 0 && !pretransformed {
        match d.objs.get(&st.vs).map(|o| &o.data) {
            Some(Data::Shader(s)) if s.parsed.is_some() => Some(st.vs),
            _ => return S_OK,
        }
    } else {
        None
    };
    let ps = if st.ps != 0 {
        match d.objs.get(&st.ps).map(|o| &o.data) {
            Some(Data::Shader(s)) if s.parsed.is_some() => Some(st.ps),
            _ => return S_OK,
        }
    } else {
        None
    };
    let Some(target) = target else { return S_OK };
    let ntargets = 1 + more.len() as u32;

    // vertex inputs: (stream slot index, elem) -> location
    let vs_shader = vs.and_then(|v| match d.objs.get(&v).map(|o| &o.data) {
        Some(Data::Shader(s)) => s.parsed.as_ref(),
        _ => None,
    });
    let mut attrs: Vec<(u32, u32, u32, u32)> = Vec::new(); // (buffer slot, page format, offset, location)
    let mut vs_inputs: Vec<(u32, u32, u32)> = Vec::new(); // (register, location, kind)
    let mut ff_inputs: Vec<(u32, u32, u32, u32)> = Vec::new();
    for e in elems.iter() {
        let Some((pf, kind)) = vertex_format(e.ty) else { continue };
        // (an FVF can declare more than the stride holds: QLIE draws TEX8 vertices with a
        // 32-byte stride; Direct3D reads past the vertex, WebGPU refuses the layout)
        let stride = streams.iter().find(|x| x.0 == e.stream).map_or(0, |x| x.2);
        if stride != 0 && e.offset + type_size(e.ty) > stride {
            continue;
        }
        let slot = used.iter().position(|&s| s == e.stream).unwrap() as u32;
        match vs_shader {
            Some(sh) => {
                // vs 1.x without dcl: register = usage order... (vs 1.1 has dcl too)
                if let Some(&(reg, _, _)) = sh.inputs.iter().find(|i| i.1 == e.usage && i.2 == e.index) {
                    if !vs_inputs.iter().any(|v| v.0 == reg) {
                        let loc = vs_inputs.len() as u32;
                        vs_inputs.push((reg, loc, kind));
                        attrs.push((slot, pf, e.offset, loc));
                    }
                }
            }
            None => {
                let loc = ff_inputs.len() as u32;
                if !ff_inputs.iter().any(|x| x.0 == e.usage && x.1 == e.index) {
                    ff_inputs.push((e.usage, e.index, loc, kind));
                    attrs.push((slot, pf, e.offset, loc));
                }
            }
        }
    }
    let rhw = vs.is_none() && elems.iter().any(|e| e.usage == shader::USAGE_POSITIONT);

    // WGSL
    let serial_of = |id: Option<u32>| match id.and_then(|v| d.objs.get(&v)).map(|o| &o.data) {
        Some(Data::Shader(s)) => s.serial,
        _ => 0,
    };
    let (vs_serial, ps_serial) = (serial_of(vs), serial_of(ps));
    let (vs_mod, vs_consts, writes) = match vs_shader {
        Some(sh) => {
            let key = ModKey::Vs(vs_serial, vs_inputs.clone());
            let m = match d.dev.as_ref().unwrap().modules.get(&key) {
                Some(&m) => Ok(m),
                None => {
                    let opts = shader::Options { link: 0, vs_inputs: vs_inputs.clone(), alpha_test: None, fog: false, addr: Vec::new(), targets: 1 };
                    let w = shader::Gen::new(sh).vertex(&opts);
                    Err((key, w))
                }
            };
            (m, sh.max_const, sh.writes)
        }
        None => {
            let mut tci = [(0u8, 0u8); 8];
            for (i, t) in tci.iter_mut().enumerate() {
                let ttff = st.tss[i][24] & 0xff;
                *t = ((st.tss[i][11] & 0xff) as u8, if ttff == 0 { 0 } else { ttff as u8 });
            }
            let light = (st.rs[137] != 0 && !rhw).then(|| {
                let has = |i: u32| ff_inputs.iter().any(|x| x.0 == shader::USAGE_COLOR && x.1 == i);
                ff::LightKey {
                    count: st.lights.values().filter(|l| l.1).count().min(ff::MAX_LIGHTS as usize) as u8,
                    specular: st.rs[29] != 0,
                    local_viewer: st.rs[142] != 0,
                    normalize: st.rs[143] != 0,
                    sources: ff::sources([st.rs[145], st.rs[146], st.rs[147], st.rs[148]], st.rs[141] != 0, [has(0), has(1)]),
                }
            });
            let key = ModKey::FfVs(ff::VsKey { inputs: ff_inputs.clone(), rhw, tci, flat: st.rs[9] == 1 && st.vs == 0 && st.ps == 0, light });
            let m = match d.dev.as_ref().unwrap().modules.get(&key) {
                Some(&m) => Ok(m),
                None => {
                    let ModKey::FfVs(k) = &key else { unreachable!() };
                    let w = ff::vertex(k);
                    Err((key, w))
                }
            };
            (m, ff::VS_CONSTS, 0xfffu32 & !0)
        }
    };
    let alpha_test = (st.rs[15] != 0 && st.rs[25] != 8).then_some(st.rs[25]);
    let ps_shader = ps.and_then(|p| match d.objs.get(&p).map(|o| &o.data) {
        Some(Data::Shader(s)) => s.parsed.as_ref(),
        _ => None,
    });
    // textures by sampler
    let mut samplers: Vec<u32> = Vec::new();
    // (samplers addressing as WebGPU's don't: the pixel shader does it)
    let addr_of = |s: u32| {
        let ss = &st.ss[s as usize];
        let flags = dxwgsl::addr::flags(ss[1], ss[2], ss[5] >= 2);
        (flags != 0).then(|| dxwgsl::addr::Addr::argb(s, flags, ss[4]))
    };
    let (ps_mod, ps_consts) = match ps_shader {
        Some(sh) => {
            // (cube and volume samplers too: their own address modes, no emulated ones)
            let mut flat = Vec::new();
            for &(r, ty) in &sh.samplers {
                samplers.push(r);
                if ty == 2 || ty == 0 {
                    flat.push(r);
                }
            }
            let link = if vs_shader.is_some() { writes } else { 0x3ff };
            let addr: Vec<_> = flat.iter().filter_map(|&s| addr_of(s)).collect();
            let key = ModKey::Ps(ps_serial, link, alpha_test, addr.clone(), ntargets);
            let m = match d.dev.as_ref().unwrap().modules.get(&key) {
                Some(&m) => Ok(m),
                None => {
                    let opts = shader::Options { link, vs_inputs: Vec::new(), alpha_test, fog: false, addr, targets: ntargets };
                    let w = shader::Gen::new(sh).pixel(&opts);
                    Err((key, w))
                }
            };
            (m, sh.max_const)
        }
        None => {
            let mut stages = Vec::new();
            let mut tex = [0u8; 8];
            for i in 0..8 {
                let s = &st.tss[i];
                if s[1] == 1 {
                    break;
                }
                let t = st.textures[i];
                let is2d = t != 0 && matches!(d.objs.get(&t).map(|o| o.iface), Some(IF_TEXTURE));
                tex[i] = if is2d { 2 } else { 0 };
                if is2d {
                    samplers.push(i as u32);
                }
                let projected = (st.tss[i][24] & 0x100 != 0) as u32;
                stages.push([s[1], s[2], s[3], s[26], s[4], s[5], s[6], s[27], s[28], projected]);
            }
            let addr = samplers.iter().filter_map(|&s| addr_of(s)).collect();
            let vs_writes = vs_shader.is_some().then_some(writes & 0x3ff);
            let key = ModKey::FfPs(ff::PsKey { stages, specular: st.rs[29] != 0, alpha_test, tex, flat: st.rs[9] == 1 && st.vs == 0 && st.ps == 0, addr, vs_writes });
            let m = match d.dev.as_ref().unwrap().modules.get(&key) {
                Some(&m) => Ok(m),
                None => {
                    let ModKey::FfPs(k) = &key else { unreachable!() };
                    let w = ff::pixel(k);
                    Err((key, w))
                }
            };
            (m, ff::PS_CONSTS)
        }
    };

    // pipeline
    let dev = d.dev.as_mut().unwrap();
    let mut module = |m: Result<u32, (ModKey, String)>| match m {
        Ok(m) => m,
        Err((key, w)) => {
            let m = shader_module(dev, w);
            dev.modules.insert(key, m);
            m
        }
    };
    let (vs_mod, ps_mod) = (module(vs_mod), module(ps_mod));
    let st = &dev.st;
    let (blend, wmask) = blend_state(st, target.4);
    // WebGPU decides facing in framebuffer coordinates (y down) as D3D does on screen, so with
    // frontFace "ccw" (1 = cull "front", 2 = cull "back"): D3DCULL_CW culls the clockwise
    // faces ("back"), D3DCULL_CCW the counterclockwise ones ("front")
    let cull = match st.rs[22] {
        2 => 2,
        3 => 1,
        _ => 0,
    };
    let indexed = index.is_some() || prim == 6;
    let isize = if widen { 4 } else { index.map_or(if prim == 6 { 2 } else { 0 }, |i| i.1) };
    let strip_fmt = if indexed && (topo == 2 || topo == 4) { isize } else { 0 };
    let mut key: Vec<u32> = vec![
        vs_mod,
        ps_mod,
        topo,
        strip_fmt,
        cull,
        fmt::page_format(target.4),
        dsg.map_or(0xff, |_| gpu::FMT_DEPTH24S8),
        // (ZWRITEENABLE writes only with ZENABLE on)
        (st.rs[7] != 0 && st.rs[14] != 0) as u32,
        if st.rs[7] != 0 { st.rs[23] } else { 8 },
    ];
    key.extend_from_slice(&blend);
    // (a target of fewer channels keeps the others as cleared: format::target_channels)
    key.push(wmask & fmt::target_channels(target.4).0);
    key.push(streams.len() as u32);
    for (i, &(sn, _, stride)) in streams.iter().enumerate() {
        let list: Vec<&(u32, u32, u32, u32)> = attrs.iter().filter(|a| a.0 == i as u32).collect();
        // (bit 31: a buffer of per-instance data)
        key.push(stride | if divider(sn).is_some() { 0x8000_0000 } else { 0 });
        key.push(list.len() as u32);
        for a in list {
            key.extend_from_slice(&[a.1, a.2, a.3]);
        }
    }
    // the stencil test (after the streams): enable, reference, read and write masks, then the
    // faces WebGPU calls front (counterclockwise on screen: D3D's CCW states with two-sided
    // stencil) and back (fail, depth fail, pass, function)
    if dsg.is_some() && st.rs[52] != 0 {
        let rs = &st.rs;
        let cw = [rs[53], rs[54], rs[55], rs[56]];
        let ccw = if rs[185] != 0 { [rs[186], rs[187], rs[188], rs[189]] } else { cw };
        key.extend_from_slice(&[1, rs[57] & 0xff, rs[58] & 0xff, rs[59] & 0xff]);
        key.extend_from_slice(&ccw);
        key.extend_from_slice(&cw);
    } else {
        key.push(0);
    }
    // the other render targets: (page format, COLORWRITEENABLE1-3)
    key.push(more.len() as u32);
    // (only shader model 2+ pixel shaders write them: see dxwgsl::d3d9::Options::targets)
    let writes_more = ps_shader.is_some_and(|s| s.major >= 2);
    for (i, (_, g)) in more.iter().enumerate() {
        key.extend_from_slice(&[fmt::page_format(g.4), if writes_more { st.rs[190 + i] & fmt::target_channels(g.4).0 } else { 0 }]);
    }
    let pipe = match dev.pipelines.get(&key) {
        Some(&p) => {
            crate::render::gpu::pipeline_hit();
            p
        }
        None => {
            let id = gpu_id();
            gpu::record(|b| {
                b.cmd(&[gpu::CREATE_PIPELINE, id, key.len() as u32]);
                b.cmd(&key);
            });
            dev.pipelines.insert(key, id);
            id
        }
    };
    (vs, ps, pretransformed, samplers, vs_consts, ps_consts, pipe, blend, wmask)
    };
    let now = (vs, ps, pretransformed, samplers, vs_consts, ps_consts, pipe, blend, wmask);
    if check {
        let kept = (prep.vs, prep.ps, prep.pretransformed, prep.samplers.clone(), prep.vs_consts, prep.ps_consts, prep.pipe, prep.blend, prep.wmask);
        if kept != now {
            prep_differs("preparation", &format!("{kept:?} kept, {now:?} now"));
        }
    } else {
        (prep.vs, prep.ps, prep.pretransformed, prep.samplers, prep.vs_consts, prep.ps_consts, prep.pipe, prep.blend, prep.wmask) = now;
    }
    }
    let Some(target) = target else { return S_OK };
    let (vs, ps, pretransformed, vs_consts, ps_consts, pipe, blend, wmask) = (prep.vs, prep.ps, prep.pretransformed, prep.vs_consts, prep.ps_consts, prep.pipe, prep.blend, prep.wmask);
    let elems = &prep.elems;
    let samplers = &prep.samplers;
    let dev = d.dev.as_mut().unwrap();
    let st = &dev.st;

    // uniforms: the last draw's, when no call since may have changed them (`stale`) and the
    // preparation is the same (all but the alpha bits of the textures bound: each draw's own)
    let vp = st.viewport;
    let ukey = (pkey, dev.uni_gen);
    let mut uni = dev.uni.take().unwrap_or_default();
    let kept = (uni.key == Some(ukey) && check).then(|| (uni.vsu.clone(), uni.psu.clone(), uni.hash));
    if uni.key != Some(ukey) || trace || kept.is_some() {
    let (vpw, vph) = (vp[2].max(1) as f32, vp[3].max(1) as f32);
    let vsu = &mut uni.vsu;
    let psu = &mut uni.psu;
    vsu.clear();
    psu.clear();
    vsu.resize(shader::uniform_size(vs_consts) as usize, 0);
    psu.resize(shader::uniform_size(ps_consts) as usize, 0);
    let put = |buf: &mut Vec<u8>, at: usize, v: u32| buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(vsu, 0, f2u(1.0 / vpw));
    put(vsu, 4, f2u(1.0 / vph));
    put(vsu, 16, st.vsb);
    put(psu, 16, st.psb);
    let ints = |buf: &mut Vec<u8>, regs: &[[i32; 4]; 16]| {
        for (i, r) in regs.iter().enumerate() {
            for (j, v) in r.iter().enumerate() {
                buf[64 + i * 16 + j * 4..][..4].copy_from_slice(&v.to_le_bytes());
            }
        }
    };
    ints(vsu, &st.vsi);
    ints(psu, &st.psi);
    let consts = |buf: &mut Vec<u8>, regs: &[[f32; 4]], n: u32| {
        for i in 0..(n as usize).min(regs.len()) {
            for j in 0..4 {
                buf[320 + i * 16 + j * 4..][..4].copy_from_slice(&regs[i][j].to_le_bytes());
            }
        }
    };
    if vs.is_some() {
        consts(vsu, &st.vsf, vs_consts);
    } else {
        put(vsu, 32, f2u(vp[0] as f32));
        put(vsu, 36, f2u(vp[1] as f32));
        put(vsu, 40, f2u(2.0 / vpw));
        put(vsu, 44, f2u(2.0 / vph));
        let get = |s: u32| st.transforms.get(&s).copied().unwrap_or(IDENT);
        let wvp = mul(&mul(&get(256), &get(2)), &get(3));
        let mut regs = [[0f32; 4]; ff::VS_CONSTS as usize];
        // WGSL: mat4x4f(c0..c3) * p with D3D's rows as the columns = transpose(M) * p = p * M
        for r in 0..4 {
            regs[r] = [wvp[4 * r], wvp[4 * r + 1], wvp[4 * r + 2], wvp[4 * r + 3]];
        }
        for t in 0..2 {
            let m = get(16 + t as u32);
            for r in 0..4 {
                regs[4 + t * 4 + r] = [m[4 * r], m[4 * r + 1], m[4 * r + 2], m[4 * r + 3]];
            }
        }
        if st.rs[137] != 0 {
            // (enabled lights by index, as they take the light registers in that order)
            let mut on: Vec<_> = st.lights.iter().filter(|l| l.1 .1).map(|(&i, l)| (i, ff::Light::from_bytes(&l.0))).collect();
            on.sort_by_key(|l| l.0);
            let lights: Vec<_> = on.into_iter().map(|l| l.1).collect();
            ff::lighting_regs(&mut regs, &st.material, st.rs[139], &get(256), &get(2), &lights);
        }
        consts(vsu, &regs, ff::VS_CONSTS);
    }
    put(psu, 12, f2u((st.rs[24] & 0xff) as f32 / 255.0));
    // (a render target of fewer bits than the page's: the steps it keeps, dxwgsl's steps())
    // (only overwrites: Windows rounds after genuine blending; the page's result otherwise
    // stays a step finer. ONE/ZERO ADD is an overwrite even with blending enabled.)
    let steps = draw_steps(st.rs[27] != 0, blend, target.4);
    for (i, m) in steps.into_iter().enumerate() {
        put(psu, 48 + 4 * i, m);
    }
    if ps.is_some() {
        consts(psu, &st.psf, ps_consts);
    } else {
        let c = |v: u32| [((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0, (v >> 24) as f32 / 255.0];
        let mut regs = [[0f32; 4]; ff::PS_CONSTS as usize];
        regs[0] = c(st.rs[60]);
        for i in 0..8 {
            regs[1 + i] = c(st.tss[i][32]);
        }
        consts(psu, &regs, ff::PS_CONSTS);
    }
    uni.hash = {
        let mut h = DrawHash::default();
        h.add(&uni.vsu);
        h.add(&uni.psu);
        std::hash::Hasher::finish(&h.0)
    };
    uni.key = Some(ukey);
    }
    if let Some((v, p, h)) = kept {
        // (the alpha bits are each draw's: not compared)
        let (mut p, mut q) = (p, uni.psu.clone());
        p[8..12].fill(0);
        q[8..12].fill(0);
        if v != uni.vsu || p != q || h != uni.hash {
            prep_differs("uniforms", &format!("vertex block same {}, pixel block same {}", v == uni.vsu, p == q));
        }
    }
    // alpha-less textures and alpha ref
    let mut alpha_bits = 0u32;
    let mut texs: Vec<(u32, u32, u32)> = Vec::new(); // (binding slot, page texture, sampler key)
    for &s in samplers.iter() {
        let t = st.textures.get(s as usize).copied().unwrap_or(0);
        let (gpu_tex, f, levels) = match d.objs.get(&t).map(|o| &o.data) {
            Some(Data::Texture(tx)) => (tx.gpu, tx.fmt, tx.level_count()),
            _ => (0, 0, 1),
        };
        if fmt::no_alpha(f) {
            alpha_bits |= 1 << s;
        }
        // (bits 16-31: red is the luminance, a render target's: format::target_channels)
        if fmt::target_channels(f).1 && s < 16 {
            alpha_bits |= 1 << (16 + s);
        }
        let ss = &st.ss[s as usize];
        let mip = if levels > 1 { ss[7] } else { 0 };
        let am = dxwgsl::addr::sampler_mode;
        let skey = (am(ss[1]) & 7) | (am(ss[2]) & 7) << 3 | (ss[5] & 3) << 6 | (ss[6] & 3) << 8 | (mip & 3) << 10 | (am(ss[3]) & 7) << 12;
        texs.push((s, gpu_tex, skey));
    }
    // (bits 8..12 of the pixel block)
    uni.psu[8..12].copy_from_slice(&alpha_bits.to_le_bytes());
    let scissor = if st.rs[174] != 0 {
        st.scissor
    } else {
        [0, 0, target.2 as i32, target.3 as i32]
    };
    let scissor = depth_clip(scissor, zsize);
    let blend_factor = st.rs[193];

    // What the draw reads from guest memory (its vertices, indices, constants), hashed, and
    // where it draws when its vertices are in pixels already (XYZRHW): the page redraws only
    // the parts of a target that changed since the last frame (render/d3d.rs, segments).
    let mut h = DrawHash::default();
    // (#119: the streams and indices read from the page copies of their buffers, by buffer
    // object and the bytes' generation; `page_buffer` makes them)
    let from_buffer = matches!(src, Src::Streams(_) | Src::Indexed { .. });
    let stream_res: Vec<Option<(u32, u32)>> = streams
        .iter()
        .map(|&(sn, _, stride)| {
            let (vb, off, _) = st.streams[sn as usize];
            let Some(Data::Buffer(b)) = d.objs.get(&vb).map(|o| &o.data) else { return None };
            let at = off as u64 + first as u64 * stride as u64;
            let fits = at % 4 == 0 && stride % 4 == 0 && at + count as u64 * stride as u64 <= b.len as u64;
            (from_buffer && divider(sn).is_none() && fits && resident(b)).then_some((vb, at as u32))
        })
        .collect();
    let index_res: Option<(u32, u32)> = match (&src, index) {
        (Src::Indexed { start, .. }, Some((_, isz, _))) if prim != 6 && !widen => {
            let Some(Data::Buffer(b)) = d.objs.get(&st.indices).map(|o| &o.data) else { return D3DERR_INVALIDCALL };
            (resident(b) && (start * isz) % 4 == 0).then_some((st.indices, start * isz))
        }
        _ => None,
    };
    // (the page copies, made or brought up to date before the draw)
    let stream_bufs: Vec<Option<(u32, u32, u64)>> = stream_res.iter().map(|r| r.map(|(vb, at)| {
        let (g, cg) = page_buffer(&mut d.objs, vb);
        (g, at, cg)
    })).collect();
    let index_buf = index_res.map(|(ib, at)| {
        let (g, cg) = page_buffer(&mut d.objs, ib);
        (g, at, cg)
    });
    for (k, &(sn, ptr, stride)) in streams.iter().enumerate() {
        if let Some((g, at, cg)) = stream_bufs[k] {
            for w in [g as u64, cg, at as u64, (count * stride) as u64] {
                h.add(&w.to_le_bytes());
            }
            RESIDENT_READ.fetch_add((count * stride) as u64, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        match divider(sn) {
            Some(dv) => h.add(std::slice::from_raw_parts(ptr as *const u8, (instances.div_ceil(dv) * stride) as usize)),
            None => h.add(std::slice::from_raw_parts((ptr + first * stride) as *const u8, (count * stride) as usize)),
        }
    }
    h.add(&instances.to_le_bytes());
    match (index, index_buf) {
        (Some((_, isz, _)), Some(_)) => {
            let (g, at, cg) = index_buf.unwrap();
            for w in [g as u64, cg, at as u64, (nverts * isz) as u64] {
                h.add(&w.to_le_bytes());
            }
        }
        (Some((iptr, isz, _)), None) => h.add(std::slice::from_raw_parts(iptr as *const u8, (nverts * isz) as usize)),
        _ => {}
    }
    h.add(&uni.hash.to_le_bytes());
    h.add(&alpha_bits.to_le_bytes());
    let mut bbox = [u32::MAX; 4];
    if let (true, Some(e)) = (pretransformed, elems.iter().find(|e| e.usage == shader::USAGE_POSITIONT && e.stream == 0)) {
        if let Some(&(_, ptr, stride)) = streams.first() {
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for v in 0..count {
                let p = (ptr + (first + v) * stride + e.offset) as *const f32;
                let (x, y) = (*p, *p.add(1));
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            if x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite() && x1 >= x0 && y1 >= y0 {
                // (a pixel of margin: sampling and the half-pixel offset)
                let c = |v: f32| (v.max(0.0).min(65535.0)) as u32;
                bbox = [c(x0.floor() - 1.0), c(y0.floor() - 1.0), c(x1.ceil() + 1.0), c(y1.ceil() + 1.0)];
            }
        }
    }
    let hash = h.finish();
    let ext = rect_draw(d, &elems, &streams, first, count, prim, index.is_none() && vs.is_none() && ps.is_none(), &samplers, wmask, ds);

    let more_rts: Vec<u32> = more.iter().map(|m| m.0).collect();
    ensure_targets(d, rt, &more_rts, ds);
    let dev = d.dev.as_mut().unwrap();
    // (uniforms of the last draw can be reused only from the same batch's data: a Clear or a
    // target change since a flush does not write them again)
    let same_batch = dev.uniform_gen == gpu::batch_gen();
    // data: vertices, indices, uniforms
    let ((cmd, ok), gen) = gpu::record_gen(|b| {
        // (buffer, offset) a stream: the data block (DATA) or a page copy
        let mut offs = Vec::new();
        for (k, &(sn, ptr, stride)) in streams.iter().enumerate() {
            if let Some((g, at, _)) = stream_bufs[k] {
                offs.push(g);
                offs.push(at);
                continue;
            }
            offs.push(DATA);
            match divider(sn) {
                // (instance i reads element i / divider: WebGPU steps once per instance, so
                // each element goes `divider` times)
                Some(dv) => {
                    let mut v = Vec::with_capacity((instances * stride) as usize);
                    for i in 0..instances {
                        v.extend_from_slice(std::slice::from_raw_parts((ptr + i / dv * stride) as *const u8, stride as usize));
                    }
                    offs.push(b.data(&v, 4));
                }
                None => {
                    let start = ptr + first * stride;
                    let len = (count * stride) as usize;
                    offs.push(b.data(std::slice::from_raw_parts(start as *const u8, len), 4));
                }
            }
        }
        let (ifmt, ioff, icount, base) = match (&index, prim) {
            (Some((iptr, isz, min)), 6) => {
                // fan, indexed
                let mut ix: Vec<u32> = Vec::new();
                let get = |i: u32| -> u32 {
                    if *isz == 4 { *((iptr + i * 4) as *const u32) } else { *((iptr + i * 2) as *const u16) as u32 }
                };
                for t in 0..prims {
                    ix.extend_from_slice(&[get(0), get(t + 1), get(t + 2)]);
                }
                let bytes: Vec<u8> = ix.iter().flat_map(|v| v.to_le_bytes()).collect();
                (4, b.data(&bytes, 4), ix.len() as u32, -(*min as i32))
            }
            (Some((iptr, _, min)), _) if widen => {
                let bytes: Vec<u8> = (0..nverts).flat_map(|k| (*((iptr + 2 * k) as *const u16) as u32).to_le_bytes()).collect();
                (4, b.data(&bytes, 4), nverts, -(*min as i32))
            }
            (Some((_, isz, min)), _) if index_buf.is_some() => {
                // (from the index buffer's page copy: the last pair, its buffer marked)
                let (g, at, _) = index_buf.unwrap();
                offs.push(g | INDEX_PAIR);
                offs.push(at);
                (*isz, 0, nverts, -(*min as i32))
            }
            (Some((iptr, isz, min)), _) => {
                let len = (nverts * isz) as usize;
                (*isz, b.data(std::slice::from_raw_parts(*iptr as *const u8, len), 4), nverts, -(*min as i32))
            }
            (None, 6) => {
                let mut ix: Vec<u16> = Vec::new();
                for t in 0..prims {
                    ix.extend_from_slice(&[0, (t + 1) as u16, (t + 2) as u16]);
                }
                let bytes: Vec<u8> = ix.iter().flat_map(|v| v.to_le_bytes()).collect();
                (2, b.data(&bytes, 4), ix.len() as u32, 0)
            }
            (None, _) => (0, 0, count, 0),
        };
        // uniforms (the same block as the last draw's when nothing changed)
        let (vsu, psu) = (&uni.vsu, &uni.psu);
        let vso = if dev.last_vs.0 == *vsu && same_batch && !b.data.is_empty() { dev.last_vs.1 } else { b.data(vsu, 256) };
        let pso = if dev.last_ps.0 == *psu && same_batch && !b.data.is_empty() { dev.last_ps.1 } else { b.data(psu, 256) };
        let mut c = vec![
            gpu::DRAW,
            pipe,
            f2u(vp[0] as f32),
            f2u(vp[1] as f32),
            f2u(vp[2] as f32),
            f2u(vp[3] as f32),
            vp[4],
            vp[5],
            scissor[0].max(0) as u32,
            scissor[1].max(0) as u32,
            (scissor[2] - scissor[0]).max(0) as u32,
            (scissor[3] - scissor[1]).max(0) as u32,
            vso,
            vsu.len() as u32,
            pso,
            psu.len() as u32,
            blend_factor,
            texs.len() as u32,
        ];
        for t in &texs {
            c.extend_from_slice(&[t.0, t.1, t.2]);
        }
        c.push(offs.len() as u32);
        c.extend_from_slice(&offs);
        // (the index format, instances above it)
        c.extend_from_slice(&[ifmt | if instances > 1 { instances << 8 } else { 0 }, ioff, icount, base as u32]);
        c.push(hash);
        c.extend_from_slice(&bbox);
        c.extend_from_slice(&ext);
        b.cmd(&c);
        ((vso, pso), true)
    });
    let _ = ok;
    for (last, now, off) in [(&mut dev.last_vs, &uni.vsu, cmd.0), (&mut dev.last_ps, &uni.psu, cmd.1)] {
        if last.0 != *now {
            last.0.clear();
            last.0.extend_from_slice(now);
        }
        last.1 = off;
    }
    dev.uni = Some(uni);
    dev.prep = Some(prep);
    dev.uniform_gen = gen;
    // mark the targets rendered
    mark_rendered(d, rt);
    for r in more_rts {
        mark_rendered(d, r);
    }
    // (big batches go out now: the page writes the data block in one piece)
    if gpu::record(|b| b.data.len()) > 32 << 20 {
        gpu::flush();
        if let Some(dev) = d.dev.as_mut() {
            dev.batch_target = None;
            dev.last_vs = (Vec::new(), 0);
            dev.last_ps = (Vec::new(), 0);
        }
    }
    S_OK
}

/// What a draw is of, besides the device's state: a draw of the same key with the same
/// `prep_gen` prepares the same (#118).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct PrepKey {
    gen: u64,
    prim: u32,
    /// 0 DrawPrimitive, 1 DrawIndexedPrimitive, 2 DrawPrimitiveUP, 3 DrawIndexedPrimitiveUP
    kind: u8,
    /// (UP: the stride is the call's)
    up_stride: u32,
    /// the index size (0: none)
    isize: u32,
    /// a 16-bit strip with a 0xffff, drawn with 32-bit indices
    widen: bool,
    /// the formats of the render targets (u32::MAX: none) and whether there is a depth buffer
    target: u32,
    ds: bool,
    more: [u32; 3],
}

/// A draw's preparation from the device's state: its vertex layout and streams, shaders,
/// samplers, pipeline.
#[derive(Default)]
struct Prep {
    key: PrepKey,
    elems: Vec<Elem>,
    fvf_stride: u32,
    used: Vec<u32>,
    vs: Option<u32>,
    ps: Option<u32>,
    pretransformed: bool,
    samplers: Vec<u32>,
    vs_consts: u32,
    ps_consts: u32,
    pipe: u32,
    blend: [u32; 7],
    wmask: u32,
}

/// A draw's uniform blocks (the pixel block's alpha bits written by each draw) and their hash.
#[derive(Default)]
struct Uni {
    key: Option<(PrepKey, u64)>,
    vsu: Vec<u8>,
    psu: Vec<u8>,
    hash: u64,
}

/// IPPAI_NOPREP=1: every draw prepares from scratch (to compare with); IPPAI_PREP_CHECK=1: a
/// draw that reuses the last preparation prepares from scratch too and says what differs (each
/// a bug of `stale`). Tests set them with `set_prep_mode`.
static PREP_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0xff);
/// the differences IPPAI_PREP_CHECK found
pub static PREP_DIFFS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn prep_mode() -> u8 {
    use std::sync::atomic::Ordering::Relaxed;
    let m = PREP_MODE.load(Relaxed);
    if m != 0xff {
        return m;
    }
    let on = |k: &str| std::env::var_os(k).is_some_and(|v| v != "0");
    let m = on("IPPAI_NOPREP") as u8 | (on("IPPAI_PREP_CHECK") as u8) << 1;
    PREP_MODE.store(m, Relaxed);
    m
}

/// 1: no reuse, 2: checked reuse, 0: reuse
#[cfg(test)]
pub(crate) fn set_prep_mode(m: u8) {
    PREP_MODE.store(m, std::sync::atomic::Ordering::Relaxed);
}

fn no_prep_cache() -> bool {
    prep_mode() & 1 != 0
}

fn prep_check() -> bool {
    prep_mode() & 2 != 0
}

fn prep_differs(what: &str, how: &str) {
    let n = PREP_DIFFS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n < 20 {
        eprintln!("d3d9: prep check: the {what} kept differs from the state's: {how}");
    }
}

/// Internal draw-change fingerprint. Hash machine words, not one i64 multiply per
/// byte. Lengths delimit streams/blocks so padding short tails cannot alias them.
/// This is a cache hint, not a persisted content ID or an authentication digest.
#[derive(Default)]
struct DrawHash(super::Fast);

impl DrawHash {
    #[inline]
    fn add(&mut self, bytes: &[u8]) {
        use std::hash::Hasher;
        self.0.write_usize(bytes.len());
        self.0.write(bytes);
    }

    fn finish(&self) -> u32 {
        use std::hash::Hasher;
        let h = self.0.finish();
        h as u32 ^ (h >> 32) as u32
    }
}

fn shader_module(dev: &mut Device, wgsl: String) -> u32 {
    if let Some(&id) = dev.shaders.get(&wgsl) {
        return id;
    }
    let id = gpu_id();
    gpu::record(|b| {
        let off = b.blob(wgsl.as_bytes());
        b.cmd(&[gpu::CREATE_SHADER, id, off, wgsl.len() as u32]);
    });
    if crate::env_on!("IPPAI_D3D_WGSL") {
        eprintln!("d3d9: shader {id}:\n{wgsl}");
    }
    dev.shaders.insert(wgsl, id);
    id
}

/// Quantize before hardware blending only when it is an exact overwrite.
#[inline]
fn draw_steps(enabled: bool, blend: [u32; 7], target: u32) -> [u32; 4] {
    // The usual 32-bit targets already have the page's precision. Keep their hot
    // path independent of blend factors, including X targets' synthetic blending.
    if matches!(target, fmt::A8R8G8B8 | fmt::X8R8G8B8) {
        return [0; 4];
    }
    // ONE/ZERO ADD overwrites just like disabled blending. X targets additionally
    // write zero to their unused alpha bits, without reading source/destination alpha.
    let overwrite = !enabled || blend == [1, 2, 1, 1, 2, 1, 1]
        || blend == [1, 2, 1, 1, 1, 1, 1] && fmt::target_x(target);
    if overwrite { fmt::target_steps(target) } else { [0; 4] }
}

/// [enable, src, dst, op, src alpha, dst alpha, op alpha] and the write mask, drawing to a
/// render target of format `target`
fn blend_state(st: &State, target: u32) -> ([u32; 7], u32) {
    let rs = &st.rs;
    let fix = |s: u32, d: u32| match s {
        12 => (5, 6), // BOTHSRCALPHA
        13 => (6, 5), // BOTHINVSRCALPHA
        _ => (s, d),
    };
    let wmask = rs[168] & 0xf;
    let x = fmt::target_x(target);
    if rs[27] == 0 {
        // (X bits: a draw leaves them 0 (blending ONE, ZERO is the draw as it is))
        return (if x { [1, 2, 1, 1, 1, 1, 1] } else { [0, 2, 1, 1, 2, 1, 1] }, wmask);
    }
    let (s, d) = fix(rs[19], rs[20]);
    let (sa, da, opa) = if rs[206] != 0 {
        let (sa, da) = fix(rs[207], rs[208]);
        (sa, da, rs[209])
    } else {
        (s, d, rs[171])
    };
    if x {
        // (a target without alpha has alpha 1 as blending reads it: DESTALPHA is ONE,
        // INVDESTALPHA ZERO, SRCALPHASAT min(As, 1 - 1) ZERO; and its X bits end up 0)
        let one = |f: u32| match f {
            7 => 2,
            8 | 11 => 1,
            f => f,
        };
        return ([1, one(s), one(d), rs[171], 1, 1, 1], wmask);
    }
    ([1, s, d, rs[171], sa, da, opa], wmask)
}

#[cfg(test)]
mod tests {
    use super::{blend_state, default_state, draw_steps, fmt, DrawHash};
    use proptest::prelude::*;

    const RT: [u32; 4] = [fmt::X8R8G8B8, fmt::X1R5G5B5, fmt::X4R4G4B4, fmt::R5G6B5];

    #[test]
    fn draw_hash_detects_mutations_and_block_boundaries() {
        let hash = |parts: &[&[u8]]| {
            let mut h = DrawHash::default();
            for p in parts {
                h.add(p);
            }
            h.finish()
        };
        // Includes all alignments/tails, uniform-sized blocks and every changed bit.
        for len in [0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 80, 320, 528, 4416] {
            let mut b: Vec<u8> = (0..len).map(|i| (i * 37) as u8).collect();
            let base = hash(&[&b]);
            for i in 0..len {
                for bit in 0..8 {
                    b[i] ^= 1 << bit;
                    assert_ne!(base, hash(&[&b]), "byte {i}, bit {bit}, length {len}");
                    b[i] ^= 1 << bit;
                }
            }
        }
        assert_ne!(hash(&[&[1]]), hash(&[&[1, 0]]));
        assert_ne!(hash(&[&[1, 2], &[3]]), hash(&[&[1], &[2, 3]]));
        assert_ne!(hash(&[&[0; 8], &[0; 16]]), hash(&[&[0; 16], &[0; 8]]));
    }

    #[test]
    fn identity_blending_keeps_overwrite_quantization() {
        for f in [fmt::A8R8G8B8, fmt::X8R8G8B8] {
            for enabled in [false, true] {
                assert_eq!(draw_steps(enabled, [1, 5, 6, 1, 5, 6, 1], f), [0; 4]);
            }
        }
        for f in [fmt::R5G6B5, fmt::X1R5G5B5, fmt::A1R5G5B5, fmt::A4R4G4B4,
                  fmt::A2R10G10B10, fmt::A2B10G10R10, fmt::V8U8, fmt::Q8W8V8U8] {
            let mut st = default_state();
            let disabled = draw_steps(st.rs[27] != 0, blend_state(&st, f).0, f);
            assert_ne!(disabled, [0; 4]);
            st.rs[27] = 1;
            st.rs[19] = 2; // ONE
            st.rs[20] = 1; // ZERO
            st.rs[171] = 1; // ADD
            assert_eq!(draw_steps(st.rs[27] != 0, blend_state(&st, f).0, f), disabled);
            st.rs[19] = 5; // SRCALPHA: rounding before blending would change its result
            st.rs[20] = 6;
            assert_eq!(draw_steps(st.rs[27] != 0, blend_state(&st, f).0, f), [0; 4]);
        }
        // Separate alpha blending can consume the unrounded source alpha even when
        // color is ONE/ZERO; do not classify that as a complete overwrite.
        let mut st = default_state();
        st.rs[27] = 1;
        st.rs[19] = 2;
        st.rs[20] = 1;
        st.rs[206] = 1;
        st.rs[207] = 5;
        st.rs[208] = 6;
        assert_eq!(draw_steps(true, blend_state(&st, fmt::A4R4G4B4).0, fmt::A4R4G4B4), [0; 4]);
    }

    #[test]
    fn x_targets_blend_with_alpha_one_and_write_x_zero() {
        // (winref d3d9rtfmt: X8R8G8B8's opaque draw 0080ff00, SRCALPHA / INVSRCALPHA 90b02000)
        let mut st = default_state();
        for f in RT {
            assert_eq!(blend_state(&st, f).0, [1, 2, 1, 1, 1, 1, 1], "{f}");
        }
        assert_eq!(blend_state(&st, fmt::A8R8G8B8).0[0], 0);
        st.rs[27] = 1;
        st.rs[19] = 7; // DESTALPHA
        st.rs[20] = 8; // INVDESTALPHA
        assert_eq!(blend_state(&st, fmt::X8R8G8B8).0, [1, 2, 1, 1, 1, 1, 1]);
        assert_eq!(blend_state(&st, fmt::A8R8G8B8).0, [1, 7, 8, 1, 7, 8, 1]);
        st.rs[19] = 11; // SRCALPHASAT: min(As, 1 - 1)
        st.rs[20] = 6;
        assert_eq!(blend_state(&st, fmt::R5G6B5).0, [1, 1, 6, 1, 1, 1, 1]);
        // (BOTHINVSRCALPHA as it is)
        st.rs[19] = 13;
        assert_eq!(blend_state(&st, fmt::X1R5G5B5).0, [1, 6, 5, 1, 1, 1, 1]);
    }

    proptest! {
        /// Any blend state: an X target's color never reads the page's alpha and its alpha
        /// comes out 0; other targets get the state as it is; the write mask is COLORWRITEENABLE's.
        #[test]
        fn x_targets_never_read_destination_alpha(src in 1u32..16, dst in 1u32..16, op in 1u32..6, sep in 0u32..2,
                                                  sa in 1u32..16, da in 1u32..16, opa in 1u32..6, en in 0u32..2, mask in 0u32..16) {
            let mut st = default_state();
            for (i, v) in [(27, en), (19, src), (20, dst), (171, op), (206, sep), (207, sa), (208, da), (209, opa), (168, mask)] {
                st.rs[i] = v;
            }
            for f in RT {
                let (b, m) = blend_state(&st, f);
                prop_assert_eq!(m, mask);
                prop_assert_eq!(b[0], 1);
                prop_assert!(![7, 8, 11].contains(&b[1]) && ![7, 8, 11].contains(&b[2]));
                prop_assert_eq!(&b[4..], &[1, 1, 1][..]);
                if en == 0 {
                    prop_assert_eq!(&b[1..4], &[2, 1, 1][..]);
                } else {
                    prop_assert_eq!(b[3], op);
                }
            }
            let (b, m) = blend_state(&st, fmt::A8R8G8B8);
            prop_assert_eq!(m, mask);
            if en != 0 && src < 12 && (sep == 0 || sa < 12) {
                prop_assert_eq!(b, [1, src, dst, op, if sep != 0 { sa } else { src }, if sep != 0 { da } else { dst }, if sep != 0 { opa } else { op }]);
            }
        }
    }

    #[test]
    fn changed_box_finds_the_written_corner() {
        // 64x8 pixels of 4 bytes, pitch 300 bytes; the frame writes x 3..41, y 2..5
        let (w, h, pitch) = (64usize, 8usize, 300usize);
        let mut mem = vec![0u8; pitch * h];
        let mut shadow = vec![0u8; w * 4 * h];
        assert_eq!(super::changed_box(&mem, pitch, w * 4, h, 4, &mut shadow), None);
        for y in 2..5 {
            for x in 3..41 {
                mem[y * pitch + x * 4 + 1] = 7;
            }
        }
        assert_eq!(super::changed_box(&mem, pitch, w * 4, h, 4, &mut shadow), Some([3, 2, 41, 5]));
        // the shadow took the new rows: nothing changed now
        assert_eq!(super::changed_box(&mem, pitch, w * 4, h, 4, &mut shadow), None);
        mem[7 * pitch + 63 * 4 + 3] = 1;
        assert_eq!(super::changed_box(&mem, pitch, w * 4, h, 4, &mut shadow), Some([63, 7, 64, 8]));
    }
}
