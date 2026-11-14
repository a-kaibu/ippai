//! The metadata page address a user pastes (crates/masu/src/meta_url.rs): no panic on any
//! text, and an answer is always `egs:<n>` / `vndb:v<n>` with n a u32 over 0 without leading
//! zeros, found again when the answer itself is given.
#![no_main]

#[path = "../../src/meta_url.rs"]
mod meta_url;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    if let Some(id) = meta_url::entry_id(s) {
        let n = id.strip_prefix("egs:").or(id.strip_prefix("vndb:v")).expect("prefix");
        assert!(!n.starts_with('0') && n.parse::<u32>().is_ok_and(|n| n > 0), "{id}");
        assert_eq!(meta_url::entry_id(&id).as_deref(), Some(id.as_str()));
    }
});
