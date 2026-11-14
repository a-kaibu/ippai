//! WMV 9 (WMV3, VC-1 Simple / Main profile) video: FFmpeg's vc1.c, vc1dec.c, vc1_block.c,
//! vc1_pred.c, vc1_mc.c, vc1_loopfilter.c and vc1dsp.c (their C code paths), ported bit for bit
//! for progressive I, P, B and BI pictures, with and without FASTTX (the Complex profile's
//! simple IDCT). The Advanced profile (WVC1), IntraX8 pictures and sprites (WMVP) are not
//! ported: they decode as `Error::Unsupported`.
//!
//! Where FFmpeg gives up on a picture whose last MB's raw skip bit is the last bit (its "<= 1 bit
//! left" check) and conceals the rest, this decodes that MB as Windows does; `ffmpeg_conceals` /
//! `ffmpeg_drops` say so for the differential tests (video_tests.rs, samples.sh).

use crate::bits::Bits;
use crate::hpel;
use crate::idct;
use crate::msmpeg4::{dc_vlc, mb_i_vlc, Error, Picture};
use crate::msmpeg4_tables as m4;
use crate::vc1_tables as t;
use crate::vlc::Vlc;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------------------
// tables

// transform types (TT_*)
const TT_8X8: i32 = 0;
const TT_8X4_BOTTOM: i32 = 1;
const TT_8X4_TOP: i32 = 2;
const TT_8X4: i32 = 3;
const TT_4X8_RIGHT: i32 = 4;
const TT_4X8_LEFT: i32 = 5;
const TT_4X8: i32 = 6;
const TT_4X4: i32 = 7;

const TTBLK_TO_TT: [[i32; 8]; 3] = [
    [TT_8X4, TT_4X8, TT_8X8, TT_4X4, TT_8X4_TOP, TT_8X4_BOTTOM, TT_4X8_RIGHT, TT_4X8_LEFT],
    [TT_8X8, TT_4X8_RIGHT, TT_4X8_LEFT, TT_4X4, TT_8X4, TT_4X8, TT_8X4_BOTTOM, TT_8X4_TOP],
    [TT_8X8, TT_4X8, TT_4X4, TT_8X4_BOTTOM, TT_4X8_RIGHT, TT_4X8_LEFT, TT_8X4, TT_8X4_TOP],
];
const TTFRM_TO_TT: [i32; 4] = [TT_8X8, TT_8X4, TT_4X8, TT_4X4];

// motion vector modes (MV_PMODE_*)
const MV_1MV_HPEL_BILIN: u8 = 0;
const MV_1MV: u8 = 1;
const MV_1MV_HPEL: u8 = 2;
const MV_MIXED_MV: u8 = 3;
const MV_INTENSITY_COMP: u8 = 4;
const PMODE: [[u8; 5]; 2] = [
    [MV_1MV_HPEL_BILIN, MV_1MV, MV_1MV_HPEL, MV_INTENSITY_COMP, MV_MIXED_MV],
    [MV_1MV, MV_MIXED_MV, MV_1MV_HPEL, MV_INTENSITY_COMP, MV_1MV_HPEL_BILIN],
];
const PMODE2: [[u8; 4]; 2] = [[MV_1MV_HPEL_BILIN, MV_1MV, MV_1MV_HPEL, MV_MIXED_MV], [MV_1MV, MV_MIXED_MV, MV_1MV_HPEL, MV_1MV_HPEL_BILIN]];

// coding sets (CS_*)
const CS_HIGH_MOT_INTRA: usize = 0;
const CS_HIGH_MOT_INTER: usize = 1;
const CS_LOW_MOT_INTRA: usize = 2;
const CS_LOW_MOT_INTER: usize = 3;
const CS_MID_RATE_INTRA: usize = 4;
const CS_MID_RATE_INTER: usize = 5;
const CS_HIGH_RATE_INTRA: usize = 6;
const CS_HIGH_RATE_INTER: usize = 7;

const QUANT_FRAME_IMPLICIT: u32 = 0;
const QUANT_FRAME_EXPLICIT: u32 = 1;
const QUANT_NON_UNIFORM: u32 = 2;

const DQPROFILE_FOUR_EDGES: u32 = 0;
const DQPROFILE_DOUBLE_EDGES: u32 = 1;
const DQPROFILE_SINGLE_EDGE: u32 = 2;
const DQPROFILE_ALL_MBS: u32 = 3;

const IMODE_RAW: i32 = 0;
const IMODE_NORM2: i32 = 1;
const IMODE_DIFF2: i32 = 2;
const IMODE_NORM6: i32 = 3;
const IMODE_DIFF6: i32 = 4;
const IMODE_ROWSKIP: i32 = 5;
const IMODE_COLSKIP: i32 = 6;

/// block_map: the storage order of an MB's blocks (0, 2 / 1, 3 swapped)
const BLOCK_MAP: [usize; 6] = [0, 2, 1, 3, 4, 5];
const SIZE_TABLE: [u32; 6] = [0, 2, 3, 4, 5, 8];
const OFFSET_TABLE1: [i32; 9] = [0, 1, 3, 7, 15, 31, 63, 127, 255];
const DCPRED: [i32; 32] = [
    -1, 1024, 512, 341, 256, 205, 171, 146, 128, 114, 102, 93, 85, 79, 73, 68, 64, 60, 57, 54, 51, 49, 47, 45, 43, 41, 39, 38, 37, 35, 34, 33,
];
const BMV_BACKWARD: u8 = 0;
const BMV_FORWARD: u8 = 1;
const BMV_INTERPOLATED: u8 = 2;

/// scale_mv: a co-located vector scaled to the forward (or, `inv`, the backward) reference
fn scale_mv(value: i32, bfrac: i32, inv: bool, qs: bool) -> i32 {
    let n = if inv { bfrac - 256 } else { bfrac };
    if !qs {
        2 * ((value * n + 255) >> 9)
    } else {
        (value * n + 128) >> 8
    }
}

/// ff_vc1_bfraction_lut (B_FRACTION_DEN 256)
const BFRACTION_LUT: [i32; 23] = [128, 85, 170, 64, 192, 51, 102, 153, 204, 43, 215, 37, 74, 111, 148, 185, 222, 32, 96, 160, 224, -1, 0];
const POPCOUNT4: [u8; 16] = [0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4];

struct Tables {
    imode: Vlc,
    norm2: Vlc,
    norm6: Vlc,
    ttmb: [Vlc; 3],
    ttblk: [Vlc; 3],
    subblkpat: [Vlc; 3],
    cbpcy: [Vlc; 4],
    mv_diff: [Vlc; 4],
    ac: [Vlc; 8],
    /// the transposed WMV1 scans (zz_8x8 of FASTTX streams), and the WMV1 ones (of the others)
    zz: [[u8; 64]; 4],
    zz_plain: [[u8; 64]; 4],
    zz_8x4: [u8; 32],
    zz_4x8: [u8; 32],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let codes = |bits: u32, lens: &[u8], codes: &[u32]| Vlc::from_codes(bits, lens, codes);
        let u = |a: &[u8]| a.iter().map(|&x| x as u32).collect::<Vec<u32>>();
        let w = |a: &[u16]| a.iter().map(|&x| x as u32).collect::<Vec<u32>>();
        let ac = |k: usize| {
            let n = t::FF_VC1_AC_SIZES[k] as usize;
            let row = &t::VC1_AC_TABLES[k * 372..k * 372 + 2 * n];
            let c: Vec<u32> = (0..n).map(|i| row[2 * i]).collect();
            let l: Vec<u8> = (0..n).map(|i| row[2 * i + 1] as u8).collect();
            Vlc::from_codes(9, &l, &c)
        };
        let mut zz = [[0u8; 64]; 4];
        for (k, z) in zz.iter_mut().enumerate() {
            for (i, v) in z.iter_mut().enumerate() {
                let x = m4::WMV1_SCANTABLE[k * 64 + i];
                *v = (x >> 3) | ((x & 7) << 3);
            }
        }
        Tables {
            imode: codes(4, &t::VC1_IMODE_BITS, &u(&t::VC1_IMODE_CODES)),
            norm2: codes(3, &t::VC1_NORM2_BITS, &u(&t::VC1_NORM2_CODES)),
            norm6: codes(9, &t::VC1_NORM6_BITS, &w(&t::VC1_NORM6_CODES)),
            ttmb: std::array::from_fn(|i| codes(9, &t::VC1_TTMB_BITS[i * 16..i * 16 + 16], &w(&t::VC1_TTMB_CODES[i * 16..i * 16 + 16]))),
            ttblk: std::array::from_fn(|i| codes(5, &t::VC1_TTBLK_BITS[i * 8..i * 8 + 8], &u(&t::VC1_TTBLK_CODES[i * 8..i * 8 + 8]))),
            subblkpat: std::array::from_fn(|i| codes(6, &t::VC1_SUBBLKPAT_BITS[i * 15..i * 15 + 15], &u(&t::VC1_SUBBLKPAT_CODES[i * 15..i * 15 + 15]))),
            cbpcy: std::array::from_fn(|i| codes(9, &t::VC1_CBPCY_P_BITS[i * 64..i * 64 + 64], &w(&t::VC1_CBPCY_P_CODES[i * 64..i * 64 + 64]))),
            mv_diff: std::array::from_fn(|i| codes(9, &t::VC1_MV_DIFF_BITS[i * 73..i * 73 + 73], &w(&t::VC1_MV_DIFF_CODES[i * 73..i * 73 + 73]))),
            ac: std::array::from_fn(ac),
            zz,
            zz_plain: std::array::from_fn(|k| std::array::from_fn(|i| m4::WMV1_SCANTABLE[k * 64 + i])),
            zz_8x4: m4::WMV2_SCANTABLEA,
            zz_4x8: m4::WMV2_SCANTABLEB,
        }
    })
}

#[inline]
fn decode012(r: &mut Bits) -> u32 {
    if r.get1() == 0 { 0 } else { r.get1() + 1 }
}
#[inline]
fn decode210(r: &mut Bits) -> u32 {
    if r.get1() != 0 { 0 } else { 2 - r.get1() }
}
#[inline]
fn mid_pred(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}
fn median4(a: i32, b: i32, c: i32, d: i32) -> i32 {
    if a < b {
        if c < d { (b.min(d) + a.max(c)) / 2 } else { (b.min(c) + a.max(d)) / 2 }
    } else if c < d {
        (a.min(d) + b.max(c)) / 2
    } else {
        (a.min(c) + b.max(d)) / 2
    }
}
#[inline]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

// ---------------------------------------------------------------------------------------
// the transforms and filters (vc1dsp.c)

/// vc1_inv_trans_8x8_c: in place, the result in i16 as C stores it
pub fn inv_trans_8x8(b: &mut [i16; 64]) {
    let mut tmp = [0i16; 64];
    for i in 0..8 {
        let s = |k: usize| b[i + k] as i32;
        let t1 = 12 * (s(0) + s(32)) + 4;
        let t2 = 12 * (s(0) - s(32)) + 4;
        let t3 = 16 * s(16) + 6 * s(48);
        let t4 = 6 * s(16) - 16 * s(48);
        let (t5, t6, t7, t8) = (t1 + t3, t2 + t4, t2 - t4, t1 - t3);
        let t1 = 16 * s(8) + 15 * s(24) + 9 * s(40) + 4 * s(56);
        let t2 = 15 * s(8) - 4 * s(24) - 16 * s(40) - 9 * s(56);
        let t3 = 9 * s(8) - 16 * s(24) + 4 * s(40) + 15 * s(56);
        let t4 = 4 * s(8) - 9 * s(24) + 15 * s(40) - 16 * s(56);
        let d = &mut tmp[i * 8..i * 8 + 8];
        d[0] = ((t5 + t1) >> 3) as i16;
        d[1] = ((t6 + t2) >> 3) as i16;
        d[2] = ((t7 + t3) >> 3) as i16;
        d[3] = ((t8 + t4) >> 3) as i16;
        d[4] = ((t8 - t4) >> 3) as i16;
        d[5] = ((t7 - t3) >> 3) as i16;
        d[6] = ((t6 - t2) >> 3) as i16;
        d[7] = ((t5 - t1) >> 3) as i16;
    }
    for i in 0..8 {
        let s = |k: usize| tmp[i + k] as i32;
        let t1 = 12 * (s(0) + s(32)) + 64;
        let t2 = 12 * (s(0) - s(32)) + 64;
        let t3 = 16 * s(16) + 6 * s(48);
        let t4 = 6 * s(16) - 16 * s(48);
        let (t5, t6, t7, t8) = (t1 + t3, t2 + t4, t2 - t4, t1 - t3);
        let t1 = 16 * s(8) + 15 * s(24) + 9 * s(40) + 4 * s(56);
        let t2 = 15 * s(8) - 4 * s(24) - 16 * s(40) - 9 * s(56);
        let t3 = 9 * s(8) - 16 * s(24) + 4 * s(40) + 15 * s(56);
        let t4 = 4 * s(8) - 9 * s(24) + 15 * s(40) - 16 * s(56);
        b[i] = ((t5 + t1) >> 7) as i16;
        b[i + 8] = ((t6 + t2) >> 7) as i16;
        b[i + 16] = ((t7 + t3) >> 7) as i16;
        b[i + 24] = ((t8 + t4) >> 7) as i16;
        b[i + 32] = ((t8 - t4 + 1) >> 7) as i16;
        b[i + 40] = ((t7 - t3 + 1) >> 7) as i16;
        b[i + 48] = ((t6 - t2 + 1) >> 7) as i16;
        b[i + 56] = ((t5 - t1 + 1) >> 7) as i16;
    }
}

fn add_dc(dst: &mut [u8], off: usize, stride: usize, w: usize, h: usize, dc: i32) {
    for y in 0..h {
        for x in 0..w {
            let p = &mut dst[off + y * stride + x];
            *p = clip8(*p as i32 + dc);
        }
    }
}

/// vc1_inv_trans_8x8_dc_c
pub fn inv_trans_8x8_dc(dst: &mut [u8], off: usize, stride: usize, b: &[i16]) {
    let dc = (3 * b[0] as i32 + 1) >> 1;
    let dc = (3 * dc + 16) >> 5;
    add_dc(dst, off, stride, 8, 8, dc);
}
/// vc1_inv_trans_8x4_dc_c
pub fn inv_trans_8x4_dc(dst: &mut [u8], off: usize, stride: usize, b: &[i16]) {
    let dc = (3 * b[0] as i32 + 1) >> 1;
    let dc = (17 * dc + 64) >> 7;
    add_dc(dst, off, stride, 8, 4, dc);
}
/// vc1_inv_trans_4x8_dc_c
pub fn inv_trans_4x8_dc(dst: &mut [u8], off: usize, stride: usize, b: &[i16]) {
    let dc = (17 * b[0] as i32 + 4) >> 3;
    let dc = (12 * dc + 64) >> 7;
    add_dc(dst, off, stride, 4, 8, dc);
}
/// vc1_inv_trans_4x4_dc_c
pub fn inv_trans_4x4_dc(dst: &mut [u8], off: usize, stride: usize, b: &[i16]) {
    let dc = (17 * b[0] as i32 + 4) >> 3;
    let dc = (17 * dc + 64) >> 7;
    add_dc(dst, off, stride, 4, 4, dc);
}

/// vc1_inv_trans_8x4_c: rows 0..4 of 8 of `b` (stride 8), added to `dst`
pub fn inv_trans_8x4(dst: &mut [u8], off: usize, stride: usize, b: &mut [i16]) {
    for i in 0..4 {
        let s = |k: usize| b[i * 8 + k] as i32;
        let t1 = 12 * (s(0) + s(4)) + 4;
        let t2 = 12 * (s(0) - s(4)) + 4;
        let t3 = 16 * s(2) + 6 * s(6);
        let t4 = 6 * s(2) - 16 * s(6);
        let (t5, t6, t7, t8) = (t1 + t3, t2 + t4, t2 - t4, t1 - t3);
        let t1 = 16 * s(1) + 15 * s(3) + 9 * s(5) + 4 * s(7);
        let t2 = 15 * s(1) - 4 * s(3) - 16 * s(5) - 9 * s(7);
        let t3 = 9 * s(1) - 16 * s(3) + 4 * s(5) + 15 * s(7);
        let t4 = 4 * s(1) - 9 * s(3) + 15 * s(5) - 16 * s(7);
        let d = &mut b[i * 8..i * 8 + 8];
        d[0] = ((t5 + t1) >> 3) as i16;
        d[1] = ((t6 + t2) >> 3) as i16;
        d[2] = ((t7 + t3) >> 3) as i16;
        d[3] = ((t8 + t4) >> 3) as i16;
        d[4] = ((t8 - t4) >> 3) as i16;
        d[5] = ((t7 - t3) >> 3) as i16;
        d[6] = ((t6 - t2) >> 3) as i16;
        d[7] = ((t5 - t1) >> 3) as i16;
    }
    for i in 0..8 {
        let s = |k: usize| b[i + k] as i32;
        let t1 = 17 * (s(0) + s(16)) + 64;
        let t2 = 17 * (s(0) - s(16)) + 64;
        let t3 = 22 * s(8) + 10 * s(24);
        let t4 = 22 * s(24) - 10 * s(8);
        let o = off + i;
        let add = |d: &mut [u8], k: usize, v: i32| d[o + k * stride] = clip8(d[o + k * stride] as i32 + v);
        add(dst, 0, (t1 + t3) >> 7);
        add(dst, 1, (t2 - t4) >> 7);
        add(dst, 2, (t2 + t4) >> 7);
        add(dst, 3, (t1 - t3) >> 7);
    }
}

