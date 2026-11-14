//! dsound: DirectSound in Rust. Buffers live in guest memory (Lock hands out pointers into
//! them); one host thread mixes the playing ones into a 44.1 kHz stereo stream of kura's
//! audio output, paced by how much that stream still has queued. COM objects are a guest
//! block whose first word is a vtable of trap addresses.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::api::{host_fn, implement, Ctx};
use crate::mem::{self, ps, put32, r16, r32, w16, w32};

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
    /// G.711: 6 A-law, 7 mu-law (8-bit codes)
    pub law: u8,
}

impl Fmt {
    pub fn block(&self) -> u32 {
        self.channels * self.bits / 8
    }
    fn read(p: u32) -> Fmt {
        let tag = r16(p);
        let bits = r16(p + 14) as u32;
        let float = tag == 3 || (tag == 0xfffe && r32(p + 24) == 3);
        let law = if matches!(tag, 6 | 7) { tag as u8 } else if tag == 0xfffe && matches!(r32(p + 24), 6 | 7) { r32(p + 24) as u8 } else { 0 };
        // (the real channel count: the stride; the mixer plays the first two)
        Fmt { rate: r32(p + 4), channels: (r16(p + 2) as u32).max(1), bits, float, law }
    }
    fn write(&self, p: u32) {
        w16(p, if self.float { 3 } else if self.law != 0 { self.law as u16 } else { 1 });
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
    /// plays left after this one when not looping (DirectMusic segment repeats)
    repeats: u32,
    /// play position in frames
    pos: f64,
    gain: (f32, f32),
    volume: i32,
    pan: i32,
    notify: Vec<(u32, u32)>,
    refs: u32,
    /// the buffer owning `data` (duplicates share it)
    owner: u32,
    /// a sound still being decoded into `data` (fill_start): it is not played past what is there
    fill: Option<Arc<Fill>>,
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
            (8, _) if f.law != 0 => g711(mem::r8(a), f.law == 6) as f32 / 32768.0,
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

/// A G.711 code as a 16-bit sample (A-law when `alaw`, else mu-law).
pub(crate) fn g711(code: u8, alaw: bool) -> i16 {
    if alaw {
        let v = code ^ 0x55;
        let (seg, mant) = ((v >> 4) & 7, (v & 15) as i32);
        let mag = if seg == 0 { (mant << 4) + 8 } else { ((mant << 4) + 0x108) << (seg - 1) };
        (if v & 0x80 != 0 { mag } else { -mag }) as i16
    } else {
        let v = !code;
        let (seg, mant) = ((v >> 4) & 7, (v & 15) as i32);
        let mag = (((mant << 3) + 0x84) << seg) - 0x84;
        (if v & 0x80 != 0 { -mag } else { mag }) as i16
    }
}

/// Whether waveOutOpen takes a WAVEFORMATEX(TENSIBLE), as Windows decides (Wine's winmm wave
/// tests): 0, WAVERR_BADFORMAT (32) or, for some with WAVE_FORMAT_DIRECT, MMSYSERR_INVALPARAM.
/// Non-extensible formats are PCM, float or G.711 (through the mapper only) of one or two
/// channels; extensible ones need cbSize 22, nAvgBytesPerSec = nBlockAlign * rate and the
/// valid bits to fit.
pub(crate) fn wave_format_check(tag: u16, ch: u16, rate: u32, avg: u32, align: u16, bits: u16, ext: Option<(u16, u16, [u8; 16])>, direct: bool) -> u32 {
    const BAD: u32 = 32;
    let soft = if direct { 11 } else { BAD };
    let (sub, valid) = match ext {
        None => {
            let sub = match tag {
                1 | 3 | 6 | 7 => tag as u32,
                _ => return BAD,
            };
            if ch > 2 {
                return BAD;
            }
            (sub, bits)
        }
        Some((cb, valid, guid)) => {
            if cb < 22 || guid[4..] != [0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71] {
                return BAD;
            }
            (u32::from_le_bytes(guid[..4].try_into().unwrap()), valid)
        }
    };
    if ch == 0 || rate == 0 || align as u32 != ch as u32 * bits as u32 / 8 {
        return BAD;
    }
    if ext.is_some() && avg != align as u32 * rate {
        return soft;
    }
    let ok_bits = bits % 8 == 0 && bits != 0;
    match sub {
        1 if !ok_bits || (bits == 32 && (valid == 0 || valid > 32)) => BAD,
        1 if !matches!(bits, 8 | 16 | 24 | 32) => soft,
        1 if bits == 32 && valid == 24 => 0,
        1 => if valid == bits { 0 } else { BAD },
        3 if !ok_bits || valid == 0 || valid > bits => BAD,
        3 if !matches!(bits, 32 | 64) => soft,
        3 => if bits == 32 && valid == 32 { 0 } else { BAD },
        6 | 7 => if !direct && ext.is_none() && bits == 8 { 0 } else { BAD },
        _ => BAD,
    }
}

fn read_format_check(p: u32, direct: bool) -> u32 {
    let tag = r16(p);
    let ext = (tag == 0xfffe).then(|| {
        let cb = r16(p + 16);
        let mut g = [0u8; 16];
        if cb >= 22 {
            g.copy_from_slice(mem::slice(p + 24, 16));
        }
        (cb, r16(p + 18), g)
    });
    wave_format_check(tag, r16(p + 2), r32(p + 4), r32(p + 8), r16(p + 12), r16(p + 14), ext, direct)
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
        // (a sound being decoded waits at what is there: a pause, not a jump over zeros)
        if let Some(f) = &b.fill {
            if !f.done() && (b.pos as usize).saturating_add(2 + STEP * b.freq as usize / RATE as usize) > f.frames() as usize {
                continue;
            }
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
                // (by the remainder: a step longer than a short looping buffer, or a position
                // set past its end, left pos past the end and the mixer read whatever followed)
                if b.looping {
                    b.pos %= frames as f64;
                } else if b.repeats > 0 {
                    b.repeats -= 1;
                    b.pos %= frames as f64;
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
    // the session volume (winmm's mixer, mixer.rs) scales all of it
    let g = crate::mixer::SESSION.gain();
    if g != 1.0 {
        out.iter_mut().for_each(|x| *x *= g);
    }
    events
}

// ---------------------------------------------------------------------------------------------
// DirectMusic's wave segments (dmime.rs): a sound in guest memory the mixer plays under `key`
// (the segment state object), `repeats` more times or forever (u32::MAX), at `volume`
// (hundredths of a dB, as DirectSound's)

/// Starts playing; false without sound (SAKE_NO_SOUND).
pub(crate) fn music_start(key: u32, data: u32, size: u32, fmt: Fmt, repeats: u32, volume: i32) -> bool {
    if no_sound() {
        return false;
    }
    start_mixer();
    with(|d| d.bufs.insert(key, music_buf(key, data, size, fmt, repeats, volume)));
    true
}

fn music_buf(key: u32, data: u32, size: u32, fmt: Fmt, repeats: u32, volume: i32) -> Buf {
    Buf {
        data,
        size,
        fmt,
        freq: fmt.rate,
        flags: 0,
        primary: false,
        playing: true,
        looping: repeats == u32::MAX,
        repeats: if repeats == u32::MAX { 0 } else { repeats },
        pos: 0.0,
        gain: gains(volume, 0),
        volume,
        pan: 0,
        notify: Vec::new(),
        refs: 1,
        owner: key,
        fill: fill_of(data),
    }
}

// ---------------------------------------------------------------------------------------------
// sounds decoded while they play (mci.rs): the PCM block is there at once, its frames are
// written as they are decoded, and the mixer does not play past them

/// The decoding of one PCM block: how many frames are in, whether it ended, and the lock a
/// writer holds while it writes (fill_drop takes it: nothing writes after it returns).
pub(crate) struct Fill {
    frames: AtomicU32,
    done: AtomicBool,
    cancelled: Mutex<bool>,
}
impl Fill {
    pub(crate) fn frames(&self) -> u32 {
        self.frames.load(Ordering::Acquire)
    }
    pub(crate) fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
    /// Runs `write` (which writes frames up to `to`) unless the block was dropped; false then.
    pub(crate) fn write(&self, to: u32, write: impl FnOnce()) -> bool {
        let g = self.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        if *g {
            return false;
        }
        write();
        self.frames.store(to, Ordering::Release);
        true
    }
    pub(crate) fn finish(&self) {
        self.done.store(true, Ordering::Release);
    }
}

static FILLS: Mutex<Option<HashMap<u32, Arc<Fill>>>> = Mutex::new(None);

fn fills<R>(f: impl FnOnce(&mut HashMap<u32, Arc<Fill>>) -> R) -> R {
    f(FILLS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default))
}

/// A block at `data` that will be filled: its Fill.
pub(crate) fn fill_start(data: u32) -> Arc<Fill> {
    let f = Arc::new(Fill { frames: AtomicU32::new(0), done: AtomicBool::new(false), cancelled: Mutex::new(false) });
    fills(|m| m.insert(data, f.clone()));
    f
}

fn fill_of(data: u32) -> Option<Arc<Fill>> {
    fills(|m| m.get(&data).cloned())
}
#[cfg(test)]
pub(crate) fn fill_of_for_tests(data: u32) -> Option<Arc<Fill>> {
    fill_of(data)
}

/// The block at `data` goes (before it is freed): its decoding stops writing to it.
pub(crate) fn fill_drop(data: u32) {
    if let Some(f) = fills(|m| m.remove(&data)) {
        *f.cancelled.lock().unwrap_or_else(|e| e.into_inner()) = true;
        f.finish();
    }
}

/// Stops and forgets `key`.
pub(crate) fn music_stop(key: u32) {
    with(|d| d.bufs.remove(&key));
}

pub(crate) fn music_playing(key: u32) -> bool {
    with(|d| d.bufs.get(&key).is_some_and(|b| b.playing))
}

/// Moves `key` to frame `pos` and/or starts or holds it (MCI's seek, pause, resume: mci.rs).
pub(crate) fn music_set(key: u32, pos: Option<f64>, playing: Option<bool>) {
    with(|d| {
        if let Some(b) = d.bufs.get_mut(&key) {
            if let Some(p) = pos {
                let frames = (b.size / b.fmt.block().max(1)) as f64;
                b.pos = if frames > 0.0 { p.clamp(0.0, frames - 1.0) } else { 0.0 };
            }
            if let Some(on) = playing {
                b.playing = on;
            }
        }
    });
}

/// Frames played of the current pass (the segment state's seek)
pub(crate) fn music_pos(key: u32) -> Option<f64> {
    with(|d| d.bufs.get(&key).map(|b| b.pos))
}

pub(crate) fn music_volume(key: u32, volume: i32) {
    with(|d| {
        if let Some(b) = d.bufs.get_mut(&key) {
            b.volume = volume;
            b.gain = gains(volume, 0);
        }
    });
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
    /// the device id it was opened on (WAVE_MAPPER -1)
    device: u32,
    /// a 16-bit program's (MMSYSTEM): its headers' lpData are far pointers, and WOM_DONE
    /// reports a header by its far pointer (linear -> far)
    w16: Option<HashMap<u32, u32>>,
}

static WAVE_CV: std::sync::Condvar = std::sync::Condvar::new();
static WAVE_LOCK: Mutex<()> = Mutex::new(());
static NEXT_WAVE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x5a00);

/// WAVEHDR: lpData, dwBufferLength, dwBytesRecorded, dwUser, dwFlags (pointers are 8 bytes on
/// x64: the flags at 24, not 16)
fn hlen(h: u32) -> u32 {
    r32(h + ps())
}
fn hflags_at(h: u32) -> u32 {
    h + 2 * ps() + 8
}
fn hflags(h: u32) -> u32 {
    r32(hflags_at(h))
}
fn set_hflags(h: u32, f: u32) {
    w32(hflags_at(h), f)
}

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
        'frames: for o in out.chunks_exact_mut(2) {
            // (the header that has this frame: one that ran out hands on to the next for the
            // same output frame, which was left silent, a click at each boundary off a step)
            let (h, i) = loop {
                let Some(&h) = w.queue.front() else { break 'frames };
                let frames = (hlen(h) / block) as usize;
                let i = w.pos as usize;
                if i < frames {
                    break (h, i);
                }
                w.queue.pop_front();
                set_hflags(h, (hflags(h) | WHDR_DONE) & !WHDR_INQUEUE);
                w.done.push(h);
                w.played += hlen(h) as u64;
                // (the part of a step past its end, into the next header)
                w.pos = (w.pos - frames as f64).max(0.0);
                finished = true;
            };
            let data = if w.w16.is_some() { crate::w16::lin(r32(h)) } else { mem::rp(h) };
            let (l, r) = sample_at(data, &w.fmt, i);
            o[0] += l * gl;
            o[1] += r * gr;
            w.pos += step;
        }
        // a header that ended exactly on this step
        if let Some(&h) = w.queue.front() {
            let frames = (hlen(h) / block) as usize;
            if w.pos as usize >= frames {
                w.queue.pop_front();
                set_hflags(h, (hflags(h) | WHDR_DONE) & !WHDR_INQUEUE);
                w.done.push(h);
                w.played += hlen(h) as u64;
                w.pos = (w.pos - frames as f64).max(0.0);
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
            Some(w) => {
                let mut done = std::mem::take(&mut w.done);
                if let Some(m) = &w.w16 {
                    done.iter_mut().for_each(|h| *h = m.get(h).copied().unwrap_or(*h));
                }
                (done, w.cb, w.closing)
            }
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
            crate::snap::point();
        }
    }
    crate::k32::exit_thread(0)
}

/// Makes `hwo` a 16-bit program's device (see Wave::w16).
pub(crate) fn wave16(hwo: u32) {
    with(|d| d.waves.get_mut(&hwo).map(|w| w.w16.get_or_insert_with(HashMap::default).clear()));
}

/// A 16-bit program's header: linear `lin`, far `far`.
pub(crate) fn wave16_header(hwo: u32, lin: u32, far: u32) {
    with(|d| d.waves.get_mut(&hwo).and_then(|w| w.w16.as_mut()).map(|m| m.insert(lin, far)));
}

/// Whether `h` is an open waveOut handle (mixer.rs: MIXER_OBJECTF_HWAVEOUT)
pub(crate) fn is_wave_handle(h: u32) -> bool {
    with(|d| d.waves.contains_key(&h))
}
fn waveOutGetNumDevs(_: &mut Ctx) -> u32 {
    1
}
const WAVE_MAPPER: u32 = 0xffff_ffff;
/// A device id (0 or WAVE_MAPPER) or an open handle's device; None for anything else.
fn wave_device(id: u32) -> Option<u32> {
    match id {
        0 | WAVE_MAPPER => Some(id),
        h => with(|d| d.waves.get(&h).map(|w| w.device)),
    }
}
/// waveOutGetDevCaps(device or handle, caps, size): the first `size` bytes of WAVEOUTCAPS
fn wave_caps(c: &mut Ctx, wide: bool) -> u32 {
    let (p, size) = (c.a(1), c.a(2));
    if wave_device(c.a(0)).is_none() {
        return 2; // MMSYSERR_BADDEVICEID
    }
    if p == 0 {
        return 11; // MMSYSERR_INVALPARAM
    }
    let name = "sake WebAudio";
    let o = if wide { 8 + 64 } else { 8 + 32 };
    let mut b = vec![0u8; o + 12];
    b[0] = 1;
    b[2] = 2;
    b[5] = 1; // driver version 1.0
    for (i, ch) in name.bytes().enumerate() {
        b[8 + if wide { 2 * i } else { i }] = ch;
    }
    b[o..o + 4].copy_from_slice(&0xfffu32.to_le_bytes());
    b[o + 4] = 2;
    b[o + 8] = 0xc;
    mem::write(p, &b[..(size as usize).min(b.len())]);
    0
}
/// waveOutGetID(handle, &id)
fn waveOutGetID(c: &mut Ctx) -> u32 {
    let Some(dev) = with(|d| d.waves.get(&c.a(0)).map(|w| w.device)) else { return 5 };
    if c.a(1) == 0 {
        return 11;
    }
    put32(c.a(1), dev);
    0
}
/// waveOutMessage(device or handle, message, p1, p2): the driver queries programs make
fn waveOutMessage(c: &mut Ctx) -> u32 {
    let (msg, p1, p2) = (c.a(1), c.a(2), c.a(3));
    // the device interface path (DRV_QUERYDEVICEINTERFACE*)
    let path: Vec<u16> = r"\\?\SWD#MMDEVAPI#{0.0.0.00000000}.{sake}#{e6327cad-dcec-4949-ae8a-991e976a79d2}".encode_utf16().chain([0]).collect();
    match msg {
        // DRVM_MAPPER_PREFERRED_GET, DRVM_MAPPER_CONSOLEVOICECOM_GET: device 0, no flags
        0x2015 | 0x2017 => {
            if p1 == 0 || p2 == 0 {
                return 11;
            }
            put32(p1, 0);
            put32(p2, 0);
            0
        }
        0x805 => 0, // DRV_QUERYMAPPABLE
        0x80c => {
            if p1 == 0 {
                return 11;
            }
            put32(p1, 2 * path.len() as u32);
            0
        }
        0x80d => {
            if p1 == 0 || p2 < 2 * path.len() as u32 {
                return 11;
            }
            for (i, u) in path.iter().enumerate() {
                w16(p1 + 2 * i as u32, *u);
            }
            0
        }
        _ => 8, // MMSYSERR_NOTSUPPORTED
    }
}
/// waveOutGetErrorText(error, text, size)
fn wave_error_text(c: &mut Ctx, wide: bool) -> u32 {
    let (e, p, n) = (c.a(0), c.a(1), c.a(2));
    let text = match e {
        0 => "The specified command was carried out.",
        1 => "Undefined external error.",
        2 => "A device ID has been used that is out of range for your system.",
        5 => "An invalid handle was passed to a system function.",
        8 => "The function requested is not supported by the device driver.",
        11 => "An invalid parameter was passed to a system function.",
        32 => "The specified format is not supported or cannot be translated. Use the Capabilities function to determine the supported formats.",
        33 => "Cannot perform this operation while media data is still playing. Reset the device, or wait until the data is finished playing.",
        34 => "The wave header was not prepared. Use the Prepare function to prepare the header, and then try again.",
        _ => return 11,
    };
    if p == 0 || n == 0 {
        return 11;
    }
    if wide {
        let w: Vec<u16> = text.encode_utf16().collect();
        mem::put_wstr(p, n, &w);
    } else {
        mem::put_cstr(p, n, text.as_bytes());
    }
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
    let (ph, device, fmtp, cb, inst, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    // a device id (not an open handle)
    if !matches!(device, 0 | WAVE_MAPPER) {
        return 2; // MMSYSERR_BADDEVICEID
    }
    if fmtp == 0 {
        return 11; // MMSYSERR_INVALPARAM
    }
    let e = read_format_check(fmtp, flags & 8 != 0);
    if e != 0 {
        // a failed open (not a query) leaves NULL
        if flags & 1 == 0 && ph != 0 {
            put32(ph, 0);
        }
        return e;
    }
    let fmt = Fmt::read(fmtp);
    if flags & 1 != 0 {
        return 0; // WAVE_FORMAT_QUERY
    }
    if ph == 0 {
        return 11;
    }
    start_mixer();
    let hwo = NEXT_WAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let cbk = (flags & 0x70000, cb, inst);
    with(|d| {
        d.waves.insert(hwo, Wave { fmt, queue: Default::default(), pos: 0.0, done: Vec::new(), paused: false, closing: false, played: 0, cb: cbk, volume: 0xffff_ffff, device, w16: None })
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
    // (headers still queued: WAVERR_STILLPLAYING and the device stays, as Windows; it closed,
    // and the mixer went on writing WHDR_DONE into headers the program had freed)
    let Some(cb) = with(|d| d.waves.get_mut(&hwo).map(|w| {
        if !w.queue.is_empty() {
            return Err(33);
        }
        w.closing = true;
        Ok(w.cb)
    })) else { return 5 };
    let cb = match cb {
        Ok(cb) => cb,
        Err(e) => return e,
    };
    WAVE_CV.notify_all();
    wave_notify(c, hwo, cb, WOM_CLOSE, 0);
    // (the device thread drops it on its next look)
    0
}
/// The flags waveOutPrepareHeader leaves: a prepared header is left alone, another one becomes
/// prepared and neither done nor queued.
pub(crate) fn prepared_flags(f: u32) -> u32 {
    if f & WHDR_PREPARED != 0 { f } else { (f | WHDR_PREPARED) & !(WHDR_DONE | WHDR_INQUEUE) }
}
/// waveOutUnprepareHeader: Err(WAVERR_STILLPLAYING) for a queued prepared header; an
/// unprepared one is left alone.
pub(crate) fn unprepared_flags(f: u32) -> Result<u32, u32> {
    match f {
        f if f & WHDR_PREPARED == 0 => Ok(f),
        f if f & WHDR_INQUEUE != 0 => Err(33),
        f => Ok(f & !WHDR_PREPARED),
    }
}
fn waveOutPrepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    if h == 0 {
        return 11;
    }
    set_hflags(h, prepared_flags(hflags(h)));
    0
}
fn waveOutUnprepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    if h == 0 {
        return 11;
    }
    match unprepared_flags(hflags(h)) {
        Ok(f) => set_hflags(h, f),
        Err(e) => return e,
    }
    0
}
fn waveOutWrite(c: &mut Ctx) -> u32 {
    let (hwo, h) = (c.a(0), c.a(1));
    if h == 0 {
        return 11;
    }
    let f = hflags(h);
    if f & WHDR_PREPARED == 0 {
        return 34; // WAVERR_UNPREPARED
    }
    if f & WHDR_INQUEUE != 0 {
        return 33; // WAVERR_STILLPLAYING
    }
    if !with(|d| d.waves.contains_key(&hwo)) {
        return 5;
    }
    set_hflags(h, (f & !WHDR_DONE) | WHDR_INQUEUE);
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
                set_hflags(h, (hflags(h) | WHDR_DONE) & !WHDR_INQUEUE);
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
    let (hwo, t, size) = (c.a(0), c.a(1), c.a(2));
    if t == 0 || size < 12 {
        return 1; // MMSYSERR_ERROR (an MMTIME is 12 bytes)
    }
    let Some((bytes, fmt)) = with(|d| d.waves.get(&hwo).map(|w| (w.played + (w.pos as u64) * w.fmt.block() as u64, w.fmt))) else { return 5 };
    let frames = bytes / fmt.block().max(1) as u64;
    match r32(t) {
        2 => w32(t + 4, frames as u32),
        // (TIME_MS and the rest: bytes, as Windows answers)
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
    if c.a(1) == 0 {
        return 11;
    }
    let v = with(|d| d.waves.get(&c.a(0)).map_or(0xffff_ffff, |w| w.volume));
    put32(c.a(1), v);
    0
}

/// Save states: wakes the device threads' waits.
pub(crate) fn snap_wake() {
    drop(WAVE_LOCK.lock());
    WAVE_CV.notify_all();
}

/// A restored save state: the mixer again, if it ran.
pub(crate) fn snap_restore() {
    let id = with(|d| d.stream);
    if id > 0 && !crate::env_on!("SAKE_NO_MIXER") {
        spawn_mixer(id);
    }
}

fn start_mixer() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let id = kura::audio::open();
        with(|d| d.stream = id);
        if crate::env_on!("SAKE_NO_MIXER") {
            return;
        }
        spawn_mixer(id);
    });
}

