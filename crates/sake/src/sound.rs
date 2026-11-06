//! dsound: DirectSound in Rust. Buffers live in guest memory (Lock hands out pointers into
//! them); one host thread mixes the playing ones into a 44.1 kHz stereo stream of kura's
//! audio output, paced by how much that stream still has queued. COM objects are a guest
//! block whose first word is a vtable of trap addresses.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::api::{host_fn, implement, Ctx};
use crate::mem::{self, put32, r16, r32, w16, w32};

pub(crate) const RATE: u32 = 44100;
/// output frames per mixing step (10 ms)
pub(crate) const STEP: usize = 441;
/// frames kept queued in the output stream (the play cursor's lead over what is heard)
pub(crate) const AHEAD: u32 = 3 * STEP as u32;

const E_NOINTERFACE: u32 = 0x8000_4002;
const DSERR_INVALIDPARAM: u32 = 0x8007_0057;

#[derive(Clone, Copy)]
pub(crate) struct Fmt {
    pub rate: u32,
    pub channels: u32,
    pub bits: u32,
    pub float: bool,
}

impl Fmt {
    pub fn block(&self) -> u32 {
        self.channels * self.bits / 8
    }
    fn read(p: u32) -> Fmt {
        let tag = r16(p);
        let bits = r16(p + 14) as u32;
        let float = tag == 3 || (tag == 0xfffe && r32(p + 24) == 3);
        Fmt { rate: r32(p + 4), channels: (r16(p + 2) as u32).clamp(1, 2), bits, float }
    }
    fn write(&self, p: u32) {
        w16(p, if self.float { 3 } else { 1 });
        w16(p + 2, self.channels as u16);
        w32(p + 4, self.rate);
        w32(p + 8, self.rate * self.block());
        w16(p + 12, self.block() as u16);
        w16(p + 14, self.bits as u16);
        w16(p + 16, 0);
    }
}

struct Buf {
    data: u32,
    size: u32,
    fmt: Fmt,
    freq: u32,
    flags: u32,
    primary: bool,
    playing: bool,
    looping: bool,
    /// play position in frames
    pos: f64,
    gain: (f32, f32),
    volume: i32,
    pan: i32,
    notify: Vec<(u32, u32)>,
    refs: u32,
    /// the buffer owning `data` (duplicates share it)
    owner: u32,
}

struct Dev {
    bufs: HashMap<u32, Buf>,
    waves: HashMap<u32, Wave>,
    stream: i32,
}

static DEV: Mutex<Option<Dev>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Dev) -> R) -> R {
    let mut g = DEV.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Dev { bufs: HashMap::default(), waves: HashMap::default(), stream: -1 }))
}

fn gains(volume: i32, pan: i32) -> (f32, f32) {
    let v = 10f32.powf(volume.clamp(-10000, 0) as f32 / 2000.0);
    let l = if pan > 0 { 10f32.powf(-pan.min(10000) as f32 / 2000.0) } else { 1.0 };
    let r = if pan < 0 { 10f32.powf(pan.max(-10000) as f32 / 2000.0) } else { 1.0 };
    let v = if volume <= -10000 { 0.0 } else { v };
    (v * l, v * r)
}

// ---------------------------------------------------------------------------------------------
// the mixer

fn sample(b: &Buf, frame: usize) -> (f32, f32) {
    sample_at(b.data, &b.fmt, frame)
}
pub(crate) fn sample_at(data: u32, f: &Fmt, frame: usize) -> (f32, f32) {
    let at = data + frame as u32 * f.block();
    let one = |a: u32| -> f32 {
        match (f.bits, f.float) {
            (8, _) => (mem::r8(a) as f32 - 128.0) / 128.0,
            (16, _) => r16(a) as i16 as f32 / 32768.0,
            (32, true) => f32::from_bits(r32(a)),
            (32, false) => r32(a) as i32 as f32 / 2_147_483_648.0,
            (24, _) => ((r32(a) << 8) as i32 >> 8) as f32 / 8_388_608.0,
            _ => 0.0,
        }
    };
    let l = one(at);
    let r = if f.channels >= 2 { one(at + f.bits / 8) } else { l };
    (l, r)
}

