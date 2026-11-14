//! wasm SIMD128 versions of the hot pixel loops: motion compensation (hpel, VC-1 bicubic and
//! chroma bilinear), the WMV 2 and VC-1 8x8 inverse transforms, and the clamped put / add of a
//! block. Each is bit for bit its scalar function's (the `*_c` ones), which the tests check
//! under node (`wasm-test.sh`). They are built only with `target_feature = "simd128"`; the
//! scalar functions call them where they apply.

use core::arch::wasm32::*;

// ---------------------------------------------------------------------------------------------
// loads and stores, bounds checked on the slice

#[inline(always)]
fn ld8(s: &[u8], o: usize) -> v128 {
    let s = &s[o..o + 8];
    // SAFETY: 8 bytes in bounds (checked above); wasm loads need no alignment
    unsafe { v128_load64_zero(s.as_ptr() as *const u64) }
}

#[inline(always)]
fn ld16(s: &[u8], o: usize) -> v128 {
    let s = &s[o..o + 16];
    // SAFETY: as ld8
    unsafe { v128_load(s.as_ptr() as *const v128) }
}

#[inline(always)]
fn st8(d: &mut [u8], o: usize, v: v128) {
    let d = &mut d[o..o + 8];
    // SAFETY: as ld8
    unsafe { v128_store64_lane::<0>(v, d.as_mut_ptr() as *mut u64) }
}

#[inline(always)]
fn st16(d: &mut [u8], o: usize, v: v128) {
    let d = &mut d[o..o + 16];
    // SAFETY: as ld8
    unsafe { v128_store(d.as_mut_ptr() as *mut v128, v) }
}

#[inline(always)]
fn ldw(s: &[i16], o: usize) -> v128 {
    let s = &s[o..o + 8];
    // SAFETY: as ld8
    unsafe { v128_load(s.as_ptr() as *const v128) }
}

#[inline(always)]
fn stw(d: &mut [i16], o: usize, v: v128) {
    let d = &mut d[o..o + 8];
    // SAFETY: as ld8
    unsafe { v128_store(d.as_mut_ptr() as *mut v128, v) }
}

/// 8 bytes from `o` as i16 lanes
#[inline(always)]
fn ld8w(s: &[u8], o: usize) -> v128 {
    u16x8_extend_low_u8x16(ld8(s, o))
}

/// The low 16 bits of each i32 lane of (lo, hi) as 8 i16 lanes (C's (int16_t) truncation)
#[inline(always)]
fn trunc16(lo: v128, hi: v128) -> v128 {
    i8x16_shuffle::<0, 1, 4, 5, 8, 9, 12, 13, 16, 17, 20, 21, 24, 25, 28, 29>(lo, hi)
}

/// i32 lanes clamped to 0..=255, as the low 8 bytes
#[inline(always)]
fn clip_i32(lo: v128, hi: v128) -> v128 {
    u8x16_narrow_i16x8(i16x8_narrow_i32x4(lo, hi), i16x8_splat(0))
}

/// The 8x8 i16 transpose of rows `r`
#[inline(always)]
fn transpose(r: [v128; 8]) -> [v128; 8] {
    let t0 = i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(r[0], r[1]);
    let t1 = i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(r[0], r[1]);
    let t2 = i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(r[2], r[3]);
    let t3 = i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(r[2], r[3]);
    let t4 = i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(r[4], r[5]);
    let t5 = i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(r[4], r[5]);
    let t6 = i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(r[6], r[7]);
    let t7 = i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(r[6], r[7]);
    let u0 = i32x4_shuffle::<0, 4, 1, 5>(t0, t2);
    let u1 = i32x4_shuffle::<2, 6, 3, 7>(t0, t2);
    let u2 = i32x4_shuffle::<0, 4, 1, 5>(t1, t3);
    let u3 = i32x4_shuffle::<2, 6, 3, 7>(t1, t3);
    let u4 = i32x4_shuffle::<0, 4, 1, 5>(t4, t6);
    let u5 = i32x4_shuffle::<2, 6, 3, 7>(t4, t6);
    let u6 = i32x4_shuffle::<0, 4, 1, 5>(t5, t7);
    let u7 = i32x4_shuffle::<2, 6, 3, 7>(t5, t7);
    [
        i64x2_shuffle::<0, 2>(u0, u4),
        i64x2_shuffle::<1, 3>(u0, u4),
        i64x2_shuffle::<0, 2>(u1, u5),
        i64x2_shuffle::<1, 3>(u1, u5),
        i64x2_shuffle::<0, 2>(u2, u6),
        i64x2_shuffle::<1, 3>(u2, u6),
        i64x2_shuffle::<0, 2>(u3, u7),
        i64x2_shuffle::<1, 3>(u3, u7),
    ]
}