fn spawn_mixer(id: i32) {
        std::thread::Builder::new()
            .name("dsound mixer".into())
            .spawn(move || {
                kura::snap::register("dsound mixer", false, 0);
                let mut out = vec![0f32; STEP * 2];
                let stats = crate::env_on!("SAKE_MIX_STATS");
                let (mut t_mix, mut t_q, mut t_w, mut t_all, mut n, mut peak) = (0f64, 0f64, 0f64, std::time::Instant::now(), 0, 0f32);
                // (a condvar's timed wait, not thread::sleep: in the browser emscripten's sleep
                // cost the thread about 0.15 cores)
                let (lock, cv) = (Mutex::new(()), std::sync::Condvar::new());
                loop {
                    kura::snap::idle();
                    drop(cv.wait_timeout(lock.lock().unwrap(), Duration::from_millis(10)).unwrap());
                    kura::snap::busy();
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
                        if stats {
                            peak = out.iter().fold(peak, |m, x| m.max(x.abs()));
                        }
                        kura::audio::write(id, &out);
                        t_w += t0.elapsed().as_secs_f64();
                        n += 1;
                    }
                    if stats && t_all.elapsed().as_secs() >= 5 {
                        let s = t_all.elapsed().as_secs_f64();
                        eprintln!("sake mixer: {n} chunks/{s:.1}s, mix {:.1} ms/s, queued {:.1} ms/s, write {:.1} ms/s, peak {peak:.3}", t_mix * 1e3 / s, t_q * 1e3 / s, t_w * 1e3 / s);
                        (t_mix, t_q, t_w, t_all, n, peak) = (0.0, 0.0, 0.0, std::time::Instant::now(), 0, 0.0);
                    }
                }
            })
            .expect("mixer thread");
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

