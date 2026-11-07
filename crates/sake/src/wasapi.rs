//! WASAPI shared-mode output on mmdevapi.rs's one render endpoint: IMMDevice::Activate's
//! IAudioClient (with its IAudioClockAdjustment), its services (IAudioRenderClient,
//! IAudioClock, IAudioStreamVolume, and the session's IAudioSessionControl(2),
//! ISimpleAudioVolume, IChannelAudioVolume), IAudioSessionManager(2) with its session
//! enumerator, and IAudioEndpointVolume.
//!
//! A client's released frames queue here at its own rate; one host thread (like dsound's mixer)
//! takes a 10 ms period of every running client, resampled to kura's 44.1 kHz stereo and scaled
//! by its stream, session and endpoint volumes, into one stream of kura's audio output, and
//! signals event-driven clients each period (once started, also while stopped, as Windows
//! does). Formats: Windows' shared-mode rules (any valid PCM or float format at the mix rate
//! and channel count; RATEADJUST: any rate; AUTOCONVERTPCM: anything valid). Exclusive mode is
//! refused as when the user disallows it; there is no capture endpoint. FMOD (Unity 5.x and
//! later) renders through this. Behaviour checked by Wine's mmdevapi render tests
//! (tests/winetest) and host/tests/wasapi.c.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::api::{vtable, Ctx, Handler};
use crate::mem::{self, ps, put64, putp, r16, r32, rp, w16, w32, wp};
use crate::sound::{sample_at, Fmt, AHEAD, RATE, STEP};
use crate::HashMap;

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_POINTER: u32 = 0x8000_4003;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_UNEXPECTED: u32 = 0x8000_ffff;
/// HRESULT_FROM_WIN32(RPC_X_NULL_REF_POINTER): the session interfaces' NULL pointer error
const NULL_REF: u32 = 0x8007_06f4;
const NOT_INITIALIZED: u32 = 0x8889_0001;
const ALREADY_INITIALIZED: u32 = 0x8889_0002;
const NOT_STOPPED: u32 = 0x8889_0005;
const BUFFER_TOO_LARGE: u32 = 0x8889_0006;
const OUT_OF_ORDER: u32 = 0x8889_0007;
const UNSUPPORTED_FORMAT: u32 = 0x8889_0008;
const INVALID_SIZE: u32 = 0x8889_0009;
const BUFFER_OPERATION_PENDING: u32 = 0x8889_000b;
const EXCLUSIVE_MODE_NOT_ALLOWED: u32 = 0x8889_000e;
const EVENTHANDLE_NOT_EXPECTED: u32 = 0x8889_0011;
const EVENTHANDLE_NOT_SET: u32 = 0x8889_0014;
const INVALID_STREAM_FLAG: u32 = 0x8889_0021;

/// AUDCLNT_STREAMFLAGS_* and AUDCLNT_SESSIONFLAGS_*: what Initialize takes
const LOOPBACK: u32 = 0x2_0000;
const EVENTCALLBACK: u32 = 0x4_0000;
const RATEADJUST: u32 = 0x10_0000;
const AUTOCONVERTPCM: u32 = 0x8000_0000;
const VALID_FLAGS: u32 = 0x1_0000 | LOOPBACK | EVENTCALLBACK | 0x8_0000 | RATEADJUST | 0x100_0000 | 0x800_0000 | AUTOCONVERTPCM | 0x7000_0000;
/// AUDCLNT_BUFFERFLAGS_SILENT
const SILENT: u32 = 2;

/// the device period (REFERENCE_TIME, 100 ns): default and minimum
pub(crate) const PERIOD: u64 = 100_000;
pub(crate) const MIN_PERIOD: u64 = 30_000;
/// the endpoint volume's range (dB) and step
const MIN_DB: f32 = -65.25;
const MAX_DB: f32 = 0.0;
const STEP_DB: f32 = 0.03125;

/// x86 stdcall: the handler pops this many bytes of arguments (x64 ignores it)
fn b(n: u32) -> u32 {
    4 * n
}

// ---------------------------------------------------------------------------------------------
// formats

/// The mix format: what the engine renders (32-bit float stereo at kura's rate)
pub(crate) const MIX: Fmt = Fmt { rate: RATE, channels: 2, bits: 32, float: true };

/// A WAVEFORMATEX(TENSIBLE)'s fields as the rules look at them
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wave {
    pub tag: u16,
    pub channels: u16,
    pub rate: u32,
    pub avg: u32,
    pub block: u16,
    pub bits: u16,
    pub cb: u16,
    pub valid: u16,
    /// SubFormat's first word (KSDATAFORMAT_SUBTYPE_PCM 1, IEEE_FLOAT 3), when the rest of it is
    /// the audio base GUID's
    pub sub: Option<u32>,
}

