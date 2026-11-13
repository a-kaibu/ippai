//! Direct3D 7 (IDirect3D7, IDirect3DDevice7, IDirect3DVertexBuffer7) drawn in software into
//! DirectDraw surfaces: the render target, its attached depth buffer and the textures are the
//! ddraw surfaces' own pixels, so Flip/Blt present what was drawn as for any other surface.
//! Fixed-function pipeline: transform and lighting (directional / point lights), near-plane
//! clipping, culling, perspective-correct Gouraud triangles with D3D's top-left rule and
//! integer pixel centers, eight texture stages (point or bilinear, wrap/mirror/clamp/border),
//! color key, alpha test, depth test and alpha blending.

use super::*;

const D3D_OK: u32 = 0;
const D3DERR_INVALIDCALL: u32 = 0x8007_0057;
const DDERR_INVALIDPIXELFORMAT: u32 = 0x8876_0091;
const D3DERR_SCENE_IN_SCENE: u32 = 0x8876_02f8;
const D3DERR_SCENE_NOT_IN_SCENE: u32 = 0x8876_02f9;
/// (what Windows returns, winref ddrefs: 0x88760825, not d3d.h's MAKE_DDHRESULT(2061))
const D3DERR_VBUF_CREATE_FAILED: u32 = 0x8876_0825;

const RS_ZENABLE: usize = 7;
const RS_SHADEMODE: usize = 9;
const RS_ZWRITEENABLE: usize = 14;
const RS_ALPHATESTENABLE: usize = 15;
const RS_SRCBLEND: usize = 19;
const RS_DESTBLEND: usize = 20;
const RS_CULLMODE: usize = 22;
const RS_ZFUNC: usize = 23;
const RS_ALPHAREF: usize = 24;
const RS_ALPHAFUNC: usize = 25;
const RS_ALPHABLENDENABLE: usize = 27;
const RS_SPECULARENABLE: usize = 29;
const RS_FOGENABLE: usize = 28;
const RS_FOGCOLOR: usize = 34;
const RS_FOGTABLEMODE: usize = 35;
const RS_FOGSTART: usize = 36;
const RS_FOGEND: usize = 37;
const RS_FOGDENSITY: usize = 38;
const RS_RANGEFOGENABLE: usize = 48;
const RS_FOGVERTEXMODE: usize = 140;
const RS_COLORKEYENABLE: usize = 41;
const RS_TEXTUREFACTOR: usize = 60;
const RS_LIGHTING: usize = 137;
const RS_AMBIENT: usize = 139;
const RS_COLORVERTEX: usize = 141;
const RS_DIFFUSEMATERIALSOURCE: usize = 145;
const RS_SPECULARMATERIALSOURCE: usize = 146;
const RS_AMBIENTMATERIALSOURCE: usize = 147;
const RS_EMISSIVEMATERIALSOURCE: usize = 148;

const TSS_COLOROP: usize = 1;
const TSS_COLORARG1: usize = 2;
const TSS_COLORARG2: usize = 3;
const TSS_ALPHAOP: usize = 4;
const TSS_ALPHAARG1: usize = 5;
const TSS_ALPHAARG2: usize = 6;
const TSS_TEXCOORDINDEX: usize = 11;
const TSS_ADDRESS: usize = 12;
const TSS_ADDRESSU: usize = 13;
const TSS_ADDRESSV: usize = 14;
const TSS_BORDERCOLOR: usize = 15;
const TSS_MAGFILTER: usize = 16;
const TSS_MINFILTER: usize = 17;

const FVF_XYZRHW: u32 = 0x4;
const FVF_NORMAL: u32 = 0x10;
const FVF_RESERVED1: u32 = 0x20;
const FVF_DIFFUSE: u32 = 0x40;
const FVF_SPECULAR: u32 = 0x80;

type C4 = [f32; 4];

#[derive(Clone)]
struct Dev {
    /// the render target and the textures (surface ids) each hold a reference of the surface
    rt: u32,
    /// the DirectDraw object (GetDirect3D)
    dd: u32,
    /// the IDirect3D7 interface that made it, of which it holds a reference
    d3d_held: u32,
    /// x, y, width, height
    vp: [i32; 4],
    zr: (f32, f32),
    /// world, view, projection
    xf: [[f32; 16]; 3],
    /// after the projection: a Direct3D 6 viewport's clip volume to -1..1 (and its Z range
    /// to 0..1)
    clip: [f32; 16],
    rs: Vec<u32>,
    tss: [[u32; 32]; 8],
    tex: [u32; 8],
    /// diffuse, ambient, specular, emissive (RGBA each), power
    mat: [f32; 17],
    /// by the game's light index (any u32, as on Windows; only the ones set or enabled)
    lights: std::collections::BTreeMap<u32, ([u32; 26], bool)>,
    rec: Option<Vec<Ch>>,
    /// between BeginScene and EndScene
    scene: bool,
}

/// One recorded state change (state blocks).
#[derive(Clone)]
enum Ch {
    Rs(usize, u32),
    Tss(usize, usize, u32),
    Tex(usize, u32),
    Xf(usize, [f32; 16]),
    Vp([i32; 4], (f32, f32)),
    Mat([f32; 17]),
    Light(u32, [u32; 26]),
    LightOn(u32, bool),
}

const IDENT: [f32; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

impl Dev {
    fn new(rt: u32, dd: u32) -> Dev {
        let s = surf(rt);
        let (w, h, z) = s.map_or((640, 480, false), |s| (s.w, s.h, s.zbuf != 0));
        let mut rs = vec![0u32; 256];
        for (k, v) in [
            (4, 1),
            (RS_ZENABLE, z as u32),
            (8, 3),
            (RS_SHADEMODE, 2),
            (RS_ZWRITEENABLE, 1),
            (16, 1),
            (RS_SRCBLEND, 2),
            (RS_DESTBLEND, 1),
            (RS_CULLMODE, 3),
            (RS_ZFUNC, 4),
            (RS_ALPHAFUNC, 8),
            (RS_TEXTUREFACTOR, 0xffff_ffff),
            (RS_FOGEND, 0x3f80_0000),
            (RS_FOGDENSITY, 0x3f80_0000),
            (136, 1),
            (RS_LIGHTING, 1),
            (RS_COLORVERTEX, 1),
            (142, 1),
            (RS_DIFFUSEMATERIALSOURCE, 1),
            (RS_SPECULARMATERIALSOURCE, 2),
        ] {
            rs[k] = v;
        }
        let mut tss = [[0u32; 32]; 8];
        for (i, t) in tss.iter_mut().enumerate() {
            t[TSS_COLOROP] = if i == 0 { 4 } else { 1 };
            t[TSS_COLORARG1] = 2;
            t[TSS_COLORARG2] = 1;
            t[TSS_ALPHAOP] = if i == 0 { 2 } else { 1 };
            t[TSS_ALPHAARG1] = 2;
            t[TSS_ALPHAARG2] = 1;
            t[TSS_TEXCOORDINDEX] = i as u32;
            t[TSS_ADDRESS] = 1;
            t[TSS_ADDRESSU] = 1;
            t[TSS_ADDRESSV] = 1;
            t[TSS_MAGFILTER] = 1;
            t[TSS_MINFILTER] = 1;
            t[18] = 1;
        }
        let mut mat = [0f32; 17];
        mat[..4].copy_from_slice(&[1., 1., 1., 1.]);
        Dev { rt, dd, d3d_held: 0, vp: [0, 0, w, h], zr: (0., 1.), xf: [IDENT; 3], clip: IDENT, rs, tss, tex: [0; 8], mat, lights: Default::default(), rec: None, scene: false }
    }

    /// Applies a change; a texture change takes a reference of the new texture and returns
    /// the old one, whose reference the caller drops (release_surfs, outside the D lock).
    #[must_use]
    fn apply(&mut self, ch: &Ch) -> u32 {
        match ch {
            Ch::Rs(k, v) => self.rs[*k] = *v,
            Ch::Tss(s, k, v) => {
                self.tss[*s][*k] = *v;
                if *k == TSS_ADDRESS {
                    self.tss[*s][TSS_ADDRESSU] = *v;
                    self.tss[*s][TSS_ADDRESSV] = *v;
                }
            }
            Ch::Tex(s, t) => {
                let old = self.tex[*s];
                if old != *t {
                    hold_surf(*t);
                    self.tex[*s] = *t;
                    return old;
                }
            }
            Ch::Xf(t, m) => self.xf[*t] = *m,
            Ch::Vp(v, z) => {
                self.vp = *v;
                self.zr = *z;
            }
            Ch::Mat(m) => self.mat = *m,
            Ch::Light(i, l) => self.lights.entry(*i).or_insert(([0; 26], false)).0 = *l,
            Ch::LightOn(i, on) => {
                self.lights
                    .entry(*i)
                    .or_insert_with(|| {
                        // enabling a light never set: a white directional light along +z
                        let mut l = [0u32; 26];
                        l[0] = 3;
                        l[1..4].copy_from_slice(&[1f32.to_bits(); 3]);
                        l[18] = 1f32.to_bits();
                        (l, false)
                    })
                    .1 = *on
            }
        }
        0
    }

    /// Everything as changes (CreateStateBlock / CaptureStateBlock of a whole state).
    fn all(&self) -> Vec<Ch> {
        let mut v: Vec<Ch> = (0..self.rs.len()).map(|k| Ch::Rs(k, self.rs[k])).collect();
        for s in 0..8 {
            for k in 1..32 {
                v.push(Ch::Tss(s, k, self.tss[s][k]));
            }
            v.push(Ch::Tex(s, self.tex[s]));
        }
        for t in 0..3 {
            v.push(Ch::Xf(t, self.xf[t]));
        }
        v.push(Ch::Vp(self.vp, self.zr));
        v.push(Ch::Mat(self.mat));
        for (&i, l) in &self.lights {
            v.push(Ch::Light(i, l.0));
            v.push(Ch::LightOn(i, l.1));
        }
        v
    }

    /// The current value of each state a recorded change sets.
    fn capture(&self, ch: &Ch) -> Ch {
        match ch {
            Ch::Rs(k, _) => Ch::Rs(*k, self.rs[*k]),
            Ch::Tss(s, k, _) => Ch::Tss(*s, *k, self.tss[*s][*k]),
            Ch::Tex(s, _) => Ch::Tex(*s, self.tex[*s]),
            Ch::Xf(t, _) => Ch::Xf(*t, self.xf[*t]),
            Ch::Vp(..) => Ch::Vp(self.vp, self.zr),
            Ch::Mat(_) => Ch::Mat(self.mat),
            Ch::Light(i, l) => Ch::Light(*i, self.lights.get(i).map_or(*l, |x| x.0)),
            Ch::LightOn(i, b) => Ch::LightOn(*i, self.lights.get(i).map_or(*b, |x| x.1)),
        }
    }
}

struct D3d {
    devs: HashMap<u32, Dev>,
    /// Direct3D 6 devices' viewports (blocks, in the order added) and the current one
    dev3: HashMap<u32, (Vec<u32>, u32)>,
    /// IDirect3DViewport3 blocks
    vps: HashMap<u32, Vp3>,
    /// IDirect3DMaterial3 blocks (their handles): D3DMATERIAL as words
    mats: HashMap<u32, [u32; 20]>,
    /// IDirect3DLight blocks: D3DLIGHT2 as words, set
    /// IDirect3DLight block -> its D3DLIGHT2 (with D3DLIGHT_ACTIVE as D3DLIGHT's), the viewport it
    /// is in (0: none)
    lights3: HashMap<u32, ([u32; 20], u32)>,
    /// guest vertex buffer block -> (memory, fvf, count)
    vbs: HashMap<u32, (u32, u32, u32)>,
    blocks: HashMap<u32, Vec<Ch>>,
    next: u32,
}

static D: Mutex<Option<D3d>> = Mutex::new(None);

fn d3d<R>(f: impl FnOnce(&mut D3d) -> R) -> R {
    let mut g = D.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| D3d { devs: HashMap::default(), dev3: HashMap::default(), vps: HashMap::default(), mats: HashMap::default(), lights3: HashMap::default(), vbs: HashMap::default(), blocks: HashMap::default(), next: 1 }))
}

/// Runs `f` on the device `c` is called on (None if it is gone).
fn dev<R>(c: &Ctx, f: impl FnOnce(&mut Dev) -> R) -> Option<R> {
    let id = r32(c.a(0) + 4);
    d3d(|d| d.devs.get_mut(&id).map(f))
}

/// A state change: recorded while a state block is being recorded, applied otherwise.
fn set(c: &mut Ctx, ch: Ch) -> u32 {
    let r = dev(c, |d| match &mut d.rec {
        Some(r) => {
            r.push(ch);
            0
        }
        None => d.apply(&ch),
    });
    match r {
        Some(old) => {
            release_surfs(c, &[old]);
            D3D_OK
        }
        None => D3DERR_INVALIDCALL,
    }
}

/// Takes a reference of surface `id` (0 or a surface that is gone: nothing).
fn hold_surf(id: u32) {
    if id != 0 {
        with(|st| st.surfs.get_mut(&id).map(|x| x.refs += 1));
    }
}

/// Drops a reference of each surface (0 and surfaces that are gone are skipped).
fn release_surfs(c: &mut Ctx, ids: &[u32]) {
    for &id in ids {
        if id != 0 && surf(id).is_some() {
            release_surface(c, id);
        }
    }
}

/// Stores state block `h`. A block holds no reference of the textures it names (Windows'
/// don't: tests/winref ddrefs); surface ids are never reused, so one gone is skipped.
fn store_block(h: u32, v: Vec<Ch>) {
    d3d(|d| d.blocks.insert(h, v));
}

fn rf(a: u32) -> f32 {
    f32::from_bits(r32(a))
}

fn guid(p: u32, d1: u32, d2: u16, d3: u16, d4: [u8; 8]) {
    w32(p, d1);
    mem::w16(p + 4, d2);
    mem::w16(p + 6, d3);
    mem::write(p + 8, &d4);
}

// ---------------------------------------------------------------------------------------------
// IDirect3D7

/// The IDirect3D7 vtable (the interface is a block of its DirectDraw object: dd_iface(obj, 4)).
pub(super) fn d3d_vtbl() -> u32 {
    static B: OnceLock<u32> = OnceLock::new();
    *B.get_or_init(|| {
        vtable(
            "IDirect3D7",
            &[
                (Q_QueryInterface, 12),
                (D_AddRef, 4),
                (D_Release, 4),
                (Q_EnumDevices, 12),
                (Q_CreateDevice, 16),
                (Q_CreateVertexBuffer, 16),
                (Q_EnumZBufferFormats, 16),
                (ok, 4),
            ],
        )
    })
}

fn Q_QueryInterface(c: &mut Ctx) -> u32 {
    let obj = dd_of(c.a(0)).0;
    // (IUnknown: the interface asked on)
    let own = if r32(c.a(0)) == d3d3_vtbl() { 5 } else { 4 };
    match guid_first(c.a(1)) {
        0 => {
            put32(c.a(2), dd_iface(obj, own));
            D3D_OK
        }
        0xf504_9e77 if own == 4 => {
            put32(c.a(2), dd_iface(obj, 4));
            D3D_OK
        }
        0xbb22_3240 if own == 5 => {
            put32(c.a(2), dd_iface(obj, 5));
            D3D_OK
        }
        // (the DirectDraw interfaces, each of its own version: ddraw7's test_texture_wrong_caps
        // asks for IDirectDraw4)
        g @ (0x15e6_5ec0 | 0x9c59_509a | 0xb3a6_f3e0 | 0x6c14_db80) => {
            let v = match g {
                0x6c14_db80 => 0,
                0xb3a6_f3e0 => 1,
                0x9c59_509a => 2,
                _ => 3,
            };
            put32(c.a(2), dd_iface(obj, v));
            D3D_OK
        }
        _ => {
            put32(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}

const DEVICES: [(&str, &str, u32, u16, u16, [u8; 8], bool); 3] = [
    ("Microsoft Direct3D RGB Software Emulation", "RGB Emulation", 0xa466_5c60, 0x2673, 0x11cf, [0xa3, 0x1a, 0x00, 0xaa, 0x00, 0xb9, 0x33, 0x56], false),
    ("Microsoft Direct3D Hardware acceleration through Direct3D HAL", "Direct3D HAL", 0x84e6_3de0, 0x46aa, 0x11cf, [0x81, 0x6f, 0x00, 0x00, 0xc0, 0x20, 0x15, 0x6e], false),
    ("Microsoft Direct3D Hardware Transform and Lighting acceleration capable device", "Direct3D T&L HAL", 0xf504_9e78, 0x4861, 0x11d2, [0xa4, 0x07, 0x00, 0xa0, 0xc9, 0x06, 0x29, 0xa8], true),
];

/// D3DDEVICEDESC7
fn write_caps(p: u32, dev: usize) {
    mem::fill(p, 236, 0);
    let (_, _, d1, d2, d3, d4, tnl) = DEVICES[dev];
    w32(p, 0x1 | 0x10 | 0x40 | 0x100 | 0x200 | 0x400 | 0x800 | 0x2000 | 0x8000 | 0x8_0000 | if tnl { 0x1_0000 } else { 0 });
    for pc in [p + 4, p + 60] {
        w32(pc, 56);
        w32(pc + 4, 0x72); // MASKZ | CULLNONE | CULLCW | CULLCCW
        w32(pc + 8, 0x1 | 0x10 | 0x20); // DITHER | ZTEST | SUBPIXEL
        w32(pc + 12, 0xff);
        w32(pc + 16, 0x1fff);
        w32(pc + 20, 0x1fff);
        w32(pc + 24, 0xff);
        w32(pc + 28, 0x000f_ffff);
        w32(pc + 32, 0x1 | 0x4 | 0x8 | 0x80); // PERSPECTIVE | ALPHA | TRANSPARENCY | ALPHAPALETTE
        w32(pc + 36, 0x0300_0303);
        w32(pc + 40, 0xff);
        w32(pc + 44, 0x1f);
    }
    w32(p + 116, 0x500); // DDBD_16 | DDBD_32
    w32(p + 120, 0x700);
    w32(p + 124, 1);
    w32(p + 128, 1);
    w32(p + 132, 2048);
    w32(p + 136, 2048);
    w32(p + 140, 2048);
    w32(p + 144, 2048);
    w32(p + 148, 1);
    for (o, v) in [(152, -2048f32), (156, -2048.), (160, 2048.), (164, 2048.)] {
        w32(p + o, v.to_bits());
    }
    w32(p + 176, 0x8_0008);
    w32(p + 180, 0x03ff_ffff);
    mem::w16(p + 184, 8);
    mem::w16(p + 186, 8);
    w32(p + 188, 8);
    w32(p + 192, 1e10f32.to_bits());
    guid(p + 196, d1, d2, d3, d4);
    w32(p + 216, 0x3b);
}

/// EnumDevices(callback, context): each device's description and name stay where they were
/// handed after the call (Wine's d3d tests: D3D7EnumLifetimeTest), and any answer but
/// DDENUMRET_OK stops it
fn Q_EnumDevices(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(1), c.a(2));
    if cb == 0 {
        return DDERR_INVALIDPARAMS;
    }
    static NAMES: OnceLock<u32> = OnceLock::new();
    let names = *NAMES.get_or_init(|| {
        let p = mem::heap_alloc(256 * DEVICES.len() as u32, true);
        for (i, (desc, name, ..)) in DEVICES.iter().enumerate() {
            let q = p + 256 * i as u32;
            mem::write(q, desc.as_bytes());
            mem::w8(q + desc.len() as u32, 0);
            mem::write(q + 128, name.as_bytes());
            mem::w8(q + 128 + name.len() as u32, 0);
        }
        p
    });
    let m = mem::heap_alloc(236, true);
    for i in 0..DEVICES.len() {
        write_caps(m, i);
        let q = names + 256 * i as u32;
        if c.call(cb, &[q, q + 128, m, ctx]) != 1 {
            break;
        }
    }
    mem::heap_free(m);
    D3D_OK
}

fn dev_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirect3DDevice7",
            &[
                (V_QueryInterface, 12),
                (V_AddRef, 4),
                (V_Release, 4),
                (V_GetCaps, 8),
                (V_EnumTextureFormats, 12),
                (V_BeginScene, 4),
                (V_EndScene, 4),
                (V_GetDirect3D, 8),
                (V_SetRenderTarget, 12),
                (V_GetRenderTarget, 8),
                (V_Clear, 28),
                (V_SetTransform, 12),
                (V_GetTransform, 12),
                (V_SetViewport, 8),
                (V_MultiplyTransform, 12),
                (V_GetViewport, 8),
                (V_SetMaterial, 8),
                (V_GetMaterial, 8),
                (V_SetLight, 12),
                (V_GetLight, 12),
                (V_SetRenderState, 12),
                (V_GetRenderState, 12),
                (V_BeginStateBlock, 4),
                (V_EndStateBlock, 8),
                (ok, 8),
                (V_DrawPrimitive, 24),
                (V_DrawIndexedPrimitive, 32),
                (ok, 8),
                (V_GetClipStatus, 8),
                (V_DrawPrimitiveStrided, 24),
                (V_DrawIndexedPrimitiveStrided, 32),
                (V_DrawPrimitiveVB, 24),
                (V_DrawIndexedPrimitiveVB, 32),
                (V_ComputeSphereVisibility, 24),
                (V_GetTexture, 12),
                (V_SetTexture, 12),
                (V_GetTextureStageState, 16),
                (V_SetTextureStageState, 16),
                (V_ValidateDevice, 8),
                (V_ApplyStateBlock, 8),
                (V_CaptureStateBlock, 8),
                (V_DeleteStateBlock, 8),
                (V_CreateStateBlock, 12),
                (V_Load, 24),
                (V_LightEnable, 12),
                (V_GetLightEnable, 12),
                (ok, 12),
                (V_GetClipPlane, 12),
                (V_GetInfo, 16),
            ],
        )
    })
}

