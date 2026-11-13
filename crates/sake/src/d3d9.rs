//! d3d9.dll: kura's Direct3D 9 (crates/kura/src/d3d9, drawn with the page's WebGPU), the
//! same module Wine's dlls/vnd3d9 hands its calls to. As there, objects are { vtable, id }
//! blocks the host allocates in guest memory; here every vtable entry is a trap whose handler
//! passes the guest's stack arguments to `vnhost_d3d9_call(interface << 8 | index, args)`.
//! x64: the vtables hold 8-byte pointers and the arguments go to the host as 8-byte slots (RCX,
//! RDX, R8, R9, then the stack; a float in XMM1-3 taken from there), as kura reads them.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::api::{host_fn, implement, Ctx};
use crate::mem::{self, ps, putp, wp};
use kura::d3d9::HostCalls;

/// Per interface (1..=15, Wine's vnd3d9 numbering, generated from its main.c), each method's
/// argument count with `this`.
const METHODS: [&[u32]; 15] = [
    // (+ IDirect3D9Ex's 5)
    &[3, 1, 1, 2, 1, 4, 3, 5, 3, 6, 7, 7, 6, 5, 4, 2, 7, 3, 5, 4, 8, 3],
    &[3, 1, 1, 1, 1, 1, 2, 2, 3, 2, 4, 4, 2, 3, 3, 1, 2, 5, 5, 3, 2, 4, 3, 9, 10, 8, 7, 7, 9, 9, 5, 3, 3, 3, 6, 4, 7, 3, 3, 2, 2, 1, 1, 7, 3, 3, 3, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 2, 2, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 2, 2, 2, 2, 2, 1, 2, 1, 4, 7, 5, 9, 7, 3, 2, 2, 2, 2, 3, 2, 2, 4, 4, 4, 4, 4, 4, 5, 5, 3, 3, 2, 2, 3, 2, 2, 4, 4, 4, 4, 4, 4, 4, 4, 2, 3, 5, 9, 6, 2, 2, 2, 3, 2, 2, 2, 10, 8, 10, 3, 4],
    &[3, 1, 1, 6, 2, 4, 2, 2, 2, 2],
    &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 3, 2, 4, 1, 2, 2],
    &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 2, 1, 1, 2, 1, 1, 3, 3, 5, 2, 2],
    &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 2, 1, 1, 2, 1, 1, 3, 4, 6, 3, 3],
    &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 2, 1, 1, 2, 1, 1, 3, 3, 5, 2, 2],
    &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 5, 1, 2],
    &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 5, 1, 2],
    &[3, 1, 1, 2, 3],
    &[3, 1, 1, 2, 3],
    &[3, 1, 1, 2, 3],
    &[3, 1, 1, 2, 1, 1],
    &[3, 1, 1, 2, 1, 1, 2, 4],
    &[3, 1, 1, 2, 5, 4, 2, 3, 2, 4, 1],
];

const NUM_IFACES: u32 = 15;

/// Direct3D 8 (kura's d3d8.rs on the same device): interface 16 + the Direct3D 9 one, each
/// method's argument count with `this` (from Wine's dlls/vnd3d8/main.c).
const METHODS8: [(u32, &[u32]); 10] = [
    (17, &[3, 1, 1, 2, 1, 4, 2, 4, 3, 6, 7, 6, 6, 4, 2, 7]),
    (18, &[3, 1, 1, 1, 1, 2, 2, 2, 2, 2, 4, 4, 2, 3, 2, 5, 4, 2, 3, 2, 8, 9, 7, 6, 6, 7, 6, 5, 6, 3, 2, 3, 2, 2, 1, 1, 7, 3, 3, 3, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 1, 2, 2, 2, 2, 3, 2, 2, 3, 3, 4, 4, 2, 4, 3, 3, 2, 2, 4, 6, 5, 9, 6, 5, 2, 2, 2, 4, 4, 4, 4, 4, 4, 3, 3, 3, 2, 2, 2, 4, 4, 4, 4, 4, 2]),
    (19, &[3, 1, 1, 5, 4]),
    (20, &[3, 1, 1, 2, 5, 4, 2, 3, 2, 4, 1]),
    (21, &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 2, 1, 1, 3, 3, 5, 2, 2]),
    (22, &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 2, 1, 1, 3, 4, 6, 3, 3]),
    (23, &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 2, 1, 1, 3, 3, 5, 2, 2]),
    (24, &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 5, 1, 2]),
    (25, &[3, 1, 1, 2, 5, 4, 2, 2, 1, 1, 1, 5, 1, 2]),
    (31, &[3, 1, 1, 2, 5, 4, 2, 3, 2, 4, 1]),
];
const NUM_IFACES8: u32 = 31;
const GETNPATCHMODE: u32 = 2 << 8 | 80;
const SURFACE_GETDC: u32 = 4 << 8 | 15;
const SURFACE_RELEASEDC: u32 = 4 << 8 | 16;

