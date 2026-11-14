//! Media sources: where a reader's bytes come from (`Data`: memory, a host file, or the game's
//! own IMFByteStream, read as samples are wanted), what the file holds (`Media`: an MP4's tracks,
//! or one sound), and the objects games see of it: the source resolver
//! (MFCreateSourceResolver: CreateObjectFromURL / FromByteStream and their Begin/End), the media
//! source, its presentation and stream descriptors and media type handlers. Readers made from
//! such a source (MFCreateSourceReaderFromMediaSource) read its streams (reader.rs).
//!
//! Unity's VideoPlayer opens its movies so: its byte stream over a part of a .resource file,
//! the resolver's CreateObjectFromByteStream, then a reader of the source.

use super::*;
use crate::audiofile;
use std::sync::Arc;

use super::mp4;

const MF_E_NO_EVENTS_AVAILABLE: u32 = 0xc00d_3e80;
const MF_E_NO_MORE_TYPES_: u32 = 0xc00d_36b9;
const MF_E_INVALIDMEDIATYPE_: u32 = 0xc00d_36b4;

/// Where a file's bytes are.
pub enum Src {
    /// (tests)
    #[cfg_attr(not(test), allow(dead_code))]
    Mem(Arc<Vec<u8>>),
    Host(Mutex<kura::fs::File>),
    /// the program's IMFByteStream (a reference held by whoever holds the Data: `release_guest`)
    Guest(u32),
}

pub struct Data {
    pub src: Src,
    pub len: u64,
    /// one read at a time of a guest stream (its position)
    lock: Mutex<()>,
}

impl Data {
    #[cfg(test)]
    pub fn mem_arc(v: Arc<Vec<u8>>) -> Data {
        Data { len: v.len() as u64, src: Src::Mem(v), lock: Mutex::new(()) }
    }
    pub fn host(f: kura::fs::File) -> Option<Data> {
        let len = f.metadata().ok()?.len();
        Some(Data { src: Src::Host(Mutex::new(f)), len, lock: Mutex::new(()) })
    }
    /// The program's byte stream `bs` (a reference taken): its length now.
    pub fn guest(c: &mut Ctx, bs: u32) -> Option<Data> {
        let tmp = mem::heap_alloc(8, true);
        let hr = com(c, bs, 4, &[tmp]);
        let len = mem::r64(tmp);
        mem::heap_free(tmp);
        if hr & 0x8000_0000 != 0 {
            return None;
        }
        add_ref(c, bs);
        Some(Data { src: Src::Guest(bs), len, lock: Mutex::new(()) })
    }
    pub fn guest_stream(&self) -> u32 {
        match self.src {
            Src::Guest(b) => b,
            _ => 0,
        }
    }
    /// `n` bytes at `off` (fewer at the end); None when they could not be read (a guest
    /// stream without a guest thread to call it on, `c` None).
    pub fn read(&self, c: Option<&mut Ctx>, off: u64, n: usize) -> Option<Vec<u8>> {
        let n = n.min(self.len.saturating_sub(off) as usize);
        match &self.src {
            Src::Mem(v) => v.get(off as usize..off as usize + n).map(|b| b.to_vec()),
            Src::Host(f) => {
                let mut f = f.lock().unwrap_or_else(|e| e.into_inner());
                let mut b = vec![0; n];
                kura::video::read_at(&mut f, off, &mut b).then_some(b)
            }
            Src::Guest(bs) => {
                let c = c?;
                let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
                guest_read(c, *bs, off, n)
            }
        }
    }
}

/// Reads `n` bytes at `off` of the program's byte stream: SetCurrentPosition, then Read until
/// they are all there (or it reads nothing more).
fn guest_read(c: &mut Ctx, bs: u32, off: u64, n: usize) -> Option<Vec<u8>> {
    if mem::x64() {
        // (one 64-bit argument)
        let f = rp(rp(bs) + ps() * 7);
        crate::call64(c.cpu, mem::widen(f), &[mem::widen(bs), off]);
    } else {
        com(c, bs, 7, &[off as u32, (off >> 32) as u32]);
    }
    const PIECE: usize = 1 << 20;
    let b = mem::heap_alloc(PIECE.min(n.max(1)) as u32, false);
    let got_at = mem::heap_alloc(4, true);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let want = (n - out.len()).min(PIECE) as u32;
        w32(got_at, 0);
        let hr = com(c, bs, 9, &[b, want, got_at]);
        let got = r32(got_at).min(want);
        if hr & 0x8000_0000 != 0 || got == 0 {
            break;
        }
        out.extend_from_slice(mem::slice(b, got));
    }
    mem::heap_free(b);
    mem::heap_free(got_at);
    (out.len() == n).then_some(out)
}

