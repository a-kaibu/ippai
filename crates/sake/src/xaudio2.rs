//! XAudio2 2.7 (CoCreateInstance CLSID_XAudio2, xaudio2_7's DllGetClassObject) and 2.8 / 2.9
//! (xaudio2_8 / xaudio2_9's XAudio2Create: no device enumeration or Initialize, the mastering
//! voice takes a device id and a stream category, voice details have ActiveFlags, GetState takes
//! flags, the mastering voice has GetChannelMask): source voices play queued XAUDIO2_BUFFERs
//! from guest memory, through submix voices' volumes, into a mastering voice that is a stream
//! of kura's audio output. The engine is one guest thread: each 10 ms pass it calls the
//! voices' OnVoiceProcessingPassStart, mixes, delivers OnBufferStart / OnBufferEnd /
//! OnLoopEnd / OnStreamEnd, then OnVoiceProcessingPassEnd (as XAudio2 calls them from its
//! own thread). Voices are mixed as a graph: each source voice renders its own buffer, runs
//! its effect chain and volume, and adds into the voice it sends to; submix voices (deepest
//! first) and then the mastering voice do the same with what they received. Effect chains
//! hold XAPOs: sake's own (xapo.rs: EQ, limiter, echo, reverbs, meter) run directly; a game's
//! own XAPO is locked for 32-bit float stereo and its Process called on the engine thread.
//! Voices are processed newest first, each one's OnVoiceProcessingPassStart, buffer
//! callbacks and OnVoiceProcessingPassEnd together (as 2.7 does). No filters or output
//! matrices; PCM and float formats only. x64: the same, with pointer-sized fields and the
//! float arguments in XMM registers.

#![allow(non_snake_case)]

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::api::{host_fn, Ctx};
use crate::mem::{self, ps, put32, putp, r16, r32, rp, w16, w32};
use crate::sound::{sample_at, vtable, Fmt, AHEAD, RATE, STEP};
use crate::HashMap;

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const XAUDIO2_E_INVALID_CALL: u32 = 0x8896_0001;
const END_OF_STREAM: u32 = 0x40;
const LOOP_INFINITE: u32 = 255;

struct XBuf {
    flags: u32,
    data: u32,
    /// frames: play region and loop region, in the buffer
    begin: u32,
    end: u32,
    loop_begin: u32,
    loop_end: u32,
    loops: u32,
    ctx: u32,
}

#[derive(PartialEq)]
enum Kind {
    Source,
    Submix,
    Master,
}

struct Voice {
    kind: Kind,
    fmt: Fmt,
    volume: f32,
    /// the voice this one sends to (0: the mastering voice)
    dest: u32,
    cb: u32,
    started: bool,
    queue: VecDeque<XBuf>,
    /// position in the front buffer, in frames
    pos: f64,
    began: bool,
    ratio: f32,
    played: f64,
    /// callbacks to deliver on the next pass (buffers flushed)
    pending: Vec<(u32, u32, u32)>,
    chain: Vec<Fx>,
    /// creation order
    seq: u64,
}

/// An effect of a voice's chain: its IXAPO (a reference held), whether it runs, and for a
/// game's own XAPO the guest buffer it processes in place (frames, then its
/// XAPO_PROCESS_BUFFER_PARAMETERS)
#[derive(Clone)]
struct Fx {
    xapo: u32,
    enabled: bool,
    builtin: bool,
    buf: u32,
}

struct Engine {
    voices: HashMap<u32, Voice>,
    master: u32,
    callbacks: Vec<u32>,
    running: bool,
    stream: i32,
    thread: bool,
    /// the 2.8 interfaces (XAudio2Create)
    v28: bool,
    seq: u64,
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();

/// The engine thread's processing passes (the callbacks, the mix, a game's XAPOs): DestroyVoice,
/// SetEffectChain, UnregisterForCallbacks and the last Release wait for the pass running when
/// they are called, as XAudio2's do (review 2026-11-14: they returned at once, and a game that
/// then deleted its IXAudio2VoiceCallback had the engine thread call through a freed vtable, or
/// had its XAPO buffer freed while Process wrote into it).
pub(crate) struct PassGate {
    /// (a pass running, passes finished)
    state: Mutex<(bool, u64)>,
    done: Condvar,
}

impl PassGate {
    pub(crate) const fn new() -> PassGate {
        PassGate { state: Mutex::new((false, 0)), done: Condvar::new() }
    }
    pub(crate) fn begin(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0 = true;
    }
    pub(crate) fn end(&self) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *g = (false, g.1 + 1);
        drop(g);
        self.done.notify_all();
    }
    /// Waits until the pass running now (if any) is over; `idle` is called between waits
    /// without the lock (save-state points). False when it took longer than `limit` (a game
    /// that blocks its own callbacks: Windows would hang for good).
    pub(crate) fn wait(&self, limit: Duration, mut idle: impl FnMut()) -> bool {
        let start = std::time::Instant::now();
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !g.0 {
            return true;
        }
        let target = g.1 + 1;
        while g.1 < target {
            if start.elapsed() >= limit {
                return false;
            }
            g = self.done.wait_timeout(g, Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner()).0;
            if g.1 < target {
                drop(g);
                idle();
                g = self.state.lock().unwrap_or_else(|e| e.into_inner());
            }
        }
        true
    }
}

static PASS: PassGate = PassGate::new();

thread_local! {
    /// whether this is the engine thread (its own callbacks may destroy voices: no waiting)
    static ENGINE_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Before a voice, its callback or its effects go away: the running pass is over.
fn wait_pass() {
    if ENGINE_THREAD.with(|t| t.get()) {
        return;
    }
    if !PASS.wait(Duration::from_secs(2), || {
        crate::snap::point();
    }) {
        eprintln!("sake: xaudio2: a processing pass took over 2 s (the game's callback waits on this thread?)");
    }
}

/// Save states: wakes the engine thread's wait.
pub(crate) fn snap_wake() {
    // (under the engine's lock: the engine thread is either before its predicate or waiting)
    drop(ENGINE.lock().unwrap_or_else(|e| e.into_inner()));
    WAKE.notify_all();
}

/// How long the engine thread sleeps when `queued` frames wait in the stream: until it is
/// below AHEAD again (review 2026-11-14: it polled every 5 ms, idle or not).
pub(crate) fn drain_wait(queued: u32) -> Duration {
    if queued < AHEAD {
        return Duration::ZERO;
    }
    let frames = (queued - AHEAD + 1) as u64;
    Duration::from_micros((frames * 1_000_000).div_ceil(RATE as u64)).max(Duration::from_millis(1))
}

/// The most buffers a source voice queues (XAUDIO2_MAX_QUEUED_BUFFERS)
pub(crate) const MAX_QUEUED: usize = 64;

/// The frames an XAUDIO2_BUFFER plays: (begin, end, loop_begin, loop_end), all within `frames`
/// (sums saturate: PlayBegin + PlayLength past 4 G frames is not a short region).
pub(crate) fn buffer_region(frames: u32, play_begin: u32, play_len: u32, loop_begin: u32, loop_len: u32, loops: u32) -> (u32, u32, u32, u32) {
    let begin = play_begin.min(frames);
    let end = if play_len == 0 { frames } else { begin.saturating_add(play_len).min(frames) };
    // the loop region: LoopLength 0 runs to the play region's end
    let lb = if loops > 0 { loop_begin.min(frames) } else { begin };
    let le = if loops > 0 && loop_len != 0 { lb.saturating_add(loop_len).min(frames) } else { end };
    (begin, end, lb, le)
}

fn with<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    let mut g = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Engine { voices: HashMap::default(), master: 0, callbacks: Vec::new(), running: false, stream: -1, thread: false, v28: false, seq: 0 }))
}

fn with_voice<R>(this: u32, f: impl FnOnce(&mut Voice) -> R) -> Option<R> {
    with(|e| e.voices.get_mut(&this).map(f))
}

/// Float argument `i` (x64: its XMM register)
fn farg(c: &Ctx, i: u32) -> f32 {
    f32::from_bits(if mem::x64() { c.xmm(i) as u32 } else { c.a(i) })
}

