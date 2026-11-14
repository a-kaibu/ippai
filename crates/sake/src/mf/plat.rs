//! mfplat's helpers around the Source Reader, as Unity's VideoPlayer (Unity 2020.3 x64) looks
//! them up before it says "Initializing Microsoft Media Foundation failed" (GetProcAddress of
//! each; one NULL and the movies are off):
//!
//! - asynchronous results (MFCreateAsyncResult, the MFASYNCRESULT block games read the
//!   callback from) and MFInvokeCallback: the callback's Invoke on a work-queue thread of ours,
//!   in the order the results were put
//! - file byte streams (MFCreateFile): IMFByteStream on a file, synchronous and Begin/End
//! - MFGetStrideForBitmapInfoHeader, MFCalculateImageSize, MFAverageTimePerFrameToFrameRate,
//!   MFFrameRateToAverageTimePerFrame (Windows' tables; Wine's mediatype.c has them too)
//! - MFTEnumEx: the decoders there are (H.264, AAC, MP3: the page's)
//! - MFCreateVideoSampleAllocatorEx: none (D3D11 frames are not given out: the reader hands
//!   out memory buffers, Unity then copies them into its textures)

use super::*;
use std::collections::VecDeque;

const MF_E_INVALIDMEDIATYPE_: u32 = 0xc00d_36b4;
const MF_E_UNEXPECTED: u32 = 0xc00d_36bb;

// --- asynchronous results ---------------------------------------------------------------------

/// What an async result holds besides its block: references, object, state, status, and what an
/// operation of ours left in it for its End call (bytes read or written).
struct Result {
    refs: u32,
    object: u32,
    state: u32,
    status: u32,
    pub done: u32,
}
static RESULTS: Mutex<Option<HashMap<u32, Result>>> = Mutex::new(None);
fn result<R>(b: u32, f: impl FnOnce(&mut Result) -> R) -> Option<R> {
    RESULTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

/// MFASYNCRESULT: the vtable, an OVERLAPPED, pCallback, hEvent
fn callback_slot(b: u32) -> u32 {
    b + ps() + if mem::x64() { 32 } else { 20 }
}
pub fn result_callback(b: u32) -> u32 {
    rp(callback_slot(b))
}

fn ar_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFAsyncResult {ac6b7889-0740-4d51-8619-905994a55cc6}
    if is_unknown(&g) || is(&g, 0xac6b_7889, 0x0740, 0x4d51) {
        result(this, |r| r.refs += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn ar_add_ref(c: &mut Ctx) -> u32 {
    result(c.a(0), |r| {
        r.refs += 1;
        r.refs
    })
    .unwrap_or(1)
}
fn ar_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = result(b, |r| {
        r.refs = r.refs.saturating_sub(1);
        r.refs
    })
    .unwrap_or(0);
    if n == 0 {
        let gone = RESULTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
        if let Some(r) = gone {
            release(c, r.object);
            release(c, r.state);
            release(c, result_callback(b));
            let ev = rp(callback_slot(b) + ps());
            if ev != 0 {
                crate::obj::close(ev);
            }
            mem::heap_free(b);
        }
    }
    n
}
/// GetState(IUnknown **) / GetObject(IUnknown **): E_POINTER when there is none (Windows', Wine's)
fn ar_get(c: &mut Ctx, which: fn(&Result) -> u32) -> u32 {
    let out = c.a(1);
    let v = result(c.a(0), |r| which(r)).unwrap_or(0);
    if out == 0 {
        return E_POINTER;
    }
    if v == 0 {
        putp(out, 0);
        return E_POINTER;
    }
    add_ref(c, v);
    putp(out, v);
    0
}
fn ar_state(c: &mut Ctx) -> u32 {
    ar_get(c, |r| r.state)
}
fn ar_object(c: &mut Ctx) -> u32 {
    ar_get(c, |r| r.object)
}
fn ar_status(c: &mut Ctx) -> u32 {
    result(c.a(0), |r| r.status).unwrap_or(E_POINTER)
}
fn ar_set_status(c: &mut Ctx) -> u32 {
    let hr = c.a(1);
    result(c.a(0), |r| r.status = hr);
    0
}
fn ar_state_noref(c: &mut Ctx) -> u32 {
    result(c.a(0), |r| r.state).unwrap_or(0)
}

fn result_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        vtable(
            "IMFAsyncResult",
            &[
                (ar_qi as Handler, 12),
                (ar_add_ref, 4),
                (ar_release, 4),
                (ar_state, 8),
                (ar_status, 4),
                (ar_set_status, 8),
                (ar_object, 8),
                (ar_state_noref, 4),
            ],
        )
    })
}