fn Q_CreateDevice(c: &mut Ctx) -> u32 {
    create_dev(c, dev_vtbl())
}

/// CreateDevice(clsid, render target, out): a device block of vtable `vt` ([vtable, id, refs])
fn create_dev(c: &mut Ctx, vt: u32) -> u32 {
    let (target, out) = (c.a(2), c.a(3));
    let Some((rt, _)) = sid(target) else {
        put32(out, 0);
        return D3DERR_INVALIDCALL;
    };
    let dd = dd_of(c.a(0)).0;
    hold_surf(rt);
    dd_addref(c.a(0));
    let mut dv = Dev::new(rt, dd);
    dv.d3d_held = c.a(0);
    let id = d3d(|d| {
        let id = d.next;
        d.next += 1;
        d.devs.insert(id, dv);
        id
    });
    let b = mem::heap_alloc(12, true);
    w32(b, vt);
    w32(b + 4, id);
    w32(b + 8, 1);
    put32(out, b);
    D3D_OK
}

fn vb_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirect3DVertexBuffer7",
            &[
                (B_QueryInterface, 12),
                (B_AddRef, 4),
                (B_Release, 4),
                (B_Lock, 16),
                (ok, 4),
                (B_ProcessVertices, 32),
                (B_GetVertexBufferDesc, 8),
                (ok, 12),
                (B_ProcessVerticesStrided, 32),
            ],
        )
    })
}

fn Q_CreateVertexBuffer(c: &mut Ctx) -> u32 {
    let (desc, out) = (c.a(1), c.a(2));
    put32(out, 0);
    if desc == 0 {
        return D3DERR_INVALIDCALL;
    }
    let (fvf, n) = (r32(desc + 8), r32(desc + 12));
    if n == 0 {
        return D3DERR_INVALIDCALL;
    }
    let Some(size) = vb_size(fvf, n) else { return D3DERR_VBUF_CREATE_FAILED };
    let m = mem::heap_alloc(size.max(4), true);
    if m == 0 {
        return D3DERR_VBUF_CREATE_FAILED;
    }
    let b = mem::heap_alloc(12, true);
    w32(b, vb_vtbl());
    w32(b + 4, r32(desc + 4));
    w32(b + 8, 1);
    d3d(|d| d.vbs.insert(b, (m, fvf, n)));
    put32(out, b);
    D3D_OK
}

fn Q_EnumZBufferFormats(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(2), c.a(3));
    let p = mem::heap_alloc(32, true);
    for (bits, mask) in [(16u32, 0xffffu32), (24, 0xff_ffff), (32, 0xffff_ffff)] {
        mem::fill(p, 32, 0);
        w32(p, 32);
        w32(p + 4, 0x400);
        w32(p + 12, bits.min(32));
        w32(p + 20, mask);
        if c.call(cb, &[p, ctx]) == 0 {
            break;
        }
    }
    mem::heap_free(p);
    D3D_OK
}

// ---------------------------------------------------------------------------------------------
// IDirect3DVertexBuffer7

fn vb(c: &Ctx) -> Option<(u32, u32, u32)> {
    d3d(|d| d.vbs.get(&c.a(0)).copied())
}

fn B_QueryInterface(c: &mut Ctx) -> u32 {
    put32(c.a(2), c.a(0));
    B_AddRef(c);
    D3D_OK
}
fn B_AddRef(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    w32(b + 8, r32(b + 8) + 1);
    r32(b + 8)
}
fn B_Release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = r32(b + 8).saturating_sub(1);
    w32(b + 8, n);
    if n == 0 {
        if let Some((m, ..)) = d3d(|d| d.vbs.remove(&b)) {
            mem::heap_free(m);
        }
    }
    n
}
/// The bytes of `n` vertices of `fvf` (None: past 32 bits or the cap). An FVF without a
/// position is sized as with D3DFVF_XYZ, as Windows does (tests/winref ddrefs).
fn vb_size(fvf: u32, n: u32) -> Option<u32> {
    let fvf = if fvf & 0xe == 0 { fvf | 0x2 } else { fvf };
    Layout::of(fvf).size.checked_mul(n).filter(|&s| s <= MAX_VB)
}

/// The largest vertex buffer made (bigger ones fail as out of memory would)
const MAX_VB: u32 = 256 << 20;

fn B_Lock(c: &mut Ctx) -> u32 {
    let Some((m, fvf, n)) = vb(c) else { return D3DERR_INVALIDCALL };
    put32(c.a(2), m);
    put32(c.a(3), vb_size(fvf, n).unwrap_or(0));
    D3D_OK
}
fn B_GetVertexBufferDesc(c: &mut Ctx) -> u32 {
    let Some((_, fvf, n)) = vb(c) else { return D3DERR_INVALIDCALL };
    let p = c.a(1);
    if p == 0 {
        return DDERR_INVALIDPARAMS;
    }
    // (dwSize is left as the caller put it, whatever it is: Wine's d3d tests VertexBufferDescTest)
    w32(p + 4, r32(c.a(0) + 4));
    w32(p + 8, fvf);
    w32(p + 12, n);
    D3D_OK
}

/// Transforms (and lights) source vertices into this XYZRHW buffer.
fn process(c: &mut Ctx, src: Option<Src>, src_index: u32, src_fvf: u32) -> u32 {
    let (op, dst_index, count, devb) = (c.a(1), c.a(2), c.a(3), c.a(6));
    let Some((dm, dfvf, dn)) = vb(c) else { return D3DERR_INVALIDCALL };
    let Some(src) = src else { return D3DERR_INVALIDCALL };
    let Some(mut d) = d3d(|d| d.devs.get(&r32(devb + 4)).cloned()) else { return D3DERR_INVALIDCALL };
    // (lit only with D3DVOP_LIGHT; a Direct3D 6 device has no D3DRENDERSTATE_LIGHTING: a material
    // and normals light)
    let lit = if r32(devb) == dev3_vtbl() { dev3_lights(&d, src_fvf) } else { d.rs[RS_LIGHTING] != 0 };
    d.rs[RS_LIGHTING] = (lit && op & D3DVOP_LIGHT != 0) as u32;
    let dl = Layout::of(dfvf);
    let t = Xform::new(&d);
    for i in 0..count.min(dn.saturating_sub(dst_index)) {
        let v = src.fetch(src_index + i, &d, &t);
        let a = dm + (dst_index + i) * dl.size;
        let s = t.screen(&v.clip);
        for (k, x) in s.iter().enumerate() {
            w32(a + 4 * k as u32, x.to_bits());
        }
        if dl.diffuse != 0 {
            w32(a + dl.diffuse, pack(v.diff));
        }
        if dl.specular != 0 {
            w32(a + dl.specular, pack(v.spec));
        }
        for k in 0..dl.ntex {
            for j in 0..dl.tsize[k] as usize {
                w32(a + dl.tex[k] + 4 * j as u32, v.tex[k][j.min(1)].to_bits());
            }
        }
    }
    D3D_OK
}
fn B_ProcessVertices(c: &mut Ctx) -> u32 {
    let (srcb, si) = (c.a(4), c.a(5));
    let vb = d3d(|d| d.vbs.get(&srcb).copied());
    let src = vb.map(|(m, fvf, _)| Src::packed(fvf, m));
    process(c, src, si, vb.map_or(0, |x| x.1))
}
fn B_ProcessVerticesStrided(c: &mut Ctx) -> u32 {
    let src = Src::strided(c.a(5), c.a(4));
    process(c, Some(src), 0, c.a(4))
}

// ---------------------------------------------------------------------------------------------
// IDirect3DDevice7: objects and state

fn V_QueryInterface(c: &mut Ctx) -> u32 {
    match guid_first(c.a(1)) {
        0xf504_9e79 | 0 => {
            put32(c.a(2), c.a(0));
            V_AddRef(c);
            D3D_OK
        }
        _ => {
            put32(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
fn V_AddRef(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    w32(b + 8, r32(b + 8) + 1);
    r32(b + 8)
}
fn V_Release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = r32(b + 8).saturating_sub(1);
    w32(b + 8, n);
    if n == 0 {
        if let Some(d) = d3d(|d| d.devs.remove(&r32(b + 4))) {
            let mut held = d.tex.to_vec();
            held.push(d.rt);
            release_surfs(c, &held);
            if d.d3d_held != 0 {
                dd_release(d.d3d_held);
            }
        }
    }
    n
}
fn V_GetCaps(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return DDERR_INVALIDPARAMS;
    }
    write_caps(c.a(1), 2);
    D3D_OK
}

/// The texture formats offered: (bpp, flags, R, G, B, A masks)
const TEX_FORMATS: [(u32, u32, u32, u32, u32, u32); 6] = [
    (16, 0x40, 0xf800, 0x07e0, 0x001f, 0),
    (16, 0x40, 0x7c00, 0x03e0, 0x001f, 0),
    (16, 0x41, 0x7c00, 0x03e0, 0x001f, 0x8000),
    (16, 0x41, 0x0f00, 0x00f0, 0x000f, 0xf000),
    (32, 0x40, 0xff_0000, 0xff00, 0xff, 0),
    (32, 0x41, 0xff_0000, 0xff00, 0xff, 0xff00_0000),
];

fn V_EnumTextureFormats(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(1), c.a(2));
    let p = mem::heap_alloc(32, true);
    for (bpp, fl, r, g, b, a) in TEX_FORMATS {
        write_pf(p, bpp as u16, [fl, r, g, b, a]);
        if c.call(cb, &[p, ctx]) == 0 {
            break;
        }
    }
    mem::heap_free(p);
    D3D_OK
}
fn V_BeginScene(c: &mut Ctx) -> u32 {
    match dev(c, |d| std::mem::replace(&mut d.scene, true)) {
        None => D3DERR_INVALIDCALL,
        Some(true) => D3DERR_SCENE_IN_SCENE,
        Some(false) => D3D_OK,
    }
}
fn V_EndScene(c: &mut Ctx) -> u32 {
    match dev(c, |d| std::mem::replace(&mut d.scene, false)) {
        None => return D3DERR_INVALIDCALL,
        Some(false) => return D3DERR_SCENE_NOT_IN_SCENE,
        Some(true) => {}
    }
    if let Some(rt) = dev(c, |d| d.rt) {
        if let Some(s) = surf(rt) {
            present_if_primary(c, rt, Rect { l: 0, t: 0, r: s.w, b: s.h });
        }
    }
    D3D_OK
}
fn V_GetDirect3D(c: &mut Ctx) -> u32 {
    let obj = dev(c, |d| d.dd).unwrap_or(0);
    let b = dd_iface(obj, 4);
    put32(c.a(1), b);
    if b == 0 { D3DERR_INVALIDCALL } else { D3D_OK }
}
/// SetRenderTarget(surface, flags), as Windows (Wine's d3d tests SetRenderTargetTest): a
/// surface without DDSCAPS_3DDEVICE is refused; a depth buffer is taken (GetRenderTarget hands
/// it back, nothing is drawn to it) but answered DDERR_INVALIDPIXELFORMAT; the viewport stays
/// as it was.
fn V_SetRenderTarget(c: &mut Ctx) -> u32 {
    let Some((rt, _)) = sid(c.a(1)) else { return DDERR_INVALIDPARAMS };
    let Some(s) = surf(rt) else { return DDERR_INVALIDPARAMS };
    if s.caps & CAPS_3DDEVICE == 0 {
        return DDERR_INVALIDCAPS;
    }
    hold_surf(rt);
    let old = dev(c, |d| std::mem::replace(&mut d.rt, rt));
    release_surfs(c, &[old.unwrap_or(rt)]);
    if s.caps & CAPS_ZBUFFER != 0 { DDERR_INVALIDPIXELFORMAT } else { D3D_OK }
}
fn V_GetRenderTarget(c: &mut Ctx) -> u32 {
    let Some(rt) = dev(c, |d| d.rt) else { return D3DERR_INVALIDCALL };
    with(|st| st.surfs.get_mut(&rt).map(|x| x.refs += 1));
    put32(c.a(1), block(rt, 2));
    D3D_OK
}
fn matrix(p: u32) -> [f32; 16] {
    std::array::from_fn(|i| rf(p + 4 * i as u32))
}
fn xf_index(t: u32) -> Option<usize> {
    match t {
        1..=3 => Some(t as usize - 1),
        _ => None,
    }
}
fn V_SetTransform(c: &mut Ctx) -> u32 {
    let Some(i) = xf_index(c.a(1)) else { return D3D_OK };
    set(c, Ch::Xf(i, matrix(c.a(2))))
}
fn V_GetTransform(c: &mut Ctx) -> u32 {
    let m = xf_index(c.a(1)).and_then(|i| dev(c, |d| d.xf[i])).unwrap_or(IDENT);
    for (k, v) in m.iter().enumerate() {
        w32(c.a(2) + 4 * k as u32, v.to_bits());
    }
    D3D_OK
}
fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    std::array::from_fn(|i| {
        let (r, col) = (i / 4, i % 4);
        (0..4).map(|k| a[r * 4 + k] * b[k * 4 + col]).sum()
    })
}
fn V_MultiplyTransform(c: &mut Ctx) -> u32 {
    let Some(i) = xf_index(c.a(1)) else { return D3D_OK };
    let m = matrix(c.a(2));
    let Some(cur) = dev(c, |d| d.xf[i]) else { return D3DERR_INVALIDCALL };
    set(c, Ch::Xf(i, mul(&m, &cur)))
}
fn V_SetViewport(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let v = [r32(p) as i32, r32(p + 4) as i32, r32(p + 8) as i32, r32(p + 12) as i32];
    set(c, Ch::Vp(v, (rf(p + 16), rf(p + 20))))
}
fn V_GetViewport(c: &mut Ctx) -> u32 {
    let Some((v, z)) = dev(c, |d| (d.vp, d.zr)) else { return D3DERR_INVALIDCALL };
    let p = c.a(1);
    for (k, x) in v.iter().enumerate() {
        w32(p + 4 * k as u32, *x as u32);
    }
    w32(p + 16, z.0.to_bits());
    w32(p + 20, z.1.to_bits());
    D3D_OK
}
fn V_SetMaterial(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return DDERR_INVALIDPARAMS;
    }
    set(c, Ch::Mat(std::array::from_fn(|i| rf(p + 4 * i as u32))))
}
fn V_GetMaterial(c: &mut Ctx) -> u32 {
    let m = dev(c, |d| d.mat).unwrap_or([0.; 17]);
    for (k, v) in m.iter().enumerate() {
        w32(c.a(1) + 4 * k as u32, v.to_bits());
    }
    D3D_OK
}
fn V_SetLight(c: &mut Ctx) -> u32 {
    let p = c.a(2);
    if p == 0 {
        return D3DERR_INVALIDCALL;
    }
    let l = std::array::from_fn(|i| r32(p + 4 * i as u32));
    if !light_ok(&l) {
        return D3DERR_INVALIDCALL;
    }
    set(c, Ch::Light(c.a(1), l))
}
/// What SetLight takes (Wine's d3d tests, as Windows): a point, spot or directional light
/// (D3DLIGHTTYPE 1..=3), and a point or spot light's constant attenuation not below 0 (a NaN
/// passes; a directional light's attenuation is not looked at)
fn light_ok(l: &[u32; 26]) -> bool {
    match l[0] {
        3 => true,
        1 | 2 => !(f32::from_bits(l[21]) < 0.),
        _ => false,
    }
}
fn V_GetLight(c: &mut Ctx) -> u32 {
    let i = c.a(1);
    let Some(l) = dev(c, |d| d.lights.get(&i).map(|x| x.0)).flatten() else { return D3DERR_INVALIDCALL };
    for (k, v) in l.iter().enumerate() {
        w32(c.a(2) + 4 * k as u32, *v);
    }
    D3D_OK
}
fn V_LightEnable(c: &mut Ctx) -> u32 {
    set(c, Ch::LightOn(c.a(1), c.a(2) != 0))
}
/// GetLightEnable's answer for an enabled light is 128, not TRUE, as Direct3D 9's (winref ddrefs)
fn light_on_value(on: bool) -> u32 {
    if on { 128 } else { 0 }
}
fn V_GetLightEnable(c: &mut Ctx) -> u32 {
    let i = c.a(1);
    let Some(on) = dev(c, |d| d.lights.get(&i).map(|x| x.1)).flatten() else { return D3DERR_INVALIDCALL };
    put32(c.a(2), light_on_value(on));
    D3D_OK
}
fn V_SetRenderState(c: &mut Ctx) -> u32 {
    let k = c.a(1) as usize;
    if k >= 256 {
        return D3DERR_INVALIDCALL;
    }
    set(c, Ch::Rs(k, c.a(2)))
}
fn V_GetRenderState(c: &mut Ctx) -> u32 {
    let k = c.a(1) as usize;
    put32(c.a(2), dev(c, |d| d.rs.get(k).copied().unwrap_or(0)).unwrap_or(0));
    D3D_OK
}
fn V_GetClipStatus(c: &mut Ctx) -> u32 {
    mem::fill(c.a(1), 24, 0);
    D3D_OK
}
fn V_GetTexture(c: &mut Ctx) -> u32 {
    let s = c.a(1) as usize & 7;
    let t = dev(c, |d| d.tex[s]).unwrap_or(0);
    if t == 0 || surf(t).is_none() {
        put32(c.a(2), 0);
    } else {
        with(|st| st.surfs.get_mut(&t).map(|x| x.refs += 1));
        put32(c.a(2), block(t, 2));
    }
    D3D_OK
}
fn V_SetTexture(c: &mut Ctx) -> u32 {
    let s = c.a(1) as usize;
    if s >= 8 {
        return D3DERR_INVALIDCALL;
    }
    let t = sid(c.a(2)).map_or(0, |x| x.0);
    set(c, Ch::Tex(s, t))
}
fn V_GetTextureStageState(c: &mut Ctx) -> u32 {
    let (s, k) = (c.a(1) as usize, c.a(2) as usize);
    put32(c.a(3), if s < 8 && k < 32 { dev(c, |d| d.tss[s][k]).unwrap_or(0) } else { 0 });
    D3D_OK
}
fn V_SetTextureStageState(c: &mut Ctx) -> u32 {
    let (s, k) = (c.a(1) as usize, c.a(2) as usize);
    if s >= 8 || k >= 32 {
        return D3DERR_INVALIDCALL;
    }
    set(c, Ch::Tss(s, k, c.a(3)))
}
fn V_ValidateDevice(c: &mut Ctx) -> u32 {
    put32(c.a(1), 1);
    D3D_OK
}
fn V_BeginStateBlock(c: &mut Ctx) -> u32 {
    dev(c, |d| d.rec = Some(Vec::new())).map_or(D3DERR_INVALIDCALL, |_| D3D_OK)
}
fn new_block(v: Vec<Ch>) -> u32 {
    let h = d3d(|d| {
        let h = d.next;
        d.next += 1;
        h
    });
    store_block(h, v);
    h
}
fn V_EndStateBlock(c: &mut Ctx) -> u32 {
    let Some(v) = dev(c, |d| d.rec.take()).flatten() else { return D3DERR_INVALIDCALL };
    let h = new_block(v);
    put32(c.a(1), h);
    D3D_OK
}
fn V_CreateStateBlock(c: &mut Ctx) -> u32 {
    let Some(all) = dev(c, |d| d.all()) else { return D3DERR_INVALIDCALL };
    let h = new_block(all);
    put32(c.a(2), h);
    D3D_OK
}
fn V_ApplyStateBlock(c: &mut Ctx) -> u32 {
    let Some(v) = d3d(|d| d.blocks.get(&c.a(1)).cloned()) else { return D3DERR_INVALIDCALL };
    let Some(old) = dev(c, |d| v.iter().map(|ch| d.apply(ch)).collect::<Vec<_>>()) else { return D3DERR_INVALIDCALL };
    release_surfs(c, &old);
    D3D_OK
}
fn V_CaptureStateBlock(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    let Some(v) = d3d(|d| d.blocks.get(&h).cloned()) else { return D3DERR_INVALIDCALL };
    let Some(nv) = dev(c, |d| v.iter().map(|ch| d.capture(ch)).collect::<Vec<_>>()) else { return D3DERR_INVALIDCALL };
    store_block(h, nv);
    D3D_OK
}
fn V_DeleteStateBlock(c: &mut Ctx) -> u32 {
    if d3d(|d| d.blocks.remove(&c.a(1))).is_none() {
        return D3DERR_INVALIDCALL;
    }
    D3D_OK
}
fn V_ComputeSphereVisibility(c: &mut Ctx) -> u32 {
    mem::fill(c.a(5), 4 * c.a(3), 0);
    D3D_OK
}
fn V_GetClipPlane(c: &mut Ctx) -> u32 {
    mem::fill(c.a(2), 16, 0);
    D3D_OK
}
fn V_GetInfo(_: &mut Ctx) -> u32 {
    1 // S_FALSE: no information
}

