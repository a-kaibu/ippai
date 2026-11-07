//! ddraw.dll: DirectDraw 1-7 in software. Surfaces are pixels in guest memory with a GDI
//! bitmap over them (so GetDC works and presenting is a StretchBlt into the window). Blt and
//! BltFast between surfaces of one format are done here (copy, color key, stretch, fill); the
//! primary surface reaches the window when it is drawn to, unlocked, released or flipped.
//! IDirect3D7 (QueryInterface of IDirectDraw7) draws into these surfaces in software
//! (d3d7.rs); the older Direct3D interfaces are not offered.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, put32, r32, w32};
use crate::user::Rect;

#[path = "d3d7.rs"]
mod d3d7;

const DD_OK: u32 = 0;
const E_NOINTERFACE: u32 = 0x8000_4002;
const DDERR_INVALIDPARAMS: u32 = 0x8007_0057;
const DDERR_UNSUPPORTED: u32 = 0x8000_4001;
const DDERR_NOTFOUND: u32 = 0x8876_00ff;
const DDERR_OUTOFMEMORY: u32 = 0x8007_000e;
const DDERR_NOCLIPLIST: u32 = 0x8876_00cd;
const DDERR_NOPALETTEATTACHED: u32 = 0x8876_0230;
const DDERR_NOCOLORKEY: u32 = 0x8876_00d7;
const DDERR_INVALIDRECT: u32 = 0x8876_0096;

const CAPS_BACKBUFFER: u32 = 0x4;
const CAPS_COMPLEX: u32 = 0x8;
const CAPS_FLIP: u32 = 0x10;
const CAPS_FRONTBUFFER: u32 = 0x20;
const CAPS_OFFSCREENPLAIN: u32 = 0x40;
const CAPS_PRIMARY: u32 = 0x200;
const CAPS_SYSTEMMEMORY: u32 = 0x800;
const CAPS_VIDEOMEMORY: u32 = 0x4000;
const CAPS_LOCALVIDMEM: u32 = 0x1000_0000;
const CAPS_ZBUFFER: u32 = 0x2_0000;

#[derive(Clone)]
struct Surf {
    w: i32,
    h: i32,
    bpp: u16,
    /// DDPIXELFORMAT flags and the R, G, B, A (or Z) masks
    pf: [u32; 5],
    pitch: u32,
    bits: u32,
    hbm: u32,
    caps: u32,
    ck_src: Option<(u32, u32)>,
    ck_dst: Option<(u32, u32)>,
    /// the next surface of a flip chain (front -> back -> ... -> front)
    next: u32,
    clipper: u32,
    palette: u32,
    dc: u32,
    refs: u32,
    /// the guest interface blocks, by vtable kind (0: v1-3, 1: v4, 2: v7)
    blocks: [u32; 3],
    lock: Option<Rect>,
    /// the attached depth buffer (Direct3D)
    zbuf: u32,
    /// the DirectDraw object that made it
    dd: u32,
    /// the IDirectDraw4/7 interface it holds a reference of (made through one of those, as on
    /// Windows; 0: none)
    dd_held: u32,
}

struct State {
    surfs: HashMap<u32, Surf>,
    /// guest block -> (surface id, kind)
    blocks: HashMap<u32, (u32, u8)>,
    clippers: HashMap<u32, u32>,
    palettes: HashMap<u32, [u32; 256]>,
    next: u32,
    hwnd: u32,
    exclusive: bool,
    mode: Option<(i32, i32, u16)>,
    primary: u32,
}

static ST: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = ST.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| State {
        surfs: HashMap::default(),
        blocks: HashMap::default(),
        clippers: HashMap::default(),
        palettes: HashMap::default(),
        next: 1,
        hwnd: 0,
        exclusive: false,
        mode: None,
        primary: 0,
    }))
}

use crate::api::vtable;

fn gcall(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let f = crate::api::export(dll, name);
    c.call(f, args)
}

fn ok(_: &mut Ctx) -> u32 {
    DD_OK
}
fn one(_: &mut Ctx) -> u32 {
    1
}
fn unsupported(_: &mut Ctx) -> u32 {
    DDERR_UNSUPPORTED
}

fn guid_first(p: u32) -> u32 {
    if p == 0 {
        0
    } else {
        r32(p)
    }
}

// ---------------------------------------------------------------------------------------------
// the display

fn display() -> (i32, i32, u16) {
    with(|s| s.mode).unwrap_or_else(|| {
        let (w, h) = crate::user::screen();
        (w, h, 32)
    })
}

fn masks(bpp: u16, r565: bool) -> (u32, u32, u32) {
    match bpp {
        16 if r565 => (0xf800, 0x07e0, 0x001f),
        16 => (0x7c00, 0x03e0, 0x001f),
        24 | 32 => (0xff0000, 0x00ff00, 0x0000ff),
        _ => (0, 0, 0),
    }
}

/// The pixel format of a plain surface: flags (PALETTEINDEXED8 | RGB, or RGB) and masks.
fn plain_pf(bpp: u16, r565: bool) -> [u32; 5] {
    let (r, g, b) = masks(bpp, r565);
    [if bpp == 8 { 0x60 } else { 0x40 }, r, g, b, 0]
}

/// DDPIXELFORMAT
fn write_pf(p: u32, bpp: u16, pf: [u32; 5]) {
    mem::fill(p, 32, 0);
    w32(p, 32);
    w32(p + 4, pf[0]);
    w32(p + 12, bpp as u32);
    for i in 0..4 {
        w32(p + 16 + 4 * i as u32, pf[1 + i]);
    }
}

/// DDSURFACEDESC(2) for a surface (or the display mode when `s` is None).
fn write_desc(p: u32, s: Option<&Surf>) {
    write_desc_mode(p, s, display())
}
fn write_desc_mode(p: u32, s: Option<&Surf>, mode: (i32, i32, u16)) {
    let size = match r32(p) {
        108 => 108,
        _ => 124,
    };
    mem::fill(p, size, 0);
    w32(p, size);
    let (w, h, bpp, pf, pitch, caps, bits) = match s {
        Some(s) => (s.w, s.h, s.bpp, s.pf, s.pitch, s.caps, 0),
        None => {
            let (w, h, b) = mode;
            (w, h, b, plain_pf(b, b == 16), stride(w, b), 0, 0)
        }
    };
    let _ = bits;
    w32(p + 4, 0x1 | 0x2 | 0x4 | 0x8 | 0x1000); // caps height width pitch pixelformat
    w32(p + 8, h as u32);
    w32(p + 12, w as u32);
    w32(p + 16, pitch);
    if s.is_none() {
        w32(p + 4, 0x2 | 0x4 | 0x8 | 0x1000 | 0x40000); // + refresh rate
        w32(p + 24, 60);
    }
    if let Some(s) = s {
        if let Some((lo, hi)) = s.ck_src {
            w32(p + 4, r32(p + 4) | 0x10000);
            w32(p + 64, lo);
            w32(p + 68, hi);
        }
        if s.caps & CAPS_FLIP != 0 && s.caps & CAPS_FRONTBUFFER != 0 {
            w32(p + 4, r32(p + 4) | 0x20);
            w32(p + 20, chain_len(s) - 1);
        }
    }
    write_pf(p + 72, bpp, pf);
    w32(p + 104, caps);
}

