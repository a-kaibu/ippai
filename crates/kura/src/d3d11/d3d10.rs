//! Direct3D 10 / 10.1 on the Direct3D 11 objects: an ID3D10Device1 is another face of the D3D11
//! device, and every D3D10 resource, view, shader, state and query another face of a D3D11 one
//! (interfaces 95.., `PAIRS`). Their methods become the D3D11 device's, immediate context's or
//! object's calls, with the few structures that differ converted (D3D10_BLEND_DESC, depth
//! stencil view descriptions, integer viewports, buffer descriptions, Map's results). Objects
//! the D3D11 calls hand out are turned into their D3D10 faces.

use super::*;

pub const IF10_DEVICE: u32 = 95;
pub const IF10_BUFFER: u32 = 96;
pub const IF10_TEX1D: u32 = 97;
pub const IF10_TEX2D: u32 = 98;
pub const IF10_TEX3D: u32 = 99;
pub const IF10_SRV: u32 = 100;
pub const IF10_RTV: u32 = 101;
pub const IF10_DSV: u32 = 102;
pub const IF10_VS: u32 = 103;
pub const IF10_GS: u32 = 104;
pub const IF10_PS: u32 = 105;
pub const IF10_LAYOUT: u32 = 106;
pub const IF10_SAMPLER: u32 = 107;
pub const IF10_BLEND: u32 = 108;
pub const IF10_DEPTH: u32 = 109;
pub const IF10_RASTER: u32 = 110;
pub const IF10_QUERY: u32 = 111;

/// (D3D11 interface, its D3D10 face)
pub const PAIRS: &[(u32, u32)] = &[
    (IF_DEVICE, IF10_DEVICE),
    (IF_BUFFER, IF10_BUFFER),
    (IF_TEX1D, IF10_TEX1D),
    (IF_TEX2D, IF10_TEX2D),
    (IF_TEX3D, IF10_TEX3D),
    (IF_SRV, IF10_SRV),
    (IF_RTV, IF10_RTV),
    (IF_DSV, IF10_DSV),
    (IF_VS, IF10_VS),
    (IF_GS, IF10_GS),
    (IF_PS, IF10_PS),
    (IF_LAYOUT, IF10_LAYOUT),
    (IF_SAMPLER, IF10_SAMPLER),
    (IF_BLEND, IF10_BLEND),
    (IF_DEPTH, IF10_DEPTH),
    (IF_RASTER, IF10_RASTER),
    (IF_QUERY, IF10_QUERY),
];

const ALL10: &[u32] = &[
    IF10_BUFFER, IF10_TEX1D, IF10_TEX2D, IF10_TEX3D, IF10_SRV, IF10_RTV, IF10_DSV, IF10_VS, IF10_GS, IF10_PS, IF10_LAYOUT, IF10_SAMPLER, IF10_BLEND, IF10_DEPTH, IF10_RASTER,
    IF10_QUERY,
];

/// IIDs (first dword) of the D3D10 interfaces
pub const IIDS10: &[(u32, &[u32])] = &[
    (0x9b7e4c0f, &[IF10_DEVICE]),
    (0x9b7e4c8f, &[IF10_DEVICE]),
    (0x9b7e4c00, ALL10), // ID3D10DeviceChild
    (0x9b7e4c01, &[IF10_BUFFER, IF10_TEX1D, IF10_TEX2D, IF10_TEX3D]),
    (0x9b7e4c02, &[IF10_BUFFER]),
    (0x9b7e4c03, &[IF10_TEX1D]),
    (0x9b7e4c04, &[IF10_TEX2D]),
    (0x9b7e4c05, &[IF10_TEX3D]),
    (0xc902b03f, &[IF10_SRV, IF10_RTV, IF10_DSV]),
    (0x9b7e4c07, &[IF10_SRV]),
    (0x9b7e4c87, &[IF10_SRV]),
    (0x9b7e4c08, &[IF10_RTV]),
    (0x9b7e4c09, &[IF10_DSV]),
    (0x9b7e4c0a, &[IF10_VS]),
    (0x6316be88, &[IF10_GS]),
    (0x4968b601, &[IF10_PS]),
    (0x9b7e4c0b, &[IF10_LAYOUT]),
    (0x9b7e4c0c, &[IF10_SAMPLER]),
    (0xedad8d19, &[IF10_BLEND]),
    (0xedad8d99, &[IF10_BLEND]),
    (0x2b4b1cc8, &[IF10_DEPTH]),
    (0xa2a07292, &[IF10_RASTER]),
    (0x9b7e4c0d, &[IF10_QUERY]),
    (0x9b7e4c0e, &[IF10_QUERY]),
    (0x9b7e4c10, &[IF10_QUERY]),
    (0x9b7e4c11, &[IF10_QUERY]),
];