fn read_wave(p: u32) -> Wave {
    let base = r16(p + 28) == 0 && r16(p + 30) == 0x10 && mem::slice(p + 32, 8) == [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
    let ext = r16(p) == 0xfffe;
    Wave {
        tag: r16(p),
        channels: r16(p + 2),
        rate: r32(p + 4),
        avg: r32(p + 8),
        block: r16(p + 12),
        bits: r16(p + 14),
        cb: r16(p + 16),
        valid: if ext { r16(p + 18) } else { 0 },
        sub: (ext && base).then(|| r32(p + 24)),
    }
}

/// Whether Windows' audio engine takes `w` (S_OK) or why not (E_INVALIDARG for a malformed
/// one, AUDCLNT_E_UNSUPPORTED_FORMAT for a well-formed one it does not play); `compatible`:
/// whether its rate and channels fit the stream (else a valid one is unsupported too).
pub(crate) fn check(w: &Wave, compatible: bool) -> Result<Fmt, u32> {
    // (WAVEFORMATEX: PCM or IEEE float of one or two channels, all of its bits valid)
    let (float, valid) = if w.tag == 0xfffe {
        if w.cb < 22 {
            return Err(E_INVALIDARG);
        }
        // (another SubFormat: unsupported, once the rest is well-formed)
        let float = match w.sub {
            Some(1) => Some(false),
            Some(3) => Some(true),
            _ => None,
        };
        (float, w.valid)
    } else {
        let float = match w.tag {
            1 => false,
            3 => true,
            _ => return Err(UNSUPPORTED_FORMAT),
        };
        if w.channels > 2 {
            return Err(E_INVALIDARG);
        }
        (Some(float), w.bits)
    };
    if w.channels == 0 || w.rate == 0 || w.block as u32 != w.channels as u32 * w.bits as u32 / 8 || w.avg != w.block as u32 * w.rate || valid == 0 || valid > w.bits {
        return Err(E_INVALIDARG);
    }
    let Some(float) = float else { return Err(UNSUPPORTED_FORMAT) };
    if float {
        match w.bits {
            32 if valid == 32 => {}
            32 | 64 => return Err(UNSUPPORTED_FORMAT),
            _ => return Err(E_INVALIDARG),
        }
    } else {
        match w.bits {
            8 | 16 | 24 | 32 if valid == w.bits || (w.bits, valid) == (32, 24) => {}
            8 | 16 | 24 | 32 => return Err(UNSUPPORTED_FORMAT),
            _ => return Err(E_INVALIDARG),
        }
    }
    if !compatible {
        return Err(UNSUPPORTED_FORMAT);
    }
    Ok(Fmt { rate: w.rate, channels: w.channels as u32, bits: w.bits as u32, float })
}

/// What shared mode does with a format: Initialize's answer (`flags`: its stream flags) and
/// IsFormatSupported's (S_FALSE: valid, but another rate or channel count than the mix)
pub(crate) fn shared_initialize(w: &Wave, flags: u32) -> Result<Fmt, u32> {
    let compatible = if flags & AUTOCONVERTPCM != 0 {
        true
    } else if flags & RATEADJUST != 0 {
        w.channels as u32 == MIX.channels
    } else {
        w.rate == MIX.rate && w.channels as u32 == MIX.channels
    };
    check(w, compatible)
}
pub(crate) fn shared_supported(w: &Wave) -> u32 {
    match check(w, true) {
        Ok(_) if w.rate == MIX.rate && w.channels as u32 == MIX.channels => S_OK,
        Ok(_) => S_FALSE,
        Err(hr) => hr,
    }
}

/// The mix format as a CoTaskMemAlloc'd WAVEFORMATEXTENSIBLE.
fn mix_format() -> u32 {
    let p = mem::heap_alloc(40, true);
    w16(p, 0xfffe);
    w16(p + 2, MIX.channels as u16);
    w32(p + 4, MIX.rate);
    w32(p + 8, MIX.rate * MIX.block());
    w16(p + 12, MIX.block() as u16);
    w16(p + 14, 32);
    w16(p + 16, 22);
    w16(p + 18, 32);
    // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT; KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    w32(p + 20, 3);
    w32(p + 24, 3);
    w16(p + 28, 0);
    w16(p + 30, 0x10);
    mem::write(p + 32, &[0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
    p
}

/// The buffer a shared-mode stream of `rate` gets for a requested duration (100 ns): the
/// duration in frames (rounded as MulDiv does), at least two device periods.
pub(crate) fn buffer_frames(rate: u32, duration: u64) -> u32 {
    let frames = |d: u64| ((d as u128 * rate as u128 + 5_000_000) / 10_000_000) as u32;
    frames(duration).max(frames(2 * PERIOD))
}

// ---------------------------------------------------------------------------------------------
// clients and sessions

struct Client {
    refs: u32,
    fmt: Option<Fmt>,
    flags: u32,
    frames: u32,
    event: u32,
    running: bool,
    /// started once: an event-driven stream's event is signalled every period from then on
    started: bool,
    /// released frames not yet played (stereo, at the stream's rate)
    queue: VecDeque<[f32; 2]>,
    /// where in the queue's front frame the next output frame falls (resampling)
    frac: f64,
    /// IAudioClockAdjustment::SetSampleRate's rate (0: the format's)
    rate: f32,
    /// frames played since the stream started (Reset: 0)
    played: u64,
    /// the frames the last GetBuffer handed out, until released
    taken: Option<u32>,
    /// where GetBuffer's frames are written (the buffer, in guest memory)
    mem: u32,
    channel: [f32; 8],
    session: usize,
    /// its IAudioSessionControl (GetService hands out the same one)
    ctl: u32,
}

impl Client {
    fn padding(&self) -> u32 {
        self.queue.len() as u32
    }
}

struct Session {
    guid: [u32; 4],
    name: Vec<u16>,
    icon: Vec<u16>,
    group: [u32; 4],
    volume: f32,
    mute: bool,
    channel: [f32; 8],
}

struct Endpoint {
    db: f32,
    mute: bool,
    channel: [f32; 2],
}

struct Engine {
    clients: HashMap<u32, Client>,
    sessions: Vec<Session>,
    endpoint: Endpoint,
    stream: i32,
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();

fn with<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    let mut g = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Engine { clients: HashMap::default(), sessions: Vec::new(), endpoint: Endpoint { db: MAX_DB, mute: false, channel: [MAX_DB; 2] }, stream: 0 }))
}

/// `f` on client `this` (an IAudioClient), or None when it is not one
fn client<R>(this: u32, f: impl FnOnce(&mut Client) -> R) -> Option<R> {
    with(|e| e.clients.get_mut(&this).map(f))
}

/// A grouping GUID for a new session (not GUID_NULL, as Windows gives)
fn new_guid(seed: u32) -> [u32; 4] {
    let t = crate::k32::qpc();
    [0x5a4b_0000 | (seed & 0xffff), t as u32 | 1, (t >> 32) as u32, 0x8000_0000 | seed]
}

/// The session of `guid` (all zeros: the process's default session, as NULL is), made the
/// first time
fn session_of(e: &mut Engine, guid: [u32; 4]) -> usize {
    if let Some(i) = e.sessions.iter().position(|s| s.guid == guid) {
        return i;
    }
    let n = e.sessions.len();
    e.sessions.push(Session { guid, name: Vec::new(), icon: Vec::new(), group: new_guid(n as u32), volume: 1.0, mute: false, channel: [1.0; 8] });
    n
}
fn guid_arg(p: u32) -> [u32; 4] {
    if p == 0 { [0; 4] } else { std::array::from_fn(|i| r32(p + 4 * i as u32)) }
}

fn endpoint_gain(ep: &Endpoint, ch: usize) -> f32 {
    let db = ep.db + ep.channel[ch.min(1)];
    if ep.mute || db <= MIN_DB { 0.0 } else { 10f32.powf(db / 20.0) }
}

/// Takes one period (`out`: stereo frames at RATE) from client `c`, resampled and scaled by
/// `gain` (left, right); returns whether it ran dry.
fn render(c: &mut Client, gain: [f32; 2], out: &mut [f32]) -> bool {
    let Some(f) = c.fmt else { return false };
    let rate = if c.rate > 0.0 { c.rate as f64 } else { f.rate as f64 };
    let step = rate / RATE as f64;
    let mut dry = false;
    for o in out.chunks_exact_mut(2) {
        let Some(&a) = c.queue.front() else {
            dry = true;
            break;
        };
        let bb = c.queue.get(1).copied().unwrap_or(a);
        let t = c.frac as f32;
        o[0] += (a[0] + (bb[0] - a[0]) * t) * gain[0];
        o[1] += (a[1] + (bb[1] - a[1]) * t) * gain[1];
        c.frac += step;
        while c.frac >= 1.0 && !c.queue.is_empty() {
            c.frac -= 1.0;
            c.queue.pop_front();
            c.played += 1;
        }
    }
    dry
}

/// A client's gain for the left and right output: its stream volumes, its session's volume,
/// mute and channel volumes, the endpoint's volume
fn gain_of(e: &Engine, c: &Client) -> [f32; 2] {
    let s = &e.sessions[c.session];
    let mono = c.fmt.is_some_and(|f| f.channels == 1);
    std::array::from_fn(|k| {
        let ch = if mono { 0 } else { k };
        let sv = if s.mute { 0.0 } else { s.volume * s.channel[ch] };
        c.channel[ch] * sv * endpoint_gain(&e.endpoint, k)
    })
}

/// One period of every running client into `out`; the event handles to signal.
fn mix(e: &mut Engine, out: &mut [f32]) -> Vec<u32> {
    let gains: Vec<(u32, [f32; 2])> = e.clients.iter().filter(|(_, c)| c.running).map(|(&k, c)| (k, gain_of(e, c))).collect();
    for (k, g) in gains {
        if let Some(c) = e.clients.get_mut(&k) {
            render(c, g, out);
        }
    }
    e.clients.values().filter(|c| c.started && c.event != 0).map(|c| c.event).collect()
}

pub(crate) fn snap_wake() {
    WAKE.notify_all();
}

/// A restored save state: the engine thread again, if it ran.
pub(crate) fn snap_restore() {
    if with(|e| e.stream) > 0 {
        spawn_engine();
    }
}

fn start_engine() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        with(|e| e.stream = kura::audio::open());
        spawn_engine();
    });
}

fn spawn_engine() {
    std::thread::Builder::new()
        .name("wasapi".into())
        .spawn(|| {
            kura::snap::register("wasapi", false, 0);
            let mut out = vec![0f32; STEP * 2];
            let lock = Mutex::new(());
            loop {
                kura::snap::idle();
                drop(WAKE.wait_timeout(lock.lock().unwrap(), Duration::from_millis(10)).unwrap());
                kura::snap::busy();
                let id = with(|e| e.stream);
                while kura::audio::queued(id) < AHEAD {
                    out.iter_mut().for_each(|x| *x = 0.0);
                    let events = with(|e| mix(e, &mut out));
                    for ev in events {
                        crate::k32::set_event(ev);
                    }
                    kura::audio::write(id, &out);
                }
            }
        })
        .expect("wasapi thread");
}

// ---------------------------------------------------------------------------------------------
// COM plumbing

type Iid = (u32, u16, u16);
fn guid_is(p: u32, g: Iid) -> bool {
    p != 0 && r32(p) == g.0 && r16(p + 4) == g.1 && r16(p + 6) == g.2
}
fn is_unknown(iid: u32) -> bool {
    iid != 0 && (0..4).all(|i| r32(iid + 4 * i) == [0, 0, 0xc0, 0x4600_0000][i as usize])
}
fn farg(c: &Ctx, i: u32) -> f32 {
    f32::from_bits(if mem::x64() { c.xmm(i) as u32 } else { c.a(i) })
}
fn put_f32(p: u32, v: f32) {
    w32(p, v.to_bits());
}
/// A REFERENCE_TIME argument: x86 two stack slots from `i`, x64 argument `x64`
fn time_arg(c: &Ctx, i: u32, x64: u32) -> u64 {
    if mem::x64() { c.a64(x64) } else { c.a(i) as u64 | (c.a(i + 1) as u64) << 32 }
}
/// A CoTaskMemAlloc'd copy of `s` (NUL-terminated)
fn task_wstr(s: &[u16]) -> u32 {
    let p = mem::heap_alloc(2 * s.len() as u32 + 2, true);
    for (i, &v) in s.iter().enumerate() {
        w16(p + 2 * i as u32, v);
    }
    p
}

