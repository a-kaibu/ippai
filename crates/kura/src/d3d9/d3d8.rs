//! Direct3D 8 on top of the Direct3D 9 device.
//!
//! d3d8.dll (dlls/vnd3d8) has the same stubs as d3d9.dll, with interfaces numbered 16 + the
//! Direct3D 9 one. Its objects are the Direct3D 9 ones (`D3d::v8` gives them the Direct3D 8
//! vtables); each Direct3D 8 method becomes the matching Direct3D 9 call, with the arguments and
//! structures that differ converted. Vertex shaders and state blocks are handles in Direct3D 8:
//! they map to Direct3D 9 objects here (a shader's declaration to a vertex declaration, the
//! `dcl`s Direct3D 9 needs put in its code).

use super::device;
use super::format as fmt;
use super::*;

/// host ids outside the interfaces (d3d8.dll's DllMain and Direct3DCreate8)
pub const INIT: u32 = 0xffff_0001;
pub const CREATE: u32 = 0xffff_0002;

/// handles above this are vertex shaders, the rest FVFs
const VS_FIXED_MAX: u32 = 0xf000_0000;

#[derive(Default)]
pub struct State {
    vs: Vec<Option<Vs>>,
    ps: Vec<Option<u32>>,
    sb: Vec<u32>,
    base_vertex: u32,
    cur_vs: u32,
    cur_ps: u32,
}

struct Vs {
    decl: u32,
    vs: u32,
    decl_raw: Vec<u32>,
    func: Vec<u32>,
    consts: Vec<(u32, [u32; 4])>,
}

unsafe fn rd(p: u32, i: usize) -> u32 {
    *((p as *const u32).add(i))
}

unsafe fn wr(p: u32, i: usize, v: u32) {
    if p != 0 {
        *((p as *mut u32).add(i)) = v;
    }
}

/// A Direct3D 9 call with these arguments (`this` first).
unsafe fn c9(d: &mut D3d, iface: u32, m: u32, this: u32, args: &[u32], after: &mut After) -> u32 {
    device::call(d, iface, m, this, args.as_ptr(), after)
}

/// The Direct3D 8 present parameters (13 dwords) as Direct3D 9 ones (14).
unsafe fn pp9(p8: u32) -> [u32; 14] {
    let mut p = [0u32; 14];
    for i in 0..13 {
        p[if i < 5 { i } else { i + 1 }] = rd(p8, i);
    }
    if p[6] == 4 {
        p[6] = 3; // SWAPEFFECT_COPY_VSYNC: COPY
    }
    p
}

unsafe fn pp8(p8: u32, p: &[u32; 14]) {
    for i in 0..13 {
        let v = p[if i < 5 { i } else { i + 1 }];
        if i != 5 {
            wr(p8, i, v);
        }
    }
}

/// D3DCAPS2_CANRENDERWINDOWED: Direct3D 8 only (d3d9caps.h no longer has it). Windows' d3d8
/// sets it on every device; the DirectX 8 SDK's D3DApp framework (凌●学園 体験版) counts a
/// device as usable only with it, and with none says the video driver lacks DirectX 8.
pub(crate) const CAPS2_CANRENDERWINDOWED: u32 = 0x0008_0000;

/// D3DCAPS9 to D3DCAPS8 (its first 53 dwords), with shader versions Direct3D 8 has and
/// Direct3D 8's CANRENDERWINDOWED.
pub(crate) fn caps8_of(c: &[u32; 76]) -> [u32; 53] {
    let mut o = [0u32; 53];
    o.copy_from_slice(&c[..53]);
    o[3] |= CAPS2_CANRENDERWINDOWED;
    o[49] = 0xfffe_0101; // vs 1.1
    o[50] = 96;
    o[51] = 0xffff_0104; // ps 1.4
    o
}

unsafe fn caps8(c: &mut [u32; 76], out: u32) {
    let o = caps8_of(c);
    std::ptr::copy_nonoverlapping(o.as_ptr(), out as *mut u32, 53);
}