/// trap address -> method id
static IDS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);

unsafe extern "C" fn alloc(size: u32) -> *mut u8 {
    mem::alloc(size).unwrap_or(0) as usize as *mut u8
}
unsafe extern "C" fn free(p: *mut u8) {
    mem::release(p as usize as u32);
}
unsafe extern "C" fn client_rect(hwnd: u32, out: *mut i32) -> i32 {
    let Some(r) = crate::user::client_rect_screen(hwnd) else { return 0 };
    std::slice::from_raw_parts_mut(out, 4).copy_from_slice(&[r.l, r.t, r.r, r.b]);
    1
}
static CALLS: HostCalls = HostCalls { alloc, free, client_rect };
pub fn host_calls() -> &'static HostCalls {
    &CALLS
}

const CREATE_DEVICE9: u32 = 1 << 8 | 16;
const CREATE_DEVICE8: u32 = 17 << 8 | 15;

/// IDirect3D9Ex's and IDirect3DDevice9Ex's methods past the Direct3D 9 ones (kura has the
/// Direct3D 9 interfaces only): each answered with its Direct3D 9 counterpart.
fn method_ex(c: &mut Ctx, id: u32) -> Option<u32> {
    let a = |c: &Ctx, i: u32| if mem::x64() { c.a64(i) as u32 } else { c.a(i) };
    let args: Vec<u32> = (0..10).map(|i| a(c, i)).collect();
    let this = args[0];
    let r = match id {
        // QueryInterface: the Ex interfaces are these objects
        // (only those of Direct3DCreate9Ex: Direct3DCreate9's object and its devices answer
        // E_NOINTERFACE, as Windows; replay_b2011_anim_team_mm asks a plain device)
        D3D9_QI | DEVICE_QI if args[1] != 0 && matches!(mem::r32(args[1]), IID_D3D9EX | IID_DEVICE9EX) && extended(id, this) => {
            d3d_call(id + 1, &[this]);
            putp(args[2], this);
            0
        }
        // GetAdapterModeCountEx(adapter, filter), EnumAdapterModesEx(adapter, filter, mode, out)
        0x111 => d3d_call(1 << 8 | 6, &[this, args[1], filter_format(args[2])]),
        0x112 => {
            let mut m = [0u32; 4];
            let r = d3d_call(1 << 8 | 7, &[this, args[1], filter_format(args[2]), args[3], m.as_mut_ptr() as usize as u32]);
            write_mode_ex(args[4], &m, r);
            r
        }
        // GetAdapterDisplayModeEx(adapter, mode, rotation)
        0x113 => {
            let mut m = [0u32; 4];
            let r = d3d_call(1 << 8 | 8, &[this, args[1], m.as_mut_ptr() as usize as u32]);
            write_mode_ex(args[2], &m, r);
            if args[3] != 0 {
                mem::w32(args[3], 1);
            }
            r
        }
        // CreateDeviceEx(adapter, type, hwnd, flags, params, fullscreen mode, out)
        0x114 => {
            let r = d3d_call(CREATE_DEVICE9, &[this, args[1], args[2], args[3], args[4], args[5], args[7]]);
            if r == 0 && args[4] & 2 == 0 && !mem::x64() {
                c.cpu.fpu.cw = 0x7f;
            }
            r
        }
        // GetAdapterLUID
        0x115 => {
            mem::w32(args[2], 1);
            mem::w32(args[2] + 4, 0);
            0
        }
        // PresentEx(src, dst, hwnd, region, flags)
        0x279 => d3d_call(2 << 8 | 17, &[this, args[1], args[2], args[3], args[4]]),
        // Get{GPUThreadPriority,MaximumFrameLatency}
        0x27a | 0x27f => {
            mem::w32(args[1], if id == 0x27f { 3 } else { 0 });
            0
        }
        // CreateRenderTargetEx, CreateOffscreenPlainSurfaceEx, CreateDepthStencilSurfaceEx: the
        // same without the usage
        0x281 => d3d_call(2 << 8 | 28, &args[..9]),
        0x282 => d3d_call(2 << 8 | 36, &args[..7]),
        0x283 => d3d_call(2 << 8 | 29, &args[..9]),
        // ResetEx(params, fullscreen mode)
        0x284 => d3d_call(2 << 8 | 16, &[this, args[1]]),
        // GetDisplayModeEx(swapchain, mode, rotation)
        0x285 => {
            let mut m = [0u32; 4];
            let r = d3d_call(2 << 8 | 8, &[this, args[1], m.as_mut_ptr() as usize as u32]);
            write_mode_ex(args[2], &m, r);
            if args[3] != 0 {
                mem::w32(args[3], 1);
            }
            r
        }
        // SetConvolutionMonoKernel, ComposeRects, SetGPUThreadPriority, WaitForVBlank,
        // CheckResourceResidency, SetMaximumFrameLatency, CheckDeviceState
        0x277 | 0x278 | 0x27b..=0x27e | 0x280 => 0,
        _ => return None,
    };
    Some(r)
}
/// The IDirect3D9 objects Direct3DCreate9Ex made.
static EXTENDED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// Whether `this` (an IDirect3D9 for D3D9_QI, a device otherwise) comes from Direct3DCreate9Ex.
fn extended(id: u32, this: u32) -> bool {
    let d3d = if id == D3D9_QI {
        this
    } else {
        // the device's IDirect3D9 (GetDirect3D), released again
        let out = mem::heap_alloc(8, true);
        let r = d3d_call(DEVICE_QI | 6, &[this, out]);
        let o = mem::rp(out);
        mem::heap_free(out);
        if r != 0 || o == 0 {
            return false;
        }
        d3d_call(D3D9_QI | 2, &[o]);
        o
    };
    EXTENDED.lock().unwrap().contains(&d3d)
}
const D3D9_QI: u32 = 1 << 8;
const DEVICE_QI: u32 = 2 << 8;
const IID_D3D9EX: u32 = 0x0217_7241;
const IID_DEVICE9EX: u32 = 0xb18b_10ce;
/// D3DDISPLAYMODEFILTER { Size, Format, ScanLineOrdering }: the format (any: X8R8G8B8)
fn filter_format(p: u32) -> u32 {
    if p == 0 { 22 } else { mem::r32(p + 4) }
}
/// D3DDISPLAYMODEEX { Size, Width, Height, RefreshRate, Format, ScanLineOrdering } from a
/// D3DDISPLAYMODE
fn write_mode_ex(p: u32, m: &[u32; 4], r: u32) {
    if p == 0 || r != 0 {
        return;
    }
    mem::w32(p, 24);
    for (i, v) in m.iter().enumerate() {
        mem::w32(p + 4 + 4 * i as u32, *v);
    }
    mem::w32(p + 20, 1);
}

