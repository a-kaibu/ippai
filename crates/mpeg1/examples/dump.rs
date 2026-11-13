//! dump IN.mpg OUT.yuv OUT.f32: the pictures (yuv420p, picture area) and audio (f32 stereo
//! interleaved) crates/mpeg1 decodes, to compare with other decoders.
use mpeg1::{ps, Audio, Video};
use std::io::Write;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let s = std::fs::read(&a[1]).unwrap();
    let mut v = Video::new();
    let mut au = Audio::new();
    for p in ps::packets(&s) {
        if ps::is_video(p.id) {
            v.push(&p.data);
        } else if ps::is_audio(p.id) {
            au.push(&p.data);
        }
    }
    v.end();
    au.end();
    let mut yuv = std::fs::File::create(&a[2]).unwrap();
    while let Some(f) = v.decode() {
        for y in 0..f.height {
            yuv.write_all(&f.y[y * f.y_stride..y * f.y_stride + f.width]).unwrap();
        }
        for p in [&f.cb, &f.cr] {
            for y in 0..f.height / 2 {
                yuv.write_all(&p[y * f.c_stride..y * f.c_stride + f.width / 2]).unwrap();
            }
        }
    }
    let mut pcm = std::fs::File::create(&a[3]).unwrap();
    while au.decode() {
        for x in &au.out {
            pcm.write_all(&x.to_le_bytes()).unwrap();
        }
    }
}