fn chain_len(s: &Surf) -> u32 {
    let first = s.next;
    if first == 0 {
        return 1;
    }
    with(|st| {
        let mut n = 1;
        let mut at = first;
        while n < 8 {
            let Some(x) = st.surfs.get(&at) else { break };
            if x.caps & CAPS_FRONTBUFFER != 0 {
                break;
            }
            n += 1;
            at = x.next;
        }
        n
    })
}

fn stride(w: i32, bpp: u16) -> u32 {
    ((w as u32 * bpp as u32 / 8) + 3) & !3
}

// ---------------------------------------------------------------------------------------------
// surfaces

fn new_surface(w: i32, h: i32, bpp: u16, r565: bool, caps: u32) -> Option<u32> {
    new_surface_pf(w, h, bpp, r565, plain_pf(bpp, r565), caps)
}
fn new_surface_pf(w: i32, h: i32, bpp: u16, r565: bool, pf: [u32; 5], caps: u32) -> Option<u32> {
    let pitch = stride(w, bpp);
    let bits = mem::alloc((pitch * h as u32).max(4))?;
    let hbm = crate::gdi::bitmap_over(w, h, bpp, bits, pitch, r565);
    Some(with(|s| {
        let id = s.next;
        s.next += 1;
        s.surfs.insert(
            id,
            Surf {
                w,
                h,
                bpp,
                pf,
                pitch,
                bits,
                hbm,
                caps,
                ck_src: None,
                ck_dst: None,
                next: 0,
                clipper: 0,
                palette: 0,
                dc: 0,
                refs: 1,
                blocks: [0; 3],
                lock: None,
                zbuf: 0,
                dd: 0,
                dd_held: 0,
            },
        );
        id
    }))
}

