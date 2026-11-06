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
    &[3, 1, 1, 2, 1, 4, 3, 5, 3, 6, 7, 7, 6, 5, 4, 2, 7],
    &[3, 1, 1, 1, 1, 1, 2, 2, 3, 2, 4, 4, 2, 3, 3, 1, 2, 5, 5, 3, 2, 4, 3, 9, 10, 8, 7, 7, 9, 9, 5, 3, 3, 3, 6, 4, 7, 3, 3, 2, 2, 1, 1, 7, 3, 3, 3, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 2, 2, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 2, 2, 2, 2, 2, 1, 2, 1, 4, 7, 5, 9, 7, 3, 2, 2, 2, 2, 3, 2, 2, 4, 4, 4, 4, 4, 4, 5, 5, 3, 3, 2, 2, 3, 2, 2, 4, 4, 4, 4, 4, 4, 4, 4, 2, 3],
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

fn method(c: &mut Ctx) -> u32 {
    let id = IDS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).copied()).unwrap_or(0);
    match id {
        // a float in ST0
        GETNPATCHMODE => {
            c.st0 = Some(0.0);
            0
        }
        SURFACE_GETDC => surface_get_dc(c),
        SURFACE_RELEASEDC => surface_release_dc(c),
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
    init();
    let out = mem::heap_alloc(8, true);
    let r = unsafe { kura::d3d9::vnhost_d3d9_create(c.a(0), out as usize as *mut u32, &CALLS) };
    let o = mem::r32(out);
    mem::heap_free(out);
    if r != 0 { 0 } else { o }
}
fn Direct3DCreate9Ex(c: &mut Ctx) -> u32 {
    putp(c.a(1), 0);
    0x8876_086a
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
