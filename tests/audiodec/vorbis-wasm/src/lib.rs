//! crates/vorbis's whole-stream decoder for the page (tests/audiodec): packets in, interleaved
//! f32 out, as kura would decode a movie's sound in the browser.

use std::cell::RefCell;

thread_local! {
    static DEC: RefCell<Option<vorbis::Decoder>> = const { RefCell::new(None) };
    static OUT: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
}

#[no_mangle]
pub extern "C" fn alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `p` came from `alloc(n)`.
#[no_mangle]
pub unsafe extern "C" fn free(p: *mut u8, n: usize) {
    drop(Vec::from_raw_parts(p, 0, n.max(1)));
}

/// Starts a stream from its identification and setup headers: its channels, or 0.
///
/// # Safety
/// The pointers hold the lengths given.
#[no_mangle]
pub unsafe extern "C" fn open(id: *const u8, id_len: usize, setup: *const u8, setup_len: usize) -> u32 {
    let d = vorbis::Decoder::new(std::slice::from_raw_parts(id, id_len), std::slice::from_raw_parts(setup, setup_len)).ok();
    let ch = d.as_ref().map_or(0, |d| d.setup.ident.channels as u32);
    DEC.with(|c| *c.borrow_mut() = d);
    OUT.with(|o| o.borrow_mut().clear());
    ch
}

/// Decodes one audio packet; its samples are appended to the output.
///
/// # Safety
/// `p` holds `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn packet(p: *const u8, n: usize) {
    let pk = std::slice::from_raw_parts(p, n);
    DEC.with(|c| {
        if let Some(d) = c.borrow_mut().as_mut() {
            OUT.with(|o| d.packet(pk, &mut o.borrow_mut()));
        }
    });
}

/// The output so far (interleaved floats): its address; `out_len` its length in floats.
#[no_mangle]
pub extern "C" fn out_ptr() -> *const f32 {
    OUT.with(|o| o.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn out_len() -> usize {
    OUT.with(|o| o.borrow().len())
}

#[no_mangle]
pub extern "C" fn out_clear() {
    OUT.with(|o| o.borrow_mut().clear());
}
