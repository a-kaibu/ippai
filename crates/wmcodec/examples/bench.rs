//! Decoding speed of a file's video (`cargo run --release --example bench FILE.wmv`; for wasm,
//! see wasm-test.sh): ms per picture, the decoder alone, and a hash of the pictures (the same
//! from every build: scalar or SIMD, host or wasm).
use wmcodec::msmpeg4::{Decoder, Version};

fn main() {
    let file = std::env::args().nth(1).expect("FILE.wmv");
    let mut r = asf::Reader::new(std::fs::read(&file).unwrap()).unwrap();
    let s = r.header.streams.iter().find(|s| matches!(s.kind, asf::Kind::Video { .. })).unwrap().clone();
    let f = &s.format;
    let rd32 = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap());
    let (w, h) = (rd32(4) as usize, rd32(8) as usize);
    let mut pics = Vec::new();
    while let Some(x) = r.next_sample() {
        if x.stream == s.number {
            pics.push(x.data);
        }
    }
    let mut spent = std::time::Duration::ZERO;
    let (mut n, mut hash) = (0, 0xcbf29ce484222325u64);
    let mut see = |p: &wmcodec::msmpeg4::Picture| {
        n += 1;
        for b in p.planes.iter().flatten() {
            hash = (hash ^ *b as u64).wrapping_mul(0x100000001b3);
        }
    };
    if &f[16..20] == b"WMV3" {
        let mut d = wmcodec::vc1::Decoder::new(w, h, &f[40..]).unwrap();
        for p in &pics {
            let t = std::time::Instant::now();
            let got = d.decode(p);
            spent += t.elapsed();
            if let Ok(Some(p)) = got {
                see(&p);
            }
        }
        if let Some(p) = d.flush() {
            see(&p);
        }
    } else {
        let v = match &f[16..20] {
            b"WMV1" => Version::Wmv1,
            b"WMV2" => Version::Wmv2,
            _ => Version::V3,
        };
        let mut d = Decoder::new(v, w, h, &f[40..]).unwrap();
        for p in &pics {
            let t = std::time::Instant::now();
            let got = d.decode(p);
            spent += t.elapsed();
            if let Ok(Some(p)) = got {
                see(&p);
            }
        }
    }
    let ms = spent.as_secs_f64() * 1000.0;
    println!("{file}: {w}x{h}, {n} pictures, {:.2} ms per picture, hash {hash:016x}", ms / n.max(1) as f64);
}
