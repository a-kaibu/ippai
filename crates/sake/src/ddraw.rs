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
const E_INVALIDARG: u32 = 0x8007_0057;
const DDERR_INVALIDPARAMS: u32 = 0x8007_0057;
const DDERR_UNSUPPORTED: u32 = 0x8000_4001;
const DDERR_NOTFOUND: u32 = 0x8876_00ff;
const DDERR_OUTOFMEMORY: u32 = 0x8007_000e;
const DDERR_NOCLIPLIST: u32 = 0x8876_00cd;
const DDERR_NOPALETTEATTACHED: u32 = 0x8876_023c;
const DDERR_NOCOLORKEY: u32 = 0x8876_00d7;
const DDERR_INVALIDRECT: u32 = 0x8876_0096;
const DDERR_INVALIDCAPS: u32 = 0x8876_0064;

const CAPS_BACKBUFFER: u32 = 0x4;
const CAPS_COMPLEX: u32 = 0x8;
const CAPS_FLIP: u32 = 0x10;
const CAPS_FRONTBUFFER: u32 = 0x20;
const CAPS_OFFSCREENPLAIN: u32 = 0x40;
const CAPS_PRIMARY: u32 = 0x200;
const CAPS_VISIBLE: u32 = 0x8000;
const DDSCAPS2_CUBEMAP: u32 = 0x200;
const DDSCAPS2_MIPMAPSUBLEVEL: u32 = 0x10000;
/// DDSCAPS2_CUBEMAP_POSITIVEX .. NEGATIVEZ
const CUBE_ORDER: [u32; 6] = [0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000];
const CUBE_FACES: u32 = 0xfc00;
const CAPS_SYSTEMMEMORY: u32 = 0x800;
const CAPS_VIDEOMEMORY: u32 = 0x4000;
const CAPS_LOCALVIDMEM: u32 = 0x1000_0000;
const CAPS_ZBUFFER: u32 = 0x2_0000;
const CAPS_3DDEVICE: u32 = 0x2000;
const CAPS_TEXTURE: u32 = 0x1000;
const CAPS_MIPMAP: u32 = 0x40_0000;
const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;

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
    /// the guest interface blocks, by vtable kind (0: v1-3, 1: v4, 2: v7, 3: IDirect3DTexture2, 4:
    /// IDirect3DTexture)
    blocks: [u32; 5],
    lock: Option<Rect>,
    /// the attached depth buffer (Direct3D)
    zbuf: u32,
    /// the DirectDraw object that made it
    dd: u32,
    /// made through an IDirectDraw7 (no IDirect3DTexture2 then: ddraw7's test_surface_qi)
    v7: bool,
    /// the IDirectDraw4/7 interface it holds a reference of (made through one of those, as on
    /// Windows; 0: none)
    dd_held: u32,
    /// a mipmap: the next smaller level (0: the last), how many levels from this one down
    /// (0: no mipmap), and whether it is a sublevel (it goes with the top level, not by its
    /// own references)
    mip: u32,
    mips: u32,
    sub: bool,
    /// a back buffer CreateSurface made with its front buffer (it goes with the front)
    implicit: bool,
    /// AddAttachedSurface: the surface this one is attached to (its `next` is this one), and
    /// the caps the attaching gave it (taken back when it is detached)
    parent: u32,
    added: u32,
    /// DDSCAPS2: a cube map's face (and DDSCAPS2_MIPMAPSUBLEVEL below its top), and the root's
    /// other faces, in the order EnumAttachedSurfaces gives them
    caps2: u32,
    faces: Vec<u32>,
    /// SetPrivateData's: by GUID, the bytes and whether they are an IUnknown it holds
    private: Vec<([u8; 16], Vec<u8>, bool)>,
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
    /// the DirectDraw objects that set the display mode and the cooperative level: when one
    /// goes, its mode and its exclusive mode go with it (as on Windows: ddraw1's tests create
    /// and release one object after the other and expect the registry's mode back)
    mode_by: u32,
    coop_by: u32,
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
        mode_by: 0,
        coop_by: 0,
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
    if p == 0 { 0 } else { r32(p) }
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
/// The bit count a pixel format says: a depth buffer of a 24-bit mask kept in 32 bits says 24
fn pf_bits(bpp: u16, pf: [u32; 5]) -> u32 {
    if pf[0] & 0x400 != 0 && bpp == 32 && pf[2] == 0xff_ffff { 24 } else { bpp as u32 }
}
fn write_pf(p: u32, bpp: u16, pf: [u32; 5]) {
    mem::fill(p, 32, 0);
    w32(p, 32);
    w32(p + 4, pf[0]);
    w32(p + 12, pf_bits(bpp, pf));
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
        // (a chain CreateSurface made, with any attached into it; one made by
        // AddAttachedSurface counts none: ddraw1's test_surface_attachment)
        if s.caps & CAPS_FLIP != 0 && s.caps & CAPS_FRONTBUFFER != 0 && s.caps & CAPS_COMPLEX != 0 {
            w32(p + 4, r32(p + 4) | 0x20);
            w32(p + 20, chain_len(s) - 1);
        }
        if s.mips != 0 {
            w32(p + 4, r32(p + 4) | DDSD_MIPMAPCOUNT);
            w32(p + 24, s.mips);
        }
    }
    write_pf(p + 72, bpp, pf);
    w32(p + 104, caps);
    if size == 124 {
        w32(p + 108, s.map_or(0, |s| s.caps2));
    }
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
                blocks: [0; 5],
                lock: None,
                zbuf: 0,
                dd: 0,
                v7: false,
                dd_held: 0,
                mip: 0,
                mips: 0,
                sub: false,
                implicit: false,
                parent: 0,
                added: 0,
                caps2: 0,
                faces: Vec::new(),
                private: Vec::new(),
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
            (unsupported, 8),
            (S_Blt, 24),
            (unsupported, 16),
            (S_BltFast, 24),
            (S_DeleteAttachedSurface, 12),
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
            m.extend_from_slice(&[(S_SetPrivateData, 20), (S_GetPrivateData, 16), (S_FreePrivateData, 8), (S_GetUniqueness, 8), (ok, 4)]);
        }
        if kind == 2 {
            m.extend_from_slice(&[(ok, 8), (S_GetZero, 8), (ok, 8), (S_GetZero, 8)]);
        }
        vtable(["IDirectDrawSurface", "IDirectDrawSurface4", "IDirectDrawSurface7"][kind as usize], &m)
    })
}