pub(crate) const IID_CLIENT: Iid = (0x1cb9_ad4c, 0xdbfa, 0x4c32);
const IID_RENDER: Iid = (0xf294_acfc, 0x3146, 0x4483);
const IID_CLOCK: Iid = (0xcd63_314f, 0x3fba, 0x4a1b);
const IID_CLOCK_ADJUST: Iid = (0xf6e4_c0a0, 0x46d9, 0x4fb8);
const IID_SIMPLE_VOLUME: Iid = (0x87ce_5498, 0x68d6, 0x44e5);
const IID_STREAM_VOLUME: Iid = (0x9301_4887, 0x242d, 0x4068);
const IID_CHANNEL_VOLUME: Iid = (0x1c15_8861, 0xb533, 0x4b30);
const IID_SESSION: Iid = (0xf4b1_a599, 0x7266, 0x4319);
const IID_SESSION2: Iid = (0xbfb7_ff88, 0x7239, 0x4fc9);
pub(crate) const IID_MANAGER: Iid = (0xbfa9_71f1, 0x4d5e, 0x40bb);
pub(crate) const IID_MANAGER2: Iid = (0x77aa_99a0, 0x1bd6, 0x484f);
const IID_ENUMERATOR: Iid = (0xe2f5_bb11, 0x0570, 0x40ca);
pub(crate) const IID_ENDPOINT_VOLUME: Iid = (0x5cdf_2c82, 0x841e, 0x4546);

// Service objects: vtable, the client they hold (0: none), reference count, session index.
// A service holds one reference of its client while it lives.

fn service(vtbl: u32, owner: u32, session: usize) -> u32 {
    let o = mem::heap_alloc(4 * ps(), true);
    wp(o, vtbl);
    wp(o + ps(), owner);
    w32(o + 2 * ps(), 1);
    w32(o + 3 * ps(), session as u32);
    if owner != 0 {
        client(owner, |c| c.refs += 1);
    }
    o
}
fn owner(this: u32) -> u32 {
    rp(this + ps())
}
fn session_idx(this: u32) -> usize {
    r32(this + 3 * ps()) as usize
}
fn svc_addref(c: &mut Ctx) -> u32 {
    addref_obj(c.a(0))
}
fn addref_obj(this: u32) -> u32 {
    let r = this + 2 * ps();
    let n = r32(r) + 1;
    w32(r, n);
    if n == 1 && owner(this) != 0 {
        client(owner(this), |c| c.refs += 1);
    }
    n
}
fn svc_release(c: &mut Ctx) -> u32 {
    let (this, r) = (c.a(0), c.a(0) + 2 * ps());
    let n = r32(r).saturating_sub(1);
    w32(r, n);
    if n == 0 && owner(this) != 0 {
        release_client(owner(this));
    }
    n
}
/// QueryInterface of a service answering IUnknown and `own`
fn svc_qi(c: &mut Ctx, own: &[Iid]) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is_unknown(iid) || own.iter().any(|&g| guid_is(iid, g)) {
        addref_obj(this);
        putp(out, this);
        S_OK
    } else {
        putp(out, 0);
        E_NOINTERFACE
    }
}

fn release_client(this: u32) -> u32 {
    with(|e| {
        let Some(c) = e.clients.get_mut(&this) else { return 0 };
        c.refs = c.refs.saturating_sub(1);
        let n = c.refs;
        if n == 0 {
            e.clients.remove(&this);
        }
        n
    })
}

// ---------------------------------------------------------------------------------------------
// IAudioClient {1cb9ad4c-dbfa-4c32-b178-c2f568a703b2}, and its IAudioClockAdjustment
// {f6e4c0a0-46d9-4fb8-be21-57a3ef2b626c} at the next pointer (one object: one count)

/// IMMDevice::Activate(IID_IAudioClient): a new client, not initialized
pub(crate) fn new_client() -> u32 {
    static V: OnceLock<(u32, u32)> = OnceLock::new();
    let (v, va) = *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[
            (ac_qi, b(3)),
            (ac_addref, b(1)),
            (ac_release, b(1)),
            (ac_initialize, b(9)),
            (ac_buffer_size, b(2)),
            (ac_latency, b(2)),
            (ac_padding, b(2)),
            (ac_supported, b(4)),
            (ac_mix_format, b(2)),
            (ac_period, b(3)),
            (ac_start, b(1)),
            (ac_stop, b(1)),
            (ac_reset, b(1)),
            (ac_set_event, b(2)),
            (ac_service, b(3)),
        ];
        let a: &[(Handler, u32)] = &[(adj_qi, b(3)), (adj_addref, b(1)), (adj_release, b(1)), (adj_rate, b(2))];
        (vtable("IAudioClient", m), vtable("IAudioClockAdjustment", a))
    });
    let o = mem::heap_alloc(2 * ps(), true);
    wp(o, v);
    wp(o + ps(), va);
    with(|e| {
        let session = session_of(e, [0; 4]);
        e.clients.insert(
            o,
            Client { refs: 1, fmt: None, flags: 0, frames: 0, event: 0, running: false, started: false, queue: VecDeque::new(), frac: 0.0, rate: 0.0, played: 0, taken: None, mem: 0, channel: [1.0; 8], session, ctl: 0 },
        )
    });
    o
}

fn client_qi(this: u32, iid: u32, out: u32) -> u32 {
    if out == 0 {
        return E_POINTER;
    }
    let to = if is_unknown(iid) || guid_is(iid, IID_CLIENT) {
        this
    } else if guid_is(iid, IID_CLOCK_ADJUST) {
        this + ps()
    } else {
        putp(out, 0);
        return E_NOINTERFACE;
    };
    client(this, |c| c.refs += 1);
    putp(out, to);
    S_OK
}
fn ac_qi(c: &mut Ctx) -> u32 {
    client_qi(c.a(0), c.a(1), c.a(2))
}
fn ac_addref(c: &mut Ctx) -> u32 {
    client(c.a(0), |c| {
        c.refs += 1;
        c.refs
    })
    .unwrap_or(0)
}
fn ac_release(c: &mut Ctx) -> u32 {
    release_client(c.a(0))
}
fn adj_qi(c: &mut Ctx) -> u32 {
    client_qi(c.a(0) - ps(), c.a(1), c.a(2))
}
fn adj_addref(c: &mut Ctx) -> u32 {
    client(c.a(0) - ps(), |c| {
        c.refs += 1;
        c.refs
    })
    .unwrap_or(0)
}
fn adj_release(c: &mut Ctx) -> u32 {
    release_client(c.a(0) - ps())
}
/// SetSampleRate(float): the rate the stream's frames are taken at (RATEADJUST streams)
fn adj_rate(c: &mut Ctx) -> u32 {
    let r = farg(c, 1);
    client(c.a(0) - ps(), |cl| {
        if cl.fmt.is_none() {
            NOT_INITIALIZED
        } else if cl.flags & RATEADJUST == 0 {
            E_INVALIDARG
        } else if !(r > 0.0 && r <= 384_000.0) {
            E_INVALIDARG
        } else {
            cl.rate = r;
            S_OK
        }
    })
    .unwrap_or(E_INVALIDARG)
}

