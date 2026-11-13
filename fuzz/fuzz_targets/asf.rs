//! The ASF demuxer sake's Windows Media readers use (crates/asf): no panic on any bytes as a
//! file, packets or objects never larger than their stated size (asf::MAX_OBJECT), and seeking
//! anywhere in it is safe.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = asf::packet(data);
    if let Ok(mut r) = asf::Reader::new(data.to_vec()) {
        let mut n = 0;
        while let Some(s) = r.next_sample() {
            assert!(s.data.len() <= asf::MAX_OBJECT as usize);
            n += 1;
            if n > 4096 {
                break;
            }
        }
        let t = data.first().copied().unwrap_or(0) as u64 * 1_000_000;
        r.seek(t);
        let _ = r.next_sample();
    }
});