/// A DirectSound device (IMMDevice::Activate of IDirectSound8), None with SAKE_NO_SOUND
pub(crate) fn device() -> Option<u32> {
    (!no_sound()).then(create_device)
}

fn no_sound() -> bool {
    crate::env_on!("SAKE_NO_SOUND")
}

fn DirectSoundCreate(c: &mut Ctx) -> u32 {
    if no_sound() {
        crate::mem::putp(c.a(1), 0);
        return 0x8878_0078;
    }
    crate::mem::putp(c.a(1), create_device());
    0
}
fn DirectSoundCreate8(c: &mut Ctx) -> u32 {
    if no_sound() {
        crate::mem::putp(c.a(1), 0);
        return 0x8878_0078;
    }
    crate::mem::putp(c.a(1), create_device());
    0
}
/// The output device DirectSoundEnumerate lists after the primary driver (a GUID of its own, as
/// Windows gives each endpoint one).
const DEVICE_GUID: [u8; 16] = [0x7c, 0x1d, 0x7f, 0x3e, 0x2b, 0x4a, 0x5d, 0x4c, 0x9a, 0x61, 0x1e, 0x2f, 0x8b, 0x30, 0x51, 0x6c];

/// What DirectSoundEnumerate lists, as Windows does: the primary sound driver (no GUID, no
/// module), then each output device with its GUID, name and module "{0.0.0.00000000}.{GUID}".
/// FMOD (Unity 4's audio) takes only entries with a GUID as drivers: with the primary one alone it
/// found no device, Unity ran without audio and crashed when a script played the title music
/// (絶対君主！小夜様！体験版). Without sound (SAKE_NO_SOUND) only the primary driver.
fn ds_devices(sound: bool) -> Vec<(Option<[u8; 16]>, &'static str, String)> {
    let mut v = vec![(None, "Primary Sound Driver", String::new())];
    if sound {
        v.push((Some(DEVICE_GUID), "Speakers (masu Audio)", format!("{{0.0.0.00000000}}.{}", guid_text(&DEVICE_GUID))));
    }
    v
}

