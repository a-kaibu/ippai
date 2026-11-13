//! wmvcore.dll, the Windows Media Format SDK's readers: WMCreateSyncReader's IWMSyncReader(2)
//! (with the IWMProfile and IWMHeaderInfo it answers to, IWMStreamConfig / IWMMediaProps /
//! IWMOutputMediaProps for the formats, INSSBuffer for the samples) over an ASF file
//! (crates/asf) opened by name or read through the program's IStream.
//!
//! KiriKiri's krmovie (#60) reads a WMV out of the game's archive this way: OpenStream on its
//! IStream, SetReadStreamSamples(TRUE) for the compressed objects, which its own DirectShow
//! source filter hands to the WMV decoder DMO (dshow_dmo.rs) in buffers of its allocator
//! (IWMSyncReader2::SetAllocateForStream). Uncompressed output (the default) goes through
//! wmdec.rs's decoders.
//!
//! Each reader is one guest block: word 0 IWMSyncReader2, 1 IWMProfile3, 2 IWMHeaderInfo3.
//! The guest's IStream is read on the calling thread (the reader's lock held, not the table's).

#![allow(non_snake_case)]

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::api::{implement, vtable, Ctx};
use crate::mem::{self, ps, put32, putp, r16, r32, rp, w16, w32, w64, wp};

pub const E_NOINTERFACE: u32 = 0x8000_4002;
pub const E_POINTER: u32 = 0x8000_4003;
pub const E_INVALIDARG: u32 = 0x8007_0057;
pub const E_UNEXPECTED: u32 = 0x8000_ffff;
pub const E_NOTIMPL: u32 = 0x8000_4001;
pub const E_OUTOFMEMORY: u32 = 0x8007_000e;
pub const NS_E_NO_MORE_SAMPLES: u32 = 0xc00d_0bcf;
pub const NS_E_INVALID_REQUEST: u32 = 0xc00d_002b;
pub const NS_E_INVALID_OUTPUT_FORMAT: u32 = 0xc00d_0bc4;
pub const NS_E_UNRECOGNIZED_STREAM_TYPE: u32 = 0xc00d_002f;
pub const ASF_E_NOTFOUND: u32 = 0xc00d_07f0;
pub const ASF_E_BUFFERTOOSMALL: u32 = 0xc00d_07d1;
const NS_E_FILE_NOT_FOUND: u32 = 0xc00d_001a;
const NS_E_INVALID_DATA: u32 = 0xc00d_002e;

/// WM_SF_CLEANPOINT, WM_SF_DISCONTINUITY
const WM_SF_CLEANPOINT: u32 = 1;
const WM_SF_DISCONTINUITY: u32 = 2;

pub type Guid = [u8; 16];
pub use asf::guid;

const fn wm(d1: u32) -> Guid {
    // {xxxxxxxx-2b2b-11d3-b36b-00c04f6108ff}
    guid(d1, 0x2b2b, 0x11d3, [0xb3, 0x6b, 0x00, 0xc0, 0x4f, 0x61, 0x08, 0xff])
}
const IID_IUNKNOWN: Guid = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_SYNC_READER: Guid = guid(0x9397_f121, 0x7705, 0x4dc9, [0xb0, 0x49, 0x98, 0xb6, 0x98, 0x18, 0x84, 0x14]);
const IID_SYNC_READER2: Guid = guid(0xfaed_3d21, 0x1b6b, 0x4af7, [0x8c, 0xb6, 0x3e, 0x18, 0x9b, 0xbc, 0x18, 0x7b]);
const IID_PROFILE: Guid = wm(0x9640_6bdb);
const IID_PROFILE2: Guid = guid(0x07e7_2d33, 0xd94e, 0x4be7, [0x88, 0x43, 0x60, 0xae, 0x5f, 0xf7, 0xe5, 0xf5]);
const IID_PROFILE3: Guid = guid(0x00ef_96cc, 0xa461, 0x4546, [0x8b, 0xcd, 0xc9, 0xa2, 0x8f, 0x0e, 0x06, 0xf5]);
const IID_HEADER_INFO: Guid = wm(0x9640_6bda);
const IID_HEADER_INFO2: Guid = guid(0x15cf_9781, 0x454e, 0x482e, [0xb3, 0x93, 0x85, 0xfa, 0xe4, 0x87, 0xa8, 0x10]);
const IID_HEADER_INFO3: Guid = guid(0x15cc_68e3, 0x27cc, 0x4ecd, [0xb2, 0x22, 0x3f, 0x5d, 0x02, 0xd8, 0x0b, 0xd5]);
const IID_STREAM_CONFIG: Guid = wm(0x9640_6bdc);
const IID_MEDIA_PROPS: Guid = wm(0x9640_6bce);
const IID_OUTPUT_MEDIA_PROPS: Guid = wm(0x9640_6bd7);
const IID_NSSBUFFER: Guid = guid(0xe1cd_3524, 0x03d7, 0x11d2, [0x9e, 0xed, 0x00, 0x60, 0x97, 0xd2, 0xd7, 0xcf]);

/// Media types and formats (DirectShow's, which WM_MEDIA_TYPE shares).
pub const fn fourcc(d1: u32) -> Guid {
    guid(d1, 0, 0x10, [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71])
}
const fn ds(d1: u32) -> Guid {
    guid(d1, 0x524f, 0x11ce, [0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70])
}
pub const MEDIATYPE_VIDEO: Guid = fourcc(0x7364_6976);
pub const MEDIATYPE_AUDIO: Guid = fourcc(0x7364_7561);
pub const SUBTYPE_RGB24: Guid = ds(0xe436_eb7d);
pub const SUBTYPE_RGB32: Guid = ds(0xe436_eb7e);
pub const SUBTYPE_PCM: Guid = fourcc(1);
pub const FORMAT_VIDEO_INFO: Guid = guid(0x0558_9f80, 0xc356, 0x11ce, [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a]);
pub const FORMAT_WAVE_FORMAT_EX: Guid = guid(0x0558_9f81, 0xc356, 0x11ce, [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a]);

fn guid_at(p: u32) -> Option<Guid> {
    (p != 0).then(|| mem::slice(p, 16).try_into().unwrap())
}
fn put_guid(p: u32, g: &Guid) {
    mem::write(p, g);
}

// ---------------------------------------------------------------------------------------------
// media types

/// A WM_MEDIA_TYPE (AM_MEDIA_TYPE's layout) on the host side.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaType {
    pub major: Guid,
    pub sub: Guid,
    pub fixed: bool,
    pub temporal: bool,
    pub sample_size: u32,
    pub format_type: Guid,
    pub format: Vec<u8>,
}

/// Bytes of a WM_MEDIA_TYPE (the format block not counted).
pub fn mt_size() -> u32 {
    if mem::x64() {
        88
    } else {
        72
    }
}