/// The reference count, after the vtable pointer
fn refs(this: u32) -> u32 {
    this + ps()
}

// ---------------------------------------------------------------------------------------------
// the engine thread

/// Callback method indices (IXAudio2VoiceCallback).
const ON_PASS_START: u32 = 0;
const ON_PASS_END: u32 = 1;
const ON_STREAM_END: u32 = 2;
const ON_BUFFER_START: u32 = 3;
const ON_BUFFER_END: u32 = 4;
const ON_LOOP_END: u32 = 5;

fn callback(c: &mut Ctx, obj: u32, method: u32, arg: Option<u32>) {
    let fp = rp(rp(obj) + ps() * method);
    match arg {
        Some(a) => c.call(fp, &[obj, a]),
        None => c.call(fp, &[obj]),
    };
}

/// Renders one pass of source voice `v` into `out` (stereo); the callbacks it produced
/// (object, method, argument)
fn render(v: &mut Voice, out: &mut [f32]) -> Vec<(u32, u32, u32)> {
    let mut ev = std::mem::take(&mut v.pending);
    if v.kind != Kind::Source || !v.started {
        return ev;
    }
    let step = v.fmt.rate as f64 * v.ratio as f64 / RATE as f64;
    for o in out.chunks_exact_mut(2) {
        let Some(b) = v.queue.front_mut() else { break };
        if !v.began {
            v.began = true;
            v.pos = b.begin as f64;
            if v.cb != 0 {
                ev.push((v.cb, ON_BUFFER_START, b.ctx));
            }
        }
        let i = v.pos as u32;
        let t = (v.pos - i as f64) as f32;
        // (only frames of the buffer: AudioBytes bounds both regions; review 2026-11-14: a
        // zero-length buffer still read its first frame, or address 0)
        let last = if b.looping() { b.end.max(b.loop_end) } else { b.end };
        if i < last && b.data != 0 {
            let (l0, r0) = sample_at(b.data, &v.fmt, i as usize);
            let (l1, r1) = if i + 1 < last { sample_at(b.data, &v.fmt, i as usize + 1) } else { (l0, r0) };
            o[0] += l0 + (l1 - l0) * t;
            o[1] += r0 + (r1 - r0) * t;
        }
        v.pos += step;
        v.played += step;
        // while loops remain the loop's end is where the buffer turns back (even past the
        // play region's end, as 2.7 does); a loop region that ends before the play region
        // begins is never taken. A step longer than the loop turns back as many times as it
        // passes the loop's end (review 2026-11-14: once per sample, so the position ran on
        // past the buffer into whatever memory follows it)
        if b.looping() && v.pos >= b.loop_end as f64 {
            let len = (b.loop_end - b.loop_begin) as f64;
            let mut wraps = ((v.pos - b.loop_begin as f64) / len).floor().max(1.0);
            if b.loops != LOOP_INFINITE {
                wraps = wraps.min(b.loops as f64);
                b.loops -= wraps as u32;
            }
            v.pos -= wraps * len;
            if !v.pos.is_finite() {
                v.pos = b.loop_begin as f64;
            }
            // (one OnLoopEnd a turn; more than a few a sample is a broken format, not music)
            if v.cb != 0 {
                for _ in 0..(wraps as u32).min(4) {
                    ev.push((v.cb, ON_LOOP_END, b.ctx));
                }
            }
        }
        if !b.looping() && v.pos >= b.end as f64 {
            // the buffer is done, and so is any next one the step passes over whole
            let mut over = v.pos - b.end as f64;
            loop {
                let b = v.queue.pop_front().unwrap();
                if v.cb != 0 {
                    ev.push((v.cb, ON_BUFFER_END, b.ctx));
                }
                if b.flags & END_OF_STREAM != 0 {
                    // (SamplesPlayed starts over after the end of a stream)
                    v.played = 0.0;
                    if v.cb != 0 {
                        ev.push((v.cb, ON_STREAM_END, u32::MAX));
                    }
                }
                v.began = false;
                let Some(n) = v.queue.front() else { break };
                v.began = true;
                v.pos = n.begin as f64 + over;
                if v.cb != 0 {
                    ev.push((v.cb, ON_BUFFER_START, n.ctx));
                }
                if n.looping() || v.pos < n.end as f64 {
                    break;
                }
                over = v.pos - n.end as f64;
            }
        }
    }
    ev
}

impl XBuf {
    fn looping(&self) -> bool {
        self.loops > 0 && self.loop_end > self.begin && self.loop_begin < self.loop_end
    }
}

/// The voice `id` sends to (its destination, or the mastering voice)
fn dest_of(e: &Engine, id: u32) -> u32 {
    match e.voices.get(&id) {
        Some(v) if v.kind == Kind::Master => 0,
        Some(v) if v.dest != 0 && e.voices.contains_key(&v.dest) => v.dest,
        Some(_) => e.master,
        None => 0,
    }
}
/// Hops from `id` to the mastering voice
fn depth(e: &Engine, mut id: u32) -> u32 {
    let mut d = 0;
    while d < 16 {
        let n = dest_of(e, id);
        if n == 0 {
            break;
        }
        id = n;
        d += 1;
    }
    d
}

/// One processing pass of the whole graph into `out` (stereo)
fn pass(c: &mut Ctx, out: &mut [f32]) {
    let n = out.len();
    // the source voices, newest first: each one's pass-start, buffer callbacks and pass-end
    let order: Vec<u32> = with(|e| {
        let mut v: Vec<(u64, u32)> = e.voices.iter().filter(|(_, v)| v.kind == Kind::Source).map(|(&k, v)| (v.seq, k)).collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v.into_iter().map(|x| x.1).collect()
    });
    let mut bufs: HashMap<u32, Vec<f32>> = HashMap::default();
    for id in order {
        let Some((cb, started, need)) = with_voice(id, |v| (v.cb, v.started, bytes_required(v))) else { continue };
        let cbs = started && cb != 0;
        if cbs {
            callback(c, cb, ON_PASS_START, Some(need));
        }
        let Some((b, ev)) = with_voice(id, |v| {
            let mut b = vec![0f32; n];
            let ev = render(v, &mut b);
            (b, ev)
        }) else {
            continue;
        };
        for (o, m, a) in ev {
            callback(c, o, m, (a != u32::MAX).then_some(a));
        }
        if cbs {
            callback(c, cb, ON_PASS_END, None);
        }
        if started {
            bufs.insert(id, b);
        }
    }
    // then the graph, from the voices furthest from the mastering voice: effects, volume, sum
    let plan: Vec<(u32, u32, f32, bool)> = with(|e| {
        let mut v: Vec<(u32, u32)> = e.voices.keys().map(|&k| (depth(e, k), k)).collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v.into_iter().map(|(_, k)| { let x = &e.voices[&k]; (k, dest_of(e, k), x.volume, x.kind == Kind::Master) }).collect()
    });
    for (id, dest, vol, master) in plan {
        let started = bufs.contains_key(&id);
        let is_source = with_voice(id, |v| v.kind == Kind::Source).unwrap_or(true);
        if is_source && !started {
            continue;
        }
        let mut b = bufs.remove(&id).unwrap_or_else(|| vec![0.0; n]);
        run_chain(c, id, &mut b);
        if vol != 1.0 {
            b.iter_mut().for_each(|x| *x *= vol);
        }
        if master {
            out.copy_from_slice(&b);
        } else if dest != 0 {
            let d = bufs.entry(dest).or_insert_with(|| vec![0.0; n]);
            d.iter_mut().zip(&b).for_each(|(a, x)| *a += x);
        }
    }
}

