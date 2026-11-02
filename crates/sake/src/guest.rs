//! C library functions as guest x86 code (guest/crt.c, built by guest/gen-guest.sh): qsort and
//! bsearch call the game's comparison function directly (no trap per call), and the small
//! string/memory functions skip the trap entirely. Placed at a fixed address before anything
//! else is allocated.

#[path = "guest_crt.rs"]
mod code;

use crate::mem;
use std::sync::atomic::{AtomicBool, Ordering};

static PLACED: AtomicBool = AtomicBool::new(false);

/// Maps the code (read+execute: the JIT translates no writable code).
pub fn init() {
    if std::env::var_os("SAKE_NO_GUEST_CRT").is_some() {
        return;
    }
    let size = (code::CODE.len() as u32 + 0xfff) & !0xfff;
    let Some(at) = mem::reserve(code::AT, size, 0x20, 0x20000, false) else {
        eprintln!("sake: no room for the guest C library at {:#x}", code::AT);
        return;
    };
    mem::commit(at, 0x20);
    mem::write(at, code::CODE);
    PLACED.store(true, Ordering::Relaxed);
}

/// Points the code's host slots at the traps (once the C library's Rust side is registered).
pub fn bind_host() {
    if !PLACED.load(Ordering::Relaxed) {
        return;
    }
    // big copies and fills, realloc's other cases: the host's traps; malloc and free: the x86
    // stubs (small blocks from the free lists)
    for (slot, name, stub) in [
        ("host_memmove", "memmove", false),
        ("host_memset", "memset", false),
        ("host_realloc", "realloc", false),
        ("host_malloc", "malloc", true),
        ("host_free", "free", true),
    ] {
        let trap = crate::api::export("msvcrt", name);
        mem::w32(sym(slot).unwrap(), if stub { crate::fast::bind("msvcrt", name, trap) } else { trap });
    }
}

fn sym(name: &str) -> Option<u32> {
    code::SYMS.iter().find(|(n, _)| *n == name).map(|&(_, a)| a)
}

/// The guest code for C library export `name` of DLL `key` (dll_key), if there is one.
pub fn lookup(key: &str, name: &str) -> Option<u32> {
    if !PLACED.load(Ordering::Relaxed) || name.starts_with("host_") {
        return None;
    }
    let crt = matches!(key, "msvcrt" | "ucrtbase" | "crtdll") || (key.starts_with("msvcr") && key[5..].bytes().all(|b| b.is_ascii_digit()));
    if !crt {
        return None;
    }
    sym(name)
}
