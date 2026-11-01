//! Direct3D 9 for Wine's guest programs, drawn with the page's WebGPU.
//!
//! The PE side (dlls/vnd3d9 in the Wine patch) turns every COM method call into
//! `vnhost_d3d9_call(id, args)`: `id` is interface << 8 | vtable index, `args` points at the
//! guest's stack arguments (`this` first). Objects are { vtable, id } blocks in guest memory that
//! this module allocates (the PE side hands over its vtables once).
//!
//! The device's state lives here; draws become WebGPU commands (`gpu`) for the page, with
//! shaders translated to WGSL (`shader`, fixed function in `ff`).

pub mod device;
pub mod ff;
pub mod format;
pub mod gpu;
pub mod shader;

use std::collections::HashMap;
use std::sync::Mutex;

pub const S_OK: u32 = 0;
pub const E_NOINTERFACE: u32 = 0x8000_4002;
pub const E_NOTIMPL: u32 = 0x8000_4001;
pub const E_OUTOFMEMORY: u32 = 0x8007_000e;
pub const D3DERR_INVALIDCALL: u32 = 0x8876_086c;
pub const D3DERR_NOTAVAILABLE: u32 = 0x8876_086a;
pub const D3DERR_NOTFOUND: u32 = 0x8876_0866;
pub const S_FALSE: u32 = 1;

// interfaces (the PE side's vtable numbers)
pub const IF_D3D9: u32 = 1;
pub const IF_DEVICE: u32 = 2;
pub const IF_SWAPCHAIN: u32 = 3;
pub const IF_SURFACE: u32 = 4;
pub const IF_TEXTURE: u32 = 5;
pub const IF_CUBE: u32 = 6;
pub const IF_VOLUMETEX: u32 = 7;
pub const IF_VB: u32 = 8;
pub const IF_IB: u32 = 9;
pub const IF_DECL: u32 = 10;
pub const IF_VS: u32 = 11;
pub const IF_PS: u32 = 12;
pub const IF_STATEBLOCK: u32 = 13;
pub const IF_QUERY: u32 = 14;
pub const IF_VOLUME: u32 = 15;

/// What the unix side provides (dlls/vnd3d9/unixlib.c).
#[repr(C)]
pub struct HostCalls {
    pub alloc: unsafe extern "C" fn(u32) -> *mut u8,
    pub free: unsafe extern "C" fn(*mut u8),
    pub client_rect: unsafe extern "C" fn(u32, *mut i32) -> i32,
}

pub struct Obj {
    pub iface: u32,
    pub refs: u32,
    /// references the device holds (bound as texture, stream, ...)
    pub binds: u32,
    /// the object public references go to (a texture's surfaces: the texture)
    pub parent: u32,
    pub addr: u32,
    pub data: Data,
    pub private: Vec<([u8; 16], Vec<u8>)>,
}

pub enum Data {
    D3d,
    Device,
    SwapChain,
    Surface(device::Surface),
    Texture(device::Texture),
    Buffer(device::Buffer),
    Decl(device::Decl),
    Shader(device::ShaderObj),
    StateBlock(Box<device::Snapshot>),
    Query(u32),
    Other,
}

pub struct D3d {
    pub vtbls: Vec<u32>,
    pub objs: HashMap<u32, Obj>,
    next: u32,
    /// free 16-byte object blocks in guest memory
    free_blocks: Vec<u32>,
    pub dev: Option<Box<device::Device>>,
    pub calls: *const HostCalls,
    pub trace: bool,
    pub counts: HashMap<u32, u32>,
}

unsafe impl Send for D3d {}

pub static D3D: Mutex<Option<D3d>> = Mutex::new(None);

/// What a call does once the lock is dropped.
pub enum After {
    None,
    Present,
    Readback(u32),
}

impl D3d {
    pub fn calls(&self) -> &HostCalls {
        unsafe { &*self.calls }
    }

    pub fn guest_alloc(&self, size: u32) -> u32 {
        unsafe { (self.calls().alloc)(size) as u32 }
    }

    pub fn guest_free(&self, p: u32) {
        if p != 0 {
            unsafe { (self.calls().free)(p as *mut u8) }
        }
    }

    /// A new object; returns its guest address.
    pub fn new_obj(&mut self, iface: u32, parent: u32, data: Data) -> u32 {
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
        self.next += 1;
        let id = self.next;
        unsafe {
            *(addr as *mut u32) = self.vtbls[iface as usize];
            *((addr + 4) as *mut u32) = id;
        }
        self.objs.insert(id, Obj { iface, refs: 1, binds: 0, parent, addr, data, private: Vec::new() });
        addr
    }