/// Mixes STEP frames of every playing buffer into `out`; fires notifications.
fn mix(d: &mut Dev, out: &mut [f32]) -> Vec<u32> {
    let mut events = Vec::new();
    if mix_waves(d, out) {
        WAVE_CV.notify_all();
    }
    for b in d.bufs.values_mut() {
        if !b.playing || b.primary || b.size == 0 {
            continue;
        }
        let block = b.fmt.block().max(1);
        let frames = (b.size / block) as usize;
        if frames == 0 {
            continue;
        }
        let step = b.freq as f64 / RATE as f64;
        let start_byte = (b.pos as u32) * block;
        let (gl, gr) = b.gain;
        let mut stopped = false;
        for o in out.chunks_exact_mut(2) {
            let i = b.pos as usize;
            let t = (b.pos - i as f64) as f32;
            let (l0, r0) = sample(b, i);
            let next = if i + 1 < frames { i + 1 } else if b.looping { 0 } else { i };
            let (l1, r1) = sample(b, next);
            o[0] += (l0 + (l1 - l0) * t) * gl;
            o[1] += (r0 + (r1 - r0) * t) * gr;
            b.pos += step;
            if b.pos >= frames as f64 {
                if b.looping {
                    b.pos -= frames as f64;
                } else {
                    b.pos = 0.0;
                    b.playing = false;
                    stopped = true;
                    break;
                }
            }
        }
        let end_byte = (b.pos as u32) * block;
        for &(off, ev) in &b.notify {
            if off == 0xffff_ffff {
                if stopped {
                    events.push(ev);
                }
                continue;
            }
            let crossed = if stopped {
                off >= start_byte
            } else if end_byte >= start_byte {
                off >= start_byte && off < end_byte
            } else {
                off >= start_byte || off < end_byte
            };
            if crossed {
                events.push(ev);
            }
        }
    }
    events
}

// ---------------------------------------------------------------------------------------------
// winmm waveOut: a queue of WAVEHDRs the mixer plays in order; a guest thread per device
// reports the finished ones (WOM_DONE to a function, window, thread or event)

const WHDR_DONE: u32 = 1;
const WHDR_PREPARED: u32 = 2;
const WHDR_INQUEUE: u32 = 0x10;
const WOM_OPEN: u32 = 0x3bb;
const WOM_CLOSE: u32 = 0x3bc;
const WOM_DONE: u32 = 0x3bd;

struct Wave {
    fmt: Fmt,
    queue: std::collections::VecDeque<u32>,
    /// play position in frames inside the first queued header
    pos: f64,
    done: Vec<u32>,
    paused: bool,
    closing: bool,
    /// bytes played since open/reset (waveOutGetPosition)
    played: u64,
    cb: (u32, u32, u32),
    volume: u32,
}

static WAVE_CV: std::sync::Condvar = std::sync::Condvar::new();
static WAVE_LOCK: Mutex<()> = Mutex::new(());
static NEXT_WAVE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x5a00);

/// Mixes the waveOut devices; true when a header finished.
fn mix_waves(d: &mut Dev, out: &mut [f32]) -> bool {
    let mut finished = false;
    for w in d.waves.values_mut() {
        if w.paused {
            continue;
        }
        let block = w.fmt.block().max(1);
        let step = w.fmt.rate as f64 / RATE as f64;
        let (gl, gr) = ((w.volume & 0xffff) as f32 / 65535.0, (w.volume >> 16) as f32 / 65535.0);
        for o in out.chunks_exact_mut(2) {
            let Some(&h) = w.queue.front() else { break };
            let frames = (r32(h + 4) / block) as usize;
            let i = w.pos as usize;
            if i >= frames {
                w.queue.pop_front();
                w32(h + 16, (r32(h + 16) | WHDR_DONE) & !WHDR_INQUEUE);
                w.done.push(h);
                w.played += (frames as u32 * block) as u64;
                w.pos = 0.0;
                finished = true;
                continue;
            }
            let (l, r) = sample_at(r32(h), &w.fmt, i);
            o[0] += l * gl;
            o[1] += r * gr;
            w.pos += step;
        }
        // a header that ended exactly on this step
        if let Some(&h) = w.queue.front() {
            if w.pos as usize >= (r32(h + 4) / block) as usize {
                w.queue.pop_front();
                w32(h + 16, (r32(h + 16) | WHDR_DONE) & !WHDR_INQUEUE);
                w.done.push(h);
                w.played += r32(h + 4) as u64;
                w.pos = 0.0;
                finished = true;
            }
        }
    }
    finished
}

fn wave_notify(c: &mut Ctx, hwo: u32, (kind, cb, inst): (u32, u32, u32), msg: u32, p1: u32) {
    match kind {
        0x30000 if cb != 0 => {
            c.call(cb, &[hwo, msg, inst, p1, 0]);
        }
        0x10000 if cb != 0 => {
            crate::user::post(cb, msg, hwo, p1);
        }
        0x20000 if cb != 0 => crate::user::post_thread_msg(cb, msg, hwo, p1),
        0x50000 if cb != 0 => crate::k32::set_event(cb),
        _ => {}
    }
}

/// The device's guest thread: delivers WOM_DONE for finished headers until the device closes.
fn wave_thread(c: &mut Ctx) -> u32 {
    let hwo = c.a(0);
    loop {
        let (done, cb, closing) = with(|d| match d.waves.get_mut(&hwo) {
            Some(w) => (std::mem::take(&mut w.done), w.cb, w.closing),
            None => (Vec::new(), (0, 0, 0), true),
        });
        for h in &done {
            wave_notify(c, hwo, cb, WOM_DONE, *h);
        }
        if closing {
            with(|d| d.waves.remove(&hwo));
            break;
        }
        if done.is_empty() {
            drop(WAVE_CV.wait_timeout(WAVE_LOCK.lock().unwrap(), Duration::from_millis(20)).unwrap());
        }
    }
    crate::k32::exit_thread(0)
}

