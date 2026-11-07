//! UuidFromString's parser (sake::misc::parse_uuid) on any UTF-16 text: it takes exactly the
//! 8-4-4-4-12 hex form, and what it gives back prints as the same text (lowercase).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let t: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let model = t.len() == 36
        && t.iter().enumerate().all(|(i, &c)| if matches!(i, 8 | 13 | 18 | 23) { c == u16::from(b'-') } else { c < 0x80 && (c as u8).is_ascii_hexdigit() });
    let got = sake::misc::parse_uuid(&t);
    assert_eq!(got.is_some(), model);
    if let Some(u) = got {
        let d1 = u32::from_le_bytes([u[0], u[1], u[2], u[3]]);
        let d2 = u16::from_le_bytes([u[4], u[5]]);
        let d3 = u16::from_le_bytes([u[6], u[7]]);
        let rest: String = u[10..].iter().map(|b| format!("{b:02x}")).collect();
        let text = format!("{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{rest}", u[8], u[9]);
        let lower: String = t.iter().map(|&c| (c as u8).to_ascii_lowercase() as char).collect();
        assert_eq!(text, lower);
    }
});