fn method(c: &mut Ctx) -> u32 {
    let id = IDS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).copied()).unwrap_or(0);
    if let Some(r) = method_ex(c, id) {
        return r;
    }
    match id {
        // a float in ST0 (x64: XMM0)
        GETNPATCHMODE => {
            let bits = d3d_call(id, &[c.a(0)]);
            c.st0 = Some(if mem::x64() { f64::from_bits(bits as u64) } else { f32::from_bits(bits) as f64 });
            0
        }
        SURFACE_GETDC => surface_get_dc(c),
        SURFACE_RELEASEDC => surface_release_dc(c),
        // IDirect3D9::CreateDevice, IDirect3D8::CreateDevice: without D3DCREATE_FPU_PRESERVE
        // the x87 goes to single precision, exceptions masked (control word 0x7f), and stays so
        CREATE_DEVICE9 | CREATE_DEVICE8 if !mem::x64() => {
            let behavior = c.a(4);
            let r = unsafe { kura::d3d9::vnhost_d3d9_call(id, (c.esp + 4) as usize as *const u32, &CALLS) };
            if r == 0 && behavior & 2 == 0 {
                c.cpu.fpu.cw = 0x7f;
            }
            r
        }
        _ if mem::x64() => {
            let n = args_of(id);
            let mut slots = [0u64; 16];
            for (i, s) in slots.iter_mut().enumerate().take(n) {
                *s = if FLOAT_ARGS.contains(&(id, i as u32)) && i < 4 { c.xmm(i as u32) & 0xffff_ffff } else { c.a64(i as u32) };
            }
            unsafe { kura::d3d9::vnhost_d3d9_call(id, slots.as_ptr() as *const u32, &CALLS) }
        }
        _ => unsafe { kura::d3d9::vnhost_d3d9_call(id, (c.esp + 4) as usize as *const u32, &CALLS) },
    }
}