/// The D3D10 face of a D3D11 interface (and back)
pub fn ten(iface: u32) -> Option<u32> {
    PAIRS.iter().find(|p| p.0 == iface).map(|p| p.1)
}
pub fn eleven(iface: u32) -> Option<u32> {
    PAIRS.iter().find(|p| p.1 == iface).map(|p| p.0)
}

/// Pointers to objects at out[0..n] (pointer-sized slots) become their D3D10 faces.
unsafe fn reface(d: &mut D11, out: u32, n: u32) {
    if out == 0 {
        return;
    }
    for i in 0..n as usize {
        let id = d.id_of(rdp(out, i));
        if id == 0 {
            continue;
        }
        if let Some(f) = d.objs.get(&id).and_then(|o| ten(o.iface)) {
            let a = d.face(id, f);
            wrp(out, i, a);
        }
    }
}

/// Runs `f` with `args` as its arguments (missing ones 0).
unsafe fn with(args: &[u32], f: impl FnOnce(&dyn Fn(usize) -> u32) -> u32) -> u32 {
    let mut v = [0u32; 16];
    v[..args.len()].copy_from_slice(args);
    f(&|i| v[i])
}

/// A scratch block of host memory the D3D11 calls can read and write (guest addresses are
/// host addresses).
fn scratch(n: usize) -> Vec<u32> {
    vec![0u32; n]
}
fn at(v: &mut [u32]) -> u32 {
    v.as_mut_ptr() as usize as u32
}

/// D3D10_{BUFFER, TEXTURE1D, TEXTURE2D, TEXTURE3D}_DESC: (dwords, MiscFlags' index)
const DESC: [(usize, usize); 4] = [(5, 4), (8, 7), (11, 10), (9, 8)];

/// D3D10_RESOURCE_MISC_* as D3D11's: SHARED_KEYEDMUTEX 0x10 and GDI_COMPATIBLE 0x20 are
/// 0x100 and 0x200 there (0x10 and 0x20 D3D11's DRAWINDIRECT_ARGS and BUFFER_ALLOW_RAW_VIEWS)
fn misc11(m: u32) -> u32 {
    m & !0x30 | (m & 0x10) << 4 | (m & 0x20) << 4
}
fn misc10(m: u32) -> u32 {
    m & !0x330 | (m & 0x100) >> 4 | (m & 0x200) >> 4
}

