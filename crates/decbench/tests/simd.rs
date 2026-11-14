//! sake's SIMD128 lanes against their scalar code, byte for byte. Meant for wasm32 with
//! +simd128 (tests/decbench/run.sh test): natively both sides are the scalar code and this
//! only checks the harness.
use kura::video::{convert, convert_scalar, Out};

const I420: u32 = 0;
const NV12: u32 = 1;
const RGBA: u32 = 2;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A track's layout at `at` in a buffer (planes padded by `pad`), and its bytes.
fn track(fmt: u32, w: u32, h: u32, pad: u32, at: u32) -> ([u32; 7], u32) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    match fmt {
        I420 => {
            let (ys, cs) = (w + pad, cw + pad);
            ([I420, at, ys, at + ys * h, cs, at + ys * h + cs * ch, cs], ys * h + 2 * cs * ch)
        }
        NV12 => {
            let (ys, cs) = (w + pad, 2 * cw + pad);
            ([NV12, at, ys, at + ys * h, cs, 0, 0], ys * h + cs * ch)
        }
        _ => {
            let s = 4 * w + pad;
            ([RGBA, at, s, 0, 0, 0, 0], s * h)
        }
    }
}

fn check(r: &mut Rng, cases: usize, max_w: u64) {
    for i in 0..cases {
        let (w, h, pad) = (1 + r.below(max_w) as u32, 1 + r.below(5) as u32, r.below(20) as u32);
        let color = [I420, NV12, RGBA][r.below(3) as usize];
        let tracks = 1 + r.below(2) as u32;
        let out = [Out::Ycc, Out::Rgb, Out::Rgb601][r.below(3) as usize];
        let (c, n) = track(color, w, h, pad, r.below(3) as u32);
        let (a, an) = if tracks > 1 { track([I420, NV12, RGBA][r.below(3) as usize], w, h, pad, n + 3) } else { ([0; 7], 0) };
        // (mostly random bytes, some runs of a value: chroma near a step for whole groups)
        let fill = r.below(4);
        let buf: Vec<u8> = (0..n + an + 32).map(|k| if fill == 0 { (k / 7) as u8 } else { r.next() as u8 }).collect();
        let layout = [c, a];
        let pitch = 4 * w as usize + r.below(9) as usize;
        let (mut x, mut y) = (vec![0xa5u8; pitch * h as usize + 4], vec![0xa5u8; pitch * h as usize + 4]);
        let ow = w - r.below(w as u64 / 4 + 1) as u32;
        unsafe {
            convert(buf.as_ptr(), &layout, tracks, w, h, x.as_mut_ptr(), pitch, ow, h, out);
            convert_scalar(buf.as_ptr(), &layout, tracks, w, h, y.as_mut_ptr(), pitch, ow, h, out);
        }
        assert!(x == y, "case {i}: {w}x{h} pad {pad} color {color} tracks {tracks} {out:?} out {ow}");
    }
}

#[test]
fn movie_frames_random() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    check(&mut r, 3000, 70);
    check(&mut r, 200, 1300);
}

