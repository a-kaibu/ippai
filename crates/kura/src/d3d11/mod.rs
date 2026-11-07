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
pub mod format;
pub mod names;
#[cfg(test)]
mod trace_tests;
#[cfg(test)]
mod map_tests;

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

// IIDs (first dword) and the interfaces that answer them
const IIDS: &[(u32, &[u32])] = &[
    (0x0000_0000, &[]), // IUnknown: any
    (0xaec22fb8, &[IF_FACTORY, IF_ADAPTER, IF_OUTPUT, IF_SWAPCHAIN, IF_DXGIDEVICE, IF_SURFACE]), // IDXGIObject
    (0x7b7166ec, &[IF_FACTORY]),
    (0x770aae78, &[IF_FACTORY]),
    (0x50c83a1c, &[IF_FACTORY]),
    (0x2411e7e1, &[IF_ADAPTER]),
    (0x29038f61, &[IF_ADAPTER]),
    (0xae02eedb, &[IF_OUTPUT]),
    (0x310d36a0, &[IF_SWAPCHAIN]),
    (0x790a45f7, &[IF_SWAPCHAIN]),
    (0x54ec77fa, &[IF_DXGIDEVICE]),
    (0x77db970f, &[IF_DXGIDEVICE]),
    (0x05008617, &[IF_DXGIDEVICE]), // IDXGIDevice2
    (0x6007896c, &[IF_DXGIDEVICE]), // IDXGIDevice3
    (0xcafcb56c, &[IF_SURFACE]),
    (0x4ae63092, &[IF_SURFACE]),
    (0x035f3ab4, &[IF_SURFACE]), // IDXGIResource (close enough: GetParent/GetDevice)
    (0x3d3e0379, &[IF_SWAPCHAIN, IF_SURFACE]),
    (0xdb6f6ddb, &[IF_DEVICE]),
    (0xa04bfb29, &[IF_DEVICE]), // ID3D11Device1
    (0x9d06dffa, &[IF_DEVICE]), // ID3D11Device2
    (0xbb2c6faa, &[IF_CONTEXT]), // ID3D11DeviceContext1
    (0x420d5b32, &[IF_CONTEXT]), // ID3D11DeviceContext2
    (0xcc86fabe, &[IF_BLEND]), // ID3D11BlendState1
    (0x1217d7a6, &[IF_RASTER]), // ID3D11RasterizerState1
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
    matches!((primary, want), (IF_DEVICE, IF_DXGIDEVICE) | (IF_TEX2D, IF_SURFACE) | (IF_DEVICE | IF_CONTEXT, IF_MULTITHREAD))
        // (a D3D11 device has no ID3D10Device face to ask for: D3D10CreateDevice makes it)
        || (primary != IF_DEVICE && d3d10::ten(primary) == Some(want))
}

pub struct Buffer {
    pub size: u32,
    pub usage: u32,
    pub bind: u32,
    pub mem: u32,
    /// page buffer with the current contents (0: none yet)
    pub gpu: u32,
    /// the guest wrote since `gpu` was made
    pub dirty: bool,
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
    pub gpu: u32,
    /// mapped subresources: (subresource, guest memory, pitch, write)
    pub maps: Vec<(u32, u32, u32, bool)>,
}

#[derive(Clone)]
pub struct View {
    pub res: u32,
    pub fmt: u32,
    /// shader view dimension (dxwgsl::Binding::Texture's numbering)
    pub vdim: u32,
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
    pub wgsl: HashMap<u32, Option<(u32, Vec<(u32, dxwgsl::Binding)>)>>,
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
}

pub enum Data {
    Factory,
    Adapter,
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
    vbs: [(u32, u32, u32); 16],
    ib: (u32, u32, u32),
    layout: u32,
    topology: u32,
    vs: u32,
    ps: u32,
    cbs: [[u32; 15]; 2],
    srvs: [[u32; 128]; 2],
    samplers: [[u32; 16]; 2],
    rtvs: [u32; 8],
    dsv: u32,
    viewport: [f32; 6],
    scissor: [i32; 4],
    blend: u32,
    blend_factor: [f32; 4],
    depth: u32,
    stencil_ref: u32,
    raster: u32,
    /// SetPredication (predicate, value): kept for GetPredication, draws are not predicated
    predicate: (u32, u32),
}

pub struct D11 {
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
    output: u32,
    st: State,
    /// the ID3DDeviceContextState whose state `st` is (0: the context's own)
    st_owner: u32,
    pipelines: HashMap<Vec<u32>, u32>,
    /// page targets set in this batch generation
    targets: Option<(u64, Vec<u32>)>,
    next_gpu: u32,
}

unsafe impl Send for D11 {}

static D11S: Mutex<Option<D11>> = Mutex::new(None);

/// What a call does after the lock is dropped.
enum After {
    None,
    Present,
    Readback(u32),
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
        // (the device, its context and the DXGI objects live as long as the process)
        if matches!(self.objs.get(&id).map(|o| o.iface), Some(IF_DEVICE | IF_CONTEXT | IF_FACTORY | IF_ADAPTER | IF_OUTPUT) | None) {
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
                output: 0,
                st: default_state(),
                st_owner: 0,
                pipelines: HashMap::new(),
                targets: None,
                next_gpu: 0,
            })
        }
    }
    0
}