/// D3DSURFACE_DESC9 to 8 (Size instead of MultiSampleQuality, before MultiSampleType)
unsafe fn surface_desc8(d9: &[u32; 8], out: u32) {
    let (_, rows) = fmt::pitch_rows(d9[0], d9[6], d9[7]);
    let size = fmt::layout(d9[0]).map_or(0, |(bpb, bs)| d9[6].div_ceil(bs) * bpb * rows);
    for (i, v) in [d9[0], d9[1], d9[2], d9[3], size, d9[4], d9[6], d9[7]].into_iter().enumerate() {
        wr(out, i, v);
    }
}

/// D3DVOLUME_DESC8 from D3DVOLUME_DESC: Size after Pool.
unsafe fn volume_desc8(d9: &[u32; 8], out: u32) {
    let (_, rows) = fmt::pitch_rows(d9[0], d9[4], d9[5]);
    let size = fmt::layout(d9[0]).map_or(0, |(bpb, bs)| d9[4].div_ceil(bs) * bpb * rows * d9[6]);
    for (i, v) in [d9[0], d9[1], d9[2], d9[3], size, d9[4], d9[5], d9[6]].into_iter().enumerate() {
        wr(out, i, v);
    }
}

// Direct3D 8 IIDs (first dword)
const IID_IUNKNOWN: u32 = 0;
const IID_D3D8: u32 = 0x1dd9e8da;
const IID_DEVICE8: u32 = 0x7385e5df;
const IID_SURFACE8: u32 = 0xb96eebca;
const IID_TEXTURE8: u32 = 0xe4cdd575;
const IID_RESOURCE8: u32 = 0x1b36bb7b;
const IID_BASETEX8: u32 = 0xb4211cfa;
const IID_VB8: u32 = 0x8aeeeac7;
const IID_IB8: u32 = 0x0e689c9a;
const IID_SWAPCHAIN8: u32 = 0x928c088b;
const IID_CUBE8: u32 = 0x3ee5b968;
const IID_VOLUMETEX8: u32 = 0x4b8aaafa;
const IID_VOLUME8: u32 = 0xbd7349f5;

unsafe fn query_interface(d: &mut D3d, id: u32, iface: u32, riid: u32, out: u32) -> u32 {
    if out == 0 || riid == 0 {
        return D3DERR_INVALIDCALL;
    }
    let ok = match rd(riid, 0) {
        IID_IUNKNOWN => true,
        IID_D3D8 => iface == IF_D3D9,
        IID_DEVICE8 => iface == IF_DEVICE,
        IID_RESOURCE8 => matches!(iface, IF_TEXTURE | IF_CUBE | IF_VOLUMETEX | IF_VB | IF_IB),
        IID_BASETEX8 => matches!(iface, IF_TEXTURE | IF_CUBE | IF_VOLUMETEX),
        IID_TEXTURE8 => iface == IF_TEXTURE,
        IID_CUBE8 => iface == IF_CUBE,
        IID_VOLUMETEX8 => iface == IF_VOLUMETEX,
        IID_SURFACE8 => iface == IF_SURFACE,
        IID_VOLUME8 => iface == IF_VOLUME,
        IID_VB8 => iface == IF_VB,
        IID_IB8 => iface == IF_IB,
        IID_SWAPCHAIN8 => iface == IF_SWAPCHAIN,
        _ => false,
    };
    if !ok {
        wr(out, 0, 0);
        return E_NOINTERFACE;
    }
    d.add_ref(id);
    wr(out, 0, d.addr_of(id));
    S_OK
}