impl MediaType {
    /// Writes it at `p` with its format block at `fmt` (guest memory).
    pub fn write(&self, p: u32, fmt: u32) {
        put_guid(p, &self.major);
        put_guid(p + 16, &self.sub);
        w32(p + 32, self.fixed as u32);
        w32(p + 36, self.temporal as u32);
        w32(p + 40, self.sample_size);
        put_guid(p + 44, &self.format_type);
        let k = if mem::x64() { 64 } else { 60 };
        wp(p + k, 0);
        w32(p + k + ps(), self.format.len() as u32);
        let at = (p + k + ps() + 4).next_multiple_of(ps());
        wp(at, if self.format.is_empty() { 0 } else { fmt });
        if !self.format.is_empty() {
            mem::write(fmt, &self.format);
        }
    }
    pub fn read(p: u32) -> Option<MediaType> {
        if p == 0 {
            return None;
        }
        let k = if mem::x64() { 64 } else { 60 };
        let n = r32(p + k + ps());
        let at = (p + k + ps() + 4).next_multiple_of(ps());
        let fp = rp(at);
        let format = if n != 0 && fp != 0 && n < 1 << 20 { mem::slice(fp, n).to_vec() } else { Vec::new() };
        Some(MediaType {
            major: guid_at(p)?,
            sub: guid_at(p + 16)?,
            fixed: r32(p + 32) != 0,
            temporal: r32(p + 36) != 0,
            sample_size: r32(p + 40),
            format_type: guid_at(p + 44)?,
            format,
        })
    }
}

/// The VIDEOINFOHEADER of a picture of `bih` (a BITMAPINFOHEADER and what follows it), at
/// `time_per_frame`.
pub fn video_info(bih: &[u8], width: u32, height: u32, bitrate: u32, time_per_frame: u64) -> Vec<u8> {
    let mut v = vec![0u8; 48];
    v[8..12].copy_from_slice(&width.to_le_bytes());
    v[12..16].copy_from_slice(&height.to_le_bytes());
    v[24..28].copy_from_slice(&width.to_le_bytes());
    v[28..32].copy_from_slice(&height.to_le_bytes());
    v[32..36].copy_from_slice(&bitrate.to_le_bytes());
    v[40..48].copy_from_slice(&time_per_frame.to_le_bytes());
    v.extend_from_slice(bih);
    v
}

/// A stream's compressed media type, as the file describes it.
pub fn stream_type(s: &asf::Stream) -> MediaType {
    match s.kind {
        asf::Kind::Audio => {
            let align = u16::from_le_bytes([s.format.get(12).copied().unwrap_or(1), s.format.get(13).copied().unwrap_or(0)]);
            MediaType { major: MEDIATYPE_AUDIO, sub: fourcc(s.codec()), fixed: true, temporal: false, sample_size: align as u32, format_type: FORMAT_WAVE_FORMAT_EX, format: s.format.clone() }
        }
        asf::Kind::Video { width, height } => MediaType {
            major: MEDIATYPE_VIDEO,
            sub: fourcc(s.codec()),
            fixed: false,
            temporal: true,
            sample_size: 0,
            format_type: FORMAT_VIDEO_INFO,
            format: video_info(&s.format, width, height, s.bitrate, s.avg_time_per_frame),
        },
        asf::Kind::Other(g) => MediaType { major: g, sub: [0; 16], fixed: false, temporal: false, sample_size: 0, format_type: [0; 16], format: s.format.clone() },
    }
}

// ---------------------------------------------------------------------------------------------
// the readers

thread_local! {
    /// The handler's context while a reader reads the guest's IStream.
    static CTX: Cell<*mut ()> = const { Cell::new(std::ptr::null_mut()) };
}

/// Runs `f` with `c` reachable to GuestStream::read_at.
fn with_ctx<R>(c: &mut Ctx, f: impl FnOnce() -> R) -> R {
    let old = CTX.with(|p| p.replace(c as *mut Ctx as *mut ()));
    let r = f();
    CTX.with(|p| p.set(old));
    r
}

/// A program's IStream (a reference held), read ahead in blocks.
struct GuestStream {
    stream: u32,
    cache: (u64, Vec<u8>),
}

const BLOCK: usize = 64 << 10;

impl GuestStream {
    fn read_guest(&mut self, off: u64, n: usize) -> Vec<u8> {
        let p = CTX.with(|p| p.get());
        if p.is_null() {
            return Vec::new();
        }
        let c = unsafe { &mut *(p as *mut Ctx) };
        let buf = mem::heap_alloc(n as u32 + 8, false);
        let got = buf + n as u32;
        // IStream::Seek(off, STREAM_SEEK_SET, NULL), Read(buf, n, &got)
        let hr = if mem::x64() {
            let f = mem::r64(mem::r64(mem::widen(self.stream) as u32) as u32 + 5 * 8);
            crate::call64(c.cpu, f, &[mem::widen(self.stream), off, 0, 0]) as u32
        } else {
            crate::dshow::com(c, self.stream, 5, &[off as u32, (off >> 32) as u32, 0, 0])
        };
        let mut out = Vec::new();
        if hr & 0x8000_0000 == 0 {
            w32(got, 0);
            let hr = crate::dshow::com(c, self.stream, 3, &[buf, n as u32, got]);
            if hr & 0x8000_0000 == 0 {
                let k = r32(got).min(n as u32);
                out = mem::slice(buf, k).to_vec();
            }
        }
        mem::heap_free(buf);
        out
    }
}

impl asf::Source for GuestStream {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        let (at, ref data) = self.cache;
        if off < at || off >= at + data.len() as u64 {
            let n = buf.len().max(BLOCK);
            self.cache = (off, self.read_guest(off, n));
        }
        let (at, ref data) = self.cache;
        let s = (off - at) as usize;
        let n = data.len().saturating_sub(s).min(buf.len());
        buf[..n].copy_from_slice(&data[s..s + n]);
        n
    }
}

/// A file by name.
struct HostFile(kura::fs::File);

