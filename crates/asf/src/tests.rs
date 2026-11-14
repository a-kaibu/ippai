use super::*;
use crate::write::*;
use proptest::prelude::*;

fn w16(v: &mut Vec<u8>, x: u16) {
    v.extend(x.to_le_bytes());
}
fn w32(v: &mut Vec<u8>, x: u32) {
    v.extend(x.to_le_bytes());
}

fn wave(tag: u16, ch: u16, rate: u32, align: u16, extra: &[u8]) -> Vec<u8> {
    let mut w = Vec::new();
    w16(&mut w, tag);
    w16(&mut w, ch);
    w32(&mut w, rate);
    w32(&mut w, rate * align as u32);
    w16(&mut w, align);
    w16(&mut w, 16);
    w16(&mut w, extra.len() as u16);
    w.extend(extra);
    w
}
fn bih(w: u32, h: u32, fourcc: &[u8; 4], extra: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    w32(&mut b, 40 + extra.len() as u32);
    w32(&mut b, w);
    w32(&mut b, h);
    w16(&mut b, 1);
    w16(&mut b, 24);
    b.extend(fourcc);
    for _ in 0..5 {
        w32(&mut b, 0);
    }
    b.extend(extra);
    b
}

fn two_streams(spread: Option<Spread>) -> Vec<Stream> {
    vec![
        Stream { number: 1, kind: Kind::Audio, format: wave(0x161, 2, 44100, 0x2e7, &[0, 0, 0, 0, 0x17, 0, 0, 0, 0, 0]), encrypted: false, time_offset: 0, spread, avg_time_per_frame: 0, bitrate: 64000 },
        Stream { number: 2, kind: Kind::Video { width: 320, height: 240 }, format: bih(320, 240, b"WMV3", &[0x4f, 0xf1, 0x08, 0x01]), encrypted: false, time_offset: 0, spread: None, avg_time_per_frame: 333_333, bitrate: 1_000_000 },
    ]
}

#[test]
fn header_fields() {
    let m = Mux { packet_size: 1024, preroll: 3000, streams: two_streams(None), attributes: vec![("WM/Title".into(), Value::Str("t".into())), ("IsVBR".into(), Value::Bool(false))], compress_small: false };
    let f = m.write(&[(2, true, 0, vec![7; 3000])], 8930);
    let h = read_header(&mut f.as_slice()).unwrap();
    assert_eq!(h.duration, 8930 * 10_000);
    assert_eq!(h.preroll, 3000);
    assert_eq!(h.packet_size, 1024);
    assert_eq!(h.streams.len(), 2);
    assert_eq!(h.streams[0].codec(), 0x161);
    assert_eq!(h.streams[1].codec(), u32::from_le_bytes(*b"WMV3"));
    assert_eq!(h.streams[1].kind, Kind::Video { width: 320, height: 240 });
    assert_eq!(h.streams[1].avg_time_per_frame, 333_333);
    assert_eq!(h.streams[0].bitrate, 64000);
    assert_eq!(h.attribute(0, "WM/Title"), Some(&Value::Str("t".into())));
    assert_eq!(h.attribute(0, "IsVBR"), Some(&Value::Bool(false)));
    assert_eq!(h.data_offset as usize, f.len() - h.packets as usize * 1024);
}

#[test]
fn not_asf() {
    assert_eq!(read_header(&mut [0u8; 64].as_slice()), Err(Error::NotAsf));
    assert_eq!(read_header(&mut [0u8; 3].as_slice()), Err(Error::Truncated));
}

#[test]
fn a_seek_goes_to_the_key_frame_before() {
    let m = Mux { packet_size: 512, preroll: 0, streams: two_streams(None), attributes: vec![], compress_small: false };
    let mut s = Vec::new();
    for k in 0..60u32 {
        s.push((2u8, k % 10 == 0, k * 33, vec![k as u8; 700]));
        s.push((1u8, true, k * 33, vec![k as u8; 100]));
    }
    let f = m.write(&s, 2000);
    let mut r = Reader::new(f).unwrap();
    r.seek(25 * 33 * 10_000);
    let first_video = std::iter::from_fn(|| r.next_sample()).find(|s| s.stream == 2).unwrap();
    assert!(first_video.key);
    assert_eq!(first_video.time, 20 * 33 * 10_000);
    r.seek(0);
    assert_eq!(r.next_sample().unwrap().time, 0);
}

