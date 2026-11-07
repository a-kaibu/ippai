//! sake's one pixel-format module (crates/sake/src/picture.rs: WIC, GDI+, OLE pictures, D3DX
//! surfaces go through it): any format, size, stride and data to straight BGRA must not panic
//! (short rows and buffers are a game's mistake, not a crash), and BGRA written back in a
//! format that keeps every bit reads back the same.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::picture::{from_bgra, to_bgra, Fmt};

const FMTS: [Fmt; 23] = [
    Fmt::Idx1, Fmt::Idx2, Fmt::Idx4, Fmt::Idx8, Fmt::BlackWhite, Fmt::Gray2, Fmt::Gray4, Fmt::Gray8, Fmt::Gray16, Fmt::Bgr555, Fmt::Bgr565,
    Fmt::Bgr24, Fmt::Rgb24, Fmt::Bgr32, Fmt::Bgra, Fmt::Pbgra, Fmt::Rgb32, Fmt::Rgba, Fmt::Prgba, Fmt::Rgb48, Fmt::Rgba64, Fmt::Cmyk, Fmt::Alpha8,
];

fuzz_target!(|data: &[u8]| {
    if data.len() < 5 {
        return;
    }
    let f = FMTS[data[0] as usize % FMTS.len()];
    let (w, h) = (data[1] as u32 % 40 + 1, data[2] as u32 % 40 + 1);
    let pad = data[3] as u32 % 8;
    let stride = f.row_bytes(w) + pad;
    let npal = data[4] as usize % 257;
    let rest = &data[5..];
    let palette: Vec<u32> = rest.chunks_exact(4).take(npal).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let pixels = &rest[(palette.len() * 4).min(rest.len())..];
    let bgra = to_bgra(f, w, h, stride, pixels, &palette);
    assert_eq!(bgra.len(), (w * h * 4) as usize);
    // back to the format, then to BGRA again: the same pixels (formats that keep 8 bits per
    // channel and alpha; for the others, at least no panic)
    let mut out = vec![0u8; (stride * h) as usize];
    from_bgra(&bgra, w, f, (0, 0, w, h), stride, &mut out, &palette);
    let again = to_bgra(f, w, h, stride, &out, &palette);
    if matches!(f, Fmt::Bgra | Fmt::Rgba | Fmt::Rgba64) {
        assert_eq!(bgra, again, "{f:?} {w}x{h}");
    }
    if matches!(f, Fmt::Bgr24 | Fmt::Rgb24 | Fmt::Bgr32 | Fmt::Rgb32 | Fmt::Rgb48) {
        let rgb = |v: &[u8]| v.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect::<Vec<_>>();
        assert_eq!(rgb(&bgra), rgb(&again), "{f:?} {w}x{h}");
    }
});