impl asf::Source for HostFile {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        use std::io::{Read, Seek, SeekFrom};
        if self.0.seek(SeekFrom::Start(off)).is_err() {
            return 0;
        }
        let mut got = 0;
        while got < buf.len() {
            match self.0.read(&mut buf[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        got
    }
}

pub type Src = Box<dyn asf::Source + Send>;

// (the raw guest pointers in it are only used on the guest's threads)
unsafe impl Send for GuestStream {}

/// One output (= one stream of the file, in header order).
pub struct Output {
    pub stream: u8,
    /// the type the output gives
    pub ty: MediaType,
    /// read compressed (SetReadStreamSamples TRUE)
    pub compressed: bool,
    pub selected: bool,
    /// the program's IWMReaderAllocatorEx for this stream / output (a reference held)
    pub alloc_stream: u32,
    pub alloc_output: u32,
    pub dec: Option<Box<dyn crate::wmdec::Decoder>>,
    /// uncompressed samples decoded, not taken yet: time, duration, bytes, key
    pub ready: VecDeque<(u64, u64, Vec<u8>, bool)>,
    pub discontinuity: bool,
}

pub struct Reader {
    pub r: Option<asf::Reader<Src>>,
    /// the program's IStream (a reference held)
    pub stream: u32,
    pub outputs: Vec<Output>,
    /// samples read from the file, not taken yet
    pub pending: VecDeque<asf::Sample>,
    pub range: (u64, u64),
    pub ended: bool,
}

impl Reader {
    fn header(&self) -> Option<&asf::Header> {
        self.r.as_ref().map(|r| &r.header)
    }
    fn output_of(&self, stream: u8) -> Option<usize> {
        self.outputs.iter().position(|o| o.stream == stream)
    }
}

struct Obj {
    refs: u32,
    r: Arc<Mutex<Reader>>,
}

static READERS: Mutex<Option<HashMap<u32, Obj>>> = Mutex::new(None);

/// interface words of a reader's block
const W_SYNC: u32 = 0;
const W_PROFILE: u32 = 1;
const W_HEADER: u32 = 2;
const WORDS: u32 = 3;

fn table<R>(f: impl FnOnce(&mut HashMap<u32, Obj>) -> R) -> R {
    f(READERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// The reader block of an interface pointer of one.
fn block(p: u32) -> Option<u32> {
    table(|m| (0..WORDS).find_map(|k| p.checked_sub(ps() * k).filter(|b| m.contains_key(b))))
}

fn reader(p: u32) -> Option<Arc<Mutex<Reader>>> {
    let b = block(p)?;
    table(|m| m.get(&b).map(|o| o.r.clone()))
}

fn lock(r: &Arc<Mutex<Reader>>) -> std::sync::MutexGuard<'_, Reader> {
    r.lock().unwrap_or_else(|e| e.into_inner())
}

struct Vt {
    sync: u32,
    profile: u32,
    header: u32,
    config: u32,
    props: u32,
    buffer: u32,
}

fn vts() -> &'static Vt {
    static V: std::sync::OnceLock<Vt> = std::sync::OnceLock::new();
    V.get_or_init(|| Vt {
        sync: vtable(
            "IWMSyncReader2",
            &[
                (qi, 12),
                (add_ref, 4),
                (release, 4),
                (sr_open, 8),
                (sr_close, 4),
                (sr_set_range, 20),
                (sr_set_range_by_frame, 24),
                (sr_get_next_sample, 32),
                (sr_set_streams_selected, 16),
                (sr_get_stream_selected, 12),
                (sr_set_read_stream_samples, 12),
                (sr_get_read_stream_samples, 12),
                (sr_get_output_setting, 24),
                (sr_set_output_setting, 24),
                (sr_get_output_count, 8),
                (sr_get_output_props, 12),
                (sr_set_output_props, 12),
                (sr_get_output_format_count, 12),
                (sr_get_output_format, 16),
                (sr_get_output_number_for_stream, 12),
                (sr_get_stream_number_for_output, 12),
                (sr_get_max_output_sample_size, 12),
                (sr_get_max_stream_sample_size, 12),
                (sr_open_stream, 8),
                // IWMSyncReader2
                (notimpl, 16),
                (notimpl, 28),
                (sr_set_allocate_for_output, 12),
                (sr_get_allocate_for_output, 12),
                (sr_set_allocate_for_stream, 12),
                (sr_get_allocate_for_stream, 12),
            ],
        ),
        profile: vtable(
            "IWMProfile3",
            &[
                (qi, 12),
                (add_ref, 4),
                (release, 4),
                (pr_get_version, 8),
                (pr_get_name, 12),
                (notimpl, 8),
                (pr_get_name, 12),
                (notimpl, 8),
                (pr_get_stream_count, 8),
                (pr_get_stream, 12),
                (pr_get_stream_by_number, 12),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 12),
                (pr_zero_count, 8),
                (notimpl, 12),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 8),
                // IWMProfile2::GetProfileID, IWMProfile3
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 8),
                (pr_zero_count, 8),
                (notimpl, 12),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 8),
                (pr_zero_count, 8),
                (notimpl, 12),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 12),
                (notimpl, 8),
                (notimpl, 8),
                (notimpl, 8),
            ],
        ),
        header: vtable(
            "IWMHeaderInfo3",
            &[
                (qi, 12),
                (add_ref, 4),
                (release, 4),
                (hi_get_attribute_count, 12),
                (hi_get_attribute_by_index, 32),
                (hi_get_attribute_by_name, 24),
                (notimpl, 24),
                (hi_zero_count, 8),
                (notimpl, 24),
                (notimpl, 16),
                (notimpl, 8),
                (hi_zero_count, 8),
                (notimpl, 28),
                (notimpl, 20),
                (notimpl, 8),
                // IWMHeaderInfo2
                (hi_zero_count, 8),
                (notimpl, 32),
                // IWMHeaderInfo3
                (hi_get_attribute_count_ex, 12),
                (notimpl, 24),
                (notimpl, 40),
                (notimpl, 32),
                (notimpl, 32),
                (notimpl, 16),
                (notimpl, 44),
                (notimpl, 32),
            ],
        ),
        config: vtable(
            "IWMStreamConfig",
            &[
                (sc_qi, 12),
                (com_add_ref, 4),
                (sc_release, 4),
                (sc_get_stream_type, 8),
                (sc_get_stream_number, 8),
                (notimpl, 8),
                (sc_get_name, 12),
                (notimpl, 8),
                (sc_get_name, 12),
                (notimpl, 8),
                (sc_get_bitrate, 8),
                (notimpl, 8),
                (sc_get_buffer_window, 8),
                (notimpl, 8),
            ],
        ),
        props: vtable(
            "IWMOutputMediaProps",
            &[
                (sc_qi, 12),
                (com_add_ref, 4),
                (sc_release, 4),
                (mp_get_type, 8),
                (mp_get_media_type, 12),
                (mp_set_media_type, 8),
                (mp_get_name, 12),
                (mp_get_name, 12),
            ],
        ),
        buffer: vtable(
            "INSSBuffer",
            &[(buf_qi, 12), (buf_add_ref, 4), (buf_release, 4), (buf_get_length, 8), (buf_set_length, 8), (buf_get_max_length, 8), (buf_get_buffer, 8), (buf_get_buffer_and_length, 12)],
        ),
    })
}

fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}

/// A new sync reader: its IWMSyncReader2.
pub fn new_sync_reader() -> u32 {
    let v = vts();
    let b = mem::heap_alloc(WORDS * ps(), true);
    wp(b + ps() * W_SYNC, v.sync);
    wp(b + ps() * W_PROFILE, v.profile);
    wp(b + ps() * W_HEADER, v.header);
    let r = Reader { r: None, stream: 0, outputs: Vec::new(), pending: VecDeque::new(), range: (0, 0), ended: false };
    table(|m| m.insert(b, Obj { refs: 1, r: Arc::new(Mutex::new(r)) }));
    b
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, riid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let Some(b) = block(this) else { return E_NOINTERFACE };
    let want = guid_at(riid).unwrap_or([0xff; 16]);
    let w = if [IID_IUNKNOWN, IID_SYNC_READER, IID_SYNC_READER2].contains(&want) {
        W_SYNC
    } else if [IID_PROFILE, IID_PROFILE2, IID_PROFILE3].contains(&want) {
        W_PROFILE
    } else if [IID_HEADER_INFO, IID_HEADER_INFO2, IID_HEADER_INFO3].contains(&want) {
        W_HEADER
    } else {
        putp(out, 0);
        return E_NOINTERFACE;
    };
    table(|m| m.get_mut(&b).map(|o| o.refs += 1));
    putp(out, b + ps() * w);
    0
}

fn add_ref(c: &mut Ctx) -> u32 {
    let Some(b) = block(c.a(0)) else { return 1 };
    table(|m| m.get_mut(&b).map(|o| {
        o.refs += 1;
        o.refs
    }))
    .unwrap_or(1)
}

fn release(c: &mut Ctx) -> u32 {
    let Some(b) = block(c.a(0)) else { return 0 };
    let (left, r) = table(|m| {
        let Some(o) = m.get_mut(&b) else { return (0, None) };
        o.refs = o.refs.saturating_sub(1);
        if o.refs > 0 {
            return (o.refs, None);
        }
        (0, m.remove(&b).map(|o| o.r))
    });
    if let Some(r) = r {
        close(c, &mut lock(&r));
    }
    left
}

