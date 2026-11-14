//! Resources of sake's own system DLLs, which programs load from them by id: shell32's AVI
//! clips (the animations of Windows' file copy dialog), made here.
//!
//! せきさば！'s installer (#222) plays shell32's clip 150 while it copies:
//! FindResourceA(shell32, 150, "AVI"), then LockResource / SizeofResource, without a check;
//! with no resources in sake's shell32 it read address 0. The clips here are sake's own
//! drawings (a page moving from one folder to another), uncompressed 8-bit DIB frames as
//! comctl32's animation control plays them; Windows' own clips are not copied.

use crate::resupdate::{Id, Tree};
use std::collections::BTreeMap;

/// The AVI ids shell32 has: 150-152 (searching) and 160-164 (move, copy, delete to the recycle
/// bin, empty it, delete)
pub const SHELL32_AVIS: &[u16] = &[150, 151, 152, 160, 161, 162, 163, 164];
/// the clips' size and length: 10 frames a second
pub const W: usize = 80;
pub const H: usize = 50;
pub const FRAMES: usize = 8;

/// A DLL's resources (None: it has none)
pub fn tree(dll_key: &str) -> Option<Tree> {
    if dll_key != "shell32" {
        return None;
    }
    let clip = copy_clip();
    let names: BTreeMap<Id, _> = SHELL32_AVIS.iter().map(|&id| (Id::Int(id), BTreeMap::from([(0x0409u16, (0u32, clip.clone()))]))).collect();
    Some(BTreeMap::from([(Id::Name("AVI".encode_utf16().collect()), names)]))
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 9);
    v.extend_from_slice(id);
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}
fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = kind.to_vec();
    b.extend_from_slice(body);
    chunk(b"LIST", &b)
}
fn le(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Frame `f`'s pixels (palette indices, bottom-up rows of W bytes: W is a multiple of 4): two
/// folders and a page between them, further along each frame
pub fn frame(f: usize) -> Vec<u8> {
    let mut px = vec![0u8; W * H];
    let mut fill = |x0: usize, y0: usize, w: usize, h: usize, c: u8| {
        for y in y0..(y0 + h).min(H) {
            for x in x0..(x0 + w).min(W) {
                // (rows bottom-up)
                px[(H - 1 - y) * W + x] = c;
            }
        }
    };
    // folders: yellow, with a darker tab
    for &x in &[4usize, W - 22] {
        fill(x, 22, 18, 20, 1);
        fill(x, 19, 8, 3, 2);
    }
    // the page: white with a grey border, on an arc from the left folder to the right one
    let t = f as f32 / (FRAMES - 1) as f32;
    let x = (8.0 + t * (W as f32 - 34.0)) as usize;
    let y = (20.0 - 16.0 * (1.0 - (2.0 * t - 1.0).powi(2))) as usize;
    fill(x, y, 10, 13, 3);
    fill(x + 1, y + 1, 8, 11, 4);
    px
}

/// The clip: RIFF AVI, one video stream of FRAMES key frames, BI_RGB 8-bit with a 16-colour
/// palette (index 0 the dialog's face colour, which the control draws as transparent), and
/// an idx1 index
pub fn copy_clip() -> Vec<u8> {
    let size = (W * H) as u32;
    // palette: B, G, R, 0
    let pal: [[u8; 4]; 16] = {
        let mut p = [[0u8; 4]; 16];
        p[0] = [0xf0, 0xf0, 0xf0, 0];
        p[1] = [0x4c, 0xc8, 0xf8, 0];
        p[2] = [0x20, 0x98, 0xd8, 0];
        p[3] = [0x80, 0x80, 0x80, 0];
        p[4] = [0xff, 0xff, 0xff, 0];
        p
    };
    let avih = le(&[100_000, size * 10, 0, 0x10, FRAMES as u32, 0, 1, size, W as u32, H as u32, 0, 0, 0, 0]);
    let mut strh = b"vidsDIB ".to_vec();
    strh.extend(le(&[0, 0, 0, 1, 10, 0, FRAMES as u32, size, u32::MAX, 0]));
    strh.extend([0u16, 0, W as u16, H as u16].iter().flat_map(|v| v.to_le_bytes()));
    let mut strf = le(&[40, W as u32, H as u32]);
    strf.extend(1u16.to_le_bytes());
    strf.extend(8u16.to_le_bytes());
    strf.extend(le(&[0, size, 0, 0, 16, 16]));
    strf.extend(pal.iter().flatten());
    let strl = list(b"strl", &[chunk(b"strh", &strh), chunk(b"strf", &strf)].concat());
    let hdrl = list(b"hdrl", &[chunk(b"avih", &avih), strl].concat());
    let mut movi = Vec::new();
    let mut idx = Vec::new();
    for f in 0..FRAMES {
        // (idx1 offsets: from the "movi" fourcc)
        let at = 4 + movi.len() as u32;
        idx.extend_from_slice(b"00db");
        idx.extend(le(&[0x10, at, size]));
        movi.extend(chunk(b"00db", &frame(f)));
    }
    let body = [b"AVI ".to_vec(), hdrl, list(b"movi", &movi), chunk(b"idx1", &idx)].concat();
    chunk(b"RIFF", &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The RIFF chunks in `b` (id, body): each one's size inside its parent
    fn chunks(b: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < b.len() {
            let id: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
            let n = u32::from_le_bytes(b.get(at + 4..at + 8)?.try_into().ok()?) as usize;
            out.push((id, b.get(at + 8..at + 8 + n)?));
            at += 8 + n + n % 2;
        }
        Some(out)
    }

    /// The clip reads back as an AVI (an independent walk of its RIFF tree): the header's frame
    /// count and size, the stream's BITMAPINFOHEADER, every frame, and an index that points at
    /// each frame's chunk
    #[test]
    fn the_clip_is_an_avi() {
        let a = copy_clip();
        let top = chunks(&a).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(&top[0].0, b"RIFF");
        let riff = top[0].1;
        assert_eq!(&riff[..4], b"AVI ");
        let parts = chunks(&riff[4..]).unwrap();
        let ids: Vec<&[u8; 4]> = parts.iter().map(|p| &p.0).collect();
        assert_eq!(ids, [b"LIST", b"LIST", b"idx1"]);
        let hdrl = parts[0].1;
        assert_eq!(&hdrl[..4], b"hdrl");
        let h = chunks(&hdrl[4..]).unwrap();
        assert_eq!(&h[0].0, b"avih");
        let avih = h[0].1;
        let d = |b: &[u8], i: usize| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
        assert_eq!(avih.len(), 56);
        assert_eq!((d(avih, 4), d(avih, 6), d(avih, 8), d(avih, 9)), (FRAMES as u32, 1, W as u32, H as u32));
        let strl = chunks(&h[1].1[4..]).unwrap();
        assert_eq!(&strl[0].1[..4], b"vids");
        let bih = strl[1].1;
        assert_eq!((d(bih, 0), d(bih, 1), d(bih, 2)), (40, W as u32, H as u32));
        assert_eq!(u16::from_le_bytes([bih[14], bih[15]]), 8);
        assert_eq!(bih.len(), 40 + 16 * 4);
        let movi = parts[1].1;
        assert_eq!(&movi[..4], b"movi");
        let frames = chunks(&movi[4..]).unwrap();
        assert_eq!(frames.len(), FRAMES);
        let idx = parts[2].1;
        assert_eq!(idx.len(), 16 * FRAMES);
        for (f, (id, px)) in frames.iter().enumerate() {
            assert_eq!(id, b"00db");
            assert_eq!(*px, &frame(f)[..]);
            let e = &idx[16 * f..16 * f + 16];
            assert_eq!(&e[..4], b"00db");
            let (at, n) = (d(e, 2) as usize, d(e, 3) as usize);
            assert_eq!(&movi[at..at + 4], b"00db");
            assert_eq!(&movi[at + 8..at + 8 + n], *px);
        }
    }

    /// shell32's resource tree has the clip under "AVI" for each id, and nothing for others
    #[test]
    fn shell32_has_the_clips() {
        let t = tree("shell32").unwrap();
        let names = &t[&Id::Name("AVI".encode_utf16().collect())];
        assert_eq!(names.len(), SHELL32_AVIS.len());
        assert!(names.contains_key(&Id::Int(150)));
        assert!(tree("user32").is_none());
        // (the tree builds into a resource directory: what hle_module maps)
        let b = crate::resupdate::build(&t, 0x1000);
        assert!(b.windows(4).any(|w| w == b"RIFF"));
    }

    proptest! {
        /// every frame is W x H indices of the palette's colours, with the page in it
        #[test]
        fn frames_are_in_the_palette(f in 0..FRAMES) {
            let px = frame(f);
            prop_assert_eq!(px.len(), W * H);
            prop_assert!(px.iter().all(|&c| c < 16));
            prop_assert!(px.iter().filter(|&&c| c == 4).count() == 8 * 11);
        }
    }
}