/// A Direct3D 8 method of interface `iface` (the Direct3D 9 number).
pub unsafe fn call(d: &mut D3d, iface: u32, m: u32, this: u32, args: *const u32, after: &mut After) -> u32 {
    let a = |i: usize| *args.add(i);
    let me = a(0);
    match (iface, m) {
        (_, 0) => query_interface(d, this, iface, a(1), a(2)),
        (_, 1) => d.add_ref(this),
        (_, 2) => d.release(this),
        (IF_D3D9, _) => d3d8_call(d, m, this, &a, after),
        (IF_DEVICE, _) => device_call(d, m, this, &a, after),
        (IF_SWAPCHAIN, 3) => c9(d, IF_SWAPCHAIN, 3, this, &[me, a(1), a(2), a(3), a(4), 0], after),
        (IF_SWAPCHAIN, 4) => c9(d, IF_SWAPCHAIN, 5, this, &[me, a(1), a(2), a(3)], after),
        (IF_SURFACE, 7..=10) => {
            if m == 8 {
                let mut d9 = [0u32; 8];
                let r = c9(d, IF_SURFACE, 12, this, &[me, d9.as_mut_ptr() as u32], after);
                if r == S_OK {
                    surface_desc8(&d9, a(1));
                }
                r
            } else {
                device::call(d, IF_SURFACE, m + 4, this, args, after)
            }
        }
        (IF_VOLUME, 8) => {
            let mut d9 = [0u32; 8];
            let r = c9(d, IF_VOLUME, 8, this, &[me, d9.as_mut_ptr() as u32], after);
            if r == S_OK {
                volume_desc8(&d9, a(1));
            }
            r
        }
        (IF_TEXTURE | IF_CUBE | IF_VOLUMETEX, 14..=18) => {
            if m == 14 {
                let mut d9 = [0u32; 8];
                let r = c9(d, iface, 17, this, &[me, a(1), d9.as_mut_ptr() as u32], after);
                if r == S_OK {
                    if iface == IF_VOLUMETEX {
                        volume_desc8(&d9, a(2));
                    } else {
                        surface_desc8(&d9, a(2));
                    }
                }
                r
            } else {
                device::call(d, iface, m + 3, this, args, after)
            }
        }
        _ => device::call(d, iface, m, this, args, after),
    }
}

unsafe fn d3d8_call(d: &mut D3d, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let me = a(0);
    match m {
        5 => {
            // GetAdapterIdentifier: the Direct3D 9 one without DeviceName[32]
            if a(3) == 0 {
                return D3DERR_INVALIDCALL;
            }
            let mut id9 = [0u8; 1100];
            let r = c9(d, IF_D3D9, 5, this, &[me, a(1), a(2), id9.as_mut_ptr() as u32], after);
            let out = a(3) as *mut u8;
            std::ptr::copy_nonoverlapping(id9.as_ptr(), out, 1024);
            std::ptr::copy_nonoverlapping(id9.as_ptr().add(1056), out.add(1024), 44);
            r
        }
        6 => c9(d, IF_D3D9, 6, this, &[me, a(1), fmt::X8R8G8B8], after),
        7 => c9(d, IF_D3D9, 7, this, &[me, a(1), fmt::X8R8G8B8, a(2), a(3)], after),
        11 => c9(d, IF_D3D9, 11, this, &[me, a(1), a(2), a(3), a(4), a(5), 0], after),
        13 => {
            let mut c = [0u32; 76];
            let r = c9(d, IF_D3D9, 14, this, &[me, a(1), a(2), c.as_mut_ptr() as u32], after);
            caps8(&mut c, a(3));
            r
        }
        14 => c9(d, IF_D3D9, 15, this, &[me, a(1)], after),
        15 => {
            // CreateDevice(adapter, type, window, flags, pp, out)
            let mut p = pp9(a(5));
            let r = c9(d, IF_D3D9, 16, this, &[me, a(1), a(2), a(3), a(4), p.as_mut_ptr() as u32, a(6)], after);
            pp8(a(5), &p);
            d.d8 = State::default();
            // (Direct3D 8's own states: ZBIAS and the rest 0, PATCHSEGMENTS 1.0,
            // SOFTWAREVERTEXPROCESSING as created; POINTSIZE_MIN 0, POINTSIZE_MAX 8192.0 on
            // either device: tests/winref/golden/d3d8state-32)
            if let Some(dev) = d.dev.as_mut().filter(|_| r == S_OK) {
                for k in [10, 30, 40, 47, 155] {
                    dev.st.rs[k] = 0;
                }
                dev.st.rs[166] = 8192f32.to_bits();
                dev.st.rs[153] = (a(4) & 0x20 != 0) as u32;
                dev.st.rs[164] = 1f32.to_bits();
            }
            r
        }
        _ => device::call(d, IF_D3D9, m, this, [me, a(1), a(2), a(3), a(4), a(5), a(6)].as_ptr(), after),
    }
}

