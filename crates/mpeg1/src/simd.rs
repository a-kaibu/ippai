//! video.rs's per-pixel loops in wasm SIMD128 (motion compensation, the IDCT and adding its
//! block, the RGB32 conversion), each the same bytes as its scalar code (crates/decbench's
//! tests hold them to it in wasm: `scalar` there is these loops' video.rs versions).
use core::arch::wasm32::*;

/// video::process's rows: `bs` (16 or 8) rows of `bs` bytes from `s` at `si` to `d` at `di`,
/// rows `dw` apart, half-pel averaged, and averaged into `d` when `interp`. The caller has
/// checked the bounds (both planes hold every byte read and written).
#[inline]
pub unsafe fn process_rows(s: &[u8], d: &mut [u8], mut si: usize, mut di: usize, dw: usize, bs: usize, odd_h: bool, odd_v: bool, interp: bool) {
    let (sp, dp) = (s.as_ptr(), d.as_mut_ptr());
    let load = |p: *const u8| if bs == 16 { v128_load(p as *const v128) } else { v128_load64_zero(p as *const u64) };
    for _ in 0..bs {
        let a = sp.add(si);
        let p = match (odd_h, odd_v) {
            (false, false) => load(a),
            (false, true) => u8x16_avgr(load(a), load(a.add(dw))),
            (true, false) => u8x16_avgr(load(a), load(a.add(1))),
            (true, true) => {
                let (w, x, y, z) = (load(a), load(a.add(1)), load(a.add(dw)), load(a.add(dw + 1)));
                let sum = |f: fn(v128) -> v128| i16x8_add(i16x8_add(f(w), f(x)), i16x8_add(f(y), f(z)));
                let two = i16x8_splat(2);
                let lo = u16x8_shr(i16x8_add(sum(u16x8_extend_low_u8x16), two), 2);
                let hi = u16x8_shr(i16x8_add(sum(u16x8_extend_high_u8x16), two), 2);
                u8x16_narrow_i16x8(lo, hi)
            }
        };
        let o = dp.add(di);
        let p = if interp { u8x16_avgr(load(o), p) } else { p };
        if bs == 16 {
            v128_store(o as *mut v128, p);
        } else {
            v128_store64_lane::<0>(p, o as *mut u64);
        }
        si += dw;
        di += dw;
    }
}

#[inline(always)]
fn m(a: v128, k: i32) -> v128 {
    i32x4_shr(i32x4_add(i32x4_mul(a, i32x4_splat(k)), i32x4_splat(128)), 8)
}

/// video::idct's column pass on 4 columns at once (b[k]: row k of them); `round` adds its row
/// pass's last (v + 128) >> 8.
#[inline(always)]
fn pass(b: [v128; 8], round: bool) -> [v128; 8] {
    let b1 = b[4];
    let b3 = i32x4_add(b[2], b[6]);
    let b4 = i32x4_sub(b[5], b[3]);
    let tmp1 = i32x4_add(b[1], b[7]);
    let tmp2 = i32x4_add(b[3], b[5]);
    let b6 = i32x4_sub(b[1], b[7]);
    let b7 = i32x4_add(tmp1, tmp2);
    let m0 = b[0];
    let k128 = i32x4_splat(128);
    let x4 = i32x4_sub(i32x4_shr(i32x4_add(i32x4_sub(i32x4_mul(b6, i32x4_splat(473)), i32x4_mul(b4, i32x4_splat(196))), k128), 8), b7);
    let x0 = i32x4_sub(x4, m(i32x4_sub(tmp1, tmp2), 362));
    let x1 = i32x4_sub(m0, b1);
    let x2 = i32x4_sub(m(i32x4_sub(b[2], b[6]), 362), b3);
    let x3 = i32x4_add(m0, b1);
    let y3 = i32x4_add(x1, x2);
    let y4 = i32x4_add(x3, b3);
    let y5 = i32x4_sub(x1, x2);
    let y6 = i32x4_sub(x3, b3);
    let y7 = i32x4_sub(i32x4_neg(x0), i32x4_shr(i32x4_add(i32x4_add(i32x4_mul(b4, i32x4_splat(473)), i32x4_mul(b6, i32x4_splat(196))), k128), 8));
    let o = [i32x4_add(b7, y4), i32x4_add(x4, y3), i32x4_sub(y5, x0), i32x4_sub(y6, y7), i32x4_add(y6, y7), i32x4_add(x0, y5), i32x4_sub(y3, x4), i32x4_sub(y4, b7)];
    if round {
        o.map(|v| i32x4_shr(i32x4_add(v, k128), 8))
    } else {
        o
    }
}

