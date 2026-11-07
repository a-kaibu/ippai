//! The bits a texture holds, through the real entry point with the page's executor on the model
//! of WebGPU: what Map(READ) of a staging texture gives back after uploads (initial data,
//! UpdateSubresource) and copies (CopyResource, CopySubresourceRegion) is the bytes written, for
//! every format the device creates, between every pair of formats (the bits within a typeless
//! group, nothing across), for every subresource of arrays, mips and 3D textures. Windows keeps
//! texels as the bytes written; the page converts some formats (16-bit UNORM/SNORM to floats,
//! 5:6:5 and A8 widened, block-compressed ones decoded), which must not show.

use super::*;
use crate::d3d9::trace_tests::{alloc, attach_page, CALLS, SERIAL};
use crate::render::model;

fn mem(n: u32) -> u32 {
    unsafe { alloc(n.max(4)) as u32 }
}
fn put(p: u32, v: &[u32]) {
    for (i, &x) in v.iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = x };
    }
}
fn put_bytes(p: u32, v: &[u8]) {
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), p as *mut u8, v.len()) };
}
fn rd1(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}
fn call1(id: u32, args: &[u32]) -> u32 {
    unsafe { call(id, args.as_ptr(), &CALLS) }
}
fn m(iface: u32, m: u32) -> u32 {
    iface << 8 | m
}

struct Rng(u64);
impl Rng {
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                (self.0 >> 24) as u8
            })
            .collect()
    }
}

/// (dimension 2 or 3, width, height, array size or depth, mips)
#[derive(Clone, Copy)]
struct Shape(u32, u32, u32, u32, u32);

impl Shape {
    fn subs(self) -> u32 {
        if self.0 == 3 {
            self.4
        } else {
            self.4 * self.3
        }
    }
    /// (row pitch, rows, slices) of a subresource
    fn layout(self, fmt: u32, sub: u32) -> (u32, u32, u32) {
        let l = sub % self.4;
        let (pitch, rows) = format::pitch_rows(fmt, (self.1 >> l).max(1), (self.2 >> l).max(1));
        (pitch, rows, if self.0 == 3 { (self.3 >> l).max(1) } else { 1 })
    }
}

struct Dev {
    sc: u32,
    dev: u32,
    ctx: u32,
}

impl Dev {
    fn new() -> Dev {
        let vt = mem(1024);
        put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
        call1(INIT, &[vt, 256]);
        attach_page();
        let desc = mem(64);
        put(desc, &[64, 64, 60, 1, 28, 0, 0, 1, 0, 0x20, 1, 1, 1, 0, 0]);
        let (sc_out, dev_out, level, ctx_out) = (mem(8), mem(8), mem(8), mem(8));
        assert_eq!(call1(CREATE_DEVICE, &[0, 1, 0, 0, 0, 0, 7, desc, sc_out, dev_out, level, ctx_out]), 0);
        Dev { sc: rd1(sc_out), dev: rd1(dev_out), ctx: rd1(ctx_out) }
    }

    /// A texture (0: refused): usage 0 default (shader resource), 3 staging (CPU read and
    /// write), with each subresource's bytes as initial data.
    fn tex(&self, s: Shape, fmt: u32, staging: bool, data: Option<&[Vec<u8>]>) -> u32 {
        if staging {
            self.make(s, fmt, (3, 0, 0x30000), data)
        } else {
            self.tex_bound(s, fmt, 8, data)
        }
    }

    /// A default texture of these bind flags.
    fn tex_bound(&self, s: Shape, fmt: u32, bind: u32, data: Option<&[Vec<u8>]>) -> u32 {
        self.make(s, fmt, (0, bind, 0), data)
    }

    fn make(&self, s: Shape, fmt: u32, (usage, bind, cpu): (u32, u32, u32), data: Option<&[Vec<u8>]>) -> u32 {
        let d = mem(64);
        if s.0 == 3 {
            put(d, &[s.1, s.2, s.3, s.4, fmt, usage, bind, cpu, 0]);
        } else {
            put(d, &[s.1, s.2, s.4, s.3, fmt, 1, 0, usage, bind, cpu, 0]);
        }
        let init = match data {
            Some(subs) => {
                let init = mem(12 * subs.len() as u32);
                for (i, b) in subs.iter().enumerate() {
                    let (pitch, rows, _) = s.layout(fmt, i as u32);
                    let p = mem(b.len() as u32);
                    put_bytes(p, b);
                    put(init + 12 * i as u32, &[p, pitch, pitch * rows]);
                }
                init
            }
            None => 0,
        };
        let o = mem(8);
        put(o, &[0]);
        let hr = call1(m(IF_DEVICE, if s.0 == 3 { 6 } else { 5 }), &[self.dev, d, init, o]);
        if hr != 0 {
            return 0;
        }
        rd1(o)
    }

    /// Map(READ) of a staging texture's subresource: its bytes (pitch * rows * slices)
    fn read(&self, t: u32, s: Shape, fmt: u32, sub: u32) -> Vec<u8> {
        gpu::pump();
        let out = mem(16);
        assert_eq!(call1(m(IF_CONTEXT, 14), &[self.ctx, t, sub, 1, 0, out]), 0, "Map of format {fmt} sub {sub}");
        let (pitch, rows, slices) = s.layout(fmt, sub);
        assert_eq!(rd1(out + 4), pitch, "row pitch of format {fmt}");
        let v = unsafe { std::slice::from_raw_parts(rd1(out) as *const u8, (pitch * rows * slices) as usize) }.to_vec();
        call1(m(IF_CONTEXT, 15), &[self.ctx, t, sub]);
        v
    }