/// Direct3D 8 texture stage states that are sampler states in Direct3D 9
fn sampler_state(tss: u32) -> Option<u32> {
    Some(match tss {
        13 => 1, // ADDRESSU
        14 => 2,
        25 => 3, // ADDRESSW
        15 => 4, // BORDERCOLOR
        16 => 5, // MAGFILTER
        17 => 6,
        18 => 7,
        19 => 8, // MIPMAPLODBIAS
        20 => 9,
        21 => 10, // MAXANISOTROPY
        _ => return None,
    })
}

/// Direct3D 8's fixed input registers (v0 position, ... v7-v14 texture coordinates) as usages
fn reg_usage(reg: u32) -> (u32, u32) {
    use dxwgsl::d3d9::*;
    match reg {
        0 => (USAGE_POSITION, 0),
        1 => (USAGE_BLENDWEIGHT, 0),
        2 => (USAGE_BLENDINDICES, 0),
        3 => (USAGE_NORMAL, 0),
        4 => (USAGE_PSIZE, 0),
        5 => (USAGE_COLOR, 0),
        6 => (USAGE_COLOR, 1),
        7..=14 => (USAGE_TEXCOORD, reg - 7),
        _ => (USAGE_TEXCOORD, 8 + (reg - 15)),
    }
}

/// A vertex shader declaration: Direct3D 9 vertex elements (8 bytes each, with the end),
/// the registers they feed, constants it loads and its tokens.
unsafe fn parse_decl(p: u32) -> Option<(Vec<u32>, Vec<u32>, Vec<(u32, [u32; 4])>, Vec<u32>)> {
    const SIZES: [u32; 8] = [4, 8, 12, 16, 4, 4, 4, 8];
    let (mut el, mut regs, mut consts, mut raw) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut stream, mut off) = (0u32, 0u32);
    let mut i = 0;
    loop {
        let t = rd(p, i);
        raw.push(t);
        i += 1;
        if t == 0xffff_ffff || i > 4096 {
            break;
        }
        match t >> 29 {
            1 => {
                // STREAM (not tessellator streams)
                if t & 0x1000_0000 == 0 {
                    stream = t & 0xf;
                    off = 0;
                }
            }
            2 => {
                if t & 0x1000_0000 != 0 {
                    // SKIP n dwords
                    off += ((t >> 16) & 0xf) * 4;
                } else {
                    let (ty, reg) = ((t >> 16) & 0xf, t & 0x1f);
                    let (usage, index) = reg_usage(reg);
                    el.push(stream | off << 16);
                    el.push(ty | usage << 16 | index << 24);
                    regs.push(reg);
                    off += *SIZES.get(ty as usize)?;
                }
            }
            4 => {
                // CONSTMEM: count registers from address
                let (n, addr) = ((t >> 25) & 0xf, t & 0x7f);
                for r in 0..n {
                    let v = [rd(p, i), rd(p, i + 1), rd(p, i + 2), rd(p, i + 3)];
                    raw.extend_from_slice(&v);
                    i += 4;
                    consts.push((addr + r, v));
                }
            }
            _ => {}
        }
    }
    el.extend_from_slice(&[0xff, 17 /* UNUSED */]);
    Some((el, regs, consts, raw))
}

