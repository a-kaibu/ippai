//! The inverse DCTs of MS-MPEG4 / WMV 1 / WMV 2 (FFmpeg's simple_idct_template.c for 8 bits,
//! simple_idct.c's 8x4 / 4x8 and wmv2dsp.c), bit for bit, in wrapping 32-bit arithmetic as C
//! does it. Blocks are 8x8 coefficients in raster order; `put` writes, `add` adds to `dst`.

#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

const W1: i32 = 22725;
const W2: i32 = 21407;
const W3: i32 = 19266;
const W4: i32 = 16383;
const W5: i32 = 12873;
const W6: i32 = 8867;
const W7: i32 = 4520;
const ROW_SHIFT: u32 = 11;
const COL_SHIFT: u32 = 20;

fn row_cond_dc(row: &mut [i16]) {
    if row[1..8].iter().all(|&x| x == 0) {
        let t = (row[0] as i32).wrapping_mul(8) as i16;
        row[..8].fill(t);
        return;
    }
    let r = |k: usize| row[k] as i32;
    let mut a0 = W4.wrapping_mul(r(0)).wrapping_add(1 << (ROW_SHIFT - 1));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 = a0.wrapping_add(W2.wrapping_mul(r(2)));
    a1 = a1.wrapping_add(W6.wrapping_mul(r(2)));
    a2 = a2.wrapping_sub(W6.wrapping_mul(r(2)));
    a3 = a3.wrapping_sub(W2.wrapping_mul(r(2)));
    let mut b0 = W1.wrapping_mul(r(1)).wrapping_add(W3.wrapping_mul(r(3)));
    let mut b1 = W3.wrapping_mul(r(1)).wrapping_sub(W7.wrapping_mul(r(3)));
    let mut b2 = W5.wrapping_mul(r(1)).wrapping_sub(W1.wrapping_mul(r(3)));
    let mut b3 = W7.wrapping_mul(r(1)).wrapping_sub(W5.wrapping_mul(r(3)));
    if row[4..8].iter().any(|&x| x != 0) {
        a0 = a0.wrapping_add(W4.wrapping_mul(r(4))).wrapping_add(W6.wrapping_mul(r(6)));
        a1 = a1.wrapping_sub(W4.wrapping_mul(r(4))).wrapping_sub(W2.wrapping_mul(r(6)));
        a2 = a2.wrapping_sub(W4.wrapping_mul(r(4))).wrapping_add(W2.wrapping_mul(r(6)));
        a3 = a3.wrapping_add(W4.wrapping_mul(r(4))).wrapping_sub(W6.wrapping_mul(r(6)));
        b0 = b0.wrapping_add(W5.wrapping_mul(r(5))).wrapping_add(W7.wrapping_mul(r(7)));
        b1 = b1.wrapping_sub(W1.wrapping_mul(r(5))).wrapping_sub(W5.wrapping_mul(r(7)));
        b2 = b2.wrapping_add(W7.wrapping_mul(r(5))).wrapping_add(W3.wrapping_mul(r(7)));
        b3 = b3.wrapping_add(W3.wrapping_mul(r(5))).wrapping_sub(W1.wrapping_mul(r(7)));
    }
    let s = ROW_SHIFT;
    row[0] = (a0.wrapping_add(b0) >> s) as i16;
    row[7] = (a0.wrapping_sub(b0) >> s) as i16;
    row[1] = (a1.wrapping_add(b1) >> s) as i16;
    row[6] = (a1.wrapping_sub(b1) >> s) as i16;
    row[2] = (a2.wrapping_add(b2) >> s) as i16;
    row[5] = (a2.wrapping_sub(b2) >> s) as i16;
    row[3] = (a3.wrapping_add(b3) >> s) as i16;
    row[4] = (a3.wrapping_sub(b3) >> s) as i16;
}