/// vc1_inv_trans_4x8_c: columns 0..4 of `b` (stride 8), added to `dst`
pub fn inv_trans_4x8(dst: &mut [u8], off: usize, stride: usize, b: &mut [i16]) {
    for i in 0..8 {
        let s = |k: usize| b[i * 8 + k] as i32;
        let t1 = 17 * (s(0) + s(2)) + 4;
        let t2 = 17 * (s(0) - s(2)) + 4;
        let t3 = 22 * s(1) + 10 * s(3);
        let t4 = 22 * s(3) - 10 * s(1);
        let d = &mut b[i * 8..i * 8 + 4];
        d[0] = ((t1 + t3) >> 3) as i16;
        d[1] = ((t2 - t4) >> 3) as i16;
        d[2] = ((t2 + t4) >> 3) as i16;
        d[3] = ((t1 - t3) >> 3) as i16;
    }
    for i in 0..4 {
        let s = |k: usize| b[i + k] as i32;
        let t1 = 12 * (s(0) + s(32)) + 64;
        let t2 = 12 * (s(0) - s(32)) + 64;
        let t3 = 16 * s(16) + 6 * s(48);
        let t4 = 6 * s(16) - 16 * s(48);
        let (t5, t6, t7, t8) = (t1 + t3, t2 + t4, t2 - t4, t1 - t3);
        let t1 = 16 * s(8) + 15 * s(24) + 9 * s(40) + 4 * s(56);
        let t2 = 15 * s(8) - 4 * s(24) - 16 * s(40) - 9 * s(56);
        let t3 = 9 * s(8) - 16 * s(24) + 4 * s(40) + 15 * s(56);
        let t4 = 4 * s(8) - 9 * s(24) + 15 * s(40) - 16 * s(56);
        let o = off + i;
        let add = |d: &mut [u8], k: usize, v: i32| d[o + k * stride] = clip8(d[o + k * stride] as i32 + v);
        add(dst, 0, (t5 + t1) >> 7);
        add(dst, 1, (t6 + t2) >> 7);
        add(dst, 2, (t7 + t3) >> 7);
        add(dst, 3, (t8 + t4) >> 7);
        add(dst, 4, (t8 - t4 + 1) >> 7);
        add(dst, 5, (t7 - t3 + 1) >> 7);
        add(dst, 6, (t6 - t2 + 1) >> 7);
        add(dst, 7, (t5 - t1 + 1) >> 7);
    }
}

/// vc1_inv_trans_4x4_c
pub fn inv_trans_4x4(dst: &mut [u8], off: usize, stride: usize, b: &mut [i16]) {
    for i in 0..4 {
        let s = |k: usize| b[i * 8 + k] as i32;
        let t1 = 17 * (s(0) + s(2)) + 4;
        let t2 = 17 * (s(0) - s(2)) + 4;
        let t3 = 22 * s(1) + 10 * s(3);
        let t4 = 22 * s(3) - 10 * s(1);
        let d = &mut b[i * 8..i * 8 + 4];
        d[0] = ((t1 + t3) >> 3) as i16;
        d[1] = ((t2 - t4) >> 3) as i16;
        d[2] = ((t2 + t4) >> 3) as i16;
        d[3] = ((t1 - t3) >> 3) as i16;
    }
    for i in 0..4 {
        let s = |k: usize| b[i + k] as i32;
        let t1 = 17 * (s(0) + s(16)) + 64;
        let t2 = 17 * (s(0) - s(16)) + 64;
        let t3 = 22 * s(8) + 10 * s(24);
        let t4 = 22 * s(24) - 10 * s(8);
        let o = off + i;
        let add = |d: &mut [u8], k: usize, v: i32| d[o + k * stride] = clip8(d[o + k * stride] as i32 + v);
        add(dst, 0, (t1 + t3) >> 7);
        add(dst, 1, (t2 - t4) >> 7);
        add(dst, 2, (t2 + t4) >> 7);
        add(dst, 3, (t1 - t3) >> 7);
    }
}

/// add_pixels_clamped
fn add_pixels(dst: &mut [u8], off: usize, stride: usize, b: &[i16; 64]) {
    for y in 0..8 {
        for x in 0..8 {
            let p = &mut dst[off + y * stride + x];
            *p = clip8(*p as i32 + b[y * 8 + x] as i32);
        }
    }
}

/// vc1_h_s_overlap_c: `left[6..8]` / `right[0..2]` of 8 rows (strides in i16s)
fn h_s_overlap(blk: &mut [[[i16; 64]; 6]], l: (usize, usize, usize), r: (usize, usize, usize), ls: usize, rs: usize, flags: u32) {
    let mut rnd1 = if flags & 2 != 0 { 3 } else { 4 };
    let mut rnd2 = 7 - rnd1;
    let (mut lo, mut ro) = (l.2, r.2);
    for _ in 0..8 {
        let a = blk[l.0][l.1][lo + 6] as i32;
        let b = blk[l.0][l.1][lo + 7] as i32;
        let c = blk[r.0][r.1][ro] as i32;
        let d = blk[r.0][r.1][ro + 1] as i32;
        let d1 = a - d;
        let d2 = a - d + b - c;
        blk[l.0][l.1][lo + 6] = ((a * 8 - d1 + rnd1) >> 3) as i16;
        blk[l.0][l.1][lo + 7] = ((b * 8 - d2 + rnd2) >> 3) as i16;
        blk[r.0][r.1][ro] = ((c * 8 + d2 + rnd1) >> 3) as i16;
        blk[r.0][r.1][ro + 1] = ((d * 8 + d1 + rnd2) >> 3) as i16;
        lo += ls;
        ro += rs;
        if flags & 1 != 0 {
            rnd2 = 7 - rnd2;
            rnd1 = 7 - rnd1;
        }
    }
}

/// vc1_v_s_overlap_c: rows 6, 7 of `top` and 0, 1 of `bottom`
fn v_s_overlap(blk: &mut [[[i16; 64]; 6]], tp: (usize, usize), bt: (usize, usize)) {
    let (mut rnd1, mut rnd2) = (4, 3);
    for i in 0..8 {
        let a = blk[tp.0][tp.1][48 + i] as i32;
        let b = blk[tp.0][tp.1][56 + i] as i32;
        let c = blk[bt.0][bt.1][i] as i32;
        let d = blk[bt.0][bt.1][8 + i] as i32;
        let d1 = a - d;
        let d2 = a - d + b - c;
        blk[tp.0][tp.1][48 + i] = ((a * 8 - d1 + rnd1) >> 3) as i16;
        blk[tp.0][tp.1][56 + i] = ((b * 8 - d2 + rnd2) >> 3) as i16;
        blk[bt.0][bt.1][i] = ((c * 8 + d2 + rnd1) >> 3) as i16;
        blk[bt.0][bt.1][8 + i] = ((d * 8 + d1 + rnd2) >> 3) as i16;
        rnd2 = 7 - rnd2;
        rnd1 = 7 - rnd1;
    }
}

/// vc1_filter_line: one line across an edge at `p` (pixels `stride` apart); true: filter the
/// other three
fn filter_line(d: &mut [u8], p: isize, stride: isize, pq: i32) -> bool {
    let g = |d: &[u8], k: isize| d[(p + k * stride) as usize] as i32;
    let a0 = (2 * (g(d, -2) - g(d, 1)) - 5 * (g(d, -1) - g(d, 0)) + 4) >> 3;
    let a0_sign = a0 >> 31;
    let a0 = (a0 ^ a0_sign) - a0_sign;
    if a0 < pq {
        let a1 = ((2 * (g(d, -4) - g(d, -1)) - 5 * (g(d, -3) - g(d, -2)) + 4) >> 3).abs();
        let a2 = ((2 * (g(d, 0) - g(d, 3)) - 5 * (g(d, 1) - g(d, 2)) + 4) >> 3).abs();
        if a1 < a0 || a2 < a0 {
            let clip = g(d, -1) - g(d, 0);
            let clip_sign = clip >> 31;
            let clip = ((clip ^ clip_sign) - clip_sign) >> 1;
            if clip != 0 {
                let a3 = a1.min(a2);
                let dd = 5 * (a3 - a0);
                let mut d_sign = dd >> 31;
                let dd = ((dd ^ d_sign) - d_sign) >> 3;
                d_sign ^= a0_sign;
                if d_sign ^ clip_sign == 0 {
                    let dd = dd.min(clip);
                    let dd = (dd ^ d_sign) - d_sign;
                    let i1 = (p - stride) as usize;
                    d[i1] = clip8(d[i1] as i32 - dd);
                    d[p as usize] = clip8(d[p as usize] as i32 + dd);
                }
                return true;
            }
        }
    }
    false
}

/// vc1_loop_filter: `len` lines from `p`, `step` apart, across edges `stride` deep
fn loop_filter(d: &mut [u8], mut p: isize, step: isize, stride: isize, len: usize, pq: i32) {
    for _ in (0..len).step_by(4) {
        if filter_line(d, p + 2 * step, stride, pq) {
            filter_line(d, p, stride, pq);
            filter_line(d, p + step, stride, pq);
            filter_line(d, p + 3 * step, stride, pq);
        }
        p += step * 4;
    }
}
/// v_loop_filterN: the horizontal edge above `p`, N pixels wide
fn v_lf(d: &mut [u8], p: isize, stride: usize, n: usize, pq: i32) {
    loop_filter(d, p, 1, stride as isize, n, pq);
}
/// h_loop_filterN: the vertical edge left of `p`, N pixels high
fn h_lf(d: &mut [u8], p: isize, stride: usize, n: usize, pq: i32) {
    loop_filter(d, p, stride as isize, 1, n, pq);
}

/// vc1_mspel_filter: one pixel of a 1-D bicubic filter
#[inline]
fn mspel_filter(s: &[u8], p: usize, stride: usize, mode: usize, r: i32) -> i32 {
    let g = |k: isize| s[(p as isize + k * stride as isize) as usize] as i32;
    match mode {
        1 => (-4 * g(-1) + 53 * g(0) + 18 * g(1) - 3 * g(2) + 32 - r) >> 6,
        2 => (-g(-1) + 9 * g(0) + 9 * g(1) - g(2) + 8 - r) >> 4,
        3 => (-3 * g(-1) + 18 * g(0) + 53 * g(1) - 4 * g(2) + 32 - r) >> 6,
        _ => g(0),
    }
}
#[inline]
fn mspel16(g: impl Fn(isize) -> i32, mode: usize) -> i32 {
    match mode {
        1 => -4 * g(-1) + 53 * g(0) + 18 * g(1) - 3 * g(2),
        2 => -g(-1) + 9 * g(0) + 9 * g(1) - g(2),
        3 => -3 * g(-1) + 18 * g(0) + 53 * g(1) - 4 * g(2),
        _ => 0,
    }
}

/// put_vc1_mspel_mc (n 8) / _16: `src` at `so` (stride `ss`), hmode / vmode quarter pels
pub fn mspel_mc(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], so: usize, ss: usize, n: usize, hmode: usize, vmode: usize, rnd: i32) {
    if hmode == 0 && vmode == 0 {
        for y in 0..n {
            dst[doff + y * ds..doff + y * ds + n].copy_from_slice(&src[so + y * ss..so + y * ss + n]);
        }
        return;
    }
    if vmode != 0 {
        if hmode != 0 {
            const SHIFT: [i32; 4] = [0, 5, 1, 5];
            let shift = (SHIFT[hmode] + SHIFT[vmode]) >> 1;
            let w = n + 3;
            let mut tmp = [0i16; 19 * 16];
            let r = (1 << (shift - 1)) + rnd - 1;
            for j in 0..n {
                for i in 0..w {
                    let p = so - 1 + j * ss + i;
                    let v = mspel16(|k| src[(p as isize + k * ss as isize) as usize] as i32, vmode);
                    tmp[j * w + i] = ((v + r) >> shift) as i16;
                }
            }
            let r = 64 - rnd;
            for j in 0..n {
                for i in 0..n {
                    let p = j * w + 1 + i;
                    let v = mspel16(|k| tmp[(p as isize + k) as usize] as i32, hmode);
                    dst[doff + j * ds + i] = clip8((v + r) >> 7);
                }
            }
            return;
        }
        let r = 1 - rnd;
        for j in 0..n {
            for i in 0..n {
                dst[doff + j * ds + i] = clip8(mspel_filter(src, so + j * ss + i, ss, vmode, r));
            }
        }
        return;
    }
    for j in 0..n {
        for i in 0..n {
            dst[doff + j * ds + i] = clip8(mspel_filter(src, so + j * ss + i, 1, hmode, rnd));
        }
    }
}

/// put_h264_chroma_mc8 (`no_rnd` false) / put_no_rnd_vc1_chroma_mc8: 8x8 bilinear, eighth pels
pub fn chroma_mc8(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], ss: usize, x: i32, y: i32, no_rnd: bool) {
    let (a, b, c, d) = ((8 - x) * (8 - y), x * (8 - y), (8 - x) * y, x * y);
    let r = if no_rnd { 28 } else { 32 };
    for j in 0..8 {
        for i in 0..8 {
            let s = |k: usize| src[j * ss + i + k] as i32;
            dst[doff + j * ds + i] = ((a * s(0) + b * s(1) + c * s(ss) + d * s(ss + 1) + r) >> 6) as u8;
        }
    }
}

// ---------------------------------------------------------------------------------------
// the decoder

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Type {
    I,
    P,
    B,
}

#[derive(Clone)]
struct Pic {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

pub struct Decoder {
    width: usize,
    height: usize,
    mb_w: usize,
    mb_h: usize,
    mbs: usize,
    b8: usize,
    ls: usize,
    uvls: usize,
    /// The last picture is one FFmpeg's decoder gives up on part way (and conceals the rest of):
    /// a damaged one, or one that ends with its last MB's raw skip bit
    pub ffmpeg_conceals: bool,
    /// The last picture is a B one FFmpeg gives up on, and so does not show
    pub ffmpeg_drops: bool,
    // sequence header
    loop_filter: bool,
    fastuvmc: bool,
    extended_mv: bool,
    dquant: u32,
    vstransform: bool,
    overlap: bool,
    rangered: bool,
    quantizer_mode: u32,
    finterpflag: bool,
    res_rtm_flag: bool,
    res_x8: bool,
    multires: bool,
    /// FASTTX: the VC-1 transforms (else the simple IDCT and untransposed scans, as WMV 1's)
    fasttx: bool,
    /// no B pictures: each picture is shown as it is decoded (else an I / P one is shown when the
    /// next I / P one comes, the B ones between them first)
    low_delay: bool,
    // picture header
    pict: Type,
    rangeredfrm: bool,
    rnd: i32,
    pq: i32,
    pqindex: u32,
    halfpq: i32,
    pquantizer: bool,
    dquantfrm: bool,
    dqprofile: u32,
    dqsbedge: u32,
    dqbilevel: bool,
    altpq: i32,
    mvrange: u32,
    k_x: u32,
    k_y: u32,
    range_x: i32,
    range_y: i32,
    x8_type: bool,
    tt_index: usize,
    mv_mode: u8,
    mv_mode2: u8,
    quarter_sample: bool,
    mspel: bool,
    use_ic: bool,
    luty: [u8; 256],
    lutuv: [u8; 256],
    mv_type_is_raw: bool,
    skip_is_raw: bool,
    dmb_is_raw: bool,
    mv_type_plane: Vec<u8>,
    skip_plane: Vec<u8>,
    direct_plane: Vec<u8>,
    /// B: the position between the references (of 256), and a BI picture (intra, not a reference)
    bfraction: i32,
    bi_type: bool,
    mv_table_index: usize,
    cbptab: usize,
    ttmbf: bool,
    ttfrm: i32,
    c_ac_table_index: u32,
    y_ac_table_index: u32,
    dc_table_index: usize,
    codingset: usize,
    codingset2: usize,
    esc3_level_length: u32,
    esc3_run_length: u32,
    // kept from picture to picture
    dc_val: Vec<i16>,
    ac_val: Vec<[i16; 16]>,
    coded_block: Vec<u8>,
    /// v->mb_type[0]: a block is intra (the dc_val layout)
    mb_type: Vec<u8>,
    /// cur_pic.motion_val[0] / [1] (offset MVO; P pictures keep their MC vectors in [1], which a
    /// B picture's direct MBs then take from `next_motion`, the newest reference's)
    motion: Vec<[i16; 2]>,
    motion1: Vec<[i16; 2]>,
    next_motion: Vec<[i16; 2]>,
    qscale: Vec<i32>,
    /// cbp / ttblk / is_intra / luma_mv: three rows of mbs, the current at 2 * mbs
    cbp: Vec<u32>,
    ttblk: Vec<i32>,
    is_intra: Vec<u8>,
    luma_mv: Vec<[i16; 2]>,
    blocks: Vec<[[i16; 64]; 6]>,
    cur_blk: usize,
    left_blk: usize,
    topleft_blk: usize,
    top_blk: usize,
    cur: Pic,
    /// the newest I / P picture (P's reference, B's backward one) and the one before it (B's
    /// forward one)
    last: Option<Pic>,
    prev: Option<Pic>,
    /// the newest I / P picture is not shown yet (`flush` gives it), and it is an I one
    pending: bool,
    last_key: bool,
    // the macroblock's
    mb_x: usize,
    mb_y: usize,
    first_slice_line: bool,
    mb_intra: bool,
    ac_pred: bool,
    a_avail: bool,
    c_avail: bool,
    bi: [usize; 6],
    mv: [[i32; 2]; 4],
    /// s->mv[1][0]: a B MB's backward vector
    mv_b: [i32; 2],
    y_dc_scale: i32,
    c_dc_scale: i32,
}

/// motion_val's offset (FFmpeg's is 4; a row more keeps the first row's reads above in it)
fn mvo(b8: usize) -> usize {
    b8 + 4
}

impl Decoder {
    /// `extra`: the WMV3 sequence header (the 4 bytes after the BITMAPINFOHEADER).
    pub fn new(width: usize, height: usize, extra: &[u8]) -> Result<Decoder, Error> {
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(Error::Params("picture size"));
        }
        if extra.is_empty() {
            return Err(Error::Params("WMV3 extradata"));
        }
        let mut r = Bits::new(extra);
        let profile = r.get(2);
        if profile == 3 {
            return Err(Error::Unsupported("VC-1 Advanced profile"));
        }
        let res_y411 = r.get1() != 0;
        let res_sprite = r.get1() != 0;
        if res_y411 {
            return Err(Error::Unsupported("old interlaced mode"));
        }
        let _frmrtq = r.get(3);
        let _bitrtq = r.get(5);
        let loop_filter = r.get1() != 0;
        let res_x8 = r.get1() != 0;
        let multires = r.get1() != 0;
        let res_fasttx = r.get1() != 0;
        let fastuvmc = r.get1() != 0;
        if profile == 0 && !fastuvmc {
            return Err(Error::Params("FASTUVMC unavailable in Simple Profile"));
        }
        let extended_mv = r.get1() != 0;
        if profile == 0 && extended_mv {
            return Err(Error::Params("extended MVs unavailable in Simple Profile"));
        }
        let dquant = r.get(2);
        let vstransform = r.get1() != 0;
        if r.get1() != 0 {
            return Err(Error::Params("RES_TRANSTAB"));
        }
        let overlap = r.get1() != 0;
        let _resync_marker = r.get1() != 0;
        let rangered = r.get1() != 0;
        let max_b_frames = r.get(3);
        let quantizer_mode = r.get(2);
        let finterpflag = r.get1() != 0;
        if res_sprite {
            return Err(Error::Unsupported("WMV3 sprites"));
        }
        let res_rtm_flag = r.get1() != 0;
        let mb_w = width.div_ceil(16);
        let mb_h = height.div_ceil(16);
        let mbs = mb_w + 1;
        let b8 = mb_w * 2 + 1;
        let mb_h2 = mb_h.next_multiple_of(2);
        let yc = b8 * (mb_h2 * 2 + 1) + mbs * (mb_h2 + 1) * 2;
        let (ls, uvls) = (mb_w * 16, mb_w * 8);
        let pic = Pic { y: vec![0; ls * mb_h * 16], u: vec![0; uvls * mb_h * 8], v: vec![0; uvls * mb_h * 8] };
        tables();
        Ok(Decoder {
            width, height, mb_w, mb_h, mbs, b8, ls, uvls,
            ffmpeg_conceals: false, ffmpeg_drops: false, loop_filter, fastuvmc, extended_mv, dquant, vstransform, overlap, rangered,
            quantizer_mode, finterpflag, res_rtm_flag, res_x8, multires, fasttx: res_fasttx, low_delay: max_b_frames == 0,
            pict: Type::I, rangeredfrm: false, rnd: 0, pq: 1, pqindex: 1, halfpq: 0, pquantizer: false,
            dquantfrm: false, dqprofile: 0, dqsbedge: 0, dqbilevel: false, altpq: 0, mvrange: 0,
            k_x: 9, k_y: 8, range_x: 256, range_y: 128, x8_type: false, tt_index: 0, mv_mode: 0, mv_mode2: 0,
            quarter_sample: false, mspel: false, use_ic: false, luty: [0; 256], lutuv: [0; 256],
            mv_type_is_raw: false, skip_is_raw: false, dmb_is_raw: false,
            mv_type_plane: vec![0; mbs * mb_h2], skip_plane: vec![0; mbs * mb_h2], direct_plane: vec![0; mbs * mb_h2],
            bfraction: 0, bi_type: false,
            mv_table_index: 0, cbptab: 0, ttmbf: false, ttfrm: 0, c_ac_table_index: 0, y_ac_table_index: 0,
            dc_table_index: 0, codingset: 0, codingset2: 0, esc3_level_length: 0, esc3_run_length: 0,
            dc_val: vec![1024; yc],
            ac_val: vec![[0; 16]; yc],
            coded_block: vec![0; b8 * (2 * mb_h2 + 1) + 1],
            mb_type: vec![0; yc],
            motion: vec![[0; 2]; mvo(b8) + b8 * (2 * mb_h2 + 1) + 8],
            motion1: vec![[0; 2]; mvo(b8) + b8 * (2 * mb_h2 + 1) + 8],
            next_motion: vec![[0; 2]; mvo(b8) + b8 * (2 * mb_h2 + 1) + 8],
            qscale: vec![0; mbs * (mb_h2 + 1)],
            cbp: vec![0; 3 * mbs],
            ttblk: vec![0; 3 * mbs],
            is_intra: vec![0; 3 * mbs],
            luma_mv: vec![[0; 2]; 3 * mbs],
            blocks: vec![[[0; 64]; 6]; mb_w + 2],
            cur_blk: 0, left_blk: 0, topleft_blk: 0, top_blk: 0,
            cur: pic,
            last: None,
            prev: None,
            pending: false,
            last_key: false,
            mb_x: 0, mb_y: 0, first_slice_line: true, mb_intra: false, ac_pred: false, a_avail: false, c_avail: false,
            bi: [0; 6], mv: [[0; 2]; 4], mv_b: [0; 2], y_dc_scale: 0, c_dc_scale: 0,
        })
    }