/// Runs voice `id`'s effect chain on `buf` (stereo frames at the engine's rate)
fn run_chain(c: &mut Ctx, id: u32, buf: &mut [f32]) {
    let chain = with_voice(id, |v| v.chain.clone()).unwrap_or_default();
    let frames = (buf.len() / 2) as u32;
    let k = ps();
    for fx in chain {
        if fx.builtin {
            crate::xapo::process_builtin(crate::xapo::builtin(fx.xapo).unwrap_or(fx.xapo), buf, 2, RATE, fx.enabled);
            continue;
        }
        // a game's XAPO: in place in its buffer
        let bytes: Vec<u8> = buf.iter().flat_map(|x| x.to_le_bytes()).collect();
        mem::write(fx.buf, &bytes);
        let pp = fx.buf + 8 * STEP as u32;
        mem::wp(pp, fx.buf);
        w32(pp + k, 1); // XAPO_BUFFER_VALID
        w32(pp + k + 4, frames);
        let f = rp(rp(fx.xapo) + 10 * k);
        c.call(f, &[fx.xapo, 1, pp, 1, pp, fx.enabled as u32]);
        if r32(pp + k) == 1 {
            for (i, x) in buf.iter_mut().enumerate() {
                *x = f32::from_bits(r32(fx.buf + 4 * i as u32));
            }
        } else {
            // XAPO_BUFFER_SILENT
            buf.iter_mut().for_each(|x| *x = 0.0);
        }
    }
}

/// bytes the voice still needs for the next pass (OnVoiceProcessingPassStart)
fn bytes_required(v: &Voice) -> u32 {
    let block = v.fmt.block().max(1) as f64;
    let need = STEP as f64 * v.fmt.rate as f64 * v.ratio as f64 / RATE as f64;
    let mut have = 0.0;
    for (i, b) in v.queue.iter().enumerate() {
        let from = if i == 0 && v.began { v.pos } else { b.begin as f64 };
        have += (b.end as f64 - from).max(0.0);
        if b.loops > 0 {
            return 0;
        }
    }
    if have >= need {
        0
    } else {
        ((need - have).ceil() * block) as u32
    }
}

/// The engine thread while the engine is stopped: asleep until StartEngine (or a save state's
/// pause; `cap` as a safety net). The flag is set under ENGINE's lock, the predicate read under
/// it: no wakeup is lost.
fn sleep_while_stopped(cap: Duration) {
    let g = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    let idle = |e: &mut Option<Engine>| !e.as_ref().is_some_and(|e| e.running) && !kura::snap::pausing();
    drop(WAKE.wait_timeout_while(g, cap, idle));
}

fn engine_thread(c: &mut Ctx) -> u32 {
    ENGINE_THREAD.with(|t| t.set(true));
    let mut out = vec![0f32; STEP * 2];
    loop {
        let (stream, running) = with(|e| (e.stream, e.running));
        if !running {
            sleep_while_stopped(Duration::from_secs(1));
            crate::snap::point();
            continue;
        }
        let wait = drain_wait(kura::audio::queued(stream));
        if !wait.is_zero() {
            kura::snap::sleep(wait);
            crate::snap::point();
            continue;
        }
        PASS.begin();
        let engine_cbs = with(|e| e.callbacks.clone());
        for &cb in &engine_cbs {
            callback(c, cb, 0, None);
        }
        out.iter_mut().for_each(|x| *x = 0.0);
        pass(c, &mut out);
        for &cb in &engine_cbs {
            callback(c, cb, 1, None);
        }
        PASS.end();
        kura::audio::write(stream, &out);
    }
}

fn start_thread() {
    let spawn = with(|e| {
        if e.thread {
            return false;
        }
        e.thread = true;
        e.stream = kura::audio::open();
        true
    });
    if spawn {
        static ENTRY: OnceLock<u32> = OnceLock::new();
        let entry = *ENTRY.get_or_init(|| host_fn("xaudio2_thread", engine_thread, 4));
        crate::spawn_gated(entry, 0, 0x10000, crate::obj::new_thread_obj(0), false);
    }
}

// ---------------------------------------------------------------------------------------------
// IXAudio2

fn xa_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IXAudio2",
            &[
                (X_QueryInterface, 12),
                (X_AddRef, 4),
                (X_Release, 4),
                (X_GetDeviceCount, 8),
                (X_GetDeviceDetails, 12),
                (X_Initialize, 12),
                (X_RegisterForCallbacks, 8),
                (X_UnregisterForCallbacks, 8),
                (X_CreateSourceVoice, 32),
                (X_CreateSubmixVoice, 32),
                (X_CreateMasteringVoice, 28),
                (X_StartEngine, 4),
                (X_StopEngine, 4),
                (X_CommitChanges, 8),
                (X_GetPerformanceData, 8),
                (X_SetDebugConfiguration, 12),
            ],
        )
    })
}

/// IXAudio2 2.8 / 2.9
fn xa28_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IXAudio2_28",
            &[
                (X_QueryInterface, 12),
                (X_AddRef, 4),
                (X_Release, 4),
                (X_RegisterForCallbacks, 8),
                (X_UnregisterForCallbacks, 8),
                (X_CreateSourceVoice, 32),
                (X_CreateSubmixVoice, 32),
                (X_CreateMasteringVoice, 32),
                (X_StartEngine, 4),
                (X_StopEngine, 4),
                (X_CommitChanges, 8),
                (X_GetPerformanceData, 8),
                (X_SetDebugConfiguration, 12),
            ],
        )
    })
}

/// XAudio2Create(ppXAudio2, Flags, XAudio2Processor) (xaudio2_8, xaudio2_9)
fn XAudio2Create(c: &mut Ctx) -> u32 {
    let pp = c.a(0);
    if pp == 0 {
        return E_INVALIDARG;
    }
    with(|e| e.v28 = true);
    let o = mem::heap_alloc(16, true);
    mem::wp(o, xa28_vtbl());
    w32(refs(o), 1);
    putp(pp, o);
    0
}

pub fn init() {
    crate::api::implement("xaudio2_8", "XAudio2Create", XAudio2Create);
    // (XAudio2CreateWithVersionInfo: the same, with the NTDDI version it was built for)
    crate::api::implement("xaudio2_8", "XAudio2CreateWithVersionInfo", XAudio2Create);
    crate::x3daudio::init();
}

/// CoCreateInstance(CLSID_XAudio2 / CLSID_XAudio2_Debug, 2.7)
pub fn create_instance(clsid: u32) -> Option<u32> {
    if !matches!(r32(clsid), 0x5a50_8685 | 0xdb05_ea35) {
        return None;
    }
    with(|e| e.v28 = false);
    let o = mem::heap_alloc(16, true);
    mem::wp(o, xa_vtbl());
    w32(refs(o), 1);
    Some(o)
}

