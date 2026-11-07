//! XAPO effects (xaudio2_8 / xaudio2_9 CreateFX, CreateAudioVolumeMeter, CreateAudioReverb; the
//! xapofx1_x CreateFX; xaudio2_7's AudioVolumeMeter and AudioReverb classes): objects with
//! IXAPO and IXAPOParameters. The volume meter measures (peak and RMS per channel of each
//! Process); the others run fxdsp's EQ, limiter, echo and reverbs on the audio, whether a
//! voice's effect chain runs them (process_builtin, in the engine) or the guest calls Process.
//!
//! The formats an XAPO takes are 32-bit float PCM (WAVE_FORMAT_IEEE_FLOAT, or EXTENSIBLE with
//! the float subtype), the same for input and output.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::Mutex;

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, ps, putp, r16, r32, rp, w32, wp};

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_INVALIDARG: u32 = 0x8007_0057;
const CLASS_E_CLASSNOTAVAILABLE: u32 = 0x8004_0111;
const XAPO_E_FORMAT_UNSUPPORTED: u32 = 0x8897_0001;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Eq,
    Limiter,
    Reverb,
    Echo,
    VolumeMeter,
    AudioReverb,
}

struct Xapo {
    kind: Kind,
    clsid: [u8; 16],
    refs: u32,
    params: Vec<u8>,
    channels: u32,
    locked: bool,
    /// FXEcho's FXECHO_INITDATA.MaxDelay (ms; 0: the largest)
    max_delay: f32,
    peak: Vec<f32>,
    rms: Vec<f32>,
    rate: u32,
    dsp: Dsp,
}

/// The running state of an effect's DSP
#[derive(Default)]
enum Dsp {
    #[default]
    None,
    Eq(crate::fxdsp::Eq),
    Limiter(crate::fxdsp::Limiter),
    Echo(crate::fxdsp::Echo),
    Reverb(crate::fxdsp::Reverb),
}

impl Xapo {
    /// Runs the effect on `buf` (interleaved, `ch` channels at `rate`); the meter measures
    fn run(&mut self, buf: &mut [f32], ch: usize, rate: u32) {
        use crate::fxdsp::*;
        if matches!(self.dsp, Dsp::None) {
            self.dsp = match self.kind {
                Kind::Eq => Dsp::Eq(Eq::default()),
                Kind::Limiter => Dsp::Limiter(Limiter::default()),
                Kind::Echo => Dsp::Echo(Echo::with_max(self.max_delay)),
                Kind::Reverb | Kind::AudioReverb => Dsp::Reverb(Reverb::default()),
                Kind::VolumeMeter => Dsp::None,
            };
        }
        let p = &self.params;
        match &mut self.dsp {
            Dsp::Eq(e) => e.run(p, buf, ch, rate),
            Dsp::Limiter(l) => l.run(p, buf, ch, rate),
            Dsp::Echo(e) => e.run(p, buf, ch, rate),
            Dsp::Reverb(r) => {
                let shape = if self.kind == Kind::Reverb { ReverbShape::fx(p) } else { ReverbShape::audio(p) };
                r.run(&shape, buf, ch, rate)
            }
            Dsp::None => {
                let (pk, rms) = levels(buf, ch);
                self.peak = pk;
                self.rms = rms;
            }
        }
    }
}

/// Runs built-in effect `o` on a voice's audio (false: `o` is not one of sake's: the guest's
/// own XAPO). A disabled effect passes the audio through (a meter then reads silence).
pub fn process_builtin(o: u32, buf: &mut [f32], ch: usize, rate: u32, enabled: bool) -> bool {
    with(o, |x| {
        if enabled {
            x.run(buf, ch, rate);
        } else if x.kind == Kind::VolumeMeter {
            x.peak.iter_mut().for_each(|v| *v = 0.0);
            x.rms.iter_mut().for_each(|v| *v = 0.0);
        }
    })
    .is_some()
}

/// The object behind either interface of a built-in effect, if `p` is one
pub fn builtin(p: u32) -> Option<u32> {
    let o = obj(p);
    with(o, |_| o)
}

static XAPOS: Mutex<Option<HashMap<u32, Xapo>>> = Mutex::new(None);

fn with<R>(o: u32, f: impl FnOnce(&mut Xapo) -> R) -> Option<R> {
    XAPOS.lock().unwrap().get_or_insert_with(HashMap::new).get_mut(&o).map(f)
}

