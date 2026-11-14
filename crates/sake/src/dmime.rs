//! DirectMusic's performance, loader and segments (dmime.dll, dmloader.dll) for games that play
//! their music and voices as wave files through DirectMusic: CLSID_DirectMusicLoader opens a
//! .wav as a segment, CLSID_DirectMusicPerformance(8) plays it (InitAudio with a standard audio
//! path, PlaySegmentEx, StopEx, IsPlaying, SetVolume on the audio path, GUID_PerfMasterVolume).
//! 凌●学園 体験版 (gakuentr.exe) quits with "SOUND init failed" when the performance does not
//! exist.
//!
//! A wave segment is the file in guest memory; each play is a segment state the DirectSound
//! mixer (sound.rs) plays under the state object's address, as many times as the segment's
//! repeats say. A primary segment replaces the primary one playing (DMUS_SEGF_SECONDARY plays
//! alongside). There is no synthesizer: a MIDI or style segment (.mid, .sgt) loads as a silent
//! segment, so a game that also has MIDI music runs without it.

use crate::api::{Ctx, Handler};
use crate::mem;
use crate::sound::Fmt;
use std::sync::Mutex;

const S_FALSE: u32 = 1;
const E_POINTER: u32 = 0x8000_4003;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const CLASS_E_NOAGGREGATION: u32 = 0x8004_0110;
/// dmerror.h (MAKE_DMHRESULTERROR(n): 0x88780000 | 0x1000 + n)
const DMUS_E_LOADER_FAILEDOPEN: u32 = 0x8878_1182;
const DMUS_E_LOADER_NOFILENAME: u32 = 0x8878_1186;
const DMUS_E_NOT_FOUND: u32 = 0x8878_1161;
const DMUS_E_TYPE_UNSUPPORTED: u32 = 0x8878_1164;

/// DMUS_SEGF_SECONDARY
const SEGF_SECONDARY: u32 = 0x80;
/// DMUS_SEG_REPEAT_INFINITE
pub const REPEAT_INFINITE: u32 = 0xffff_ffff;
/// MUSIC_TIME ticks a second at the default tempo (DMUS_PPQ 768 a quarter note, 120 a minute)
pub const TICKS_PER_SECOND: u64 = 1536;

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let (b, c) = (b.to_le_bytes(), c.to_le_bytes());
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}
const DM: [u8; 8] = [0x87, 0x04, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd];
pub const CLSID_PERFORMANCE: [u8; 16] = guid(0xd2ac_2881, 0xb39b, 0x11d1, DM);
pub const CLSID_SEGMENT: [u8; 16] = guid(0xd2ac_2882, 0xb39b, 0x11d1, DM);
pub const CLSID_SEGMENT_STATE: [u8; 16] = guid(0xd2ac_2883, 0xb39b, 0x11d1, DM);
pub const CLSID_LOADER: [u8; 16] = guid(0xd2ac_2892, 0xb39b, 0x11d1, DM);
const GUID_ALL_TYPES: [u8; 16] = guid(0xd2ac_2893, 0xb39b, 0x11d1, DM);
const GUID_PERF_MASTER_VOLUME: [u8; 16] = guid(0xd2ac_28b1, 0xb39b, 0x11d1, DM);
const GUID_PERF_MASTER_TEMPO: [u8; 16] = guid(0xd2ac_28b0, 0xb39b, 0x11d1, DM);
const IID_UNKNOWN: [u8; 16] = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const PERF_IIDS: [[u8; 16]; 3] = [
    guid(0x07d4_3d03, 0x6523, 0x11d2, [0x87, 0x1d, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd]),
    guid(0x6fc2_cae0, 0xbc78, 0x11d2, [0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6]),
    guid(0x679c_4137, 0xc62e, 0x4147, [0xb2, 0xb4, 0x9d, 0x56, 0x9a, 0xcb, 0x25, 0x4c]),
];
const LOADER_IIDS: [[u8; 16]; 2] = [
    guid(0x2ffa_aca2, 0x5dca, 0x11d2, [0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6]),
    guid(0x19e7_c08c, 0x0a44, 0x4e6a, [0xa1, 0x16, 0x59, 0x5a, 0x7c, 0xd5, 0xde, 0x8c]),
];
const SEGMENT_IIDS: [[u8; 16]; 3] = [
    guid(0xf960_29a2, 0x4282, 0x11d2, [0x87, 0x17, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd]),
    guid(0xd388_94d1, 0xc052, 0x11d2, [0x87, 0x2f, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd]),
    guid(0xc678_4488, 0x41a3, 0x418f, [0xaa, 0x15, 0xb3, 0x50, 0x93, 0xba, 0x42, 0xd4]),
];
const STATE_IIDS: [[u8; 16]; 2] = [
    guid(0xa3af_dcc7, 0xd3ee, 0x11d1, [0xbc, 0x8d, 0x00, 0xa0, 0xc9, 0x22, 0xe6, 0xeb]),
    guid(0xa50e_4730, 0x0ae4, 0x48a7, [0x98, 0x39, 0xbc, 0x04, 0xbf, 0xe0, 0x77, 0x72]),
];
const AUDIOPATH_IIDS: [[u8; 16]; 1] = [guid(0xc876_31f5, 0x23be, 0x4986, [0x88, 0x36, 0x05, 0x83, 0x2f, 0xcc, 0x48, 0xf9])];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Performance,
    Loader,
    Segment,
    State,
    AudioPath,
}

/// Whether an object of `kind` answers QueryInterface for `iid`.
pub fn answers(kind: Kind, iid: &[u8; 16]) -> bool {
    *iid == IID_UNKNOWN
        || match kind {
            Kind::Performance => PERF_IIDS.contains(iid),
            Kind::Loader => LOADER_IIDS.contains(iid),
            Kind::Segment => SEGMENT_IIDS.contains(iid),
            Kind::State => STATE_IIDS.contains(iid),
            Kind::AudioPath => AUDIOPATH_IIDS.contains(iid),
        }
}

// ---------------------------------------------------------------------------------------------
// what plays: a model without guest memory (tests check it alone)

/// A wave segment's sound: where its samples are and their format.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sound {
    pub data: u32,
    pub size: u32,
    pub rate: u32,
    pub channels: u32,
    pub bits: u32,
    pub float: bool,
}

impl Sound {
    pub fn frames(&self) -> u64 {
        let block = (self.channels * self.bits / 8).max(1);
        (self.size / block) as u64
    }
    /// The segment's length in MUSIC_TIME (at the default tempo)
    pub fn length(&self) -> u32 {
        if self.rate == 0 {
            return 0;
        }
        (self.frames() * TICKS_PER_SECOND / self.rate as u64).min(i32::MAX as u64) as u32
    }
}

