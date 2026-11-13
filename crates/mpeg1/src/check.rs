//! The properties fuzz/fuzz_targets/mpeg1.rs and tests/props.rs hold any bytes to: whatever
//! the input, nothing panics; a stream cut into any slices demuxes and decodes to the same
//! packets, pictures and samples as the whole; every picture is the size its header gives, its
//! planes hold it; every audio frame is 1152 samples in [-1, 1] (a little over, for clipping
//! streams: finite, |x| < 256; the output clips).

use crate::{ps, Audio, Demux, Video};

#[derive(Debug, PartialEq)]
pub struct Decoded {
    pub packets: Vec<ps::Packet>,
    /// a hash of each picture (FNV-1a of its planes)
    pub pictures: Vec<(usize, usize, u64)>,
    pub samples: Vec<u32>,
}

pub fn fnv(h: u64, d: &[u8]) -> u64 {
    d.iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Demux and decode `data` fed in the slices `cuts` gives (each a length, then the rest).
pub fn decode(data: &[u8], cuts: &[usize]) -> Decoded {
    let mut m = Demux::new();
    let (mut v, mut a) = (Video::new(), Audio::new());
    let mut out = Decoded { packets: vec![], pictures: vec![], samples: vec![] };
    let take = |m: &mut Demux, v: &mut Video, a: &mut Audio, out: &mut Decoded, last: bool| {
        while let Some(p) = m.next() {
            if ps::is_video(p.id) {
                v.push(&p.data);
            } else if ps::is_audio(p.id) {
                a.push(&p.data);
            }
            out.packets.push(p);
        }
        if last {
            v.end();
            a.end();
        }
        loop {
            let Some(f) = v.decode() else { break };
            assert!(f.y.len() >= f.y_stride * f.height && f.y_stride >= f.width);
            assert!(f.cb.len() >= f.c_stride * f.height / 2 && f.c_stride >= f.width / 2);
            let mut x = 0xcbf29ce484222325;
            for y in 0..f.height {
                x = fnv(x, &f.y[y * f.y_stride..][..f.width]);
            }
            for p in [&f.cb, &f.cr] {
                for y in 0..f.height / 2 {
                    x = fnv(x, &p[y * f.c_stride..][..f.width / 2]);
                }
            }
            let size = (f.width, f.height);
            let h = v.header().expect("a picture without a sequence header");
            assert_eq!(size, (h.width, h.height));
            out.pictures.push((size.0, size.1, x));
        }
        while a.decode() {
            assert_eq!(a.out.len(), 1152 * 2);
            for &s in &a.out {
                assert!(s.abs() < 256.0, "{s}");
                out.samples.push(s.to_bits());
            }
        }
    };
    let mut at = 0;
    for &c in cuts {
        let end = (at + c).min(data.len());
        m.push(&data[at..end]);
        at = end;
        take(&mut m, &mut v, &mut a, &mut out, false);
    }
    m.push(&data[at..]);
    m.end();
    take(&mut m, &mut v, &mut a, &mut out, true);
    out
}

/// The whole check: `data` cut by `cuts` decodes as `data` whole.
pub fn check(data: &[u8], cuts: &[usize]) {
    let whole = decode(data, &[]);
    assert_eq!(decode(data, cuts), whole, "cut at {cuts:?}");
    let _ = crate::probe(data, data, true);
}