    fn release(&self, t: u32) {
        if t != 0 {
            call1(m(IF_TEX2D, 2), &[t]);
        }
    }

    fn done(self) {
        call1(m(IF_SWAPCHAIN, 2), &[self.sc]);
        call1(m(IF_CONTEXT, 2), &[self.ctx]);
        call1(m(IF_DEVICE, 2), &[self.dev]);
        gpu::set_pump(None);
        let errors = model::with(|m| std::mem::take(&mut m.errors));
        assert!(errors.is_empty(), "WebGPU errors {:?}", &errors[..errors.len().min(10)]);
    }
}

/// The formats a texture of is made with initial data (not depth: no initial data)
fn formats() -> Vec<u32> {
    (1..=132).filter(|&f| format::info(f).is_some_and(|i| !format::is_depth(i.page)) && check::format(f).is_some_and(|r| r.0) && !(19..=22).contains(&f) && !(44..=47).contains(&f)).collect()
}

fn random_subs(rng: &mut Rng, s: Shape, fmt: u32) -> Vec<Vec<u8>> {
    (0..s.subs())
        .map(|sub| {
            let (pitch, rows, slices) = s.layout(fmt, sub);
            rng.bytes((pitch * rows * slices) as usize)
        })
        .collect()
}

#[test]
fn every_format_and_pair_keeps_the_bits() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let fmts = formats();
    assert!(fmts.len() > 80, "{} formats", fmts.len());
    let s = Shape(2, 4, 4, 1, 1);
    for &f in &fmts {
        let data = random_subs(&mut rng, s, f);
        // a staging texture's own initial data, and a default texture's copied to one
        let st = dev.tex(s, f, true, Some(&data));
        assert_ne!(st, 0, "staging texture of format {f}");
        assert_eq!(dev.read(st, s, f, 0), data[0], "initial data of format {f}");
        let src = dev.tex(s, f, false, Some(&data));
        // (P8 and A8P8: no shader resource of them, a default texture bound to nothing)
        let src = if src == 0 && matches!(f, 113 | 114) { dev.tex_bound(s, f, 0, Some(&data)) } else { src };
        assert_ne!(src, 0, "texture of format {f}");
        for &g in fmts.iter().filter(|&&g| format::info(g).unwrap().bpp == format::info(f).unwrap().bpp && format::info(g).unwrap().block == format::info(f).unwrap().block) {
            let dst = dev.tex(s, g, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, dst, src]);
            let got = dev.read(dst, s, g, 0);
            let want = if format::copy_compatible(f, g) { data[0].clone() } else { vec![0; got.len()] };
            assert_eq!(got, want, "CopyResource {f} -> {g}");
            dev.release(dst);
        }
        dev.release(st);
        dev.release(src);
    }
    dev.done();
}

/// One format of each way the page holds texels: as the guest's bytes (R8G8B8A8, R32), converted
/// (R16G16B16A16 UNORM, R16 SNORM, B5G6R5, A8), block-compressed (BC1, BC3, BC7); every
/// subresource of arrays with mips and of 3D textures, whole and boxes of them, across the
/// conversions (R16G16B16A16 UINT <-> UNORM, R16 UINT <-> SNORM).
#[test]
fn every_subresource_keeps_the_bits() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let pairs = [(28, 28), (41, 41), (11, 11), (58, 58), (85, 85), (65, 65), (71, 71), (77, 77), (98, 98), (12, 11), (11, 12), (57, 58), (58, 57), (10, 11), (71, 72)];
    for shape in [Shape(2, 8, 8, 3, 4), Shape(3, 8, 4, 4, 3)] {
        for (f, g) in pairs {
            let data = random_subs(&mut rng, shape, f);
            let src = dev.tex(shape, f, false, Some(&data));
            assert_ne!(src, 0, "format {f}");
            // CopyResource: every subresource
            let dst = dev.tex(shape, g, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, dst, src]);
            for sub in 0..shape.subs() {
                assert_eq!(dev.read(dst, shape, g, sub), data[sub as usize], "CopyResource {f} -> {g}, {} sub {sub}", shape.0);
            }
            // CopySubresourceRegion: the last layer's level 0 box (4, 0, 1)-(8, 4, 3) of the
            // source (whole blocks) to (0, dy, 0) of the first (dy 4, 0 for 8x4 3D ones)
            let ssub = if shape.0 == 3 { 0 } else { shape.subs() - shape.4 };
            let dy = if shape.0 == 3 { 0 } else { 4 };
            let dst2 = dev.tex(shape, g, true, None);
            let bx = mem(24);
            put(bx, &[4, 0, 1, 8, 4, 3]);
            call1(m(IF_CONTEXT, 46), &[dev.ctx, dst2, 0, 0, dy, 0, src, ssub, bx]);
            let got = dev.read(dst2, shape, g, 0);
            let (pitch, rows, slices) = shape.layout(f, 0);
            let fi = format::info(f).unwrap();
            let mut want = vec![0u8; got.len()];
            let (bx0, row) = (4 / fi.block * fi.bpp, 4 / fi.block * fi.bpp);
            let (by_src, by_dst, n) = (0, dy / fi.block, 4 / fi.block);
            let (z_src, depth) = if shape.0 == 3 { (1, 2) } else { (0, 1) };
            for z in 0..depth.min(slices) {
                for r in 0..n {
                    let from = (((z_src + z) * rows + by_src + r) * pitch + bx0) as usize;
                    let to = ((z * rows + by_dst + r) * pitch) as usize;
                    want[to..to + row as usize].copy_from_slice(&data[ssub as usize][from..from + row as usize]);
                }
            }
            assert_eq!(got, want, "CopySubresourceRegion {f} -> {g}, {}", shape.0);
            // UpdateSubresource of the same box into the staging copy's level 0: nothing (as
            // Windows, winref d3d11copy: the staging texture hashes as CopyResource left it)
            let up = rng.bytes((row * n * depth) as usize);
            let p = mem(up.len() as u32);
            put_bytes(p, &up);
            put(bx, &[4, dy, z_src, 8, dy + 4, z_src + depth]);
            call1(m(IF_CONTEXT, 48), &[dev.ctx, dst, 0, bx, p, row, row * n]);
            assert_eq!(dev.read(dst, shape, g, 0), data[0], "UpdateSubresource {g}, {}", shape.0);
            for t in [src, dst, dst2] {
                dev.release(t);
            }
        }
    }
    dev.done();
}