/// What Load copies, given the sizes of the source's and the destination's mipmap levels: the
/// destination's first level is the source's level of its size, and each level after it the
/// next of both; the rect (of the source's top level, all of it when None) and the point (in
/// the same scale, 0,0 when None) halve with each level. None (DDERR_INVALIDPARAMS): no source
/// level of that size, more destination levels than the source has from there, an empty rect,
/// or one off either surface (ddraw7's test_device_load).
fn load_plan(src: &[(i32, i32)], dst: &[(i32, i32)], rect: Option<Rect>, point: Option<(i32, i32)>) -> Option<Vec<(usize, Rect, (i32, i32))>> {
    let first = *dst.first()?;
    let off = src.iter().position(|&s| s == first)?;
    if dst.len() > src.len() - off {
        return None;
    }
    let mut r = rect.unwrap_or(Rect { l: 0, t: 0, r: src[0].0, b: src[0].1 });
    if !rect_ok(r, src[0].0, src[0].1) {
        return None;
    }
    let mut p = point.unwrap_or((0, 0));
    let half = |r: &mut Rect, p: &mut (i32, i32)| {
        *r = Rect { l: r.l / 2, t: r.t / 2, r: (r.r + 1) / 2, b: (r.b + 1) / 2 };
        *p = (p.0 / 2, p.1 / 2);
    };
    for _ in 0..off {
        half(&mut r, &mut p);
    }
    let (w, h) = dst[0];
    if p.0 < 0 || p.1 < 0 || p.0 + (r.r - r.l) > w || p.1 + (r.b - r.t) > h {
        return None;
    }
    let mut plan = vec![];
    for k in 0..dst.len() {
        if k > 0 {
            half(&mut r, &mut p);
        }
        plan.push((off + k, r, p));
    }
    Some(plan)
}

/// A surface and its mipmap levels below it.
fn levels(top: u32) -> Vec<Surf> {
    let mut v = vec![];
    let mut at = top;
    while let Some(x) = surf(at).filter(|_| v.len() < 16) {
        at = x.mip;
        v.push(x);
    }
    v
}

/// The top of a cube map's face `face` (the root, or one of its other faces).
fn face_top(root: &Surf, root_id: u32, face: u32) -> Option<u32> {
    if root.caps2 & face != 0 {
        return Some(root_id);
    }
    root.faces.iter().copied().find(|&f| surf(f).is_some_and(|x| x.caps2 & face != 0))
}

/// Load(dst, dst point, src, src rect, flags): a copy between textures of one format, level by
/// level (load_plan), of each cube map face `flags` names; the palette's colours and the source
/// color key (its low value for both) go too.
fn V_Load(c: &mut Ctx) -> u32 {
    let (Some((d, _)), Some((s, _))) = (sid(c.a(1)), sid(c.a(3))) else { return DDERR_INVALIDPARAMS };
    let (Some(ds), Some(ss)) = (surf(d), surf(s)) else { return DDERR_INVALIDPARAMS };
    let rect = (c.a(4) != 0).then(|| Rect::read(c.a(4)));
    let point = (c.a(2) != 0).then(|| (r32(c.a(2)) as i32, r32(c.a(2) + 4) as i32));
    let faces = c.a(5) & CUBE_FACES;
    let cube = |x: &Surf| x.caps2 & DDSCAPS2_CUBEMAP != 0;
    if cube(&ds) != cube(&ss) || (faces != 0 && !cube(&ds)) {
        return DDERR_INVALIDPARAMS;
    }
    let pairs: Vec<(u32, u32)> = if cube(&ds) {
        let mut v = vec![];
        for f in CUBE_ORDER.iter().copied().filter(|f| faces & f != 0) {
            let (Some(a), Some(b)) = (face_top(&ds, d, f), face_top(&ss, s, f)) else { return DDERR_INVALIDPARAMS };
            v.push((a, b));
        }
        v
    } else {
        vec![(d, s)]
    };
    let mut work = vec![];
    for (a, b) in pairs {
        let (dl, sl) = (levels(a), levels(b));
        let size = |v: &[Surf]| v.iter().map(|x| (x.w, x.h)).collect::<Vec<_>>();
        let Some(plan) = load_plan(&size(&sl), &size(&dl), rect, point) else { return DDERR_INVALIDPARAMS };
        for (k, (si, r, (x, y))) in plan.into_iter().enumerate() {
            work.push((dl[k].clone(), sl[si].clone(), r, x, y));
        }
    }
    for (dx, sx, r, x, y) in work {
        if dx.bpp == sx.bpp && r.l < r.r && r.t < r.b {
            blit(&dx, Rect { l: x, t: y, r: x + r.r - r.l, b: y + r.b - r.t }, &sx, r, None, (false, false));
        }
    }
    if ds.palette != 0 && ss.palette != 0 {
        with(|st| {
            if let Some(p) = st.palettes.get(&ss.palette).copied() {
                st.palettes.insert(ds.palette, p);
            }
        });
    }
    if let Some((lo, _)) = ss.ck_src {
        with(|st| st.surfs.get_mut(&d).map(|x| x.ck_src = Some((lo, lo))));
    }
    D3D_OK
}

// ---------------------------------------------------------------------------------------------
// pixel formats

/// Unpacks pixels of one format to 0..1 channels and packs them back.
#[derive(Clone, Copy)]
struct Fmt {
    /// (shift, max value) of R, G, B, A (max 0: the channel is not there)
    ch: [(u32, u32); 4],
}

impl Fmt {
    fn of(s: &Surf) -> Fmt {
        let m = [s.pf[1], s.pf[2], s.pf[3], s.pf[4]];
        Fmt { ch: m.map(|m| if m == 0 { (0, 0) } else { (m.trailing_zeros(), m >> m.trailing_zeros()) }) }
    }
    #[inline]
    fn unpack(&self, v: u32) -> C4 {
        std::array::from_fn(|i| {
            let (sh, mx) = self.ch[i];
            if mx == 0 { 1. } else { ((v >> sh) & mx) as f32 / mx as f32 }
        })
    }
    #[inline]
    fn pack(&self, c: C4) -> u32 {
        let mut v = 0;
        for i in 0..4 {
            let (sh, mx) = self.ch[i];
            v |= ((c[i].clamp(0., 1.) * mx as f32 + 0.5) as u32) << sh;
        }
        v
    }
}

fn unpack_argb(v: u32) -> C4 {
    [(v >> 16 & 0xff) as f32 / 255., (v >> 8 & 0xff) as f32 / 255., (v & 0xff) as f32 / 255., (v >> 24) as f32 / 255.]
}
fn pack(c: C4) -> u32 {
    let b = |x: f32| (x.clamp(0., 1.) * 255. + 0.5) as u32;
    b(c[3]) << 24 | b(c[0]) << 16 | b(c[1]) << 8 | b(c[2])
}

// ---------------------------------------------------------------------------------------------
// vertices

/// Where each part of an FVF vertex is (byte offsets, 0: absent but for position).
#[derive(Clone, Copy)]
struct Layout {
    size: u32,
    rhw: bool,
    normal: u32,
    diffuse: u32,
    specular: u32,
    ntex: usize,
    tex: [u32; 8],
    tsize: [u32; 8],
}

impl Layout {
    fn of(fvf: u32) -> Layout {
        let pos = fvf & 0xe;
        let mut o = match pos {
            0x4 => 16,
            0x2 => 12,
            p if p >= 6 => 12 + 4 * ((p - 4) / 2),
            _ => 0,
        };
        let mut l = Layout { size: 0, rhw: pos == FVF_XYZRHW, normal: 0, diffuse: 0, specular: 0, ntex: 0, tex: [0; 8], tsize: [2; 8] };
        if fvf & FVF_NORMAL != 0 {
            l.normal = o;
            o += 12;
        }
        if fvf & FVF_RESERVED1 != 0 {
            o += 4;
        }
        if fvf & FVF_DIFFUSE != 0 {
            l.diffuse = o;
            o += 4;
        }
        if fvf & FVF_SPECULAR != 0 {
            l.specular = o;
            o += 4;
        }
        l.ntex = ((fvf >> 8) & 0xf).min(8) as usize;
        for i in 0..l.ntex {
            l.tsize[i] = [2, 3, 4, 1][(fvf >> (16 + 2 * i) & 3) as usize];
            l.tex[i] = o;
            o += 4 * l.tsize[i];
        }
        l.size = o;
        l
    }
}

/// A source of vertices: (address, stride) of each part.
#[derive(Clone, Copy)]
struct Src {
    l: Layout,
    pos: (u32, u32),
    normal: (u32, u32),
    diffuse: (u32, u32),
    specular: (u32, u32),
    tex: [(u32, u32); 8],
}

impl Src {
    fn packed(fvf: u32, base: u32) -> Src {
        let l = Layout::of(fvf);
        let at = |o: u32, present: bool| if present { (base + o, l.size) } else { (0, 0) };
        Src {
            l,
            pos: (base, l.size),
            normal: at(l.normal, fvf & FVF_NORMAL != 0),
            diffuse: at(l.diffuse, fvf & FVF_DIFFUSE != 0),
            specular: at(l.specular, fvf & FVF_SPECULAR != 0),
            tex: std::array::from_fn(|i| at(l.tex[i], i < l.ntex)),
        }
    }
    /// D3DDRAWPRIMITIVESTRIDEDDATA
    fn strided(fvf: u32, p: u32) -> Src {
        let l = Layout::of(fvf);
        let e = |i: u32, present: bool| if present { (r32(p + 8 * i), r32(p + 8 * i + 4)) } else { (0, 0) };
        Src {
            l,
            pos: e(0, true),
            normal: e(1, fvf & FVF_NORMAL != 0),
            diffuse: e(2, fvf & FVF_DIFFUSE != 0),
            specular: e(3, fvf & FVF_SPECULAR != 0),
            tex: std::array::from_fn(|i| e(4 + i as u32, i < l.ntex)),
        }
    }

    fn fetch(&self, i: u32, d: &Dev, t: &Xform) -> Vtx {
        let p = self.pos.0 + i * self.pos.1;
        let (x, y, z) = (rf(p), rf(p + 4), rf(p + 8));
        let diff = (self.diffuse.0 != 0).then(|| unpack_argb(r32(self.diffuse.0 + i * self.diffuse.1)));
        let spec = (self.specular.0 != 0).then(|| unpack_argb(r32(self.specular.0 + i * self.specular.1)));
        let mut tex = [[0f32; 2]; 8];
        for k in 0..self.l.ntex {
            let a = self.tex[k].0 + i * self.tex[k].1;
            tex[k] = [rf(a), if self.l.tsize[k] >= 2 { rf(a + 4) } else { 0. }];
        }
        if self.l.rhw {
            let rhw = rf(p + 12);
            let rhw = if rhw.is_finite() && rhw != 0. { rhw } else { 1. };
            return Vtx {
                clip: [x, y, z, 1. / rhw],
                screen: true,
                diff: diff.unwrap_or([1.; 4]),
                spec: spec.unwrap_or([0.; 4]),
                tex,
            };
        }
        let (diff, spec) = if d.rs[RS_LIGHTING] != 0 {
            let n = (self.normal.0 != 0).then(|| {
                let a = self.normal.0 + i * self.normal.1;
                [rf(a), rf(a + 4), rf(a + 8)]
            });
            t.light(d, [x, y, z], n, diff, spec)
        } else {
            (diff.unwrap_or([1.; 4]), spec.unwrap_or([0.; 4]))
        };
        let v = [x, y, z, 1.];
        let clip = std::array::from_fn(|c| (0..4).map(|r| v[r] * t.wvp[r * 4 + c]).sum());
        let mut spec = spec;
        if let Some(f) = vertex_fog(d, &t.wv, v) {
            spec[3] = f;
        }
        Vtx { clip, screen: false, diff, spec, tex }
    }
}

/// The fog factor of an untransformed vertex at `v` (1: no fog, 0: all fog), when FOGENABLE and a
/// vertex fog mode (or, as a stand-in, the table fog mode: the device does not offer table fog)
/// are on: LINEAR (end - d) / (end - start), EXP e^-(density d), EXP2 e^-(density d)^2, of the
/// eye space depth d (the distance with RANGEFOGENABLE). None: the specular alpha stays the fog
/// factor, as Windows (Wine's ddraw visual tests fog_test).
fn vertex_fog(d: &Dev, wv: &[f32; 16], v: [f32; 4]) -> Option<f32> {
    if d.rs[RS_FOGENABLE] == 0 {
        return None;
    }
    let mode = if d.rs[RS_FOGTABLEMODE] != 0 { d.rs[RS_FOGTABLEMODE] } else { d.rs[RS_FOGVERTEXMODE] };
    let e: [f32; 3] = std::array::from_fn(|c| (0..4).map(|r| v[r] * wv[r * 4 + c]).sum());
    let dist = if d.rs[RS_RANGEFOGENABLE] != 0 { (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt() } else { e[2] };
    let (start, end, dens) = (f32::from_bits(d.rs[RS_FOGSTART]), f32::from_bits(d.rs[RS_FOGEND]), f32::from_bits(d.rs[RS_FOGDENSITY]));
    let f = match mode {
        1 => (-dens * dist).exp(),
        2 => (-(dens * dist) * (dens * dist)).exp(),
        3 if end != start => (end - dist) / (end - start),
        3 => if dist < end { 1. } else { 0. },
        _ => return None,
    };
    Some(f.clamp(0., 1.))
}

/// A vertex before rasterizing: clip-space position (or screen position and 1/rhw for
/// transformed vertices) and its colors and texture coordinates.
#[derive(Clone, Copy)]
struct Vtx {
    clip: [f32; 4],
    screen: bool,
    diff: C4,
    spec: C4,
    tex: [[f32; 2]; 8],
}

fn lerp_v(a: &Vtx, b: &Vtx, t: f32) -> Vtx {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Vtx {
        clip: std::array::from_fn(|i| l(a.clip[i], b.clip[i])),
        screen: a.screen,
        diff: std::array::from_fn(|i| l(a.diff[i], b.diff[i])),
        spec: std::array::from_fn(|i| l(a.spec[i], b.spec[i])),
        tex: std::array::from_fn(|k| [l(a.tex[k][0], b.tex[k][0]), l(a.tex[k][1], b.tex[k][1])]),
    }
}

struct Xform {
    wvp: [f32; 16],
    /// world * view (the eye space vertex fog is reckoned in)
    wv: [f32; 16],
    world: [f32; 16],
    vp: [f32; 4],
    zr: (f32, f32),
}

impl Xform {
    fn new(d: &Dev) -> Xform {
        let world = d.xf[0];
        let wv = mul(&world, &d.xf[1]);
        Xform {
            wvp: mul(&mul(&wv, &d.xf[2]), &d.clip),
            wv,
            world,
            vp: [d.vp[0] as f32, d.vp[1] as f32, d.vp[2] as f32, d.vp[3] as f32],
            zr: d.zr,
        }
    }
    /// x, y, z, rhw on the screen
    fn screen(&self, c: &[f32; 4]) -> [f32; 4] {
        let rhw = 1. / c[3];
        [
            self.vp[0] + (1. + c[0] * rhw) * self.vp[2] * 0.5,
            self.vp[1] + (1. - c[1] * rhw) * self.vp[3] * 0.5,
            self.zr.0 + c[2] * rhw * (self.zr.1 - self.zr.0),
            rhw,
        ]
    }

    /// Fixed-function lighting in world space: (diffuse, specular).
    fn light(&self, d: &Dev, p: [f32; 3], n: Option<[f32; 3]>, vd: Option<C4>, vs: Option<C4>) -> (C4, C4) {
        let m = &d.mat;
        let mc = |i: usize| -> C4 { [m[i * 4], m[i * 4 + 1], m[i * 4 + 2], m[i * 4 + 3]] };
        let cv = d.rs[RS_COLORVERTEX] != 0;
        let src = |rs: usize, mat: usize| -> C4 {
            match (cv, d.rs[rs]) {
                (true, 1) if vd.is_some() => vd.unwrap(),
                (true, 2) if vs.is_some() => vs.unwrap(),
                _ => mc(mat),
            }
        };
        let (diff_m, amb_m, spec_m, emis_m) =
            (src(RS_DIFFUSEMATERIALSOURCE, 0), src(RS_AMBIENTMATERIALSOURCE, 1), src(RS_SPECULARMATERIALSOURCE, 2), src(RS_EMISSIVEMATERIALSOURCE, 3));
        let amb = unpack_argb(d.rs[RS_AMBIENT]);
        let mut out: C4 = std::array::from_fn(|i| emis_m[i] + amb[i] * amb_m[i]);
        let w = &self.world;
        let wp: [f32; 3] = std::array::from_fn(|c| p[0] * w[c] + p[1] * w[4 + c] + p[2] * w[8 + c] + w[12 + c]);
        let wn = n.map(|n| {
            let v: [f32; 3] = std::array::from_fn(|c| n[0] * w[c] + n[1] * w[4 + c] + n[2] * w[8 + c]);
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
            v.map(|x| x / l)
        });
        for (l, on) in d.lights.values() {
            if !on {
                continue;
            }
            let f = |i: usize| f32::from_bits(l[i]);
            let (ldiff, lamb) = ([f(1), f(2), f(3), f(4)], [f(9), f(10), f(11), f(12)]);
            let (dir, att) = if l[0] == 3 {
                let v = [-f(16), -f(17), -f(18)];
                (v, 1.)
            } else {
                let v = [f(13) - wp[0], f(14) - wp[1], f(15) - wp[2]];
                let dist = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                if dist > f(19) && f(19) > 0. {
                    continue;
                }
                let a = f(21) + f(22) * dist + f(23) * dist * dist;
                (v, if a > 0. { 1. / a } else { 1. })
            };
            let dl = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt().max(1e-12);
            let ndl = wn.map_or(0., |n| ((n[0] * dir[0] + n[1] * dir[1] + n[2] * dir[2]) / dl).max(0.));
            for i in 0..3 {
                out[i] += att * (lamb[i] * amb_m[i] + ndl * ldiff[i] * diff_m[i]);
            }
        }
        out[3] = diff_m[3];
        let _ = spec_m;
        (out.map(|x| x.clamp(0., 1.)), [0.; 4])
    }
}

// ---------------------------------------------------------------------------------------------
// the rasterizer

#[derive(Clone)]
struct Tex {
    bits: *const u8,
    w: i32,
    h: i32,
    pitch: usize,
    bpp: usize,
    fmt: Fmt,
    pal: Option<Box<[u32; 256]>>,
    key: Option<(u32, u32)>,
}

impl Tex {
    #[inline]
    fn raw(&self, x: i32, y: i32) -> u32 {
        read_px(unsafe { self.bits.add(y as usize * self.pitch + x as usize * self.bpp) }, self.bpp as u32)
    }
    #[inline]
    fn texel(&self, x: i32, y: i32, ck: bool) -> C4 {
        let v = self.raw(x, y);
        if ck {
            if let Some((lo, hi)) = self.key {
                if v >= lo && v <= hi {
                    return [0.; 4];
                }
            }
        }
        match &self.pal {
            Some(p) => {
                let e = p[v as usize & 0xff];
                [(e >> 16 & 0xff) as f32 / 255., (e >> 8 & 0xff) as f32 / 255., (e & 0xff) as f32 / 255., 1.]
            }
            None => self.fmt.unpack(v),
        }
    }
}

#[derive(Clone)]
struct Stage {
    cop: u32,
    ca: [u32; 2],
    aop: u32,
    aa: [u32; 2],
    tex: Option<Tex>,
    tci: usize,
    addr: [u32; 2],
    border: C4,
    linear: bool,
}

/// `Raster::copy_plan`: pixel (x, y) is texel (x + dx, y + dy); `same`: one pixel format, so the
/// texel's bits (but those of no channel) are the pixel's.
struct TexCopy<'a> {
    tex: &'a Tex,
    dx: i32,
    dy: i32,
    same: bool,
    mask: u32,
}

struct Raster {
    rt: *mut u8,
    pitch: usize,
    bpp: usize,
    fmt: Fmt,
    z: Option<(*mut u8, usize, usize, f32)>,
    clip: [i32; 4],
    ztest: Option<u32>,
    zwrite: bool,
    blend: Option<(u32, u32)>,
    atest: Option<(u32, f32)>,
    ck: bool,
    flat: bool,
    spec: bool,
    /// FOGENABLE: the fog colour, blended in by the specular alpha (the fog factor)
    fog: Option<[f32; 3]>,
    cull: u32,
    stages: Vec<Stage>,
    tfactor: C4,
    ntc: usize,
    /// copy_plan's shortcut (off: every fragment through the pipeline, which tests compare it with)
    copy: bool,
}

fn cmp(f: u32, a: f32, b: f32) -> bool {
    match f {
        1 => false,
        2 => a < b,
        3 => a == b,
        4 => a <= b,
        5 => a > b,
        6 => a != b,
        7 => a >= b,
        _ => true,
    }
}

fn wrap(i: i32, n: i32, mode: u32) -> Option<i32> {
    match mode {
        2 => {
            let p = i.rem_euclid(2 * n);
            Some(if p >= n { 2 * n - 1 - p } else { p })
        }
        3 => Some(i.clamp(0, n - 1)),
        4 => (0..n).contains(&i).then_some(i),
        _ => Some(i.rem_euclid(n)),
    }
}

