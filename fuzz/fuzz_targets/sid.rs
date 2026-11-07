//! ConvertStringSidToSid's parser (sake::security::parse_sid) on any text: no panic, at most 15
//! sub-authorities and a 48-bit authority, and what it parses prints (ConvertSidToStringSid's
//! sid_to_string) as text that parses back to the same SID.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(t) = std::str::from_utf8(data) else { return };
    if let Some(s) = sake::security::parse_sid(t) {
        assert!(s.1.len() <= 15 && s.0 < 1 << 48);
        let text = sake::security::sid_to_string(&s);
        assert_eq!(sake::security::parse_sid(&text), Some(s));
    }
});