/// B8G8R8X8 UNORM and UNORM_SRGB: Map gives back the bytes written, but the page's texels (what
/// shaders sample) hold alpha 1 (Windows' answer in winref d3d11tex: format 93 samples alpha 1.0)
#[test]
fn x_channels_sample_as_one() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    let s = Shape(2, 4, 4, 1, 1);
    let data = vec![(0..64u32).map(|i| (i * 37 + 5) as u8).collect::<Vec<u8>>()];
    for f in [88, 93] {
        let t = dev.tex(s, f, false, Some(&data));
        assert_ne!(t, 0);
        let g = {
            let g = D11S.lock().unwrap();
            let d = g.as_ref().unwrap();
            match &d.objs[&d.id_of(t)].data {
                Data::Texture(t) => t.gpu,
                _ => 0,
            }
        };
        let rb = mem(64);
        gpu::pump();
        gpu::record(|b| b.cmd(&[gpu::READBACK, 0x5300_0000 | f, g, 0, 0, 0, 4, 4, rb, 16]));
        assert!(gpu::wait_readback(0x5300_0000 | f));
        let page = unsafe { std::slice::from_raw_parts(rb as *const u8, 64) }.to_vec();
        let want: Vec<u8> = data[0].iter().enumerate().map(|(i, &b)| if i % 4 == 3 { 255 } else { b }).collect();
        assert_eq!(page, want, "page texels of format {f}");
        dev.release(t);
    }
    dev.done();
}

/// CopySubresourceRegion of random boxes (on blocks, inside the source) to random places of
/// random subresources, between random formats of one group, against the bytes moved as Windows
/// moves them; the destination's other bytes stay as they were.
#[test]
fn random_regions_move_the_bytes() {
    use proptest::prelude::*;
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    // (source, destination) pairs: page-held, converted, block-compressed, across the ways
    let pairs = [(28u32, 28u32), (28, 30), (41, 41), (11, 11), (12, 11), (11, 12), (58, 57), (85, 85), (65, 65), (71, 72), (77, 77), (98, 99), (2, 3), (10, 11)];
    let cfg = ProptestConfig { cases: 96, failure_persistence: None, ..ProptestConfig::default() };
    let ran = std::cell::Cell::new(0u32);
    proptest!(cfg, |(pair in 0..pairs.len(), dim3 in any::<bool>(), sub_pick in 0u32..12, d_pick in 0u32..12,
                     bx in (0u32..4, 0u32..4, 1u32..5, 1u32..5, 0u32..3, 1u32..3), at in (0u32..4, 0u32..4, 0u32..3), seed in any::<u64>())| {
        let (f, g) = pairs[pair];
        let fi = format::info(f).unwrap();
        let blk = fi.block;
        let shape = if dim3 { Shape(3, 16, 8, 4, 3) } else { Shape(2, 16, 16, 3, 4) };
        let (ssub, dsub) = (sub_pick % shape.subs(), d_pick % shape.subs());
        let lvl = |sub: u32| {
            let l = sub % shape.4;
            ((shape.1 >> l).max(1), (shape.2 >> l).max(1), if dim3 { (shape.3 >> l).max(1) } else { 1 })
        };
        let (sw, sh, sd) = lvl(ssub);
        let (dw, dh, dd) = lvl(dsub);
        // (a box of whole blocks inside the source, its place inside the destination)
        let (bw_blocks, bh_blocks) = (sw.div_ceil(blk), sh.div_ceil(blk));
        let x0 = bx.0 % bw_blocks;
        let y0 = bx.1 % bh_blocks;
        let w = bx.2.min(bw_blocks - x0);
        let h = bx.3.min(bh_blocks - y0);
        let z0 = bx.4 % sd;
        let depth = bx.5.min(sd - z0);
        let (dbw, dbh) = (dw.div_ceil(blk), dh.div_ceil(blk));
        prop_assume!(w <= dbw && h <= dbh && depth <= dd);
        let (ax, ay, az) = (at.0 % (dbw - w + 1), at.1 % (dbh - h + 1), at.2 % (dd - depth + 1));
        ran.set(ran.get() + 1);
        let mut rng = Rng(seed | 1);
        let sdata = random_subs(&mut rng, shape, f);
        let ddata = random_subs(&mut rng, shape, g);
        let src = dev.tex(shape, f, false, Some(&sdata));
        let dst = dev.tex(shape, g, true, Some(&ddata));
        prop_assert!(src != 0 && dst != 0);
        // (texel coordinates; a box's end past a level's edge is its last block's end)
        let b = mem(24);
        let ex = |v: u32, lim: u32| (v * blk).min(lim.div_ceil(blk) * blk);
        put(b, &[x0 * blk, y0 * blk, z0, ex(x0 + w, sw).max(x0 * blk + 1), ex(y0 + h, sh).max(y0 * blk + 1), z0 + depth]);
        let (zx, zy) = (ax * blk, ay * blk);
        call1(m(IF_CONTEXT, 46), &[dev.ctx, dst, dsub, zx, zy, if dim3 { az } else { 0 }, src, ssub, b]);
        let got = dev.read(dst, shape, g, dsub);
        let (sp, srows, _) = shape.layout(f, ssub);
        let (dp, drows, _) = shape.layout(g, dsub);
        let mut want = ddata[dsub as usize].clone();
        let (az, z0) = if dim3 { (az, z0) } else { (0, 0) };
        let row = (w * fi.bpp) as usize;
        for z in 0..depth {
            for r in 0..h {
                let from = (((z0 + z) * srows + y0 + r) * sp + x0 * fi.bpp) as usize;
                let to = (((az + z) * drows + ay + r) * dp + ax * fi.bpp) as usize;
                want[to..to + row].copy_from_slice(&sdata[ssub as usize][from..from + row]);
            }
        }
        dev.release(src);
        dev.release(dst);
        prop_assert_eq!(&got, &want, "{} -> {} {}d sub {} -> {} box {:?} at {:?}", f, g, shape.0, ssub, dsub, (x0, y0, z0, w, h, depth), (ax, ay, az));
    });
    assert!(ran.get() >= 96, "{} cases ran", ran.get());
    dev.done();
}