    /// The visible picture size.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The stream has B pictures: an I / P picture comes out when the next one goes in (or at
    /// `flush`), the B ones between them as they go in.
    pub fn delayed(&self) -> bool {
        !self.low_delay
    }

    /// The last picture decoded is a B (or BI) one.
    pub fn last_was_b(&self) -> bool {
        self.pict == Type::B
    }

    /// zz_8x8
    fn zz(&self) -> &'static [[u8; 64]; 4] {
        if self.fasttx { &tables().zz } else { &tables().zz_plain }
    }
    /// (left_blk_sh, top_blk_sh): where AC prediction puts the left / top row
    fn blk_sh(&self) -> (usize, usize) {
        if self.fasttx { (0, 3) } else { (3, 0) }
    }
    /// vc1_inv_trans_8x8 of an intra block (in place)
    fn itrans(&self, b: &mut [i16; 64]) {
        if self.fasttx {
            inv_trans_8x8(b);
        } else {
            idct::simple_inplace(b);
        }
    }

    #[inline]
    fn dci(&self, n: usize) -> usize {
        self.b8 + 1 + self.bi[n]
    }
    #[inline]
    fn wrap(&self, n: usize) -> usize {
        if n < 4 { self.b8 } else { self.mbs }
    }
    #[inline]
    fn mvi(&self, n: usize) -> usize {
        mvo(self.b8) + self.bi[n]
    }

    fn init_block_index(&mut self) {
        let (b8, mbs, mb_h) = (self.b8, self.mbs, self.mb_h);
        let (x, y) = (self.mb_x, self.mb_y);
        self.bi = [
            b8 * (2 * y) + 2 * x,
            b8 * (2 * y) + 1 + 2 * x,
            b8 * (2 * y + 1) + 2 * x,
            b8 * (2 * y + 1) + 1 + 2 * x,
            mbs * (y + 1) + b8 * mb_h * 2 + x,
            mbs * (y + mb_h + 2) + b8 * mb_h * 2 + x,
        ];
    }
    /// s->dest[k] of the MB
    fn dest(&self, k: usize) -> isize {
        if k == 0 {
            (self.mb_y * 16 * self.ls + self.mb_x * 16) as isize
        } else {
            (self.mb_y * 8 * self.uvls + self.mb_x * 8) as isize
        }
    }
    fn plane(&mut self, k: usize) -> &mut Vec<u8> {
        match k {
            0 => &mut self.cur.y,
            1 => &mut self.cur.u,
            _ => &mut self.cur.v,
        }
    }
}

// ---------------------------------------------------------------------------------------
// headers (vc1.c)

impl Decoder {
    /// `which`: the MV type (0), skip (1) or direct (2) plane
    fn bitplane(&mut self, r: &mut Bits, which: u8) -> Result<(bool, i32), Error> {
        let (width, height, stride) = (self.mb_w, self.mb_h, self.mbs);
        let plane = match which {
            0 => &mut self.mv_type_plane,
            1 => &mut self.skip_plane,
            _ => &mut self.direct_plane,
        };
        let tb = tables();
        let invert = r.get1() as u8;
        let imode = tb.imode.read(r);
        let rowskip = |p: &mut [u8], base: usize, w: usize, h: usize, r: &mut Bits| {
            for y in 0..h {
                if r.get1() == 0 {
                    p[base + y * stride..base + y * stride + w].fill(0);
                } else {
                    for x in 0..w {
                        p[base + y * stride + x] = r.get1() as u8;
                    }
                }
            }
        };
        let colskip = |p: &mut [u8], base: usize, w: usize, h: usize, r: &mut Bits| {
            for x in 0..w {
                if r.get1() == 0 {
                    for y in 0..h {
                        p[base + y * stride + x] = 0;
                    }
                } else {
                    for y in 0..h {
                        p[base + y * stride + x] = r.get1() as u8;
                    }
                }
            }
        };
        match imode {
            IMODE_RAW => return Ok((true, invert as i32)),
            IMODE_DIFF2 | IMODE_NORM2 => {
                let mut pp = 0usize;
                let mut y;
                let mut offset;
                if (height * width) & 1 != 0 {
                    plane[pp] = r.get1() as u8;
                    pp += 1;
                    y = 1;
                    offset = 1;
                    if offset == width {
                        offset = 0;
                        pp += stride - width;
                    }
                } else {
                    y = 0;
                    offset = 0;
                }
                while y < height * width {
                    let code = tb.norm2.read(r);
                    plane[pp] = (code & 1) as u8;
                    pp += 1;
                    offset += 1;
                    if offset == width {
                        offset = 0;
                        pp += stride - width;
                    }
                    plane[pp] = ((code >> 1) & 1) as u8;
                    pp += 1;
                    offset += 1;
                    if offset == width {
                        offset = 0;
                        pp += stride - width;
                    }
                    y += 2;
                }
            }
            IMODE_DIFF6 | IMODE_NORM6 => {
                if height % 3 == 0 && width % 3 != 0 {
                    // 2x3
                    let mut pp = 0;
                    for _ in (0..height).step_by(3) {
                        for x in ((width & 1)..width).step_by(2) {
                            let code = tb.norm6.read(r);
                            if code < 0 {
                                return Err(Error::Data("NORM-6 VLC"));
                            }
                            plane[pp + x] = (code & 1) as u8;
                            plane[pp + x + 1] = ((code >> 1) & 1) as u8;
                            plane[pp + x + stride] = ((code >> 2) & 1) as u8;
                            plane[pp + x + 1 + stride] = ((code >> 3) & 1) as u8;
                            plane[pp + x + stride * 2] = ((code >> 4) & 1) as u8;
                            plane[pp + x + 1 + stride * 2] = ((code >> 5) & 1) as u8;
                        }
                        pp += stride * 3;
                    }
                    if width & 1 != 0 {
                        colskip(plane, 0, 1, height, r);
                    }
                } else {
                    // 3x2
                    let mut pp = (height & 1) * stride;
                    for _ in ((height & 1)..height).step_by(2) {
                        for x in ((width % 3)..width).step_by(3) {
                            let code = tb.norm6.read(r);
                            if code < 0 {
                                return Err(Error::Data("NORM-6 VLC"));
                            }
                            plane[pp + x] = (code & 1) as u8;
                            plane[pp + x + 1] = ((code >> 1) & 1) as u8;
                            plane[pp + x + 2] = ((code >> 2) & 1) as u8;
                            plane[pp + x + stride] = ((code >> 3) & 1) as u8;
                            plane[pp + x + 1 + stride] = ((code >> 4) & 1) as u8;
                            plane[pp + x + 2 + stride] = ((code >> 5) & 1) as u8;
                        }
                        pp += stride * 2;
                    }
                    let x = width % 3;
                    if x != 0 {
                        colskip(plane, 0, x, height, r);
                    }
                    if height & 1 != 0 {
                        rowskip(plane, x, width - x, 1, r);
                    }
                }
            }
            IMODE_ROWSKIP => rowskip(plane, 0, width, height, r),
            IMODE_COLSKIP => colskip(plane, 0, width, height, r),
            _ => {}
        }
        if imode == IMODE_DIFF2 || imode == IMODE_DIFF6 {
            plane[0] ^= invert;
            for x in 1..width {
                plane[x] ^= plane[x - 1];
            }
            for y in 1..height {
                let p = y * stride;
                plane[p] ^= plane[p - stride];
                for x in 1..width {
                    if plane[p + x - 1] != plane[p + x - stride] {
                        plane[p + x] ^= invert;
                    } else {
                        plane[p + x] ^= plane[p + x - 1];
                    }
                }
            }
        } else if invert != 0 {
            for v in plane[..stride * height].iter_mut() {
                *v = (*v == 0) as u8;
            }
        }
        Ok((false, (imode << 1) + invert as i32))
    }

    fn vop_dquant(&mut self, r: &mut Bits) {
        if self.dquant != 2 {
            self.dquantfrm = r.get1() != 0;
            if !self.dquantfrm {
                return;
            }
            self.dqprofile = r.get(2);
            match self.dqprofile {
                DQPROFILE_SINGLE_EDGE | DQPROFILE_DOUBLE_EDGES => self.dqsbedge = r.get(2),
                DQPROFILE_ALL_MBS => {
                    self.dqbilevel = r.get1() != 0;
                    if !self.dqbilevel {
                        self.halfpq = 0;
                        return;
                    }
                }
                _ => {}
            }
        }
        let pqdiff = r.get(3) as i32;
        self.altpq = if pqdiff == 7 { r.get(5) as i32 } else { self.pq + pqdiff + 1 };
    }