/// What a file holds: its streams in the file's order.
pub struct Media {
    pub tracks: Vec<mp4::Track>,
    pub duration: Option<u64>,
    /// "video/mp4", "audio/mpeg", ... (MF_PD_MIME_TYPE)
    pub mime: &'static str,
}

/// Reads what the file in `d` holds; None when no source of ours opens it.
pub fn open_media(c: Option<&mut Ctx>, d: &Data) -> Option<Media> {
    let mut c = c;
    let head = d.read(c.as_deref_mut(), 0, 12)?;
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        let m = mp4::open(d.len, &mut |o, n| d.read(c.as_deref_mut(), o, n))?;
        return Some(Media { tracks: m.tracks, duration: m.duration, mime: "video/mp4" });
    }
    // a sound file: read whole (they are small), as audiofile reads them
    if d.len > 1 << 30 {
        return None;
    }
    let all = d.read(c, 0, d.len as usize)?;
    let t = audiofile::open(&all)?;
    let mime = if head.starts_with(b"RIFF") { "audio/wav" } else { "audio/mpeg" };
    Some(Media { duration: t.duration, tracks: vec![mp4::Track::Audio(t)], mime })
}

// --- media types of video -----------------------------------------------------------------

pub fn k_frame_size() -> Guid {
    g("1652c33d-d6b2-4012-b834-72030849a37d")
}
pub fn k_frame_rate() -> Guid {
    g("c459a2e8-3d2c-4e44-b132-fee5156c7bb0")
}
pub fn k_aspect() -> Guid {
    g("c6376a1e-8d0a-4027-be45-6d9a0ad39bb6")
}
pub fn k_interlace() -> Guid {
    g("e2724bb8-e676-4806-b4b2-a8d6efb44ccd")
}
pub fn k_stride() -> Guid {
    g("644b4e48-1e02-4516-b0eb-c01ca9d49ac6")
}
pub fn k_fixed_size() -> Guid {
    g("b8ebefaf-b718-4e04-b0a9-116775e3321b")
}
pub fn k_sample_size() -> Guid {
    g("dad3ab78-1990-408b-bce2-eba673dacc10")
}
pub fn k_profile() -> Guid {
    g("ad76a80b-2d5c-4e0b-b375-64e520137036")
}
pub fn k_level() -> Guid {
    g("96f66574-11c5-4015-8666-bff516436da7")
}
pub fn k_seq_header() -> Guid {
    g("3c036de7-3ad0-4c9e-9216-ee6d6ac21cb3")
}
pub fn k_bitrate() -> Guid {
    g("20332624-fb0d-4d9e-bd0d-cbf6786c102e")
}
pub fn k_matrix() -> Guid {
    g("3e23d450-2c75-4d25-a00e-b91670d12327")
}
pub fn k_nominal_range() -> Guid {
    g("c21b8ee5-b956-4071-8daf-325edf5cab11")
}
fn k_pd_duration() -> Guid {
    g("6c990d33-bb8e-477a-8598-0d5d96fcd88a")
}
fn k_pd_mime() -> Guid {
    g("6c990d37-bb8e-477a-8598-0d5d96fcd88a")
}
pub fn major_video() -> Guid {
    g("73646976-0000-0010-8000-00aa00389b71")
}
/// MFVideoFormat_* by FOURCC (or D3DFORMAT) in Data1
pub fn video_format(fourcc: u32) -> Guid {
    let mut b = g("00000000-0000-0010-8000-00aa00389b71");
    b[0..4].copy_from_slice(&fourcc.to_le_bytes());
    b
}
pub const H264: u32 = u32::from_le_bytes(*b"H264");
pub const NV12: u32 = u32::from_le_bytes(*b"NV12");
pub const I420: u32 = u32::from_le_bytes(*b"I420");
pub const IYUV: u32 = u32::from_le_bytes(*b"IYUV");
pub const YV12: u32 = u32::from_le_bytes(*b"YV12");
pub const YUY2: u32 = u32::from_le_bytes(*b"YUY2");
pub const RGB32: u32 = 22;
pub const ARGB32: u32 = 21;