fn surf_vtbl(kind: u8) -> u32 {
    static V: [OnceLock<u32>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    *V[kind as usize].get_or_init(|| {
        let mut m: Vec<(Handler, u32)> = vec![
            (S_QueryInterface, 12),
            (S_AddRef, 4),
            (S_Release, 4),
            (S_AddAttachedSurface, 8),
            (S_DeleteAttachedSurface, 12),
            (S_Blt, 24),
            (unsupported, 16),
            (S_BltFast, 24),
            (ok, 12),
            (S_EnumAttachedSurfaces, 12),
            (unsupported, 16),
            (S_Flip, 12),
            (S_GetAttachedSurface, 12),
            (ok, 8),
            (S_GetCaps, 8),
            (S_GetClipper, 8),
            (S_GetColorKey, 12),
            (S_GetDC, 8),
            (ok, 8),
            (unsupported, 12),
            (S_GetPalette, 8),
            (S_GetPixelFormat, 8),
            (S_GetSurfaceDesc, 8),
            (ok, 12),
            (ok, 4),
            (S_Lock, 20),
            (S_ReleaseDC, 8),
            (ok, 4),
            (S_SetClipper, 8),
            (S_SetColorKey, 12),
            (unsupported, 12),
            (S_SetPalette, 8),
            (S_Unlock, 8),
            (unsupported, 24),
            (unsupported, 8),
            (unsupported, 12),
            // IDirectDrawSurface2
            (S_GetDDInterface, 8),
            (ok, 8),
            (ok, 8),
            // IDirectDrawSurface3
            (unsupported, 12),
        ];
        if kind >= 1 {
            m.extend_from_slice(&[
                (ok, 20),
                (unsupported, 16),
                (ok, 8),
                (S_GetUniqueness, 8),
                (ok, 4),
            ]);
        }
        if kind == 2 {
            m.extend_from_slice(&[(ok, 8), (S_GetZero, 8), (ok, 8), (S_GetZero, 8)]);
        }
        vtable(
            [
                "IDirectDrawSurface",
                "IDirectDrawSurface4",
                "IDirectDrawSurface7",
            ][kind as usize],
            &m,
        )
    })
}

/// The guest interface for surface `id` as vtable kind `kind`.
fn block(id: u32, kind: u8) -> u32 {
    if let Some(b) = with(|s| s.surfs.get(&id).map(|x| x.blocks[kind as usize])).filter(|&b| b != 0)
    {
        return b;
    }
    let b = mem::heap_alloc(8, true);
    w32(b, surf_vtbl(kind));
    w32(b + 4, id);
    with(|s| {
        s.blocks.insert(b, (id, kind));
        if let Some(x) = s.surfs.get_mut(&id) {
            x.blocks[kind as usize] = b;
        }
    });
    b
}

fn sid(block: u32) -> Option<(u32, u8)> {
    if block == 0 {
        return None;
    }
    with(|s| s.blocks.get(&block).copied())
}

fn surf(id: u32) -> Option<Surf> {
    with(|s| s.surfs.get(&id).cloned())
}

fn this(c: &Ctx) -> (u32, u8) {
    sid(c.a(0)).unwrap_or((0, 0))
}

fn S_QueryInterface(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let kind = match guid_first(c.a(1)) {
        0x6c14_db81 | 0x5780_5885 | 0xda04_4e00 => 0,
        0x0b2b_8630 => 1,
        0x0667_5a80 => 2,
        0 => 2,
        _ => {
            put32(c.a(2), 0);
            return E_NOINTERFACE;
        }
    };
    with(|s| s.surfs.get_mut(&id).map(|x| x.refs += 1));
    put32(c.a(2), block(id, kind));
    DD_OK
}
fn S_AddRef(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    with(|s| {
        s.surfs.get_mut(&id).map_or(0, |x| {
            x.refs += 1;
            x.refs
        })
    })
}
fn S_Release(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    release_surface(c, id)
}
fn release_surface(c: &mut Ctx, id: u32) -> u32 {
    let left = with(|s| {
        s.surfs.get_mut(&id).map_or(0, |x| {
            x.refs = x.refs.saturating_sub(1);
            x.refs
        })
    });
    if left == 0 {
        // a flip chain goes with its front buffer
        let mut chain = vec![id];
        with(|s| {
            if let Some(f) = s.surfs.get(&id) {
                if f.caps & CAPS_FRONTBUFFER != 0 {
                    let mut at = f.next;
                    while at != 0 && at != id && chain.len() < 8 {
                        chain.push(at);
                        at = s.surfs.get(&at).map_or(0, |x| x.next);
                    }
                }
            }
        });
        for i in chain {
            free_surface(c, i);
        }
    }
    left
}

fn free_surface(c: &mut Ctx, id: u32) {
    let Some(s) = with(|st| {
        let s = st.surfs.remove(&id)?;
        for b in s.blocks {
            st.blocks.remove(&b);
        }
        if st.primary == id {
            st.primary = 0;
        }
        Some(s)
    }) else {
        return;
    };
    if s.dc != 0 {
        gcall(c, "gdi32", "DeleteDC", &[s.dc]);
    }
    gcall(c, "gdi32", "DeleteObject", &[s.hbm]);
    mem::release(s.bits);
    if s.zbuf != 0 {
        release_surface(c, s.zbuf);
    }
    if s.dd_held != 0 {
        dd_release(s.dd_held);
    }
}

fn S_AddAttachedSurface(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some((z, _)) = sid(c.a(1)) else {
        return DDERR_INVALIDPARAMS;
    };
    if surf(z).is_some_and(|x| x.caps & CAPS_ZBUFFER != 0) {
        with(|st| {
            st.surfs.get_mut(&z).map(|x| x.refs += 1);
            st.surfs.get_mut(&id).map(|x| x.zbuf = z)
        });
    }
    DD_OK
}
fn S_DeleteAttachedSurface(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let z = sid(c.a(2)).map(|x| x.0);
    let old = with(|st| {
        let x = st.surfs.get_mut(&id)?;
        if x.zbuf != 0 && (z.is_none() || z == Some(x.zbuf)) {
            Some(std::mem::take(&mut x.zbuf))
        } else {
            None
        }
    });
    if let Some(o) = old {
        release_surface(c, o);
    }
    DD_OK
}

fn S_GetAttachedSurface(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let want = r32(c.a(1));
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    if s.zbuf != 0 && want & CAPS_ZBUFFER != 0 {
        with(|st| st.surfs.get_mut(&s.zbuf).map(|x| x.refs += 1));
        put32(c.a(2), block(s.zbuf, kind));
        return DD_OK;
    }
    if s.next != 0 && want & (CAPS_BACKBUFFER | CAPS_FLIP) != 0 {
        with(|st| st.surfs.get_mut(&s.next).map(|x| x.refs += 1));
        put32(c.a(2), block(s.next, kind));
        return DD_OK;
    }
    put32(c.a(2), 0);
    DDERR_NOTFOUND
}

fn S_EnumAttachedSurfaces(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    if s.next != 0 && s.caps & CAPS_FRONTBUFFER != 0 {
        if let Some(n) = surf(s.next) {
            let d = mem::heap_alloc(124, true);
            w32(d, if kind == 0 { 108 } else { 124 });
            write_desc(d, Some(&n));
            with(|st| st.surfs.get_mut(&s.next).map(|x| x.refs += 1));
            let b = block(s.next, kind);
            c.call(c.a(2), &[b, d, c.a(1)]);
            mem::heap_free(d);
        }
    }
    DD_OK
}

fn S_GetCaps(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    w32(c.a(1), s.caps);
    if kind == 2 {
        mem::fill(c.a(1) + 4, 12, 0);
    }
    DD_OK
}
fn S_GetSurfaceDesc(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    write_desc(c.a(1), Some(&s));
    DD_OK
}
fn S_GetPixelFormat(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    write_pf(c.a(1), s.bpp, s.pf);
    DD_OK
}
fn S_GetDDInterface(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let obj = surf(id).map_or(0, |s| s.dd);
    let b = dd_iface(obj, [0, 2, 3][kind as usize]);
    mem::putp(c.a(1), b);
    if b == 0 {
        DDERR_INVALIDPARAMS
    } else {
        DD_OK
    }
}
fn S_GetUniqueness(c: &mut Ctx) -> u32 {
    put32(c.a(1), 1);
    DD_OK
}
fn S_GetZero(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    DD_OK
}

fn S_SetColorKey(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (flags, ck) = (c.a(1), c.a(2));
    let key = if ck == 0 {
        None
    } else {
        Some((r32(ck), r32(ck + 4)))
    };
    with(|s| {
        if let Some(x) = s.surfs.get_mut(&id) {
            if flags & 0x8 != 0 {
                x.ck_src = key;
            }
            if flags & 0x2 != 0 {
                x.ck_dst = key;
            }
        }
    });
    DD_OK
}
fn S_GetColorKey(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    let k = if c.a(1) & 0x8 != 0 {
        s.ck_src
    } else {
        s.ck_dst
    };
    match k {
        Some((lo, hi)) => {
            w32(c.a(2), lo);
            w32(c.a(2) + 4, hi);
            DD_OK
        }
        None => DDERR_NOCOLORKEY,
    }
}

fn S_SetClipper(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let cl = c.a(1);
    with(|s| s.surfs.get_mut(&id).map(|x| x.clipper = cl));
    DD_OK
}
fn S_GetClipper(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let cl = surf(id).map_or(0, |s| s.clipper);
    put32(c.a(1), cl);
    if cl == 0 {
        DDERR_NOCLIPLIST
    } else {
        DD_OK
    }
}

fn S_SetPalette(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let p = c.a(1);
    with(|s| s.surfs.get_mut(&id).map(|x| x.palette = p));
    apply_palette(id);
    DD_OK
}
fn S_GetPalette(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let p = surf(id).map_or(0, |s| s.palette);
    mem::putp(c.a(1), p);
    if p == 0 {
        DDERR_NOPALETTEATTACHED
    } else {
        DD_OK
    }
}

/// Gives an 8-bit surface's bitmap its palette (its own, or the primary's).
fn apply_palette(id: u32) {
    let Some((hbm, pal)) = with(|s| {
        let x = s.surfs.get(&id)?;
        if x.bpp != 8 {
            return None;
        }
        let p = if x.palette != 0 {
            x.palette
        } else {
            s.surfs.get(&s.primary).map_or(0, |p| p.palette)
        };
        Some((x.hbm, *s.palettes.get(&p)?))
    }) else {
        return;
    };
    crate::gdi::set_bitmap_palette(hbm, &pal);
}

fn S_Lock(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (rp, desc) = (c.a(1), c.a(2));
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    let r = if rp == 0 {
        Rect {
            l: 0,
            t: 0,
            r: s.w,
            b: s.h,
        }
    } else {
        Rect::read(rp)
    };
    write_desc(desc, Some(&s));
    w32(desc + 4, r32(desc + 4) | 0x800);
    let bpp = s.bpp as u32 / 8;
    w32(
        desc + 36,
        s.bits + r.t.max(0) as u32 * s.pitch + r.l.max(0) as u32 * bpp,
    );
    with(|st| st.surfs.get_mut(&id).map(|x| x.lock = Some(r)));
    DD_OK
}
fn S_Unlock(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let r = with(|st| st.surfs.get_mut(&id).and_then(|x| x.lock.take()));
    if let Some(r) = r {
        present_if_primary(c, id, r);
    }
    DD_OK
}

fn S_GetDC(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    apply_palette(id);
    let dc = gcall(c, "gdi32", "CreateCompatibleDC", &[0]);
    gcall(c, "gdi32", "SelectObject", &[dc, s.hbm]);
    with(|st| st.surfs.get_mut(&id).map(|x| x.dc = dc));
    put32(c.a(1), dc);
    DD_OK
}
fn S_ReleaseDC(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    if s.dc != 0 {
        gcall(c, "gdi32", "DeleteDC", &[s.dc]);
        with(|st| st.surfs.get_mut(&id).map(|x| x.dc = 0));
    }
    present_if_primary(
        c,
        id,
        Rect {
            l: 0,
            t: 0,
            r: s.w,
            b: s.h,
        },
    );
    DD_OK
}

fn S_Flip(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(front) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    let target = match sid(c.a(1)) {
        Some((t, _)) => t,
        None => front.next,
    };
    if target == 0 || target == id {
        return DDERR_INVALIDPARAMS;
    }
    // the pixels move, the interfaces stay
    with(|st| {
        let (Some(mut a), Some(mut b)) =
            (st.surfs.get(&id).cloned(), st.surfs.get(&target).cloned())
        else {
            return;
        };
        std::mem::swap(&mut a.bits, &mut b.bits);
        std::mem::swap(&mut a.hbm, &mut b.hbm);
        st.surfs.insert(id, a);
        st.surfs.insert(target, b);
    });
    present_if_primary(
        c,
        id,
        Rect {
            l: 0,
            t: 0,
            r: front.w,
            b: front.h,
        },
    );
    DD_OK
}

/// Shows part of the primary surface (rect in its coordinates) in the window.
fn present_if_primary(c: &mut Ctx, id: u32, r: Rect) {
    let Some(s) = surf(id) else { return };
    if s.caps & CAPS_PRIMARY == 0 {
        return;
    }
    let (coop, exclusive) = with(|st| (st.hwnd, st.exclusive));
    let hwnd = with(|st| st.clippers.get(&s.clipper).copied())
        .filter(|&h| h != 0)
        .unwrap_or(coop);
    if hwnd == 0 {
        return;
    }
    apply_palette(id);
    let Some(cr) = crate::user::client_rect_screen(hwnd) else {
        return;
    };
    // windowed: the primary is the screen; exclusive: it is the window's client area
    let (ox, oy) = if exclusive { (0, 0) } else { (cr.l, cr.t) };
    let r = Rect {
        l: r.l.max(0),
        t: r.t.max(0),
        r: r.r.min(s.w),
        b: r.b.min(s.h),
    };
    if r.r <= r.l || r.b <= r.t {
        return;
    }
    let dc = gcall(c, "user32", "GetDC", &[hwnd]);
    let mdc = gcall(c, "gdi32", "CreateCompatibleDC", &[dc]);
    let old = gcall(c, "gdi32", "SelectObject", &[mdc, s.hbm]);
    gcall(
        c,
        "gdi32",
        "BitBlt",
        &[
            dc,
            (r.l - ox) as u32,
            (r.t - oy) as u32,
            (r.r - r.l) as u32,
            (r.b - r.t) as u32,
            mdc,
            r.l as u32,
            r.t as u32,
            0x00cc_0020,
        ],
    );
    gcall(c, "gdi32", "SelectObject", &[mdc, old]);
    gcall(c, "gdi32", "DeleteDC", &[mdc]);
    gcall(c, "user32", "ReleaseDC", &[hwnd, dc]);
}

/// A rect Blt takes as it is: not empty and inside the surface (DDERR_INVALIDRECT otherwise;
/// a clipper clips a destination instead).
fn rect_ok(r: Rect, w: i32, h: i32) -> bool {
    r.l < r.r && r.t < r.b && r.l >= 0 && r.t >= 0 && r.r <= w && r.b <= h
}

fn clip_rect(r: Rect, w: i32, h: i32) -> Rect {
    Rect {
        l: r.l.max(0),
        t: r.t.max(0),
        r: r.r.min(w),
        b: r.b.min(h),
    }
}

fn read_px(p: *const u8, bpp: u32) -> u32 {
    unsafe {
        match bpp {
            1 => *p as u32,
            2 => (p as *const u16).read_unaligned() as u32,
            3 => *p as u32 | (*p.add(1) as u32) << 8 | (*p.add(2) as u32) << 16,
            _ => (p as *const u32).read_unaligned(),
        }
    }
}
fn write_px(p: *mut u8, bpp: u32, v: u32) {
    unsafe {
        match bpp {
            1 => *p = v as u8,
            2 => (p as *mut u16).write_unaligned(v as u16),
            3 => {
                *p = v as u8;
                *p.add(1) = (v >> 8) as u8;
                *p.add(2) = (v >> 16) as u8;
            }
            _ => (p as *mut u32).write_unaligned(v),
        }
    }
}

fn px_mask(bpp: u32) -> u32 {
    if bpp >= 4 {
        0x00ff_ffff
    } else {
        (1u32 << (bpp * 8)) - 1
    }
}

/// The source pixel of destination pixel `i` of `d` stretched from `s`, as Windows' HEL steps:
/// a 16.16 step rounded up, starting half a step in (but at 0 when widening a row).
fn stretch_pick(i: i32, s: i32, d: i32, row: bool) -> i32 {
    let step = (((s as i64) << 16) + d as i64 - 1) / d as i64;
    let start = if row && d > s { 0 } else { step / 2 };
    ((i as i64 * step + start) >> 16) as i32
}

/// Copies `sr` of `src` to `dr` of `dst` (same format), stretching with the nearest pixel,
/// mirrored left-right / up-down if asked, skipping source pixels in the key range.
fn blit(dst: &Surf, dr: Rect, src: &Surf, sr: Rect, key: Option<(u32, u32)>, mirror: (bool, bool)) {
    let bpp = dst.bpp as u32 / 8;
    let (dw, dh, sw, sh) = (dr.r - dr.l, dr.b - dr.t, sr.r - sr.l, sr.b - sr.t);
    if dw <= 0 || dh <= 0 || sw <= 0 || sh <= 0 {
        return;
    }
    let base_d = mem::slice_mut(dst.bits, dst.pitch * dst.h as u32).as_mut_ptr();
    let base_s = mem::slice(src.bits, src.pitch * src.h as u32).as_ptr();
    let mask = px_mask(bpp);
    let same = dw == sw && dh == sh && !mirror.0 && !mirror.1;
    // overlapping copies within one surface go bottom-up / right-to-left as needed
    let overlap = dst.bits == src.bits;
    let rows: Box<dyn Iterator<Item = i32>> = if overlap && dr.t > sr.t {
        Box::new((0..dh).rev())
    } else {
        Box::new(0..dh)
    };
    for y in rows {
        let dy = dr.t + y;
        let sy = if same {
            sr.t + y
        } else {
            let k = if dh == sh {
                y
            } else {
                stretch_pick(y, sh, dh, false)
            };
            if mirror.1 {
                sr.b - 1 - k
            } else {
                sr.t + k
            }
        };
        if dy < 0 || dy >= dst.h || sy < 0 || sy >= src.h {
            continue;
        }
        let drow = unsafe { base_d.add((dy as u32 * dst.pitch) as usize) };
        let srow = unsafe { base_s.add((sy as u32 * src.pitch) as usize) };
        if same && key.is_none() {
            let (mut x0, mut sx0, mut n) = (dr.l, sr.l, dw);
            if x0 < 0 {
                sx0 -= x0;
                n += x0;
                x0 = 0;
            }
            if sx0 < 0 {
                x0 -= sx0;
                n += sx0;
                sx0 = 0;
            }
            n = n.min(dst.w - x0).min(src.w - sx0);
            if n > 0 {
                unsafe {
                    std::ptr::copy(
                        srow.add((sx0 as u32 * bpp) as usize),
                        drow.add((x0 as u32 * bpp) as usize),
                        (n as u32 * bpp) as usize,
                    );
                }
            }
            continue;
        }
        for x in 0..dw {
            let dx = dr.l + x;
            let sx = if same {
                sr.l + x
            } else {
                let k = if dw == sw {
                    x
                } else {
                    stretch_pick(x, sw, dw, true)
                };
                if mirror.0 {
                    sr.r - 1 - k
                } else {
                    sr.l + k
                }
            };
            if dx < 0 || dx >= dst.w || sx < 0 || sx >= src.w {
                continue;
            }
            let v = read_px(unsafe { srow.add((sx as u32 * bpp) as usize) }, bpp);
            if let Some((lo, hi)) = key {
                let m = v & mask;
                if m >= lo & mask && m <= hi.max(lo) & mask {
                    continue;
                }
            }
            write_px(unsafe { drow.add((dx as u32 * bpp) as usize) }, bpp, v);
        }
    }
}

fn fill(dst: &Surf, r: Rect, color: u32) {
    let r = clip_rect(r, dst.w, dst.h);
    let bpp = dst.bpp as u32 / 8;
    for y in r.t..r.b {
        let row = dst.bits + y as u32 * dst.pitch;
        let p = mem::slice_mut(row, dst.pitch).as_mut_ptr();
        for x in r.l..r.r {
            write_px(unsafe { p.add((x as u32 * bpp) as usize) }, bpp, color);
        }
    }
}

/// Blt between surfaces of different formats: through GDI.
fn gdi_blit(c: &mut Ctx, dst: &Surf, dr: Rect, src: &Surf, sr: Rect) {
    let d = gcall(c, "gdi32", "CreateCompatibleDC", &[0]);
    let s = gcall(c, "gdi32", "CreateCompatibleDC", &[0]);
    gcall(c, "gdi32", "SelectObject", &[d, dst.hbm]);
    gcall(c, "gdi32", "SelectObject", &[s, src.hbm]);
    gcall(c, "gdi32", "SetStretchBltMode", &[d, 3]);
    gcall(
        c,
        "gdi32",
        "StretchBlt",
        &[
            d,
            dr.l as u32,
            dr.t as u32,
            (dr.r - dr.l) as u32,
            (dr.b - dr.t) as u32,
            s,
            sr.l as u32,
            sr.t as u32,
            (sr.r - sr.l) as u32,
            (sr.b - sr.t) as u32,
            0x00cc_0020,
        ],
    );
    gcall(c, "gdi32", "DeleteDC", &[s]);
    gcall(c, "gdi32", "DeleteDC", &[d]);
}

/// Where a Blt to the primary lands: windowed games blit in screen coordinates.
fn S_Blt(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (drp, srcb, srp, flags, fx) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let Some(dst) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    let dr = if drp == 0 {
        Rect {
            l: 0,
            t: 0,
            r: dst.w,
            b: dst.h,
        }
    } else {
        Rect::read(drp)
    };
    if flags & 0x400 != 0 {
        // COLORFILL
        let color = if fx != 0 { r32(fx + 80) } else { 0 };
        fill(&dst, dr, color);
        present_if_primary(c, id, dr);
        return DD_OK;
    }
    if flags & 0x20000 != 0 && fx != 0 && srcb == 0 {
        // ROP without a source: BLACKNESS / WHITENESS
        let rop = r32(fx + 8);
        fill(&dst, dr, if rop == 0x00ff_0062 { 0xffff_ffff } else { 0 });
        present_if_primary(c, id, dr);
        return DD_OK;
    }
    let Some((sid_, _)) = sid(srcb) else {
        return DDERR_INVALIDPARAMS;
    };
    let Some(src) = surf(sid_) else {
        return DDERR_INVALIDPARAMS;
    };
    let sr = if srp == 0 {
        Rect {
            l: 0,
            t: 0,
            r: src.w,
            b: src.h,
        }
    } else {
        Rect::read(srp)
    };
    if (dst.clipper == 0 && !rect_ok(dr, dst.w, dst.h)) || !rect_ok(sr, src.w, src.h) {
        return DDERR_INVALIDRECT;
    }
    let key = if flags & 0x10000 != 0 && fx != 0 {
        Some((r32(fx + 92), r32(fx + 96)))
    } else if flags & 0x8000 != 0 {
        src.ck_src
    } else {
        None
    };
    // DDBLT_DDFX: DDBLTFX_MIRRORLEFTRIGHT / MIRRORUPDOWN
    let ddfx = if flags & 0x800 != 0 && fx != 0 {
        r32(fx + 4)
    } else {
        0
    };
    if src.bpp == dst.bpp {
        blit(&dst, dr, &src, sr, key, (ddfx & 2 != 0, ddfx & 4 != 0));
    } else {
        gdi_blit(c, &dst, dr, &src, sr);
    }
    present_if_primary(c, id, dr);
    DD_OK
}

fn S_BltFast(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (x, y, srcb, srp, trans) = (c.a(1) as i32, c.a(2) as i32, c.a(3), c.a(4), c.a(5));
    let Some(dst) = surf(id) else {
        return DDERR_INVALIDPARAMS;
    };
    let Some((sid_, _)) = sid(srcb) else {
        return DDERR_INVALIDPARAMS;
    };
    let Some(src) = surf(sid_) else {
        return DDERR_INVALIDPARAMS;
    };
    let sr = if srp == 0 {
        Rect {
            l: 0,
            t: 0,
            r: src.w,
            b: src.h,
        }
    } else {
        Rect::read(srp)
    };
    let dr = Rect {
        l: x,
        t: y,
        r: x + sr.r - sr.l,
        b: y + sr.b - sr.t,
    };
    if !rect_ok(dr, dst.w, dst.h) || !rect_ok(sr, src.w, src.h) {
        return DDERR_INVALIDRECT;
    }
    let key = if trans & 1 != 0 { src.ck_src } else { None };
    if src.bpp == dst.bpp {
        blit(&dst, dr, &src, sr, key, (false, false));
    } else {
        gdi_blit(c, &dst, dr, &src, sr);
    }
    present_if_primary(c, id, dr);
    DD_OK
}

// ---------------------------------------------------------------------------------------------
// clippers and palettes

fn clipper_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirectDrawClipper",
            &[
                (C_QueryInterface, 12),
                (one, 4),
                (one, 4),
                (C_GetClipList, 16),
                (C_GetHWnd, 8),
                (ok, 12),
                (C_IsClipListChanged, 8),
                (ok, 12),
                (C_SetHWnd, 12),
            ],
        )
    })
}
fn new_clipper() -> u32 {
    let b = mem::heap_alloc(8, true);
    w32(b, clipper_vtbl());
    with(|s| s.clippers.insert(b, 0));
    b
}
fn C_QueryInterface(c: &mut Ctx) -> u32 {
    put32(c.a(2), c.a(0));
    DD_OK
}
fn C_GetClipList(c: &mut Ctx) -> u32 {
    // RGNDATA with the window's client rectangle
    let h = with(|s| s.clippers.get(&c.a(0)).copied()).unwrap_or(0);
    let r = crate::user::client_rect_screen(h).unwrap_or(Rect {
        l: 0,
        t: 0,
        r: 0,
        b: 0,
    });
    let need = 32 + 16;
    if c.a(2) == 0 {
        put32(c.a(3), need);
        return DD_OK;
    }
    w32(c.a(2), 32);
    w32(c.a(2) + 4, 1);
    w32(c.a(2) + 8, 1);
    w32(c.a(2) + 12, 16);
    r.write(c.a(2) + 16);
    r.write(c.a(2) + 32);
    DD_OK
}
fn C_GetHWnd(c: &mut Ctx) -> u32 {
    let h = with(|s| s.clippers.get(&c.a(0)).copied()).unwrap_or(0);
    mem::putp(c.a(1), h);
    DD_OK
}
fn C_IsClipListChanged(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    DD_OK
}
fn C_SetHWnd(c: &mut Ctx) -> u32 {
    let (b, h) = (c.a(0), c.a(2));
    with(|s| s.clippers.insert(b, h));
    DD_OK
}