/// no mixer devices (MMSYSERR_BADDEVICEID)
fn mixerOpen(c: &mut Ctx) -> u32 {
    put32(c.a(0), 0);
    2
}
fn mixerGetDevCaps(_: &mut Ctx) -> u32 {
    2
}
fn waveOutGetNumDevs(_: &mut Ctx) -> u32 {
    1
}
fn wave_caps(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(1);
    let name = "sake WebAudio";
    let o = if wide { 8 + 64 } else { 8 + 32 };
    for i in 0..o + 12 {
        mem::w8(p + i, 0);
    }
    w16(p, 1);
    w16(p + 2, 2);
    w32(p + 4, 0x100);
    for (i, b) in name.bytes().enumerate() {
        if wide {
            w16(p + 8 + 2 * i as u32, b as u16);
        } else {
            mem::w8(p + 8 + i as u32, b);
        }
    }
    w32(p + o, 0xfff);
    w16(p + o + 4, 2);
    w32(p + o + 8, 0xc);
    0
}
fn waveOutGetDevCapsA(c: &mut Ctx) -> u32 {
    wave_caps(c, false)
}
fn waveOutGetDevCapsW(c: &mut Ctx) -> u32 {
    wave_caps(c, true)
}
/// waveOutOpen(phwo, device, format, callback, instance, flags)
fn waveOutOpen(c: &mut Ctx) -> u32 {
    let (ph, fmtp, cb, inst, flags) = (c.a(0), c.a(2), c.a(3), c.a(4), c.a(5));
    let fmt = Fmt::read(fmtp);
    if !matches!(fmt.bits, 8 | 16 | 24 | 32) || fmt.rate == 0 {
        return 32; // WAVERR_BADFORMAT
    }
    if flags & 1 != 0 {
        return 0;
    }
    start_mixer();
    let hwo = NEXT_WAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let cbk = (flags & 0x70000, cb, inst);
    with(|d| {
        d.waves.insert(hwo, Wave { fmt, queue: Default::default(), pos: 0.0, done: Vec::new(), paused: false, closing: false, played: 0, cb: cbk, volume: 0xffff_ffff })
    });
    put32(ph, hwo);
    static ENTRY: OnceLock<u32> = OnceLock::new();
    let entry = *ENTRY.get_or_init(|| host_fn("wave_thread", wave_thread, 4));
    crate::spawn_gated(entry, hwo, 0x10000, crate::obj::new_thread_obj(0), false);
    wave_notify(c, hwo, cbk, WOM_OPEN, 0);
    0
}
fn waveOutClose(c: &mut Ctx) -> u32 {
    let hwo = c.a(0);
    let Some(cb) = with(|d| d.waves.get_mut(&hwo).map(|w| {
        w.closing = true;
        w.cb
    })) else { return 5 };
    WAVE_CV.notify_all();
    wave_notify(c, hwo, cb, WOM_CLOSE, 0);
    // (the device thread drops it on its next look)
    0
}
fn waveOutPrepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    w32(h + 16, r32(h + 16) | WHDR_PREPARED);
    0
}
fn waveOutUnprepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    if r32(h + 16) & WHDR_INQUEUE != 0 {
        return 33; // WAVERR_STILLPLAYING
    }
    w32(h + 16, r32(h + 16) & !WHDR_PREPARED);
    0
}
fn waveOutWrite(c: &mut Ctx) -> u32 {
    let (hwo, h) = (c.a(0), c.a(1));
    w32(h + 16, (r32(h + 16) & !WHDR_DONE) | WHDR_INQUEUE);
    if with(|d| d.waves.get_mut(&hwo).map(|w| w.queue.push_back(h))).is_none() {
        return 5; // MMSYSERR_INVALHANDLE
    }
    0
}
fn waveOutReset(c: &mut Ctx) -> u32 {
    let hwo = c.a(0);
    with(|d| {
        if let Some(w) = d.waves.get_mut(&hwo) {
            for h in w.queue.drain(..) {
                w32(h + 16, (r32(h + 16) | WHDR_DONE) & !WHDR_INQUEUE);
                w.done.push(h);
            }
            w.pos = 0.0;
            w.played = 0;
            w.paused = false;
        }
    });
    WAVE_CV.notify_all();
    0
}
fn waveOutPause(c: &mut Ctx) -> u32 {
    with(|d| d.waves.get_mut(&c.a(0)).map(|w| w.paused = true));
    0
}
fn waveOutRestart(c: &mut Ctx) -> u32 {
    with(|d| d.waves.get_mut(&c.a(0)).map(|w| w.paused = false));
    0
}
/// waveOutGetPosition(hwo, MMTIME*, size): bytes, samples or ms
fn waveOutGetPosition(c: &mut Ctx) -> u32 {
    let (hwo, t) = (c.a(0), c.a(1));
    let Some((bytes, fmt)) = with(|d| d.waves.get(&hwo).map(|w| (w.played + (w.pos as u64) * w.fmt.block() as u64, w.fmt))) else { return 5 };
    let frames = bytes / fmt.block().max(1) as u64;
    match r32(t) {
        2 => w32(t + 4, frames as u32),
        1 => w32(t + 4, (frames * 1000 / fmt.rate as u64) as u32),
        _ => {
            w32(t, 4);
            w32(t + 4, bytes as u32)
        }
    }
    0
}
fn waveOutSetVolume(c: &mut Ctx) -> u32 {
    let (hwo, v) = (c.a(0), c.a(1));
    // a handle, or a device id: every open device
    with(|d| match d.waves.get_mut(&hwo) {
        Some(w) => w.volume = v,
        None => d.waves.values_mut().for_each(|w| w.volume = v),
    });
    0
}
fn waveOutGetVolume(c: &mut Ctx) -> u32 {
    let v = with(|d| d.waves.get(&c.a(0)).map_or(0xffff_ffff, |w| w.volume));
    put32(c.a(1), v);
    0
}