/// The PCM samples of a WAVE file's bytes: (offset, bytes, rate, channels, bits, float).
/// None for anything else (compressed waves, MIDI, styles).
pub fn wave_pcm(b: &[u8]) -> Option<(u32, u32, u32, u32, u32, bool)> {
    let t = crate::audiofile::open(b)?;
    let crate::audiofile::Codec::Pcm { bits, float } = t.codec else { return None };
    let first = t.packets.first()?;
    let last = t.packets.last()?;
    let end = last.0 + last.1 as u64;
    Some((first.0 as u32, (end - first.0) as u32, t.rate, t.channels, bits as u32, float))
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateInfo {
    pub segment: u32,
    pub primary: bool,
    pub path: u32,
    /// MUSIC_TIME it started at
    pub start: u32,
}

/// The performance's segment states: what a play / stop does to them.
#[derive(Default, Debug)]
pub struct Plays {
    pub states: Vec<(u32, StateInfo)>,
}

impl Plays {
    /// A new state `key` of `info`: the states it replaces (a primary one stops the primary
    /// playing; a secondary one replaces nothing).
    pub fn play(&mut self, key: u32, info: StateInfo) -> Vec<u32> {
        let stop: Vec<u32> = if info.primary { self.states.iter().filter(|s| s.1.primary).map(|s| s.0).collect() } else { Vec::new() };
        self.states.retain(|s| !stop.contains(&s.0));
        self.states.push((key, info));
        stop
    }
    /// StopEx / Stop of an object: a state, a segment's states, an audio path's, or (0) all.
    pub fn stop(&mut self, obj: u32) -> Vec<u32> {
        let stop: Vec<u32> = self.states.iter().filter(|s| obj == 0 || s.0 == obj || s.1.segment == obj || s.1.path == obj).map(|s| s.0).collect();
        self.states.retain(|s| !stop.contains(&s.0));
        stop
    }
    /// The states that have ended (by the mixer) leave.
    pub fn reap(&mut self, playing: impl Fn(u32) -> bool) -> Vec<u32> {
        let gone: Vec<u32> = self.states.iter().filter(|s| !playing(s.0)).map(|s| s.0).collect();
        self.states.retain(|s| !gone.contains(&s.0));
        gone
    }
    pub fn of_segment(&self, seg: u32) -> Vec<u32> {
        self.states.iter().filter(|s| s.1.segment == seg).map(|s| s.0).collect()
    }
}

/// A volume (hundredths of a dB) of the master and an audio path's: added, at most 0 dB (the
/// mixer does not amplify), silence at -96 dB and below.
pub fn total_volume(master: i32, path: i32) -> i32 {
    master.saturating_add(path).clamp(-10000, 0)
}

// ---------------------------------------------------------------------------------------------
// the objects: [vtable, references, ...] in guest memory

struct Segment {
    sound: Option<Sound>,
    /// the file in guest memory (freed with the segment)
    file: u32,
    repeats: u32,
    length: u32,
    start: u32,
    loop_points: (u32, u32),
    /// a clone's original (whose samples it plays): the clone holds a reference on it, given
    /// back when the clone goes
    parent: u32,
}

struct Perf {
    master: i32,
    path: u32,
    plays: Plays,
    t0: std::time::Instant,
}

#[derive(Default)]
struct World {
    segments: crate::HashMap<u32, Segment>,
    perfs: crate::HashMap<u32, Perf>,
    /// audio path -> (performance, volume)
    paths: crate::HashMap<u32, (u32, i32)>,
    /// segment state -> (performance, info)
    states: crate::HashMap<u32, (u32, StateInfo)>,
    /// loader -> (search directory, cache on, cache: full path lowercased -> segment)
    loaders: crate::HashMap<u32, (String, bool, crate::HashMap<String, u32>)>,
}

static WORLD: Mutex<Option<World>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut World) -> R) -> R {
    let mut g = WORLD.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(World::default))
}

fn read16(p: u32) -> [u8; 16] {
    let mut g = [0u8; 16];
    g.copy_from_slice(mem::slice(p, 16));
    g
}

fn new_object(vt: u32, kind: Kind) -> u32 {
    let k = mem::ps();
    let o = mem::heap_alloc(4 * k, true);
    mem::wp(o, vt);
    mem::w32(o + k, 1);
    mem::w32(o + 2 * k, kind as u32);
    o
}

fn kind_of(o: u32) -> Option<Kind> {
    if o == 0 {
        return None;
    }
    Some(match mem::r32(o + 2 * mem::ps()) {
        0 => Kind::Performance,
        1 => Kind::Loader,
        2 => Kind::Segment,
        3 => Kind::State,
        4 => Kind::AudioPath,
        _ => return None,
    })
}

/// CoCreateInstance / CoGetClassObject of the performance or the loader: None for another class.
pub fn create_instance(clsid: u32, outer: u32, iid: u32, out: u32) -> Option<u32> {
    let kind = match read16(clsid) {
        CLSID_PERFORMANCE => Kind::Performance,
        CLSID_LOADER => Kind::Loader,
        CLSID_SEGMENT => Kind::Segment,
        _ => return None,
    };
    if out == 0 {
        return Some(E_POINTER);
    }
    mem::putp(out, 0);
    if outer != 0 {
        return Some(CLASS_E_NOAGGREGATION);
    }
    if iid == 0 || !answers(kind, &read16(iid)) {
        return Some(E_NOINTERFACE);
    }
    let o = match kind {
        Kind::Performance => new_perf(),
        Kind::Loader => new_loader(),
        _ => new_segment(None, 0),
    };
    mem::putp(out, o);
    Some(0)
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let ok = iid != 0 && kind_of(this).is_some_and(|k| answers(k, &read16(iid)));
    if !ok {
        mem::putp(out, 0);
        return E_NOINTERFACE;
    }
    add_ref_obj(this);
    mem::putp(out, this);
    0
}

fn add_ref_obj(o: u32) -> u32 {
    let p = o + mem::ps();
    let n = mem::r32(p).wrapping_add(1);
    mem::w32(p, n);
    n
}

fn add_ref(c: &mut Ctx) -> u32 {
    add_ref_obj(c.a(0))
}

/// The references left; at 0 the object goes (a segment stops its states and frees its file).
pub fn release_count(refs: u32) -> (u32, bool) {
    match refs {
        0 => (0, false),
        1 => (0, true),
        n => (n - 1, false),
    }
}

/// The object block of `o` goes: its kind word is spoiled first (a call through a stale pointer
/// finds no object), and the block is freed after com.rs's quarantine of recent frees.
fn retire(o: u32) {
    mem::w32(o + 2 * mem::ps(), 0xff);
    crate::com::free_later(o);
}

fn release_obj(o: u32) -> u32 {
    if kind_of(o).is_none() {
        return 0;
    }
    let p = o + mem::ps();
    let (n, gone) = release_count(mem::r32(p));
    mem::w32(p, n);
    if gone {
        match kind_of(o) {
            Some(Kind::Segment) => {
                let (seg, states) = with(|w| {
                    let states: Vec<u32> = w.states.iter().filter(|s| s.1 .1.segment == o).map(|s| *s.0).collect();
                    for s in &states {
                        if let Some((perf, _)) = w.states.remove(s) {
                            if let Some(p) = w.perfs.get_mut(&perf) {
                                p.plays.stop(*s);
                            }
                        }
                    }
                    (w.segments.remove(&o), states)
                });
                for s in states {
                    crate::sound::music_stop(s);
                    // (the performance's reference on the state)
                    release_obj(s);
                }
                if let Some(s) = &seg {
                    if s.file != 0 {
                        mem::heap_free(s.file);
                    }
                    if s.parent != 0 {
                        release_obj(s.parent);
                    }
                }
            }
            Some(Kind::Performance) => {
                close_down(o);
                with(|w| w.perfs.remove(&o));
            }
            Some(Kind::Loader) => {
                let cache = with(|w| w.loaders.remove(&o)).map(|l| l.2).unwrap_or_default();
                for (_, seg) in cache {
                    release_obj(seg);
                }
            }
            Some(Kind::State) => {
                with(|w| w.states.remove(&o));
            }
            Some(Kind::AudioPath) => with(|w| {
                w.paths.remove(&o);
                for p in w.perfs.values_mut().filter(|p| p.path == o) {
                    p.path = 0;
                }
            }),
            None => return n,
        }
        retire(o);
    }
    n
}

fn release(c: &mut Ctx) -> u32 {
    release_obj(c.a(0))
}

fn ok(_: &mut Ctx) -> u32 {
    0
}
fn not_impl(_: &mut Ctx) -> u32 {
    0x8000_4001
}

// ---------------------------------------------------------------------------------------------
// the performance