    /// The object id at a guest address (0: none/not ours).
    pub fn id_of(&self, addr: u32) -> u32 {
        if addr == 0 {
            return 0;
        }
        let id = unsafe { *((addr + 4) as *const u32) };
        if self.objs.get(&id).is_some_and(|o| o.addr == addr) { id } else { 0 }
    }

    pub fn addr_of(&self, id: u32) -> u32 {
        self.objs.get(&id).map_or(0, |o| o.addr)
    }

    /// The object whose references count for `id` (itself or its container).
    fn owner(&self, id: u32) -> u32 {
        match self.objs.get(&id) {
            Some(o) if o.parent != 0 => o.parent,
            _ => id,
        }
    }

    pub fn add_ref(&mut self, id: u32) -> u32 {
        let id = self.owner(id);
        match self.objs.get_mut(&id) {
            Some(o) => {
                o.refs += 1;
                o.refs
            }
            None => 0,
        }
    }

    pub fn release(&mut self, id: u32) -> u32 {
        let id = self.owner(id);
        let Some(o) = self.objs.get_mut(&id) else { return 0 };
        o.refs = o.refs.saturating_sub(1);
        let r = o.refs;
        if r == 0 && o.binds == 0 {
            self.destroy(id);
        }
        r
    }

    pub fn bind(&mut self, id: u32) {
        let id = self.owner(id);
        if let Some(o) = self.objs.get_mut(&id) {
            o.binds += 1;
        }
    }

    pub fn unbind(&mut self, id: u32) {
        let id = self.owner(id);
        let Some(o) = self.objs.get_mut(&id) else { return };
        o.binds = o.binds.saturating_sub(1);
        if o.refs == 0 && o.binds == 0 {
            self.destroy(id);
        }
    }

    fn destroy(&mut self, id: u32) {
        let Some(o) = self.objs.remove(&id) else { return };
        self.free_blocks.push(o.addr);
        unsafe { *((o.addr + 4) as *mut u32) = 0 };
        match o.data {
            Data::Texture(t) => {
                for l in t.levels {
                    self.destroy(l);
                }
                if t.gpu != 0 {
                    gpu::record(|b| b.cmd(&[gpu::DESTROY_TEXTURE, t.gpu]));
                }
            }
            Data::Surface(s) => {
                self.guest_free(s.mem);
                if s.gpu != 0 && s.own_gpu {
                    gpu::record(|b| b.cmd(&[gpu::DESTROY_TEXTURE, s.gpu]));
                }
            }
            Data::Buffer(b) => self.guest_free(b.mem),
            Data::Device => {
                if let Some(dev) = self.dev.take() {
                    dev.release_all(self);
                }
            }
            _ => {}
        }
    }
}