fn guid_bytes(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let mut g = [0u8; 16];
    g[..4].copy_from_slice(&d1.to_le_bytes());
    g[4..6].copy_from_slice(&d2.to_le_bytes());
    g[6..8].copy_from_slice(&d3.to_le_bytes());
    g[8..].copy_from_slice(&d4);
    g
}

/// The effect a CLSID names (FXEQ, FXMasteringLimiter, FXReverb, FXEcho, their 2.7 forms,
/// AudioVolumeMeter / AudioReverb of 2.0-2.7 and 2.8+).
pub fn kind_of(g: &[u8; 16]) -> Option<Kind> {
    let fx27 = |n: u8| guid_bytes(0xa90b_c001, 0xe897, 0xe897, [0x74, 0x39, 0x43, 0x55, 0, 0, 0, n]);
    let table: [([u8; 16], Kind); 8] = [
        (guid_bytes(0xf5e0_1117, 0xd6c4, 0x485a, [0xa3, 0xf5, 0x69, 0x51, 0x96, 0xf3, 0xdb, 0xfa]), Kind::Eq),
        (guid_bytes(0xc413_7916, 0x2be1, 0x46fd, [0x85, 0x99, 0x44, 0x15, 0x36, 0xf4, 0x98, 0x56]), Kind::Limiter),
        (guid_bytes(0x7d9a_ca56, 0xcb68, 0x4807, [0xb6, 0x32, 0xb1, 0x37, 0x35, 0x2e, 0x85, 0x96]), Kind::Reverb),
        (guid_bytes(0x5039_d740, 0xf736, 0x449a, [0x84, 0xd3, 0xa5, 0x62, 0x02, 0x55, 0x7b, 0x87]), Kind::Echo),
        (fx27(0), Kind::Eq),
        (fx27(1), Kind::Limiter),
        (fx27(2), Kind::Reverb),
        (fx27(3), Kind::Echo),
    ];
    if let Some(t) = table.iter().find(|t| &t.0 == g) {
        return Some(t.1);
    }
    let meters = [
        guid_bytes(0xc0c5_6f46, 0x29b1, 0x44e9, [0x99, 0x39, 0xa3, 0x2c, 0xe8, 0x68, 0x67, 0xe2]),
        guid_bytes(0xc1e3_f122, 0xa2ea, 0x442c, [0x85, 0x4f, 0x20, 0xd9, 0x8f, 0x83, 0x57, 0xa1]),
        guid_bytes(0xf5ca_7b34, 0x8055, 0x42c0, [0xb8, 0x36, 0x21, 0x61, 0x29, 0xeb, 0x7e, 0x30]),
        guid_bytes(0xe180_344b, 0xac83, 0x4483, [0x95, 0x9e, 0x18, 0xa5, 0xc5, 0x6a, 0x5e, 0x19]),
        guid_bytes(0xc733_8b95, 0x52b8, 0x4542, [0xaa, 0x79, 0x42, 0xeb, 0x01, 0x6c, 0x8c, 0x1c]),
        guid_bytes(0x2139_e6da, 0xc341, 0x4774, [0x9a, 0xc3, 0xb4, 0xe0, 0x26, 0x34, 0x7f, 0x64]),
        guid_bytes(0xe48c_5a3f, 0x93ef, 0x43bb, [0xa0, 0x92, 0x2c, 0x7c, 0xeb, 0x94, 0x6f, 0x27]),
        guid_bytes(0xcac1_105f, 0x619b, 0x4d04, [0x83, 0x1a, 0x44, 0xe1, 0xcb, 0xf1, 0x2d, 0x57]),
        // 2.8+ (xaudio2fx.h of the Windows SDK)
        guid_bytes(0x4fc3_b166, 0x972a, 0x40cf, [0xbc, 0x37, 0x7d, 0xb0, 0x3d, 0xb2, 0xfb, 0xa3]),
    ];
    let reverbs = [
        guid_bytes(0x6f6e_a3a9, 0x2cf5, 0x41cf, [0x91, 0xc1, 0x21, 0x70, 0xb1, 0x54, 0x00, 0x63]),
        guid_bytes(0xf476_9300, 0xb949, 0x4df9, [0xb3, 0x33, 0x00, 0xd3, 0x39, 0x32, 0xe9, 0xa6]),
        guid_bytes(0x629c_f0de, 0x3ecc, 0x41e7, [0x99, 0x26, 0xf7, 0xe4, 0x3e, 0xeb, 0xec, 0x51]),
        guid_bytes(0x9cab_402c, 0x1d37, 0x44b4, [0x88, 0x6d, 0xfa, 0x4f, 0x36, 0x17, 0x0a, 0x4c]),
        guid_bytes(0x8bb7_778b, 0x645b, 0x4475, [0x9a, 0x73, 0x1d, 0xe3, 0x17, 0x0b, 0xd3, 0xaf]),
        guid_bytes(0xd06d_f0d0, 0x8518, 0x441e, [0x82, 0x2f, 0x54, 0x51, 0xd5, 0xc5, 0x95, 0xb8]),
        guid_bytes(0xcece_c95a, 0xd894, 0x491a, [0xbe, 0xe3, 0x5e, 0x10, 0x6f, 0xb5, 0x9f, 0x2d]),
        guid_bytes(0x6a93_130e, 0x1d53, 0x41d1, [0xa9, 0xcf, 0xe7, 0x58, 0x80, 0x0b, 0xb1, 0x79]),
        guid_bytes(0xc263_3b16, 0x471b, 0x4498, [0xb8, 0xc5, 0x4f, 0x09, 0x59, 0xe2, 0xec, 0x09]),
    ];
    if meters.contains(g) {
        return Some(Kind::VolumeMeter);
    }
    reverbs.contains(g).then_some(Kind::AudioReverb)
}