/// Entry from d3d9's dispatcher: ids INIT.. and interfaces 64+.
pub unsafe fn call(id: u32, args: *const u32, calls: *const HostCalls) -> u32 {
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
        After::Present => gpu::present(),
        After::Readback(req) => {
            gpu::wait_readback(req);
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
    let want = match IIDS.iter().chain(d3d10::IIDS10).find(|x| x.0 == first) {
        Some((0, _)) => primary,
        Some((_, ifs)) => {
            if ifs.contains(&iface) {
                iface
            } else if ifs.contains(&primary) {
                primary
            } else if let Some(&f) = ifs.iter().find(|&&f| other_face(primary, f) || (d3d10::eleven(iface).is_some() && d3d10::ten(primary) == Some(f))) {
                f
            } else {
                0
            }
        }
        None => 0,
    };
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
        let a = d.new_obj(IF_ADAPTER, Data::Adapter);
        d.adapter = d.id_of(a);
        let o = d.new_obj(IF_OUTPUT, Data::Output);
        d.output = d.id_of(o);
        // (the adapter and the output: only the application's references count, as in Windows'
        // "Adapter has N references left")
        for id in [d.adapter, d.output] {
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
        }
        d.add_ref(d.device);
    }
    wr(level_out, 0, level);
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
        let sc = new_swapchain(d, p[11], p[0], p[1], p[4], p[9]);
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

/// D3D10CreateDevice(AndSwapChain)(1): the D3D11 device's D3D10 face
unsafe fn create_device10(d: &mut D11, args: *const u32) -> u32 {
    let a = |i: usize| *args.add(i);
    let dev_out = a(8);
    let mut dev = [0u32; 2];
    let mut level = 0u32;
    let v = [a(0), a(1), a(2), a(3), 0, 0, 7, a(6), a(7), dev.as_mut_ptr() as usize as u32, &mut level as *mut u32 as usize as u32, 0];
    let r = create_device(d, v.as_ptr());
    if r != S_OK {
        return r;
    }
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

/// Lets go of everything a state binds (ClearState, a new device).
fn unbind_state(d: &mut D11, old: &State) {
    let mut held: Vec<u32> = Vec::new();
    held.extend(old.vbs.iter().map(|v| v.0));
    held.extend([old.ib.0, old.layout, old.vs, old.ps, old.dsv, old.blend, old.depth, old.raster, old.predicate.0]);
    for s in 0..2 {
        held.extend(old.cbs[s]);
        held.extend(old.srvs[s]);
        held.extend(old.samplers[s]);
    }
    held.extend(old.rtvs);
    for h in held.into_iter().filter(|&h| h != 0) {
        d.unbind(h);
    }
}

fn default_state() -> State {
    State {
        vbs: [(0, 0, 0); 16],
        ib: (0, 0, 0),
        layout: 0,
        topology: 0,
        vs: 0,
        ps: 0,
        cbs: [[0; 15]; 2],
        srvs: [[0; 128]; 2],
        samplers: [[0; 16]; 2],
        rtvs: [0; 8],
        dsv: 0,
        viewport: [0.0; 6],
        scissor: [0; 4],
        blend: 0,
        blend_factor: [1.0; 4],
        depth: 0,
        stencil_ref: 0,
        raster: 0,
        predicate: (0, 0),
    }
}

unsafe fn new_swapchain(d: &mut D11, hwnd: u32, w: u32, h: u32, fmt: u32, count: u32) -> u32 {
    let _ = count;
    let mut rect = [0i32; 4];
    (d.calls().client_rect)(hwnd, rect.as_mut_ptr());
    let w = if w == 0 { (rect[2] - rect[0]).max(1) as u32 } else { w };
    let h = if h == 0 { (rect[3] - rect[1]).max(1) as u32 } else { h };
    let fmt = if format::info(fmt).is_some() { fmt } else { 28 };
    let buffer = new_texture(d, 2, w, h, 1, 1, fmt, 0, 0x8 | 0x20, false);
    let bid = d.id_of(buffer);
    d.add_ref(d.device);
    let sc = d.new_obj(IF_SWAPCHAIN, Data::SwapChain(SwapChain { device: d.device, hwnd, w, h, fmt, buffer: bid, presents: 0 }));
    if d.trace {
        eprintln!("d3d11: swap chain {w}x{h} format {fmt} window {hwnd:#x}");
    }
    sc
}

/// A texture object (address).
fn new_texture(d: &mut D11, dim: u32, w: u32, h: u32, layers: u32, mips: u32, fmt: u32, usage: u32, bind: u32, cube: bool) -> u32 {
    let page = format::info(fmt).map_or(format::P_BGRA8, |f| f.page);
    let max = 32 - w.max(h).max(1).leading_zeros();
    let mips = if mips == 0 { max } else { mips.min(max) };
    let gpu = page_id();
    let layers = layers.max(1);
    gpu::record(|q| q.cmd(&[gpu::CREATE_TEXTURE2, gpu, w.max(1), h.max(1), layers, mips, page, match dim {
        3 => 1,
        1 => 2,
        _ => 0,
    }]));
    let iface = match dim {
        1 => IF_TEX1D,
        3 => IF_TEX3D,
        _ => IF_TEX2D,
    };
    d.new_obj(iface, Data::Texture(Texture { dim, w, h, layers, mips, fmt, page, usage, bind, cube, gpu, maps: Vec::new() }))
}

/// Uploads a subresource from guest memory (`pitch` bytes per row) to the page texture.
unsafe fn upload_texture(t: &Texture, sub: u32, mem: u32, pitch: u32, bx: Option<[u32; 6]>) {
    if t.gpu == 0 || format::is_depth(t.page) || mem == 0 {
        return;
    }
    let (level, layer) = (sub % t.mips, sub / t.mips);
    let (lw, lh) = ((t.w >> level).max(1), (t.h >> level).max(1));
    let [x0, y0, z0, x1, y1, z1] = bx.unwrap_or([0, 0, 0, lw, lh, 1]);
    let Some(f) = format::info(t.fmt) else { return };
    let (x0, y0) = (x0 / f.block * f.block, y0 / f.block * f.block);
    let (w, h) = (x1.min(lw).saturating_sub(x0), y1.min(lh).saturating_sub(y0));
    if w == 0 || h == 0 {
        return;
    }
    let src = mem as *const u8;
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
    for z in z0..z1.max(z0 + 1) {
        let lz = if t.dim == 3 { z } else { layer };
        gpu::record(|q| {
            let off = q.blob(&out);
            q.cmd(&[gpu::WRITE_TEXTURE2, t.gpu, level, lz, x0, y0, w, h, bpr, off]);
        });
        if t.dim != 3 {
            break;
        }
    }
}

unsafe fn method(d: &mut D11, iface: u32, m: u32, this: u32, args: *const u32, after: &mut After) -> u32 {
    let a = |i: usize| *args.add(i);
    match iface {
        // IDXGIObject: SetPrivateData, SetPrivateDataInterface, GetPrivateData
        IF_FACTORY | IF_ADAPTER | IF_OUTPUT | IF_SWAPCHAIN | IF_DXGIDEVICE | IF_SURFACE if (3..=5).contains(&m) => private_data(d, [5, 6, 4][m as usize - 3], this, &a),
        IF_FACTORY => factory_call(d, m, this, &a),
        IF_ADAPTER => match m {
            6 => parent(d, d.factory, a(1), a(2)),
            7 => {
                // EnumOutputs
                if a(1) != 0 {
                    wrp(a(2), 0, 0);
                    return DXGI_ERROR_NOT_FOUND;
                }
                d.add_ref(d.output);
                wrp(a(2), 0, d.addr_of(d.output));
                S_OK
            }
            8 | 10 if a(1) == 0 => E_INVALIDARG,
            8 | 10 => {
                // GetDesc/GetDesc1: Description[128] WCHAR, ids, memory sizes
                let n = match (m, ptr()) {
                    (8, 4) => 292,
                    (_, 4) => 296,
                    (8, _) => 304,
                    _ => 312,
                };
                std::ptr::write_bytes(a(1) as *mut u8, 0, n);
                for (i, c) in "vnvm WebGPU".encode_utf16().enumerate() {
                    *((a(1) + i as u32 * 2) as *mut u16) = c;
                }
                wr(a(1) + 256, 0, 0x10de); // VendorId
                wr(a(1) + 256, 1, 0x1c82);
                wrp(a(1) + 272, 0, 512 << 20); // DedicatedVideoMemory (SIZE_T)
                wrp(a(1) + 272, 2, 1024 << 20); // SharedSystemMemory
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
            _ => E_NOTIMPL,
        },
        IF_OUTPUT => output_call(d, m, &a),
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
                    let o = new_texture(d, 2, w, h, 1, 1, fmt, 0, bind, false);
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
        IF_SURFACE => match m {
            6 | 7 => parent(d, d.device, a(1), a(2)),
            8 => {
                // GetDesc: Width Height Format SampleDesc
                let Some(Data::Texture(t)) = d.objs.get(&this).map(|o| &o.data) else { return E_FAIL };
                for (i, v) in [t.w, t.h, t.fmt, 1, 0].into_iter().enumerate() {
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
            _ => E_NOTIMPL,
        },
        IF_DEVICE => device_call(d, m, this, &a),
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

unsafe fn factory_call(d: &mut D11, m: u32, _this: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    match m {
        3..=5 => S_OK,
        6 => E_NOINTERFACE,
        7 | 12 => {
            if a(1) != 0 {
                wrp(a(2), 0, 0);
                return DXGI_ERROR_NOT_FOUND;
            }
            d.add_ref(d.adapter);
            wrp(a(2), 0, d.addr_of(d.adapter));
            S_OK
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
            let sc = new_swapchain(d, p[11], p[0], p[1], p[4], p[9]);
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
            let sc = new_swapchain(d, a(2), rd(desc, 0), rd(desc, 1), rd(desc, 2), rd(desc, 7));
            wrp(a(6), 0, sc);
            if sc == 0 { E_FAIL } else { S_OK }
        }
        16 | 24 => DXGI_ERROR_UNSUPPORTED,
        17..=23 => E_NOTIMPL,
        _ => E_NOTIMPL,
    }
}

const MODES: [(u32, u32); 9] = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 960), (1280, 1024), (1366, 768), (1600, 900), (1920, 1080)];

unsafe fn output_call(d: &mut D11, m: u32, a: &dyn Fn(usize) -> u32) -> u32 {
    match m {
        6 => parent(d, d.adapter, a(1), a(2)),
        7 => {
            // GetDesc: DeviceName[32] WCHAR, DesktopCoordinates, AttachedToDesktop, Rotation, Monitor
            let p = a(1);
            std::ptr::write_bytes(p as *mut u8, 0, 96);
            for (i, c) in "\\\\.\\DISPLAY1".encode_utf16().enumerate() {
                *((p + i as u32 * 2) as *mut u16) = c;
            }
            let (mut w, mut h) = (0, 0);
            crate::display::vnhost_display_size(&mut w, &mut h);
            wr(p + 64, 2, w as u32);
            wr(p + 64, 3, h as u32);
            wr(p + 64, 4, 1);
            wr(p + 64, 5, 1);
            wr(p + 64, 6, 1); // HMONITOR: win32u numbers monitors from 1
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
            std::thread::sleep(std::time::Duration::from_millis(16));
            S_OK
        }
        11..=18 => S_OK,
        _ => E_NOTIMPL,
    }
}

unsafe fn swapchain_call(d: &mut D11, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let Some(Data::SwapChain(sc)) = d.objs.get(&this).map(|o| &o.data) else { return E_FAIL };
    let (device, hwnd, w, h, fmt, buffer) = (sc.device, sc.hwnd, sc.w, sc.h, sc.fmt, sc.buffer);
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
            }
            *after = After::Present;
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
            put_swap_desc(a(1), [w, h, 60, 1, fmt, 0, 0, 1, 0, 0x20, 1, hwnd, 1, 0, 0]);
            S_OK
        }
        13 => {
            // ResizeBuffers(count, w, h, format, flags)
            let mut rect = [0i32; 4];
            (d.calls().client_rect)(hwnd, rect.as_mut_ptr());
            let nw = if a(2) == 0 { (rect[2] - rect[0]).max(1) as u32 } else { a(2) };
            let nh = if a(3) == 0 { (rect[3] - rect[1]).max(1) as u32 } else { a(3) };
            let nf = if a(4) == 0 { fmt } else { a(4) };
            let nb = new_texture(d, 2, nw, nh, 1, 1, nf, 0, 0x8 | 0x20, false);
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
            for (k, v) in [w, h, fmt, 0, 1, 0, 0x20, 1, 0, 0, 0, 0].into_iter().enumerate() {
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
            let (size, usage, bind) = (rd(desc, 0), rd(desc, 1), rd(desc, 2));
            if size == 0 || (usage == 1 && a(2) == 0) || (a(2) != 0 && rdp(a(2), 0) == 0) {
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
            let o = d.new_obj(IF_BUFFER, Data::Buffer(Buffer { size, usage, bind, mem, gpu: 0, dirty: true }));
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
            let (dim, w, h, layers, mips, fmt, usage, bind, misc) = match m {
                4 => (1, rd(desc, 0), 1, rd(desc, 2), rd(desc, 1), rd(desc, 3), rd(desc, 4), rd(desc, 5), rd(desc, 7)),
                5 => (2, rd(desc, 0), rd(desc, 1), rd(desc, 3), rd(desc, 2), rd(desc, 4), rd(desc, 7), rd(desc, 8), rd(desc, 10)),
                _ => (3, rd(desc, 0), rd(desc, 1), rd(desc, 2), rd(desc, 3), rd(desc, 4), rd(desc, 5), rd(desc, 6), rd(desc, 8)),
            };
            if format::info(fmt).is_none() {
                if d.trace {
                    eprintln!("d3d11: texture format {fmt} not supported");
                }
                return E_INVALIDARG;
            }
            // (empty sizes, no samples, an immutable texture without its texels, initial data
            // with a missing level)
            let samples = if m == 5 { rd(desc, 5) } else { 1 };
            if w == 0 || h == 0 || layers == 0 || samples == 0 || (usage == 1 && a(2) == 0) {
                return E_INVALIDARG;
            }
            if a(2) != 0 {
                let max = 32 - w.max(h).max(if dim == 3 { layers } else { 1 }).leading_zeros();
                let n = if mips == 0 { max } else { mips.min(max) } * if dim == 3 { 1 } else { layers };
                if (0..n).any(|sub| rdp(a(2) + sub * (ptr() + 8), 0) == 0) {
                    return E_INVALIDARG;
                }
            }
            if a(3) == 0 {
                return S_FALSE;
            }
            let o = new_texture(d, dim, w, h, layers, mips, fmt, usage, bind, misc & 4 != 0);
            let id = d.id_of(o);
            if a(2) != 0 {
                let Some(Data::Texture(t)) = d.objs.get(&id).map(|o| &o.data) else { return E_FAIL };
                for sub in 0..t.mips * if dim == 3 { 1 } else { t.layers } {
                    // D3D11_SUBRESOURCE_DATA { pSysMem, SysMemPitch, SysMemSlicePitch }
                    let init = a(2) + sub * (ptr() + 8);
                    upload_texture(t, sub, rdp(init, 0), rd(init + ptr(), 0), None);
                }
            }
            wrp(a(3), 0, o);
            S_OK
        }
        7..=10 => create_view(d, m, a(1), a(2), a(3)),
        11 => {
            // CreateInputLayout(elems, n, code, len, out)
            let mut v = Vec::new();
            let mut next = [0u32; 16];
            for i in 0..a(2) {
                // D3D11_INPUT_ELEMENT_DESC { SemanticName, then six UINTs }
                let e = a(1) + i * (ptr() + 24);
                let name = std::ffi::CStr::from_ptr(rdp(e, 0) as *const std::ffi::c_char).to_string_lossy().into_owned();
                let f = e + ptr();
                let (index, fmt, slot, off, class) = (rd(f, 0), rd(f, 1), rd(f, 2) & 15, rd(f, 3), rd(f, 4));
                let off = if off == 0xffff_ffff { next[slot as usize] } else { off };
                let size = format::vertex_format(fmt).map_or(4, vertex_size);
                next[slot as usize] = off + size;
                v.push(Elem { name, index, fmt, slot, offset: off, instance: class == 1 });
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
            if a(4) == 0 {
                return S_FALSE;
            }
            let o = d.new_obj(iface, Data::Shader(Shader { code, pixel: iface == IF_PS, wgsl: HashMap::new() }));
            wrp(a(4), 0, o);
            S_OK
        }
        14 => {
            // CreateGeometryShaderWithStreamOutput(..., out at 9)
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
            let desc: Vec<u32> = (0..n).map(|i| rd(a(1), i)).collect();
            if a(2) == 0 {
                return S_FALSE;
            }
            // (one object per description while it lives, as Windows gives back)
            let same = d.objs.iter().find(|(_, o)| o.iface == iface && matches!(&o.data, Data::State(x) if *x == desc)).map(|(&id, _)| id);
            if let Some(id) = same {
                d.add_ref(id);
                wrp(a(2), 0, d.addr_of(id));
                return S_OK;
            }
            let o = d.new_obj(iface, Data::State(desc));
            wrp(a(2), 0, o);
            S_OK
        }
        24 | 25 => {
            let t = if a(1) != 0 { rd(a(1), 0) } else { 0 };
            if a(2) == 0 {
                return S_FALSE;
            }
            let o = d.new_obj(IF_QUERY, Data::Query(t, 0, 0));
            wrp(a(2), 0, o);
            S_OK
        }
        26 => E_INVALIDARG,
        27 => DXGI_ERROR_INVALID_CALL, // deferred contexts
        28 => E_NOTIMPL,
        29 => {
            // CheckFormatSupport(format, out)
            let bits = format::support(a(1));
            wr(a(2), 0, bits);
            if bits != 0 { S_OK } else { E_FAIL }
        }
        30 => {
            wr(a(3), 0, if a(2) == 1 { 1 } else { 0 });
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
            std::ptr::write_bytes(a(2) as *mut u8, 0, a(3) as usize);
            if (a(1) == 0 || a(1) == 4) && a(3) >= 4 {
                wr(a(2), 0, 1);
            }
            S_OK
        }
        34..=36 => private_data(d, m - 30, this, a),
        37 => 0xb000,
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
        43 | 50 => device_call(d, 40, this, a),
        44 | 51 => DXGI_ERROR_INVALID_CALL, // deferred contexts
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
            if a(2) == 0 {
                return S_FALSE;
            }
            let o = d.new_obj(IF_BLEND, Data::State(desc));
            wrp(a(2), 0, o);
            S_OK
        }
        46 => device_call(d, 22, this, a), // CreateRasterizerState1 (ForcedSampleCount ignored)
        47 => {
            // CreateDeviceContextState(flags, levels, count, sdk, iid, level_out, state_out)
            if a(6) != 0 {
                wr(a(6), 0, 0xb000);
            }
            if a(7) == 0 {
                return S_FALSE;
            }
            let o = d.new_obj(IF_LINKAGE, Data::CtxState(Some(Box::new(default_state()))));
            wrp(a(7), 0, o);
            S_OK
        }
        48 | 49 => E_NOTIMPL,
        52 => 0,
        53 => {
            // CheckMultisampleQualityLevels1(format, count, flags, out)
            wr(a(4), 0, if a(2) == 1 { 1 } else { 0 });
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
    if m == 7 && desc != 0 && matches!(rd(desc, 1), 1 | 11) {
        if let Some(Data::Buffer(b)) = d.objs.get(&rid).map(|o| &o.data) {
            return create_buffer_srv(d, rid, b.size, desc, out);
        }
    }
    let Some(Data::Texture(t)) = d.objs.get(&rid).map(|o| &o.data) else {
        // (other buffer views are not supported)
        if out != 0 {
            let o = d.new_obj([IF_SRV, IF_UAV, IF_RTV, IF_DSV][m as usize - 7], Data::Other);
            wrp(out, 0, o);
            return S_OK;
        }
        return E_INVALIDARG;
    };
    let (tdim, tmips, tlayers, tfmt, cube) = (t.dim, t.mips, t.layers, t.fmt, t.cube);
    let mut v = View {
        res: rid,
        fmt: tfmt,
        vdim: match tdim {
            1 => 0,
            3 => 5,
            _ if cube => 3,
            _ if tlayers > 1 => 2,
            _ => 1,
        },
        mip: 0,
        mips: tmips,
        layer: 0,
        layers: tlayers,
        buf: None,
    };
    if desc != 0 {
        let fmt = rd(desc, 0);
        if fmt != 0 {
            v.fmt = fmt;
        }
        let vd = rd(desc, 1);
        let u = |i: usize| rd(desc, 2 + i + (m == 10) as usize);
        match m {
            7 => {
                // SRV: dimension, then (most detailed mip, mips[, first slice, slices])
                v.mip = u(0);
                v.mips = if u(1) == 0xffff_ffff { tmips - v.mip } else { u(1) };
                match vd {
                    2 | 4 | 8 => {}
                    3 | 5 => {
                        v.layer = u(2);
                        v.layers = u(3);
                    }
                    9 => v.vdim = 3,
                    10 => v.vdim = 4,
                    _ => {}
                }
                v.vdim = match vd {
                    2 | 3 => 0,
                    4 => 1,
                    5 => 2,
                    9 => 3,
                    10 => 4,
                    8 => 5,
                    _ => v.vdim,
                };
            }
            9 => {
                // RTV: (mip slice[, first slice, slices])
                v.mip = u(0);
                v.mips = 1;
                if matches!(vd, 3 | 5 | 8) {
                    v.layer = u(1);
                    v.layers = u(2);
                }
            }
            10 => {
                // DSV (after Flags): (mip slice[, first slice, slices])
                v.mip = u(0);
                v.mips = 1;
                if matches!(vd, 2 | 4) {
                    v.layer = u(1);
                    v.layers = u(2);
                }
            }
            _ => {}
        }
    } else if m != 7 {
        v.mips = 1;
    }
    if out == 0 {
        return S_FALSE;
    }
    d.bind(rid);
    let o = d.new_obj([IF_SRV, IF_UAV, IF_RTV, IF_DSV][m as usize - 7], Data::View(v));
    wrp(out, 0, o);
    S_OK
}

/// CreateShaderResourceView of a buffer (BUFFER or BUFFEREX: format, dimension, first element,
/// elements): a view of a one-row texture with the elements (at most 8192, WebGPU's width limit)
unsafe fn create_buffer_srv(d: &mut D11, rid: u32, size: u32, desc: u32, out: u32) -> u32 {
    let (fmt, first, n) = (rd(desc, 0), rd(desc, 2), rd(desc, 3));
    let Some(f) = format::info(fmt).filter(|f| f.block == 1) else { return E_INVALIDARG };
    let n = n.min(size.saturating_sub(first * f.bpp) / f.bpp.max(1)).clamp(1, 8192);
    if out == 0 {
        return S_FALSE;
    }
    let tex = new_texture(d, 2, n, 1, 1, 1, fmt, 0, 8, false);
    let tid = d.id_of(tex);
    // (the view's own texture and the buffer: held privately, see `destroy`)
    d.bind(tid);
    d.release(tid);
    d.bind(rid);
    let buf = Some(BufSrc { buf: rid, off: first * f.bpp, bytes: n * f.bpp, last: Vec::new() });
    let v = View { res: tid, fmt, vdim: 1, mip: 0, mips: 1, layer: 0, layers: 1, buf };
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
    if let Some(Data::Texture(t)) = d.objs.get(&v.res).map(|o| &o.data) {
        unsafe { upload_texture(t, 0, now.as_ptr() as u32, b.bytes, None) };
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
            for (i, v) in [b.size, b.usage, b.bind, if b.usage == 2 { 0x10000 } else { 0 }, 0, 0].into_iter().enumerate() {
                wr(a(1), i, v);
            }
            S_OK
        }
        (IF_TEX1D | IF_TEX2D | IF_TEX3D, 10, Some(Data::Texture(t))) => {
            let cpu = match t.usage {
                2 => 0x10000,
                3 => 0x30000,
                _ => 0,
            };
            let misc = if t.cube { 4 } else { 0 };
            let v: Vec<u32> = match iface {
                IF_TEX1D => vec![t.w, t.mips, t.layers, t.fmt, t.usage, t.bind, cpu, misc],
                IF_TEX2D => vec![t.w, t.h, t.mips, t.layers, t.fmt, 1, 0, t.usage, t.bind, cpu, misc],
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
            let vd = match (iface, v.vdim) {
                (IF_SRV, 0) => 2,
                (IF_SRV, 1) => 4,
                (IF_SRV, 2) => 5,
                (IF_SRV, 3) => 9,
                (IF_SRV, 4) => 10,
                (IF_SRV, _) => 8,
                (IF_DSV, 2) => 4,
                (IF_DSV, _) => 3,
                (_, 2) => 5,
                (_, 5) => 8,
                _ => 4,
            };
            let mut w = vec![v.fmt, vd];
            if iface == IF_DSV {
                w.push(0);
            }
            // (the union: SRV MostDetailedMip MipLevels FirstArraySlice ArraySize; RTV DSV UAV
            // MipSlice FirstArraySlice ArraySize. RTV and UAV descs are 20 bytes, DSV 24, SRV 24)
            if iface == IF_SRV {
                w.extend_from_slice(&[v.mip, v.mips, v.layer, v.layers]);
            } else {
                w.extend_from_slice(&[v.mip, v.layer, v.layers]);
            }
            for (i, x) in w.into_iter().enumerate() {
                wr(a(1), i, x);
            }
            S_OK
        }
        // states: GetDesc
        (IF_BLEND | IF_DEPTH | IF_RASTER | IF_SAMPLER, 7, Some(Data::State(s))) => {
            std::ptr::copy_nonoverlapping(s.as_ptr(), a(1) as *mut u32, s.len());
            S_OK
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
        7 => 4,     // SO_OVERFLOW_PREDICATE
        12..=15 => 4, // SO_OVERFLOW_PREDICATE_STREAM0..3
        _ => 16,    // SO_STATISTICS(_STREAM0..3)
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
        8 | 25 => set_slots(d, &mut st.srvs[(m == 8) as usize], a(1), a(2), a(3)),
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
                if s >= 16 {
                    break;
                }
                let new = if a(3) == 0 { 0 } else { d.id_of(rdp(a(3), i as usize)) };
                let old = st.vbs[s].0;
                st.vbs[s] = (new, if a(4) != 0 { rd(a(4), i as usize) } else { 0 }, if a(5) != 0 { rd(a(5), i as usize) } else { 0 });
                d.rebind(new, old);
            }
        }
        19 => {
            let new = d.id_of(a(1));
            let old = st.ib.0;
            st.ib = (new, a(2), a(3));
            d.rebind(new, old);
        }
        20 => draw(d, st, true, a(1), a(2).max(1), a(3), a(4) as i32, a(5)),
        21 => draw(d, st, false, a(1), a(2).max(1), a(3), 0, a(4)),
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
            if a(2) == 0 {
                return if a(3) == 0 { S_OK } else { E_INVALIDARG };
            }
            if a(3) != query_size(t) {
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
            // OMSetRenderTargets(n, rtvs, dsv) (+ UAVs, ignored)
            let n = if m == 34 && a(1) == 0xffff_ffff { u32::MAX } else { a(1) };
            if n != u32::MAX {
                let mut rt = st.rtvs;
                set_slots(d, &mut rt, 0, 8, 0);
                set_slots(d, &mut rt, 0, n.min(8), a(2));
                st.rtvs = rt;
                set_one(d, &mut st.dsv, a(3));
                // the viewport follows the first target when the game never set one
                if st.viewport[2] == 0.0 {
                    if let Some((w, h)) = view_size(d, st.rtvs[0]).or_else(|| view_size(d, st.dsv)) {
                        st.viewport = [0.0, 0.0, w as f32, h as f32, 0.0, 1.0];
                    }
                }
            }
        }
        35 => {
            set_one(d, &mut st.blend, a(1));
            st.blend_factor = if a(2) != 0 { [rdf(a(2), 0), rdf(a(2), 1), rdf(a(2), 2), rdf(a(2), 3)] } else { [1.0; 4] };
        }
        36 => {
            set_one(d, &mut st.depth, a(1));
            st.stencil_ref = a(2);
        }
        43 => set_one(d, &mut st.raster, a(1)),
        44 => {
            if a(1) > 0 && a(2) != 0 {
                for i in 0..6 {
                    st.viewport[i] = rdf(a(2), i);
                }
                if d.trace {
                    eprintln!("d3d11: viewport {:?}", st.viewport);
                }
            }
        }
        45 => {
            if a(1) > 0 && a(2) != 0 {
                for i in 0..4 {
                    st.scissor[i] = rd(a(2), i) as i32;
                }
            }
        }
        46 => copy_region(d, a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8)),
        47 => copy_resource(d, a(1), a(2)),
        48 => update_subresource(d, a(1), a(2), a(3), a(4), a(5)),
        50 => {
            // ClearRenderTargetView(view, color)
            let v = d.id_of(a(1));
            if let Some((tex, level, layer)) = view_target(d, v) {
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
                gpu::record(|q| q.cmd(&[gpu::CLEAR_VIEW, tex, level, layer, 1, c[0], c[1], c[2], c[3], 0, 0]));
                d.targets = None;
            }
        }
        53 => {
            // ClearDepthStencilView(view, flags, depth, stencil)
            let v = d.id_of(a(1));
            if let Some((tex, level, layer)) = view_target(d, v) {
                gpu::record(|q| q.cmd(&[gpu::CLEAR_VIEW, tex, level, layer, (a(2) & 3) << 1, 0, 0, 0, 0, a(3), a(4) & 0xff]));
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
        22 | 23 | 31 | 32 | 37 | 49 | 51 | 52 | 55 | 57 | 58 | 59..=71 => {}
        38..=42 => {}
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
            // other stages' shaders: none
            wrp(a(1), 0, 0);
            if a(3) != 0 {
                wr(a(3), 0, 0);
            }
        }
        81 | 87 | 88 | 97..=109 => {
            // other stages: nothing bound
            for i in 0..a(2) {
                wrp(a(3), i as usize, 0);
            }
        }
        83 => wr(a(1), 0, st.topology),
        89 | 90 => {
            if a(2) != 0 {
                get_slots(d, &st.rtvs, 0, a(1), a(2));
            }
            if a(3) != 0 {
                get_one(d, st.dsv, a(3));
            }
        }
        91 => {
            get_one(d, st.blend, a(1));
            if a(2) != 0 {
                for i in 0..4 {
                    wr(a(2), i, st.blend_factor[i].to_bits());
                }
            }
            wr(a(3), 0, 0xffff_ffff);
        }
        92 => {
            get_one(d, st.depth, a(1));
            wr(a(2), 0, st.stencil_ref);
        }
        93 => {
            for i in 0..a(1) {
                wrp(a(2), i as usize, 0);
            }
        }
        94 => get_one(d, st.raster, a(1)),
        95 => {
            if a(1) != 0 {
                if a(2) != 0 && rd(a(1), 0) > 0 {
                    for i in 0..6 {
                        wr(a(2), i, st.viewport[i].to_bits());
                    }
                }
                wr(a(1), 0, 1);
            }
        }
        96 => {
            if a(1) != 0 {
                if a(2) != 0 && rd(a(1), 0) > 0 {
                    for i in 0..4 {
                        wr(a(2), i, st.scissor[i] as u32);
                    }
                }
                wr(a(1), 0, 1);
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
        132 => {} // ClearView
        // ID3D11DeviceContext2: tiled resources and annotations
        134..=139 | 141..=143 => {}
        140 => return 0, // IsAnnotationEnabled
        _ => return E_NOTIMPL,
    }
    S_OK
}

/// SwapDeviceContextState(state, prev_out): `st` (the current state) goes back into the object
/// it came from (or a new one for the context's own state) and `state`'s is loaded.
unsafe fn swap_state(d: &mut D11, st: &mut State, state: u32, prev_out: u32) {
    let new = d.id_of(state);
    let Some(Data::CtxState(slot)) = d.objs.get_mut(&new).map(|o| &mut o.data) else { return };
    let Some(loaded) = slot.take() else { return }; // (already current)
    let cur = std::mem::replace(st, *loaded);
    let old = d.st_owner;
    d.st_owner = new;
    if let Some(Data::CtxState(slot)) = d.objs.get_mut(&old).map(|o| &mut o.data) {
        *slot = Some(Box::new(cur));
        if prev_out != 0 {
            d.add_ref(old);
            wrp(prev_out, 0, d.addr_of(old));
        }
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

/// Map(resource, subresource, type, flags, out); `dxgi`: IDXGISurface::Map's DXGI_MAPPED_RECT
unsafe fn map(d: &mut D11, res: u32, sub: u32, ty: u32, out: u32, dxgi: bool, after: &mut After) -> u32 {
    let id = d.id_of(res);
    let mut req = 0;
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
            b.dirty = true;
            put_mapped(out, b.mem, b.size, b.size, dxgi);
            S_OK
        }
        Some(Data::Texture(t)) => {
            let level = sub % t.mips.max(1);
            let (lw, lh) = ((t.w >> level).max(1), (t.h >> level).max(1));
            let (pitch, rows) = format::pitch_rows(t.fmt, lw, lh);
            let depth = if t.dim == 3 { (t.layers >> level).max(1) } else { 1 };
            let size = pitch * rows * depth;
            let write = ty != 1;
            let mem = match t.maps.iter().position(|m| m.0 == sub) {
                Some(i) => {
                    t.maps[i].3 = write;
                    t.maps[i].1
                }
                None => 0,
            };
            let mem = if mem != 0 {
                mem
            } else {
                let p = (*d.calls).alloc;
                let mem = p(size.max(16)) as u32;
                if let Some(Data::Texture(t)) = d.objs.get_mut(&id).map(|o| &mut o.data) {
                    t.maps.push((sub, mem, pitch, write));
                }
                mem
            };
            if let Some(Data::Texture(t)) = d.objs.get(&id).map(|o| &o.data) {
                // reading or writing part: the page's pixels (formats the page keeps as they are: the page writes
                // lw * its bpp bytes a row, past the end of a smaller guest copy, e.g. A8 or BC)
                let same = match format::info(t.fmt) {
                    Some(f) => f.bpp == format::page_bpp(t.page) && f.block == 1 && f.conv == 0,
                    None => format::page_bpp(t.page) == 4,
                };
                // (D3D11_MAP_WRITE keeps what is there too: a part written goes back with the
                // rest as the page has it, a copy into the texture included)
                if ty != 4 && same && t.dim != 3 {
                    d.next_gpu += 1;
                    req = 0x4000_0000 | d.next_gpu;
                    let (g, l) = (t.gpu, level);
                    gpu::record(|q| q.cmd(&[gpu::READBACK, req, g, l, 0, 0, lw, lh, mem, pitch]));
                }
            }
            put_mapped(out, mem, pitch, pitch * rows, dxgi);
            S_OK
        }
        _ => E_INVALIDARG,
    };
    if req != 0 {
        *after = After::Readback(req);
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
    if let Some(Data::Texture(t)) = d.objs.get(&id).map(|o| &o.data) {
        if let Some(&(_, mem, pitch, write)) = t.maps.iter().find(|m| m.0 == sub) {
            if write {
                upload_texture(t, sub, mem, pitch, None);
            }
        }
    }
}

/// UpdateSubresource(resource, subresource, box, data, row pitch, depth pitch)
unsafe fn update_subresource(d: &mut D11, res: u32, sub: u32, bx: u32, data: u32, pitch: u32) {
    let id = d.id_of(res);
    let b = if bx != 0 { Some([rd(bx, 0), rd(bx, 1), rd(bx, 2), rd(bx, 3), rd(bx, 4), rd(bx, 5)]) } else { None };
    match d.objs.get_mut(&id).map(|o| &mut o.data) {
        Some(Data::Buffer(buf)) => {
            let (x0, x1) = b.map_or((0, buf.size), |b| (b[0], b[3].min(buf.size)));
            if x1 > x0 {
                std::ptr::copy(data as *const u8, (buf.mem + x0) as *mut u8, (x1 - x0) as usize);
                buf.dirty = true;
                if d.trace && buf.size <= 256 {
                    let f: Vec<f32> = (0..buf.size / 4).map(|i| *((buf.mem + 4 * i) as *const f32)).collect();
                    eprintln!("d3d11: buffer {id} = {f:?}");
                }
            }
        }
        Some(Data::Texture(t)) => {
            // the box's texels start at `data`
            let t: &Texture = t;
            match b {
                // (upload_texture reads the box's rows from `data` on: the box's first texel)
                Some(bb) => upload_texture(t, sub, data, pitch, Some(bb)),
                None => upload_texture(t, sub, data, pitch, None),
            }
        }
        _ => {}
    }
}

unsafe fn copy_resource(d: &mut D11, dst: u32, src: u32) {
    let (di, si) = (d.id_of(dst), d.id_of(src));
    match (d.objs.get(&di).map(|o| &o.data), d.objs.get(&si).map(|o| &o.data)) {
        (Some(Data::Buffer(a)), Some(Data::Buffer(b))) => {
            std::ptr::copy(b.mem as *const u8, a.mem as *mut u8, a.size.min(b.size) as usize);
            if let Some(Data::Buffer(a)) = d.objs.get_mut(&di).map(|o| &mut o.data) {
                a.dirty = true;
            }
        }
        (Some(Data::Texture(a)), Some(Data::Texture(b))) => {
            let mut cmds = Vec::new();
            for l in 0..a.mips.min(b.mips) {
                let (w, h) = ((b.w >> l).max(1), (b.h >> l).max(1));
                cmds.push([gpu::COPY, b.gpu, l, 0, 0, w, h, a.gpu, l, 0, 0, w, h, 0]);
            }
            gpu::record(|q| {
                for c in &cmds {
                    q.cmd(c);
                }
            });
            d.targets = None;
        }
        _ => {}
    }
}

/// CopySubresourceRegion(dst, dst sub, x, y, z, src, src sub, box)
unsafe fn copy_region(d: &mut D11, dst: u32, dsub: u32, x: u32, y: u32, _z: u32, src: u32, ssub: u32, bx: u32) {
    let (di, si) = (d.id_of(dst), d.id_of(src));
    match (d.objs.get(&di).map(|o| &o.data), d.objs.get(&si).map(|o| &o.data)) {
        (Some(Data::Buffer(a)), Some(Data::Buffer(b))) => {
            let (s0, s1) = if bx != 0 { (rd(bx, 0), rd(bx, 3)) } else { (0, b.size) };
            let n = s1.min(b.size).saturating_sub(s0).min(a.size.saturating_sub(x));
            std::ptr::copy((b.mem + s0) as *const u8, (a.mem + x) as *mut u8, n as usize);
            if let Some(Data::Buffer(a)) = d.objs.get_mut(&di).map(|o| &mut o.data) {
                a.dirty = true;
            }
        }
        (Some(Data::Texture(a)), Some(Data::Texture(b))) => {
            let (sl, dl) = (ssub % b.mips, dsub % a.mips);
            let (bw, bh) = ((b.w >> sl).max(1), (b.h >> sl).max(1));
            let (x0, y0, x1, y1) = if bx != 0 { (rd(bx, 0), rd(bx, 1), rd(bx, 3), rd(bx, 4)) } else { (0, 0, bw, bh) };
            let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
            let c = [gpu::COPY, b.gpu, sl, x0, y0, w, h, a.gpu, dl, x, y, w, h, 0];
            gpu::record(|q| q.cmd(&c));
            d.targets = None;
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// draws

/// A shader's page shader and bindings (translated on first use; vertex shaders per pixel shader).
fn shader_for(d: &mut D11, sh: u32, ps: u32) -> Option<(u32, Vec<(u32, dxwgsl::Binding)>)> {
    let ps_code = match d.objs.get(&ps).map(|o| &o.data) {
        Some(Data::Shader(s)) => Some(s.code.clone()),
        _ => None,
    };
    let trace = d.trace;
    let Some(Data::Shader(s)) = d.objs.get_mut(&sh).map(|o| &mut o.data) else { return None };
    let key = if s.pixel { 0 } else { ps };
    if let Some(r) = s.wgsl.get(&key) {
        return r.clone();
    }
    let r = match dxwgsl::translate(&s.code, s.pixel, if s.pixel { None } else { ps_code.as_deref() }) {
        Ok(w) => {
            let id = page_id();
            gpu::record(|q| {
                let off = q.blob(w.code.as_bytes());
                q.cmd(&[gpu::CREATE_SHADER, id, off, w.code.len() as u32]);
            });
            if trace {
                eprintln!("d3d11: {} shader {sh} -> {} bindings {:?}", if s.pixel { "pixel" } else { "vertex" }, id, w.bindings);
                if std::env::var("IPPAI_D3D_TRACE").as_deref() == Ok("2") {
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

#[allow(clippy::too_many_arguments)]
unsafe fn draw(d: &mut D11, st: &State, indexed: bool, count: u32, instances: u32, start: u32, base: i32, first_instance: u32) {
    if count == 0 || st.vs == 0 {
        return;
    }
    let Some((vs, vb)) = shader_for(d, st.vs, st.ps) else { return };
    let ps = if st.ps != 0 { shader_for(d, st.ps, 0) } else { None };
    if std::env::var("IPPAI_D3D_TRACE").as_deref() == Ok("2") {
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
                slots.push((e.3, e.5 as u32, Vec::new()));
                slots.len() - 1
            }
        };
        slots[i].2.push([vf, e.4, inp.reg]);
    }
    slots.sort_by_key(|s| s.0);
    let blend = state_desc(d, st.blend).cloned();
    let ds = state_desc(d, st.depth).cloned();
    let rs = state_desc(d, st.raster).cloned();
    let topo = match st.topology {
        1 => 0,
        2 => 1,
        3 => 2,
        4 => 3,
        5 => 4,
        _ => 3,
    };
    let strip = if indexed && matches!(topo, 2 | 4) { if st.ib.1 == 42 { 2 } else { 1 } } else { 0 };
    let mut key = vec![vs, ps.as_ref().map_or(0, |p| p.0), topo, strip];
    // rasterizer: cull, front ccw, depth bias, slope, clamp
    let r = rs.unwrap_or_else(|| vec![3, 3, 0, 0, 0, 0, 1, 0, 0, 0]);
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
        key.extend_from_slice(&[st.vbs[s.0 as usize].1, s.1, s.2.len() as u32]);
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
    w.extend(st.viewport.iter().map(|f| f.to_bits()));
    let scissor_on = r.get(7).copied().unwrap_or(0) != 0;
    if scissor_on {
        let s = st.scissor;
        w.extend_from_slice(&[s[0].max(0) as u32, s[1].max(0) as u32, (s[2] - s[0]).max(0) as u32, (s[3] - s[1]).max(0) as u32, 1]);
    } else {
        w.extend_from_slice(&[0, 0, 0, 0, 0]);
    }
    w.extend(st.blend_factor.iter().map(|f| f.to_bits()));
    w.push(st.stencil_ref);
    w.push(slots.len() as u32);
    let vbs: Vec<(u32, u32)> = slots.iter().map(|s| (st.vbs[s.0 as usize].0, st.vbs[s.0 as usize].2)).collect();
    for (buf, off) in vbs {
        let g = buffer_gpu(d, buf);
        w.extend_from_slice(&[g, off]);
    }
    if indexed {
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
                    entries.push(e);
                }
                dxwgsl::Binding::Sampler(cmp) => {
                    let s = bi - 160;
                    let id = st.samplers[stage].get(s as usize).copied().unwrap_or(0);
                    let desc = state_desc(d, id).cloned().unwrap_or_else(|| vec![0x15, 3, 3, 3, 0, 1, 8, 0, 0, 0, 0, 0, 0x7f7f_ffff]);
                    let key = sampler_key(&desc, *cmp);
                    entries.push([*bi, if *cmp { 3 } else { 2 }, key, desc[11], desc[12], 0, 0, 0]);
                }
            }
        }
        w.push(entries.len() as u32);
        for e in entries {
            w.extend_from_slice(&e);
        }
    }
    w.extend_from_slice(&[indexed as u32, count, instances, start, base as u32, first_instance]);
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
        let (buf, stride, off) = st.vbs[s.0 as usize];
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
    w.push(if known { h | 1 } else { 0 });
    gpu::record(|q| q.cmd(&w));
}

/// D3D11_SAMPLER_DESC as the page's sampler key: address u v w (3 bits each), mag, min, mip
/// (linear 1 bit each), anisotropic, compare function (4 bits)
fn sampler_key(s: &[u32], cmp: bool) -> u32 {
    let f = s[0];
    let addr = |a: u32| match a {
        1 => 0, // wrap
        2 => 1, // mirror
        3 => 2, // clamp
        4 => 2, // border (no border colors in WebGPU)
        5 => 1,
        _ => 0,
    };
    let (mip, mag, min) = (f & 1, (f >> 2) & 1, (f >> 4) & 1);
    let aniso = (f & 0x40 != 0) as u32;
    let func = if cmp { s[6] & 15 } else { 0 };
    addr(s[1]) | addr(s[2]) << 3 | addr(s[3]) << 6 | mag << 9 | min << 10 | mip << 11 | aniso << 12 | func << 13 | (s[5].clamp(1, 16)) << 17
}
