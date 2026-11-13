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

/// testsrc.mpg with `pad` bytes of padding packets (of up to `size` bytes each) before its end
/// code, as アクアリウム (aquras) 体験版's opening ends (#230).
fn padded(pad: usize, size: usize) -> Vec<u8> {
    let mut d = data();
    if d.ends_with(&[0, 0, 1, 0xb9]) {
        d.truncate(d.len() - 4);
    }
    let mut left = pad;
    while left > 0 {
        let n = left.clamp(6, size.max(6)).min(65535 + 6);
        d.extend([0, 0, 1, 0xbe]);
        d.extend(((n - 6) as u16).to_be_bytes());
        d.extend(std::iter::repeat_n(0xff, n - 6));
        left = left.saturating_sub(n);
    }
    d.extend([0, 0, 1, 0xb9]);
    d
}

/// probe_stream over `d`, read in the parts it asks for (each inside the stream, counted).
fn probe_parts(d: &[u8], part: u64) -> (Option<mpeg1::Info>, usize) {
    let mut read = 0;
    let i = mpeg1::probe_stream(d.len() as u64, part, |pos, n| {
        assert!(pos as usize + n <= d.len(), "read {n} at {pos} of {}", d.len());
        read += n;
        Some(d[pos as usize..pos as usize + n].to_vec())
    });
    (i, read)
}

#[test]
fn a_stream_ending_in_padding_has_its_duration() {
    let whole = mpeg1::probe(&data(), &data(), true).unwrap();
    // (the aquras case: the last picture 366 KB before the end, read 256 KB at a time)
    let d = padded(374776, 2028);
    assert_eq!(mpeg1::probe(&d[..256 << 10], &d[d.len() - (256 << 10)..], false).unwrap().duration, 0.0);
    let (i, _) = probe_parts(&d, 256 << 10);
    assert_eq!(i.unwrap(), whole);
}

#[test]
fn a_stream_without_times_stops_looking() {
    // (all padding after a picture-less head: no more than MAX_TAIL of its end read)
    let mut d = vec![0, 0, 1, 0xba, 0x21, 0, 1, 0, 1, 0x80, 0, 1];
    d.extend(data().into_iter().skip(12).take(4000));
    let mut d2 = d.clone();
    d2.resize(40 << 20, 0);
    let (_, read) = probe_parts(&d2, 256 << 10);
    assert!(read as u64 <= (256 << 10) + 2 * mpeg1::MAX_TAIL, "{read}");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// However much padding ends the stream and whatever part size it is read in, the facts are
    /// those of the whole stream while its last picture is within MAX_TAIL of the end; and the
    /// reads stay in the stream.
    #[test]
    fn padding_never_changes_the_facts(pad in prop_oneof![0usize..4000, 0usize..3 << 20], size in 6usize..70000, part in prop_oneof![Just(256u64 << 10), 1u64..64 << 10]) {
        let whole = mpeg1::probe(&data(), &data(), true).unwrap();
        let d = padded(pad, size);
        let (i, _) = probe_parts(&d, part);
        // (a head too short for the first picture's header: nothing found)
        let Some(i) = i else {
            prop_assert!(part < 16 << 10, "{part}");
            return Ok(());
        };
        prop_assert_eq!((i.width, i.height, i.rate), (whole.width, whole.height, whole.rate));
        // (a short head may hold no sound yet: only the facts the head holds are asked of
        // small parts)
        prop_assert!(i.audio_rate == whole.audio_rate || i.audio_rate == 0 && part < 16 << 10);
        if part >= 16 << 10 {
            prop_assert_eq!(i.audio_rate, whole.audio_rate);
            prop_assert!((i.duration - whole.duration).abs() < 1e-9, "{} vs {}", i.duration, whole.duration);
        }
    }
}
