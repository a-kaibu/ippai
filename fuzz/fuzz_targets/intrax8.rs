//! IntraX8 pictures (crates/wmcodec::intrax8, FFmpeg's intrax8.c ported) of any size, quantizer
//! and bits: no panic, and it stops inside the picture.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wmcodec::bits::Bits;
use wmcodec::intrax8::{IntraX8, Plane};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let (mb_w, mb_h) = (data[0] as usize % 16 + 1, data[1] as usize % 16 + 1);
    let (dquant, offset, lf) = (data[2] as i32 % 62 + 2, (data[3] & 31) as i32, data[3] & 0x80 != 0);
    let (ls, uvls) = (mb_w * 16, mb_w * 8);
    let (mut y, mut u, mut v) = (vec![0u8; ls * mb_h * 16], vec![0u8; uvls * mb_h * 8], vec![0u8; uvls * mb_h * 8]);
    let mut x8 = IntraX8::new(mb_w, mb_h);
    // (twice: the second picture starts from the first one's predictions)
    for _ in 0..2 {
        let mut r = Bits::new(&data[4..]);
        let planes = [Plane { px: &mut y, stride: ls }, Plane { px: &mut u, stride: uvls }, Plane { px: &mut v, stride: uvls }];
        assert!(x8.decode(&mut r, planes, dquant, offset, lf) <= mb_h * 2);
    }
});