fn palette_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirectDrawPalette",
            &[
                (C_QueryInterface, 12),
                (one, 4),
                (one, 4),
                (P_GetCaps, 8),
                (P_GetEntries, 20),
                (ok, 16),
                (P_SetEntries, 20),
            ],
        )
    })
}
fn P_GetCaps(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0x4 | 0x40); // 8BIT | ALLOW256
    DD_OK
}
fn P_GetEntries(c: &mut Ctx) -> u32 {
    let (b, base, n, out) = (c.a(0), c.a(2), c.a(3), c.a(4));
    let pal = with(|s| s.palettes.get(&b).copied()).unwrap_or([0; 256]);
    for i in 0..n.min(256u32.saturating_sub(base)) {
        let v = pal[(base + i) as usize];
        w32(
            out + 4 * i,
            (v >> 16 & 0xff) | (v & 0xff00) | (v & 0xff) << 16,
        );
    }
    DD_OK
}
fn P_SetEntries(c: &mut Ctx) -> u32 {
    let (b, base, n, inp) = (c.a(0), c.a(2), c.a(3), c.a(4));
    with(|s| {
        if let Some(p) = s.palettes.get_mut(&b) {
            for i in 0..n.min(256u32.saturating_sub(base)) {
                let e = r32(inp + 4 * i);
                p[(base + i) as usize] = (e & 0xff) << 16 | (e & 0xff00) | (e >> 16 & 0xff);
            }
        }
    });
    let ids: Vec<u32> = with(|s| s.surfs.keys().copied().collect());
    for id in ids {
        apply_palette(id);
    }
    DD_OK
}