    /// ff_vc1_parse_frame_header (Simple / Main)
    fn frame_header(&mut self, r: &mut Bits) -> Result<(), Error> {
        if self.finterpflag {
            let _interpfrm = r.get1();
        }
        r.skip(2);
        self.rangeredfrm = self.rangered && r.get1() != 0;
        self.pict = if r.get1() != 0 {
            Type::P
        } else if !self.low_delay && r.get1() == 0 {
            Type::B
        } else {
            Type::I
        };
        // read_bfraction: a B picture of fraction 0 is a BI one
        let mut bi = false;
        if self.pict == Type::B {
            let mut k = r.get(3) as usize;
            if k == 7 {
                k = 7 + r.get(4) as usize;
            }
            if k == 21 {
                return Err(Error::Data("BFRACTION"));
            }
            self.bfraction = BFRACTION_LUT[k];
            bi = self.bfraction == 0;
        }
        let intra = self.pict == Type::I || bi;
        if intra {
            r.skip(7);
        }
        if intra {
            self.rnd = 1;
        }
        if self.pict == Type::P {
            self.rnd ^= 1;
        }
        if r.left() < 5 {
            return Err(Error::Data("picture header"));
        }
        let pqindex = r.get(5);
        if pqindex == 0 {
            return Err(Error::Data("PQINDEX 0"));
        }
        self.pq = t::VC1_PQUANT_TABLE[if self.quantizer_mode == QUANT_FRAME_IMPLICIT { 0 } else { 32 } + pqindex as usize] as i32;
        self.pqindex = pqindex;
        self.halfpq = if pqindex < 9 { r.get1() as i32 } else { 0 };
        self.pquantizer = match self.quantizer_mode {
            QUANT_FRAME_IMPLICIT => pqindex < 9,
            QUANT_NON_UNIFORM => false,
            QUANT_FRAME_EXPLICIT => r.get1() != 0,
            _ => true,
        };
        self.dquantfrm = false;
        if self.extended_mv {
            self.mvrange = r.unary(0, 3);
        }
        self.k_x = self.mvrange + 9 + (self.mvrange >> 1);
        self.k_y = self.mvrange + 8;
        self.range_x = 1 << (self.k_x - 1);
        self.range_y = 1 << (self.k_y - 1);
        if self.multires && self.pict != Type::B {
            let _respic = r.get(2);
        }
        self.x8_type = self.res_x8 && intra && r.get1() != 0;
        // rotate_luts: no intensity compensation unless this P picture asks for it (a B picture's
        // forward MC keeps that of the newest P one)
        if self.pict != Type::B {
            self.use_ic = false;
        }
        if self.pict == Type::P {
            self.tt_index = ((self.pq > 4) as usize) + ((self.pq > 12) as usize);
            let lowquant = (self.pq <= 12) as usize;
            self.mv_mode = PMODE[lowquant][r.unary(1, 4) as usize];
            if self.mv_mode == MV_INTENSITY_COMP {
                self.mv_mode2 = PMODE2[lowquant][r.unary(1, 3) as usize];
                let lumscale = r.get(6) as i32;
                let lumshift = r.get(6) as i32;
                self.use_ic = true;
                let (scale, shift) = if lumscale == 0 {
                    (-64, (255 - lumshift * 2) * 64 + if lumshift > 31 { 128 << 6 } else { 0 })
                } else {
                    (lumscale + 32, if lumshift > 31 { (lumshift - 64) * 64 } else { lumshift << 6 })
                };
                for i in 0..256 {
                    self.luty[i] = clip8((scale * i as i32 + shift + 32) >> 6);
                    self.lutuv[i] = clip8((scale * (i as i32 - 128) + 128 * 64 + 32) >> 6);
                }
            }
            let m = if self.mv_mode == MV_INTENSITY_COMP { self.mv_mode2 } else { self.mv_mode };
            self.quarter_sample = m != MV_1MV_HPEL && m != MV_1MV_HPEL_BILIN;
            self.mspel = m != MV_1MV_HPEL_BILIN;
            if m == MV_MIXED_MV {
                let (raw, _) = self.bitplane(r, 0)?;
                self.mv_type_is_raw = raw;
            } else {
                self.mv_type_is_raw = false;
                self.mv_type_plane.fill(0);
            }
            let (raw, _) = self.bitplane(r, 1)?;
            self.skip_is_raw = raw;
            if r.left() < 4 {
                return Err(Error::Data("picture header"));
            }
            self.mv_table_index = r.get(2) as usize;
            self.cbptab = r.get(2) as usize;
            if self.dquant != 0 {
                self.vop_dquant(r);
            }
            if self.vstransform {
                self.ttmbf = r.get1() != 0;
                self.ttfrm = if self.ttmbf { TTFRM_TO_TT[r.get(2) as usize] } else { 0 };
            } else {
                self.ttmbf = true;
                self.ttfrm = TT_8X8;
            }
        }
        if self.pict == Type::B && !bi {
            self.tt_index = ((self.pq > 4) as usize) + ((self.pq > 12) as usize);
            self.mv_mode = if r.get1() != 0 { MV_1MV } else { MV_1MV_HPEL_BILIN };
            self.quarter_sample = self.mv_mode == MV_1MV;
            self.mspel = self.quarter_sample;
            let (raw, _) = self.bitplane(r, 2)?;
            self.dmb_is_raw = raw;
            let (raw, _) = self.bitplane(r, 1)?;
            self.skip_is_raw = raw;
            self.mv_table_index = r.get(2) as usize;
            self.cbptab = r.get(2) as usize;
            if self.dquant != 0 {
                self.vop_dquant(r);
            }
            if self.vstransform {
                self.ttmbf = r.get1() != 0;
                self.ttfrm = if self.ttmbf { TTFRM_TO_TT[r.get(2) as usize] } else { 0 };
            } else {
                self.ttmbf = true;
                self.ttfrm = TT_8X8;
            }
        }
        if !self.x8_type {
            self.c_ac_table_index = decode012(r);
            if intra {
                self.y_ac_table_index = decode012(r);
            }
            self.dc_table_index = r.get1() as usize;
        }
        self.bi_type = bi;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// pictures

impl Decoder {
    /// Decodes one compressed picture: Ok(None) for one that gives no picture.
    pub fn decode(&mut self, buf: &[u8]) -> Result<Option<Picture>, Error> {
        if buf.is_empty() {
            return Ok(None);
        }
        let mut r = Bits::new(buf);
        self.frame_header(&mut r)?;
        self.ffmpeg_conceals = false;
        self.ffmpeg_drops = false;
        if self.x8_type {
            return Err(Error::Unsupported("WMV3 IntraX8 picture"));
        }
        // (FFmpeg skips B pictures without both references)
        if self.pict == Type::B && self.prev.is_none() {
            return Ok(None);
        }
        if self.pict == Type::P && self.last.is_none() {
            // (ff_mpv_alloc_dummy_frames: a gray reference)
            let n = self.cur.y.len();
            let c = self.cur.u.len();
            self.last = Some(Pic { y: vec![0x80; n], u: vec![0x80; c], v: vec![0x80; c] });
        }
        self.esc3_level_length = 0;
        self.cur_blk = 0;
        self.left_blk = usize::MAX;
        self.topleft_blk = 1;
        self.top_blk = 2;
        // (a picture's motion vectors start at 0)
        self.motion.fill([0; 2]);
        self.motion1.fill([0; 2]);
        let res = match self.pict {
            Type::I => self.i_blocks(&mut r),
            Type::P => self.p_blocks(&mut r),
            _ if self.bi_type => self.i_blocks(&mut r),
            _ => self.b_blocks(&mut r),
        };
        // a picture damaged part way is still the picture (and the next reference), as in FFmpeg
        if res.is_err() {
            self.ffmpeg_conceals = true;
        }
        if self.pict == Type::B {
            // (FFmpeg drops a B picture it gives up on)
            self.ffmpeg_drops = self.ffmpeg_conceals;
            return Ok(Some(self.picture()));
        }
        // ff_mpv_frame_end: the picture is the next reference
        std::mem::swap(&mut self.motion1, &mut self.next_motion);
        let shown = if self.low_delay { Some(self.picture()) } else { self.last.as_ref().map(|p| self.shown(p, self.last_key)) };
        let old = self.prev.take();
        self.prev = self.last.replace(self.cur.clone());
        if let Some(o) = old {
            self.cur = o;
        }
        self.last_key = self.pict == Type::I;
        self.pending = !self.low_delay;
        Ok(shown)
    }

    /// The picture still held at the end (of a stream with B pictures): the newest I / P one.
    pub fn flush(&mut self) -> Option<Picture> {
        if !std::mem::take(&mut self.pending) {
            return None;
        }
        self.last.as_ref().map(|p| self.shown(p, self.last_key))
    }

    fn shown(&self, p: &Pic, key: bool) -> Picture {
        Picture {
            width: self.width,
            height: self.height,
            stride: [self.ls, self.uvls],
            planes: [p.y.clone(), p.u.clone(), p.v.clone()],
            key,
        }
    }

    fn picture(&self) -> Picture {
        Picture {
            width: self.width,
            height: self.height,
            stride: [self.ls, self.uvls],
            planes: [self.cur.y.clone(), self.cur.u.clone(), self.cur.v.clone()],
            key: self.pict == Type::I,
        }
    }

    fn next_blk(&mut self) {
        let n = self.mb_w + 2;
        let inc = |i: usize| if i == usize::MAX { 0 } else { (i + 1) % n };
        self.topleft_blk = inc(self.topleft_blk);
        self.top_blk = inc(self.top_blk);
        self.left_blk = inc(self.left_blk);
        self.cur_blk = inc(self.cur_blk);
    }

    fn i_blocks(&mut self, r: &mut Bits) -> Result<(), Error> {
        self.codingset = match self.y_ac_table_index {
            0 => if self.pqindex <= 8 { CS_HIGH_RATE_INTRA } else { CS_LOW_MOT_INTRA },
            1 => CS_HIGH_MOT_INTRA,
            _ => CS_MID_RATE_INTRA,
        };
        self.codingset2 = match self.c_ac_table_index {
            0 => if self.pqindex <= 8 { CS_HIGH_RATE_INTER } else { CS_LOW_MOT_INTER },
            1 => CS_HIGH_MOT_INTER,
            _ => CS_MID_RATE_INTER,
        };
        self.y_dc_scale = t::WMV3_DC_SCALE_TABLE[self.pq as usize] as i32;
        self.c_dc_scale = self.y_dc_scale;
        self.mb_intra = true;
        self.first_slice_line = true;
        for mb_y in 0..self.mb_h {
            self.mb_y = mb_y;
            for mb_x in 0..self.mb_w {
                self.mb_x = mb_x;
                self.init_block_index();
                self.blocks[self.cur_blk] = [[0; 64]; 6];
                let mut cbp = mb_i_vlc().read(r);
                self.ac_pred = r.get1() != 0;
                for k in 0..6 {
                    let d = self.dci(k);
                    self.mb_type[d] = 1;
                    let mut val = (cbp >> (5 - k)) & 1;
                    if k < 4 {
                        // vc1_coded_block_pred
                        let xy = self.bi[k] + self.b8 + 1;
                        let w = self.b8;
                        let (a, b, c) = (self.coded_block[xy - 1], self.coded_block[xy - 1 - w], self.coded_block[xy - w]);
                        let pred = if b == c { a } else { c };
                        val ^= pred as i32;
                        self.coded_block[xy] = val as u8;
                    }
                    cbp |= val << (5 - k);
                    let cs = if k < 4 { self.codingset } else { self.codingset2 };
                    let (cb, bm) = (self.cur_blk, BLOCK_MAP[k]);
                    let mut blk = self.blocks[cb][bm];
                    self.i_block(r, &mut blk, k, val != 0, cs)?;
                    self.itrans(&mut blk);
                    self.blocks[cb][bm] = blk;
                }
                if self.overlap && self.pq >= 9 {
                    self.i_overlap_filter();
                    if self.rangeredfrm {
                        for b in self.blocks[self.cur_blk].iter_mut() {
                            for v in b.iter_mut() {
                                *v = v.wrapping_mul(2);
                            }
                        }
                    }
                    self.put_blocks_clamped(true);
                } else {
                    if self.rangeredfrm {
                        for b in self.blocks[self.cur_blk].iter_mut() {
                            for v in b.iter_mut() {
                                *v = v.wrapping_sub(64).wrapping_mul(2);
                            }
                        }
                    }
                    self.put_blocks_clamped(false);
                }
                if self.loop_filter {
                    self.i_loop_filter();
                }
                if r.left() < 0 {
                    return Err(Error::Data("bits overconsumption"));
                }
                self.next_blk();
            }
            self.first_slice_line = false;
        }
        Ok(())
    }

    fn p_blocks(&mut self, r: &mut Bits) -> Result<(), Error> {
        // (FFmpeg picks both from the chroma AC table index)
        self.codingset = match self.c_ac_table_index {
            0 => if self.pqindex <= 8 { CS_HIGH_RATE_INTRA } else { CS_LOW_MOT_INTRA },
            1 => CS_HIGH_MOT_INTRA,
            _ => CS_MID_RATE_INTRA,
        };
        self.codingset2 = match self.c_ac_table_index {
            0 => if self.pqindex <= 8 { CS_HIGH_RATE_INTER } else { CS_LOW_MOT_INTER },
            1 => CS_HIGH_MOT_INTER,
            _ => CS_MID_RATE_INTER,
        };
        self.first_slice_line = true;
        self.cbp.fill(0);
        let mbs = self.mbs;
        for mb_y in 0..self.mb_h {
            self.mb_y = mb_y;
            for mb_x in 0..self.mb_w {
                self.mb_x = mb_x;
                self.init_block_index();
                // FFmpeg gives up here (and conceals the rest) with one bit left, even where the
                // last MB's skip bit is all there is; we decode it, as Windows does
                if (self.mv_type_is_raw || self.skip_is_raw) && r.left() <= 1 {
                    self.ffmpeg_conceals = true;
                }
                self.p_mb(r)?;
                if self.loop_filter {
                    self.p_loop_filter();
                }
                if r.left() < 0 {
                    return Err(Error::Data("bits overconsumption"));
                }
                self.next_blk();
            }
            self.cbp.copy_within(mbs..3 * mbs, 0);
            self.ttblk.copy_within(mbs..3 * mbs, 0);
            self.is_intra.copy_within(mbs..3 * mbs, 0);
            self.luma_mv.copy_within(mbs..3 * mbs, 0);
            self.first_slice_line = false;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// blocks (vc1_block.c)

impl Decoder {
    /// vc1_decode_ac_coeff: (last, run, level)
    fn ac_coeff(&mut self, r: &mut Bits, cs: usize) -> Result<(bool, i32, i32), Error> {
        let tb = tables();
        let size = t::FF_VC1_AC_SIZES[cs];
        let idx = |i: i32| (cs * 185 + i as usize) * 2;
        let index = tb.ac[cs].read(r);
        if index < 0 {
            return Err(Error::Data("AC VLC"));
        }
        let (lst, run, level, sign);
        if index != size - 1 {
            run = t::VC1_INDEX_DECODE_TABLE[idx(index)] as i32;
            level = t::VC1_INDEX_DECODE_TABLE[idx(index) + 1] as i32;
            lst = index >= t::VC1_LAST_DECODE_TABLE[cs] || r.left() < 0;
            sign = r.get1() as i32;
        } else {
            let escape = decode210(r);
            if escape != 2 {
                let index = tb.ac[cs].read(r);
                if index < 0 || index >= size - 1 {
                    return Err(Error::Data("AC escape"));
                }
                let mut ru = t::VC1_INDEX_DECODE_TABLE[idx(index)] as i32;
                let mut le = t::VC1_INDEX_DECODE_TABLE[idx(index) + 1] as i32;
                lst = index >= t::VC1_LAST_DECODE_TABLE[cs];
                let at = |tab: &[u8], w: usize, k: i32| tab.get(cs * w + k as usize).copied().unwrap_or(0) as i32;
                if escape == 0 {
                    le += if lst { at(&t::VC1_LAST_DELTA_LEVEL_TABLE, 44, ru) } else { at(&t::VC1_DELTA_LEVEL_TABLE, 31, ru) };
                } else {
                    ru += 1 + if lst { at(&t::VC1_LAST_DELTA_RUN_TABLE, 10, le) } else { at(&t::VC1_DELTA_RUN_TABLE, 57, le) };
                }
                run = ru;
                level = le;
                sign = r.get1() as i32;
            } else {
                lst = r.get1() != 0;
                if self.esc3_level_length == 0 {
                    if self.pq < 8 || self.dquantfrm {
                        self.esc3_level_length = r.get(3);
                        if self.esc3_level_length == 0 {
                            self.esc3_level_length = r.get(2) + 8;
                        }
                    } else {
                        self.esc3_level_length = r.unary(1, 6) + 2;
                    }
                    self.esc3_run_length = 3 + r.get(2);
                }
                run = r.get(self.esc3_run_length) as i32;
                sign = r.get1() as i32;
                level = r.get(self.esc3_level_length) as i32;
            }
        }
        Ok((lst, run, (level ^ -sign) + sign))
    }

    /// The DC differential (ff_msmp4_dc_vlc, its escape and the low-quantizer bits).
    fn dc_diff(&mut self, r: &mut Bits, n: usize, q: i32) -> i32 {
        let mut dcdiff = dc_vlc(self.dc_table_index, n >= 4).read(r);
        if dcdiff != 0 {
            let m = if q == 1 || q == 2 { 3 - q } else { 0 };
            if dcdiff == 119 {
                dcdiff = r.get((8 + m) as u32) as i32;
            } else if m != 0 {
                dcdiff = (dcdiff << m) + r.get(m as u32) as i32 - ((1 << m) - 1);
            }
            if r.get1() != 0 {
                dcdiff = -dcdiff;
            }
        }
        dcdiff
    }

    /// vc1_i_pred_dc: (prediction, direction 1 left / 0 top)
    fn i_pred_dc(&self, n: usize) -> (i32, bool) {
        let scale = if n < 4 { self.y_dc_scale } else { self.c_dc_scale };
        let wrap = self.wrap(n);
        let i = self.dci(n);
        let mut c = self.dc_val[i - 1] as i32;
        let mut b = self.dc_val[i - 1 - wrap] as i32;
        let mut a = self.dc_val[i - wrap] as i32;
        let edge = if self.pq < 9 || !self.overlap { DCPRED[scale as usize] } else { 0 };
        if self.first_slice_line && n != 2 && n != 3 {
            b = edge;
            a = edge;
        }
        if self.mb_x == 0 && n != 1 && n != 3 {
            b = edge;
            c = edge;
        }
        if (a - b).abs() <= (b - c).abs() { (c, true) } else { (a, false) }
    }

    /// vc1_decode_i_block
    fn i_block(&mut self, r: &mut Bits, block: &mut [i16; 64], n: usize, coded: bool, cs: usize) -> Result<(), Error> {
        let mut dcdiff = self.dc_diff(r, n, self.pq);
        let (pred, left) = self.i_pred_dc(n);
        dcdiff += pred;
        let di = self.dci(n);
        self.dc_val[di] = dcdiff as i16;
        let scale = if n < 4 { self.y_dc_scale } else { self.c_dc_scale };
        block[0] = (dcdiff * scale) as i16;
        let ac2 = di;
        let ac = if left { di - 1 } else { di - self.wrap(n) };
        let scale = self.pq * 2 + self.halfpq;
        let (lsh, tsh) = self.blk_sh();
        if coded {
            let zz = if self.ac_pred { if !left { &self.zz()[2] } else { &self.zz()[3] } } else { &self.zz()[1] };
            let mut i = 1usize;
            loop {
                let (last, skip, value) = self.ac_coeff(r, cs)?;
                i += skip as usize;
                if i > 63 {
                    break;
                }
                block[zz[i] as usize] = value as i16;
                i += 1;
                if last {
                    break;
                }
            }
            if self.ac_pred {
                let (sh, o) = if left { (lsh, 0) } else { (tsh, 8) };
                let src = self.ac_val[ac];
                for k in 1..8 {
                    block[k << sh] = block[k << sh].wrapping_add(src[k + o]);
                }
            }
            let a2 = &mut self.ac_val[ac2];
            for k in 1..8 {
                a2[k] = block[k << lsh];
                a2[k + 8] = block[k << tsh];
            }
            for k in 1..64 {
                if block[k] != 0 {
                    let mut v = block[k] as i32 * scale;
                    if !self.pquantizer {
                        v += if (v as i16) < 0 { -self.pq } else { self.pq };
                    }
                    block[k] = v as i16;
                }
            }
        } else {
            self.ac_val[ac2] = [0; 16];
            if self.ac_pred {
                let (sh, o) = if left { (lsh, 0) } else { (tsh, 8) };
                let src = self.ac_val[ac];
                self.ac_val[ac2][o..o + 8].copy_from_slice(&src[o..o + 8]);
                for k in 1..8 {
                    let mut v = (src[k + o] as i32 * scale) as i16;
                    if !self.pquantizer && v != 0 {
                        v = (v as i32 + if v < 0 { -self.pq } else { self.pq }) as i16;
                    }
                    block[k << sh] = v;
                }
            }
        }
        Ok(())
    }

    /// ff_vc1_pred_dc (P pictures' intra blocks)
    fn pred_dc(&self, n: usize, a_avail: bool, c_avail: bool) -> (i32, bool) {
        let mb_pos = self.mb_x + self.mb_y * self.mbs;
        let dq = |q: i32| t::WMV3_DC_SCALE_TABLE[q as usize] as u32;
        let q1 = self.qscale[mb_pos].abs();
        let dqi = dq(q1) as i32 - 1;
        if dqi < 0 {
            return (0, false);
        }
        let wrap = self.wrap(n);
        let i = self.dci(n);
        let mut c = self.dc_val[i - 1] as i32;
        let mut b = self.dc_val[i - 1 - wrap] as i32;
        let mut a = self.dc_val[i - wrap] as i32;
        let scale = |v: i32, q2: i32| ((v as u32).wrapping_mul(dq(q2)).wrapping_mul(t::VC1_DQSCALE[dqi as usize] as u32).wrapping_add(0x20000) as i32) >> 18;
        if c_avail && n != 1 && n != 3 {
            let q2 = self.qscale[mb_pos - 1].abs();
            if q2 != 0 && q2 != q1 {
                c = scale(c, q2);
            }
        }
        if a_avail && n != 2 && n != 3 {
            let q2 = self.qscale[mb_pos - self.mbs].abs();
            if q2 != 0 && q2 != q1 {
                a = scale(a, q2);
            }
        }
        if a_avail && c_avail && n != 3 {
            let mut off = mb_pos;
            if n != 1 {
                off -= 1;
            }
            if n != 2 {
                off -= self.mbs;
            }
            let q2 = self.qscale[off].abs();
            if q2 != 0 && q2 != q1 {
                b = scale(b, q2);
            }
        }
        if c_avail && (!a_avail || (a - b).abs() <= (b - c).abs()) {
            (c, true)
        } else if a_avail {
            (a, false)
        } else {
            (0, true)
        }
    }

    /// vc1_decode_intra_block (P pictures)
    fn intra_block(&mut self, r: &mut Bits, block: &mut [i16; 64], n: usize, coded: bool, mquant: i32, cs: usize) -> Result<(), Error> {
        let mb_pos = self.mb_x + self.mb_y * self.mbs;
        let (a_avail, c_avail) = (self.a_avail, self.c_avail);
        let mut use_pred = self.ac_pred;
        *block = [0; 64];
        let quant = mquant.abs().clamp(0, 31);
        self.y_dc_scale = t::WMV3_DC_SCALE_TABLE[quant as usize] as i32;
        self.c_dc_scale = self.y_dc_scale;
        let mut dcdiff = self.dc_diff(r, n, quant);
        let (pred, mut left) = self.pred_dc(n, a_avail, c_avail);
        dcdiff += pred;
        let di = self.dci(n);
        self.dc_val[di] = dcdiff as i16;
        block[0] = (dcdiff * if n < 4 { self.y_dc_scale } else { self.c_dc_scale }) as i16;
        if !a_avail {
            left = true;
        }
        if !c_avail {
            left = false;
        }
        if !a_avail && !c_avail {
            use_pred = false;
        }
        let ac2 = di;
        let ac = if left { di - 1 } else { di - self.wrap(n) };
        let scale = quant * 2 + if mquant < 0 { 0 } else { self.halfpq };
        let q1 = self.qscale[mb_pos];
        let mut q2 = 0;
        if left && c_avail && mb_pos != 0 {
            q2 = self.qscale[mb_pos - 1];
        }
        if !left && a_avail && mb_pos >= self.mbs {
            q2 = self.qscale[mb_pos - self.mbs];
        }
        if (left && n == 1) || (!left && n == 2) || n == 3 {
            q2 = q1;
        }
        let (lsh, tsh) = self.blk_sh();
        let halfpq = self.halfpq;
        let qq = |q: i32| q.abs() * 2 + if q < 0 { 0 } else { halfpq } - 1;
        let rescale = |v: i16, q2: i32, q1: i32| ((v as i32 * q2) as u32).wrapping_mul(t::VC1_DQSCALE[(q1 - 1) as usize] as u32).wrapping_add(0x20000) as i32 >> 18;
        if coded {
            let mut i = 1usize;
            loop {
                let (last, skip, value) = self.ac_coeff(r, cs)?;
                i += skip as usize;
                if i > 63 {
                    break;
                }
                block[self.zz()[0][i] as usize] = value as i16;
                i += 1;
                if last {
                    break;
                }
            }
            if use_pred {
                let q1 = qq(q1);
                if q1 < 1 {
                    return Err(Error::Data("AC prediction quantizer"));
                }
                let q2 = if q2 != 0 { qq(q2) } else { 0 };
                let src = self.ac_val[ac];
                let (sh, o) = if left { (lsh, 0) } else { (tsh, 8) };
                for k in 1..8 {
                    let p = if q2 != 0 && q1 != q2 { rescale(src[k + o], q2, q1) } else { src[k + o] as i32 };
                    block[k << sh] = (block[k << sh] as i32 + p) as i16;
                }
            }
            let a2 = &mut self.ac_val[ac2];
            for k in 1..8 {
                a2[k] = block[k << lsh];
                a2[k + 8] = block[k << tsh];
            }
            for k in 1..64 {
                if block[k] != 0 {
                    let mut v = block[k] as i32 * scale;
                    if !self.pquantizer {
                        v += if (v as i16) < 0 { -quant } else { quant };
                    }
                    block[k] = v as i16;
                }
            }
        } else {
            self.ac_val[ac2] = [0; 16];
            let o = if left { 0 } else { 8 };
            if use_pred {
                let src = self.ac_val[ac];
                self.ac_val[ac2][o..o + 8].copy_from_slice(&src[o..o + 8]);
                let q1 = qq(q1);
                if q1 < 1 {
                    return Err(Error::Data("AC prediction quantizer"));
                }
                let q2 = if q2 != 0 { qq(q2) } else { 0 };
                if q2 != 0 && q1 != q2 {
                    for k in 1..8 {
                        self.ac_val[ac2][k + o] = rescale(self.ac_val[ac2][k + o], q2, q1) as i16;
                    }
                }
                let sh = if left { lsh } else { tsh };
                for k in 1..8 {
                    let mut v = (self.ac_val[ac2][k + o] as i32 * scale) as i16;
                    if !self.pquantizer && v != 0 {
                        v = (v as i32 + if v < 0 { -quant } else { quant }) as i16;
                    }
                    block[k << sh] = v;
                }
            }
        }
        Ok(())
    }

    /// vc1_decode_p_block: the pattern of 4x4 parts coded
    #[allow(clippy::too_many_arguments)]
    fn p_block(&mut self, r: &mut Bits, block: &mut [i16; 64], n: usize, mquant: i32, ttmb: i32, first_block: bool, plane: usize, dst: usize, ls: usize, block_tt: &mut i32) -> Result<i32, Error> {
        let tb = tables();
        let quant = mquant.abs();
        *block = [0; 64];
        let mut ttblk = ttmb & 7;
        let mut subblkpat: i32 = 0;
        if ttmb == -1 {
            let k = tb.ttblk[self.tt_index].read(r);
            if k < 0 {
                return Err(Error::Data("TTBLK VLC"));
            }
            ttblk = TTBLK_TO_TT[self.tt_index][k as usize];
        }
        if ttblk == TT_4X4 {
            subblkpat = !(tb.subblkpat[self.tt_index].read(r) + 1);
        }
        if ttblk != TT_8X8 && ttblk != TT_4X4 && ((self.ttmbf || (ttmb != -1 && ttmb & 8 != 0 && !first_block)) || (!self.res_rtm_flag && !first_block)) {
            subblkpat = decode012(r) as i32;
            if subblkpat != 0 {
                subblkpat ^= 3;
            }
            if ttblk == TT_8X4_TOP || ttblk == TT_8X4_BOTTOM {
                ttblk = TT_8X4;
            }
            if ttblk == TT_4X8_RIGHT || ttblk == TT_4X8_LEFT {
                ttblk = TT_4X8;
            }
        }
        let scale = quant * 2 + if mquant < 0 { 0 } else { self.halfpq };
        if ttblk == TT_8X4_TOP || ttblk == TT_8X4_BOTTOM {
            subblkpat = 2 - (ttblk == TT_8X4_TOP) as i32;
            ttblk = TT_8X4;
        }
        if ttblk == TT_4X8_RIGHT || ttblk == TT_4X8_LEFT {
            subblkpat = 2 - (ttblk == TT_4X8_LEFT) as i32;
            ttblk = TT_4X8;
        }
        let cs = self.codingset2;
        let put = |block: &mut [i16; 64], idx: usize, value: i32, pq: bool| {
            let mut v = value * scale;
            if !pq {
                v += if (v as i16) < 0 { -quant } else { quant };
            }
            block[idx] = v as i16;
        };
        let pquant = self.pquantizer;
        let mut pat = 0;
        match ttblk {
            TT_8X8 => {
                pat = 0xF;
                let mut i = 0usize;
                loop {
                    let (last, skip, value) = self.ac_coeff(r, cs)?;
                    i += skip as usize;
                    if i > 63 {
                        break;
                    }
                    put(block, self.zz()[0][i] as usize, value, pquant);
                    i += 1;
                    if last {
                        break;
                    }
                }
                let fast = self.fasttx;
                let d = self.plane(plane);
                if !fast && i == 1 {
                    idct::simple_add(&mut d[dst..], ls, block);
                } else if i == 1 {
                    inv_trans_8x8_dc(d, dst, ls, block);
                } else {
                    if fast {
                        inv_trans_8x8(block);
                    } else {
                        idct::simple_inplace(block);
                    }
                    add_pixels(d, dst, ls, block);
                }
            }
            TT_4X4 => {
                pat = !subblkpat & 0xF;
                for j in 0..4 {
                    let mut last = subblkpat & (1 << (3 - j)) != 0;
                    let mut i = 0usize;
                    let off = (j & 1) * 4 + (j & 2) * 16;
                    while !last {
                        let (l, skip, value) = self.ac_coeff(r, cs)?;
                        last = l;
                        i += skip as usize;
                        if i > 15 {
                            break;
                        }
                        put(block, t::VC1_SIMPLE_PROGRESSIVE_4X4_ZZ[i] as usize + off, value, pquant);
                        i += 1;
                    }
                    if subblkpat & (1 << (3 - j)) == 0 {
                        let o = dst + (j & 1) * 4 + (j & 2) * 2 * ls;
                        let fast = self.fasttx;
                        let d = self.plane(plane);
                        if !fast {
                            idct::simple44_add(d, o, ls, &mut block[off..]);
                        } else if i == 1 {
                            inv_trans_4x4_dc(d, o, ls, &block[off..]);
                        } else {
                            inv_trans_4x4(d, o, ls, &mut block[off..]);
                        }
                    }
                }
            }
            TT_8X4 => {
                pat = !((subblkpat & 2) * 6 + (subblkpat & 1) * 3) & 0xF;
                for j in 0..2 {
                    let mut last = subblkpat & (1 << (1 - j)) != 0;
                    let mut i = 0usize;
                    let off = j * 32;
                    while !last {
                        let (l, skip, value) = self.ac_coeff(r, cs)?;
                        last = l;
                        i += skip as usize;
                        if i > 31 {
                            break;
                        }
                        put(block, tb.zz_8x4[i] as usize + off, value, pquant);
                        i += 1;
                    }
                    if subblkpat & (1 << (1 - j)) == 0 {
                        let o = dst + j * 4 * ls;
                        let fast = self.fasttx;
                        let d = self.plane(plane);
                        if !fast {
                            idct::simple84_add(d, o, ls, &mut block[off..]);
                        } else if i == 1 {
                            inv_trans_8x4_dc(d, o, ls, &block[off..]);
                        } else {
                            inv_trans_8x4(d, o, ls, &mut block[off..]);
                        }
                    }
                }
            }
            TT_4X8 => {
                pat = !(subblkpat * 5) & 0xF;
                for j in 0..2 {
                    let mut last = subblkpat & (1 << (1 - j)) != 0;
                    let mut i = 0usize;
                    let off = j * 4;
                    while !last {
                        let (l, skip, value) = self.ac_coeff(r, cs)?;
                        last = l;
                        i += skip as usize;
                        if i > 31 {
                            break;
                        }
                        put(block, tb.zz_4x8[i] as usize + off, value, pquant);
                        i += 1;
                    }
                    if subblkpat & (1 << (1 - j)) == 0 {
                        let o = dst + j * 4;
                        let fast = self.fasttx;
                        let d = self.plane(plane);
                        if !fast {
                            idct::simple48_add(d, o, ls, &mut block[off..]);
                        } else if i == 1 {
                            inv_trans_4x8_dc(d, o, ls, &block[off..]);
                        } else {
                            inv_trans_4x8(d, o, ls, &mut block[off..]);
                        }
                    }
                }
            }
            _ => {}
        }
        *block_tt |= ttblk << (n * 4);
        Ok(pat)
    }

    /// GET_MQUANT
    fn mquant(&self, r: &mut Bits, mut mquant: i32) -> i32 {
        if !self.dquantfrm {
            return mquant;
        }
        let mut edges = 0;
        if self.dqprofile == DQPROFILE_ALL_MBS {
            if self.dqbilevel {
                mquant = if r.get1() != 0 { -self.altpq } else { self.pq };
            } else {
                let mqdiff = r.get(3) as i32;
                mquant = if mqdiff != 7 { -self.pq - mqdiff } else { -(r.get(5) as i32) };
            }
        }
        if self.dqprofile == DQPROFILE_SINGLE_EDGE {
            edges = 1 << self.dqsbedge;
        } else if self.dqprofile == DQPROFILE_DOUBLE_EDGES {
            edges = (3 << self.dqsbedge) % 15;
        } else if self.dqprofile == DQPROFILE_FOUR_EDGES {
            edges = 15;
        }
        if edges & 1 != 0 && self.mb_x == 0 {
            mquant = -self.altpq;
        }
        if edges & 2 != 0 && self.mb_y == 0 {
            mquant = -self.altpq;
        }
        if edges & 4 != 0 && self.mb_x == self.mb_w - 1 {
            mquant = -self.altpq;
        }
        if edges & 8 != 0 && self.mb_y == self.mb_h - 1 {
            mquant = -self.altpq;
        }
        if mquant == 0 || !(-31..=31).contains(&mquant) {
            mquant = 1;
        }
        mquant
    }

    /// GET_MVDATA: (dmv_x, dmv_y, mb_has_coeffs); sets mb_intra
    fn mvdata(&mut self, r: &mut Bits) -> (i32, i32, bool) {
        let mut index = 1 + tables().mv_diff[self.mv_table_index].read(r);
        let has = index > 36;
        if has {
            index -= 37;
        }
        self.mb_intra = false;
        let q = self.quarter_sample;
        match index {
            0 => (0, 0, has),
            35 => {
                let x = r.get(self.k_x - 1 + q as u32) as i32;
                let y = r.get(self.k_y - 1 + q as u32) as i32;
                (x, y, has)
            }
            36 => {
                self.mb_intra = true;
                (0, 0, has)
            }
            _ => {
                let comp = |r: &mut Bits, i1: usize| {
                    let mut d = OFFSET_TABLE1[i1];
                    let val = SIZE_TABLE[i1] as i32 - (!q && i1 == 5) as i32;
                    if val > 0 {
                        let v = r.get(val as u32) as i32;
                        let sign = -(v & 1);
                        d = (sign ^ ((v >> 1) + d)) - sign;
                    }
                    d
                };
                let index = index.max(0) as usize;
                let x = comp(r, index % 6);
                let y = comp(r, index / 6);
                (x, y, has)
            }
        }
    }

    /// vc1_decode_p_mb
    fn p_mb(&mut self, r: &mut Bits) -> Result<(), Error> {
        let mb_pos = self.mb_x + self.mb_y * self.mbs;
        let mut ttmb = self.ttfrm;
        let mut first_block = true;
        let (mut block_cbp, mut block_tt, mut block_intra) = (0u32, 0i32, 0u32);
        let mut mquant = self.pq;
        let fourmv = if self.mv_type_is_raw { r.get1() != 0 } else { self.mv_type_plane[mb_pos] != 0 };
        let skipped = if self.skip_is_raw { r.get1() != 0 } else { self.skip_plane[mb_pos] != 0 };
        let ls = self.ls;
        let tb = tables();
        'end: {
            if !fourmv {
                if !skipped {
                    let (dx, dy, has) = self.mvdata(r);
                    self.pred_mv(r, 0, dx, dy, true);
                    let cbp;
                    if self.mb_intra && !has {
                        mquant = self.mquant(r, mquant);
                        self.ac_pred = r.get1() != 0;
                        cbp = 0;
                    } else if has {
                        if self.mb_intra {
                            self.ac_pred = r.get1() != 0;
                        }
                        cbp = tb.cbpcy[self.cbptab].read(r);
                        mquant = self.mquant(r, mquant);
                    } else {
                        mquant = self.pq;
                        cbp = 0;
                    }
                    self.qscale[mb_pos] = mquant;
                    if !self.ttmbf && !self.mb_intra && has {
                        ttmb = tb.ttmb[self.tt_index].read(r);
                    }
                    if !self.mb_intra {
                        self.mc_1mv(0);
                    }
                    for i in 0..6 {
                        let d = self.dci(i);
                        self.dc_val[d] = 0;
                        let val = (cbp >> (5 - i)) & 1 != 0;
                        self.mb_type[d] = self.mb_intra as u8;
                        if self.mb_intra {
                            self.avail(i);
                            let cs = if i & 4 != 0 { self.codingset2 } else { self.codingset };
                            let (cb, bm) = (self.cur_blk, BLOCK_MAP[i]);
                            let mut blk = self.blocks[cb][bm];
                            self.intra_block(r, &mut blk, i, val, mquant, cs)?;
                            self.itrans(&mut blk);
                            if self.rangeredfrm {
                                for v in blk.iter_mut() {
                                    *v = v.wrapping_mul(2);
                                }
                            }
                            self.blocks[cb][bm] = blk;
                            block_cbp |= 0xF << (i << 2);
                            block_intra |= 1 << i;
                        } else if val {
                            let (plane, dst, stride) = self.block_dst(i);
                            let mut blk = [0i16; 64];
                            let pat = self.p_block(r, &mut blk, i, mquant, ttmb, first_block, plane, dst, stride, &mut block_tt)?;
                            self.blocks[self.cur_blk][BLOCK_MAP[i]] = blk;
                            block_cbp |= (pat as u32) << (i << 2);
                            if !self.ttmbf && ttmb < 8 {
                                ttmb = -1;
                            }
                            first_block = false;
                        }
                    }
                } else {
                    self.mb_intra = false;
                    for i in 0..6 {
                        let d = self.dci(i);
                        self.mb_type[d] = 0;
                        self.dc_val[d] = 0;
                    }
                    self.qscale[mb_pos] = 0;
                    self.pred_mv(r, 0, 0, 0, true);
                    self.mc_1mv(0);
                }
            } else if !skipped {
                let (mut intra_count, mut coded_inter) = (0, false);
                let mut is_intra = [false; 6];
                let mut is_coded = [false; 6];
                let cbp = tb.cbpcy[self.cbptab].read(r);
                for i in 0..6 {
                    let val = (cbp >> (5 - i)) & 1 != 0;
                    let d = self.dci(i);
                    self.dc_val[d] = 0;
                    self.mb_intra = false;
                    if i < 4 {
                        let (mut dx, mut dy, mut has) = (0, 0, false);
                        if val {
                            (dx, dy, has) = self.mvdata(r);
                        }
                        self.pred_mv(r, i, dx, dy, false);
                        if !self.mb_intra {
                            self.mc_4mv_luma(i);
                        }
                        intra_count += self.mb_intra as i32;
                        is_intra[i] = self.mb_intra;
                        is_coded[i] = has;
                    }
                    if i & 4 != 0 {
                        is_intra[i] = intra_count >= 3;
                        is_coded[i] = val;
                    }
                    if i == 4 {
                        self.mc_4mv_chroma();
                    }
                    self.mb_type[d] = is_intra[i] as u8;
                    if !coded_inter {
                        coded_inter = !is_intra[i] && is_coded[i];
                    }
                }
                if intra_count == 0 && !coded_inter {
                    break 'end;
                }
                mquant = self.mquant(r, mquant);
                self.qscale[mb_pos] = mquant;
                let mut intrapred = false;
                for i in 0..6 {
                    if is_intra[i] {
                        let d = self.dci(i);
                        if ((!self.first_slice_line || i == 2 || i == 3) && self.mb_type[d - self.wrap(i)] != 0) || ((self.mb_x != 0 || i == 1 || i == 3) && self.mb_type[d - 1] != 0) {
                            intrapred = true;
                            break;
                        }
                    }
                }
                self.ac_pred = intrapred && r.get1() != 0;
                if !self.ttmbf && coded_inter {
                    ttmb = tb.ttmb[self.tt_index].read(r);
                }
                for i in 0..6 {
                    self.mb_intra = is_intra[i];
                    if is_intra[i] {
                        self.avail(i);
                        let cs = if i & 4 != 0 { self.codingset2 } else { self.codingset };
                        let (cb, bm) = (self.cur_blk, BLOCK_MAP[i]);
                        let mut blk = self.blocks[cb][bm];
                        self.intra_block(r, &mut blk, i, is_coded[i], mquant, cs)?;
                        self.itrans(&mut blk);
                        if self.rangeredfrm {
                            for v in blk.iter_mut() {
                                *v = v.wrapping_mul(2);
                            }
                        }
                        self.blocks[cb][bm] = blk;
                        block_cbp |= 0xF << (i << 2);
                        block_intra |= 1 << i;
                    } else if is_coded[i] {
                        let (plane, dst, stride) = self.block_dst(i);
                        let mut blk = [0i16; 64];
                        let pat = self.p_block(r, &mut blk, i, mquant, ttmb, first_block, plane, dst, stride, &mut block_tt)?;
                        self.blocks[self.cur_blk][BLOCK_MAP[i]] = blk;
                        block_cbp |= (pat as u32) << (i << 2);
                        if !self.ttmbf && ttmb < 8 {
                            ttmb = -1;
                        }
                        first_block = false;
                    }
                }
            } else {
                self.mb_intra = false;
                self.qscale[mb_pos] = 0;
                for i in 0..6 {
                    let d = self.dci(i);
                    self.mb_type[d] = 0;
                    self.dc_val[d] = 0;
                }
                for i in 0..4 {
                    self.pred_mv(r, i, 0, 0, false);
                    self.mc_4mv_luma(i);
                }
                self.mc_4mv_chroma();
                self.qscale[mb_pos] = 0;
            }
        }
        let _ = ls;
        if self.overlap && self.pq >= 9 {
            self.p_overlap_filter();
        }
        self.put_blocks_clamped(true);
        let k = 2 * self.mbs + self.mb_x;
        self.cbp[k] = block_cbp;
        self.ttblk[k] = block_tt;
        self.is_intra[k] = block_intra as u8;
        Ok(())
    }

    fn b_blocks(&mut self, r: &mut Bits) -> Result<(), Error> {
        self.codingset = match self.c_ac_table_index {
            0 => if self.pqindex <= 8 { CS_HIGH_RATE_INTRA } else { CS_LOW_MOT_INTRA },
            1 => CS_HIGH_MOT_INTRA,
            _ => CS_MID_RATE_INTRA,
        };
        self.codingset2 = match self.c_ac_table_index {
            0 => if self.pqindex <= 8 { CS_HIGH_RATE_INTER } else { CS_LOW_MOT_INTER },
            1 => CS_HIGH_MOT_INTER,
            _ => CS_MID_RATE_INTER,
        };
        self.first_slice_line = true;
        let mbs = self.mbs;
        for mb_y in 0..self.mb_h {
            self.mb_y = mb_y;
            for mb_x in 0..self.mb_w {
                self.mb_x = mb_x;
                self.init_block_index();
                if (self.skip_is_raw || self.dmb_is_raw) && r.left() <= 1 {
                    self.ffmpeg_conceals = true;
                }
                self.b_mb(r)?;
                if self.loop_filter {
                    self.i_loop_filter();
                }
                if r.left() < 0 {
                    return Err(Error::Data("bits overconsumption"));
                }
            }
            self.cbp.copy_within(mbs..3 * mbs, 0);
            self.ttblk.copy_within(mbs..3 * mbs, 0);
            self.is_intra.copy_within(mbs..3 * mbs, 0);
            self.first_slice_line = false;
        }
        Ok(())
    }

    /// vc1_b_mc
    fn b_mc(&mut self, direct: bool, mode: u8) {
        if direct || mode == BMV_INTERPOLATED {
            self.mc_1mv(0);
            self.interp_mc();
        } else {
            self.mc_1mv((mode == BMV_BACKWARD) as usize);
        }
    }

    /// vc1_decode_b_mb
    fn b_mb(&mut self, r: &mut Bits) -> Result<(), Error> {
        let mb_pos = self.mb_x + self.mb_y * self.mbs;
        let mut ttmb = self.ttfrm;
        let mut first_block = true;
        let mut mquant = self.pq;
        let mut has = false;
        let mut bmvtype = BMV_BACKWARD;
        let tb = tables();
        self.mb_intra = false;
        let direct = if self.dmb_is_raw { r.get1() != 0 } else { self.direct_plane[mb_pos] != 0 };
        let skipped = if self.skip_is_raw { r.get1() != 0 } else { self.skip_plane[mb_pos] != 0 };
        let (mut dmv_x, mut dmv_y) = ([0i32; 2], [0i32; 2]);
        for i in 0..6 {
            let d = self.dci(i);
            self.mb_type[d] = 0;
            self.dc_val[d] = 0;
        }
        self.qscale[mb_pos] = 0;
        if !direct {
            if !skipped {
                let (x, y, h) = self.mvdata(r);
                has = h;
                dmv_x = [x, x];
                dmv_y = [y, y];
            }
            if skipped || !self.mb_intra {
                let later = self.bfraction >= 128;
                bmvtype = match decode012(r) {
                    0 => if later { BMV_BACKWARD } else { BMV_FORWARD },
                    1 => if later { BMV_FORWARD } else { BMV_BACKWARD },
                    _ => {
                        dmv_x[0] = 0;
                        dmv_y[0] = 0;
                        BMV_INTERPOLATED
                    }
                };
            }
        }
        for i in 0..6 {
            let d = self.dci(i);
            self.mb_type[d] = self.mb_intra as u8;
        }
        if skipped {
            if direct {
                bmvtype = BMV_INTERPOLATED;
            }
            self.pred_b_mv(dmv_x, dmv_y, direct, bmvtype);
            self.b_mc(direct, bmvtype);
            return Ok(());
        }
        let cbp;
        if direct {
            cbp = tb.cbpcy[self.cbptab].read(r);
            mquant = self.mquant(r, mquant);
            self.mb_intra = false;
            self.qscale[mb_pos] = mquant;
            if !self.ttmbf {
                ttmb = tb.ttmb[self.tt_index].read(r);
            }
            self.pred_b_mv([0; 2], [0; 2], direct, bmvtype);
            self.b_mc(direct, bmvtype);
        } else {
            if !has && !self.mb_intra {
                // no coded blocks: as good as skipped
                self.pred_b_mv(dmv_x, dmv_y, direct, bmvtype);
                self.b_mc(direct, bmvtype);
                return Ok(());
            }
            if self.mb_intra && !has {
                mquant = self.mquant(r, mquant);
                self.qscale[mb_pos] = mquant;
                self.ac_pred = r.get1() != 0;
                cbp = 0;
                self.pred_b_mv(dmv_x, dmv_y, direct, bmvtype);
            } else {
                if bmvtype == BMV_INTERPOLATED {
                    let (x, y, h) = self.mvdata(r);
                    has = h;
                    dmv_x[0] = x;
                    dmv_y[0] = y;
                    if !has {
                        // interpolated skipped block
                        self.pred_b_mv(dmv_x, dmv_y, direct, bmvtype);
                        self.b_mc(direct, bmvtype);
                        return Ok(());
                    }
                }
                self.pred_b_mv(dmv_x, dmv_y, direct, bmvtype);
                if !self.mb_intra {
                    self.b_mc(direct, bmvtype);
                }
                if self.mb_intra {
                    self.ac_pred = r.get1() != 0;
                }
                cbp = tb.cbpcy[self.cbptab].read(r);
                mquant = self.mquant(r, mquant);
                self.qscale[mb_pos] = mquant;
                if !self.ttmbf && !self.mb_intra && has {
                    ttmb = tb.ttmb[self.tt_index].read(r);
                }
            }
        }
        let mut block_tt = 0;
        for i in 0..6 {
            let d = self.dci(i);
            self.dc_val[d] = 0;
            let val = (cbp >> (5 - i)) & 1 != 0;
            self.mb_type[d] = self.mb_intra as u8;
            let (plane, dst, stride) = self.block_dst(i);
            if self.mb_intra {
                self.avail(i);
                let cs = if i & 4 != 0 { self.codingset2 } else { self.codingset };
                let mut blk = [0i16; 64];
                self.intra_block(r, &mut blk, i, val, mquant, cs)?;
                self.itrans(&mut blk);
                if self.rangeredfrm {
                    for v in blk.iter_mut() {
                        *v = v.wrapping_mul(2);
                    }
                }
                // put_signed_pixels_clamped
                let p = self.plane(plane);
                for y in 0..8 {
                    for x in 0..8 {
                        p[dst + y * stride + x] = clip8(blk[y * 8 + x] as i32 + 128);
                    }
                }
            } else if val {
                let mut blk = [0i16; 64];
                self.p_block(r, &mut blk, i, mquant, ttmb, first_block, plane, dst, stride, &mut block_tt)?;
                if !self.ttmbf && ttmb < 8 {
                    ttmb = -1;
                }
                first_block = false;
            }
        }
        Ok(())
    }

    /// ff_vc1_pred_b_mv: s->mv[0][0] / [1][0] and motion_val[0] / [1] at the MB's first block
    fn pred_b_mv(&mut self, mut dmv_x: [i32; 2], mut dmv_y: [i32; 2], direct: bool, mvtype: u8) {
        let (rx, ry) = (self.range_x, self.range_y);
        if !self.quarter_sample {
            for k in 0..2 {
                dmv_x[k] *= 2;
                dmv_y[k] *= 2;
            }
        }
        let wrap = self.b8;
        let xy = self.mvi(0);
        if self.mb_intra {
            self.motion[xy] = [0, 0];
            self.motion1[xy] = [0, 0];
            return;
        }
        let qs = self.quarter_sample;
        let bf = self.bfraction;
        let n = self.next_motion[xy];
        let (bx, by) = (self.mb_x as i32, self.mb_y as i32);
        let (mw, mh) = (self.mb_w as i32, self.mb_h as i32);
        let px0 = (scale_mv(n[0] as i32, bf, false, qs)).clamp(-60 - (bx << 6), (mw << 6) - 4 - (bx << 6));
        let py0 = (scale_mv(n[1] as i32, bf, false, qs)).clamp(-60 - (by << 6), (mh << 6) - 4 - (by << 6));
        let px1 = (scale_mv(n[0] as i32, bf, true, qs)).clamp(-60 - (bx << 6), (mw << 6) - 4 - (bx << 6));
        let py1 = (scale_mv(n[1] as i32, bf, true, qs)).clamp(-60 - (by << 6), (mh << 6) - 4 - (by << 6));
        self.mv[0] = [px0, py0];
        self.mv_b = [px1, py1];
        if !direct {
            for dir in 0..2 {
                if dir == 0 && mvtype == BMV_BACKWARD || dir == 1 && mvtype == BMV_FORWARD {
                    continue;
                }
                let mv = if dir == 0 { &mut self.motion } else { &mut self.motion1 };
                if self.mb_x == 0 {
                    mv[xy - 2] = [0, 0];
                }
                let c = mv[xy - 2].map(|v| v as i32);
                let (mut px, mut py) = if !self.first_slice_line {
                    let a = mv[xy - wrap * 2].map(|v| v as i32);
                    let off: isize = if self.mb_x == self.mb_w - 1 { -2 } else { 2 };
                    let b = mv[(xy as isize - wrap as isize * 2 + off) as usize].map(|v| v as i32);
                    if self.mb_w == 1 { (a[0], a[1]) } else { (mid_pred(a[0], b[0], c[0]), mid_pred(a[1], b[1], c[1])) }
                } else if self.mb_x != 0 {
                    (c[0], c[1])
                } else {
                    (0, 0)
                };
                // pullback (8.3.5.3.4)
                let (qx, qy) = (bx << 5, by << 5);
                let (xm, ym) = ((mw << 5) - 4, (mh << 5) - 4);
                let mvmin = 4 - (1 << 5);
                if qx + px < mvmin {
                    px = mvmin - qx;
                }
                if qy + py < mvmin {
                    py = mvmin - qy;
                }
                if qx + px > xm {
                    px = xm - qx;
                }
                if qy + py > ym {
                    py = ym - qy;
                }
                let m = [((px + dmv_x[dir] + rx) & ((rx << 1) - 1)) - rx, ((py + dmv_y[dir] + ry) & ((ry << 1) - 1)) - ry];
                if dir == 0 {
                    self.mv[0] = m;
                } else {
                    self.mv_b = m;
                }
            }
        }
        self.motion[xy] = [self.mv[0][0] as i16, self.mv[0][1] as i16];
        self.motion1[xy] = [self.mv_b[0] as i16, self.mv_b[1] as i16];
    }

    /// a_avail / c_avail of an intra block
    fn avail(&mut self, i: usize) {
        let d = self.dci(i);
        self.a_avail = (i == 2 || i == 3 || !self.first_slice_line) && self.mb_type[d - self.wrap(i)] != 0;
        self.c_avail = (i == 1 || i == 3 || self.mb_x != 0) && self.mb_type[d - 1] != 0;
    }

    /// (plane, offset, stride) of block `i` of the MB
    fn block_dst(&self, i: usize) -> (usize, usize, usize) {
        if i < 4 {
            (0, self.dest(0) as usize + (i & 1) * 8 + (i & 2) * 4 * self.ls, self.ls)
        } else {
            (i - 3, self.dest(1) as usize, self.uvls)
        }
    }

    /// vc1_put_blocks_clamped: the intra blocks of the MB up and left (once overlap smoothed)
    fn put_blocks_clamped(&mut self, signed: bool) {
        let (ls, uvls, b8, mbs) = (self.ls as isize, self.uvls as isize, self.b8, self.mbs);
        let d0 = self.dest(0);
        let dc = self.dest(1);
        let put = |this: &mut Self, blk: usize, i: usize, at: isize| {
            let stride = if i > 3 { uvls } else { ls } as usize;
            let b = this.blocks[blk][BLOCK_MAP[i]];
            let p = this.plane(if i > 3 { i - 3 } else { 0 });
            for y in 0..8 {
                for x in 0..8 {
                    let v = b[y * 8 + x] as i32;
                    p[at as usize + y * stride + x] = if signed { clip8(v + 128) } else { clip8(v) };
                }
            }
        };
        let wrap = |i: usize| if i > 3 { mbs } else { b8 };
        if !self.first_slice_line {
            if self.mb_x != 0 {
                for i in 0..6 {
                    let d = self.dci(i);
                    let k = if i > 3 { d - wrap(i) - 1 } else { d - 2 * wrap(i) - 2 };
                    if self.mb_type[k] != 0 {
                        let at = if i > 3 { dc - 8 * uvls - 8 } else { d0 + ((i as isize & 2) - 4) * 4 * ls + ((i as isize & 1) - 2) * 8 };
                        put(self, self.topleft_blk, i, at);
                    }
                }
            }
            if self.mb_x == self.mb_w - 1 {
                for i in 0..6 {
                    let d = self.dci(i);
                    let k = if i > 3 { d - wrap(i) } else { d - 2 * wrap(i) };
                    if self.mb_type[k] != 0 {
                        let at = if i > 3 { dc - 8 * uvls } else { d0 + ((i as isize & 2) - 4) * 4 * ls + (i as isize & 1) * 8 };
                        put(self, self.top_blk, i, at);
                    }
                }
            }
        }
        if self.mb_y == self.mb_h - 1 {
            if self.mb_x != 0 {
                for i in 0..6 {
                    let d = self.dci(i);
                    let k = if i > 3 { d - 1 } else { d - 2 };
                    if self.mb_type[k] != 0 {
                        let at = if i > 3 { dc - 8 } else { d0 + (i as isize & 2) * 4 * ls + ((i as isize & 1) - 2) * 8 };
                        put(self, self.left_blk, i, at);
                    }
                }
            }
            if self.mb_x == self.mb_w - 1 {
                for i in 0..6 {
                    if self.mb_type[self.dci(i)] != 0 {
                        let at = if i > 3 { dc } else { d0 + (i as isize & 2) * 4 * ls + (i as isize & 1) * 8 };
                        put(self, self.cur_blk, i, at);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// overlap smoothing and the loop filter (vc1_loopfilter.c)

impl Decoder {
    fn h_overlap(&mut self, left: usize, right: usize, i: usize) {
        let b = &mut self.blocks;
        match i {
            0 => h_s_overlap(b, (left, 2, 0), (right, 0, 0), 8, 8, 1),
            1 => h_s_overlap(b, (right, 0, 0), (right, 2, 0), 8, 8, 1),
            2 => h_s_overlap(b, (left, 3, 0), (right, 1, 0), 8, 8, 1),
            3 => h_s_overlap(b, (right, 1, 0), (right, 3, 0), 8, 8, 1),
            _ => h_s_overlap(b, (left, i, 0), (right, i, 0), 8, 8, 1),
        }
    }
    fn v_overlap(&mut self, top: usize, bottom: usize, i: usize) {
        let b = &mut self.blocks;
        match i {
            0 => v_s_overlap(b, (top, 1), (bottom, 0)),
            1 => v_s_overlap(b, (top, 3), (bottom, 2)),
            2 => v_s_overlap(b, (bottom, 0), (bottom, 1)),
            3 => v_s_overlap(b, (bottom, 2), (bottom, 3)),
            _ => v_s_overlap(b, (top, i), (bottom, i)),
        }
    }

    /// ff_vc1_i_overlap_filter (pq >= 9)
    fn i_overlap_filter(&mut self) {
        let (tl, t, l, c) = (self.topleft_blk, self.top_blk, self.left_blk, self.cur_blk);
        for i in 0..6 {
            if self.mb_x == 0 && (i & 5) != 1 {
                continue;
            }
            self.h_overlap(if self.mb_x != 0 { l } else { c }, c, i);
        }
        for i in 0..6 {
            if self.first_slice_line && i & 2 == 0 {
                continue;
            }
            if self.mb_x != 0 {
                self.v_overlap(if self.first_slice_line { l } else { tl }, l, i);
            }
            if self.mb_x == self.mb_w - 1 {
                self.v_overlap(if self.first_slice_line { c } else { t }, c, i);
            }
        }
    }

    /// ff_vc1_p_overlap_filter
    fn p_overlap_filter(&mut self) {
        let (tl, t, l, c) = (self.topleft_blk, self.top_blk, self.left_blk, self.cur_blk);
        for i in 0..6 {
            if self.mb_x == 0 && (i & 5) != 1 {
                continue;
            }
            let d = self.dci(i);
            if self.mb_type[d] != 0 && self.mb_type[d - 1] != 0 {
                self.h_overlap(if self.mb_x != 0 { l } else { c }, c, i);
            }
        }
        for i in 0..6 {
            if self.first_slice_line && i & 2 == 0 {
                continue;
            }
            let d = self.dci(i);
            let w = self.wrap(i);
            let x = (i > 3) as usize;
            if self.mb_x != 0 && self.mb_type[d - 2 + x] != 0 && self.mb_type[d - w - 2 + x] != 0 {
                self.v_overlap(if self.first_slice_line { l } else { tl }, l, i);
            }
            if self.mb_x == self.mb_w - 1 && self.mb_type[d] != 0 && self.mb_type[d - w] != 0 {
                self.v_overlap(if self.first_slice_line { c } else { t }, c, i);
            }
        }
    }

    /// (plane, position) of block `i` at luma `dest` / chroma `cdest`
    fn lf_dst(&self, dest: isize, cdest: isize, i: usize) -> (usize, isize, usize) {
        if i > 3 {
            (i - 3, cdest, self.uvls)
        } else {
            (0, dest + (i as isize & 2) * 4 * self.ls as isize + (i as isize & 1) * 8, self.ls)
        }
    }

    fn i_h_lf(&mut self, dest: isize, cdest: isize, flags: u32, i: usize) {
        if i & 2 != 0 {
            return;
        }
        if flags & LEFT_EDGE == 0 || (i & 5) == 1 {
            let (k, p, s) = self.lf_dst(dest, cdest, i);
            let pq = self.pq;
            h_lf(self.plane(k), p, s, if i > 3 { 8 } else { 16 }, pq);
        }
    }
    fn i_v_lf(&mut self, dest: isize, cdest: isize, flags: u32, i: usize) {
        if (i & 5) == 1 {
            return;
        }
        if flags & TOP_EDGE == 0 || i & 2 != 0 {
            let (k, p, s) = self.lf_dst(dest, cdest, i);
            let pq = self.pq;
            v_lf(self.plane(k), p, s, if i > 3 { 8 } else { 16 }, pq);
        }
    }

    /// ff_vc1_i_loop_filter
    fn i_loop_filter(&mut self) {
        let (ls, uvls) = (self.ls as isize, self.uvls as isize);
        let (d0, dc) = (self.dest(0), self.dest(1));
        let last_x = self.mb_x == self.mb_w - 1;
        if !self.first_slice_line {
            let dest = d0 - 16 * ls - 16;
            let flags = if self.mb_y == 1 { TOP_EDGE } else { 0 };
            if self.mb_x != 0 {
                for i in 0..6 {
                    self.i_v_lf(dest, dc - 8 * uvls - 8, flags, i);
                }
            }
            if last_x {
                for i in 0..6 {
                    self.i_v_lf(dest + 16, dc - 8 * uvls, flags, i);
                }
            }
        }
        if self.mb_y == self.mb_h - 1 {
            let dest = d0 - 16;
            let flags = if self.first_slice_line { TOP_EDGE | BOTTOM_EDGE } else { BOTTOM_EDGE };
            if self.mb_x != 0 {
                for i in 0..6 {
                    self.i_v_lf(dest, dc - 8, flags, i);
                }
            }
            if last_x {
                for i in 0..6 {
                    self.i_v_lf(dest + 16, dc, flags, i);
                }
            }
        }
        if self.mb_y >= 2 {
            let dest = d0 - 32 * ls - 16;
            if self.mb_x != 0 {
                let flags = if self.mb_x == 1 { LEFT_EDGE } else { 0 };
                for i in 0..6 {
                    self.i_h_lf(dest, dc - 16 * uvls - 8, flags, i);
                }
            }
            if last_x {
                let flags = if self.mb_x == 0 { LEFT_EDGE | RIGHT_EDGE } else { RIGHT_EDGE };
                for i in 0..6 {
                    self.i_h_lf(dest + 16, dc - 16 * uvls, flags, i);
                }
            }
        }
        if self.mb_y == self.mb_h - 1 {
            if self.mb_y >= 1 {
                let dest = d0 - 16 * ls - 16;
                if self.mb_x != 0 {
                    let flags = if self.mb_x == 1 { LEFT_EDGE } else { 0 };
                    for i in 0..6 {
                        self.i_h_lf(dest, dc - 8 * uvls - 8, flags, i);
                    }
                }
                if last_x {
                    let flags = if self.mb_x == 0 { LEFT_EDGE | RIGHT_EDGE } else { RIGHT_EDGE };
                    for i in 0..6 {
                        self.i_h_lf(dest + 16, dc - 8 * uvls, flags, i);
                    }
                }
            }
            let dest = d0 - 16;
            if self.mb_x != 0 {
                let flags = if self.mb_x == 1 { LEFT_EDGE } else { 0 };
                for i in 0..6 {
                    self.i_h_lf(dest, dc - 8, flags, i);
                }
            }
            if last_x {
                let flags = if self.mb_x == 0 { LEFT_EDGE | RIGHT_EDGE } else { RIGHT_EDGE };
                for i in 0..6 {
                    self.i_h_lf(dest + 16, dc, flags, i);
                }
            }
        }
    }

    /// The facts of MB (mb_x + dx, row dy of the three kept) and its block `i`'s motion
    /// vector index: (cbp at, motion index, its stride to the next block right / below).
    fn p_lf_mv(&self, i: usize, dx: isize, dy: isize, right: bool) -> ([i16; 2], [i16; 2]) {
        if i > 3 {
            let k = (2 * self.mbs as isize + self.mb_x as isize + dx + dy * self.mbs as isize) as usize;
            let n = if right { k + 1 } else { k + self.mbs };
            (self.luma_mv[k], self.luma_mv[n])
        } else {
            let k = (self.mvi(i) as isize + 2 * dy * self.b8 as isize + 2 * dx) as usize;
            let n = if right { k + 1 } else { k + self.b8 };
            (self.motion[k], self.motion[n])
        }
    }

    /// vc1_p_h_loop_filter of block `i` of the MB at (dx, dy) MBs from this one
    fn p_h_lf(&mut self, dest: isize, cdest: isize, dx: isize, dy: isize, flags: u32, i: usize) {
        let (k, dst, ls) = self.lf_dst(dest, cdest, i);
        let m = (2 * self.mbs as isize + self.mb_x as isize + dx + dy * self.mbs as isize) as usize;
        let pq = self.pq;
        let left_cbp = self.cbp[m] >> (i * 4);
        if flags & RIGHT_EDGE == 0 || i & 5 == 0 {
            let left_is_intra = self.is_intra[m] & (1 << i) != 0;
            let (right_is_intra, right_cbp) = if i > 3 {
                (self.is_intra[m + 1] & (1 << i) != 0, self.cbp[m + 1] >> (i * 4))
            } else if i & 1 != 0 {
                (self.is_intra[m + 1] & (1 << (i - 1)) != 0, self.cbp[m + 1] >> ((i - 1) * 4))
            } else {
                (self.is_intra[m] & (1 << (i + 1)) != 0, self.cbp[m] >> ((i + 1) * 4))
            };
            let (mv0, mv1) = self.p_lf_mv(i, dx, dy, true);
            let p = self.plane(k);
            if left_is_intra || right_is_intra || mv0 != mv1 {
                h_lf(p, dst + 8, ls, 8, pq);
            } else {
                let idx = (left_cbp | (right_cbp >> 1)) & 5;
                if idx & 1 != 0 {
                    h_lf(p, dst + 4 * ls as isize + 8, ls, 4, pq);
                }
                if idx & 4 != 0 {
                    h_lf(p, dst + 8, ls, 4, pq);
                }
            }
        }
        let tt = (self.ttblk[m] >> (i * 4)) & 0xf;
        if tt == TT_4X4 || tt == TT_4X8 {
            let p = self.plane(k);
            if left_cbp & 3 != 0 {
                h_lf(p, dst + 4 * ls as isize + 4, ls, 4, pq);
            }
            if left_cbp & 12 != 0 {
                h_lf(p, dst + 4, ls, 4, pq);
            }
        }
    }

    /// vc1_p_v_loop_filter of block `i` of the MB at (dx, dy)
    fn p_v_lf(&mut self, dest: isize, cdest: isize, dx: isize, dy: isize, flags: u32, i: usize) {
        let (k, dst, ls) = self.lf_dst(dest, cdest, i);
        let mbs = self.mbs;
        let m = (2 * mbs as isize + self.mb_x as isize + dx + dy * mbs as isize) as usize;
        let pq = self.pq;
        let top_cbp = self.cbp[m] >> (i * 4);
        if flags & BOTTOM_EDGE == 0 || i < 2 {
            let top_is_intra = self.is_intra[m] & (1 << i) != 0;
            let (bottom_is_intra, bottom_cbp) = if i > 3 {
                (self.is_intra[m + mbs] & (1 << i) != 0, self.cbp[m + mbs] >> (i * 4))
            } else if i < 2 {
                (self.is_intra[m] & (1 << (i + 2)) != 0, self.cbp[m] >> ((i + 2) * 4))
            } else {
                (self.is_intra[m + mbs] & (1 << (i - 2)) != 0, self.cbp[m + mbs] >> ((i - 2) * 4))
            };
            let (mv0, mv1) = self.p_lf_mv(i, dx, dy, false);
            let p = self.plane(k);
            if top_is_intra || bottom_is_intra || mv0 != mv1 {
                v_lf(p, dst + 8 * ls as isize, ls, 8, pq);
            } else {
                let idx = (top_cbp | (bottom_cbp >> 2)) & 3;
                if idx & 1 != 0 {
                    v_lf(p, dst + 8 * ls as isize + 4, ls, 4, pq);
                }
                if idx & 2 != 0 {
                    v_lf(p, dst + 8 * ls as isize, ls, 4, pq);
                }
            }
        }
        let tt = (self.ttblk[m] >> (i * 4)) & 0xf;
        if tt == TT_4X4 || tt == TT_8X4 {
            let p = self.plane(k);
            if top_cbp & 5 != 0 {
                v_lf(p, dst + 4 * ls as isize + 4, ls, 4, pq);
            }
            if top_cbp & 10 != 0 {
                v_lf(p, dst + 4 * ls as isize, ls, 4, pq);
            }
        }
    }

    /// ff_vc1_p_loop_filter
    fn p_loop_filter(&mut self) {
        let (ls, uvls) = (self.ls as isize, self.uvls as isize);
        let (d0, dc) = (self.dest(0), self.dest(1));
        let (x, y) = (self.mb_x, self.mb_y);
        let last_x = x == self.mb_w - 1;
        let last_y = y == self.mb_h - 1;
        let v = |s: &mut Self, dest: isize, cd: isize, dx: isize, dy: isize, flags: u32| {
            for i in 0..6 {
                s.p_v_lf(dest, cd, dx, dy, flags, i);
            }
        };
        let h = |s: &mut Self, dest: isize, cd: isize, dx: isize, dy: isize, flags: u32| {
            for i in 0..6 {
                s.p_h_lf(dest, cd, dx, dy, flags, i);
            }
        };
        if y >= 2 {
            let flags = if y == 2 { TOP_EDGE } else { 0 };
            if x != 0 {
                v(self, d0 - 32 * ls - 16, dc - 16 * uvls - 8, -1, -2, flags);
            }
            if last_x {
                v(self, d0 - 32 * ls, dc - 16 * uvls, 0, -2, flags);
            }
        }
        if last_y {
            if x != 0 {
                if y >= 1 {
                    let flags = if y == 1 { TOP_EDGE } else { 0 };
                    v(self, d0 - 16 * ls - 16, dc - 8 * uvls - 8, -1, -1, flags);
                }
                let flags = if y == 0 { TOP_EDGE | BOTTOM_EDGE } else { BOTTOM_EDGE };
                v(self, d0 - 16, dc - 8, -1, 0, flags);
            }
            if last_x {
                if y >= 1 {
                    let flags = if y == 1 { TOP_EDGE } else { 0 };
                    v(self, d0 - 16 * ls, dc - 8 * uvls, 0, -1, flags);
                }
                let flags = if y == 0 { TOP_EDGE | BOTTOM_EDGE } else { BOTTOM_EDGE };
                v(self, d0, dc, 0, 0, flags);
            }
        }
        if y >= 2 {
            if x >= 2 {
                let flags = if x == 2 { LEFT_EDGE } else { 0 };
                h(self, d0 - 32 * ls - 32, dc - 16 * uvls - 16, -2, -2, flags);
            }
            if last_x {
                if x >= 1 {
                    let flags = if x == 1 { LEFT_EDGE } else { 0 };
                    h(self, d0 - 32 * ls - 16, dc - 16 * uvls - 8, -1, -2, flags);
                }
                let flags = if x != 0 { RIGHT_EDGE } else { LEFT_EDGE | RIGHT_EDGE };
                h(self, d0 - 32 * ls, dc - 16 * uvls, 0, -2, flags);
            }
        }
        if last_y {
            if y >= 1 {
                if x >= 2 {
                    let flags = if x == 2 { LEFT_EDGE } else { 0 };
                    h(self, d0 - 16 * ls - 32, dc - 8 * uvls - 16, -2, -1, flags);
                }
                if last_x {
                    if x >= 1 {
                        let flags = if x == 1 { LEFT_EDGE } else { 0 };
                        h(self, d0 - 16 * ls - 16, dc - 8 * uvls - 8, -1, -1, flags);
                    }
                    let flags = if x != 0 { RIGHT_EDGE } else { LEFT_EDGE | RIGHT_EDGE };
                    h(self, d0 - 16 * ls, dc - 8 * uvls, 0, -1, flags);
                }
            }
            if x >= 2 {
                let flags = if x == 2 { LEFT_EDGE } else { 0 };
                h(self, d0 - 32, dc - 16, -2, 0, flags);
            }
            if last_x {
                if x >= 1 {
                    let flags = if x == 1 { LEFT_EDGE } else { 0 };
                    h(self, d0 - 16, dc - 8, -1, 0, flags);
                }
                let flags = if x != 0 { RIGHT_EDGE } else { LEFT_EDGE | RIGHT_EDGE };
                h(self, d0, dc, 0, 0, flags);
            }
        }
    }
}

const LEFT_EDGE: u32 = 1;
const RIGHT_EDGE: u32 = 2;
const TOP_EDGE: u32 = 4;
const BOTTOM_EDGE: u32 = 8;

// ---------------------------------------------------------------------------------------
// motion vectors and compensation (vc1_pred.c, vc1_mc.c)

impl Decoder {
    /// ff_vc1_pred_mv (progressive, forward)
    fn pred_mv(&mut self, r: &mut Bits, n: usize, mut dmv_x: i32, mut dmv_y: i32, mv1: bool) {
        if !self.quarter_sample {
            dmv_x *= 2;
            dmv_y *= 2;
        }
        let wrap = self.b8;
        let xy = self.mvi(n);
        let ti = |s: &Self, k: usize| s.mb_type[k - mvo(s.b8) + s.b8 + 1] != 0;
        if self.mb_intra {
            self.mv[n] = [0, 0];
            self.motion[xy] = [0, 0];
            if mv1 {
                self.motion[xy + 1] = [0, 0];
                self.motion[xy + wrap] = [0, 0];
                self.motion[xy + wrap + 1] = [0, 0];
                let k = 2 * self.mbs + self.mb_x;
                self.luma_mv[k] = [0, 0];
            }
            return;
        }
        let a_valid = !self.first_slice_line || n == 2 || n == 3;
        let mut b_valid = a_valid;
        let c_valid = self.mb_x != 0 || n == 1 || n == 3;
        let mw = self.mb_w as isize;
        let off: isize = if mv1 {
            b_valid = b_valid && self.mb_w > 1;
            if self.mb_x == self.mb_w - 1 { -1 } else { 2 }
        } else {
            match n {
                0 => {
                    if self.res_rtm_flag {
                        if self.mb_x != 0 { -1 } else { 1 }
                    } else if self.mb_x != 0 {
                        -1
                    } else {
                        2 * mw - wrap as isize - 1
                    }
                }
                1 => if self.mb_x == self.mb_w - 1 { -1 } else { 1 },
                2 => 1,
                _ => -1,
            }
        };
        let get = |s: &Self, k: isize| s.motion[k as usize].map(|v| v as i32);
        let xyi = xy as isize;
        let a = if a_valid { get(self, xyi - wrap as isize) } else { [0, 0] };
        let b = if b_valid { get(self, xyi - wrap as isize + off) } else { [0, 0] };
        let c = if c_valid { get(self, xyi - 1) } else { [0, 0] };
        let count = a_valid as i32 + b_valid as i32 + c_valid as i32;
        let (mut px, mut py) = if a_valid {
            (a[0], a[1])
        } else if c_valid {
            (c[0], c[1])
        } else if b_valid {
            (b[0], b[1])
        } else {
            (0, 0)
        };
        if count > 1 {
            px = mid_pred(a[0], b[0], c[0]);
            py = mid_pred(a[1], b[1], c[1]);
        }
        // pullback
        let qx = ((self.mb_x as i32) << 6) + if n == 1 || n == 3 { 32 } else { 0 };
        let qy = ((self.mb_y as i32) << 6) + if n == 2 || n == 3 { 32 } else { 0 };
        let xm = ((self.mb_w as i32) << 6) - 4;
        let ym = ((self.mb_h as i32) << 6) - 4;
        let mvmin = if mv1 { -60 } else { -28 };
        if qx + px < mvmin {
            px = mvmin - qx;
        }
        if qy + py < mvmin {
            py = mvmin - qy;
        }
        if qx + px > xm {
            px = xm - qx;
        }
        if qy + py > ym {
            py = ym - qy;
        }
        // hybrid prediction
        if a_valid && c_valid {
            let sum = if ti(self, xy - wrap) { px.abs() + py.abs() } else { (px - a[0]).abs() + (py - a[1]).abs() };
            if sum > 32 {
                (px, py) = if r.get1() != 0 { (a[0], a[1]) } else { (c[0], c[1]) };
            } else {
                let sum = if ti(self, xy - 1) { px.abs() + py.abs() } else { (px - c[0]).abs() + (py - c[1]).abs() };
                if sum > 32 {
                    (px, py) = if r.get1() != 0 { (a[0], a[1]) } else { (c[0], c[1]) };
                }
            }
        }
        let (rx, ry) = (self.range_x, self.range_y);
        let mx = ((px + dmv_x + rx) & ((rx << 1) - 1)) - rx;
        let my = ((py + dmv_y + ry) & ((ry << 1) - 1)) - ry;
        self.mv[n] = [mx, my];
        let m = [mx as i16, my as i16];
        self.motion[xy] = m;
        if mv1 {
            self.motion[xy + 1] = m;
            self.motion[xy + wrap] = m;
            self.motion[xy + wrap + 1] = m;
        }
    }

    /// The reference's pixels around (x, y): `w` x `h` (clamped to the coded picture, then
    /// range-reduced / intensity-compensated as FFmpeg does its edge-emulated copies).
    /// `dir` 0: the forward reference (a P picture's, or a B one's older), 1: a B one's backward
    fn fetch(&self, dir: usize, k: usize, x: i32, y: i32, w: usize, h: usize, out: &mut [u8]) {
        let last = if dir == 0 && self.pict == Type::B { self.prev.as_ref() } else { self.last.as_ref() };
        let last = last.expect("reference");
        let (p, stride, pw, ph) = match k {
            0 => (&last.y, self.ls, self.ls as i32, (self.mb_h * 16) as i32),
            1 => (&last.u, self.uvls, self.uvls as i32, (self.mb_h * 8) as i32),
            _ => (&last.v, self.uvls, self.uvls as i32, (self.mb_h * 8) as i32),
        };
        hpel::window(p, stride, pw, ph, x, y, w, h, out);
        if self.rangeredfrm {
            for v in out[..w * h].iter_mut() {
                *v = (((*v as i32 - 128) >> 1) + 128) as u8;
            }
        }
        if self.use_ic && dir == 0 {
            let lut = if k == 0 { &self.luty } else { &self.lutuv };
            for v in out[..w * h].iter_mut() {
                *v = lut[*v as usize];
            }
        }
    }

    /// Luma `n` x `n` at dest offset `doff` from the quarter-pel vector (mx, my) at (src_x, src_y)
    /// of reference `dir`; `avg`: averaged into what is there (a B MB's second prediction)
    fn luma_mc(&mut self, dir: usize, doff: usize, src_x: i32, src_y: i32, mx: i32, my: i32, n: usize, avg: bool) {
        let mut win = [0u8; 19 * 19];
        let w = n + 3;
        self.fetch(dir, 0, src_x - 1, src_y - 1, w, w, &mut win);
        let ls = self.ls;
        let rnd = self.rnd;
        let mspel = self.mspel;
        let mut tmp = [0u8; 256];
        let (d, o, ds) = if avg { (&mut tmp[..], 0, n) } else { (&mut self.cur.y[..], doff, ls) };
        if mspel {
            mspel_mc(d, o, ds, &win, w + 1, w, n, (mx & 3) as usize, (my & 3) as usize, rnd);
        } else {
            let dxy = ((my & 2) | ((mx & 2) >> 1)) as usize;
            hpel::hpel(d, o, ds, &win[w + 1..], w, n, n, dxy, rnd == 0);
        }
        if avg {
            avg_into(&mut self.cur.y, doff, ls, &tmp, n, n);
        }
    }

    /// Chroma 8x8 from the eighth-pel-ish (uvmx, uvmy) at (uvsrc_x, uvsrc_y) of reference `dir`
    fn chroma_mc(&mut self, dir: usize, uvsrc_x: i32, uvsrc_y: i32, uvmx: i32, uvmy: i32, avg: bool) {
        let doff = self.dest(1) as usize;
        let (x, y) = ((uvmx & 3) << 1, (uvmy & 3) << 1);
        let no_rnd = self.rnd != 0;
        let uvls = self.uvls;
        for k in 1..3 {
            let mut win = [0u8; 81];
            self.fetch(dir, k, uvsrc_x, uvsrc_y, 9, 9, &mut win);
            if avg {
                let mut tmp = [0u8; 64];
                chroma_mc8(&mut tmp, 0, 8, &win, 9, x, y, no_rnd);
                avg_into(self.plane(k), doff, uvls, &tmp, 8, 8);
            } else {
                chroma_mc8(self.plane(k), doff, uvls, &win, 9, x, y, no_rnd);
            }
        }
    }

    fn fast_uv(&self, v: i32) -> i32 {
        if self.fastuvmc { v + if v < 0 { v & 1 } else { -(v & 1) } } else { v }
    }

    /// ff_vc1_mc_1mv: from reference `dir` (s->mv[dir][0])
    fn mc_1mv(&mut self, dir: usize) {
        let [mx, my] = if dir == 0 { self.mv[0] } else { self.mv_b };
        // the vectors a later B picture's direct MBs take
        if self.pict == Type::P {
            for i in 0..4 {
                let k = self.mvi(i);
                self.motion1[k] = [mx as i16, my as i16];
            }
        }
        let mut uvmx = (mx + ((mx & 3) == 3) as i32) >> 1;
        let mut uvmy = (my + ((my & 3) == 3) as i32) >> 1;
        let k = 2 * self.mbs + self.mb_x;
        self.luma_mv[k] = [uvmx as i16, uvmy as i16];
        uvmx = self.fast_uv(uvmx);
        uvmy = self.fast_uv(uvmy);
        let (bx, by) = (self.mb_x as i32, self.mb_y as i32);
        let (mw, mh) = (self.mb_w as i32, self.mb_h as i32);
        let src_x = (bx * 16 + (mx >> 2)).clamp(-16, mw * 16);
        let src_y = (by * 16 + (my >> 2)).clamp(-16, mh * 16);
        let uvsrc_x = (bx * 8 + (uvmx >> 2)).clamp(-8, mw * 8);
        let uvsrc_y = (by * 8 + (uvmy >> 2)).clamp(-8, mh * 8);
        let doff = self.dest(0) as usize;
        self.luma_mc(dir, doff, src_x, src_y, mx, my, 16, false);
        self.chroma_mc(dir, uvsrc_x, uvsrc_y, uvmx, uvmy, false);
    }

    /// ff_vc1_interp_mc: the backward prediction averaged into the forward one
    fn interp_mc(&mut self) {
        let [mx, my] = self.mv_b;
        let mut uvmx = (mx + ((mx & 3) == 3) as i32) >> 1;
        let mut uvmy = (my + ((my & 3) == 3) as i32) >> 1;
        // (FFmpeg rounds these away from 0 where ff_vc1_mc_1mv rounds towards it)
        if self.fastuvmc {
            uvmx += if uvmx < 0 { -(uvmx & 1) } else { uvmx & 1 };
            uvmy += if uvmy < 0 { -(uvmy & 1) } else { uvmy & 1 };
        }
        let (bx, by) = (self.mb_x as i32, self.mb_y as i32);
        let (mw, mh) = (self.mb_w as i32, self.mb_h as i32);
        let src_x = (bx * 16 + (mx >> 2)).clamp(-16, mw * 16);
        let src_y = (by * 16 + (my >> 2)).clamp(-16, mh * 16);
        let uvsrc_x = (bx * 8 + (uvmx >> 2)).clamp(-8, mw * 8);
        let uvsrc_y = (by * 8 + (uvmy >> 2)).clamp(-8, mh * 8);
        let doff = self.dest(0) as usize;
        self.luma_mc(1, doff, src_x, src_y, mx, my, 16, true);
        self.chroma_mc(1, uvsrc_x, uvsrc_y, uvmx, uvmy, true);
    }

    /// ff_vc1_mc_4mv_luma (forward, put)
    fn mc_4mv_luma(&mut self, n: usize) {
        let [mx, my] = self.mv[n];
        let off = self.ls * 4 * (n & 2) + (n & 1) * 8;
        let (bx, by) = (self.mb_x as i32, self.mb_y as i32);
        let (mw, mh) = (self.mb_w as i32, self.mb_h as i32);
        let src_x = (bx * 16 + (n as i32 & 1) * 8 + (mx >> 2)).clamp(-16, mw * 16);
        let src_y = (by * 16 + (n as i32 & 2) * 4 + (my >> 2)).clamp(-16, mh * 16);
        let doff = self.dest(0) as usize + off;
        self.luma_mc(0, doff, src_x, src_y, mx, my, 8, false);
    }

    /// ff_vc1_mc_4mv_chroma (forward)
    fn mc_4mv_chroma(&mut self) {
        let mut idx = 0;
        for i in 0..4 {
            if self.mb_type[self.dci(i)] == 0 {
                idx |= 1 << i;
            }
        }
        const INDEX2: [u8; 16] = [0, 0, 0, 0x01, 0, 0x02, 0x12, 0, 0, 0x03, 0x13, 0, 0x23, 0, 0, 0];
        let mv = self.mv;
        let (tx, ty) = match POPCOUNT4[idx] {
            4 => (median4(mv[0][0], mv[1][0], mv[2][0], mv[3][0]), median4(mv[0][1], mv[1][1], mv[2][1], mv[3][1])),
            3 => {
                let (a, b, c) = ((idx > 0xd) as usize, 1 + (idx > 0xb) as usize, 2 + (idx > 0x7) as usize);
                (mid_pred(mv[a][0], mv[b][0], mv[c][0]), mid_pred(mv[a][1], mv[b][1], mv[c][1]))
            }
            2 => {
                let (a, b) = ((INDEX2[idx] >> 4) as usize, (INDEX2[idx] & 0xf) as usize);
                ((mv[a][0] + mv[b][0]) / 2, (mv[a][1] + mv[b][1]) / 2)
            }
            _ => {
                let k = 2 * self.mbs + self.mb_x;
                self.luma_mv[k] = [0, 0];
                let k = self.mvi(0);
                self.motion1[k] = [0, 0];
                return;
            }
        };
        let (tx, ty) = (tx as i16 as i32, ty as i16 as i32);
        let k = self.mvi(0);
        self.motion1[k] = [tx as i16, ty as i16];
        let mut uvmx = (tx + ((tx & 3) == 3) as i32) >> 1;
        let mut uvmy = (ty + ((ty & 3) == 3) as i32) >> 1;
        let k = 2 * self.mbs + self.mb_x;
        self.luma_mv[k] = [uvmx as i16, uvmy as i16];
        uvmx = self.fast_uv(uvmx);
        uvmy = self.fast_uv(uvmy);
        let (bx, by) = (self.mb_x as i32, self.mb_y as i32);
        let (mw, mh) = (self.mb_w as i32, self.mb_h as i32);
        let uvsrc_x = (bx * 8 + (uvmx >> 2)).clamp(-8, mw * 8);
        let uvsrc_y = (by * 8 + (uvmy >> 2)).clamp(-8, mh * 8);
        self.chroma_mc(0, uvsrc_x, uvsrc_y, uvmx, uvmy, false);
    }
}

/// dst = (dst + src + 1) >> 1 over `w` x `h` (the avg_ ops of FFmpeg's DSP)
fn avg_into(dst: &mut [u8], doff: usize, ds: usize, src: &[u8], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let d = &mut dst[doff + y * ds + x];
            *d = ((*d as u32 + src[y * w + x] as u32 + 1) >> 1) as u8;
        }
    }
}

/// Hand-made WMV3 streams for tests (of this crate and of sake's decoder DMO): every MB
/// skipped, so every picture is the gray a P picture without a reference starts from.
#[doc(hidden)]
pub mod synth {
    struct W(Vec<u8>, usize);
    impl W {
        fn put(&mut self, v: u32, n: usize) {
            for k in (0..n).rev() {
                if self.1 % 8 == 0 {
                    self.0.push(0);
                }
                let b = (v >> k) & 1;
                *self.0.last_mut().unwrap() |= (b as u8) << (7 - self.1 % 8);
                self.1 += 1;
            }
        }
    }

    /// The sequence header: Main profile, FASTTX, RTM, up to one B picture between references
    /// (`b`) or none.
    pub fn extra(b: bool) -> [u8; 4] {
        let mut w = W(Vec::new(), 0);
        // profile, y411, sprite, frmrtq, bitrtq, loop filter, x8, multires, fasttx, fastuvmc,
        // extended mv, dquant, vstransform, transtab, overlap, resync, rangered
        for (v, n) in [(1, 2), (0, 1), (0, 1), (0, 3), (0, 5), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 2), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1)] {
            w.put(v, n);
        }
        // max B pictures, quantizer, finterp, rtm
        for (v, n) in [(b as u32, 3), (0, 2), (0, 1), (1, 1)] {
            w.put(v, n);
        }
        w.0.try_into().unwrap()
    }

    /// A picture of `mb_h` MB rows, all skipped: 'P' or 'B' (`b`: of a stream with B pictures).
    pub fn picture(kind: char, b: bool, mb_h: usize) -> Vec<u8> {
        let mut w = W(Vec::new(), 0);
        w.put(0, 2); // frame count
        let all = |w: &mut W| {
            // a bit plane of all 1: inverted, ROWSKIP, every row "0"
            w.put(1, 1);
            w.put(2, 3);
            for _ in 0..mb_h {
                w.put(0, 1);
            }
        };
        if kind == 'P' {
            w.put(1, 1);
            w.put(10, 5); // PQINDEX
            w.put(1, 1); // MVMODE 1MV
            all(&mut w); // skipped
        } else {
            assert!(b);
            w.put(0, 2); // not P, B
            w.put(0, 3); // BFRACTION 1/2
            w.put(10, 5);
            w.put(1, 1); // MVMODE 1MV
            all(&mut w); // direct
            all(&mut w); // skipped
        }
        w.put(0, 2); // MV table
        w.put(0, 2); // CBPCY table
        w.put(0, 1); // AC table
        w.put(0, 1); // DC table
        w.put(0, 16);
        w.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The DC-only transforms are the full ones on a block with only its DC.
        #[test]
        fn dc_transforms_are_the_full_ones(dc in -2048i16..2048, px in prop::collection::vec(any::<u8>(), 64)) {
            let mut b = [0i16; 64];
            b[0] = dc;
            let mut full = b;
            inv_trans_8x8(&mut full);
            let (mut a, mut c) = (px.clone(), px.clone());
            inv_trans_8x8_dc(&mut a, 0, 8, &b);
            add_pixels(&mut c, 0, 8, &full);
            prop_assert_eq!(&a, &c);
            type Dc = fn(&mut [u8], usize, usize, &[i16]);
            type Full = fn(&mut [u8], usize, usize, &mut [i16]);
            let pairs: [(Dc, Full); 3] = [
                (inv_trans_8x4_dc, inv_trans_8x4),
                (inv_trans_4x8_dc, inv_trans_4x8),
                (inv_trans_4x4_dc, inv_trans_4x4),
            ];
            for (dcf, full) in pairs {
                let (mut a, mut c) = (px.clone(), px.clone());
                let mut bb = b;
                dcf(&mut a, 0, 8, &b);
                full(&mut c, 0, 8, &mut bb);
                prop_assert_eq!(&a, &c);
            }
        }

        /// mspel with no fraction is a copy; the 1-D modes match their formulas at the middle.
        #[test]
        fn mspel_copy_and_half(src in prop::collection::vec(any::<u8>(), 19 * 19), rnd in 0i32..2) {
            let mut d = [0u8; 256];
            mspel_mc(&mut d, 0, 16, &src, 20, 19, 16, 0, 0, rnd);
            for y in 0..16 {
                prop_assert_eq!(&d[y * 16..y * 16 + 16], &src[20 + y * 19..20 + y * 19 + 16]);
            }
            mspel_mc(&mut d, 0, 16, &src, 20, 19, 16, 2, 0, rnd);
            for y in 0..16 {
                for x in 0..16 {
                    let g = |k: usize| src[20 + y * 19 + x + k - 1] as i32;
                    let want = ((-g(0) + 9 * g(1) + 9 * g(2) - g(3) + 8 - rnd) >> 4).clamp(0, 255);
                    prop_assert_eq!(d[y * 16 + x] as i32, want);
                }
            }
        }
    }

    #[test]
    fn simple_and_main_headers() {
        // すみっこ op.wmv: Main, no loop filter, FASTTX, DQUANT 1, VSTRANSFORM, OVERLAP, RTM
        let d = Decoder::new(640, 480, &[0x4f, 0xf1, 0x1a, 0x01]).unwrap();
        assert!(!d.loop_filter && d.overlap && d.vstransform && d.res_rtm_flag && !d.rangered);
        assert_eq!(d.dquant, 1);
        assert!(matches!(Decoder::new(640, 480, &[0xc0, 0, 0, 0]), Err(Error::Unsupported(_))));
        assert!(Decoder::new(640, 480, &[]).is_err());
    }

    /// With B pictures, I / P pictures come out one late (the B ones between first), and the
    /// last at `flush`; without, each as it goes in.
    #[test]
    fn pictures_come_out_in_display_order() {
        let mut d = Decoder::new(48, 32, &synth::extra(true)).unwrap();
        assert!(d.delayed());
        // P0 (on the gray stand-in reference: FFmpeg shows that), P1, B, P2
        let got: Vec<_> = ['P', 'P', 'B', 'P'].iter().map(|&k| (d.decode(&synth::picture(k, true, 2)).unwrap().is_some(), d.last_was_b())).collect();
        assert_eq!(got, [(true, false), (true, false), (true, true), (true, false)]);
        let f = d.flush().unwrap();
        assert_eq!((f.width, f.height), (48, 32));
        assert!(f.planes[0].iter().all(|&v| v == 0x80));
        assert!(d.flush().is_none());
        let mut d = Decoder::new(48, 32, &synth::extra(false)).unwrap();
        assert!(!d.delayed());
        for _ in 0..3 {
            assert!(d.decode(&synth::picture('P', false, 2)).unwrap().is_some());
        }
        assert!(d.flush().is_none());
    }

    /// A B picture before there are two references is dropped (as FFmpeg does).
    #[test]
    fn b_without_references_is_dropped() {
        let mut d = Decoder::new(32, 32, &synth::extra(true)).unwrap();
        assert!(d.decode(&synth::picture('B', true, 2)).unwrap().is_none());
    }

    mod robust {
        use super::*;

        proptest! {
            #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]
            /// Any sequence header, size and pictures: an error or pictures of the size, never a
            /// panic.
            #[test]
            fn any_bytes(w in 1usize..90, h in 1usize..90, extra in any::<[u8; 4]>(), pics in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..400), 1..6)) {
                let Ok(mut d) = Decoder::new(w, h, &extra) else { return Ok(()) };
                for p in &pics {
                    if let Ok(Some(p)) = d.decode(p) {
                        prop_assert_eq!((p.width, p.height), (w, h));
                        prop_assert_eq!(p.planes[0].len(), w.div_ceil(16) * 16 * h.div_ceil(16) * 16);
                    }
                }
                let _ = d.flush();
            }

            /// The hand-made pictures with bits flipped, in any order, of streams with and without
            /// B pictures: never a panic.
            #[test]
            fn damaged_pictures(b in any::<bool>(), kinds in prop::collection::vec(0u8..3, 1..8), flips in prop::collection::vec((any::<usize>(), 0u8..8), 0..6)) {
                let Ok(mut d) = Decoder::new(80, 48, &synth::extra(b)) else { panic!() };
                for k in kinds {
                    let kind = if b && k == 2 { 'B' } else { 'P' };
                    let mut p = synth::picture(kind, b, 3);
                    for &(at, bit) in &flips {
                        let n = p.len();
                        p[at % n] ^= 1 << bit;
                    }
                    let _ = d.decode(&p);
                }
            }
        }
    }
}
