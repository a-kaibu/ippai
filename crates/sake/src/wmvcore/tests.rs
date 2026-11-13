use super::*;
use asf::write::Mux;
use proptest::prelude::*;

fn wave_pcm(ch: u16, rate: u32) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend(1u16.to_le_bytes());
    w.extend(ch.to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend((rate * 2 * ch as u32).to_le_bytes());
    w.extend((2 * ch).to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(0u16.to_le_bytes());
    w
}
fn bih_rgb(w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(40u32.to_le_bytes());
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0u8; 24]);
    b
}

fn streams() -> Vec<asf::Stream> {
    vec![
        asf::Stream { number: 1, kind: asf::Kind::Audio, format: wave_pcm(2, 8000), encrypted: false, time_offset: 0, spread: None, avg_time_per_frame: 0, bitrate: 256000 },
        asf::Stream { number: 2, kind: asf::Kind::Video { width: 4, height: 2 }, format: bih_rgb(4, 2), encrypted: false, time_offset: 0, spread: None, avg_time_per_frame: 400_000, bitrate: 0 },
    ]
}

fn reader(samples: &[(u8, bool, u32, Vec<u8>)]) -> Reader {
    let m = Mux { packet_size: 600, preroll: 0, streams: streams(), attributes: vec![("WM/VideoWidth".into(), asf::Value::U32(4))], compress_small: false };
    let f = m.write(samples, 1000);
    let mut r = Reader { r: None, stream: 0, outputs: Vec::new(), pending: VecDeque::new(), range: (0, 0), ended: false };
    assert_eq!(open_src(&mut r, Box::new(f)), 0);
    r
}

/// A 4x2 RGB24 picture, bottom-up (rows of 12 bytes: already a multiple of 4).
fn picture(k: u8) -> Vec<u8> {
    (0..24).map(|i| i as u8 ^ k).collect()
}

#[test]
fn compressed_in_file_order() {
    let s: Vec<_> = (0..6u32).map(|k| ((k % 2 + 1) as u8, k % 3 == 0, k * 40, vec![k as u8; 30 + k as usize])).collect();
    let mut r = reader(&s);
    for o in &mut r.outputs {
        o.compressed = true;
    }
    for (n, key, t, d) in &s {
        let (k, time, _dur, data, kk) = next_sample(&mut r, 0).unwrap();
        assert_eq!((r.outputs[k].stream, time, &data, kk), (*n, *t as u64 * 10_000, d, *key));
    }
    assert!(next_sample(&mut r, 0).is_none());
}

#[test]
fn one_stream_leaves_the_others_for_later() {
    let s: Vec<_> = (0..8u32).map(|k| ((k % 2 + 1) as u8, true, k * 40, vec![k as u8; 20])).collect();
    let mut r = reader(&s);
    for o in &mut r.outputs {
        o.compressed = true;
    }
    let v: Vec<u8> = std::iter::from_fn(|| next_sample(&mut r, 2)).map(|x| x.3[0]).collect();
    assert_eq!(v, [1, 3, 5, 7]);
    let a: Vec<u8> = std::iter::from_fn(|| next_sample(&mut r, 0)).map(|x| x.3[0]).collect();
    assert_eq!(a, [0, 2, 4, 6]);
}

#[test]
fn a_range_ends_the_samples() {
    let s: Vec<_> = (0..8u32).map(|k| (2u8, true, k * 100, vec![k as u8; 20])).collect();
    let mut r = reader(&s);
    r.outputs[1].compressed = true;
    r.range = (0, 3_500_000);
    let v: Vec<u8> = std::iter::from_fn(|| next_sample(&mut r, 0)).map(|x| x.3[0]).collect();
    assert_eq!(v, [0, 1, 2, 3]);
}

#[test]
fn uncompressed_output_is_decoded() {
    let pcm: Vec<u8> = (0..64u8).collect();
    let s = vec![(1u8, true, 0u32, pcm.clone()), (2u8, true, 0, picture(0)), (2u8, false, 40, picture(9))];
    let mut r = reader(&s);
    // default output types: PCM as it is, RGB24 bottom-up
    assert_eq!(r.outputs[0].ty, crate::wmdec::pcm_type(2, 8000));
    assert_eq!(r.outputs[1].ty, crate::wmdec::rgb_type(4, 2, 24, 400_000));
    let mut got = Vec::new();
    while let Some((k, t, d, data, _)) = next_sample(&mut r, 0) {
        got.push((r.outputs[k].stream, t, d, data));
    }
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], (1, 0, 64 / 4 * 10_000_000 / 8000, pcm));
    assert_eq!(got[1].3, picture(0));
    assert_eq!((got[2].1, got[2].2, &got[2].3), (400_000, 400_000, &picture(9)));
}

