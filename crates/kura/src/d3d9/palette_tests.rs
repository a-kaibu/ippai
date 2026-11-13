//! Palettes (SetPaletteEntries, SetCurrentTexturePalette) and P8 textures: the texels a draw
//! of a P8 texture puts on the page are the current palette's entries for its indices (red,
//! green, blue, and the flags as the alpha), whichever palette was current when the texture
//! was filled: a change of entries or of the current palette shows at the next draw.
//! (アオイシロ's text window: P8 font and frame textures, which drew grey boxes when the
//! indices were taken for luminance.) Random sequences of calls are checked against a model.

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, Rng, CALLS, SERIAL};
use super::*;
use crate::render::model;

const INVALIDCALL: u32 = 0x8876086c;

fn device() -> u32 {
    attach_page();
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 1, 75, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    rd(o)
}

fn quad(dev: u32) {
    let v = unsafe { alloc(80) as u32 };
    let mut b = Vec::new();
    for (x, y) in [(0f32, 0f32), (64.0, 0.0), (0.0, 64.0), (64.0, 64.0)] {
        for f in [x - 0.5, y - 0.5, 0.5, 1.0] {
            b.extend(f.to_le_bytes());
        }
        b.extend(0xffff_ffffu32.to_le_bytes());
    }
    put_bytes(v, &b);
    call(IF_DEVICE, 89, &[dev, 0x44]);
    call(IF_DEVICE, 41, &[dev]);
    assert_eq!(call(IF_DEVICE, 83, &[dev, 5, 2, v, 20]), 0);
    call(IF_DEVICE, 42, &[dev]);
    gpu::flush();
    gpu::pump();
}

fn init() {
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
}

/// A w x h P8 texture (managed) and its page texture's index in the model.
fn p8_texture(dev: u32, w: u32, h: u32) -> (u32, usize) {
    let from = model::with(|m| m.objs.len());
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, w, h, 1, 0, format::P8, 1, o, 0]), 0);
    let tex = rd(o);
    gpu::flush();
    gpu::pump();
    let at = model::with(|m| m.objs[from..].iter().position(|o| matches!(o, model::Obj::Texture(t) if t.w == w && t.h == h)))
        .expect("no page texture")
        + from;
    (tex, at)
}

fn fill(tex: u32, w: u32, h: u32, idx: &[u8]) {
    let lr = out();
    assert_eq!(call(IF_TEXTURE, 19, &[tex, 0, lr, 0, 0]), 0);
    let (pitch, bits) = (rd(lr), rd(lr + 4));
    for y in 0..h {
        put_bytes(bits + y * pitch, &idx[(y * w) as usize..][..w as usize]);
    }
    assert_eq!(call(IF_TEXTURE, 20, &[tex, 0]), 0);
}

fn set_entries(dev: u32, n: u32, pal: &[u8; 1024]) -> u32 {
    let p = unsafe { alloc(1024) as u32 };
    put_bytes(p, pal);
    call(IF_DEVICE, 71, &[dev, n, p])
}

fn page_texels(at: usize) -> Vec<u64> {
    model::with(|m| match &m.objs[at] {
        model::Obj::Texture(t) => t.texels[0].clone(),
        _ => panic!("not a texture"),
    })
}

fn want(pal: &[u8; 1024], idx: &[u8]) -> Vec<u64> {
    idx.iter().map(|&i| u32::from_le_bytes(format::p8_texel(pal, i)) as u64).collect()
}

fn draw_with(dev: u32, tex: u32) {
    call(IF_DEVICE, 65, &[dev, 0, tex]);
    quad(dev);
    call(IF_DEVICE, 65, &[dev, 0, 0]);
}

fn palette(r: &mut Rng) -> Box<[u8; 1024]> {
    let mut p = Box::new([0u8; 1024]);
    for b in p.iter_mut() {
        *b = r.next() as u8;
    }
    p
}

#[test]
fn palette_calls_answer_as_set() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let dev = device();
    let mut r = Rng(0x5eed_1234_5678_9abd);
    let p = palette(&mut r);
    let o = unsafe { alloc(1024) as u32 };
    // (a palette never set: nothing to read)
    assert_eq!(call(IF_DEVICE, 72, &[dev, 3, o]), INVALIDCALL);
    assert_eq!(set_entries(dev, 3, &p), 0);
    assert_eq!(call(IF_DEVICE, 72, &[dev, 3, o]), 0);
    assert_eq!(unsafe { std::slice::from_raw_parts(o as *const u8, 1024) }, &p[..]);
    assert_eq!(call(IF_DEVICE, 71, &[dev, 3, 0]), INVALIDCALL, "no entries");
    assert_eq!(call(IF_DEVICE, 72, &[dev, 3, 0]), INVALIDCALL, "nowhere to write");
    let c = out();
    assert_eq!(call(IF_DEVICE, 74, &[dev, c]), 0);
    assert_eq!(rd(c), 0, "palette 0 is current at first");
    assert_eq!(call(IF_DEVICE, 73, &[dev, 3]), 0);
    assert_eq!(call(IF_DEVICE, 74, &[dev, c]), 0);
    assert_eq!(rd(c), 3);
    // (a palette number past 0xffff is refused)
    assert_eq!(call(IF_DEVICE, 73, &[dev, 0x10000]), INVALIDCALL);
    assert_eq!(set_entries(dev, 0x10000, &p), INVALIDCALL);
}