fn g(s: &str) -> Guid {
    crate::actctx::parse_guid(s).unwrap()
}
fn ratio(a: u32, b: u32) -> Val {
    Val::U64((a as u64) << 32 | b as u64)
}

/// The native type of an H.264 track (Windows' MP4 source: H264, size, rate, aspect,
/// progressive, profile and level, the sequence header, bit rate).
pub fn video_native(v: &mp4::Video) -> Store {
    let mut s = Store::default();
    s.set(reader::k_major(), Val::Guid(major_video()));
    s.set(reader::k_subtype(), Val::Guid(video_format(H264)));
    s.set(k_frame_size(), ratio(v.w, v.h));
    s.set(k_frame_rate(), ratio(v.rate.0, v.rate.1));
    s.set(k_aspect(), ratio(1, 1));
    s.set(k_interlace(), Val::U32(2));
    s.set(k_profile(), Val::U32(v.profile));
    s.set(k_level(), Val::U32(v.level));
    s.set(k_seq_header(), Val::Blob(v.head.clone()));
    s.set(k_bitrate(), Val::U32(v.bitrate));
    s
}
/// The decoded types a video track's reader gives (the decoder's outputs, then the video
/// processor's RGB): FOURCC, bits per pixel.
pub const DECODED: [u32; 7] = [NV12, YV12, IYUV, I420, YUY2, RGB32, ARGB32];
/// The type of decoded frames of `sub` of a `w` x `h` track.
pub fn video_decoded(v: &mp4::Video, sub: u32) -> Option<Store> {
    let stride = plat::stride_for(sub, v.w)?.unsigned_abs();
    let size = plat::image_size(sub, v.w, v.h)?;
    let mut s = Store::default();
    s.set(reader::k_major(), Val::Guid(major_video()));
    s.set(reader::k_subtype(), Val::Guid(video_format(sub)));
    s.set(k_frame_size(), ratio(v.w, v.h));
    s.set(k_frame_rate(), ratio(v.rate.0, v.rate.1));
    s.set(k_aspect(), ratio(1, 1));
    s.set(k_interlace(), Val::U32(2));
    s.set(reader::k_independent(), Val::U32(1));
    s.set(k_fixed_size(), Val::U32(1));
    s.set(k_sample_size(), Val::U32(size));
    // (top-down: what the decoder and the video processor give)
    s.set(k_stride(), Val::U32(stride));
    if !matches!(sub, RGB32 | ARGB32) {
        // MFVideoTransferMatrix_BT709 1 (HD) / BT601 2, MFNominalRange_16_235 2
        s.set(k_matrix(), Val::U32(if v.h >= 720 { 1 } else { 2 }));
        s.set(k_nominal_range(), Val::U32(2));
    }
    Some(s)
}

/// The type a track's descriptor offers (its native one).
pub fn native_of(t: &mp4::Track) -> Store {
    match t {
        mp4::Track::Video(v) => video_native(v),
        mp4::Track::Audio(a) => reader::native_type(a),
    }
}

// --- the objects ----------------------------------------------------------------------------