/// A Direct3D 8 vertex shader's tokens (to its end), and the same with the `dcl`s of `regs`.
unsafe fn shader_code(p: u32, regs: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut code = vec![rd(p, 0)];
    let mut i = 1;
    loop {
        let t = rd(p, i);
        i += 1;
        code.push(t);
        if t == 0xffff || i > 1 << 20 {
            break;
        }
        let n = match t & 0xffff {
            0xfffe => (t >> 16) & 0x7fff, // comment
            0x51 => 5,                    // def: dst and 4 values
            _ => 0,
        } as usize;
        for k in 0..n {
            code.push(rd(p, i + k));
        }
        i += n;
    }
    let mut with = vec![code[0]];
    for &r in regs {
        let (usage, index) = reg_usage(r);
        with.extend_from_slice(&[0x1f, 0x8000_0000 | usage | index << 16, 0x900f_0000 | r]);
    }
    with.extend_from_slice(&code[1..]);
    (code, with)
}

unsafe fn device_call(d: &mut D3d, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let me = a(0);
    let dev = IF_DEVICE;
    // (methods of Direct3D 8's own, not only forwarded to Direct3D 9's through device::call: what
    // draws prepared may change, device::stale)
    if !matches!(m, 3 | 4 | 6 | 9..=12 | 15..=19 | 29 | 32..=52 | 58..=64 | 66..=73 | 79 | 80 | 83 | 91 | 92) {
        if let Some(dv) = d.dev.as_mut() {
            dv.stale_all();
        }
    }
    // the same method in Direct3D 9, arguments unchanged
    let same = |d: &mut D3d, m9: u32, after: &mut After| {
        let args: Vec<u32> = (0..10).map(a).collect();
        device::call(d, dev, m9, this, args.as_ptr(), after)
    };
    match m {
        3 | 4 | 6 | 9 | 10 | 11 | 12 => same(d, m, after),
        5 => S_OK, // ResourceManagerDiscardBytes
        7 => {
            let mut c = [0u32; 76];
            let r = c9(d, dev, 7, this, &[me, c.as_mut_ptr() as u32], after);
            caps8(&mut c, a(1));
            r
        }
        8 => c9(d, dev, 8, this, &[me, 0, a(1)], after),
        13 => {
            // CreateAdditionalSwapChain(params, out)
            if a(1) == 0 {
                return D3DERR_INVALIDCALL;
            }
            let mut p = pp9(a(1));
            let r = c9(d, dev, 13, this, &[me, p.as_mut_ptr() as u32, a(2)], after);
            if r == 0 {
                pp8(a(1), &p);
            }
            r
        }
        14 => {
            let mut p = pp9(a(1));
            let r = c9(d, dev, 16, this, &[me, p.as_mut_ptr() as u32], after);
            pp8(a(1), &p);
            d.d8.base_vertex = 0;
            r
        }
        15 => same(d, 17, after),
        16 => c9(d, dev, 18, this, &[me, 0, a(1), a(2), a(3)], after),
        17 => c9(d, dev, 19, this, &[me, 0, a(1)], after),
        18 => c9(d, dev, 21, this, &[me, 0, a(1), a(2)], after),
        19 => c9(d, dev, 22, this, &[me, 0, a(1)], after),
        // Create{Texture, VolumeTexture, CubeTexture, VertexBuffer, IndexBuffer}: no shared handle
        20..=24 => {
            let mut args: Vec<u32> = (0..10).map(a).collect();
            args[[8, 9, 7, 6, 6][m as usize - 20]] = 0;
            device::call(d, dev, m + 3, this, args.as_ptr(), after)
        }
        25 => c9(d, dev, 28, this, &[me, a(1), a(2), a(3), a(4), 0, a(5), a(6), 0], after),
        26 => c9(d, dev, 29, this, &[me, a(1), a(2), a(3), a(4), 0, 0, a(5), 0], after),
        // CreateImageSurface: a system memory surface
        27 => c9(d, dev, 36, this, &[me, a(1), a(2), a(3), 2, a(4), 0], after),
        28 => copy_rects(d, this, a(1), a(2), a(3), a(4), a(5), after),
        29 => same(d, 31, after),
        30 => D3DERR_INVALIDCALL, // GetFrontBuffer
        31 => {
            // SetRenderTarget(target or NULL: keep it, depth stencil or NULL: none)
            if a(1) != 0 {
                let r = c9(d, dev, 37, this, &[me, 0, a(1)], after);
                if r != S_OK {
                    return r;
                }
            }
            c9(d, dev, 39, this, &[me, a(2)], after)
        }
        32 => c9(d, dev, 38, this, &[me, 0, a(1)], after),
        33 => same(d, 40, after),
        51 => {
            // GetRenderState: Direct3D 8's states (LINEPATTERN, ZVISIBLE, EDGEANTIALIAS, ZBIAS,
            // SOFTWAREVERTEXPROCESSING, PATCHSEGMENTS are not Direct3D 9's); the unused and
            // legacy ones, Direct3D 9's own among them, read 0xbaadcafe (as Windows:
            // tests/winref/golden/d3d8state-32)
            let (st, out) = (a(1), a(2));
            let unset = matches!(st, 0 | 42 | 50 | 61..=63 | 96..=127 | 138 | 149 | 150 | 165 | 169 | 174..=255);
            if out == 0 || !(unset || device::rs_valid(st) || matches!(st, 10 | 30 | 153 | 164)) {
                return D3DERR_INVALIDCALL;
            }
            let Some(dev) = d.dev.as_ref() else { return D3DERR_INVALIDCALL };
            wr(out, 0, if unset { 0xbaad_cafe } else { dev.st.rs[st as usize] });
            S_OK
        }
        34..=50 => same(d, m + 7, after),
        52 => same(d, 60, after),
        53 => {
            let mut o = 0u32;
            let r = c9(d, dev, 61, this, &[me, &mut o as *mut u32 as u32], after);
            if r == S_OK {
                wr(a(1), 0, new_token(d, o));
            }
            r
        }
        54 | 55 => match token(d, a(1)) {
            Some(sb) => c9(d, IF_STATEBLOCK, if m == 54 { 5 } else { 4 }, sb, &[d.addr_of(sb)], after),
            None => D3DERR_INVALIDCALL,
        },
        56 => match token(d, a(1)) {
            Some(sb) => {
                d.d8.sb[a(1) as usize - 1] = 0;
                d.release(sb);
                S_OK
            }
            None => D3DERR_INVALIDCALL,
        },
        57 => {
            let mut o = 0u32;
            let r = c9(d, dev, 59, this, &[me, a(1), &mut o as *mut u32 as u32], after);
            if r == S_OK {
                wr(a(2), 0, new_token(d, o));
            }
            r
        }
        58 | 59 => same(d, m + 4, after),
        60 | 61 => same(d, m + 4, after),
        62 | 63 => {
            // Get/SetTextureStageState: some are sampler states in Direct3D 9
            match sampler_state(a(2)) {
                Some(s) => c9(d, dev, m + 6, this, &[me, a(1), s, a(3)], after),
                None => same(d, m + 4, after),
            }
        }
        64 => same(d, 70, after),
        65 => S_FALSE, // GetInfo
        66..=69 => same(d, m + 5, after),
        70 => same(d, 81, after),
        71 => {
            // DrawIndexedPrimitive(type, min index, vertices, start index, primitives): the base
            // vertex is SetIndices'
            let base = d.d8.base_vertex;
            c9(d, dev, 82, this, &[me, a(1), base, a(2), a(3), a(4), a(5)], after)
        }
        72 | 73 => same(d, m + 11, after),
        74 => D3DERR_INVALIDCALL, // ProcessVertices
        75 => create_vs(d, this, a(1), a(2), a(3), after),
        76 => set_vs(d, this, a(1), after),
        77 => {
            wr(a(1), 0, d.d8.cur_vs);
            S_OK
        }
        78 => {
            let i = a(1).wrapping_sub(VS_FIXED_MAX + 1) as usize;
            let Some(Some(vs)) = d.d8.vs.get_mut(i).map(|v| v.take()) else { return D3DERR_INVALIDCALL };
            d.release(vs.decl);
            if vs.vs != 0 {
                d.release(vs.vs);
            }
            S_OK
        }
        79 | 80 => same(d, m + 15, after),
        81 | 82 => {
            // GetVertexShaderDeclaration/Function(handle, data, size in bytes)
            let i = a(1).wrapping_sub(VS_FIXED_MAX + 1) as usize;
            let Some(Some(vs)) = d.d8.vs.get(i) else { return D3DERR_INVALIDCALL };
            let t = if m == 81 { &vs.decl_raw } else { &vs.func };
            give(t, a(2), a(3))
        }
        83 => c9(d, dev, 100, this, &[me, a(1), a(2), 0, a(3)], after),
        84 => {
            let mut off = 0u32;
            c9(d, dev, 101, this, &[me, a(1), a(2), &mut off as *mut u32 as u32, a(3)], after)
        }
        85 => {
            d.d8.base_vertex = a(2);
            c9(d, dev, 104, this, &[me, a(1)], after)
        }
        86 => {
            wr(a(2), 0, d.d8.base_vertex);
            c9(d, dev, 105, this, &[me, a(1)], after)
        }
        87 => {
            let mut o = 0u32;
            let r = c9(d, dev, 106, this, &[me, a(1), &mut o as *mut u32 as u32], after);
            if r == S_OK {
                let id = d.id_of(o);
                d.d8.ps.push(Some(id));
                wr(a(2), 0, d.d8.ps.len() as u32);
            }
            r
        }
        88 => {
            let h = a(1);
            let ps = if h == 0 {
                0
            } else {
                match d.d8.ps.get(h as usize - 1) {
                    Some(&Some(id)) => d.addr_of(id),
                    _ => return D3DERR_INVALIDCALL,
                }
            };
            d.d8.cur_ps = h;
            c9(d, dev, 107, this, &[me, ps], after)
        }
        89 => {
            wr(a(1), 0, d.d8.cur_ps);
            S_OK
        }
        90 => {
            let h = a(1) as usize;
            let Some(Some(id)) = h.checked_sub(1).and_then(|i| d.d8.ps.get_mut(i)).map(|p| p.take()) else {
                return D3DERR_INVALIDCALL;
            };
            d.release(id);
            S_OK
        }
        91 | 92 => same(d, m + 18, after),
        93 => {
            let h = a(1) as usize;
            let Some(&Some(id)) = h.checked_sub(1).and_then(|i| d.d8.ps.get(i)) else { return D3DERR_INVALIDCALL };
            let addr = d.addr_of(id);
            c9(d, IF_PS, 4, id, &[addr, a(2), a(3)], after)
        }
        94..=96 => D3DERR_INVALIDCALL, // patches
        _ => E_NOTIMPL,
    }
}

