//! C library functions as guest x86 code (guest/crt.c, built by guest/gen-guest.sh): qsort and
//! bsearch call the game's comparison function directly (no trap per call), and the small
//! string/memory functions skip the trap entirely. Placed at a fixed address before anything
//! else is allocated. x64 programs get the x86-64 build, which also has the x64 forms of
//! fast.rs's stubs (critical sections, last error, TLS, small heap blocks, errno).

#[path = "guest_crt.rs"]
mod code;
#[path = "guest_crt64.rs"]
mod code64;

use crate::mem;
use std::sync::atomic::{AtomicBool, Ordering};

static PLACED: AtomicBool = AtomicBool::new(false);

/// Maps the code (read+execute: the JIT translates no writable code). VirtualQuery calls it
/// MEM_IMAGE: the system DLLs' functions it stands for (GetLastError, HeapAlloc, ...) are image
/// code on Windows, and programs that check their imports look (tests/apiimage.c).
pub fn init() {
    if crate::env_on!("SAKE_NO_GUEST_CRT") {
        return;
    }
    let (want, bytes) = if mem::x64() { (code64::AT, code64::CODE) } else { (code::AT, code::CODE) };
    let size = (bytes.len() as u32 + 0xfff) & !0xfff;
    let Some(at) = mem::reserve(want, size, 0x20, 0x100_0000, false) else {
        eprintln!("sake: no room for the guest C library at {want:#x}");
        return;
    };
    mem::commit(at, 0x20);
    mem::write(at, bytes);
    PLACED.store(true, Ordering::Relaxed);
}

/// Points the code's host slots at the traps (once the C library's Rust side is registered).
pub fn bind_host() {
    if !PLACED.load(Ordering::Relaxed) {
        return;
    }
    if mem::x64() {
        // (x64: the fast paths are in the guest code too, the traps behind them)
        for (slot, dll, name) in [
            ("host_memmove", "msvcrt", "memmove"),
            ("host_memset", "msvcrt", "memset"),
            ("host_realloc", "msvcrt", "realloc"),
            ("host_malloc", "msvcrt", "malloc"),
            ("host_free", "msvcrt", "free"),
            ("host_enter_cs", "kernel32", "EnterCriticalSection"),
            ("host_tls_get", "kernel32", "TlsGetValue"),
            ("host_heap_alloc", "kernel32", "HeapAlloc"),
            ("host_heap_free", "kernel32", "HeapFree"),
        ] {
            mem::w64(sym(slot).unwrap(), crate::api::export(dll, name) as u64);
        }
        mem::w64(sym("host_cs_wake").unwrap(), crate::api::host_fn("cs_wake64", cs_wake, 0) as u64);
        mem::w64(sym("slab_table").unwrap(), mem::slab_table() as u64);
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

fn cs_wake(c: &mut crate::api::Ctx) -> u32 {
    crate::k32::cs_wake(c.a(0));
    0
}

fn sym(name: &str) -> Option<u32> {
    let syms = if mem::x64() { code64::SYMS } else { code::SYMS };
    syms.iter().find(|(n, _)| *n == name).map(|&(_, a)| a)
}

/// x64: the kernel32 / ntdll exports the guest code has fast paths of (and their Rtl names).
fn x64_fast(key: &str, name: &str) -> Option<&'static str> {
    if !matches!(key, "kernel32" | "kernelbase" | "ntdll") {
        return None;
    }
    Some(match name {
        "EnterCriticalSection" | "RtlEnterCriticalSection" => "EnterCriticalSection",
        "LeaveCriticalSection" | "RtlLeaveCriticalSection" => "LeaveCriticalSection",
        "GetLastError" | "RtlGetLastWin32Error" => "GetLastError",
        "SetLastError" | "RtlSetLastWin32Error" => "SetLastError",
        "TlsGetValue" => "TlsGetValue",
        "HeapAlloc" | "RtlAllocateHeap" => "HeapAlloc",
        "HeapFree" | "RtlFreeHeap" => "HeapFree",
        _ => return None,
    })
}

/// The guest code for C library export `name` of DLL `key` (dll_key), if there is one.
pub fn lookup(key: &str, name: &str) -> Option<u32> {
    if !PLACED.load(Ordering::Relaxed) || name.starts_with("host_") || name == "slab_table" {
        return None;
    }
    if mem::x64() {
        if let Some(n) = x64_fast(key, name) {
            return sym(n);
        }
    } else if matches!(name, "EnterCriticalSection" | "LeaveCriticalSection" | "GetLastError" | "SetLastError" | "TlsGetValue" | "HeapAlloc" | "HeapFree" | "malloc" | "free" | "_errno") {
        return None;
    }
    let crt = matches!(key, "msvcrt" | "ucrtbase" | "crtdll") || (key.starts_with("msvcr") && key[5..].bytes().all(|b| b.is_ascii_digit()));
    if !crt {
        return None;
    }
    sym(name)
}
