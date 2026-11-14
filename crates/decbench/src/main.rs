//! decbench DIR [REPS]: times sake's own decoders on the inputs tests/decbench/run.sh writes to
//! DIR (pic.png, pic.jpg, movie.mpg; the Vorbis stream is the vorbis crate's test data), one
//! line per case: `BENCH <name> <ms per rep>`. Built for wasm32 (with and without SIMD128) and
//! run under node, as the page's sake.wasm runs in V8.
use std::time::Instant;

fn time(name: &str, reps: usize, mut f: impl FnMut() -> u64) {
    // (one untimed rep: tier-up, caches)
    let mut sink = f();
    let t = Instant::now();
    for _ in 0..reps {
        sink = sink.wrapping_add(f());
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / reps as f64;
    println!("BENCH {name} {ms:.3} (check {sink:x})");
}

fn sum(b: &[u8]) -> u64 {
    b.iter().step_by(61).fold(0u64, |a, &x| a.wrapping_mul(31).wrapping_add(x as u64))
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    // (decbench jpeg IN OUT: jpeg-decoder's pixels of IN, raw, to set this build's against
    // Windows' answers: tests/decbench/run.sh jpeg)
    if a[1] == "jpeg" {
        let mut d = jpeg_decoder::Decoder::new(std::fs::File::open(&a[2]).unwrap());
        std::fs::write(&a[3], d.decode().unwrap()).unwrap();
        return;
    }
    let dir = std::path::PathBuf::from(&a[1]);
    let reps: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let only = a.get(3).cloned().unwrap_or_default();
    let want = |n: &str| only.is_empty() || only.split(',').any(|o| n.starts_with(o));

    if want("png") {
        let data = std::fs::read(dir.join("pic.png")).unwrap();
        time("png_1280x720", reps, || {
            let d = png::Decoder::new(&data[..]);
            let mut r = d.read_info().unwrap();
            let mut buf = vec![0; r.output_buffer_size()];
            r.next_frame(&mut buf).unwrap();
            sum(&buf)
        });
    }
    if want("jpeg") {
        let data = std::fs::read(dir.join("pic.jpg")).unwrap();
        time("jpeg_1280x720", reps, || {
            let mut d = jpeg_decoder::Decoder::new(&data[..]);
            sum(&d.decode().unwrap())
        });
    }
    if want("idec") {
        // (what the guest still does for a JPEG the page decoded: its planes -> BGR)
        let (w, h) = (1280u32, 720u32);
        let layout = kura::idec::Layout::I420;
        let bytes = (0..layout.size(w, h)).map(|i| ((i as u32).wrapping_mul(2654435761) >> 13) as u8).collect();
        let p = kura::idec::Picture { w, h, layout, bytes };
        time("idec_i420_bgr24_1280x720", reps * 5, || sum(&p.bgr24().unwrap()));
    }
    if want("mpeg1") {
        let data = std::fs::read(dir.join("movie.mpg")).unwrap();
        let mut frames = 0;
        time("mpeg1_640x480_frame", reps, || {
            let mut v = mpeg1::Video::new();
            for p in mpeg1::ps::packets(&data) {
                if mpeg1::ps::is_video(p.id) {
                    v.push(&p.data);
                }
            }
            v.end();
            let mut s = 0;
            frames = 0;
            while let Some(f) = v.decode() {
                s ^= sum(&f.y);
                frames += 1;
            }
            s
        });
        println!("(mpeg1: {frames} frames a rep)");
        // the picture -> DirectShow RGB32 conversion
        let mut v = mpeg1::Video::new();
        for p in mpeg1::ps::packets(&data) {
            if mpeg1::ps::is_video(p.id) {
                v.push(&p.data);
            }
        }
        v.end();
        let f = v.decode().unwrap();
        let mut out = vec![0u8; f.width * f.height * 4];
        time("mpeg1_to_bgrx_640x480", reps * 10, || {
            mpeg1::video::to_bgrx(&f, &mut out, f.width * 4, true);
            sum(&out)
        });
    }
    if want("yuv") {
        let (w, h) = (1280u32, 720u32);
        let buf: Vec<u8> = (0..w * h * 3 / 2).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let layout = [[0, 0, w, w * h, w / 2, w * h * 5 / 4, w / 2], [0; 7]];
        let mut d = vec![0u8; (4 * w * h) as usize];
        for (name, out) in [("yuv_rgb601_1280x720", kura::video::Out::Rgb601), ("yuv_rgb_1280x720", kura::video::Out::Rgb), ("yuv_ycc_1280x720", kura::video::Out::Ycc)] {
            time(name, reps * 5, || {
                unsafe { kura::video::convert(buf.as_ptr(), &layout, 1, w, h, d.as_mut_ptr(), 4 * w as usize, w, h, out) };
                sum(&d)
            });
        }
    }
    if want("vorbis") {
        let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../vorbis/testdata/stereo-q5.ogg")).unwrap();
        let (packets, _) = vorbis::ogg::packets(&data);
        let mut n = 0;
        time("vorbis_stereo_q5", reps * 5, || {
            let mut d = vorbis::Decoder::new(&packets[0], &packets[2]).unwrap();
            let mut out = Vec::new();
            for p in &packets[3..] {
                d.packet(p, &mut out);
            }
            n = out.len();
            out.iter().step_by(97).fold(0u64, |a, x| a.wrapping_add(x.to_bits() as u64))
        });
        println!("(vorbis: {n} samples a rep)");
    }
    if want("dxt") {
        use kura::d3d9::format as f;
        let (w, h) = (1024u32, 1024u32);
        for (name, fmt, bs) in [("dxt1_1024", f::DXT1, 8u32), ("dxt5_1024", f::DXT5, 16)] {
            let src: Vec<u8> = (0..(w / 4) * (h / 4) * bs).map(|i| (i.wrapping_mul(2246822519) >> 11) as u8).collect();
            let mut out = Vec::new();
            time(name, reps, || {
                out.clear();
                unsafe { f::convert(fmt, src.as_ptr(), (w / 4 * bs) as usize, w, h, &mut out) };
                sum(&out)
            });
        }
    }
}
