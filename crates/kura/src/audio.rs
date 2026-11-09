//! The sound card behind sake's audio APIs (waveOut, DirectSound, XAudio2).
//!
//! Each render stream queues its frames here. The wall clock plays them: on
//! every call the elapsed time is mixed (44.1 kHz stereo f32) into `out`, which the
//! page reads from the ring (`vnhost_audio_ring`) into WebAudio. Under node nobody drains it and
//! `out` just stays capped, so the timing games wait on is the same in both.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// the driver's mix format too: most VN audio is 44.1 kHz, which dsound then mixes without
/// its FIR resampler (costly in x87 code); the browser converts to the device's rate
pub const RATE: u32 = 44100;
/// frames kept for the page at most (older ones are dropped)
const OUT_CAP: usize = RATE as usize / 4;

struct Stream {
    /// stereo frames at RATE
    q: VecDeque<[f32; 2]>,
}

struct Card {
    streams: HashMap<i32, Stream>,
    next_id: i32,
    /// time up to which `out` has been mixed
    clock: Instant,
    out: VecDeque<[f32; 2]>,
}

static CARD: Mutex<Option<Card>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Card) -> T) -> T {
    let mut g = CARD.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(|| Card { streams: HashMap::new(), next_id: 1, clock: Instant::now(), out: VecDeque::new() });
    c.advance();
    f(c)
}

impl Card {
    /// Mixes the time elapsed since `clock`.
    fn advance(&mut self) {
        let now = Instant::now();
        let n = (now.duration_since(self.clock).as_secs_f64() * RATE as f64) as u64;
        if n == 0 {
            return;
        }
        self.clock += std::time::Duration::from_nanos(n * 1_000_000_000 / RATE as u64);
        if self.streams.values().all(|s| s.q.is_empty()) {
            return;
        }
        // a long stall (a hung page, a debugger) doesn't replay seconds at once
        let n = n.min(RATE as u64) as usize;
        let start = self.out.len();
        self.out.resize(start + n, [0.0; 2]);
        for s in self.streams.values_mut() {
            let k = s.q.len().min(n);
            for (o, f) in self.out.range_mut(start..).zip(s.q.drain(..k)) {
                o[0] += f[0];
                o[1] += f[1];
            }
        }
        if RING_ON.load(Ordering::Relaxed) {
            ring_put(&mut self.out);
        }
        if self.out.len() > OUT_CAP {
            let extra = self.out.len() - OUT_CAP;
            self.out.drain(..extra);
        }
    }
}

/// A new stream of stereo f32 frames at RATE.
pub fn open() -> i32 {
    // (always the first: in every run's log, whether the game opened any sound at all)
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| eprintln!("sake: audio: the first sound stream opened (the page plays the ring: {})", RING_ON.load(Ordering::Relaxed)));
    if stats() {
        eprintln!("vnvm audio: open");
    }
    with(|c| {
        let id = c.next_id;
        c.next_id += 1;
        c.streams.insert(id, Stream { q: VecDeque::new() });
        id
    })
}

pub fn close(id: i32) {
    with(|c| c.streams.remove(&id));
}

/// Frames of stream `id` not played yet.
pub fn queued(id: i32) -> u32 {
    with(|c| c.streams.get(&id).map_or(0, |s| s.q.len() as u32))
}

/// Queues interleaved stereo frames.
pub fn write(id: i32, frames: &[f32]) {
    with(|c| {
        let Some(s) = c.streams.get_mut(&id) else { return };
        let before = s.q.len();
        s.q.extend(frames.chunks_exact(2).map(|f| [f[0], f[1]]));
        if stats() {
            let (sum, n) = s.q.range(before..).fold((0f64, 0usize), |(a, n), f| (a + (f[0] * f[0]) as f64, n + 1));
            static ACC: Mutex<(f64, usize)> = Mutex::new((0.0, 0));
            let mut acc = ACC.lock().unwrap_or_else(|e| e.into_inner());
            acc.0 += sum;
            acc.1 += n;
            if acc.1 >= RATE as usize * 5 {
                eprintln!("vnvm audio: rms {:.4} over {} frames", (acc.0 / acc.1 as f64).sqrt(), acc.1);
                *acc = (0.0, 0);
            }
        }
    })
}

/// IPPAI_AUDIO_STATS=1: the level of what the streams write, every 5 s of frames
fn stats() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("IPPAI_AUDIO_STATS").is_some())
}

/// Frames in the ring the page's AudioWorklet reads (a power of two; 0.74 s).
const RING_FRAMES: usize = 1 << 15;

/// What the page plays: mixed frames (interleaved stereo f32) at `head % RING_FRAMES`, `head`
/// counting every frame written. The AudioWorklet reads it straight from the shared wasm memory
/// with its own read position (web/src/audio.ts), so nothing goes through the page's own
/// thread, where a long frame would make the sound stutter.
#[repr(C)]
struct Ring {
    head: AtomicU32,
    _pad: u32,
    data: std::cell::UnsafeCell<[f32; RING_FRAMES * 2]>,
}
unsafe impl Sync for Ring {}

static RING: Ring = Ring { head: AtomicU32::new(0), _pad: 0, data: std::cell::UnsafeCell::new([0.0; RING_FRAMES * 2]) };
static RING_ON: AtomicBool = AtomicBool::new(false);

fn ring_put(out: &mut VecDeque<[f32; 2]>) {
    let head = RING.head.load(Ordering::Relaxed);
    let data = unsafe { &mut *RING.data.get() };
    let n = out.len() as u32;
    for (i, f) in out.drain(..).enumerate() {
        let at = (head.wrapping_add(i as u32) as usize & (RING_FRAMES - 1)) * 2;
        data[at] = f[0];
        data[at + 1] = f[1];
    }
    // (after the frames: the worklet reads up to head)
    RING.head.store(head.wrapping_add(n), Ordering::Release);
}

/// The page: from now on mixed frames go to the ring; returns its address (head at +0, frames
/// from +8) and writes its size in frames to `frames`.
#[no_mangle]
pub unsafe extern "C" fn vnhost_audio_ring(frames: *mut u32) -> *const u8 {
    *frames = RING_FRAMES as u32;
    RING_ON.store(true, Ordering::Relaxed);
    &RING as *const Ring as *const u8
}