/// A media source of ours: the file's bytes and what they hold; whether it was shut down.
pub struct Source {
    refs: u32,
    pub data: Arc<Data>,
    pub media: Arc<Media>,
    shut: bool,
}
static SOURCES: Mutex<Option<HashMap<u32, Source>>> = Mutex::new(None);
fn source<R>(b: u32, f: impl FnOnce(&mut Source) -> R) -> Option<R> {
    SOURCES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
/// The bytes and media of source `b`, when it is one of ours.
pub fn source_of(b: u32) -> Option<(Arc<Data>, Arc<Media>)> {
    source(b, |s| (s.shut == false).then(|| (s.data.clone(), s.media.clone()))).flatten()
}

fn ms_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFMediaSource {279a808d-aec7-40c8-9c6b-a6b492c78a66}, IMFMediaEventGenerator
    // {2cd0bd52-bcd5-4b89-b62c-eadc0c031e7d}
    if is_unknown(&g) || is(&g, 0x279a_808d, 0xaec7, 0x40c8) || is(&g, 0x2cd0_bd52, 0xbcd5, 0x4b89) {
        source(this, |s| s.refs += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn ms_add_ref(c: &mut Ctx) -> u32 {
    source(c.a(0), |s| {
        s.refs += 1;
        s.refs
    })
    .unwrap_or(1)
}
fn ms_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = source(b, |s| {
        s.refs = s.refs.saturating_sub(1);
        s.refs
    })
    .unwrap_or(0);
    if n == 0 {
        let gone = SOURCES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
        if let Some(s) = gone {
            release_data(c, s.data);
            mem::heap_free(b);
        }
    }
    n
}
/// Drops a holder's reference of `d`: the guest stream's when it was the last.
pub fn release_data(c: &mut Ctx, d: Arc<Data>) {
    if let Ok(d) = Arc::try_unwrap(d) {
        release(c, d.guest_stream());
    }
}
fn ms_shut(c: &mut Ctx) -> u32 {
    source(c.a(0), |s| s.shut = true);
    0
}
fn ms_live(c: &Ctx) -> bool {
    source(c.a(0), |s| !s.shut).unwrap_or(false)
}
/// GetEvent / BeginGetEvent / EndGetEvent / QueueEvent: no events (nothing runs a source of
/// ours but a reader)
fn ms_event(c: &mut Ctx) -> u32 {
    if !ms_live(c) {
        return MF_E_SHUTDOWN;
    }
    MF_E_NO_EVENTS_AVAILABLE
}
fn ms_characteristics(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    if !ms_live(c) {
        return MF_E_SHUTDOWN;
    }
    // MFMEDIASOURCE_CAN_SEEK | CAN_PAUSE
    w32(out, 6);
    0
}
fn ms_pd(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let Some((_, m)) = source_of(c.a(0)) else { return MF_E_SHUTDOWN };
    putp(out, new_pd(&m, None));
    0
}
fn ms_start(c: &mut Ctx) -> u32 {
    if !ms_live(c) {
        return MF_E_SHUTDOWN;
    }
    0
}

fn source_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        vtable(
            "IMFMediaSource",
            &[
                (ms_qi as Handler, 12),
                (ms_add_ref, 4),
                (ms_release, 4),
                (ms_event, 12),
                (ms_event, 12),
                (ms_event, 12),
                (ms_event, 20),
                (ms_characteristics, 8),
                (ms_pd, 8),
                (ms_start, 16),
                (ms_start, 4), // Stop
                (ms_start, 4), // Pause
                (ms_shut, 4),
            ],
        )
    })
}
pub fn new_source(data: Arc<Data>, media: Media) -> u32 {
    let b = object(source_vtable(), 1);
    SOURCES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Source { refs: 1, data, media: Arc::new(media), shut: false });
    b
}

/// Objects that are attribute stores with more methods (descriptors), and what those methods
/// read: the media, the stream's index, and per stream whether it is selected.
#[derive(Clone)]
struct Side {
    media: Arc<Media>,
    stream: usize,
    selected: Arc<Mutex<Vec<bool>>>,
}
static SIDES: Mutex<Option<HashMap<u32, Side>>> = Mutex::new(None);
fn side(b: u32) -> Option<Side> {
    SIDES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&b).cloned()
}
fn new_attr_object(vt: u32, s: Store, sd: Side) -> u32 {
    let b = object(vt, 1);
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, s));
    SIDES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, sd);
    b
}

/// The streams a reader or a presentation selects at first: the first of each kind (Windows'
/// MP4 source).
pub fn first_selected(m: &Media) -> Vec<bool> {
    let (mut v, mut a) = (false, false);
    m.tracks
        .iter()
        .map(|t| match t {
            mp4::Track::Video(_) => !std::mem::replace(&mut v, true),
            mp4::Track::Audio(_) => !std::mem::replace(&mut a, true),
        })
        .collect()
}

