//! Host runtime for running Wine's unix side as wasm32 (Hangover-style vnvm).
//!
//! * `fd`  – in-process pipes and unix sockets shared by the Wine client and wineserver
//! * `mem` – mmap for one linear memory with the guest address space below 2 GiB
//! * `dl`  – dlopen/dlsym over unix libraries that are statically linked in
//! * `cpu` – the x86 run loop the signal layer drives
//! * `crashlog` – what a run did lately (guest exceptions, stderr, threads), for the page's crash reports
//! * `proc` – Wine processes: address partitions, environments, exit
//! * `vfs` – the browser's file system: packed trees read lazily from the chunk store
//! * `display` – the screen behind Wine's browser display driver
//! * `wrap` – the libc entry points (`-Wl,--wrap=...`) that route to the above
#![cfg_attr(target_arch = "wasm64", feature(simd_wasm64))]

/// Whether environment variable `$name` is set, read once (std::env::var takes a lock and walks
/// the environment on every call).
#[macro_export]
macro_rules! env_on {
    ($name:literal) => {{
        static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *V.get_or_init(|| std::env::var_os($name).is_some())
    }};
}


pub mod audio;
pub mod d3d9;
pub mod d3d11;
pub mod gl;
pub mod cpu;
pub mod crashlog;
pub mod jit;
pub mod display;
pub mod dl;
pub mod fd;
pub mod mem;
pub mod proc;
pub mod vfs;
mod sys;
mod wrap;

pub(crate) fn set_errno(e: libc::c_int) {
    unsafe { *libc::__errno_location() = e };
}

pub(crate) fn errno() -> libc::c_int {
    unsafe { *libc::__errno_location() }
}
