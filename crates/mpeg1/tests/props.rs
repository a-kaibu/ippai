//! check.rs's properties over testsrc.mpg with bytes changed, cut anywhere (the fuzz target
//! fuzz/fuzz_targets/mpeg1.rs runs the same over any input).

use proptest::prelude::*;

fn data() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/testsrc.mpg")).unwrap()
}

#[test]
fn the_stream_cut_anywhere_decodes_the_same() {
    let d = data();
    for step in [1, 7, 188, 2048, 4093] {
        let cuts = vec![step; d.len() / step];
        mpeg1::check::check(&d, &cuts);
    }
    let w = mpeg1::check::decode(&d, &[]);
    assert_eq!(w.pictures.len(), 30);
    assert_eq!(w.samples.len(), 46 * 1152 * 2);
}

#[test]
fn a_last_frame_cut_inside_its_crc() {
    // (a layer II header with CRC, then the stream ends: the frame's data start past its end;
    // found by fuzz/fuzz_targets/mpeg1.rs, fuzz/regressions/mpeg1/crc-frame-cut-short)
    for d in [&[0xff, 0xfc, 0x10, 0xff][..], &[0xff, 0xfc, 0x10, 0xff, 0x00], &[0, 0, 1, 0xc0, 0, 4, 0xff, 0xfc, 0x10, 0xff]] {
        for cut in 0..d.len() {
            mpeg1::check::check(d, &[cut]);
        }
    }
    let d = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/regressions/mpeg1/crc-frame-cut-short")).unwrap();
    mpeg1::check::check(&d[1..], &[]);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn changed_streams_never_break(changes in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..40), cuts in prop::collection::vec(0usize..5000, 0..12), keep in 0.5f64..1.0) {
        let mut d = data();
        for (i, b) in &changes {
            let i = i.index(d.len());
            d[i] = *b;
        }
        d.truncate((d.len() as f64 * keep) as usize);
        mpeg1::check::check(&d, &cuts);
    }

    #[test]
    fn any_bytes_never_break(head in prop::sample::select(vec![vec![0, 0, 1, 0xba], vec![0, 0, 1, 0xb3], vec![0xff, 0xfd]]), body in prop::collection::vec(any::<u8>(), 0..4000), cuts in prop::collection::vec(0usize..600, 0..8)) {
        let mut d = head;
        d.extend(body);
        mpeg1::check::check(&d, &cuts);
    }
}