fn perf_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        // (stack bytes with `this`; __int64 and REFERENCE_TIME arguments are 8)
        let m: &[(Handler, u32)] = &[
            (qi, 12),
            (add_ref, 4),
            (release, 4),
            (p_init, 16),               // Init(IDirectMusic**, IDirectSound*, HWND)
            (p_play_segment, 24),       // PlaySegment(seg, flags, i64 start, IDirectMusicSegmentState**)
            (p_stop, 20),               // Stop(seg, state, mtTime, flags)
            (p_get_segment_state, 12),  // GetSegmentState(**state, mtTime)
            (ok, 8),                    // SetPrepareTime(ms)
            (p_get_dword_1000, 8),      // GetPrepareTime(*ms)
            (ok, 8),                    // SetBumperLength(ms)
            (p_get_dword_50, 8),        // GetBumperLength(*ms)
            (not_impl, 8),              // SendPMsg(pmsg)
            (p_music_to_ref, 12),       // MusicToReferenceTime(mt, *rt)
            (p_ref_to_music, 16),       // ReferenceToMusicTime(rt, *mt)
            (p_is_playing, 12),         // IsPlaying(seg, state)
            (p_get_time, 12),           // GetTime(*rt, *mt)
            (not_impl, 12),             // AllocPMsg(cb, **pmsg)
            (not_impl, 8),              // FreePMsg(pmsg)
            (not_found_out, 8),         // GetGraph(**graph)
            (ok, 8),                    // SetGraph(graph)
            (ok, 16),                   // SetNotificationHandle(handle, rtMinimum)
            (s_false_out, 8),           // GetNotificationPMsg(**pmsg)
            (ok, 8),                    // AddNotificationType(guid)
            (ok, 8),                    // RemoveNotificationType(guid)
            (ok, 8),                    // AddPort(port)
            (ok, 8),                    // RemovePort(port)
            (ok, 16),                   // AssignPChannelBlock(block, port, group)
            (ok, 20),                   // AssignPChannel(pch, port, group, mch)
            (not_found, 20),            // PChannelInfo(pch, **port, *group, *mch)
            (not_impl, 36),             // DownloadInstrument(...)
            (ok, 12),                   // Invalidate(mt, flags)
            (not_found, 28),            // GetParam(guid, group, index, mt, *next, param)
            (ok, 24),                   // SetParam(guid, group, index, mt, param)
            (p_get_global, 16),         // GetGlobalParam(guid, param, size)
            (p_set_global, 16),         // SetGlobalParam(guid, param, size)
            (p_get_rt_now, 8),          // GetLatencyTime(*rt)
            (p_get_rt_now, 8),          // GetQueueTime(*rt)
            (ok, 12),                   // AdjustTime(rt)
            (p_close_down, 4),          // CloseDown()
            (p_resolved, 20),           // GetResolvedTime(rt, *rt, flags)
            (not_impl, 24),             // MIDIToMusic
            (not_impl, 24),             // MusicToMIDI
            (not_impl, 28),             // TimeToRhythm
            (not_impl, 28),             // RhythmToTime
            (p_init_audio, 32),         // InitAudio(IDirectMusic**, IDirectSound**, HWND, path type, pchannels, flags, params)
            (p_play_segment_ex, 40),    // PlaySegmentEx(source, name, transition, flags, i64 start, **state, from, path)
            (p_stop_ex, 20),            // StopEx(obj, i64 time, flags)
            (not_impl, 12),             // ClonePMsg
            (p_create_audio_path, 16),  // CreateAudioPath(config, activate, **path)
            (p_create_std_path, 20),    // CreateStandardAudioPath(type, pchannels, activate, **path)
            (p_set_default_path, 8),    // SetDefaultAudioPath(path)
            (p_get_default_path, 8),    // GetDefaultAudioPath(**path)
            (not_found, 32),            // GetParamEx
            (ok, 28),                   // SetParamEx
        ];
        crate::api::vtable("IDirectMusicPerformance8", m)
    })
}

fn new_perf() -> u32 {
    let o = new_object(perf_vtbl(), Kind::Performance);
    with(|w| w.perfs.insert(o, Perf { master: 0, path: 0, plays: Plays::default(), t0: std::time::Instant::now() }));
    o
}

fn not_found(_: &mut Ctx) -> u32 {
    DMUS_E_NOT_FOUND
}
fn not_found_out(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        mem::putp(c.a(1), 0);
    }
    DMUS_E_NOT_FOUND
}
fn s_false_out(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        mem::putp(c.a(1), 0);
    }
    S_FALSE
}
fn p_get_dword_1000(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(1), 1000);
    0
}
fn p_get_dword_50(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(1), 50);
    0
}

/// The out pointers of Init / InitAudio: a DirectMusic and a DirectSound object when asked for.
fn init_outs(dm: u32, ds: u32, ds_is_pp: bool) {
    if dm != 0 && mem::rp(dm) == 0 {
        mem::putp(dm, crate::dmusic::new_object());
    }
    if ds_is_pp && ds != 0 && mem::rp(ds) == 0 {
        mem::putp(ds, crate::sound::device().unwrap_or(0));
    }
}

fn p_init(c: &mut Ctx) -> u32 {
    init_outs(c.a(1), c.a(2), false);
    0
}

fn p_init_audio(c: &mut Ctx) -> u32 {
    let (this, dm, ds, path_type) = (c.a(0), c.a(1), c.a(2), c.a(4));
    init_outs(dm, ds, true);
    if path_type != 0 {
        let p = new_path(this);
        with(|w| {
            if let Some(perf) = w.perfs.get_mut(&this) {
                perf.path = p;
            }
        });
    }
    0
}

fn new_path(perf: u32) -> u32 {
    let o = new_object(path_vtbl(), Kind::AudioPath);
    with(|w| w.paths.insert(o, (perf, 0)));
    o
}

fn p_create_audio_path(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    mem::putp(out, new_path(this));
    0
}
fn p_create_std_path(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(4));
    if out == 0 {
        return E_POINTER;
    }
    mem::putp(out, new_path(this));
    0
}
fn p_set_default_path(c: &mut Ctx) -> u32 {
    let (this, path) = (c.a(0), c.a(1));
    with(|w| {
        if let Some(p) = w.perfs.get_mut(&this) {
            p.path = path;
        }
    });
    0
}
fn p_get_default_path(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let p = with(|w| w.perfs.get(&this).map_or(0, |p| p.path));
    mem::putp(out, p);
    if p == 0 {
        return DMUS_E_NOT_FOUND;
    }
    add_ref_obj(p);
    0
}

/// Starts state(s) of `seg` on `perf` (flags DMUS_SEGF_*), on `path` (0: the default one).
fn play(perf: u32, seg: u32, flags: u32, path: u32, out: u32) -> u32 {
    if kind_of(seg) != Some(Kind::Segment) {
        return E_INVALIDARG;
    }
    let state = new_object(state_vtbl(), Kind::State);
    let r = with(|w| {
        let p = w.perfs.get_mut(&perf)?;
        let path = if path != 0 { path } else { p.path };
        let now = (p.t0.elapsed().as_secs_f64() * TICKS_PER_SECOND as f64) as u32;
        let info = StateInfo { segment: seg, primary: flags & SEGF_SECONDARY == 0, path, start: now };
        let stop = p.plays.play(state, info.clone());
        let gone = p.plays.reap(|k| k == state || crate::sound::music_playing(k));
        let volume = total_volume(p.master, w.paths.get(&path).map_or(0, |x| x.1));
        for s in stop.iter().chain(gone.iter()) {
            w.states.remove(s);
        }
        w.states.insert(state, (perf, info));
        let s = w.segments.get(&seg)?;
        Some((stop, gone, s.sound, s.repeats, volume))
    });
    let Some((stop, gone, sound, repeats, volume)) = r else {
        release_obj(state);
        return E_INVALIDARG;
    };
    // (a state the performance lets go of: its reference goes, the block with the last one)
    for s in stop.into_iter().chain(gone) {
        crate::sound::music_stop(s);
        release_obj(s);
    }
    if let Some(s) = sound {
        let fmt = Fmt { rate: s.rate, channels: s.channels, bits: s.bits, float: s.float, law: 0 };
        crate::sound::music_start(state, s.data, s.size, fmt, repeats, volume);
    }
    if out != 0 {
        add_ref_obj(state);
        mem::putp(out, state);
    }
    0
}