/// A new async result (references taken of all three).
pub fn new_result(c: &mut Ctx, object: u32, callback: u32, state: u32) -> u32 {
    let size = callback_slot(0) + 2 * ps();
    let b = mem::heap_alloc(size, true);
    wp(b, result_vtable());
    wp(callback_slot(b), callback);
    add_ref(c, object);
    add_ref(c, callback);
    add_ref(c, state);
    RESULTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Result { refs: 1, object, state, status: 0, done: 0 });
    b
}
pub fn set_done(b: u32, n: u32, status: u32) {
    result(b, |r| {
        r.done = n;
        r.status = status;
    });
}
/// Result `b`'s object becomes `o` (its reference handed over; the one before released).
pub fn set_object(c: &mut Ctx, b: u32, o: u32) {
    let old = result(b, |r| std::mem::replace(&mut r.object, o));
    match old {
        Some(old) => release(c, old),
        None => release(c, o),
    }
}
/// Result `b`'s object (no reference taken).
pub fn object_of(b: u32) -> u32 {
    result(b, |r| r.object).unwrap_or(0)
}
pub fn done_of(b: u32) -> Option<(u32, u32)> {
    result(b, |r| (r.done, r.status))
}

/// MFCreateAsyncResult(IUnknown *object, IMFAsyncCallback *, IUnknown *state, IMFAsyncResult **)
fn MFCreateAsyncResult(c: &mut Ctx) -> u32 {
    let (o, cb, st, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_INVALIDARG;
    }
    let b = new_result(c, o, cb, st);
    putp(out, b);
    0
}

// --- the work queue: callbacks invoked in order on a thread of ours ---------------------------

struct Queue {
    items: VecDeque<u32>,
    busy: bool,
}
static QUEUE: Mutex<Queue> = Mutex::new(Queue { items: VecDeque::new(), busy: false });

/// Puts result `r` (a reference taken here) on the work queue: its callback's Invoke(r) runs on
/// the queue's thread, after the results put before it.
pub fn invoke(c: &mut Ctx, r: u32) {
    add_ref(c, r);
    let start = {
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        q.items.push_back(r);
        !std::mem::replace(&mut q.busy, true)
    };
    if start {
        static ENTRY: OnceLock<u32> = OnceLock::new();
        let entry = *ENTRY.get_or_init(|| crate::api::host_fn("mf_work_queue", queue_thread, 4));
        let h = crate::obj::new_thread_obj(0);
        crate::spawn_gated(entry, 0, 0x10000, h, false);
    }
}
fn next_item() -> Option<u32> {
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let r = q.items.pop_front();
    if r.is_none() {
        q.busy = false;
    }
    r
}
fn queue_thread(c: &mut Ctx) -> u32 {
    while let Some(r) = next_item() {
        let cb = result_callback(r);
        if cb != 0 {
            add_ref(c, cb);
            // IMFAsyncCallback: GetParameters 3, Invoke 4
            com(c, cb, 4, &[r]);
            release(c, cb);
        }
        release(c, r);
        crate::snap::point();
    }
    0
}

/// MFInvokeCallback(IMFAsyncResult *)
fn MFInvokeCallback(c: &mut Ctx) -> u32 {
    let r = c.a(0);
    if r == 0 {
        return E_INVALIDARG;
    }
    invoke(c, r);
    0
}

/// MFPutWorkItem(DWORD queue, IMFAsyncCallback *, IUnknown *state) and MFPutWorkItemEx(queue,
/// IMFAsyncResult *): our one queue whatever the queue.
fn MFPutWorkItem(c: &mut Ctx) -> u32 {
    let (cb, st) = (c.a(1), c.a(2));
    if cb == 0 {
        return E_INVALIDARG;
    }
    let r = new_result(c, 0, cb, st);
    invoke(c, r);
    release(c, r);
    0
}
fn MFPutWorkItemEx(c: &mut Ctx) -> u32 {
    let r = c.a(1);
    if r == 0 {
        return E_INVALIDARG;
    }
    invoke(c, r);
    0
}

// --- file byte streams ----------------------------------------------------------------------

