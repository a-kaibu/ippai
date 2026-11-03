//! Host runtime of sake in wasm32: the x86 run loop and JIT, the page's file system, display,
//! audio, video and drawing.
//!
//! * `cpu` – the x86 run loop
//! * `crashlog` – what a run did lately (guest exceptions, stderr, threads), for the page's crash reports
//! * `vfs` / `fs` – the browser's file system: packed trees read lazily from the chunk store
//! * `display` – the screen and the page's input
//! * `render` – the thread that draws (WebGPU)
//! * `wrap` – the few libc entry points (`-Wl,--wrap=...`) vnhost takes over
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
pub mod video;
pub mod d3d9;
pub mod d3d11;
pub mod gl;
pub mod cpu;
pub mod crashlog;
pub mod jit;
pub mod display;
pub mod render;
pub mod vfs;
pub mod fs;
mod wrap;
