//! XAudio2 2.7 (CoCreateInstance CLSID_XAudio2): source voices play queued XAUDIO2_BUFFERs
//! from guest memory, through submix voices' volumes, into a mastering voice that is a stream
//! of kura's audio output. The engine is one guest thread: each 10 ms pass it calls the
//! voices' OnVoiceProcessingPassStart, mixes, delivers OnBufferStart / OnBufferEnd /
//! OnLoopEnd / OnStreamEnd, then OnVoiceProcessingPassEnd (as XAudio2 calls them from its
//! own thread). No effects, filters or output matrices; PCM and float formats only. x64: the
//! same, with pointer-sized fields and the float arguments in XMM registers.

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
}

struct Engine {
    voices: HashMap<u32, Voice>,
    master: u32,
    callbacks: Vec<u32>,
    running: bool,
    stream: i32,
    thread: bool,
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();

fn with<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    let mut g = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Engine { voices: HashMap::default(), master: 0, callbacks: Vec::new(), running: false, stream: -1, thread: false }))
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

fn gain(e: &Engine, mut id: u32) -> f32 {
    let mut g = 1.0;
    for _ in 0..8 {
        let Some(v) = e.voices.get(&id) else { break };
        g *= v.volume;
        if v.kind == Kind::Master {
            break;
        }
        id = if v.dest == 0 { e.master } else { v.dest };
    }
    g
}

/// Mixes one pass of every started source voice into `out`; returns the callbacks
/// (object, method, argument) the pass produced.
fn mix(e: &mut Engine, out: &mut [f32]) -> Vec<(u32, u32, u32)> {
    let mut ev = Vec::new();
    let gains: HashMap<u32, f32> = e.voices.iter().filter(|(_, v)| v.kind == Kind::Source).map(|(&k, _)| (k, gain(e, k))).collect();
    for (id, v) in e.voices.iter_mut() {
        ev.append(&mut v.pending);
        if v.kind != Kind::Source || !v.started {
            continue;
        }
        let g = gains[id];
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
            let (l0, r0) = sample_at(b.data, &v.fmt, i as usize);
            let (l1, r1) = if i + 1 < b.end { sample_at(b.data, &v.fmt, i as usize + 1) } else { (l0, r0) };
            o[0] += (l0 + (l1 - l0) * t) * g;
            o[1] += (r0 + (r1 - r0) * t) * g;
            v.pos += step;
            v.played += step;
            if b.loops > 0 && v.pos >= b.loop_end as f64 {
                v.pos -= (b.loop_end - b.loop_begin).max(1) as f64;
                if b.loops != LOOP_INFINITE {
                    b.loops -= 1;
                }
                if v.cb != 0 {
                    ev.push((v.cb, ON_LOOP_END, b.ctx));
                }
            } else if v.pos >= b.end as f64 {
                let over = v.pos - b.end as f64;
                let b = v.queue.pop_front().unwrap();
                if v.cb != 0 {
                    ev.push((v.cb, ON_BUFFER_END, b.ctx));
                    if b.flags & END_OF_STREAM != 0 {
                        ev.push((v.cb, ON_STREAM_END, u32::MAX));
                    }
                }
                v.began = false;
                if let Some(n) = v.queue.front() {
                    v.began = true;
                    v.pos = n.begin as f64 + over;
                    if v.cb != 0 {
                        ev.push((v.cb, ON_BUFFER_START, n.ctx));
                    }
                }
            }
        }
    }
    ev
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

