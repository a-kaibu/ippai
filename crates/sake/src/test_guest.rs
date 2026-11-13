//! Test-only guest memory and x86 argument frames; never linked into the runtime.
use crate::{api::{Ctx, Handler}, mem};
use std::sync::{Mutex, MutexGuard, Once};
static SERIAL: Mutex<()> = Mutex::new(());
pub fn enter() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    static MAP: Once = Once::new();
    MAP.call_once(|| unsafe {
        let p = libc::mmap(mem::LO as usize as *mut _, (mem::HI - mem::LO) as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE | libc::MAP_NORESERVE, -1, 0);
        assert_eq!(p as usize, mem::LO as usize, "guest memory: {}", std::io::Error::last_os_error());
    });
    assert_eq!(mem::ps(), 4);
    guard
}
pub fn call(f: Handler, args: &[u32]) -> u32 {
    let sp = mem::heap_alloc(4 + 4 * args.len() as u32, true);
    for (i, &a) in args.iter().enumerate() { mem::w32(sp + 4 + 4 * i as u32, a); }
    let mut cpu = koji::Cpu::default();
    let result = f(&mut Ctx { cpu: &mut cpu, esp: sp, edx: None, jumped: false, st0: None, trap: 0, this: None });
    mem::heap_free(sp);
    result
}
/// Makes the calling test thread a guest thread (a TEB: last_error, TLS), once per host thread.
pub fn guest_thread() {
    thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if !DONE.with(|d| d.replace(true)) {
        crate::enter(0);
    }
}