fn start_mixer() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let id = kura::audio::open();
        with(|d| d.stream = id);
        if crate::env_on!("SAKE_NO_MIXER") {
            return;
        }
        std::thread::Builder::new()
            .name("dsound mixer".into())
            .spawn(move || {
                let mut out = vec![0f32; STEP * 2];
                let stats = crate::env_on!("SAKE_MIX_STATS");
                let (mut t_mix, mut t_q, mut t_w, mut t_all, mut n) = (0f64, 0f64, 0f64, std::time::Instant::now(), 0);
                // (a condvar's timed wait, not thread::sleep: in the browser emscripten's sleep
                // cost the thread about 0.15 cores)
                let (lock, cv) = (Mutex::new(()), std::sync::Condvar::new());
                loop {
                    drop(cv.wait_timeout(lock.lock().unwrap(), Duration::from_millis(10)).unwrap());
                    loop {
                        let t0 = std::time::Instant::now();
                        let q = kura::audio::queued(id);
                        t_q += t0.elapsed().as_secs_f64();
                        if q >= AHEAD {
                            break;
                        }
                        let t0 = std::time::Instant::now();
                        out.iter_mut().for_each(|x| *x = 0.0);
                        let ev = with(|d| mix(d, &mut out));
                        for e in ev {
                            crate::k32::set_event(e);
                        }
                        t_mix += t0.elapsed().as_secs_f64();
                        let t0 = std::time::Instant::now();
                        kura::audio::write(id, &out);
                        t_w += t0.elapsed().as_secs_f64();
                        n += 1;
                    }
                    if stats && t_all.elapsed().as_secs() >= 5 {
                        let s = t_all.elapsed().as_secs_f64();
                        eprintln!("sake mixer: {n} chunks/{s:.1}s, mix {:.1} ms/s, queued {:.1} ms/s, write {:.1} ms/s", t_mix * 1e3 / s, t_q * 1e3 / s, t_w * 1e3 / s);
                        (t_mix, t_q, t_w, t_all, n) = (0.0, 0.0, 0.0, std::time::Instant::now(), 0);
                    }
                }
            })
            .expect("mixer thread");
    });
}

// ---------------------------------------------------------------------------------------------
// COM plumbing

pub(crate) use crate::api::vtable;

type Handler = crate::api::Handler;

fn ds_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirectSound8",
            &[
                (DS_QueryInterface, 12),
                (DS_AddRef, 4),
                (DS_Release, 4),
                (DS_CreateSoundBuffer, 16),
                (DS_GetCaps, 8),
                (DS_DuplicateSoundBuffer, 12),
                (DS_SetCooperativeLevel, 12),
                (DS_Compact, 4),
                (DS_GetSpeakerConfig, 8),
                (DS_SetSpeakerConfig, 8),
                (DS_Initialize, 8),
                (DS_VerifyCertification, 8),
            ],
        )
    })
}

fn buf_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IDirectSoundBuffer8",
            &[
                (B_QueryInterface, 12),
                (B_AddRef, 4),
                (B_Release, 4),
                (B_GetCaps, 8),
                (B_GetCurrentPosition, 12),
                (B_GetFormat, 16),
                (B_GetVolume, 8),
                (B_GetPan, 8),
                (B_GetFrequency, 8),
                (B_GetStatus, 8),
                (B_Initialize, 12),
                (B_Lock, 32),
                (B_Play, 16),
                (B_SetCurrentPosition, 8),
                (B_SetFormat, 8),
                (B_SetVolume, 8),
                (B_SetPan, 8),
                (B_SetFrequency, 8),
                (B_Stop, 4),
                (B_Unlock, 20),
                (B_Restore, 4),
                (B_SetFX, 16),
                (B_AcquireResources, 16),
                (B_GetObjectInPath, 20),
            ],
        )
    })
}

fn notify_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| vtable("IDirectSoundNotify", &[(N_QueryInterface, 12), (N_AddRef, 4), (N_Release, 4), (N_SetNotificationPositions, 12)]))
}