/// Copies `t` (dwords) out to `data` (NULL: just the size) with its size in bytes at `size`.
unsafe fn give(t: &[u32], data: u32, size: u32) -> u32 {
    let n = t.len() as u32 * 4;
    if data != 0 {
        if rd(size, 0) < n {
            wr(size, 0, n);
            return D3DERR_INVALIDCALL;
        }
        std::ptr::copy_nonoverlapping(t.as_ptr(), data as *mut u32, t.len());
    }
    wr(size, 0, n);
    S_OK
}

fn new_token(d: &mut D3d, addr: u32) -> u32 {
    let id = d.id_of(addr);
    d.d8.sb.push(id);
    d.d8.sb.len() as u32
}

fn token(d: &D3d, t: u32) -> Option<u32> {
    let id = *d.d8.sb.get((t as usize).checked_sub(1)?)?;
    (id != 0).then_some(id)
}

unsafe fn create_vs(d: &mut D3d, this: u32, decl: u32, func: u32, out: u32, after: &mut After) -> u32 {
    if decl == 0 || out == 0 {
        return D3DERR_INVALIDCALL;
    }
    let Some((el, regs, consts, decl_raw)) = parse_decl(decl) else { return D3DERR_INVALIDCALL };
    let me = d.addr_of(this);
    let mut o = 0u32;
    let r = c9(d, IF_DEVICE, 86, this, &[me, el.as_ptr() as u32, &mut o as *mut u32 as u32], after);
    if r != S_OK {
        return r;
    }
    let decl_id = d.id_of(o);
    let (mut vs, mut code) = (0, Vec::new());
    if func != 0 {
        let (raw, with) = shader_code(func, &regs);
        let mut s = 0u32;
        let r = c9(d, IF_DEVICE, 91, this, &[me, with.as_ptr() as u32, &mut s as *mut u32 as u32], after);
        if r != S_OK {
            d.release(decl_id);
            return r;
        }
        vs = d.id_of(s);
        code = raw;
    }
    let v = Vs { decl: decl_id, vs, decl_raw, func: code, consts };
    let i = match d.d8.vs.iter().position(|x| x.is_none()) {
        Some(i) => {
            d.d8.vs[i] = Some(v);
            i
        }
        None => {
            d.d8.vs.push(Some(v));
            d.d8.vs.len() - 1
        }
    };
    wr(out, 0, VS_FIXED_MAX + 1 + i as u32);
    S_OK
}