/// One column (`b` from its top coefficient, stride 8): the 8 outputs, top to bottom.
fn col(b: &[i16], c: usize) -> [i32; 8] {
    let r = |k: usize| b[c + 8 * k] as i32;
    let mut a0 = W4.wrapping_mul(r(0) + ((1 << (COL_SHIFT - 1)) / W4));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 = a0.wrapping_add(W2.wrapping_mul(r(2)));
    a1 = a1.wrapping_add(W6.wrapping_mul(r(2)));
    a2 = a2.wrapping_sub(W6.wrapping_mul(r(2)));
    a3 = a3.wrapping_sub(W2.wrapping_mul(r(2)));
    let mut b0 = W1.wrapping_mul(r(1)).wrapping_add(W3.wrapping_mul(r(3)));
    let mut b1 = W3.wrapping_mul(r(1)).wrapping_sub(W7.wrapping_mul(r(3)));
    let mut b2 = W5.wrapping_mul(r(1)).wrapping_sub(W1.wrapping_mul(r(3)));
    let mut b3 = W7.wrapping_mul(r(1)).wrapping_sub(W5.wrapping_mul(r(3)));
    if r(4) != 0 {
        a0 = a0.wrapping_add(W4.wrapping_mul(r(4)));
        a1 = a1.wrapping_sub(W4.wrapping_mul(r(4)));
        a2 = a2.wrapping_sub(W4.wrapping_mul(r(4)));
        a3 = a3.wrapping_add(W4.wrapping_mul(r(4)));
    }
    if r(5) != 0 {
        b0 = b0.wrapping_add(W5.wrapping_mul(r(5)));
        b1 = b1.wrapping_sub(W1.wrapping_mul(r(5)));
        b2 = b2.wrapping_add(W7.wrapping_mul(r(5)));
        b3 = b3.wrapping_add(W3.wrapping_mul(r(5)));
    }
    if r(6) != 0 {
        a0 = a0.wrapping_add(W6.wrapping_mul(r(6)));
        a1 = a1.wrapping_sub(W2.wrapping_mul(r(6)));
        a2 = a2.wrapping_add(W2.wrapping_mul(r(6)));
        a3 = a3.wrapping_sub(W6.wrapping_mul(r(6)));
    }
    if r(7) != 0 {
        b0 = b0.wrapping_add(W7.wrapping_mul(r(7)));
        b1 = b1.wrapping_sub(W5.wrapping_mul(r(7)));
        b2 = b2.wrapping_add(W3.wrapping_mul(r(7)));
        b3 = b3.wrapping_sub(W1.wrapping_mul(r(7)));
    }
    let s = COL_SHIFT;
    [
        a0.wrapping_add(b0) >> s,
        a1.wrapping_add(b1) >> s,
        a2.wrapping_add(b2) >> s,
        a3.wrapping_add(b3) >> s,
        a3.wrapping_sub(b3) >> s,
        a2.wrapping_sub(b2) >> s,
        a1.wrapping_sub(b1) >> s,
        a0.wrapping_sub(b0) >> s,
    ]
}

/// ff_simple_idct_put_int16_8bit
pub fn simple_put(dst: &mut [u8], stride: usize, b: &mut [i16; 64]) {
    for i in 0..8 {
        row_cond_dc(&mut b[i * 8..i * 8 + 8]);
    }
    for c in 0..8 {
        for (k, v) in col(b, c).into_iter().enumerate() {
            dst[k * stride + c] = clip(v);
        }
    }
}

/// ff_simple_idct_add_int16_8bit
pub fn simple_add(dst: &mut [u8], stride: usize, b: &mut [i16; 64]) {
    for i in 0..8 {
        row_cond_dc(&mut b[i * 8..i * 8 + 8]);
    }
    for c in 0..8 {
        for (k, v) in col(b, c).into_iter().enumerate() {
            let d = &mut dst[k * stride + c];
            *d = clip(*d as i32 + v);
        }
    }
}

// the 4-point transforms of the 8x4 and 4x8 IDCTs
const C1: i32 = (0.6532814824f64 * std::f64::consts::SQRT_2 * 4096.0 + 0.5) as i32;
const C2: i32 = (0.2705980501f64 * std::f64::consts::SQRT_2 * 4096.0 + 0.5) as i32;
const C3: i32 = (0.5f64 * std::f64::consts::SQRT_2 * 4096.0 + 0.5) as i32;
const C_SHIFT: u32 = 4 + 1 + 12;
const R1: i32 = (0.6532814824f64 * std::f64::consts::SQRT_2 * 32768.0 + 0.5) as i32;
const R2: i32 = (0.2705980501f64 * std::f64::consts::SQRT_2 * 32768.0 + 0.5) as i32;
const R3: i32 = (0.5f64 * std::f64::consts::SQRT_2 * 32768.0 + 0.5) as i32;
const R_SHIFT: u32 = 11;