/// Every Y, Cb, Cr in both RGB outputs and Ycc: a 256x2 picture of every Y per (Cb, Cr) pair
/// (I420 and NV12).
#[test]
fn every_ycc() {
    for out in [Out::Rgb601, Out::Rgb, Out::Ycc] {
        for nv12 in [false, true] {
            let mut x = vec![0u8; 256 * 2 * 4];
            let mut y = x.clone();
            for cb in 0..=255u8 {
                for cr in 0..=255u8 {
                    let mut b: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
                    let layout = if nv12 {
                        b.extend((0..128).flat_map(|_| [cb, cr]));
                        [NV12, 0, 256, 512, 256, 0, 0]
                    } else {
                        b.extend((0..128).map(|_| cb));
                        b.extend((0..128).map(|_| cr));
                        [I420, 0, 256, 512, 128, 640, 128]
                    };
                    unsafe {
                        convert(b.as_ptr(), &[layout, [0; 7]], 1, 256, 2, x.as_mut_ptr(), 1024, 256, 2, out);
                        convert_scalar(b.as_ptr(), &[layout, [0; 7]], 1, 256, 2, y.as_mut_ptr(), 1024, 256, 2, out);
                    }
                    assert!(x == y, "{out:?} nv12 {nv12} cb {cb} cr {cr}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// crates/mpeg1 (natively: only the scalar code against itself)

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use mpeg1::simd;

/// Every case of motion compensation (block size, half-pel h / v, interpolated) at random
/// places of random planes.
#[test]
fn mpeg1_motion() {
    let mut r = Rng(77);
    for _ in 0..20000 {
        let bs = [8usize, 16][r.below(2) as usize];
        let dw = bs * (1 + r.below(6) as usize) + r.below(3) as usize;
        let len = dw * (bs + 2) + 64;
        let s: Vec<u8> = (0..len).map(|_| r.next() as u8).collect();
        let d0: Vec<u8> = (0..len).map(|_| r.next() as u8).collect();
        let (oh, ov, interp) = (r.below(2) == 1, r.below(2) == 1, r.below(2) == 1);
        let room = len - ((bs - 1 + 1) * dw + bs + 1);
        let (si, di) = (r.below(room as u64) as usize, r.below((len - ((bs - 1) * dw + bs)) as u64) as usize);
        let (mut a, mut b) = (d0.clone(), d0.clone());
        mpeg1::video::process_rows(&s, &mut a, si, di, dw, bs, oh, ov, interp);
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        unsafe {
            simd::process_rows(&s, &mut b, si, di, dw, bs, oh, ov, interp)
        };
        #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
        mpeg1::video::process_rows(&s, &mut b, si, di, dw, bs, oh, ov, interp);
        assert!(a == b, "bs {bs} dw {dw} si {si} di {di} {oh} {ov} {interp}");
    }
}

/// The IDCT and its add on random blocks: small (as streams have), any i32 (corrupt data:
/// wrapping, saturation), sparse.
#[test]
fn mpeg1_idct() {
    let mut r = Rng(5);
    for i in 0..50000 {
        let kind = i % 3;
        let blk: [i32; 64] = std::array::from_fn(|_| match kind {
            0 => (r.below(4096) as i32) - 2048,
            1 => r.next() as i32,
            _ => if r.below(8) == 0 { (r.below(600) as i32) - 300 } else { 0 },
        });
        let dw = 8 + r.below(24) as usize;
        let d0: Vec<u8> = (0..dw * 8 + 8).map(|_| r.next() as u8).collect();
        let di = r.below(8) as usize;
        let intra = r.below(2) == 1;
        let (mut a, mut b) = (d0.clone(), d0.clone());
        let (mut x, mut y) = (blk, blk);
        mpeg1::video::idct_put_scalar(&mut x, &mut a, di, dw, intra);
        mpeg1::video::idct_put(&mut y, &mut b, di, dw, intra);
        assert!(a == b && x == y, "block {blk:?} intra {intra}");
    }
}

/// RGB32 of random pictures: sizes, strides, bottom-up or not, short outputs.
#[test]
fn mpeg1_bgrx() {
    let mut r = Rng(11);
    for _ in 0..3000 {
        let (w, h) = (1 + r.below(90) as usize, 1 + r.below(9) as usize);
        let ys = w.next_multiple_of(16) + 16 * r.below(2) as usize;
        let hh = h.next_multiple_of(16);
        let mut bytes = |n: usize| (0..n).map(|_| r.next() as u8).collect::<Vec<u8>>();
        let f = mpeg1::Frame { width: w, height: h, y_stride: ys, c_stride: ys / 2, y: bytes(ys * hh), cb: bytes(ys / 2 * hh / 2), cr: bytes(ys / 2 * hh / 2) };
        let stride = w * 4 + 4 * r.below(3) as usize;
        let len = stride * h - if r.below(3) == 0 { r.below((stride * h) as u64 / 2 + 1) as usize } else { 0 };
        let bottom_up = r.below(2) == 1;
        let (mut a, mut b) = (vec![7u8; len], vec![7u8; len]);
        mpeg1::video::to_bgrx_scalar(&f, &mut a, stride, bottom_up);
        mpeg1::video::to_bgrx(&f, &mut b, stride, bottom_up);
        assert!(a == b, "{w}x{h} stride {stride} len {len} bottom_up {bottom_up}");
    }
}

// ---------------------------------------------------------------------------------------------
// DXT textures (kura::d3d9::format::convert): whole blocks in SIMD, edge blocks one by one

#[test]
fn dxt_textures() {
    use kura::d3d9::format as f;
    let mut r = Rng(3);
    for i in 0..600 {
        let fmt = [f::DXT1, f::DXT2, f::DXT3, f::DXT4, f::DXT5][i % 5];
        let bs = if fmt == f::DXT1 { 8 } else { 16 };
        let (w, h) = (1 + r.below(40) as usize, 1 + r.below(24) as usize);
        let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
        let pitch = bw * bs + r.below(3) as usize * 8;
        // (random blocks, some with c0 <= c1 / a0 <= a1 picked on purpose)
        let mut src: Vec<u8> = (0..pitch * bh).map(|_| r.next() as u8).collect();
        for k in 0..bw * bh {
            if r.below(2) == 0 {
                let at = (k / bw) * pitch + (k % bw) * bs + bs - 8;
                src.swap(at, at + 2);
                src.swap(at + 1, at + 3);
                if bs == 16 {
                    src.swap(at - 8, at - 7);
                }
            }
        }
        let mut got = Vec::new();
        unsafe { f::convert(fmt, src.as_ptr(), pitch, w as u32, h as u32, &mut got) };
        let mut want = vec![0u8; w * h * 4];
        for by in 0..bh {
            for bx in 0..bw {
                let px = f::decode_dxt(fmt, &src[by * pitch + bx * bs..][..bs]);
                for (k, p) in px.iter().enumerate() {
                    let (x, y) = (bx * 4 + k % 4, by * 4 + k / 4);
                    if x < w && y < h {
                        want[(y * w + x) * 4..][..4].copy_from_slice(p);
                    }
                }
            }
        }
        assert!(got == want, "fmt {fmt:x} {w}x{h}");
    }
}