#[inline(always)]
fn transpose4(a: v128, b: v128, c: v128, d: v128) -> [v128; 4] {
    let (t0, t1) = (i32x4_shuffle::<0, 4, 1, 5>(a, b), i32x4_shuffle::<2, 6, 3, 7>(a, b));
    let (t2, t3) = (i32x4_shuffle::<0, 4, 1, 5>(c, d), i32x4_shuffle::<2, 6, 3, 7>(c, d));
    [i64x2_shuffle::<0, 2>(t0, t2), i64x2_shuffle::<1, 3>(t0, t2), i64x2_shuffle::<0, 2>(t1, t3), i64x2_shuffle::<1, 3>(t1, t3)]
}

/// m[r][h]: row r, columns 4h..4h+4 -> the same of the transposed matrix.
#[inline(always)]
fn transpose8(m: &[[v128; 2]; 8]) -> [[v128; 2]; 8] {
    let mut t = [[i32x4_splat(0); 2]; 8];
    for rb in 0..2 {
        for h in 0..2 {
            let q = transpose4(m[4 * rb][h], m[4 * rb + 1][h], m[4 * rb + 2][h], m[4 * rb + 3][h]);
            for (k, v) in q.into_iter().enumerate() {
                t[4 * h + k][rb] = v;
            }
        }
    }
    t
}

/// video::idct of `blk`, then its rows added to (`intra`: written to) `d` at `di`, rows `dw`
/// apart, clamped to bytes; the block is left zero. The caller has checked `d` holds 8 rows.
#[inline]
pub unsafe fn idct_put(blk: &mut [i32; 64], d: &mut [u8], di: usize, dw: usize, intra: bool) {
    let bp = blk.as_ptr() as *const v128;
    let mut mm = [[i32x4_splat(0); 2]; 8];
    for (r, row) in mm.iter_mut().enumerate() {
        *row = [v128_load(bp.add(2 * r)), v128_load(bp.add(2 * r + 1))];
    }
    for h in 0..2 {
        let o = pass(std::array::from_fn(|r| mm[r][h]), false);
        for r in 0..8 {
            mm[r][h] = o[r];
        }
    }
    let mut t = transpose8(&mm);
    for h in 0..2 {
        let o = pass(std::array::from_fn(|r| t[r][h]), true);
        for r in 0..8 {
            t[r][h] = o[r];
        }
    }
    let mm = transpose8(&t);
    let dp = d.as_mut_ptr();
    for (r, row) in mm.iter().enumerate() {
        let o = dp.add(di + r * dw);
        // (saturating i32 -> i16 then adding saturated keeps where each sum is past 0 or 255)
        let v = i16x8_narrow_i32x4(row[0], row[1]);
        let v = if intra { v } else { i16x8_add_sat(v, u16x8_extend_low_u8x16(v128_load64_zero(o as *const u64))) };
        v128_store64_lane::<0>(u8x16_narrow_i16x8(v, v), o as *mut u64);
    }
    *blk = [0; 64];
}