struct Bs {
    refs: u32,
    file: Option<kura::fs::File>,
    pos: u64,
    caps: u32,
}
static STREAMS: Mutex<Option<HashMap<u32, Bs>>> = Mutex::new(None);
fn bs<R>(b: u32, f: impl FnOnce(&mut Bs) -> R) -> Option<R> {
    STREAMS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
const CAN_READ: u32 = 1;
const CAN_WRITE: u32 = 2;
const CAN_SEEK: u32 = 4;
/// MF_E_INVALID_STREAM_STATE? Windows answers a closed stream with this
const CLOSED: u32 = MF_E_SHUTDOWN;

fn bs_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFByteStream {ad4c1b00-4bf7-422f-9175-756693d9130d}
    if is_unknown(&g) || is(&g, 0xad4c_1b00, 0x4bf7, 0x422f) {
        bs(this, |s| s.refs += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn bs_add_ref(c: &mut Ctx) -> u32 {
    bs(c.a(0), |s| {
        s.refs += 1;
        s.refs
    })
    .unwrap_or(1)
}
fn bs_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = bs(b, |s| {
        s.refs = s.refs.saturating_sub(1);
        s.refs
    })
    .unwrap_or(0);
    if n == 0 && STREAMS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b).is_some() {
        mem::heap_free(b);
    }
    n
}
fn bs_caps(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    w32(out, bs(c.a(0), |s| s.caps).unwrap_or(0));
    0
}
fn len_of(s: &Bs) -> Option<u64> {
    s.file.as_ref().and_then(|f| f.metadata().ok()).map(|m| m.len())
}
fn bs_len(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    match bs(c.a(0), |s| len_of(s)).flatten() {
        Some(n) => {
            w64(out, n);
            0
        }
        None => CLOSED,
    }
}
fn bs_set_len(c: &mut Ctx) -> u32 {
    let n = c.q(1);
    bs(c.a(0), |s| match &s.file {
        Some(f) if s.caps & CAN_WRITE != 0 => {
            if f.set_len(n).is_ok() {
                0
            } else {
                E_FAIL
            }
        }
        Some(_) => 0x8007_0005, // E_ACCESSDENIED
        None => CLOSED,
    })
    .unwrap_or(E_INVALIDARG)
}
fn bs_pos(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_INVALIDARG;
    }
    w64(out, bs(c.a(0), |s| s.pos).unwrap_or(0));
    0
}
fn bs_set_pos(c: &mut Ctx) -> u32 {
    let p = c.q(1);
    bs(c.a(0), |s| s.pos = p);
    0
}
fn bs_eos(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_INVALIDARG;
    }
    let end = bs(c.a(0), |s| len_of(s).is_none_or(|n| s.pos >= n)).unwrap_or(true);
    w32(out, end as u32);
    0
}
/// Reads `n` bytes at the stream's position into guest memory `p`; the bytes read.
fn read_into(b: u32, p: u32, n: u32) -> Result2 {
    use std::io::{Read, Seek, SeekFrom};
    bs(b, |s| {
        let pos = s.pos;
        let Some(f) = s.file.as_mut() else { return Err(CLOSED) };
        f.seek(SeekFrom::Start(pos)).map_err(|_| E_FAIL)?;
        let Some(buf) = mem::guest_mut(p, n) else { return Err(E_FAIL) };
        let mut got = 0usize;
        while got < buf.len() {
            match f.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(_) => return Err(E_FAIL),
            }
        }
        s.pos += got as u64;
        Ok(got as u32)
    })
    .unwrap_or(Err(E_INVALIDARG))
}
type Result2 = std::result::Result<u32, u32>;
fn write_from(b: u32, p: u32, n: u32) -> Result2 {
    use std::io::{Seek, SeekFrom, Write};
    bs(b, |s| {
        if s.caps & CAN_WRITE == 0 {
            return Err(0x8007_0005);
        }
        let pos = s.pos;
        let Some(f) = s.file.as_mut() else { return Err(CLOSED) };
        f.seek(SeekFrom::Start(pos)).map_err(|_| E_FAIL)?;
        f.write_all(mem::slice(p, n)).map_err(|_| E_FAIL)?;
        s.pos += n as u64;
        Ok(n)
    })
    .unwrap_or(Err(E_INVALIDARG))
}
/// Read(BYTE *, ULONG, ULONG *read) / Write(const BYTE *, ULONG, ULONG *written)
fn bs_rw(c: &mut Ctx, f: fn(u32, u32, u32) -> Result2) -> u32 {
    let (b, p, n, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if p == 0 && n != 0 {
        return E_INVALIDARG;
    }
    match f(b, p, n) {
        Ok(k) => {
            if out != 0 {
                w32(out, k);
            }
            0
        }
        Err(e) => e,
    }
}
fn bs_read(c: &mut Ctx) -> u32 {
    bs_rw(c, read_into)
}
fn bs_write(c: &mut Ctx) -> u32 {
    bs_rw(c, write_from)
}
/// BeginRead / BeginWrite(BYTE *, ULONG, IMFAsyncCallback *, IUnknown *state): done at once,
/// the callback told on the work queue; End* gives the count.
fn bs_begin(c: &mut Ctx, f: fn(u32, u32, u32) -> Result2) -> u32 {
    let (b, p, n, cb, st) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if cb == 0 || (p == 0 && n != 0) {
        return E_INVALIDARG;
    }
    let r = new_result(c, b, cb, st);
    match f(b, p, n) {
        Ok(k) => set_done(r, k, 0),
        Err(e) => set_done(r, 0, e),
    }
    invoke(c, r);
    release(c, r);
    0
}
fn bs_begin_read(c: &mut Ctx) -> u32 {
    bs_begin(c, read_into)
}
fn bs_begin_write(c: &mut Ctx) -> u32 {
    bs_begin(c, write_from)
}
/// EndRead / EndWrite(IMFAsyncResult *, ULONG *)
fn bs_end(c: &mut Ctx) -> u32 {
    let (r, out) = (c.a(1), c.a(2));
    if r == 0 || out == 0 {
        return E_INVALIDARG;
    }
    let Some((n, hr)) = done_of(r) else { return E_INVALIDARG };
    w32(out, n);
    hr
}
/// Seek(MFBYTESTREAM_SEEK_ORIGIN, LONGLONG, DWORD flags, QWORD *current)
fn bs_seek(c: &mut Ctx) -> u32 {
    let (origin, off, out) = if mem::x64() { (c.a(1), c.a64(2) as i64, c.a(4)) } else { (c.a(1), c.q(2) as i64, c.a(5)) };
    let r = bs(c.a(0), |s| {
        let base = match origin {
            0 => 0i64,
            1 => s.pos as i64,
            _ => return Err(E_INVALIDARG),
        };
        let p = base.checked_add(off).filter(|&p| p >= 0).ok_or(E_INVALIDARG)?;
        s.pos = p as u64;
        Ok(s.pos)
    })
    .unwrap_or(Err(E_INVALIDARG));
    match r {
        Ok(p) => {
            if out != 0 {
                w64(out, p);
            }
            0
        }
        Err(e) => e,
    }
}
fn bs_flush(c: &mut Ctx) -> u32 {
    use std::io::Write;
    bs(c.a(0), |s| {
        if let Some(f) = s.file.as_mut() {
            let _ = f.flush();
        }
    });
    0
}
fn bs_close(c: &mut Ctx) -> u32 {
    bs(c.a(0), |s| s.file = None);
    0
}

fn stream_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        vtable(
            "IMFByteStream",
            &[
                (bs_qi as Handler, 12),
                (bs_add_ref, 4),
                (bs_release, 4),
                (bs_caps, 8),
                (bs_len, 8),
                (bs_set_len, 12),
                (bs_pos, 8),
                (bs_set_pos, 12),
                (bs_eos, 8),
                (bs_read, 16),
                (bs_begin_read, 20),
                (bs_end, 12),
                (bs_write, 16),
                (bs_begin_write, 20),
                (bs_end, 12),
                (bs_seek, 24),
                (bs_flush, 4),
                (bs_close, 4),
            ],
        )
    })
}

