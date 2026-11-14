//! The profile (.ini) cache (sake::ini::fuzz_cache): any mix of WritePrivateProfileString,
//! the game writing the file itself (any bytes, just now or with an older time) and removals;
//! every read answers what a fresh read of the file says.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let d = std::env::temp_dir().join(format!("ippai-fuzz-profile-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    sake::ini::fuzz_cache(&d, data);
});

// What the page or emscripten gives kura's file system (sake/src/lib.rs host_stubs, there
// only for its tests): no store to fetch from, and a clock at 0
#[no_mangle]
pub extern "C" fn vnvm_fetch(_name: *const std::ffi::c_char, _len: *mut usize) -> *mut u8 {
    std::ptr::null_mut()
}
#[no_mangle]
pub extern "C" fn vnvm_fetch_hint(_names: *const std::ffi::c_char, _len: usize) {}
#[no_mangle]
pub extern "C" fn emscripten_get_now() -> f64 {
    0.0
}