fn gpu_of(t: u32) -> u32 {
    let g = D11S.lock().unwrap();
    let d = g.as_ref().unwrap();
    match &d.objs[&d.id_of(t)].data {
        Data::Texture(t) => t.gpu,
        _ => 0,
    }
}

/// The page's pixels of a level and layer (READBACK, 4 bytes a texel), None: refused (the
/// readback answered, the memory untouched).
fn page_read(g: u32, word: u32, w: u32, h: u32, req: u32) -> Option<Vec<u8>> {
    let rb = mem(4 * w * h);
    unsafe { std::ptr::write_bytes(rb as *mut u8, 0xa5, (4 * w * h) as usize) };
    gpu::pump();
    gpu::record(|b| b.cmd(&[gpu::READBACK, req, g, word, 0, 0, w, h, rb, 4 * w]));
    assert!(gpu::wait_readback(req));
    let v = unsafe { std::slice::from_raw_parts(rb as *const u8, (4 * w * h) as usize) }.to_vec();
    (!v.iter().all(|&b| b == 0xa5)).then_some(v)
}

/// The page's COPY and READBACK with layers and 3D slices: within one texture between layers,
/// several slices between formats the page copies through a buffer, every level's slice count,
/// and nothing (no WebGPU error, nothing changed) for a level, layer or slice past the texture
/// or a stretching copy of layers.
#[test]
fn page_copies_and_readbacks_stay_inside() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    // a 4x4 array of 2 layers and 2 levels: layer 0 level 0 to layer 1 level 0 of itself
    let arr = Shape(2, 4, 4, 2, 2);
    let data = random_subs(&mut rng, arr, 28);
    let a = dev.tex(arr, 28, false, Some(&data));
    call1(m(IF_CONTEXT, 46), &[dev.ctx, a, 2, 0, 0, 0, a, 0, 0]);
    let ga = gpu_of(a);
    assert_eq!(page_read(ga, 1 << 16, 4, 4, 0x5400_0001).as_deref(), Some(&data[0][..]), "layer 1 after the copy");
    assert_eq!(page_read(ga, 1 | 1 << 16, 2, 2, 0x5400_0002).as_deref(), Some(&data[3][..]), "layer 1 level 1 kept");
    // refused: level 2 of 2, layer 2 of 2
    assert_eq!(page_read(ga, 2, 1, 1, 0x5400_0003), None);
    assert_eq!(page_read(ga, 2 << 16, 4, 4, 0x5400_0004), None);
    // a 3D texture 4x4x4 of 3 levels (4, 2, 1 slices) to one of another format of the group
    // (R8G8B8A8 UNORM to UINT: through a buffer), every slice of every level
    let vol = Shape(3, 4, 4, 4, 3);
    let vdata = random_subs(&mut rng, vol, 28);
    let v = dev.tex(vol, 28, false, Some(&vdata));
    let v2 = dev.tex(vol, 30, false, None);
    call1(m(IF_CONTEXT, 47), &[dev.ctx, v2, v]);
    let gv2 = gpu_of(v2);
    for (l, (w, slices)) in [(4u32, 4u32), (2, 2), (1, 1)].into_iter().enumerate() {
        let n = (4 * w * w) as usize;
        for z in 0..slices {
            let got = page_read(gv2, l as u32 | z << 16, w, w, 0x5400_0010 + 4 * l as u32 + z);
            assert_eq!(got.as_deref(), Some(&vdata[l][z as usize * n..(z as usize + 1) * n]), "3D level {l} slice {z}");
        }
        assert_eq!(page_read(gv2, l as u32 | slices << 16, w, w, 0x5400_0020 + l as u32), None, "3D level {l} slice {slices}");
    }
    // slices 1-2 of the 3D texture's level 0 to slices 2-3 of another format's (through a buffer)
    let v3 = dev.tex(vol, 30, false, None);
    let bx = mem(24);
    put(bx, &[0, 0, 1, 4, 4, 3]);
    call1(m(IF_CONTEXT, 46), &[dev.ctx, v3, 0, 0, 0, 2, v, 0, bx]);
    let gv3 = gpu_of(v3);
    for z in 0..4u32 {
        let want = if z >= 2 { vdata[0][(z as usize - 1) * 64..z as usize * 64].to_vec() } else { vec![0; 64] };
        assert_eq!(page_read(gv3, z << 16, 4, 4, 0x5400_0028 + z).unwrap(), want, "3D slice {z} after the region copy");
    }
    // a wide one through a buffer (rows of more than 256 bytes)
    let wide = Shape(2, 128, 2, 1, 1);
    let wdata = random_subs(&mut rng, wide, 28);
    let w1 = dev.tex(wide, 28, false, Some(&wdata));
    let w2 = dev.tex(wide, 30, false, None);
    call1(m(IF_CONTEXT, 47), &[dev.ctx, w2, w1]);
    assert_eq!(page_read(gpu_of(w2), 0, 128, 2, 0x5400_002f).unwrap(), wdata[0], "128 wide through a buffer");
    // readbacks of boxes past a level (level 1 is 2x2): refused
    assert_eq!(page_read(ga, 1, 4, 4, 0x5400_0005), None);
    assert_eq!(page_read(ga, 1, 2, 1, 0x5400_0006).map(|v| v.len()), Some(8));
    let rb = mem(64);
    for (k, (x, y, w, h)) in [(1u32, 0u32, 2u32, 1u32), (0, 1, 1, 2)].into_iter().enumerate() {
        unsafe { std::ptr::write_bytes(rb as *mut u8, 0xa5, 64) };
        gpu::pump();
        let req = 0x5400_0007 + k as u32;
        gpu::record(|b| b.cmd(&[gpu::READBACK, req, ga, 1, x, y, w, h, rb, 16]));
        assert!(gpu::wait_readback(req));
        assert!(unsafe { std::slice::from_raw_parts(rb as *const u8, 64) }.iter().all(|&b| b == 0xa5), "box {x},{y} {w}x{h} of a 2x2 level read");
    }
    // page COPY commands past the textures, or stretching layers: nothing
    let other = dev.tex(arr, 28, false, Some(&random_subs(&mut rng, arr, 28)));
    let (g, go) = (gpu_of(a), gpu_of(other));
    let before: Vec<_> = (0..2).map(|k| page_read(ga, k << 16, 4, 4, 0x5400_0030 + k)).collect();
    let vol_before: Vec<_> = (0..2).map(|k| page_read(gv2, 1 | k << 16, 2, 2, 0x5400_0034 + k)).collect();
    for c in [
        [gpu::COPY, go, 2 << 16, 0, 0, 4, 4, g, 0, 0, 0, 4, 4, 0],
        [gpu::COPY, go, 0, 0, 0, 4, 4, g, 2 << 16, 0, 0, 4, 4, 0],
        [gpu::COPY, go, 0, 0, 0, 4, 4, g, 1 << 16, 0, 0, 4, 4, 2 << 16],
        [gpu::COPY, go, 1 << 16, 0, 0, 4, 4, g, 0, 0, 0, 2, 2, 1],
        [gpu::COPY, go, 0, 0, 0, 4, 4, g, 1 << 16, 0, 0, 2, 2, 1],
        [gpu::COPY, go, 0, 0, 0, 4, 4, g, 0, 0, 0, 4, 4, 1 | 2 << 16],
        [gpu::COPY, go, 1 << 16, 0, 0, 4, 4, g, 1 << 16, 0, 0, 2, 2, 1],
        // (level 1 of the 3D textures has 2 slices)
        [gpu::COPY, gpu_of(v), 1 | 2 << 16, 0, 0, 2, 2, gv2, 1, 0, 0, 2, 2, 0],
        [gpu::COPY, gpu_of(v), 1, 0, 0, 2, 2, gv2, 1 | 2 << 16, 0, 0, 2, 2, 0],
    ] {
        gpu::record(|b| b.cmd(&c));
    }
    let after: Vec<_> = (0..2).map(|k| page_read(ga, k << 16, 4, 4, 0x5400_0040 + k)).collect();
    assert_eq!(before, after, "copies past the texture or stretching layers changed it");
    let vol_after: Vec<_> = (0..2).map(|k| page_read(gv2, 1 | k << 16, 2, 2, 0x5400_0044 + k)).collect();
    assert_eq!(vol_before, vol_after, "copies past a 3D level's slices changed it");
    // a stretching copy (Direct3D 9's StretchRect) of 2D textures of one layer: drawn; of an
    // array's layer 0: nothing (above), no WebGPU error
    let flat = Shape(2, 4, 4, 1, 1);
    let (p1, p2) = (dev.tex_bound(flat, 28, 0x28, Some(&random_subs(&mut rng, flat, 28))), dev.tex_bound(flat, 28, 0x28, None));
    gpu::record(|b| b.cmd(&[gpu::COPY, gpu_of(p1), 0, 0, 0, 4, 4, gpu_of(p2), 0, 0, 0, 2, 2, 1]));
    assert_ne!(page_read(gpu_of(p2), 0, 4, 4, 0x5400_0050).unwrap(), vec![0; 64], "a linear stretch drew nothing");
    gpu::record(|b| b.cmd(&[gpu::COPY, go, 0, 0, 0, 4, 4, g, 0, 0, 0, 2, 2, 1]));
    assert_eq!(page_read(ga, 0, 4, 4, 0x5400_0051), before[0], "a stretch into an array's layer drew");
    dev.release(p1);
    dev.release(p2);
    for t in [a, v, v2, v3, w1, w2, other] {
        dev.release(t);
    }
    dev.done();
}

