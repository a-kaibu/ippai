//! Direct3D 11 and DXGI for the guest programs, drawn with the page's WebGPU.
//!
//! Same scheme as `d3d9`: d3d11.dll (dlls/vnd3d11) turns every COM method call into a call
//! here with id = interface << 8 | vtable index (interfaces 64+, `names`), through d3d9.dll's
//! dispatcher. Objects are { vtable, id } blocks in guest memory; an object answering several
//! interfaces (the device is also an IDXGIDevice) has a block per interface ("faces").
//!
//! Draws become commands of the d3d9 batch queue (`d3d9::gpu`, run by crates/kura/src/render/d3d.rs):
//! shaders go DXBC -> WGSL (the dxwgsl crate), buffers are guest memory copied into a new page
//! buffer whenever a draw needs them after the guest wrote them.

pub mod d2d;
pub mod d3d10;
mod deferred;
pub mod check;
pub mod format;
mod hazards;
pub mod names;
pub mod winfmt;
#[cfg(test)]
mod trace_tests;
#[cfg(test)]
mod map_tests;
#[cfg(test)]
mod bind_tests;
#[cfg(test)]
mod draw_tests;
#[cfg(test)]
mod deferred_tests;
#[cfg(test)]
mod desc_tests;
#[cfg(test)]
mod copy_tests;
#[cfg(test)]
mod msaa_tests;
#[cfg(test)]
mod bits_tests;
#[cfg(test)]
mod compute_tests;
#[cfg(test)]
mod dxgi_tests;

use crate::d3d9::gpu;
use crate::d3d9::HostCalls;
use std::collections::HashMap;
use std::sync::Mutex;

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
const E_FAIL: u32 = 0x8000_4005;
const DXGI_ERROR_NOT_FOUND: u32 = 0x887a_0002;
const DXGI_ERROR_INVALID_CALL: u32 = 0x887a_0001;
const DXGI_ERROR_UNSUPPORTED: u32 = 0x887a_0004;

/// host ids outside the interfaces (d3d11.dll)
pub const INIT: u32 = 0xffff_0011;
pub const CREATE_DEVICE: u32 = 0xffff_0012;
pub const CREATE_FACTORY: u32 = 0xffff_0013;
/// D3D10CreateDeviceAndSwapChain1(adapter, driver, software, flags, level, sdk, desc, swapchain, device)
pub const CREATE_DEVICE10: u32 = 0xffff_0014;

pub const IF_FACTORY: u32 = 64;
pub const IF_ADAPTER: u32 = 65;
pub const IF_OUTPUT: u32 = 66;
pub const IF_SWAPCHAIN: u32 = 67;
pub const IF_DXGIDEVICE: u32 = 68;
pub const IF_SURFACE: u32 = 69;
pub const IF_DEVICE: u32 = 70;
pub const IF_CONTEXT: u32 = 71;
pub const IF_BUFFER: u32 = 72;
pub const IF_TEX1D: u32 = 73;
pub const IF_TEX2D: u32 = 74;
pub const IF_TEX3D: u32 = 75;
pub const IF_SRV: u32 = 76;
pub const IF_RTV: u32 = 77;
pub const IF_DSV: u32 = 78;
pub const IF_UAV: u32 = 79;
pub const IF_VS: u32 = 80;
pub const IF_PS: u32 = 81;
pub const IF_GS: u32 = 82;
pub const IF_HS: u32 = 83;
pub const IF_DS: u32 = 84;
pub const IF_CS: u32 = 85;
pub const IF_LAYOUT: u32 = 86;
pub const IF_SAMPLER: u32 = 87;
pub const IF_BLEND: u32 = 88;
pub const IF_DEPTH: u32 = 89;
pub const IF_RASTER: u32 = 90;
pub const IF_QUERY: u32 = 91;
pub const IF_LINKAGE: u32 = 92;
pub const IF_CMDLIST: u32 = 93;
/// ID3D10Multithread, on the device and the immediate context (Media Foundation asks for it)
pub const IF_MULTITHREAD: u32 = 94;
/// IDXGIResource, on every buffer and texture (crates/kura/src/d3d11/d3d10.rs has 95..=111)
pub const IF_DXGIRES: u32 = 112;
/// ID3DUserDefinedAnnotation, on every context (PIX events: nobody is listening)
pub const IF_ANNOTATION: u32 = 113;

// IIDs (first dword) and the interfaces that answer them
const IIDS: &[(u32, &[u32])] = &[
    (0x0000_0000, &[]), // IUnknown: any
    (0xaec22fb8, &[IF_FACTORY, IF_ADAPTER, IF_OUTPUT, IF_SWAPCHAIN, IF_DXGIDEVICE, IF_SURFACE]), // IDXGIObject
    (0x7b7166ec, &[IF_FACTORY]),
    (0x770aae78, &[IF_FACTORY]),
    (0x50c83a1c, &[IF_FACTORY]),
    (0x25483823, &[IF_FACTORY]), // IDXGIFactory3
    (0x1bc6ea02, &[IF_FACTORY]), // IDXGIFactory4
    (0x7632e1f5, &[IF_FACTORY]), // IDXGIFactory5
    (0xc1b6694f, &[IF_FACTORY]), // IDXGIFactory6
    (0xa4966eed, &[IF_FACTORY]), // IDXGIFactory7
    (0x2411e7e1, &[IF_ADAPTER]),
    (0x29038f61, &[IF_ADAPTER]),
    (0x0aa1ae0a, &[IF_ADAPTER]), // IDXGIAdapter2
    (0x645967a4, &[IF_ADAPTER]), // IDXGIAdapter3
    (0x3c8d99d1, &[IF_ADAPTER]), // IDXGIAdapter4
    (0xae02eedb, &[IF_OUTPUT]),
    (0x00cddea8, &[IF_OUTPUT]), // IDXGIOutput1
    (0x595e39d1, &[IF_OUTPUT]), // IDXGIOutput2
    (0x8a6bb301, &[IF_OUTPUT]), // IDXGIOutput3
    (0xdc7dca35, &[IF_OUTPUT]), // IDXGIOutput4
    (0x80a07424, &[IF_OUTPUT]), // IDXGIOutput5
    (0x068346e8, &[IF_OUTPUT]), // IDXGIOutput6
    (0x310d36a0, &[IF_SWAPCHAIN]),
    (0x790a45f7, &[IF_SWAPCHAIN]),
    (0x54ec77fa, &[IF_DXGIDEVICE]),
    (0x77db970f, &[IF_DXGIDEVICE]),
    (0x05008617, &[IF_DXGIDEVICE]), // IDXGIDevice2
    (0x6007896c, &[IF_DXGIDEVICE]), // IDXGIDevice3
    (0xcafcb56c, &[IF_SURFACE]),
    (0x4ae63092, &[IF_SURFACE]),
    (0xaba496dd, &[IF_SURFACE]), // IDXGISurface2
    (0x035f3ab4, &[IF_DXGIRES]), // IDXGIResource
    (0xb2daad8b, &[IF_ANNOTATION]), // ID3DUserDefinedAnnotation
    (0x3d3e0379, &[IF_SWAPCHAIN, IF_SURFACE]),
    (0xdb6f6ddb, &[IF_DEVICE]),
    (0xa04bfb29, &[IF_DEVICE]), // ID3D11Device1
    (0x9d06dffa, &[IF_DEVICE]), // ID3D11Device2
    (0xa05c8c37, &[IF_DEVICE]), // ID3D11Device3
    (0x8992ab71, &[IF_DEVICE]), // ID3D11Device4
    (0x8ffde202, &[IF_DEVICE]), // ID3D11Device5
    (0xbb2c6faa, &[IF_CONTEXT]), // ID3D11DeviceContext1
    (0x420d5b32, &[IF_CONTEXT]), // ID3D11DeviceContext2
    (0xb4e3c01d, &[IF_CONTEXT]), // ID3D11DeviceContext3
    (0x917600da, &[IF_CONTEXT]), // ID3D11DeviceContext4
    (0xcc86fabe, &[IF_BLEND]), // ID3D11BlendState1
    (0x1217d7a6, &[IF_RASTER]), // ID3D11RasterizerState1
    (0x6fbd02fb, &[IF_RASTER]), // ID3D11RasterizerState2
    (0x5c1e0d8a, &[IF_LINKAGE]), // ID3DDeviceContextState (a device child like the linkage)
    (0xc0bfa96c, &[IF_CONTEXT]),
    (0x1841e5c8, &[IF_CONTEXT, IF_BUFFER, IF_TEX1D, IF_TEX2D, IF_TEX3D, IF_SRV, IF_RTV, IF_DSV, IF_UAV, IF_VS, IF_PS, IF_GS, IF_HS, IF_DS, IF_CS, IF_LAYOUT, IF_SAMPLER, IF_BLEND, IF_DEPTH, IF_RASTER, IF_QUERY, IF_LINKAGE, IF_CMDLIST]),
    (0xdc8e63f3, &[IF_BUFFER, IF_TEX1D, IF_TEX2D, IF_TEX3D]),
    (0x48570b85, &[IF_BUFFER]),
    (0xf8fb5c27, &[IF_TEX1D]),
    (0x6f15aaf2, &[IF_TEX2D]),
    (0x037e866e, &[IF_TEX3D]),
    (0x839d1216, &[IF_SRV, IF_RTV, IF_DSV, IF_UAV]),
    (0xb0e06fe0, &[IF_SRV]),
    (0xdfdba067, &[IF_RTV]),
    (0x9fdac92a, &[IF_DSV]),
    (0x28acf509, &[IF_UAV]),
    (0x3b301d64, &[IF_VS]),
    (0xea82e40d, &[IF_PS]),
    (0x38325b96, &[IF_GS]),
    (0x8e5c6061, &[IF_HS]),
    (0xf582c508, &[IF_DS]),
    (0x4f5b196e, &[IF_CS]),
    (0xe4819ddc, &[IF_LAYOUT]),
    (0xda6fea51, &[IF_SAMPLER]),
    (0x75b68faa, &[IF_BLEND]),
    (0x03823efb, &[IF_DEPTH]),
    (0x9bb4ab81, &[IF_RASTER]),
    (0x4b35d0cd, &[IF_QUERY]),
    (0xd6c00747, &[IF_QUERY]),
    (0xddf57cba, &[IF_LINKAGE]),
    (0xa24bc4d1, &[IF_CMDLIST]),
    (0x9b7e4e00, &[IF_MULTITHREAD]),
];

/// Interfaces another face of the same object can answer (device <-> IDXGIDevice, 2D textures
/// <-> IDXGISurface)
fn other_face(primary: u32, want: u32) -> bool {
    matches!((primary, want), (IF_DEVICE, IF_DXGIDEVICE) | (IF_TEX2D, IF_SURFACE) | (IF_DEVICE | IF_CONTEXT, IF_MULTITHREAD) | (IF_CONTEXT, IF_ANNOTATION))
        // (a D3D11 device has no ID3D10Device face to ask for: D3D10CreateDevice makes it)
        || (primary != IF_DEVICE && d3d10::ten(primary) == Some(want))
}

pub struct Buffer {
    pub size: u32,
    pub usage: u32,
    pub bind: u32,
    /// what GetDesc gives back
    pub cpu: u32,
    pub misc: u32,
    pub stride: u32,
    pub mem: u32,
    /// page buffer with the current contents (0: none yet)
    pub gpu: u32,
    /// the guest wrote since `gpu` was made
    pub dirty: bool,
    /// every write since the last upload was through a Map NO_OVERWRITE (bytes no draw before
    /// reads: only the changed ones go up, in place)
    pub stream: bool,
    /// a dynamic buffer's bytes as last uploaded (what `stream` uploads are diffed against)
    pub shadow: Vec<u8>,
}

pub struct Texture {
    /// 1, 2, 3
    pub dim: u32,
    pub w: u32,
    pub h: u32,
    /// array size (depth for 3D)
    pub layers: u32,
    pub mips: u32,
    pub fmt: u32,
    pub page: u32,
    pub usage: u32,
    pub bind: u32,
    pub cube: bool,
    /// what GetDesc gives back: CPU access, misc flags, (samples, quality)
    pub cpu: u32,
    pub misc: u32,
    pub samples: (u32, u32),
    /// a swap chain's buffer
    pub backbuffer: bool,
    pub gpu: u32,
    /// mapped subresources: (subresource, guest memory, pitch, write)
    pub maps: Vec<(u32, u32, u32, bool)>,
    /// a staging texture's subresources whose map memory holds what the page has (since their
    /// last Unmap nothing wrote them): a Map reads no pixels back
    pub clean: Vec<u32>,
    /// the guest's bytes of each subresource written (rows of `format::pitch_rows`, slice after
    /// slice), for formats the page keeps converted (see `keeps_bits`): what Map and copies give
    /// back, as Windows' are the bits written
    pub bits: HashMap<u32, Vec<u8>>,
    /// a staging texture made without initial data that nothing wrote on the page yet: zeros
    /// there (a new WebGPU texture), so a subresource's first Map reads nothing back
    pub pristine: bool,
}

/// The page's texels are the guest's bytes (read back as they are).
fn holds_bits(t: &Texture) -> bool {
    match format::info(t.fmt) {
        Some(f) => f.bpp == format::page_bpp(t.page) && f.block == 1 && f.conv == 0 && !f.no_alpha,
        None => format::page_bpp(t.page) == 4,
    }
}

/// The guest's bytes are kept beside the page's texels: a format the page converts (and
/// 32-bit float RGBA, kept as halves on a device without float32-filterable), of a texture
/// nothing but uploads and copies write (no render target, depth or UAV binding).
fn keeps_bits(t: &Texture) -> bool {
    (!holds_bits(t) || t.page == format::P_RGBA32F) && t.bind & 0xe0 == 0 && !format::is_depth(t.page)
}

/// A subresource's (level, layer or 0 for 3D, width, height, slices).
fn sub_size(t: &Texture, sub: u32) -> (u32, u32, u32, u32, u32) {
    let level = sub % t.mips.max(1);
    let layer = if t.dim == 3 { 0 } else { sub / t.mips.max(1) };
    let depth = if t.dim == 3 { (t.layers >> level).max(1) } else { 1 };
    (level, layer, (t.w >> level).max(1), (t.h >> level).max(1), depth)
}

#[derive(Clone)]
pub struct View {
    pub res: u32,
    pub fmt: u32,
    /// what GetDesc gives back (format, dimension, a DSV's flags, the union)
    pub desc: Vec<u32>,
    pub mip: u32,
    pub mips: u32,
    pub layer: u32,
    pub layers: u32,
    /// a buffer view (shader resource view of a buffer): `res` is then a one-row texture of
    /// its elements, copied from the buffer when they change (WGSL has no texel buffers; the
    /// shaders fetch from that texture, see wgsl.rs)
    pub buf: Option<BufSrc>,
}

#[derive(Clone)]
pub struct BufSrc {
    pub buf: u32,
    /// first byte, bytes
    pub off: u32,
    pub bytes: u32,
    /// what the texture holds
    pub last: Vec<u8>,
}

pub struct Shader {
    pub code: Vec<u8>,
    pub pixel: bool,
    /// page shader and bindings, per pixel shader for vertex shaders (key 0 otherwise)
    /// by the pixel shader a vertex shader feeds (see shader_for) and the samplers addressing
    /// as WebGPU's don't (dxwgsl::addr)
    pub wgsl: HashMap<(u32, Vec<dxwgsl::addr::Addr>), Option<(u32, Vec<(u32, dxwgsl::Binding)>)>>,
}

pub struct Elem {
    pub name: String,
    pub index: u32,
    pub fmt: u32,
    pub slot: u32,
    pub offset: u32,
    pub instance: bool,
}

pub struct SwapChain {
    pub device: u32,
    pub hwnd: u32,
    pub w: u32,
    pub h: u32,
    pub fmt: u32,
    pub buffer: u32,
    pub presents: u32,
    /// its buffer's DXGI_SAMPLE_DESC (count, quality)
    pub samples: (u32, u32),
}

pub enum Data {
    Factory,
    /// an adapter: WARP's (Microsoft Basic Render Driver) when true
    Adapter(bool),
    Output,
    Device,
    Context,
    SwapChain(SwapChain),
    Buffer(Buffer),
    Texture(Texture),
    View(View),
    Shader(Shader),
    Layout(Vec<Elem>),
    /// state objects: their description's dwords
    State(Vec<u32>),
    /// (type, state: 0 never begun or ended, 1 between Begin and End, 2 ended; timestamp)
    Query(u32, u32, u64),
    /// ID3DDeviceContextState: the context state it holds while another one is current
    CtxState(Option<Box<State>>),
    /// a deferred context: its state and the list it records
    Deferred(Box<deferred::Deferred>),
    CmdList(Box<deferred::CmdList>),
    Other,
}

pub struct Obj {
    pub iface: u32,
    pub refs: u32,
    pub binds: u32,
    /// (interface, guest block)
    pub faces: Vec<(u32, u32)>,
    pub data: Data,
    /// (guid, data, the object of SetPrivateDataInterface it holds a reference to)
    pub private: Vec<([u8; 16], Vec<u8>, u32)>,
}

#[derive(Clone)]
pub struct State {
    /// (D3D11_IA_VERTEX_INPUT_RESOURCE_SLOT_COUNT: 32)
    vbs: [(u32, u32, u32); 32],
    ib: (u32, u32, u32),
    layout: u32,
    topology: u32,
    vs: u32,
    ps: u32,
    /// per stage: VS, PS, GS, HS, DS, CS (only VS and PS draw)
    cbs: [[u32; 15]; 6],
    srvs: [[u32; 128]; 6],
    samplers: [[u32; 16]; 6],
    /// GS, HS, DS and CS shaders (kept for Get*)
    shaders: [u32; 4],
    /// unordered access views: the output merger's and the compute shader's (kept for Get*)
    uavs: [[u32; 8]; 2],
    /// OMSetBlendState's sample mask; SOSetTargets' buffers (kept for Get*)
    sample_mask: u32,
    so: [u32; 4],
    rtvs: [u32; 8],
    dsv: u32,
    /// (all 16: RSGetViewports gives them back; draws use the first)
    viewport: [[f32; 6]; 16],
    scissor: [[i32; 4]; 16],
    blend: u32,
    blend_factor: [f32; 4],
    depth: u32,
    stencil_ref: u32,
    raster: u32,
    /// SetPredication (predicate, value): kept for GetPredication, draws are not predicated
    predicate: (u32, u32),
    /// how many viewports and scissor rects RSSet* gave (RSGet* report them)
    nviewports: u32,
    nscissors: u32,
}

pub struct D11 {
    /// CreateDeviceContextState was called: QueryInterface gives the device's D3D10 faces
    ten_faces: bool,
    /// the device was last made by D3D10CreateDevice*: its children's D3D10 faces give it back
    /// (GetDevice), those of a D3D11 device's children give none
    made10: bool,
    /// the feature level the device was last made at (GetFeatureLevel, which shaders it takes)
    level: u32,
    /// (queries' End count: their timestamps)
    ticks: u64,
    vtbls: Vec<u32>,
    pub objs: HashMap<u32, Obj>,
    next: u32,
    free_blocks: Vec<u32>,
    calls: *const HostCalls,
    trace: bool,
    /// ID3D10Multithread::SetMultithreadProtected (only reported back)
    mt_protected: bool,
    counts: HashMap<u32, u32>,
    device: u32,
    context: u32,
    factory: u32,
    adapter: u32,
    /// the second adapter, Microsoft Basic Render Driver (WARP: no outputs), last on every
    /// Windows since 8
    warp: u32,
    output: u32,
    st: State,
    /// the ID3DDeviceContextState whose state `st` is (0: the context's own)
    st_owner: u32,
    pipelines: HashMap<Vec<u32>, u32>,
    /// point sprites' viewport uniform (page buffer, the half size written to it)
    sprite_view: (u32, [f32; 2]),
    /// page targets set in this batch generation
    targets: Option<(u64, Vec<u32>)>,
    next_gpu: u32,
    /// the page's lost devices seen (render::lost_gen, see `after_lost`)
    lost_seen: u32,
}

unsafe impl Send for D11 {}

static D11S: Mutex<Option<D11>> = Mutex::new(None);

/// What a call does after the lock is dropped.
enum After {
    None,
    /// (to window `.0`)
    Present(u32),
    /// readbacks to wait for
    Readback(Vec<u32>),
    /// milliseconds to sleep
    Sleep(u64),
}

/// IPPAI_D3D_TRACE=2 (the shaders' WGSL, each draw's first vertices): read once, not per draw.
fn trace2() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("IPPAI_D3D_TRACE").as_deref() == Ok("2"))
}

unsafe fn rd(p: u32, i: usize) -> u32 {
    *((p as *const u32).add(i))
}

unsafe fn wr(p: u32, i: usize, v: u32) {
    if p != 0 {
        *((p as *mut u32).add(i)) = v;
    }
}

unsafe fn rdf(p: u32, i: usize) -> f32 {
    f32::from_bits(rd(p, i))
}

/// Size of the guest's pointers (and HWNDs, SIZE_Ts): 8 for 64-bit programs.
/// Guest addresses are below 4G either way.
use crate::d3d9::device::ptr;

/// Pointer `i` of an array of pointers.
unsafe fn rdp(p: u32, i: usize) -> u32 {
    rd(p, i * (ptr() / 4) as usize)
}

unsafe fn wrp(p: u32, i: usize, v: u32) {
    wr(p, i * (ptr() / 4) as usize, v);
    if ptr() == 8 {
        wr(p, i * 2 + 1, 0);
    }
}

/// DXGI_SWAP_CHAIN_DESC as 15 dwords in its 32-bit layout (OutputWindow at 11)
unsafe fn swap_desc(desc: u32) -> [u32; 15] {
    let mut p = [0u32; 15];
    for (i, v) in p.iter_mut().enumerate() {
        // (64-bit: OutputWindow is 8-byte aligned, at dword 12)
        let k = if ptr() == 8 && i >= 11 { i + 1 + (i > 11) as usize } else { i };
        *v = rd(desc, k);
    }
    p
}

unsafe fn put_swap_desc(desc: u32, p: [u32; 15]) {
    for (i, v) in p.into_iter().enumerate() {
        let k = if ptr() == 8 && i >= 11 { i + 1 + (i > 11) as usize } else { i };
        wr(desc, k, v);
    }
    if ptr() == 8 {
        wr(desc, 11, 0);
        wr(desc, 13, 0);
    }
}

/// D3D11_MAPPED_SUBRESOURCE { pData, RowPitch, DepthPitch }
unsafe fn put_mapped(out: u32, mem: u32, row: u32, depth: u32, dxgi: bool) {
    if dxgi {
        // DXGI_MAPPED_RECT: Pitch, pBits
        wr(out, 0, row);
        wrp(out + ptr(), 0, mem);
        return;
    }
    wrp(out, 0, mem);
    wr(out + ptr(), 0, row);
    wr(out + ptr(), 1, depth);
}