fn engine_thread(c: &mut Ctx) -> u32 {
    let mut out = vec![0f32; STEP * 2];
    let lock = Mutex::new(());
    loop {
        let (stream, running) = with(|e| (e.stream, e.running));
        if !running || kura::audio::queued(stream) >= AHEAD {
            drop(WAKE.wait_timeout(lock.lock().unwrap(), Duration::from_millis(5)).unwrap());
            continue;
        }
        let (engine_cbs, starts): (Vec<u32>, Vec<(u32, u32)>) = with(|e| {
            let s = e.voices.values().filter(|v| v.kind == Kind::Source && v.started && v.cb != 0).map(|v| (v.cb, bytes_required(v))).collect();
            (e.callbacks.clone(), s)
        });
        for &cb in &engine_cbs {
            callback(c, cb, 0, None);
        }
        for &(cb, n) in &starts {
            callback(c, cb, ON_PASS_START, Some(n));
        }
        out.iter_mut().for_each(|x| *x = 0.0);
        let ev = with(|e| mix(e, &mut out));
        for (cb, m, a) in ev {
            callback(c, cb, m, (a != u32::MAX).then_some(a));
        }
        for &(cb, _) in &starts {
            callback(c, cb, ON_PASS_END, None);
        }
        for &cb in &engine_cbs {
            callback(c, cb, 1, None);
        }
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

/// CoCreateInstance(CLSID_XAudio2 / CLSID_XAudio2_Debug, 2.7)
pub fn create_instance(clsid: u32) -> Option<u32> {
    if !matches!(r32(clsid), 0x5a50_8685 | 0xdb05_ea35) {
        return None;
    }
    let o = mem::heap_alloc(16, true);
    mem::wp(o, xa_vtbl());
    w32(refs(o), 1);
    Some(o)
}

fn X_QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    // IUnknown, IXAudio2 (2.7)
    if matches!(r32(iid), 0 | 0x8bcf_1f58) {
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
        with(|e| {
            e.running = false;
            e.voices.clear();
            e.callbacks.clear();
            e.master = 0;
        });
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
    let f = p + 1028;
    w16(f, 0xfffe);
    w16(f + 2, 2);
    w32(f + 4, RATE);
    w32(f + 8, RATE * 8);
    w16(f + 12, 8);
    w16(f + 14, 32);
    w16(f + 16, 22);
    w16(f + 18, 32);
    w32(f + 20, 3); // SPEAKER_FRONT_LEFT | RIGHT
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {00000003-0000-0010-8000-00aa00389b71}
    for (i, b) in [3u8, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71].iter().enumerate() {
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
    0
}

/// XAUDIO2_VOICE_SENDS { SendCount, pSends } of XAUDIO2_SEND_DESCRIPTOR { Flags, pOutputVoice }
fn first_send(sends: u32) -> u32 {
    if sends == 0 || r32(sends) == 0 {
        return 0;
    }
    rp(rp(sends + ps()) + ps())
}

fn new_voice(kind: Kind, fmt: Fmt, dest: u32, cb: u32) -> u32 {
    let vt = if kind == Kind::Source { src_vtbl() } else { voice_vtbl() };
    let o = mem::heap_alloc(8, true);
    mem::wp(o, vt);
    let v = Voice { kind, fmt, volume: 1.0, dest, cb, started: false, queue: VecDeque::new(), pos: 0.0, began: false, ratio: 1.0, played: 0.0, pending: Vec::new() };
    with(|e| {
        if v.kind == Kind::Master {
            e.master = o;
        }
        e.voices.insert(o, v);
    });
    o
}

/// CreateSourceVoice(pp, format, flags, max ratio, callback, sends, effects)
fn X_CreateSourceVoice(c: &mut Ctx) -> u32 {
    let (pp, wf, cb, sends) = (c.a(1), c.a(2), c.a(5), c.a(6));
    let tag = r16(wf);
    let sub = if tag == 0xfffe { r32(wf + 24) } else { tag as u32 };
    let bits = r16(wf + 14) as u32;
    if !matches!(sub, 1 | 3) || !matches!(bits, 8 | 16 | 24 | 32) || r32(wf + 4) == 0 {
        eprintln!("sake: XAudio2 source format {tag:#x} ({bits} bits) not supported");
        putp(pp, 0);
        return E_INVALIDARG;
    }
    let fmt = Fmt { rate: r32(wf + 4), channels: (r16(wf + 2) as u32).max(1), bits, float: sub == 3 };
    putp(pp, new_voice(Kind::Source, fmt, first_send(sends), cb));
    0
}
/// CreateSubmixVoice(pp, channels, rate, flags, stage, sends, effects)
fn X_CreateSubmixVoice(c: &mut Ctx) -> u32 {
    let (pp, ch, rate, sends) = (c.a(1), c.a(2), c.a(3), c.a(6));
    let fmt = Fmt { rate: if rate == 0 { RATE } else { rate }, channels: ch.max(1), bits: 32, float: true };
    putp(pp, new_voice(Kind::Submix, fmt, first_send(sends), 0));
    0
}
/// CreateMasteringVoice(pp, channels, rate, flags, device, effects)
fn X_CreateMasteringVoice(c: &mut Ctx) -> u32 {
    let (pp, ch, rate) = (c.a(1), c.a(2), c.a(3));
    if with(|e| e.master != 0) {
        putp(pp, 0);
        return XAUDIO2_E_INVALID_CALL;
    }
    let fmt = Fmt { rate: if rate == 0 { RATE } else { rate }, channels: if ch == 0 { 2 } else { ch }, bits: 32, float: true };
    putp(pp, new_voice(Kind::Master, fmt, 0, 0));
    start_thread();
    with(|e| e.running = true);
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

/// XAUDIO2_VOICE_DETAILS: CreationFlags, InputChannels, InputSampleRate
fn V_GetVoiceDetails(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if let Some((ch, rate)) = with_voice(c.a(0), |v| (v.fmt.channels, v.fmt.rate)) {
        w32(p, 0);
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
fn V_SetEffectChain(_: &mut Ctx) -> u32 {
    0
}
fn V_EnableEffect(_: &mut Ctx) -> u32 {
    0
}
fn V_DisableEffect(_: &mut Ctx) -> u32 {
    0
}
fn V_GetEffectState(c: &mut Ctx) -> u32 {
    put32(c.a(2), 1);
    0
}
fn V_SetEffectParameters(_: &mut Ctx) -> u32 {
    0
}
fn V_GetEffectParameters(_: &mut Ctx) -> u32 {
    E_INVALIDARG
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
fn V_DestroyVoice(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    with(|e| {
        e.voices.remove(&this);
        if e.master == this {
            e.master = 0;
        }
    });
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
    // (x64: the fields past pAudioData move by 4, pContext by 8)
    let k = ps() - 4;
    let r = |o: u32| r32(p + if o <= 8 { o } else if o < 32 { o + k } else { o + 2 * k });
    with_voice(this, |v| {
        let frames = r(4) / v.fmt.block().max(1);
        let begin = r(12).min(frames);
        let end = if r(16) == 0 { frames } else { (begin + r(16)).min(frames) };
        let loops = r(28);
        let loop_begin = if loops > 0 { r(20).clamp(begin, end) } else { begin };
        let loop_end = if loops > 0 && r(24) != 0 { (loop_begin + r(24)).min(end) } else { end };
        v.queue.push_back(XBuf { flags: r(0), data: r(8), begin, end, loop_begin, loop_end, loops, ctx: r(32) });
    })
    .map_or(E_INVALIDARG, |_| 0)
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
/// XAUDIO2_VOICE_STATE: pCurrentBufferContext, BuffersQueued, SamplesPlayed (u64; 8-aligned)
fn S_GetState(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let (ctx, n, played) = with_voice(c.a(0), |v| (v.queue.front().map_or(0, |b| b.ctx), v.queue.len() as u32, v.played as u64)).unwrap_or((0, 0, 0));
    let k = ps();
    mem::wp(p, ctx);
    w32(p + k, n);
    mem::w64(p + 8 + 2 * (k - 4), played);
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