/// video::to_bgrx's rows `2 * row`, `2 * row + 1` for chroma columns `col..col + 8` (16
/// pixels each); false when a pixel is past `out` (the caller does those one by one).
#[inline]
pub unsafe fn bgrx16(f: &crate::Frame, out: &mut [u8], stride: usize, bottom_up: bool, h: usize, row: usize, col: usize) -> bool {
    let rows = [2 * row, 2 * row + 1].map(|y| if bottom_up { h - 1 - y } else { y });
    if rows.iter().any(|&oy| oy * stride + (col * 2 + 16) * 4 > out.len()) {
        return false;
    }
    let ci = row * f.c_stride + col;
    let wide = |b: v128| i16x8_sub(u16x8_extend_low_u8x16(b), i16x8_splat(128));
    let cr = wide(v128_load64_zero(f.cr.as_ptr().add(ci) as *const u64));
    let cb = wide(v128_load64_zero(f.cb.as_ptr().add(ci) as *const u64));
    let (crl, crh, cbl, cbh) = (i32x4_extend_low_i16x8(cr), i32x4_extend_high_i16x8(cr), i32x4_extend_low_i16x8(cb), i32x4_extend_high_i16x8(cb));
    let k = i32x4_splat;
    let r = [i32x4_shr(i32x4_mul(crl, k(104597)), 16), i32x4_shr(i32x4_mul(crh, k(104597)), 16)];
    let g = [
        i32x4_shr(i32x4_add(i32x4_mul(cbl, k(25674)), i32x4_mul(crl, k(53278))), 16),
        i32x4_shr(i32x4_add(i32x4_mul(cbh, k(25674)), i32x4_mul(crh, k(53278))), 16),
    ];
    let b = [i32x4_shr(i32x4_mul(cbl, k(132201)), 16), i32x4_shr(i32x4_mul(cbh, k(132201)), 16)];
    // (chroma terms per pixel: each sample twice)
    let dup = |t: [v128; 2]| [i32x4_shuffle::<0, 0, 1, 1>(t[0], t[0]), i32x4_shuffle::<2, 2, 3, 3>(t[0], t[0]), i32x4_shuffle::<0, 0, 1, 1>(t[1], t[1]), i32x4_shuffle::<2, 2, 3, 3>(t[1], t[1])];
    let (r, g, b) = (dup(r), dup(g), dup(b));
    for (dy, &oy) in rows.iter().enumerate() {
        let yrow = v128_load(f.y.as_ptr().add((2 * row + dy) * f.y_stride + col * 2) as *const v128);
        let y16 = [i16x8_sub(u16x8_extend_low_u8x16(yrow), i16x8_splat(16)), i16x8_sub(u16x8_extend_high_u8x16(yrow), i16x8_splat(16))];
        let yv: [v128; 4] = std::array::from_fn(|q| {
            let h = y16[q / 2];
            let w = if q % 2 == 0 { i32x4_extend_low_i16x8(h) } else { i32x4_extend_high_i16x8(h) };
            i32x4_shr(i32x4_mul(w, k(76309)), 16)
        });
        let ch = |t: &[v128; 4], add: bool| {
            let s: [v128; 4] = std::array::from_fn(|q| if add { i32x4_add(yv[q], t[q]) } else { i32x4_sub(yv[q], t[q]) });
            u8x16_narrow_i16x8(i16x8_narrow_i32x4(s[0], s[1]), i16x8_narrow_i32x4(s[2], s[3]))
        };
        let (bb, gg, rr) = (ch(&b, true), ch(&g, false), ch(&r, true));
        let ff = u8x16_splat(0xff);
        let bg_lo = u8x16_shuffle::<0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23>(bb, gg);
        let bg_hi = u8x16_shuffle::<8, 24, 9, 25, 10, 26, 11, 27, 12, 28, 13, 29, 14, 30, 15, 31>(bb, gg);
        let rf_lo = u8x16_shuffle::<0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23>(rr, ff);
        let rf_hi = u8x16_shuffle::<8, 24, 9, 25, 10, 26, 11, 27, 12, 28, 13, 29, 14, 30, 15, 31>(rr, ff);
        let o = out.as_mut_ptr().add(oy * stride + col * 8) as *mut v128;
        v128_store(o, i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(bg_lo, rf_lo));
        v128_store(o.add(1), i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(bg_lo, rf_lo));
        v128_store(o.add(2), i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(bg_hi, rf_hi));
        v128_store(o.add(3), i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(bg_hi, rf_hi));
    }
    true
}