/// Drops what the reader holds of the file and of the program.
fn close(c: &mut Ctx, r: &mut Reader) {
    let held: Vec<u32> = r.outputs.iter().flat_map(|o| [o.alloc_stream, o.alloc_output]).chain([r.stream]).filter(|&p| p != 0).collect();
    r.r = None;
    r.stream = 0;
    r.outputs.clear();
    r.pending.clear();
    r.ended = false;
    for p in held {
        crate::dshow::com(c, p, 2, &[]);
    }
}

/// Opens the file in `src`: its outputs, one per stream, uncompressed by default.
fn open_src(r: &mut Reader, src: Src) -> u32 {
    let a = match asf::Reader::new(src) {
        Ok(a) => a,
        Err(asf::Error::NotAsf) => return NS_E_INVALID_DATA,
        Err(_) => return NS_E_INVALID_DATA,
    };
    r.outputs = a
        .header
        .streams
        .iter()
        .map(|s| {
            let ty = crate::wmdec::default_output(s).unwrap_or_else(|| stream_type(s));
            Output { stream: s.number, ty, compressed: false, selected: true, alloc_stream: 0, alloc_output: 0, dec: None, ready: VecDeque::new(), discontinuity: true }
        })
        .collect();
    r.r = Some(a);
    r.pending.clear();
    r.range = (0, 0);
    r.ended = false;
    0
}

/// Open(LPCWSTR)
fn sr_open(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    if c.a(1) == 0 {
        return E_INVALIDARG;
    }
    let name = String::from_utf16_lossy(&mem::wstr(c.a(1)));
    let mut r = lock(&rr);
    if r.r.is_some() {
        return E_UNEXPECTED;
    }
    let Some(path) = crate::files::resolve(&name) else { return NS_E_FILE_NOT_FOUND };
    let Ok(f) = kura::fs::File::open(&path) else { return NS_E_FILE_NOT_FOUND };
    open_src(&mut r, Box::new(HostFile(f)))
}

/// OpenStream(IStream *)
fn sr_open_stream(c: &mut Ctx) -> u32 {
    let (this, stream) = (c.a(0), c.a(1));
    let Some(rr) = reader(this) else { return E_UNEXPECTED };
    if stream == 0 {
        return E_INVALIDARG;
    }
    let mut r = lock(&rr);
    if r.r.is_some() {
        return E_UNEXPECTED;
    }
    crate::dshow::com(c, stream, 1, &[]);
    let hr = with_ctx(c, || open_src(&mut r, Box::new(GuestStream { stream, cache: (0, Vec::new()) })));
    if hr != 0 {
        drop(r);
        crate::dshow::com(c, stream, 2, &[]);
        return hr;
    }
    r.stream = stream;
    0
}

fn sr_close(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let mut r = lock(&rr);
    if r.r.is_none() {
        return NS_E_INVALID_REQUEST;
    }
    close(c, &mut r);
    0
}

/// A QWORD argument at 32-bit slots `i`, `i + 1` (x64: argument `x`).
fn q(c: &Ctx, i: u32, x: u32) -> u64 {
    if mem::x64() {
        c.a64(x)
    } else {
        c.a(i) as u64 | (c.a(i + 1) as u64) << 32
    }
}

/// SetRange(QWORD start, LONGLONG duration)
fn sr_set_range(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (start, dur) = (q(c, 1, 1), q(c, 3, 2));
    let mut r = lock(&rr);
    let r = &mut *r;
    let Some(a) = r.r.as_mut() else { return NS_E_INVALID_REQUEST };
    if (dur as i64) < 0 {
        return E_INVALIDARG;
    }
    with_ctx(c, || a.seek(start));
    r.pending.clear();
    r.range = (start, dur);
    r.ended = false;
    for o in &mut r.outputs {
        o.ready.clear();
        o.discontinuity = true;
        if let Some(d) = o.dec.as_mut() {
            d.flush();
        }
    }
    0
}

/// SetRangeByFrame(WORD stream, QWORD frame, LONGLONG frames): by the stream's frame rate
fn sr_set_range_by_frame(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (n, frame, count) = (c.a(1) as u8, q(c, 2, 2), q(c, 4, 3));
    let t = {
        let r = lock(&rr);
        let Some(h) = r.header() else { return NS_E_INVALID_REQUEST };
        let Some(s) = h.stream(n) else { return E_INVALIDARG };
        let per = if s.avg_time_per_frame != 0 { s.avg_time_per_frame } else { 333_667 };
        (frame.saturating_mul(per), (count as i64).max(0) as u64 * per)
    };
    let mut r = lock(&rr);
    let r = &mut *r;
    let a = r.r.as_mut().unwrap();
    with_ctx(c, || a.seek(t.0));
    r.pending.clear();
    r.range = t;
    r.ended = false;
    for o in &mut r.outputs {
        o.ready.clear();
        o.discontinuity = true;
        if let Some(d) = o.dec.as_mut() {
            d.flush();
        }
    }
    0
}

/// Writes a new INSSBuffer of `data` (ours, or the program's allocator's) into `*out`.
fn hand_out(c: &mut Ctx, alloc: (u32, bool, u32), data: &[u8], time: u64, dur: u64, flags: u32, out: u32) -> u32 {
    let (a, by_stream, num) = alloc;
    if a != 0 {
        // IWMReaderAllocatorEx::AllocateForStreamEx / AllocateForOutputEx(num, cb, &buf, flags,
        // time, duration, context)
        let slot = if by_stream { 3 } else { 4 };
        let pp = mem::heap_alloc(8, true);
        let hr = if mem::x64() {
            let f = rp(rp(a) + ps() * slot);
            crate::call64(c.cpu, mem::widen(f), &[mem::widen(a), num as u64, data.len() as u64, pp as u64, flags as u64, time, dur, 0]) as u32
        } else {
            crate::dshow::com(c, a, slot, &[num, data.len() as u32, pp, flags, time as u32, (time >> 32) as u32, dur as u32, (dur >> 32) as u32, 0])
        };
        let b = rp(pp);
        mem::heap_free(pp);
        if hr & 0x8000_0000 != 0 || b == 0 {
            return if hr & 0x8000_0000 != 0 { hr } else { E_OUTOFMEMORY };
        }
        // GetBuffer(&p), SetLength(n)
        let p = mem::heap_alloc(8, true);
        crate::dshow::com(c, b, 6, &[p]);
        let dst = rp(p);
        mem::heap_free(p);
        if dst != 0 {
            mem::write(dst, data);
        }
        crate::dshow::com(c, b, 4, &[data.len() as u32]);
        putp(out, b);
        return 0;
    }
    putp(out, new_buffer(data));
    0
}