fn X_QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    // IUnknown and the object's own IXAudio2: 2.7's, or 2.8's and 2.9's (one engine serves both)
    let v27 = mem::rp(this) == xa_vtbl();
    if matches!((r32(iid), v27), (0, _) | (0x8bcf_1f58, true) | (0x60d8_dac8 | 0x2b02_e3cf, false)) {
        w32(refs(this), r32(refs(this)) + 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn X_AddRef(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    w32(refs(this), r32(refs(this)) + 1);
    r32(refs(this))
}
fn X_Release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = r32(refs(this)).saturating_sub(1);
    w32(refs(this), n);
    if n == 0 {
        let chains: Vec<Vec<Fx>> = with(|e| {
            e.running = false;
            e.callbacks.clear();
            e.master = 0;
            e.voices.drain().map(|(_, v)| v.chain).collect()
        });
        // (the graph is empty: the pass now running is the last that may use any of it)
        wait_pass();
        for ch in chains {
            release_chain(c, ch);
        }
        // (the voices' own objects stay: a game may still call DestroyVoice through them)
        mem::heap_free(this);
    }
    n
}
fn X_GetDeviceCount(c: &mut Ctx) -> u32 {
    put32(c.a(1), 1);
    0
}
/// XAUDIO2_DEVICE_DETAILS: DeviceID[256], DisplayName[256], Role, WAVEFORMATEXTENSIBLE
fn X_GetDeviceDetails(c: &mut Ctx) -> u32 {
    let (index, p) = (c.a(1), c.a(2));
    if index != 0 {
        return E_INVALIDARG;
    }
    for i in 0..1068 {
        mem::w8(p + i, 0);
    }
    let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    mem::put_wstr(p, 256, &w("sake"));
    mem::put_wstr(p + 512, 256, &w("sake WebAudio"));
    w32(p + 1024, 0xf); // GlobalDefaultDevice
    // the device's mix format: 16-bit PCM stereo, as Windows reports it
    let f = p + 1028;
    w16(f, 0xfffe);
    w16(f + 2, 2);
    w32(f + 4, RATE);
    w32(f + 8, RATE * 4);
    w16(f + 12, 4);
    w16(f + 14, 16);
    w16(f + 16, 22);
    w16(f + 18, 16);
    w32(f + 20, 3); // SPEAKER_FRONT_LEFT | RIGHT
    // KSDATAFORMAT_SUBTYPE_PCM {00000001-0000-0010-8000-00aa00389b71}
    for (i, b) in [1u8, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71].iter().enumerate() {
        mem::w8(f + 24 + i as u32, *b);
    }
    0
}
fn X_Initialize(_: &mut Ctx) -> u32 {
    0
}
fn X_RegisterForCallbacks(c: &mut Ctx) -> u32 {
    let cb = c.a(1);
    with(|e| {
        if cb != 0 && !e.callbacks.contains(&cb) {
            e.callbacks.push(cb);
        }
    });
    0
}
fn X_UnregisterForCallbacks(c: &mut Ctx) -> u32 {
    let cb = c.a(1);
    with(|e| e.callbacks.retain(|&x| x != cb));
    wait_pass();
    0
}

/// XAUDIO2_VOICE_SENDS { SendCount, pSends } of XAUDIO2_SEND_DESCRIPTOR { Flags, pOutputVoice }
fn first_send(sends: u32) -> u32 {
    if sends == 0 || r32(sends) == 0 {
        return 0;
    }
    // (pack(1): the pointers follow the u32s at +4 on x64 too)
    rp(rp(sends + 4) + 4)
}

fn new_voice(kind: Kind, fmt: Fmt, dest: u32, cb: u32) -> u32 {
    let v28 = with(|e| e.v28);
    let vt = match kind {
        Kind::Source if v28 => src28_vtbl(),
        Kind::Source => src_vtbl(),
        Kind::Master if v28 => master28_vtbl(),
        _ => voice_vtbl(),
    };
    let o = mem::heap_alloc(8, true);
    mem::wp(o, vt);
    with(|e| {
        e.seq += 1;
        let v = Voice { kind, fmt, volume: 1.0, dest, cb, started: false, queue: VecDeque::new(), pos: 0.0, began: false, ratio: 1.0, played: 0.0, pending: Vec::new(), chain: Vec::new(), seq: e.seq };
        if v.kind == Kind::Master {
            e.master = o;
        }
        e.voices.insert(o, v);
    });
    o
}

/// CreateSourceVoice(pp, format, flags, max ratio, callback, sends, effects)
fn X_CreateSourceVoice(c: &mut Ctx) -> u32 {
    let (pp, wf, cb, sends, fx) = (c.a(1), c.a(2), c.a(5), c.a(6), c.a(7));
    let chain = match read_chain(c, fx) {
        Ok(ch) => ch,
        Err(e) => {
            putp(pp, 0);
            return e;
        }
    };
    let tag = r16(wf);
    let sub = if tag == 0xfffe { r32(wf + 24) } else { tag as u32 };
    let bits = r16(wf + 14) as u32;
    if !matches!(sub, 1 | 3) || !matches!(bits, 8 | 16 | 24 | 32) || r32(wf + 4) == 0 {
        eprintln!("sake: XAudio2 source format {tag:#x} ({bits} bits) not supported");
        putp(pp, 0);
        return E_INVALIDARG;
    }
    let fmt = Fmt { rate: r32(wf + 4), channels: (r16(wf + 2) as u32).max(1), bits, float: sub == 3, law: 0 };
    let o = new_voice(Kind::Source, fmt, first_send(sends), cb);
    with_voice(o, |v| v.chain = chain);
    putp(pp, o);
    0
}
/// CreateSubmixVoice(pp, channels, rate, flags, stage, sends, effects)
fn X_CreateSubmixVoice(c: &mut Ctx) -> u32 {
    let (pp, ch, rate, sends, fx) = (c.a(1), c.a(2), c.a(3), c.a(6), c.a(7));
    let chain = match read_chain(c, fx) {
        Ok(ch) => ch,
        Err(e) => {
            putp(pp, 0);
            return e;
        }
    };
    let fmt = Fmt { rate: if rate == 0 { RATE } else { rate }, channels: ch.max(1), bits: 32, float: true, law: 0 };
    let o = new_voice(Kind::Submix, fmt, first_send(sends), 0);
    with_voice(o, |v| v.chain = chain);
    putp(pp, o);
    0
}
/// CreateMasteringVoice(pp, channels, rate, flags, device, effects[, category])
fn X_CreateMasteringVoice(c: &mut Ctx) -> u32 {
    let (pp, ch, rate, fx) = (c.a(1), c.a(2), c.a(3), c.a(6));
    if with(|e| e.master != 0) {
        putp(pp, 0);
        return XAUDIO2_E_INVALID_CALL;
    }
    let chain = match read_chain(c, fx) {
        Ok(ch) => ch,
        Err(e) => {
            putp(pp, 0);
            return e;
        }
    };
    let fmt = Fmt { rate: if rate == 0 { RATE } else { rate }, channels: if ch == 0 { 2 } else { ch }, bits: 32, float: true, law: 0 };
    let o = new_voice(Kind::Master, fmt, 0, 0);
    with_voice(o, |v| v.chain = chain);
    putp(pp, o);
    start_thread();
    with(|e| e.running = true);
    WAKE.notify_all();
    0
}
fn X_StartEngine(_: &mut Ctx) -> u32 {
    start_thread();
    with(|e| e.running = true);
    WAKE.notify_all();
    0
}
fn X_StopEngine(_: &mut Ctx) -> u32 {
    with(|e| e.running = false);
    0
}
fn X_CommitChanges(_: &mut Ctx) -> u32 {
    0
}
/// XAUDIO2_PERFORMANCE_DATA: 2 UINT64, 12 UINT32
fn X_GetPerformanceData(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    for i in 0..64 {
        mem::w8(p + i, 0);
    }
    0
}
fn X_SetDebugConfiguration(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// IXAudio2Voice / IXAudio2SourceVoice (2.7: no IUnknown)

const VOICE_METHODS: [(crate::api::Handler, u32); 19] = [
    (V_GetVoiceDetails, 8),
    (V_SetOutputVoices, 8),
    (V_SetEffectChain, 8),
    (V_EnableEffect, 12),
    (V_DisableEffect, 12),
    (V_GetEffectState, 12),
    (V_SetEffectParameters, 20),
    (V_GetEffectParameters, 16),
    (V_SetFilterParameters, 12),
    (V_GetFilterParameters, 8),
    (V_SetOutputFilterParameters, 16),
    (V_GetOutputFilterParameters, 12),
    (V_SetVolume, 12),
    (V_GetVolume, 8),
    (V_SetChannelVolumes, 16),
    (V_GetChannelVolumes, 12),
    (V_SetOutputMatrix, 24),
    (V_GetOutputMatrix, 20),
    (V_DestroyVoice, 4),
];

fn voice_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| vtable("IXAudio2Voice", &VOICE_METHODS))
}

fn src_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let mut m = VOICE_METHODS.to_vec();
        m.extend_from_slice(&[
            (S_Start, 12),
            (S_Stop, 12),
            (S_SubmitSourceBuffer, 12),
            (S_FlushSourceBuffers, 4),
            (S_Discontinuity, 4),
            (S_ExitLoop, 8),
            (S_GetState, 8),
            (S_SetFrequencyRatio, 12),
            (S_GetFrequencyRatio, 8),
            (S_SetSourceSampleRate, 8),
        ]);
        vtable("IXAudio2SourceVoice", &m)
    })
}

