//! Enigma Virtual Box unpacking for `masu pack` (crates/masu/src/evb.rs) on any file: no panic,
//! every file it would write stays inside the folder (a plain relative path) and inside the exe's
//! bytes, and aPLib depacking keeps to its cap.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/evb.rs"]
mod evb;

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;
use std::path::Component;

fuzz_target!(|data: &[u8]| {
    let _ = evb::depack(data, 1 << 16).map(|v| assert!(v.len() <= 1 << 16));
    let head = &data[..data.len().min(4096)];
    let mut read = |o: u64, l: usize| Ok(data.get(o as usize..).map(|d| d[..d.len().min(l)].to_vec()).unwrap_or_default());
    // (a box at the start too, as if found in a .enigma1)
    let at = evb::locate(head, &mut read).ok().flatten().unwrap_or(0);
    let mut c = Cursor::new(data);
    if let Ok(files) = evb::parse(&mut c, at, data.len() as u64) {
        for (p, src) in files {
            assert!(p.components().all(|c| matches!(c, Component::Normal(_))), "{}", p.display());
            if let Some(src) = src {
                assert!(src.offset + src.stored <= data.len() as u64);
                let mut out = Vec::new();
                if let Ok(n) = evb::extract(&mut c, &src, &mut out, 1 << 20) {
                    assert_eq!(n as usize, out.len());
                    assert_eq!(n, src.size);
                }
            }
        }
    }
});