/// Initialize(share mode, stream flags, buffer duration, periodicity, format, session guid)
fn ac_initialize(c: &mut Ctx) -> u32 {
    let (this, mode, flags) = (c.a(0), c.a(1), c.a(2));
    let duration = time_arg(c, 3, 3);
    let (fmt, guid) = if mem::x64() { (c.a(5), c.a(6)) } else { (c.a(7), c.a(8)) };
    if mode > 1 {
        return E_INVALIDARG;
    }
    if flags & !VALID_FLAGS != 0 {
        return INVALID_STREAM_FLAG;
    }
    if fmt == 0 {
        return E_POINTER;
    }
    let w = read_wave(fmt);
    let guid = guid_arg(guid);
    let r = with(|e| {
        let Some(cl) = e.clients.get(&this) else { return E_INVALIDARG };
        if cl.fmt.is_some() {
            return ALREADY_INITIALIZED;
        }
        if mode == 1 {
            return if flags & AUTOCONVERTPCM != 0 { E_INVALIDARG } else { EXCLUSIVE_MODE_NOT_ALLOWED };
        }
        // (no capture endpoint to loop back)
        if flags & LOOPBACK != 0 {
            return 0x8889_000a;
        }
        let f = match shared_initialize(&w, flags) {
            Ok(f) => f,
            Err(hr) => return hr,
        };
        let session = session_of(e, guid);
        let cl = e.clients.get_mut(&this).unwrap();
        cl.fmt = Some(f);
        cl.flags = flags;
        cl.frames = buffer_frames(f.rate, duration);
        cl.mem = mem::heap_alloc(cl.frames * f.block(), true);
        cl.session = session;
        S_OK
    });
    if r == S_OK {
        start_engine();
    }
    r
}

/// a getter of an initialized client's value into `*out`
fn get(c: &mut Ctx, f: impl FnOnce(&Client) -> u32) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    match client(this, |cl| cl.fmt.is_some().then(|| f(cl))) {
        Some(Some(v)) => {
            w32(out, v);
            S_OK
        }
        _ => NOT_INITIALIZED,
    }
}
fn ac_buffer_size(c: &mut Ctx) -> u32 {
    get(c, |cl| cl.frames)
}
fn ac_latency(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    if client(this, |cl| cl.fmt.is_some()) != Some(true) {
        return NOT_INITIALIZED;
    }
    // (the engine's lead: what kura's stream keeps queued)
    put64(out, AHEAD as u64 * 10_000_000 / RATE as u64);
    S_OK
}
fn ac_padding(c: &mut Ctx) -> u32 {
    get(c, |cl| cl.padding())
}
/// IsFormatSupported(share mode, format, closest match**)
fn ac_supported(c: &mut Ctx) -> u32 {
    let (mode, fmt, closest) = (c.a(1), c.a(2), c.a(3));
    if closest != 0 {
        putp(closest, 0);
    }
    if mode > 1 {
        return E_INVALIDARG;
    }
    if fmt == 0 || (mode == 0 && closest == 0) {
        return E_POINTER;
    }
    if mode == 1 {
        return EXCLUSIVE_MODE_NOT_ALLOWED;
    }
    let hr = shared_supported(&read_wave(fmt));
    if hr == S_FALSE {
        putp(closest, mix_format());
    }
    hr
}
fn ac_mix_format(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), mix_format());
    S_OK
}
/// GetDevicePeriod(default*, minimum*): either may be NULL
fn ac_period(c: &mut Ctx) -> u32 {
    let (d, m) = (c.a(1), c.a(2));
    if d == 0 && m == 0 {
        return E_POINTER;
    }
    if d != 0 {
        put64(d, PERIOD);
    }
    if m != 0 {
        put64(m, MIN_PERIOD);
    }
    S_OK
}
fn ac_start(c: &mut Ctx) -> u32 {
    let r = client(c.a(0), |cl| {
        if cl.fmt.is_none() {
            Err(NOT_INITIALIZED)
        } else if cl.running {
            Err(NOT_STOPPED)
        } else if cl.flags & EVENTCALLBACK != 0 && cl.event == 0 {
            Err(EVENTHANDLE_NOT_SET)
        } else {
            cl.running = true;
            cl.started = true;
            Ok(cl.event)
        }
    });
    match r {
        Some(Ok(ev)) => {
            // (the buffer has room now: an event-driven stream's first period)
            if ev != 0 {
                crate::k32::set_event(ev);
            }
            WAKE.notify_all();
            S_OK
        }
        Some(Err(hr)) => hr,
        None => E_INVALIDARG,
    }
}
fn ac_stop(c: &mut Ctx) -> u32 {
    client(c.a(0), |cl| {
        if cl.fmt.is_none() {
            NOT_INITIALIZED
        } else if !cl.running {
            S_FALSE
        } else {
            cl.running = false;
            S_OK
        }
    })
    .unwrap_or(E_INVALIDARG)
}
/// Reset: a stopped stream's queued frames and position go
fn ac_reset(c: &mut Ctx) -> u32 {
    client(c.a(0), |cl| {
        if cl.fmt.is_none() {
            NOT_INITIALIZED
        } else if cl.running {
            NOT_STOPPED
        } else if cl.taken.is_some() {
            BUFFER_OPERATION_PENDING
        } else {
            cl.queue.clear();
            cl.frac = 0.0;
            cl.played = 0;
            S_OK
        }
    })
    .unwrap_or(E_INVALIDARG)
}
fn ac_set_event(c: &mut Ctx) -> u32 {
    let (this, h) = (c.a(0), c.a(1));
    client(this, |cl| {
        if cl.fmt.is_none() {
            NOT_INITIALIZED
        } else if h == 0 {
            E_INVALIDARG
        } else if cl.flags & EVENTCALLBACK == 0 {
            EVENTHANDLE_NOT_EXPECTED
        } else if cl.event != 0 {
            E_UNEXPECTED
        } else {
            cl.event = h;
            S_OK
        }
    })
    .unwrap_or(E_INVALIDARG)
}
/// GetService(riid, void**)
fn ac_service(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if iid == 0 || out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let Some(Some((session, flags, ctl))) = client(this, |cl| cl.fmt.map(|_| (cl.session, cl.flags, cl.ctl))) else { return NOT_INITIALIZED };
    let is = |g: Iid| guid_is(iid, g);
    let o = if is(IID_RENDER) {
        service(render_vtbl(), this, session)
    } else if is(IID_CLOCK) {
        service(clock_vtbl(), this, session)
    } else if is(IID_STREAM_VOLUME) {
        service(stream_vtbl(), this, session)
    } else if is(IID_SIMPLE_VOLUME) {
        service(simple_vtbl(), this, session)
    } else if is(IID_CHANNEL_VOLUME) {
        service(channel_vtbl(), this, session)
    } else if is(IID_SESSION) {
        // (the same control every time)
        if ctl != 0 {
            addref_obj(ctl);
            ctl
        } else {
            let o = service(session_vtbl(), this, session);
            client(this, |cl| cl.ctl = o);
            o
        }
    } else if is(IID_CLOCK_ADJUST) {
        if flags & RATEADJUST == 0 {
            return E_INVALIDARG;
        }
        client(this, |c| c.refs += 1);
        this + ps()
    } else {
        return E_NOINTERFACE;
    };
    putp(out, o);
    S_OK
}

// ---------------------------------------------------------------------------------------------
// IAudioRenderClient {f294acfc-3146-4483-a7bf-addca7c260e2}