/// A byte stream of ours on host file `f`.
pub fn new_file_stream(f: kura::fs::File, caps: u32) -> u32 {
    let b = object(stream_vtable(), 1);
    STREAMS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Bs { refs: 1, file: Some(f), pos: 0, caps });
    b
}

/// MFCreateFile(MF_FILE_ACCESSMODE, MF_FILE_OPENMODE, MF_FILE_FLAGS, LPCWSTR url, IMFByteStream **)
fn MFCreateFile(c: &mut Ctx) -> u32 {
    let (access, mode, url, out) = (c.a(0), c.a(1), c.a(3), c.a(4));
    if url == 0 || out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let path = reader::file_url_path(&String::from_utf16_lossy(&mem::wstr(url)));
    let (rd, wr) = (access & 1 != 0, access & 2 != 0);
    if !rd && !wr {
        return E_INVALIDARG;
    }
    let host = crate::files::resolve(&path);
    // MF_OPENMODE_FAIL_IF_NOT_EXIST 0, FAIL_IF_EXIST 1, RESET_IF_EXIST 2, APPEND_IF_EXIST 3,
    // DELETE_IF_EXIST 4
    let exists = host.as_ref().is_some_and(|h| kura::fs::metadata(h).is_ok_and(|m| m.is_file()));
    let f = match (mode, exists) {
        (0, false) => return 0x8007_0000 | crate::files::missing(&path),
        (1, true) => return 0x8007_0050, // ERROR_FILE_EXISTS
        (0 | 3, true) if !wr => kura::fs::File::open(host.unwrap()),
        (0..=4, _) => {
            let h = host.unwrap_or_else(|| crate::files::resolve_new(&path));
            kura::fs::OpenOptions::new()
                .read(rd)
                .write(wr)
                .create(mode != 0)
                .truncate(mode == 2 || mode == 4)
                .open(h)
        }
        _ => return E_INVALIDARG,
    };
    let Ok(f) = f else { return 0x8007_0005 };
    let caps = if rd { CAN_READ } else { 0 } | if wr { CAN_WRITE } else { 0 } | CAN_SEEK;
    let b = new_file_stream(f, caps);
    if mode == 3 {
        let n = bs(b, |s| len_of(s).unwrap_or(0)).unwrap_or(0);
        bs(b, |s| s.pos = n);
    }
    putp(out, b);
    0
}