/// IXAudio2SourceVoice 2.8: GetState(pVoiceState, Flags)
fn src28_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let mut m = VOICE_METHODS.to_vec();
        m.extend_from_slice(&[
            (S_Start, 12),
            (S_Stop, 12),
            (S_SubmitSourceBuffer, 12),
            (S_FlushSourceBuffers, 4),
            (S_Discontinuity, 4),
            (S_ExitLoop, 8),
            (S_GetState28, 12),
            (S_SetFrequencyRatio, 12),
            (S_GetFrequencyRatio, 8),
            (S_SetSourceSampleRate, 8),
        ]);
        vtable("IXAudio2SourceVoice_28", &m)
    })
}

/// IXAudio2MasteringVoice 2.8: GetChannelMask
fn master28_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let mut m = VOICE_METHODS.to_vec();
        m.push((M_GetChannelMask, 8));
        vtable("IXAudio2MasteringVoice_28", &m)
    })
}

/// the speakers of the mastering voice's channels (SPEAKER_STEREO for 2, else the first n)
fn M_GetChannelMask(c: &mut Ctx) -> u32 {
    let ch = with_voice(c.a(0), |v| v.fmt.channels).unwrap_or(2);
    let mask = match ch {
        1 => 0x4,
        2 => 0x3,
        n => (1u32 << n.min(18)) - 1,
    };
    if c.a(1) == 0 {
        return E_INVALIDARG;
    }
    put32(c.a(1), mask);
    0
}

/// XAUDIO2_VOICE_DETAILS: CreationFlags, (2.8: ActiveFlags,) InputChannels, InputSampleRate
fn V_GetVoiceDetails(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let v28 = with(|e| e.v28);
    if let Some((ch, rate)) = with_voice(c.a(0), |v| (v.fmt.channels, v.fmt.rate)) {
        let mut p = p;
        w32(p, 0);
        if v28 {
            p += 4;
            w32(p, 0);
        }
        w32(p + 4, ch);
        w32(p + 8, rate);
    }
    0
}
fn V_SetOutputVoices(c: &mut Ctx) -> u32 {
    let d = first_send(c.a(1));
    with_voice(c.a(0), |v| v.dest = d);
    0
}
/// IID_IXAPO (2.8+) / IID_IXAPO of 2.7, and IXAPOParameters' of each
const IID_IXAPO: [u8; 16] = [0x84, 0xb9, 0x10, 0xa4, 0x39, 0x98, 0x19, 0x48, 0xa0, 0xbe, 0x28, 0x56, 0xae, 0x6b, 0x3a, 0xdb];
const IID_IXAPO27: [u8; 16] = [0x01, 0xc0, 0x0b, 0xa9, 0x97, 0xe8, 0x97, 0xe8, 0x55, 0xe4, 0x9e, 0x47, 0, 0, 0, 0];
const IID_IXAPOPARAMS: [u8; 16] = [0x66, 0x5c, 0xd9, 0x26, 0xf2, 0x80, 0x9a, 0x49, 0xad, 0x54, 0x5a, 0xe7, 0xf0, 0x1c, 0x6d, 0x98];
const IID_IXAPOPARAMS27: [u8; 16] = [0x01, 0xc0, 0x0b, 0xa9, 0x97, 0xe8, 0x97, 0xe8, 0x55, 0xe4, 0x9e, 0x47, 0, 0, 0, 1];

/// QueryInterface(iid) on COM object `o`: the interface (a reference held), or 0
fn query(c: &mut Ctx, o: u32, iid: &[u8; 16]) -> u32 {
    let k = ps();
    let m = mem::heap_alloc(16 + k, true);
    mem::write(m, iid);
    let hr = c.call(rp(rp(o)), &[o, m, m + 16]);
    let r = if hr == 0 { rp(m + 16) } else { 0 };
    mem::heap_free(m);
    r
}
/// Method `i` of the object's vtable, called with `args` after `this`
fn method(c: &mut Ctx, o: u32, i: u32, args: &[u32]) -> u32 {
    let mut a = vec![o];
    a.extend_from_slice(args);
    c.call(rp(rp(o) + i * ps()), &a)
}