fn p_play_segment(c: &mut Ctx) -> u32 {
    // (x86: the __int64 takes two slots)
    let out = if mem::x64() { c.a(4) } else { c.a(5) };
    play(c.a(0), c.a(1), c.a(2), 0, out)
}
fn p_play_segment_ex(c: &mut Ctx) -> u32 {
    let (this, source, flags) = (c.a(0), c.a(1), c.a(4));
    let (out, path) = if mem::x64() { (c.a(6), c.a(8)) } else { (c.a(7), c.a(9)) };
    play(this, source, flags, path, out)
}

fn stop(perf: u32, obj: u32) -> u32 {
    let stop = with(|w| {
        let Some(p) = w.perfs.get_mut(&perf) else { return Vec::new() };
        let s = p.plays.stop(obj);
        for k in &s {
            w.states.remove(k);
        }
        s
    });
    for s in stop {
        crate::sound::music_stop(s);
        release_obj(s);
    }
    0
}
fn p_stop(c: &mut Ctx) -> u32 {
    let (this, seg, state) = (c.a(0), c.a(1), c.a(2));
    if seg == 0 && state == 0 {
        return stop(this, 0);
    }
    if state != 0 {
        stop(this, state);
    }
    if seg != 0 {
        stop(this, seg);
    }
    0
}
fn p_stop_ex(c: &mut Ctx) -> u32 {
    stop(c.a(0), c.a(1))
}

fn p_is_playing(c: &mut Ctx) -> u32 {
    let (this, seg, state) = (c.a(0), c.a(1), c.a(2));
    if seg == 0 && state == 0 {
        return E_POINTER;
    }
    let keys = with(|w| {
        w.perfs.get(&this).map_or(Vec::new(), |p| if state != 0 { p.plays.states.iter().filter(|s| s.0 == state).map(|s| s.0).collect() } else { p.plays.of_segment(seg) })
    });
    if keys.iter().any(|&k| crate::sound::music_playing(k)) {
        0
    } else {
        S_FALSE
    }
}

fn p_get_segment_state(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let s = with(|w| w.perfs.get(&this).and_then(|p| p.plays.states.iter().rev().find(|s| s.1.primary && crate::sound::music_playing(s.0)).map(|s| s.0)));
    match s {
        Some(s) => {
            add_ref_obj(s);
            mem::putp(out, s);
            0
        }
        None => {
            mem::putp(out, 0);
            DMUS_E_NOT_FOUND
        }
    }
}

fn now_rt(perf: u32) -> u64 {
    with(|w| w.perfs.get(&perf).map_or(0, |p| p.t0.elapsed().as_nanos() as u64 / 100))
}
fn p_get_time(c: &mut Ctx) -> u32 {
    let (this, rt, mt) = (c.a(0), c.a(1), c.a(2));
    let t = now_rt(this);
    if rt != 0 {
        mem::w64(rt, t);
    }
    if mt != 0 {
        mem::w32(mt, (t * TICKS_PER_SECOND / 10_000_000) as u32);
    }
    0
}
fn p_get_rt_now(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w64(c.a(1), now_rt(c.a(0)));
    0
}
fn p_music_to_ref(c: &mut Ctx) -> u32 {
    let (mt, out) = (c.a(1) as i32, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    mem::w64(out, (mt as i64 * 10_000_000 / TICKS_PER_SECOND as i64) as u64);
    0
}
fn p_ref_to_music(c: &mut Ctx) -> u32 {
    let (rt, out) = if mem::x64() { (c.a64(1), c.a(2)) } else { (c.a(1) as u64 | (c.a(2) as u64) << 32, c.a(3)) };
    if out == 0 {
        return E_POINTER;
    }
    mem::w32(out, (rt as i64 * TICKS_PER_SECOND as i64 / 10_000_000) as u32);
    0
}
fn p_resolved(c: &mut Ctx) -> u32 {
    let (rt, out) = if mem::x64() { (c.a64(1), c.a(2)) } else { (c.a(1) as u64 | (c.a(2) as u64) << 32, c.a(3)) };
    if out != 0 {
        mem::w64(out, rt);
    }
    0
}

fn p_get_global(c: &mut Ctx) -> u32 {
    let (this, g, p, size) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if g == 0 || p == 0 {
        return E_POINTER;
    }
    match read16(g) {
        GUID_PERF_MASTER_VOLUME if size >= 4 => {
            mem::w32(p, with(|w| w.perfs.get(&this).map_or(0, |x| x.master)) as u32);
            0
        }
        GUID_PERF_MASTER_TEMPO if size >= 4 => {
            mem::w32(p, 1f32.to_bits());
            0
        }
        _ => DMUS_E_TYPE_UNSUPPORTED,
    }
}
fn p_set_global(c: &mut Ctx) -> u32 {
    let (this, g, p, size) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if g == 0 || p == 0 {
        return E_POINTER;
    }
    if read16(g) == GUID_PERF_MASTER_VOLUME && size >= 4 {
        let v = mem::r32(p) as i32;
        let upd = with(|w| {
            let Some(perf) = w.perfs.get_mut(&this) else { return Vec::new() };
            perf.master = v;
            let states: Vec<(u32, u32)> = perf.plays.states.iter().map(|s| (s.0, s.1.path)).collect();
            states.into_iter().map(|(k, path)| (k, total_volume(v, w.paths.get(&path).map_or(0, |x| x.1)))).collect::<Vec<_>>()
        });
        for (k, vol) in upd {
            crate::sound::music_volume(k, vol);
        }
    }
    0
}

fn close_down(perf: u32) {
    stop(perf, 0);
}
fn p_close_down(c: &mut Ctx) -> u32 {
    close_down(c.a(0));
    0
}

// ---------------------------------------------------------------------------------------------
// audio paths

fn path_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[
            (qi, 12),
            (add_ref, 4),
            (release, 4),
            (path_object, 32), // GetObjectInPath(pch, stage, buffer, guid, index, iid, **out)
            (ok, 8),           // Activate(bool)
            (path_volume, 12), // SetVolume(volume, duration)
            (path_convert, 12), // ConvertPChannel(in, *out)
        ];
        crate::api::vtable("IDirectMusicAudioPath", m)
    })
}

fn path_object(c: &mut Ctx) -> u32 {
    if c.a(7) != 0 {
        mem::putp(c.a(7), 0);
    }
    DMUS_E_NOT_FOUND
}
fn path_convert(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(2), c.a(1));
    0
}
fn path_volume(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1) as i32);
    if !(-9600..=0).contains(&v) {
        return E_INVALIDARG;
    }
    let upd = with(|w| {
        let Some(e) = w.paths.get_mut(&this) else { return Vec::new() };
        e.1 = v;
        let perf = e.0;
        let Some(p) = w.perfs.get(&perf) else { return Vec::new() };
        p.plays.states.iter().filter(|s| s.1.path == this).map(|s| (s.0, total_volume(p.master, v))).collect()
    });
    for (k, vol) in upd {
        crate::sound::music_volume(k, vol);
    }
    0
}

// ---------------------------------------------------------------------------------------------
// segments and their states