/// A GUID's registry form in lower case, "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}".
fn guid_text(g: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    let tail: String = g[10..].iter().map(|b| format!("{b:02x}")).collect();
    format!("{{{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{tail}}}", g[8], g[9])
}

fn enumerate(c: &mut Ctx, wide: bool) -> u32 {
    let (cb, ctx) = (c.a(0), c.a(1));
    if cb == 0 {
        return 0x8007_0057;
    }
    let buf = mem::heap_alloc(16 + 2 * 128 + 2 * 128, true);
    let (guid, desc, module) = (buf, buf + 16, buf + 16 + 256);
    for (g, d, m) in ds_devices(!no_sound()) {
        if let Some(g) = g {
            mem::write(guid, &g);
        }
        if wide {
            mem::put_wstr(desc, 128, &d.encode_utf16().collect::<Vec<_>>());
            mem::put_wstr(module, 128, &m.encode_utf16().collect::<Vec<_>>());
        } else {
            mem::put_cstr(desc, 128, d.as_bytes());
            mem::put_cstr(module, 128, m.as_bytes());
        }
        // (the callback's FALSE stops the listing)
        if c.call(cb, &[if g.is_some() { guid } else { 0 }, desc, module, ctx]) == 0 {
            break;
        }
    }
    mem::heap_free(buf);
    0
}
/// DSDEVID_DefaultPlayback {DEF00000-9C6D-47ED-AAF1-4DDA8F2B5C03}; DefaultCapture, its voice
/// forms differ only in the first byte (01, 02, 03)
const DSDEVID_DEFAULT: [u8; 16] = [0x00, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03];
const DSERR_NODRIVER: u32 = 0x8878_0078;
/// GetDeviceID(source, dest)'s answer, as Wine's: GUID_NULL and the default (voice) playback
/// ids are the output device; the default capture ids have no device (sake records nothing);
/// any other GUID is given back as it is. cmvs64.exe (リアライブ 体験版) compares the default
/// playback device every frame with the one it opened on: the function was missing, the answer
/// was whatever the stack held, and the game restarted its sound and warned
/// 「再生デバイスが変更されました」 every frame.
fn device_id(src: [u8; 16], sound: bool) -> Result<[u8; 16], u32> {
    let default = src[1..] == DSDEVID_DEFAULT[1..] && src[0] <= 3;
    if src == [0; 16] || (default && src[0] & 1 == 0) {
        return if sound { Ok(DEVICE_GUID) } else { Err(DSERR_NODRIVER) };
    }
    if default {
        return Err(DSERR_NODRIVER);
    }
    Ok(src)
}
fn GetDeviceID(c: &mut Ctx) -> u32 {
    let (src, dst) = (c.a(0), c.a(1));
    if src == 0 || dst == 0 {
        return 0x8007_0057;
    }
    let g: [u8; 16] = mem::slice(src, 16).try_into().unwrap();
    match device_id(g, !no_sound()) {
        Ok(d) => {
            mem::write(dst, &d);
            0
        }
        Err(e) => e,
    }
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
    let o = c.a(0);
    let left = {
        let mut g = DS_REFS.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::default);
        let Some(r) = m.get_mut(&o) else { return 0 };
        *r = r.saturating_sub(1);
        let left = *r;
        if left == 0 {
            m.remove(&o);
        }
        left
    };
    // (the device object goes with its last reference: one per DirectSoundCreate was kept)
    if left == 0 {
        mem::heap_free(o);
    }
    left
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
    let fmt = if wfx != 0 { Fmt::read(wfx) } else { Fmt { rate: 22050, channels: 2, bits: 16, float: false, law: 0 } };
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
                repeats: 0,
                pos: 0.0,
                gain,
                volume: 0,
                pan: 0,
                notify: Vec::new(),
                refs: 1,
                owner: o,
                fill: None,
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
            repeats: 0,
            pos: 0.0,
            gain: s.gain,
            volume: s.volume,
            pan: s.pan,
            notify: Vec::new(),
            refs: 1,
            owner: s.owner,
            fill: s.fill.clone(),
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
            put32(out, child(notify_vtbl(), this));
            0
        }
        // IDirectSound3DListener (the primary buffer) / IDirectSound3DBuffer: accepted, the sound
        // is not positioned
        0x279a_fa84 => {
            put32(out, child(listener3d_vtbl(), this));
            0
        }
        0x279a_fa86 => {
            put32(out, child(buffer3d_vtbl(), this));
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
    buf_release(c.a(0))
}
/// Release of buffer `this` (the guest's, or a notify / 3D object's reference on it): at the
/// last one it leaves the mixer, and its PCM goes once no duplicate shares it.
fn buf_release(this: u32) -> u32 {
    let gone = with(|d| {
        let b = d.bufs.get_mut(&this)?;
        b.refs = b.refs.saturating_sub(1);
        if b.refs > 0 {
            return Some(Err(b.refs));
        }
        let b = d.bufs.remove(&this).unwrap();
        // the data goes once neither the buffer nor a duplicate of it is left
        let shared = d.bufs.values().any(|x| x.owner == b.owner);
        Some(Ok((!shared && b.data != 0).then_some(b.data)))
    });
    match gone {
        // (not a buffer: released once too often)
        None => 0,
        Some(Err(n)) => n,
        Some(Ok(data)) => {
            if let Some(p) = data {
                mem::heap_free(p);
            }
            mem::heap_free(this);
            0
        }
    }
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
    // (past the end: DSERR_INVALIDPARAM, as Windows; the mixer read past the buffer)
    with_buf(this, |b| {
        if p >= b.size && b.size != 0 {
            return DSERR_INVALIDPARAM;
        }
        b.pos = (p / b.fmt.block().max(1)) as f64;
        0
    })
    .unwrap_or(0)
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
        w32(this + 8, r32(this + 8) + 1);
        put32(out, this);
        return 0;
    }
    let _ = buf;
    put32(out, 0);
    E_NOINTERFACE
}
/// An interface object of buffer `buf` (IDirectSoundNotify, IDirectSound3DBuffer/Listener):
/// {vtable, buffer, references}; it holds a reference on the buffer until its last Release.
fn child(vtbl: u32, buf: u32) -> u32 {
    with_buf(buf, |b| b.refs += 1);
    let o = new_object(vtbl, buf);
    w32(o + 8, 1);
    o
}
fn N_AddRef(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let n = r32(o + 8) + 1;
    w32(o + 8, n);
    n
}
fn N_Release(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let n = r32(o + 8);
    if n == 0 {
        return 0;
    }
    w32(o + 8, n - 1);
    if n == 1 {
        let buf = r32(o + 4);
        mem::heap_free(o);
        buf_release(buf);
    }
    n - 1
}
/// DSBPOSITIONNOTIFY {dwOffset, hEventNotify}: 8 bytes on x86; 16 on x64, the handle at 8 (after
/// 4 bytes of padding). Reading x64's as x86's took the padding for the events, so none was ever
/// set: cmvs64.exe (リアライブ 体験版) waits for its stream's notifications, takes the silence for a
/// lost device and says 「再生デバイスが変更されました」 over and over, never past its logo.
fn notify_list(n: u32, p: u32, wide: bool, read: impl Fn(u32) -> u32) -> Vec<(u32, u32)> {
    let (size, ev) = if wide { (16, 8) } else { (8, 4) };
    (0..n).map(|i| (read(p + size * i), read(p + size * i + ev))).collect()
}
fn N_SetNotificationPositions(c: &mut Ctx) -> u32 {
    let (this, n, p) = (c.a(0), c.a(1), c.a(2));
    // (DSERR_INVALIDPARAM: positions but no array, or more than any buffer could hold)
    if (n != 0 && p == 0) || n > 0x10_0000 {
        return 0x8007_0057;
    }
    let buf = r32(this + 4);
    let list = notify_list(n, p, crate::mem::x64(), r32);
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
const SND_RESOURCE: u32 = 0x4_0004;

/// The "fmt " and "data" chunks of a RIFF WAVE image: their offsets, and data's length
fn wave_chunks(b: &[u8]) -> Option<(usize, usize, usize)> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let (mut at, mut fmt, mut data) = (12, None, None);
    while at < b.len() && b.len() - at >= 8 {
        let n = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap());
        // (64-bit sums: on wasm32 `at + 8 + n` with n = 0xFFFFFFF8 wrapped to `at`, an endless
        // loop on the PlaySound caller's thread, review A-7)
        let (end, next) = crate::audiofile::riff_chunk(at + 8, n, b.len());
        match &b[at..at + 4] {
            b"fmt " => fmt = Some(at + 8),
            b"data" => data = Some((at + 8, end - (at + 8))),
            _ => {}
        }
        at = next;
    }
    Some((fmt?, data?.0, data?.1))
}