/// x64: float parameters passed in XMM registers (method id, argument index with `this`):
/// IDirect3DDevice9::SetNPatchMode. (Clear's Z is the 6th argument: on the stack.)
const FLOAT_ARGS: [(u32, u32); 1] = [(2 << 8 | 79, 1)];

/// The method's argument count with `this`.
fn args_of(id: u32) -> usize {
    let (i, m) = (id >> 8, (id & 0xff) as usize);
    let list = if i >= 1 && i <= NUM_IFACES { Some(METHODS[i as usize - 1]) } else { METHODS8.iter().find(|x| x.0 == i).map(|x| x.1) };
    list.and_then(|l| l.get(m).copied()).unwrap_or(16).min(16) as usize
}

/// Open surface DCs: dc -> (surface, bitmap, previous bitmap)
static DCS: Mutex<Option<HashMap<u32, (u32, u32, u32)>>> = Mutex::new(None);

/// Calls the host directly (x64: the arguments as 8-byte slots).
fn d3d_call(id: u32, args: &[u32]) -> u32 {
    if mem::x64() {
        let wide: Vec<u64> = args.iter().map(|&a| a as u64).collect();
        return unsafe { kura::d3d9::vnhost_d3d9_call(id, wide.as_ptr() as *const u32, &CALLS) };
    }
    unsafe { kura::d3d9::vnhost_d3d9_call(id, args.as_ptr(), &CALLS) }
}

/// Level 0 of an IDirect3DTexture9 (or an IDirect3DSurface9 when `surface`) as top-down BGRA
/// rows: the 32-bit formats as they are, 16-bit R5G6B5 / X1R5G5B5 widened; None for others or
/// when it can't be locked.
pub(crate) fn read_bgra(obj: u32, surface: bool) -> Option<(u32, u32, Vec<u8>)> {
    let (desc_m, lock_m, unlock_m) = if surface { (4 << 8 | 12, 4 << 8 | 13, 4 << 8 | 14) } else { (5 << 8 | 17, 5 << 8 | 19, 5 << 8 | 20) };
    let mut desc = [0u32; 8];
    let lvl: &[u32] = if surface { &[] } else { &[0] };
    let a = |rest: &[u32]| -> Vec<u32> { [&[obj][..], lvl, rest].concat() };
    if d3d_call(desc_m, &a(&[desc.as_mut_ptr() as usize as u32])) != 0 {
        return None;
    }
    let (fmt, w, h) = (desc[0], desc[6], desc[7]);
    let bpp = match fmt {
        21 | 22 => 4,
        23 | 24 | 25 => 2,
        _ => return None,
    };
    let mut lr = [0u32; 4];
    if d3d_call(lock_m, &a(&[lr.as_mut_ptr() as usize as u32, 0, 0x10])) != 0 {
        return None;
    }
    let (pitch, bits) = (lr[0], lr[ps() as usize / 4]);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let row = mem::slice(bits + y * pitch, w * bpp);
        out.extend(widen_bgra(&row, fmt));
    }
    d3d_call(unlock_m, &a(&[]));
    Some((w, h, out))
}

