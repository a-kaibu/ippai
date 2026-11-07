//! `ippai add`'s unpacking (crates/ippai/src/unpack.rs: zip, InstallShield, zlib-stream setups,
//! the installers inside) on any file, as a .zip or an .exe (the first byte picks), without the
//! tools: no panic, no allocation over 16 MiB, never more written than the caps, nothing
//! outside the output folder.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/unpack.rs"]
mod unpack;

use libfuzzer_sys::fuzz_target;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static MAX: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        MAX.fetch_max(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        MAX.fetch_max(l.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        MAX.fetch_max(n, Ordering::Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const LIM: unpack::Limits = unpack::Limits { files: 32, file_bytes: 1 << 20, total_bytes: 2 << 20, tool_time: Duration::from_secs(1), tools: false };

/// (entries, bytes) under `d`.
fn du(d: &Path) -> (u64, u64) {
    let (mut n, mut b) = (0, 0);
    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
        let m = e.path().symlink_metadata().unwrap();
        n += 1;
        if m.is_dir() {
            let (n2, b2) = du(&e.path());
            n += n2;
            b += b2;
        } else {
            b += m.len();
        }
    }
    (n, b)
}

fuzz_target!(|data: &[u8]| {
    let Some((&k, body)) = data.split_first() else { return };
    let dir = std::env::temp_dir().join(format!("ippai-fuzz-unpack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(if k & 1 == 0 { "x.zip" } else { "x.exe" });
    // (a full disk is no finding)
    if std::fs::write(&src, body).is_err() {
        return;
    }
    let out = dir.join("out");
    MAX.store(0, Ordering::Relaxed);
    let _ = unpack::unpack_with(&src, &out, LIM);
    let max = MAX.load(Ordering::Relaxed);
    // (the 1 MiB read buffers, the zlib header scan's 1 MiB)
    assert!(max <= 16 << 20, "an allocation of {max} bytes");
    let (n, b) = du(&out);
    // (folders made for a file's path are not counted: a few per file)
    assert!(b <= LIM.total_bytes && n <= LIM.files * 8, "{n} entries, {b} bytes");
    let mut top: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    top.sort();
    assert!(top.len() <= 2, "{top:?}");
    let _ = std::fs::remove_dir_all(&dir);
});