/// The guest interface for surface `id` as vtable kind `kind`.
fn block(id: u32, kind: u8) -> u32 {
    if let Some(b) = with(|s| s.surfs.get(&id).map(|x| x.blocks[kind as usize])).filter(|&b| b != 0) {
        return b;
    }
    let b = mem::heap_alloc(8, true);
    w32(b, match kind {
        3 => d3d7::tex2_vtbl(),
        4 => d3d7::tex_vtbl(),
        _ => surf_vtbl(kind),
    });
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
    // (no IID: E_INVALIDARG; IUnknown is the IDirectDrawSurface: ddraw7's test_surface_qi)
    if c.a(1) == 0 {
        put32(c.a(2), 0);
        return E_INVALIDARG;
    }
    let kind = match guid_first(c.a(1)) {
        0x6c14_db81 | 0x5780_5885 | 0xda04_4e00 | 0 => 0,
        0x0b2b_8630 => 1,
        0x0667_5a80 => 2,
        // IDirect3DTexture2, IDirect3DTexture (of a Direct3D 6 or older surface)
        0x9328_1502 if !surf(id).map_or(true, |x| x.v7) => 3,
        0x2cdc_d9e0 if !surf(id).map_or(true, |x| x.v7) => 4,
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
    with(|s| s.surfs.get_mut(&id).map_or(0, |x| {
        x.refs += 1;
        x.refs
    }))
}
fn S_Release(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    release_surface(c, id)
}
fn release_surface(c: &mut Ctx, id: u32) -> u32 {
    let left = with(|s| s.surfs.get_mut(&id).map_or(0, |x| {
        x.refs = x.refs.saturating_sub(1);
        x.refs
    }));
    if left == 0 {
        // (a mipmap's sublevel goes with its top level, a back buffer with its front buffer)
        if surf(id).is_some_and(|x| x.sub || x.implicit) {
            return 0;
        }
        // a flip chain goes with its front buffer, a mipmap's levels with the top one
        let mut chain = vec![id];
        with(|s| {
            let faces = s.surfs.get(&id).map_or(vec![], |x| x.faces.clone());
            for top in std::iter::once(id).chain(faces) {
                if top != id {
                    chain.push(top);
                }
                let mut at = s.surfs.get(&top).map_or(0, |x| x.mip);
                while at != 0 && chain.len() < 256 {
                    chain.push(at);
                    at = s.surfs.get(&at).map_or(0, |x| x.mip);
                }
            }
            if let Some(f) = s.surfs.get(&id) {
                if f.caps & CAPS_FRONTBUFFER != 0 {
                    // (one attached into the chain is not of it: it is let go, below)
                    let mut at = f.next;
                    for _ in 0..16 {
                        let Some(x) = s.surfs.get(&at).filter(|_| at != id) else { break };
                        if x.implicit {
                            chain.push(at);
                        }
                        at = x.next;
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
    // (a surface attached to this one: its reference goes, it is on its own again)
    if s.next != 0 && surf(s.next).is_some_and(|x| x.parent == id) {
        detach(s.next);
        release_surface(c, s.next);
    }
    for (_, d, unk) in s.private {
        if unk {
            unk_release(c, &d);
        }
    }
    if s.dd_held != 0 {
        dd_release(s.dd_held);
    }
}

const DDERR_CANNOTATTACHSURFACE: u32 = 0x8876_000a;
const DDERR_SURFACEALREADYATTACHED: u32 = 0x8876_019a;
const DDERR_SURFACENOTATTACHED: u32 = 0x8876_01cc;
const DDERR_CANNOTDETACHSURFACE: u32 = 0x8876_0014;

/// The first surface of the chain `id` is attached in (itself when it is attached to none).
fn chain_root(id: u32) -> u32 {
    let mut at = id;
    for _ in 0..16 {
        match surf(at).map_or(0, |x| x.parent) {
            0 => return at,
            p => at = p,
        }
    }
    at
}

/// Takes `x` off the surface it is attached to, with the caps the attaching gave it.
fn detach(x: u32) {
    with(|st| {
        let Some((p, next)) = st.surfs.get(&x).map(|s| (s.parent, s.next)) else { return };
        // (one put into a chain CreateSurface made: the chain closes up again)
        let spliced = next != 0 && st.surfs.get(&next).map_or(true, |n| n.parent != x);
        if let Some(ps) = st.surfs.get_mut(&p) {
            if ps.next == x {
                ps.next = if spliced { next } else { 0 };
            }
        }
        if let Some(xs) = st.surfs.get_mut(&x) {
            if spliced {
                xs.next = 0;
            }
            xs.caps &= !(CAPS_FLIP | CAPS_FRONTBUFFER | xs.added);
            xs.parent = 0;
            xs.added = 0;
        }
    });
}

/// AddAttachedSurface(surface): a depth buffer to draw with, or a surface of the same size
/// as the next of a flip chain (ddraw1's test_surface_attachment): the first surface becomes
/// FLIP and FRONTBUFFER (unless it is a back buffer), the attached one FLIP, and BACKBUFFER when
/// it is the first one's and was neither. It holds a reference of the attached surface. A
/// surface of another chain, of another size, or the primary is DDERR_CANNOTATTACHSURFACE; one
/// attached the other way round DDERR_SURFACEALREADYATTACHED.
fn S_AddAttachedSurface(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some((x, _)) = sid(c.a(1)) else { return DDERR_INVALIDPARAMS };
    let (Some(s), Some(xs)) = (surf(id), surf(x)) else { return DDERR_INVALIDPARAMS };
    if xs.caps & CAPS_ZBUFFER != 0 {
        with(|st| {
            st.surfs.get_mut(&x).map(|v| v.refs += 1);
            st.surfs.get_mut(&id).map(|v| v.zbuf = x)
        });
        return DD_OK;
    }
    if x == id || xs.implicit {
        return DDERR_CANNOTATTACHSURFACE;
    }
    if s.next == x || xs.next == id || chain_root(id) == x {
        return DDERR_SURFACEALREADYATTACHED;
    }
    // (into a chain CreateSurface made, after this one: it flips with it, and the front buffer
    // counts it as a back buffer; ddraw1's test_surface_attachment)
    let into = in_made_chain(&s);
    if xs.parent != 0 || xs.next != 0 || (s.next != 0 && !into) || xs.caps & CAPS_PRIMARY != 0 || (s.w, s.h, s.bpp) != (xs.w, xs.h, xs.bpp) {
        return DDERR_CANNOTATTACHSURFACE;
    }
    let root = s.parent == 0 && !into;
    with(|st| {
        if let Some(v) = st.surfs.get_mut(&id) {
            v.next = x;
            v.caps |= CAPS_FLIP;
            if root && v.caps & CAPS_BACKBUFFER == 0 {
                v.caps |= CAPS_FRONTBUFFER;
            }
        }
        if let Some(v) = st.surfs.get_mut(&x) {
            v.parent = id;
            v.next = if into { s.next } else { 0 };
            v.refs += 1;
            v.added = if root && v.caps & (CAPS_FRONTBUFFER | CAPS_BACKBUFFER) == 0 { CAPS_BACKBUFFER } else { 0 };
            v.caps |= CAPS_FLIP | v.added;
        }
    });
    DD_OK
}

/// Whether `s` is of a flip chain CreateSurface made (a back buffer of one, or its front).
fn in_made_chain(s: &Surf) -> bool {
    s.implicit || (s.caps & CAPS_FRONTBUFFER != 0 && s.caps & CAPS_COMPLEX != 0 && s.next != 0)
}
/// DeleteAttachedSurface(flags, surface): a depth buffer, or a surface attached to this one
/// (its reference goes); the surface this one is attached to is DDERR_CANNOTDETACHSURFACE, any
/// other DDERR_SURFACENOTATTACHED.
fn S_DeleteAttachedSurface(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let z = sid(c.a(2)).map(|x| x.0);
    let old = with(|st| {
        let x = st.surfs.get_mut(&id)?;
        if x.zbuf != 0 && (z.is_none() || z == Some(x.zbuf)) { Some(std::mem::take(&mut x.zbuf)) } else { None }
    });
    if let Some(o) = old {
        release_surface(c, o);
        return DD_OK;
    }
    let Some(x) = z else { return DD_OK };
    match surf(x) {
        Some(xs) if xs.parent == id => {
            detach(x);
            release_surface(c, x);
            DD_OK
        }
        Some(_) if surf(id).is_some_and(|s| s.parent == x) || chain_root(id) == x => DDERR_CANNOTDETACHSURFACE,
        Some(xs) if xs.caps & CAPS_ZBUFFER == 0 => DDERR_SURFACENOTATTACHED,
        _ => DD_OK,
    }
}

fn S_GetAttachedSurface(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let want = r32(c.a(1));
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    if s.zbuf != 0 && want & CAPS_ZBUFFER != 0 {
        with(|st| st.surfs.get_mut(&s.zbuf).map(|x| x.refs += 1));
        put32(c.a(2), block(s.zbuf, kind));
        return DD_OK;
    }
    // (the next of a flip chain: asked for as a back buffer or a flip surface, or by caps it
    // has, e.g. COMPLEX: ddraw7.c)
    // (the last of a chain made by AddAttachedSurface flips to its first)
    let next = if s.next == 0 && s.parent != 0 && s.caps & CAPS_FLIP != 0 { chain_root(id) } else { s.next };
    let next_has = |w: u32| surf(next).is_some_and(|n| w != 0 && n.caps & w == w);
    if next != 0 && (want & (CAPS_BACKBUFFER | CAPS_FLIP) != 0 || (s.mip == 0 && next_has(want))) {
        with(|st| st.surfs.get_mut(&next).map(|x| x.refs += 1));
        put32(c.a(2), block(next, kind));
        return DD_OK;
    }
    // (a cube map's face, asked for by DDSCAPS2: the root gives its other faces' tops, a face
    // its next level; ddraw7's test_getdc)
    let face = if kind >= 1 { r32(c.a(1) + 4) & CUBE_FACES } else { 0 };
    if face != 0 {
        let at = s.faces.iter().copied().find(|f| surf(*f).is_some_and(|x| x.caps2 & face != 0));
        let at = at.or((s.caps2 & face != 0 && s.mip != 0).then_some(s.mip));
        let Some(at) = at else {
            put32(c.a(2), 0);
            return DDERR_NOTFOUND;
        };
        with(|st| st.surfs.get_mut(&at).map(|x| x.refs += 1));
        put32(c.a(2), block(at, kind));
        return DD_OK;
    }
    // (the next mipmap level: asked for as TEXTURE, MIPMAP or COMPLEX, which it is)
    // (or by DDSCAPS2_MIPMAPSUBLEVEL alone: ddraw7's test_device_load)
    let sublevel = kind >= 1 && r32(c.a(1) + 4) & DDSCAPS2_MIPMAPSUBLEVEL != 0;
    if s.mip != 0 && (want & (CAPS_TEXTURE | CAPS_MIPMAP | CAPS_COMPLEX) != 0 || sublevel) && want & !(CAPS_TEXTURE | CAPS_MIPMAP | CAPS_COMPLEX) == 0 {
        with(|st| st.surfs.get_mut(&s.mip).map(|x| x.refs += 1));
        put32(c.a(2), block(s.mip, kind));
        return DD_OK;
    }
    put32(c.a(2), 0);
    DDERR_NOTFOUND
}

fn S_EnumAttachedSurfaces(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    // (a cube map's root: its other faces, then its own next level: dsurface's CubeMapTest)
    let att = if s.next != 0 && s.caps & CAPS_FRONTBUFFER != 0 { s.next } else { s.mip };
    for att in s.faces.iter().copied().chain(std::iter::once(att)).filter(|&a| a != 0) {
        if let Some(n) = surf(att) {
            let d = mem::heap_alloc(124, true);
            w32(d, if kind == 0 { 108 } else { 124 });
            write_desc(d, Some(&n));
            with(|st| st.surfs.get_mut(&att).map(|x| x.refs += 1));
            let b = block(att, kind);
            c.call(c.a(2), &[b, d, c.a(1)]);
            mem::heap_free(d);
        }
    }
    DD_OK
}

fn S_GetCaps(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    w32(c.a(1), s.caps);
    if kind == 2 {
        mem::fill(c.a(1) + 4, 12, 0);
    }
    DD_OK
}
fn S_GetSurfaceDesc(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    write_desc(c.a(1), Some(&s));
    DD_OK
}
fn S_GetPixelFormat(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    write_pf(c.a(1), s.bpp, s.pf);
    DD_OK
}
fn S_GetDDInterface(c: &mut Ctx) -> u32 {
    let (id, kind) = this(c);
    let obj = surf(id).map_or(0, |s| s.dd);
    let b = dd_iface(obj, [0, 2, 3][kind as usize]);
    mem::putp(c.a(1), b);
    if b == 0 { DDERR_INVALIDPARAMS } else { DD_OK }
}
const DDERR_MOREDATA: u32 = 0x8876_02b2;
const DDSPD_IUNKNOWNPOINTER: u32 = 1;

fn unk_release(c: &mut Ctx, d: &[u8]) {
    let p = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
    c.call(r32(r32(p) + 8), &[p]);
}
/// Takes the private data of `guid` off surface `id` (the IUnknown it held goes); whether
/// there was any.
fn take_private(c: &mut Ctx, id: u32, guid: [u8; 16]) -> bool {
    let old = with(|st| {
        let v = &mut st.surfs.get_mut(&id)?.private;
        let i = v.iter().position(|e| e.0 == guid)?;
        Some(v.remove(i))
    });
    if let Some((_, d, true)) = &old {
        unk_release(c, d);
    }
    old.is_some()
}
/// SetPrivateData(guid, data, size, flags): a copy of `size` bytes, or with
/// DDSPD_IUNKNOWNPOINTER an interface pointer it holds a reference of (any other size is
/// DDERR_INVALIDPARAMS, and takes off what was there, as ddraw4/7 do; ddraw4's
/// test_private_data). Each mipmap level has its own.
fn S_SetPrivateData(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (data, size, flags) = (c.a(2), c.a(3), c.a(4));
    if surf(id).is_none() || c.a(1) == 0 || data == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let guid: [u8; 16] = mem::slice(c.a(1), 16).try_into().unwrap();
    let unk = flags & DDSPD_IUNKNOWNPOINTER != 0;
    if unk && size != 4 {
        if size != 0 {
            take_private(c, id, guid);
        }
        return DDERR_INVALIDPARAMS;
    }
    if unk {
        c.call(r32(r32(data) + 4), &[data]);
    }
    // (an interface: the pointer itself)
    let bytes = if unk { data.to_le_bytes().to_vec() } else { mem::slice(data, size).to_vec() };
    take_private(c, id, guid);
    with(|st| st.surfs.get_mut(&id).map(|x| x.private.push((guid, bytes, unk))));
    DD_OK
}
/// GetPrivateData(guid, buffer, size): DDERR_NOTFOUND (nothing written), DDERR_MOREDATA
/// with the size it needs, or the data (an interface pointer without a reference).
fn S_GetPrivateData(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (buf, size) = (c.a(2), c.a(3));
    if c.a(1) == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let guid: [u8; 16] = mem::slice(c.a(1), 16).try_into().unwrap();
    let Some(d) = with(|st| st.surfs.get(&id)?.private.iter().find(|e| e.0 == guid).map(|e| e.1.clone())) else {
        return DDERR_NOTFOUND;
    };
    if size == 0 {
        return DDERR_INVALIDPARAMS;
    }
    if r32(size) < d.len() as u32 {
        w32(size, d.len() as u32);
        return DDERR_MOREDATA;
    }
    if buf == 0 {
        return DDERR_INVALIDPARAMS;
    }
    mem::write(buf, &d);
    w32(size, d.len() as u32);
    DD_OK
}
fn S_FreePrivateData(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    if c.a(1) == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let guid: [u8; 16] = mem::slice(c.a(1), 16).try_into().unwrap();
    if take_private(c, id, guid) { DD_OK } else { DDERR_NOTFOUND }
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
    let (id, kind) = this(c);
    let (flags, ck) = (c.a(1), c.a(2));
    // (through IDirectDrawSurface7 a mipmap's sublevel has its top's: ddraw7's test_device_load;
    // older ones key each level, ddraw4's test_ck_complex)
    if kind == 2 && surf(id).is_some_and(|x| x.caps2 & DDSCAPS2_MIPMAPSUBLEVEL != 0) {
        return 0x8876_025b;
    }
    let key = if ck == 0 { None } else { Some((r32(ck), r32(ck + 4))) };
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
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    let k = if c.a(1) & 0x8 != 0 { s.ck_src } else { s.ck_dst };
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
    if cl == 0 { DDERR_NOCLIPLIST } else { DD_OK }
}

/// SetPalette: a mipmap's sublevel takes none (DDERR_NOTONMIPMAPSUBLEVEL: dsurface.c)
fn S_SetPalette(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    // (a cube map's other faces are not sublevels: their tops take one)
    if surf(id).is_some_and(|x| x.caps2 & DDSCAPS2_MIPMAPSUBLEVEL != 0) {
        return 0x8876_025b;
    }
    let p = c.a(1);
    with(|s| s.surfs.get_mut(&id).map(|x| x.palette = p));
    apply_palette(id);
    DD_OK
}
fn S_GetPalette(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let p = surf(id).map_or(0, |s| s.palette);
    mem::putp(c.a(1), p);
    if p == 0 { DDERR_NOPALETTEATTACHED } else { DD_OK }
}

/// Gives an 8-bit surface's bitmap its palette (its own, or the primary's).
fn apply_palette(id: u32) {
    let Some((hbm, pal)) = with(|s| {
        let x = s.surfs.get(&id)?;
        if x.bpp != 8 {
            return None;
        }
        let p = if x.palette != 0 { x.palette } else { s.surfs.get(&s.primary).map_or(0, |p| p.palette) };
        Some((x.hbm, *s.palettes.get(&p)?))
    }) else {
        return;
    };
    crate::gdi::set_bitmap_palette(hbm, &pal);
}

fn S_Lock(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (rp, desc) = (c.a(1), c.a(2));
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    let r = if rp == 0 { Rect { l: 0, t: 0, r: s.w, b: s.h } } else { Rect::read(rp) };
    write_desc(desc, Some(&s));
    w32(desc + 4, r32(desc + 4) | 0x800);
    let bpp = s.bpp as u32 / 8;
    w32(desc + 36, s.bits + r.t.max(0) as u32 * s.pitch + r.l.max(0) as u32 * bpp);
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
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    apply_palette(id);
    let dc = gcall(c, "gdi32", "CreateCompatibleDC", &[0]);
    gcall(c, "gdi32", "SelectObject", &[dc, s.hbm]);
    with(|st| st.surfs.get_mut(&id).map(|x| x.dc = dc));
    put32(c.a(1), dc);
    DD_OK
}
fn S_ReleaseDC(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(s) = surf(id) else { return DDERR_INVALIDPARAMS };
    if s.dc != 0 {
        gcall(c, "gdi32", "DeleteDC", &[s.dc]);
        with(|st| st.surfs.get_mut(&id).map(|x| x.dc = 0));
    }
    present_if_primary(c, id, Rect { l: 0, t: 0, r: s.w, b: s.h });
    DD_OK
}

const DDERR_NOTFLIPPABLE: u32 = 0x8876_0246;
const DDERR_NOEXCLUSIVEMODE: u32 = 0x8876_00e1;

/// Which surface of a flip chain (front first) each one shows after a flip: with no target
/// every one moves up (the front shows the first back buffer, the last back buffer the old
/// front); with a target only the front and the target trade.
fn flip_order(n: usize, target: Option<usize>) -> Vec<usize> {
    match target {
        Some(t) => (0..n).map(|i| if i == 0 { t } else if i == t { 0 } else { i }).collect(),
        None => (0..n).map(|i| (i + 1) % n).collect(),
    }
}

/// Flip(target, flags): only the front buffer of a chain flips, to a surface of its own chain
/// (DDERR_NOTFLIPPABLE otherwise: a back buffer, an unrelated surface, the front itself), and a
/// primary chain only in exclusive mode (ddraw2's test_flip). The pixels move, the interfaces
/// stay. (Trading pixels with an unrelated surface left the front with that surface's memory
/// once it was released: a later Lock wrote over the program's code.)
fn S_Flip(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let Some(front) = surf(id) else { return DDERR_INVALIDPARAMS };
    if front.caps & CAPS_FRONTBUFFER == 0 || front.next == 0 {
        return DDERR_NOTFLIPPABLE;
    }
    if front.caps & CAPS_PRIMARY != 0 && !with(|st| st.exclusive) {
        return DDERR_NOEXCLUSIVEMODE;
    }
    let chain: Vec<u32> = with(|st| {
        let mut v = vec![id];
        let mut at = front.next;
        while at != 0 && at != id && v.len() < 8 {
            v.push(at);
            at = st.surfs.get(&at).map_or(0, |x| x.next);
        }
        v
    });
    let target = match c.a(1) {
        0 => None,
        b => match sid(b).and_then(|(t, _)| chain.iter().position(|&x| x == t)) {
            Some(i) if i != 0 => Some(i),
            _ => return DDERR_NOTFLIPPABLE,
        },
    };
    let order = flip_order(chain.len(), target);
    with(|st| {
        let mem: Vec<(u32, u32)> = chain.iter().map(|k| st.surfs.get(k).map_or((0, 0), |x| (x.bits, x.hbm))).collect();
        for (i, k) in chain.iter().enumerate() {
            if let Some(x) = st.surfs.get_mut(k) {
                (x.bits, x.hbm) = mem[order[i]];
            }
        }
    });
    present_if_primary(c, id, Rect { l: 0, t: 0, r: front.w, b: front.h });
    DD_OK
}

/// Shows part of the primary surface (rect in its coordinates) in the window.
fn present_if_primary(c: &mut Ctx, id: u32, r: Rect) {
    let Some(s) = surf(id) else { return };
    if s.caps & CAPS_PRIMARY == 0 {
        return;
    }
    let (coop, exclusive) = with(|st| (st.hwnd, st.exclusive));
    let hwnd = with(|st| st.clippers.get(&s.clipper).copied()).filter(|&h| h != 0).unwrap_or(coop);
    if hwnd == 0 {
        return;
    }
    apply_palette(id);
    let Some(cr) = crate::user::client_rect_screen(hwnd) else { return };
    // windowed: the primary is the screen; exclusive: it is the window's client area
    let (ox, oy) = if exclusive { (0, 0) } else { (cr.l, cr.t) };
    let r = Rect { l: r.l.max(0), t: r.t.max(0), r: r.r.min(s.w), b: r.b.min(s.h) };
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
        &[dc, (r.l - ox) as u32, (r.t - oy) as u32, (r.r - r.l) as u32, (r.b - r.t) as u32, mdc, r.l as u32, r.t as u32, 0x00cc_0020],
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
    Rect { l: r.l.max(0), t: r.t.max(0), r: r.r.min(w), b: r.b.min(h) }
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
    if bpp >= 4 { 0x00ff_ffff } else { (1u32 << (bpp * 8)) - 1 }
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
    let rows: Box<dyn Iterator<Item = i32>> = if overlap && dr.t > sr.t { Box::new((0..dh).rev()) } else { Box::new(0..dh) };
    for y in rows {
        let dy = dr.t + y;
        let sy = if same {
            sr.t + y
        } else {
            let k = if dh == sh { y } else { stretch_pick(y, sh, dh, false) };
            if mirror.1 { sr.b - 1 - k } else { sr.t + k }
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
                    std::ptr::copy(srow.add((sx0 as u32 * bpp) as usize), drow.add((x0 as u32 * bpp) as usize), (n as u32 * bpp) as usize);
                }
            }
            continue;
        }
        for x in 0..dw {
            let dx = dr.l + x;
            let sx = if same {
                sr.l + x
            } else {
                let k = if dw == sw { x } else { stretch_pick(x, sw, dw, true) };
                if mirror.0 { sr.r - 1 - k } else { sr.l + k }
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
        &[d, dr.l as u32, dr.t as u32, (dr.r - dr.l) as u32, (dr.b - dr.t) as u32, s, sr.l as u32, sr.t as u32, (sr.r - sr.l) as u32, (sr.b - sr.t) as u32, 0x00cc_0020],
    );
    gcall(c, "gdi32", "DeleteDC", &[s]);
    gcall(c, "gdi32", "DeleteDC", &[d]);
}

/// Where a Blt to the primary lands: windowed games blit in screen coordinates.
fn S_Blt(c: &mut Ctx) -> u32 {
    let (id, _) = this(c);
    let (drp, srcb, srp, flags, fx) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let Some(dst) = surf(id) else { return DDERR_INVALIDPARAMS };
    let dr = if drp == 0 { Rect { l: 0, t: 0, r: dst.w, b: dst.h } } else { Rect::read(drp) };
    if flags & 0x400 != 0 {
        // COLORFILL
        let color = if fx != 0 { r32(fx + 80) } else { 0 };
        fill(&dst, dr, color);
        present_if_primary(c, id, dr);
        return DD_OK;
    }
    if flags & 0x0200_0000 != 0 {
        // DEPTHFILL: a depth buffer's own values, fx's dwFillDepth (the union of dwFillColor)
        if dst.caps & CAPS_ZBUFFER == 0 || fx == 0 {
            return DDERR_INVALIDPARAMS;
        }
        fill(&dst, clip_rect(dr, dst.w, dst.h), r32(fx + 80));
        return DD_OK;
    }
    if flags & 0x20000 != 0 && fx != 0 && srcb == 0 {
        // ROP without a source: BLACKNESS / WHITENESS
        let rop = r32(fx + 8);
        fill(&dst, dr, if rop == 0x00ff_0062 { 0xffff_ffff } else { 0 });
        present_if_primary(c, id, dr);
        return DD_OK;
    }
    let Some((sid_, _)) = sid(srcb) else { return DDERR_INVALIDPARAMS };
    let Some(src) = surf(sid_) else { return DDERR_INVALIDPARAMS };
    let sr = if srp == 0 { Rect { l: 0, t: 0, r: src.w, b: src.h } } else { Rect::read(srp) };
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
    let ddfx = if flags & 0x800 != 0 && fx != 0 { r32(fx + 4) } else { 0 };
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
    let Some(dst) = surf(id) else { return DDERR_INVALIDPARAMS };
    let Some((sid_, _)) = sid(srcb) else { return DDERR_INVALIDPARAMS };
    let Some(src) = surf(sid_) else { return DDERR_INVALIDPARAMS };
    let sr = if srp == 0 { Rect { l: 0, t: 0, r: src.w, b: src.h } } else { Rect::read(srp) };
    let dr = Rect { l: x, t: y, r: x + sr.r - sr.l, b: y + sr.b - sr.t };
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
    let r = crate::user::client_rect_screen(h).unwrap_or(Rect { l: 0, t: 0, r: 0, b: 0 });
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
            &[(C_QueryInterface, 12), (one, 4), (one, 4), (P_GetCaps, 8), (P_GetEntries, 20), (ok, 16), (P_SetEntries, 20)],
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
        w32(out + 4 * i, (v >> 16 & 0xff) | (v & 0xff00) | (v & 0xff) << 16);
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
    static B: [OnceLock<u32>; 4] = [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];
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
            (D_EnumSurfaces, 20),
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
            (if v == 0 { D_SetDisplayMode1 } else { D_SetDisplayMode }, if v == 0 { 16 } else { 24 }),
            (D_WaitForVerticalBlank, 12),
        ];
        if v >= 1 {
            m.push((D_GetAvailableVidMem, 16));
        }
        if v >= 2 {
            m.extend_from_slice(&[(D_GetSurfaceFromDC, 12), (ok, 4), (ok, 4), (D_GetDeviceIdentifier, 12)]);
        }
        if v == 3 {
            m.extend_from_slice(&[(unsupported, 16), (unsupported, 12)]);
        }
        vtable(["IDirectDraw", "IDirectDraw2", "IDirectDraw4", "IDirectDraw7"][v], &m)
    })
}

/// One DirectDraw object (each DirectDrawCreate / CoCreateInstance makes its own). As on
/// Windows, each interface version counts its own references (IDirect3D7 shares
/// IDirectDraw7's); the object goes when all of them are 0.
struct DdObj {
    refs: [u32; 4],
    /// the guest interface blocks by version index, then IDirect3D7 (0 until asked for)
    blocks: [u32; 6],
    /// made by DirectDrawCreateEx (or as CLSID_DirectDraw7): none of its interfaces gives
    /// IDirect3D3 (refcount.c: not even its IDirectDraw4)
    ex: bool,
}

/// object id -> object; guest blocks are [vtable, version index, object id]
static DDS: Mutex<Option<(HashMap<u32, DdObj>, u32)>> = Mutex::new(None);

fn dds<R>(f: impl FnOnce(&mut HashMap<u32, DdObj>, &mut u32) -> R) -> R {
    let mut g = DDS.lock().unwrap_or_else(|e| e.into_inner());
    let (m, next) = g.get_or_insert_with(|| (HashMap::default(), 1));
    f(m, next)
}

/// A new DirectDraw object; returns its id.
fn dd_new(ex: bool) -> u32 {
    dds(|m, next| {
        let id = *next;
        *next += 1;
        m.insert(id, DdObj { refs: [0; 4], blocks: [0; 6], ex });
        id
    })
}

/// The guest interface of object `obj` as version index `v` (4: IDirect3D7, 5: IDirect3D3), one reference
/// added (QueryInterface / create). 0 if the object is gone.
fn dd_iface(obj: u32, v: usize) -> u32 {
    let Some(b) = dds(|m, _| m.get(&obj).map(|o| o.blocks[v])) else { return 0 };
    let b = if b != 0 {
        b
    } else {
        let b = mem::heap_alloc(12, true);
        w32(b, match v {
            4 => d3d7::d3d_vtbl(),
            5 => d3d7::d3d3_vtbl(),
            _ => dd_vtbl(v),
        });
        // (the reference counts: IDirect3D7's are IDirectDraw7's, IDirect3D3's IDirectDraw4's)
        w32(b + 4, if v == 5 { 2 } else { v.min(3) as u32 });
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
    dds(|m, _| m.get_mut(&obj).map_or(0, |o| {
        o.refs[v] += 1;
        o.refs[v]
    }))
}

/// Drops a reference of interface `b`; the object goes with its last one (the guest blocks
/// stay allocated, so a stray call on a released interface finds no object instead of freed
/// memory).
fn dd_release(b: u32) -> u32 {
    let (obj, v) = dd_of(b);
    let (n, gone) = dds(|m, _| {
        let Some(o) = m.get_mut(&obj) else { return (0, false) };
        o.refs[v] = o.refs[v].saturating_sub(1);
        let n = o.refs[v];
        let gone = o.refs == [0; 4];
        if gone {
            m.remove(&obj);
        }
        (n, gone)
    });
    if gone {
        with(|s| {
            if s.mode_by == obj {
                s.mode = None;
                s.mode_by = 0;
            }
            if s.coop_by == obj {
                s.exclusive = false;
                s.coop_by = 0;
            }
        });
    }
    n
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
        // IDirect3D3: not from an object DirectDrawCreateEx made
        0xbb22_3240 if !dds(|m, _| m.get(&dd_of(c.a(0)).0).map_or(true, |o| o.ex)) => 5,
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
    let mut caps = if flags & 1 != 0 { r32(d + 104) } else { CAPS_OFFSCREENPLAIN };
    // (cube map faces without DDSCAPS2_CUBEMAP: refused, as Windows, Wine's dsurface tests
    // CubeMapTest)
    if flags & 1 != 0 && kind == 2 && r32(d + 108) & 0xfc00 != 0 && r32(d + 108) & 0x200 == 0 {
        put32(out, 0);
        return DDERR_INVALIDCAPS;
    }
    let (mw, mh, mbpp) = display();
    let (mut bpp, mut r565) = (mbpp, mbpp == 16);
    let mut pfv = None;
    if flags & 0x1000 != 0 {
        let pf = d + 72;
        let pff = r32(pf + 4);
        if pff & 0x20 != 0 {
            bpp = 8;
        } else if pff & 0x400 != 0 {
            // (a 24-bit depth buffer is kept in 32 bits a pixel, as Windows pads it: Wine's d3d
            // tests z_format_test, a pitch of 4 bytes a pixel; write_pf still says 24)
            bpp = match r32(pf + 12) as u16 {
                24 => 32,
                b => b,
            };
            pfv = Some([0x400, 0, r32(pf + 20), 0, 0]);
        } else if pff & 0x40 != 0 {
            bpp = r32(pf + 12) as u16;
            r565 = bpp == 16 && r32(pf + 20) == 0x07e0;
            let a = if pff & 1 != 0 { r32(pf + 28) } else { 0 };
            pfv = Some([0x40 | (a != 0) as u32, r32(pf + 16), r32(pf + 20), r32(pf + 24), a]);
        }
    } else if caps & CAPS_ZBUFFER != 0 {
        bpp = 16;
        pfv = Some([0x400, 0, 0xffff, 0, 0]);
    }
    let (w, h) = if caps & CAPS_PRIMARY != 0 {
        if with(|s| s.exclusive) { (mw, mh) } else { crate::user::screen() }
    } else {
        (r32(d + 12) as i32, r32(d + 8) as i32)
    };
    if w <= 0 || h <= 0 || !matches!(bpp, 8 | 16 | 24 | 32) {
        return DDERR_INVALIDPARAMS;
    }
    if caps & CAPS_SYSTEMMEMORY == 0 {
        caps |= CAPS_VIDEOMEMORY | CAPS_LOCALVIDMEM;
    }
    // (the primary is what is on the screen: DDSCAPS_VISIBLE, ddraw1's test_surface_attachment)
    if caps & CAPS_PRIMARY != 0 {
        caps |= CAPS_VISIBLE;
    }
    // a mipmap is a texture; a complex surface is a flip chain, a mipmap or a cube map; a
    // mipmap count is of a complex one, and not 0; a complex mipmap without one goes down to
    // 1x1 (ddraw7's test_mipmap)
    let cube = kind == 2 && flags & 1 != 0 && r32(d + 108) & 0x200 != 0;
    let mip = caps & CAPS_MIPMAP != 0;
    if (mip && caps & CAPS_TEXTURE == 0) || (caps & CAPS_COMPLEX != 0 && !mip && caps & CAPS_FLIP == 0 && !cube) {
        put32(out, 0);
        return DDERR_INVALIDCAPS;
    }
    let faces = r32(d + 108) & CUBE_FACES;
    if cube && faces == 0 {
        put32(out, 0);
        return DDERR_INVALIDPARAMS;
    }
    let levels = match (mip, caps & CAPS_COMPLEX != 0, flags & DDSD_MIPMAPCOUNT != 0) {
        (false, ..) => 0,
        (true, false, true) => {
            put32(out, 0);
            return DDERR_INVALIDCAPS;
        }
        (true, false, false) => 1,
        (true, true, true) if r32(d + 24) == 0 => {
            put32(out, 0);
            return DDERR_INVALIDPARAMS;
        }
        (true, true, true) => r32(d + 24).min(mip_levels(w, h)),
        (true, true, false) => mip_levels(w, h),
    };
    let backs = if flags & 0x20 != 0 && caps & CAPS_FLIP != 0 { r32(d + 20).clamp(1, 3) } else { 0 };
    if backs > 0 {
        caps |= CAPS_FRONTBUFFER;
    }
    let pf = pfv.filter(|p| p[1] | p[2] | p[3] != 0).unwrap_or_else(|| plain_pf(bpp, r565));
    let Some(id) = new_surface_pf(w, h, bpp, r565, pf, caps) else { return DDERR_OUTOFMEMORY };
    let (owner, v7) = (dd_of(c.a(0)).0, dd_version(c) == 3);
    let held = if kind >= 1 { c.a(0) } else { 0 };
    if held != 0 {
        dd_addref(held);
    }
    with(|s| {
        s.surfs.get_mut(&id).map(|x| {
            x.dd = owner;
            x.v7 = v7;
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
    // a cube map: the first face asked for (in +X -X +Y -Y +Z -Z) is the root, the others go
    // with it, each with its own mipmap
    let face_bits = if cube { CUBE_ORDER.iter().copied().filter(|f| faces & f != 0).collect() } else { vec![0] };
    let mut tops = vec![id];
    for _ in 1..face_bits.len() {
        let Some(f) = new_surface_pf(w, h, bpp, r565, pf, caps) else { return DDERR_OUTOFMEMORY };
        with(|s| {
            s.surfs.get_mut(&f).map(|x| {
                x.dd = owner;
                x.v7 = v7;
                x.sub = true;
            })
        });
        tops.push(f);
    }
    for (&top, &face) in tops.iter().zip(&face_bits) {
        let cube2 = if cube { DDSCAPS2_CUBEMAP | face } else { 0 };
        with(|s| s.surfs.get_mut(&top).map(|x| {
            x.mips = levels;
            x.caps2 = cube2;
        }));
        let mut up = top;
        for i in 1..levels {
            let (lw, lh) = ((w >> i).max(1), (h >> i).max(1));
            let Some(m) = new_surface_pf(lw, lh, bpp, r565, pf, caps) else { return DDERR_OUTOFMEMORY };
            with(|s| {
                s.surfs.get_mut(&m).map(|x| {
                    x.dd = owner;
                    x.v7 = v7;
                    x.sub = true;
                    x.mips = levels - i;
                    x.caps2 = cube2 | DDSCAPS2_MIPMAPSUBLEVEL;
                });
                s.surfs.get_mut(&up).map(|x| x.mip = m)
            });
            up = m;
        }
    }
    with(|s| s.surfs.get_mut(&id).map(|x| x.faces = tops[1..].iter().rev().copied().collect()));
    let mut prev = id;
    for i in 0..backs {
        let bc = (caps & !(CAPS_PRIMARY | CAPS_FRONTBUFFER)) | CAPS_BACKBUFFER * (i == 0) as u32 | CAPS_FLIP | CAPS_COMPLEX;
        let Some(b) = new_surface(w, h, bpp, r565, bc) else { return DDERR_OUTOFMEMORY };
        with(|s| {
            s.surfs.get_mut(&b).map(|x| {
                x.dd = owner;
                x.v7 = v7;
                x.implicit = true;
            });
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

/// The levels of a full mipmap of a w x h top level: halving down to 1x1 (128x32: 8)
fn mip_levels(w: i32, h: i32) -> u32 {
    32 - (w.max(h).max(1) as u32).leading_zeros()
}

const MODES: [(i32, i32); 5] = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 960)];

fn D_EnumDisplayModes(c: &mut Ctx) -> u32 {
    let (filter, ctx, cb) = (c.a(2), c.a(3), c.a(4));
    let size = if dd_version(c) >= 2 { 124 } else { 108 };
    let d = mem::heap_alloc(124, true);
    'all: for bpp in [8u16, 16, 32] {
        for (w, h) in MODES {
            if filter != 0 && r32(filter + 4) & 0x6 == 0x6 && (r32(filter + 12) as i32 != w || r32(filter + 8) as i32 != h) {
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

/// EnumSurfaces(flags, desc, context, callback): DDENUMSURFACES_DOESEXIST with ALL (or MATCH:
/// the caps `desc` names, when it names them) hands each surface of this object to the callback
/// with a reference it releases, and its description; CANBECREATED enumerates nothing.
fn D_EnumSurfaces(c: &mut Ctx) -> u32 {
    let (flags, want, ctx, cb) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if cb == 0 {
        return DDERR_INVALIDPARAMS;
    }
    if flags & 0x10 == 0 {
        return DD_OK;
    }
    let (obj, v) = dd_of(c.a(0));
    let caps = if flags & 1 == 0 && want != 0 && r32(want + 4) & 1 != 0 { r32(want + 104) } else { 0 };
    let mut ids: Vec<u32> = with(|s| s.surfs.iter().filter(|(_, x)| x.dd == obj && x.caps & caps == caps).map(|(&i, _)| i).collect());
    ids.sort_unstable();
    let kind = [0u8, 0, 1, 2][v];
    let d = mem::heap_alloc(124, true);
    for id in ids {
        let Some(s) = surf(id) else { continue };
        with(|st| st.surfs.get_mut(&id).map(|x| x.refs += 1));
        w32(d, if v >= 2 { 124 } else { 108 });
        write_desc(d, Some(&s));
        if c.call(cb, &[block(id, kind), d, ctx]) == 0 {
            break;
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
        w32(p + 4, 0x40 | 0x0400_0000 | 0x200 | 0x0040_0000 | 0x0080_0000);
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
    put32(c.a(1), in_vblank(beam_clock()) as u32);
    DD_OK
}

// The monitor's 60 Hz beam, from a clock of its own: WaitForVerticalBlank waits for the next
// frame and GetVerticalBlankStatus says when the beam is in the blank (the last 5% of a frame).
// Games time it to tell the refresh rate: MBTRIAL0930's mb_trial.exe divides
// QueryPerformanceFrequency by the counter's difference across two waits, and a wait that
// returned at once made that a division by 0 (c0000094 at start).
const HZ: f64 = 60.0;
fn beam_clock() -> f64 {
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}
fn in_vblank(t: f64) -> bool {
    (t * HZ).fract() >= 0.95
}
/// The whole milliseconds from `t` (seconds on the beam's clock) until the next frame has begun:
/// 1..=17, crossing exactly one frame start
fn ms_to_next_frame(t: f64) -> u32 {
    let frame = (t * HZ).floor();
    let mut ms = (((frame + 1.0) / HZ - t) * 1000.0).ceil().max(1.0) as u32;
    // (rounding: never short of the start, never past the one after)
    while ((t + ms as f64 / 1000.0) * HZ).floor() <= frame {
        ms += 1;
    }
    while ms > 1 && ((t + ms as f64 / 1000.0) * HZ).floor() > frame + 1.0 {
        ms -= 1;
    }
    ms
}
const DDWAITVB_BLOCKBEGIN: u32 = 1;
const DDWAITVB_BLOCKBEGINEVENT: u32 = 2;
const DDWAITVB_BLOCKEND: u32 = 4;
/// What WaitForVerticalBlank(flags) does: Ok(wait for the next frame?) or the error (an event
/// to signal is DDERR_UNSUPPORTED, as on Windows and Wine)
fn vblank_wait_kind(flags: u32) -> Result<bool, u32> {
    if flags & DDWAITVB_BLOCKBEGINEVENT != 0 {
        return Err(DDERR_UNSUPPORTED);
    }
    Ok(flags & (DDWAITVB_BLOCKBEGIN | DDWAITVB_BLOCKEND) != 0)
}
/// IDirectDraw::WaitForVerticalBlank(this, flags, event)
fn D_WaitForVerticalBlank(c: &mut Ctx) -> u32 {
    match vblank_wait_kind(c.a(1)) {
        Ok(true) => {
            crate::k32::sleep_ms(ms_to_next_frame(beam_clock()));
            DD_OK
        }
        Ok(false) => DD_OK,
        Err(e) => e,
    }
}
fn D_RestoreDisplayMode(_: &mut Ctx) -> u32 {
    with(|s| s.mode = None);
    DD_OK
}
fn D_SetCooperativeLevel(c: &mut Ctx) -> u32 {
    let (h, flags) = (c.a(1), c.a(2));
    let by = dd_of(c.a(0)).0;
    with(|s| {
        s.hwnd = h;
        s.exclusive = flags & 0x10 != 0;
        s.coop_by = by;
    });
    DD_OK
}
fn set_mode(c: &Ctx, w: u32, h: u32, bpp: u32) -> u32 {
    if !matches!(bpp, 8 | 16 | 24 | 32) {
        return DDERR_INVALIDPARAMS;
    }
    let by = dd_of(c.a(0)).0;
    with(|s| {
        s.mode = Some((w as i32, h as i32, bpp as u16));
        s.mode_by = by;
    });
    DD_OK
}
fn D_SetDisplayMode1(c: &mut Ctx) -> u32 {
    set_mode(c, c.a(1), c.a(2), c.a(3))
}
fn D_SetDisplayMode(c: &mut Ctx) -> u32 {
    set_mode(c, c.a(1), c.a(2), c.a(3))
}
fn D_GetAvailableVidMem(c: &mut Ctx) -> u32 {
    put32(c.a(2), 256 << 20);
    put32(c.a(3), 240 << 20);
    DD_OK
}
/// The surface whose GetDC gave `dc`, with a reference more.
fn surface_of_dc(dc: u32) -> Option<u32> {
    if dc == 0 {
        return None;
    }
    with(|st| {
        let (&id, x) = st.surfs.iter_mut().find(|(_, x)| x.dc == dc)?;
        x.refs += 1;
        Some(id)
    })
}
/// IDirectDraw4/7::GetSurfaceFromDC(dc, surface): IDirectDraw4's gives the IDirectDrawSurface
/// (ddraw4's test_getdc), IDirectDraw7's the IDirectDrawSurface7.
fn D_GetSurfaceFromDC(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_INVALIDARG;
    }
    let kind = if dd_version(c) == 3 { 2 } else { 0 };
    match surface_of_dc(c.a(1)) {
        Some(id) => {
            w32(c.a(2), block(id, kind));
            DD_OK
        }
        None => {
            w32(c.a(2), 0);
            DDERR_NOTFOUND
        }
    }
}
/// GetSurfaceFromDC(dc, surface, device_dc) (ddraw.dll's export): the IDirectDrawSurface and
/// the DC it draws with.
fn GetSurfaceFromDC(c: &mut Ctx) -> u32 {
    let (out, dev) = (c.a(1), c.a(2));
    if out == 0 {
        return E_INVALIDARG;
    }
    if dev == 0 {
        w32(out, 0);
        return E_INVALIDARG;
    }
    match surface_of_dc(c.a(0)) {
        Some(id) => {
            w32(out, block(id, 0));
            w32(dev, c.a(0));
            DD_OK
        }
        None => {
            w32(out, 0);
            w32(dev, 0);
            DDERR_NOTFOUND
        }
    }
}
/// DDDEVICEIDENTIFIER's size (IDirectDraw4: two 512-byte names, the driver version, four ids
/// and a GUID) or DDDEVICEIDENTIFIER2's (IDirectDraw7: and dwWHQLLevel). Writing more ran over
/// the caller's stack: ddraw1's ddraw_get_identifier returned to 0.
fn ident_size(v: usize) -> u32 {
    512 + 512 + 8 + 16 + 16 + if v == 3 { 4 } else { 0 }
}
fn D_GetDeviceIdentifier(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    mem::fill(p, ident_size(dd_version(c)), 0);
    mem::write(p, b"sake");
    mem::write(p + 512, b"sake DirectDraw");
    DD_OK
}

// ---------------------------------------------------------------------------------------------
// exports

fn DirectDrawCreate(c: &mut Ctx) -> u32 {
    crate::mem::putp(c.a(1), dd_iface(dd_new(false), 0));
    DD_OK
}
fn DirectDrawCreateEx(c: &mut Ctx) -> u32 {
    crate::mem::putp(c.a(1), dd_iface(dd_new(true), 3));
    DD_OK
}
fn DirectDrawCreateClipper(c: &mut Ctx) -> u32 {
    crate::mem::putp(c.a(1), new_clipper());
    DD_OK
}
/// DirectDrawEnumerate(Ex)A: the primary display only (a NULL GUID); a NULL callback, or flags
/// other than DDENUM_ATTACHEDSECONDARYDEVICES / DETACHEDSECONDARYDEVICES / NONDISPLAYDEVICES,
/// are DDERR_INVALIDPARAMS. The W ones are DDERR_UNSUPPORTED whatever they are given, and call
/// nothing (ddrawmodes.c).
fn enumerate(c: &mut Ctx, ex: bool) -> u32 {
    let (cb, ctx, flags) = (c.a(0), c.a(1), if ex { c.a(2) } else { 0 });
    if cb == 0 || flags & !7 != 0 {
        return DDERR_INVALIDPARAMS;
    }
    let d = mem::heap_alloc(64, true);
    mem::write(d, b"Primary Display Driver\0");
    mem::write(d + 32, b"display\0");
    if ex {
        c.call(cb, &[0, d, d + 32, ctx, 0]);
    } else {
        c.call(cb, &[0, d, d + 32, ctx]);
    }
    mem::heap_free(d);
    DD_OK
}
fn DirectDrawEnumerateA(c: &mut Ctx) -> u32 {
    enumerate(c, false)
}
fn DirectDrawEnumerateExA(c: &mut Ctx) -> u32 {
    enumerate(c, true)
}
fn DirectDrawEnumerateW(_: &mut Ctx) -> u32 {
    DDERR_UNSUPPORTED
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
    Some(dd_iface(dd_new(mem::r32(clsid) == 0x3c30_5196), v))
}

/// The pixels of the surface interface `block` (amstream.rs draws movie frames into them):
/// width, height, bit count, pixel format (flags, masks), pitch and address. None for anything
/// that is not one of our surfaces.
pub fn surface_memory(block: u32) -> Option<(i32, i32, u16, [u32; 5], u32, u32)> {
    let (id, kind) = sid(block)?;
    if kind > 2 {
        return None;
    }
    surf(id).map(|s| (s.w, s.h, s.bpp, s.pf, s.pitch, s.bits))
}

/// Pixels written straight into surface `block` reach the window if it is the primary one.
pub fn drawn(c: &mut Ctx, block: u32, r: Rect) {
    if let Some((id, _)) = sid(block) {
        present_if_primary(c, id, r);
    }
}

/// The display mode's pixel format as a DDPIXELFORMAT at `p`.
pub fn write_display_pf(p: u32) {
    let (_, _, b) = display();
    write_pf(p, b, plain_pf(b, b == 16));
}

/// A new DirectDraw object's IDirectDraw (one reference).
pub fn new_direct_draw() -> u32 {
    dd_iface(dd_new(false), 0)
}

pub fn init() {
    implement("ddraw", "DirectDrawCreate", DirectDrawCreate);
    implement("ddraw", "DirectDrawCreateEx", DirectDrawCreateEx);
    implement("ddraw", "DirectDrawCreateClipper", DirectDrawCreateClipper);
    implement("ddraw", "GetSurfaceFromDC", GetSurfaceFromDC);
    implement("ddraw", "DirectDrawEnumerateA", DirectDrawEnumerateA);
    implement("ddraw", "DirectDrawEnumerateExA", DirectDrawEnumerateExA);
    implement("ddraw", "DirectDrawEnumerateW", DirectDrawEnumerateW);
    implement("ddraw", "DirectDrawEnumerateExW", DirectDrawEnumerateW);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// A wait for the vertical blank lasts 1..=17 ms and crosses exactly one frame start,
        /// from any point of the beam's clock
        #[test]
        fn a_vblank_wait_crosses_one_frame_start(t in 0.0f64..100_000.0) {
            let ms = ms_to_next_frame(t);
            prop_assert!((1..=17).contains(&ms), "{ms}");
            let (a, b) = ((t * HZ).floor(), ((t + ms as f64 / 1000.0) * HZ).floor());
            prop_assert_eq!(b, a + 1.0);
        }

        /// mb_trial.exe's check: QueryPerformanceFrequency over the counter's difference
        /// across two waits is a rate in 55..=65, never a division by 0
        #[test]
        fn two_vblank_waits_measure_about_60_hz(t in 0.0f64..100_000.0) {
            let t1 = t + ms_to_next_frame(t) as f64 / 1000.0;
            let t2 = t1 + ms_to_next_frame(t1) as f64 / 1000.0;
            // (the counter at 10 MHz, as kernel32's QueryPerformanceFrequency)
            let ticks = ((t2 - t1) * 1e7) as u64;
            prop_assert!(ticks > 0);
            let rate = 10_000_000 / ticks;
            prop_assert!((55..=65).contains(&rate), "{rate}");
        }
    }

    #[test]
    fn vblank_wait_flags() {
        assert_eq!(vblank_wait_kind(DDWAITVB_BLOCKBEGIN), Ok(true));
        assert_eq!(vblank_wait_kind(DDWAITVB_BLOCKEND), Ok(true));
        assert_eq!(vblank_wait_kind(DDWAITVB_BLOCKBEGINEVENT), Err(DDERR_UNSUPPORTED));
        assert_eq!(vblank_wait_kind(0), Ok(false));
        // the blank is the last twentieth of each frame
        assert!(!in_vblank(0.0) && in_vblank(0.96 / HZ) && !in_vblank(1.0 / HZ + 0.001));
    }

    /// ddraw.h numbers its errors in decimal: MAKE_DDHRESULT(10) is 0x8876000a, not 0x8876006e
    /// (AddAttachedSurface's "cannot attach" read as DDERR_INVALIDCLIPLIST; ddraw1's
    /// test_surface_attachment).
    #[test]
    fn error_codes_are_ddraw_h_decimal_ones() {
        let dd = |n: u32| 0x8876_0000 | n;
        for (got, want) in [
            (DDERR_CANNOTATTACHSURFACE, 10),
            (DDERR_CANNOTDETACHSURFACE, 20),
            (DDERR_INVALIDCAPS, 100),
            (DDERR_INVALIDRECT, 150),
            (DDERR_NOCLIPLIST, 205),
            (DDERR_NOCOLORKEY, 215),
            (DDERR_NOEXCLUSIVEMODE, 225),
            (DDERR_NOTFOUND, 255),
            (DDERR_SURFACEALREADYATTACHED, 410),
            (DDERR_SURFACENOTATTACHED, 460),
            (DDERR_NOPALETTEATTACHED, 572),
            (DDERR_NOTFLIPPABLE, 582),
            (DDERR_MOREDATA, 690),
        ] {
            assert_eq!(got, dd(want), "{want}");
        }
    }

    /// A 24-bit depth buffer is kept in 32 bits a pixel (Windows' pitch: Wine's d3d
    /// z_format_test, winref d3d7state) and still says 24; other formats say what they keep.
    #[test]
    fn depth_bits() {
        assert_eq!(pf_bits(32, [0x400, 0, 0xff_ffff, 0, 0]), 24);
        assert_eq!(pf_bits(32, [0x400, 0, 0xffff_ffff, 0, 0]), 32);
        assert_eq!(pf_bits(16, [0x400, 0, 0xffff, 0, 0]), 16);
        assert_eq!(pf_bits(32, [0x40, 0xff_0000, 0xff_ffff, 0xff, 0]), 32, "an RGB format is as it is");
        assert_eq!(stride(1024, 32), 4096);
    }

    #[test]
    fn mipmap_levels_as_windows_counts_them() {
        // ddraw7.c test_mipmap: 128x32 has 8, 32x64 7
        assert_eq!(mip_levels(128, 32), 8);
        assert_eq!(mip_levels(32, 64), 7);
        assert_eq!(mip_levels(1, 1), 1);
        assert_eq!(mip_levels(256, 256), 9);
        assert_eq!(mip_levels(3, 1), 2);
    }

    #[test]
    fn flips_move_every_buffer_up_or_trade_with_the_target() {
        // front, b1, b2, b3: the front shows b1, b1 shows b2, b2 shows b3, b3 the old front
        assert_eq!(flip_order(4, None), [1, 2, 3, 0]);
        assert_eq!(flip_order(2, None), [1, 0]);
        assert_eq!(flip_order(4, Some(2)), [2, 1, 0, 3]);
        // n flips of an n-chain: every buffer back where it was
        for n in 2..=8 {
            let mut at: Vec<usize> = (0..n).collect();
            for _ in 0..n {
                let o = flip_order(n, None);
                at = o.iter().map(|&i| at[i]).collect();
            }
            assert_eq!(at, (0..n).collect::<Vec<_>>());
        }
    }

    /// sizeof(DDDEVICEIDENTIFIER) 1064 and sizeof(DDDEVICEIDENTIFIER2) 1068, as ddraw.h lays
    /// them out (GetDeviceIdentifier writes no more than the version's struct)
    #[test]
    fn device_identifier_sizes() {
        assert_eq!(ident_size(2), 1064);
        assert_eq!(ident_size(3), 1068);
    }
}