fn sample_strategy() -> impl Strategy<Value = Vec<(u8, bool, u32, Vec<u8>)>> {
    prop::collection::vec((1u8..=3, any::<bool>(), prop::collection::vec(any::<u8>(), 0..2500)), 0..40).prop_map(|v| {
        v.into_iter().enumerate().map(|(i, (s, k, d))| (s, k, i as u32 * 20, d)).collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    /// Whatever objects go in come out whole, in order, with their times and key flags.
    #[test]
    fn objects_round_trip(samples in sample_strategy(), packet in 300usize..3000, preroll in 0u64..5000, small in any::<bool>()) {
        let mut streams = two_streams(None);
        streams.push(Stream { number: 3, kind: Kind::Other([9; 16]), format: vec![], encrypted: false, time_offset: 0, spread: None, avg_time_per_frame: 0, bitrate: 0 });
        let m = Mux { packet_size: packet, preroll, streams, attributes: vec![], compress_small: small };
        let f = m.write(&samples, 1000);
        let mut r = Reader::new(f).unwrap();
        let got: Vec<_> = std::iter::from_fn(|| r.next_sample()).collect();
        // (an empty object: a compressed payload cannot hold one, and a plain one is whole at once)
        let want: Vec<_> = samples.iter().map(|(s, k, t, d)| Sample { stream: *s, key: *k, time: *t as u64 * 10_000, data: d.clone() }).collect();
        prop_assert_eq!(got.len(), want.len());
        for s in 1..=3u8 {
            let g: Vec<_> = got.iter().filter(|x| x.stream == s).collect();
            let w: Vec<_> = want.iter().filter(|x| x.stream == s).collect();
            prop_assert_eq!(g, w);
        }
    }

    /// The Audio Spread interleaving is undone, for any span.
    #[test]
    fn spread_round_trip(span in 2u8..8, chunks in 1u16..8, cs in 1u16..64, seed in any::<u64>()) {
        let s = Spread { span, packet_size: chunks * cs, chunk_size: cs };
        let n = s.packet_size as usize * span as usize;
        let d: Vec<u8> = (0..n).map(|i| (seed.wrapping_mul(i as u64 + 1) >> 7) as u8).collect();
        prop_assert_eq!(descramble(&scramble(&d, s), s), d.clone());
        // (a size that is not a whole span stays as it is)
        prop_assert_eq!(descramble(&d[..n - 1], s), d[..n - 1].to_vec());
    }

    /// Spread audio comes out as it went in.
    #[test]
    fn spread_audio_through_the_file(span in 2u8..6, cs in 1u16..40, objs in 1usize..6) {
        let s = Spread { span, packet_size: cs * 3, chunk_size: cs };
        let m = Mux { packet_size: 1500, preroll: 0, streams: two_streams(Some(s)), attributes: vec![], compress_small: false };
        let n = s.packet_size as usize * span as usize;
        let samples: Vec<_> = (0..objs).map(|k| (1u8, true, k as u32 * 10, (0..n).map(|i| (i * 7 + k) as u8).collect::<Vec<u8>>())).collect();
        let mut r = Reader::new(m.write(&samples, 100)).unwrap();
        for (_, _, _, d) in &samples {
            prop_assert_eq!(&r.next_sample().unwrap().data, d);
        }
    }

    /// Damaged files never panic, nor make objects past their stated size.
    #[test]
    fn damage_is_safe(samples in sample_strategy(), flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..40)) {
        let m = Mux { packet_size: 700, preroll: 100, streams: two_streams(None), attributes: vec![("A".into(), Value::U32(1))], compress_small: true };
        let mut f = m.write(&samples, 1000);
        for (i, x) in flips {
            let k = i.index(f.len());
            f[k] ^= x;
        }
        if let Ok(mut r) = Reader::new(f) {
            let mut n = 0;
            while let Some(s) = r.next_sample() {
                prop_assert!(s.data.len() <= MAX_OBJECT as usize);
                n += 1;
                if n > 10_000 { break; }
            }
            r.seek(500 * 10_000);
            let _ = r.next_sample();
        }
    }

    /// Any bytes as a packet: no panic.
    #[test]
    fn any_packet(b in prop::collection::vec(any::<u8>(), 0..600)) {
        let _ = packet(&b);
    }
}

/// Differential: a WMV the real muxer made (ffmpeg's asf muxer: WMV2 + WMA2) reads as ffprobe
/// lists its packets (sizes, times, key frames per stream).
#[test]
fn matches_ffprobe() {
    use std::process::Command;
    if Command::new("ffmpeg").arg("-version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("asf-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.wmv");
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=176x144:rate=15:duration=3", "-f", "lavfi", "-i", "sine=frequency=440:duration=3"])
        .args(["-c:v", "wmv2", "-g", "10", "-c:a", "wmav2", "-b:a", "64k", "-packetsize", "3200"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "packet=stream_index,pts,size,flags", "-of", "csv=p=0"])
        .arg(&path)
        .output()
        .unwrap();
    let text = String::from_utf8(probe.stdout).unwrap();
    let f = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let mut r = Reader::new(f).unwrap();
    // ffprobe's stream index k is the k-th stream of the header
    let numbers: Vec<u8> = r.header.streams.iter().map(|s| s.number).collect();
    let got: Vec<Sample> = std::iter::from_fn(|| r.next_sample()).collect();
    let mut want: Vec<(u8, u64, usize, bool)> = Vec::new();
    for l in text.lines() {
        let v: Vec<&str> = l.split(',').collect();
        let idx: usize = v[0].parse().unwrap();
        let pts: u64 = v[1].parse().unwrap();
        let size: usize = v[2].parse().unwrap();
        want.push((numbers[idx], pts * 10_000, size, v[3].starts_with('K')));
    }
    assert!(want.len() > 40, "{text}");
    for s in &numbers {
        let g: Vec<_> = got.iter().filter(|x| x.stream == *s).map(|x| (x.stream, x.time, x.data.len(), x.key || r.header.stream(*s).is_some_and(|t| t.kind == Kind::Audio))).collect();
        let w: Vec<_> = want.iter().filter(|x| x.0 == *s).cloned().collect();
        assert_eq!(g, w, "stream {s}");
    }
    assert!(r.header.duration >= 29_000_000 && r.header.duration <= 31_000_000, "{}", r.header.duration);
}

/// Review A-7: a header declaring packets of 1 MiB + 1 to 4 GiB is refused (each packet is read
/// into a Vec of that size: 2-4 GiB is an abort on wasm32); 1 MiB still opens.
#[test]
fn huge_packet_sizes_are_refused() {
    let m = Mux { packet_size: 1024, preroll: 0, streams: two_streams(None), attributes: vec![], compress_small: false };
    let f = m.write(&[(2, true, 0, vec![7; 3000])], 1000);
    // (File Properties: minimum and maximum data packet size, side by side)
    let pat: Vec<u8> = [1024u32.to_le_bytes(), 1024u32.to_le_bytes()].concat();
    let at = f.windows(8).position(|w| w == &pat[..]).unwrap();
    for (size, ok) in [(MAX_PACKET, true), (MAX_PACKET + 1, false), (0x7fff_ffff, false), (0xffff_ffff, false), (0, false)] {
        let mut g = f.clone();
        g[at..at + 4].copy_from_slice(&size.to_le_bytes());
        g[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
        let h = read_header(&mut g.as_slice());
        assert_eq!(h.is_ok(), ok, "{size:#x}");
        if ok {
            assert_eq!(h.unwrap().packet_size, size);
        }
    }
}

fn header_of(streams: Vec<Stream>) -> Header {
    Header { streams, duration: 0, preroll: 0, flags: 2, packet_size: 1024, max_bitrate: 0, data_offset: 0, packets: 0, ..Default::default() }
}

/// review b00: objects cut into fragments of any sizes, some sent again whole or in part
/// (overlapping what came): every object comes out as it was, no hole of zeros counted as
/// data; payloads of a stream the header has not are dropped
#[test]
fn reassembly_of_overlapping_fragments() {
    proptest!(ProptestConfig { cases: 300, ..ProptestConfig::default() }, |(objs in prop::collection::vec((1u8..3, prop::collection::vec(any::<u8>(), 0..400)), 1..8), cuts in prop::collection::vec(1usize..90, 1..40), again in prop::collection::vec((any::<prop::sample::Index>(), 1usize..120), 0..8), stray in any::<bool>())| {
        let mut a = Assembler::new(&header_of(two_streams(None)));
        let mut got = vec![];
        let mut want = vec![];
        for (no, (stream, data)) in objs.iter().enumerate() {
            want.push((*stream, data.clone()));
            let mut frags: Vec<(usize, usize)> = vec![];
            let mut at = 0;
            let mut k = 0;
            while at < data.len() || frags.is_empty() {
                let n = cuts[k % cuts.len()].min(data.len() - at);
                frags.push((at, n));
                at += n;
                k += 1;
                if n == 0 { break }
            }
            // (each fragment but the last possibly followed by a resend of an earlier stretch;
            // after the last the object is whole and gone: a payload at 0 is a new object of
            // the same number, as muxers that number every object 0 write them)
            let mut sends = vec![];
            for (i, &(off, n)) in frags.iter().enumerate() {
                sends.push((off, n));
                if let Some((ix, len)) = again.get(i).filter(|_| i + 1 < frags.len()) {
                    let end = off + n;
                    let start = ix.index(end + 1);
                    sends.push((start, (end - start).min(*len)));
                }
            }
            for (off, n) in sends {
                if stray {
                    // (stream 9: not in the header)
                    got.extend(a.push(Payload { stream: 9, key: true, object: no as u32, offset: off as u32, object_size: data.len() as u32, time: 0, data: data[off..off + n].to_vec() }));
                }
                got.extend(a.push(Payload { stream: *stream, key: true, object: no as u32, offset: off as u32, object_size: data.len() as u32, time: 0, data: data[off..off + n].to_vec() }));
            }
        }
        let got: Vec<_> = got.into_iter().map(|s| (s.stream, s.data)).collect();
        prop_assert_eq!(got, want);
    });
}

/// review b00: an object whose size word says 64 MiB takes memory as its bytes arrive (it was
/// 64 MiB of zeros at once, per stream, per new object); a gap drops it
#[test]
fn objects_take_memory_as_they_come() {
    let mut a = Assembler::new(&header_of(two_streams(None)));
    for obj in 0..200u32 {
        for s in [1u8, 2] {
            assert!(a.push(Payload { stream: s, key: true, object: obj, offset: 0, object_size: u32::MAX, time: 0, data: vec![1; 10] }).is_none());
        }
    }
    let held: usize = a.held();
    assert!(held < 1 << 20, "{held}");
    a.reset();
    // (a gap: the object is dropped; the next payload in order would have completed it)
    assert!(a.push(Payload { stream: 1, key: true, object: 7, offset: 0, object_size: 8, time: 0, data: vec![1; 4] }).is_none());
    assert!(a.push(Payload { stream: 1, key: true, object: 7, offset: 6, object_size: 8, time: 0, data: vec![1; 2] }).is_none());
    assert!(a.push(Payload { stream: 1, key: true, object: 7, offset: 4, object_size: 8, time: 0, data: vec![1; 2] }).is_none());
    assert_eq!(a.held(), 0);
}