/// Is `g` an IXAPO (2.8+ or 2.7), IXAPOParameters (either), or IUnknown: (is IUnknown or IXAPO,
/// is IXAPOParameters)
fn iface_of(g: &[u8; 16]) -> (bool, bool) {
    let unknown = guid_bytes(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    let xapo = guid_bytes(0xa410_b984, 0x9839, 0x4819, [0xa0, 0xbe, 0x28, 0x56, 0xae, 0x6b, 0x3a, 0xdb]);
    let params = guid_bytes(0x26d9_5c66, 0x80f2, 0x499a, [0xad, 0x54, 0x5a, 0xe7, 0xf0, 0x1c, 0x6d, 0x98]);
    let x27 = |n: u8| guid_bytes(0xa90b_c001, 0xe897, 0xe897, [0x55, 0xe4, 0x9e, 0x47, 0, 0, 0, n]);
    (*g == unknown || *g == xapo || *g == x27(0), *g == params || *g == x27(1))
}

fn read_guid(p: u32) -> [u8; 16] {
    mem::slice(p, 16).try_into().unwrap()
}

/// The object pointer from either interface's `this`
fn obj(this: u32) -> u32 {
    if with(this, |_| ()).is_some() {
        this
    } else {
        this - ps()
    }
}

fn QueryInterface(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let o = obj(this);
    let (x, p) = iface_of(&read_guid(iid));
    if !x && !p {
        putp(out, 0);
        return E_NOINTERFACE;
    }
    with(o, |x| x.refs += 1);
    putp(out, if x { o } else { o + ps() });
    0
}
fn AddRef(c: &mut Ctx) -> u32 {
    with(obj(c.a(0)), |x| {
        x.refs += 1;
        x.refs
    })
    .unwrap_or(1)
}
fn Release(c: &mut Ctx) -> u32 {
    let o = obj(c.a(0));
    let n = with(o, |x| {
        x.refs = x.refs.saturating_sub(1);
        x.refs
    })
    .unwrap_or(0);
    if n == 0 && XAPOS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&o).is_some() {
        mem::heap_free(o);
    }
    n
}

fn name(k: Kind) -> &'static str {
    match k {
        Kind::Eq => "FXEQ",
        Kind::Limiter => "FXMasteringLimiter",
        Kind::Reverb => "FXReverb",
        Kind::Echo => "FXEcho",
        Kind::VolumeMeter => "AudioVolumeMeter",
        Kind::AudioReverb => "AudioReverb",
    }
}

/// GetRegistrationProperties(ppRegistrationProperties): XAPO_REGISTRATION_PROPERTIES in
/// CoTaskMemAlloc memory
fn GetRegistrationProperties(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let Some((k, g)) = with(obj(this), |x| (x.kind, x.clsid)) else { return E_INVALIDARG };
    let p = mem::heap_alloc(1068, true);
    mem::write(p, &g);
    let n: Vec<u16> = name(k).encode_utf16().collect();
    mem::put_wstr(p + 16, 256, &n);
    // MajorVersion 1, MinorVersion 0; Flags: channels, frame rate, bits per sample, buffer
    // count must match (0xf) and in-place supported (0x10); one input and one output buffer
    for (i, v) in [1u32, 0, 0x1f, 1, 1, 1, 1].iter().enumerate() {
        w32(p + 1040 + 4 * i as u32, *v);
    }
    putp(out, p);
    0
}

