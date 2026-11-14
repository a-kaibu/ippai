//! Electron's app.asar unpacking for `masu pack` (crates/masu/src/asar.rs) on any file: no
//! panic, and every file it would write stays inside the folder (a plain relative path) and
//! inside the archive's bytes.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/asar.rs"]
mod asar;

use libfuzzer_sys::fuzz_target;
use std::path::Component;

fuzz_target!(|data: &[u8]| {
    if let Ok(Some((start, files))) = asar::parse(data, data.len() as u64) {
        assert!(start <= data.len() as u64);
        for (p, src) in files {
            assert!(p.components().all(|c| matches!(c, Component::Normal(_))), "{}", p.display());
            if let Some(asar::Source::Packed { offset, size }) = src {
                assert!(start + offset + size <= data.len() as u64);
            }
        }
    }
});