fn new_object(vtbl: u32, extra: u32) -> u32 {
    let o = mem::heap_alloc(16, true);
    w32(o, vtbl);
    w32(o + 4, extra);
    o
}

static DS_REFS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);

fn create_device() -> u32 {
    start_mixer();
    let o = new_object(ds_vtbl(), 0);
    DS_REFS.lock().unwrap().get_or_insert_with(HashMap::default).insert(o, 1);
    o
}

fn no_sound() -> bool {
    crate::env_on!("SAKE_NO_SOUND")
}

fn DirectSoundCreate(c: &mut Ctx) -> u32 {
    if no_sound() {
        put32(c.a(1), 0);
        return 0x8878_0078;
    }
    put32(c.a(1), create_device());
    0
}
fn DirectSoundCreate8(c: &mut Ctx) -> u32 {
    if no_sound() {
        put32(c.a(1), 0);
        return 0x8878_0078;
    }
    put32(c.a(1), create_device());
    0
}
fn enumerate(c: &mut Ctx, wide: bool) -> u32 {
    let (cb, ctx) = (c.a(0), c.a(1));
    let buf = mem::heap_alloc(256, true);
    let guid = buf;
    let desc = buf + 16;
    let module = buf + 160;
    if wide {
        let d: Vec<u16> = "Primary Sound Driver".encode_utf16().collect();
        mem::put_wstr(desc, 64, &d);
    } else {
        mem::put_cstr(desc, 64, b"Primary Sound Driver");
    }
    let _ = guid;
    c.call(cb, &[0, desc, module, ctx]);
    mem::heap_free(buf);
    0
}
fn DirectSoundEnumerateA(c: &mut Ctx) -> u32 {
    enumerate(c, false)
}
fn DirectSoundEnumerateW(c: &mut Ctx) -> u32 {
    enumerate(c, true)
}

/// CoCreateInstance(CLSID_DirectSound / DirectSound8).
pub fn create_instance(_c: &mut Ctx, clsid: u32, _iid: u32) -> Option<u32> {
    match r32(clsid) {
        0x47d4_d946 | 0x3901_cc3f => Some(create_device()),
        _ => None,
    }
}

fn DS_QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    match r32(iid) {
        0 | 0x279a_fa83 | 0xc50a_7e93 => {
            DS_AddRef(c);
            put32(out, this);
            0
        }
        _ => {
            put32(out, 0);
            E_NOINTERFACE
        }
    }
}
fn DS_AddRef(c: &mut Ctx) -> u32 {
    let mut g = DS_REFS.lock().unwrap();
    let r = g.get_or_insert_with(HashMap::default).entry(c.a(0)).or_insert(1);
    *r += 1;
    *r
}
fn DS_Release(c: &mut Ctx) -> u32 {
    let mut g = DS_REFS.lock().unwrap();
    let r = g.get_or_insert_with(HashMap::default).entry(c.a(0)).or_insert(1);
    *r = r.saturating_sub(1);
    *r
}
fn DS_CreateSoundBuffer(c: &mut Ctx) -> u32 {
    let (desc, out) = (c.a(1), c.a(2));
    if desc == 0 || out == 0 {
        return DSERR_INVALIDPARAM;
    }
    let flags = r32(desc + 4);
    let primary = flags & 1 != 0;
    let bytes = r32(desc + 8);
    let wfx = r32(desc + 16);
    let fmt = if wfx != 0 { Fmt::read(wfx) } else { Fmt { rate: 22050, channels: 2, bits: 16, float: false } };
    let size = if primary { 0 } else { bytes };
    let data = if size > 0 { mem::heap_alloc(size, true) } else { 0 };
    let o = new_object(buf_vtbl(), 0);
    let gain = gains(0, 0);
    with(|d| {
        d.bufs.insert(
            o,
            Buf {
                data,
                size,
                fmt,
                freq: fmt.rate,
                flags,
                primary,
                playing: false,
                looping: false,
                pos: 0.0,
                gain,
                volume: 0,
                pan: 0,
                notify: Vec::new(),
                refs: 1,
                owner: o,
            },
        )
    });
    put32(out, o);
    0
}
fn DS_GetCaps(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let n = r32(p).clamp(4, 96);
    mem::fill(p + 4, n - 4, 0);
    w32(p + 4, 0x0f5f);
    w32(p + 8, 100);
    w32(p + 12, 200_000);
    w32(p + 16, 1);
    w32(p + 20, 64);
    w32(p + 24, 64);
    w32(p + 28, 64);
    0
}
fn DS_DuplicateSoundBuffer(c: &mut Ctx) -> u32 {
    let (src, out) = (c.a(1), c.a(2));
    let o = new_object(buf_vtbl(), 0);
    let ok = with(|d| {
        let Some(s) = d.bufs.get(&src) else { return false };
        let b = Buf {
            data: s.data,
            size: s.size,
            fmt: s.fmt,
            freq: s.freq,
            flags: s.flags,
            primary: s.primary,
            playing: false,
            looping: false,
            pos: 0.0,
            gain: s.gain,
            volume: s.volume,
            pan: s.pan,
            notify: Vec::new(),
            refs: 1,
            owner: s.owner,
        };
        d.bufs.insert(o, b);
        true
    });
    if !ok {
        return DSERR_INVALIDPARAM;
    }
    put32(out, o);
    0
}
fn DS_SetCooperativeLevel(_: &mut Ctx) -> u32 {
    0
}
fn DS_Compact(_: &mut Ctx) -> u32 {
    0
}
fn DS_GetSpeakerConfig(c: &mut Ctx) -> u32 {
    put32(c.a(1), 4);
    0
}
fn DS_SetSpeakerConfig(_: &mut Ctx) -> u32 {
    0
}
fn DS_Initialize(_: &mut Ctx) -> u32 {
    0
}
fn DS_VerifyCertification(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    0
}