// --- video format arithmetic --------------------------------------------------------------

/// A video subtype's (bits per pixel, alignment mask, bottom-up): Windows' table (Wine's
/// mediatype.c video_formats). By the subtype's Data1, a FOURCC or a D3DFORMAT.
pub fn video_format(d1: u32) -> Option<(u32, u32, bool)> {
    const fn cc(s: &[u8; 4]) -> u32 {
        u32::from_le_bytes(*s)
    }
    Some(match d1 {
        // RGB1, RGB4 (MFVideoFormat_RGB1/4 are not D3DFORMATs: their own GUIDs, by Data1)
        0xe436_eb78 => (1, 0, true),
        0xe436_eb79 => (4, 0, true),
        20 => (24, 3, true),           // RGB24 (D3DFMT_R8G8B8)
        21 | 22 => (32, 3, true),      // ARGB32, RGB32
        23 | 24 => (16, 3, true),      // RGB565, RGB555 (D3DFMT_X1R5G5B5 = 24)
        32 => (32, 3, true),           // ABGR32 (D3DFMT_A8B8G8R8)
        31 | 35 => (32, 3, true),      // A2R10G10B10, A2B10G10R10
        41 => (8, 3, true),            // RGB8 (D3DFMT_P8)
        50 => (8, 3, true),            // L8
        80 | 81 => (16, 3, false),     // D16, L16
        113 => (64, 3, true),          // A16B16G16R16F
        x if x == cc(b"AYUV") => (32, 3, false),
        x if x == cc(b"I420") || x == cc(b"IYUV") || x == cc(b"NV12") || x == cc(b"NV11") || x == cc(b"YV12") => (12, 0, false),
        x if x == cc(b"IMC2") || x == cc(b"IMC4") => (12, 0, false),
        x if x == cc(b"IMC1") || x == cc(b"IMC3") => (16, 3, false),
        x if x == cc(b"P010") => (24, 0, false),
        x if x == cc(b"UYVY") || x == cc(b"YUY2") || x == cc(b"YVYU") => (16, 0, false),
        _ => return None,
    })
}
/// The stride of a row `width` pixels wide (negative: bottom-up), as MFGetStrideForBitmapInfoHeader.
pub fn stride_for(d1: u32, width: u32) -> Option<i32> {
    let (bpp, align, up) = video_format(d1)?;
    let s = if bpp < 8 {
        width * bpp / 8
    } else if d1 == u32::from_le_bytes(*b"P010") {
        width * 2
    } else {
        (width * (bpp / 8) + align) & !align
    } as i32;
    Some(if up { -s } else { s })
}
/// MFGetStrideForBitmapInfoHeader(DWORD format, DWORD width, LONG *stride)
fn MFGetStrideForBitmapInfoHeader(c: &mut Ctx) -> u32 {
    let (f, w, out) = (c.a(0), c.a(1), c.a(2));
    match stride_for(f, w) {
        Some(s) => {
            w32(out, s as u32);
            0
        }
        None => {
            w32(out, 0);
            MF_E_INVALIDMEDIATYPE_
        }
    }
}
/// Bytes of a `w` x `h` image of a subtype (MFCalculateImageSize; Wine's mediatype.c).
pub fn image_size(d1: u32, w: u32, h: u32) -> Option<u32> {
    let (bpp, _, _) = video_format(d1)?;
    let cc = |s: &[u8; 4]| u32::from_le_bytes(*s);
    Some(if [cc(b"IMC2"), cc(b"IMC4"), cc(b"NV12"), cc(b"YV12"), cc(b"I420"), cc(b"IYUV")].contains(&d1) {
        // 2x2 blocks, the chroma interleaved for half the height
        w.next_multiple_of(2) * h * 3 / 2
    } else if d1 == cc(b"P010") {
        w.next_multiple_of(2) * h * 3
    } else if d1 == cc(b"NV11") {
        w.next_multiple_of(4) * h * 3 / 2
    } else if matches!(d1, 50 | 80 | 81) {
        w * (bpp / 8) * h
    } else {
        stride_for(d1, w)?.unsigned_abs() * h
    })
}
/// MFCalculateImageSize(REFGUID subtype, UINT32 width, UINT32 height, UINT32 *size)
fn MFCalculateImageSize(c: &mut Ctx) -> u32 {
    let (g, w, h, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if h == 0 {
        return E_INVALIDARG;
    }
    let g = guid_at(g);
    let d1 = u32::from_le_bytes(g[0..4].try_into().unwrap());
    match video_format(d1).and(image_size(d1, w, h)) {
        Some(s) => {
            w32(out, s);
            0
        }
        None => {
            w32(out, 0);
            E_INVALIDARG
        }
    }
}

/// Windows' frame rates by their average frame times (100 ns), each matched within 30 units.
const KNOWN_RATES: [(u64, u32, u32); 8] = [
    (417_188, 24000, 1001),
    (416_667, 24, 1),
    (400_000, 25, 1),
    (333_667, 30000, 1001),
    (333_333, 30, 1),
    (200_000, 50, 1),
    (166_833, 60000, 1001),
    (166_667, 60, 1),
];
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
/// MFAverageTimePerFrameToFrameRate's answer
pub fn frame_rate(avg: u64) -> (u32, u32) {
    if let Some(&(_, n, d)) = KNOWN_RATES.iter().find(|r| avg + 30 >= r.0 && avg <= r.0 + 30) {
        return (n, d);
    }
    if avg == 0 {
        return (0, 0);
    }
    let t = avg.min(100_000_000);
    let g = gcd(10_000_000, t);
    ((10_000_000 / g) as u32, (t / g) as u32)
}
/// MFFrameRateToAverageTimePerFrame's answer
pub fn frame_time(n: u32, d: u32) -> u64 {
    if let Some(&(t, _, _)) = KNOWN_RATES.iter().find(|r| r.1 == n && r.2 == d) {
        return t;
    }
    if n == 0 { 0 } else { d as u64 * 10_000_000 / n as u64 }
}
/// MFAverageTimePerFrameToFrameRate(UINT64, UINT32 *num, UINT32 *den)
fn MFAverageTimePerFrameToFrameRate(c: &mut Ctx) -> u32 {
    let (avg, k) = (c.q(0), if mem::x64() { 1 } else { 2 });
    let (n, d) = frame_rate(avg);
    w32(c.a(k), n);
    w32(c.a(k + 1), d);
    0
}
/// MFFrameRateToAverageTimePerFrame(UINT32 num, UINT32 den, UINT64 *)
fn MFFrameRateToAverageTimePerFrame(c: &mut Ctx) -> u32 {
    let t = frame_time(c.a(0), c.a(1));
    w64(c.a(2), t);
    0
}

// --- transforms -----------------------------------------------------------------------------

/// The decoders MFTEnumEx lists: category Data1, input subtype Data1, friendly name.
const DECODERS: [(u32, u32, &str); 3] = [
    (0xd6c0_2d4b, 0x3436_3248, "Microsoft H264 Video Decoder MFT"), // H264
    (0x9ea7_3fb4, 0x1610, "Microsoft AAC Audio Decoder MFT"),       // MFAudioFormat_AAC
    (0x9ea7_3fb4, 0x55, "MP3 Decoder MFT"),                         // MFAudioFormat_MP3
];
/// MFT_FRIENDLY_NAME_Attribute {314ffbae-5b41-4c95-9c19-4e7d586face3}
fn k_friendly() -> Guid {
    [0xae, 0xfb, 0x4f, 0x31, 0x41, 0x5b, 0x95, 0x4c, 0x9c, 0x19, 0x4e, 0x7d, 0x58, 0x6f, 0xac, 0xe3]
}
/// The decoders matching category `cat` and input subtype `input` (None: any).
pub fn decoders(cat: &Guid, input: Option<&Guid>) -> Vec<&'static str> {
    DECODERS
        .iter()
        .filter(|d| u32::from_le_bytes(cat[0..4].try_into().unwrap()) == d.0)
        .filter(|d| input.is_none_or(|g| u32::from_le_bytes(g[0..4].try_into().unwrap()) == d.1))
        .map(|d| d.2)
        .collect()
}