#[test]
fn rgb32_output() {
    let s = vec![(2u8, true, 0u32, picture(0))];
    let mut r = reader(&s);
    r.outputs[1].ty = crate::wmdec::rgb_type(4, 2, 32, 400_000);
    let (_, _, _, data, _) = next_sample(&mut r, 2).unwrap();
    let want: Vec<u8> = picture(0).chunks(3).flat_map(|p| [p[0], p[1], p[2], 0]).collect();
    assert_eq!(data, want);
}

#[test]
fn attributes_of_the_header() {
    let r = reader(&[(2u8, true, 0, picture(0))]);
    let h = r.header().unwrap();
    let a = attributes(h, 0);
    let get = |n: &str| a.iter().find(|x| x.0 == n).map(|x| x.1.clone());
    assert_eq!(get("Duration"), Some(asf::Value::U64(1000 * 10_000)));
    assert_eq!(get("Seekable"), Some(asf::Value::Bool(true)));
    assert_eq!(get("HasVideo"), Some(asf::Value::Bool(true)));
    assert_eq!(get("WM/VideoWidth"), Some(asf::Value::U32(4)));
    assert_eq!(attr_bytes(&asf::Value::Str("ab".into())), (1, vec![b'a', 0, b'b', 0, 0, 0]));
    assert_eq!(attr_bytes(&asf::Value::Bool(true)), (3, vec![1, 0, 0, 0]));
}

#[test]
fn stream_types() {
    let st = streams();
    let a = stream_type(&st[0]);
    assert_eq!((a.major, a.sub, a.format_type, a.sample_size), (MEDIATYPE_AUDIO, fourcc(1), FORMAT_WAVE_FORMAT_EX, 4));
    let v = stream_type(&st[1]);
    assert_eq!((v.major, v.format_type, v.temporal), (MEDIATYPE_VIDEO, FORMAT_VIDEO_INFO, true));
    // VIDEOINFOHEADER: rcSource / rcTarget 4x2, AvgTimePerFrame, then the BITMAPINFOHEADER
    assert_eq!(&v.format[8..16], &[4, 0, 0, 0, 2, 0, 0, 0]);
    assert_eq!(u64::from_le_bytes(v.format[40..48].try_into().unwrap()), 400_000);
    assert_eq!(&v.format[48..], &bih_rgb(4, 2)[..]);
}

proptest! {
    /// Pulling per stream or from all in any order gives each stream's samples once, in order
    /// (a model of the reader's queues).
    #[test]
    fn pulls_take_each_sample_once(kinds in prop::collection::vec(1u8..=2, 1..30), pulls in prop::collection::vec(0u8..=2, 1..80)) {
        let s: Vec<_> = kinds.iter().enumerate().map(|(i, &n)| (n, true, i as u32 * 10, vec![i as u8; 10])).collect();
        let mut r = reader(&s);
        for o in &mut r.outputs {
            o.compressed = true;
        }
        let mut next = [0usize; 3];
        let per: [Vec<u8>; 3] = [vec![], s.iter().filter(|x| x.0 == 1).map(|x| x.3[0]).collect(), s.iter().filter(|x| x.0 == 2).map(|x| x.3[0]).collect()];
        let mut left: Vec<u8> = s.iter().map(|x| x.3[0]).collect();
        for p in pulls.into_iter().chain(std::iter::repeat_n(0, 40)) {
            let got = next_sample(&mut r, p);
            match got {
                Some((k, _, _, d, _)) => {
                    let n = r.outputs[k].stream as usize;
                    prop_assert!(p == 0 || p as usize == n);
                    prop_assert_eq!(d[0], per[n][next[n]]);
                    next[n] += 1;
                    if p == 0 {
                        // (from all: the earliest left, in file order)
                        prop_assert_eq!(d[0], left[0]);
                    }
                    left.retain(|&x| x != d[0]);
                }
                None => {
                    let done = if p == 0 { left.is_empty() } else { next[p as usize] == per[p as usize].len() };
                    prop_assert!(done);
                }
            }
        }
        prop_assert!(left.is_empty());
    }
}