/// ID3D10Device1 methods.
pub(super) unsafe fn device(d: &mut D11, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let args: Vec<u32> = (0..12).map(a).collect();
    let ctx = |d: &mut D11, m11: u32, v: &[u32], after: &mut After| with(v, |b| context_call(d, m11, b, after));
    let dev = |d: &mut D11, m11: u32, v: &[u32]| with(v, |b| device_call(d, m11, this, b));
    // same arguments as the D3D11 context's
    const CTX: &[(u32, u32)] = &[
        (3, 7), (4, 8), (6, 10), (8, 12), (9, 13), (10, 16), (11, 17), (12, 18), (13, 19), (14, 20), (15, 21), (16, 22), (18, 24), (19, 25), (20, 26), (21, 30), (22, 31),
        (23, 32), (24, 33), (25, 35), (26, 36), (27, 37), (28, 38), (29, 43), (31, 45), (32, 46), (33, 47), (34, 48), (35, 50), (36, 53), (37, 54), (38, 57), (50, 83),
        (62, 96), (69, 110), (70, 111),
    ];
    if let Some(&(_, m11)) = CTX.iter().find(|x| x.0 == m) {
        return ctx(d, m11, &args, after);
    }
    // getters of objects: (D3D10 method, D3D11 method, where the objects are: argument, count)
    const GET: &[(u32, u32, usize, i32)] = &[
        (39, 72, 3, -2), (40, 73, 3, -2), (42, 75, 3, -2), (44, 77, 3, -2), (45, 78, 1, 1), (47, 80, 1, 1), (48, 81, 3, -2), (51, 84, 3, -2), (52, 85, 3, -2),
        (54, 87, 3, -2), (55, 88, 3, -2), (57, 91, 1, 1), (58, 92, 1, 1), (59, 93, 2, -1), (60, 94, 1, 1),
    ];
    if let Some(&(_, m11, slot, count)) = GET.iter().find(|x| x.0 == m) {
        let r = ctx(d, m11, &args, after);
        let n = if count > 0 { count as u32 } else { args[(-count) as usize] };
        reface(d, args[slot], n);
        return r;
    }
    let mut zero = [0u32; 2];
    let z = at(&mut zero);
    match m {
        // *SetShader(shader): no class instances
        5 | 7 | 17 => ctx(d, [9, 11, 23][[5, 7, 17].iter().position(|&x| x == m).unwrap()], &[args[0], args[1], 0, 0], after),
        // *GetShader(out)
        41 | 43 | 49 => {
            let m11 = [74, 76, 82][[41, 43, 49].iter().position(|&x| x == m).unwrap()];
            let r = ctx(d, m11, &[args[0], args[1], 0, z], after);
            reface(d, args[1], 1);
            r
        }
        // IAGetVertexBuffers(start, n, buffers, strides, offsets)
        46 => {
            let r = ctx(d, 79, &args, after);
            reface(d, args[3], args[2]);
            r
        }
        // OMGetRenderTargets(n, rtvs, dsv)
        56 => {
            let r = ctx(d, 89, &args, after);
            reface(d, args[2], args[1]);
            reface(d, args[3], 1);
            r
        }
        // RSSetViewports(n, D3D10_VIEWPORT { INT x, y; UINT w, h; FLOAT min, max })
        30 => {
            if args[1] == 0 || args[2] == 0 {
                return S_OK;
            }
            let (p, n) = (args[2], args[1].min(16) as usize);
            let mut v = scratch(6 * n);
            for k in 0..n {
                for i in 0..4 {
                    let x = rd(p, 6 * k + i);
                    v[6 * k + i] = if i < 2 { (x as i32 as f32).to_bits() } else { (x as f32).to_bits() };
                }
                v[6 * k + 4] = rd(p, 6 * k + 4);
                v[6 * k + 5] = rd(p, 6 * k + 5);
            }
            ctx(d, 44, &[args[0], args[1], at(&mut v)], after)
        }
        61 => {
            // RSGetViewports(n*, out)
            let mut v = scratch(6 * 16);
            let r = ctx(d, 95, &[args[0], args[1], if args[2] != 0 { at(&mut v) } else { 0 }], after);
            if args[2] != 0 && args[1] != 0 {
                for k in 0..(rd(args[1], 0) as usize).min(16) {
                    for i in 0..4 {
                        let f = f32::from_bits(v[6 * k + i]);
                        wr(args[2], 6 * k + i, if i < 2 { f as i32 as u32 } else { f as u32 });
                    }
                    wr(args[2], 6 * k + 4, v[6 * k + 4]);
                    wr(args[2], 6 * k + 5, v[6 * k + 5]);
                }
            }
            r
        }
        53 => {
            // GetPredication(predicate, value)
            if args[1] != 0 {
                wrp(args[1], 0, 0);
            }
            if args[2] != 0 {
                wr(args[2], 0, 0);
            }
            S_OK
        }
        63 => S_OK,
        64 => S_OK,
        65 => 0,
        66..=68 => private_data(d, m - 62, this, a),
        71..=74 => {
            // Create{Buffer, Texture1D/2D/3D}(desc, init, out): the description with D3D11's
            // misc flags (and a buffer's StructureByteStride 0)
            if args[1] == 0 {
                return E_INVALIDARG;
            }
            let (n, k) = DESC[m as usize - 71];
            let mut desc = scratch(n.max(6));
            for i in 0..n {
                desc[i] = rd(args[1], i);
            }
            desc[k] = misc11(desc[k]);
            let r = dev(d, m - 68, &[args[0], at(&mut desc), args[2], args[3]]);
            reface(d, args[3], 1);
            r
        }
        78 => {
            // CreateInputLayout(.., out at 5)
            let r = dev(d, 11, &args);
            reface(d, args[if m == 78 { 5 } else { 3 }], 1);
            r
        }
        75 | 76 | 98 => {
            let r = dev(d, if m == 76 { 9 } else { 7 }, &args);
            reface(d, args[3], 1);
            r
        }
        77 => {
            // CreateDepthStencilView: D3D11's description has Flags after ViewDimension
            let mut desc = scratch(8);
            let p = args[2];
            let pd = if p != 0 {
                desc[0] = rd(p, 0);
                desc[1] = rd(p, 1);
                for i in 2..5 {
                    desc[i + 1] = rd(p, i);
                }
                at(&mut desc)
            } else {
                0
            };
            let r = dev(d, 10, &[args[0], args[1], pd, args[3]]);
            reface(d, args[3], 1);
            r
        }
        79 | 80 | 82 => {
            // Create*Shader(code, len, out): no class linkage
            let m11 = match m {
                79 => 12,
                80 => 13,
                _ => 15,
            };
            let r = dev(d, m11, &[args[0], args[1], args[2], 0, args[3]]);
            reface(d, args[3], 1);
            r
        }
        81 => {
            // CreateGeometryShaderWithStreamOutput(code, len, decls, n, stride, out)
            let mut v = [0u32; 10];
            v[9] = args[6];
            let r = dev(d, 14, &v);
            reface(d, args[6], 1);
            r
        }
        83 => {
            // CreateBlendState(D3D10_BLEND_DESC): AlphaToCoverage, BlendEnable[8], the six
            // factors and operations, RenderTargetWriteMask[8] (bytes) -> D3D11_BLEND_DESC
            let p = args[1];
            if p == 0 {
                return E_INVALIDARG;
            }
            let mut desc = scratch(66);
            desc[0] = rd(p, 0);
            desc[1] = 1;
            for t in 0..8 {
                let b = 2 + t * 8;
                desc[b] = rd(p, 1 + t);
                for k in 0..6 {
                    desc[b + 1 + k] = rd(p, 9 + k);
                }
                desc[b + 7] = *((p + 60 + t as u32) as *const u8) as u32;
            }
            let r = dev(d, 20, &[args[0], at(&mut desc), args[2]]);
            reface(d, args[2], 1);
            r
        }
        84..=88 | 99 => {
            let m11 = match m {
                84 => 21,
                85 => 22,
                86 => 23,
                87 => 24,
                88 => 25,
                _ => 20,
            };
            let r = dev(d, m11, &args);
            reface(d, args[2], 1);
            r
        }
        89 => E_INVALIDARG,
        90 => dev(d, 29, &args),
        91 => dev(d, 30, &args),
        92 => dev(d, 31, &args),
        93 => E_INVALIDARG,
        94 => 0,
        95 => E_NOTIMPL,
        96 => S_OK,
        97 => {
            for i in 1..3 {
                if args[i] != 0 {
                    wr(args[i], 0, 0);
                }
            }
            S_OK
        }
        100 => 0xa100,
        _ => E_NOTIMPL,
    }
}