// ---------------------------------------------------------------------------------------------
// IDirectDraw 1/2/4/7

/// The IDirectDraw vtable of interface version index 0..=3 (1, 2, 4, 7).
fn dd_vtbl(v: usize) -> u32 {
    static B: [OnceLock<u32>; 4] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    *B[v].get_or_init(|| {
        let mut m: Vec<(Handler, u32)> = vec![
            (D_QueryInterface, 12),
            (D_AddRef, 4),
            (D_Release, 4),
            (ok, 4),
            (D_CreateClipper, 16),
            (D_CreatePalette, 20),
            (D_CreateSurface, 16),
            (unsupported, 12),
            (D_EnumDisplayModes, 20),
            (unsupported, 20),
            (ok, 4),
            (D_GetCaps, 12),
            (D_GetDisplayMode, 8),
            (D_GetFourCCCodes, 12),
            (D_GetGDISurface, 8),
            (D_GetMonitorFrequency, 8),
            (D_GetScanLine, 8),
            (D_GetVerticalBlankStatus, 8),
            (ok, 8),
            (D_RestoreDisplayMode, 4),
            (D_SetCooperativeLevel, 12),
            (
                if v == 0 {
                    D_SetDisplayMode1
                } else {
                    D_SetDisplayMode
                },
                if v == 0 { 16 } else { 24 },
            ),
            (ok, 12),
        ];
        if v >= 1 {
            m.push((D_GetAvailableVidMem, 16));
        }
        if v >= 2 {
            m.extend_from_slice(&[
                (D_GetSurfaceFromDC, 12),
                (ok, 4),
                (ok, 4),
                (D_GetDeviceIdentifier, 12),
            ]);
        }
        if v == 3 {
            m.extend_from_slice(&[(unsupported, 16), (unsupported, 12)]);
        }
        vtable(
            [
                "IDirectDraw",
                "IDirectDraw2",
                "IDirectDraw4",
                "IDirectDraw7",
            ][v],
            &m,
        )
    })
}