/// GetNextSample(WORD stream, INSSBuffer **, QWORD *time, QWORD *duration, DWORD *flags,
/// DWORD *output, WORD *stream)
fn sr_get_next_sample(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let want = c.a(1) as u16;
    let (pbuf, ptime, pdur, pflags, pout, pnum) = (c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    if pbuf == 0 || ptime == 0 || pdur == 0 || pflags == 0 {
        return E_POINTER;
    }
    if want == 0 && (pout == 0 && pnum == 0) {
        return E_POINTER;
    }
    let mut g = lock(&rr);
    let r = &mut *g;
    if r.r.is_none() {
        return NS_E_INVALID_REQUEST;
    }
    if want != 0 {
        match r.output_of(want as u8) {
            None => return E_INVALIDARG,
            Some(k) if !r.outputs[k].selected => return NS_E_INVALID_REQUEST,
            _ => {}
        }
    }
    let got = with_ctx(c, || next_sample(r, want as u8));
    let Some((k, time, dur, data, key)) = got else {
        return NS_E_NO_MORE_SAMPLES;
    };
    let o = &mut r.outputs[k];
    let mut flags = if key { WM_SF_CLEANPOINT } else { 0 };
    if std::mem::replace(&mut o.discontinuity, false) {
        flags |= WM_SF_DISCONTINUITY;
    }
    let alloc = if o.alloc_stream != 0 && o.compressed {
        (o.alloc_stream, true, o.stream as u32)
    } else if o.alloc_output != 0 && !o.compressed {
        (o.alloc_output, false, k as u32)
    } else {
        (0, false, 0)
    };
    let stream = o.stream;
    drop(g);
    let hr = hand_out(c, alloc, &data, time, dur, flags, pbuf);
    if hr != 0 {
        return hr;
    }
    w64(ptime, time);
    w64(pdur, dur);
    w32(pflags, flags);
    put32(pout, k as u32);
    if pnum != 0 {
        w16(pnum, stream as u16);
    }
    0
}

/// The next sample of stream `want` (0: of any selected stream): output, time, duration,
/// bytes, key.
pub fn next_sample(r: &mut Reader, want: u8) -> Option<(usize, u64, u64, Vec<u8>, bool)> {
    let end = if r.range.1 != 0 { r.range.0.saturating_add(r.range.1) } else { u64::MAX };
    loop {
        // uncompressed samples decoded already
        let ready = r.outputs.iter().enumerate().filter(|(_, o)| o.selected && !o.compressed && (want == 0 || o.stream == want)).filter_map(|(k, o)| o.ready.front().map(|s| (k, s.0))).min_by_key(|x| x.1);
        // a sample from the file, of this output, before any later one
        let at = r.pending.iter().position(|s| want == 0 || s.stream == want);
        if let Some((k, t)) = ready {
            if at.is_none_or(|i| r.pending[i].time >= t) {
                let (time, dur, data, key) = r.outputs[k].ready.pop_front().unwrap();
                return Some((k, time, dur, data, key));
            }
        }
        if let Some(i) = at {
            let s = r.pending.remove(i).unwrap();
            let k = r.output_of(s.stream)?;
            let dur = r.header().and_then(|h| h.stream(s.stream)).map(|st| sample_duration(st, &s)).unwrap_or(0);
            let o = &mut r.outputs[k];
            if o.compressed {
                if s.time >= end {
                    r.ended = true;
                    continue;
                }
                return Some((k, s.time, dur, s.data, s.key));
            }
            // decode it; what it gives comes out by time
            let h = r.r.as_ref()?.header.clone();
            let o = &mut r.outputs[k];
            if o.dec.is_none() {
                o.dec = h.stream(s.stream).and_then(|st| crate::wmdec::decoder(st, &o.ty));
            }
            let Some(d) = o.dec.as_mut() else { continue };
            let start = r.range.0;
            for (t, dur, data) in d.decode(&s) {
                if t + dur <= start && dur != 0 || t >= end {
                    continue;
                }
                o.ready.push_back((t, dur, data, true));
            }
            continue;
        }
        if r.ended {
            return None;
        }
        // more from the file
        let a = r.r.as_mut()?;
        match a.next_sample() {
            Some(s) => {
                let sel = r.outputs.iter().any(|o| o.stream == s.stream && o.selected);
                if sel {
                    r.pending.push_back(s);
                }
            }
            None => {
                // the end: what the decoders hold back comes out
                r.ended = true;
                for o in &mut r.outputs {
                    if let Some(d) = o.dec.as_mut() {
                        for (t, dur, data) in d.drain() {
                            if t < end {
                                o.ready.push_back((t, dur, data, true));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A compressed sample's duration: a frame's (video), its bytes' time (audio).
pub fn sample_duration(s: &asf::Stream, x: &asf::Sample) -> u64 {
    let bps = u32::from_le_bytes(s.format.get(8..12).and_then(|b| b.try_into().ok()).unwrap_or([0; 4])) as u64;
    match s.kind {
        asf::Kind::Video { .. } => s.avg_time_per_frame,
        asf::Kind::Audio if bps != 0 => x.data.len() as u64 * 10_000_000 / bps,
        _ => 0,
    }
}

/// SetStreamsSelected(WORD count, WORD *numbers, WMT_STREAM_SELECTION *selections)
fn sr_set_streams_selected(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (n, nums, sels) = (c.a(1) & 0xffff, c.a(2), c.a(3));
    if n == 0 || nums == 0 || sels == 0 {
        return E_INVALIDARG;
    }
    let mut r = lock(&rr);
    if r.r.is_none() {
        return NS_E_INVALID_REQUEST;
    }
    for i in 0..n {
        let s = r16(nums + 2 * i) as u8;
        let Some(k) = r.output_of(s) else { return E_INVALIDARG };
        // WMT_OFF 0, WMT_CLEANPOINT_ONLY 1, WMT_ON 2
        r.outputs[k].selected = r32(sels + 4 * i) != 0;
    }
    0
}

fn sr_get_stream_selected(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (s, out) = (c.a(1) as u8, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(k) = r.output_of(s) else { return E_INVALIDARG };
    w32(out, if r.outputs[k].selected { 2 } else { 0 });
    0
}

fn sr_set_read_stream_samples(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (s, on) = (c.a(1) as u8, c.a(2) != 0);
    let mut r = lock(&rr);
    if r.r.is_none() {
        return NS_E_INVALID_REQUEST;
    }
    let Some(k) = r.output_of(s) else { return E_INVALIDARG };
    r.outputs[k].compressed = on;
    0
}

fn sr_get_read_stream_samples(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (s, out) = (c.a(1) as u8, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(k) = r.output_of(s) else { return E_INVALIDARG };
    w32(out, r.outputs[k].compressed as u32);
    0
}

/// GetOutputSetting / SetOutputSetting: the settings (e.g. "VideoSampleDurations") are taken
/// and kept to nothing
fn sr_get_output_setting(c: &mut Ctx) -> u32 {
    let _ = c;
    ASF_E_NOTFOUND
}
fn sr_set_output_setting(c: &mut Ctx) -> u32 {
    let _ = c;
    0
}

fn sr_get_output_count(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    if r.r.is_none() {
        return NS_E_INVALID_REQUEST;
    }
    w32(c.a(1), r.outputs.len() as u32);
    0
}

/// GetOutputProps(DWORD output, IWMOutputMediaProps **): a copy of its type
fn sr_get_output_props(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (k, out) = (c.a(1) as usize, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    if r.r.is_none() {
        return NS_E_INVALID_REQUEST;
    }
    let Some(o) = r.outputs.get(k) else { return E_INVALIDARG };
    putp(out, new_props(Props::Output(o.ty.clone())));
    0
}

/// SetOutputProps(DWORD output, IWMOutputMediaProps *): the type it holds, when the output can
/// give it
fn sr_set_output_props(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (k, p) = (c.a(1) as usize, c.a(2));
    if p == 0 {
        return E_POINTER;
    }
    // (the program's props: ask them for their type)
    let ty = match props_type(p) {
        Some(t) => t,
        None => {
            let n = mem::heap_alloc(4, true);
            crate::dshow::com(c, p, 4, &[0, n]);
            let size = r32(n);
            let buf = mem::heap_alloc(size.max(mt_size()), true);
            w32(n, size);
            let hr = crate::dshow::com(c, p, 4, &[buf, n]);
            let t = if hr == 0 { MediaType::read(buf) } else { None };
            mem::heap_free(buf);
            mem::heap_free(n);
            match t {
                Some(t) => t,
                None => return E_INVALIDARG,
            }
        }
    };
    let mut g = lock(&rr);
    let r = &mut *g;
    let Some(h) = r.header().cloned() else { return NS_E_INVALID_REQUEST };
    let Some(o) = r.outputs.get_mut(k) else { return E_INVALIDARG };
    let Some(s) = h.stream(o.stream) else { return E_INVALIDARG };
    if !crate::wmdec::formats(s).iter().any(|f| f.major == ty.major && f.sub == ty.sub) {
        return NS_E_INVALID_OUTPUT_FORMAT;
    }
    o.ty = ty;
    o.dec = None;
    o.ready.clear();
    0
}

fn sr_get_output_format_count(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (k, out) = (c.a(1) as usize, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(h) = r.header() else { return NS_E_INVALID_REQUEST };
    let Some(s) = r.outputs.get(k).and_then(|o| h.stream(o.stream)) else { return E_INVALIDARG };
    w32(out, crate::wmdec::formats(s).len() as u32);
    0
}

fn sr_get_output_format(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (k, i, out) = (c.a(1) as usize, c.a(2) as usize, c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(h) = r.header() else { return NS_E_INVALID_REQUEST };
    let Some(s) = r.outputs.get(k).and_then(|o| h.stream(o.stream)) else { return E_INVALIDARG };
    let Some(f) = crate::wmdec::formats(s).into_iter().nth(i) else { return E_INVALIDARG };
    putp(out, new_props(Props::Output(f)));
    0
}

fn sr_get_output_number_for_stream(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (s, out) = (c.a(1) as u8, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(k) = r.output_of(s) else { return E_INVALIDARG };
    w32(out, k as u32);
    0
}

fn sr_get_stream_number_for_output(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (k, out) = (c.a(1) as usize, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(o) = r.outputs.get(k) else { return E_INVALIDARG };
    w16(out, o.stream as u16);
    0
}

fn sr_get_max_output_sample_size(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (k, out) = (c.a(1) as usize, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(o) = r.outputs.get(k) else { return E_INVALIDARG };
    w32(out, crate::wmdec::sample_size(&o.ty).max(1));
    0
}

fn sr_get_max_stream_sample_size(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (s, out) = (c.a(1) as u8, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(h) = r.header() else { return NS_E_INVALID_REQUEST };
    let Some(st) = h.stream(s) else { return E_INVALIDARG };
    // (the largest object the file can hold: its packets', or a picture's when uncompressed)
    let n = match st.kind {
        asf::Kind::Video { width, height } => (width * height * 4).max(h.packet_size),
        _ => h.packet_size * 4,
    };
    w32(out, n);
    0
}

fn set_alloc(c: &mut Ctx, by_stream: bool) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (n, a) = (c.a(1), c.a(2));
    let old = {
        let mut r = lock(&rr);
        let k = if by_stream { r.output_of(n as u8) } else { Some(n as usize).filter(|&k| k < r.outputs.len()) };
        let Some(k) = k else { return E_INVALIDARG };
        let o = &mut r.outputs[k];
        let slot = if by_stream { &mut o.alloc_stream } else { &mut o.alloc_output };
        std::mem::replace(slot, a)
    };
    if a != 0 {
        crate::dshow::com(c, a, 1, &[]);
    }
    if old != 0 {
        crate::dshow::com(c, old, 2, &[]);
    }
    0
}

fn get_alloc(c: &mut Ctx, by_stream: bool) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (n, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let a = {
        let r = lock(&rr);
        let k = if by_stream { r.output_of(n as u8) } else { Some(n as usize).filter(|&k| k < r.outputs.len()) };
        let Some(k) = k else { return E_INVALIDARG };
        if by_stream { r.outputs[k].alloc_stream } else { r.outputs[k].alloc_output }
    };
    if a != 0 {
        crate::dshow::com(c, a, 1, &[]);
    }
    putp(out, a);
    0
}

fn sr_set_allocate_for_output(c: &mut Ctx) -> u32 {
    set_alloc(c, false)
}
fn sr_get_allocate_for_output(c: &mut Ctx) -> u32 {
    get_alloc(c, false)
}
fn sr_set_allocate_for_stream(c: &mut Ctx) -> u32 {
    set_alloc(c, true)
}
fn sr_get_allocate_for_stream(c: &mut Ctx) -> u32 {
    get_alloc(c, true)
}

// ---- IWMProfile

/// WMT_VER_9_0
fn pr_get_version(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 0x0009_0000);
    0
}

/// GetName / GetDescription(WCHAR *, DWORD *chars): empty
fn pr_get_name(c: &mut Ctx) -> u32 {
    let (buf, n) = (c.a(1), c.a(2));
    if n == 0 {
        return E_POINTER;
    }
    if buf != 0 && r32(n) >= 1 {
        w16(buf, 0);
    }
    w32(n, 1);
    0
}

fn pr_zero_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 0);
    0
}

fn pr_get_stream_count(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(h) = r.header() else { return NS_E_INVALID_REQUEST };
    w32(c.a(1), h.streams.len() as u32);
    0
}

fn pr_get_stream(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (i, out) = (c.a(1) as usize, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(h) = r.header() else { return NS_E_INVALID_REQUEST };
    let Some(s) = h.streams.get(i) else { return E_INVALIDARG };
    putp(out, new_props(Props::Stream(s.clone())));
    0
}

fn pr_get_stream_by_number(c: &mut Ctx) -> u32 {
    let Some(rr) = reader(c.a(0)) else { return E_UNEXPECTED };
    let (n, out) = (c.a(1) as u8, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let r = lock(&rr);
    let Some(h) = r.header() else { return NS_E_INVALID_REQUEST };
    let Some(s) = h.stream(n) else {
        putp(out, 0);
        return NS_E_UNRECOGNIZED_STREAM_TYPE;
    };
    putp(out, new_props(Props::Stream(s.clone())));
    0
}

// ---- IWMHeaderInfo

/// WMT_ATTR_DATATYPE: DWORD 0, STRING 1, BINARY 2, BOOL 3, QWORD 4, WORD 5, GUID 6
fn attr_bytes(v: &asf::Value) -> (u32, Vec<u8>) {
    match v {
        asf::Value::U32(x) => (0, x.to_le_bytes().to_vec()),
        asf::Value::Str(s) => (1, s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect()),
        asf::Value::Bytes(b) => (2, b.clone()),
        asf::Value::Bool(b) => (3, (*b as u32).to_le_bytes().to_vec()),
        asf::Value::U64(x) => (4, x.to_le_bytes().to_vec()),
        asf::Value::U16(x) => (5, x.to_le_bytes().to_vec()),
        asf::Value::Guid(g) => (6, g.to_vec()),
    }
}

/// The attributes the reader answers for `stream` (0: the file): the file's own, and those
/// Windows makes of the header (Duration, Seekable, Title ...).
pub fn attributes(h: &asf::Header, stream: u16) -> Vec<(String, asf::Value)> {
    let mut v: Vec<(String, asf::Value)> = Vec::new();
    if stream == 0 {
        v.push(("Duration".into(), asf::Value::U64(h.duration)));
        v.push(("Bitrate".into(), asf::Value::U32(h.max_bitrate)));
        v.push(("Seekable".into(), asf::Value::Bool(h.flags & 2 != 0)));
        v.push(("Stridable".into(), asf::Value::Bool(false)));
        v.push(("Broadcast".into(), asf::Value::Bool(h.flags & 1 != 0)));
        v.push(("Is_Protected".into(), asf::Value::Bool(false)));
        v.push(("Is_Trusted".into(), asf::Value::Bool(false)));
        v.push(("Signature_Name".into(), asf::Value::Str(String::new())));
        v.push(("HasAudio".into(), asf::Value::Bool(h.streams.iter().any(|s| s.kind == asf::Kind::Audio))));
        v.push(("HasImage".into(), asf::Value::Bool(false)));
        v.push(("HasScript".into(), asf::Value::Bool(false)));
        v.push(("HasVideo".into(), asf::Value::Bool(h.streams.iter().any(|s| matches!(s.kind, asf::Kind::Video { .. })))));
        v.push(("CurrentBitrate".into(), asf::Value::U32(h.max_bitrate)));
        v.push(("OptimalBitrate".into(), asf::Value::U32(h.max_bitrate)));
        v.push(("Title".into(), asf::Value::Str(h.title.clone())));
        v.push(("Author".into(), asf::Value::Str(h.author.clone())));
        v.push(("Copyright".into(), asf::Value::Str(h.copyright.clone())));
        v.push(("Description".into(), asf::Value::Str(h.description.clone())));
    }
    for a in h.attributes.iter().filter(|a| a.stream == stream) {
        if let Some(e) = v.iter_mut().find(|e| e.0 == a.name) {
            e.1 = a.value.clone();
        } else {
            v.push((a.name.clone(), a.value.clone()));
        }
    }
    v
}

fn hi_attrs(c: &Ctx, stream: u16) -> Result<Vec<(String, asf::Value)>, u32> {
    let rr = reader(c.a(0)).ok_or(E_UNEXPECTED)?;
    let r = lock(&rr);
    let h = r.header().ok_or(NS_E_INVALID_REQUEST)?;
    Ok(attributes(h, stream))
}

/// GetAttributeCount(WORD stream, WORD *count)
fn hi_get_attribute_count(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    if out == 0 {
        return E_INVALIDARG;
    }
    match hi_attrs(c, c.a(1) as u16) {
        Ok(v) => {
            w16(out, v.len() as u16);
            0
        }
        Err(e) => e,
    }
}

/// GetAttributeCountEx(WORD stream, WORD *count)
fn hi_get_attribute_count_ex(c: &mut Ctx) -> u32 {
    hi_get_attribute_count(c)
}

fn hi_zero_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_INVALIDARG;
    }
    w32(c.a(1), 0);
    0
}

/// Writes a value: type, and the bytes into `value` (`*len` bytes there; NULL: the length only).
fn put_value(ty_p: u32, value: u32, len_p: u32, v: &asf::Value) -> u32 {
    let (ty, b) = attr_bytes(v);
    put32(ty_p, ty);
    if len_p == 0 {
        return E_INVALIDARG;
    }
    let room = r16(len_p) as usize;
    w16(len_p, b.len() as u16);
    if value == 0 {
        return 0;
    }
    if room < b.len() {
        return ASF_E_BUFFERTOOSMALL;
    }
    mem::write(value, &b);
    0
}

/// GetAttributeByIndex(WORD index, WORD *stream, WCHAR *name, WORD *chars, type, value, *len)
fn hi_get_attribute_by_index(c: &mut Ctx) -> u32 {
    let (i, ps_, name, chars, ty, value, len) = (c.a(1) as usize, c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    if ps_ == 0 || chars == 0 || len == 0 {
        return E_INVALIDARG;
    }
    let v = match hi_attrs(c, r16(ps_)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some((n, val)) = v.get(i) else { return E_INVALIDARG };
    let w: Vec<u16> = n.encode_utf16().chain([0]).collect();
    let room = r16(chars) as usize;
    w16(chars, w.len() as u16);
    if name != 0 {
        if room < w.len() {
            return ASF_E_BUFFERTOOSMALL;
        }
        for (k, ch) in w.iter().enumerate() {
            w16(name + 2 * k as u32, *ch);
        }
    }
    put_value(ty, value, len, val)
}

/// GetAttributeByName(WORD *stream, LPCWSTR name, type, BYTE *value, WORD *len)
fn hi_get_attribute_by_name(c: &mut Ctx) -> u32 {
    let (ps_, name, ty, value, len) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if ps_ == 0 || name == 0 || ty == 0 || len == 0 {
        return E_INVALIDARG;
    }
    let want = String::from_utf16_lossy(&mem::wstr(name));
    let v = match hi_attrs(c, r16(ps_)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some((_, val)) = v.iter().find(|(n, _)| n.eq_ignore_ascii_case(&want)) else { return ASF_E_NOTFOUND };
    put_value(ty, value, len, val)
}

// ---------------------------------------------------------------------------------------------
// stream configs, media props, buffers

enum Props {
    /// IWMStreamConfig (and IWMMediaProps) of a stream
    Stream(asf::Stream),
    /// IWMOutputMediaProps of a type
    Output(MediaType),
}

struct PropsObj {
    refs: u32,
    p: Props,
}

static PROPS: Mutex<Option<HashMap<u32, PropsObj>>> = Mutex::new(None);

fn props<R>(f: impl FnOnce(&mut HashMap<u32, PropsObj>) -> R) -> R {
    f(PROPS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// A stream config's block: word 0 IWMStreamConfig, 1 IWMMediaProps; an output props' word 0.
fn new_props(p: Props) -> u32 {
    let v = vts();
    let b = mem::heap_alloc(2 * ps(), true);
    match p {
        Props::Stream(_) => {
            wp(b, v.config);
            wp(b + ps(), v.props);
        }
        Props::Output(_) => wp(b, v.props),
    }
    props(|m| m.insert(b, PropsObj { refs: 1, p }));
    b
}

fn props_block(p: u32) -> Option<u32> {
    props(|m| [p, p.wrapping_sub(ps())].into_iter().find(|b| m.get(b).is_some_and(|o| *b == p || matches!(o.p, Props::Stream(_)))))
}

/// The type of props `p` when they are ours.
fn props_type(p: u32) -> Option<MediaType> {
    let b = props_block(p)?;
    props(|m| match &m.get(&b)?.p {
        Props::Stream(s) => Some(stream_type(s)),
        Props::Output(t) => Some(t.clone()),
    })
}

fn sc_qi(c: &mut Ctx) -> u32 {
    let (this, riid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let Some(b) = props_block(this) else { return E_NOINTERFACE };
    let want = guid_at(riid).unwrap_or([0xff; 16]);
    let stream = props(|m| matches!(m.get(&b).map(|o| &o.p), Some(Props::Stream(_))));
    let w = if want == IID_IUNKNOWN {
        Some(0)
    } else if stream && want == IID_STREAM_CONFIG {
        Some(0)
    } else if stream && want == IID_MEDIA_PROPS {
        Some(1)
    } else if !stream && (want == IID_MEDIA_PROPS || want == IID_OUTPUT_MEDIA_PROPS) {
        Some(0)
    } else {
        None
    };
    let Some(w) = w else {
        putp(out, 0);
        return E_NOINTERFACE;
    };
    props(|m| m.get_mut(&b).map(|o| o.refs += 1));
    putp(out, b + ps() * w);
    0
}

fn com_add_ref(c: &mut Ctx) -> u32 {
    let Some(b) = props_block(c.a(0)) else { return 1 };
    props(|m| m.get_mut(&b).map(|o| {
        o.refs += 1;
        o.refs
    }))
    .unwrap_or(1)
}

fn sc_release(c: &mut Ctx) -> u32 {
    let Some(b) = props_block(c.a(0)) else { return 0 };
    props(|m| {
        let Some(o) = m.get_mut(&b) else { return 0 };
        o.refs = o.refs.saturating_sub(1);
        if o.refs == 0 {
            m.remove(&b);
            return 0;
        }
        o.refs
    })
}

fn with_props<R>(p: u32, f: impl FnOnce(&mut Props) -> R) -> Option<R> {
    let b = props_block(p)?;
    props(|m| m.get_mut(&b).map(|o| f(&mut o.p)))
}

fn sc_get_stream_type(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    match with_props(c.a(0), |p| match p {
        Props::Stream(s) => stream_type(s).major,
        Props::Output(t) => t.major,
    }) {
        Some(g) => {
            put_guid(out, &g);
            0
        }
        None => E_UNEXPECTED,
    }
}

fn sc_get_stream_number(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    match with_props(c.a(0), |p| match p {
        Props::Stream(s) => s.number as u16,
        Props::Output(_) => 0,
    }) {
        Some(n) => {
            w16(out, n);
            0
        }
        None => E_UNEXPECTED,
    }
}

/// GetStreamName / GetConnectionName(WCHAR *, WORD *chars): "", as files without names have
fn sc_get_name(c: &mut Ctx) -> u32 {
    let (buf, n) = (c.a(1), c.a(2));
    if n == 0 {
        return E_POINTER;
    }
    if buf != 0 && r16(n) >= 1 {
        w16(buf, 0);
    }
    w16(n, 1);
    0
}

fn sc_get_bitrate(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let v = with_props(c.a(0), |p| match p {
        Props::Stream(s) => s.bitrate,
        Props::Output(_) => 0,
    })
    .unwrap_or(0);
    w32(out, v);
    0
}

fn sc_get_buffer_window(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    w32(out, 3000);
    0
}

fn mp_get_type(c: &mut Ctx) -> u32 {
    sc_get_stream_type(c)
}

/// GetMediaType(WM_MEDIA_TYPE *, DWORD *size): NULL first gives the size
fn mp_get_media_type(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(1), c.a(2));
    if n == 0 {
        return E_POINTER;
    }
    let Some(t) = props_type(c.a(0)) else { return E_UNEXPECTED };
    let need = mt_size() + t.format.len() as u32;
    if p == 0 {
        w32(n, need);
        return 0;
    }
    if r32(n) < need {
        w32(n, need);
        return ASF_E_BUFFERTOOSMALL;
    }
    t.write(p, p + mt_size());
    w32(n, need);
    0
}

/// SetMediaType(WM_MEDIA_TYPE *)
fn mp_set_media_type(c: &mut Ctx) -> u32 {
    let Some(t) = MediaType::read(c.a(1)) else { return E_POINTER };
    match with_props(c.a(0), |p| match p {
        Props::Output(o) => {
            *o = t;
            0
        }
        Props::Stream(_) => E_NOTIMPL,
    }) {
        Some(hr) => hr,
        None => E_UNEXPECTED,
    }
}

/// GetStreamGroupName / GetConnectionName: ""
fn mp_get_name(c: &mut Ctx) -> u32 {
    sc_get_name(c)
}

// ---- INSSBuffer: our samples (word 0 the vtable; the bytes on the guest heap)

struct Buf {
    refs: u32,
    data: u32,
    len: u32,
    max: u32,
}

static BUFS: Mutex<Option<HashMap<u32, Buf>>> = Mutex::new(None);

fn bufs<R>(f: impl FnOnce(&mut HashMap<u32, Buf>) -> R) -> R {
    f(BUFS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

pub fn new_buffer(data: &[u8]) -> u32 {
    let b = mem::heap_alloc(2 * ps(), true);
    wp(b, vts().buffer);
    let n = data.len() as u32;
    let d = mem::heap_alloc(n.max(1), false);
    mem::write(d, data);
    bufs(|m| m.insert(b, Buf { refs: 1, data: d, len: n, max: n }));
    b
}

fn buf_qi(c: &mut Ctx) -> u32 {
    let (this, riid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let want = guid_at(riid).unwrap_or([0xff; 16]);
    if want != IID_IUNKNOWN && want != IID_NSSBUFFER {
        putp(out, 0);
        return E_NOINTERFACE;
    }
    bufs(|m| m.get_mut(&this).map(|o| o.refs += 1));
    putp(out, this);
    0
}

fn buf_add_ref(c: &mut Ctx) -> u32 {
    bufs(|m| m.get_mut(&c.a(0)).map(|o| {
        o.refs += 1;
        o.refs
    }))
    .unwrap_or(1)
}

fn buf_release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let (left, free) = bufs(|m| {
        let Some(o) = m.get_mut(&this) else { return (0, None) };
        o.refs = o.refs.saturating_sub(1);
        if o.refs > 0 {
            return (o.refs, None);
        }
        (0, m.remove(&this).map(|o| o.data))
    });
    if let Some(d) = free {
        mem::heap_free(d);
        mem::heap_free(this);
    }
    left
}

fn buf_get_length(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_INVALIDARG;
    }
    let Some(n) = bufs(|m| m.get(&c.a(0)).map(|o| o.len)) else { return E_UNEXPECTED };
    w32(c.a(1), n);
    0
}

fn buf_set_length(c: &mut Ctx) -> u32 {
    let n = c.a(1);
    bufs(|m| match m.get_mut(&c.a(0)) {
        Some(o) if n <= o.max => {
            o.len = n;
            0
        }
        Some(_) => E_INVALIDARG,
        None => E_UNEXPECTED,
    })
}

fn buf_get_max_length(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_INVALIDARG;
    }
    let Some(n) = bufs(|m| m.get(&c.a(0)).map(|o| o.max)) else { return E_UNEXPECTED };
    w32(c.a(1), n);
    0
}

fn buf_get_buffer(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_INVALIDARG;
    }
    let Some(d) = bufs(|m| m.get(&c.a(0)).map(|o| o.data)) else { return E_UNEXPECTED };
    putp(c.a(1), d);
    0
}

fn buf_get_buffer_and_length(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 || c.a(2) == 0 {
        return E_INVALIDARG;
    }
    let Some((d, n)) = bufs(|m| m.get(&c.a(0)).map(|o| (o.data, o.len))) else { return E_UNEXPECTED };
    putp(c.a(1), d);
    w32(c.a(2), n);
    0
}

// ---------------------------------------------------------------------------------------------

pub fn init() {
    // WMCreateSyncReader(IUnknown *cert, DWORD rights, IWMSyncReader **)
    implement("wmvcore", "WMCreateSyncReader", |c| {
        let out = c.a(2);
        if out == 0 {
            return E_INVALIDARG;
        }
        putp(out, new_sync_reader());
        0
    });
}

#[cfg(test)]
mod tests;