/// The page texels of a P8 texture: the current palette's entries, again after every change.
#[test]
fn p8_draws_read_the_current_palette() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let dev = device();
    model::with(|m| std::mem::take(&mut m.errors));
    let mut r = Rng(0x0a01_5170_0000_0001);
    let (w, h) = (4, 4);
    let idx: Vec<u8> = (0..w * h).map(|i| (i * 17 + 3) as u8).collect();
    let (p0, p1, p2) = (palette(&mut r), palette(&mut r), palette(&mut r));
    assert_eq!(set_entries(dev, 0, &p0), 0);
    let (tex, at) = p8_texture(dev, w, h);
    fill(tex, w, h, &idx);
    draw_with(dev, tex);
    assert_eq!(page_texels(at), want(&p0, &idx), "palette 0");
    // new entries for the current palette
    assert_eq!(set_entries(dev, 0, &p1), 0);
    draw_with(dev, tex);
    assert_eq!(page_texels(at), want(&p1, &idx), "palette 0's new entries");
    // another palette made current
    assert_eq!(set_entries(dev, 5, &p2), 0);
    draw_with(dev, tex);
    assert_eq!(page_texels(at), want(&p1, &idx), "palette 5 set but not current");
    assert_eq!(call(IF_DEVICE, 73, &[dev, 5]), 0);
    draw_with(dev, tex);
    assert_eq!(page_texels(at), want(&p2, &idx), "palette 5 current");
    // filled again while palette 5 is current
    let idx2: Vec<u8> = idx.iter().map(|i| i ^ 0x5a).collect();
    fill(tex, w, h, &idx2);
    draw_with(dev, tex);
    assert_eq!(page_texels(at), want(&p2, &idx2), "new indices");
    call(IF_TEXTURE, 2, &[tex]);
    let e = model::with(|m| std::mem::take(&mut m.errors));
    assert!(e.is_empty(), "{:?}", &e[..e.len().min(8)]);
    gpu::set_pump(None);
}

/// Random calls (entries of palettes 0-3, current palette, refills, draws) on two P8
/// textures: after each draw the page has what the model says.
#[test]
fn p8_random_sequences_match_the_model() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let dev = device();
    model::with(|m| std::mem::take(&mut m.errors));
    let seeds = std::env::var("P8_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(24u64);
    for seed in 1..=seeds {
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut pals: std::collections::HashMap<u32, Box<[u8; 1024]>> = Default::default();
        // (each run starts from palette 0, which may be set already: give it entries)
        let p = palette(&mut r);
        assert_eq!(set_entries(dev, 0, &p), 0);
        pals.insert(0, p);
        assert_eq!(call(IF_DEVICE, 73, &[dev, 0]), 0);
        let mut cur = 0u32;
        let sizes = [(1 + r.below(7) as u32, 1 + r.below(7) as u32), (1 + r.below(7) as u32, 1 + r.below(7) as u32)];
        let texs: Vec<(u32, usize, u32, u32)> = sizes
            .iter()
            .map(|&(w, h)| {
                // (sizes no other page texture of this test has)
                let (w, h) = (w + 64, h + 64);
                let (t, at) = p8_texture(dev, w, h);
                (t, at, w, h)
            })
            .collect();
        let mut idx: Vec<Vec<u8>> = texs.iter().map(|&(_, _, w, h)| (0..w * h).map(|_| r.next() as u8).collect()).collect();
        for (k, &(t, _, w, h)) in texs.iter().enumerate() {
            fill(t, w, h, &idx[k]);
        }
        for _ in 0..12 {
            match r.below(4) {
                0 => {
                    let n = r.below(4) as u32;
                    let p = palette(&mut r);
                    assert_eq!(set_entries(dev, n, &p), 0);
                    pals.insert(n, p);
                }
                1 => {
                    let n = *pals.keys().nth(r.below(pals.len())).unwrap();
                    assert_eq!(call(IF_DEVICE, 73, &[dev, n]), 0);
                    cur = n;
                }
                2 => {
                    let k = r.below(texs.len());
                    let (t, _, w, h) = texs[k];
                    idx[k] = (0..w * h).map(|_| r.next() as u8).collect();
                    fill(t, w, h, &idx[k]);
                }
                _ => {
                    let k = r.below(texs.len());
                    let (t, at, _, _) = texs[k];
                    draw_with(dev, t);
                    assert_eq!(page_texels(at), want(&pals[&cur], &idx[k]), "seed {seed}: texture {k}, palette {cur}");
                }
            }
        }
        for &(t, ..) in &texs {
            call(IF_TEXTURE, 2, &[t]);
        }
    }
    let e = model::with(|m| std::mem::take(&mut m.errors));
    assert!(e.is_empty(), "{:?}", &e[..e.len().min(8)]);
    gpu::set_pump(None);
}
