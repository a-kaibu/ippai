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

/// What Windows gives back of `bytes` written as `fmt`: the bytes, but the X of B8G8R8X8 (the
/// page keeps alpha 1 there; Windows' answer comes with the winref probe d3d11copy)
fn seen(fmt: u32, bytes: &[u8]) -> Vec<u8> {
    let mut v = bytes.to_vec();
    if matches!(format::info(fmt), Some(f) if f.no_alpha) {
        for px in v.chunks_exact_mut(4) {
            px[3] = 0;
        }
    }
    v
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
        assert_eq!(seen(f, &dev.read(st, s, f, 0)), seen(f, &data[0]), "initial data of format {f}");
        let src = dev.tex(s, f, false, Some(&data));
        // (P8 and A8P8: no shader resource of them, a default texture bound to nothing)
        let src = if src == 0 && matches!(f, 113 | 114) { dev.tex_bound(s, f, 0, Some(&data)) } else { src };
        assert_ne!(src, 0, "texture of format {f}");
        for &g in fmts.iter().filter(|&&g| format::info(g).unwrap().bpp == format::info(f).unwrap().bpp && format::info(g).unwrap().block == format::info(f).unwrap().block) {
            let dst = dev.tex(s, g, true, None);
            call1(m(IF_CONTEXT, 47), &[dev.ctx, dst, src]);
            let got = dev.read(dst, s, g, 0);
            let want = if format::copy_compatible(f, g) { seen(g, &data[0]) } else { vec![0; got.len()] };
            assert_eq!(seen(g, &got), want, "CopyResource {f} -> {g}");
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
                assert_eq!(seen(g, &dev.read(dst, shape, g, sub)), seen(f, &data[sub as usize]), "CopyResource {f} -> {g}, {} sub {sub}", shape.0);
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
            assert_eq!(seen(g, &got), seen(f, &want), "CopySubresourceRegion {f} -> {g}, {}", shape.0);
            // UpdateSubresource of the same box into the staging copy's level 0
            let up = rng.bytes((row * n * depth) as usize);
            let p = mem(up.len() as u32);
            put_bytes(p, &up);
            put(bx, &[4, dy, z_src, 8, dy + 4, z_src + depth]);
            call1(m(IF_CONTEXT, 48), &[dev.ctx, dst, 0, bx, p, row, row * n]);
            let mut want = data[0].clone();
            for z in 0..depth {
                for r in 0..n {
                    let to = (((z_src + z) * rows + dy / fi.block + r) * pitch + bx0) as usize;
                    let from = ((z * n + r) * row) as usize;
                    want[to..to + row as usize].copy_from_slice(&up[from..from + row as usize]);
                }
            }
            assert_eq!(seen(g, &dev.read(dst, shape, g, 0)), seen(f, &want), "UpdateSubresource {g}, {}", shape.0);
            for t in [src, dst, dst2] {
                dev.release(t);
            }
        }
    }
    dev.done();
}
