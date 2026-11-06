//! strftime and mktime on any format and struct tm: no panic; mktime's result, when it has one,
//! is in range and gmtime of it gives back the normalized fields.
#![no_main]
use crt::time;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 36 {
        return;
    }
    let mut tm: time::Tm = std::array::from_fn(|i| i32::from_le_bytes(data[4 * i..4 * i + 4].try_into().unwrap()));
    let f: Vec<u16> = data[36..].chunks(2).map(|c| c[0] as u16 | (c.get(1).copied().unwrap_or(0) as u16) << 8).collect();
    let _ = time::strftime(&f, &tm, (9 * 3600, "Tokyo Standard Time"));
    if let Some(t) = time::mktime(&mut tm, 64, 0) {
        assert!((0..=time::MAX_TIME64).contains(&t));
        assert_eq!(time::gmtime(t, 64), Some(tm));
        let _ = time::strftime(&f, &tm, (0, "UTC")).map(|s| assert!(s.len() < 1 << 20));
    }
});
