//! sake::dshow_mpeg::pack_into (a movie's picture written straight into a renderer's sample,
//! review b06) for any sizes, depth, flip and bytes: no panic; with room, every byte of the
//! sample written (what was there before never shows), the same as packing a copy.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|d: &[u8]| {
    if d.len() < 6 {
        return;
    }
    let (sw, sh, w, h) = ((d[0] % 40) as u32, (d[1] % 40) as u32, (d[2] % 40) as u32, (d[3] % 40) as u32);
    let bits = [32u16, 24, 16, 15][(d[4] & 3) as usize];
    let flip = d[4] & 4 != 0;
    let src: Vec<u8> = d[6..].iter().copied().cycle().take((4 * sw * sh) as usize).collect();
    if src.len() < (4 * sw * sh) as usize {
        return;
    }
    let size = sake::dshow_mpeg::dib_stride(w, bits) as usize * h as usize;
    let mut a = vec![0u8; size];
    let mut b = vec![0xffu8; size];
    assert!(sake::dshow_mpeg::pack_into(&src, (sw, sh), flip, (w, h, bits), &mut a));
    assert!(sake::dshow_mpeg::pack_into(&src, (sw, sh), flip, (w, h, bits), &mut b));
    assert_eq!(a, b);
    let mut short = vec![d[5]; size.saturating_sub(1)];
    if size > 0 {
        assert!(!sake::dshow_mpeg::pack_into(&src, (sw, sh), flip, (w, h, bits), &mut short));
    }
});