fn render_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| vtable("IAudioRenderClient", &[(rc_qi, b(3)), (svc_addref, b(1)), (svc_release, b(1)), (rc_get, b(3)), (rc_release, b(3))]))
}
fn rc_qi(c: &mut Ctx) -> u32 {
    svc_qi(c, &[IID_RENDER])
}
/// GetBuffer(frames, BYTE**): the client's buffer, room for `frames`, silent (none for 0
/// frames, which takes nothing)
fn rc_get(c: &mut Ctx) -> u32 {
    let (this, n, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = client(owner(this), |cl| {
        if cl.taken.is_some() {
            return Err(OUT_OF_ORDER);
        }
        if n > cl.frames - cl.padding().min(cl.frames) {
            return Err(BUFFER_TOO_LARGE);
        }
        if n == 0 {
            return Ok((0, 0, false));
        }
        cl.taken = Some(n);
        let f = cl.fmt.unwrap_or(MIX);
        Ok((cl.mem, n * f.block(), f.bits == 8 && !f.float))
    });
    match r {
        Some(Ok((p, bytes, u8))) => {
            if p != 0 {
                mem::fill(p, bytes, if u8 { 0x80 } else { 0 });
            }
            putp(out, p);
            S_OK
        }
        Some(Err(hr)) => {
            putp(out, 0);
            hr
        }
        None => NOT_INITIALIZED,
    }
}
/// ReleaseBuffer(frames, flags): the first `frames` of what GetBuffer handed out are queued
fn rc_release(c: &mut Ctx) -> u32 {
    let (this, n, flags) = (c.a(0), c.a(1), c.a(2));
    client(owner(this), |cl| {
        if n == 0 {
            cl.taken = None;
            return S_OK;
        }
        let Some(taken) = cl.taken else { return OUT_OF_ORDER };
        if n > taken {
            return INVALID_SIZE;
        }
        if flags & !SILENT != 0 {
            return E_INVALIDARG;
        }
        cl.taken = None;
        let f = cl.fmt.unwrap_or(MIX);
        for i in 0..n as usize {
            let (l, r) = if flags & SILENT != 0 { (0.0, 0.0) } else { sample_at(cl.mem, &f, i) };
            cl.queue.push_back([l, r]);
        }
        S_OK
    })
    .unwrap_or(NOT_INITIALIZED)
}

// ---------------------------------------------------------------------------------------------
// IAudioClock {cd63314f-3fba-4a1b-812c-ef96358728e7}: the position in bytes of the stream

fn clock_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| vtable("IAudioClock", &[(ck_qi, b(3)), (svc_addref, b(1)), (svc_release, b(1)), (ck_freq, b(2)), (ck_pos, b(3)), (ck_chars, b(2))]))
}
fn ck_qi(c: &mut Ctx) -> u32 {
    svc_qi(c, &[IID_CLOCK])
}
fn ck_freq(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let f = client(owner(this), |cl| cl.fmt.unwrap_or(MIX)).unwrap_or(MIX);
    put64(out, (f.rate * f.block()) as u64);
    S_OK
}
/// GetPosition(position*, qpc position*): the frames played, in bytes; the performance
/// counter then, in 100 ns units
fn ck_pos(c: &mut Ctx) -> u32 {
    let (this, pos, qpc) = (c.a(0), c.a(1), c.a(2));
    if pos == 0 {
        return E_POINTER;
    }
    let (played, block) = client(owner(this), |cl| (cl.played, cl.fmt.unwrap_or(MIX).block())).unwrap_or((0, MIX.block()));
    put64(pos, played * block as u64);
    if qpc != 0 {
        put64(qpc, crate::k32::qpc());
    }
    S_OK
}
fn ck_chars(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 0);
    S_OK
}

// ---------------------------------------------------------------------------------------------
// IAudioStreamVolume {93014887-242d-4068-8a15-cf5e93b90fe3}: the stream's volume per channel

fn stream_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IAudioStreamVolume",
            &[(st_qi, b(3)), (svc_addref, b(1)), (svc_release, b(1)), (st_count, b(2)), (st_set, b(3)), (st_get, b(3)), (st_set_all, b(3)), (st_get_all, b(3))],
        )
    })
}
fn st_qi(c: &mut Ctx) -> u32 {
    svc_qi(c, &[IID_STREAM_VOLUME])
}
fn channels_of(this: u32) -> u32 {
    client(owner(this), |cl| cl.fmt.map_or(2, |f| f.channels)).unwrap_or(2)
}
fn ok_volume(v: f32) -> bool {
    (0.0..=1.0).contains(&v)
}
fn st_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), channels_of(c.a(0)));
    S_OK
}
fn st_set(c: &mut Ctx) -> u32 {
    let (this, i, v) = (c.a(0), c.a(1), farg(c, 2));
    if i >= channels_of(this) || !ok_volume(v) {
        return E_INVALIDARG;
    }
    client(owner(this), |cl| cl.channel[i as usize] = v);
    S_OK
}
fn st_get(c: &mut Ctx) -> u32 {
    let (this, i, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if i >= channels_of(this) {
        return E_INVALIDARG;
    }
    put_f32(out, client(owner(this), |cl| cl.channel[i as usize]).unwrap_or(1.0));
    S_OK
}
fn st_set_all(c: &mut Ctx) -> u32 {
    let (this, n, p) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return E_POINTER;
    }
    if n != channels_of(this) {
        return E_INVALIDARG;
    }
    let v: Vec<f32> = (0..n).map(|i| f32::from_bits(r32(p + 4 * i))).collect();
    if !v.iter().all(|&x| ok_volume(x)) {
        return E_INVALIDARG;
    }
    client(owner(this), |cl| cl.channel[..v.len()].copy_from_slice(&v));
    S_OK
}
fn st_get_all(c: &mut Ctx) -> u32 {
    let (this, n, p) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return E_POINTER;
    }
    if n != channels_of(this) {
        return E_INVALIDARG;
    }
    let v = client(owner(this), |cl| cl.channel).unwrap_or([1.0; 8]);
    for i in 0..n {
        put_f32(p + 4 * i, v[i as usize]);
    }
    S_OK
}

// ---------------------------------------------------------------------------------------------
// the session's interfaces: ISimpleAudioVolume {87ce5498-68d6-44e5-9215-6da47ef883d8},
// IChannelAudioVolume {1c158861-b533-4b30-b1cf-e853e51c59b8} (the mix format's channels),
// IAudioSessionControl2 {bfb7ff88-7239-4fc9-8fa2-07c950be9c6d}. Their NULL pointers give
// RPC_X_NULL_REF_POINTER, as Windows' (marshalled) session objects do.

fn sess<R>(this: u32, f: impl FnOnce(&mut Session) -> R) -> R {
    with(|e| {
        let i = session_idx(this).min(e.sessions.len().saturating_sub(1));
        if e.sessions.is_empty() {
            session_of(e, [0; 4]);
        }
        f(&mut e.sessions[i])
    })
}

fn simple_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "ISimpleAudioVolume",
            &[(sv_qi, b(3)), (svc_addref, b(1)), (svc_release, b(1)), (sv_set_volume, b(3)), (sv_get_volume, b(2)), (sv_set_mute, b(3)), (sv_get_mute, b(2))],
        )
    })
}
fn sv_qi(c: &mut Ctx) -> u32 {
    svc_qi(c, &[IID_SIMPLE_VOLUME])
}
fn sv_set_volume(c: &mut Ctx) -> u32 {
    let v = farg(c, 1);
    if !ok_volume(v) {
        return E_INVALIDARG;
    }
    sess(c.a(0), |s| s.volume = v);
    S_OK
}
fn sv_get_volume(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    put_f32(c.a(1), sess(c.a(0), |s| s.volume));
    S_OK
}
fn sv_set_mute(c: &mut Ctx) -> u32 {
    let m = c.a(1) != 0;
    sess(c.a(0), |s| s.mute = m);
    S_OK
}
fn sv_get_mute(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    w32(c.a(1), sess(c.a(0), |s| s.mute) as u32);
    S_OK
}

fn channel_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IChannelAudioVolume",
            &[(cv_qi, b(3)), (svc_addref, b(1)), (svc_release, b(1)), (cv_count, b(2)), (cv_set, b(4)), (cv_get, b(3)), (cv_set_all, b(4)), (cv_get_all, b(3))],
        )
    })
}
fn cv_qi(c: &mut Ctx) -> u32 {
    svc_qi(c, &[IID_CHANNEL_VOLUME])
}
fn cv_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    w32(c.a(1), MIX.channels);
    S_OK
}
/// SetChannelVolume(index, level, event context)
fn cv_set(c: &mut Ctx) -> u32 {
    let (this, i, v) = (c.a(0), c.a(1), farg(c, 2));
    if i >= MIX.channels || !ok_volume(v) {
        return E_INVALIDARG;
    }
    sess(this, |s| s.channel[i as usize] = v);
    S_OK
}
fn cv_get(c: &mut Ctx) -> u32 {
    let (this, i, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return NULL_REF;
    }
    if i >= MIX.channels {
        return E_INVALIDARG;
    }
    put_f32(out, sess(this, |s| s.channel[i as usize]));
    S_OK
}
/// SetAllVolumes(count, levels, event context)
fn cv_set_all(c: &mut Ctx) -> u32 {
    let (this, n, p) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return NULL_REF;
    }
    if n != MIX.channels {
        return E_INVALIDARG;
    }
    let v: Vec<f32> = (0..n).map(|i| f32::from_bits(r32(p + 4 * i))).collect();
    if !v.iter().all(|&x| ok_volume(x)) {
        return E_INVALIDARG;
    }
    sess(this, |s| s.channel[..v.len()].copy_from_slice(&v));
    S_OK
}
fn cv_get_all(c: &mut Ctx) -> u32 {
    let (this, n, p) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return NULL_REF;
    }
    if n != MIX.channels {
        return E_INVALIDARG;
    }
    let v = sess(this, |s| s.channel);
    for i in 0..n {
        put_f32(p + 4 * i, v[i as usize]);
    }
    S_OK
}

