//! Exactly pl_mpeg: testsrc.mpg's pictures as pl_mpeg 0.x itself gave them (its
//! plm_decode_video, built from pl_mpeg.h, written out as yuv420p and hashed with FNV-1a). pl
//! gives 29: its last shows the B picture's reference again in place of the final P picture
//! after it, and stops; ours shows both in order, as FFmpeg does (tests/ffmpeg.rs). (Audio is no longer pl_mpeg's: see audio.rs.)

use mpeg1::check::{decode, fnv};

#[test]
fn pictures_are_pl_mpegs() {
    let d = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/testsrc.mpg")).unwrap();
    assert_eq!(decode(&d, &[]).pictures.len(), 30);
    // (pl's 28 first pictures, one hash over them all)
    let s = d;
    let mut v = mpeg1::Video::new();
    for p in mpeg1::ps::packets(&s) {
        if mpeg1::ps::is_video(p.id) {
            v.push(&p.data);
        }
    }
    v.end();
    let mut h = 0xcbf29ce484222325;
    let mut n = 0;
    while let Some(f) = v.decode() {
        if n == 28 {
            break;
        }
        for y in 0..f.height {
            h = fnv(h, &f.y[y * f.y_stride..][..f.width]);
        }
        for p in [&f.cb, &f.cr] {
            for y in 0..f.height / 2 {
                h = fnv(h, &p[y * f.c_stride..][..f.width / 2]);
            }
        }
        n += 1;
    }
    assert_eq!(h, 0x5d50fef16508889c);
}