fn with_buf<R>(this: u32, f: impl FnOnce(&mut Buf) -> R) -> Option<R> {
    with(|d| d.bufs.get_mut(&this).map(f))
}

fn B_QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    match r32(iid) {
        0 | 0x279a_fa85 | 0x6825_a449 => {
            with_buf(this, |b| b.refs += 1);
            put32(out, this);
            0
        }
        0xb021_0783 => {
            with_buf(this, |b| b.refs += 1);
            put32(out, new_object(notify_vtbl(), this));
            0
        }
        // IDirectSound3DListener (the primary buffer) / IDirectSound3DBuffer: accepted, the sound
        // is not positioned
        0x279a_fa84 => {
            put32(out, new_object(listener3d_vtbl(), this));
            0
        }
        0x279a_fa86 => {
            put32(out, new_object(buffer3d_vtbl(), this));
            0
        }
        _ => {
            put32(out, 0);
            E_NOINTERFACE
        }
    }
}

fn listener3d_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let mut m: Vec<(Handler, u32)> = vec![(N_QueryInterface, 12), (N_AddRef, 4), (N_Release, 4)];
        // Get{AllParameters,DistanceFactor,DopplerFactor,Orientation,Position,RolloffFactor,Velocity},
        // Set{AllParameters,DistanceFactor,DopplerFactor,Orientation,Position,RolloffFactor,Velocity},
        // CommitDeferredSettings
        for b in [8, 8, 8, 12, 8, 8, 8, 12, 12, 12, 32, 20, 12, 20, 4] {
            m.push((ok3d, b));
        }
        vtable("IDirectSound3DListener", &m)
    })
}

fn buffer3d_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let mut m: Vec<(Handler, u32)> = vec![(N_QueryInterface, 12), (N_AddRef, 4), (N_Release, 4)];
        // Get{AllParameters,ConeAngles,ConeOrientation,ConeOutsideVolume,MaxDistance,MinDistance,
        // Mode,Position,Velocity}, Set{the same}
        for b in [8, 12, 8, 8, 8, 8, 8, 8, 8, 12, 16, 20, 12, 12, 12, 12, 20, 20] {
            m.push((ok3d, b));
        }
        vtable("IDirectSound3DBuffer", &m)
    })
}