impl Stage {
    fn sample(&self, uv: [f32; 2], ck: bool) -> C4 {
        let Some(t) = &self.tex else { return [1.; 4] };
        let get = |x: i32, y: i32| match (wrap(x, t.w, self.addr[0]), wrap(y, t.h, self.addr[1])) {
            (Some(x), Some(y)) => t.texel(x, y, ck),
            _ => self.border,
        };
        let (fx, fy) = (uv[0] * t.w as f32, uv[1] * t.h as f32);
        if !self.linear {
            return get(fx.floor() as i32, fy.floor() as i32);
        }
        let (gx, gy) = (fx - 0.5, fy - 0.5);
        let (x0, y0) = (gx.floor(), gy.floor());
        let (ax, ay) = (gx - x0, gy - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let (a, b, c, d) = (get(x0, y0), get(x0 + 1, y0), get(x0, y0 + 1), get(x0 + 1, y0 + 1));
        std::array::from_fn(|i| (a[i] * (1. - ax) + b[i] * ax) * (1. - ay) + (c[i] * (1. - ax) + d[i] * ax) * ay)
    }
}

fn arg(a: u32, diff: &C4, cur: &C4, tex: &C4, tf: &C4, spec: &C4) -> C4 {
    let mut v = match a & 0xf {
        0 => *diff,
        2 => *tex,
        3 => *tf,
        4 => *spec,
        _ => *cur,
    };
    if a & 0x10 != 0 {
        v = v.map(|x| 1. - x);
    }
    if a & 0x20 != 0 {
        v = [v[3]; 4];
    }
    v
}

/// One texture operation on one channel set (`n` channels from `o`).
fn op(o: u32, a1: &C4, a2: &C4, diff: &C4, tex: &C4, tf: &C4, cur: &C4, out: &mut C4, from: usize, to: usize) {
    for i in from..to {
        let (x, y) = (a1[i], a2[i]);
        out[i] = match o {
            2 => x,
            3 => y,
            4 => x * y,
            5 => x * y * 2.,
            6 => x * y * 4.,
            7 => x + y,
            8 => x + y - 0.5,
            9 => (x + y - 0.5) * 2.,
            10 => x - y,
            11 => x + y - x * y,
            12 => x * diff[3] + y * (1. - diff[3]),
            13 => x * tex[3] + y * (1. - tex[3]),
            14 => x * tf[3] + y * (1. - tf[3]),
            15 => x + y * (1. - tex[3]),
            16 => x * cur[3] + y * (1. - cur[3]),
            18 => x + a1[3] * y,
            19 => x * y + a1[3],
            20 => x + (1. - a1[3]) * y,
            21 => (1. - x) * y + a1[3],
            24 => {
                let d = (0..3).map(|k| (a1[k] - 0.5) * (a2[k] - 0.5)).sum::<f32>() * 4.;
                d
            }
            _ => x * y,
        }
        .clamp(0., 1.);
    }
}

impl Raster {
    fn new(d: &Dev) -> Option<Raster> {
        let rt = surf(d.rt)?;
        if rt.bpp < 16 || rt.caps & CAPS_ZBUFFER != 0 {
            return None;
        }
        let pal_of = |s: &Surf| with(|st| st.palettes.get(&s.palette).map(|p| Box::new(*p)));
        let z = (d.rs[RS_ZENABLE] != 0).then(|| surf(rt.zbuf)).flatten().map(|z| {
            let bytes = (z.bpp as usize / 8).max(2);
            let mask = if z.pf[2] != 0 { z.pf[2] } else if bytes == 2 { 0xffff } else { 0xff_ffff };
            (mem::slice_mut(z.bits, z.pitch * z.h as u32).as_mut_ptr(), z.pitch as usize, bytes, mask as f32)
        });
        let vp = [d.vp[0].max(0), d.vp[1].max(0), (d.vp[0] + d.vp[2]).min(rt.w), (d.vp[1] + d.vp[3]).min(rt.h)];
        let mut stages = Vec::new();
        let mut ntc = 0;
        for i in 0..8 {
            let t = &d.tss[i];
            if t[TSS_COLOROP] <= 1 {
                break;
            }
            let tex = surf(d.tex[i]).filter(|s| s.bpp >= 8 && s.w > 0 && s.h > 0).map(|s| Tex {
                bits: mem::slice(s.bits, s.pitch * s.h as u32).as_ptr(),
                w: s.w,
                h: s.h,
                pitch: s.pitch as usize,
                bpp: s.bpp as usize / 8,
                fmt: Fmt::of(&s),
                pal: if s.bpp == 8 { Some(pal_of(&s).unwrap_or_else(|| Box::new([0; 256]))) } else { None },
                key: s.ck_src,
            });
            let tci = (t[TSS_TEXCOORDINDEX] & 7) as usize;
            ntc = ntc.max(tci + 1);
            stages.push(Stage {
                cop: t[TSS_COLOROP],
                ca: [t[TSS_COLORARG1], t[TSS_COLORARG2]],
                aop: t[TSS_ALPHAOP],
                aa: [t[TSS_ALPHAARG1], t[TSS_ALPHAARG2]],
                tex,
                tci,
                addr: [t[TSS_ADDRESSU], t[TSS_ADDRESSV]],
                border: unpack_argb(t[TSS_BORDERCOLOR]),
                linear: t[TSS_MAGFILTER] >= 2,
            });
        }
        Some(Raster {
            rt: mem::slice_mut(rt.bits, rt.pitch * rt.h as u32).as_mut_ptr(),
            pitch: rt.pitch as usize,
            bpp: rt.bpp as usize / 8,
            fmt: Fmt::of(&rt),
            ztest: z.map(|_| d.rs[RS_ZFUNC]),
            zwrite: z.is_some() && d.rs[RS_ZWRITEENABLE] != 0,
            z,
            clip: vp,
            blend: (d.rs[RS_ALPHABLENDENABLE] != 0).then(|| match d.rs[RS_SRCBLEND] {
                12 => (5, 6),
                13 => (6, 5),
                s => (s, d.rs[RS_DESTBLEND]),
            }),
            atest: (d.rs[RS_ALPHATESTENABLE] != 0).then(|| (d.rs[RS_ALPHAFUNC], (d.rs[RS_ALPHAREF] & 0xff) as f32 / 255.)),
            ck: d.rs[RS_COLORKEYENABLE] != 0,
            flat: d.rs[RS_SHADEMODE] == 1,
            spec: d.rs[RS_SPECULARENABLE] != 0,
            fog: (d.rs[RS_FOGENABLE] != 0).then(|| {
                let c = unpack_argb(d.rs[RS_FOGCOLOR]);
                [c[0], c[1], c[2]]
            }),
            cull: d.rs[RS_CULLMODE],
            stages,
            tfactor: unpack_argb(d.rs[RS_TEXTUREFACTOR]),
            ntc,
            copy: true,
        })
    }

    /// The color of a fragment (None: discarded by the color key or the alpha test).
    #[inline]
    fn shade(&self, diff: &C4, spec: &C4, tc: &[[f32; 2]; 8]) -> Option<C4> {
        let mut cur = *diff;
        for s in &self.stages {
            let uses_tex = |a: &[u32; 2], o: u32| (a[0] & 0xf == 2 && o != 3) || (a[1] & 0xf == 2 && o != 2) || matches!(o, 13 | 15);
            let tex = if s.tex.is_some() {
                let t = s.sample(tc[s.tci], self.ck);
                if self.ck && s.tex.as_ref().is_some_and(|t| t.key.is_some()) && t[3] < 0.5 && t[0] + t[1] + t[2] == 0. && !s.linear {
                    return None;
                }
                t
            } else {
                [1.; 4]
            };
            let mut out = cur;
            let (ctex, atex) = (uses_tex(&s.ca, s.cop), uses_tex(&s.aa, s.aop));
            if s.tex.is_some() || !ctex {
                let (a1, a2) = (arg(s.ca[0], diff, &cur, &tex, &self.tfactor, spec), arg(s.ca[1], diff, &cur, &tex, &self.tfactor, spec));
                op(s.cop, &a1, &a2, diff, &tex, &self.tfactor, &cur, &mut out, 0, 3);
            }
            if s.aop > 1 && (s.tex.is_some() || !atex) {
                let (a1, a2) = (arg(s.aa[0], diff, &cur, &tex, &self.tfactor, spec), arg(s.aa[1], diff, &cur, &tex, &self.tfactor, spec));
                op(s.aop, &a1, &a2, diff, &tex, &self.tfactor, &cur, &mut out, 3, 4);
            }
            if self.ck && s.linear && s.tex.as_ref().is_some_and(|t| t.key.is_some()) && tex[3] < 0.5 && s.tex.as_ref().unwrap().fmt.ch[3].1 == 0 {
                return None;
            }
            cur = out;
        }
        if self.spec {
            for i in 0..3 {
                cur[i] = (cur[i] + spec[i]).min(1.);
            }
        }
        if let Some(fc) = self.fog {
            let f = spec[3].clamp(0., 1.);
            for i in 0..3 {
                cur[i] = f * cur[i] + (1. - f) * fc[i];
            }
        }
        if let Some((f, r)) = self.atest {
            if !cmp(f, cur[3], r) {
                return None;
            }
        }
        Some(cur)
    }

    #[inline]
    fn put(&self, x: i32, y: i32, z: f32, diff: &C4, spec: &C4, tc: &[[f32; 2]; 8]) {
        let zp = self.z.map(|(p, pitch, bytes, max)| (unsafe { p.add(y as usize * pitch + x as usize * bytes) }, bytes, max));
        let zv = zp.map(|(_, _, max)| (z.clamp(0., 1.) * max) as u32);
        if let (Some(f), Some((p, bytes, _)), Some(zv)) = (self.ztest, zp, zv) {
            let old = if bytes == 2 { read_px(p, 2) } else { read_px(p, 4) & if bytes == 3 { 0xff_ffff } else { !0 } };
            if !cmp(f, zv as f32, old as f32) {
                return;
            }
        }
        let Some(c) = self.shade(diff, spec, tc) else { return };
        let p = unsafe { self.rt.add(y as usize * self.pitch + x as usize * self.bpp) };
        let out = match self.blend {
            None => c,
            Some((sb, db)) => {
                let d = self.fmt.unpack(read_px(p, self.bpp as u32));
                let f = |b: u32, src: bool| -> C4 {
                    match b {
                        1 => [0.; 4],
                        3 => c,
                        4 => c.map(|x| 1. - x),
                        5 => [c[3]; 4],
                        6 => [1. - c[3]; 4],
                        7 => [d[3]; 4],
                        8 => [1. - d[3]; 4],
                        9 => d,
                        10 => d.map(|x| 1. - x),
                        11 => {
                            let s = c[3].min(1. - d[3]);
                            [s, s, s, 1.]
                        }
                        _ => {
                            let _ = src;
                            [1.; 4]
                        }
                    }
                };
                let (fs, fd) = (f(sb, true), f(db, false));
                std::array::from_fn(|i| (c[i] * fs[i] + d[i] * fd[i]).clamp(0., 1.))
            }
        };
        write_px(p, self.bpp as u32, self.fmt.pack(out));
        if self.zwrite {
            if let (Some((p, bytes, _)), Some(zv)) = (zp, zv) {
                write_px(p, bytes.min(4) as u32, zv);
            }
        }
    }

    /// A triangle whose fragments are its texture's texels unchanged (`tri`'s copy): one stage
    /// whose color and alpha are the texture's (SELECTARG of TEXTURE, or MODULATE by a diffuse
    /// or texture factor of 1), no blending, alpha test, color key, fog, specular or depth, one
    /// rhw at all three vertices and texture coordinates that put every pixel center (x, y) on
    /// texel center (x + dx, y + dy) inside the texture: what the rasterizer computes there is
    /// that texel (bilinear filtering's weights are 1 and 0), so it is copied without the
    /// fragment pipeline. KiriKiri 2 presents every frame so: a full-screen quad of a
    /// screen-sized texture, at x - 0.5 and u in 0..1.
    fn copy_plan(&self, v: [(&[f32; 4], &Vtx); 3], bbox: [i32; 4]) -> Option<TexCopy<'_>> {
        let [s] = self.stages.as_slice() else { return None };
        let tex = s.tex.as_ref()?;
        if !self.copy {
            return None;
        }
        if self.blend.is_some() || self.atest.is_some() || self.fog.is_some() || self.spec || self.z.is_some() || tex.pal.is_some() || (self.ck && tex.key.is_some()) {
            return None;
        }
        if v.iter().any(|(p, _)| p[3] != v[0].0[3]) {
            return None;
        }
        // TEXTURE (2) as given, DIFFUSE (0; CURRENT (1) is the diffuse in the first stage) or
        // TFACTOR (3) only where that factor is 1
        let one = |a: u32, ch: std::ops::Range<usize>| match a {
            0 | 1 => v.iter().all(|(_, x)| x.diff[ch.clone()].iter().all(|&c| c == 1.)),
            3 => self.tfactor[ch].iter().all(|&c| c == 1.),
            _ => false,
        };
        let is_tex = |o: u32, a: &[u32; 2], ch: std::ops::Range<usize>| match o {
            2 => a[0] == 2,
            3 => a[1] == 2,
            4 => (a[0] == 2 && one(a[1], ch.clone())) || (a[1] == 2 && one(a[0], ch)),
            _ => false,
        };
        if !is_tex(s.cop, &s.ca, 0..3) {
            return None;
        }
        // (with ALPHAOP disabled the alpha is the diffuse one: fine where the target has none)
        if !is_tex(s.aop, &s.aa, 3..4) && !(s.aop <= 1 && self.fmt.ch[3].1 == 0) {
            return None;
        }
        let off = |k: usize, n: i32| -> Option<i32> {
            let r = (v[0].1.tex[s.tci][k] * n as f32 - 0.5 - v[0].0[k]).round();
            v.iter().all(|(p, x)| (x.tex[s.tci][k] * n as f32 - 0.5 - p[k] - r).abs() <= 1. / 4096.).then_some(r as i32)
        };
        let (dx, dy) = (off(0, tex.w)?, off(1, tex.h)?);
        if bbox[0] + dx < 0 || bbox[2] + dx >= tex.w || bbox[1] + dy < 0 || bbox[3] + dy >= tex.h {
            return None;
        }
        let mask = (0..4).fold(0u32, |m, i| m | self.fmt.ch[i].1 << self.fmt.ch[i].0);
        Some(TexCopy { tex, dx, dy, same: tex.bpp == self.bpp && tex.fmt.ch == self.fmt.ch, mask })
    }

    /// One triangle of screen-space vertices ([x, y, z, rhw]).
    fn tri(&self, v: [(&[f32; 4], &Vtx); 3]) {
        let [(a, va), (b, vb), (c, vc)] = v;
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if area == 0. || !area.is_finite() {
            return;
        }
        // y down: area > 0 is clockwise on the screen
        if (self.cull == 2 && area > 0.) || (self.cull == 3 && area < 0.) {
            return;
        }
        let (b, vb, c, vc, area) = if area < 0. { (c, vc, b, vb, -area) } else { (b, vb, c, vc, area) };
        let minx = a[0].min(b[0]).min(c[0]).ceil().max(self.clip[0] as f32) as i32;
        let maxx = a[0].max(b[0]).max(c[0]).floor().min(self.clip[2] as f32 - 1.) as i32;
        let miny = a[1].min(b[1]).min(c[1]).ceil().max(self.clip[1] as f32) as i32;
        let maxy = a[1].max(b[1]).max(c[1]).floor().min(self.clip[3] as f32 - 1.) as i32;
        if minx > maxx || miny > maxy {
            return;
        }
        // edge functions: e0 opposite a (b->c), e1 opposite b (c->a), e2 opposite c (a->b)
        let edge = |p: &[f32; 4], q: &[f32; 4]| {
            let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
            let tl = (dy == 0. && dx > 0.) || dy < 0.;
            (dx, dy, p[0], p[1], tl)
        };
        let es = [edge(b, c), edge(c, a), edge(a, b)];
        let w = [a[3], b[3], c[3]];
        let nt = self.ntc;
        let vs = [va, vb, vc];
        if let Some(cp) = self.copy_plan([(a, va), (b, vb), (c, vc)], [minx, miny, maxx, maxy]) {
            for y in miny..=maxy {
                let py = y as f32;
                let row = unsafe { self.rt.add(y as usize * self.pitch) };
                for x in minx..=maxx {
                    let px = x as f32;
                    // (the test of the loop below, NaN included)
                    if es.iter().all(|&(dx, dy, ox, oy, tl)| {
                        let e = dx * (py - oy) - dy * (px - ox);
                        !(e < 0. || (e == 0. && !tl))
                    }) {
                        let v = cp.tex.raw(x + cp.dx, y + cp.dy);
                        let v = if cp.same { v & cp.mask } else { self.fmt.pack(cp.tex.fmt.unpack(v)) };
                        write_px(unsafe { row.add(x as usize * self.bpp) }, self.bpp as u32, v);
                    }
                }
            }
            return;
        }
        for y in miny..=maxy {
            let py = y as f32;
            for x in minx..=maxx {
                let px = x as f32;
                let mut l = [0f32; 3];
                let mut inside = true;
                for (k, (dx, dy, ox, oy, tl)) in es.iter().enumerate() {
                    let e = dx * (py - oy) - dy * (px - ox);
                    if e < 0. || (e == 0. && !tl) {
                        inside = false;
                        break;
                    }
                    l[k] = e / area;
                }
                if !inside {
                    continue;
                }
                let z = l[0] * a[2] + l[1] * b[2] + l[2] * c[2];
                let pw = [l[0] * w[0], l[1] * w[1], l[2] * w[2]];
                let s = pw[0] + pw[1] + pw[2];
                let pw = if s != 0. && s.is_finite() { pw.map(|x| x / s) } else { l };
                let mix4 = |f: &dyn Fn(&Vtx) -> C4| -> C4 {
                    let (p, q, r) = (f(vs[0]), f(vs[1]), f(vs[2]));
                    std::array::from_fn(|i| pw[0] * p[i] + pw[1] * q[i] + pw[2] * r[i])
                };
                let (diff, spec) = if self.flat { (va.diff, va.spec) } else { (mix4(&|v| v.diff), mix4(&|v| v.spec)) };
                let mut tc = [[0f32; 2]; 8];
                for k in 0..nt {
                    for j in 0..2 {
                        tc[k][j] = pw[0] * vs[0].tex[k][j] + pw[1] * vs[1].tex[k][j] + pw[2] * vs[2].tex[k][j];
                    }
                }
                self.put(x, y, z, &diff, &spec, &tc);
            }
        }
    }

    fn point(&self, p: &[f32; 4], v: &Vtx) {
        let (x, y) = (p[0].round() as i32, p[1].round() as i32);
        if x >= self.clip[0] && x < self.clip[2] && y >= self.clip[1] && y < self.clip[3] {
            self.put(x, y, p[2], &v.diff, &v.spec, &v.tex);
        }
    }

    fn line(&self, a: (&[f32; 4], &Vtx), b: (&[f32; 4], &Vtx)) {
        let n = (b.0[0] - a.0[0]).abs().max((b.0[1] - a.0[1]).abs()).ceil().min(8192.) as i32;
        for i in 0..n.max(1) {
            let t = i as f32 / n.max(1) as f32;
            let p: [f32; 4] = std::array::from_fn(|k| a.0[k] + (b.0[k] - a.0[k]) * t);
            self.point(&p, &if self.flat { *a.1 } else { lerp_v(a.1, b.1, t) });
        }
    }
}

/// Clips a polygon of clip-space vertices to z >= 0 and w > 0.
fn clip_near(poly: Vec<Vtx>) -> Vec<Vtx> {
    let mut p = poly;
    // (near z >= 0, w > 0, and far z <= w: Windows draws nothing past the far plane, clipping
    // on or off, Wine's ddraw visual tests depth_clamp_test)
    for plane in [|v: &Vtx| v.clip[2], |v: &Vtx| v.clip[3] - 1e-6, |v: &Vtx| v.clip[3] - v.clip[2]] {
        if p.iter().all(|v| plane(v) >= 0.) {
            continue;
        }
        let mut out = Vec::with_capacity(p.len() + 2);
        for i in 0..p.len() {
            let (a, b) = (&p[i], &p[(i + 1) % p.len()]);
            let (da, db) = (plane(a), plane(b));
            if da >= 0. {
                out.push(*a);
            }
            if (da >= 0.) != (db >= 0.) {
                out.push(lerp_v(a, b, da / (da - db)));
            }
        }
        p = out;
        if p.len() < 3 {
            return p;
        }
    }
    p
}

