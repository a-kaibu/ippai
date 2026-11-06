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
    rt: u32,
    /// x, y, width, height
    vp: [i32; 4],
    zr: (f32, f32),
    /// world, view, projection
    xf: [[f32; 16]; 3],
    rs: Vec<u32>,
    tss: [[u32; 32]; 8],
    tex: [u32; 8],
    /// diffuse, ambient, specular, emissive (RGBA each), power
    mat: [f32; 17],
    lights: Vec<([u32; 26], bool)>,
    rec: Option<Vec<Ch>>,
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
    Light(usize, [u32; 26]),
    LightOn(usize, bool),
}

const IDENT: [f32; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

impl Dev {
    fn new(rt: u32) -> Dev {
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
        Dev { rt, vp: [0, 0, w, h], zr: (0., 1.), xf: [IDENT; 3], rs, tss, tex: [0; 8], mat, lights: Vec::new(), rec: None }
    }

    fn apply(&mut self, ch: &Ch) {
        match ch {
            Ch::Rs(k, v) => self.rs[*k] = *v,
            Ch::Tss(s, k, v) => {
                self.tss[*s][*k] = *v;
                if *k == TSS_ADDRESS {
                    self.tss[*s][TSS_ADDRESSU] = *v;
                    self.tss[*s][TSS_ADDRESSV] = *v;
                }
            }
            Ch::Tex(s, t) => self.tex[*s] = *t,
            Ch::Xf(t, m) => self.xf[*t] = *m,
            Ch::Vp(v, z) => {
                self.vp = *v;
                self.zr = *z;
            }
            Ch::Mat(m) => self.mat = *m,
            Ch::Light(i, l) => {
                if self.lights.len() <= *i {
                    self.lights.resize(*i + 1, ([0; 26], false));
                }
                self.lights[*i].0 = *l;
            }
            Ch::LightOn(i, on) => {
                if self.lights.len() <= *i {
                    let mut l = [0u32; 26];
                    l[0] = 3;
                    l[1..4].copy_from_slice(&[1f32.to_bits(); 3]);
                    l[17] = 1f32.to_bits(); // direction (0, 0, 1)
                    self.lights.resize(*i + 1, (l, false));
                }
                self.lights[*i].1 = *on;
            }
        }
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
        for (i, l) in self.lights.iter().enumerate() {
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
            Ch::Light(i, l) => Ch::Light(*i, self.lights.get(*i).map_or(*l, |x| x.0)),
            Ch::LightOn(i, b) => Ch::LightOn(*i, self.lights.get(*i).map_or(*b, |x| x.1)),
        }
    }
}

struct D3d {
    devs: HashMap<u32, Dev>,
    /// guest vertex buffer block -> (memory, fvf, count)
    vbs: HashMap<u32, (u32, u32, u32)>,
    blocks: HashMap<u32, Vec<Ch>>,
    next: u32,
}

static D: Mutex<Option<D3d>> = Mutex::new(None);

fn d3d<R>(f: impl FnOnce(&mut D3d) -> R) -> R {
    let mut g = D.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| D3d { devs: HashMap::default(), vbs: HashMap::default(), blocks: HashMap::default(), next: 1 }))
}

/// Runs `f` on the device `c` is called on (None if it is gone).
fn dev<R>(c: &Ctx, f: impl FnOnce(&mut Dev) -> R) -> Option<R> {
    let id = r32(c.a(0) + 4);
    d3d(|d| d.devs.get_mut(&id).map(f))
}