/// A WAVEFORMATEX(TENSIBLE) of 32-bit float samples
fn is_float(f: u32) -> bool {
    if f == 0 {
        return false;
    }
    let tag = r16(f);
    let bits = r16(f + 14);
    let sub_float = tag == 0xfffe && r16(f + 16) >= 22 && r32(f + 24) == 3;
    bits == 32 && (tag == 3 || sub_float)
}

/// IsInputFormatSupported / IsOutputFormatSupported(pOtherFormat, pRequestedFormat,
/// ppSupportedFormat): float PCM is; for anything else the nearest (the request as float)
fn IsFormatSupported(c: &mut Ctx) -> u32 {
    let (want, out) = (c.a(2), c.a(3));
    if want == 0 {
        return E_POINTER;
    }
    if is_float(want) {
        if out != 0 {
            putp(out, 0);
        }
        return 0;
    }
    if out != 0 {
        let p = mem::heap_alloc(18, true);
        let (ch, rate) = (r16(want + 2).max(1) as u32, r32(want + 4));
        mem::w16(p, 3);
        mem::w16(p + 2, ch as u16);
        w32(p + 4, rate);
        w32(p + 8, rate * ch * 4);
        mem::w16(p + 12, (ch * 4) as u16);
        mem::w16(p + 14, 32);
        putp(out, p);
    }
    XAPO_E_FORMAT_UNSUPPORTED
}

/// Initialization data: FXEcho's is FXECHO_INITDATA { MaxDelay }; the others' their
/// initial parameters
fn init_data(x: &mut Xapo, data: Vec<u8>) {
    if data.is_empty() {
        return;
    }
    if x.kind == Kind::Echo {
        if data.len() >= 4 {
            x.max_delay = u32::from_le_bytes(data[..4].try_into().unwrap()) as f32;
        }
    } else {
        x.params = data;
    }
}

/// Initialize(pData, DataByteSize): the effect's initial parameters
fn Initialize(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a(2));
    let data = if p != 0 { mem::slice(p, n.min(4096)).to_vec() } else { Vec::new() };
    with(obj(this), |x| init_data(x, data));
    0
}
fn Reset(c: &mut Ctx) -> u32 {
    with(obj(c.a(0)), |x| {
        x.peak.iter_mut().for_each(|v| *v = 0.0);
        x.rms.iter_mut().for_each(|v| *v = 0.0);
        x.dsp = Dsp::None;
    });
    0
}
/// LockForProcess(nIn, pIn, nOut, pOut): XAPO_LOCKFORPROCESS_BUFFER_PARAMETERS { pFormat,
/// MaxFrameCount }; one input and one output, float, the same channel count
fn LockForProcess(c: &mut Ctx) -> u32 {
    let (this, nin, pin, nout, pout) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if nin != 1 || nout != 1 || pin == 0 || pout == 0 {
        return E_INVALIDARG;
    }
    let (fi, fo) = (rp(pin), rp(pout));
    if !is_float(fi) || !is_float(fo) || r16(fi + 2) != r16(fo + 2) {
        return XAPO_E_FORMAT_UNSUPPORTED;
    }
    let (ch, rate) = (r16(fi + 2) as u32, r32(fi + 4));
    with(obj(this), |x| {
        x.channels = ch;
        x.rate = rate.max(1);
        x.dsp = Dsp::None;
        x.locked = true;
        x.peak = vec![0.0; ch as usize];
        x.rms = vec![0.0; ch as usize];
    });
    0
}
fn UnlockForProcess(c: &mut Ctx) -> u32 {
    with(obj(c.a(0)), |x| x.locked = false);
    0
}