fn idct4col_add(dst: &mut [u8], off: usize, stride: usize, b: &[i16], c: usize) {
    let (a0, a1, a2, a3) = (b[c] as i32, b[c + 8] as i32, b[c + 16] as i32, b[c + 24] as i32);
    let c0 = (a0 + a2).wrapping_mul(C3).wrapping_add(1 << (C_SHIFT - 1));
    let c2 = (a0 - a2).wrapping_mul(C3).wrapping_add(1 << (C_SHIFT - 1));
    let c1 = a1.wrapping_mul(C1).wrapping_add(a3.wrapping_mul(C2));
    let c3 = a1.wrapping_mul(C2).wrapping_sub(a3.wrapping_mul(C1));
    for (k, v) in [c0.wrapping_add(c1), c2.wrapping_add(c3), c2.wrapping_sub(c3), c0.wrapping_sub(c1)].into_iter().enumerate() {
        let d = &mut dst[off + k * stride];
        *d = clip(*d as i32 + (v >> C_SHIFT));
    }
}

fn idct4row(row: &mut [i16]) {
    let (a0, a1, a2, a3) = (row[0] as i32, row[1] as i32, row[2] as i32, row[3] as i32);
    let c0 = (a0 + a2).wrapping_mul(R3).wrapping_add(1 << (R_SHIFT - 1));
    let c2 = (a0 - a2).wrapping_mul(R3).wrapping_add(1 << (R_SHIFT - 1));
    let c1 = a1.wrapping_mul(R1).wrapping_add(a3.wrapping_mul(R2));
    let c3 = a1.wrapping_mul(R2).wrapping_sub(a3.wrapping_mul(R1));
    // (unsigned shifts in C: the same low 16 bits)
    row[0] = (c0.wrapping_add(c1) >> R_SHIFT) as i16;
    row[1] = (c2.wrapping_add(c3) >> R_SHIFT) as i16;
    row[2] = (c2.wrapping_sub(c3) >> R_SHIFT) as i16;
    row[3] = (c0.wrapping_sub(c1) >> R_SHIFT) as i16;
}

/// ff_simple_idct84_add: 8 wide, 4 high (the first 4 rows of `b`).
pub fn simple84_add(dst: &mut [u8], off: usize, stride: usize, b: &mut [i16; 64]) {
    for i in 0..4 {
        row_cond_dc(&mut b[i * 8..i * 8 + 8]);
    }
    for c in 0..8 {
        idct4col_add(dst, off + c, stride, b, c);
    }
}

/// ff_simple_idct48_add: 4 wide (the first 4 columns of `b`), 8 high.
pub fn simple48_add(dst: &mut [u8], off: usize, stride: usize, b: &mut [i16; 64]) {
    for i in 0..8 {
        idct4row(&mut b[i * 8..i * 8 + 8]);
    }
    for c in 0..4 {
        for (k, v) in col(b, c).into_iter().enumerate() {
            let d = &mut dst[off + k * stride + c];
            *d = clip(*d as i32 + v);
        }
    }
}

const V0: i32 = 2048;
const V1: i32 = 2841;
const V2: i32 = 2676;
const V3: i32 = 2408;
const V5: i32 = 1609;
const V6: i32 = 1108;
const V7: i32 = 565;

fn wmv2_row(b: &mut [i16]) {
    let r = |k: usize| b[k] as i32;
    let a1 = V1 * r(1) + V7 * r(7);
    let a7 = V7 * r(1) - V1 * r(7);
    let a5 = V5 * r(5) + V3 * r(3);
    let a3 = V3 * r(5) - V5 * r(3);
    let a2 = V2 * r(2) + V6 * r(6);
    let a6 = V6 * r(2) - V2 * r(6);
    let a0 = V0 * r(0) + V0 * r(4);
    let a4 = V0 * r(0) - V0 * r(4);
    let s1 = 181i32.wrapping_mul(a1 - a5 + a7 - a3).wrapping_add(128) >> 8;
    let s2 = 181i32.wrapping_mul(a1 - a5 - a7 + a3).wrapping_add(128) >> 8;
    b[0] = ((a0 + a2 + a1 + a5 + (1 << 7)) >> 8) as i16;
    b[1] = ((a4 + a6 + s1 + (1 << 7)) >> 8) as i16;
    b[2] = ((a4 - a6 + s2 + (1 << 7)) >> 8) as i16;
    b[3] = ((a0 - a2 + a7 + a3 + (1 << 7)) >> 8) as i16;
    b[4] = ((a0 - a2 - a7 - a3 + (1 << 7)) >> 8) as i16;
    b[5] = ((a4 - a6 - s2 + (1 << 7)) >> 8) as i16;
    b[6] = ((a4 + a6 - s1 + (1 << 7)) >> 8) as i16;
    b[7] = ((a0 + a2 - a1 - a5 + (1 << 7)) >> 8) as i16;
}