/// One row of a D3DFMT_A8R8G8B8 / X8R8G8B8 / R5G6B5 / X1R5G5B5 / A1R5G5B5 surface as BGRA
/// (X formats opaque).
pub(crate) fn widen_bgra(row: &[u8], fmt: u32) -> Vec<u8> {
    let x5 = |v: u16, s: u32| ((v >> s & 31) as u32 * 255 / 31) as u8;
    match fmt {
        21 => row.to_vec(),
        22 => row.chunks(4).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        23 => row
            .chunks(2)
            .flat_map(|p| {
                let v = u16::from_le_bytes([p[0], p[1]]);
                [x5(v, 0), ((v >> 5 & 63) as u32 * 255 / 63) as u8, x5(v, 11), 255]
            })
            .collect(),
        _ => row
            .chunks(2)
            .flat_map(|p| {
                let v = u16::from_le_bytes([p[0], p[1]]);
                let a = if fmt == 25 && v & 0x8000 == 0 { 0 } else { 255 };
                [x5(v, 0), x5(v, 5), x5(v, 10), a]
            })
            .collect(),
    }
}

/// IDirect3DSurface9::GetDC: the surface locked, a DIB section over its pixels in a memory DC.
fn surface_get_dc(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let mut desc = [0u32; 8];
    if d3d_call(4 << 8 | 12, &[this, desc.as_mut_ptr() as usize as u32]) != 0 {
        return 0x8876_086c;
    }
    let (fmt, w, h) = (desc[0], desc[6], desc[7]);
    let (bpp, r565) = match fmt {
        0x15 | 0x16 => (32, false),
        0x17 => (16, true),
        0x18 | 0x19 => (16, false),
        _ => return 0x8876_086c,
    };
    // D3DLOCKED_RECT { Pitch, pBits } (x64: pBits at 8)
    let mut lr = [0u32; 4];
    if d3d_call(4 << 8 | 13, &[this, lr.as_mut_ptr() as usize as u32, 0, 0]) != 0 {
        return 0x8876_086c;
    }
    let bits = lr[ps() as usize / 4];
    let hbm = crate::gdi::bitmap_over(w as i32, h as i32, bpp, bits, lr[0], r565);
    let f = crate::api::export("gdi32", "CreateCompatibleDC");
    let dc = c.call(f, &[0]);
    let f = crate::api::export("gdi32", "SelectObject");
    let old = c.call(f, &[dc, hbm]);
    DCS.lock().unwrap().get_or_insert_with(HashMap::default).insert(dc, (this, hbm, old));
    putp(out, dc);
    0
}

fn surface_release_dc(c: &mut Ctx) -> u32 {
    let (this, dc) = (c.a(0), c.a(1));
    let Some((s, hbm, old)) = DCS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&dc) else { return 0x8876_086c };
    let _ = this;
    for (dll, name, args) in [("gdi32", "SelectObject", vec![dc, old]), ("gdi32", "DeleteDC", vec![dc]), ("gdi32", "DeleteObject", vec![hbm])] {
        let f = crate::api::export(dll, name);
        c.call(f, &args);
    }
    d3d_call(4 << 8 | 14, &[s]);
    0
}

/// The vtables, given to the host once.
fn init() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let k = ps();
        let table = mem::heap_alloc(k * (NUM_IFACES + 1), true);
        let mut ids = HashMap::default();
        for i in 1..=NUM_IFACES {
            let m = METHODS[i as usize - 1];
            let v = mem::heap_alloc(k * m.len() as u32, true);
            for (n, &args) in m.iter().enumerate() {
                let id = i << 8 | n as u32;
                let a = host_fn(&format!("d3d9:{i}:{n}"), method, 4 * args);
                ids.insert(a, id);
                wp(v + k * n as u32, a);
            }
            wp(table + k * i, v);
        }
        *IDS.lock().unwrap() = Some(ids);
        unsafe { kura::d3d9::vnhost_d3d9_init(table as usize as *const u32, NUM_IFACES + 1) };
    });
}