fn session_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IAudioSessionControl2",
            &[
                (se_qi, b(3)),
                (svc_addref, b(1)),
                (svc_release, b(1)),
                (se_state, b(2)),
                (se_get_name, b(2)),
                (se_set_name, b(3)),
                (se_get_icon, b(2)),
                (se_set_icon, b(3)),
                (se_get_group, b(2)),
                (se_set_group, b(3)),
                (se_notify, b(2)),
                (se_notify, b(2)),
                (se_identifier, b(2)),
                (se_instance, b(2)),
                (se_pid, b(2)),
                (se_system, b(1)),
                (se_ducking, b(2)),
            ],
        )
    })
}
/// IAudioSessionControl(2) itself; the session's volume interfaces as objects of their own
fn se_qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let v = if guid_is(iid, IID_SIMPLE_VOLUME) {
        simple_vtbl()
    } else if guid_is(iid, IID_CHANNEL_VOLUME) {
        channel_vtbl()
    } else {
        return svc_qi(c, &[IID_SESSION, IID_SESSION2]);
    };
    putp(out, service(v, owner(this), session_idx(this)));
    S_OK
}
/// GetState(AudioSessionState*): active while one of its streams runs
fn se_state(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return NULL_REF;
    }
    let i = session_idx(this);
    let on = with(|e| e.clients.values().any(|cl| cl.session == i && cl.running));
    w32(out, on as u32);
    S_OK
}
fn get_str(out: u32, f: impl FnOnce(&Session) -> Vec<u16>, this: u32) -> u32 {
    if out == 0 {
        return E_POINTER;
    }
    let s = sess(this, |s| f(s));
    putp(out, task_wstr(&s));
    S_OK
}
fn se_get_name(c: &mut Ctx) -> u32 {
    get_str(c.a(1), |s| s.name.clone(), c.a(0))
}
fn se_get_icon(c: &mut Ctx) -> u32 {
    get_str(c.a(1), |s| s.icon.clone(), c.a(0))
}
/// SetDisplayName / SetIconPath(string, event context)
fn se_set_name(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    let v = mem::wstr(c.a(1));
    sess(c.a(0), |s| s.name = v);
    S_OK
}
fn se_set_icon(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    let v = mem::wstr(c.a(1));
    sess(c.a(0), |s| s.icon = v);
    S_OK
}
fn se_get_group(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return NULL_REF;
    }
    let g = sess(c.a(0), |s| s.group);
    for (i, w) in g.iter().enumerate() {
        w32(out + 4 * i as u32, *w);
    }
    S_OK
}
fn se_set_group(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    let g = guid_arg(c.a(1));
    sess(c.a(0), |s| s.group = g);
    S_OK
}
/// Register / UnregisterAudioSessionNotification(IAudioSessionEvents*): none are sent
fn se_notify(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 { NULL_REF } else { S_OK }
}
/// "<device id>|<exe>%b<session guid>", as Windows forms them
fn identifier(this: u32) -> Vec<u16> {
    let g = sess(this, |s| s.guid);
    let exe = crate::k32::exe_path(true);
    let guid = format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g[0],
        g[1] & 0xffff,
        g[1] >> 16,
        g[2] as u8,
        (g[2] >> 8) as u8,
        (g[2] >> 16) as u8,
        (g[2] >> 24) as u8,
        g[3] as u8,
        (g[3] >> 8) as u8,
        (g[3] >> 16) as u8,
        (g[3] >> 24) as u8
    );
    format!("{}|{exe}%b{guid}", crate::mmdevapi::DEVICE_ID).encode_utf16().collect()
}
fn se_identifier(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    putp(c.a(1), task_wstr(&identifier(c.a(0))));
    S_OK
}
fn se_instance(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    let mut s = identifier(c.a(0));
    s.extend(format!("|1%b{}", crate::pid()).encode_utf16());
    putp(c.a(1), task_wstr(&s));
    S_OK
}
fn se_pid(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    w32(c.a(1), crate::pid());
    S_OK
}
/// IsSystemSoundsSession: not it (S_FALSE)
fn se_system(_: &mut Ctx) -> u32 {
    S_FALSE
}
fn se_ducking(_: &mut Ctx) -> u32 {
    S_OK
}

// ---------------------------------------------------------------------------------------------
// IAudioSessionManager2 {77aa99a0-1bd6-484f-8bc7-2c654c9a9b6f} (and IAudioSessionManager
// {bfa971f1-4d5e-40bb-935e-967039bfbee4}): one per process; its session enumerator
// {e2f5bb11-0570-40ca-acdd-3aa01277dee8} holds the sessions there were when it was made

pub(crate) fn manager() -> u32 {
    static O: OnceLock<u32> = OnceLock::new();
    *O.get_or_init(|| {
        let v = vtable(
            "IAudioSessionManager2",
            &[
                (sm_qi, b(3)),
                (sm_ref, b(1)),
                (sm_ref, b(1)),
                (sm_control, b(4)),
                (sm_volume, b(4)),
                (sm_enum, b(2)),
                (sm_notify, b(2)),
                (sm_notify, b(2)),
                (sm_duck, b(3)),
                (sm_notify, b(2)),
            ],
        );
        let o = mem::heap_alloc(2 * ps(), true);
        wp(o, v);
        o
    })
}
fn sm_qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is_unknown(iid) || guid_is(iid, IID_MANAGER) || guid_is(iid, IID_MANAGER2) {
        putp(out, this);
        S_OK
    } else {
        putp(out, 0);
        E_NOINTERFACE
    }
}
fn sm_ref(_: &mut Ctx) -> u32 {
    1
}
/// GetAudioSessionControl(session guid, flags, IAudioSessionControl**)
fn sm_control(c: &mut Ctx) -> u32 {
    let (g, out) = (c.a(1), c.a(3));
    if out == 0 {
        return NULL_REF;
    }
    let i = with(|e| session_of(e, guid_arg(g)));
    putp(out, service(session_vtbl(), 0, i));
    S_OK
}
/// GetSimpleAudioVolume(session guid, flags, ISimpleAudioVolume**)
fn sm_volume(c: &mut Ctx) -> u32 {
    let (g, out) = (c.a(1), c.a(3));
    if out == 0 {
        return NULL_REF;
    }
    let i = with(|e| session_of(e, guid_arg(g)));
    putp(out, service(simple_vtbl(), 0, i));
    S_OK
}
/// GetSessionEnumerator(IAudioSessionEnumerator**): a snapshot of the sessions
fn sm_enum(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return NULL_REF;
    }
    static V: OnceLock<u32> = OnceLock::new();
    let v = *V.get_or_init(|| vtable("IAudioSessionEnumerator", &[(en_qi, b(3)), (svc_addref, b(1)), (svc_release, b(1)), (en_count, b(2)), (en_get, b(3))]));
    let n = with(|e| e.sessions.len()) as u32;
    // (the count where the session index goes)
    let o = service(v, 0, n as usize);
    putp(out, o);
    S_OK
}
fn sm_notify(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 { NULL_REF } else { S_OK }
}
/// RegisterDuckNotification(session id, IAudioVolumeDuckNotification*)
fn sm_duck(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 { NULL_REF } else { S_OK }
}
fn en_qi(c: &mut Ctx) -> u32 {
    svc_qi(c, &[IID_ENUMERATOR])
}
fn en_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return NULL_REF;
    }
    w32(c.a(1), session_idx(c.a(0)) as u32);
    S_OK
}
/// GetSession(index, IAudioSessionControl**)
fn en_get(c: &mut Ctx) -> u32 {
    let (this, i, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return NULL_REF;
    }
    if i as usize >= session_idx(this) {
        return E_INVALIDARG;
    }
    putp(out, service(session_vtbl(), 0, i as usize));
    S_OK
}

// ---------------------------------------------------------------------------------------------
// IAudioEndpointVolume {5cdf2c82-841e-4546-9722-0cf74078229a}: the device's master volume (dB)
// and mute, which scale everything the engine plays

