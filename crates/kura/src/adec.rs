//! Sound the page decodes with WebCodecs' AudioDecoder (web/src/adec.ts), for codecs sake has no
//! decoder of its own for: MP3 through msacm32 (crates/sake/src/acm.rs). Vorbis stays native
//! (crates/vorbis: less CPU than AudioDecoder, and sake takes over games' own libvorbis at a step
//! AudioDecoder has no equivalent of; tests/audiodec measures both).
//!
//! The guest opens a stream (a WebCodecs codec string, rate, channels, description), pushes its
//! chunks and waits (`decode`) until the page has decoded every chunk pushed; the page lists the
//! streams when the doorbell rings (`vnhost_adec_list`), feeds the chunks it has not fed yet to an
//! AudioDecoder (`vnhost_adec_chunk`), hands back the samples of each output (`vnhost_adec_pcm`,
//! interleaved f32) and says when it has nothing left in flight (`vnhost_adec_done`).
//!
//! AudioDecoder.flush() would say that too, but it ends the stream: the next chunk decodes
//! without what came before (an MP3 frame's bit reservoir, Vorbis's overlap), so the page counts
//! outputs instead (see web/src/adec.ts).
//!
//! `reset` starts a stream again (a seek): chunks and samples still here go, and the page makes a
//! new decoder; what the old one still hands back is dropped (by generation).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

struct Stream {
    /// the codec string, then the description (as the page reads them: stay put while listed)
    codec: Vec<u8>,
    desc: Vec<u8>,
    rate: u32,
    channels: u32,
    /// bumped by `reset`: the page's decoder for an older one is thrown away
    gen: u32,
    /// chunks not taken by the page yet (their sequence numbers follow `taken`)
    queue: VecDeque<Vec<u8>>,
    /// chunks pushed, and chunks the page has decoded (every output of them handed back)
    pushed: u64,
    taken: u64,
    done: u64,
    /// samples handed back, interleaved (`channels` per frame)
    pcm: VecDeque<f32>,
    /// the page cannot decode it
    failed: bool,
    /// closed by the guest: dropped on the page's next list (the page reads `codec`/`desc` only
    /// right after listing, on its own thread)
    closed: bool,
}