/// A RIFF WAVE image with MS or IMA ADPCM data as 16-bit PCM (PlaySound goes through ACM on
/// Windows); others as they are.
pub(crate) fn adpcm_to_pcm(image: Vec<u8>) -> Vec<u8> {
    let Some((fmt, data, size)) = wave_chunks(&image) else { return image };
    let f = &image[fmt..];
    if f.len() < 20 {
        return image;
    }
    let u16_at = |o: usize| u16::from_le_bytes([f[o], f[o + 1]]) as usize;
    let (tag, ch, rate, align) = (u16_at(0), u16_at(2).max(1), u32::from_le_bytes(f[4..8].try_into().unwrap()), u16_at(12));
    if !matches!(tag, 2 | 0x11) || align == 0 {
        return image;
    }
    let spb = u16_at(18);
    let mut pcm = Vec::new();
    for b in image[data..data + size].chunks(align) {
        if tag == 2 {
            crate::acm::ms_block(b, ch, spb, &mut pcm);
        } else {
            crate::acm::ima_block(b, ch, spb, &mut pcm);
        }
    }
    let bytes: Vec<u8> = pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut out = Vec::with_capacity(44 + bytes.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, ch as u16] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2 * ch as u32).to_le_bytes());
    for v in [2 * ch as u16, 16] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytes);
    out
}

/// PlaySound's sound: a WAVE resource of `module`, or a file (without an extension: with
/// ".wav"; a name ending in a dot is not found).
fn sound_image(name: u32, module: u32, flags: u32, wide: bool) -> Option<Vec<u8>> {
    if flags & SND_RESOURCE == SND_RESOURCE {
        let m = if module == 0 { crate::k32::module_handle(0, false) } else { module };
        let e = crate::pe::find_resource(m, &crate::pe::ResId::Name("WAVE".encode_utf16().collect()), &crate::pe::ResId::from_arg(name, wide))?;
        return Some(mem::slice(m + r32(e), r32(e + 4)).to_vec());
    }
    let path = crate::k32::arg_str(name, wide);
    let read = |p: &str| crate::files::resolve(p).and_then(|h| kura::fs::read(h).ok());
    read(&path).or_else(|| {
        let base = path.rsplit(['\\', '/']).next().unwrap_or("");
        (!base.contains('.')).then(|| read(&format!("{path}.wav"))).flatten()
    })
}

fn play_sound(name: u32, module: u32, flags: u32, wide: bool) -> u32 {
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
    let image = if flags & SND_RESOURCE != SND_RESOURCE && flags & SND_MEMORY != 0 {
        // (a RIFF image: its length from its header, at most what is mapped there; anything
        // else is no sound: a garbage length asked for up to 4 GiB)
        if mem::accessible(name, 12, false, true) < 12 || r32(name) != u32::from_le_bytes(*b"RIFF") {
            return 0;
        }
        let n = (8 + r32(name + 4) as u64).min(u32::MAX as u64) as u32;
        mem::slice(name, mem::accessible(name, n, false, true)).to_vec()
    } else {
        match sound_image(name, module, flags, wide) {
            Some(b) => b,
            None => return 0,
        }
    };
    let image = adpcm_to_pcm(image);
    let Some((fmt, data, size)) = wave_chunks(&image) else { return 0 };
    let p = mem::heap_alloc(image.len() as u32, false);
    if p == 0 {
        return 0;
    }
    mem::write(p, &image);
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
                repeats: 0,
                pos: 0.0,
                gain: gains(0, 0),
                volume: 0,
                pan: 0,
                notify: Vec::new(),
                refs: 1,
                owner: p,
                fill: None,
            },
        )
    });
    if flags & SND_ASYNC == 0 && flags & SND_LOOP == 0 {
        // synchronous: back when it has played, or another thread stopped it (PlaySound(NULL));
        // waiting in steps that are save-state points and see TerminateThread
        let ms = size as u64 * 1000 / (f.rate * f.block()).max(1) as u64;
        let end = std::time::Instant::now() + Duration::from_millis(ms);
        loop {
            let now = std::time::Instant::now();
            if now >= end || !with(|d| d.bufs.get(&PLAY_SOUND).is_some_and(|b| b.owner == p && b.playing)) {
                break;
            }
            std::thread::sleep((end - now).min(Duration::from_millis(50)));
            crate::snap::point();
            crate::k32::check_terminate();
        }
    }
    1
}
fn PlaySoundA(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), c.a(1), c.a(2), false)
}
fn PlaySoundW(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), c.a(1), c.a(2), true)
}
fn sndPlaySoundA(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), 0, c.a(1) & !0x4_0000, false)
}
fn sndPlaySoundW(c: &mut Ctx) -> u32 {
    play_sound(c.a(0), 0, c.a(1) & !0x4_0000, true)
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
        ("waveOutGetID", waveOutGetID),
        ("waveOutMessage", waveOutMessage),
        ("waveOutGetErrorTextA", |c| wave_error_text(c, false)),
        ("waveOutGetErrorTextW", |c| wave_error_text(c, true)),
    ] {
        implement("winmm", n, f);
    }
    implement("winmm", "PlaySoundA", PlaySoundA);
    implement("winmm", "PlaySound", PlaySoundA);
    implement("winmm", "PlaySoundW", PlaySoundW);
    implement("winmm", "sndPlaySoundA", sndPlaySoundA);
    implement("winmm", "sndPlaySoundW", sndPlaySoundW);
    implement("dsound", "DirectSoundCreate", DirectSoundCreate);
    implement("dsound", "DirectSoundCreate8", DirectSoundCreate8);
    implement("dsound", "DirectSoundEnumerateA", DirectSoundEnumerateA);
    implement("dsound", "DirectSoundEnumerateW", DirectSoundEnumerateW);
    implement("dsound", "GetDeviceID", GetDeviceID);
}

#[cfg(test)]
mod wave_tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn g711_reference_points() {
        // ITU-T G.711's end points and zeros
        assert_eq!(g711(0xff, false), 0);
        assert_eq!(g711(0x7f, false), 0);
        assert_eq!(g711(0x80, false), 32124);
        assert_eq!(g711(0x00, false), -32124);
        assert_eq!(g711(0xd5, true), 8);
        assert_eq!(g711(0x55, true), -8);
        assert_eq!(g711(0xaa, true), 32256);
        assert_eq!(g711(0x2a, true), -32256);
    }

    #[test]
    fn g711_is_odd_and_monotone() {
        for alaw in [false, true] {
            for code in 0..=255u8 {
                assert_eq!(g711(code, alaw), -g711(code ^ 0x80, alaw), "code {code:#x} alaw {alaw}");
            }
            // the magnitudes grow with the code's 7 magnitude bits (complemented for mu-law,
            // even bits flipped for A-law)
            let mag = |m: u8| if alaw { g711((m ^ 0x55) | 0x80, true) } else { g711(!m | 0x80, false) };
            let m: Vec<i16> = (0..128u8).map(|i| mag(i).abs()).collect();
            assert!(m.windows(2).all(|w| w[0] < w[1]), "alaw {alaw}: {m:?}");
        }
    }

    const KS: [u8; 12] = [0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71];
    fn guid(sub: u32) -> [u8; 16] {
        let mut g = [0; 16];
        g[..4].copy_from_slice(&sub.to_le_bytes());
        g[4..].copy_from_slice(&KS);
        g
    }

    #[test]
    fn windows_format_rules() {
        let pcm = |ch: u16, bits: u16| wave_format_check(1, ch, 48000, 0, ch * bits / 8, bits, None, false);
        assert_eq!(pcm(2, 16), 0);
        assert_eq!(pcm(0, 16), 32);
        assert_eq!(pcm(3, 16), 32);
        assert_eq!(pcm(2, 12), 32);
        assert_eq!(wave_format_check(1, 2, 48000, 0, 4, 40, None, true), 32); // block align
        assert_eq!(wave_format_check(1, 1, 48000, 0, 5, 40, None, true), 11); // 40-bit PCM, direct
        assert_eq!(wave_format_check(1, 1, 48000, 0, 5, 40, None, false), 32);
        assert_eq!(wave_format_check(6, 1, 8000, 0, 1, 8, None, false), 0); // A-law through the mapper
        assert_eq!(wave_format_check(6, 1, 8000, 0, 1, 8, None, true), 32);
        assert_eq!(wave_format_check(3, 2, 48000, 0, 8, 32, None, false), 0);
        assert_eq!(wave_format_check(3, 2, 48000, 0, 16, 64, None, false), 32);
        assert_eq!(wave_format_check(2, 2, 48000, 0, 4, 16, None, false), 32); // ADPCM
        // extensible: 6 channels are fine, the average rate must match
        let ext = |ch: u16, avg: u32, valid: u16, sub: u32, direct: bool| {
            wave_format_check(0xfffe, ch, 48000, avg, ch * 4, 32, Some((22, valid, guid(sub))), direct)
        };
        assert_eq!(ext(6, 6 * 4 * 48000, 32, 3, false), 0);
        assert_eq!(ext(6, 6 * 4 * 48000, 24, 1, false), 0);
        assert_eq!(ext(6, 6 * 4 * 48000, 20, 1, false), 32);
        assert_eq!(ext(2, 0, 32, 3, true), 11);
        assert_eq!(ext(2, 0, 32, 3, false), 32);
        assert_eq!(wave_format_check(0xfffe, 2, 48000, 384000, 8, 32, Some((21, 32, guid(3))), false), 32);
    }

    #[test]
    fn header_flag_rules() {
        // Wine's wave test sequence: done unprepared, prepare clears it, a second prepare keeps
        // what is set
        assert_eq!(unprepared_flags(WHDR_DONE), Ok(WHDR_DONE));
        assert_eq!(prepared_flags(WHDR_DONE | WHDR_INQUEUE), WHDR_PREPARED);
        assert_eq!(prepared_flags(WHDR_PREPARED | WHDR_INQUEUE), WHDR_PREPARED | WHDR_INQUEUE);
        assert_eq!(unprepared_flags(WHDR_PREPARED | WHDR_INQUEUE), Err(33));
        assert_eq!(unprepared_flags(WHDR_PREPARED | WHDR_DONE), Ok(WHDR_DONE));
    }

    proptest! {
        #[test]
        fn prepare_is_idempotent_and_unprepare_inverts(f in any::<u32>()) {
            let p = prepared_flags(f);
            prop_assert!(p & WHDR_PREPARED != 0);
            prop_assert_eq!(prepared_flags(p), p);
            if let Ok(u) = unprepared_flags(p) {
                prop_assert_eq!(u & WHDR_PREPARED, 0);
                prop_assert_eq!(u | WHDR_PREPARED, p);
            } else {
                prop_assert!(p & WHDR_INQUEUE != 0);
            }
        }

        #[test]
        fn accepted_formats_are_playable(tag in prop_oneof![Just(1u16), Just(3), Just(6), Just(7), Just(2), Just(0xfffe)],
                                         ch in 0u16..9, bits in 0u16..70, rate in 0u32..200000, direct in any::<bool>(),
                                         valid in 0u16..70, sub in prop_oneof![Just(1u32), Just(3), Just(6)]) {
            let align = (ch as u32 * bits as u32 / 8) as u16;
            let ext = (tag == 0xfffe).then(|| (22, valid, guid(sub)));
            if wave_format_check(tag, ch, rate, align as u32 * rate, align, bits, ext, direct) == 0 {
                // what the mixer can read: whole bytes, 8-32 bits (float only 32), a channel
                prop_assert!(ch >= 1 && rate > 0);
                prop_assert!(matches!(bits, 8 | 16 | 24 | 32));
            }
        }
    }
}