/// Page ids for textures and buffers (shared with d3d9's: one page)
pub(crate) fn page_id() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x4000_0000);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl D11 {
    fn calls(&self) -> &HostCalls {
        unsafe { &*self.calls }
    }

    fn guest_alloc(&self, size: u32) -> u32 {
        unsafe { (self.calls().alloc)(size.max(16)) as u32 }
    }

    fn guest_free(&self, p: u32) {
        if p != 0 {
            unsafe { (self.calls().free)(p as *mut u8) }
        }
    }

    fn block(&mut self, iface: u32, id: u32) -> u32 {
        if self.free_blocks.is_empty() {
            let page = self.guest_alloc(65536);
            if page == 0 {
                return 0;
            }
            for i in (0..65536 / 16).rev() {
                self.free_blocks.push(page + i * 16);
            }
        }
        let addr = self.free_blocks.pop().unwrap();
        unsafe {
            wrp(addr, 0, self.vtbls.get(iface as usize).copied().unwrap_or(0));
            *((addr + ptr()) as *mut u32) = id;
        }
        addr
    }

    /// A new object: returns its guest address.
    fn new_obj(&mut self, iface: u32, data: Data) -> u32 {
        self.next += 1;
        let id = self.next;
        let addr = self.block(iface, id);
        self.objs.insert(id, Obj { iface, refs: 1, binds: 0, faces: vec![(iface, addr)], data, private: Vec::new() });
        if holds_device(iface) && iface != IF_CONTEXT && self.device != 0 {
            self.add_ref(self.device);
        }
        addr
    }

    /// The block of `id` for interface `iface` (made on first use).
    fn face(&mut self, id: u32, iface: u32) -> u32 {
        let Some(o) = self.objs.get(&id) else { return 0 };
        if let Some(&(_, a)) = o.faces.iter().find(|f| f.0 == iface) {
            return a;
        }
        let a = self.block(iface, id);
        self.objs.get_mut(&id).unwrap().faces.push((iface, a));
        a
    }

    fn id_of(&self, addr: u32) -> u32 {
        if addr == 0 {
            return 0;
        }
        let id = unsafe { *((addr + ptr()) as *const u32) };
        if self.objs.get(&id).is_some_and(|o| o.faces.iter().any(|f| f.1 == addr)) { id } else { 0 }
    }

    fn addr_of(&self, id: u32) -> u32 {
        self.objs.get(&id).map_or(0, |o| o.faces[0].1)
    }

    fn add_ref(&mut self, id: u32) -> u32 {
        match self.objs.get_mut(&id) {
            Some(o) => {
                o.refs += 1;
                let r = o.refs;
                // (a device child holds the device while anyone holds the child)
                if r == 1 && holds_device(o.iface) {
                    self.add_ref(self.device);
                }
                r
            }
            None => 0,
        }
    }

    fn release(&mut self, id: u32) -> u32 {
        let Some(o) = self.objs.get_mut(&id) else { return 0 };
        let was = o.refs;
        o.refs = o.refs.saturating_sub(1);
        let r = o.refs;
        let (iface, binds) = (o.iface, o.binds);
        // (one device per process here: "two" devices, one stored in the other's private data,
        // are one holding itself. Once only those references are left, nobody holds it)
        let selfs = o.private.iter().filter(|p| p.2 == id).count() as u32;
        if r > 0 && r == selfs && id == self.device {
            o.private.retain(|p| p.2 != id);
            o.refs = 0;
            if holds_device(iface) {
                self.release(self.device);
            }
            if binds == 0 {
                self.destroy(id);
            }
            return 0;
        }
        if id == self.device && self.trace && r > 0 {
            // (who still holds the device: device children the application has not let go of)
            let mut left: Vec<String> = self.objs.iter().filter(|(_, o)| o.refs > 0 && holds_device(o.iface) && o.iface != IF_CONTEXT).map(|(&k, o)| format!("{}#{k}x{}", name(o.iface, 0).split(':').next().unwrap_or(""), o.refs)).collect();
            left.sort();
            eprintln!("d3d11: device released, {r} left: {}", left.join(" "));
        }
        if was == 1 && holds_device(iface) {
            self.release(self.device);
        }
        if r == 0 && binds == 0 {
            self.destroy(id);
        }
        r
    }

    fn bind(&mut self, id: u32) {
        if let Some(o) = self.objs.get_mut(&id) {
            o.binds += 1;
        }
    }

    fn unbind(&mut self, id: u32) {
        let Some(o) = self.objs.get_mut(&id) else { return };
        o.binds = o.binds.saturating_sub(1);
        if o.refs == 0 && o.binds == 0 {
            self.destroy(id);
        }
    }

    /// Replaces a bound object (binds the new, unbinds the old).
    fn rebind(&mut self, new: u32, old: u32) {
        if new != 0 {
            self.bind(new);
        }
        if old != 0 {
            self.unbind(old);
        }
    }

    fn destroy(&mut self, id: u32) {
        // (the device, its context and the DXGI objects live as long as the process; but the
        // adapter objects each enumeration makes, Windows' new ones every time)
        let made = id != self.adapter && id != self.warp && matches!(self.objs.get(&id).map(|o| &o.data), Some(Data::Adapter(_)));
        if !made && matches!(self.objs.get(&id).map(|o| o.iface), Some(IF_DEVICE | IF_CONTEXT | IF_FACTORY | IF_ADAPTER | IF_OUTPUT) | None) {
            return;
        }
        let o = self.objs.remove(&id).unwrap();
        for &(_, _, held) in &o.private {
            if held != 0 {
                self.release(held);
            }
        }
        for (_, a) in &o.faces {
            unsafe { *((a + ptr()) as *mut u32) = 0 };
            self.free_blocks.push(*a);
        }
        match o.data {
            Data::Buffer(b) => {
                self.guest_free(b.mem);
                if b.gpu != 0 {
                    gpu::record(|q| q.cmd(&[gpu::DESTROY_BUFFER, b.gpu]));
                }
            }
            Data::Texture(t) => {
                for (_, mem, _, _) in t.maps {
                    self.guest_free(mem);
                }
                if t.gpu != 0 {
                    gpu::record(|q| q.cmd(&[gpu::DESTROY_TEXTURE, t.gpu]));
                }
            }
            Data::View(v) => {
                // (a view holds its resource privately: the resource's own count does not change)
                if let Some(b) = &v.buf {
                    self.unbind(b.buf);
                }
                self.unbind(v.res);
            }
            Data::Deferred(x) => deferred::drop_deferred(self, *x),
            Data::CmdList(l) => deferred::drop_list(self, *l),
            Data::CtxState(Some(s)) => unbind_state(self, &s),
            Data::SwapChain(s) => {
                self.release(s.buffer);
                // (the reference new_swapchain took)
                self.release(s.device);
            }
            _ => {}
        }
    }
}

/// d3d11.dll's DllMain: its vtables.
pub unsafe fn init(vtbls: *const u32, count: u32) -> u32 {
    let v: Vec<u32> = (0..count as usize).map(|i| rdp(vtbls as u32, i)).collect();
    let mut g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_mut() {
        Some(d) => d.vtbls = v,
        None => {
            *g = Some(D11 {
                ten_faces: false,
                level: 0xb000,
                made10: false,
                ticks: 0,
                vtbls: v,
                objs: HashMap::new(),
                next: 0,
                free_blocks: Vec::new(),
                calls: std::ptr::null(),
                trace: crate::env_on!("IPPAI_D3D_TRACE"),
                mt_protected: false,
                counts: HashMap::new(),
                device: 0,
                context: 0,
                factory: 0,
                adapter: 0,
                warp: 0,
                output: 0,
                st: default_state(),
                st_owner: 0,
                pipelines: HashMap::new(),
                sprite_view: (0, [0.0; 2]),
                targets: None,
                next_gpu: 0,
                lost_seen: crate::render::lost_gen(),
            })
        }
    }
    0
}

/// The arguments of method `m` of interface `iface` (as many as it has: the caller's array is
/// no longer), then zeros up to `n`.
pub(crate) fn args_of(iface: u32, m: u32, a: &dyn Fn(usize) -> u32, n: usize) -> Vec<u32> {
    let k = names::ARGS.get(iface.wrapping_sub(64) as usize).and_then(|v| v.get(m as usize)).map_or(0, |&k| k as usize);
    (0..n).map(|i| if i < k { a(i) } else { 0 }).collect()
}

/// Entry from d3d9's dispatcher: ids INIT.. and interfaces 64+.
pub unsafe fn call(id: u32, args: *const u32, calls: *const HostCalls) -> u32 {
    crate::usage::d3d(id);
    // (64-bit: the arguments are 8-byte slots; their low halves from here on)
    let mut low = [0u32; 16];
    let args = if ptr() == 8 {
        for (i, v) in low.iter_mut().enumerate() {
            *v = *args.add(i * 2);
        }
        low.as_ptr()
    } else {
        args
    };
    if id == INIT {
        return init(*args as *const u32, *args.add(1));
    }
    let (r, after) = {
        let mut g = D11S.lock().unwrap_or_else(|e| e.into_inner());
        let Some(d) = g.as_mut() else { return E_FAIL };
        d.calls = calls;
        after_lost(d);
        let a = |i: usize| *args.add(i);
        let mut after = After::None;
        let r = match id {
            CREATE_DEVICE => create_device(d, args),
            CREATE_DEVICE10 => create_device10(d, args),
            CREATE_FACTORY => {
                let f = dxgi_objects(d);
                let r = query_interface(d, f, IF_FACTORY, a(1), a(2));
                d.release(f);
                r
            }
            _ => {
                let (iface, m) = (id >> 8, id & 0xff);
                let this = d.id_of(a(0));
                let r = if this == 0 {
                    E_INVALIDARG
                } else {
                    match m {
                        0 => query_interface(d, this, iface, a(1), a(2)),
                        1 => d.add_ref(this),
                        2 => d.release(this),
                        _ => method(d, iface, m, this, args, &mut after),
                    }
                };
                if d.trace {
                    *d.counts.entry(id).or_default() += 1;
                    let n = d.counts[&id];
                    if n <= 20 || n.is_power_of_two() {
                        let nargs = (1..6).map(|i| format!("{:#x}", a(i))).collect::<Vec<_>>().join(",");
                        eprintln!("d3d11: {} [{n}] ({nargs}) -> {:#x}", name(iface, m), r);
                    }
                }
                r
            }
        };
        (r, after)
    };
    match after {
        After::None => {}
        After::Present(hwnd) => gpu::present(hwnd),
        After::Sleep(ms) => std::thread::sleep(std::time::Duration::from_millis(ms)),
        After::Readback(reqs) => {
            for req in reqs {
                gpu::wait_readback(req);
            }
        }
    }
    r
}

pub fn name(iface: u32, m: u32) -> String {
    match names::NAMES.get(iface.wrapping_sub(64) as usize) {
        Some((i, ms)) => format!("{i}::{}", ms.get(m as usize).copied().unwrap_or("?")),
        None => format!("{iface}::{m}"),
    }
}

unsafe fn query_interface(d: &mut D11, id: u32, iface: u32, riid: u32, out: u32) -> u32 {
    if out == 0 || riid == 0 {
        return E_INVALIDARG;
    }
    let first = rd(riid, 0);
    let primary = d.objs.get(&id).map_or(0, |o| o.iface);
    // (a resource's IDXGIObject, IDXGIDeviceSubObject and IDXGIResource are one face, its
    // IDXGISurface another, only for a texture of one level and one layer (1D or 2D))
    if matches!(primary, IF_BUFFER | IF_TEX1D | IF_TEX2D | IF_TEX3D) {
        let surface = matches!(d.objs.get(&id).map(|o| &o.data), Some(Data::Texture(t)) if t.mips == 1 && t.layers == 1 && t.dim != 3);
        let want = match first {
            0xaec22fb8 | 0x3d3e0379 | 0x035f3ab4 => IF_DXGIRES,
            0xcafcb56c | 0x4ae63092 | 0xaba496dd if surface => IF_SURFACE,
            0xcafcb56c | 0x4ae63092 | 0xaba496dd => 0,
            _ => u32::MAX,
        };
        if want != u32::MAX {
            if want == 0 {
                wrp(out, 0, 0);
                return E_NOINTERFACE;
            }
            d.add_ref(id);
            wrp(out, 0, d.face(id, want));
            return S_OK;
        }
    }
    let want = match IIDS.iter().chain(d3d10::IIDS10).find(|x| x.0 == first) {
        Some((0, _)) => primary,
        Some((_, ifs)) => {
            if ifs.contains(&iface) {
                iface
            } else if ifs.contains(&primary) {
                primary
            } else if let Some(&f) = ifs.iter().find(|&&f| other_face(primary, f) || ((d.ten_faces || d3d10::eleven(iface).is_some()) && d3d10::ten(primary) == Some(f))) {
                f
            } else {
                0
            }
        }
        None => 0,
    };
    // (a deferred context is no ID3D11Multithread)
    let want = if want == IF_MULTITHREAD && primary == IF_CONTEXT && id != d.context { 0 } else { want };
    if want == 0 {
        if d.trace {
            eprintln!("d3d11: QueryInterface({first:08x}) on {}: no", name(iface, 0));
        }
        wrp(out, 0, 0);
        return E_NOINTERFACE;
    }
    d.add_ref(id);
    wrp(out, 0, d.face(id, want));
    S_OK
}

/// The factory, adapter and output (made once); returns the factory with a new reference.
fn dxgi_objects(d: &mut D11) -> u32 {
    if d.factory == 0 {
        let f = d.new_obj(IF_FACTORY, Data::Factory);
        d.factory = d.id_of(f);
        let a = d.new_obj(IF_ADAPTER, Data::Adapter(false));
        d.adapter = d.id_of(a);
        let w = d.new_obj(IF_ADAPTER, Data::Adapter(true));
        d.warp = d.id_of(w);
        let o = d.new_obj(IF_OUTPUT, Data::Output);
        d.output = d.id_of(o);
        // (the adapters and the output: only the application's references count, as in Windows'
        // "Adapter has N references left")
        for id in [d.adapter, d.warp, d.output] {
            d.objs.get_mut(&id).unwrap().refs = 0;
        }
    } else {
        d.add_ref(d.factory);
    }
    d.factory
}

/// D3D11CreateDeviceAndSwapChain's arguments
unsafe fn create_device(d: &mut D11, args: *const u32) -> u32 {
    let a = |i: usize| *args.add(i);
    let (adapter, driver, levels, nlevels, desc, sc_out, dev_out, level_out, ctx_out) = (a(0), a(1), a(4), a(5), a(7), a(8), a(9), a(10), a(11));
    // (an adapter goes with D3D_DRIVER_TYPE_UNKNOWN, and only with it)
    if (adapter != 0) != (driver == 0) {
        for o in [sc_out, dev_out, ctx_out] {
            wrp(o, 0, 0);
        }
        wr(level_out, 0, 0);
        return E_INVALIDARG;
    }
    // D3D11_CREATE_DEVICE_VIDEO_SUPPORT: the adapter has no video decoder or processor (no
    // ID3D11VideoDevice), so a device for video fails as on a driver without video support
    // (Microsoft Basic Render Driver); Unity's VideoPlayer then decodes in software through
    // Media Foundation (#227), not into D3D11 textures this device could not give it
    if a(3) & 0x800 != 0 {
        for o in [sc_out, dev_out, ctx_out] {
            wrp(o, 0, 0);
        }
        wr(level_out, 0, 0);
        return DXGI_ERROR_UNSUPPORTED;
    }
    // (the swap chain's description is checked before anything is made: no window, or a format
    // a swap chain can't have, and nothing comes back)
    if desc != 0 && sc_out != 0 {
        let p = swap_desc(desc);
        let bad = if p[11] == 0 {
            DXGI_ERROR_INVALID_CALL
        } else if !matches!(p[4], 10 | 24 | 28 | 29 | 87 | 91) {
            E_INVALIDARG
        } else {
            S_OK
        };
        if bad != S_OK {
            for o in [sc_out, dev_out, ctx_out] {
                wrp(o, 0, 0);
            }
            wr(level_out, 0, 0);
            return bad;
        }
    }
    // feature level: the first asked for that is 11_0 or lower
    let mut level = 0xb000;
    if levels != 0 && nlevels > 0 {
        match (0..nlevels as usize).map(|i| rd(levels, i)).find(|&l| l <= 0xb000) {
            Some(l) => level = l,
            None => return E_INVALIDARG,
        }
    }
    dxgi_objects(d);
    d.release(d.factory);
    if d.device == 0 {
        let dev = d.new_obj(IF_DEVICE, Data::Device);
        d.device = d.id_of(dev);
        let ctx = d.new_obj(IF_CONTEXT, Data::Context);
        d.context = d.id_of(ctx);
        // (unreferenced until handed out: see `add_ref`)
        d.objs.get_mut(&d.context).unwrap().refs = 0;
        d.st = default_state();
    } else {
        // (a device nobody holds any more: the new one starts from the default state)
        if d.objs.get(&d.device).is_some_and(|o| o.refs == 0) && d.objs.get(&d.context).is_some_and(|o| o.refs == 0) {
            let old = std::mem::replace(&mut d.st, default_state());
            unbind_state(d, &old);
            // (and without what the old one kept in its private data)
            for id in [d.device, d.context] {
                let old = d.objs.get_mut(&id).map(|o| std::mem::take(&mut o.private)).unwrap_or_default();
                for (_, _, held) in old {
                    if held != 0 && held != id {
                        d.release(held);
                    }
                }
            }
        }
        d.add_ref(d.device);
    }
    wr(level_out, 0, level);
    d.level = level;
    d.made10 = false;
    if dev_out != 0 {
        wrp(dev_out, 0, d.addr_of(d.device));
    } else {
        d.release(d.device);
    }
    if ctx_out != 0 {
        d.add_ref(d.context);
        wrp(ctx_out, 0, d.addr_of(d.context));
    }
    if desc != 0 && sc_out != 0 {
        let p = swap_desc(desc);
        let sc = new_swapchain(d, p[11], p[0], p[1], p[4], (p[7], p[8]));
        wrp(sc_out, 0, sc);
        if sc == 0 {
            return E_FAIL;
        }
    }
    if d.trace {
        eprintln!("d3d11: device, feature level {level:#x}");
    }
    // (nothing asked for back: only a check that the device can be made)
    if dev_out == 0 && ctx_out == 0 && sc_out == 0 {
        return S_FALSE;
    }
    S_OK
}

/// Stored length of a state description: blend states keep D3D11_BLEND_DESC and then
/// LogicOpEnable, LogicOp per target; rasterizer states D3D11_RASTERIZER_DESC2.
fn state_len(iface: u32) -> usize {
    match iface {
        IF_BLEND => 66 + 16,
        IF_DEPTH => 13,
        IF_RASTER => 12,
        _ => 13,
    }
}

/// A state object for `desc`: one object per description while it lives, as Windows gives
/// back; S_FALSE without an out pointer.
unsafe fn new_state(d: &mut D11, iface: u32, desc: Vec<u32>, out: u32) -> u32 {
    if out == 0 {
        return S_FALSE;
    }
    let same = d.objs.iter().find(|(_, o)| o.iface == iface && matches!(&o.data, Data::State(x) if *x == desc)).map(|(&id, _)| id);
    if let Some(id) = same {
        d.add_ref(id);
        wrp(out, 0, d.addr_of(id));
        return S_OK;
    }
    let o = d.new_obj(iface, Data::State(desc));
    wrp(out, 0, o);
    S_OK
}

/// A state description as the state object keeps it (what GetDesc gives and what two
/// descriptions are compared by), or None for one Windows refuses (E_INVALIDARG): fields the
/// state ignores set as Windows sets them, BOOLs as 0 or 1 (tests/winref/golden/d3d11desc-32).
/// Blend: the targets in use (all with IndependentBlendEnable, else the first, which every
/// target then is) have masks of at most 0xf and, blending, blends of 1-11 or 14-19 (no color
/// ones for alpha) and operations of 1-5; a target that does not blend has ONE, ZERO, ADD.
/// Depth stencil: a write mask of 0 or 1 and, testing depth, a function of 1-8 (else LESS and a
/// full write mask); with stencil, operations and functions of 1-8 (else masks 0xff and KEEP,
/// KEEP, KEEP, ALWAYS); the bytes after the two stencil masks are padding. Rasterizer: fill
/// 2-3, cull 1-3, a depth bias clamp that is a number. Sampler: one of the filters, addresses
/// of 1-5, at most 16 anisotropy (0 without an anisotropic filter), a comparison of 1-8 with a
/// comparison filter (else NEVER), a bias of -16 to 15.99 and LODs that are numbers; the
/// border color is 0 without a BORDER address.
fn normalize_state(m: u32, desc: &mut [u32]) -> Option<()> {
    let b = |v: &mut u32| *v = (*v != 0) as u32;
    let ok = |c: bool| c.then_some(());
    let f = |v: u32| f32::from_bits(v);
    match m {
        20 => {
            b(&mut desc[0]);
            b(&mut desc[1]);
            let used = if desc[1] != 0 { 8 } else { 1 };
            let blend = |v: u32| matches!(v, 1..=11 | 14..=19);
            let alpha = |v: u32| blend(v) && !matches!(v, 3 | 4 | 9 | 10 | 16 | 17);
            for t in 0..used {
                let rt = &mut desc[2 + t * 8..2 + t * 8 + 8];
                rt[7] &= 0xff;
                ok(rt[7] <= 0xf)?;
                b(&mut rt[0]);
                if rt[0] != 0 {
                    ok(blend(rt[1]) && blend(rt[2]) && alpha(rt[4]) && alpha(rt[5]) && (1..=5).contains(&rt[3]) && (1..=5).contains(&rt[6]))?;
                } else {
                    rt[1..7].copy_from_slice(&[2, 1, 1, 2, 1, 1]);
                }
            }
            if used == 1 {
                let rt0: Vec<u32> = desc[2..10].to_vec();
                for t in 1..8 {
                    desc[2 + t * 8..10 + t * 8].copy_from_slice(&rt0);
                }
            }
        }
        21 => {
            b(&mut desc[0]);
            ok(desc[1] <= 1)?;
            if desc[0] != 0 {
                ok((1..=8).contains(&desc[2]))?;
            } else {
                (desc[1], desc[2]) = (1, 2);
            }
            b(&mut desc[3]);
            desc[4] &= 0xffff;
            if desc[3] != 0 {
                ok(desc[5..13].iter().all(|v| (1..=8).contains(v)))?;
            } else {
                desc[4] = 0xffff;
                for f in 0..2 {
                    desc[5 + 4 * f..9 + 4 * f].copy_from_slice(&[1, 1, 1, 8]);
                }
            }
        }
        22 => {
            ok((2..=3).contains(&desc[0]) && (1..=3).contains(&desc[1]) && !f(desc[4]).is_nan())?;
            b(&mut desc[2]);
            for v in &mut desc[6..10] {
                b(v);
            }
        }
        23 => {
            let filter = desc[0];
            ok(filter & !0x1ff == 0 && matches!(filter & 0x7f, 0 | 1 | 4 | 5 | 0x10 | 0x11 | 0x14 | 0x15 | 0x55))?;
            ok(desc[1..4].iter().all(|v| (1..=5).contains(v)) && desc[5] <= 16)?;
            if filter & 0x7f != 0x55 {
                desc[5] = 0;
            }
            if filter & 0x180 == 0x80 {
                ok((1..=8).contains(&desc[6]))?;
            } else {
                desc[6] = 1;
            }
            ok((-16.0..=15.99).contains(&f(desc[4])) && !f(desc[11]).is_nan() && !f(desc[12]).is_nan())?;
            if !desc[1..4].contains(&4) {
                desc[7..11].fill(0);
            }
        }
        _ => {}
    }
    Some(())
}

/// D3D10CreateDevice(AndSwapChain)(1): the D3D11 device's D3D10 face
unsafe fn create_device10(d: &mut D11, args: *const u32) -> u32 {
    let a = |i: usize| *args.add(i);
    let dev_out = a(8);
    // (out slots on the heap, not the stack: the host's heap is where 32-bit pointers reach in
    // native tests, a stack frame under ASan is not)
    let mut dev = vec![0u32; 2];
    let mut level = vec![0u32; 1];
    let v = [a(0), a(1), a(2), a(3), 0, 0, 7, a(6), a(7), dev.as_mut_ptr() as usize as u32, level.as_mut_ptr() as usize as u32, 0];
    let r = create_device(d, v.as_ptr());
    if r != S_OK {
        return r;
    }
    d.made10 = true;
    let id = d.id_of(rdp(dev.as_mut_ptr() as usize as u32, 0));
    if dev_out != 0 {
        let f = d.face(id, d3d10::IF10_DEVICE);
        wrp(dev_out, 0, f);
    } else {
        d.release(id);
    }
    S_OK
}

/// Whether an object of this interface is a device child: one that holds a reference to the
/// device while the application holds one to it (as in Windows: "Device has N references left").
fn holds_device(iface: u32) -> bool {
    // (a swap chain holds its device for as long as it lives: new_swapchain)
    !matches!(iface, IF_DEVICE | IF_FACTORY | IF_ADAPTER | IF_OUTPUT | IF_SWAPCHAIN)
}

/// The stage (2 GS, 3 HS, 4 DS, 5 CS) of a context method of the other stages.
fn stage_of(m: u32) -> usize {
    match m {
        22 | 23 | 31 | 32 | 81 | 82 | 87 | 88 => 2,
        59..=62 | 97..=100 => 3,
        63..=66 | 101..=104 => 4,
        _ => 5,
    }
}

/// Lets go of everything a state binds (ClearState, a new device).
fn unbind_state(d: &mut D11, old: &State) {
    for h in deferred::state_objects(old) {
        d.unbind(h);
    }
}