/// ClearRenderTargetView of an array view clears every layer it has, no other (Wine's
/// test_clear_render_target_view_2d: layers 1-3 of 5).
#[test]
fn array_view_clears_its_layers() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    let s = Shape(2, 4, 4, 5, 1);
    let t = dev.tex_bound(s, 28, 0x28, None);
    assert_ne!(t, 0);
    // D3D11_RENDER_TARGET_VIEW_DESC: R8G8B8A8_UNORM, TEXTURE2DARRAY, mip 0, layers 1..4
    let desc = mem(24);
    put(desc, &[28, 5, 0, 1, 3]);
    let out = mem(8);
    assert_eq!(call1(m(IF_DEVICE, 9), &[dev.dev, t, desc, out]), 0);
    let rtv = rd1(out);
    let red = mem(16);
    put(red, &[1f32.to_bits(), 0, 0, 1f32.to_bits()]);
    let g = gpu_of(t);
    call1(m(IF_CONTEXT, 50), &[dev.ctx, rtv, red]);
    for layer in 0..5u32 {
        let got = page_read(g, layer << 16, 4, 4, 0x5500_0000 + layer).unwrap();
        let want: Vec<u8> = if (1..4).contains(&layer) { [255u8, 0, 0, 255].repeat(16) } else { vec![0; 64] };
        assert_eq!(got, want, "layer {layer}");
    }
    call1(m(IF_RTV, 2), &[rtv]);
    dev.release(t);
    dev.done();
}