/// Methods of the D3D10 faces of resources, views, shaders, states and queries.
pub(super) unsafe fn child(d: &mut D11, iface: u32, m: u32, this: u32, a: &dyn Fn(usize) -> u32, after: &mut After) -> u32 {
    let Some(i11) = eleven(iface) else { return E_NOTIMPL };
    let args: Vec<u32> = (0..8).map(a).collect();
    let me = d.face(this, iface);
    match (iface, m) {
        (_, 3) => {
            // GetDevice: the D3D10 one (none for a D3D11 device's resources)
            if !d.made10 && !d.ten_faces {
                wrp(args[1], 0, 0);
                return S_OK;
            }
            d.add_ref(d.device);
            let f = d.face(d.device, IF10_DEVICE);
            wrp(args[1], 0, f);
            S_OK
        }
        (_, 4..=6) => private_data(d, m, this, a),
        (IF10_BUFFER | IF10_TEX1D | IF10_TEX2D | IF10_TEX3D, 7..=9) => child_call(d, i11, m, this, a),
        (IF10_BUFFER | IF10_TEX1D | IF10_TEX2D | IF10_TEX3D, 10) => {
            // Map: buffers (type, flags, void**), textures (sub, type, flags, out): 1D a pointer,
            // 2D { pData, RowPitch }, 3D { pData, RowPitch, DepthPitch }
            let (sub, ty, out) = if iface == IF10_BUFFER { (0, args[1], args[3]) } else { (args[1], args[2], args[4]) };
            let mut m11 = scratch(4);
            let r = map(d, me, sub, ty, at(&mut m11), false, after);
            if r != S_OK || out == 0 {
                return r;
            }
            let fields = match iface {
                IF10_TEX2D => 2,
                IF10_TEX3D => 3,
                _ => 1,
            };
            wrp(out, 0, rdp(at(&mut m11), 0));
            for f in 1..fields {
                wr(out + ptr(), f - 1, rd(at(&mut m11) + ptr(), f - 1));
            }
            S_OK
        }
        (IF10_BUFFER | IF10_TEX1D | IF10_TEX2D | IF10_TEX3D, 11) => {
            unmap(d, me, if iface == IF10_BUFFER { 0 } else { args[1] });
            S_OK
        }
        (IF10_BUFFER | IF10_TEX1D | IF10_TEX2D | IF10_TEX3D, 12) => {
            // GetDesc: D3D10's misc flags (and no StructureByteStride)
            let (n, k) = DESC[(iface - IF10_BUFFER) as usize];
            let mut v = scratch(12);
            let r = with(&[args[0], at(&mut v)], |b| child_call(d, i11, 10, this, b));
            v[k] = misc10(v[k]);
            for i in 0..n {
                wr(args[1], i, v[i]);
            }
            r
        }
        (IF10_SRV | IF10_RTV | IF10_DSV, 7) => {
            let r = child_call(d, i11, 7, this, a);
            reface(d, args[1], 1);
            r
        }
        (IF10_DSV, 8) => {
            // GetDesc: without D3D11's Flags
            let mut v = scratch(6);
            let r = with(&[args[0], at(&mut v)], |b| child_call(d, i11, 8, this, b));
            for (i, k) in [0, 1, 3, 4, 5].into_iter().enumerate() {
                wr(args[1], i, v[k]);
            }
            r
        }
        (IF10_SRV | IF10_RTV, 8 | 9) => child_call(d, i11, 8, this, a),
        (IF10_BLEND, 7) => {
            // GetDesc: D3D10_BLEND_DESC from the D3D11 description
            let Some(Data::State(s)) = d.objs.get(&this).map(|o| &o.data) else { return E_FAIL };
            let p = args[1];
            wr(p, 0, s[0]);
            for t in 0..8 {
                wr(p, 1 + t, s[2 + t * 8]);
                *((p + 60 + t as u32) as *mut u8) = s[2 + t * 8 + 7] as u8;
            }
            for k in 0..6 {
                wr(p, 9 + k, s[3 + k]);
            }
            S_OK
        }
        (IF10_BLEND, 8) | (IF10_SAMPLER | IF10_DEPTH | IF10_RASTER, 7) => child_call(d, i11, 7, this, a),
        // queries: Begin, End, GetData(data, size, flags), GetDataSize, GetDesc
        (IF10_QUERY, 7 | 8) => with(&[0, me], |b| context_call(d, 20 + m, b, after)),
        (IF10_QUERY, 9) => with(&[0, me, args[1], args[2], args[3]], |b| context_call(d, 29, b, after)),
        (IF10_QUERY, 10) => child_call(d, i11, 7, this, a),
        (IF10_QUERY, 11) => child_call(d, i11, 8, this, a),
        _ => E_NOTIMPL,
    }
}