/// Draws `prim` (1 points, 2 lines, 3 strip, 4 triangles, 5 strip, 6 fan) of vertices
/// `idx` (or start..start+count) from `src`.
fn draw(c: &mut Ctx, prim: u32, src: Src, idx: Option<(u32, u32)>, start: u32, count: u32) -> u32 {
    let Some(d) = dev(c, |d| d.clone()) else { return D3DERR_INVALIDCALL };
    let Some(r) = Raster::new(&d) else { return D3D_OK };
    let t = Xform::new(&d);
    let n = idx.map_or(count, |i| i.1);
    let n = n.min(1 << 20);
    let mut cache: HashMap<u32, Vtx> = HashMap::default();
    let mut get = |k: u32| -> Vtx {
        let i = match idx {
            Some((p, _)) => mem::r16(p + 2 * k) as u32 + start,
            None => start + k,
        };
        *cache.entry(i).or_insert_with(|| src.fetch(i, &d, &t))
    };
    let scr = |v: &Vtx| if v.screen { [v.clip[0], v.clip[1], v.clip[2], 1. / v.clip[3]] } else { t.screen(&v.clip) };
    let tri = |a: Vtx, b: Vtx, cc: Vtx| {
        if a.screen {
            let (sa, sb, sc) = (scr(&a), scr(&b), scr(&cc));
            r.tri([(&sa, &a), (&sb, &b), (&sc, &cc)]);
            return;
        }
        let p = clip_near(vec![a, b, cc]);
        if p.len() < 3 {
            return;
        }
        let s: Vec<[f32; 4]> = p.iter().map(|v| scr(v)).collect();
        for i in 1..p.len() - 1 {
            r.tri([(&s[0], &p[0]), (&s[i], &p[i]), (&s[i + 1], &p[i + 1])]);
        }
    };
    match prim {
        4 => (0..n / 3).for_each(|i| tri(get(3 * i), get(3 * i + 1), get(3 * i + 2))),
        5 => (0..n.saturating_sub(2)).for_each(|i| {
            if i % 2 == 0 { tri(get(i), get(i + 1), get(i + 2)) } else { tri(get(i + 1), get(i), get(i + 2)) }
        }),
        6 => {
            let f = get(0);
            (1..n.saturating_sub(1)).for_each(|i| tri(f, get(i), get(i + 1)))
        }
        1 => (0..n).for_each(|i| {
            let v = get(i);
            if v.screen || v.clip[3] > 0. {
                r.point(&scr(&v), &v)
            }
        }),
        2 | 3 => {
            let pairs: Vec<(u32, u32)> = if prim == 2 { (0..n / 2).map(|i| (2 * i, 2 * i + 1)).collect() } else { (0..n.saturating_sub(1)).map(|i| (i, i + 1)).collect() };
            for (i, j) in pairs {
                let (a, b) = (get(i), get(j));
                if a.screen || (a.clip[3] > 0. && b.clip[3] > 0.) {
                    let (sa, sb) = (scr(&a), scr(&b));
                    r.line((&sa, &a), (&sb, &b));
                }
            }
        }
        _ => return D3DERR_INVALIDCALL,
    }
    D3D_OK
}

fn V_DrawPrimitive(c: &mut Ctx) -> u32 {
    let (prim, fvf, verts, n) = (c.a(1), c.a(2), c.a(3), c.a(4));
    draw(c, prim, Src::packed(fvf, verts), None, 0, n)
}
fn V_DrawIndexedPrimitive(c: &mut Ctx) -> u32 {
    let (prim, fvf, verts, idx, ni) = (c.a(1), c.a(2), c.a(3), c.a(5), c.a(6));
    draw(c, prim, Src::packed(fvf, verts), Some((idx, ni)), 0, 0)
}
fn V_DrawPrimitiveStrided(c: &mut Ctx) -> u32 {
    let (prim, fvf, data, n) = (c.a(1), c.a(2), c.a(3), c.a(4));
    draw(c, prim, Src::strided(fvf, data), None, 0, n)
}
fn V_DrawIndexedPrimitiveStrided(c: &mut Ctx) -> u32 {
    let (prim, fvf, data, idx, ni) = (c.a(1), c.a(2), c.a(3), c.a(5), c.a(6));
    draw(c, prim, Src::strided(fvf, data), Some((idx, ni)), 0, 0)
}
fn V_DrawPrimitiveVB(c: &mut Ctx) -> u32 {
    let (prim, vbb, start, n) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((m, fvf, _)) = d3d(|d| d.vbs.get(&vbb).copied()) else { return D3DERR_INVALIDCALL };
    draw(c, prim, Src::packed(fvf, m), None, start, n)
}
fn V_DrawIndexedPrimitiveVB(c: &mut Ctx) -> u32 {
    let (prim, vbb, start, idx, ni) = (c.a(1), c.a(2), c.a(3), c.a(5), c.a(6));
    let Some((m, fvf, _)) = d3d(|d| d.vbs.get(&vbb).copied()) else { return D3DERR_INVALIDCALL };
    draw(c, prim, Src::packed(fvf, m), Some((idx, ni)), start, 0)
}

/// Clear(count, rects, flags, color, z, stencil)
fn V_Clear(c: &mut Ctx) -> u32 {
    let (n, rects, flags, color, z) = (c.a(1), c.a(2), c.a(3), c.a(4), f32::from_bits(c.a(5)));
    let Some(vp) = dev(c, |d| d.vp) else { return D3DERR_INVALIDCALL };
    clear_dev(r32(c.a(0) + 4), vp, n, rects, flags, color, z)
}

/// Clears device `id`'s render target (flags 1) and depth buffer (2) in `rects` (none: all of
/// it), each clipped to the viewport `vp` (x, y, width, height).
fn clear_dev(id: u32, vp: [i32; 4], n: u32, rects: u32, flags: u32, color: u32, z: f32) -> u32 {
    let Some(rt) = d3d(|d| d.devs.get(&id).map(|d| d.rt)) else { return D3DERR_INVALIDCALL };
    let Some(rt) = surf(rt) else { return D3D_OK };
    let vp = Rect { l: vp[0], t: vp[1], r: vp[0] + vp[2], b: vp[1] + vp[3] };
    let mut rs = Vec::new();
    if n == 0 || rects == 0 {
        rs.push(vp);
    } else {
        for i in 0..n.min(4096) {
            let p = rects + 16 * i;
            let r = Rect { l: r32(p) as i32, t: r32(p + 4) as i32, r: r32(p + 8) as i32, b: r32(p + 12) as i32 };
            rs.push(Rect { l: r.l.max(vp.l), t: r.t.max(vp.t), r: r.r.min(vp.r), b: r.b.min(vp.b) });
        }
    }
    let zs = surf(rt.zbuf);
    for r in rs {
        if flags & 1 != 0 && rt.bpp >= 16 {
            let v = Fmt::of(&rt).pack(unpack_argb(color));
            fill(&rt, clip_rect(r, rt.w, rt.h), v);
        }
        if flags & 2 != 0 {
            if let Some(zs) = &zs {
                let max = if zs.pf[2] != 0 { zs.pf[2] } else if zs.bpp == 16 { 0xffff } else { 0xff_ffff };
                fill(zs, clip_rect(r, zs.w, zs.h), (z.clamp(0., 1.) * max as f32) as u32);
            }
        }
    }
    D3D_OK
}

// ---------------------------------------------------------------------------------------------
// Direct3D 6: IDirect3D3, IDirect3DDevice3, IDirect3DViewport3, IDirect3DMaterial3,
// IDirect3DLight, IDirect3DTexture2. A Device3 is a device of the same kind as a Device7 (its
// block [vtable, id, refs]), with the methods whose arguments are the same shared; viewports
// and materials are objects of their own, the current viewport the device's viewport.

const CLASS_E_NOAGGREGATION: u32 = 0x8004_0110;
const DDERR_ALREADYINITIALIZED: u32 = 0x8876_0005;
const D3DERR_VIEWPORTDATANOTSET: u32 = 0x8876_0305;
const D3DERR_VIEWPORTHASNODEVICE: u32 = 0x8876_0306;
const D3DERR_NOCURRENTVIEWPORT: u32 = 0x8876_0307;

/// An IDirect3DViewport3: D3DVIEWPORT2's words (dwSize, X, Y, width, height, clip x/y/w/h,
/// min/max z) once set, the background material handle, the background depth surface, the
/// device it was added to (its id), its lights (blocks)
#[derive(Clone, Default)]
struct Vp3 {
    data: Option<[u32; 11]>,
    bg: u32,
    depth: u32,
    dev: u32,
    lights: Vec<u32>,
    /// the device light indices its lights were last given (1..=lit)
    lit: u32,
    refs: u32,
}

pub(super) fn d3d3_vtbl() -> u32 {
    static B: OnceLock<u32> = OnceLock::new();
    *B.get_or_init(|| {
        vtable(
            "IDirect3D3",
            &[
                (Q_QueryInterface, 12),
                (D_AddRef, 4),
                (D_Release, 4),
                (T_EnumDevices, 12),
                (T_CreateLight, 12),
                (T_CreateMaterial, 12),
                (T_CreateViewport, 12),
                (T_FindDevice, 12),
                (T_CreateDevice, 20),
                (Q_CreateVertexBuffer, 20),
                (Q_EnumZBufferFormats, 16),
                (ok, 4),
            ],
        )
    })
}

/// D3DDEVICEDESC (Direct3D 6's, 252 bytes; the first `size` of them) of device `dev` from its
/// D3DDEVICEDESC7; `empty`: a software device's hardware description (dwSize only)
fn write_desc3(p: u32, size: u32, dev: usize, empty: bool) {
    mem::fill(p, size, 0);
    w32(p, size);
    if empty {
        return;
    }
    let d7 = mem::heap_alloc(236, true);
    write_caps(d7, dev);
    let mut b = [0u8; 252];
    let put = |b: &mut [u8; 252], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 0, size);
    put(&mut b, 4, 0x7ff); // D3DDD_COLORMODEL ... D3DDD_MAXVERTEXCOUNT
    put(&mut b, 8, 2); // D3DCOLOR_RGB
    put(&mut b, 12, r32(d7));
    put(&mut b, 16, 8);
    put(&mut b, 20, 1); // D3DTRANSFORMCAPS_CLIP
    put(&mut b, 24, 1);
    put(&mut b, 28, 16);
    put(&mut b, 32, 0x7); // D3DLIGHTCAPS_POINT | SPOT | DIRECTIONAL
    put(&mut b, 36, 1); // D3DLIGHTINGMODEL_RGB
    put(&mut b, 40, 8);
    for k in 0..28 {
        put(&mut b, 44 + 4 * k, r32(d7 + 4 + 4 * k as u32));
    }
    put(&mut b, 156, r32(d7 + 116));
    put(&mut b, 160, r32(d7 + 120));
    put(&mut b, 168, 0xffff);
    for k in 0..4 {
        put(&mut b, 172 + 4 * k, r32(d7 + 124 + 4 * k as u32));
    }
    for k in 0..3 {
        put(&mut b, 204 + 4 * k, r32(d7 + 140 + 4 * k as u32));
    }
    for k in 0..9 {
        put(&mut b, 216 + 4 * k, r32(d7 + 152 + 4 * k as u32));
    }
    b[248..252].copy_from_slice(&mem::slice(d7 + 184, 4));
    mem::heap_free(d7);
    mem::write(p, &b[..size.min(252) as usize]);
}

/// The sizes of D3DDEVICEDESC: Direct3D 1, 5, 6
fn desc3_size_ok(n: u32) -> bool {
    matches!(n, 172 | 204 | 252)
}

/// EnumDevices(callback, context): the RGB (software: an empty hardware description) and HAL
/// devices, callback(guid, description, name, hardware desc, software desc, context)
fn T_EnumDevices(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(1), c.a(2));
    if cb == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let names = mem::heap_alloc(256 * 2, true);
    let (g, hal, hel) = (mem::heap_alloc(16, true), mem::heap_alloc(252, true), mem::heap_alloc(252, true));
    for i in 0..2 {
        let (desc, name, d1, d2, d3, d4, _) = DEVICES[i];
        let q = names + 256 * i as u32;
        mem::write(q, desc.as_bytes());
        mem::w8(q + desc.len() as u32, 0);
        mem::write(q + 128, name.as_bytes());
        mem::w8(q + 128 + name.len() as u32, 0);
        guid(g, d1, d2, d3, d4);
        write_desc3(hal, 252, i, i == 0);
        write_desc3(hel, 252, i, false);
        if c.call(cb, &[g, q, q + 128, hal, hel, ctx]) != 1 {
            break;
        }
    }
    for p in [names, g, hal, hel] {
        mem::heap_free(p);
    }
    D3D_OK
}

/// FindDevice(D3DFINDDEVICESEARCH, D3DFINDDEVICERESULT): the device of the GUID asked for
/// (D3DFDS_GUID), else the HAL one; the result's guid and descriptions
fn T_FindDevice(c: &mut Ctx) -> u32 {
    let (s, r) = (c.a(1), c.a(2));
    if s == 0 || r == 0 || r32(r) < 4 + 16 + 2 * 172 {
        return DDERR_INVALIDPARAMS;
    }
    let mut i = 1;
    if r32(s + 4) & 0x10 != 0 {
        let want = r32(s + 52);
        match DEVICES[..2].iter().position(|d| d.2 == want) {
            Some(k) => i = k,
            None => return DDERR_NOTFOUND,
        }
    }
    let n = (r32(r) - 20) / 2;
    let (_, _, d1, d2, d3, d4, _) = DEVICES[i];
    guid(r + 4, d1, d2, d3, d4);
    write_desc3(r + 20, n, i, i == 0);
    write_desc3(r + 20 + n, n, i, false);
    D3D_OK
}

/// A block [vtable, id, refs] for a new viewport, material or light (outer: no aggregation)
fn new_obj(c: &Ctx, vt: u32) -> Option<u32> {
    put32(c.a(1), 0);
    if c.a(2) != 0 {
        return None;
    }
    let b = mem::heap_alloc(12, true);
    w32(b, vt);
    w32(b + 4, b);
    w32(b + 8, 1);
    put32(c.a(1), b);
    Some(b)
}
fn T_CreateViewport(c: &mut Ctx) -> u32 {
    let Some(b) = new_obj(c, vp3_vtbl()) else { return CLASS_E_NOAGGREGATION };
    d3d(|d| d.vps.insert(b, Vp3 { refs: 1, ..Default::default() }));
    D3D_OK
}
fn T_CreateMaterial(c: &mut Ctx) -> u32 {
    let Some(b) = new_obj(c, mat3_vtbl()) else { return CLASS_E_NOAGGREGATION };
    let mut m = [0u32; 20];
    m[0] = 80;
    d3d(|d| d.mats.insert(b, m));
    D3D_OK
}
fn T_CreateLight(c: &mut Ctx) -> u32 {
    let Some(b) = new_obj(c, light3_vtbl()) else { return CLASS_E_NOAGGREGATION };
    d3d(|d| d.lights3.insert(b, ([0; 20], 0)));
    D3D_OK
}
/// CreateDevice(clsid, IDirectDrawSurface4, out, outer): the RGB or HAL device
fn T_CreateDevice(c: &mut Ctx) -> u32 {
    if c.a(4) != 0 {
        put32(c.a(3), 0);
        return CLASS_E_NOAGGREGATION;
    }
    if c.a(1) == 0 || !DEVICES[..2].iter().any(|d| d.2 == r32(c.a(1))) {
        put32(c.a(3), 0);
        return DDERR_INVALIDPARAMS;
    }
    let r = create_dev(c, dev3_vtbl());
    if r == D3D_OK {
        let id = r32(r32(c.a(3)) + 4);
        d3d(|d| d.dev3.insert(id, (Vec::new(), 0)));
    }
    r
}

fn dev3_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirect3DDevice3",
            &[
                (E_QueryInterface, 12),
                (V_AddRef, 4),
                (E_Release, 4),
                (E_GetCaps, 12),
                (E_GetStats, 8),
                (E_AddViewport, 8),
                (E_DeleteViewport, 8),
                (E_NextViewport, 16),
                (V_EnumTextureFormats, 12),
                (V_BeginScene, 4),
                (V_EndScene, 4),
                (E_GetDirect3D, 8),
                (E_SetCurrentViewport, 8),
                (E_GetCurrentViewport, 8),
                (V_SetRenderTarget, 12),
                (E_GetRenderTarget, 8),
                (unsupported, 16),
                (unsupported, 24),
                (unsupported, 8),
                (unsupported, 8),
                (unsupported, 8),
                (E_GetRenderState, 12),
                (E_SetRenderState, 12),
                (E_GetLightState, 12),
                (E_SetLightState, 12),
                (V_SetTransform, 12),
                (V_GetTransform, 12),
                (V_MultiplyTransform, 12),
                (E_DrawPrimitive, 24),
                (E_DrawIndexedPrimitive, 32),
                (ok, 8),
                (V_GetClipStatus, 8),
                (E_DrawPrimitiveStrided, 24),
                (E_DrawIndexedPrimitiveStrided, 32),
                (E_DrawPrimitiveVB, 24),
                (E_DrawIndexedPrimitiveVB3, 24),
                (V_ComputeSphereVisibility, 24),
                (E_GetTexture, 12),
                (V_SetTexture, 12),
                (V_GetTextureStageState, 16),
                (V_SetTextureStageState, 16),
                (V_ValidateDevice, 8),
            ],
        )
    })
}

const D3DVOP_LIGHT: u32 = 0x400;
const D3DDP_DONOTLIGHT: u32 = 0x10;
/// Whether a Direct3D 6 device lights vertices of `fvf`: with a material and normals
fn dev3_lights(d: &Dev, fvf: u32) -> bool {
    d.rs[0xff] != 0 && fvf & 0x10 != 0
}
/// D3DRENDERSTATE_LIGHTING for a Direct3D 6 draw of `fvf` with `flags` (D3DDP_DONOTLIGHT)
fn lit3(c: &mut Ctx, fvf: u32, flags: u32) {
    dev(c, |d| d.rs[RS_LIGHTING] = (dev3_lights(d, fvf) && flags & D3DDP_DONOTLIGHT == 0) as u32);
}
fn E_DrawPrimitive(c: &mut Ctx) -> u32 {
    lit3(c, c.a(2), c.a(5));
    V_DrawPrimitive(c)
}
fn E_DrawIndexedPrimitive(c: &mut Ctx) -> u32 {
    lit3(c, c.a(2), c.a(7));
    V_DrawIndexedPrimitive(c)
}
fn E_DrawPrimitiveStrided(c: &mut Ctx) -> u32 {
    lit3(c, c.a(2), c.a(5));
    V_DrawPrimitiveStrided(c)
}
fn E_DrawIndexedPrimitiveStrided(c: &mut Ctx) -> u32 {
    lit3(c, c.a(2), c.a(7));
    V_DrawIndexedPrimitiveStrided(c)
}
fn E_DrawPrimitiveVB(c: &mut Ctx) -> u32 {
    let fvf = d3d(|d| d.vbs.get(&c.a(2)).map_or(0, |x| x.1));
    lit3(c, fvf, c.a(5));
    V_DrawPrimitiveVB(c)
}
fn E_DrawIndexedPrimitiveVB3(c: &mut Ctx) -> u32 {
    let fvf = d3d(|d| d.vbs.get(&c.a(2)).map_or(0, |x| x.1));
    lit3(c, fvf, c.a(5));
    E_DrawIndexedPrimitiveVB(c)
}
fn unsupported(_: &mut Ctx) -> u32 {
    DDERR_UNSUPPORTED
}