/// Per-channel peak and RMS of `n` interleaved frames of `ch` channels
pub fn levels(samples: &[f32], ch: usize) -> (Vec<f32>, Vec<f32>) {
    let mut peak = vec![0f32; ch];
    let mut sum = vec![0f64; ch];
    let frames = if ch == 0 { 0 } else { samples.len() / ch };
    for f in 0..frames {
        for i in 0..ch {
            let v = samples[f * ch + i];
            peak[i] = peak[i].max(v.abs());
            sum[i] += (v as f64) * (v as f64);
        }
    }
    let rms = sum.iter().map(|s| if frames == 0 { 0.0 } else { (s / frames as f64).sqrt() as f32 }).collect();
    (peak, rms)
}

/// Process(nIn, pIn, nOut, pOut, IsEnabled): XAPO_PROCESS_BUFFER_PARAMETERS { pBuffer,
/// BufferFlags, ValidFrameCount }; the effect runs on the input into the output (in place
/// or not); disabled, the input passes through. A silent input (XAPO_BUFFER_SILENT) is run as
/// zeros (a reverb's or echo's tail goes on)
fn Process(c: &mut Ctx) -> u32 {
    let (this, nin, pin, nout, pout, enabled) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5) != 0);
    if nin != 1 || nout != 1 || pin == 0 || pout == 0 {
        return 0;
    }
    let k = ps();
    let (ib, flags, frames) = (rp(pin), r32(pin + k), r32(pin + k + 4));
    let o = obj(this);
    let Some((ch, rate)) = with(o, |x| (x.channels, x.rate)) else { return 0 };
    let bytes = frames.saturating_mul(ch).saturating_mul(4);
    let ob = rp(pout);
    let mut s: Vec<f32> = if flags == 1 && ib != 0 {
        mem::slice(ib, bytes).chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
    } else {
        vec![0.0; (frames * ch) as usize]
    };
    process_builtin(o, &mut s, ch.max(1) as usize, rate, enabled);
    if ob != 0 {
        let v: Vec<u8> = s.iter().flat_map(|x| x.to_le_bytes()).collect();
        mem::write(ob, &v);
    }
    // (the output holds audio now)
    w32(pout + k, if ob != 0 { 1 } else { flags });
    w32(pout + k + 4, frames);
    0
}
fn CalcFrames(c: &mut Ctx) -> u32 {
    c.a(1)
}

/// IXAPOParameters::SetParameters(pParameters, ByteSize)
fn SetParameters(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return 0;
    }
    let v = mem::slice(p, n.min(4096)).to_vec();
    with(obj(this), |x| x.params = v);
    0
}
/// GetParameters(pParameters, ByteSize): the volume meter fills XAUDIO2FX_VOLUMEMETER_LEVELS
/// { pPeakLevels, pRMSLevels, ChannelCount } from the last Process; the others give back what
/// was set
fn GetParameters(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return 0;
    }
    let Some((kind, params, peak, rms)) = with(obj(this), |x| (x.kind, x.params.clone(), x.peak.clone(), x.rms.clone())) else { return 0 };
    if kind == Kind::VolumeMeter {
        let k = ps();
        let (pp, pr, cnt) = (rp(p), rp(p + k), r32(p + 2 * k));
        for i in 0..cnt.min(peak.len() as u32) {
            if pp != 0 {
                w32(pp + 4 * i, peak[i as usize].to_bits());
            }
            if pr != 0 {
                w32(pr + 4 * i, rms[i as usize].to_bits());
            }
        }
        return 0;
    }
    let mut v = params;
    v.resize(n.min(4096) as usize, 0);
    mem::write(p, &v);
    0
}

fn vtables() -> (u32, u32) {
    static VT: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    *VT.get_or_init(|| {
        let x: [(Handler, u32); 13] = [
            (QueryInterface, 12),
            (AddRef, 4),
            (Release, 4),
            (GetRegistrationProperties, 8),
            (IsFormatSupported, 16),
            (IsFormatSupported, 16),
            (Initialize, 12),
            (Reset, 4),
            (LockForProcess, 20),
            (UnlockForProcess, 4),
            (Process, 24),
            (CalcFrames, 8),
            (CalcFrames, 8),
        ];
        let p: [(Handler, u32); 5] = [(QueryInterface, 12), (AddRef, 4), (Release, 4), (SetParameters, 12), (GetParameters, 12)];
        (crate::api::vtable("IXAPO", &x), crate::api::vtable("IXAPOParameters", &p))
    })
}