fn load_block(b: &[i16; 64]) -> [v128; 8] {
    core::array::from_fn(|k| ldw(b, k * 8))
}

fn store_block(b: &mut [i16; 64], v: [v128; 8]) {
    for (k, v) in v.into_iter().enumerate() {
        stw(b, k * 8, v);
    }
}

// ---------------------------------------------------------------------------------------------
// motion compensation

/// hpel::hpel_c for blocks 8 or 16 wide (false: not done, any other width)
pub fn hpel(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], ss: usize, w: usize, h: usize, dxy: usize, rnd: bool) -> bool {
    if w != 8 && w != 16 {
        return false;
    }
    let one = u8x16_splat(1);
    for y in 0..h {
        let (a, b) = (y * ss, (y + 1) * ss);
        let ld = |o: usize| if w == 8 { ld8(src, o) } else { ld16(src, o) };
        let v = match dxy & 3 {
            0 => ld(a),
            1 | 2 => {
                let (p, q) = (ld(a), if dxy & 3 == 1 { ld(a + 1) } else { ld(b) });
                let r = u8x16_avgr(p, q);
                // (avgr rounds up: (p + q) >> 1 is that less the low bit of p ^ q)
                if rnd { r } else { u8x16_sub(r, v128_and(v128_xor(p, q), one)) }
            }
            _ => {
                let (p, q, s, t) = (ld(a), ld(a + 1), ld(b), ld(b + 1));
                let k = u16x8_splat(1 + rnd as u16);
                let lo = u16x8_add(u16x8_add(u16x8_extend_low_u8x16(p), u16x8_extend_low_u8x16(q)), u16x8_add(u16x8_extend_low_u8x16(s), u16x8_extend_low_u8x16(t)));
                let hi = u16x8_add(u16x8_add(u16x8_extend_high_u8x16(p), u16x8_extend_high_u8x16(q)), u16x8_add(u16x8_extend_high_u8x16(s), u16x8_extend_high_u8x16(t)));
                u8x16_narrow_i16x8(u16x8_shr(u16x8_add(lo, k), 2), u16x8_shr(u16x8_add(hi, k), 2))
            }
        };
        if w == 8 { st8(dst, doff + y * ds, v) } else { st16(dst, doff + y * ds, v) }
    }
    true
}

/// vc1::chroma_mc8_c
pub fn chroma_mc8(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], ss: usize, x: i32, y: i32, no_rnd: bool) {
    let (a, b, c, d) = ((8 - x) * (8 - y), x * (8 - y), (8 - x) * y, x * y);
    let (a, b, c, d) = (i16x8_splat(a as i16), i16x8_splat(b as i16), i16x8_splat(c as i16), i16x8_splat(d as i16));
    let r = i16x8_splat(if no_rnd { 28 } else { 32 });
    for j in 0..8 {
        let o = j * ss;
        let s = i16x8_add(i16x8_add(i16x8_mul(a, ld8w(src, o)), i16x8_mul(b, ld8w(src, o + 1))), i16x8_add(i16x8_mul(c, ld8w(src, o + ss)), i16x8_mul(d, ld8w(src, o + ss + 1))));
        // (at most 64 * 255 + 32: in range of u16, and so of the logical shift)
        let v = u16x8_shr(i16x8_add(s, r), 6);
        st8(dst, doff + j * ds, u8x16_narrow_i16x8(v, v));
    }
}

/// The VC-1 bicubic taps of `mode` (mspel16)
fn taps(mode: usize) -> [i16; 4] {
    match mode {
        1 => [-4, 53, 18, -3],
        2 => [-1, 9, 9, -1],
        _ => [-3, 18, 53, -4],
    }
}