fn wmv2_col(b: &mut [i16], c: usize) {
    let r = |k: usize| b[c + 8 * k] as i32;
    let a1 = (V1 * r(1) + V7 * r(7) + 4) >> 3;
    let a7 = (V7 * r(1) - V1 * r(7) + 4) >> 3;
    let a5 = (V5 * r(5) + V3 * r(3) + 4) >> 3;
    let a3 = (V3 * r(5) - V5 * r(3) + 4) >> 3;
    let a2 = (V2 * r(2) + V6 * r(6) + 4) >> 3;
    let a6 = (V6 * r(2) - V2 * r(6) + 4) >> 3;
    let a0 = (V0 * r(0) + V0 * r(4)) >> 3;
    let a4 = (V0 * r(0) - V0 * r(4)) >> 3;
    let s1 = 181i32.wrapping_mul(a1 - a5 + a7 - a3).wrapping_add(128) >> 8;
    let s2 = 181i32.wrapping_mul(a1 - a5 - a7 + a3).wrapping_add(128) >> 8;
    let out = [
        a0 + a2 + a1 + a5,
        a4 + a6 + s1,
        a4 - a6 + s2,
        a0 - a2 + a7 + a3,
        a0 - a2 - a7 - a3,
        a4 - a6 - s2,
        a4 + a6 - s1,
        a0 + a2 - a1 - a5,
    ];
    for (k, v) in out.into_iter().enumerate() {
        b[c + 8 * k] = ((v + (1 << 13)) >> 14) as i16;
    }
}

fn wmv2(b: &mut [i16; 64]) {
    for i in 0..8 {
        wmv2_row(&mut b[i * 8..i * 8 + 8]);
    }
    for c in 0..8 {
        wmv2_col(b, c);
    }
}

pub fn wmv2_put(dst: &mut [u8], off: usize, stride: usize, b: &mut [i16; 64]) {
    wmv2(b);
    for y in 0..8 {
        for x in 0..8 {
            dst[off + y * stride + x] = clip(b[y * 8 + x] as i32);
        }
    }
}

pub fn wmv2_add(dst: &mut [u8], off: usize, stride: usize, b: &mut [i16; 64]) {
    wmv2(b);
    for y in 0..8 {
        for x in 0..8 {
            let d = &mut dst[off + y * stride + x];
            *d = clip(*d as i32 + b[y * 8 + x] as i32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The 8-point DCT-II basis, scaled as the IDCTs expect (orthonormal * 8 / 8).
    fn reference(b: &[i16; 64]) -> [f64; 64] {
        let c = |u: usize| if u == 0 { (0.125f64).sqrt() } else { 0.5 };
        let mut out = [0f64; 64];
        for y in 0..8 {
            for x in 0..8 {
                let mut s = 0.0;
                for v in 0..8 {
                    for u in 0..8 {
                        s += c(u) * c(v) * b[v * 8 + u] as f64 * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos() * (((2 * y + 1) * v) as f64 * std::f64::consts::PI / 16.0).cos();
                    }
                }
                out[y * 8 + x] = s;
            }
        }
        out
    }

    #[test]
    fn constants() {
        assert_eq!((C1, C2, C3), (3784, 1567, 2896));
        assert_eq!((R1, R2, R3), (30274, 12540, 23170));
    }

    proptest! {
        /// Both 8x8 IDCTs are within IEEE 1180-like error of the exact transform for
        /// coefficients small enough not to clip.
        #[test]
        fn close_to_the_real_transform(c in prop::collection::vec(-60i16..60, 64), sparse in 0usize..64) {
            let mut b = [0i16; 64];
            for (i, v) in c.iter().enumerate() {
                if i <= sparse {
                    b[i] = *v;
                }
            }
            let want = reference(&b);
            for wmv in [false, true] {
                let mut dst = [128u8; 64];
                let mut x = b;
                if wmv { wmv2_add(&mut dst, 0, 8, &mut x) } else { simple_add(&mut dst, 8, &mut x) }
                for i in 0..64 {
                    let w = (128.0 + want[i]).clamp(0.0, 255.0);
                    prop_assert!((dst[i] as f64 - w).abs() <= 2.0, "{} {i}: {} vs {w}", wmv, dst[i]);
                }
            }
        }

        /// put == add onto zeros (for values that do not go below 0).
        #[test]
        fn put_is_add_to_zero(c in prop::collection::vec(-300i16..300, 64)) {
            let mut b = [0i16; 64];
            b.copy_from_slice(&c);
            b[0] = 2000;
            let (mut x, mut y) = (b, b);
            let mut p = [0u8; 64];
            let mut a = [0u8; 64];
            simple_put(&mut p, 8, &mut x);
            simple_add(&mut a, 8, &mut y);
            prop_assert_eq!(p, a);
        }
    }
}