fn segment_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[
            (qi, 12),
            (add_ref, 4),
            (release, 4),
            (seg_get_length, 8),    // GetLength(*mt)
            (seg_set_length, 8),    // SetLength(mt)
            (seg_get_repeats, 8),   // GetRepeats(*n)
            (seg_set_repeats, 8),   // SetRepeats(n)
            (seg_get_zero, 8),      // GetDefaultResolution(*flags)
            (ok, 8),                // SetDefaultResolution(flags)
            (not_found, 20),        // GetTrack(clsid, group, index, **track)
            (not_found, 12),        // GetTrackGroup(track, *group)
            (ok, 12),               // InsertTrack(track, group)
            (not_found, 8),         // RemoveTrack(track)
            (not_impl, 16),         // InitPlay(**state, perf, flags)
            (not_found_out, 8),     // GetGraph(**graph)
            (ok, 8),                // SetGraph(graph)
            (ok, 8),                // AddNotificationType(guid)
            (ok, 8),                // RemoveNotificationType(guid)
            (not_found, 28),        // GetParam(guid, group, index, mt, *next, param)
            (ok, 24),               // SetParam(guid, group, index, mt, param)
            (seg_clone, 16),        // Clone(start, end, **seg)
            (seg_set_start, 8),     // SetStartPoint(mt)
            (seg_get_start, 8),     // GetStartPoint(*mt)
            (seg_set_loop, 12),     // SetLoopPoints(start, end)
            (seg_get_loop, 12),     // GetLoopPoints(*start, *end)
            (ok, 12),               // SetPChannelsUsed(n, pchannels)
            (ok, 24),               // SetTrackConfig(clsid, group, index, on, off)
            (not_found_out, 8),     // GetAudioPathConfig(**config)
            (not_impl, 20),         // Compose(mt, from, to, **seg)
            (ok, 8),                // Download(path or performance)
            (ok, 8),                // Unload(path or performance)
        ];
        crate::api::vtable("IDirectMusicSegment8", m)
    })
}

fn new_segment(sound: Option<Sound>, file: u32) -> u32 {
    let o = new_object(segment_vtbl(), Kind::Segment);
    let length = sound.map_or(0, |s| s.length());
    with(|w| w.segments.insert(o, Segment { sound, file, repeats: 0, length, start: 0, loop_points: (0, 0), parent: 0 }));
    o
}

fn seg_field<R>(o: u32, f: impl FnOnce(&mut Segment) -> R) -> Option<R> {
    with(|w| w.segments.get_mut(&o).map(f))
}

fn seg_get_length(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(1), seg_field(c.a(0), |s| s.length).unwrap_or(0));
    0
}
fn seg_set_length(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    seg_field(c.a(0), |s| s.length = v);
    0
}
fn seg_get_repeats(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(1), seg_field(c.a(0), |s| s.repeats).unwrap_or(0));
    0
}
fn seg_set_repeats(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    seg_field(c.a(0), |s| s.repeats = v);
    0
}
fn seg_get_zero(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(1), 0);
    0
}
fn seg_set_start(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    seg_field(c.a(0), |s| s.start = v);
    0
}
fn seg_get_start(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(1), seg_field(c.a(0), |s| s.start).unwrap_or(0));
    0
}
fn seg_set_loop(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(1), c.a(2));
    seg_field(c.a(0), |s| s.loop_points = (a, b));
    0
}
fn seg_get_loop(c: &mut Ctx) -> u32 {
    let (pa, pb) = (c.a(1), c.a(2));
    if pa == 0 || pb == 0 {
        return E_POINTER;
    }
    let (a, b) = seg_field(c.a(0), |s| s.loop_points).unwrap_or((0, 0));
    mem::w32(pa, a);
    mem::w32(pb, b);
    0
}
fn seg_clone(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    // (the clone shares the file: the original keeps it alive, so the clone holds a reference)
    let sound = seg_field(this, |s| (s.sound, s.repeats));
    let Some((sound, repeats)) = sound else { return E_INVALIDARG };
    let o = new_segment(sound, 0);
    add_ref_obj(this);
    seg_field(o, |s| {
        s.repeats = repeats;
        s.parent = this;
    });
    mem::putp(out, o);
    0
}

fn state_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[
            (qi, 12),
            (add_ref, 4),
            (release, 4),
            (st_get_repeats, 8),    // GetRepeats(*n)
            (st_get_segment, 8),    // GetSegment(**seg)
            (st_get_start, 8),      // GetStartTime(*mt)
            (st_get_seek, 8),       // GetSeek(*mt)
            (seg_get_zero, 8),      // GetStartPoint(*mt)
            (ok, 24),               // SetTrackConfig
            (path_object, 32),      // GetObjectInPath
        ];
        crate::api::vtable("IDirectMusicSegmentState8", m)
    })
}

fn state_info(o: u32) -> Option<StateInfo> {
    with(|w| w.states.get(&o).map(|s| s.1.clone()))
}
fn st_get_repeats(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let seg = state_info(c.a(0)).map_or(0, |s| s.segment);
    mem::w32(c.a(1), seg_field(seg, |s| s.repeats).unwrap_or(0));
    0
}
fn st_get_segment(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    match state_info(c.a(0)) {
        Some(s) => {
            add_ref_obj(s.segment);
            mem::putp(c.a(1), s.segment);
            0
        }
        None => {
            mem::putp(c.a(1), 0);
            DMUS_E_NOT_FOUND
        }
    }
}
fn st_get_start(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::w32(c.a(1), state_info(c.a(0)).map_or(0, |s| s.start));
    0
}
fn st_get_seek(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let rate = state_info(this).and_then(|s| seg_field(s.segment, |g| g.sound.map_or(0, |x| x.rate))).unwrap_or(0);
    let pos = crate::sound::music_pos(this).unwrap_or(0.0);
    mem::w32(out, if rate == 0 { 0 } else { (pos * TICKS_PER_SECOND as f64 / rate as f64) as u32 });
    0
}

// ---------------------------------------------------------------------------------------------
// the loader

fn loader_vtbl() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[
            (qi, 12),
            (add_ref, 4),
            (release, 4),
            (l_get_object, 16),        // GetObject(desc, iid, **out)
            (ok, 8),                   // SetObject(desc)
            (l_set_search_dir, 16),    // SetSearchDirectory(class, path, clear)
            (s_false3, 16),            // ScanDirectory(class, ext, cache file)
            (ok, 8),                   // CacheObject(obj)
            (l_release_object, 8),     // ReleaseObject(obj)
            (l_clear_cache, 8),        // ClearCache(class)
            (l_enable_cache, 12),      // EnableCache(class, enable)
            (s_false3, 16),            // EnumObject(class, index, desc)
            (ok, 4),                   // CollectGarbage()
            (l_release_object, 8),     // ReleaseObjectByUnknown(obj)
            (l_load_from_file, 20),    // LoadObjectFromFile(clsid, iid, path, **out)
        ];
        crate::api::vtable("IDirectMusicLoader8", m)
    })
}

fn new_loader() -> u32 {
    let o = new_object(loader_vtbl(), Kind::Loader);
    with(|w| w.loaders.insert(o, (String::new(), true, crate::HashMap::default())));
    o
}

fn s_false3(_: &mut Ctx) -> u32 {
    S_FALSE
}

fn wide(p: u32) -> String {
    String::from_utf16_lossy(&mem::wstr(p))
}

/// The file a loader opens for `name`: as it is when absolute (or FULLPATH), else in the search
/// directory, else the current one.
pub fn join(dir: &str, name: &str, full: bool) -> String {
    let abs = full || name.starts_with('\\') || name.starts_with('/') || name.as_bytes().get(1) == Some(&b':');
    if abs || dir.is_empty() {
        return name.to_string();
    }
    if dir.ends_with('\\') || dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}\\{name}") }
}

fn l_set_search_dir(c: &mut Ctx) -> u32 {
    let (this, path) = (c.a(0), c.a(2));
    if path == 0 {
        return E_POINTER;
    }
    let d = wide(path);
    with(|w| {
        if let Some(l) = w.loaders.get_mut(&this) {
            l.0 = d;
        }
    });
    0
}
fn l_enable_cache(c: &mut Ctx) -> u32 {
    let (this, on) = (c.a(0), c.a(2) != 0);
    with(|w| {
        if let Some(l) = w.loaders.get_mut(&this) {
            l.1 = on;
        }
    });
    0
}
fn l_clear_cache(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let cache = with(|w| w.loaders.get_mut(&this).map(|l| std::mem::take(&mut l.2))).unwrap_or_default();
    for (_, s) in cache {
        release_obj(s);
    }
    0
}
fn l_release_object(c: &mut Ctx) -> u32 {
    let (this, obj) = (c.a(0), c.a(1));
    let found = with(|w| {
        let l = w.loaders.get_mut(&this)?;
        let k = l.2.iter().find(|e| *e.1 == obj)?.0.clone();
        l.2.remove(&k)
    });
    match found {
        Some(s) => {
            release_obj(s);
            0
        }
        None => S_FALSE,
    }
}