pub(crate) fn endpoint_volume() -> u32 {
    static O: OnceLock<u32> = OnceLock::new();
    *O.get_or_init(|| {
        let v = vtable(
            "IAudioEndpointVolume",
            &[
                (ev_qi, b(3)),
                (sm_ref, b(1)),
                (sm_ref, b(1)),
                (sm_notify, b(2)),
                (sm_notify, b(2)),
                (ev_count, b(2)),
                (ev_set_db, b(3)),
                (ev_set_scalar, b(3)),
                (ev_get_db, b(2)),
                (ev_get_scalar, b(2)),
                (ev_set_ch_db, b(4)),
                (ev_set_ch_scalar, b(4)),
                (ev_get_ch_db, b(3)),
                (ev_get_ch_scalar, b(3)),
                (ev_set_mute, b(3)),
                (ev_get_mute, b(2)),
                (ev_step_info, b(3)),
                (ev_step_up, b(2)),
                (ev_step_down, b(2)),
                (ev_hardware, b(2)),
                (ev_range, b(4)),
            ],
        );
        let o = mem::heap_alloc(2 * ps(), true);
        wp(o, v);
        o
    })
}
fn ev_qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is_unknown(iid) || guid_is(iid, IID_ENDPOINT_VOLUME) {
        putp(out, this);
        S_OK
    } else {
        putp(out, 0);
        E_NOINTERFACE
    }
}
/// level (0..1) <-> dB: the scalar is linear in dB over the range
pub(crate) fn db_of(scalar: f32) -> f32 {
    MIN_DB + (MAX_DB - MIN_DB) * scalar
}
pub(crate) fn scalar_of(db: f32) -> f32 {
    (db - MIN_DB) / (MAX_DB - MIN_DB)
}
fn ok_db(db: f32) -> bool {
    (MIN_DB..=MAX_DB).contains(&db)
}
fn ev_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), MIX.channels);
    S_OK
}
fn set_db(db: f32) -> u32 {
    if !ok_db(db) {
        return E_INVALIDARG;
    }
    with(|e| e.endpoint.db = db);
    S_OK
}
fn ev_set_db(c: &mut Ctx) -> u32 {
    set_db(farg(c, 1))
}
fn ev_set_scalar(c: &mut Ctx) -> u32 {
    let s = farg(c, 1);
    if !ok_volume(s) {
        return E_INVALIDARG;
    }
    set_db(db_of(s))
}
fn ev_get_db(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put_f32(c.a(1), with(|e| e.endpoint.db));
    S_OK
}
fn ev_get_scalar(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put_f32(c.a(1), scalar_of(with(|e| e.endpoint.db)));
    S_OK
}
fn set_ch_db(i: u32, db: f32) -> u32 {
    if i >= MIX.channels || !ok_db(db) {
        return E_INVALIDARG;
    }
    with(|e| e.endpoint.channel[i as usize] = db);
    S_OK
}
fn ev_set_ch_db(c: &mut Ctx) -> u32 {
    set_ch_db(c.a(1), farg(c, 2))
}
fn ev_set_ch_scalar(c: &mut Ctx) -> u32 {
    let s = farg(c, 2);
    if !ok_volume(s) {
        return E_INVALIDARG;
    }
    set_ch_db(c.a(1), db_of(s))
}
fn get_ch(c: &mut Ctx, f: fn(f32) -> f32) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if i >= MIX.channels {
        return E_INVALIDARG;
    }
    put_f32(out, f(with(|e| e.endpoint.channel[i as usize])));
    S_OK
}
fn ev_get_ch_db(c: &mut Ctx) -> u32 {
    get_ch(c, |d| d)
}
fn ev_get_ch_scalar(c: &mut Ctx) -> u32 {
    get_ch(c, scalar_of)
}
/// SetMute(mute, event context): S_FALSE when it was so already
fn ev_set_mute(c: &mut Ctx) -> u32 {
    let m = c.a(1) != 0;
    if with(|e| std::mem::replace(&mut e.endpoint.mute, m)) == m { S_FALSE } else { S_OK }
}
fn ev_get_mute(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), with(|e| e.endpoint.mute) as u32);
    S_OK
}
fn steps() -> u32 {
    ((MAX_DB - MIN_DB) / STEP_DB) as u32 + 1
}
/// GetVolumeStepInfo(step*, count*)
fn ev_step_info(c: &mut Ctx) -> u32 {
    let (step, count) = (c.a(1), c.a(2));
    if step == 0 && count == 0 {
        return E_POINTER;
    }
    let db = with(|e| e.endpoint.db);
    if step != 0 {
        w32(step, ((db - MIN_DB) / STEP_DB).round() as u32);
    }
    if count != 0 {
        w32(count, steps());
    }
    S_OK
}
fn step_by(k: f32) -> u32 {
    with(|e| e.endpoint.db = (e.endpoint.db + k * STEP_DB).clamp(MIN_DB, MAX_DB));
    S_OK
}
fn ev_step_up(_: &mut Ctx) -> u32 {
    step_by(1.0)
}
fn ev_step_down(_: &mut Ctx) -> u32 {
    step_by(-1.0)
}
/// QueryHardwareSupport(mask*): none in hardware
fn ev_hardware(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 0);
    S_OK
}
/// GetVolumeRange(min dB*, max dB*, increment dB*)
fn ev_range(c: &mut Ctx) -> u32 {
    let (lo, hi, inc) = (c.a(1), c.a(2), c.a(3));
    if lo == 0 || hi == 0 || inc == 0 {
        return E_POINTER;
    }
    put_f32(lo, MIN_DB);
    put_f32(hi, MAX_DB);
    put_f32(inc, STEP_DB);
    S_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(tag: u16, channels: u16, rate: u32, bits: u16, valid: u16, sub: Option<u32>) -> Wave {
        let block = (channels as u32 * bits as u32 / 8) as u16;
        Wave { tag, channels, rate, avg: block as u32 * rate, block, bits, cb: if tag == 0xfffe { 22 } else { 0 }, valid, sub }
    }
    fn ext(channels: u16, rate: u32, bits: u16, valid: u16, sub: u32) -> Wave {
        wave(0xfffe, channels, rate, bits, valid, Some(sub))
    }

    /// Shared mode's answers for the formats Wine's render test tries (its expectations are
    /// Windows' answers): every sample format, channel count and rate around the mix format.
    #[test]
    fn shared_mode_format_rules() {
        let r = RATE;
        // the mix format, extensible and not; another bit width at the mix rate and channels
        assert_eq!(shared_supported(&ext(2, r, 32, 32, 3)), S_OK);
        assert_eq!(shared_supported(&wave(3, 2, r, 32, 0, None)), S_OK);
        assert_eq!(shared_supported(&ext(2, r, 32, 32, 1)), S_OK);
        assert_eq!(shared_supported(&wave(1, 2, r, 16, 0, None)), S_OK);
        assert_eq!(shared_supported(&ext(2, r, 32, 24, 1)), S_OK);
        assert_eq!(shared_supported(&ext(2, r, 24, 24, 1)), S_OK);
        assert_eq!(shared_supported(&ext(2, r, 8, 8, 1)), S_OK);
        // valid formats of another rate or channel count: S_FALSE (a closest match)
        for w in [ext(2, 48000, 32, 32, 3), ext(1, r, 32, 32, 3), ext(6, r, 16, 16, 1), wave(1, 1, 22050, 16, 0, None), ext(8, 96000, 32, 32, 3)] {
            assert_eq!(shared_supported(&w), S_FALSE, "{w:?}");
        }
        // malformed: E_INVALIDARG
        let mut bad = ext(2, r, 32, 32, 3);
        bad.avg += 1;
        assert_eq!(shared_supported(&bad), E_INVALIDARG);
        let mut bad = ext(2, r, 32, 32, 3);
        bad.block -= 1;
        assert_eq!(shared_supported(&bad), E_INVALIDARG);
        let mut bad = ext(2, r, 32, 32, 3);
        bad.cb = 21;
        assert_eq!(shared_supported(&bad), E_INVALIDARG);
        // (another SubFormat: malformed fields first; A-law with no valid bits)
        assert_eq!(shared_supported(&ext(2, r, 8, 0, 6)), E_INVALIDARG);
        assert_eq!(shared_supported(&ext(2, r, 16, 17, 7)), E_INVALIDARG);
        for w in [ext(0, r, 32, 32, 3), ext(2, 0, 32, 32, 3), ext(2, r, 32, 0, 3), ext(2, r, 32, 33, 3), ext(2, r, 15, 15, 1), ext(2, r, 16, 16, 3), ext(2, r, 96, 96, 3), wave(1, 4, r, 16, 0, None)] {
            assert_eq!(shared_supported(&w), E_INVALIDARG, "{w:?}");
        }
        // well-formed but not played: AUDCLNT_E_UNSUPPORTED_FORMAT
        for w in [ext(2, r, 16, 15, 1), ext(2, r, 32, 16, 1), ext(2, r, 32, 31, 3), ext(2, r, 64, 64, 3), ext(2, r, 8, 8, 6), wave(6, 2, r, 8, 0, None), wave(2, 2, r, 16, 0, None)] {
            assert_eq!(shared_supported(&w), UNSUPPORTED_FORMAT, "{w:?}");
        }
    }

    #[test]
    fn initialize_flags_widen_what_shared_mode_takes() {
        let other_rate = ext(2, 48000, 16, 16, 1);
        let mono = ext(1, RATE, 32, 32, 3);
        assert_eq!(shared_initialize(&other_rate, 0).err(), Some(UNSUPPORTED_FORMAT));
        assert!(shared_initialize(&other_rate, RATEADJUST).is_ok());
        assert!(shared_initialize(&other_rate, AUTOCONVERTPCM).is_ok());
        assert_eq!(shared_initialize(&mono, RATEADJUST).err(), Some(UNSUPPORTED_FORMAT));
        assert!(shared_initialize(&mono, AUTOCONVERTPCM).is_ok());
        // (not a valid format: the same error with any flags)
        assert_eq!(shared_initialize(&ext(2, RATE, 32, 31, 3), AUTOCONVERTPCM).err(), Some(UNSUPPORTED_FORMAT));
        assert_eq!(shared_initialize(&ext(2, 0, 32, 32, 3), AUTOCONVERTPCM).err(), Some(E_INVALIDARG));
        let f = shared_initialize(&ext(6, 22050, 24, 24, 1), AUTOCONVERTPCM).unwrap();
        assert_eq!((f.rate, f.channels, f.bits, f.float, f.block()), (22050, 6, 24, false, 18));
    }

    #[test]
    fn buffer_holds_the_duration_rounded_and_at_least_two_periods() {
        // Wine's render test: a 0.5 s buffer is exactly MulDiv(duration, rate, 10^7) frames
        assert_eq!(buffer_frames(44100, 5_000_000), 22050);
        assert_eq!(buffer_frames(48000, 5_000_000), 24000);
        for rate in [8000u32, 22050, 44100, 48000, 96000] {
            let muldiv = |d: u64| ((d * rate as u64 + 5_000_000) / 10_000_000) as u32;
            assert_eq!(buffer_frames(rate, 0), muldiv(2 * PERIOD));
            for d in [1u64, 100_000, 200_000, 200_001, 333_333, 10_000_000, 20_000_000] {
                assert_eq!(buffer_frames(rate, d), muldiv(d).max(muldiv(2 * PERIOD)), "{rate} {d}");
            }
        }
    }

    fn test_client(rate: u32) -> Client {
        Client {
            refs: 1,
            fmt: Some(Fmt { rate, channels: 2, bits: 32, float: true }),
            flags: 0,
            frames: 0,
            event: 0,
            running: true,
            started: true,
            queue: VecDeque::new(),
            frac: 0.0,
            rate: 0.0,
            played: 0,
            taken: None,
            mem: 0,
            channel: [1.0; 8],
            session: 0,
            ctl: 0,
        }
    }

    /// A period consumes the stream's frames at its rate against the engine's, and the frames
    /// played add up over periods (no drift from the fractional position).
    #[test]
    fn render_consumes_at_the_streams_rate() {
        for rate in [22050, 44100, 48000, 32000, 96000] {
            let mut c = test_client(rate);
            let total = rate as usize * 2;
            c.queue.extend((0..total).map(|i| [i as f32, -(i as f32)]));
            let mut out = vec![0f32; STEP * 2];
            for _ in 0..100 {
                out.iter_mut().for_each(|x| *x = 0.0);
                assert!(!render(&mut c, [1.0; 2], &mut out));
            }
            // 100 periods = 1 s of output = `rate` frames of the stream's (within one frame)
            assert!((c.played as i64 - rate as i64).abs() <= 1, "{rate}: {}", c.played);
            assert_eq!(c.queue.len() as u64 + c.played, total as u64);
        }
        // SetSampleRate's rate instead of the format's
        let mut c = test_client(44100);
        c.rate = 48000.0;
        c.queue.extend((0..96000).map(|_| [0.0, 0.0]));
        let mut out = vec![0f32; STEP * 2];
        for _ in 0..100 {
            render(&mut c, [1.0; 2], &mut out);
        }
        assert!((c.played as i64 - 48000).abs() <= 1, "{}", c.played);
    }

    #[test]
    fn render_at_the_engine_rate_is_exact_and_runs_dry() {
        let mut c = test_client(RATE);
        c.queue.extend((0..STEP + 10).map(|i| [i as f32 / 1000.0, 0.5]));
        let mut out = vec![0f32; STEP * 2];
        assert!(!render(&mut c, [1.0; 2], &mut out));
        for i in 0..STEP {
            assert_eq!(out[2 * i], i as f32 / 1000.0);
            assert_eq!(out[2 * i + 1], 0.5);
        }
        assert_eq!(c.played, STEP as u64);
        out.iter_mut().for_each(|x| *x = 0.0);
        assert!(render(&mut c, [1.0; 2], &mut out));
        assert_eq!(c.played, STEP as u64 + 10);
        assert!(out[2 * 10..].iter().all(|&x| x == 0.0));
    }

    /// Stream, session and endpoint volumes multiply; mute silences.
    #[test]
    fn gains_multiply_stream_session_and_endpoint_volumes() {
        let mut e = Engine { clients: HashMap::default(), sessions: Vec::new(), endpoint: Endpoint { db: MAX_DB, mute: false, channel: [MAX_DB; 2] }, stream: 0 };
        session_of(&mut e, [0; 4]);
        let mut c = test_client(RATE);
        c.channel[1] = 0.25;
        e.sessions[0].volume = 0.5;
        e.sessions[0].channel[0] = 0.5;
        assert_eq!(gain_of(&e, &c), [0.25, 0.125]);
        e.endpoint.db = -6.0;
        let g = gain_of(&e, &c);
        assert!((g[0] - 0.25 * 10f32.powf(-0.3)).abs() < 1e-6);
        e.endpoint.db = MIN_DB;
        assert_eq!(gain_of(&e, &c), [0.0, 0.0]);
        e.endpoint.db = MAX_DB;
        e.sessions[0].mute = true;
        assert_eq!(gain_of(&e, &c), [0.0, 0.0]);
        e.sessions[0].mute = false;
        e.endpoint.mute = true;
        assert_eq!(gain_of(&e, &c), [0.0, 0.0]);
        // a mono stream: its one channel's volumes on both sides
        e.endpoint.mute = false;
        c.fmt = Some(Fmt { rate: RATE, channels: 1, bits: 16, float: false });
        assert_eq!(gain_of(&e, &c), [0.25, 0.25]);
    }

    #[test]
    fn sessions_by_guid_and_null_is_the_default() {
        let mut e = Engine { clients: HashMap::default(), sessions: Vec::new(), endpoint: Endpoint { db: MAX_DB, mute: false, channel: [MAX_DB; 2] }, stream: 0 };
        let d = session_of(&mut e, [0; 4]);
        let a = session_of(&mut e, [1, 2, 3, 4]);
        assert_ne!(d, a);
        assert_eq!(session_of(&mut e, [0; 4]), d);
        assert_eq!(session_of(&mut e, [1, 2, 3, 4]), a);
        assert!(e.sessions.iter().all(|s| s.group != [0; 4]));
        assert_ne!(e.sessions[0].group, e.sessions[1].group);
    }

    #[test]
    fn endpoint_scalar_and_db_round_trip() {
        assert_eq!(db_of(0.0), MIN_DB);
        assert_eq!(db_of(1.0), MAX_DB);
        for i in 0..=100 {
            let s = i as f32 / 100.0;
            assert!((scalar_of(db_of(s)) - s).abs() < 1e-6);
        }
        assert_eq!(steps(), 2089);
    }
}
