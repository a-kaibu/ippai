//! The host's buffer that its processes share the kernel in (host/vnkern.js): a lock, the
//! generation (what waiting is on), the state's bytes, and the list of processes that ended
//! (the host adds a process to it when its instance exits, whatever ended it).

use super::enc::Enc;
use super::Kern;

#[cfg(target_os = "emscripten")]
mod sys {
    extern "C" {
        pub fn vnhost_kern_shared() -> i32;
        pub fn vnhost_kern_can_share() -> i32;
        pub fn vnhost_kern_start_sharing();
        pub fn vnhost_kern_lock();
        pub fn vnhost_kern_unlock();
        pub fn vnhost_kern_get(dst: *mut u8, cap: usize) -> usize;
        pub fn vnhost_kern_put(src: *const u8, len: usize) -> i32;
        pub fn vnhost_kern_gen() -> u32;
        pub fn vnhost_kern_bump() -> u32;
        pub fn vnhost_kern_notify();
        pub fn vnhost_kern_wait(seen: u32, ms: f64) -> i32;
        pub fn vnhost_kern_dead(i: u32, out: *mut u32) -> i32;
        pub fn vnhost_spawn(pid: u32, exe: *const u8, len: usize) -> i32;
    }
}

/// Whether the kernel is in the shared buffer (this process started another, or was started).
pub fn shared() -> bool {
    #[cfg(target_os = "emscripten")]
    return unsafe { sys::vnhost_kern_shared() != 0 };
    #[cfg(not(target_os = "emscripten"))]
    false
}
/// Whether the host has a shared buffer and can start processes.
pub fn can_share() -> bool {
    #[cfg(target_os = "emscripten")]
    return unsafe { sys::vnhost_kern_can_share() != 0 };
    #[cfg(not(target_os = "emscripten"))]
    false
}
pub fn start_sharing() {
    #[cfg(target_os = "emscripten")]
    unsafe {
        sys::vnhost_kern_start_sharing()
    }
}
pub fn lock() {
    #[cfg(target_os = "emscripten")]
    unsafe {
        sys::vnhost_kern_lock()
    }
}
pub fn unlock() {
    #[cfg(target_os = "emscripten")]
    unsafe {
        sys::vnhost_kern_unlock()
    }
}
pub fn gen() -> u32 {
    #[cfg(target_os = "emscripten")]
    return unsafe { sys::vnhost_kern_gen() };
    #[cfg(not(target_os = "emscripten"))]
    0
}
/// A new generation (after a store): its number.
pub fn bump() -> u32 {
    #[cfg(target_os = "emscripten")]
    return unsafe { sys::vnhost_kern_bump() };
    #[cfg(not(target_os = "emscripten"))]
    0
}
pub fn notify() {
    #[cfg(target_os = "emscripten")]
    unsafe {
        sys::vnhost_kern_notify()
    }
}
/// Waits while the generation is `seen`, at most `ms`.
pub fn wait(seen: u32, ms: f64) {
    #[cfg(target_os = "emscripten")]
    unsafe {
        sys::vnhost_kern_wait(seen, ms);
    }
    let _ = (seen, ms);
}
/// The state as the shared buffer has it (None if it has none yet).
pub fn load() -> Option<Kern> {
    #[cfg(target_os = "emscripten")]
    {
        let n = unsafe { sys::vnhost_kern_get(std::ptr::null_mut(), 0) };
        if n == 0 {
            return None;
        }
        let mut v = vec![0u8; n];
        unsafe { sys::vnhost_kern_get(v.as_mut_ptr(), n) };
        let k = Kern::get(&mut v.as_slice());
        assert!(k.is_some(), "sake: the shared kernel state does not decode");
        k
    }
    #[cfg(not(target_os = "emscripten"))]
    None
}
pub fn store(k: &Kern) {
    let mut v = Vec::new();
    k.put(&mut v);
    #[cfg(target_os = "emscripten")]
    {
        let ok = unsafe { sys::vnhost_kern_put(v.as_ptr(), v.len()) };
        assert!(ok != 0, "sake: the shared kernel state outgrew its buffer ({} bytes)", v.len());
    }
}
/// The host's `i`th ended process: (id, exit code).
pub fn dead(i: u32) -> Option<(u32, u32)> {
    #[cfg(target_os = "emscripten")]
    {
        let mut out = [0u32; 2];
        if unsafe { sys::vnhost_kern_dead(i, out.as_mut_ptr()) } != 0 {
            return Some((out[0], out[1]));
        }
    }
    let _ = i;
    None
}
/// Asks the host to start process `pid` from host file `exe` (its record is in the kernel).
pub fn spawn(pid: u32, exe: &str) -> bool {
    #[cfg(target_os = "emscripten")]
    {
        // (the child sees the files this process wrote, the exe the launcher made too)
        kura::vfs::share();
        return unsafe { sys::vnhost_spawn(pid, exe.as_ptr(), exe.len()) != 0 };
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        let _ = (pid, exe);
        false
    }
}