fn E_QueryInterface(c: &mut Ctx) -> u32 {
    match guid_first(c.a(1)) {
        // IDirect3DDevice3, IUnknown
        0xb0ab_3b60 | 0 => {
            put32(c.a(2), c.a(0));
            V_AddRef(c);
            D3D_OK
        }
        _ => {
            put32(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
/// Release: the device's viewports go with it (each one's reference dropped)
fn E_Release(c: &mut Ctx) -> u32 {
    let id = r32(c.a(0) + 4);
    let n = V_Release(c);
    if n == 0 {
        if let Some((vps, cur)) = d3d(|d| d.dev3.remove(&id)) {
            for v in vps.into_iter().chain((cur != 0).then_some(cur)) {
                d3d(|d| d.vps.get_mut(&v).map(|x| x.dev = 0));
                vp_release(v);
            }
        }
    }
    n
}
/// GetCaps(hardware desc, software desc): each D3DDEVICEDESC of its own dwSize
fn E_GetCaps(c: &mut Ctx) -> u32 {
    let (hal, hel) = (c.a(1), c.a(2));
    if hal == 0 || hel == 0 || !desc3_size_ok(r32(hal)) || !desc3_size_ok(r32(hel)) {
        return DDERR_INVALIDPARAMS;
    }
    write_desc3(hal, r32(hal), 1, false);
    write_desc3(hel, r32(hel), 1, false);
    D3D_OK
}
fn E_GetStats(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || r32(p) != 24 {
        return DDERR_INVALIDPARAMS;
    }
    mem::fill(p + 4, 20, 0);
    D3D_OK
}
fn E_GetDirect3D(c: &mut Ctx) -> u32 {
    let obj = dev(c, |d| d.dd).unwrap_or(0);
    let b = dd_iface(obj, 5);
    put32(c.a(1), b);
    if b == 0 { D3DERR_INVALIDCALL } else { D3D_OK }
}
fn E_GetRenderTarget(c: &mut Ctx) -> u32 {
    let Some(rt) = dev(c, |d| d.rt) else { return D3DERR_INVALIDCALL };
    with(|st| st.surfs.get_mut(&rt).map(|x| x.refs += 1));
    put32(c.a(1), block(rt, 1));
    D3D_OK
}
/// D3DRENDERSTATE_TEXTUREHANDLE (1): stage 0's texture, by its IDirect3DTexture2 handle
fn E_SetRenderState(c: &mut Ctx) -> u32 {
    // (LIGHTING, LOCALVIEWER, NORMALIZENORMALS are Direct3D 7's: taken, and ignored)
    if matches!(c.a(1), 137 | 142 | 143) {
        return D3D_OK;
    }
    if c.a(1) == 1 {
        let t = c.a(2);
        if t != 0 && surf(t).is_none() {
            return DDERR_INVALIDPARAMS;
        }
        return set(c, Ch::Tex(0, t));
    }
    V_SetRenderState(c)
}
fn E_GetRenderState(c: &mut Ctx) -> u32 {
    if matches!(c.a(1), 137 | 142 | 143) {
        put32(c.a(2), 0xffff_ffff);
        return D3D_OK;
    }
    if c.a(1) == 1 {
        put32(c.a(2), dev(c, |d| d.tex[0]).unwrap_or(0));
        return D3D_OK;
    }
    V_GetRenderState(c)
}
/// D3DLIGHTSTATE_*: the render states they are in Direct3D 7 (AMBIENT, FOGMODE, FOGSTART, FOGEND,
/// FOGDENSITY, COLORVERTEX); MATERIAL: the material of that handle; COLORMODEL: RGB only
fn light_state_rs(k: u32) -> Option<usize> {
    match k {
        2 => Some(RS_AMBIENT),
        4 => Some(RS_FOGVERTEXMODE),
        5 => Some(RS_FOGSTART),
        6 => Some(RS_FOGEND),
        7 => Some(RS_FOGDENSITY),
        8 => Some(RS_COLORVERTEX),
        _ => None,
    }
}
fn E_SetLightState(c: &mut Ctx) -> u32 {
    let (k, v) = (c.a(1), c.a(2));
    match k {
        1 => {
            let m = if v == 0 { None } else { d3d(|d| d.mats.get(&v).copied()) };
            if v != 0 && m.is_none() {
                return DDERR_INVALIDPARAMS;
            }
            let mut f = [0f32; 17];
            if let Some(m) = m {
                for (i, x) in f.iter_mut().enumerate() {
                    *x = f32::from_bits(m[1 + i]);
                }
            }
            let r = set(c, Ch::Mat(f));
            dev(c, |d| d.rs[0xff] = v);
            r
        }
        3 => if v == 2 { D3D_OK } else { DDERR_INVALIDPARAMS },
        _ => match light_state_rs(k) {
            Some(i) => set(c, Ch::Rs(i, v)),
            None => DDERR_INVALIDPARAMS,
        },
    }
}
fn E_GetLightState(c: &mut Ctx) -> u32 {
    let (k, p) = (c.a(1), c.a(2));
    if p == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let v = match k {
        1 => dev(c, |d| d.rs[0xff]),
        3 => Some(2),
        _ => match light_state_rs(k) {
            Some(i) => dev(c, |d| d.rs[i]),
            None => return DDERR_INVALIDPARAMS,
        },
    };
    put32(p, v.unwrap_or(0));
    D3D_OK
}
/// DrawIndexedPrimitiveVB(type, vertex buffer, indices, index count, flags): from vertex 0
fn E_DrawIndexedPrimitiveVB(c: &mut Ctx) -> u32 {
    let (prim, vbb, idx, ni) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((m, fvf, _)) = d3d(|d| d.vbs.get(&vbb).copied()) else { return D3DERR_INVALIDCALL };
    draw(c, prim, Src::packed(fvf, m), Some((idx, ni)), 0, 0)
}
fn E_GetTexture(c: &mut Ctx) -> u32 {
    let s = c.a(1) as usize;
    if s >= 8 || c.a(2) == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let t = dev(c, |d| d.tex[s]).unwrap_or(0);
    if t == 0 || surf(t).is_none() {
        put32(c.a(2), 0);
    } else {
        with(|st| st.surfs.get_mut(&t).map(|x| x.refs += 1));
        put32(c.a(2), block(t, 3));
    }
    D3D_OK
}

/// The device's viewports and current one, when it is a Direct3D 6 device
fn vps_of(c: &Ctx) -> Option<(Vec<u32>, u32)> {
    let id = r32(c.a(0) + 4);
    d3d(|d| d.dev3.get(&id).cloned())
}
/// AddViewport(viewport): added to the device (a reference held); one of another device:
/// DDERR_INVALIDPARAMS
fn E_AddViewport(c: &mut Ctx) -> u32 {
    let (id, v) = (r32(c.a(0) + 4), c.a(1));
    let Some(x) = d3d(|d| d.vps.get(&v).cloned()) else { return DDERR_INVALIDPARAMS };
    if x.dev != 0 {
        return DDERR_INVALIDPARAMS;
    }
    d3d(|d| {
        if let Some(l) = d.dev3.get_mut(&id) {
            l.0.push(v);
        }
        if let Some(x) = d.vps.get_mut(&v) {
            x.dev = id;
            x.refs += 1;
        }
    });
    D3D_OK
}
fn E_DeleteViewport(c: &mut Ctx) -> u32 {
    let (id, v) = (r32(c.a(0) + 4), c.a(1));
    let removed = d3d(|d| {
        let l = d.dev3.get_mut(&id)?;
        let i = l.0.iter().position(|&x| x == v)?;
        l.0.remove(i);
        // (Device3's, unlike Device2's, also lets go of it as the current viewport)
        let cur = l.1 == v;
        if cur {
            l.1 = 0;
        }
        if let Some(x) = d.vps.get_mut(&v) {
            x.dev = 0;
        }
        Some(cur)
    });
    let Some(cur) = removed else { return DDERR_INVALIDPARAMS };
    if cur {
        vp_release(v);
    }
    vp_release(v);
    D3D_OK
}
/// NextViewport(reference, out, D3DNEXT_NEXT | HEAD | TAIL)
fn E_NextViewport(c: &mut Ctx) -> u32 {
    let (r, out, flags) = (c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return DDERR_INVALIDPARAMS;
    }
    put32(out, 0);
    let Some((l, _)) = vps_of(c) else { return D3DERR_INVALIDCALL };
    let v = match flags {
        1 => l.iter().position(|&x| x == r).and_then(|i| l.get(i + 1).copied()),
        2 => l.first().copied(),
        4 => l.last().copied(),
        _ => return DDERR_INVALIDPARAMS,
    };
    match v {
        Some(v) => {
            put32(out, v);
            D3D_OK
        }
        None => D3DERR_INVALIDCALL,
    }
}
/// SetCurrentViewport(viewport): one added to the device, set or not; its data becomes the
/// device's viewport; holds a reference of it
fn E_SetCurrentViewport(c: &mut Ctx) -> u32 {
    let (id, v) = (r32(c.a(0) + 4), c.a(1));
    let Some((l, cur)) = vps_of(c) else { return D3DERR_INVALIDCALL };
    if !l.contains(&v) {
        return DDERR_INVALIDPARAMS;
    }
    if cur != v {
        d3d(|d| {
            d.vps.get_mut(&v).map(|x| x.refs += 1);
            d.dev3.get_mut(&id).map(|x| x.1 = v);
        });
        if cur != 0 {
            vp_lights_off(cur);
            vp_release(cur);
        }
    }
    vp_lights(v);
    apply_vp(v)
}
fn E_GetCurrentViewport(c: &mut Ctx) -> u32 {
    // (none: the pointer is left as it is, NULL or not)
    let out = c.a(1);
    let cur = vps_of(c).map_or(0, |x| x.1);
    if cur == 0 {
        return D3DERR_NOCURRENTVIEWPORT;
    }
    if out == 0 {
        return DDERR_INVALIDPARAMS;
    }
    put32(out, cur);
    d3d(|d| d.vps.get_mut(&cur).map(|x| x.refs += 1));
    D3D_OK
}
/// Viewport `v`'s data as its device's viewport (when it has data)
fn apply_vp(v: u32) -> u32 {
    let Some(x) = d3d(|d| d.vps.get(&v).cloned()) else { return DDERR_INVALIDPARAMS };
    let Some(p) = x.data else { return D3D_OK };
    let ch = Ch::Vp([p[1] as i32, p[2] as i32, p[3] as i32, p[4] as i32], (0., 1.));
    let dev = x.dev;
    let f = |i: usize| f32::from_bits(p[i]);
    let m = clip_space([f(5), f(6), f(7), f(8)], (f(9), f(10)));
    d3d(|d| d.devs.get_mut(&dev).map(|d| {
        let _ = d.apply(&ch);
        d.clip = m;
    }));
    D3D_OK
}
/// The matrix after the projection that takes a D3DVIEWPORT2's clip volume (x, y, width,
/// height; min z, max z) to -1..1 (0..1 in Z)
fn clip_space(c: [f32; 4], z: (f32, f32)) -> [f32; 16] {
    let (sx, sy, sz) = (2. / c[2], 2. / c[3], 1. / (z.1 - z.0));
    let (ox, oy, oz) = (-2. * c[0] / c[2] - 1., -2. * c[1] / c[3] + 1., -z.0 / (z.1 - z.0));
    [sx, 0., 0., 0., 0., sy, 0., 0., 0., 0., sz, 0., ox, oy, oz, 1.]
}

// IDirect3DViewport3

fn vp3_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirect3DViewport3",
            &[
                (P_QueryInterface, 12),
                (P_AddRef, 4),
                (P_Release, 4),
                (P_Initialize, 8),
                (P_GetViewport, 8),
                (P_SetViewport, 8),
                (unsupported, 20),
                (unsupported, 12),
                (P_SetBackground, 8),
                (P_GetBackground, 12),
                (P_SetBackgroundDepth, 8),
                (P_GetBackgroundDepth, 12),
                (P_Clear, 16),
                (P_AddLight, 8),
                (P_DeleteLight, 8),
                (P_NextLight, 16),
                (P_GetViewport2, 8),
                (P_SetViewport2, 8),
                (P_SetBackgroundDepth, 8),
                (P_GetBackgroundDepth2, 12),
                (P_Clear2, 28),
            ],
        )
    })
}
fn vp_of(c: &Ctx) -> Option<Vp3> {
    d3d(|d| d.vps.get(&c.a(0)).cloned())
}
fn vp_release(v: u32) -> u32 {
    d3d(|d| {
        let Some(x) = d.vps.get_mut(&v) else { return 0 };
        x.refs = x.refs.saturating_sub(1);
        let n = x.refs;
        if n == 0 {
            d.vps.remove(&v);
        }
        n
    })
}
fn P_QueryInterface(c: &mut Ctx) -> u32 {
    match guid_first(c.a(1)) {
        // IDirect3DViewport3, 2, 1, IUnknown
        0xb0ab_3b61 | 0x9328_1500 | 0x4417_c146 | 0 => {
            put32(c.a(2), c.a(0));
            P_AddRef(c);
            D3D_OK
        }
        _ => {
            put32(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
fn P_AddRef(c: &mut Ctx) -> u32 {
    d3d(|d| d.vps.get_mut(&c.a(0)).map_or(0, |x| {
        x.refs += 1;
        x.refs
    }))
}
fn P_Release(c: &mut Ctx) -> u32 {
    vp_release(c.a(0))
}
fn P_Initialize(_: &mut Ctx) -> u32 {
    DDERR_ALREADYINITIALIZED
}
/// The new data of the viewport: applied to its device when it is the device's current one
fn vp_set(c: &mut Ctx, data: [u32; 11]) -> u32 {
    let v = c.a(0);
    match d3d(|d| d.vps.get(&v).map(|x| x.dev)) {
        None => return DDERR_INVALIDPARAMS,
        Some(0) => return D3DERR_VIEWPORTHASNODEVICE,
        _ => {}
    }
    let Some(x) = d3d(|d| d.vps.get_mut(&v).map(|x| {
        x.data = Some(data);
        x.clone()
    })) else { return DDERR_INVALIDPARAMS };
    if x.dev != 0 && d3d(|d| d.dev3.get(&x.dev).is_some_and(|l| l.1 == v)) {
        return apply_vp(v);
    }
    D3D_OK
}
/// SetViewport2(D3DVIEWPORT2): dwSize 44, the clip volume kept as given
fn P_SetViewport2(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || r32(p) != 44 {
        return DDERR_INVALIDPARAMS;
    }
    let d: [u32; 11] = std::array::from_fn(|i| r32(p + 4 * i as u32));
    vp_set(c, d)
}
fn P_GetViewport2(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || r32(p) != 44 {
        return DDERR_INVALIDPARAMS;
    }
    let Some(x) = vp_of(c) else { return DDERR_INVALIDPARAMS };
    let Some(d) = x.data else { return D3DERR_VIEWPORTDATANOTSET };
    // (the Z range is the clip volume's, not the device's: reported as 0..1)
    for (i, w) in d.iter().enumerate().take(9).skip(1) {
        w32(p + 4 * i as u32, *w);
    }
    w32(p + 36, 0);
    w32(p + 40, 1f32.to_bits());
    D3D_OK
}
/// SetViewport(D3DVIEWPORT): position and size, the z range; its clip volume (-1..1 by
/// width and height) is D3DVIEWPORT2's
fn P_SetViewport(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || r32(p) != 44 {
        return DDERR_INVALIDPARAMS;
    }
    // (kept as D3DVIEWPORT2: the clip volume is where the scale puts the edges of the
    // rectangle around its center; the Z range is always 0..1, ddraw4's test_viewport_object)
    let (w, h) = (r32(p + 12), r32(p + 16));
    let (sx, sy) = (f32::from_bits(r32(p + 20)), f32::from_bits(r32(p + 24)));
    let clip = viewport_clip(w as f32, h as f32, sx, sy);
    let d = [44, r32(p + 4), r32(p + 8), w, h, clip[0].to_bits(), clip[1].to_bits(), clip[2].to_bits(), clip[3].to_bits(), 0, 1f32.to_bits()];
    vp_set(c, d)
}
fn P_GetViewport(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || r32(p) != 44 {
        return DDERR_INVALIDPARAMS;
    }
    let Some(x) = vp_of(c) else { return DDERR_INVALIDPARAMS };
    let Some(d) = x.data else { return D3DERR_VIEWPORTDATANOTSET };
    let f = |i: usize| f32::from_bits(d[i]);
    let v = viewport_scale(d[3] as f32, d[4] as f32, [f(5), f(6), f(7), f(8)]);
    for (i, v) in [d[1], d[2], d[3], d[4], v[0].to_bits(), v[1].to_bits(), v[2].to_bits(), v[3].to_bits(), 0, 1f32.to_bits()].iter().enumerate() {
        w32(p + 4 + 4 * i as u32, *v);
    }
    D3D_OK
}
/// D3DVIEWPORT's scale (x, y) of a width x height rectangle as D3DVIEWPORT2's clip volume
/// (x, y, width, height)
fn viewport_clip(w: f32, h: f32, sx: f32, sy: f32) -> [f32; 4] {
    [-(w / 2.) / sx, (h / 2.) / sy, w / sx, h / sy]
}
/// D3DVIEWPORT2's clip volume as D3DVIEWPORT's (scale x, scale y, max x, max y)
fn viewport_scale(w: f32, h: f32, clip: [f32; 4]) -> [f32; 4] {
    [w / clip[2], h / clip[3], clip[0] + clip[2], clip[1]]
}
fn P_SetBackground(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    if h != 0 && !d3d(|d| d.mats.contains_key(&h)) {
        return DDERR_INVALIDPARAMS;
    }
    d3d(|d| d.vps.get_mut(&c.a(0)).map(|x| x.bg = h));
    D3D_OK
}
fn P_GetBackground(c: &mut Ctx) -> u32 {
    let (h, valid) = (c.a(1), c.a(2));
    if h == 0 || valid == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let bg = vp_of(c).map_or(0, |x| x.bg);
    put32(h, bg);
    put32(valid, (bg != 0) as u32);
    D3D_OK
}
fn P_SetBackgroundDepth(c: &mut Ctx) -> u32 {
    let s = sid(c.a(1)).map_or(0, |x| x.0);
    d3d(|d| d.vps.get_mut(&c.a(0)).map(|x| x.depth = s));
    D3D_OK
}
fn P_GetBackgroundDepth(c: &mut Ctx) -> u32 {
    background_depth(c, 0)
}
fn P_GetBackgroundDepth2(c: &mut Ctx) -> u32 {
    background_depth(c, 1)
}
fn background_depth(c: &mut Ctx, kind: u8) -> u32 {
    let (out, valid) = (c.a(1), c.a(2));
    if out == 0 || valid == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let s = vp_of(c).map_or(0, |x| x.depth);
    if s == 0 || surf(s).is_none() {
        put32(out, 0);
        put32(valid, 0);
    } else {
        with(|st| st.surfs.get_mut(&s).map(|x| x.refs += 1));
        put32(out, block(s, kind));
        put32(valid, 1);
    }
    D3D_OK
}
/// The device and rectangle of a viewport to clear
fn vp_target(c: &Ctx) -> Result<(u32, [i32; 4]), u32> {
    let Some(x) = vp_of(c) else { return Err(DDERR_INVALIDPARAMS) };
    if x.dev == 0 {
        return Err(D3DERR_VIEWPORTHASNODEVICE);
    }
    let Some(d) = x.data else { return Err(D3DERR_VIEWPORTDATANOTSET) };
    Ok((x.dev, [d[1] as i32, d[2] as i32, d[3] as i32, d[4] as i32]))
}
/// Clear(count, rects, D3DCLEAR_TARGET | ZBUFFER): the background material's diffuse color
/// (black without one), depth 1
fn P_Clear(c: &mut Ctx) -> u32 {
    let (n, rects, flags) = (c.a(1), c.a(2), c.a(3));
    if rects == 0 || n == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let (dev, vp) = match vp_target(c) {
        Ok(x) => x,
        Err(e) => return e,
    };
    let bg = vp_of(c).map_or(0, |x| x.bg);
    let m = d3d(|d| d.mats.get(&bg).copied());
    let color = m.map_or(0, |m| {
        let ch = |k: usize| (f32::from_bits(m[1 + k]).clamp(0., 1.) * 255. + 0.5) as u32;
        ch(3) << 24 | ch(0) << 16 | ch(1) << 8 | ch(2)
    });
    clear_dev(dev, vp, n, rects, flags & 3, color, 1.)
}
/// Clear2(count, rects, flags, color, z, stencil)
fn P_Clear2(c: &mut Ctx) -> u32 {
    let (n, rects, flags, color, z) = (c.a(1), c.a(2), c.a(3), c.a(4), f32::from_bits(c.a(5)));
    if rects == 0 || n == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let (dev, vp) = match vp_target(c) {
        Ok(x) => x,
        Err(e) => return e,
    };
    clear_dev(dev, vp, n, rects, flags & 3, color, z)
}
fn P_AddLight(c: &mut Ctx) -> u32 {
    let (v, l) = (c.a(0), c.a(1));
    match d3d(|d| d.lights3.get(&l).map(|x| x.1)) {
        None => return DDERR_INVALIDPARAMS,
        // (in this viewport or another one: ddraw4's test_lighting)
        Some(0) => {}
        Some(_) => return D3DERR_LIGHTHASVIEWPORT,
    }
    d3d(|d| {
        d.vps.get_mut(&v).map(|x| x.lights.push(l));
        d.lights3.get_mut(&l).map(|x| x.1 = v);
    });
    w32(l + 8, r32(l + 8) + 1);
    vp_lights(v);
    D3D_OK
}
fn P_DeleteLight(c: &mut Ctx) -> u32 {
    let l = c.a(1);
    let v = c.a(0);
    let found = d3d(|d| d.vps.get_mut(&v).and_then(|x| x.lights.iter().position(|&y| y == l).map(|i| x.lights.remove(i))).is_some());
    if !found {
        return D3DERR_LIGHTNOTINTHISVIEWPORT;
    }
    d3d(|d| d.lights3.get_mut(&l).map(|x| x.1 = 0));
    vp_lights(v);
    light_release(l);
    D3D_OK
}
fn P_NextLight(c: &mut Ctx) -> u32 {
    let (r, out, flags) = (c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return DDERR_INVALIDPARAMS;
    }
    put32(out, 0);
    let l = vp_of(c).map_or(vec![], |x| x.lights);
    let v = match flags {
        1 => l.iter().position(|&x| x == r).and_then(|i| l.get(i + 1).copied()),
        2 => l.first().copied(),
        4 => l.last().copied(),
        _ => return DDERR_INVALIDPARAMS,
    };
    match v {
        Some(v) => {
            w32(v + 8, r32(v + 8) + 1);
            put32(out, v);
            D3D_OK
        }
        None => D3DERR_INVALIDCALL,
    }
}

// IDirect3DMaterial3

fn mat3_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| vtable("IDirect3DMaterial3", &[(M_QueryInterface, 12), (O_AddRef, 4), (M_Release, 4), (M_SetMaterial, 8), (M_GetMaterial, 8), (M_GetHandle, 12)]))
}
fn O_AddRef(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    w32(b + 8, r32(b + 8) + 1);
    r32(b + 8)
}
fn M_QueryInterface(c: &mut Ctx) -> u32 {
    match guid_first(c.a(1)) {
        // IDirect3DMaterial3, 2, 1, IUnknown
        0xca9c_46f4 | 0x9328_1503 | 0x4417_c144 | 0 => {
            put32(c.a(2), c.a(0));
            O_AddRef(c);
            D3D_OK
        }
        _ => {
            put32(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
fn M_Release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = r32(b + 8).saturating_sub(1);
    w32(b + 8, n);
    if n == 0 {
        d3d(|d| d.mats.remove(&b));
    }
    n
}
/// SetMaterial(D3DMATERIAL): dwSize 80
fn M_SetMaterial(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || r32(p) != 80 {
        return DDERR_INVALIDPARAMS;
    }
    let m: [u32; 20] = std::array::from_fn(|i| r32(p + 4 * i as u32));
    d3d(|d| d.mats.insert(c.a(0), m));
    D3D_OK
}
fn M_GetMaterial(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || r32(p) != 80 {
        return DDERR_INVALIDPARAMS;
    }
    let Some(m) = d3d(|d| d.mats.get(&c.a(0)).copied()) else { return DDERR_INVALIDPARAMS };
    for (i, w) in m.iter().enumerate().skip(1) {
        w32(p + 4 * i as u32, *w);
    }
    D3D_OK
}
/// GetHandle(device, handle): the material's handle (its block)
fn M_GetHandle(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return DDERR_INVALIDPARAMS;
    }
    put32(c.a(2), c.a(0));
    D3D_OK
}

// IDirect3DLight

fn light3_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| vtable("IDirect3DLight", &[(L_QueryInterface, 12), (O_AddRef, 4), (L_Release, 4), (P_Initialize, 8), (L_SetLight, 8), (L_GetLight, 8)]))
}
fn L_QueryInterface(c: &mut Ctx) -> u32 {
    match guid_first(c.a(1)) {
        0x4417_c142 | 0 => {
            put32(c.a(2), c.a(0));
            O_AddRef(c);
            D3D_OK
        }
        _ => {
            put32(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
fn light_release(b: u32) -> u32 {
    let n = r32(b + 8).saturating_sub(1);
    w32(b + 8, n);
    if n == 0 {
        d3d(|d| d.lights3.remove(&b));
    }
    n
}
fn L_Release(c: &mut Ctx) -> u32 {
    light_release(c.a(0))
}
/// SetLight(D3DLIGHT, 76 bytes, or D3DLIGHT2, 80: its dwFlags; a D3DLIGHT is an active one)
fn L_SetLight(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 || !(1..=4).contains(&r32(p + 4)) {
        return DDERR_INVALIDPARAMS;
    }
    let mut l = [0u32; 20];
    for (i, w) in l.iter_mut().enumerate().take(19) {
        *w = r32(p + 4 * i as u32);
    }
    l[19] = if r32(p) >= 80 { r32(p + 76) } else { D3DLIGHT_ACTIVE };
    let Some(v) = d3d(|d| d.lights3.get_mut(&c.a(0)).map(|x| {
        x.0 = l;
        x.1
    })) else { return DDERR_INVALIDPARAMS };
    if v != 0 {
        vp_lights(v);
    }
    D3D_OK
}
const D3DLIGHT_ACTIVE: u32 = 1;
const D3DLIGHT_NO_SPECULAR: u32 = 2;
const D3DERR_LIGHTHASVIEWPORT: u32 = 0x8876_0000 | 751;
const D3DERR_LIGHTNOTINTHISVIEWPORT: u32 = 0x8876_0000 | 752;
/// D3DLIGHT2 as D3DLIGHT7: its color the diffuse and (unless D3DLIGHT_NO_SPECULAR) specular one
fn light7_of(l: &[u32; 20]) -> [u32; 26] {
    let mut o = [0u32; 26];
    o[0] = l[1];
    o[1..5].copy_from_slice(&l[2..6]);
    if l[19] & D3DLIGHT_NO_SPECULAR == 0 {
        o[5..9].copy_from_slice(&l[2..6]);
    }
    o[13..26].copy_from_slice(&l[6..19]);
    o
}
/// Viewport `v`'s lights as its device's (when it is the device's current viewport): the active
/// ones at indices 1.., the indices it gave before off
fn vp_lights(v: u32) {
    let Some(x) = d3d(|d| d.vps.get(&v).cloned()) else { return };
    if x.dev == 0 || !d3d(|d| d.dev3.get(&x.dev).is_some_and(|l| l.1 == v)) {
        return;
    }
    let lights: Vec<_> = x.lights.iter().map(|l| d3d(|d| d.lights3.get(l).map(|y| y.0)).unwrap_or([0; 20])).collect();
    d3d(|d| {
        let Some(dv) = d.devs.get_mut(&x.dev) else { return };
        for i in 1..=x.lit {
            let _ = dv.apply(&Ch::LightOn(i, false));
        }
        for (i, l) in lights.iter().enumerate() {
            let i = i as u32 + 1;
            let _ = dv.apply(&Ch::Light(i, light7_of(l)));
            let _ = dv.apply(&Ch::LightOn(i, l[19] & D3DLIGHT_ACTIVE != 0));
        }
        d.vps.get_mut(&v).map(|y| y.lit = y.lit.max(lights.len() as u32));
    });
}
/// Viewport `v`'s lights off on its device (another viewport becomes the current one)
fn vp_lights_off(v: u32) {
    let Some(x) = d3d(|d| d.vps.get(&v).cloned()) else { return };
    d3d(|d| {
        if let Some(dv) = d.devs.get_mut(&x.dev) {
            for i in 1..=x.lit {
                let _ = dv.apply(&Ch::LightOn(i, false));
            }
        }
    });
}
fn L_GetLight(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return DDERR_INVALIDPARAMS;
    }
    let Some((l, _)) = d3d(|d| d.lights3.get(&c.a(0)).copied()) else { return DDERR_INVALIDPARAMS };
    // (the first dwSize bytes of what was set, its flags as D3DLIGHT2's)
    for i in 1..(r32(p) as usize / 4).min(20) {
        w32(p + 4 * i as u32, l[i]);
    }
    D3D_OK
}

// IDirect3DTexture2: the surface's interface of kind 3 (a reference of the surface)

pub(super) fn tex2_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| vtable("IDirect3DTexture2", &[(S_QueryInterface, 12), (S_AddRef, 4), (S_Release, 4), (X_GetHandle, 12), (ok, 12), (X_Load, 8)]))
}
/// IDirect3DTexture: Texture2's methods, with Initialize (the surface is one already),
/// PaletteChanged and Unload
pub(super) fn tex_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirect3DTexture",
            &[(S_QueryInterface, 12), (S_AddRef, 4), (S_Release, 4), (X_Initialize, 12), (X_GetHandle, 12), (ok, 12), (X_Load, 8), (ok, 4)],
        )
    })
}
fn X_Initialize(_c: &mut Ctx) -> u32 {
    DDERR_ALREADYINITIALIZED
}
/// GetHandle(device, handle): the surface's id (what D3DRENDERSTATE_TEXTUREHANDLE takes)
fn X_GetHandle(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return DDERR_INVALIDPARAMS;
    }
    put32(c.a(2), sid(c.a(0)).map_or(0, |x| x.0));
    D3D_OK
}
/// Load(source texture): every level the two have of the same size, and the palette
fn X_Load(c: &mut Ctx) -> u32 {
    let (Some((d, _)), Some((s, _))) = (sid(c.a(0)), sid(c.a(1))) else { return DDERR_INVALIDPARAMS };
    let (dl, sl) = (levels(d), levels(s));
    if dl.is_empty() || sl.is_empty() || (dl[0].w, dl[0].h) != (sl[0].w, sl[0].h) {
        return DDERR_INVALIDPARAMS;
    }
    for (dx, sx) in dl.iter().zip(sl.iter()) {
        if dx.bpp == sx.bpp {
            let r = Rect { l: 0, t: 0, r: sx.w, b: sx.h };
            blit(dx, r, sx, r, None, (false, false));
        }
    }
    // (the source's color key goes with it; a destination's own one stays when the source has
    // none: ddraw4's test_texture_load_ckey)
    if let Some(k) = surf(s).and_then(|x| x.ck_src) {
        with(|st| st.surfs.get_mut(&d).map(|x| x.ck_src = Some(k)));
    }
    let (dp, sp) = (dl[0].palette, sl[0].palette);
    if dp != 0 && sp != 0 {
        with(|st| {
            if let Some(p) = st.palettes.get(&sp).copied() {
                st.palettes.insert(dp, p);
            }
        });
    }
    D3D_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ddraw7's test_device_load: the source level of the destination's size is where the copy
    /// starts; the rect and point halve with each level; too many destination levels, an empty
    /// rect or one off the destination is DDERR_INVALIDPARAMS.
    #[test]
    fn load_goes_level_by_level() {
        let mips = |w: i32, n: usize| (0..n).map(|i| ((w >> i).max(1), (w >> i).max(1))).collect::<Vec<_>>();
        let r = |l, t, r, b| Rect { l, t, r, b };
        // (tests 0, 1, 4, 5, 9 of the table)
        assert!(load_plan(&mips(128, 8), &mips(128, 8), Some(r(0, 0, 0, 0)), Some((0, 0))).is_none());
        assert!(load_plan(&mips(128, 8), &mips(128, 8), Some(r(0, 0, 100, 100)), Some((50, 50))).is_none());
        assert!(load_plan(&[(128, 128)], &[(256, 128)], None, None).is_none());
        assert!(load_plan(&[(256, 128)], &[(128, 128)], None, None).is_none());
        assert!(load_plan(&mips(128, 4), &mips(128, 8), Some(r(30, 20, 93, 52)), Some((31, 31))).is_none());
        // (test 2: every level, halving)
        let p = load_plan(&mips(128, 8), &mips(128, 8), Some(r(30, 20, 93, 52)), Some((31, 31))).unwrap();
        assert_eq!(p.len(), 8);
        assert_eq!(p[0], (0, r(30, 20, 93, 52), (31, 31)));
        assert_eq!(p[1], (1, r(15, 10, 47, 26), (15, 15)));
        assert_eq!(p[7], (7, r(0, 0, 1, 1), (0, 0)));
        // (tests 10, 11: a smaller destination starts two levels down)
        let p = load_plan(&mips(128, 8), &[(32, 32)], Some(r(32, 32, 96, 96)), Some((32, 32))).unwrap();
        assert_eq!(p, vec![(2, r(8, 8, 24, 24), (8, 8))]);
        let p = load_plan(&mips(128, 8), &mips(32, 4), Some(r(0, 0, 64, 64)), Some((0, 0))).unwrap();
        assert_eq!(p.iter().map(|e| e.0).collect::<Vec<_>>(), vec![2, 3, 4, 5]);
        assert_eq!(p[3].1, r(0, 0, 2, 2));
        // (test 8: fewer destination levels than the source has)
        assert_eq!(load_plan(&mips(128, 8), &mips(128, 4), None, None).unwrap().len(), 4);
    }

    /// d3d.h's numbers are decimal; CreateVertexBuffer's failure is what Windows returns
    /// (winref ddrefs), not d3d.h's MAKE_DDHRESULT(2061).
    #[test]
    fn error_codes_are_d3d_h_decimal_ones() {
        assert_eq!(D3DERR_SCENE_IN_SCENE, 0x8876_0000 | 760);
        assert_eq!(D3DERR_SCENE_NOT_IN_SCENE, 0x8876_0000 | 761);
        assert_eq!(D3DERR_VBUF_CREATE_FAILED, 0x8876_0825);
    }

    /// The clip volume of ddraw4's test_process_vertices (x 2, y 3, 4 x 5, z -2..3) takes its
    /// points where Windows puts them on the 100 x 200 viewport at (10, 20)
    #[test]
    fn clip_volume_maps_to_the_viewport() {
        let m = clip_space([2., 3., 4., 5.], (-2., 3.));
        let on_screen = |p: [f32; 3]| {
            let v: [f32; 4] = std::array::from_fn(|c| p[0] * m[c] + p[1] * m[4 + c] + p[2] * m[8 + c] + m[12 + c]);
            [10. + (v[0] + 1.) * 50., 20. + (1. - v[1]) * 100., v[2]]
        };
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4);
        assert!(near(on_screen([-1., -1., -1.]), [-65., 180., 0.2]));
        assert!(near(on_screen([0., 0., 0.]), [-40., 140., 0.4]));
        assert!(near(on_screen([1., 1., 1.]), [-15., 100., 0.6]));
        assert_eq!(clip_space([-1., 1., 2., 2.], (0., 1.)), IDENT);
    }

    /// A D3DLIGHT2 as the D3DLIGHT7 its device gets: its color diffuse and specular (none with
    /// D3DLIGHT_NO_SPECULAR), no ambient, the rest moved over
    #[test]
    fn light2_as_light7() {
        let mut l: [u32; 20] = std::array::from_fn(|i| 100 + i as u32);
        l[1] = 3;
        l[19] = D3DLIGHT_ACTIVE;
        let o = light7_of(&l);
        assert_eq!(o[0], 3);
        assert_eq!(o[1..5], [102, 103, 104, 105]);
        assert_eq!(o[5..9], [102, 103, 104, 105]);
        assert_eq!(o[9..13], [0; 4]);
        assert_eq!(o[13], 106);
        assert_eq!(o[25], 118);
        l[19] |= D3DLIGHT_NO_SPECULAR;
        assert_eq!(light7_of(&l)[5..9], [0; 4]);
        assert_eq!(D3DERR_LIGHTHASVIEWPORT, 0x8876_02ef);
    }

    /// D3DVIEWPORT <-> D3DVIEWPORT2 as Windows converts them (ddraw4's test_viewport_object:
    /// a 480 x 360 rectangle)
    #[test]
    fn viewport_scale_and_clip_volume() {
        assert_eq!(viewport_scale(480., 360., [2., -1.75, 2.5, -1.5]), [192., -240., 4.5, -1.75]);
        assert_eq!(viewport_scale(480., 360., [-1.5, 1.75, -1.5, 2.]), [-320., 180., -3., 1.75]);
        assert_eq!(viewport_clip(480., 360., -320., 180.), [0.75, 1., -1.5, 2.]);
        assert_eq!(viewport_clip(480., 360., 192., -240.), [-1.25, -0.75, 2.5, -1.5]);
        // (the usual one: half the size as the scale is the -1..1 square)
        assert_eq!(viewport_clip(640., 480., 320., 240.), [-1., 1., 2., 2.]);
    }

    /// Direct3D 6's viewport errors (d3d.h: 773, 774, 775) and D3DDEVICEDESC's three sizes
    #[test]
    fn dx6_viewport_errors_and_desc_sizes() {
        assert_eq!(D3DERR_VIEWPORTDATANOTSET, 0x8876_0000 | 773);
        assert_eq!(D3DERR_VIEWPORTHASNODEVICE, 0x8876_0000 | 774);
        assert_eq!(D3DERR_NOCURRENTVIEWPORT, 0x8876_0000 | 775);
        assert!([172, 204, 252].iter().all(|&n| desc3_size_ok(n)));
        assert!(![0, 171, 236, 253].iter().any(|&n| desc3_size_ok(n)));
    }

    const ARGB: Fmt = Fmt { ch: [(16, 255), (8, 255), (0, 255), (24, 255)] };

    /// ddrefs: GetLightEnable of a light LightEnable turned on reads 128, of one off 0
    #[test]
    fn light_enable_reads_as_windows() {
        let g = include_str!("../../../tests/winref/golden/ddrefs-32/stdout.txt");
        let lines: Vec<&str> = g.lines().collect();
        let after = |what: &str| lines[lines.iter().position(|l| l.starts_with(what)).unwrap() + 1].trim();
        assert_eq!(after("GetLightEnable 150000 "), format!("on {}", light_on_value(true)));
        assert_eq!(after("GetLightEnable 7 "), format!("on {}", light_on_value(false)));
    }

    fn raster(buf: &mut [u32], w: i32, h: i32) -> Raster {
        Raster {
            rt: buf.as_mut_ptr() as *mut u8,
            pitch: w as usize * 4,
            bpp: 4,
            fmt: ARGB,
            z: None,
            clip: [0, 0, w, h],
            ztest: None,
            zwrite: false,
            blend: None,
            atest: None,
            ck: false,
            flat: false,
            spec: false,
            fog: None,
            cull: 1,
            stages: Vec::new(),
            tfactor: [1.; 4],
            ntc: 1,
            copy: true,
        }
    }

    fn vtx(x: f32, y: f32, u: f32, v: f32, diff: C4) -> ([f32; 4], Vtx) {
        let mut tex = [[0.; 2]; 8];
        tex[0] = [u, v];
        ([x, y, 0.5, 1.], Vtx { clip: [x, y, 0.5, 1.], screen: true, diff, spec: [0.; 4], tex })
    }

    fn quad(r: &Raster, x0: f32, y0: f32, x1: f32, y1: f32, diff: C4) {
        let a = vtx(x0, y0, 0., 0., diff);
        let b = vtx(x1, y0, 1., 0., diff);
        let c = vtx(x0, y1, 0., 1., diff);
        let d = vtx(x1, y1, 1., 1., diff);
        r.tri([(&a.0, &a.1), (&b.0, &b.1), (&c.0, &c.1)]);
        r.tri([(&c.0, &c.1), (&b.0, &b.1), (&d.0, &d.1)]);
    }

    /// Random quads and fans drawn with additive blending: every pixel is written at most
    /// once by triangles sharing an edge (the top-left rule), and a quad on integer-plus-half
    /// coordinates covers exactly its pixels.
    #[test]
    fn shared_edges_cover_once() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as u32
        };
        for _ in 0..300 {
            let (w, h) = (24, 20);
            let mut buf = vec![0u32; (w * h) as usize];
            let mut r = raster(&mut buf, w, h);
            r.blend = Some((2, 2));
            let one = [1. / 255., 0., 0., 0.];
            // a fan around a random center: its triangles share edges
            let cx = rnd(w as u32 * 8) as f32 / 8.;
            let cy = rnd(h as u32 * 8) as f32 / 8.;
            let n = 3 + rnd(8);
            let pts: Vec<_> = (0..n)
                .map(|i| {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    let rad = 2. + rnd(80) as f32 / 8.;
                    vtx(cx + rad * t.cos(), cy + rad * t.sin(), 0., 0., one)
                })
                .collect();
            let c = vtx(cx, cy, 0., 0., one);
            for i in 0..n as usize {
                let (p, q) = (&pts[i], &pts[(i + 1) % n as usize]);
                r.tri([(&c.0, &c.1), (&p.0, &p.1), (&q.0, &q.1)]);
            }
            assert!(buf.iter().all(|&p| p >> 16 & 0xff <= 1), "a pixel drawn twice");
        }
        let (w, h) = (16, 16);
        let mut buf = vec![0u32; 256];
        let mut r = raster(&mut buf, w, h);
        r.blend = Some((2, 2));
        quad(&r, 2.5, 3.5, 9.5, 7.5, [1. / 255., 0., 0., 0.]);
        for y in 0..h {
            for x in 0..w {
                let inside = (3..=9).contains(&x) && (4..=7).contains(&y);
                assert_eq!(buf[(y * w + x) as usize] >> 16 & 0xff, inside as u32, "({x}, {y})");
            }
        }
    }

    /// A texture drawn 1:1 on the half-pixel offset (D3D's pixel centers are integers, texel
    /// centers half-integers) comes out exactly, with point and with bilinear filtering.
    #[test]
    fn one_to_one_texture_is_exact() {
        let (tw, th) = (8, 6);
        let texels: Vec<u32> = (0..tw * th).map(|i| 0xff00_0000 | (i as u32 * 0x0103_07)).collect();
        for linear in [false, true] {
            let (w, h) = (12, 10);
            let mut buf = vec![0u32; (w * h) as usize];
            let mut r = raster(&mut buf, w, h);
            r.stages.push(Stage {
                cop: 2,
                ca: [2, 1],
                aop: 2,
                aa: [2, 1],
                tex: Some(Tex { bits: texels.as_ptr() as *const u8, w: tw, h: th, pitch: tw as usize * 4, bpp: 4, fmt: ARGB, pal: None, key: None }),
                tci: 0,
                addr: [3, 3],
                border: [0.; 4],
                linear,
            });
            quad(&r, 1.5, 2.5, 1.5 + tw as f32, 2.5 + th as f32, [1.; 4]);
            for y in 0..th {
                for x in 0..tw {
                    assert_eq!(buf[((y + 3) * w + x + 2) as usize], texels[(y * tw + x) as usize], "linear {linear} ({x}, {y})");
                }
            }
        }
    }

    /// Every 16-bit value of each 16-bit texture format survives unpack + pack.
    #[test]
    fn formats_round_trip() {
        for (bpp, _, r, g, b, a) in TEX_FORMATS {
            let f = Fmt { ch: [r, g, b, a].map(|m| if m == 0 { (0, 0) } else { (m.trailing_zeros(), m >> m.trailing_zeros()) }) };
            let all = r | g | b | a;
            let vals: Box<dyn Iterator<Item = u32>> = if bpp == 16 { Box::new(0..0x10000) } else { Box::new((0..0x10000u32).map(|i| i.wrapping_mul(0x9e37_79b9))) };
            for v in vals {
                assert_eq!(f.pack(f.unpack(v)), v & all, "{v:#x} in {r:#x}/{g:#x}/{b:#x}/{a:#x}");
            }
        }
    }

    /// Near clipping keeps every output vertex in front (z >= 0, w > 0) and keeps the
    /// triangles wholly in front as they are.
    #[test]
    fn near_clip() {
        let v = |x: f32, y: f32, z: f32, w: f32| Vtx { clip: [x, y, z, w], screen: false, diff: [1.; 4], spec: [0.; 4], tex: [[0.; 2]; 8] };
        let p = clip_near(vec![v(0., 0., 0.5, 1.), v(1., 0., 0.5, 1.), v(0., 1., 0.5, 1.)]);
        assert_eq!(p.len(), 3);
        let p = clip_near(vec![v(0., 0., -1., 1.), v(1., 0., 1., 1.), v(0., 1., 1., 1.)]);
        assert_eq!(p.len(), 4);
        assert!(p.iter().all(|x| x.clip[2] >= -1e-6 && x.clip[3] > 0.));
        assert!(clip_near(vec![v(0., 0., -1., 1.), v(1., 0., -1., 1.), v(0., 1., -2., 1.)]).len() < 3);
    }

    /// The far plane too (z <= w), clipping on or off (Wine's ddraw visual depth_clamp_test):
    /// a triangle wholly past it is gone, one across it is cut at z = w; and whatever is left of
    /// any triangle lies between the planes.
    #[test]
    fn far_clip() {
        let v = |x: f32, y: f32, z: f32, w: f32| Vtx { clip: [x, y, z, w], screen: false, diff: [1.; 4], spec: [0.; 4], tex: [[0.; 2]; 8] };
        assert!(clip_near(vec![v(0., 0., 10., 1.), v(1., 0., 10., 1.), v(0., 1., 10., 1.)]).len() < 3);
        let p = clip_near(vec![v(0., 0., 0.5, 1.), v(1., 0., 2., 1.), v(0., 1., 2., 1.)]);
        assert_eq!(p.len(), 3);
        assert!(p.iter().all(|x| x.clip[2] <= x.clip[3] + 1e-6));
        let mut r = 0x9e37_79b9u32;
        let mut f = || {
            r ^= r << 13;
            r ^= r >> 17;
            r ^= r << 5;
            (r % 4000) as f32 / 1000. - 2.
        };
        for _ in 0..20_000 {
            let t = [v(f(), f(), f(), f()), v(f(), f(), f(), f()), v(f(), f(), f(), f())];
            for x in clip_near(t.to_vec()) {
                assert!(x.clip[2] >= -1e-4 && x.clip[3] > 0. && x.clip[2] <= x.clip[3] + 1e-4, "{:?}", x.clip);
            }
        }
    }

    /// SetLight's checks (Wine's d3d LightTest, winref d3d7state)
    #[test]
    fn light_checks() {
        let l = |t: u32, att0: f32| {
            let mut l = [0u32; 26];
            l[0] = t;
            l[21] = att0.to_bits();
            l
        };
        assert!(!light_ok(&l(0, 0.)) && !light_ok(&l(4, 0.)) && !light_ok(&l(12345, 0.)));
        for t in [1, 2] {
            assert!(!light_ok(&l(t, -1.)) && !light_ok(&l(t, f32::NEG_INFINITY)));
            assert!(light_ok(&l(t, 0.)) && light_ok(&l(t, 1.)) && light_ok(&l(t, f32::INFINITY)) && light_ok(&l(t, f32::NAN)));
        }
        assert!(light_ok(&l(3, -1.)), "a directional light's attenuation is not looked at");
        // enabling a light never set makes the default one: white, along +z
        let mut d = Dev::new(0, 0);
        assert_eq!(d.apply(&Ch::LightOn(20, true)), 0);
        let mut want = [0u32; 26];
        want[0] = 3;
        want[1..4].copy_from_slice(&[1f32.to_bits(); 3]);
        want[18] = 1f32.to_bits();
        assert_eq!(d.lights[&20].0, want);
    }

    /// Vertex fog of untransformed vertices: the modes' formulas of the eye depth, clamped, and
    /// nothing (the specular alpha stays) without FOGENABLE or a mode
    #[test]
    fn vertex_fog_modes() {
        let mut d = Dev::new(0, 0);
        let at = |d: &Dev, z: f32| vertex_fog(d, &IDENT, [0., 0., z, 1.]);
        assert_eq!(at(&d, 0.5), None);
        d.rs[RS_FOGENABLE] = 1;
        assert_eq!(at(&d, 0.5), None);
        d.rs[RS_FOGVERTEXMODE] = 3;
        // (FOGSTART 0, FOGEND 1 by default)
        assert_eq!(at(&d, 0.25), Some(0.75));
        assert_eq!(at(&d, 1.5), Some(0.));
        assert_eq!(at(&d, -1.), Some(1.));
        d.rs[RS_FOGSTART] = 2f32.to_bits();
        d.rs[RS_FOGEND] = 2f32.to_bits();
        assert_eq!(at(&d, 1.), Some(1.));
        assert_eq!(at(&d, 3.), Some(0.));
        d.rs[RS_FOGVERTEXMODE] = 1;
        d.rs[RS_FOGDENSITY] = 0.5f32.to_bits();
        assert!((at(&d, 2.).unwrap() - (-1f32).exp()).abs() < 1e-6);
        d.rs[RS_FOGVERTEXMODE] = 2;
        assert!((at(&d, 2.).unwrap() - (-1f32).exp()).abs() < 1e-6);
        assert!((at(&d, 4.).unwrap() - (-4f32).exp()).abs() < 1e-6);
        // RANGEFOGENABLE: the distance, not the depth
        d.rs[RS_FOGVERTEXMODE] = 3;
        d.rs[RS_FOGSTART] = 0;
        d.rs[RS_FOGEND] = 10f32.to_bits();
        d.rs[RS_RANGEFOGENABLE] = 1;
        assert!((vertex_fog(&d, &IDENT, [3., 4., 0., 1.]).unwrap() - 0.5).abs() < 1e-6);
        // through the world and view transforms (eye z = 2 z + 1)
        d.rs[RS_RANGEFOGENABLE] = 0;
        let mut wv = IDENT;
        wv[10] = 2.;
        wv[14] = 1.;
        assert!((vertex_fog(&d, &wv, [0., 0., 2., 1.]).unwrap() - 0.5).abs() < 1e-6);
    }

    /// Vertex buffer sizes: counts whose byte size passes 32 bits (or the cap) and FVFs with
    /// no position are refused, never wrapped to a small buffer.
    #[test]
    fn vertex_buffer_sizes() {
        let tl = FVF_XYZRHW | FVF_DIFFUSE | FVF_SPECULAR | 0x100; // 32 bytes
        assert_eq!(vb_size(tl, 3), Some(96));
        assert_eq!(vb_size(tl, 0), Some(0));
        assert_eq!(vb_size(tl, 0x0800_0000), None, "4 GiB wraps to 0 in u32");
        assert_eq!(vb_size(tl, 0x1000_0001), None);
        assert_eq!(vb_size(tl, u32::MAX), None);
        assert_eq!(vb_size(tl, MAX_VB / 32), Some(MAX_VB));
        assert_eq!(vb_size(tl, MAX_VB / 32 + 1), None);
        assert_eq!(vb_size(FVF_DIFFUSE, 3), Some(48), "no position: sized as XYZ");
        for fvf in (2..0x1000u32).step_by(2).filter(|f| f & 0xe != 0) {
            for n in [1u32, 7, 0x0010_0000, 0x7fff_ffff, u32::MAX] {
                let want = Layout::of(fvf).size as u64 * n as u64;
                assert_eq!(vb_size(fvf, n), (want <= MAX_VB as u64).then_some(want as u32), "{fvf:#x} x {n}");
            }
        }
    }

    /// Light indices are the game's: any u32 is kept without room for the ones below it, and
    /// only lights set or enabled exist.
    #[test]
    fn sparse_lights() {
        let mut d = Dev::new(0, 0);
        let l = [7u32; 26];
        for i in [0u32, 0xffff_fffe, 100_000] {
            assert_eq!(d.apply(&Ch::Light(i, l)), 0);
        }
        assert_eq!(d.apply(&Ch::LightOn(0x7fff_fff0, true)), 0);
        assert_eq!(d.lights.len(), 4);
        assert_eq!(d.lights[&0xffff_fffe].0, l);
        assert!(!d.lights[&100_000].1);
        assert!(d.lights[&0x7fff_fff0].1);
        assert_eq!(d.lights[&0x7fff_fff0].0[0], 3, "a default directional light");
        assert!(!d.lights.contains_key(&3));
        // the whole state round-trips through a state block
        let mut e = Dev::new(0, 0);
        for ch in d.all() {
            let _ = e.apply(&ch);
        }
        assert_eq!(e.lights, d.lights);
    }

    /// Raster's texel copy (copy_plan) against the fragment pipeline it stands for
    mod texcopy {
        use super::super::*;

        // ----------------------------------------------------------------------------------------
        // Raster's texel copy (copy_plan) against the fragment pipeline it stands for

        const XRGB: [u32; 4] = [0xff_0000, 0xff00, 0xff, 0];
        const ARGB: [u32; 4] = [0xff_0000, 0xff00, 0xff, 0xff00_0000];
        const RGB565: [u32; 4] = [0xf800, 0x7e0, 0x1f, 0];
        const ARGB1555: [u32; 4] = [0x7c00, 0x3e0, 0x1f, 0x8000];
        const FMTS: [[u32; 4]; 4] = [XRGB, ARGB, RGB565, ARGB1555];

        fn fmt_of(m: [u32; 4]) -> Fmt {
            Fmt { ch: m.map(|m| if m == 0 { (0, 0) } else { (m.trailing_zeros(), m >> m.trailing_zeros()) }) }
        }

        struct Pic {
            px: Vec<u8>,
            w: i32,
            h: i32,
            bpp: usize,
            fmt: Fmt,
        }

        impl Pic {
            fn new(m: [u32; 4], w: i32, h: i32, seed: u32) -> Pic {
                let bpp = if m[0] > 0xffff { 4 } else { 2 };
                let mut x = seed | 1;
                let px = (0..w as usize * h as usize * bpp).map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; x as u8 }).collect();
                Pic { px, w, h, bpp, fmt: fmt_of(m) }
            }
            fn at(&self, x: i32, y: i32) -> u32 {
                read_px(self.px[(y * self.w) as usize * self.bpp + x as usize * self.bpp..].as_ptr(), self.bpp as u32)
            }
            fn tex(&self) -> Tex {
                Tex { bits: self.px.as_ptr(), w: self.w, h: self.h, pitch: self.w as usize * self.bpp, bpp: self.bpp, fmt: self.fmt, pal: None, key: None }
            }
        }

        struct Setup {
            cop: u32,
            ca: [u32; 2],
            aop: u32,
            aa: [u32; 2],
            linear: bool,
            addr: u32,
            tfactor: C4,
        }

        /// KiriKiri 2's present (apt245, apt247): MODULATE(TEXTURE, CURRENT: the diffuse in the
        /// first stage), alpha SELECTARG1(TEXTURE), bilinear, clamp
        const KIRIKIRI: Setup = Setup { cop: 4, ca: [2, 1], aop: 2, aa: [2, 1], linear: true, addr: 3, tfactor: [1.; 4] };

        fn raster(rt: &mut Pic, tex: &Pic, s: &Setup, copy: bool) -> Raster {
            Raster {
                rt: rt.px.as_mut_ptr(),
                pitch: rt.w as usize * rt.bpp,
                bpp: rt.bpp,
                fmt: rt.fmt,
                z: None,
                clip: [0, 0, rt.w, rt.h],
                ztest: None,
                zwrite: false,
                blend: None,
                atest: None,
                ck: false,
                flat: false,
                spec: false,
                fog: None,
                cull: 1,
                stages: vec![Stage { cop: s.cop, ca: s.ca, aop: s.aop, aa: s.aa, tex: Some(tex.tex()), tci: 0, addr: [s.addr; 2], border: [0.; 4], linear: s.linear }],
                tfactor: s.tfactor,
                ntc: 1,
                copy,
            }
        }

        fn vtx(x: f32, y: f32, rhw: f32, u: f32, v: f32, diff: C4) -> ([f32; 4], Vtx) {
            let mut tex = [[0.; 2]; 8];
            tex[0] = [u, v];
            ([x, y, 0.5, rhw], Vtx { clip: [x, y, 0.5, 1. / rhw], screen: true, diff, spec: [0.; 4], tex })
        }

        /// A triangle strip of four vertices, as draw() splits it.
        fn strip(r: &Raster, q: &[([f32; 4], Vtx); 4]) {
            r.tri([(&q[0].0, &q[0].1), (&q[1].0, &q[1].1), (&q[2].0, &q[2].1)]);
            r.tri([(&q[2].0, &q[2].1), (&q[1].0, &q[1].1), (&q[3].0, &q[3].1)]);
        }

        /// The quad from (x0, y0) to (x1, y1) (pixel edges) showing texels from (tx, ty) on, one to a
        /// pixel, `sub` texels further.
        fn aligned(tex: &Pic, x0: i32, y0: i32, x1: i32, y1: i32, tx: i32, ty: i32, sub: f32, rhw: [f32; 4], diff: C4) -> [([f32; 4], Vtx); 4] {
            let (tw, th) = (tex.w as f32, tex.h as f32);
            let u = |x: i32| (x - x0 + tx) as f32 / tw + sub / tw;
            let v = |y: i32| (y - y0 + ty) as f32 / th + sub / th;
            let p = |x: i32| x as f32 - 0.5;
            [
                vtx(p(x0), p(y0), rhw[0], u(x0), v(y0), diff),
                vtx(p(x1), p(y0), rhw[1], u(x1), v(y0), diff),
                vtx(p(x0), p(y1), rhw[2], u(x0), v(y1), diff),
                vtx(p(x1), p(y1), rhw[3], u(x1), v(y1), diff),
            ]
        }

        /// Both ways of drawing `q` onto copies of `rt`: (with the copy, through the pipeline).
        fn both(rt: &Pic, tex: &Pic, s: &Setup, q: &[([f32; 4], Vtx); 4]) -> (Pic, Pic) {
            let run = |copy| {
                let mut out = Pic { px: rt.px.clone(), ..*rt };
                let r = raster(&mut out, tex, s, copy);
                strip(&r, q);
                out
            };
            (run(true), run(false))
        }

        /// Pixels differ by more than `tol` steps in a channel (or in bits no channel has).
        fn diff_px(a: &Pic, b: &Pic, tol: u32) -> Option<(i32, i32, u32, u32)> {
            let mask = (0..4).fold(0u32, |m, i| m | a.fmt.ch[i].1 << a.fmt.ch[i].0);
            for y in 0..a.h {
                for x in 0..a.w {
                    let (p, q) = (a.at(x, y), b.at(x, y));
                    let bad = (p & !mask) != (q & !mask)
                        || a.fmt.ch.iter().any(|&(sh, mx)| ((p >> sh & mx) as i64 - (q >> sh & mx) as i64).unsigned_abs() as u32 > tol);
                    if bad {
                        return Some((x, y, p, q));
                    }
                }
            }
            None
        }

        impl Clone for Pic {
            fn clone(&self) -> Pic {
                Pic { px: self.px.clone(), ..*self }
            }
        }

        /// KiriKiri 2 presents every frame by drawing its 1280x720 screen texture over the whole
        /// target (x - 0.5, u 0..1): the copy takes it, and every pixel is its texel (the target has
        /// no alpha), as the pipeline draws it.
        #[test]
        fn kirikiri_present_is_a_copy() {
            let tex = Pic::new(ARGB, 64, 48, 7);
            let rt = Pic::new(XRGB, 64, 48, 9);
            let q = aligned(&tex, 0, 0, 64, 48, 0, 0, 0., [1.; 4], [1.; 4]);
            let mut probe = rt.clone();
            let r = raster(&mut probe, &tex, &KIRIKIRI, true);
            assert!(r.copy_plan([(&q[0].0, &q[0].1), (&q[1].0, &q[1].1), (&q[2].0, &q[2].1)], [0, 0, 63, 47]).is_some());
            let (fast, slow) = both(&rt, &tex, &KIRIKIRI, &q);
            for y in 0..48 {
                for x in 0..64 {
                    assert_eq!(fast.at(x, y), tex.at(x, y) & 0xff_ffff, "({x}, {y})");
                }
            }
            assert_eq!(diff_px(&fast, &slow, 0), None);
        }

        /// What the copy must leave to the pipeline: half a texel off, another rhw at one vertex, a
        /// diffuse that is not white, blending, texels past the texture's edge, two stages.
        #[test]
        fn copy_declines_what_changes_texels() {
            let tex = Pic::new(ARGB, 32, 32, 3);
            let mut rt = Pic::new(XRGB, 32, 32, 5);
            let plan = |r: &Raster, q: &[([f32; 4], Vtx); 4], bb: [i32; 4]| r.copy_plan([(&q[0].0, &q[0].1), (&q[1].0, &q[1].1), (&q[2].0, &q[2].1)], bb).is_some();
            let r = raster(&mut rt, &tex, &KIRIKIRI, true);
            assert!(plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
            assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0.5, [1.; 4], [1.; 4]), [4, 4, 19, 19]));
            assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1., 0.5, 1., 1.], [1.; 4]), [4, 4, 19, 19]));
            assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1., 0.5, 1., 1.]), [4, 4, 19, 19]));
            assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 20, 0, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
            let mut r = raster(&mut rt, &tex, &KIRIKIRI, true);
            r.blend = Some((5, 6));
            assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
            let mut r = raster(&mut rt, &tex, &KIRIKIRI, true);
            r.stages.push(r.stages[0].clone());
            assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
            // (DIFFUSE as the argument is the same, white)
            let r = raster(&mut rt, &tex, &Setup { ca: [0, 2], ..KIRIKIRI }, true);
            assert!(plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
            // (a texture factor of 1 is no change; another is, and so is a complemented texture)
        let half = Setup { ca: [2, 3], tfactor: [0.5, 1., 1., 1.], ..KIRIKIRI };
        let r = raster(&mut rt, &tex, &Setup { ca: [2, 3], ..KIRIKIRI }, true);
        assert!(plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
        let r = raster(&mut rt, &tex, &half, true);
        assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
        let r = raster(&mut rt, &tex, &Setup { cop: 2, ca: [0x12, 0], ..KIRIKIRI }, true);
        assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
        // (a texel offset within 1/4096 of a whole one is that one; 1/2048 is not)
        let r = raster(&mut rt, &tex, &KIRIKIRI, true);
        assert!(plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 1. / 8192., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
        assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 1. / 1024., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
        // (the target's alpha comes from the texture: ALPHAOP disabled gives the diffuse's)
            let mut rt_a = Pic::new(ARGB, 32, 32, 5);
            let r = raster(&mut rt_a, &tex, &Setup { aop: 1, ..KIRIKIRI }, true);
            assert!(!plan(&r, &aligned(&tex, 4, 4, 20, 20, 2, 3, 0., [1.; 4], [1.; 4]), [4, 4, 19, 19]));
        }

        mod copy_props {
            use super::*;
            use proptest::prelude::*;

            proptest! {
                #![proptest_config(ProptestConfig::with_cases(2048))]
                /// Whatever the formats, rectangle, texel offset (whole or not), rhw, diffuse,
                /// texture factor, stage operations, filter and addressing, drawing with the copy
                /// gives what the fragment pipeline gives: the same bits with point sampling, and
                /// with bilinear filtering at most one step off a channel (the pipeline's weights
                /// at a texel center are 1 - e and e for a rounding e of the interpolation).
                #[test]
                fn copy_draws_what_the_pipeline_draws(
                    rf in 0usize..4, tf in 0usize..4, rw in 1i32..24, rh in 1i32..24,
                    rect in (0i32..24, 0i32..24, 0i32..24, 0i32..24), tx in -2i32..6, ty in -2i32..6,
                    extra in (-2i32..5, -2i32..5),
                    sub in prop_oneof![4 => Just(0f32), 1 => Just(0.5f32), 1 => Just(-0.5f32), 1 => Just(0.25f32), 1 => Just(1. / 2048.), 1 => -1f32..1f32],
                    rhw in prop_oneof![2 => Just([1f32; 4]), 1 => Just([0.5f32; 4]), 1 => proptest::array::uniform4(0.25f32..2.)],
                    diff in prop_oneof![2 => Just([1f32; 4]), 1 => proptest::array::uniform4(0f32..=1.)],
                    tfactor in prop_oneof![Just([1f32; 4]), proptest::array::uniform4(0f32..=1.)],
                    cop in prop_oneof![1 => Just(2u32), 1 => Just(3), 2 => Just(4), 1 => Just(7)],
                    ca in (prop_oneof![3 => Just(2u32), 1 => Just(0), 1 => Just(3), 1 => Just(1), 1 => Just(0x12)], prop_oneof![1 => Just(2u32), 2 => Just(0), 1 => Just(3), 1 => Just(1)]),
                    aop in prop_oneof![1 => Just(1u32), 3 => Just(2), 1 => Just(3), 1 => Just(4)],
                    aa in (prop_oneof![3 => Just(2u32), 1 => Just(0), 1 => Just(3)], prop_oneof![Just(2u32), Just(0), Just(1)]),
                    linear in any::<bool>(), addr in 1u32..5, seed in any::<u32>(),
                ) {
                    let (x0, x1) = (rect.0.min(rect.2).min(rw), rect.0.max(rect.2).min(rw));
                    let (y0, y1) = (rect.1.min(rect.3).min(rh), rect.1.max(rect.3).min(rh));
                    // (mostly a texture the rectangle's texels fit in, sometimes one they run past)
                    let tw = (x1 - x0 + tx.max(0) + extra.0).max(1);
                    let th = (y1 - y0 + ty.max(0) + extra.1).max(1);
                    let tex = Pic::new(FMTS[tf], tw, th, seed);
                    let rt = Pic::new(FMTS[rf], rw, rh, seed ^ 0x9e37_79b9);
                    let s = Setup { cop, ca: [ca.0, ca.1], aop, aa: [aa.0, aa.1], linear, addr, tfactor };
                    let q = aligned(&tex, x0, y0, x1, y1, tx, ty, sub, rhw, diff);
                    let (fast, slow) = both(&rt, &tex, &s, &q);
                    prop_assert_eq!(diff_px(&fast, &slow, if linear { 1 } else { 0 }), None);
                }
            }
        }
    }
}