/// A new XAPO of class `g` (refcount 1), or None for a class that is not an effect.
pub fn create(g: &[u8; 16]) -> Option<u32> {
    let kind = kind_of(g)?;
    let (vx, vp) = vtables();
    let k = ps();
    let o = mem::heap_alloc(2 * k, true);
    wp(o, vx);
    wp(o + k, vp);
    XAPOS.lock().unwrap().get_or_insert_with(HashMap::new).insert(o, Xapo { kind, clsid: *g, refs: 1, params: Vec::new(), channels: 0, locked: false, max_delay: 0.0, peak: Vec::new(), rms: Vec::new(), rate: 48000, dsp: Dsp::None });
    Some(o)
}

/// CoCreateInstance of xaudio2_7's AudioVolumeMeter / AudioReverb (the FX classes are not
/// registered: only CreateFX makes them)
pub fn create_instance(clsid: u32) -> Option<u32> {
    let g = read_guid(clsid);
    match kind_of(&g)? {
        Kind::VolumeMeter | Kind::AudioReverb => create(&g),
        _ => None,
    }
}

/// CreateFX(clsid, ppEffect[, pInitData, InitDataByteSize])
fn create_fx(c: &mut Ctx, init: bool) -> u32 {
    let (g, out) = (c.a(0), c.a(1));
    if g == 0 || out == 0 {
        return E_POINTER;
    }
    let Some(o) = create(&read_guid(g)) else {
        putp(out, 0);
        return CLASS_E_CLASSNOTAVAILABLE;
    };
    if init && c.a(2) != 0 {
        let v = mem::slice(c.a(2), c.a(3).min(4096)).to_vec();
        with(o, |x| init_data(x, v));
    }
    putp(out, o);
    0
}

pub fn init() {
    // (AudioVolumeMeter / AudioReverb of 2.8+ carry the SDK's CLSID_AudioVolumeMeter and
    // CLSID_AudioReverb in their registration properties)
    for dll in ["xaudio2_8", "xaudio2_9"] {
        implement(dll, "CreateFX", |c| create_fx(c, true));
        implement(dll, "CreateAudioVolumeMeter", |c| {
            let out = c.a(0);
            if out == 0 {
                return E_POINTER;
            }
            let g = guid_bytes(0x4fc3_b166, 0x972a, 0x40cf, [0xbc, 0x37, 0x7d, 0xb0, 0x3d, 0xb2, 0xfb, 0xa3]);
            putp(out, create(&g).unwrap());
            0
        });
        implement(dll, "CreateAudioReverb", |c| {
            let out = c.a(0);
            if out == 0 {
                return E_POINTER;
            }
            let g = guid_bytes(0xc263_3b16, 0x471b, 0x4498, [0xb8, 0xc5, 0x4f, 0x09, 0x59, 0xe2, 0xec, 0x09]);
            putp(out, create(&g).unwrap());
            0
        });
    }
    for dll in ["xapofx1_0", "xapofx1_1", "xapofx1_2", "xapofx1_3", "xapofx1_4", "xapofx1_5"] {
        implement(dll, "CreateFX", |c| create_fx(c, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        let g = guid_bytes(0x7d9a_ca56, 0xcb68, 0x4807, [0xb6, 0x32, 0xb1, 0x37, 0x35, 0x2e, 0x85, 0x96]);
        assert_eq!(kind_of(&g), Some(Kind::Reverb));
        let g27 = guid_bytes(0xa90b_c001, 0xe897, 0xe897, [0x74, 0x39, 0x43, 0x55, 0, 0, 0, 3]);
        assert_eq!(kind_of(&g27), Some(Kind::Echo));
        let avm27 = guid_bytes(0xcac1_105f, 0x619b, 0x4d04, [0x83, 0x1a, 0x44, 0xe1, 0xcb, 0xf1, 0x2d, 0x57]);
        assert_eq!(kind_of(&avm27), Some(Kind::VolumeMeter));
        assert_eq!(kind_of(&[0; 16]), None);
        assert_eq!(iface_of(&guid_bytes(0xa90b_c001, 0xe897, 0xe897, [0x55, 0xe4, 0x9e, 0x47, 0, 0, 0, 1])), (false, true));
    }

    #[test]
    fn meter_levels() {
        let (p, r) = levels(&[0.5, -1.0, -0.5, 0.0], 2);
        assert_eq!(p, vec![0.5, 1.0]);
        assert_eq!(r, vec![0.5, (0.5f64).sqrt() as f32]);
        let (p, r) = levels(&[], 2);
        assert_eq!((p, r), (vec![0.0, 0.0], vec![0.0, 0.0]));
    }
}