/// CopyResource between depth formats and their color ones (D16_UNORM / R16_UNORM, D32_FLOAT /
/// R32_FLOAT, one typeless group each), as the winref probe d3d11copy shows Windows doing it: a
/// depth texture's clear (0.375) comes out as its bits, and a color texture's bytes go into a
/// depth texture and back.
#[test]
fn depth_copies_keep_the_bits() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    let s = Shape(2, 4, 4, 1, 1);
    let data: Vec<u8> = (0..64u32).map(|i| (i * 37 + 5) as u8).collect();
    for (depth, color, bpp, clear) in [(55u32, 56u32, 2usize, 0x6000u32), (40, 41, 4, 0.375f32.to_bits())] {
        let want_clear: Vec<u8> = clear.to_le_bytes()[..bpp].repeat(16);
        // (32-bit float depth in 0..1: WebGPU's depth holds no other; Windows keeps any bits)
        let bytes = if bpp == 2 { data[..32].to_vec() } else { (0..16u32).flat_map(|k| (k as f32 / 15.0).to_bits().to_le_bytes()).collect() };
        // a depth texture's clear, to its color format, to itself and to a staging one of each
        let ds = dev.tex_bound(s, depth, 0x40, None);
        assert_ne!(ds, 0, "depth texture {depth}");
        let out = mem(8);
        assert_eq!(call1(m(IF_DEVICE, 10), &[dev.dev, ds, 0, out]), 0);
        let dsv = rd1(out);
        call1(m(IF_CONTEXT, 53), &[dev.ctx, dsv, 1, 0.375f32.to_bits(), 0]);
        for to in [color, depth] {
            let st = dev.tex(s, to, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, st, ds]);
            assert_eq!(dev.read(st, s, to, 0), want_clear, "the clear of {depth} copied to {to}");
            dev.release(st);
        }
        let c = dev.tex(s, color, false, None);
        call1(m(IF_CONTEXT, 47), &[dev.ctx, c, ds]);
        let st = dev.tex(s, color, true, None);
        call1(m(IF_CONTEXT, 47), &[dev.ctx, st, c]);
        assert_eq!(dev.read(st, s, color, 0), want_clear, "the clear of {depth} through a default {color}");
        dev.release(st);
        // a color texture's bytes into a depth one, and back out
        let src = dev.tex(s, color, false, Some(&[bytes.clone()]));
        let ds2 = dev.tex_bound(s, depth, 0x40, None);
        call1(m(IF_CONTEXT, 47), &[dev.ctx, ds2, src]);
        for to in [color, depth] {
            let st = dev.tex(s, to, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, st, ds2]);
            assert_eq!(dev.read(st, s, to, 0), bytes, "{color}'s bytes through depth {depth} to {to}");
            dev.release(st);
        }
        // an integer texture of the group (its bits) whole into the depth one (R32_SINT); a box
        // of one (R32_UINT) to another place of it does nothing, as on Windows (winref
        // d3d11copy's "depth in01 R32_UINT box": a depth texture is copied whole subresources)
        if depth == 40 {
            let si = dev.tex(s, 43, false, Some(&[bytes.clone()]));
            let ds3 = dev.tex_bound(s, depth, 0x40, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, ds3, si]);
            let st = dev.tex(s, color, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, st, ds3]);
            assert_eq!(dev.read(st, s, color, 0), bytes, "R32_SINT into D32_FLOAT");
            for t in [st, ds3, si] {
                dev.release(t);
            }
            // into level 1 of a depth texture of 2 levels (2x2): a box of 2x2 of the color one,
            // nothing; level 0 of another 4x4 one whole, all of it
            let mips = Shape(2, 4, 4, 1, 2);
            let dm = dev.tex_bound(mips, depth, 0x40, None);
            let st = dev.tex(mips, color, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, st, dm]);
            let was = dev.read(st, mips, color, 1);
            dev.release(st);
            let bx = mem(24);
            put(bx, &[2, 2, 0, 4, 4, 1]);
            call1(m(IF_CONTEXT, 46), &[dev.ctx, dm, 1, 0, 0, 0, src, 0, bx]);
            let st = dev.tex(mips, color, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, st, dm]);
            assert_eq!(dev.read(st, mips, color, 1), was, "a box into level 1 of D32_FLOAT: nothing");
            dev.release(st);
            put(bx, &[0, 0, 0, 4, 4, 1]);
            call1(m(IF_CONTEXT, 46), &[dev.ctx, dm, 0, 0, 0, 0, src, 0, bx]);
            let st = dev.tex(mips, color, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, st, dm]);
            assert_eq!(dev.read(st, mips, color, 0), bytes, "the whole box into level 0 of D32_FLOAT");
            dev.release(st);
            dev.release(dm);
            let rev: Vec<u8> = (0..16u32).flat_map(|k| ((15 - k) as f32 / 30.0).to_bits().to_le_bytes()).collect();
            let u = dev.tex(s, 42, false, Some(&[rev.clone()]));
            let bx = mem(24);
            put(bx, &[1, 1, 0, 3, 3, 1]);
            call1(m(IF_CONTEXT, 46), &[dev.ctx, ds2, 0, 2, 0, 0, u, 0, bx]);
            let st = dev.tex(s, color, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, st, ds2]);
            assert_eq!(dev.read(st, s, color, 0), bytes, "a box of R32_UINT into D32_FLOAT: nothing");
            dev.release(st);
            dev.release(u);
        }
        call1(m(IF_DSV, 2), &[dsv]);
        for t in [ds, ds2, c, src] {
            dev.release(t);
        }
    }
    dev.done();
}