fn ok3d(_: &mut Ctx) -> u32 {
    0
}
fn B_AddRef(c: &mut Ctx) -> u32 {
    with_buf(c.a(0), |b| {
        b.refs += 1;
        b.refs
    })
    .unwrap_or(1)
}
fn B_Release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let (left, data) = with(|d| {
        let Some(b) = d.bufs.get_mut(&this) else { return (0, None) };
        b.refs = b.refs.saturating_sub(1);
        if b.refs > 0 {
            return (b.refs, None);
        }
        let b = d.bufs.remove(&this).unwrap();
        // the data goes once neither the buffer nor a duplicate of it is left
        let shared = d.bufs.values().any(|x| x.owner == b.owner);
        (0, (!shared && b.data != 0).then_some(b.data))
    });
    if let Some(p) = data {
        mem::heap_free(p);
    }
    if left == 0 {
        mem::heap_free(this);
    }
    left
}
fn B_GetCaps(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    with_buf(c.a(0), |b| {
        w32(p + 4, b.flags);
        w32(p + 8, b.size);
        w32(p + 12, 0);
        w32(p + 16, 0);
    });
    0
}
fn B_GetCurrentPosition(c: &mut Ctx) -> u32 {
    let (this, play, write) = (c.a(0), c.a(1), c.a(2));
    with_buf(this, |b| {
        let block = b.fmt.block().max(1);
        let p = (b.pos as u32 * block).min(b.size.saturating_sub(block));
        let lead = (b.freq / 100) * block;
        put32(play, p);
        put32(write, if b.size > 0 { (p + lead) % b.size } else { 0 });
    });
    0
}
fn B_GetFormat(c: &mut Ctx) -> u32 {
    let (this, wfx, size, written) = (c.a(0), c.a(1), c.a(2), c.a(3));
    put32(written, 18);
    if wfx != 0 && size >= 16 {
        with_buf(this, |b| b.fmt.write(wfx));
    }
    0
}
fn B_GetVolume(c: &mut Ctx) -> u32 {
    put32(c.a(1), with_buf(c.a(0), |b| b.volume).unwrap_or(0) as u32);
    0
}
fn B_GetPan(c: &mut Ctx) -> u32 {
    put32(c.a(1), with_buf(c.a(0), |b| b.pan).unwrap_or(0) as u32);
    0
}
fn B_GetFrequency(c: &mut Ctx) -> u32 {
    put32(c.a(1), with_buf(c.a(0), |b| b.freq).unwrap_or(RATE));
    0
}
fn B_GetStatus(c: &mut Ctx) -> u32 {
    let s = with_buf(c.a(0), |b| if b.playing { 1 | if b.looping { 4 } else { 0 } } else { 0 }).unwrap_or(0);
    put32(c.a(1), s);
    0
}
fn B_Initialize(_: &mut Ctx) -> u32 {
    0x8878_0082
}
fn B_Lock(c: &mut Ctx) -> u32 {
    let (this, off, bytes, p1, n1, p2, n2, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    let r = with_buf(this, |b| {
        if b.size == 0 {
            return None;
        }
        let block = b.fmt.block().max(1);
        let mut off = off;
        if flags & 1 != 0 {
            off = (b.pos as u32 * block + (b.freq / 100) * block) % b.size;
        }
        let bytes = if flags & 2 != 0 { b.size } else { bytes.min(b.size) };
        let off = off % b.size;
        let first = bytes.min(b.size - off);
        Some((b.data + off, first, b.data, bytes - first))
    })
    .flatten();
    match r {
        Some((a1, l1, a2, l2)) => {
            put32(p1, a1);
            put32(n1, l1);
            put32(p2, if l2 > 0 { a2 } else { 0 });
            put32(n2, l2);
            0
        }
        None => DSERR_INVALIDPARAM,
    }
}
fn B_Play(c: &mut Ctx) -> u32 {
    let (this, flags) = (c.a(0), c.a(3));
    with_buf(this, |b| {
        b.playing = true;
        b.looping = flags & 1 != 0;
    });
    0
}
fn B_SetCurrentPosition(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    with_buf(this, |b| b.pos = (p / b.fmt.block().max(1)) as f64);
    0
}
fn B_SetFormat(c: &mut Ctx) -> u32 {
    let (this, wfx) = (c.a(0), c.a(1));
    if wfx != 0 {
        let f = Fmt::read(wfx);
        with_buf(this, |b| {
            b.fmt = f;
            b.freq = f.rate;
        });
    }
    0
}
fn B_SetVolume(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.i(1));
    with_buf(this, |b| {
        b.volume = v;
        b.gain = gains(v, b.pan);
    });
    0
}
fn B_SetPan(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.i(1));
    with_buf(this, |b| {
        b.pan = v;
        b.gain = gains(b.volume, v);
    });
    0
}
fn B_SetFrequency(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1));
    with_buf(this, |b| b.freq = if v == 0 { b.fmt.rate } else { v.clamp(100, 200_000) });
    0
}
fn B_Stop(c: &mut Ctx) -> u32 {
    let stop_events: Vec<u32> = with_buf(c.a(0), |b| {
        let was = b.playing;
        b.playing = false;
        if was {
            b.notify.iter().filter(|n| n.0 == 0xffff_ffff).map(|n| n.1).collect()
        } else {
            Vec::new()
        }
    })
    .unwrap_or_default();
    for e in stop_events {
        crate::k32::set_event(e);
    }
    0
}
fn B_Unlock(_: &mut Ctx) -> u32 {
    0
}
fn B_Restore(_: &mut Ctx) -> u32 {
    0
}
fn B_SetFX(_: &mut Ctx) -> u32 {
    0x8000_4001
}
fn B_AcquireResources(_: &mut Ctx) -> u32 {
    0
}
fn B_GetObjectInPath(_: &mut Ctx) -> u32 {
    E_NOINTERFACE
}

fn N_QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    let buf = r32(this + 4);
    if r32(iid) == 0xb021_0783 || r32(iid) == 0 {
        put32(out, this);
        return 0;
    }
    let _ = buf;
    put32(out, 0);
    E_NOINTERFACE
}
fn N_AddRef(_: &mut Ctx) -> u32 {
    1
}
fn N_Release(_: &mut Ctx) -> u32 {
    0
}
fn N_SetNotificationPositions(c: &mut Ctx) -> u32 {
    let (this, n, p) = (c.a(0), c.a(1), c.a(2));
    let buf = r32(this + 4);
    let list: Vec<(u32, u32)> = (0..n).map(|i| (r32(p + 8 * i), r32(p + 8 * i + 4))).collect();
    with_buf(buf, |b| b.notify = list);
    0
}

// ---------------------------------------------------------------------------------------------
// PlaySound / sndPlaySound: one sound at a time (a new one stops the last), a .wav file or a
// RIFF image in memory, played as a buffer of the mixer's own