/// One DirectDraw object (each DirectDrawCreate / CoCreateInstance makes its own). As on
/// Windows, each interface version counts its own references (IDirect3D7 shares
/// IDirectDraw7's); the object goes when all of them are 0.
struct DdObj {
    refs: [u32; 4],
    /// the guest interface blocks by version index, then IDirect3D7 (0 until asked for)
    blocks: [u32; 5],
}

/// object id -> object; guest blocks are [vtable, version index, object id]
static DDS: Mutex<Option<(HashMap<u32, DdObj>, u32)>> = Mutex::new(None);

fn dds<R>(f: impl FnOnce(&mut HashMap<u32, DdObj>, &mut u32) -> R) -> R {
    let mut g = DDS.lock().unwrap_or_else(|e| e.into_inner());
    let (m, next) = g.get_or_insert_with(|| (HashMap::default(), 1));
    f(m, next)
}

/// A new DirectDraw object; returns its id.
fn dd_new() -> u32 {
    dds(|m, next| {
        let id = *next;
        *next += 1;
        m.insert(
            id,
            DdObj {
                refs: [0; 4],
                blocks: [0; 5],
            },
        );
        id
    })
}

/// The guest interface of object `obj` as version index `v` (4: IDirect3D7), one reference
/// added (QueryInterface / create). 0 if the object is gone.
fn dd_iface(obj: u32, v: usize) -> u32 {
    let Some(b) = dds(|m, _| m.get(&obj).map(|o| o.blocks[v])) else {
        return 0;
    };
    let b = if b != 0 {
        b
    } else {
        let b = mem::heap_alloc(12, true);
        w32(b, if v == 4 { d3d7::d3d_vtbl() } else { dd_vtbl(v) });
        w32(b + 4, v.min(3) as u32);
        w32(b + 8, obj);
        dds(|m, _| m.get_mut(&obj).map(|o| o.blocks[v] = b));
        b
    };
    dd_addref(b);
    b
}