#[cfg(test)]
mod play_sound_tests {
    use super::*;
    use proptest::prelude::*;

    fn riff(tag: u16, ch: u16, align: u16, spb: u16, data: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        for v in [tag, ch] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&22050u32.to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        for v in [align, 4, 2, spb] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        b.extend_from_slice(&(f.len() as u32).to_le_bytes());
        b.extend_from_slice(&f);
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn pcm_stays() {
        let b = riff(1, 2, 4, 0, &[1, 2, 3, 4]);
        assert_eq!(adpcm_to_pcm(b.clone()), b);
    }

    /// Review A-7: chunk lengths whose sum with the offset passes 2^32 (0xFFFFFFF8 made
    /// `at += 8 + n` add 0 on wasm32: PlaySound never returned) end the walk; a data chunk of
    /// length 0xFFFFFFFF (a streamed WAV) is the bytes the file has.
    #[test]
    fn huge_chunk_lengths_end() {
        let good = riff(1, 2, 4, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        for n in [0xffff_fff8u32, 0xffff_fff7, 0xffff_fff0, 0xffff_ffff, 0x8000_0001] {
            let mut b = b"RIFF\0\0\0\0WAVEjunk".to_vec();
            b.extend(n.to_le_bytes());
            b.extend(&good[12..]);
            assert_eq!(wave_chunks(&b), None, "{n:#x}");
            assert_eq!(adpcm_to_pcm(b.clone()), b);
        }
        let mut b = good.clone();
        let k = b.len() - 8 - 4;
        b[k..k + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let (_, data, size) = wave_chunks(&b).unwrap();
        assert_eq!((data, size), (b.len() - 8, 8));
    }

    proptest! {
        /// Any bytes after the RIFF header: the walk ends and the data is inside the file.
        #[test]
        fn any_chunks(tail in proptest::collection::vec(any::<u8>(), 0..400), lens in proptest::collection::vec(any::<u32>(), 0..6)) {
            let mut b = b"RIFF\0\0\0\0WAVE".to_vec();
            for (i, n) in lens.iter().enumerate() {
                b.extend([b"fmt ", b"data", b"LIST"][i % 3]);
                b.extend(n.to_le_bytes());
                b.extend(&tail[..tail.len().min(i * 13)]);
            }
            b.extend(&tail);
            if let Some((fmt, data, size)) = wave_chunks(&b) {
                prop_assert!(fmt <= b.len() && data + size <= b.len());
            }
            let _ = adpcm_to_pcm(b);
        }
    }

    proptest! {
        /// IMA ADPCM: every block becomes samples-per-block frames of 16-bit PCM, in a WAVE
        /// image the player reads back
        #[test]
        fn ima_becomes_pcm(ch in 1u16..3, blocks in 0usize..4, data in proptest::collection::vec(any::<u8>(), 512)) {
            let align = 256 * ch as usize;
            let spb = (align - 4 * ch as usize) * 8 / (4 * ch as usize) + 1;
            let body = &data.iter().cycle().take(align * blocks).copied().collect::<Vec<u8>>();
            let out = adpcm_to_pcm(riff(0x11, ch, align as u16, spb as u16, body));
            let (fmt, _, size) = wave_chunks(&out).unwrap();
            prop_assert_eq!(&out[fmt..fmt + 2], &[1, 0]);
            prop_assert_eq!(size, blocks * spb * ch as usize * 2);
        }
    }
}

#[cfg(test)]
mod music_tests {
    use super::*;
    use crate::test_guest::enter;
    use proptest::prelude::*;

    /// A mixer of its own (not the global one a running mixer thread would also advance).
    fn dev() -> Dev {
        Dev { bufs: HashMap::default(), waves: HashMap::default(), stream: -1 }
    }

    /// A sound still being decoded is held at what is there (a pause, not zeros played over)
    /// and goes on once more is written; finished, it plays to its end whatever was written
    #[test]
    fn held_at_what_is_decoded() {
        let _g = enter();
        let n = 4000u32;
        let data = mem::heap_alloc(n * 2, true);
        for i in 0..n { w16(data + 2 * i, 16000); }
        let fill = fill_start(data);
        let fmt = Fmt { rate: RATE, channels: 1, bits: 16, float: false, law: 0 };
        let mut d = dev();
        d.bufs.insert(1, music_buf(1, data, n * 2, fmt, 0, 0));
        let mut out = vec![0f32; STEP * 2];
        mix(&mut d, &mut out);
        assert!(out.iter().all(|&x| x == 0.0));
        assert_eq!(d.bufs[&1].pos, 0.0);
        // 1000 frames in: two steps play, the third waits
        assert!(fill.write(1000, || {}));
        let mut played = 0;
        for _ in 0..4 {
            out.iter_mut().for_each(|x| *x = 0.0);
            mix(&mut d, &mut out);
            played += out.chunks_exact(2).filter(|f| f[0] != 0.0).count();
        }
        assert_eq!(played, 2 * STEP);
        assert!(d.bufs[&1].pos as u32 <= 1000);
        // finished at 1000 of 4000 frames: the rest plays (as zeros would: what is in the block)
        fill.finish();
        for _ in 0..20 {
            mix(&mut d, &mut out);
        }
        assert!(!d.bufs[&1].playing);
        fill_drop(data);
        assert!(!fill.write(n, || panic!("written after the drop")));
        mem::heap_free(data);
    }

    proptest! {
        /// A DirectMusic wave of n frames at the output rate with r repeats sounds for exactly
        /// n * (r + 1) frames, then stops; with REPEAT_INFINITE it never stops. -96 dB is silence.
        #[test]
        fn repeats_play_whole_passes(n in 1u32..300, r in 0u32..4, infinite in any::<bool>(), mute in any::<bool>()) {
            let _g = enter();
            let data = mem::heap_alloc(n * 2, true);
            for i in 0..n { w16(data + 2 * i, 16000); }
            let fmt = Fmt { rate: RATE, channels: 1, bits: 16, float: false, law: 0 };
            let mut d = dev();
            let repeats = if infinite { u32::MAX } else { r };
            d.bufs.insert(1, music_buf(1, data, n * 2, fmt, repeats, if mute { -10000 } else { 0 }));
            let total = (n * (r + 1) + 50) as usize;
            let mut out = vec![0f32; total * 2];
            mix(&mut d, &mut out);
            let sounding = out.chunks_exact(2).filter(|f| f[0] != 0.0).count();
            let playing = d.bufs[&1].playing;
            mem::heap_free(data);
            if mute {
                prop_assert_eq!(sounding, 0);
            } else if infinite {
                prop_assert_eq!(sounding, total);
                prop_assert!(playing);
            } else {
                prop_assert_eq!(sounding, (n * (r + 1)) as usize);
                prop_assert!(!playing);
            }
        }
    }
}

#[cfg(test)]
mod enumerate_tests {
    use super::{device_id, ds_devices, guid_text, DEVICE_GUID, DSDEVID_DEFAULT, DSERR_NODRIVER};
    use proptest::prelude::*;

    #[test]
    fn device_ids_as_wine() {
        let id = |b: u8| { let mut g = DSDEVID_DEFAULT; g[0] = b; g };
        // the default playback and voice playback ids, and GUID_NULL: the device the listing has
        assert_eq!(device_id(id(0), true), Ok(DEVICE_GUID));
        assert_eq!(device_id(id(2), true), Ok(DEVICE_GUID));
        assert_eq!(device_id([0; 16], true), Ok(DEVICE_GUID));
        assert_eq!(ds_devices(true)[1].0, Some(DEVICE_GUID));
        // capture: none
        assert_eq!(device_id(id(1), true), Err(DSERR_NODRIVER));
        assert_eq!(device_id(id(3), true), Err(DSERR_NODRIVER));
        // without sound no playback device either
        assert_eq!(device_id(id(0), false), Err(DSERR_NODRIVER));
        // the device's own GUID, and the id the default stays: asked twice, the same answer
        assert_eq!(device_id(DEVICE_GUID, true), Ok(DEVICE_GUID));
        assert_eq!(device_id(id(0), true), device_id(device_id(id(0), true).unwrap(), true));
    }

    proptest! {
        /// any GUID that is not one of the four default ids nor GUID_NULL comes back unchanged;
        /// the answer never depends on anything but the GUID (cmvs64.exe asks every frame)
        #[test]
        fn other_guids_come_back(g in proptest::array::uniform16(any::<u8>()), sound: bool) {
            let default = g[1..] == DSDEVID_DEFAULT[1..] && g[0] <= 3;
            prop_assume!(!default && g != [0; 16]);
            prop_assert_eq!(device_id(g, sound), Ok(g));
            prop_assert_eq!(device_id(g, sound), device_id(g, sound));
        }
    }

    #[test]
    fn primary_then_the_device() {
        let v = ds_devices(true);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].0, v[0].1, v[0].2.as_str()), (None, "Primary Sound Driver", ""));
        assert_eq!(v[1].0, Some(DEVICE_GUID));
        assert_eq!(v[1].2, "{0.0.0.00000000}.{3e7f1d7c-4a2b-4c5d-9a61-1e2f8b30516c}");
        assert_eq!(ds_devices(false).len(), 1);
        // (Windows' own spelling of a known GUID: DSDEVID_DefaultPlayback)
        let g = [0xd6, 0xb0, 0x88, 0xde, 0x91, 0x1c, 0xd2, 0x11, 0x8f, 0xd3, 0x00, 0xc0, 0x4f, 0xa3, 0xc2, 0x1c];
        assert_eq!(guid_text(&g), "{de88b0d6-1c91-11d2-8fd3-00c04fa3c21c}");
    }