fn default_state() -> State {
    State {
        vbs: [(0, 0, 0); 32],
        ib: (0, 0, 0),
        layout: 0,
        topology: 0,
        vs: 0,
        ps: 0,
        cbs: [[0; 15]; 6],
        srvs: [[0; 128]; 6],
        samplers: [[0; 16]; 6],
        shaders: [0; 4],
        uavs: [[0; 8]; 2],
        sample_mask: 0xffff_ffff,
        so: [0; 4],
        rtvs: [0; 8],
        dsv: 0,
        viewport: [[0.0; 6]; 16],
        scissor: [[0; 4]; 16],
        blend: 0,
        blend_factor: [1.0; 4],
        depth: 0,
        stencil_ref: 0,
        raster: 0,
        predicate: (0, 0),
        nviewports: 0,
        nscissors: 0,
    }
}

/// A swap chain (address) whose buffer has `samples` (DXGI_SAMPLE_DESC: count, quality; a
/// multisampled one with BitBlt-model swap effects, resolved when it is presented).
unsafe fn new_swapchain(d: &mut D11, hwnd: u32, w: u32, h: u32, fmt: u32, samples: (u32, u32)) -> u32 {
    let mut rect = [0i32; 4];
    (d.calls().client_rect)(hwnd, rect.as_mut_ptr());
    let w = if w == 0 { (rect[2] - rect[0]).max(1) as u32 } else { w };
    let h = if h == 0 { (rect[3] - rect[1]).max(1) as u32 } else { h };
    let fmt = if format::info(fmt).is_some() { fmt } else { 28 };
    let buffer = new_backbuffer(d, w, h, fmt, samples);
    let bid = d.id_of(buffer);
    d.add_ref(d.device);
    let sc = d.new_obj(IF_SWAPCHAIN, Data::SwapChain(SwapChain { device: d.device, hwnd, w, h, fmt, buffer: bid, presents: 0, samples }));
    // (always: in every run's log, a game's picture starts here)
    eprintln!("sake: d3d11 swap chain {w}x{h} format {fmt} samples {} window {hwnd:#x} (client {}x{})", samples.0, rect[2] - rect[0], rect[3] - rect[1]);
    sc
}

/// A swap chain's buffer (address): views of it may take the sRGB twin of its format
/// (DXGI's back buffer cast).
fn new_backbuffer(d: &mut D11, w: u32, h: u32, fmt: u32, samples: (u32, u32)) -> u32 {
    let o = new_texture(d, 2, w, h, 1, 1, fmt, 0, 0x8 | 0x20, false, samples.0);
    let id = d.id_of(o);
    if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
        t.backbuffer = true;
        t.samples = (samples.0.max(1), samples.1);
    }
    o
}

/// A texture object (address); `samples` above 1: multisampled (on the page 4x where WebGPU
/// can, see render::d3d create_texture2).
#[allow(clippy::too_many_arguments)]
fn new_texture(d: &mut D11, dim: u32, w: u32, h: u32, layers: u32, mips: u32, fmt: u32, usage: u32, bind: u32, cube: bool, samples: u32) -> u32 {
    let page = format::page(fmt, bind).unwrap_or(format::P_BGRA8);
    let max = 32 - w.max(h).max(1).leading_zeros();
    let mips = if mips == 0 { max } else { mips.min(max) };
    let gpu = page_id();
    let layers = layers.max(1);
    // (bit 8: bound for unordered access, a storage texture on the page; bit 9: multisampled;
    // bit 10: 24-bit UNORM depth, which the page's depth24plus may hold as floats)
    let uav = if bind & 0x80 != 0 { 0x100 } else { 0 } | if samples > 1 { 0x200 } else { 0 } | if (44..=47).contains(&fmt) { 0x400 } else { 0 };
    gpu::record(|q| q.cmd(&[gpu::CREATE_TEXTURE2, gpu, w.max(1), h.max(1), layers, mips, page, uav | match dim {
        3 => 1,
        1 => 2,
        _ => 0,
    }]));
    let iface = match dim {
        1 => IF_TEX1D,
        3 => IF_TEX3D,
        _ => IF_TEX2D,
    };
    d.new_obj(iface, Data::Texture(Texture {
        dim,
        w,
        h,
        layers,
        mips,
        fmt,
        page,
        usage,
        bind,
        cube,
        cpu: match usage {
            2 => 0x10000,
            3 => 0x30000,
            _ => 0,
        },
        misc: if cube { 4 } else { 0 },
        samples: (samples.max(1), 0),
        backbuffer: false,
        gpu,
        maps: Vec::new(),
        clean: Vec::new(),
        bits: HashMap::new(),
        pristine: false,
    }))
}

/// Uploads a subresource (a box of it) from guest memory (`pitch` bytes per row, `slice` per 3D
/// slice) to the page texture, and keeps the bytes for formats the page converts.
unsafe fn upload_texture(t: &mut Texture, sub: u32, mem: usize, pitch: u32, slice: u32, bx: Option<[u32; 6]>) {
    // (of depth only 16-bit: WebGPU writes into no other; copies into 32-bit float depth are
    // drawn, see render/d3d.rs draw_depth)
    if t.gpu == 0 || matches!(t.page, format::P_D24S8 | format::P_D32F) || mem == 0 {
        return;
    }
    let (level, layer, lw, lh, ld) = sub_size(t, sub);
    let [x0, y0, z0, x1, y1, z1] = bx.unwrap_or([0, 0, 0, lw, lh, ld]);
    // (2D: one slice, whatever the box's front and back)
    let (z0, z1) = if t.dim == 3 { (z0.min(ld), z1.min(ld)) } else { (0, 1) };
    let Some(f) = format::info(t.fmt) else { return };
    let (x0, y0) = (x0 / f.block * f.block, y0 / f.block * f.block);
    let (w, h) = (x1.min(lw).saturating_sub(x0), y1.min(lh).saturating_sub(y0));
    if w == 0 || h == 0 || z1 <= z0 {
        return;
    }
    keep_bits(t, sub, mem, pitch, slice, [x0, y0, z0, w, h, z1]);
    for z in z0..z1 {
        let src = (mem as *const u8).add(((z - z0) * slice) as usize);
        let (out, bpr) = page_texels(t, &f, src, pitch, w, h);
        let lz = if t.dim == 3 { z } else { layer };
        gpu::record(|q| {
            let off = q.blob(&out);
            q.cmd(&[gpu::WRITE_TEXTURE2, t.gpu, level, lz, x0, y0, w, h, bpr, off]);
        });
    }
}

/// The page's device was lost and everything on it made again empty (render/mod.rs
/// `vnhost_render_lost`): what the guest still has goes up again. Every buffer from its guest
/// memory (the next use uploads it whole), every texture whose bytes are kept (`keeps_bits`)
/// from those. The rest (render targets, textures in formats the page holds as they are) is
/// zeros until the game writes it again.
fn after_lost(d: &mut D11) {
    let g = crate::render::lost_gen();
    if g == d.lost_seen {
        return;
    }
    d.lost_seen = g;
    let mut ids: Vec<u32> = d.objs.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        match d.objs.get_mut(&id).map(|o| &mut o.data) {
            Some(Data::Buffer(b)) if b.gpu != 0 => {
                b.dirty = true;
                b.stream = false;
            }
            Some(Data::Texture(t)) if t.gpu != 0 && !t.bits.is_empty() => {
                let mut subs: Vec<(u32, Vec<u8>)> = t.bits.iter().map(|(&k, v)| (k, v.clone())).collect();
                subs.sort_unstable_by_key(|s| s.0);
                for (sub, bytes) in subs {
                    let (_, _, lw, lh, _) = sub_size(t, sub);
                    let (pitch, rows) = format::pitch_rows(t.fmt, lw, lh);
                    unsafe { upload_texture(t, sub, bytes.as_ptr() as usize, pitch, pitch * rows, None) };
                }
                t.clean.clear();
            }
            _ => {}
        }
    }
}

/// Keeps the bytes of a box (x, y, z, width, height, end z, inside the subresource, x and y on
/// blocks) of guest texels for a format the page converts (see `keeps_bits`).
unsafe fn keep_bits(t: &mut Texture, sub: u32, mem: usize, pitch: u32, slice: u32, b: [u32; 6]) {
    let Some(f) = format::info(t.fmt).filter(|_| keeps_bits(t)) else { return };
    let [x0, y0, z0, w, h, z1] = b;
    let (_, _, lw, lh, ld) = sub_size(t, sub);
    let (gp, rows) = format::pitch_rows(t.fmt, lw, lh);
    let keep = t.bits.entry(sub).or_insert_with(|| vec![0; (gp * rows * ld) as usize]);
    let (bx0, by0) = (x0 / f.block, y0 / f.block);
    let (row, n) = format::pitch_rows(t.fmt, w, h);
    for z in z0..z1 {
        for r in 0..n {
            let from = (mem as *const u8).add(((z - z0) * slice + r * pitch) as usize);
            let at = ((z * rows + by0 + r) * gp + bx0 * f.bpp) as usize;
            keep[at..at + row as usize].copy_from_slice(std::slice::from_raw_parts(from, row as usize));
        }
    }
}