fn pd_count(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    w32(out, side(c.a(0)).map_or(0, |s| s.media.tracks.len() as u32));
    0
}
/// GetStreamDescriptorByIndex(DWORD, BOOL *selected, IMFStreamDescriptor **)
fn pd_by_index(c: &mut Ctx) -> u32 {
    let (i, sel, out) = (c.a(1) as usize, c.a(2), c.a(3));
    if sel == 0 || out == 0 {
        return E_POINTER;
    }
    let Some(s) = side(c.a(0)) else { return E_INVALIDARG };
    if i >= s.media.tracks.len() {
        return E_INVALIDARG;
    }
    w32(sel, s.selected.lock().unwrap_or_else(|e| e.into_inner())[i] as u32);
    putp(out, new_sd(&s.media, i));
    0
}
fn pd_select(c: &mut Ctx, on: bool) -> u32 {
    let i = c.a(1) as usize;
    let Some(s) = side(c.a(0)) else { return E_INVALIDARG };
    let mut v = s.selected.lock().unwrap_or_else(|e| e.into_inner());
    match v.get_mut(i) {
        Some(x) => {
            *x = on;
            0
        }
        None => E_INVALIDARG,
    }
}
fn pd_clone(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let Some(s) = side(c.a(0)) else { return E_INVALIDARG };
    let sel = s.selected.lock().unwrap_or_else(|e| e.into_inner()).clone();
    putp(out, new_pd(&s.media, Some(sel)));
    0
}
fn pd_qi(c: &mut Ctx) -> u32 {
    // IMFPresentationDescriptor {03cb2711-24d7-4db6-a17f-f3a7a479a536}
    reader::typed_qi(c, 0x03cb_2711, 0x24d7, 0x4db6)
}
fn pd_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (pd_qi, 12);
        m.extend([(pd_count as Handler, 8), (pd_by_index, 16), (|c: &mut Ctx| pd_select(c, true), 8), (|c: &mut Ctx| pd_select(c, false), 8), (pd_clone, 8)]);
        vtable("IMFPresentationDescriptor", &m)
    })
}
fn new_pd(m: &Arc<Media>, sel: Option<Vec<bool>>) -> u32 {
    let mut s = Store::default();
    if let Some(d) = m.duration {
        s.set(k_pd_duration(), Val::U64(d));
    }
    s.set(k_pd_mime(), Val::Str(m.mime.encode_utf16().collect()));
    let selected = Arc::new(Mutex::new(sel.unwrap_or_else(|| first_selected(m))));
    new_attr_object(pd_vtable(), s, Side { media: m.clone(), stream: 0, selected })
}

fn sd_qi(c: &mut Ctx) -> u32 {
    // IMFStreamDescriptor {56c03d9c-9dbb-45f5-ab4b-d80f47c05938}
    reader::typed_qi(c, 0x56c0_3d9c, 0x9dbb, 0x45f5)
}
fn sd_id(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    // (the MP4 source names a stream by its track: 1, 2, ...)
    w32(out, side(c.a(0)).map_or(0, |s| s.stream as u32 + 1));
    0
}
fn sd_handler(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let Some(s) = side(c.a(0)) else { return E_INVALIDARG };
    putp(out, new_handler(&s.media, s.stream));
    0
}
fn sd_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (sd_qi, 12);
        m.extend([(sd_id as Handler, 8), (sd_handler, 8)]);
        vtable("IMFStreamDescriptor", &m)
    })
}
fn new_sd(m: &Arc<Media>, i: usize) -> u32 {
    new_attr_object(sd_vtable(), Store::default(), Side { media: m.clone(), stream: i, selected: Arc::new(Mutex::new(Vec::new())) })
}

// IMFMediaTypeHandler: a block (vtable, then the current type), the stream by SIDES

