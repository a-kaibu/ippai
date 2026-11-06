//! Damaged input never panics or reads out of bounds: random and mutated audio packets, and
//! mutated setup headers (a game hands the hook whatever is in its files).

use proptest::prelude::*;
use vorbis::{ogg, Decoder};

fn stream(name: &str) -> Vec<Vec<u8>> {
    let data = std::fs::read(format!("{}/testdata/{name}.ogg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    ogg::packets(&data).0
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn random_packets(which in 0usize..5, packets in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..600), 1..20)) {
        let names = ["stereo-q5", "mono-q0", "surround-q3", "stereo-q10-silence", "stereo-48k"];
        let p = stream(names[which]);
        let mut d = Decoder::new(&p[0], &p[2]).unwrap();
        let mut out = Vec::new();
        for q in &packets {
            d.packet(q, &mut out);
        }
        prop_assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn mutated_packets(which in 0usize..5, flips in prop::collection::vec((any::<usize>(), 0u8..8), 1..40), cut in any::<usize>()) {
        let names = ["stereo-q5", "mono-q0", "surround-q3", "stereo-q10-silence", "stereo-48k"];
        let p = stream(names[which]);
        let mut d = Decoder::new(&p[0], &p[2]).unwrap();
        let mut out = Vec::new();
        for (i, q) in p[3..].iter().enumerate() {
            let mut q = q.clone();
            if !q.is_empty() {
                for &(at, bit) in &flips {
                    let n = q.len();
                    q[at % n] ^= 1 << bit;
                }
                if i % 3 == 0 {
                    q.truncate(cut % (q.len() + 1));
                }
            }
            d.packet(&q, &mut out);
        }
    }

    #[test]
    fn mutated_setup(which in 0usize..5, flips in prop::collection::vec((any::<usize>(), 0u8..8), 1..8), cut in any::<usize>()) {
        let names = ["stereo-q5", "mono-q0", "surround-q3", "stereo-q10-silence", "stereo-48k"];
        let p = stream(names[which]);
        let mut s = p[2].clone();
        let n = s.len();
        for &(at, bit) in &flips {
            // (past the "\x05vorbis" signature)
            s[7 + at % (n - 7)] ^= 1 << bit;
        }
        if cut % 4 == 0 {
            s.truncate(7 + cut % (n - 7));
        }
        if let Ok(mut d) = Decoder::new(&p[0], &s) {
            let mut out = Vec::new();
            for q in &p[3..] {
                d.packet(q, &mut out);
            }
        }
    }
}