/// The page's copies of bits (COPY filter 2) into and out of depth it cannot do, refused with no
/// WebGPU error and nothing changed: of other texel sizes, from depth into 32-bit float depth,
/// stretching, depth24plus-stencil8 either way, from a 3D texture, of more than one layer.
#[test]
fn depth_copies_the_page_refuses() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let dev = Dev::new();
    let mut rng = Rng(0x00de_e9c0_f1e5_u64);
    let s = Shape(2, 4, 4, 1, 1);
    let arr = Shape(2, 4, 4, 2, 1);
    let d32 = dev.tex_bound(s, 40, 0x40, None);
    let d32a = dev.tex_bound(arr, 40, 0x40, None);
    let d16 = dev.tex_bound(s, 55, 0x40, None);
    let d24 = dev.tex_bound(s, 45, 0x40, None);
    let r16f = dev.tex(s, 54, false, Some(&random_subs(&mut rng, s, 54)));
    let r32f = dev.tex(s, 41, false, Some(&random_subs(&mut rng, s, 41)));
    let r32a = dev.tex(arr, 41, false, Some(&random_subs(&mut rng, arr, 41)));
    let vol = dev.tex(Shape(3, 4, 4, 1, 1), 41, false, Some(&random_subs(&mut rng, Shape(3, 4, 4, 1, 1), 41)));
    for t in [d32, d32a, d16, d24, r16f, r32f, r32a, vol] {
        assert_ne!(t, 0);
    }
    let out = mem(8);
    for t in [d32, d32a] {
        assert_eq!(call1(m(IF_DEVICE, 10), &[dev.dev, t, 0, out]), 0);
        call1(m(IF_CONTEXT, 53), &[dev.ctx, rd1(out), 1, 0.5f32.to_bits(), 0]);
        call1(m(IF_DSV, 2), &[rd1(out)]);
    }
    let g = gpu_of;
    let n = std::cell::Cell::new(0x5600_0000u32);
    let read = |t: u32, layer: u32| {
        n.set(n.get() + 1);
        page_read(g(t), layer << 16, 4, 4, n.get())
    };
    let before = [read(d32, 0), read(d32a, 0), read(d32a, 1), read(r32f, 0)];
    assert_eq!(before[0].as_deref(), Some(&0.5f32.to_bits().to_le_bytes().repeat(16)[..]), "the clear of D32");
    for c in [
        [gpu::COPY, g(r16f), 0, 0, 0, 4, 4, g(d32), 0, 0, 0, 4, 4, 2],
        [gpu::COPY, g(d16), 0, 0, 0, 4, 4, g(d32), 0, 0, 0, 4, 4, 2],
        [gpu::COPY, g(r32f), 0, 0, 0, 4, 4, g(d32), 0, 0, 0, 2, 2, 2],
        [gpu::COPY, g(r32f), 0, 0, 0, 4, 4, g(d32), 0, 0, 0, 4, 2, 2],
        [gpu::COPY, g(r32f), 0, 0, 0, 4, 4, g(d32), 0, 0, 0, 2, 4, 2],
        [gpu::COPY, g(d24), 0, 0, 0, 4, 4, g(r32f), 0, 0, 0, 4, 4, 2],
        [gpu::COPY, g(r32f), 0, 0, 0, 4, 4, g(d24), 0, 0, 0, 4, 4, 2],
        [gpu::COPY, g(vol), 0, 0, 0, 4, 4, g(d32), 0, 0, 0, 4, 4, 2],
        [gpu::COPY, g(r32a), 0, 0, 0, 4, 4, g(d32a), 0, 0, 0, 4, 4, 2 | 2 << 16],
    ] {
        gpu::record(|b| b.cmd(&c));
    }
    let after = [read(d32, 0), read(d32a, 0), read(d32a, 1), read(r32f, 0)];
    assert_eq!(before, after, "a refused copy changed a texture");
    for t in [d32, d32a, d16, d24, r16f, r32f, r32a, vol] {
        dev.release(t);
    }
    dev.done();
}