/// the buffer's key in `Dev::bufs` (not a guest address)
const PLAY_SOUND: u32 = 0xffff_fff0;
const SND_ASYNC: u32 = 1;
const SND_MEMORY: u32 = 4;
const SND_LOOP: u32 = 8;
const SND_NOSTOP: u32 = 0x10;
const SND_PURGE: u32 = 0x40;

/// The "fmt " and "data" chunks of a RIFF WAVE image: their offsets, and data's length
fn wave_chunks(b: &[u8]) -> Option<(usize, usize, usize)> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let (mut at, mut fmt, mut data) = (12, None, None);
    while at + 8 <= b.len() {
        let n = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
        match &b[at..at + 4] {
            b"fmt " => fmt = Some(at + 8),
            b"data" => data = Some((at + 8, n.min(b.len() - at - 8))),
            _ => {}
        }
        at += 8 + n + (n & 1);
    }
    Some((fmt?, data?.0, data?.1))
}

fn play_sound(name: u32, flags: u32, wide: bool) -> u32 {
    let playing = with(|d| d.bufs.get(&PLAY_SOUND).is_some_and(|b| b.playing));
    if flags & SND_NOSTOP != 0 && playing {
        return 0;
    }
    // (owner: the image's start; data points into it)
    let old = with(|d| d.bufs.remove(&PLAY_SOUND)).map(|b| b.owner);
    if let Some(p) = old {
        mem::heap_free(p);
    }
    if name == 0 || flags & SND_PURGE != 0 || no_sound() {
        return 1;
    }
    let image = if flags & SND_MEMORY != 0 {
        // (a RIFF image: its length from its header)
        mem::slice(name, 8 + r32(name + 4)).to_vec()
    } else {
        let path = crate::k32::arg_str(name, wide);
        match crate::files::resolve(&path).and_then(|h| kura::fs::read(h).ok()) {
            Some(b) => b,
            None => return 0,
        }
    };
    let Some((fmt, data, size)) = wave_chunks(&image) else { return 0 };
    let p = mem::heap_alloc(image.len() as u32, false);
    mem::slice_mut(p, image.len() as u32).copy_from_slice(&image);
    let f = Fmt::read(p + fmt as u32);
    if f.bits == 0 || f.rate == 0 {
        mem::heap_free(p);
        return 0;
    }
    start_mixer();
    with(|d| {
        d.bufs.insert(
            PLAY_SOUND,
            Buf {
                data: p + data as u32,
                size: size as u32,
                fmt: f,
                freq: f.rate,
                flags: 0,
                primary: false,
                playing: true,
                looping: flags & SND_LOOP != 0,
                pos: 0.0,
                gain: gains(0, 0),
                volume: 0,
                pan: 0,
                notify: Vec::new(),
                refs: 1,
                owner: p,
            },
        )
    });
    if flags & SND_ASYNC == 0 && flags & SND_LOOP == 0 {
        // synchronous: back when it has played
        let ms = size as u64 * 1000 / (f.rate * f.block()).max(1) as u64;
        std::thread::sleep(Duration::from_millis(ms));
    }
    1
}
fn PlaySoundA(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), c.a(2), false)
}
fn PlaySoundW(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), c.a(2), true)
}
fn sndPlaySoundA(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), c.a(1), false)
}
fn sndPlaySoundW(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), c.a(1), true)
}

pub fn init() {
    for (n, f) in [
        ("waveOutGetNumDevs", waveOutGetNumDevs as crate::api::Handler),
        ("waveOutGetDevCapsA", waveOutGetDevCapsA),
        ("waveOutGetDevCapsW", waveOutGetDevCapsW),
        ("waveOutOpen", waveOutOpen),
        ("waveOutClose", waveOutClose),
        ("waveOutPrepareHeader", waveOutPrepareHeader),
        ("waveOutUnprepareHeader", waveOutUnprepareHeader),
        ("waveOutWrite", waveOutWrite),
        ("waveOutReset", waveOutReset),
        ("waveOutPause", waveOutPause),
        ("waveOutRestart", waveOutRestart),
        ("waveOutGetPosition", waveOutGetPosition),
        ("waveOutSetVolume", waveOutSetVolume),
        ("waveOutGetVolume", waveOutGetVolume),
    ] {
        implement("winmm", n, f);
    }
    implement("winmm", "mixerOpen", mixerOpen);
    implement("winmm", "PlaySoundA", PlaySoundA);
    implement("winmm", "PlaySound", PlaySoundA);
    implement("winmm", "PlaySoundW", PlaySoundW);
    implement("winmm", "sndPlaySoundA", sndPlaySoundA);
    implement("winmm", "sndPlaySoundW", sndPlaySoundW);
    implement("winmm", "mixerGetDevCapsA", mixerGetDevCaps);
    implement("winmm", "mixerGetDevCapsW", mixerGetDevCaps);
    implement("dsound", "DirectSoundCreate", DirectSoundCreate);
    implement("dsound", "DirectSoundCreate8", DirectSoundCreate8);
    implement("dsound", "DirectSoundEnumerateA", DirectSoundEnumerateA);
    implement("dsound", "DirectSoundEnumerateW", DirectSoundEnumerateW);
}
