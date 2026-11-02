//! d3d9.dll: vnhost's Direct3D 9 (crates/vnhost/src/d3d9, drawn with the page's WebGPU), the
//! same module Wine's dlls/vnd3d9 hands its calls to. As there, objects are { vtable, id }
//! blocks the host allocates in guest memory; here every vtable entry is a trap whose handler
//! passes the guest's stack arguments to `vnhost_d3d9_call(interface << 8 | index, args)`.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::api::{host_fn, implement, Ctx};
use crate::mem::{self, put32, w32};
use vnhost::d3d9::HostCalls;

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
        _ => unsafe { vnhost::d3d9::vnhost_d3d9_call(id, (c.esp + 4) as usize as *const u32, &CALLS) },
    }
}

/// Open surface DCs: dc -> (surface, bitmap, previous bitmap)
static DCS: Mutex<Option<HashMap<u32, (u32, u32, u32)>>> = Mutex::new(None);

fn d3d_call(id: u32, args: &[u32]) -> u32 {
    unsafe { vnhost::d3d9::vnhost_d3d9_call(id, args.as_ptr(), &CALLS) }
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
    let mut lr = [0u32; 2];
    if d3d_call(4 << 8 | 13, &[this, lr.as_mut_ptr() as usize as u32, 0, 0]) != 0 {
        return 0x8876_086c;
    }
    let hbm = crate::gdi::bitmap_over(w as i32, h as i32, bpp, lr[1], lr[0], r565);
    let f = crate::api::export("gdi32", "CreateCompatibleDC");
    let dc = c.call(f, &[0]);
    let f = crate::api::export("gdi32", "SelectObject");
    let old = c.call(f, &[dc, hbm]);
    DCS.lock().unwrap().get_or_insert_with(HashMap::new).insert(dc, (this, hbm, old));
    put32(out, dc);
    0
}

fn surface_release_dc(c: &mut Ctx) -> u32 {
    let (this, dc) = (c.a(0), c.a(1));
    let Some((s, hbm, old)) = DCS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&dc) else { return 0x8876_086c };
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
        let table = mem::heap_alloc(4 * (NUM_IFACES + 1), true);
        let mut ids = HashMap::new();
        for i in 1..=NUM_IFACES {
            let m = METHODS[i as usize - 1];
            let v = mem::heap_alloc(4 * m.len() as u32, true);
            for (n, &args) in m.iter().enumerate() {
                let id = i << 8 | n as u32;
                let a = host_fn(&format!("d3d9:{i}:{n}"), method, 4 * args);
                ids.insert(a, id);
                w32(v + 4 * n as u32, a);
            }
            w32(table + 4 * i, v);
        }
        *IDS.lock().unwrap() = Some(ids);
        unsafe { vnhost::d3d9::vnhost_d3d9_init(table as usize as *const u32, NUM_IFACES + 1) };
    });
}

fn Direct3DCreate9(c: &mut Ctx) -> u32 {
    init();
    let out = mem::heap_alloc(4, true);
    let r = unsafe { vnhost::d3d9::vnhost_d3d9_create(c.a(0), out as usize as *mut u32, &CALLS) };
    let o = mem::r32(out);
    mem::heap_free(out);
    if r != 0 { 0 } else { o }
}
fn Direct3DCreate9Ex(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
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
    for n in ["D3DPERF_BeginEvent", "D3DPERF_EndEvent", "D3DPERF_GetStatus", "D3DPERF_QueryRepeatFrame", "D3DPERF_SetMarker",
        "D3DPERF_SetOptions", "D3DPERF_SetRegion", "DebugSetMute", "Direct3DShaderValidatorCreate9"]
    {
        implement("d3d9", n, zero);
    }
}