/// d3d8.dll's vtables, merged into the host's after d3d9's.
fn init8() {
    init();
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let k = ps();
        let table = mem::heap_alloc(k * (NUM_IFACES8 + 1), true);
        let mut g = IDS.lock().unwrap();
        let ids = g.get_or_insert_with(HashMap::default);
        for &(i, m) in METHODS8.iter() {
            let v = mem::heap_alloc(k * m.len() as u32, true);
            for (n, &args) in m.iter().enumerate() {
                let a = host_fn(&format!("d3d8:{i}:{n}"), method, 4 * args);
                ids.insert(a, i << 8 | n as u32);
                wp(v + k * n as u32, a);
            }
            wp(table + k * i, v);
        }
        drop(g);
        unsafe { kura::d3d9::vnhost_d3d9_init(table as usize as *const u32, NUM_IFACES8 + 1) };
    });
}

fn Direct3DCreate8(_: &mut Ctx) -> u32 {
    init8();
    let out = mem::heap_alloc(8, true);
    let r = d3d_call(kura::d3d9::d3d8::CREATE, &[out]);
    let o = mem::r32(out);
    mem::heap_free(out);
    if r != 0 { 0 } else { o }
}
/// Validate{Vertex,Pixel}Shader: any code passes; the error text out-pointer is the last argument.
fn validate8(c: &mut Ctx, out: u32) -> u32 {
    putp(out, 0);
    if c.a(0) != 0 { 0 } else { 0x8000_4005 }
}
fn ValidateVertexShader(c: &mut Ctx) -> u32 {
    validate8(c, c.a(4))
}
fn ValidatePixelShader(c: &mut Ctx) -> u32 {
    validate8(c, c.a(3))
}

fn Direct3DCreate9(c: &mut Ctx) -> u32 {
    let o = create9(c);
    // (an address an Ex object had before it was released)
    EXTENDED.lock().unwrap().retain(|&e| e != o);
    o
}
fn create9(c: &mut Ctx) -> u32 {
    init();
    let out = mem::heap_alloc(8, true);
    let r = unsafe { kura::d3d9::vnhost_d3d9_create(c.a(0), out as usize as *mut u32, &CALLS) };
    let o = mem::r32(out);
    mem::heap_free(out);
    if r != 0 { 0 } else { o }
}
/// Direct3DCreate9Ex(version, out): the Direct3D 9 object, whose vtable has the Ex methods.
fn Direct3DCreate9Ex(c: &mut Ctx) -> u32 {
    let out = if mem::x64() { c.a64(1) as u32 } else { c.a(1) };
    let o = create9(c);
    if o != 0 {
        let mut ex = EXTENDED.lock().unwrap();
        if !ex.contains(&o) {
            ex.push(o);
        }
    }
    putp(out, o);
    if o == 0 { 0x8876_086a } else { 0 }
}
fn Direct3DCreate9On12(c: &mut Ctx) -> u32 {
    Direct3DCreate9(c)
}
fn zero(_: &mut Ctx) -> u32 {
    0
}

pub fn register() {
    implement("d3d9", "Direct3DCreate9", Direct3DCreate9);
    implement("d3d9", "Direct3DCreate9Ex", Direct3DCreate9Ex);
    implement("d3d9", "Direct3DCreate9On12", Direct3DCreate9On12);
    implement("d3d8", "Direct3DCreate8", Direct3DCreate8);
    implement("d3d8", "ValidateVertexShader", ValidateVertexShader);
    implement("d3d8", "ValidatePixelShader", ValidatePixelShader);
    implement("d3d8", "DebugSetMute", zero);
    for n in ["D3DPERF_BeginEvent", "D3DPERF_EndEvent", "D3DPERF_GetStatus", "D3DPERF_QueryRepeatFrame", "D3DPERF_SetMarker",
        "D3DPERF_SetOptions", "D3DPERF_SetRegion", "DebugSetMute", "Direct3DShaderValidatorCreate9"]
    {
        implement("d3d9", n, zero);
    }
}