/// IMFActivate of a decoder: an attribute store (its name) that activates nothing (the reader
/// decodes; programs that list decoders only look at them).
fn act_activate(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    if out != 0 {
        putp(out, 0);
    }
    MF_E_UNEXPECTED
}
fn activate_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m.push((act_activate, 16));
        m.push((ok, 4)); // ShutdownObject
        m.push((ok, 4)); // DetachObject
        vtable("IMFActivate", &m)
    })
}
fn new_activate(name: &str) -> u32 {
    let b = object(activate_vtable(), 1);
    let mut s = Store::default();
    s.set(k_friendly(), Val::Str(name.encode_utf16().collect()));
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, s));
    b
}

/// MFTEnumEx(GUID category, UINT32 flags, const MFT_REGISTER_TYPE_INFO *in, *out, IMFActivate
/// ***, UINT32 *count): GUID by value (x86: 4 slots; x64: its address)
fn MFTEnumEx(c: &mut Ctx) -> u32 {
    let cat = guid_at(c.byval(0));
    let k = if mem::x64() { 1 } else { 4 };
    let (input, list, count) = (c.a(k + 1), c.a(k + 3), c.a(k + 4));
    if list == 0 || count == 0 {
        return E_POINTER;
    }
    // MFT_REGISTER_TYPE_INFO: major type, subtype
    let sub = (input != 0).then(|| guid_at(input + 16));
    let found = decoders(&cat, sub.as_ref());
    w32(count, found.len() as u32);
    if found.is_empty() {
        putp(list, 0);
        return 0;
    }
    let arr = mem::heap_alloc(ps() * found.len() as u32, true);
    for (i, n) in found.iter().enumerate() {
        wp(arr + ps() * i as u32, new_activate(n));
    }
    putp(list, arr);
    0
}