fn mh_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFMediaTypeHandler {e93dcf6c-4b07-4e1e-8123-aa16ed6eadf5}
    if is_unknown(&g) || is(&g, 0xe93d_cf6c, 0x4b07, 0x4e1e) {
        add_ref(c, this);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
static HANDLER_REFS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
fn mh_add_ref(c: &mut Ctx) -> u32 {
    let mut g = HANDLER_REFS.lock().unwrap_or_else(|e| e.into_inner());
    let n = g.get_or_insert_with(HashMap::new).entry(c.a(0)).or_insert(1);
    *n += 1;
    *n
}
fn mh_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = {
        let mut g = HANDLER_REFS.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        let n = m.get(&b).copied().unwrap_or(1).saturating_sub(1);
        if n == 0 {
            m.remove(&b);
        } else {
            m.insert(b, n);
        }
        n
    };
    if n == 0 {
        SIDES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
        release(c, rp(b + ps()));
        mem::heap_free(b);
    }
    n
}
/// IsMediaTypeSupported(IMFMediaType *, IMFMediaType **closest): the native type's major type
/// and subtype
fn mh_supported(c: &mut Ctx) -> u32 {
    let (t, closest) = (c.a(1), c.a(2));
    if closest != 0 {
        putp(closest, 0);
    }
    let (Some(s), Some(want)) = (side(c.a(0)), store_of(t)) else { return E_POINTER };
    let native = native_of(&s.media.tracks[s.stream]);
    if reader::equal_flags(&native, &want) & 3 == 3 {
        0
    } else {
        MF_E_INVALIDMEDIATYPE_
    }
}
fn mh_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 1);
    0
}
fn mh_by_index(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if i != 0 {
        return MF_E_NO_MORE_TYPES_;
    }
    let Some(s) = side(c.a(0)) else { return E_INVALIDARG };
    putp(out, reader::new_type(native_of(&s.media.tracks[s.stream])));
    0
}
fn mh_set_current(c: &mut Ctx) -> u32 {
    let t = c.a(1);
    if t == 0 {
        return E_POINTER;
    }
    if mh_supported(c) != 0 {
        return MF_E_INVALIDMEDIATYPE_;
    }
    add_ref(c, t);
    let old = rp(c.a(0) + ps());
    wp(c.a(0) + ps(), t);
    release(c, old);
    0
}
fn mh_current(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let cur = rp(c.a(0) + ps());
    if cur != 0 {
        add_ref(c, cur);
        putp(out, cur);
        return 0;
    }
    mh_by_index_0(c, out)
}
fn mh_by_index_0(c: &mut Ctx, out: u32) -> u32 {
    let Some(s) = side(c.a(0)) else { return E_INVALIDARG };
    putp(out, reader::new_type(native_of(&s.media.tracks[s.stream])));
    0
}
fn mh_major(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let Some(s) = side(c.a(0)) else { return E_INVALIDARG };
    let g = match &s.media.tracks[s.stream] {
        mp4::Track::Video(_) => major_video(),
        mp4::Track::Audio(_) => reader::major_audio(),
    };
    mem::write(out, &g);
    0
}
fn handler_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        vtable(
            "IMFMediaTypeHandler",
            &[(mh_qi as Handler, 12), (mh_add_ref, 4), (mh_release, 4), (mh_supported, 12), (mh_count, 8), (mh_by_index, 12), (mh_set_current, 8), (mh_current, 8), (mh_major, 8)],
        )
    })
}
fn new_handler(m: &Arc<Media>, i: usize) -> u32 {
    let b = object(handler_vtable(), 2);
    SIDES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Side { media: m.clone(), stream: i, selected: Arc::new(Mutex::new(Vec::new())) });
    b
}

// --- the source resolver --------------------------------------------------------------------

const MF_RESOLUTION_MEDIASOURCE: u32 = 1;
const MF_RESOLUTION_BYTESTREAM: u32 = 2;