    proptest! {
        /// any GUID's text is 38 characters of the registry form and reads back to the same bytes
        #[test]
        fn guid_text_round_trips(g in proptest::array::uniform16(any::<u8>())) {
            let t = guid_text(&g);
            prop_assert_eq!(t.len(), 38);
            let h: String = t.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            prop_assert_eq!(h.len(), 32);
            let b: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect();
            let mut back = [0u8; 16];
            back[..4].copy_from_slice(&[b[3], b[2], b[1], b[0]]);
            back[4..6].copy_from_slice(&[b[5], b[4]]);
            back[6..8].copy_from_slice(&[b[7], b[6]]);
            back[8..].copy_from_slice(&b[8..]);
            prop_assert_eq!(back, g);
        }
    }
}

#[cfg(test)]
mod notify_layout_tests {
    use super::notify_list;
    use proptest::prelude::*;

    /// DSBPOSITIONNOTIFY arrays as mingw-w64's dsound.h lays them out (x86: offset, handle; x64:
    /// offset, 4 bytes of padding, an 8-byte handle), any entries, any padding bytes: the list is
    /// exactly the (offset, event) pairs, whatever the padding holds.
    fn layout(e: &[(u32, u32, u32)], wide: bool) -> Vec<u8> {
        let mut v = Vec::new();
        for &(off, ev, pad) in e {
            v.extend_from_slice(&off.to_le_bytes());
            if wide {
                v.extend_from_slice(&pad.to_le_bytes());
                v.extend_from_slice(&(ev as u64).to_le_bytes());
            } else {
                v.extend_from_slice(&ev.to_le_bytes());
            }
        }
        v
    }

    proptest! {
        #[test]
        fn entries_read_as_written(e in proptest::collection::vec((any::<u32>(), any::<u32>(), any::<u32>()), 0..64),
                                   base in 0u32..0x1000, wide: bool) {
            let bytes = layout(&e, wide);
            let read = |a: u32| {
                let i = (a - base) as usize;
                u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap())
            };
            let got = notify_list(e.len() as u32, base, wide, read);
            let want: Vec<(u32, u32)> = e.iter().map(|&(o, ev, _)| (o, ev)).collect();
            prop_assert_eq!(got, want);
        }
    }

    /// cmvs64.exe's four positions: with the x86 reading the events were the padding (0)
    #[test]
    fn x64_events_are_not_the_padding() {
        let e = [(0, 0x104, 0), (0x1800, 0x108, 0), (0x3000, 0x10c, 0), (0x4800, 0x110, 0)];
        let bytes = layout(&e, true);
        let read = |a: u32| u32::from_le_bytes(bytes[a as usize..a as usize + 4].try_into().unwrap());
        assert_eq!(notify_list(4, 0, true, read).iter().map(|x| x.1).collect::<Vec<_>>(), [0x104, 0x108, 0x10c, 0x110]);
        assert_eq!(notify_list(4, 0, false, read)[1].1, 0, "x86's reading of an x64 array");
    }
}

/// Buffer lifetimes (review B-3): IDirectSoundNotify and the 3D interfaces hold a reference on
/// their buffer until their own last Release; the buffer and its PCM go with the last of all.
#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    const IID_NOTIFY: u32 = 0xb021_0783;
    const IID_3DBUFFER: u32 = 0x279a_fa86;

    fn buffer() -> (u32, u32) {
        let wfx = mem::heap_alloc(20, true);
        // WAVE_FORMAT_PCM, 2 channels, 44100, 176400 bytes/s, align 4, 16 bits
        mem::w16(wfx, 1);
        mem::w16(wfx + 2, 2);
        w32(wfx + 4, 44100);
        w32(wfx + 8, 176400);
        mem::w16(wfx + 12, 4);
        mem::w16(wfx + 14, 16);
        // DSBUFFERDESC: size, DSBCAPS_CTRLPOSITIONNOTIFY, 4096 bytes, reserved, format
        let desc = mem::heap_alloc(36, true);
        for (o, v) in [(0, 36), (4, 0x100), (8, 4096), (16, wfx)] {
            w32(desc + o, v);
        }
        let out = mem::heap_alloc(8, true);
        assert_eq!(call(DS_CreateSoundBuffer, &[0, desc, out]), 0);
        let b = r32(out);
        let data = with_buf(b, |x| x.data).unwrap();
        for p in [wfx, desc, out] {
            mem::heap_free(p);
        }
        (b, data)
    }
    fn qi(b: u32, iid: u32) -> u32 {
        let g = mem::heap_alloc(16, true);
        w32(g, iid);
        let out = mem::heap_alloc(8, true);
        assert_eq!(call(B_QueryInterface, &[b, g, out]), 0);
        let o = r32(out);
        mem::heap_free(g);
        mem::heap_free(out);
        o
    }
    fn alive(b: u32) -> bool {
        with(|d| d.bufs.contains_key(&b))
    }

    /// cmvs's streaming pattern: a buffer, its notify interface, both released: nothing stays.
    #[test]
    fn notify_releases_its_buffer() {
        let _g = enter();
        let (b, data) = buffer();
        let n = qi(b, IID_NOTIFY);
        assert_eq!(with_buf(b, |x| x.refs), Some(2));
        assert_eq!(call(B_Release, &[b]), 1);
        assert!(alive(b));
        assert_eq!(call(N_Release, &[n]), 0);
        assert!(!alive(b));
        assert!(!mem::heap_owns(data) && !mem::heap_owns(b) && !mem::heap_owns(n));
    }

    proptest! {
        /// Any mix of AddRef / Release / QueryInterface on a buffer and its interfaces agrees
        /// with one count each: the buffer lives while any count does, and every block goes at
        /// the end.
        #[test]
        fn counts_as_a_model(ops in proptest::collection::vec((0u8..5, any::<prop::sample::Index>()), 0..40)) {
            let _g = enter();
            let (b, data) = buffer();
            // [0]: the buffer's own count (the guest's), then each interface object's
            let mut objs = vec![(b, 1u32)];
            for (op, k) in ops {
                let live: Vec<usize> = (0..objs.len()).filter(|&i| objs[i].1 > 0).collect();
                if live.is_empty() {
                    break;
                }
                let i = live[k.index(live.len())];
                let (o, n) = objs[i];
                let is_buf = i == 0;
                match op {
                    0 | 1 if objs[0].1 > 0 || objs.iter().skip(1).any(|x| x.1 > 0) => {
                        if alive(b) {
                            let iid = if op == 0 { IID_NOTIFY } else { IID_3DBUFFER };
                            objs.push((qi(b, iid), 1));
                        }
                    }
                    2 => {
                        let r = call(if is_buf { B_AddRef } else { N_AddRef }, &[o]);
                        objs[i].1 = n + 1;
                        if !is_buf {
                            prop_assert_eq!(r, n + 1);
                        }
                    }
                    _ => {
                        call(if is_buf { B_Release } else { N_Release }, &[o]);
                        objs[i].1 = n - 1;
                    }
                }
                let held = objs.iter().skip(1).filter(|x| x.1 > 0).count() as u32;
                prop_assert_eq!(alive(b), objs[0].1 > 0 || held > 0);
                if alive(b) {
                    prop_assert_eq!(with_buf(b, |x| x.refs), Some(objs[0].1 + held));
                }
            }
            for (i, &(o, n)) in objs.iter().enumerate() {
                for _ in 0..n {
                    call(if i == 0 { B_Release } else { N_Release }, &[o]);
                }
            }
            prop_assert!(!alive(b));
            prop_assert!(!mem::heap_owns(data));
        }
    }
}