/// The float stereo format effects are locked for (a WAVEFORMATEXTENSIBLE, as XAudio2's)
fn fx_format() -> u32 {
    static F: OnceLock<u32> = OnceLock::new();
    *F.get_or_init(|| {
        let f = mem::heap_alloc(40, true);
        w16(f, 0xfffe);
        w16(f + 2, 2);
        w32(f + 4, RATE);
        w32(f + 8, RATE * 8);
        w16(f + 12, 8);
        w16(f + 14, 32);
        w16(f + 16, 22);
        w16(f + 18, 32);
        w32(f + 20, 3);
        mem::write(f + 24, &[3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);
        f
    })
}

/// XAUDIO2_EFFECT_CHAIN { EffectCount, pEffectDescriptors } of XAUDIO2_EFFECT_DESCRIPTOR
/// { pEffect, InitialState, OutputChannels }: each effect's IXAPO, locked for processing (a
/// missing effect, or one that is no IXAPO of this version, is XAUDIO2_E_INVALID_CALL)
fn read_chain(c: &mut Ctx, chain: u32) -> Result<Vec<Fx>, u32> {
    if chain == 0 || r32(chain) == 0 {
        return Ok(Vec::new());
    }
    let k = ps();
    // XAudio2 structs are pack(1): the descriptor pointer follows the count at +4
    let (n, d) = (r32(chain), rp(chain + 4));
    let iid = if with(|e| e.v28) { IID_IXAPO } else { IID_IXAPO27 };
    let mut v = Vec::new();
    for i in 0..n.min(64) {
        let at = d + i * (k + 8);
        let (unk, on) = (rp(at), r32(at + k) != 0);
        let x = if unk == 0 { 0 } else { query(c, unk, &iid) };
        if x == 0 {
            release_chain(c, v);
            return Err(XAUDIO2_E_INVALID_CALL);
        }
        let builtin = crate::xapo::builtin(x).is_some();
        // XAPO_LOCKFORPROCESS_BUFFER_PARAMETERS { pFormat, MaxFrameCount }, in and out
        let lp = mem::heap_alloc(2 * k, true);
        mem::wp(lp, fx_format());
        w32(lp + k, STEP as u32);
        let hr = method(c, x, 8, &[1, lp, 1, lp]);
        mem::heap_free(lp);
        if hr != 0 {
            method(c, x, 2, &[]);
            release_chain(c, v);
            return Err(XAUDIO2_E_INVALID_CALL);
        }
        let buf = if builtin { 0 } else { mem::heap_alloc(8 * STEP as u32 + 4 * k, true) };
        v.push(Fx { xapo: x, enabled: on, builtin, buf });
    }
    Ok(v)
}
/// Unlocks and releases a chain's effects
fn release_chain(c: &mut Ctx, chain: Vec<Fx>) {
    for fx in chain {
        method(c, fx.xapo, 9, &[]);
        method(c, fx.xapo, 2, &[]);
        if fx.buf != 0 {
            mem::heap_free(fx.buf);
        }
    }
}

/// SetEffectChain(pEffectChain): the new chain replaces the old (NULL: none)
fn V_SetEffectChain(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    let chain = match read_chain(c, p) {
        Ok(ch) => ch,
        Err(e) => return e,
    };
    let old = with_voice(this, |v| std::mem::replace(&mut v.chain, chain));
    if let Some(old) = old {
        wait_pass();
        release_chain(c, old);
    }
    0
}
/// EnableEffect / DisableEffect(EffectIndex, OperationSet)
fn set_enabled(c: &mut Ctx, on: bool) -> u32 {
    let (this, i) = (c.a(0), c.a(1) as usize);
    with_voice(this, |v| match v.chain.get_mut(i) {
        Some(fx) => {
            fx.enabled = on;
            0
        }
        None => E_INVALIDARG,
    })
    .unwrap_or(E_INVALIDARG)
}
fn V_EnableEffect(c: &mut Ctx) -> u32 {
    set_enabled(c, true)
}
fn V_DisableEffect(c: &mut Ctx) -> u32 {
    set_enabled(c, false)
}
/// GetEffectState(EffectIndex, pEffectState): XAUDIO2_EFFECT_STATE { bEnabled }
fn V_GetEffectState(c: &mut Ctx) -> u32 {
    let (this, i, p) = (c.a(0), c.a(1) as usize, c.a(2));
    if let Some(Some(on)) = with_voice(this, |v| v.chain.get(i).map(|f| f.enabled)) {
        put32(p, on as u32);
    }
    0
}
/// The IXAPOParameters of effect `i` of the voice (a reference held), or 0
fn fx_params(c: &mut Ctx, this: u32, i: usize) -> u32 {
    let Some(Some(x)) = with_voice(this, |v| v.chain.get(i).map(|f| f.xapo)) else { return 0 };
    let iid = if with(|e| e.v28) { IID_IXAPOPARAMS } else { IID_IXAPOPARAMS27 };
    query(c, x, &iid)
}
/// SetEffectParameters(EffectIndex, pParameters, ParametersByteSize, OperationSet): the
/// effect's IXAPOParameters::SetParameters
fn V_SetEffectParameters(c: &mut Ctx) -> u32 {
    let (this, i, p, n) = (c.a(0), c.a(1) as usize, c.a(2), c.a(3));
    let ip = fx_params(c, this, i);
    if ip == 0 {
        return E_INVALIDARG;
    }
    method(c, ip, 3, &[p, n]);
    method(c, ip, 2, &[]);
    0
}
/// GetEffectParameters(EffectIndex, pParameters, ParametersByteSize)
fn V_GetEffectParameters(c: &mut Ctx) -> u32 {
    let (this, i, p, n) = (c.a(0), c.a(1) as usize, c.a(2), c.a(3));
    let ip = fx_params(c, this, i);
    if ip == 0 {
        return E_INVALIDARG;
    }
    method(c, ip, 4, &[p, n]);
    method(c, ip, 2, &[]);
    0
}
fn V_SetFilterParameters(_: &mut Ctx) -> u32 {
    0
}
/// XAUDIO2_FILTER_PARAMETERS: Type (LowPassFilter), Frequency 1.0, OneOverQ 1.0
fn V_GetFilterParameters(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    w32(p, 0);
    w32(p + 4, 1.0f32.to_bits());
    w32(p + 8, 1.0f32.to_bits());
    0
}
fn V_SetOutputFilterParameters(_: &mut Ctx) -> u32 {
    0
}
fn V_GetOutputFilterParameters(c: &mut Ctx) -> u32 {
    let p = c.a(2);
    w32(p, 0);
    w32(p + 4, 1.0f32.to_bits());
    w32(p + 8, 1.0f32.to_bits());
    0
}
fn V_SetVolume(c: &mut Ctx) -> u32 {
    let vol = farg(c, 1);
    with_voice(c.a(0), |v| v.volume = if vol.is_finite() { vol } else { 0.0 });
    0
}
fn V_GetVolume(c: &mut Ctx) -> u32 {
    let vol = with_voice(c.a(0), |v| v.volume).unwrap_or(1.0);
    put32(c.a(1), vol.to_bits());
    0
}
fn V_SetChannelVolumes(_: &mut Ctx) -> u32 {
    0
}
fn V_GetChannelVolumes(c: &mut Ctx) -> u32 {
    for i in 0..c.a(1) {
        w32(c.a(2) + 4 * i, 1.0f32.to_bits());
    }
    0
}
fn V_SetOutputMatrix(_: &mut Ctx) -> u32 {
    0
}
/// (an identity matrix)
fn V_GetOutputMatrix(c: &mut Ctx) -> u32 {
    let (src, dst, p) = (c.a(2), c.a(3), c.a(4));
    for d in 0..dst {
        for s in 0..src {
            w32(p + 4 * (d * src + s), if d == s || src == 1 { 1.0f32 } else { 0.0 }.to_bits());
        }
    }
    0
}
/// DestroyVoice: not while another voice sends to it (the mastering voice: while any other
/// voice is left); its effects are released
fn V_DestroyVoice(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let chain = with(|e| {
        let busy = e.voices.keys().any(|&k| k != this && dest_of(e, k) == this);
        if busy {
            return None;
        }
        let v = e.voices.remove(&this)?;
        if e.master == this {
            e.master = 0;
        }
        Some(v.chain)
    });
    if let Some(ch) = chain {
        // (the voice is out of the graph: the pass now running is the last that may use it)
        wait_pass();
        release_chain(c, ch);
        mem::heap_free(this);
    }
    0
}

fn S_Start(c: &mut Ctx) -> u32 {
    with_voice(c.a(0), |v| v.started = true);
    WAKE.notify_all();
    0
}
fn S_Stop(c: &mut Ctx) -> u32 {
    with_voice(c.a(0), |v| v.started = false);
    0
}
/// XAUDIO2_BUFFER: Flags, AudioBytes, pAudioData, PlayBegin, PlayLength, LoopBegin,
/// LoopLength, LoopCount, pContext (x64: the pointers 8 bytes, aligned)
fn S_SubmitSourceBuffer(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    // pack(1): on x64 the fields past the 8-byte pAudioData move by 4
    let k = ps() - 4;
    let r = |o: u32| if o == 8 || o == 32 { rp(p + o + if o == 32 { k } else { 0 }) } else { r32(p + if o <= 8 { o } else { o + k }) };
    if p == 0 || (r(8) == 0 && r(4) != 0) {
        return XAUDIO2_E_INVALID_CALL;
    }
    with_voice(this, |v| {
        // (XAUDIO2_MAX_QUEUED_BUFFERS: a game that submits without waiting grew the queue
        // without bound)
        if v.queue.len() >= MAX_QUEUED {
            return XAUDIO2_E_INVALID_CALL;
        }
        let frames = r(4) / v.fmt.block().max(1);
        let loops = r(28);
        let (begin, end, loop_begin, loop_end) = buffer_region(frames, r(12), r(16), r(20), r(24), loops);
        v.queue.push_back(XBuf { flags: r(0), data: r(8), begin, end, loop_begin, loop_end, loops, ctx: r(32) });
        0
    })
    .unwrap_or(E_INVALIDARG)
}
/// drops the queued buffers (but the one playing, while started); each gets OnBufferEnd
fn S_FlushSourceBuffers(c: &mut Ctx) -> u32 {
    with_voice(c.a(0), |v| {
        let keep = (v.started && v.began) as usize;
        let cb = v.cb;
        for b in v.queue.drain(keep.min(v.queue.len())..) {
            if cb != 0 {
                v.pending.push((cb, ON_BUFFER_END, b.ctx));
            }
        }
        if keep == 0 {
            v.began = false;
        }
    });
    0
}
fn S_Discontinuity(_: &mut Ctx) -> u32 {
    0
}
fn S_ExitLoop(c: &mut Ctx) -> u32 {
    with_voice(c.a(0), |v| {
        if let Some(b) = v.queue.front_mut() {
            b.loops = 0;
        }
    });
    0
}
/// GetState(pVoiceState, Flags): XAUDIO2_VOICE_NOSAMPLESPLAYED (0x100) leaves SamplesPlayed
fn S_GetState28(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if c.a(2) & 0x100 != 0 {
        let (ctx, n) = with_voice(c.a(0), |v| (v.queue.front().map_or(0, |b| b.ctx), v.queue.len() as u32)).unwrap_or((0, 0));
        mem::wp(p, ctx);
        w32(p + ps(), n);
        return 0;
    }
    S_GetState(c)
}
/// XAUDIO2_VOICE_STATE (pack(1)): pCurrentBufferContext, BuffersQueued, SamplesPlayed (u64)
fn S_GetState(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let (ctx, n, played) = with_voice(c.a(0), |v| (v.queue.front().map_or(0, |b| b.ctx), v.queue.len() as u32, v.played as u64)).unwrap_or((0, 0, 0));
    let k = ps();
    mem::wp(p, ctx);
    w32(p + k, n);
    mem::w64(p + k + 4, played);
    0
}
fn S_SetFrequencyRatio(c: &mut Ctx) -> u32 {
    let r = farg(c, 1);
    with_voice(c.a(0), |v| v.ratio = if r.is_finite() { r.clamp(1.0 / 1024.0, 1024.0) } else { 1.0 });
    0
}
fn S_GetFrequencyRatio(c: &mut Ctx) -> u32 {
    let r = with_voice(c.a(0), |v| v.ratio).unwrap_or(1.0);
    put32(c.a(1), r.to_bits());
    0
}
fn S_SetSourceSampleRate(c: &mut Ctx) -> u32 {
    let rate = c.a(1);
    with_voice(c.a(0), |v| {
        if rate != 0 {
            v.fmt.rate = rate;
        }
    });
    0
}

/// review 2026-11-14 (b11): SubmitSourceBuffer's limits, the objects DestroyVoice and the last
/// Release free, and the engine thread's sleep while stopped
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod b11 {
    use super::*;
    use crate::test_guest::{call, enter};
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;

    const FMT: Fmt = Fmt { rate: 44100, channels: 2, bits: 16, float: false, law: 0 };

    /// an XAUDIO2_BUFFER (32-bit layout)
    fn xbuf(bytes: u32, data: u32, begin: u32, len: u32, loop_begin: u32, loop_len: u32, loops: u32) -> u32 {
        let p = mem::heap_alloc(36, true);
        for (o, v) in [(4, bytes), (8, data), (12, begin), (16, len), (20, loop_begin), (24, loop_len), (28, loops)] {
            w32(p + o, v);
        }
        p
    }
    fn queued(v: u32) -> usize {
        with_voice(v, |v| v.queue.len()).unwrap()
    }

    #[test]
    fn a_source_voice_queues_at_most_64_buffers() {
        let _g = enter();
        let v = new_voice(Kind::Source, FMT, 0, 0);
        let data = mem::heap_alloc(400, true);
        let b = xbuf(400, data, 0, 0, 0, 0, 0);
        for _ in 0..MAX_QUEUED {
            assert_eq!(call(S_SubmitSourceBuffer, &[v, b]), 0);
        }
        assert_eq!(call(S_SubmitSourceBuffer, &[v, b]), XAUDIO2_E_INVALID_CALL);
        assert_eq!(queued(v), MAX_QUEUED);
        assert_eq!(call(V_DestroyVoice, &[v]), 0);
        mem::heap_free(b);
        mem::heap_free(data);
    }

    #[test]
    fn a_buffer_without_data_is_refused() {
        let _g = enter();
        let v = new_voice(Kind::Source, FMT, 0, 0);
        let b = xbuf(400, 0, 0, 0, 0, 0, 0);
        assert_eq!(call(S_SubmitSourceBuffer, &[v, b]), XAUDIO2_E_INVALID_CALL);
        assert_eq!(call(S_SubmitSourceBuffer, &[v, 0]), XAUDIO2_E_INVALID_CALL);
        assert_eq!(queued(v), 0);
        assert_eq!(call(V_DestroyVoice, &[v]), 0);
        mem::heap_free(b);
    }

    /// PlayBegin + PlayLength past 4 G: the region runs to the buffer's end (it wrapped to a
    /// region ending before it began; debug builds panicked)
    #[test]
    fn huge_lengths_saturate() {
        let _g = enter();
        let v = new_voice(Kind::Source, FMT, 0, 0);
        let data = mem::heap_alloc(400, true);
        let b = xbuf(400, data, 10, u32::MAX, 20, u32::MAX - 5, 2);
        assert_eq!(call(S_SubmitSourceBuffer, &[v, b]), 0);
        let (begin, end, lb, le) = with_voice(v, |v| {
            let x = v.queue.front().unwrap();
            (x.begin, x.end, x.loop_begin, x.loop_end)
        })
        .unwrap();
        assert_eq!((begin, end, lb, le), (10, 100, 20, 100));
        assert_eq!(call(V_DestroyVoice, &[v]), 0);
        mem::heap_free(b);
        mem::heap_free(data);
    }

    #[test]
    fn destroy_voice_frees_the_voice_object() {
        let _g = enter();
        let v = new_voice(Kind::Source, FMT, 0, 0);
        assert!(mem::heap_owns(v));
        assert_eq!(call(V_DestroyVoice, &[v]), 0);
        assert!(!mem::heap_owns(v), "the voice's 8 bytes stayed allocated");
        // (a second DestroyVoice of the same pointer touches nothing)
        assert_eq!(call(V_DestroyVoice, &[v]), 0);
    }

    #[test]
    fn a_voice_others_send_to_is_kept() {
        let _g = enter();
        let sub = new_voice(Kind::Submix, FMT, 0, 0);
        let src = new_voice(Kind::Source, FMT, sub, 0);
        if dest_of_voice(src) == sub {
            assert_eq!(call(V_DestroyVoice, &[sub]), 0);
            assert!(mem::heap_owns(sub));
        }
        assert_eq!(call(V_DestroyVoice, &[src]), 0);
        assert_eq!(call(V_DestroyVoice, &[sub]), 0);
        assert!(!mem::heap_owns(sub) && !mem::heap_owns(src));
    }
    fn dest_of_voice(v: u32) -> u32 {
        with(|e| dest_of(e, v))
    }

    #[test]
    fn the_last_release_frees_the_instance() {
        let _g = enter();
        let o = create_instance({
            let g = mem::heap_alloc(16, true);
            w32(g, 0x5a50_8685);
            g
        })
        .unwrap();
        assert_eq!(call(X_AddRef, &[o]), 2);
        assert_eq!(call(X_Release, &[o]), 1);
        assert!(mem::heap_owns(o));
        assert_eq!(call(X_Release, &[o]), 0);
        assert!(!mem::heap_owns(o), "the instance's 16 bytes stayed allocated");
    }

    /// stopped: the engine thread sleeps (no 5 ms polling) and StartEngine wakes it at once
    #[test]
    fn a_stopped_engine_sleeps_until_started() {
        let _g = enter();
        with(|e| e.running = false);
        let back = Arc::new(AtomicBool::new(false));
        let t = {
            let back = back.clone();
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                sleep_while_stopped(Duration::from_secs(20));
                back.store(true, SeqCst);
                t0.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(!back.load(SeqCst), "the engine thread woke while stopped");
        with(|e| e.running = true);
        WAKE.notify_all();
        let took = t.join().unwrap();
        assert!(took < Duration::from_secs(5), "StartEngine's wakeup was lost ({took:?})");
        with(|e| e.running = false);
    }

    /// thousands of start/stop races: the sleeper always sees a start (a lost wakeup would
    /// leave it asleep for the whole cap)
    #[test]
    fn starts_racing_the_sleep_are_not_lost() {
        let _g = enter();
        for _ in 0..2000 {
            with(|e| e.running = false);
            let t = std::thread::spawn(|| {
                let t0 = std::time::Instant::now();
                sleep_while_stopped(Duration::from_secs(10));
                t0.elapsed()
            });
            std::hint::spin_loop();
            with(|e| e.running = true);
            WAKE.notify_all();
            assert!(t.join().unwrap() < Duration::from_secs(5));
        }
        with(|e| e.running = false);
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod issue88 {
    use super::*;
    use crate::test_guest::{enter, call};
    #[test]
    fn voice_state_writes_packed_fields_without_touching_neighbours() {
        let _g = enter();
        let p = mem::heap_alloc(24, false);
        mem::fill(p, 24, 0xcc);
        assert_eq!(call(S_GetState, &[0, p + 4]), 0);
        assert_eq!(mem::r32(p), 0xcccccccc);
        assert_eq!(mem::slice(p + 4, 16), &[0; 16]);
        assert_eq!(mem::r32(p + 20), 0xcccccccc);
        mem::heap_free(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::SeqCst};
    use std::sync::Arc;

    /// a source voice of a format whose samples are never read (bits 0: sample_at gives 0)
    fn voice(rate: u32, ratio: f32) -> Voice {
        Voice {
            kind: Kind::Source,
            fmt: Fmt { rate, channels: 2, bits: 0, float: false, law: 0 },
            volume: 1.0,
            dest: 0,
            cb: 0,
            started: true,
            queue: VecDeque::new(),
            pos: 0.0,
            began: false,
            ratio,
            played: 0.0,
            pending: Vec::new(),
            chain: Vec::new(),
            seq: 0,
        }
    }
    fn buf(begin: u32, end: u32, loop_begin: u32, loop_end: u32, loops: u32) -> XBuf {
        XBuf { flags: 0, data: 0, begin, end, loop_begin, loop_end, loops, ctx: 0 }
    }

    /// review 2026-11-14: a step longer than the loop turned back once per sample, so the position
    /// ran on past the buffer (reading whatever memory follows it as audio)
    #[test]
    fn short_loops_stay_in_the_buffer() {
        let mut v = voice(200_000, 1024.0);
        v.queue.push_back(buf(0, 100, 10, 12, LOOP_INFINITE));
        let mut out = vec![0f32; STEP * 2];
        for _ in 0..50 {
            render(&mut v, &mut out);
            assert!(v.pos >= 10.0 && v.pos < 12.0, "{}", v.pos);
        }
        // a counted loop: as many turns as it has, then on to the end of the play region
        let mut v = voice(48_000, 1.0);
        v.queue.push_back(buf(0, 20, 2, 4, 3));
        v.cb = 1;
        let ev = render(&mut v, &mut vec![0f32; 2 * 30]);
        assert_eq!(ev.iter().filter(|e| e.1 == ON_LOOP_END).count(), 3);
        assert_eq!(ev.iter().filter(|e| e.1 == ON_BUFFER_END).count(), 1);
    }

    /// a step longer than the next buffer passes over it whole (it ends too), not past its end
    #[test]
    fn long_steps_pass_short_buffers() {
        let mut v = voice(RATE * 8, 1.0);
        v.cb = 1;
        for _ in 0..4 {
            v.queue.push_back(buf(0, 3, 0, 3, 0));
        }
        v.queue.push_back(buf(0, 100, 0, 100, 0));
        // (8 frames a sample: 3 + 3 + 2 into the third buffer)
        let ev = render(&mut v, &mut vec![0f32; 2]);
        assert_eq!(ev.iter().filter(|e| e.1 == ON_BUFFER_END).count(), 2);
        assert_eq!(v.pos, 2.0);
        assert_eq!(v.queue.len(), 3);
    }

    proptest! {
        /// after any pass the position is inside the front buffer's play/loop region
        #[test]
        fn position_stays_in_the_buffer(
            rate in 1000u32..=200_000, ratio in 0.001f32..=1024.0,
            bufs in proptest::collection::vec((0u32..64, 0u32..64, 0u32..64, 0u32..64, prop_oneof![Just(0u32), 1u32..5, Just(LOOP_INFINITE)]), 1..6),
            passes in 1usize..4,
        ) {
            let mut v = voice(rate, ratio);
            for (a, b, c, d, loops) in bufs {
                let (begin, end) = (a.min(b), a.max(b));
                let (lb, le) = (c.min(d), c.max(d));
                v.queue.push_back(buf(begin, end, lb, le, loops));
            }
            let mut out = vec![0f32; 2 * 64];
            for _ in 0..passes {
                render(&mut v, &mut out);
                if let (Some(b), true) = (v.queue.front(), v.began) {
                    let last = if b.looping() { b.end.max(b.loop_end) } else { b.end };
                    prop_assert!(v.pos.is_finite());
                    prop_assert!(v.pos < last as f64 || b.looping(), "pos {} last {}", v.pos, last);
                    if b.looping() && b.loops == LOOP_INFINITE {
                        prop_assert!(v.pos < b.loop_end.max(b.end) as f64);
                    }
                }
            }
        }
    }

    proptest! {
        /// buffer_region against a 64-bit model: every bound within the buffer, the play region
        /// in order, sums never wrapping
        #[test]
        fn buffer_regions_match_the_wide_model(frames in prop_oneof![0u32..300, any::<u32>()], b in any::<u32>(), l in prop_oneof![0u32..300, any::<u32>()], lb in any::<u32>(), ll in prop_oneof![0u32..300, any::<u32>()], loops in prop_oneof![Just(0u32), 1u32..4, Just(LOOP_INFINITE)]) {
            let (begin, end, lbo, leo) = buffer_region(frames, b, l, lb, ll, loops);
            let f = frames as u64;
            let mb = (b as u64).min(f);
            let me = if l == 0 { f } else { (mb + l as u64).min(f) };
            let mlb = if loops > 0 { (lb as u64).min(f) } else { mb };
            let mle = if loops > 0 && ll != 0 { (mlb + ll as u64).min(f) } else { me };
            prop_assert_eq!((begin as u64, end as u64, lbo as u64, leo as u64), (mb, me, mlb, mle));
            prop_assert!(begin <= end && end <= frames && lbo <= leo.max(lbo) && leo <= frames);
        }

        /// drain_wait: none below AHEAD; otherwise long enough that the stream is below AHEAD
        /// afterwards, and at most a millisecond longer than that; more queued, never shorter
        #[test]
        fn drain_waits_are_just_long_enough(q in 0u32..(AHEAD * 4), more in 0u32..1000) {
            let w = drain_wait(q);
            if q < AHEAD {
                prop_assert!(w.is_zero());
            } else {
                let played = (w.as_secs_f64() * RATE as f64).floor() as u64;
                prop_assert!((q as u64).saturating_sub(played) < AHEAD as u64);
                let exact = (q - AHEAD + 1) as f64 / RATE as f64;
                prop_assert!(w.as_secs_f64() <= exact.max(0.001) + 1e-6);
            }
            prop_assert!(drain_wait(q.saturating_add(more)) >= w);
        }
    }

    #[test]
    fn gate_without_a_pass_does_not_wait() {
        let g = PassGate::new();
        assert!(g.wait(Duration::from_millis(1), || {}));
        g.begin();
        g.end();
        assert!(g.wait(Duration::from_millis(1), || {}));
    }

    #[test]
    fn gate_gives_up_on_a_pass_that_never_ends() {
        let g = PassGate::new();
        g.begin();
        let t = std::time::Instant::now();
        assert!(!g.wait(Duration::from_millis(100), || {}));
        assert!(t.elapsed() >= Duration::from_millis(100));
    }

    /// DestroyVoice's order: the voice leaves the graph, then the caller waits for the running
    /// pass, then frees the callback. The engine thread must never use a callback after it was
    /// freed (thousands of interleavings, passes and destroys racing).
    #[test]
    fn destroyed_callbacks_are_not_called() {
        struct World {
            gate: PassGate,
            /// the voice's callback in the graph (0: destroyed)
            in_graph: AtomicU32,
            freed: AtomicBool,
            stop: AtomicBool,
            calls: AtomicU32,
        }
        for round in 0..200 {
            let w = Arc::new(World { gate: PassGate::new(), in_graph: AtomicU32::new(7), freed: AtomicBool::new(false), stop: AtomicBool::new(false), calls: AtomicU32::new(0) });
            let e = {
                let w = w.clone();
                std::thread::spawn(move || {
                    while !w.stop.load(SeqCst) {
                        w.gate.begin();
                        // (the pass snapshots the graph, then calls out with no lock held)
                        let cb = w.in_graph.load(SeqCst);
                        for _ in 0..3 {
                            if cb != 0 {
                                assert!(!w.freed.load(SeqCst), "callback used after it was freed");
                                w.calls.fetch_add(1, SeqCst);
                            }
                            std::hint::spin_loop();
                        }
                        w.gate.end();
                    }
                })
            };
            while w.calls.load(SeqCst) < (round % 7) {
                std::hint::spin_loop();
            }
            w.in_graph.store(0, SeqCst);
            assert!(w.gate.wait(Duration::from_secs(5), || {}));
            w.freed.store(true, SeqCst);
            for _ in 0..50 {
                std::hint::spin_loop();
            }
            w.stop.store(true, SeqCst);
            e.join().expect("engine thread");
        }
    }
}