/// A source of ours on `data`, or the HRESULT why none.
fn resolve(c: &mut Ctx, data: Data) -> Result<u32, u32> {
    let Some(media) = open_media(Some(c), &data) else {
        release(c, data.guest_stream());
        return Err(MF_E_UNSUPPORTED_BYTESTREAM_TYPE);
    };
    Ok(new_source(Arc::new(data), media))
}
/// The host file of a URL: a byte stream (flag BYTESTREAM) or a source on it
fn from_url(c: &mut Ctx, url: u32, flags: u32) -> Result<(u32, u32), u32> {
    if url == 0 {
        return Err(E_POINTER);
    }
    let path = reader::file_url_path(&String::from_utf16_lossy(&mem::wstr(url)));
    let missing = || 0x8007_0000 | crate::files::missing(&path);
    let host = crate::files::resolve(&path).ok_or_else(missing)?;
    let f = kura::fs::File::open(&host).map_err(|_| missing())?;
    if flags & MF_RESOLUTION_BYTESTREAM != 0 && flags & MF_RESOLUTION_MEDIASOURCE == 0 {
        return Ok((1, plat::new_file_stream(f, 1 | 4)));
    }
    let d = Data::host(f).ok_or(E_FAIL)?;
    Ok((0, resolve(c, d)?))
}
fn from_stream(c: &mut Ctx, bs: u32, flags: u32) -> Result<(u32, u32), u32> {
    if bs == 0 {
        return Err(E_POINTER);
    }
    if flags & MF_RESOLUTION_MEDIASOURCE == 0 {
        return Err(E_INVALIDARG);
    }
    let d = Data::guest(c, bs).ok_or(E_FAIL)?;
    Ok((0, resolve(c, d)?))
}
fn put_object(r: Result<(u32, u32), u32>, ty: u32, out: u32) -> u32 {
    match r {
        Ok((t, o)) => {
            if ty != 0 {
                w32(ty, t);
            }
            putp(out, o);
            0
        }
        Err(e) => {
            putp(out, 0);
            e
        }
    }
}
/// CreateObjectFromURL(LPCWSTR, DWORD flags, IPropertyStore *, MF_OBJECT_TYPE *, IUnknown **)
fn sv_from_url(c: &mut Ctx) -> u32 {
    let (url, flags, ty, out) = (c.a(1), c.a(2), c.a(4), c.a(5));
    if out == 0 {
        return E_POINTER;
    }
    let r = from_url(c, url, flags);
    put_object(r, ty, out)
}
/// CreateObjectFromByteStream(IMFByteStream *, LPCWSTR, DWORD flags, IPropertyStore *,
/// MF_OBJECT_TYPE *, IUnknown **)
fn sv_from_stream(c: &mut Ctx) -> u32 {
    let (bs, flags, ty, out) = (c.a(1), c.a(3), c.a(5), c.a(6));
    if out == 0 {
        return E_POINTER;
    }
    let r = from_stream(c, bs, flags);
    put_object(r, ty, out)
}
/// The object and type a Begin* made, by its result
static MADE: Mutex<Option<HashMap<u32, (u32, u32)>>> = Mutex::new(None);
fn begin(c: &mut Ctx, r: Result<(u32, u32), u32>, cancel: u32, cb: u32, state: u32) -> u32 {
    if cb == 0 {
        if let Ok((_, o)) = r {
            release(c, o);
        }
        return E_INVALIDARG;
    }
    if cancel != 0 {
        putp(cancel, 0);
    }
    let res = plat::new_result(c, 0, cb, state);
    match r {
        Ok((t, o)) => {
            MADE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(res, (t, o));
            plat::set_done(res, 0, 0);
        }
        Err(e) => plat::set_done(res, 0, e),
    }
    plat::invoke(c, res);
    release(c, res);
    0
}
/// BeginCreateObjectFromURL(LPCWSTR, DWORD, IPropertyStore *, IUnknown **cancel, callback, state)
fn sv_begin_url(c: &mut Ctx) -> u32 {
    let (url, flags, cancel, cb, st) = (c.a(1), c.a(2), c.a(4), c.a(5), c.a(6));
    let r = from_url(c, url, flags);
    begin(c, r, cancel, cb, st)
}
/// BeginCreateObjectFromByteStream(IMFByteStream *, LPCWSTR, DWORD, IPropertyStore *,
/// IUnknown **cancel, callback, state)
fn sv_begin_stream(c: &mut Ctx) -> u32 {
    let (bs, flags, cancel, cb, st) = (c.a(1), c.a(3), c.a(5), c.a(6), c.a(7));
    let r = from_stream(c, bs, flags);
    begin(c, r, cancel, cb, st)
}
/// End*(IMFAsyncResult *, MF_OBJECT_TYPE *, IUnknown **)
fn sv_end(c: &mut Ctx) -> u32 {
    let (res, ty, out) = (c.a(1), c.a(2), c.a(3));
    if res == 0 || out == 0 {
        return E_POINTER;
    }
    let made = MADE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&res);
    let hr = plat::done_of(res).map_or(E_INVALIDARG, |d| d.1);
    match made {
        Some((t, o)) => put_object(Ok((t, o)), ty, out),
        None => put_object(Err(if hr == 0 { E_INVALIDARG } else { hr }), ty, out),
    }
}

fn resolver_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFSourceResolver {fbe5a32d-a497-4b61-bb85-97b1a848a6e3}
    if is_unknown(&g) || is(&g, 0xfbe5_a32d, 0xa497, 0x4b61) {
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}

/// MFCreateSourceResolver(IMFSourceResolver **): one resolver (it holds nothing)
fn MFCreateSourceResolver(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return E_POINTER;
    }
    static R: OnceLock<u32> = OnceLock::new();
    let r = *R.get_or_init(|| {
        let one = |_: &mut Ctx| 1;
        let vt = vtable(
            "IMFSourceResolver",
            &[
                (resolver_qi as Handler, 12),
                (one, 4),
                (one, 4),
                (sv_from_url, 24),
                (sv_from_stream, 28),
                (sv_begin_url, 28),
                (sv_end, 16),
                (sv_begin_stream, 32),
                (sv_end, 16),
                (ok, 8), // CancelObjectCreation
            ],
        );
        object(vt, 1)
    });
    putp(out, r);
    0
}

pub fn init() {
    implement("mfplat", "MFCreateSourceResolver", MFCreateSourceResolver);
}