/// winref d3d11copy's subresources() on the same inputs, against Windows' hashes
/// (tests/winref/golden/d3d11copy-32): every subresource after CopyResource, the
/// CopySubresourceRegion box, and an UpdateSubresource of the staging copy (nothing on Windows).
#[test]
fn subresources_hash_as_windows() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *D11S.lock().unwrap() = None;
    let golden = std::fs::read_to_string(format!("{}/../../tests/winref/golden/d3d11copy-32/stdout.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let dev = Dev::new();
    // the probe's subfmts: (source, destination, name, bytes per block, block)
    let fmts: [(u32, u32, &str); 18] = [
        (28, 28, "R8G8B8A8_UNORM"), (41, 41, "R32_FLOAT"), (11, 11, "R16G16B16A16_UNORM"), (58, 58, "R16_SNORM"),
        (85, 85, "B5G6R5_UNORM"), (65, 65, "A8_UNORM"), (71, 71, "BC1_UNORM"), (77, 77, "BC3_UNORM"), (98, 98, "BC7_UNORM"),
        (2, 2, "R32G32B32A32_FLOAT"), (12, 11, "R16G16B16A16_UINT>UNORM"), (11, 12, "R16G16B16A16_UNORM>UINT"),
        (57, 58, "R16_UINT>SNORM"), (58, 57, "R16_SNORM>UINT"), (10, 11, "R16G16B16A16_FLOAT>UNORM"), (71, 72, "BC1_UNORM>SRGB"),
        (2, 3, "R32G32B32A32_FLOAT>UINT"), (0, 0, ""),
    ];
    let fnv = |v: &[u8], s: Shape, f: u32, sub: u32| {
        let fi = format::info(f).unwrap();
        let (pitch, rows, slices) = s.layout(f, sub);
        let w = ((s.1 >> (sub % s.4)).max(1) + fi.block - 1) / fi.block * fi.bpp;
        let mut h = 2166136261u32;
        for z in 0..slices {
            for y in 0..rows {
                for x in 0..w {
                    h = (h ^ v[((z * rows + y) * pitch + x) as usize] as u32).wrapping_mul(16777619);
                }
            }
        }
        h
    };
    let (mut n, mut bad) = (0, Vec::new());
    for shape in [Shape(2, 8, 8, 3, 4), Shape(3, 8, 4, 4, 3)] {
        let dim = shape.0;
        for (i, &(f, g, name)) in fmts.iter().enumerate().filter(|(_, x)| x.0 != 0) {
            let data: Vec<Vec<u8>> = (0..shape.subs())
                .map(|s| {
                    let (pitch, rows, slices) = shape.layout(f, s);
                    (0..pitch * rows * slices).map(|k| ((k * 131 + s * 29 + 7) ^ (k >> 3) ^ (i as u32 * 17)) as u8).collect()
                })
                .collect();
            let src = dev.tex(shape, f, false, Some(&data));
            let (dst, dst2) = (dev.tex(shape, g, true, None), dev.tex(shape, g, true, None));
            let mut check = |what: &str, t: u32, sub: u32| {
                let line = format!("{what} {name} {dim}d sub {sub} {:08x}", fnv(&dev.read(t, shape, g, sub), shape, g, sub));
                n += 1;
                if !golden.lines().any(|l| l == line) {
                    bad.push(line);
                }
            };
            call1(m(IF_CONTEXT, 47), &[dev.ctx, dst, src]);
            for s in 0..shape.subs() {
                check("resource", dst, s);
            }
            let (dy, ssub) = if dim == 3 { (0, 0) } else { (4, 8) };
            let bx = mem(24);
            put(bx, &[4, 0, if dim == 3 { 1 } else { 0 }, 8, 4, if dim == 3 { 3 } else { 1 }]);
            call1(m(IF_CONTEXT, 46), &[dev.ctx, dst2, 0, 0, dy, 0, src, ssub, bx]);
            check("region", dst2, 0);
            let fi = format::info(f).unwrap();
            let (z, depth, row, nrows) = (if dim == 3 { 1 } else { 0 }, if dim == 3 { 2 } else { 1 }, 4 / fi.block * fi.bpp, 4 / fi.block);
            let up: Vec<u8> = (0..row * nrows * depth).map(|k| (k * 53 + i as u32) as u8).collect();
            let p = mem(up.len() as u32);
            put_bytes(p, &up);
            put(bx, &[4, dy, z, 8, dy + 4, z + depth]);
            call1(m(IF_CONTEXT, 48), &[dev.ctx, dst, 0, bx, p, row, row * nrows]);
            check("update", dst, 0);
            for t in [src, dst, dst2] {
                dev.release(t);
            }
        }
    }
    dev.done();
    assert!(n > 300, "{n} hashes");
    assert!(bad.is_empty(), "{} of {n} differ from Windows (not in the golden):\n{}", bad.len(), bad.join("\n"));
}