#[cfg(test)]
mod review_b09_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    fn dev() -> Dev {
        Dev { bufs: HashMap::default(), waves: HashMap::default(), stream: -1 }
    }
    /// A WAVEHDR (32-bit) over `frames` 16-bit mono samples numbered from `first`.
    fn header(first: u32, frames: u32) -> u32 {
        let data = mem::heap_alloc(frames * 2 + 2, true);
        for i in 0..frames {
            w16(data + 2 * i, (first + i + 1) as u16);
        }
        let h = mem::heap_alloc(32, true);
        w32(h, data);
        w32(h + 4, frames * 2);
        set_hflags(h, WHDR_PREPARED | WHDR_INQUEUE);
        h
    }
    fn wave(rate: u32, queue: &[u32]) -> Wave {
        Wave { fmt: Fmt { rate, channels: 1, bits: 16, float: false, law: 0 }, queue: queue.iter().copied().collect(), pos: 0.0, done: Vec::new(), paused: false, closing: false, played: 0, cb: (0, 0, 0), volume: 0xffff_ffff, device: 0, w16: None }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Headers of any lengths at any rate play as one stream: output frame k is sample
        /// floor(k * rate / 44100) of all the headers' samples one after the other, no silent
        /// frame where one header hands over to the next, and each header is done in order.
        #[test]
        fn headers_play_as_one_stream(lens in proptest::collection::vec(0u32..40, 1..6), rate in prop::sample::select(vec![8000u32, 11025, 22050, 44100, 48000, 96000])) {
            let _g = enter();
            let mut first = 0;
            let mut hs = Vec::new();
            for &n in &lens { hs.push(header(first, n)); first += n; }
            let total = first;
            let mut d = dev();
            d.waves.insert(1, wave(rate, &hs));
            let mut out = vec![0f32; 2 * 600];
            mix_waves(&mut d, &mut out);
            let step = rate as f64 / RATE as f64;
            let value = |p: u32| if p < total { (p + 1) as f32 / 32768.0 } else { 0.0 };
            for (k, o) in out.chunks_exact(2).enumerate() {
                let x = k as f64 * step;
                let p = x as u32;
                // (on a sample boundary the mixer's running position and k * step may round to
                // either side: 441 * 8000 / 44100 is 80 here and 79.999... there; both samples
                // are right)
                let edge = (x - x.round()).abs() < 1e-6 && x.round() >= 1.0;
                let ok = o[0] == value(p) || edge && o[0] == value(x.round() as u32 - 1);
                prop_assert!(ok, "frame {} (sample {} of {}): {} not {}", k, p, total, o[0], value(p));
            }
            let w = &d.waves[&1];
            prop_assert_eq!(&w.done, &hs[..w.done.len()].to_vec());
            if (600.0 * step) as u32 > total + 1 { prop_assert_eq!(w.done.len(), hs.len()); }
            for h in hs { mem::heap_free(mem::rp(h)); mem::heap_free(h); }
        }

        /// (the boundary case another run found: 441 * 8000 / 44100 is a whole sample)
        #[test]
        fn headers_play_as_one_stream_on_a_sample_boundary(_x in 0u8..1) {
            let _g = enter();
            let lens = [26u32, 15, 8, 15, 16];
            let mut first = 0;
            let mut hs = Vec::new();
            for &n in &lens { hs.push(header(first, n)); first += n; }
            let mut d = dev();
            d.waves.insert(1, wave(8000, &hs));
            let mut out = vec![0f32; 2 * 600];
            mix_waves(&mut d, &mut out);
            // every sample once, in order, then silence (frame 441 may still be the last one)
            let step = 8000f64 / RATE as f64;
            for (k, o) in out.chunks_exact(2).enumerate() {
                let p = (k as f64 * step) as u32;
                if k == 441 {
                    prop_assert!(o[0] == 0.0 || o[0] == 80.0 / 32768.0);
                } else {
                    prop_assert_eq!(o[0], if p < 80 { (p + 1) as f32 / 32768.0 } else { 0.0 }, "frame {}", k);
                }
            }
            prop_assert_eq!(d.waves[&1].done.len(), 5);
            for h in hs { mem::heap_free(mem::rp(h)); mem::heap_free(h); }
        }

        /// A looping buffer of 1 or 2 frames played much faster than the output (or set past
        /// its end) stays inside its frames.
        #[test]
        fn looping_position_stays_inside(frames in 1u32..4, freq in 44100u32..400_000, start in 0f64..10.0) {
            let _g = enter();
            let data = mem::heap_alloc(frames * 2, true);
            let mut d = dev();
            let fmt = Fmt { rate: RATE, channels: 1, bits: 16, float: false, law: 0 };
            let mut b = music_buf(1, data, frames * 2, fmt, 0, 0);
            b.freq = freq;
            b.looping = true;
            b.playing = true;
            b.pos = start % frames as f64;
            d.bufs.insert(1, b);
            let mut out = vec![0f32; 2 * 200];
            mix(&mut d, &mut out);
            prop_assert!(d.bufs[&1].pos < frames as f64, "pos {}", d.bufs[&1].pos);
            mem::heap_free(data);
        }
    }

    /// waveOutClose with headers queued is WAVERR_STILLPLAYING and the device stays.
    #[test]
    fn close_with_queued_headers_is_still_playing() {
        let _g = enter();
        let h = header(0, 10);
        let hwo = 0x7a_0001;
        with(|d| d.waves.insert(hwo, wave(RATE, &[h])));
        assert_eq!(call(waveOutClose, &[hwo]), 33);
        assert!(with(|d| d.waves.get(&hwo).is_some_and(|w| !w.closing)));
        assert_eq!(call(waveOutReset, &[hwo]), 0);
        with(|d| d.waves.remove(&hwo));
        mem::heap_free(mem::rp(h));
        mem::heap_free(h);
    }

    /// PlaySound(SND_MEMORY) of what is no RIFF image, or one whose length runs past mapped
    /// memory, plays nothing and asks for no more than is there.
    #[test]
    fn memory_sounds_need_a_riff_header() {
        let _g = enter();
        let p = mem::heap_alloc(64, true);
        mem::write(p, b"JUNK\xf0\xff\xff\xff");
        assert_eq!(play_sound(p, 0, SND_MEMORY | SND_ASYNC, false), 0);
        mem::write(p, b"RIFF\xf0\xff\xff\x7fWAVE");
        assert_eq!(play_sound(p, 0, SND_MEMORY | SND_ASYNC, false), 0);
        mem::heap_free(p);
    }

    /// SetCurrentPosition past the buffer: DSERR_INVALIDPARAM, the position unchanged.
    #[test]
    fn position_past_the_end_is_refused() {
        let _g = enter();
        let data = mem::heap_alloc(400, true);
        let fmt = Fmt { rate: RATE, channels: 2, bits: 16, float: false, law: 0 };
        let key = 0x7b_0001;
        with(|d| d.bufs.insert(key, music_buf(key, data, 400, fmt, 0, 0)));
        assert_eq!(call(B_SetCurrentPosition, &[key, 396]), 0);
        assert_eq!(with(|d| d.bufs[&key].pos), 99.0);
        assert_eq!(call(B_SetCurrentPosition, &[key, 400]), DSERR_INVALIDPARAM);
        assert_eq!(with(|d| d.bufs[&key].pos), 99.0);
        music_set(key, Some(1e9), None);
        assert_eq!(with(|d| d.bufs[&key].pos), 99.0);
        with(|d| d.bufs.remove(&key));
        mem::heap_free(data);
    }
}