unsafe fn set_vs(d: &mut D3d, this: u32, h: u32, after: &mut After) -> u32 {
    let me = d.addr_of(this);
    if h <= VS_FIXED_MAX {
        d.d8.cur_vs = h;
        c9(d, IF_DEVICE, 92, this, &[me, 0], after);
        return c9(d, IF_DEVICE, 89, this, &[me, h], after);
    }
    let Some(Some(vs)) = d.d8.vs.get((h - VS_FIXED_MAX - 1) as usize) else { return D3DERR_INVALIDCALL };
    let (decl, shader, consts) = (d.addr_of(vs.decl), d.addr_of(vs.vs), vs.consts.clone());
    d.d8.cur_vs = h;
    c9(d, IF_DEVICE, 87, this, &[me, decl], after);
    for (reg, v) in consts {
        c9(d, IF_DEVICE, 94, this, &[me, reg, v.as_ptr() as u32, 1], after);
    }
    c9(d, IF_DEVICE, 92, this, &[me, shader], after)
}

/// CopyRects(src, rects or NULL: all, count, dst, points or NULL: the rects' positions)
unsafe fn copy_rects(d: &mut D3d, this: u32, src: u32, rects: u32, n: u32, dst: u32, points: u32, after: &mut After) -> u32 {
    let me = d.addr_of(this);
    let sid = d.id_of(src);
    let (sw, sh, rendered) = match d.objs.get(&sid).map(|o| &o.data) {
        Some(Data::Surface(s)) => (s.w, s.h, s.gpu_newer && s.gpu != 0),
        _ => return D3DERR_INVALIDCALL,
    };
    let n = if rects == 0 { 1 } else { n };
    for i in 0..n as usize {
        let r = if rects == 0 { [0, 0, sw, sh] } else { [rd(rects, i * 4), rd(rects, i * 4 + 1), rd(rects, i * 4 + 2), rd(rects, i * 4 + 3)] };
        let p = if points == 0 { [r[0], r[1]] } else { [rd(points, i * 2), rd(points, i * 2 + 1)] };
        let mut hr = D3DERR_INVALIDCALL;
        if !rendered {
            hr = c9(d, IF_DEVICE, 30, this, &[me, src, r.as_ptr() as u32, dst, p.as_ptr() as u32], after);
        }
        if hr != S_OK && rendered && r == [0, 0, sw, sh] && p == [0, 0] {
            // a render target to system memory (screenshots): GetRenderTargetData
            hr = c9(d, IF_DEVICE, 32, this, &[me, src, dst], after);
        }
        if hr != S_OK {
            let dr = [p[0], p[1], p[0] + r[2] - r[0], p[1] + r[3] - r[1]];
            hr = c9(d, IF_DEVICE, 34, this, &[me, src, r.as_ptr() as u32, dst, dr.as_ptr() as u32, 0], after);
        }
        if hr != S_OK {
            return hr;
        }
    }
    S_OK
}