/// MFCreateVideoSampleAllocatorEx(REFIID, void **): none (see the top)
fn MFCreateVideoSampleAllocatorEx(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out != 0 {
        putp(out, 0);
    }
    E_NOTIMPL
}

pub fn init() {
    implement("mfplat", "MFCreateAsyncResult", MFCreateAsyncResult);
    implement("mfplat", "MFInvokeCallback", MFInvokeCallback);
    implement("mfplat", "MFPutWorkItem", MFPutWorkItem);
    implement("mfplat", "MFPutWorkItemEx", MFPutWorkItemEx);
    implement("mfplat", "MFCreateFile", MFCreateFile);
    implement("mfplat", "MFGetStrideForBitmapInfoHeader", MFGetStrideForBitmapInfoHeader);
    implement("mfplat", "MFCalculateImageSize", MFCalculateImageSize);
    implement("mfplat", "MFAverageTimePerFrameToFrameRate", MFAverageTimePerFrameToFrameRate);
    implement("mfplat", "MFFrameRateToAverageTimePerFrame", MFFrameRateToAverageTimePerFrame);
    implement("mfplat", "MFTEnumEx", MFTEnumEx);
    implement("mfplat", "MFCreateVideoSampleAllocatorEx", MFCreateVideoSampleAllocatorEx);
}