/// A box of guest texels (`pitch` bytes per row) as the page's (and its bytes per row).
unsafe fn page_texels(t: &Texture, f: &format::Format, src: *const u8, pitch: u32, w: u32, h: u32) -> (Vec<u8>, u32) {
    let mut out = Vec::new();
    let bpr;
    if f.conv == format::F32_TO_F16 {
        for y in 0..h {
            let p = src.add((y * pitch) as usize) as *const f32;
            for x in 0..(w * 4) as usize {
                out.extend_from_slice(&format::f16_bits(p.add(x).read_unaligned()).to_le_bytes());
            }
        }
        bpr = w * 8;
    } else if let Some(v) = format::to_floats(f.conv, format::page_bpp(t.page) / 4, src, pitch as usize, w, h) {
        out = v;
        bpr = w * format::page_bpp(t.page);
    } else if let Some(v) = format::bc67_texels(f.conv, src, pitch as usize, w, h) {
        out = v;
        bpr = w * format::page_bpp(t.page);
    } else if let Some(v) = format::widened_texels(f.conv, src, pitch as usize, w, h) {
        out = v;
        bpr = w * format::page_bpp(t.page);
    } else if f.conv != 0 {
        crate::d3d9::format::convert(f.conv, src, pitch as usize, w, h, &mut out);
        bpr = w * format::page_bpp(t.page);
    } else {
        let row = w.div_ceil(f.block) * f.bpp;
        for y in 0..h.div_ceil(f.block) {
            let p = src.add((y * pitch) as usize);
            out.extend_from_slice(std::slice::from_raw_parts(p, row as usize));
        }
        if f.no_alpha {
            for px in out.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
        bpr = row;
    }
    (out, bpr)
}

unsafe fn method(d: &mut D11, iface: u32, m: u32, this: u32, args: *const u32, after: &mut After) -> u32 {
    let a = |i: usize| *args.add(i);
    match iface {
        // IDXGIObject: SetPrivateData, SetPrivateDataInterface, GetPrivateData
        IF_FACTORY | IF_ADAPTER | IF_OUTPUT | IF_SWAPCHAIN | IF_DXGIDEVICE | IF_SURFACE | IF_DXGIRES if (3..=5).contains(&m) => private_data(d, [5, 6, 4][m as usize - 3], this, &a),
        IF_FACTORY => factory_call(d, m, this, &a),
        IF_ADAPTER => adapter_call(d, m, this, &a),
        IF_OUTPUT => output_call(d, m, &a, after),
        IF_SWAPCHAIN => swapchain_call(d, m, this, &a, after),
        // Enter/Leave: every call already runs under the host's lock
        IF_MULTITHREAD => match m {
            5 => std::mem::replace(&mut d.mt_protected, a(1) != 0) as u32,
            6 => d.mt_protected as u32,
            _ => 0,
        },
        IF_DXGIDEVICE => match m {
            6 | 7 => {
                // GetParent(riid, out) / GetAdapter(out)
                if m == 6 {
                    return parent(d, d.adapter, a(1), a(2));
                }
                d.add_ref(d.adapter);
                wrp(a(1), 0, d.addr_of(d.adapter));
                S_OK
            }
            8 => {
                // CreateSurface(desc, count, usage, shared, out[count]): 2D textures
                if a(1) == 0 || a(2) == 0 || a(5) == 0 {
                    return E_INVALIDARG;
                }
                let (w, h, fmt) = (rd(a(1), 0), rd(a(1), 1), rd(a(1), 2));
                if format::info(fmt).is_none() {
                    return E_INVALIDARG;
                }
                let usage = a(3);
                let bind = if usage & 0x20 != 0 { 0x20 } else { 0 } | if usage & 0x10 != 0 { 8 } else { 0 } | if usage & 0x400 != 0 { 0x80 } else { 0 };
                for i in 0..a(2) {
                    let o = new_texture(d, 2, w, h, 1, 1, fmt, 0, bind, false, 1);
                    let f = d.face(d.id_of(o), IF_SURFACE);
                    wrp(a(5), i as usize, f);
                }
                S_OK
            }
            9 => {
                // QueryResourceResidency(resources, status, n): all in memory
                std::ptr::write_bytes(a(2) as *mut u8, 0, 4 * a(3) as usize);
                S_OK
            }
            10 | 12 => S_OK,
            11 | 13 => {
                wr(a(1), 0, if m == 13 { 3 } else { 0 });
                S_OK
            }
            // IDXGIDevice2/3: OfferResources, ReclaimResources(n, res, discarded), Trim
            14 | 17 => S_OK,
            15 => {
                if a(3) != 0 {
                    std::ptr::write_bytes(a(3) as *mut u8, 0, 4 * a(1) as usize);
                }
                S_OK
            }
            _ => E_NOTIMPL,
        },
        // BeginEvent / EndEvent: -1 (no tool attached); SetMarker; GetStatus: FALSE
        IF_ANNOTATION => match m {
            3 | 4 => 0xffff_ffff,
            _ => 0,
        },
        IF_DXGIRES => match m {
            6 | 7 => parent(d, d.device, a(1), a(2)),
            8 => {
                // GetSharedHandle: none (nothing is shared across processes)
                wr(a(1), 0, 0);
                S_OK
            }
            9 => {
                // GetUsage: DXGI_USAGE_SHADER_INPUT / RENDER_TARGET_OUTPUT / UNORDERED_ACCESS by
                // the bind flags (and BACK_BUFFER for a swap chain's)
                let (bind, back) = match d.objs.get(&this).map(|o| &o.data) {
                    Some(Data::Texture(t)) => (t.bind, t.backbuffer),
                    Some(Data::Buffer(b)) => (b.bind, false),
                    _ => (0, false),
                };
                let mut u = 0;
                for (b, f) in [(0x8, 0x10), (0x20, 0x20), (0x80, 0x400)] {
                    if bind & b != 0 {
                        u |= f;
                    }
                }
                if back {
                    u |= 0x40;
                }
                wr(a(1), 0, u);
                S_OK
            }
            10 | 11 => {
                let primary = d.objs.get(&this).map_or(0, |o| o.iface);
                child_call(d, primary, m - 2, this, &a)
            }
            _ => E_NOTIMPL,
        },
        IF_SURFACE => match m {
            6 | 7 => parent(d, d.device, a(1), a(2)),
            8 => {
                // GetDesc: Width Height Format SampleDesc
                let Some(Data::Texture(t)) = d.objs.get(&this).map(|o| &o.data) else { return E_FAIL };
                for (i, v) in [t.w, t.h, t.fmt, t.samples.0, t.samples.1].into_iter().enumerate() {
                    wr(a(1), i, v);
                }
                S_OK
            }
            // Map(rect, flags): DXGI_MAP_READ 1 / WRITE 2 / DISCARD 4 as D3D11_MAP 1 / 2 / 3 / 4
            9 => {
                let f = a(2);
                let ty = if f & 4 != 0 { 4 } else if f & 3 == 3 { 3 } else if f & 2 != 0 { 2 } else { 1 };
                return map(d, d.addr_of(this), 0, ty, a(1), true, after);
            }
            10 => {
                unmap(d, d.addr_of(this), 0);
                S_OK
            }
            // IDXGISurface2::GetResource(riid, out, subresource): the texture it is (a surface is
            // one level of one layer: subresource 0)
            13 => {
                let r = query_interface(d, this, IF_TEX2D, a(1), a(2));
                if a(3) != 0 {
                    wr(a(3), 0, 0);
                }
                r
            }
            _ => E_NOTIMPL,
        },
        IF_DEVICE => device_call(d, m, this, &a),
        IF_CONTEXT if this != d.context => deferred::call(d, this, m, &a, after),
        IF_CONTEXT => context_call(d, m, &a, after),
        d3d10::IF10_DEVICE => d3d10::device(d, m, this, &a, after),
        d3d10::IF10_BUFFER..=d3d10::IF10_QUERY => d3d10::child(d, iface, m, this, &a, after),
        _ => child_call(d, iface, m, this, &a),
    }
}

unsafe fn parent(d: &mut D11, id: u32, riid: u32, out: u32) -> u32 {
    let iface = d.objs.get(&id).map_or(0, |o| o.iface);
    query_interface(d, id, iface, riid, out)
}

/// IDXGIAdapter..IDXGIAdapter4 of sake's GPU (`d.adapter`) and of WARP (`d.warp`).
unsafe fn adapter_call(d: &mut D11, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    let warp = is_warp(d, this);
    match m {
        6 => parent(d, d.factory, a(1), a(2)),
        7 => {
            // EnumOutputs (WARP has none)
            if a(2) == 0 {
                return E_INVALIDARG;
            }
            if a(1) != 0 || warp {
                wrp(a(2), 0, 0);
                return DXGI_ERROR_NOT_FOUND;
            }
            d.add_ref(d.output);
            wrp(a(2), 0, d.addr_of(d.output));
            S_OK
        }
        8 | 10 | 11 | 18 if a(1) == 0 => E_INVALIDARG,
        8 | 10 | 11 | 18 => {
            adapter_desc(a(1), warp, adapter_luid(d, this), m);
            S_OK
        }
        9 => {
            // CheckInterfaceSupport(iid, umd version): only the D3D10 devices answer
            if a(1) == 0 || !matches!(rd(a(1), 0), 0x9b7e4c0f | 0x9b7e4c8f) || rd(a(1), 1) != 0x4106_342c {
                return DXGI_ERROR_UNSUPPORTED;
            }
            // (30.0.15.1179)
            wr(a(2), 0, 15 << 16 | 1179);
            wr(a(2), 1, 30 << 16);
            S_OK
        }
        12 | 14 | 16 => {
            // Register*Event(event, cookie)
            if a(1) == 0 || a(2) == 0 {
                return DXGI_ERROR_INVALID_CALL;
            }
            wr(a(2), 0, 1);
            S_OK
        }
        13 | 17 => 0, // Unregister*
        15 => {
            // QueryVideoMemoryInfo(node, segment group local / non-local, info)
            if a(1) != 0 || a(2) > 1 || a(3) == 0 {
                return E_INVALIDARG;
            }
            let budget: u64 = match (warp, a(2)) {
                (true, 0) | (false, 1) => 0,
                (true, _) => 1 << 30,
                (false, _) => 512 << 20,
            };
            for (i, v) in [budget, 0, budget / 2, 0].into_iter().enumerate() {
                wr(a(3), 2 * i, v as u32);
                wr(a(3), 2 * i + 1, (v >> 32) as u32);
            }
            S_OK
        }
        // SetVideoMemoryReservation(node, group, bytes)
        19 => {
            if a(1) != 0 || a(2) > 1 {
                return E_INVALIDARG;
            }
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

/// DXGI_ADAPTER_DESC (m 8), DESC1 (10), DESC2 (11) or DESC3 (18) of sake's GPU or WARP:
/// Description[128] WCHAR, the ids, three SIZE_T memory sizes, the LUID; then Flags
/// (DXGI_ADAPTER_FLAG_SOFTWARE for WARP) and the two preemption granularities.
unsafe fn adapter_desc(p: u32, warp: bool, luid: u32, m: u32) {
    let at = 272 + 3 * ptr();
    let n = match m {
        8 => at + 8,
        10 => (at + 12).next_multiple_of(ptr()),
        _ => (at + 20).next_multiple_of(ptr()),
    };
    std::ptr::write_bytes(p as *mut u8, 0, n as usize);
    let (name, vendor, device, dedicated, shared) =
        if warp { ("Microsoft Basic Render Driver", 0x1414, 0x8c, 0, 0xffff_0000) } else { ("ippai WebGPU", 0x10de, 0x1c82, 512 << 20, 1024 << 20) };
    for (i, c) in name.encode_utf16().enumerate() {
        *((p + i as u32 * 2) as *mut u16) = c;
    }
    wr(p + 256, 0, vendor);
    wr(p + 256, 1, device);
    wrp(p + 272, 0, dedicated); // DedicatedVideoMemory (SIZE_T)
    wrp(p + 272, 2, shared); // SharedSystemMemory
    wr(p + at, 0, luid);
    if m != 8 {
        wr(p + at, 2, if warp { 2 } else { 0 });
    }
    if m == 11 || m == 18 {
        // (DXGI_GRAPHICS_PREEMPTION_PRIMITIVE_BOUNDARY, DXGI_COMPUTE_PREEMPTION_DISPATCH_BOUNDARY)
        wr(p + at, 3, 1);
        wr(p + at, 4, 1);
    }
}

/// The adapters in EnumAdapters' order: sake's GPU, then WARP
fn adapters(d: &D11) -> [u32; 2] {
    [d.adapter, d.warp]
}

/// Whether adapter object `id` is WARP's
fn is_warp(d: &D11, id: u32) -> bool {
    matches!(d.objs.get(&id).map(|o| &o.data), Some(Data::Adapter(true)))
}

/// An adapter's LUID (AdapterLuid): its place in the list, from 0x1000
fn adapter_luid(d: &D11, id: u32) -> u32 {
    0x1000 + is_warp(d, id) as u32
}

/// EnumAdapters(1), EnumAdapterByGpuPreference, EnumAdapterByLuid, EnumWarpAdapter: adapter
/// `id` (0: none such) as `riid` (None: IDXGIAdapter itself)
unsafe fn give_adapter(d: &mut D11, id: u32, riid: Option<u32>, out: u32) -> u32 {
    if out == 0 {
        return if id == 0 { DXGI_ERROR_NOT_FOUND } else { E_INVALIDARG };
    }
    if id == 0 {
        wrp(out, 0, 0);
        return DXGI_ERROR_NOT_FOUND;
    }
    // (a new object each time, as Windows': winref dxgienum's "same 0")
    let warp = is_warp(d, id);
    let n = d.new_obj(IF_ADAPTER, Data::Adapter(warp));
    let n = d.id_of(n);
    match riid {
        Some(riid) => {
            let hr = query_interface(d, n, IF_ADAPTER, riid, out);
            d.release(n);
            hr
        }
        None => {
            wrp(out, 0, d.addr_of(n));
            S_OK
        }
    }
}

unsafe fn factory_call(d: &mut D11, m: u32, _this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    match m {
        3..=5 => S_OK,
        6 => E_NOINTERFACE,
        7 | 12 => {
            // EnumAdapters / EnumAdapters1(index, out)
            let id = adapters(d).get(a(1) as usize).copied().unwrap_or(0);
            give_adapter(d, id, None, a(2))
        }
        8 => S_OK,
        9 => {
            wrp(a(1), 0, 0);
            S_OK
        }
        10 => {
            // CreateSwapChain(device, desc, out)
            let desc = a(2);
            if desc == 0 || a(3) == 0 {
                return E_INVALIDARG;
            }
            let p = swap_desc(desc);
            let sc = new_swapchain(d, p[11], p[0], p[1], p[4], (p[7], p[8]));
            wrp(a(3), 0, sc);
            if sc == 0 { E_FAIL } else { S_OK }
        }
        11 => DXGI_ERROR_UNSUPPORTED,
        13 => 1, // IsCurrent
        14 => 0,
        15 => {
            // CreateSwapChainForHwnd(device, hwnd, desc1, fullscreen desc, output, out)
            let desc = a(3);
            if desc == 0 || a(6) == 0 {
                return E_INVALIDARG;
            }
            let sc = new_swapchain(d, a(2), rd(desc, 0), rd(desc, 1), rd(desc, 2), (rd(desc, 4), rd(desc, 5)));
            wrp(a(6), 0, sc);
            if sc == 0 { E_FAIL } else { S_OK }
        }
        16 | 24 => DXGI_ERROR_UNSUPPORTED,
        17..=23 => E_NOTIMPL,
        25 => 0, // GetCreationFlags
        26 => {
            // EnumAdapterByLuid(luid, riid, out): the LUID two stack dwords on x86, one
            // register (its low half here) on x64
            let (high, riid, out) = if ptr() == 8 { (0, a(2), a(3)) } else { (a(2), a(3), a(4)) };
            let id = adapters(d).into_iter().find(|&x| high == 0 && adapter_luid(d, x) == a(1)).unwrap_or(0);
            if riid == 0 {
                return E_INVALIDARG;
            }
            if id == 0 {
                wrp(out, 0, 0);
                return DXGI_ERROR_NOT_FOUND; // (an unknown LUID, winref dxgienum)
            }
            give_adapter(d, id, Some(riid), out)
        }
        27 => {
            // EnumWarpAdapter(riid, out)
            if a(1) == 0 {
                return E_INVALIDARG;
            }
            give_adapter(d, d.warp, Some(a(1)), a(2))
        }
        28 => {
            // CheckFeatureSupport(feature, data, size): DXGI_FEATURE_PRESENT_ALLOW_TEARING, FALSE
            // (another size: DXGI_ERROR_INVALID_CALL, winref dxgienum)
            if a(1) != 0 || a(2) == 0 {
                return E_INVALIDARG;
            }
            if a(3) != 4 {
                return DXGI_ERROR_INVALID_CALL;
            }
            wr(a(2), 0, 0);
            S_OK
        }
        29 => {
            // EnumAdapterByGpuPreference(index, preference, riid, out): one order whatever the
            // preference (UNSPECIFIED, MINIMUM_POWER, HIGH_PERFORMANCE, and others alike as on
            // Windows: winref dxgienum's preference 3)
            if a(3) == 0 {
                return E_INVALIDARG;
            }
            let id = adapters(d).get(a(1) as usize).copied().unwrap_or(0);
            give_adapter(d, id, Some(a(3)), a(4))
        }
        30 => {
            // RegisterAdaptersChangedEvent(event, cookie): adapters never change here
            if a(1) == 0 || a(2) == 0 {
                return DXGI_ERROR_INVALID_CALL;
            }
            wr(a(2), 0, 1);
            S_OK
        }
        31 => S_OK, // UnregisterAdaptersChangedNotification
        _ => E_NOTIMPL,
    }
}

const MODES: [(u32, u32); 9] = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 960), (1280, 1024), (1366, 768), (1600, 900), (1920, 1080)];

unsafe fn output_call(d: &mut D11, m: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    match m {
        6 => parent(d, d.adapter, a(1), a(2)),
        7 => {
            // GetDesc: DeviceName[32] WCHAR, DesktopCoordinates, AttachedToDesktop, Rotation, Monitor
            // (92 bytes; 96 on x64, where the HMONITOR is 8: writing 96 on x86 overwrote what
            // follows, Unity 2019's factory pointer)
            let p = a(1);
            std::ptr::write_bytes(p as *mut u8, 0, (88 + ptr()) as usize);
            for (i, c) in "\\\\.\\DISPLAY1".encode_utf16().enumerate() {
                *((p + i as u32 * 2) as *mut u16) = c;
            }
            let (mut w, mut h) = (0, 0);
            crate::display::vnhost_display_size(&mut w, &mut h);
            wr(p + 64, 2, w as u32);
            wr(p + 64, 3, h as u32);
            wr(p + 64, 4, 1);
            wr(p + 64, 5, 1);
            wr(p + 64, 6, crate::display::HMONITOR);
            S_OK
        }
        8 => {
            // GetDisplayModeList(format, flags, count, modes)
            let (count, modes) = (a(3), a(4));
            if count == 0 {
                return E_INVALIDARG;
            }
            if modes != 0 {
                let n = rd(count, 0).min(MODES.len() as u32);
                for (i, (w, h)) in MODES.iter().take(n as usize).enumerate() {
                    let p = modes + i as u32 * 28;
                    for (k, v) in [*w, *h, 60, 1, a(1), 0, 0].into_iter().enumerate() {
                        wr(p, k, v);
                    }
                }
                wr(count, 0, n);
            } else {
                wr(count, 0, MODES.len() as u32);
            }
            S_OK
        }
        9 => {
            // FindClosestMatchingMode(mode, closest, device)
            let (want, out) = (a(1), a(2));
            let (w, h) = (rd(want, 0), rd(want, 1));
            let best = MODES.iter().min_by_key(|(x, y)| x.abs_diff(w) + y.abs_diff(h)).unwrap();
            let f = if rd(want, 4) != 0 { rd(want, 4) } else { 28 };
            for (k, v) in [best.0, best.1, 60, 1, f, 0, 0].into_iter().enumerate() {
                wr(out, k, v);
            }
            S_OK
        }
        10 => {
            // WaitForVBlank: a frame's wait, after the device's lock is let go (other threads'
            // calls go on meanwhile)
            *after = After::Sleep(16);
            S_OK
        }
        11..=18 => S_OK,
        19 => {
            // GetDisplayModeList1(format, flags, count, modes): DXGI_MODE_DESC1 (Stereo after)
            let (count, modes) = (a(3), a(4));
            if count == 0 {
                return E_INVALIDARG;
            }
            if modes != 0 {
                let n = rd(count, 0).min(MODES.len() as u32);
                for (i, (w, h)) in MODES.iter().take(n as usize).enumerate() {
                    let p = modes + i as u32 * 32;
                    for (k, v) in [*w, *h, 60, 1, a(1), 0, 0, 0].into_iter().enumerate() {
                        wr(p, k, v);
                    }
                }
                wr(count, 0, n);
            } else {
                wr(count, 0, MODES.len() as u32);
            }
            S_OK
        }
        20 => {
            // FindClosestMatchingMode1(mode1, closest1, device)
            let r = output_call(d, 9, a, after);
            wr(a(2), 7, 0);
            r
        }
        21 => E_NOTIMPL,                // GetDisplaySurfaceData1
        22 | 26 => E_NOTIMPL,           // DuplicateOutput(1): no desktop duplication
        23 => 0,                        // SupportsOverlays: FALSE
        24 | 25 => {
            // CheckOverlaySupport / CheckOverlayColorSpaceSupport(.., flags): none
            let out = if m == 24 { a(3) } else { a(4) };
            if out == 0 {
                return E_INVALIDARG;
            }
            wr(out, 0, 0);
            DXGI_ERROR_UNSUPPORTED
        }
        27 => {
            // GetDesc1: GetDesc's, then BitsPerColor, ColorSpace (G22 P709), the sRGB primaries
            // and white point, and luminances
            if a(1) == 0 {
                return E_INVALIDARG;
            }
            output_call(d, 7, a, after);
            let p = a(1) + 88 + ptr();
            let f = |x: f32| x.to_bits();
            for (k, v) in [8, 0, f(0.64), f(0.33), f(0.30), f(0.60), f(0.15), f(0.06), f(0.3127), f(0.3290), f(0.5), f(270.0), f(270.0)].into_iter().enumerate() {
                wr(p, k, v);
            }
            S_OK
        }
        28 => {
            // CheckHardwareCompositionSupport(flags): none
            if a(1) == 0 {
                return E_INVALIDARG;
            }
            wr(a(1), 0, 0);
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

unsafe fn swapchain_call(d: &mut D11, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let Some(Data::SwapChain(sc)) = d.objs.get(&this).map(|o| &o.data) else { return E_FAIL };
    let (device, hwnd, w, h, fmt, buffer, samples) = (sc.device, sc.hwnd, sc.w, sc.h, sc.fmt, sc.buffer, sc.samples);
    match m {
        3..=5 => S_OK,
        6 => parent(d, d.factory, a(1), a(2)),
        7 => parent(d, device, a(1), a(2)),
        8 | 22 => {
            // Present(sync, flags) / Present1(sync, flags, params)
            if a(2) & 1 != 0 {
                return S_OK; // DXGI_PRESENT_TEST
            }
            let mut rect = [0i32; 4];
            (d.calls().client_rect)(hwnd, rect.as_mut_ptr());
            let tex = match d.objs.get(&buffer).map(|o| &o.data) {
                Some(Data::Texture(t)) => t.gpu,
                _ => 0,
            };
            gpu::record(|q| q.cmd(&[gpu::PRESENT, tex, rect[0] as u32, rect[1] as u32, rect[2] as u32, rect[3] as u32, w, h, hwnd]));
            d.targets = None;
            if let Some(Data::SwapChain(sc)) = d.objs.get_mut(&this).map(|o| &mut o.data) {
                sc.presents += 1;
                // (always, the 1st, 60th and 600th: where the frames go, in every run's log)
                if matches!(sc.presents, 1 | 60 | 600) {
                    eprintln!("sake: d3d11 present {} of {w}x{h} to window {hwnd:#x} client {:?}", sc.presents, rect);
                }
            }
            *after = After::Present(hwnd);
            S_OK
        }
        9 => {
            // GetBuffer(index, riid, out)
            let iface = IF_TEX2D;
            query_interface(d, buffer, iface, a(2), a(3))
        }
        10 => S_OK,
        11 => {
            wr(a(1), 0, 0);
            wrp(a(2), 0, 0);
            S_OK
        }
        12 => {
            put_swap_desc(a(1), [w, h, 60, 1, fmt, 0, 0, samples.0, samples.1, 0x20, 1, hwnd, 1, 0, 0]);
            S_OK
        }
        13 => {
            // ResizeBuffers(count, w, h, format, flags)
            let mut rect = [0i32; 4];
            (d.calls().client_rect)(hwnd, rect.as_mut_ptr());
            let nw = if a(2) == 0 { (rect[2] - rect[0]).max(1) as u32 } else { a(2) };
            let nh = if a(3) == 0 { (rect[3] - rect[1]).max(1) as u32 } else { a(3) };
            let nf = if a(4) == 0 { fmt } else { a(4) };
            let nb = new_backbuffer(d, nw, nh, nf, samples);
            let nb = d.id_of(nb);
            if let Some(Data::SwapChain(sc)) = d.objs.get_mut(&this).map(|o| &mut o.data) {
                sc.w = nw;
                sc.h = nh;
                sc.fmt = nf;
                sc.buffer = nb;
            }
            d.release(buffer);
            S_OK
        }
        14 => S_OK,
        15 => {
            d.add_ref(d.output);
            wrp(a(1), 0, d.addr_of(d.output));
            S_OK
        }
        16 => DXGI_ERROR_UNSUPPORTED,
        17 => {
            let n = match d.objs.get(&this).map(|o| &o.data) {
                Some(Data::SwapChain(sc)) => sc.presents,
                _ => 0,
            };
            wr(a(1), 0, n);
            S_OK
        }
        18 => {
            let p = a(1);
            for (k, v) in [w, h, fmt, 0, samples.0, samples.1, 0x20, 1, 0, 0, 0, 0].into_iter().enumerate() {
                wr(p, k, v);
            }
            S_OK
        }
        19 => {
            for (k, v) in [60, 1, 0, 0, 1].into_iter().enumerate() {
                wr(a(1), k, v);
            }
            S_OK
        }
        20 => {
            wrp(a(1), 0, hwnd);
            S_OK
        }
        23 => 0,
        _ => DXGI_ERROR_INVALID_CALL,
    }
}

// ---------------------------------------------------------------------------------------------
// ID3D11Device

unsafe fn device_call(d: &mut D11, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    let _ = this;
    match m {
        3 => {
            // CreateBuffer(desc, init, out)
            let desc = a(1);
            if desc == 0 {
                return E_INVALIDARG;
            }
            // D3D11_BUFFER_DESC { ByteWidth, Usage, BindFlags, CPUAccessFlags, MiscFlags, StructureByteStride }
            let (size, usage, bind, cpu, misc) = (rd(desc, 0), rd(desc, 1), rd(desc, 2), rd(desc, 3), rd(desc, 4));
            let stride = match check::buffer(size, usage, bind, cpu, misc, rd(desc, 5), a(2) != 0) {
                Ok(s) => s,
                Err(hr) => return hr,
            };
            if a(2) != 0 && rdp(a(2), 0) == 0 {
                return E_INVALIDARG;
            }
            if a(3) == 0 {
                return S_FALSE;
            }
            let mem = d.guest_alloc(size.next_multiple_of(16));
            if mem == 0 {
                return E_OUTOFMEMORY;
            }
            std::ptr::write_bytes(mem as *mut u8, 0, size as usize);
            if a(2) != 0 {
                std::ptr::copy_nonoverlapping(rdp(a(2), 0) as *const u8, mem as *mut u8, size as usize);
            }
            let o = d.new_obj(IF_BUFFER, Data::Buffer(Buffer { size, usage, bind, cpu, misc, stride, mem, gpu: 0, dirty: true, stream: false, shadow: Vec::new() }));
            if d.trace && size <= 256 {
                let f: Vec<f32> = (0..size / 4).map(|i| *((mem + 4 * i) as *const f32)).collect();
                eprintln!("d3d11: new buffer {o} bind {bind:#x} usage {usage} = {f:?}");
            }
            wrp(a(3), 0, o);
            S_OK
        }
        4..=6 => {
            // CreateTexture1D/2D/3D(desc, init, out)
            let desc = a(1);
            if desc == 0 {
                return E_INVALIDARG;
            }
            // D3D11_TEXTURE1D/2D/3D_DESC
            let r = |i| rd(desc, i);
            let t = match m {
                4 => check::Tex { dim: 1, w: r(0), h: 1, layers: r(2), mips: r(1), fmt: r(3), samples: 1, quality: 0, usage: r(4), bind: r(5), cpu: r(6), misc: r(7) },
                5 => check::Tex { dim: 2, w: r(0), h: r(1), layers: r(3), mips: r(2), fmt: r(4), samples: r(5), quality: r(6), usage: r(7), bind: r(8), cpu: r(9), misc: r(10) },
                _ => check::Tex { dim: 3, w: r(0), h: r(1), layers: r(2), mips: r(3), fmt: r(4), samples: 1, quality: 0, usage: r(5), bind: r(6), cpu: r(7), misc: r(8) },
            };
            let mips = match check::texture(&t, a(2) != 0) {
                Ok(n) => n,
                Err(hr) => {
                    if d.trace {
                        eprintln!("d3d11: texture {t:?} refused");
                    }
                    return hr;
                }
            };
            let check::Tex { dim, w, h, layers, fmt, usage, bind, misc, .. } = t;
            // (sizes its memory cannot hold: before anything multiplies them in u32)
            let (d3, n, s) = if dim == 3 { (layers, 1, 1) } else { (1, layers, t.samples) };
            let bytes = format::texture_bytes(fmt, w, h, d3, n, mips, s).max(format::page_texture_bytes(fmt, w, h, d3, n, mips, s));
            if bytes > format::MAX_TEXTURE_BYTES {
                if d.trace {
                    eprintln!("d3d11: texture {t:?} of {bytes} bytes refused");
                }
                return E_OUTOFMEMORY;
            }
            // (initial data with a missing level)
            if a(2) != 0 {
                let n = mips * if dim == 3 { 1 } else { layers };
                if (0..n).any(|sub| rdp(a(2) + sub * (ptr() + 8), 0) == 0) {
                    return E_INVALIDARG;
                }
            }
            if a(3) == 0 {
                return S_FALSE;
            }
            let o = new_texture(d, dim, w, h, layers, mips, fmt, usage, bind, misc & 4 != 0, t.samples);
            let id = d.id_of(o);
            if let Some(Data::Texture(x)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
                (x.cpu, x.misc, x.samples) = (t.cpu, misc, (t.samples, t.quality));
                // (ANGLE makes a staging texture for each upload and maps it once to write)
                x.pristine = usage == 3 && a(2) == 0;
            }
            if a(2) != 0 {
                let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) else { return E_FAIL };
                for sub in 0..t.mips * if dim == 3 { 1 } else { t.layers } {
                    // D3D11_SUBRESOURCE_DATA { pSysMem, SysMemPitch, SysMemSlicePitch }
                    let init = a(2) + sub * (ptr() + 8);
                    upload_texture(t, sub, rdp(init, 0) as usize, rd(init + ptr(), 0), rd(init + ptr(), 1), None);
                }
            }
            wrp(a(3), 0, o);
            S_OK
        }
        7..=10 => {
            let hr = create_view(d, m, a(1), a(2), a(3));
            // (a view refused: the out pointer cleared, as Windows does)
            if hr & 0x8000_0000 != 0 && a(3) != 0 {
                wrp(a(3), 0, 0);
            }
            hr
        }
        11 => {
            // CreateInputLayout(elems, n, code, len, out)
            let mut v = Vec::new();
            let mut raw = Vec::new();
            let mut next = [0u32; 16];
            for i in 0..a(2) {
                // D3D11_INPUT_ELEMENT_DESC { SemanticName, then six UINTs }
                let e = a(1) + i * (ptr() + 24);
                let name = std::ffi::CStr::from_ptr(rdp(e, 0) as *const std::ffi::c_char).to_string_lossy().into_owned();
                let f = e + ptr();
                raw.push((rd(f, 0), rd(f, 1), rd(f, 2), rd(f, 4), rd(f, 5)));
                let (index, fmt, slot, off, class) = (rd(f, 0), rd(f, 1), rd(f, 2) & 15, rd(f, 3), rd(f, 4));
                let off = if off == 0xffff_ffff { next[slot as usize] } else { off };
                let size = format::vertex_format(fmt).map_or(4, vertex_size);
                next[slot as usize] = off + size;
                v.push(Elem { name, index, fmt, slot, offset: off, instance: class == 1 });
            }
            let code = if a(3) == 0 { &[][..] } else { std::slice::from_raw_parts(a(3) as *const u8, a(4) as usize) };
            let names: Vec<(&str, u32)> = v.iter().map(|e| (e.name.as_str(), e.index)).collect();
            let slots: Vec<(u32, bool)> = v.iter().map(|e| (e.slot, e.instance)).collect();
            let elems: Vec<_> = v.iter().zip(&raw).map(|(e, r)| (e.name.as_str(), r.0, r.1, r.2, r.3, r.4)).collect();
            if let Err(hr) = check::input_elements(&elems).and_then(|()| check::input_layout(a(1) != 0, &names, &slots, code)) {
                wrp(a(5), 0, 0);
                return hr;
            }
            if a(5) == 0 {
                return S_FALSE;
            }
            if d.trace {
                for e in &v {
                    eprintln!("d3d11:   layout {}{} format {} slot {} offset {}{}", e.name, e.index, e.fmt, e.slot, e.offset, if e.instance { " instance" } else { "" });
                }
            }
            let o = d.new_obj(IF_LAYOUT, Data::Layout(v));
            wrp(a(5), 0, o);
            S_OK
        }
        12 | 13 | 15..=18 => {
            // Create*Shader(code, len, linkage, out)
            let code = std::slice::from_raw_parts(a(1) as *const u8, a(2) as usize).to_vec();
            let iface = match m {
                12 => IF_VS,
                13 => IF_GS,
                15 => IF_PS,
                16 => IF_HS,
                17 => IF_DS,
                _ => IF_CS,
            };
            if let Err(hr) = check::shader(&code, m, d.level) {
                if a(4) != 0 {
                    wrp(a(4), 0, 0);
                }
                return hr;
            }
            if a(4) == 0 {
                return S_FALSE;
            }
            let o = d.new_obj(iface, Data::Shader(Shader { code, pixel: iface == IF_PS, wgsl: HashMap::new() }));
            wrp(a(4), 0, o);
            S_OK
        }
        14 => {
            // CreateGeometryShaderWithStreamOutput(code, len, decls, n, strides, nstrides, stream,
            // linkage, out): a geometry or vertex shader's bytecode (or none: SO of the VS)
            if a(1) != 0 {
                let code = std::slice::from_raw_parts(a(1) as *const u8, a(2) as usize);
                if check::shader(code, 13, d.level).is_err() && check::shader(code, 12, d.level).is_err() {
                    if a(9) != 0 {
                        wrp(a(9), 0, 0);
                    }
                    return E_INVALIDARG;
                }
            }
            let o = d.new_obj(IF_GS, Data::Other);
            wrp(a(9), 0, o);
            S_OK
        }
        19 => {
            let o = d.new_obj(IF_LINKAGE, Data::Other);
            wrp(a(1), 0, o);
            S_OK
        }
        20..=23 => {
            // Create{Blend, DepthStencil, Rasterizer, Sampler}State(desc, out)
            let (iface, n) = match m {
                20 => (IF_BLEND, 66),
                21 => (IF_DEPTH, 13),
                22 => (IF_RASTER, 10),
                _ => (IF_SAMPLER, 13),
            };
            if a(1) == 0 {
                return E_INVALIDARG;
            }
            let mut desc: Vec<u32> = (0..n).map(|i| rd(a(1), i)).collect();
            if normalize_state(m, &mut desc).is_none() {
                return E_INVALIDARG;
            }
            // (the newer descriptions' fields at the end: a blend state's LogicOpEnable and
            // LogicOp per target, a rasterizer state's ForcedSampleCount and ConservativeRaster)
            desc.resize(state_len(iface), 0);
            new_state(d, iface, desc, a(2))
        }
        24 | 25 => {
            // CreateQuery/CreatePredicate(desc { Query, MiscFlags }, out): the predicates are
            // OCCLUSION_PREDICATE and SO_OVERFLOW_PREDICATE, PREDICATEHINT only for them
            if a(1) == 0 {
                return E_INVALIDARG;
            }
            let (t, misc) = (rd(a(1), 0), rd(a(1), 1));
            let predicate = matches!(t, 5 | 7);
            if t > 15 || misc & !1 != 0 || misc != 0 && !predicate || m == 25 && !predicate {
                return E_INVALIDARG;
            }
            if a(2) == 0 {
                return S_FALSE;
            }
            let o = d.new_obj(IF_QUERY, Data::Query(t, 0, 0));
            wrp(a(2), 0, o);
            S_OK
        }
        26 => E_INVALIDARG,
        27 => deferred::create(d, a(1), a(2)),
        28 => E_NOTIMPL,
        29 => {
            // CheckFormatSupport(format, out)
            // (Windows' answers: what the page does with a format is its own business)
            if a(2) == 0 {
                return E_INVALIDARG;
            }
            let bits = check::support(a(1));
            wr(a(2), 0, bits.unwrap_or(0));
            if bits.is_some() { S_OK } else { E_FAIL }
        }
        30 => {
            // CheckMultisampleQualityLevels(format, count, out)
            if a(3) == 0 {
                return E_INVALIDARG;
            }
            wr(a(3), 0, check::quality_levels(a(1), a(2)));
            S_OK
        }
        31 => {
            std::ptr::write_bytes(a(1) as *mut u8, 0, 12);
            S_OK
        }
        32 => E_INVALIDARG,
        33 => {
            // CheckFeatureSupport(feature, data, size): nothing optional, but for what WARP
            // answers too: concurrent creates (THREADING) and compute on 10.x (the device is
            // free-threaded here and compute runs at every level)
            // FORMAT_SUPPORT(2): { InFormat, OutFormatSupport(2) }, Windows' answers
            if a(1) == 2 || a(1) == 3 {
                if a(2) == 0 || a(3) != 8 {
                    return E_INVALIDARG;
                }
                let f = rd(a(2), 0);
                let bits = if a(1) == 2 { check::support(f) } else { check::support2(f) };
                wr(a(2), 1, bits.unwrap_or(0));
                return if bits.is_some() { S_OK } else { E_FAIL };
            }
            std::ptr::write_bytes(a(2) as *mut u8, 0, a(3) as usize);
            if (a(1) == 0 || a(1) == 4) && a(3) >= 4 {
                wr(a(2), 0, 1);
            }
            S_OK
        }
        34..=36 => private_data(d, m - 30, this, a),
        37 => d.level,
        38 => 0,
        39 => S_OK,
        40 => {
            d.add_ref(d.context);
            wrp(a(1), 0, d.addr_of(d.context));
            S_OK
        }
        41 => S_OK,
        42 => 0,
        // ID3D11Device1/2 (Direct2D uses these)
        43 | 50 | 61 => device_call(d, 40, this, a), // GetImmediateContext1/2/3
        44 | 51 | 62 => deferred::create(d, a(1), a(2)), // CreateDeferredContext1/2/3
        45 => {
            // CreateBlendState1(desc, out): render targets of 10 dwords (LogicOpEnable, LogicOp)
            // to the 8 of D3D11_BLEND_DESC
            if a(1) == 0 {
                return E_INVALIDARG;
            }
            let mut desc = vec![rd(a(1), 0), rd(a(1), 1)];
            for t in 0..8 {
                let b = 2 + t * 10;
                desc.push(rd(a(1), b));
                desc.extend((3..9).map(|i| rd(a(1), b + i - 1)));
                desc.push(rd(a(1), b + 9));
            }
            if normalize_state(20, &mut desc).is_none() {
                return E_INVALIDARG;
            }
            // (winref d3d11copy: a logic op with IndependentBlendEnable is refused)
            let logic = |t: usize| rd(a(1), 2 + t * 10 + 1) != 0;
            if desc[1] != 0 && (0..8).any(logic) {
                return E_INVALIDARG;
            }
            for t in 0..8 {
                let src = if desc[1] != 0 { t } else { 0 };
                let on = rd(a(1), 2 + src * 10 + 1);
                desc.extend([on, if on != 0 { rd(a(1), 2 + src * 10 + 8) } else { 0 }]);
            }
            new_state(d, IF_BLEND, desc, a(2))
        }
        46 | 56 => {
            // CreateRasterizerState1/2: D3D11_RASTERIZER_DESC with ForcedSampleCount (and
            // ConservativeRaster) at its end
            if a(1) == 0 {
                return E_INVALIDARG;
            }
            let mut desc: Vec<u32> = (0..if m == 46 { 11 } else { 12 }).map(|i| rd(a(1), i)).collect();
            // ForcedSampleCount 0, 1, 4 or 8, and not 1 with MultisampleEnable (winref
            // d3d11copy on Windows: 3, 16 and 32 are refused).
            // ConservativeRaster ON: none on the page (WebGPU has none; WARP refuses it too)
            if !matches!(desc[10], 0 | 1 | 4 | 8) || (desc[10] == 1 && desc[8] != 0) || desc.get(11).is_some_and(|&c| c != 0) {
                return E_INVALIDARG;
            }
            if normalize_state(22, &mut desc).is_none() {
                return E_INVALIDARG;
            }
            desc.resize(state_len(IF_RASTER), 0);
            new_state(d, IF_RASTER, desc, a(2))
        }
        47 => {
            // CreateDeviceContextState(flags, levels, count, sdk, iid, level_out, state_out)
            let level = if a(2) != 0 { (0..a(3) as usize).map(|i| rd(a(2), i)).find(|&l| l <= 0xb000) } else { None };
            let Some(level) = level else {
                wr(a(6), 0, 0);
                wrp(a(7), 0, 0);
                return E_INVALIDARG;
            };
            wr(a(6), 0, level);
            // (from now on the device answers for its D3D10 interfaces too, as Windows does)
            d.ten_faces = true;
            if a(7) == 0 {
                return S_FALSE;
            }
            let o = d.new_obj(IF_LINKAGE, Data::CtxState(Some(Box::new(default_state()))));
            wrp(a(7), 0, o);
            S_OK
        }
        48 | 49 => E_NOTIMPL,
        52 => 0,
        // ID3D11Device3: the *1 / *2 descriptions extend the old ones at their ends (texture
        // layout, conservative rasterization, plane slices of planar formats, a query's
        // context type), which this device does not have: the old creation reads its prefix
        54 => device_call(d, 5, this, a),  // CreateTexture2D1
        55 => device_call(d, 6, this, a),  // CreateTexture3D1
        57 => device_call(d, 7, this, a),  // CreateShaderResourceView1
        58 => device_call(d, 8, this, a),  // CreateUnorderedAccessView1
        59 => device_call(d, 9, this, a),  // CreateRenderTargetView1
        60 => device_call(d, 24, this, a), // CreateQuery1
        // Write / ReadFromSubresource: only for textures mapped with a CPU-visible layout,
        // which none here is
        63 | 64 => 0,
        // ID3D11Device4: the device is never removed (cookie 0 is valid)
        65 => {
            if a(1) == 0 || a(2) == 0 {
                return E_INVALIDARG;
            }
            wr(a(2), 0, 0);
            S_OK
        }
        66 => 0,
        // ID3D11Device5: fences
        67 | 68 => E_NOTIMPL,
        53 => {
            // CheckMultisampleQualityLevels1(format, count, flags, out)
            if a(4) == 0 {
                return E_INVALIDARG;
            }
            wr(a(4), 0, check::quality_levels(a(1), a(2)));
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

fn vertex_size(f: u32) -> u32 {
    match f {
        0 | 4 | 8 => 4,
        1 | 5 | 9 => 8,
        2 | 6 | 10 => 12,
        3 | 7 | 11 => 16,
        17 | 19 | 21 | 23 | 25 => 8,
        26 | 27 | 30 | 31 => 2,
        _ => 4,
    }
}

/// Get/Set/SetInterface PrivateData as methods 4/5/6 of a device child (`m` in that numbering).
unsafe fn private_data(d: &mut D11, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    // (as Windows: Get/Set/SetInterface on a store per object; an interface stored holds a
    // reference to the object, given out again with one more by Get)
    let mut guid = [0u8; 16];
    if a(1) == 0 {
        return E_INVALIDARG;
    }
    std::ptr::copy_nonoverlapping(a(1) as *const u8, guid.as_mut_ptr(), 16);
    let iface_obj = if m == 6 { d.id_of(a(2)) } else { 0 };
    let Some(o) = d.objs.get_mut(&this) else { return E_FAIL };
    let found = o.private.iter().position(|x| x.0 == guid);
    match m {
        4 => {
            // GetPrivateData(guid, size*, data)
            if a(2) == 0 {
                return E_INVALIDARG;
            }
            let Some(i) = found else {
                wr(a(2), 0, 0);
                return DXGI_ERROR_NOT_FOUND;
            };
            let (v, held) = (o.private[i].1.clone(), o.private[i].2);
            let room = rd(a(2), 0);
            wr(a(2), 0, v.len() as u32);
            if a(3) == 0 {
                return S_OK;
            }
            if room < v.len() as u32 {
                return 0x887a_0003; // DXGI_ERROR_MORE_DATA
            }
            if held != 0 {
                d.add_ref(held);
            }
            std::ptr::copy_nonoverlapping(v.as_ptr(), a(3) as *mut u8, v.len());
            S_OK
        }
        _ => {
            // SetPrivateData(guid, size, data) / SetPrivateDataInterface(guid, object)
            let (bytes, held) = if m == 5 {
                if a(3) == 0 {
                    // (no data: the entry goes, if there is one)
                    let Some(i) = found else { return S_FALSE };
                    let old = o.private.remove(i).2;
                    if old != 0 {
                        d.release(old);
                    }
                    return S_OK;
                }
                (std::slice::from_raw_parts(a(3) as *const u8, a(2) as usize).to_vec(), 0)
            } else {
                let mut b = a(2).to_le_bytes().to_vec();
                b.resize(ptr() as usize, 0);
                (b, iface_obj)
            };
            let old = found.map_or(0, |i| o.private.remove(i).2);
            o.private.push((guid, bytes, held));
            if held != 0 {
                d.add_ref(held);
            }
            if old != 0 {
                d.release(old);
            }
            S_OK
        }
    }
}

/// Create{ShaderResource, UnorderedAccess, RenderTarget, DepthStencil}View(resource, desc, out)
unsafe fn create_view(d: &mut D11, m: u32, res: u32, desc: u32, out: u32) -> u32 {
    let rid = d.id_of(res);
    let kind = [check::SRV, check::UAV, check::RTV, check::DSV][m as usize - 7];
    // (the description's words: SRV 6, RTV and UAV 5, DSV 6)
    let words: Option<Vec<u32>> = (desc != 0).then(|| (0..if kind == check::RTV || kind == check::UAV { 5 } else { 6 }).map(|i| rd(desc, i)).collect());
    if let Some(Data::Buffer(b)) = d.objs.get(&rid).map(|o| &o.data) {
        let bv = match check::buffer_view(kind, b.size, b.bind, b.misc, b.stride, words.as_deref()) {
            Ok(v) => v,
            Err(hr) => return hr,
        };
        if out == 0 {
            return S_FALSE;
        }
        if kind == check::SRV {
            return create_buffer_srv(d, rid, bv, out);
        }
        // (buffer UAVs and render targets: kept, not drawn with)
        let o = d.new_obj([IF_SRV, IF_UAV, IF_RTV, IF_DSV][m as usize - 7], Data::Other);
        wrp(out, 0, o);
        return S_OK;
    }
    let Some(Data::Texture(t)) = d.objs.get(&rid).map(|o| &o.data) else { return E_INVALIDARG };
    let r = check::Res { dim: t.dim, fmt: t.fmt, mips: t.mips, layers: t.layers, bind: t.bind, cube: t.cube, samples: t.samples.0, backbuffer: t.backbuffer };
    let c = match check::view(kind, &r, words.as_deref()) {
        Ok(c) => c,
        Err(hr) => {
            if d.trace {
                eprintln!("d3d11: view {kind} of {r:?} as {words:?} refused");
            }
            return hr;
        }
    };
    let v = View { res: rid, fmt: c.desc[0], desc: c.desc, mip: c.mip, mips: c.mips, layer: c.layer, layers: c.layers, buf: None };
    if out == 0 {
        return S_FALSE;
    }
    d.bind(rid);
    let o = d.new_obj([IF_SRV, IF_UAV, IF_RTV, IF_DSV][m as usize - 7], Data::View(v));
    wrp(out, 0, o);
    S_OK
}

/// CreateShaderResourceView of a buffer (checked: `bv`): a view of a one-row texture with the
/// elements (at most 8192, WebGPU's width limit; raw and structured views as R32_UINT elements)
unsafe fn create_buffer_srv(d: &mut D11, rid: u32, bv: check::BufView, out: u32) -> u32 {
    let fmt = if matches!(bv.fmt, 0 | 39) { 42 } else { bv.fmt };
    // (the 96-bit formats have no texels on the page: a view of nothing, as Windows takes it)
    let f = format::info(fmt).filter(|f| f.block == 1).unwrap_or(format::Format { page: format::P_R32UI, bpp: 4, block: 1, conv: 0, no_alpha: false });
    let n = (bv.bytes / f.bpp.max(1)).clamp(1, 8192);
    let tex = new_texture(d, 2, n, 1, 1, 1, fmt, 0, 8, false, 1);
    let tid = d.id_of(tex);
    // (the view's own texture and the buffer: held privately, see `destroy`)
    d.bind(tid);
    d.release(tid);
    d.bind(rid);
    let buf = Some(BufSrc { buf: rid, off: bv.offset, bytes: n * f.bpp, last: Vec::new() });
    let v = View { res: tid, fmt: bv.fmt, desc: bv.desc, mip: 0, mips: 1, layer: 0, layers: 1, buf };
    let o = d.new_obj(IF_SRV, Data::View(v));
    wrp(out, 0, o);
    S_OK
}

/// A buffer view's texture, brought up to date with its buffer.
fn buffer_view_sync(d: &mut D11, view: u32) {
    let Some(Data::View(v)) = d.objs.get(&view).map(|o| &o.data) else { return };
    let Some(b) = &v.buf else { return };
    let Some(Data::Buffer(buf)) = d.objs.get(&b.buf).map(|o| &o.data) else { return };
    if buf.mem == 0 {
        return;
    }
    let now = unsafe { std::slice::from_raw_parts((buf.mem + b.off) as *const u8, b.bytes as usize) };
    if now == b.last.as_slice() {
        return;
    }
    let now = now.to_vec();
    if d.trace {
        eprintln!("d3d11: buffer view {view} <- {} bytes of buffer {}", now.len(), b.buf);
    }
    let (res, bytes) = (v.res, b.bytes);
    if let Some(Data::Texture(t)) = d.objs.get_mut(&res).map(|o| &mut o.data) {
        unsafe { upload_texture(t, 0, now.as_ptr() as usize, bytes, 0, None) };
    }
    if let Some(Data::View(v)) = d.objs.get_mut(&view).map(|o| &mut o.data) {
        if let Some(b) = v.buf.as_mut() {
            b.last = now;
        }
    }
}

/// Methods of device children: views, shaders, states, resources.
unsafe fn child_call(d: &mut D11, iface: u32, m: u32, this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    match m {
        3 => {
            d.add_ref(d.device);
            wrp(a(1), 0, d.addr_of(d.device));
            return S_OK;
        }
        4..=6 => return private_data(d, m, this, a),
        _ => {}
    }
    let data = d.objs.get(&this).map(|o| &o.data);
    match (iface, m, data) {
        // resources: GetType, SetEvictionPriority, GetEvictionPriority, GetDesc
        (IF_BUFFER | IF_TEX1D | IF_TEX2D | IF_TEX3D, 7, _) => {
            wr(a(1), 0, iface - IF_BUFFER + 1);
            S_OK
        }
        (IF_BUFFER | IF_TEX1D | IF_TEX2D | IF_TEX3D, 8, _) => S_OK,
        (IF_BUFFER | IF_TEX1D | IF_TEX2D | IF_TEX3D, 9, _) => 0,
        (IF_BUFFER, 10, Some(Data::Buffer(b))) => {
            for (i, v) in [b.size, b.usage, b.bind, b.cpu, b.misc, b.stride].into_iter().enumerate() {
                wr(a(1), i, v);
            }
            S_OK
        }
        (IF_TEX1D | IF_TEX2D | IF_TEX3D, 10, Some(Data::Texture(t))) => {
            let (cpu, misc) = (t.cpu, t.misc);
            let v: Vec<u32> = match iface {
                IF_TEX1D => vec![t.w, t.mips, t.layers, t.fmt, t.usage, t.bind, cpu, misc],
                IF_TEX2D => vec![t.w, t.h, t.mips, t.layers, t.fmt, t.samples.0, t.samples.1, t.usage, t.bind, cpu, misc],
                _ => vec![t.w, t.h, t.layers, t.mips, t.fmt, t.usage, t.bind, cpu, misc],
            };
            for (i, x) in v.into_iter().enumerate() {
                wr(a(1), i, x);
            }
            S_OK
        }
        // views: GetResource, GetDesc
        (IF_SRV | IF_RTV | IF_DSV | IF_UAV, 7, Some(Data::View(v))) => {
            let r = v.buf.as_ref().map_or(v.res, |b| b.buf);
            d.add_ref(r);
            wrp(a(1), 0, d.addr_of(r));
            S_OK
        }
        (IF_SRV | IF_RTV | IF_DSV | IF_UAV, 8, Some(Data::View(v))) => {
            let w = v.desc.clone();
            for (i, x) in w.into_iter().enumerate() {
                wr(a(1), i, x);
            }
            S_OK
        }
        // states: GetDesc (GetDesc1/2: the newer descriptions); a NULL one: nothing written (the
        // guest's address 0 is the host's memory's start, not a fault)
        (IF_BLEND | IF_DEPTH | IF_RASTER | IF_SAMPLER, 7..=9, _) if a(1) == 0 => 0,
        (IF_BLEND | IF_DEPTH | IF_RASTER | IF_SAMPLER, 7, Some(Data::State(s))) => {
            let n = match iface {
                IF_BLEND => 66,
                IF_RASTER => 10,
                _ => s.len(),
            };
            std::ptr::copy_nonoverlapping(s.as_ptr(), a(1) as *mut u32, n.min(s.len()));
            0
        }
        (IF_RASTER, 8 | 9, Some(Data::State(s))) => {
            std::ptr::copy_nonoverlapping(s.as_ptr(), a(1) as *mut u32, if m == 8 { 11 } else { 12 });
            0
        }
        (IF_BLEND, 8, Some(Data::State(s))) => {
            // D3D11_BLEND_DESC1: targets of BlendEnable, LogicOpEnable, the 6 factors and
            // operations, LogicOp, RenderTargetWriteMask
            wr(a(1), 0, s[0]);
            wr(a(1), 1, s[1]);
            for t in 0..8 {
                let (b, o) = (2 + t * 10, 2 + t * 8);
                wr(a(1), b, s[o]);
                wr(a(1), b + 1, s[66 + t * 2]);
                for k in 0..6 {
                    wr(a(1), b + 2 + k, s[o + 1 + k]);
                }
                // (a target without a logic op reads back NOOP, winref d3d11copy)
                wr(a(1), b + 8, if s[66 + t * 2] != 0 { s[67 + t * 2] } else { 4 });
                wr(a(1), b + 9, s[o + 7]);
            }
            0
        }
        // queries: GetDataSize, GetDesc
        (IF_QUERY, 7, Some(Data::Query(t, ..))) => query_size(*t),
        (IF_QUERY, 8, Some(Data::Query(t, ..))) => {
            wr(a(1), 0, *t);
            wr(a(1), 1, 0);
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

fn query_size(t: u32) -> u32 {
    match t {
        0 => 4,     // EVENT
        1 => 8,     // OCCLUSION
        2 => 8,     // TIMESTAMP
        3 => 16,    // TIMESTAMP_DISJOINT
        4 => 88,    // PIPELINE_STATISTICS
        5 => 4,     // OCCLUSION_PREDICATE
        // SO_OVERFLOW_PREDICATE(_STREAM0..3) at 7, 9, 11, 13, 15; SO_STATISTICS(_STREAM0..3)
        // at 6, 8, 10, 12, 14
        7..=15 if t % 2 == 1 => 4,
        _ => 16,
    }
}

// ---------------------------------------------------------------------------------------------
// ID3D11DeviceContext

/// Sets `n` slots from `start` of a slot array to the objects in `p` (guest addresses).
unsafe fn set_slots(d: &mut D11, slots: &mut [u32], start: u32, n: u32, p: u32) {
    for i in 0..n {
        let s = (start + i) as usize;
        if s >= slots.len() {
            break;
        }
        let new = if p == 0 { 0 } else { d.id_of(rdp(p, i as usize)) };
        let old = slots[s];
        slots[s] = new;
        d.rebind(new, old);
    }
}

/// Gets `n` slots from `start` into `p` (with references).
unsafe fn get_slots(d: &mut D11, slots: &[u32], start: u32, n: u32, p: u32) {
    // (no array: nothing asked for, no reference given)
    if p == 0 {
        return;
    }
    for i in 0..n {
        let id = slots.get((start + i) as usize).copied().unwrap_or(0);
        if id != 0 {
            d.add_ref(id);
        }
        wrp(p, i as usize, d.addr_of(id));
    }
}

unsafe fn set_one(d: &mut D11, slot: &mut u32, addr: u32) {
    let new = d.id_of(addr);
    let old = *slot;
    *slot = new;
    d.rebind(new, old);
}

unsafe fn get_one(d: &mut D11, id: u32, out: u32) {
    // (no out pointer: nothing asked for, no reference given)
    if out == 0 {
        return;
    }
    if id != 0 {
        d.add_ref(id);
    }
    wrp(out, 0, d.addr_of(id));
}

unsafe fn context_call(d: &mut D11, m: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let mut st = std::mem::replace(&mut d.st, default_state());
    let r = context_call2(d, &mut st, m, a, after);
    d.st = st;
    r
}

unsafe fn context_call2(d: &mut D11, st: &mut State, m: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    match m {
        3 => {
            d.add_ref(d.device);
            wrp(a(1), 0, d.addr_of(d.device));
        }
        4..=6 => {
            let ctx = d.context;
            return private_data(d, m, ctx, a);
        }
        7 | 16 => set_slots(d, &mut st.cbs[(m == 16) as usize], a(1), a(2), a(3)),
        8 | 25 => {
            set_slots(d, &mut st.srvs[(m == 8) as usize], a(1), a(2), a(3));
            hazards::after_srvs(d, st, (m == 8) as usize, a(1), a(2));
        }
        9 => set_one(d, &mut st.ps, a(1)),
        10 | 26 => set_slots(d, &mut st.samplers[(m == 10) as usize], a(1), a(2), a(3)),
        11 => set_one(d, &mut st.vs, a(1)),
        12 => draw(d, st, true, a(1), 1, a(2), a(3) as i32, 0),
        13 => draw(d, st, false, a(1), 1, a(2), 0, 0),
        14 => return map(d, a(1), a(2), a(3), a(5), false, after),
        15 => unmap(d, a(1), a(2)),
        17 => set_one(d, &mut st.layout, a(1)),
        18 => {
            // IASetVertexBuffers(start, n, buffers, strides, offsets)
            for i in 0..a(2) {
                let s = (a(1) + i) as usize;
                if s >= 32 {
                    break;
                }
                let new = if a(3) == 0 { 0 } else { d.id_of(rdp(a(3), i as usize)) };
                let old = st.vbs[s].0;
                st.vbs[s] = (new, if a(4) != 0 { rd(a(4), i as usize) } else { 0 }, if a(5) != 0 { rd(a(5), i as usize) } else { 0 });
                d.rebind(new, old);
            }
            hazards::after_buffers(d, st);
        }
        19 => {
            let new = d.id_of(a(1));
            let old = st.ib.0;
            st.ib = (new, a(2), a(3));
            d.rebind(new, old);
        }
        // (no instances: nothing drawn)
        20 => draw(d, st, true, a(1), a(2), a(3), a(4) as i32, a(5)),
        21 => draw(d, st, false, a(1), a(2), a(3), 0, a(4)),
        24 => st.topology = a(1),
        27 | 28 => {
            // Begin, End: an event or a timestamp has no Begin
            let q = d.id_of(a(1));
            d.ticks += 1;
            let now = d.ticks;
            if let Some(Data::Query(t, state, stamp)) = d.objs.get_mut(&q).map(|o| &mut o.data) {
                if m == 28 {
                    *state = 2;
                    *stamp = now * 1000;
                } else if !matches!(*t, 0 | 2) {
                    *state = 1;
                }
            }
        }
        29 => {
            // GetData(query, data, size, flags): everything is done at once; the data only once
            // the query has ended, and only all of it
            let q = d.id_of(a(1));
            let Some(&Data::Query(t, state, stamp)) = d.objs.get(&q).map(|o| &o.data) else { return E_INVALIDARG };
            if state != 2 {
                return DXGI_ERROR_INVALID_CALL;
            }
            // (no size: only whether it is done; a size: all of it, somewhere)
            if a(3) == 0 {
                return S_OK;
            }
            if a(2) == 0 || a(3) != query_size(t) {
                return E_INVALIDARG;
            }
            std::ptr::write_bytes(a(2) as *mut u8, 0, a(3) as usize);
            match t {
                0 | 5 => wr(a(2), 0, 1),
                1 => wr(a(2), 0, 1000),
                2 => {
                    wr(a(2), 0, stamp as u32);
                    wr(a(2), 1, (stamp >> 32) as u32);
                }
                3 => wr(a(2), 0, 1_000_000_000),
                _ => {}
            }
        }
        33 | 34 => {
            // OMSetRenderTargets(n, rtvs, dsv) (+ UAVs (start, n, uavs): kept, not drawn with)
            if m == 34 && a(5) != 0xffff_ffff {
                let mut u = st.uavs[0];
                set_slots(d, &mut u, 0, 8, 0);
                set_slots(d, &mut u, a(4), a(5).min(8), a(6));
                st.uavs[0] = u;
            }
            let n = if m == 34 && a(1) == 0xffff_ffff { u32::MAX } else { a(1) };
            if n != u32::MAX {
                let mut rt = st.rtvs;
                set_slots(d, &mut rt, 0, 8, 0);
                set_slots(d, &mut rt, 0, n.min(8), a(2));
                st.rtvs = rt;
                set_one(d, &mut st.dsv, a(3));
                if m == 33 {
                    let mut u = st.uavs[0];
                    set_slots(d, &mut u, 0, 8, 0);
                    st.uavs[0] = u;
                }
                hazards::after_outputs(d, st);
                // the viewport follows the first target when the game never set one
                if st.viewport[0][2] == 0.0 {
                    if let Some((w, h)) = view_size(d, st.rtvs[0]).or_else(|| view_size(d, st.dsv)) {
                        st.viewport[0] = [0.0, 0.0, w as f32, h as f32, 0.0, 1.0];
                    }
                }
            }
        }
        35 => {
            set_one(d, &mut st.blend, a(1));
            st.blend_factor = if a(2) != 0 { [rdf(a(2), 0), rdf(a(2), 1), rdf(a(2), 2), rdf(a(2), 3)] } else { [1.0; 4] };
            st.sample_mask = a(3);
        }
        36 => {
            set_one(d, &mut st.depth, a(1));
            st.stencil_ref = a(2);
        }
        43 => set_one(d, &mut st.raster, a(1)),
        44 => {
            st.nviewports = if a(2) != 0 { a(1).min(16) } else { 0 };
            if a(2) != 0 {
                for k in 0..st.nviewports as usize {
                    for i in 0..6 {
                        st.viewport[k][i] = rdf(a(2), 6 * k + i);
                    }
                }
                if d.trace {
                    eprintln!("d3d11: viewport {:?}", st.viewport[0]);
                }
            }
        }
        45 => {
            st.nscissors = if a(2) != 0 { a(1).min(16) } else { 0 };
            if a(2) != 0 {
                for k in 0..st.nscissors as usize {
                    for i in 0..4 {
                        st.scissor[k][i] = rd(a(2), 4 * k + i) as i32;
                    }
                }
            }
        }
        46 => copy_region(d, a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8)),
        47 => copy_resource(d, a(1), a(2)),
        57 => resolve(d, a(1), a(2), a(3), a(4), a(5)),
        48 => update_subresource(d, a(1), a(2), a(3), a(4), a(5), a(6)),
        50 => {
            // ClearRenderTargetView(view, color)
            let v = d.id_of(a(1));
            if let Some((tex, level, layer)) = view_target(d, v) {
                let layers = view_layers(d, v);
                let mut c = [rd(a(2), 0), rd(a(2), 1), rd(a(2), 2), rd(a(2), 3)];
                // 8-bit UNORM targets: the color as the runtime converts it (each channel
                // clamped, times 255 in floats, rounded to even), not left to the GPU's own
                if let Some(Data::View(View { fmt: 28 | 49 | 61 | 65 | 87 | 88, .. })) = d.objs.get(&v).map(|o| &o.data) {
                    for x in &mut c {
                        let f = f32::from_bits(*x);
                        let f = if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) };
                        *x = ((f * 255.0).round_ties_even() / 255.0).to_bits();
                    }
                }
                // (every layer (or 3D slice) the view has)
                for l in layer..layer + layers {
                    gpu::record(|q| q.cmd(&[gpu::CLEAR_VIEW, tex, level, l, 1, c[0], c[1], c[2], c[3], 0, 0]));
                }
                d.targets = None;
            }
        }
        53 => {
            // ClearDepthStencilView(view, flags, depth, stencil)
            let v = d.id_of(a(1));
            if let Some((tex, level, layer)) = view_target(d, v) {
                for l in layer..layer + view_layers(d, v) {
                    gpu::record(|q| q.cmd(&[gpu::CLEAR_VIEW, tex, level, l, (a(2) & 3) << 1, 0, 0, 0, 0, a(3), a(4) & 0xff]));
                }
                d.targets = None;
            }
        }
        54 => {
            let v = d.id_of(a(1));
            if let Some(Data::View(v)) = d.objs.get(&v).map(|o| &o.data) {
                if let Some(Data::Texture(t)) = d.objs.get(&v.res).map(|o| &o.data) {
                    let g = t.gpu;
                    gpu::record(|q| q.cmd(&[gpu::GEN_MIPS, g]));
                }
            }
        }
        30 => {
            let mut p = st.predicate.0;
            set_one(d, &mut p, a(1));
            st.predicate = (p, a(2));
        }
        86 => {
            get_one(d, st.predicate.0, a(1));
            wr(a(2), 0, st.predicate.1);
        }
        // GS, HS, DS, CS: constant buffers, resources, samplers, shaders (kept, not drawn with)
        22 | 62 | 66 | 71 => set_slots(d, &mut st.cbs[stage_of(m)], a(1), a(2), a(3)),
        31 | 59 | 63 | 67 => {
            set_slots(d, &mut st.srvs[stage_of(m)], a(1), a(2), a(3));
            hazards::after_srvs(d, st, stage_of(m), a(1), a(2));
        }
        32 | 61 | 65 | 70 => set_slots(d, &mut st.samplers[stage_of(m)], a(1), a(2), a(3)),
        23 | 60 | 64 | 69 => set_one(d, &mut st.shaders[stage_of(m) - 2], a(1)),
        68 => {
            set_slots(d, &mut st.uavs[1], a(1), a(2), a(3));
            hazards::after_outputs(d, st);
        }
        37 => {
            // SOSetTargets(n, buffers, offsets): kept, nothing streams out
            let mut so = st.so;
            set_slots(d, &mut so, 0, 4, 0);
            set_slots(d, &mut so, 0, a(1).min(4), a(2));
            st.so = so;
            hazards::after_buffers(d, st);
        }
        49 | 51 | 52 | 55 => {}
        41 => dispatch(d, st, a(1), a(2), a(3)),
        42 => {
            // DispatchIndirect(args, offset): the group counts from the buffer
            if let Some(Data::Buffer(b)) = d.objs.get(&d.id_of(a(1))).map(|o| &o.data) {
                if a(2) as u64 + 12 <= b.size as u64 {
                    let p = b.mem + a(2);
                    let (x, y, z) = (rd(p, 0), rd(p, 1), rd(p, 2));
                    dispatch(d, st, x, y, z);
                }
            }
        }
        38..=40 => {}
        72 | 77 => get_slots(d, &st.cbs[(m == 77) as usize], a(1), a(2), a(3)),
        73 | 84 => get_slots(d, &st.srvs[(m == 73) as usize], a(1), a(2), a(3)),
        74 => {
            get_one(d, st.ps, a(1));
            wr(a(3), 0, 0);
        }
        75 | 85 => get_slots(d, &st.samplers[(m == 75) as usize], a(1), a(2), a(3)),
        76 => {
            get_one(d, st.vs, a(1));
            wr(a(3), 0, 0);
        }
        78 => get_one(d, st.layout, a(1)),
        79 => {
            for i in 0..a(2) {
                let s = (a(1) + i) as usize;
                let (b, stride, off) = st.vbs.get(s).copied().unwrap_or_default();
                if a(3) != 0 {
                    get_one(d, b, a(3) + i * ptr());
                }
                wr(a(4), i as usize, stride);
                wr(a(5), i as usize, off);
            }
        }
        80 => {
            get_one(d, st.ib.0, a(1));
            wr(a(2), 0, st.ib.1);
            wr(a(3), 0, st.ib.2);
        }
        82 | 98 | 102 | 107 => {
            get_one(d, st.shaders[stage_of(m) - 2], a(1));
            if a(3) != 0 {
                wr(a(3), 0, 0);
            }
        }
        81 | 100 | 104 | 109 => get_slots(d, &st.cbs[stage_of(m)], a(1), a(2), a(3)),
        87 | 97 | 101 | 105 => get_slots(d, &st.srvs[stage_of(m)], a(1), a(2), a(3)),
        88 | 99 | 103 | 108 => get_slots(d, &st.samplers[stage_of(m)], a(1), a(2), a(3)),
        106 => get_slots(d, &st.uavs[1], a(1), a(2), a(3)),
        83 => wr(a(1), 0, st.topology),
        89 | 90 => {
            if a(2) != 0 {
                get_slots(d, &st.rtvs, 0, a(1), a(2));
            }
            if a(3) != 0 {
                get_one(d, st.dsv, a(3));
            }
            if m == 90 && a(6) != 0 {
                get_slots(d, &st.uavs[0], a(4), a(5), a(6));
            }
        }
        91 => {
            get_one(d, st.blend, a(1));
            if a(2) != 0 {
                for i in 0..4 {
                    wr(a(2), i, st.blend_factor[i].to_bits());
                }
            }
            wr(a(3), 0, st.sample_mask);
        }
        92 => {
            get_one(d, st.depth, a(1));
            wr(a(2), 0, st.stencil_ref);
        }
        93 => get_slots(d, &st.so, 0, a(1), a(2)),
        94 => get_one(d, st.raster, a(1)),
        95 | 96 => {
            // RSGetViewports/RSGetScissorRects(count, out): the count set; the array gets those
            // and zeros after them
            if a(1) != 0 {
                let (have, words) = if m == 95 { (st.nviewports, 6) } else { (st.nscissors, 4) };
                let room = rd(a(1), 0);
                wr(a(1), 0, have);
                if a(2) != 0 {
                    for k in 0..room as usize {
                        for i in 0..words {
                            let v = if (k as u32) >= have { 0 } else if m == 95 { st.viewport[k][i].to_bits() } else { st.scissor[k][i] as u32 };
                            wr(a(2), k * words + i, v);
                        }
                    }
                }
            }
        }
        110 => {
            // ClearState
            let old = std::mem::replace(st, default_state());
            unbind_state(d, &old);
        }
        111 => gpu::flush(),
        112 => return 0, // immediate
        113 => return 0,
        114 => return DXGI_ERROR_INVALID_CALL,
        // ID3D11DeviceContext1
        115 => return context_call2(d, st, 46, a, after), // CopySubresourceRegion1 (flags ignored)
        116 => return context_call2(d, st, 48, a, after), // UpdateSubresource1
        117 | 118 | 133 => {}                           // Discard*
        // *SetConstantBuffers1(start, n, buffers, first, count): whole buffers (offsets ignored)
        119..=124 => return context_call2(d, st, [7, 62, 66, 22, 16, 71][m as usize - 119], a, after),
        125..=130 => return context_call2(d, st, [72, 100, 104, 81, 77, 109][m as usize - 125], a, after),
        131 => swap_state(d, st, a(1), a(2)),
        58 => deferred::execute(d, st, a(1), a(2), after),
        132 => {} // ClearView
        // ID3D11DeviceContext2: tiled resources and annotations
        134..=139 | 141..=143 => {}
        140 => return 0, // IsAnnotationEnabled
        // ID3D11DeviceContext3: Flush1(type, event); hardware protection (none: off)
        144 => return context_call2(d, st, 111, a, after),
        145 => {}
        146 => {
            if a(1) != 0 {
                wr(a(1), 0, 0);
            }
        }
        // ID3D11DeviceContext4: Signal / Wait on fences (CreateFence gives none)
        147 | 148 => return E_INVALIDARG,
        _ => return E_NOTIMPL,
    }
    S_OK
}

/// SwapDeviceContextState(state, prev_out): `st` (the current state) goes back into the object
/// it came from (or a new one for the context's own state) and `state`'s is loaded.
unsafe fn swap_state(d: &mut D11, st: &mut State, state: u32, prev_out: u32) {
    let new = d.id_of(state);
    let Some(Data::CtxState(slot)) = d.objs.get_mut(&new).map(|o| &mut o.data) else {
        // (no state: nothing changes, and no previous one comes back)
        wrp(prev_out, 0, 0);
        return;
    };
    let Some(loaded) = slot.take() else {
        // (already current: it is the previous one too)
        if prev_out != 0 {
            d.add_ref(new);
            wrp(prev_out, 0, state);
        }
        return;
    };
    let cur = std::mem::replace(st, *loaded);
    let old = d.st_owner;
    d.st_owner = new;
    // (the context holds its current state privately: the application may let go of it)
    d.bind(new);
    if let Some(Data::CtxState(slot)) = d.objs.get_mut(&old).map(|o| &mut o.data) {
        *slot = Some(Box::new(cur));
        if prev_out != 0 {
            d.add_ref(old);
            wrp(prev_out, 0, d.addr_of(old));
        }
        d.unbind(old);
    } else {
        // the context's own state (or one whose object is gone): a new object
        let o = d.new_obj(IF_LINKAGE, Data::CtxState(Some(Box::new(cur))));
        if prev_out != 0 {
            wrp(prev_out, 0, o);
        } else {
            let id = d.id_of(o);
            d.release(id);
        }
    }
}

fn view_size(d: &D11, v: u32) -> Option<(u32, u32)> {
    let Some(Data::View(v)) = d.objs.get(&v).map(|o| &o.data) else { return None };
    let Some(Data::Texture(t)) = d.objs.get(&v.res).map(|o| &o.data) else { return None };
    Some(((t.w >> v.mip).max(1), (t.h >> v.mip).max(1)))
}

/// A render target or depth view's page texture, level and layer.
fn view_target(d: &D11, v: u32) -> Option<(u32, u32, u32)> {
    let Some(Data::View(v)) = d.objs.get(&v).map(|o| &o.data) else { return None };
    let Some(Data::Texture(t)) = d.objs.get(&v.res).map(|o| &o.data) else { return None };
    Some((t.gpu, v.mip, v.layer))
}

/// How many array layers (3D slices) a view covers (1 at least).
fn view_layers(d: &D11, v: u32) -> u32 {
    match d.objs.get(&v).map(|o| &o.data) {
        Some(Data::View(v)) => v.layers.max(1),
        _ => 1,
    }
}

/// Map(resource, subresource, type, flags, out); `dxgi`: IDXGISurface::Map's DXGI_MAPPED_RECT
unsafe fn map(d: &mut D11, res: u32, sub: u32, ty: u32, out: u32, dxgi: bool, after: &mut After) -> u32 {
    let id = d.id_of(res);
    let mut reqs = Vec::new();
    // (what the runtime allows: staging resources read and write, dynamic ones write discard,
    // and buffers no overwrite too; default and immutable ones are not mapped)
    let usage = match d.objs.get(&id).map(|o| &o.data) {
        Some(Data::Buffer(b)) => Some((b.usage, true)),
        Some(Data::Texture(t)) => Some((t.usage, false)),
        _ => None,
    };
    if let Some((usage, buffer)) = usage.filter(|_| !dxgi) {
        let ok = match usage {
            3 => (1..=3).contains(&ty),
            2 => ty == 4 || (ty == 5 && buffer),
            _ => false,
        };
        if !ok {
            return E_INVALIDARG;
        }
    }
    let r = match d.objs.get_mut(&id).map(|o| &mut o.data) {
        Some(Data::Buffer(b)) => {
            // (D3D11_MAP_WRITE_NO_OVERWRITE: 5)
            b.stream = ty == 5 && (b.stream || !b.dirty);
            b.dirty = true;
            put_mapped(out, b.mem, b.size, b.size, dxgi);
            S_OK
        }
        Some(Data::Texture(t)) => {
            let level = sub % t.mips.max(1);
            let (lw, lh) = ((t.w >> level).max(1), (t.h >> level).max(1));
            let (pitch, rows) = format::pitch_rows(t.fmt, lw, lh);
            let depth = if t.dim == 3 { (t.layers >> level).max(1) } else { 1 };
            // (in 64 bits: a 2048^3 texture's level 0 does not fit in 32)
            let Some(size) = u32::try_from(pitch as u64 * rows as u64 * depth as u64).ok().filter(|&n| n < 0x8000_0000) else {
                wrp(out, 0, 0);
                return E_OUTOFMEMORY;
            };
            let write = ty != 1;
            let mem = match t.maps.iter().position(|m| m.0 == sub) {
                Some(i) => {
                    t.maps[i].3 = write;
                    t.maps[i].1
                }
                None => 0,
            };
            let fresh = mem == 0;
            let mem = if mem != 0 {
                mem
            } else {
                let p = (*d.calls).alloc;
                let mem = p(size.max(16)) as u32;
                if mem == 0 {
                    wrp(out, 0, 0);
                    return E_OUTOFMEMORY;
                }
                if t.pristine {
                    std::ptr::write_bytes(mem as *mut u8, 0, size.max(16) as usize);
                }
                if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
                    t.maps.push((sub, mem, pitch, write));
                }
                mem
            };
            if let Some(Data::Texture(t)) = d.objs.get(&id).map(|o| &o.data) {
                // reading or writing part: what the texture holds (D3D11_MAP_WRITE keeps what is
                // there too: a part written goes back with the rest), the page's pixels where they
                // are the guest's bytes (one readback a slice), else the bytes kept of the last writes;
                // nothing where the map memory holds them already (since the last Unmap nothing wrote
                // the subresource: ANGLE uploads every texture through a staging one mapped so, and
                // the waits cost Godot seconds)
                let (_, layer, ..) = sub_size(t, sub);
                if ty != 4 && holds_bits(t) && !keeps_bits(t) && !t.clean.contains(&sub) && !(fresh && t.pristine) {
                    for z in 0..depth {
                        d.next_gpu += 1;
                        let req = 0x4000_0000 | d.next_gpu;
                        let (g, l) = (t.gpu, level | (layer + z) << 16);
                        gpu::record(|q| q.cmd(&[gpu::READBACK, req, g, l, 0, 0, lw, lh, mem + z * pitch * rows, pitch]));
                        reqs.push(req);
                    }
                } else if ty != 4 && !t.clean.contains(&sub) {
                    match t.bits.get(&sub) {
                        Some(b) => std::ptr::copy_nonoverlapping(b.as_ptr(), mem as *mut u8, b.len().min(size as usize)),
                        None => std::ptr::write_bytes(mem as *mut u8, 0, size as usize),
                    }
                }
            }
            put_mapped(out, mem, pitch, pitch * rows, dxgi);
            S_OK
        }
        _ => E_INVALIDARG,
    };
    if !reqs.is_empty() {
        *after = After::Readback(reqs);
    }
    r
}

unsafe fn unmap(d: &mut D11, res: u32, sub: u32) {
    let id = d.id_of(res);
    if d.trace {
        if let Some(Data::Buffer(b)) = d.objs.get(&id).map(|o| &o.data) {
            if b.size <= 256 {
                let f: Vec<f32> = (0..b.size / 4).map(|i| *((b.mem + 4 * i) as *const f32)).collect();
                eprintln!("d3d11: buffer {id} = {f:?}");
            }
        }
    }
    if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
        if let Some(&(_, mem, pitch, write)) = t.maps.iter().find(|m| m.0 == sub) {
            if write {
                let (_, _, lw, lh, _) = sub_size(t, sub);
                let rows = format::pitch_rows(t.fmt, lw, lh).1;
                upload_texture(t, sub, mem as usize, pitch, pitch * rows, None);
            }
            // (the memory holds what the texture has now: read back or kept at the Map, or sent
            // up here)
            if t.usage == 3 && (holds_bits(t) || keeps_bits(t)) && !t.clean.contains(&sub) {
                t.clean.push(sub);
            }
        }
    }
}

/// The page's copy of texture `id` changed (a copy or update into it): its map memory is stale.
fn texture_written(d: &mut D11, id: u32) {
    if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
        t.clean.clear();
        t.pristine = false;
    }
}

/// UpdateSubresource(resource, subresource, box, data, row pitch, depth pitch). Of a staging or
/// immutable resource it does nothing (Windows: winref d3d11copy's "update" of a staging texture
/// hashes as before it, every format, 2D and 3D).
unsafe fn update_subresource(d: &mut D11, res: u32, sub: u32, bx: u32, data: u32, pitch: u32, slice: u32) {
    let id = d.id_of(res);
    let usage = match d.objs.get(&id).map(|o| &o.data) {
        Some(Data::Buffer(b)) => b.usage,
        Some(Data::Texture(t)) => t.usage,
        _ => 0,
    };
    if matches!(usage, 1 | 3) {
        return;
    }
    let b = if bx != 0 { Some([rd(bx, 0), rd(bx, 1), rd(bx, 2), rd(bx, 3), rd(bx, 4), rd(bx, 5)]) } else { None };
    match d.objs.get_mut(&id).map(|o| &mut o.data) {
        Some(Data::Buffer(buf)) => {
            let (x0, x1) = b.map_or((0, buf.size), |b| (b[0], b[3].min(buf.size)));
            if x1 > x0 {
                std::ptr::copy(data as *const u8, (buf.mem + x0) as *mut u8, (x1 - x0) as usize);
                buf.dirty = true;
                buf.stream = false;
                if d.trace && buf.size <= 256 {
                    let f: Vec<f32> = (0..buf.size / 4).map(|i| *((buf.mem + 4 * i) as *const f32)).collect();
                    eprintln!("d3d11: buffer {id} = {f:?}");
                }
            }
        }
        Some(Data::Texture(t)) => {
            // the box's texels start at `data`
            t.clean.clear();
            t.pristine = false;
            match b {
                // (upload_texture reads the box's rows from `data` on: the box's first texel)
                Some(bb) => upload_texture(t, sub, data as usize, pitch, slice, Some(bb)),
                None => upload_texture(t, sub, data as usize, pitch, slice, None),
            }
        }
        _ => {}
    }
}

/// A 2D texture's width, height and DXGI format, by a guest pointer to it (Media Foundation's
/// Media Engine draws its frames into the game's textures: sake's mf.rs).
pub fn texture_desc(res: u32) -> Option<(u32, u32, u32)> {
    let g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    let d = g.as_ref()?;
    match d.objs.get(&d.id_of(res)).map(|o| &o.data) {
        Some(Data::Texture(t)) if t.dim == 2 => Some((t.w, t.h, t.fmt)),
        _ => None,
    }
}

/// Writes guest texels (rows of `pitch` bytes from `data`) into box `bx` (left top right bottom)
/// of texture `res`'s first subresource, as UpdateSubresource. Whether it is one.
pub unsafe fn write_texture(res: u32, data: u32, pitch: u32, bx: [u32; 4]) -> bool {
    write_texture_from(res, data as usize as *const u8, pitch, bx)
}

/// [`write_texture`] from texels anywhere in memory (sake's own frames: no guest copy).
pub unsafe fn write_texture_from(res: u32, data: *const u8, pitch: u32, bx: [u32; 4]) -> bool {
    let mut g = D11S.lock().unwrap_or_else(|e| e.into_inner());
    let Some(d) = g.as_mut() else { return false };
    let id = d.id_of(res);
    let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) else { return false };
    t.clean.clear();
    t.pristine = false;
    upload_texture(t, 0, data as usize, pitch, pitch * (bx[3] - bx[1]), Some([bx[0], bx[1], 0, bx[2], bx[3], 1]));
    true
}

unsafe fn copy_resource(d: &mut D11, dst: u32, src: u32) {
    let (di, si) = (d.id_of(dst), d.id_of(src));
    texture_written(d, di);
    match (d.objs.get(&di).map(|o| &o.data), d.objs.get(&si).map(|o| &o.data)) {
        (Some(Data::Buffer(a)), Some(Data::Buffer(b))) => {
            std::ptr::copy(b.mem as *const u8, a.mem as *mut u8, a.size.min(b.size) as usize);
            if let Some(Data::Buffer(a)) = d.objs.get_mut(&di).map(|o| &mut o.data) {
                a.dirty = true;
                a.stream = false;
            }
        }
        (Some(Data::Texture(a)), Some(Data::Texture(b))) => {
            // (every subresource both have: Windows wants them alike)
            let (mips, layers) = (a.mips.min(b.mips), if a.dim == 3 { 1 } else { a.layers.min(b.layers) });
            let (bm, am) = (b.mips, a.mips);
            for layer in 0..layers {
                for l in 0..mips {
                    copy_sub(d, di, layer * am + l, [0; 3], si, layer * bm + l, None);
                }
            }
        }
        _ => {}
    }
}

/// ResolveSubresource(dst, dst sub, src, src sub, format): a multisampled 2D texture's
/// subresource into one of a single-sample one of its size, of formats of one typeless group
/// (Windows does nothing with any other; `format` is a typed one of the group).
unsafe fn resolve(d: &mut D11, dst: u32, dsub: u32, src: u32, ssub: u32, fmt: u32) {
    let (di, si) = (d.id_of(dst), d.id_of(src));
    let (Some(Data::Texture(a)), Some(Data::Texture(b))) = (d.objs.get(&di).map(|o| &o.data), d.objs.get(&si).map(|o| &o.data)) else { return };
    let (dl, dly) = (dsub % a.mips.max(1), dsub / a.mips.max(1));
    let ok = a.dim == 2
        && b.dim == 2
        && a.samples.0 == 1
        && b.samples.0 > 1
        && dly < a.layers
        && ssub < b.layers
        && ((a.w >> dl).max(1), (a.h >> dl).max(1)) == (b.w, b.h)
        && format::copy_compatible(a.fmt, b.fmt)
        && format::copy_compatible(fmt, b.fmt);
    if !ok {
        return;
    }
    let cmd = [gpu::RESOLVE, a.gpu, dl, dly, b.gpu, ssub];
    gpu::record(|q| q.cmd(&cmd));
    d.targets = None;
}

/// CopySubresourceRegion(dst, dst sub, x, y, z, src, src sub, box)
unsafe fn copy_region(d: &mut D11, dst: u32, dsub: u32, x: u32, y: u32, z: u32, src: u32, ssub: u32, bx: u32) {
    let (di, si) = (d.id_of(dst), d.id_of(src));
    texture_written(d, di);
    match (d.objs.get(&di).map(|o| &o.data), d.objs.get(&si).map(|o| &o.data)) {
        (Some(Data::Buffer(a)), Some(Data::Buffer(b))) => {
            let (s0, s1) = if bx != 0 { (rd(bx, 0), rd(bx, 3)) } else { (0, b.size) };
            let n = s1.min(b.size).saturating_sub(s0).min(a.size.saturating_sub(x));
            std::ptr::copy((b.mem + s0) as *const u8, (a.mem + x) as *mut u8, n as usize);
            if let Some(Data::Buffer(a)) = d.objs.get_mut(&di).map(|o| &mut o.data) {
                a.dirty = true;
                a.stream = false;
            }
        }
        (Some(Data::Texture(_)), Some(Data::Texture(_))) => {
            let b = (bx != 0).then(|| [rd(bx, 0), rd(bx, 1), rd(bx, 2), rd(bx, 3), rd(bx, 4), rd(bx, 5)]);
            copy_sub(d, di, dsub, [x, y, z], si, ssub, b);
        }
        _ => {}
    }
}

/// Copies a box of subresource `ssub` of texture `si` (all of it without one) to `at` in
/// subresource `dsub` of `di`: the bits, between formats of one typeless group (Windows copies
/// nothing between others, nor a box reaching outside either subresource). On the page where both
/// textures' texels are the guest's bytes, or both are the same conversion of them; else through
/// the bytes (those kept, or read back), uploaded as the destination converts them.
/// Whether `n` units from `at` end by `end` (without wrapping: a box at 0xffff_fff0 does not fit).
fn fits(at: u32, n: u32, end: u32) -> bool {
    at.checked_add(n).is_some_and(|e| e <= end)
}

unsafe fn copy_sub(d: &mut D11, di: u32, dsub: u32, at: [u32; 3], si: u32, ssub: u32, bx: Option<[u32; 6]>) {
    let (Some(Data::Texture(a)), Some(Data::Texture(b))) = (d.objs.get(&di).map(|o| &o.data), d.objs.get(&si).map(|o| &o.data)) else { return };
    if !format::copy_compatible(a.fmt, b.fmt) || dsub >= a.mips * if a.dim == 3 { 1 } else { a.layers } || ssub >= b.mips * if b.dim == 3 { 1 } else { b.layers } {
        return;
    }
    let (Some(fa), Some(fb)) = (format::info(a.fmt), format::info(b.fmt)) else { return };
    let (sl, slayer, sw, sh, sd) = sub_size(b, ssub);
    let (dl, dlayer, dw, dh, dd) = sub_size(a, dsub);
    let [x0, y0, z0, x1, y1, z1] = bx.unwrap_or([0, 0, 0, sw, sh, sd]);
    let (z0, z1) = if b.dim == 3 { (z0, z1) } else { (0, 1) };
    let dz = if a.dim == 3 { at[2] } else { 0 };
    // (block-compressed boxes: whole blocks, the last ones past a level's edge)
    let pad = |v: u32, f: &format::Format| v.div_ceil(f.block) * f.block;
    let (w, h, depth) = (x1.saturating_sub(x0), y1.saturating_sub(y0), z1.saturating_sub(z0));
    if w == 0 || h == 0 || depth == 0 || x1 > pad(sw, &fb) || y1 > pad(sh, &fb) || z1 > sd || !fits(at[0], w, pad(dw, &fa)) || !fits(at[1], h, pad(dh, &fa)) || !fits(dz, depth, dd) {
        return;
    }
    // a depth-stencil or multisampled texture is copied whole subresources only (Windows: winref
    // d3d11copy's box of R32_UINT into D32_FLOAT leaves it as it was)
    let whole = at == [0, 0, 0] && [x0, y0, z0, x1, y1, z1] == [0, 0, 0, sw, sh, sd] && (sw, sh, sd) == (dw, dh, dd);
    if (a.bind | b.bind) & 0x40 != 0 || a.samples.0 > 1 || b.samples.0 > 1 {
        if !whole {
            return;
        }
    }
    texture_written(d, di);
    let (Some(Data::Texture(a)), Some(Data::Texture(b))) = (d.objs.get(&di).map(|o| &o.data), d.objs.get(&si).map(|o| &o.data)) else { return };
    let bits = if a.fmt != b.fmt { 2 } else { 0 };
    // (the page's texels of both the same function of the guest's bytes; 32-bit float RGBA
    // possibly halves, of another size than the integer ones)
    let f32x4 = |t: &Texture| t.page == format::P_RGBA32F;
    let on_page = (holds_bits(a) && holds_bits(b) && f32x4(a) == f32x4(b)) || (a.page == b.page && fa.conv == fb.conv && fa.bpp == fb.bpp && fa.block == fb.block && fa.no_alpha == fb.no_alpha);
    let (ag, bg, keeps, bfmt) = (a.gpu, b.gpu, keeps_bits(a), b.fmt);
    let gpu_copy = |d: &mut D11| {
        // (the page's boxes inside its levels)
        let (cw, ch) = (w.min(sw.saturating_sub(x0)).min(dw.saturating_sub(at[0])), h.min(sh.saturating_sub(y0)).min(dh.saturating_sub(at[1])));
        let c = [gpu::COPY, bg, sl | (slayer + z0) << 16, x0, y0, cw, ch, ag, dl | (dlayer + dz) << 16, at[0], at[1], cw, ch, bits | depth << 16];
        gpu::record(|q| q.cmd(&c));
        d.targets = None;
    };
    if on_page {
        gpu_copy(d);
        if !keeps {
            return;
        }
    }
    // the source's bytes of the box (kept ones, or the page's read back): the destination's kept
    // ones, and its page texels as it converts them
    let (pitch, rows) = format::pitch_rows(bfmt, w, h);
    let Some(mem) = source_bytes(d, si, ssub, [x0, y0, z0, w, h, depth]) else {
        if !on_page {
            gpu_copy(d);
        }
        return;
    };
    if let Some(Data::Texture(a)) = d.objs.get_mut(&di).map(|o| &mut o.data) {
        if on_page {
            keep_bits(a, dsub, mem as usize, pitch, pitch * rows, [at[0], at[1], dz, w, h, dz + depth]);
        } else {
            upload_texture(a, dsub, mem as usize, pitch, pitch * rows, Some([at[0], at[1], dz, at[0] + w, at[1] + h, dz + depth]));
        }
    }
    ((*d.calls).free)(mem as *mut u8);
    d.targets = None;
}

/// A box (x, y, z, width, height, slices; x and y on blocks) of a texture's subresource as the
/// guest's bytes (rows of `format::pitch_rows`, slice after slice) in memory from the host's
/// allocator: those kept, or the page's read back where they are its texels. None: neither.
unsafe fn source_bytes(d: &mut D11, id: u32, sub: u32, b: [u32; 6]) -> Option<u32> {
    let Some(Data::Texture(t)) = d.objs.get(&id).map(|o| &o.data) else { return None };
    let f = format::info(t.fmt)?;
    let [x0, y0, z0, w, h, depth] = b;
    let (keeps, holds, g) = (keeps_bits(t), holds_bits(t), t.gpu);
    if !keeps && !holds {
        return None;
    }
    let (pitch, rows) = format::pitch_rows(t.fmt, w, h);
    // (in 64 bits; past what the allocator gives, or nothing from it: none)
    let size = u32::try_from(pitch as u64 * rows as u64 * depth as u64).ok().filter(|&n| n < 0x8000_0000)?;
    let mem = ((*d.calls).alloc)(size.max(16)) as u32;
    if mem == 0 {
        return None;
    }
    // (what nothing wrote, and what a readback leaves alone: zeros)
    std::ptr::write_bytes(mem as *mut u8, 0, size as usize);
    let (level, layer, lw, lh, _) = sub_size(t, sub);
    if keeps {
        let (spitch, srows) = format::pitch_rows(t.fmt, lw, lh);
        let (bx, by) = (x0 / f.block, y0 / f.block);
        if let Some(kept) = t.bits.get(&sub) {
            for z in 0..depth {
                for r in 0..rows {
                    let s0 = (((z0 + z) * srows + by + r) * spitch + bx * f.bpp) as usize;
                    std::ptr::copy_nonoverlapping(kept.as_ptr().add(s0), (mem + (z * rows + r) * pitch) as *mut u8, pitch as usize);
                }
            }
        }
        return Some(mem);
    }
    let mut reqs = Vec::new();
    for z in 0..depth {
        d.next_gpu += 1;
        let req = 0x4000_0000 | d.next_gpu;
        let c = [gpu::READBACK, req, g, level | (layer + z0 + z) << 16, x0, y0, w, h, mem + z * pitch * rows, pitch];
        gpu::record(|q| q.cmd(&c));
        reqs.push(req);
    }
    for req in reqs {
        gpu::wait_readback(req);
    }
    Some(mem)
}

// ---------------------------------------------------------------------------------------------
// draws

/// A shader's page shader and bindings (translated on first use; vertex shaders per pixel shader).
/// `samplers`: a pixel shader's (those with BORDER or MIRROR_ONCE addressing: the shader does it)
fn shader_for(d: &mut D11, sh: u32, ps: u32, sprite: Option<f32>, samplers: &[u32]) -> Option<(u32, Vec<(u32, dxwgsl::Binding)>)> {
    let mut addr = Vec::new();
    if matches!(d.objs.get(&sh).map(|o| &o.data), Some(Data::Shader(s)) if s.pixel) {
        for (n, &id) in samplers.iter().enumerate() {
            if let Some(desc) = state_desc(d, id) {
                let flags = dxwgsl::addr::flags(desc[1], desc[2], desc[0] & 4 != 0 || desc[0] & 0x40 != 0);
                if flags != 0 {
                    addr.push(dxwgsl::addr::Addr { sampler: n as u32, flags, border: [desc[7], desc[8], desc[9], desc[10]] });
                }
            }
        }
    }
    // (point sprite variants of a vertex shader: the pixel shader's id with the top bit set)
    let Some(Data::Shader(s)) = d.objs.get(&sh).map(|o| &o.data) else { return None };
    let key = (if s.pixel { 0 } else if sprite.is_some() { ps | 0x8000_0000 } else { ps }, addr);
    if let Some(r) = s.wgsl.get(&key) {
        return r.clone();
    }
    // (the pixel shader's code copied only for a translation, not on every draw: review b02)
    let ps_code = match d.objs.get(&ps).map(|o| &o.data) {
        Some(Data::Shader(s)) => Some(s.code.clone()),
        _ => None,
    };
    let trace = d.trace;
    let Some(Data::Shader(s)) = d.objs.get_mut(&sh).map(|o| &mut o.data) else { return None };
    let w = match sprite {
        Some(max) if !s.pixel => dxwgsl::translate_point_sprite(&s.code, ps_code.as_deref(), max),
        _ => dxwgsl::translate_addr(&s.code, s.pixel, if s.pixel { None } else { ps_code.as_deref() }, &key.1),
    };
    let r = match w {
        Ok(w) => {
            let id = page_id();
            gpu::record(|q| {
                let off = q.blob(w.code.as_bytes());
                q.cmd(&[gpu::CREATE_SHADER, id, off, w.code.len() as u32]);
            });
            if trace {
                eprintln!("d3d11: {} shader {sh} -> {} bindings {:?}", if s.pixel { "pixel" } else { "vertex" }, id, w.bindings);
                if trace2() {
                    eprintln!("{}", w.code);
                }
            }
            Some((id, w.bindings))
        }
        Err(e) => {
            eprintln!("d3d11: shader {sh}: {e}");
            None
        }
    };
    s.wgsl.insert(key, r.clone());
    r
}

fn state_desc(d: &D11, id: u32) -> Option<&Vec<u32>> {
    match d.objs.get(&id).map(|o| &o.data) {
        Some(Data::State(s)) => Some(s),
        _ => None,
    }
}

/// D3D11_BLEND to d3d9.ts BLEND2 (color; `alpha`: as an alpha factor)
fn blend_factor(b: u32, alpha: bool) -> u32 {
    // BLEND2: zero one src one-minus-src src-alpha one-minus-src-alpha dst-alpha
    // one-minus-dst-alpha dst one-minus-dst src-alpha-saturated constant one-minus-constant
    match b {
        1 => 0,
        2 => 1,
        3 => if alpha { 4 } else { 2 },
        4 => if alpha { 5 } else { 3 },
        5 => 4,
        6 => 5,
        7 => 6,
        8 => 7,
        9 => if alpha { 6 } else { 8 },
        10 => if alpha { 7 } else { 9 },
        11 => 10,
        14 => 11,
        15 => 12,
        _ => 1,
    }
}

/// The page buffer with `id`'s current contents (made when the guest wrote it).
fn buffer_gpu(d: &mut D11, id: u32) -> u32 {
    let Some(Data::Buffer(b)) = d.objs.get_mut(&id).map(|o| &mut o.data) else { return 0 };
    if b.dirty || b.gpu == 0 {
        // (written in place: the page moves it to a new GPU buffer when draws before use it)
        let made = b.gpu;
        let g = if made != 0 { made } else { page_id() };
        let (mem, size) = (b.mem, b.size);
        b.gpu = g;
        b.dirty = false;
        let bytes = unsafe { std::slice::from_raw_parts(mem as *const u8, size as usize) };
        if made != 0 && b.stream && b.shadow.len() == bytes.len() {
            // (ANGLE streams each draw's vertices into a big dynamic buffer: uploading all of it
            // per draw cost Godot seconds)
            b.stream = false;
            if let Some((lo, hi)) = changed_range(&b.shadow, bytes) {
                b.shadow[lo..hi].copy_from_slice(&bytes[lo..hi]);
                gpu::record(|q| {
                    let off = q.blob(&bytes[lo..hi]);
                    q.cmd(&[gpu::WRITE_BUFFER, g, lo as u32 | gpu::WRITE_UNUSED, off, (hi - lo) as u32]);
                });
            }
            return g;
        }
        b.stream = false;
        if b.usage == 2 {
            b.shadow.clear();
            b.shadow.extend_from_slice(bytes);
        }
        gpu::record(|q| {
            let off = q.blob(bytes);
            if made == 0 {
                q.cmd(&[gpu::CREATE_BUFFER, g, size.next_multiple_of(16).max(16)]);
            }
            q.cmd(&[gpu::WRITE_BUFFER, g, 0, off, size]);
        });
    }
    b.gpu
}

/// The bytes where `new` differs from `old` (same length), widened to whole 4-byte words (as
/// writeBuffer writes them; the end may be `new`'s length); None when equal.
pub(crate) fn changed_range(old: &[u8], new: &[u8]) -> Option<(usize, usize)> {
    let n = new.len().min(old.len());
    // (8 bytes at a time from each end, as words: a slice compare is a memcmp call, and this
    // runs over a megabyte a draw)
    let word = |b: &[u8], at: usize| u64::from_ne_bytes(b[at..at + 8].try_into().unwrap());
    let mut lo = 0;
    while lo + 8 <= n && word(old, lo) == word(new, lo) {
        lo += 8;
    }
    while lo < n && old[lo] == new[lo] {
        lo += 1;
    }
    if lo == n {
        return None;
    }
    let mut hi = n;
    while hi >= lo + 8 && word(old, hi - 8) == word(new, hi - 8) {
        hi -= 8;
    }
    while old[hi - 1] == new[hi - 1] {
        hi -= 1;
    }
    Some((lo & !3, hi.next_multiple_of(4).min(n)))
}

/// WebGPU has 8 vertex buffers, D3D11 16 slots, and ANGLE gives every GL attribute its own
/// slot, often on one buffer. When a draw uses more than 8, slots on the same buffer with the
/// same stride and step become one (offsets moved into the attributes) where the attributes
/// still fit in a vertex. `slots`: (slot, step, attributes [format, offset, location]); the
/// result: (buffer, stride, offset) per slot index used in `slots` afterwards.
fn merge_vertex_buffers(vbs: &[(u32, u32, u32); 32], slots: &mut Vec<(u32, u32, Vec<[u32; 3]>)>) -> Vec<(u32, u32, u32)> {
    let mut vbx = vbs.to_vec();
    if slots.len() <= 8 {
        return vbx;
    }
    let mut out: Vec<(u32, u32, Vec<[u32; 3]>)> = Vec::new();
    for (slot, step, attrs) in std::mem::take(slots) {
        let (buf, stride, off) = vbs[slot as usize];
        let limit = if stride == 0 { 2048 } else { stride };
        let fits = |base: u32, list: &[[u32; 3]], from: u32| list.iter().all(|a| from - base + a[1] + vertex_size(a[0]) <= limit);
        let into = out.iter().position(|(s2, st2, a2)| {
            let (b2, str2, o2) = vbx[*s2 as usize];
            if b2 != buf || str2 != stride || *st2 != step || buf == 0 {
                return false;
            }
            let base = o2.min(off);
            fits(base, a2, o2) && fits(base, &attrs, off)
        });
        match into {
            Some(i) => {
                let s2 = out[i].0 as usize;
                let o2 = vbx[s2].2;
                let base = o2.min(off);
                for a in &mut out[i].2 {
                    a[1] += o2 - base;
                }
                out[i].2.extend(attrs.into_iter().map(|a| [a[0], a[1] + off - base, a[2]]));
                vbx[s2].2 = base;
            }
            None => out.push((slot, step, attrs)),
        }
    }
    *slots = out;
    vbx
}

#[allow(clippy::too_many_arguments)]
/// The edges of a draw's triangles (list, or strip with `strip`) as line-list indices: each
/// triangle's a-b, b-c, c-a, of its index buffer's indices (a strip's restart index cuts it)
/// or of the vertex numbers. None when the indices are out of the buffer.
unsafe fn wire_edges(d: &D11, st: &State, indexed: bool, count: u32, start: u32, strip: bool) -> Option<Vec<u32>> {
    let idx: Vec<u32> = if indexed {
        let isz = if st.ib.1 == 42 { 4u64 } else { 2 };
        let Some(Data::Buffer(b)) = d.objs.get(&st.ib.0).map(|o| &o.data) else { return None };
        if st.ib.2 as u64 + (start as u64 + count as u64) * isz > b.size as u64 {
            return None;
        }
        let p = b.mem + st.ib.2 + start * isz as u32;
        (0..count as usize).map(|k| if isz == 4 { rd(p, k) } else { *(p as *const u16).add(k) as u32 }).collect()
    } else {
        (start..start.saturating_add(count)).collect()
    };
    let restart = if !indexed { None } else if st.ib.1 == 42 { Some(u32::MAX) } else { Some(0xffff) };
    Some(triangle_edges(&idx, strip, restart))
}

/// See `wire_edges`: the edges of the triangles of `idx`.
fn triangle_edges(idx: &[u32], strip: bool, restart: Option<u32>) -> Vec<u32> {
    let mut e = Vec::new();
    let mut tri = |a: u32, b: u32, c: u32| e.extend_from_slice(&[a, b, b, c, c, a]);
    if strip {
        for run in idx.split(|&v| Some(v) == restart) {
            for t in run.windows(3) {
                tri(t[0], t[1], t[2]);
            }
        }
    } else {
        for t in idx.chunks_exact(3) {
            tri(t[0], t[1], t[2]);
        }
    }
    e
}

unsafe fn draw(d: &mut D11, st: &State, indexed: bool, count: u32, instances: u32, start: u32, base: i32, first_instance: u32) {
    if count == 0 || instances == 0 || st.vs == 0 {
        return;
    }
    // GL_POINTS through ANGLE: its point sprite geometry shader, done as an instance per point
    // (non-indexed, non-instanced point lists; see dxwgsl::translate_point_sprite)
    let sprite = match d.objs.get(&st.shaders[0]).map(|o| &o.data) {
        Some(Data::Shader(s)) if st.topology == 1 && !indexed && instances <= 1 => dxwgsl::point_sprite_gs(&s.code),
        _ => None,
    };
    let (count, instances, start, first_instance) = if sprite.is_some() { (6, count, 0, start) } else { (count, instances, start, first_instance) };
    let Some((vs, vb)) = shader_for(d, st.vs, st.ps, sprite, &[]) else { return };
    let ps = if st.ps != 0 { shader_for(d, st.ps, 0, None, &st.samplers[1]) } else { None };
    if trace2() {
        // the first vertices and the vertex shader's constants
        let words = |id: u32, from: u32, n: u32| -> Vec<String> {
            match d.objs.get(&id).map(|o| &o.data) {
                Some(Data::Buffer(b)) => (0..n).filter(|i| from + i * 4 + 4 <= b.size).map(|i| format!("{:08x}", rd(b.mem + from, i as usize))).collect(),
                _ => Vec::new(),
            }
        };
        let (vb0, stride, off) = st.vbs[0];
        eprintln!("d3d11: draw {count} from {start} vb stride {stride} offset {off}: {}", words(vb0, off + start * stride, (stride / 4) * 2).join(" "));
        for (i, &c) in st.cbs[0].iter().enumerate().filter(|c| *c.1 != 0).take(4) {
            eprintln!("d3d11:   vs cb{i}: {}", words(c, 0, 16).join(" "));
        }
    }
    // targets
    let mut colors = Vec::new();
    let mut formats = Vec::new();
    for &v in &st.rtvs {
        if let Some((tex, level, layer)) = view_target(d, v) {
            let page = match d.objs.get(&v).map(|o| &o.data) {
                Some(Data::View(vv)) => match d.objs.get(&vv.res).map(|o| &o.data) {
                    Some(Data::Texture(t)) => t.page,
                    _ => 0,
                },
                _ => 0,
            };
            colors.extend_from_slice(&[tex, level, layer]);
            formats.push(page);
        } else {
            break;
        }
    }
    let depth = view_target(d, st.dsv);
    let depth_fmt = depth.and_then(|_| match d.objs.get(&st.dsv).map(|o| &o.data) {
        Some(Data::View(v)) => match d.objs.get(&v.res).map(|o| &o.data) {
            Some(Data::Texture(t)) => Some(t.page),
            _ => None,
        },
        _ => None,
    });
    if colors.is_empty() && depth.is_none() {
        return;
    }
    let mut tw = vec![(colors.len() / 3) as u32];
    tw.extend_from_slice(&colors);
    let (dt, dl, dly) = depth.unwrap_or((0, 0, 0));
    tw.extend_from_slice(&[dt, dl, dly]);
    let gen = gpu::batch_gen();
    if d.targets.as_ref().is_none_or(|(g, t)| *g != gen || *t != tw) {
        let mut c = vec![gpu::SET_TARGETS2];
        c.extend_from_slice(&tw);
        gpu::record(|q| q.cmd(&c));
        d.targets = Some((gen, tw));
    }
    // pipeline
    let layout = match d.objs.get(&st.layout).map(|o| &o.data) {
        Some(Data::Layout(l)) => l.iter().map(|e| (e.name.clone(), e.index, e.fmt, e.slot, e.offset, e.instance)).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let vs_code = match d.objs.get(&st.vs).map(|o| &o.data) {
        Some(Data::Shader(s)) => s.code.clone(),
        _ => return,
    };
    let inputs = dxwgsl::signature(&vs_code, false);
    // vertex buffers used: slot -> (attributes)
    let mut slots: Vec<(u32, u32, Vec<[u32; 3]>)> = Vec::new(); // (slot, step, attrs: format offset location)
    for inp in &inputs {
        if inp.sysval != 0 {
            continue;
        }
        let Some(e) = layout.iter().find(|e| e.0.eq_ignore_ascii_case(inp.name()) && e.1 == inp.index) else { continue };
        let Some(vf) = format::vertex_format(e.2) else { continue };
        let i = match slots.iter().position(|s| s.0 == e.3) {
            Some(i) => i,
            None => {
                slots.push((e.3, if sprite.is_some() { 1 } else { e.5 as u32 }, Vec::new()));
                slots.len() - 1
            }
        };
        slots[i].2.push([vf, e.4, inp.reg]);
    }
    slots.sort_by_key(|s| s.0);
    // the vertex buffers the draw reads: (buffer, stride, offset) by slot
    let vbx = merge_vertex_buffers(&st.vbs, &mut slots);
    if slots.len() > 8 {
        // (more than WebGPU's 8: an invalid pipeline would lose the whole frame)
        return;
    }
    let blend = state_desc(d, st.blend).cloned();
    let ds = state_desc(d, st.depth).cloned();
    let rs = state_desc(d, st.raster).cloned();
    let topo = match st.topology {
        _ if sprite.is_some() => 3,
        1 => 0,
        2 => 1,
        3 => 2,
        4 => 3,
        5 => 4,
        _ => 3,
    };
    let r = rs.unwrap_or_else(|| vec![3, 3, 0, 0, 0, 0, 1, 0, 0, 0]);
    // FillMode WIREFRAME (2) of triangles: their edges as a line list of its own indices (#64)
    let wire = if r[0] == 2 && matches!(topo, 3 | 4) && sprite.is_none() { wire_edges(d, st, indexed, count, start, topo == 4) } else { None };
    let topo = if wire.is_some() { 1 } else { topo };
    let strip = if wire.is_none() && indexed && matches!(topo, 2 | 4) { if st.ib.1 == 42 { 2 } else { 1 } } else { 0 };
    let mut key = vec![vs, ps.as_ref().map_or(0, |p| p.0), topo, strip];
    // rasterizer: cull, front ccw, depth bias, slope, clamp
    // (front ccw in bit 0, DepthClipEnable off in bit 1)
    key.extend_from_slice(&[r[1], (r[2] != 0) as u32 | ((r[6] == 0) as u32) << 1, r[3], r[5], r[4]]);
    // depth stencil
    match (depth_fmt, &ds) {
        (Some(f), Some(s)) => {
            key.extend_from_slice(&[f, s[0], s[1], s[2], s[3], s[4] & 0xffff]);
            key.extend_from_slice(&s[5..13]);
        }
        (Some(f), None) => key.extend_from_slice(&[f, 1, 1, 2, 0, 0xffff, 1, 1, 1, 8, 1, 1, 1, 8]),
        _ => key.push(0xffff),
    }
    // vertex buffers
    key.push(slots.len() as u32);
    for s in &slots {
        key.extend_from_slice(&[vbx[s.0 as usize].1, s.1, s.2.len() as u32]);
        for at in &s.2 {
            key.extend_from_slice(at);
        }
    }
    // targets
    let b = blend.unwrap_or_else(|| {
        let mut v = vec![0, 0];
        for _ in 0..8 {
            v.extend_from_slice(&[0, 2, 1, 1, 2, 1, 1, 0xf]);
        }
        v
    });
    key.push(formats.len() as u32);
    for (i, f) in formats.iter().enumerate() {
        let rt = if b[1] != 0 { i } else { 0 };
        let t = &b[2 + rt * 8..2 + rt * 8 + 8];
        key.extend_from_slice(&[*f, t[0], blend_factor(t[1], false), blend_factor(t[2], false), t[3], blend_factor(t[4], true), blend_factor(t[5], true), t[6], t[7] & 0xf]);
    }
    // multisampling: AlphaToCoverageEnable, the sample mask (render::d3d pipeline2)
    key.extend_from_slice(&[b[0], st.sample_mask]);
    let pipe = match d.pipelines.get(&key) {
        Some(&p) => {
            crate::render::gpu::pipeline_hit();
            p
        }
        None => {
            let id = page_id();
            let mut c = vec![gpu::CREATE_PIPELINE2, id, key.len() as u32];
            c.extend_from_slice(&key);
            gpu::record(|q| q.cmd(&c));
            d.pipelines.insert(key, id);
            id
        }
    };
    // draw
    let mut w = vec![gpu::DRAW2, pipe];
    w.extend(st.viewport[0].iter().map(|f| f.to_bits()));
    let scissor_on = r.get(7).copied().unwrap_or(0) != 0;
    if scissor_on {
        let s = st.scissor[0];
        w.extend_from_slice(&[s[0].max(0) as u32, s[1].max(0) as u32, (s[2] - s[0]).max(0) as u32, (s[3] - s[1]).max(0) as u32, 1]);
    } else {
        w.extend_from_slice(&[0, 0, 0, 0, 0]);
    }
    w.extend(st.blend_factor.iter().map(|f| f.to_bits()));
    w.push(st.stencil_ref);
    w.push(slots.len() as u32);
    let vbs: Vec<(u32, u32)> = slots.iter().map(|s| (vbx[s.0 as usize].0, vbx[s.0 as usize].2)).collect();
    for (buf, off) in vbs {
        let g = buffer_gpu(d, buf);
        w.extend_from_slice(&[g, off]);
    }
    let wire_buf = wire.as_ref().map(|e| {
        let g = page_id();
        let bytes: Vec<u8> = e.iter().flat_map(|v| v.to_le_bytes()).collect();
        gpu::record(|q| {
            let off = q.blob(&bytes);
            q.cmd(&[gpu::CREATE_BUFFER, g, bytes.len() as u32]);
            q.cmd(&[gpu::WRITE_BUFFER, g, 0, off, bytes.len() as u32]);
        });
        g
    });
    if let Some(g) = wire_buf {
        w.extend_from_slice(&[g, 0, 2]);
    } else if indexed {
        let g = buffer_gpu(d, st.ib.0);
        w.extend_from_slice(&[g, st.ib.2, if st.ib.1 == 42 { 2 } else { 1 }]);
    } else {
        w.extend_from_slice(&[0, 0, 0]);
    }
    // bindings: group 0 vertex, 1 pixel
    let mut cbs_read = Vec::new();
    for (stage, binds) in [(0usize, &vb), (1usize, &ps.as_ref().map_or(Vec::new(), |p| p.1.clone()))] {
        let mut entries: Vec<[u32; 8]> = Vec::new();
        for (bi, kind) in binds.iter() {
            match kind {
                dxwgsl::Binding::Uniform(_) if stage == 0 && *bi == 15 && sprite.is_some() => {
                    let half = [st.viewport[0][2] * 0.5, st.viewport[0][3] * 0.5];
                    if d.sprite_view.0 == 0 || d.sprite_view.1 != half {
                        let made = d.sprite_view.0;
                        let g = if made != 0 { made } else { page_id() };
                        d.sprite_view = (g, half);
                        let mut bytes = Vec::new();
                        for f in [half[0].max(1.0), half[1].max(1.0), 0.0, 0.0] {
                            bytes.extend(f.to_le_bytes());
                        }
                        gpu::record(|q| {
                            let off = q.blob(&bytes);
                            if made == 0 {
                                q.cmd(&[gpu::CREATE_BUFFER, g, 16]);
                            }
                            q.cmd(&[gpu::WRITE_BUFFER, g, 0, off, 16]);
                        });
                    }
                    entries.push([*bi, 0, d.sprite_view.0, 0, 16, 16, 0, 0]);
                }
                dxwgsl::Binding::Uniform(size) => {
                    let cb = st.cbs[stage].get(*bi as usize).copied().unwrap_or(0);
                    let g = if cb != 0 { buffer_gpu(d, cb) } else { 0 };
                    cbs_read.push(cb);
                    let bsize = match d.objs.get(&cb).map(|o| &o.data) {
                        Some(Data::Buffer(b)) => b.size.next_multiple_of(16).max(16),
                        _ => 0,
                    };
                    entries.push([*bi, 0, g, 0, bsize.min((*size).max(16).max(bsize)), *size, 0, 0]);
                }
                dxwgsl::Binding::Texture(vdim, sample) => {
                    let t = bi - 16;
                    let sv = st.srvs[stage].get(t as usize).copied().unwrap_or(0);
                    buffer_view_sync(d, sv);
                    let e = match d.objs.get(&sv).map(|o| &o.data) {
                        Some(Data::View(v)) => match d.objs.get(&v.res).map(|o| &o.data) {
                            Some(Data::Texture(tx)) => [*bi, 1, tx.gpu, *vdim, v.mip, v.mips.min(tx.mips - v.mip.min(tx.mips - 1)).max(1), v.layer, v.layers.max(1)],
                            _ => [*bi, 1, 0, *vdim, 0, 1, 0, 1],
                        },
                        _ => [*bi, 1, 0, *vdim, 0, 1, 0, 1],
                    };
                    let mut e = e;
                    if *sample != 0 {
                        e[1] = 3 + *sample; // uint/sint/depth textures
                    }
                    // (a stencil view, X32_TYPELESS_G8X24_UINT or X24_TYPELESS_G8_UINT: not the
                    // page's float copy of the depth, which depth views read: unbound)
                    if e[1] == 1 && matches!(d.objs.get(&sv).map(|o| &o.data), Some(Data::View(v)) if matches!(v.fmt, 22 | 47)) {
                        e[2] = 0;
                    }
                    entries.push(e);
                }
                dxwgsl::Binding::Sampler(cmp) => {
                    let s = bi - 160;
                    let id = st.samplers[stage].get(s as usize).copied().unwrap_or(0);
                    let desc = state_desc(d, id).cloned().unwrap_or_else(|| vec![0x15, 3, 3, 3, 0, 1, 8, 0, 0, 0, 0, 0, 0x7f7f_ffff]);
                    let key = sampler_key(&desc, *cmp);
                    entries.push([*bi, if *cmp { 3 } else { 2 }, key, desc[11], desc[12], 0, 0, 0]);
                }
                // (compute shaders only)
                dxwgsl::Binding::Storage(..) => {}
            }
        }
        w.push(entries.len() as u32);
        for e in entries {
            w.extend_from_slice(&e);
        }
    }
    match &wire {
        // (the edges hold the vertex numbers of a draw without indices: no base then)
        Some(e) => w.extend_from_slice(&[1, e.len() as u32, instances, 0, if indexed { base as u32 } else { 0 }, first_instance]),
        None => w.extend_from_slice(&[indexed as u32, count, instances, start, base as u32, first_instance]),
    }
    // What it reads from its buffers (vertices, indices, constants), hashed: the page tells a
    // frame that draws the same as the last one, though from another place in a ring buffer
    // and with other bytes written around it (0: unknown)
    let mem = |id: u32| match d.objs.get(&id).map(|o| &o.data) {
        Some(Data::Buffer(b)) => Some((b.mem, b.size)),
        _ => None,
    };
    let mut h = 0x811c_9dc5u32;
    let mut add = |p: u32, n: u32| {
        for &b in std::slice::from_raw_parts(p as *const u8, n as usize) {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
    };
    let mut known = count <= 4096;
    let verts: Vec<u32> = if !known {
        Vec::new()
    } else if indexed {
        let isz = if st.ib.1 == 42 { 4 } else { 2 };
        match mem(st.ib.0) {
            Some((m, size)) if st.ib.2 as u64 + (start as u64 + count as u64) * isz <= size as u64 => {
                let p = m + st.ib.2 + start * isz as u32;
                (0..count as usize).map(|k| if isz == 4 { rd(p, k) } else { *(p as *const u16).add(k) as u32 }.wrapping_add(base as u32)).collect()
            }
            _ => {
                known = false;
                Vec::new()
            }
        }
    } else {
        (start..start.saturating_add(count)).collect()
    };
    for s in &slots {
        let (buf, stride, off) = vbx[s.0 as usize];
        let Some((m, size)) = mem(buf) else {
            known = false;
            break;
        };
        let at = |v: u32| off as u64 + v as u64 * stride as u64;
        let vs: Vec<u32> = if s.1 != 0 { (first_instance..first_instance.saturating_add(instances)).collect() } else { verts.clone() };
        if stride == 0 || vs.iter().any(|&v| at(v) + stride as u64 > size as u64) {
            known = false;
            break;
        }
        for v in vs {
            add(m + at(v) as u32, stride);
        }
    }
    for cb in cbs_read {
        match mem(cb) {
            Some((m, size)) => add(m, size),
            None => {}
        }
    }
    // (a wireframe draw's edges are a buffer of its own, gone after it: never the same)
    w.push(if known && wire_buf.is_none() { h | 1 } else { 0 });
    gpu::record(|q| q.cmd(&w));
    if let Some(g) = wire_buf {
        gpu::record(|q| q.cmd(&[gpu::DESTROY_BUFFER, g]));
    }
}

/// A compute shader's page shader and bindings for UAVs of these page formats (slot, format).
fn compute_shader_for(d: &mut D11, cs: u32, fmts: &[(u32, u32)]) -> Option<(u32, Vec<(u32, dxwgsl::Binding)>)> {
    let trace = d.trace;
    let Some(Data::Shader(s)) = d.objs.get_mut(&cs).map(|o| &mut o.data) else { return None };
    let key = fmts.iter().fold(0x811c_9dc5u32, |h, &(n, f)| (h ^ (n << 8 | f)).wrapping_mul(0x0100_0193));
    let key = (key, Vec::new());
    if let Some(r) = s.wgsl.get(&key) {
        return r.clone();
    }
    let names: Vec<(u32, &str)> = fmts.iter().map(|&(n, f)| (n, crate::render::d3d::TEX_FORMATS.get(f as usize).copied().unwrap_or("rgba8unorm"))).collect();
    let r = match dxwgsl::translate_compute(&s.code, &names) {
        Ok(w) => {
            let id = page_id();
            gpu::record(|q| {
                let off = q.blob(w.code.as_bytes());
                q.cmd(&[gpu::CREATE_SHADER, id, off, w.code.len() as u32]);
            });
            if trace {
                eprintln!("d3d11: compute shader {cs} -> {id} bindings {:?}", w.bindings);
                if trace2() {
                    eprintln!("{}", w.code);
                }
            }
            Some((id, w.bindings))
        }
        Err(e) => {
            eprintln!("d3d11: compute shader {cs}: {e}");
            None
        }
    };
    s.wgsl.insert(key, r.clone());
    r
}

/// Dispatch(x, y, z): the compute shader on the page (render::d3d DISPATCH), its typed UAVs as
/// storage textures. (D3D11_CS_DISPATCH_MAX_THREAD_GROUPS_PER_DIMENSION: 65535; more is
/// invalid and runs nothing)
unsafe fn dispatch(d: &mut D11, st: &State, x: u32, y: u32, z: u32) {
    let cs = st.shaders[3];
    if cs == 0 || x == 0 || y == 0 || z == 0 || x > 65535 || y > 65535 || z > 65535 {
        return;
    }
    let code = match d.objs.get(&cs).map(|o| &o.data) {
        Some(Data::Shader(s)) => s.code.clone(),
        _ => return,
    };
    // the texture behind a view: (page texture, page format)
    fn tex_of(d: &D11, v: u32) -> Option<(u32, u32, &View)> {
        let Some(Data::View(vv)) = d.objs.get(&v).map(|o| &o.data) else { return None };
        match d.objs.get(&vv.res).map(|o| &o.data) {
            Some(Data::Texture(t)) if vv.buf.is_none() => Some((t.gpu, t.page, vv)),
            _ => None,
        }
    }
    let mut fmts = Vec::new();
    for (n, _, _) in dxwgsl::uav_slots(&code).unwrap_or_default() {
        let v = st.uavs[1].get(n as usize).copied().unwrap_or(0);
        if let Some((_, page, _)) = tex_of(d, v) {
            fmts.push((n, page));
        }
    }
    let Some((sh, binds)) = compute_shader_for(d, cs, &fmts) else { return };
    let mut w = vec![gpu::DISPATCH, sh, x, y, z, binds.len() as u32];
    for (bi, kind) in &binds {
        let e = match kind {
            dxwgsl::Binding::Uniform(size) => {
                let cb = st.cbs[5].get(*bi as usize).copied().unwrap_or(0);
                let g = if cb != 0 { buffer_gpu(d, cb) } else { 0 };
                [*bi, 0, g, 0, 0, *size, 0, 0]
            }
            dxwgsl::Binding::Storage(vdim, _) => {
                let n = bi - 200;
                let v = st.uavs[1].get(n as usize).copied().unwrap_or(0);
                match tex_of(d, v) {
                    Some((g, page, vv)) => [*bi, 7, g, *vdim, vv.mip, page, vv.layer, vv.layers.max(1)],
                    // (no UAV bound: writes go nowhere, as in Direct3D; nothing to run then)
                    None => return,
                }
            }
            dxwgsl::Binding::Texture(vdim, sample) if *bi >= 300 => {
                let n = bi - 300;
                let v = st.uavs[1].get(n as usize).copied().unwrap_or(0);
                match tex_of(d, v) {
                    Some((g, _, vv)) => [*bi, 8, g, *vdim, vv.mip, *sample, vv.layer, vv.layers.max(1)],
                    None => [*bi, 8, 0, *vdim, 0, *sample, 0, 1],
                }
            }
            dxwgsl::Binding::Texture(vdim, sample) => {
                let t = bi - 16;
                let sv = st.srvs[5].get(t as usize).copied().unwrap_or(0);
                buffer_view_sync(d, sv);
                let mut e = match d.objs.get(&sv).map(|o| &o.data) {
                    Some(Data::View(v)) => match d.objs.get(&v.res).map(|o| &o.data) {
                        Some(Data::Texture(tx)) => [*bi, 1, tx.gpu, *vdim, v.mip, v.mips.min(tx.mips - v.mip.min(tx.mips - 1)).max(1), v.layer, v.layers.max(1)],
                        _ => [*bi, 1, 0, *vdim, 0, 1, 0, 1],
                    },
                    _ => [*bi, 1, 0, *vdim, 0, 1, 0, 1],
                };
                if *sample != 0 {
                    e[1] = 3 + *sample;
                }
                e
            }
            dxwgsl::Binding::Sampler(cmp) => {
                let s = bi - 160;
                let id = st.samplers[5].get(s as usize).copied().unwrap_or(0);
                let desc = state_desc(d, id).cloned().unwrap_or_else(|| vec![0x15, 3, 3, 3, 0, 1, 8, 0, 0, 0, 0, 0, 0x7f7f_ffff]);
                [*bi, if *cmp { 3 } else { 2 }, sampler_key(&desc, *cmp), desc[11], desc[12], 0, 0, 0]
            }
        };
        w.extend_from_slice(&e);
    }
    gpu::record(|q| q.cmd(&w));
    // (what the page drew to is not what kura thinks it last drew: targets set again)
    d.targets = None;
}

/// D3D11_SAMPLER_DESC as the page's sampler key: address u v w (3 bits each), mag, min, mip
/// (linear 1 bit each), anisotropic, compare function (4 bits)
fn sampler_key(s: &[u32], cmp: bool) -> u32 {
    let f = s[0];
    let addr = |a: u32| match a {
        1 => 0, // wrap
        2 => 1, // mirror
        3 => 2, // clamp
        4 | 5 => 2, // border, mirror once: clamped, the pixel shader does the rest (dxwgsl::addr)
        _ => 0,
    };
    let (mip, mag, min) = (f & 1, (f >> 2) & 1, (f >> 4) & 1);
    let aniso = (f & 0x40 != 0) as u32;
    let func = if cmp { s[6] & 15 } else { 0 };
    addr(s[1]) | addr(s[2]) << 3 | addr(s[3]) << 6 | mag << 9 | min << 10 | mip << 11 | aniso << 12 | func << 13 | (s[5].clamp(1, 16)) << 17
}

#[cfg(test)]
mod vb_merge_tests {
    use super::merge_vertex_buffers;
    use proptest::prelude::*;

    #[test]
    fn nine_slots_on_one_buffer_become_one() {
        // ANGLE: 9 float4 attributes, each its own slot on buffer 7 at offset 16*i, stride 144
        let mut vbs = [(0u32, 0u32, 0u32); 32];
        let mut slots = Vec::new();
        for i in 0..9u32 {
            vbs[i as usize] = (7, 144, 1000 + 16 * i);
            slots.push((i, 0, vec![[3, 0, i]]));
        }
        let vbx = merge_vertex_buffers(&vbs, &mut slots);
        assert_eq!(slots.len(), 1);
        assert_eq!(vbx[slots[0].0 as usize], (7, 144, 1000));
        let offs: Vec<u32> = slots[0].2.iter().map(|a| a[1]).collect();
        assert_eq!(offs, (0..9).map(|i| 16 * i).collect::<Vec<_>>());
    }

    #[test]
    fn eight_or_fewer_stay() {
        let mut vbs = [(0u32, 0u32, 0u32); 32];
        vbs[0] = (1, 16, 0);
        vbs[1] = (1, 16, 4);
        let mut slots = vec![(0, 0, vec![[0, 0, 0]]), (1, 0, vec![[0, 0, 1]])];
        merge_vertex_buffers(&vbs, &mut slots);
        assert_eq!(slots.len(), 2);
    }

    proptest! {
        /// every attribute still reads the same bytes of the same buffer, and fits its vertex
        #[test]
        fn merging_keeps_each_attribute_address(n in 9usize..16, bufs in prop::collection::vec(1u32..4, 16),
            strides in prop::collection::vec(prop::sample::select(vec![0u32, 16, 32, 64]), 16),
            offs in prop::collection::vec(0u32..64, 16), fmts in prop::collection::vec(0u32..4, 16)) {
            let mut vbs = [(0u32, 0u32, 0u32); 32];
            let mut slots = Vec::new();
            for i in 0..n {
                vbs[i] = (bufs[i], strides[i], offs[i]);
                slots.push((i as u32, 0, vec![[fmts[i], 0, i as u32]]));
            }
            let want: Vec<(u32, u32, u32, u32)> = (0..n).map(|i| (i as u32, bufs[i], strides[i], offs[i])).collect();
            let vbx = merge_vertex_buffers(&vbs, &mut slots);
            let mut got = Vec::new();
            for (s, _, attrs) in &slots {
                let (b, st, o) = vbx[*s as usize];
                for a in attrs {
                    let limit = if st == 0 { 2048 } else { st };
                    prop_assert!(a[1] + super::vertex_size(a[0]) <= limit || attrs.len() == 1);
                    got.push((a[2], b, st, o + a[1]));
                }
            }
            got.sort();
            prop_assert_eq!(got, want);
        }
    }
}

#[cfg(test)]
mod changed_range_tests {
    use super::changed_range;
    use proptest::prelude::*;

    #[test]
    fn cases() {
        assert_eq!(changed_range(&[0; 16], &[0; 16]), None);
        assert_eq!(changed_range(&[], &[]), None);
        let mut b = [0u8; 16];
        b[5] = 1;
        assert_eq!(changed_range(&[0; 16], &b), Some((4, 8)));
        b[15] = 1;
        assert_eq!(changed_range(&[0; 16], &b), Some((4, 16)));
        // (a length not a multiple of 4: the end stops at it)
        let mut c = [0u8; 10];
        c[9] = 2;
        assert_eq!(changed_range(&[0; 10], &c), Some((8, 10)));
        c[0] = 2;
        assert_eq!(changed_range(&[0; 10], &c), Some((0, 10)));
    }

    proptest! {
        /// Copying the range from `new` onto `old` gives `new`; the range is word-aligned and
        /// starts and ends within a word of the first and last difference.
        #[test]
        fn covers_every_difference(old in proptest::collection::vec(0u8..3, 0..300), flips in proptest::collection::vec((0usize..300, 1u8..3), 0..6)) {
            let mut new = old.clone();
            for (at, x) in flips {
                if at < new.len() {
                    new[at] ^= x;
                }
            }
            let diffs: Vec<usize> = (0..old.len()).filter(|&i| old[i] != new[i]).collect();
            match changed_range(&old, &new) {
                None => prop_assert!(diffs.is_empty()),
                Some((lo, hi)) => {
                    prop_assert!(!diffs.is_empty());
                    prop_assert!(lo % 4 == 0 && (hi % 4 == 0 || hi == new.len()) && lo < hi && hi <= new.len());
                    let (first, last) = (diffs[0], *diffs.last().unwrap());
                    prop_assert!(lo <= first && first < lo + 4, "lo {} first {}", lo, first);
                    prop_assert!(last < hi && hi <= (last + 4) & !3 || hi == new.len(), "hi {} last {}", hi, last);
                    let mut patched = old.clone();
                    patched[lo..hi].copy_from_slice(&new[lo..hi]);
                    prop_assert_eq!(patched, new);
                }
            }
        }
    }
}

#[cfg(test)]
mod wire_tests {
    use super::triangle_edges;

    #[test]
    fn edges_of_lists_and_strips() {
        assert_eq!(triangle_edges(&[0, 1, 2, 3, 4, 5, 6], false, None), [0, 1, 1, 2, 2, 0, 3, 4, 4, 5, 5, 3]);
        assert_eq!(triangle_edges(&[0, 1, 2, 3], true, None), [0, 1, 1, 2, 2, 0, 1, 2, 2, 3, 3, 1]);
        // a strip cut by the restart index
        assert_eq!(triangle_edges(&[0, 1, 2, 0xffff, 5, 6, 7], true, Some(0xffff)), [0, 1, 1, 2, 2, 0, 5, 6, 6, 7, 7, 5]);
        assert!(triangle_edges(&[0, 1], true, None).is_empty());
    }

    proptest::proptest! {
        /// Every triangle gives three edges that chain back to its first vertex
        #[test]
        fn three_closed_edges_a_triangle(idx in proptest::collection::vec(0u32..50, 0..40), strip: bool) {
            let e = triangle_edges(&idx, strip, None);
            let tris = if strip { idx.len().saturating_sub(2) } else { idx.len() / 3 };
            proptest::prop_assert_eq!(e.len(), tris * 6);
            for t in e.chunks(6) {
                proptest::prop_assert_eq!((t[1], t[3], t[5]), (t[2], t[4], t[0]));
            }
        }
    }
}