/// A state change: recorded while a state block is being recorded, applied otherwise.
fn set(c: &Ctx, ch: Ch) -> u32 {
    dev(c, |d| match &mut d.rec {
        Some(r) => r.push(ch),
        None => d.apply(&ch),
    })
    .map_or(D3DERR_INVALIDCALL, |_| D3D_OK)
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

/// The Direct3D object (one, like the DirectDraw object).
pub(super) fn d3d_block() -> u32 {
    static B: OnceLock<u32> = OnceLock::new();
    *B.get_or_init(|| {
        let vt = vtable(
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
        );
        let b = mem::heap_alloc(8, true);
        w32(b, vt);
        b
    })
}

fn Q_QueryInterface(c: &mut Ctx) -> u32 {
    match guid_first(c.a(1)) {
        0xf504_9e77 | 0 => {
            put32(c.a(2), d3d_block());
            dd_created()
        }
        0x15e6_5ec0 => {
            put32(c.a(2), dd_block(3));
            dd_created()
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

fn Q_EnumDevices(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(1), c.a(2));
    let m = mem::heap_alloc(236 + 256, true);
    for (i, (desc, name, ..)) in DEVICES.iter().enumerate() {
        write_caps(m, i);
        mem::write(m + 236, desc.as_bytes());
        mem::w8(m + 236 + desc.len() as u32, 0);
        mem::write(m + 236 + 128, name.as_bytes());
        mem::w8(m + 236 + 128 + name.len() as u32, 0);
        if c.call(cb, &[m + 236, m + 236 + 128, m, ctx]) == 0 {
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
                (ok, 4),
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
    let (target, out) = (c.a(2), c.a(3));
    let Some((rt, _)) = sid(target) else {
        put32(out, 0);
        return D3DERR_INVALIDCALL;
    };
    let id = d3d(|d| {
        let id = d.next;
        d.next += 1;
        d.devs.insert(id, Dev::new(rt));
        id
    });
    let b = mem::heap_alloc(12, true);
    w32(b, dev_vtbl());
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
    let (fvf, n) = (r32(desc + 8), r32(desc + 12));
    let size = Layout::of(fvf).size * n;
    let m = mem::heap_alloc(size.max(4), true);
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
fn B_Lock(c: &mut Ctx) -> u32 {
    let Some((m, fvf, n)) = vb(c) else { return D3DERR_INVALIDCALL };
    put32(c.a(2), m);
    put32(c.a(3), Layout::of(fvf).size * n);
    D3D_OK
}
fn B_GetVertexBufferDesc(c: &mut Ctx) -> u32 {
    let Some((_, fvf, n)) = vb(c) else { return D3DERR_INVALIDCALL };
    let p = c.a(1);
    w32(p, 16);
    w32(p + 4, r32(c.a(0) + 4));
    w32(p + 8, fvf);
    w32(p + 12, n);
    D3D_OK
}

/// Transforms (and lights) source vertices into this XYZRHW buffer.
fn process(c: &mut Ctx, src: Option<Src>, src_index: u32) -> u32 {
    let (dst_index, count, devb) = (c.a(2), c.a(3), c.a(6));
    let Some((dm, dfvf, dn)) = vb(c) else { return D3DERR_INVALIDCALL };
    let Some(src) = src else { return D3DERR_INVALIDCALL };
    let Some(d) = d3d(|d| d.devs.get(&r32(devb + 4)).cloned()) else { return D3DERR_INVALIDCALL };
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
    let src = d3d(|d| d.vbs.get(&srcb).copied()).map(|(m, fvf, _)| Src::packed(fvf, m));
    process(c, src, si)
}
fn B_ProcessVerticesStrided(c: &mut Ctx) -> u32 {
    let src = Src::strided(c.a(5), c.a(4));
    process(c, Some(src), 0)
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
        d3d(|d| d.devs.remove(&r32(b + 4)));
    }
    n
}
fn V_GetCaps(c: &mut Ctx) -> u32 {
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
fn V_EndScene(c: &mut Ctx) -> u32 {
    if let Some(rt) = dev(c, |d| d.rt) {
        if let Some(s) = surf(rt) {
            present_if_primary(c, rt, Rect { l: 0, t: 0, r: s.w, b: s.h });
        }
    }
    D3D_OK
}
fn V_GetDirect3D(c: &mut Ctx) -> u32 {
    put32(c.a(1), d3d_block());
    dd_created()
}
fn V_SetRenderTarget(c: &mut Ctx) -> u32 {
    let Some((rt, _)) = sid(c.a(1)) else { return D3DERR_INVALIDCALL };
    let (w, h) = surf(rt).map_or((0, 0), |s| (s.w, s.h));
    dev(c, |d| {
        d.rt = rt;
        d.vp = [0, 0, w, h];
    });
    D3D_OK
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
    set(c, Ch::Light(c.a(1) as usize, std::array::from_fn(|i| r32(p + 4 * i as u32))))
}
fn V_GetLight(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    let Some(l) = dev(c, |d| d.lights.get(i).map(|x| x.0)).flatten() else { return D3DERR_INVALIDCALL };
    for (k, v) in l.iter().enumerate() {
        w32(c.a(2) + 4 * k as u32, *v);
    }
    D3D_OK
}
fn V_LightEnable(c: &mut Ctx) -> u32 {
    set(c, Ch::LightOn(c.a(1) as usize, c.a(2) != 0))
}
fn V_GetLightEnable(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    let Some(on) = dev(c, |d| d.lights.get(i).map(|x| x.1)).flatten() else { return D3DERR_INVALIDCALL };
    put32(c.a(2), on as u32);
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
    d3d(|d| {
        let h = d.next;
        d.next += 1;
        d.blocks.insert(h, v);
        h
    })
}
fn V_EndStateBlock(c: &mut Ctx) -> u32 {
    let Some(v) = dev(c, |d| d.rec.take()).flatten() else { return D3DERR_INVALIDCALL };
    put32(c.a(1), new_block(v));
    D3D_OK
}
fn V_CreateStateBlock(c: &mut Ctx) -> u32 {
    let Some(all) = dev(c, |d| d.all()) else { return D3DERR_INVALIDCALL };
    put32(c.a(2), new_block(all));
    D3D_OK
}
fn V_ApplyStateBlock(c: &mut Ctx) -> u32 {
    let Some(v) = d3d(|d| d.blocks.get(&c.a(1)).cloned()) else { return D3DERR_INVALIDCALL };
    dev(c, |d| v.iter().for_each(|ch| d.apply(ch)));
    D3D_OK
}
fn V_CaptureStateBlock(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    let Some(v) = d3d(|d| d.blocks.get(&h).cloned()) else { return D3DERR_INVALIDCALL };
    let Some(nv) = dev(c, |d| v.iter().map(|ch| d.capture(ch)).collect::<Vec<_>>()) else { return D3DERR_INVALIDCALL };
    d3d(|d| d.blocks.insert(h, nv));
    D3D_OK
}
fn V_DeleteStateBlock(c: &mut Ctx) -> u32 {
    d3d(|d| d.blocks.remove(&c.a(1)));
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

/// Load(dst, dst point, src, src rect, flags): a copy between textures of one format.
fn V_Load(c: &mut Ctx) -> u32 {
    let (Some((d, _)), Some((s, _))) = (sid(c.a(1)), sid(c.a(3))) else { return D3DERR_INVALIDCALL };
    let (Some(ds), Some(ss)) = (surf(d), surf(s)) else { return D3DERR_INVALIDCALL };
    let sr = if c.a(4) == 0 { Rect { l: 0, t: 0, r: ss.w, b: ss.h } } else { Rect::read(c.a(4)) };
    let (x, y) = if c.a(2) == 0 { (sr.l, sr.t) } else { (r32(c.a(2)) as i32, r32(c.a(2) + 4) as i32) };
    let dr = Rect { l: x, t: y, r: x + sr.r - sr.l, b: y + sr.b - sr.t };
    if ds.bpp == ss.bpp && rect_ok(dr, ds.w, ds.h) && rect_ok(sr, ss.w, ss.h) {
        blit(&ds, dr, &ss, sr, None, (false, false));
    }
    if ss.ck_src.is_some() {
        with(|st| st.surfs.get_mut(&d).map(|x| x.ck_src = ss.ck_src));
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
        Vtx { clip, screen: false, diff, spec, tex }
    }
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
    world: [f32; 16],
    vp: [f32; 4],
    zr: (f32, f32),
}

impl Xform {
    fn new(d: &Dev) -> Xform {
        let world = d.xf[0];
        Xform {
            wvp: mul(&mul(&world, &d.xf[1]), &d.xf[2]),
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
        for (l, on) in &d.lights {
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
    cull: u32,
    stages: Vec<Stage>,
    tfactor: C4,
    ntc: usize,
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
        if rt.bpp < 16 {
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
            cull: d.rs[RS_CULLMODE],
            stages,
            tfactor: unpack_argb(d.rs[RS_TEXTUREFACTOR]),
            ntc,
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
    for plane in [|v: &Vtx| v.clip[2], |v: &Vtx| v.clip[3] - 1e-6] {
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
    let Some(d) = dev(c, |d| d.clone()) else { return D3DERR_INVALIDCALL };
    let Some(rt) = surf(d.rt) else { return D3D_OK };
    let vp = Rect { l: d.vp[0], t: d.vp[1], r: d.vp[0] + d.vp[2], b: d.vp[1] + d.vp[3] };
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

#[cfg(test)]
mod tests {
    use super::*;

    const ARGB: Fmt = Fmt { ch: [(16, 255), (8, 255), (0, 255), (24, 255)] };

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
            cull: 1,
            stages: Vec::new(),
            tfactor: [1.; 4],
            ntc: 1,
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
}