/// (object id, version index) of a guest DirectDraw / Direct3D block.
fn dd_of(b: u32) -> (u32, usize) {
    (r32(b + 8), (r32(b + 4) as usize).min(3))
}

fn dd_version(c: &Ctx) -> usize {
    dd_of(c.a(0)).1
}

fn dd_addref(b: u32) -> u32 {
    let (obj, v) = dd_of(b);
    dds(|m, _| {
        m.get_mut(&obj).map_or(0, |o| {
            o.refs[v] += 1;
            o.refs[v]
        })
    })
}

/// Drops a reference of interface `b`; the object goes with its last one (the guest blocks
/// stay allocated, so a stray call on a released interface finds no object instead of freed
/// memory).
fn dd_release(b: u32) -> u32 {
    let (obj, v) = dd_of(b);
    dds(|m, _| {
        let Some(o) = m.get_mut(&obj) else { return 0 };
        o.refs[v] = o.refs[v].saturating_sub(1);
        let n = o.refs[v];
        if o.refs == [0; 4] {
            m.remove(&obj);
        }
        n
    })
}

fn D_AddRef(c: &mut Ctx) -> u32 {
    dd_addref(c.a(0))
}
fn D_Release(c: &mut Ctx) -> u32 {
    dd_release(c.a(0))
}

fn D_QueryInterface(c: &mut Ctx) -> u32 {
    let v = match guid_first(c.a(1)) {
        0x6c14_db80 | 0 => 0,
        0xb3a6_f3e0 => 1,
        0x9c59_509a => 2,
        0x15e6_5ec0 => 3,
        0xf504_9e77 if dd_version(c) == 3 => 4,
        _ => {
            put32(c.a(2), 0);
            return E_NOINTERFACE;
        }
    };
    put32(c.a(2), dd_iface(dd_of(c.a(0)).0, v));
    DD_OK
}

fn D_CreateClipper(c: &mut Ctx) -> u32 {
    put32(c.a(2), new_clipper());
    DD_OK
}
fn D_CreatePalette(c: &mut Ctx) -> u32 {
    let (entries, out) = (c.a(2), c.a(3));
    let b = mem::heap_alloc(8, true);
    w32(b, palette_vtbl());
    let mut pal = [0u32; 256];
    if entries != 0 {
        for (i, p) in pal.iter_mut().enumerate() {
            let e = r32(entries + 4 * i as u32);
            *p = (e & 0xff) << 16 | (e & 0xff00) | (e >> 16 & 0xff);
        }
    }
    with(|s| s.palettes.insert(b, pal));
    put32(out, b);
    DD_OK
}

fn D_CreateSurface(c: &mut Ctx) -> u32 {
    let (d, out) = (c.a(1), c.a(2));
    let kind = match dd_version(c) {
        3 => 2,
        2 => 1,
        _ => 0,
    };
    let flags = r32(d + 4);
    let mut caps = if flags & 1 != 0 {
        r32(d + 104)
    } else {
        CAPS_OFFSCREENPLAIN
    };
    let (mw, mh, mbpp) = display();
    let (mut bpp, mut r565) = (mbpp, mbpp == 16);
    let mut pfv = None;
    if flags & 0x1000 != 0 {
        let pf = d + 72;
        let pff = r32(pf + 4);
        if pff & 0x20 != 0 {
            bpp = 8;
        } else if pff & 0x400 != 0 {
            bpp = r32(pf + 12) as u16;
            pfv = Some([0x400, 0, r32(pf + 20), 0, 0]);
        } else if pff & 0x40 != 0 {
            bpp = r32(pf + 12) as u16;
            r565 = bpp == 16 && r32(pf + 20) == 0x07e0;
            let a = if pff & 1 != 0 { r32(pf + 28) } else { 0 };
            pfv = Some([
                0x40 | (a != 0) as u32,
                r32(pf + 16),
                r32(pf + 20),
                r32(pf + 24),
                a,
            ]);
        }
    } else if caps & CAPS_ZBUFFER != 0 {
        bpp = 16;
        pfv = Some([0x400, 0, 0xffff, 0, 0]);
    }
    let (w, h) = if caps & CAPS_PRIMARY != 0 {
        if with(|s| s.exclusive) {
            (mw, mh)
        } else {
            crate::user::screen()
        }
    } else {
        (r32(d + 12) as i32, r32(d + 8) as i32)
    };
    if w <= 0 || h <= 0 || !matches!(bpp, 8 | 16 | 24 | 32) {
        return DDERR_INVALIDPARAMS;
    }
    if caps & CAPS_SYSTEMMEMORY == 0 {
        caps |= CAPS_VIDEOMEMORY | CAPS_LOCALVIDMEM;
    }
    let backs = if flags & 0x20 != 0 && caps & CAPS_FLIP != 0 {
        r32(d + 20).clamp(1, 3)
    } else {
        0
    };
    if backs > 0 {
        caps |= CAPS_FRONTBUFFER;
    }
    let pf = pfv
        .filter(|p| p[1] | p[2] | p[3] != 0)
        .unwrap_or_else(|| plain_pf(bpp, r565));
    let Some(id) = new_surface_pf(w, h, bpp, r565, pf, caps) else {
        return DDERR_OUTOFMEMORY;
    };
    let owner = dd_of(c.a(0)).0;
    let held = if kind >= 1 { c.a(0) } else { 0 };
    if held != 0 {
        dd_addref(held);
    }
    with(|s| {
        s.surfs.get_mut(&id).map(|x| {
            x.dd = owner;
            x.dd_held = held;
        })
    });
    if flags & 0x10000 != 0 {
        let key = (r32(d + 64), r32(d + 68));
        with(|s| s.surfs.get_mut(&id).map(|x| x.ck_src = Some(key)));
    }
    if caps & CAPS_PRIMARY != 0 {
        with(|s| s.primary = id);
    }
    let mut prev = id;
    for i in 0..backs {
        let bc = (caps & !(CAPS_PRIMARY | CAPS_FRONTBUFFER))
            | CAPS_BACKBUFFER * (i == 0) as u32
            | CAPS_FLIP
            | CAPS_COMPLEX;
        let Some(b) = new_surface(w, h, bpp, r565, bc) else {
            return DDERR_OUTOFMEMORY;
        };
        with(|s| {
            s.surfs.get_mut(&b).map(|x| x.dd = owner);
            s.surfs.get_mut(&prev).map(|x| x.next = b)
        });
        prev = b;
    }
    if backs > 0 {
        with(|s| s.surfs.get_mut(&prev).map(|x| x.next = id));
    }
    put32(out, block(id, kind));
    DD_OK
}