/// The 4-tap filter of 8 pixels in i16 (no overflow: at most 71 * 255)
#[inline(always)]
fn tap8(src: &[u8], p: usize, step: usize, t: [i16; 4]) -> v128 {
    let m = |k: usize, c: i16| i16x8_mul(ld8w(src, p + k * step - step), i16x8_splat(c));
    i16x8_add(i16x8_add(m(0, t[0]), m(1, t[1])), i16x8_add(m(2, t[2]), m(3, t[3])))
}

/// vc1::mspel_mc_c (n 8 or 16), the filtered cases
pub fn mspel_mc(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], so: usize, ss: usize, n: usize, hmode: usize, vmode: usize, rnd: i32) {
    // (columns in chunks of 8; a last chunk that would go past `w` is moved back over the one
    // before, as each lane is its own column)
    let chunks = |w: usize| (0..w.div_ceil(8)).map(move |c| (c * 8).min(w - 8));
    if vmode != 0 && hmode != 0 {
        const SHIFT: [i32; 4] = [0, 5, 1, 5];
        let shift = ((SHIFT[hmode] + SHIFT[vmode]) >> 1) as u32;
        let w = n + 3;
        let mut tmp = [0i16; 19 * 16];
        let r = i16x8_splat(((1 << (shift - 1)) + rnd - 1) as i16);
        let tv = taps(vmode);
        for j in 0..n {
            for i in chunks(w) {
                let v = tap8(src, so - 1 + j * ss + i, ss, tv);
                stw(&mut tmp, j * w + i, i16x8_shr(i16x8_add(v, r), shift));
            }
        }
        let r = i32x4_splat(64 - rnd);
        let th = taps(hmode);
        for j in 0..n {
            for i in chunks(n) {
                let p = j * w + 1 + i;
                let (mut lo, mut hi) = (r, r);
                for (k, &c) in th.iter().enumerate() {
                    let v = ldw(&tmp, p + k - 1);
                    lo = i32x4_add(lo, i32x4_extmul_low_i16x8(v, i16x8_splat(c)));
                    hi = i32x4_add(hi, i32x4_extmul_high_i16x8(v, i16x8_splat(c)));
                }
                st8(dst, doff + j * ds + i, clip_i32(i32x4_shr(lo, 7), i32x4_shr(hi, 7)));
            }
        }
        return;
    }
    let (mode, step, r) = if vmode != 0 { (vmode, ss, 1 - rnd) } else { (hmode, 1, rnd) };
    let (add, shift) = if mode == 2 { (8 - r, 4) } else { (32 - r, 6) };
    let add = i16x8_splat(add as i16);
    let t = taps(mode);
    for j in 0..n {
        for i in chunks(n) {
            let v = i16x8_shr(i16x8_add(tap8(src, so + j * ss + i, step, t), add), shift);
            st8(dst, doff + j * ds + i, u8x16_narrow_i16x8(v, v));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// transforms

/// idct::wmv2_c (ff_wmv2_idct_c), in place
pub fn wmv2(b: &mut [i16; 64]) {
    // rows: the lanes are rows, so the block is transposed for them, and back for the columns
    let t = transpose(load_block(b));
    let rows = wmv2_pass(t, false);
    store_block(b, wmv2_pass(transpose(rows), true));
}

/// One wmv2 pass over 8 lanes: `v[k]` coefficient k of each lane's row / column.
#[inline(always)]
fn wmv2_pass(v: [v128; 8], col: bool) -> [v128; 8] {
    const W: [i32; 8] = [2048, 2841, 2676, 2408, 2048, 1609, 1108, 565];
    let half = |v: v128, hi: bool| if hi { i32x4_extend_high_i16x8(v) } else { i32x4_extend_low_i16x8(v) };
    let mut out = [[i32x4_splat(0); 2]; 8];
    for (h, hi) in [false, true].into_iter().enumerate() {
        let r: [v128; 8] = core::array::from_fn(|k| half(v[k], hi));
        let m = |c: i32, x: v128| i32x4_mul(i32x4_splat(c), x);
        let (four, three) = (i32x4_splat(if col { 4 } else { 0 }), if col { 3 } else { 0 });
        let sh = |x: v128| i32x4_shr(i32x4_add(x, four), three);
        let a1 = sh(i32x4_add(m(W[1], r[1]), m(W[7], r[7])));
        let a7 = sh(i32x4_sub(m(W[7], r[1]), m(W[1], r[7])));
        let a5 = sh(i32x4_add(m(W[5], r[5]), m(W[3], r[3])));
        let a3 = sh(i32x4_sub(m(W[3], r[5]), m(W[5], r[3])));
        let a2 = sh(i32x4_add(m(W[2], r[2]), m(W[6], r[6])));
        let a6 = sh(i32x4_sub(m(W[6], r[2]), m(W[2], r[6])));
        let a0 = i32x4_shr(i32x4_add(m(W[0], r[0]), m(W[0], r[4])), three);
        let a4 = i32x4_shr(i32x4_sub(m(W[0], r[0]), m(W[0], r[4])), three);
        let k128 = i32x4_splat(128);
        let s1 = i32x4_shr(i32x4_add(m(181, i32x4_add(i32x4_sub(a1, a5), i32x4_sub(a7, a3))), k128), 8);
        let s2 = i32x4_shr(i32x4_add(m(181, i32x4_sub(i32x4_sub(a1, a5), i32x4_sub(a7, a3))), k128), 8);
        let o = [
            i32x4_add(i32x4_add(a0, a2), i32x4_add(a1, a5)),
            i32x4_add(i32x4_add(a4, a6), s1),
            i32x4_add(i32x4_sub(a4, a6), s2),
            i32x4_add(i32x4_sub(a0, a2), i32x4_add(a7, a3)),
            i32x4_sub(i32x4_sub(a0, a2), i32x4_add(a7, a3)),
            i32x4_sub(i32x4_sub(a4, a6), s2),
            i32x4_sub(i32x4_add(a4, a6), s1),
            i32x4_sub(i32x4_add(a0, a2), i32x4_add(a1, a5)),
        ];
        let (rnd, s) = if col { (1 << 13, 14) } else { (1 << 7, 8) };
        for k in 0..8 {
            out[k][h] = i32x4_shr(i32x4_add(o[k], i32x4_splat(rnd)), s);
        }
    }
    core::array::from_fn(|k| trunc16(out[k][0], out[k][1]))
}

/// vc1::inv_trans_8x8_c, in place
pub fn vc1_8x8(b: &mut [i16; 64]) {
    let d = vc1_pass(load_block(b), false);
    store_block(b, vc1_pass(transpose(d), true));
}

/// One VC-1 8-point pass over 8 lanes, `v[k]` coefficient k of each lane
#[inline(always)]
fn vc1_pass(v: [v128; 8], second: bool) -> [v128; 8] {
    let half = |v: v128, hi: bool| if hi { i32x4_extend_high_i16x8(v) } else { i32x4_extend_low_i16x8(v) };
    let mut out = [[i32x4_splat(0); 2]; 8];
    for (h, hi) in [false, true].into_iter().enumerate() {
        let s: [v128; 8] = core::array::from_fn(|k| half(v[k], hi));
        let m = |c: i32, x: v128| i32x4_mul(i32x4_splat(c), x);
        let rnd = i32x4_splat(if second { 64 } else { 4 });
        let t1 = i32x4_add(m(12, i32x4_add(s[0], s[4])), rnd);
        let t2 = i32x4_add(m(12, i32x4_sub(s[0], s[4])), rnd);
        let t3 = i32x4_add(m(16, s[2]), m(6, s[6]));
        let t4 = i32x4_sub(m(6, s[2]), m(16, s[6]));
        let (t5, t6, t7, t8) = (i32x4_add(t1, t3), i32x4_add(t2, t4), i32x4_sub(t2, t4), i32x4_sub(t1, t3));
        let u1 = i32x4_add(i32x4_add(m(16, s[1]), m(15, s[3])), i32x4_add(m(9, s[5]), m(4, s[7])));
        let u2 = i32x4_sub(i32x4_sub(m(15, s[1]), m(4, s[3])), i32x4_add(m(16, s[5]), m(9, s[7])));
        let u3 = i32x4_add(i32x4_sub(m(9, s[1]), m(16, s[3])), i32x4_add(m(4, s[5]), m(15, s[7])));
        let u4 = i32x4_sub(i32x4_add(i32x4_sub(m(4, s[1]), m(9, s[3])), m(15, s[5])), m(16, s[7]));
        let o = [
            i32x4_add(t5, u1),
            i32x4_add(t6, u2),
            i32x4_add(t7, u3),
            i32x4_add(t8, u4),
            i32x4_sub(t8, u4),
            i32x4_sub(t7, u3),
            i32x4_sub(t6, u2),
            i32x4_sub(t5, u1),
        ];
        for k in 0..8 {
            out[k][h] = if second {
                let one = i32x4_splat((k >= 4) as i32);
                i32x4_shr(i32x4_add(o[k], one), 7)
            } else {
                i32x4_shr(o[k], 3)
            };
        }
    }
    core::array::from_fn(|k| trunc16(out[k][0], out[k][1]))
}

/// `b` clamped to 0..=255 into the 8x8 at `off` (put_pixels_clamped)
pub fn put8x8(dst: &mut [u8], off: usize, stride: usize, b: &[i16; 64]) {
    for y in 0..8 {
        let v = ldw(b, y * 8);
        st8(dst, off + y * stride, u8x16_narrow_i16x8(v, v));
    }
}

/// `b` added to the 8x8 at `off`, clamped (add_pixels_clamped)
pub fn add8x8(dst: &mut [u8], off: usize, stride: usize, b: &[i16; 64]) {
    for y in 0..8 {
        let o = off + y * stride;
        // (saturating: a sum past i16 is past 255 too)
        let v = i16x8_add_sat(ld8w(dst, o), ldw(b, y * 8));
        st8(dst, o, u8x16_narrow_i16x8(v, v));
    }
}

// ---------------------------------------------------------------------------------------------
// colour

#[inline(always)]
fn ld4(s: &[u8], o: usize) -> v128 {
    let s = &s[o..o + 4];
    // SAFETY: as ld8
    unsafe { v128_load32_zero(s.as_ptr() as *const u32) }
}

/// msmpeg4::bgra_row_c for the first 8n pixels of the row; returns how many it did
pub fn bgra_row(y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) -> usize {
    let n = y.len() / 8 * 8;
    let alpha = u8x16_splat(255);
    for x in (0..n).step_by(8) {
        // (chroma doubled: lanes 0 0 1 1 2 2 3 3)
        let dup = |c: v128| i16x8_sub(u16x8_extend_low_u8x16(i8x16_shuffle::<0, 0, 1, 1, 2, 2, 3, 3, 0, 0, 0, 0, 0, 0, 0, 0>(c, c)), i16x8_splat(128));
        let (d, e) = (dup(ld4(u, x / 2)), dup(ld4(v, x / 2)));
        let l = i16x8_sub(ld8w(y, x), i16x8_splat(16));
        let px = |hi: bool| {
            let ext = |v: v128| if hi { i32x4_extend_high_i16x8(v) } else { i32x4_extend_low_i16x8(v) };
            let (c, d, e) = (i32x4_add(i32x4_mul(ext(l), i32x4_splat(298)), i32x4_splat(128)), ext(d), ext(e));
            let m = |k: i32, x: v128| i32x4_mul(i32x4_splat(k), x);
            let b = i32x4_shr(i32x4_add(c, m(516, d)), 8);
            let g = i32x4_shr(i32x4_sub(c, i32x4_add(m(100, d), m(208, e))), 8);
            let r = i32x4_shr(i32x4_add(c, m(409, e)), 8);
            (b, g, r)
        };
        let ((b0, g0, r0), (b1, g1, r1)) = (px(false), px(true));
        let (b, g, r) = (clip_i32(b0, b1), clip_i32(g0, g1), clip_i32(r0, r1));
        let bg = i8x16_shuffle::<0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23>(b, g);
        let ra = i8x16_shuffle::<0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23>(r, alpha);
        st16(out, x * 4, i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(bg, ra));
        st16(out, x * 4 + 16, i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(bg, ra));
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hpel as hp, idct, vc1};
    use proptest::prelude::*;

    fn block() -> impl Strategy<Value = [i16; 64]> {
        // (all sizes of coefficient: small ones as in pictures, and any i16)
        prop_oneof![prop::array::uniform32(-300i16..300).prop_flat_map(|a| prop::array::uniform32(-2048i16..2048).prop_map(move |b| { let mut x = [0; 64]; x[..32].copy_from_slice(&a); x[32..].copy_from_slice(&b); x })), prop::collection::vec(any::<i16>(), 64).prop_map(|v| v.try_into().unwrap())]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

        #[test]
        fn hpel_is_scalar(w in prop::sample::select(vec![8usize, 16]), h in prop::sample::select(vec![8usize, 16]), dxy in 0usize..4, rnd in any::<bool>(), ss in 17usize..40, src in prop::collection::vec(any::<u8>(), 40 * 17), doff in 0usize..9) {
            let (mut a, mut b) = (vec![3u8; 40 * 17], vec![3u8; 40 * 17]);
            prop_assert!(hpel(&mut a, doff, 24, &src, ss, w, h, dxy, rnd));
            hp::hpel_c(&mut b, doff, 24, &src, ss, w, h, dxy, rnd);
            prop_assert_eq!(a, b);
        }

        #[test]
        fn chroma_mc8_is_scalar(x in 0i32..8, y in 0i32..8, no_rnd in any::<bool>(), ss in 9usize..30, src in prop::collection::vec(any::<u8>(), 30 * 9)) {
            let (mut a, mut b) = (vec![3u8; 64], vec![3u8; 64]);
            chroma_mc8(&mut a, 0, 8, &src, ss, x, y, no_rnd);
            vc1::chroma_mc8_c(&mut b, 0, 8, &src, ss, x, y, no_rnd);
            prop_assert_eq!(a, b);
        }

        #[test]
        fn mspel_mc_is_scalar(n in prop::sample::select(vec![8usize, 16]), hmode in 0usize..4, vmode in 0usize..4, rnd in 0i32..2, extra in 0usize..6, src in prop::collection::vec(any::<u8>(), 25 * 20)) {
            prop_assume!(hmode != 0 || vmode != 0);
            let ss = n + 3 + extra;
            let (mut a, mut b) = (vec![3u8; 16 * 16], vec![3u8; 16 * 16]);
            mspel_mc(&mut a, 0, 16, &src, ss + 1, ss, n, hmode, vmode, rnd);
            vc1::mspel_mc_c(&mut b, 0, 16, &src, ss + 1, ss, n, hmode, vmode, rnd);
            prop_assert_eq!(a, b);
        }

        #[test]
        fn wmv2_is_scalar(b in block()) {
            let (mut x, mut y) = (b, b);
            wmv2(&mut x);
            idct::wmv2_c(&mut y);
            prop_assert_eq!(x, y);
        }

        #[test]
        fn vc1_8x8_is_scalar(b in block()) {
            let (mut x, mut y) = (b, b);
            vc1_8x8(&mut x);
            vc1::inv_trans_8x8_c(&mut y);
            prop_assert_eq!(x, y);
        }

        #[test]
        fn bgra_row_is_scalar(w in 1usize..40, px in prop::collection::vec(any::<u8>(), 40 + 20 + 20)) {
            let (y, u, v) = (&px[..w], &px[40..40 + w.div_ceil(2)], &px[60..60 + w.div_ceil(2)]);
            let (mut a, mut b) = (vec![0u8; w * 4], vec![0u8; w * 4]);
            let n = bgra_row(y, u, v, &mut a);
            crate::msmpeg4::bgra_row_c(&y[n..], &u[n / 2..], &v[n / 2..], &mut a[n * 4..]);
            crate::msmpeg4::bgra_row_c(y, u, v, &mut b);
            prop_assert_eq!(a, b);
        }

        #[test]
        fn put_and_add_clamp(b in block(), px in prop::collection::vec(any::<u8>(), 64)) {
            let (mut x, mut y) = (px.clone(), px.clone());
            add8x8(&mut x, 0, 8, &b);
            for k in 0..64 {
                y[k] = (y[k] as i32 + b[k] as i32).clamp(0, 255) as u8;
            }
            prop_assert_eq!(&x, &y);
            put8x8(&mut x, 0, 8, &b);
            prop_assert!(x.iter().zip(&b).all(|(&p, &c)| p as i32 == (c as i32).clamp(0, 255)));
        }
    }
}