/// A segment of the file's bytes: the samples copied into guest memory for a PCM wave, a
/// silent segment for anything else.
fn segment_of_bytes(b: &[u8]) -> u32 {
    segment_of_pcm(b, wave_pcm(b))
}
/// segment_of_bytes with the wave already parsed (`pcm`: wave_pcm's answer); a wave the guest
/// heap has no room for is a silent segment.
fn segment_of_pcm(b: &[u8], pcm: Option<(u32, u32, u32, u32, u32, bool)>) -> u32 {
    match pcm {
        Some((off, size, rate, channels, bits, float)) => {
            let file = mem::heap_alloc(size.max(1), false);
            if file == 0 {
                eprintln!("sake: dmime: no guest memory for a wave of {size} bytes (a silent segment)");
                return new_segment(None, 0);
            }
            mem::write(file, &b[off as usize..(off + size) as usize]);
            new_segment(Some(Sound { data: file, size, rate, channels, bits, float }), file)
        }
        None => new_segment(None, 0),
    }
}

/// Loads (or finds in the cache) `path` for loader `this`: the segment with a reference for
/// the caller.
fn load_file(this: u32, path: &str) -> Result<u32, u32> {
    let key = crate::files::full_path(path).to_lowercase();
    let cached = with(|w| w.loaders.get(&this).and_then(|l| if l.1 { l.2.get(&key).copied() } else { None }));
    if let Some(s) = cached {
        add_ref_obj(s);
        return Ok(s);
    }
    let f = crate::files::open(path, 0x8000_0000, 3).map_err(|_| DMUS_E_LOADER_FAILEDOPEN)?;
    let bytes = f.read_vec(f.size() as usize);
    // (parsed once: the log line and the segment)
    let pcm = wave_pcm(&bytes);
    if pcm.is_none() {
        eprintln!("sake: dmime: {path}: no sound (not a PCM wave; a silent segment)");
    }
    let s = segment_of_pcm(&bytes, pcm);
    let caching = with(|w| {
        let l = w.loaders.get_mut(&this)?;
        l.1.then(|| l.2.insert(key, s))
    });
    if caching.is_some() {
        add_ref_obj(s);
    }
    Ok(s)
}

fn hand_out(seg: u32, iid: u32, out: u32) -> u32 {
    if iid != 0 && !answers(Kind::Segment, &read16(iid)) {
        release_obj(seg);
        mem::putp(out, 0);
        return E_NOINTERFACE;
    }
    mem::putp(out, seg);
    0
}