const MODES: [(i32, i32); 5] = [
    (640, 480),
    (800, 600),
    (1024, 768),
    (1280, 720),
    (1280, 960),
];

fn D_EnumDisplayModes(c: &mut Ctx) -> u32 {
    let (filter, ctx, cb) = (c.a(2), c.a(3), c.a(4));
    let size = if dd_version(c) >= 2 { 124 } else { 108 };
    let d = mem::heap_alloc(124, true);
    'all: for bpp in [8u16, 16, 32] {
        for (w, h) in MODES {
            if filter != 0
                && r32(filter + 4) & 0x6 == 0x6
                && (r32(filter + 12) as i32 != w || r32(filter + 8) as i32 != h)
            {
                continue;
            }
            w32(d, size);
            write_desc_mode(d, None, (w, h, bpp));
            if c.call(cb, &[d, ctx]) == 0 {
                break 'all;
            }
        }
    }
    mem::heap_free(d);
    DD_OK
}

fn D_GetCaps(c: &mut Ctx) -> u32 {
    for p in [c.a(1), c.a(2)] {
        if p == 0 {
            continue;
        }
        let size = r32(p).clamp(4, 380);
        mem::fill(p + 4, size - 4, 0);
        // BLT | BLTCOLORFILL | BLTSTRETCH | COLORKEY | PALETTE
        w32(
            p + 4,
            0x40 | 0x0400_0000 | 0x200 | 0x0040_0000 | 0x0080_0000,
        );
        w32(p + 12, 0x200); // CKEYCAPS_SRCBLT
        w32(p + 16, 0x0002_0000 | 0x0004_0000); // FXCAPS_BLTSTRETCHX/Y
        if size >= 68 {
            w32(p + 60, 256 << 20);
            w32(p + 64, 240 << 20);
        }
    }
    DD_OK
}

fn D_GetDisplayMode(c: &mut Ctx) -> u32 {
    write_desc(c.a(1), None);
    DD_OK
}
fn D_GetFourCCCodes(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    DD_OK
}
fn D_GetGDISurface(c: &mut Ctx) -> u32 {
    let p = with(|s| s.primary);
    if p == 0 {
        put32(c.a(1), 0);
        return DDERR_NOTFOUND;
    }
    let kind = match dd_version(c) {
        3 => 2,
        2 => 1,
        _ => 0,
    };
    // (a reference of the primary for the caller, as Windows gives)
    with(|s| s.surfs.get_mut(&p).map(|x| x.refs += 1));
    put32(c.a(1), block(p, kind));
    DD_OK
}
fn D_GetMonitorFrequency(c: &mut Ctx) -> u32 {
    put32(c.a(1), 60);
    DD_OK
}
fn D_GetScanLine(c: &mut Ctx) -> u32 {
    let t = crate::k32::ticks_ms();
    put32(c.a(1), (t % 16) as u32 * display().1 as u32 / 16);
    DD_OK
}
fn D_GetVerticalBlankStatus(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    DD_OK
}
fn D_RestoreDisplayMode(_: &mut Ctx) -> u32 {
    with(|s| s.mode = None);
    DD_OK
}
fn D_SetCooperativeLevel(c: &mut Ctx) -> u32 {
    let (h, flags) = (c.a(1), c.a(2));
    with(|s| {
        s.hwnd = h;
        s.exclusive = flags & 0x10 != 0;
    });
    DD_OK
}
fn set_mode(w: u32, h: u32, bpp: u32) -> u32 {
    if !matches!(bpp, 8 | 16 | 24 | 32) {
        return DDERR_INVALIDPARAMS;
    }
    with(|s| s.mode = Some((w as i32, h as i32, bpp as u16)));
    DD_OK
}
fn D_SetDisplayMode1(c: &mut Ctx) -> u32 {
    set_mode(c.a(1), c.a(2), c.a(3))
}
fn D_SetDisplayMode(c: &mut Ctx) -> u32 {
    set_mode(c.a(1), c.a(2), c.a(3))
}
fn D_GetAvailableVidMem(c: &mut Ctx) -> u32 {
    put32(c.a(2), 256 << 20);
    put32(c.a(3), 240 << 20);
    DD_OK
}
fn D_GetSurfaceFromDC(c: &mut Ctx) -> u32 {
    put32(c.a(2), 0);
    DDERR_NOTFOUND
}
fn D_GetDeviceIdentifier(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    mem::fill(p, 0x448, 0);
    mem::write(p, b"sake");
    mem::write(p + 512, b"sake DirectDraw");
    DD_OK
}

// ---------------------------------------------------------------------------------------------
// exports

fn DirectDrawCreate(c: &mut Ctx) -> u32 {
    put32(c.a(1), dd_iface(dd_new(), 0));
    DD_OK
}
fn DirectDrawCreateEx(c: &mut Ctx) -> u32 {
    put32(c.a(1), dd_iface(dd_new(), 3));
    DD_OK
}
fn DirectDrawCreateClipper(c: &mut Ctx) -> u32 {
    put32(c.a(1), new_clipper());
    DD_OK
}
fn DirectDrawEnumerateA(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(0), c.a(1));
    let d = mem::heap_alloc(64, true);
    mem::write(d, b"Primary Display Driver\0");
    mem::write(d + 32, b"display\0");
    c.call(cb, &[0, d, d + 32, ctx]);
    mem::heap_free(d);
    DD_OK
}
fn DirectDrawEnumerateExA(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(0), c.a(1));
    let d = mem::heap_alloc(64, true);
    mem::write(d, b"Primary Display Driver\0");
    mem::write(d + 32, b"display\0");
    c.call(cb, &[0, d, d + 32, ctx, 0]);
    mem::heap_free(d);
    DD_OK
}

/// CoCreateInstance of CLSID_DirectDraw / CLSID_DirectDraw7 (the game calls Initialize).
pub fn create_instance(clsid: u32, iid: u32) -> Option<u32> {
    match mem::r32(clsid) {
        0xd7b7_0ee0 | 0x3c30_5196 => {}
        0x5938_17a0 => return Some(new_clipper()),
        _ => return None,
    }
    let v = match guid_first(iid) {
        0xb3a6_f3e0 => 1,
        0x9c59_509a => 2,
        0x15e6_5ec0 => 3,
        _ => 0,
    };
    Some(dd_iface(dd_new(), v))
}

pub fn init() {
    implement("ddraw", "DirectDrawCreate", DirectDrawCreate);
    implement("ddraw", "DirectDrawCreateEx", DirectDrawCreateEx);
    implement("ddraw", "DirectDrawCreateClipper", DirectDrawCreateClipper);
    implement("ddraw", "DirectDrawEnumerateA", DirectDrawEnumerateA);
    implement("ddraw", "DirectDrawEnumerateExA", DirectDrawEnumerateExA);
}