fn with<R>(f: impl FnOnce(&mut D3d) -> R) -> R {
    let mut g = D3D.lock().unwrap_or_else(|e| e.into_inner());
    f(g.as_mut().expect("d3d9 not initialized"))
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_d3d9_init(vtbls: *const u32, count: u32) -> u32 {
    let v = std::slice::from_raw_parts(vtbls, count as usize).to_vec();
    let mut g = D3D.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(D3d {
            vtbls: v,
            objs: HashMap::new(),
            next: 0,
            free_blocks: Vec::new(),
            dev: None,
            calls: std::ptr::null(),
            trace: std::env::var_os("VNVM_D3D_TRACE").is_some(),
            counts: HashMap::new(),
        });
    } else if let Some(d) = g.as_mut() {
        d.vtbls = v;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_d3d9_create(sdk: u32, out: *mut u32, calls: *const HostCalls) -> u32 {
    with(|d| {
        d.calls = calls;
        if d.trace {
            eprintln!("d3d9: Direct3DCreate9({sdk:#x})");
        }
        *out = d.new_obj(IF_D3D9, 0, Data::D3d);
        if *out == 0 { E_OUTOFMEMORY } else { 0 }
    })
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_d3d9_call(id: u32, args: *const u32, calls: *const HostCalls) -> u32 {
    let (r, after) = {
        let mut g = D3D.lock().unwrap_or_else(|e| e.into_inner());
        let d = g.as_mut().expect("d3d9 not initialized");
        d.calls = calls;
        let iface = id >> 8;
        let method = id & 0xff;
        let a = |i: usize| *args.add(i);
        let this = d.id_of(a(0));
        let mut after = After::None;
        let r = if this == 0 && method != 0xf0 {
            if d.trace {
                eprintln!("d3d9: {} on a stale object {:#x}", name(iface, method), a(0));
            }
            D3DERR_INVALIDCALL
        } else {
            match method {
                0 => query_interface(d, this, iface, a(1) as *const u8, a(2) as *mut u32),
                1 => d.add_ref(this),
                2 => d.release(this),
                _ => device::call(d, iface, method, this, args, &mut after),
            }
        };
        if d.trace {
            *d.counts.entry(id).or_default() += 1;
            let n = d.counts[&id];
            if n <= 20 || n.is_power_of_two() {
                let nargs = (1..6).map(|i| format!("{:#x}", a(i))).collect::<Vec<_>>().join(",");
                eprintln!("d3d9: {} [{n}] ({nargs}) -> {:#x}", name(iface, method), r);
            }
        }
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

// IIDs by their first 32 bits
const IID_IUNKNOWN: u32 = 0x00000000;
const IID_D3D9: u32 = 0x81bdcbca;
const IID_DEVICE: u32 = 0xd0223b96;
const IID_RESOURCE: u32 = 0x05eec05d;
const IID_BASETEX: u32 = 0x580ca87e;
const IID_TEXTURE: u32 = 0x85c31227;
const IID_CUBE: u32 = 0xfff32f81;
const IID_SURFACE: u32 = 0x0cfbaf3a;
const IID_VB: u32 = 0xb64bb1b5;
const IID_IB: u32 = 0x7c9dd65e;
const IID_SWAPCHAIN: u32 = 0x794950f2;

unsafe fn query_interface(d: &mut D3d, id: u32, iface: u32, riid: *const u8, out: *mut u32) -> u32 {
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let first = (riid as *const u32).read_unaligned();
    let ok = match first {
        IID_IUNKNOWN => true,
        IID_D3D9 => iface == IF_D3D9,
        IID_DEVICE => iface == IF_DEVICE,
        IID_RESOURCE => matches!(iface, IF_SURFACE | IF_TEXTURE | IF_CUBE | IF_VOLUMETEX | IF_VB | IF_IB),
        IID_BASETEX => matches!(iface, IF_TEXTURE | IF_CUBE | IF_VOLUMETEX),
        IID_TEXTURE => iface == IF_TEXTURE,
        IID_CUBE => iface == IF_CUBE,
        IID_SURFACE => iface == IF_SURFACE,
        IID_VB => iface == IF_VB,
        IID_IB => iface == IF_IB,
        IID_SWAPCHAIN => iface == IF_SWAPCHAIN,
        _ => false,
    };
    if !ok {
        *out = 0;
        if d.trace {
            eprintln!("d3d9: QueryInterface({first:08x}) on interface {iface}: no");
        }
        return E_NOINTERFACE;
    }
    d.add_ref(id);
    *out = d.addr_of(id);
    S_OK
}

pub fn name(iface: u32, m: u32) -> String {
    const UNK: [&str; 3] = ["QueryInterface", "AddRef", "Release"];
    const RES: [&str; 8] =
        ["GetDevice", "SetPrivateData", "GetPrivateData", "FreePrivateData", "SetPriority", "GetPriority", "PreLoad", "GetType"];
    const D3D9: [&str; 14] = [
        "RegisterSoftwareDevice", "GetAdapterCount", "GetAdapterIdentifier", "GetAdapterModeCount", "EnumAdapterModes",
        "GetAdapterDisplayMode", "CheckDeviceType", "CheckDeviceFormat", "CheckDeviceMultiSampleType",
        "CheckDepthStencilMatch", "CheckDeviceFormatConversion", "GetDeviceCaps", "GetAdapterMonitor", "CreateDevice",
    ];
    const DEV: [&str; 116] = [
        "TestCooperativeLevel", "GetAvailableTextureMem", "EvictManagedResources", "GetDirect3D", "GetDeviceCaps",
        "GetDisplayMode", "GetCreationParameters", "SetCursorProperties", "SetCursorPosition", "ShowCursor",
        "CreateAdditionalSwapChain", "GetSwapChain", "GetNumberOfSwapChains", "Reset", "Present", "GetBackBuffer",
        "GetRasterStatus", "SetDialogBoxMode", "SetGammaRamp", "GetGammaRamp", "CreateTexture", "CreateVolumeTexture",
        "CreateCubeTexture", "CreateVertexBuffer", "CreateIndexBuffer", "CreateRenderTarget",
        "CreateDepthStencilSurface", "UpdateSurface", "UpdateTexture", "GetRenderTargetData", "GetFrontBufferData",
        "StretchRect", "ColorFill", "CreateOffscreenPlainSurface", "SetRenderTarget", "GetRenderTarget",
        "SetDepthStencilSurface", "GetDepthStencilSurface", "BeginScene", "EndScene", "Clear", "SetTransform",
        "GetTransform", "MultiplyTransform", "SetViewport", "GetViewport", "SetMaterial", "GetMaterial", "SetLight",
        "GetLight", "LightEnable", "GetLightEnable", "SetClipPlane", "GetClipPlane", "SetRenderState",
        "GetRenderState", "CreateStateBlock", "BeginStateBlock", "EndStateBlock", "SetClipStatus", "GetClipStatus",
        "GetTexture", "SetTexture", "GetTextureStageState", "SetTextureStageState", "GetSamplerState",
        "SetSamplerState", "ValidateDevice", "SetPaletteEntries", "GetPaletteEntries", "SetCurrentTexturePalette",
        "GetCurrentTexturePalette", "SetScissorRect", "GetScissorRect", "SetSoftwareVertexProcessing",
        "GetSoftwareVertexProcessing", "SetNPatchMode", "GetNPatchMode", "DrawPrimitive", "DrawIndexedPrimitive",
        "DrawPrimitiveUP", "DrawIndexedPrimitiveUP", "ProcessVertices", "CreateVertexDeclaration",
        "SetVertexDeclaration", "GetVertexDeclaration", "SetFVF", "GetFVF", "CreateVertexShader", "SetVertexShader",
        "GetVertexShader", "SetVertexShaderConstantF", "GetVertexShaderConstantF", "SetVertexShaderConstantI",
        "GetVertexShaderConstantI", "SetVertexShaderConstantB", "GetVertexShaderConstantB", "SetStreamSource",
        "GetStreamSource", "SetStreamSourceFreq", "GetStreamSourceFreq", "SetIndices", "GetIndices",
        "CreatePixelShader", "SetPixelShader", "GetPixelShader", "SetPixelShaderConstantF", "GetPixelShaderConstantF",
        "SetPixelShaderConstantI", "GetPixelShaderConstantI", "SetPixelShaderConstantB", "GetPixelShaderConstantB",
        "DrawRectPatch", "DrawTriPatch", "DeletePatch", "CreateQuery",
    ];
    const SC: [&str; 7] =
        ["Present", "GetFrontBufferData", "GetBackBuffer", "GetRasterStatus", "GetDisplayMode", "GetDevice", "GetPresentParameters"];
    const SURF: [&str; 6] = ["GetContainer", "GetDesc", "LockRect", "UnlockRect", "GetDC", "ReleaseDC"];
    const BASE: [&str; 6] = ["SetLOD", "GetLOD", "GetLevelCount", "SetAutoGenFilterType", "GetAutoGenFilterType", "GenerateMipSubLevels"];
    const TEX: [&str; 5] = ["GetLevelDesc", "GetSurfaceLevel", "LockRect", "UnlockRect", "AddDirtyRect"];
    const BUF: [&str; 3] = ["Lock", "Unlock", "GetDesc"];
    let ifname = [
        "?", "IDirect3D9", "Device", "SwapChain", "Surface", "Texture", "CubeTexture", "VolumeTexture", "VertexBuffer",
        "IndexBuffer", "VertexDeclaration", "VertexShader", "PixelShader", "StateBlock", "Query", "Volume",
    ]
    .get(iface as usize)
    .copied()
    .unwrap_or("?");
    let m = m as usize;
    let n: Option<&str> = if m < 3 {
        Some(UNK[m])
    } else {
        match iface {
            IF_D3D9 => D3D9.get(m - 3).copied(),
            IF_DEVICE => DEV.get(m - 3).copied(),
            IF_SWAPCHAIN => SC.get(m - 3).copied(),
            IF_SURFACE | IF_TEXTURE | IF_CUBE | IF_VOLUMETEX | IF_VB | IF_IB if m < 11 => RES.get(m - 3).copied(),
            IF_SURFACE => SURF.get(m - 11).copied(),
            IF_TEXTURE | IF_CUBE | IF_VOLUMETEX if m < 17 => BASE.get(m - 11).copied(),
            IF_TEXTURE | IF_CUBE | IF_VOLUMETEX => TEX.get(m - 17).copied(),
            IF_VB | IF_IB => BUF.get(m - 11).copied(),
            _ => None,
        }
    };
    format!("{}::{}", ifname, n.map_or(format!("#{m}"), |s| s.to_string()))
}