fn l_load_from_file(c: &mut Ctx) -> u32 {
    let (this, clsid, iid, path, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if out == 0 {
        return E_POINTER;
    }
    mem::putp(out, 0);
    if path == 0 {
        return DMUS_E_LOADER_NOFILENAME;
    }
    if clsid != 0 && read16(clsid) != CLSID_SEGMENT {
        return DMUS_E_LOADER_FAILEDOPEN;
    }
    let dir = with(|w| w.loaders.get(&this).map(|l| l.0.clone())).unwrap_or_default();
    match load_file(this, &join(&dir, &wide(path), false)) {
        Ok(s) => hand_out(s, iid, out),
        Err(e) => e,
    }
}

/// DMUS_OBJECTDESC (x86): dwSize 0, dwValidData 4, guidObject 8, guidClass 24, ftDate 40,
/// vVersion 48, wszName 56, wszCategory 184, wszFileName 312, llMemLength 832, pbMemData 840.
fn l_get_object(c: &mut Ctx) -> u32 {
    let (this, desc, iid, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if desc == 0 || out == 0 {
        return E_POINTER;
    }
    mem::putp(out, 0);
    let valid = mem::r32(desc + 4);
    if valid & 2 != 0 && read16(desc + 24) != CLSID_SEGMENT && read16(desc + 24) != GUID_ALL_TYPES {
        return DMUS_E_LOADER_FAILEDOPEN;
    }
    if valid & 0x400 != 0 {
        // DMUS_OBJ_MEMORY
        let (len, data) = (mem::r64(desc + 832) as u32, mem::r32(desc + 840));
        if data == 0 || len == 0 {
            return E_POINTER;
        }
        let bytes = mem::slice(data, len).to_vec();
        return hand_out(segment_of_bytes(&bytes), iid, out);
    }
    if valid & 0x10 == 0 {
        return DMUS_E_LOADER_NOFILENAME;
    }
    let name = String::from_utf16_lossy(&mem::wstrn(desc + 312, 260));
    let dir = with(|w| w.loaders.get(&this).map(|l| l.0.clone())).unwrap_or_default();
    match load_file(this, &join(&dir, &name, valid & 0x20 != 0)) {
        Ok(s) => hand_out(s, iid, out),
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn guids_as_dmusici_h() {
        // DEFINE_GUID(CLSID_DirectMusicPerformance, 0xd2ac2881,0xb39b,0x11d1,0x87,0x04,0x00,0x60,0x08,0x93,0xb1,0xbd)
        assert_eq!(CLSID_PERFORMANCE, [0x81, 0x28, 0xac, 0xd2, 0x9b, 0xb3, 0xd1, 0x11, 0x87, 0x04, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd]);
        // IID_IDirectMusicPerformance8 0x679c4137,0xc62e,0x4147,0xb2,0xb4,0x9d,0x56,0x9a,0xcb,0x25,0x4c
        assert_eq!(PERF_IIDS[2], [0x37, 0x41, 0x9c, 0x67, 0x2e, 0xc6, 0x47, 0x41, 0xb2, 0xb4, 0x9d, 0x56, 0x9a, 0xcb, 0x25, 0x4c]);
        assert_eq!(LOADER_IIDS[1][..4], [0x8c, 0xc0, 0xe7, 0x19]);
        assert_eq!(SEGMENT_IIDS[2][..4], [0x88, 0x44, 0x78, 0xc6]);
        assert!(answers(Kind::Segment, &IID_UNKNOWN));
        assert!(!answers(Kind::Segment, &PERF_IIDS[2]));
        assert!(!answers(Kind::Performance, &CLSID_PERFORMANCE));
    }

    fn wav(rate: u32, ch: u16, bits: u16, frames: u32) -> Vec<u8> {
        let block = ch as u32 * bits as u32 / 8;
        let data = frames * block;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&ch.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * block).to_le_bytes());
        b.extend_from_slice(&(block as u16).to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        b.extend((0..data).map(|i| i as u8));
        b
    }

    #[test]
    fn a_pcm_wave_is_its_data_chunk() {
        let b = wav(22050, 2, 16, 1000);
        assert_eq!(wave_pcm(&b), Some((44, 4000, 22050, 2, 16, false)));
        // MIDI, a style, a truncated header: no sound
        assert_eq!(wave_pcm(b"MThd\0\0\0\x06\0\x01\0\x01\x01\xe0"), None);
        assert_eq!(wave_pcm(b"RIFF\0\0\0\0DMSG"), None);
        assert_eq!(wave_pcm(&b[..30]), None);
    }

    #[test]
    fn lengths_in_music_time() {
        // one second at the default tempo: 2 quarter notes of DMUS_PPQ 768
        let s = Sound { data: 0, size: 44100 * 4, rate: 44100, channels: 2, bits: 16, float: false };
        assert_eq!(s.length(), 1536);
        let s = Sound { data: 0, size: 11025, rate: 22050, channels: 1, bits: 8, float: false };
        assert_eq!(s.length(), 768);
        assert_eq!(Sound { rate: 0, ..s }.length(), 0);
    }

    #[test]
    fn paths_join_as_the_loader_searches() {
        assert_eq!(join("C:\\game", "bin\\JG_01.WAV", false), "C:\\game\\bin\\JG_01.WAV");
        assert_eq!(join("C:\\game\\", "a.wav", false), "C:\\game\\a.wav");
        assert_eq!(join("", "bin\\a.wav", false), "bin\\a.wav");
        assert_eq!(join("C:\\game", "D:\\x.wav", false), "D:\\x.wav");
        assert_eq!(join("C:\\game", "x.wav", true), "x.wav");
        assert_eq!(join("C:\\game", "\\root.wav", false), "\\root.wav");
    }

    #[derive(Clone, Debug)]
    enum Op {
        Play { seg: u32, secondary: bool, path: u32 },
        Stop(u32),
        End(u32),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (1u32..4, any::<bool>(), 0u32..3).prop_map(|(seg, secondary, path)| Op::Play { seg, secondary, path }),
            (0u32..120).prop_map(Op::Stop),
            (100u32..120).prop_map(Op::End),
        ]
    }

    proptest! {
        /// Any sequence of plays, stops and ends: at most one primary state at a time, the
        /// newest primary; a stop of a segment, path or state leaves none of it; every state
        /// a play or stop takes out was one that was there; secondary states stay until
        /// stopped or ended.
        #[test]
        fn plays_keep_one_primary(ops in proptest::collection::vec(op(), 1..60)) {
            let mut p = Plays::default();
            let mut next = 100u32;
            let mut ended: Vec<u32> = Vec::new();
            for o in ops {
                let before: Vec<u32> = p.states.iter().map(|s| s.0).collect();
                match o {
                    Op::Play { seg, secondary, path } => {
                        let key = next;
                        next += 1;
                        let secs: Vec<u32> = p.states.iter().filter(|s| !s.1.primary).map(|s| s.0).collect();
                        let stopped = p.play(key, StateInfo { segment: seg, primary: !secondary, path: 200 + path, start: 0 });
                        prop_assert!(stopped.iter().all(|k| before.contains(k)));
                        if secondary { prop_assert!(stopped.is_empty()); }
                        for s in secs { prop_assert!(p.states.iter().any(|x| x.0 == s)); }
                        if !secondary {
                            let prim: Vec<u32> = p.states.iter().filter(|s| s.1.primary).map(|s| s.0).collect();
                            prop_assert_eq!(prim, vec![key]);
                        }
                    }
                    Op::Stop(obj) => {
                        let obj = match obj { 0..=3 => obj, 4..=6 => 200 + obj - 4, x => x };
                        let stopped = p.stop(obj);
                        prop_assert!(stopped.iter().all(|k| before.contains(k)));
                        prop_assert!(p.states.iter().all(|s| obj != 0 && s.0 != obj && s.1.segment != obj && s.1.path != obj));
                    }
                    Op::End(k) => {
                        ended.push(k);
                        let gone = p.reap(|x| !ended.contains(&x));
                        prop_assert!(gone.iter().all(|k| before.contains(k) && ended.contains(k)));
                    }
                }
                prop_assert!(p.states.iter().filter(|s| s.1.primary).count() <= 1);
                let mut keys: Vec<u32> = p.states.iter().map(|s| s.0).collect();
                keys.sort();
                keys.dedup();
                prop_assert_eq!(keys.len(), p.states.len());
            }
        }

        /// Volumes add and stay in DirectSound's range; louder in never gives quieter out.
        #[test]
        fn volumes_add_within_range(m in -20000i32..2000, a in -20000i32..2000, d in 0i32..500) {
            let v = total_volume(m, a);
            prop_assert!((-10000..=0).contains(&v));
            prop_assert!(total_volume(m, a.saturating_add(d)) >= v);
            prop_assert_eq!(total_volume(m, a), total_volume(a, m));
        }

        /// Any bytes: wave_pcm never panics, and what it gives lies inside the bytes.
        #[test]
        fn wave_pcm_stays_in_bounds(b in proptest::collection::vec(any::<u8>(), 0..200), head in any::<bool>()) {
            let mut b = b;
            if head && b.len() >= 12 { b[..12].copy_from_slice(b"RIFF\0\0\0\0WAVE"); }
            if let Some((off, size, ..)) = wave_pcm(&b) {
                prop_assert!(off as usize + size as usize <= b.len());
            }
        }

        /// References: n AddRefs and n + 1 Releases free once, at the last.
        #[test]
        fn refcount_frees_once(n in 0u32..500) {
            let mut refs = 1 + n;
            let mut frees = 0;
            for k in 0..=n {
                let (r, gone) = release_count(refs);
                refs = r;
                frees += gone as u32;
                prop_assert_eq!(gone, k == n);
            }
            prop_assert_eq!(frees, 1);
        }
    }

    /// The interfaces as 凌●学園 uses them: a performance (InitAudio with a standard path), a
    /// loader's segment of a wave in memory, repeats, PlaySegmentEx; a second primary segment
    /// replaces the first; a secondary one plays along; StopEx of a segment, then of all.
    #[test]
    fn performance_plays_wave_segments() {
        use crate::test_guest::{call, enter};
        let _g = enter();
        // (no mixer thread: the states stay playing while the test looks)
        unsafe { std::env::set_var("SAKE_NO_MIXER", "1") };
        let g = |v: &[u8; 16]| {
            let p = mem::heap_alloc(16, false);
            mem::write(p, v);
            p
        };
        let out = mem::heap_alloc(16, true);
        assert_eq!(create_instance(g(&CLSID_PERFORMANCE), 0, g(&PERF_IIDS[2]), out), Some(0));
        let perf = mem::r32(out);
        assert_eq!(create_instance(g(&CLSID_LOADER), 0, g(&LOADER_IIDS[1]), out), Some(0));
        let loader = mem::r32(out);
        // aggregation and other interfaces refused
        assert_eq!(create_instance(g(&CLSID_LOADER), 1, g(&LOADER_IIDS[1]), out), Some(CLASS_E_NOAGGREGATION));
        assert_eq!(create_instance(g(&CLSID_LOADER), 0, g(&PERF_IIDS[2]), out), Some(E_NOINTERFACE));
        assert_eq!(create_instance(g(&IID_UNKNOWN), 0, g(&PERF_IIDS[2]), out), None);

        // InitAudio(&dm, &ds, hwnd, DMUS_APATH_SHARED_STEREOPLUSREVERB, 64, DMUS_AUDIOF_ALL, NULL)
        assert_eq!(call(p_init_audio, &[perf, 0, 0, 0, 1, 64, 0x3f, 0]), 0);
        assert_eq!(call(p_get_default_path, &[perf, out]), 0);
        let path = mem::r32(out);
        assert_eq!(kind_of(path), Some(Kind::AudioPath));

        // GetObject(desc: CLASS | MEMORY) of two waves
        let seg = |frames: u32| {
            let b = wav(22050, 2, 16, frames);
            let data = mem::heap_alloc(b.len() as u32, false);
            mem::write(data, &b);
            let desc = mem::heap_alloc(848, true);
            mem::w32(desc, 848);
            mem::w32(desc + 4, 0x402);
            mem::write(desc + 24, &CLSID_SEGMENT);
            mem::w64(desc + 832, b.len() as u64);
            mem::w32(desc + 840, data);
            assert_eq!(call(l_get_object, &[loader, desc, g(&SEGMENT_IIDS[2]), out]), 0);
            mem::r32(out)
        };
        let (a, b) = (seg(22050), seg(11025));
        assert_eq!(call(seg_get_length, &[a, out]), 0);
        assert_eq!(mem::r32(out), 1536);
        assert_eq!(call(seg_set_repeats, &[a, REPEAT_INFINITE]), 0);
        assert_eq!(call(seg_get_repeats, &[a, out]), 0);
        assert_eq!(mem::r32(out), REPEAT_INFINITE);

        // PlaySegmentEx(a, NULL, NULL, 0, i64 0, &state, NULL, NULL) (x86: the __int64 in two slots)
        let st = mem::heap_alloc(8, true);
        assert_eq!(call(p_play_segment_ex, &[perf, a, 0, 0, 0, 0, 0, st, 0, 0]), 0);
        let sa = mem::r32(st);
        assert_eq!(kind_of(sa), Some(Kind::State));
        assert_eq!(call(p_is_playing, &[perf, a, 0]), 0);
        assert_eq!(call(p_is_playing, &[perf, 0, sa]), 0);
        assert_eq!(call(st_get_segment, &[sa, out]), 0);
        assert_eq!(mem::r32(out), a);
        // a secondary b plays along
        assert_eq!(call(p_play_segment_ex, &[perf, b, 0, 0, SEGF_SECONDARY, 0, 0, st, 0, 0]), 0);
        let sb = mem::r32(st);
        assert_eq!(call(p_is_playing, &[perf, a, 0]), 0);
        assert_eq!(call(p_is_playing, &[perf, b, 0]), 0);
        // a primary b replaces a, the secondary state stays
        assert_eq!(call(p_play_segment_ex, &[perf, b, 0, 0, 0, 0, 0, st, 0, 0]), 0);
        assert_eq!(call(p_is_playing, &[perf, a, 0]), S_FALSE);
        assert_eq!(call(p_is_playing, &[perf, 0, sb]), 0);
        // the path's volume and the master volume reach what plays
        assert_eq!(call(path_volume, &[path, (-600i32) as u32, 0]), 0);
        assert_eq!(call(path_volume, &[path, (-20000i32) as u32, 0]), E_INVALIDARG);
        let v = mem::heap_alloc(4, true);
        mem::w32(v, (-300i32) as u32);
        assert_eq!(call(p_set_global, &[perf, g(&GUID_PERF_MASTER_VOLUME), v, 4]), 0);
        mem::w32(v, 0);
        assert_eq!(call(p_get_global, &[perf, g(&GUID_PERF_MASTER_VOLUME), v, 4]), 0);
        assert_eq!(mem::r32(v) as i32, -300);
        // StopEx(b) stops both of b's states; StopEx(NULL) everything
        assert_eq!(call(p_stop_ex, &[perf, b, 0, 0, 0]), 0);
        assert_eq!(call(p_is_playing, &[perf, b, 0]), S_FALSE);
        assert_eq!(call(p_play_segment_ex, &[perf, a, 0, 0, 0, 0, 0, 0, 0, 0]), 0);
        assert_eq!(call(p_stop_ex, &[perf, 0, 0, 0, 0]), 0);
        assert_eq!(call(p_is_playing, &[perf, a, 0]), S_FALSE);
        assert_eq!(call(p_is_playing, &[perf, 0, 0]), E_POINTER);
        // a MIDI file is a silent segment, not an error
        let midi = b"MThd\0\0\0\x06\0\x01\0\x01\x01\xe0";
        let data = mem::heap_alloc(midi.len() as u32, false);
        mem::write(data, midi);
        let desc = mem::heap_alloc(848, true);
        mem::w32(desc, 848);
        mem::w32(desc + 4, 0x400);
        mem::w64(desc + 832, midi.len() as u64);
        mem::w32(desc + 840, data);
        assert_eq!(call(l_get_object, &[loader, desc, g(&SEGMENT_IIDS[2]), out]), 0);
        let m = mem::r32(out);
        assert_eq!(call(p_play_segment_ex, &[perf, m, 0, 0, 0, 0, 0, 0, 0, 0]), 0);
        assert_eq!(call(p_is_playing, &[perf, m, 0]), S_FALSE);
        // no file name, no memory: refused
        mem::w32(desc + 4, 2);
        mem::write(desc + 24, &CLSID_SEGMENT);
        assert_eq!(call(l_get_object, &[loader, desc, 0, out]), DMUS_E_LOADER_NOFILENAME);
        for o in [a, b, m, perf, loader] {
            call(release, &[o]);
        }
    }

    /// Clones and plays in any order (as engines that Clone a segment per voice line): a
    /// clone holds its original, whose samples stay exactly while it or any clone of it lives;
    /// a state lives while the performance plays it or the game holds it, and its object goes
    /// with the last of the two; a performance's released states leave nothing in the tables.
    mod lifetimes {
        use super::super::*;
        use super::wav;
        use crate::test_guest::{call, enter};
        use proptest::prelude::*;

        fn guid(v: &[u8; 16]) -> u32 {
            let p = mem::heap_alloc(16, false);
            mem::write(p, v);
            p
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]
            #[test]
            fn clones_and_states_as_a_model(ops in proptest::collection::vec((0u8..6, any::<prop::sample::Index>()), 0..50)) {
                let _g = enter();
                unsafe { std::env::set_var("SAKE_NO_MIXER", "1") };
                let out = mem::heap_alloc(16, true);
                prop_assert_eq!(create_instance(guid(&CLSID_PERFORMANCE), 0, guid(&PERF_IIDS[2]), out), Some(0));
                let perf = mem::r32(out);
                let orig = segment_of_bytes(&wav(22050, 1, 16, 400));
                let file = with(|w| w.segments[&orig].file);
                // (segment, its original or 0, the game's references)
                let mut segs: Vec<(u32, u32, u32)> = vec![(orig, 0, 1)];
                // (state, held by the game)
                let mut states: Vec<(u32, u32)> = vec![];
                let mut dead = vec![];
                for (op, k) in ops {
                    let live: Vec<usize> = (0..segs.len()).filter(|&i| segs[i].2 > 0).collect();
                    match op {
                        0 if !live.is_empty() => {
                            let (o, ..) = segs[live[k.index(live.len())]];
                            prop_assert_eq!(call(seg_clone, &[o, 0, 0, out]), 0);
                            segs.push((mem::r32(out), o, 1));
                        }
                        1 if !live.is_empty() => {
                            let i = live[k.index(live.len())];
                            segs[i].2 -= 1;
                            call(release, &[segs[i].0]);
                        }
                        2 | 3 if !live.is_empty() => {
                            let (o, ..) = segs[live[k.index(live.len())]];
                            let st = if op == 2 { out } else { 0 };
                            prop_assert_eq!(call(p_play_segment_ex, &[perf, o, 0, 0, SEGF_SECONDARY, 0, 0, st, 0, 0]), 0);
                            if op == 2 {
                                states.push((mem::r32(out), 1));
                            }
                        }
                        4 if !states.is_empty() => {
                            let i = k.index(states.len());
                            let (st, _) = states.swap_remove(i);
                            call(release, &[st]);
                            dead.push(st);
                        }
                        _ => {
                            call(p_stop_ex, &[perf, 0, 0, 0, 0]);
                        }
                    }
                    // a segment's object is there while the game, a clone or a playing state
                    // holds it; the original's samples while any segment of its family lives
                    let family_alive = segs.iter().any(|s| s.2 > 0);
                    prop_assert_eq!(mem::heap_owns(file), family_alive || with(|w| w.segments.contains_key(&orig)));
                    for &st in &dead {
                        // (a state the game let go: gone once the performance did too)
                        let playing = with(|w| w.perfs[&perf].plays.states.iter().any(|s| s.0 == st));
                        prop_assert_eq!(kind_of(st).is_some(), playing, "state {:#x}", st);
                    }
                    for &(st, _) in &states {
                        prop_assert_eq!(kind_of(st), Some(Kind::State));
                    }
                }
                call(p_stop_ex, &[perf, 0, 0, 0, 0]);
                for (o, _, n) in segs.iter().copied() {
                    for _ in 0..n {
                        call(release, &[o]);
                    }
                }
                for (st, _) in states {
                    call(release, &[st]);
                    prop_assert!(kind_of(st).is_none());
                }
                prop_assert!(!mem::heap_owns(file), "the wave outlived every segment");
                prop_assert!(segs.iter().all(|s| kind_of(s.0).is_none()));
                call(release, &[perf]);
                prop_assert!(kind_of(perf).is_none());
                prop_assert!(!with(|w| w.perfs.contains_key(&perf)));
                mem::heap_free(out);
            }
        }
    }
}