static STREAMS: Mutex<Option<HashMap<u32, Stream>>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();
/// The guest adds 1 and wakes the page (Atomics.waitAsync) when there is something new for it.
static DOORBELL: AtomicI32 = AtomicI32::new(0);
/// A page has listed streams: there is a page to decode.
static PAGE: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicU32 = AtomicU32::new(1);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Stream>) -> R) -> R {
    let mut g = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn ring() {
    DOORBELL.fetch_add(1, Ordering::SeqCst);
    #[cfg(target_os = "emscripten")]
    unsafe {
        extern "C" {
            fn emscripten_futex_wake(addr: *const std::ffi::c_void, count: i32) -> i32;
        }
        emscripten_futex_wake(DOORBELL.as_ptr() as *const std::ffi::c_void, i32::MAX);
    }
}

/// Whether a page decodes (none under node: sake says it has no such codec, as before). The page
/// says so in the environment (IPPAI_ADEC=1: the guest starts before the page first lists
/// streams), or by listing them. IPPAI_ADEC_OFF: none.
pub fn available() -> bool {
    (PAGE.load(Ordering::Relaxed) || crate::env_on!("IPPAI_ADEC")) && !crate::env_on!("IPPAI_ADEC_OFF")
}

/// A stream of `codec` (WebCodecs' name: "mp3", "opus", "mp4a.40.2" ...); None without a page.
pub fn open(codec: &str, rate: u32, channels: u32, desc: &[u8]) -> Option<u32> {
    if !available() || channels == 0 || rate == 0 {
        return None;
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let s = Stream {
        codec: codec.as_bytes().to_vec(),
        desc: desc.to_vec(),
        rate,
        channels,
        gen: 0,
        queue: VecDeque::new(),
        pushed: 0,
        taken: 0,
        done: 0,
        pcm: VecDeque::new(),
        failed: false,
        closed: false,
    };
    with(|m| m.insert(id, s));
    ring();
    Some(id)
}

pub fn close(id: u32) {
    with(|m| {
        if let Some(s) = m.get_mut(&id) {
            s.closed = true;
            s.queue.clear();
            s.pcm.clear();
        }
    });
    WAKE.notify_all();
    ring();
}

/// Starts stream `id` again: nothing of what was pushed before comes out.
pub fn reset(id: u32) {
    with(|m| {
        if let Some(s) = m.get_mut(&id) {
            s.gen = s.gen.wrapping_add(1);
            s.queue.clear();
            s.pcm.clear();
            s.pushed = 0;
            s.taken = 0;
            s.done = 0;
        }
    });
    ring();
}

/// Whether the page gave up on stream `id` (no decoder for its codec, or a decoding error).
pub fn failed(id: u32) -> bool {
    with(|m| m.get(&id).is_none_or(|s| s.failed))
}

/// Pushes chunks (one encoded frame each) of stream `id`.
pub fn push<'a>(id: u32, chunks: impl IntoIterator<Item = &'a [u8]>) {
    let any = with(|m| {
        let Some(s) = m.get_mut(&id).filter(|s| !s.closed && !s.failed) else { return false };
        let n0 = s.pushed;
        for c in chunks {
            s.queue.push_back(c.to_vec());
            s.pushed += 1;
        }
        s.pushed > n0
    });
    if any {
        ring();
    }
}

/// Waits (up to `timeout`) until the page has decoded every chunk pushed to stream `id`; false
/// when it did not in time (or the stream failed). What was decoded is there to `take` either way.
pub fn wait(id: u32, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    let mut g = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let Some(s) = g.get_or_insert_with(HashMap::new).get(&id) else { return false };
        if s.failed || s.closed {
            return false;
        }
        if s.done >= s.pushed {
            return true;
        }
        let now = Instant::now();
        if now >= end {
            return false;
        }
        // (in slices: a page woken late still gets its doorbell)
        g = WAKE.wait_timeout(g, (end - now).min(Duration::from_millis(5))).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// Moves up to `max` decoded samples (interleaved floats) of stream `id` to `out`; how many.
pub fn take(id: u32, max: usize, out: &mut Vec<f32>) -> usize {
    with(|m| {
        let Some(s) = m.get_mut(&id) else { return 0 };
        let n = max.min(s.pcm.len());
        out.extend(s.pcm.drain(..n));
        n
    })
}

/// Pushes `chunks`, waits for them, and moves every decoded sample to `out`: the samples
/// (interleaved), or None when the page failed or did not answer in `timeout`.
pub fn decode<'a>(id: u32, chunks: impl IntoIterator<Item = &'a [u8]>, timeout: Duration, out: &mut Vec<f32>) -> Option<usize> {
    push(id, chunks);
    let ok = wait(id, timeout);
    let n = take(id, usize::MAX, out);
    ok.then_some(n)
}

// ---------------------------------------------------------------------------------------------
// the page

/// The doorbell's address (the page waits on it with Atomics.waitAsync).
#[no_mangle]
pub extern "C" fn vnhost_adec_doorbell() -> u32 {
    DOORBELL.as_ptr() as u32
}

/// Lists up to `max` open streams into `out`, 10 words each: id, generation, codec string ptr
/// and bytes, rate, channels, description ptr and bytes, chunks waiting for the page, whether it
/// failed. Closed streams go first (the page drops its decoders for ids it no longer sees).
///
/// # Safety
/// `out` must hold `max * 10` words.
#[no_mangle]
pub unsafe extern "C" fn vnhost_adec_list(out: *mut u32, max: u32) -> u32 {
    let out = std::slice::from_raw_parts_mut(out, max as usize * 10);
    let rows = rows(max as usize);
    for (n, r) in rows.iter().enumerate() {
        out[n * 10..n * 10 + 10].copy_from_slice(&[
            r.id,
            r.gen,
            r.codec.0 as u32,
            r.codec.1 as u32,
            r.rate,
            r.channels,
            r.desc.0 as u32,
            r.desc.1 as u32,
            r.queued,
            r.failed as u32,
        ]);
    }
    rows.len() as u32
}

/// A stream as the page lists it (address and length of its codec string and description).
struct Row {
    id: u32,
    gen: u32,
    codec: (usize, usize),
    rate: u32,
    channels: u32,
    desc: (usize, usize),
    queued: u32,
    failed: bool,
}

fn rows(max: usize) -> Vec<Row> {
    PAGE.store(true, Ordering::Relaxed);
    with(|m| {
        m.retain(|_, s| !s.closed);
        m.iter()
            .take(max)
            .map(|(&id, s)| Row {
                id,
                gen: s.gen,
                codec: (s.codec.as_ptr() as usize, s.codec.len()),
                rate: s.rate,
                channels: s.channels,
                desc: (s.desc.as_ptr() as usize, s.desc.len()),
                queued: s.queue.len() as u32,
                failed: s.failed,
            })
            .collect()
    })
}

/// The page's copy of a chunk (only the page's thread uses it).
static CHUNK: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// The next chunk of stream `id` (generation `gen`) for the page to decode: its bytes' address
/// (`*size` bytes, copied for the page) and its sequence number in `*seq`; 0 when there is none.
///
/// # Safety
/// `size` and `seq` must be writable.
#[no_mangle]
pub unsafe extern "C" fn vnhost_adec_chunk(id: u32, gen: u32, size: *mut u32, seq: *mut u32) -> u32 {
    let Some((b, n)) = next_chunk(id, gen) else { return 0 };
    let mut c = CHUNK.lock().unwrap_or_else(|e| e.into_inner());
    *c = b;
    *size = c.len() as u32;
    *seq = n;
    c.reserve(1);
    c.as_ptr() as u32
}

/// The next chunk of stream `id` (generation `gen`) and its sequence number.
fn next_chunk(id: u32, gen: u32) -> Option<(Vec<u8>, u32)> {
    with(|m| {
        let s = m.get_mut(&id).filter(|s| s.gen == gen && !s.closed)?;
        let b = s.queue.pop_front()?;
        s.taken += 1;
        Some((b, s.taken as u32))
    })
}

/// Samples the page decoded for stream `id` (generation `gen`): `frames` frames of `channels`
/// interleaved floats at `pcm`. Another channel count than the stream's is mixed to it (mono
/// copied; more channels: the first ones).
///
/// # Safety
/// `pcm` must hold `frames * channels` floats.
#[no_mangle]
pub unsafe extern "C" fn vnhost_adec_pcm(id: u32, gen: u32, pcm: *const f32, frames: u32, channels: u32) {
    if channels == 0 {
        return;
    }
    let src = std::slice::from_raw_parts(pcm, frames as usize * channels as usize);
    with(|m| {
        let Some(s) = m.get_mut(&id).filter(|s| s.gen == gen && !s.closed) else { return };
        put(&mut s.pcm, src, channels as usize, s.channels as usize);
    });
}

/// `src` (frames of `from` channels) as frames of `to` channels onto `dst`.
fn put(dst: &mut VecDeque<f32>, src: &[f32], from: usize, to: usize) {
    if from == to {
        dst.extend(src);
        return;
    }
    for f in src.chunks_exact(from) {
        for c in 0..to {
            dst.push_back(if from == 1 { f[0] } else { f[c.min(from - 1)] });
        }
    }
}

/// Every chunk of stream `id` (generation `gen`) up to sequence number `seq` is decoded and its
/// samples handed back.
#[no_mangle]
pub extern "C" fn vnhost_adec_done(id: u32, gen: u32, seq: u32) {
    with(|m| {
        if let Some(s) = m.get_mut(&id).filter(|s| s.gen == gen) {
            s.done = s.done.max((seq as u64).min(s.taken));
        }
    });
    WAKE.notify_all();
}

/// The page cannot decode stream `id` (generation `gen`): its waits end, and it takes no chunks.
#[no_mangle]
pub extern "C" fn vnhost_adec_fail(id: u32, gen: u32) {
    with(|m| {
        if let Some(s) = m.get_mut(&id).filter(|s| s.gen == gen) {
            s.failed = true;
            s.queue.clear();
        }
    });
    WAKE.notify_all();
}

/// A page for other crates' tests (no browser): one pass over the streams, every waiting chunk
/// decoded by `dec` (the chunk's bytes, the stream's channels -> interleaved samples) and done.
#[doc(hidden)]
pub fn test_page_pass(dec: &mut dyn FnMut(&[u8], u32) -> Vec<f32>) {
    for r in rows(usize::MAX) {
        let mut last = 0;
        while let Some((b, seq)) = next_chunk(r.id, r.gen) {
            let pcm = dec(&b, r.channels);
            unsafe { vnhost_adec_pcm(r.id, r.gen, pcm.as_ptr(), (pcm.len() / r.channels.max(1) as usize) as u32, r.channels) };
            last = seq;
        }
        if last > 0 {
            vnhost_adec_done(r.id, r.gen, last);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Tests share the streams and the page flag: one at a time.
    pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

    pub(crate) fn page_on() -> std::sync::MutexGuard<'static, ()> {
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        PAGE.store(true, Ordering::Relaxed);
        g
    }

    // (the page's exports hand out 32-bit addresses: wasm's; here the same through usize)
    fn list() -> Vec<Row> {
        rows(64)
    }

    fn chunk(id: u32, gen: u32) -> Option<(Vec<u8>, u32)> {
        next_chunk(id, gen)
    }

    /// A page in a thread: takes every chunk, "decodes" chunk k (its bytes b) to b.len() frames
    /// of `ch` channels holding b[i] / 256 + k, says done once per batch; stops with `stop`.
    pub(crate) fn fake_page(ch: u32, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                for row in list() {
                    let (id, gen) = (row.id, row.gen);
                    let mut last = 0;
                    while let Some((b, seq)) = chunk(id, gen) {
                        let pcm: Vec<f32> = b.iter().flat_map(|&v| (0..ch).map(move |_| v as f32 / 256.0 + seq as f32)).collect();
                        unsafe { vnhost_adec_pcm(id, gen, pcm.as_ptr(), b.len() as u32, ch) };
                        last = seq;
                    }
                    if last > 0 {
                        vnhost_adec_done(id, gen, last);
                    }
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        })
    }

    #[test]
    fn no_page_no_stream() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        PAGE.store(false, Ordering::Relaxed);
        assert_eq!(open("mp3", 44100, 2, &[]), None);
        assert_eq!(open("mp3", 0, 2, &[]), None);
        PAGE.store(true, Ordering::Relaxed);
        assert_eq!(open("mp3", 44100, 0, &[]), None);
        let id = open("mp3", 44100, 2, &[]).unwrap();
        close(id);
    }

    #[test]
    fn list_shows_what_the_page_reads() {
        let _g = page_on();
        let id = open("opus", 48000, 2, &[1, 2, 3]).unwrap();
        push(id, [&[9u8, 9][..], &[8][..]]);
        let row = list().into_iter().find(|r| r.id == id).unwrap();
        let codec = unsafe { std::slice::from_raw_parts(row.codec.0 as *const u8, row.codec.1) };
        let desc = unsafe { std::slice::from_raw_parts(row.desc.0 as *const u8, row.desc.1) };
        assert_eq!((codec, desc, row.rate, row.channels, row.queued, row.failed), (&b"opus"[..], &[1u8, 2, 3][..], 48000, 2, 2, false));
        assert_eq!(chunk(id, row.gen), Some((vec![9, 9], 1)));
        assert_eq!(chunk(id, row.gen + 1), None, "another generation's page takes nothing");
        assert_eq!(chunk(id, row.gen), Some((vec![8], 2)));
        assert_eq!(chunk(id, row.gen), None);
        close(id);
        assert!(list().iter().all(|r| r.id != id), "a closed stream is not listed");
        assert_eq!(chunk(id, 0), None);
    }

    #[test]
    fn waits_for_the_page() {
        let _g = page_on();
        let stop = Arc::new(AtomicBool::new(false));
        let page = fake_page(2, stop.clone());
        let id = open("mp3", 44100, 2, &[]).unwrap();
        let mut out = Vec::new();
        assert_eq!(decode(id, [&[0u8, 128][..], &[64][..]], Duration::from_secs(5), &mut out), Some(6));
        assert_eq!(out, [1.0, 1.0, 1.5, 1.5, 2.25, 2.25]);
        // a reset: the old chunks' numbering starts again
        reset(id);
        out.clear();
        assert_eq!(decode(id, [&[0u8][..]], Duration::from_secs(5), &mut out), Some(2));
        assert_eq!(out, [1.0, 1.0]);
        stop.store(true, Ordering::Relaxed);
        page.join().unwrap();
        close(id);
    }

    #[test]
    fn no_answer_times_out() {
        let _g = page_on();
        let id = open("mp3", 44100, 1, &[]).unwrap();
        let t = Instant::now();
        let mut out = Vec::new();
        assert_eq!(decode(id, [&[1u8][..]], Duration::from_millis(30), &mut out), None);
        assert!(t.elapsed() >= Duration::from_millis(30));
        // a failed stream ends its waits at once and takes nothing more
        vnhost_adec_fail(id, 0);
        assert!(failed(id));
        let t = Instant::now();
        assert!(!wait(id, Duration::from_secs(5)));
        assert!(t.elapsed() < Duration::from_secs(1));
        push(id, [&[1u8][..]]);
        assert_eq!(chunk(id, 0), None);
        close(id);
        assert!(failed(id), "a stream not there counts as failed");
    }

    #[test]
    fn old_generations_are_dropped() {
        let _g = page_on();
        let id = open("mp3", 44100, 1, &[]).unwrap();
        push(id, [&[1u8][..]]);
        assert!(chunk(id, 0).is_some());
        reset(id);
        // what the old decoder hands back late, and its done, count for nothing
        unsafe { vnhost_adec_pcm(id, 0, [0.5f32].as_ptr(), 1, 1) };
        vnhost_adec_done(id, 0, 1);
        push(id, [&[2u8][..]]);
        assert!(!wait(id, Duration::from_millis(10)));
        let mut out = Vec::new();
        assert_eq!(take(id, 99, &mut out), 0);
        // the page can't say more is done than it took
        vnhost_adec_done(id, 1, 7);
        assert!(!wait(id, Duration::from_millis(10)));
        close(id);
    }

    #[test]
    fn channels_fitted() {
        let mut d = VecDeque::new();
        put(&mut d, &[1.0, 2.0], 1, 2);
        put(&mut d, &[3.0, 4.0, 5.0], 3, 2);
        put(&mut d, &[6.0, 7.0], 2, 1);
        assert_eq!(Vec::from(d), [1.0, 1.0, 2.0, 2.0, 3.0, 4.0, 6.0]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Any chunks in any batches, against a page in its own thread: every sample comes back,
        /// in order, each batch's once its wait ends.
        #[test]
        fn batches_in_order(batches in proptest::collection::vec(proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..6), 0..5), 1..6), ch in 1u32..3) {
            let _g = page_on();
            let stop = Arc::new(AtomicBool::new(false));
            let page = fake_page(ch, stop.clone());
            let id = open("mp3", 22050, ch, &[]).unwrap();
            let mut seq = 0u32;
            for b in &batches {
                let mut out = Vec::new();
                let got = decode(id, b.iter().map(|c| &c[..]), Duration::from_secs(10), &mut out);
                let mut want = Vec::new();
                for c in b {
                    seq += 1;
                    want.extend(c.iter().flat_map(|&v| (0..ch).map(move |_| v as f32 / 256.0 + seq as f32)));
                }
                prop_assert_eq!(got, Some(want.len()));
                prop_assert_eq!(out, want);
            }
            stop.store(true, Ordering::Relaxed);
            page.join().unwrap();
            close(id);
        }
    }
}
