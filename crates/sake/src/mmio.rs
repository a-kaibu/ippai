//! winmm's multimedia file I/O (mmio*): RIFF files (WAVE, AVI headers) a game opens by name,
//! or a block of its memory (FOURCC_MEM). A file is read whole at open; one opened for writing
//! (MMIO_CREATE / MMIO_WRITE) is held in memory and written out at mmioFlush and mmioClose.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::mem::{self, r32, w32};

const MMIOERR_CANNOTOPEN: u32 = 257;
const MMIOERR_CHUNKNOTFOUND: u32 = 265;
const MMIOERR_CANNOTWRITE: u32 = 260;
const MMIOERR_INVALIDFILE: u32 = 272;
const MMIO_DIRTY: u32 = 0x1000_0000;
const MMIO_ALLOCBUF: u32 = 0x0001_0000;
/// MMIO_DEFAULTBUFFER
const DEFAULT_BUFFER: u32 = 8192;
const FOURCC_MEM: u32 = u32::from_le_bytes(*b"MEM ");
const FOURCC_DOS: u32 = u32::from_le_bytes(*b"DOS ");
const FOURCC_RIFF: u32 = u32::from_le_bytes(*b"RIFF");
const FOURCC_LIST: u32 = u32::from_le_bytes(*b"LIST");
/// The largest file mmio holds in memory for writing (a seek of a writable file to 3 GB and a
/// write there would grow it to that much host memory: refused instead, as a full disk).
const MAX_WRITE: u64 = 512 << 20;

/// Where the chunk after the one at `at` of `size` data bytes starts (8 header bytes, the data
/// padded to even), or None when that is not past `at` in a 32-bit file (a size near 4 GB:
/// the walk would come back to `at`, or before it, for ever).
fn chunk_after(at: u32, size: u32) -> Option<u32> {
    let next = at as u64 + 8 + ((size as u64 + 1) & !1);
    (next <= u32::MAX as u64).then_some(next as u32)
}

enum Data {
    File(Vec<u8>),
    /// guest memory: address, size
    Mem(u32, u32),
    /// an open file of the game's (DOS I/O procedure, adwInfo[0]), read where it lies: the file,
    /// the offset of position 0 in it, the length (read only; archives can be bigger than memory),
    /// the handle (mmioClose closes it unless MMIO_FHOPEN)
    Handle(std::sync::Arc<crate::files::File>, u64, u32, u32),
}

struct Mmio {
    data: Data,
    pos: u32,
    /// the file to write back to (opened for writing) and whether there is something to write
    out: Option<String>,
    dirty: bool,
    /// the I/O buffer (MMIO_ALLOCBUF, or made by mmioAdvance): guest address, size; the file
    /// offset its contents start at and how many bytes it holds
    buf: Option<(u32, u32)>,
    buf_off: u32,
    buf_len: u32,
}

/// MMIOINFO field offsets: (wErrorRet, cchBuffer, pchBuffer, pchNext, pchEndRead, lDiskOffset,
/// adwInfo, hmmio, size); pointers and HTASK are 8 bytes on x64, and mmsystem.h packs it to 1.
struct Info {
    err: u32,
    cch: u32,
    buf: u32,
    next: u32,
    end_read: u32,
    disk: u32,
    adw: u32,
    hmmio: u32,
    size: u32,
}
fn info() -> Info {
    if mem::ps() == 8 {
        Info { err: 16, cch: 28, buf: 32, next: 40, end_read: 48, disk: 68, adw: 72, hmmio: 92, size: 100 }
    } else {
        Info { err: 12, cch: 20, buf: 24, next: 28, end_read: 32, disk: 44, adw: 48, hmmio: 68, size: 72 }
    }
}

/// `n` bytes of `v` at `pos`, as many as it has (none past its end: mmioSeek goes there).
fn part(v: &[u8], pos: u32, n: u32) -> &[u8] {
    let at = (pos as usize).min(v.len());
    &v[at..at.saturating_add(n as usize).min(v.len())]
}

impl Mmio {
    /// The I/O buffer, made at first use (`want` bytes, 0: MMIO_DEFAULTBUFFER).
    fn buffer(&mut self, want: u32) -> (u32, u32) {
        *self.buf.get_or_insert_with(|| {
            let n = if want == 0 { DEFAULT_BUFFER } else { want };
            self.buf_off = self.pos;
            self.buf_len = 0;
            (mem::heap_alloc(n, true), n)
        })
    }
    /// MMIOINFO's buffer fields for the position: pchNext inside the bytes the buffer holds, or
    /// an emptied buffer at the position.
    fn put_buffer(&mut self, info: u32) {
        let Some((b, n)) = self.buf else { return };
        let mi = self::info();
        if self.pos < self.buf_off || self.pos as u64 > self.buf_off as u64 + self.buf_len as u64 {
            self.buf_off = self.pos;
            self.buf_len = 0;
        }
        w32(info + mi.cch, n);
        mem::wp(info + mi.buf, b);
        mem::wp(info + mi.next, b + (self.pos - self.buf_off));
        mem::wp(info + mi.end_read, b + self.buf_len);
        mem::wp(info + mi.end_read + mem::ps(), b + n);
        w32(info + mi.disk - 4, self.buf_off);
        w32(info + mi.disk, self.buf_off.wrapping_add(self.buf_len));
    }
    /// The position MMIOINFO's pchNext stands for (the program read from the buffer itself), if it
    /// points into this buffer's bytes.
    fn pos_of(&self, info: u32) -> Option<u32> {
        let (b, _) = self.buf?;
        let next = mem::rp(info + self::info().next);
        (next >= b && next as u64 <= b as u64 + self.buf_len as u64).then(|| self.buf_off.wrapping_add(next - b))
    }
    fn len(&self) -> u32 {
        match &self.data {
            Data::File(v) => v.len() as u32,
            Data::Mem(_, n) | Data::Handle(_, _, n, _) => *n,
        }
    }
    /// copies up to `n` bytes at the position to guest `dst`
    fn read(&mut self, dst: u32, n: u32) -> u32 {
        let n = n.min(self.len().saturating_sub(self.pos));
        match &self.data {
            Data::File(v) => mem::write(dst, part(v, self.pos, n)),
            Data::Mem(a, _) => mem::copy(dst, a + self.pos, n),
            Data::Handle(f, base, _, _) => {
                let got = f.seek(std::io::SeekFrom::Start(base + self.pos as u64)).map_or(0, |_| f.read_into(dst, n));
                self.pos += got;
                return got;
            }
        }
        self.pos += n;
        n
    }
    /// writes guest bytes at the position (a file grows; memory stops at its end)
    fn write(&mut self, src: u32, n: u32) -> u32 {
        let n = match &mut self.data {
            Data::File(v) => {
                let end = self.pos as u64 + n as u64;
                if end > MAX_WRITE {
                    return u32::MAX;
                }
                let end = end as usize;
                if v.len() < end {
                    v.resize(end, 0);
                }
                v[self.pos as usize..end].copy_from_slice(mem::slice(src, n));
                n
            }
            Data::Mem(a, len) => {
                let n = n.min(len.saturating_sub(self.pos));
                mem::copy(*a + self.pos, src, n);
                n
            }
            Data::Handle(..) => return 0,
        };
        self.pos += n;
        self.dirty = true;
        n
    }
    fn put_u32(&mut self, v: u32) {
        let t = v.to_le_bytes();
        match &mut self.data {
            Data::File(d) => {
                let end = self.pos as u64 + 4;
                if end > MAX_WRITE {
                    return;
                }
                let end = end as usize;
                if d.len() < end {
                    d.resize(end, 0);
                }
                d[self.pos as usize..end].copy_from_slice(&t);
            }
            Data::Mem(a, len) => {
                if self.pos as u64 + 4 <= *len as u64 {
                    w32(*a + self.pos, v);
                }
            }
            Data::Handle(..) => return,
        }
        self.pos = self.pos.wrapping_add(4);
        self.dirty = true;
    }
    fn flush(&mut self) -> bool {
        if let (Some(p), true, Data::File(v)) = (&self.out, self.dirty, &self.data) {
            let h = crate::files::resolve(p).unwrap_or_else(|| crate::files::resolve_new(p));
            if kura::fs::write(h, v).is_err() {
                return false;
            }
            self.dirty = false;
        }
        true
    }
    fn u32_at(&self, off: u32) -> Option<u32> {
        if off as u64 + 4 > self.len() as u64 {
            return None;
        }
        Some(match &self.data {
            Data::File(v) => u32::from_le_bytes(v[off as usize..off as usize + 4].try_into().unwrap()),
            Data::Mem(a, _) => r32(a + off),
            Data::Handle(f, base, _, _) => {
                f.seek(std::io::SeekFrom::Start(base + off as u64))?;
                u32::from_le_bytes(f.read_vec(4).try_into().ok()?)
            }
        })
    }
}

static FILES: Mutex<Option<HashMap<u32, Mmio>>> = Mutex::new(None);
static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x6d00);

fn with<R>(h: u32, f: impl FnOnce(&mut Mmio) -> R) -> Option<R> {
    FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default).get_mut(&h).map(f)
}

/// mmioOpen(name, MMIOINFO*, flags)
fn open(c: &mut Ctx, wide: bool) -> u32 {
    let (name, info, flags) = (c.a(0), c.a(1), c.a(2));
    let mi = self::info();
    let fail = |e: u32| {
        if info != 0 {
            w32(info + mi.err, e);
        }
        0
    };
    // deleting, parsing: not supported
    if flags & 0x0300 != 0 {
        return fail(MMIOERR_CANNOTOPEN);
    }
    let writing = flags & 3 != 0;
    let mut out = None;
    if crate::env_on!("SAKE_TRACE_FILES") && info != 0 {
        eprintln!("sake: mmioOpen({name:#x}, {info:#x}, flags {flags:#x}) {:x?}", (0..18).map(|i| r32(info + 4 * i)).collect::<Vec<_>>());
    }
    let proc = io_proc(name, info, wide);
    let data = if let Some(p) = proc {
        match read_via(c, p, name, info, flags) {
            Some(v) => Data::File(v),
            None => return fail(MMIOERR_CANNOTOPEN),
        }
    } else if name == 0 && info != 0 && matches!(r32(info + 4), 0 | FOURCC_DOS) && r32(info + mi.adw) != 0 {
        // adwInfo[0]: an open file's handle (with no I/O procedure or the DOS one named, as
        // RGSS2 opens Game.rgss2a), read from where it is (a RIFF in an archive)
        let Some(d) = from_handle(r32(info + mi.adw)) else {
            return fail(MMIOERR_CANNOTOPEN);
        };
        d
    } else if info != 0 && r32(info + 4) == FOURCC_MEM {
        Data::Mem(mem::rp(info + mi.buf), r32(info + mi.cch))
    } else {
        if name == 0 {
            return fail(MMIOERR_CANNOTOPEN);
        }
        let path = crate::k32::arg_str(name, wide);
        if flags & 0x5000 == 0x4000 {
            // MMIO_EXIST: only whether it is there (a file: not read)
            return crate::files::resolve(&path).and_then(|h| kura::fs::metadata(h).ok()).is_some_and(|m| !m.is_dir()) as u32;
        }
        if flags & 0x1000 != 0 {
            // MMIO_CREATE: a new, empty file
            let h = crate::files::resolve(&path).unwrap_or_else(|| crate::files::resolve_new(&path));
            if kura::fs::write(h, Vec::new()).is_err() {
                return fail(MMIOERR_CANNOTOPEN);
            }
            out = Some(path);
            Data::File(Vec::new())
        } else {
            let Some(v) = crate::files::resolve(&path).and_then(|h| kura::fs::read(h).ok()) else { return fail(MMIOERR_CANNOTOPEN) };
            if writing {
                out = Some(path);
            }
            Data::File(v)
        }
    };
    let h = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default).insert(h, Mmio { data, pos: 0, out, dirty: false, buf: None, buf_off: 0, buf_len: 0 });
    if flags & MMIO_ALLOCBUF != 0 {
        let want = if info != 0 { r32(info + mi.cch) } else { 0 };
        with(h, |f| f.buffer(want));
    }
    if info != 0 {
        w32(info + mi.err, 0);
        mem::wp(info + mi.hmmio, h);
    }
    h
}
/// The RIFF at an open file's position, else the rest of the file, read in place
fn from_handle(h: u32) -> Option<Data> {
    use std::io::SeekFrom;
    let f = crate::obj::file(h)?;
    let at = f.seek(SeekFrom::Current(0))?;
    let head = f.read_vec(8);
    f.seek(SeekFrom::Start(at))?;
    let rest = f.size().saturating_sub(at);
    Some(Data::Handle(f, at, handle_len(&head, rest), h))
}
/// The length an mmio file on a handle gets: a RIFF's (header included, within the file), else
/// the rest of the file (up to 4 GB - 1: mmio positions are 32-bit)
pub(crate) fn handle_len(head: &[u8], rest: u64) -> u32 {
    let rest = rest.min(u32::MAX as u64) as u32;
    match head {
        [b'R', b'I', b'F', b'F', a, b, c, d, ..] => u32::from_le_bytes([*a, *b, *c, *d]).saturating_add(8).min(rest),
        _ => rest,
    }
}

/// I/O procedures by FOURCC (mmioInstallIOProc)
static PROCS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// The game's I/O procedure for this open: MMIOINFO's pIOProc, its fccIOProc installed, or the
/// one installed for the name's "file.EXT+" extension.
fn io_proc(name: u32, info: u32, wide: bool) -> Option<u32> {
    let procs = PROCS.lock().unwrap();
    let find = |f: u32| procs.iter().rev().find(|&&(k, _)| k == f).map(|&(_, p)| p);
    if info != 0 {
        if r32(info + 8) != 0 {
            return Some(r32(info + 8));
        }
        let f = r32(info + 4);
        if f != 0 && f != FOURCC_MEM && f != FOURCC_DOS {
            if let Some(p) = find(f) {
                return Some(p);
            }
        }
    }
    if name != 0 {
        let path = crate::k32::arg_str(name, wide);
        let head = &path[..path.find('+')?];
        let ext = &head[head.rfind('.')? + 1..];
        // (as given, then upper-cased: mmioStringToFOURCC with or without MMIO_TOUPPER)
        let mut b = [b' '; 4];
        for (i, ch) in ext.bytes().take(4).enumerate() {
            b[i] = ch;
        }
        return find(u32::from_le_bytes(b)).or_else(|| find(u32::from_le_bytes(b.map(|c| c.to_ascii_uppercase()))));
    }
    None
}

/// The whole stream through the game's I/O procedure (MMIOM_OPEN, READs, MMIOM_CLOSE), in the
/// game's MMIOINFO (or one of ours): its files are small (sound effects in an archive).
fn read_via(c: &mut Ctx, proc: u32, name: u32, info: u32, flags: u32) -> Option<Vec<u8>> {
    const MMIOM_READ: u32 = 0;
    const MMIOM_OPEN: u32 = 3;
    const MMIOM_CLOSE: u32 = 4;
    let tmp = mem::alloc(0x10000 + 72)?;
    let mi = if info != 0 { info } else { tmp + 0x10000 };
    w32(mi + 8, proc);
    w32(mi + 44, 0);
    if c.call(proc, &[mi, MMIOM_OPEN, name, flags]) != 0 {
        mem::release(tmp);
        return None;
    }
    let mut v = Vec::new();
    loop {
        let n = c.call(proc, &[mi, MMIOM_READ, tmp, 0x10000]) as i32;
        if n <= 0 || v.len() > 256 << 20 {
            break;
        }
        // (no more than it was asked for: a procedure answering more would copy past `tmp`)
        v.extend_from_slice(mem::slice(tmp, (n as u32).min(0x10000)));
    }
    c.call(proc, &[mi, MMIOM_CLOSE, 0, 0]);
    mem::release(tmp);
    Some(v)
}
/// mmioInstallIOProc(fcc, proc, flags): INSTALL 0x10000, REMOVE 0x20000, FINDPROC 0x40000
fn mmioInstallIOProcA(c: &mut Ctx) -> u32 {
    let (fcc, proc, flags) = (c.a(0), c.a(1), c.a(2));
    let mut procs = PROCS.lock().unwrap();
    if flags & 0x10000 != 0 {
        procs.push((fcc, proc));
        return proc;
    }
    let i = procs.iter().rposition(|&(k, p)| k == fcc && (flags & 0x20000 == 0 || p == proc || proc == 0));
    match i {
        Some(i) if flags & 0x20000 != 0 => {
            procs.remove(i);
            proc
        }
        Some(i) => procs[i].1,
        None => 0,
    }
}
fn mmioOpenA(c: &mut Ctx) -> u32 {
    open(c, false)
}
fn mmioOpenW(c: &mut Ctx) -> u32 {
    open(c, true)
}
fn mmioClose(c: &mut Ctx) -> u32 {
    let f = FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default).remove(&c.a(0));
    match f {
        Some(mut f) => {
            // the I/O buffer goes with the file (MMIO_ALLOCBUF or mmioAdvance's)
            if let Some((b, _)) = f.buf.take() {
                mem::heap_free(b);
            }
            // the DOS I/O procedure closes the program's file too, unless MMIO_FHOPEN (0x10)
            if let Data::Handle(.., h) = f.data {
                if c.a(1) & 0x10 == 0 {
                    crate::obj::close(h);
                }
            }
            if f.flush() { 0 } else { MMIOERR_CANNOTWRITE }
        }
        None => MMIOERR_INVALIDFILE,
    }
}
fn mmioFlush(c: &mut Ctx) -> u32 {
    with(c.a(0), |f| if f.flush() { 0 } else { MMIOERR_CANNOTWRITE }).unwrap_or(MMIOERR_INVALIDFILE)
}
fn mmioRead(c: &mut Ctx) -> u32 {
    let (h, buf, n) = (c.a(0), c.a(1), c.i(2));
    with(h, |f| f.read(buf, n.max(0) as u32)).unwrap_or(u32::MAX)
}
fn mmioWrite(c: &mut Ctx) -> u32 {
    let (h, buf, n) = (c.a(0), c.a(1), c.i(2));
    with(h, |f| if f.out.is_none() && matches!(f.data, Data::File(_) | Data::Handle(..)) { u32::MAX } else { f.write(buf, n.max(0) as u32) }).unwrap_or(u32::MAX)
}
/// mmioSeek(h, offset, origin) -> new position
fn mmioSeek(c: &mut Ctx) -> u32 {
    let (h, off, origin) = (c.a(0), c.i(1), c.a(2));
    with(h, |f| {
        let base = match origin {
            1 => f.pos as i64,
            2 => f.len() as i64,
            _ => 0,
        };
        let p = base + off as i64;
        // memory stays within its buffer; a file seeks anywhere past its start and answers a
        // position before it (as Windows: -5 for SEEK_SET -5) without moving
        match f.data {
            Data::Mem(..) if p < 0 || p > f.len() as i64 => return u32::MAX,
            _ if p < 0 => return p as i32 as u32,
            _ => {}
        }
        f.pos = p as u32;
        f.pos
    })
    .unwrap_or(u32::MAX)
}

/// mmioDescend(h, MMCKINFO* ck, parent, flags): MMCKINFO = ckid, cksize, fccType, dwDataOffset,
/// dwFlags. FINDCHUNK 0x10 / FINDRIFF 0x20 / FINDLIST 0x40 search for ck's ckid (and fccType).
fn mmioDescend(c: &mut Ctx) -> u32 {
    let (h, ck, parent, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (want_id, want_type) = (r32(ck), r32(ck + 8));
    let end_limit = if parent != 0 { r32(parent + 12).saturating_add(r32(parent + 4)) } else { u32::MAX };
    with(h, |f| {
        loop {
            let at = f.pos;
            if at as u64 + 8 > end_limit.min(f.len()) as u64 {
                return MMIOERR_CHUNKNOTFOUND;
            }
            let (Some(id), Some(size)) = (f.u32_at(at), f.u32_at(at + 4)) else { return MMIOERR_CHUNKNOTFOUND };
            let list = id == FOURCC_RIFF || id == FOURCC_LIST;
            let ty = if list { f.u32_at(at + 8).unwrap_or(0) } else { 0 };
            let found = match flags & 0x70 {
                0x10 => id == want_id,
                0x20 => id == FOURCC_RIFF && ty == want_type,
                0x40 => id == FOURCC_LIST && ty == want_type,
                _ => true,
            };
            if found {
                w32(ck, id);
                w32(ck + 4, size);
                w32(ck + 8, ty);
                w32(ck + 12, at + 8);
                w32(ck + 16, 0);
                f.pos = at + 8 + if list { 4 } else { 0 };
                return 0;
            }
            // (a chunk size that would end the walk at or before `at`: no such chunk)
            match chunk_after(at, size) {
                Some(next) => f.pos = next,
                None => return MMIOERR_CHUNKNOTFOUND,
            }
        }
    })
    .unwrap_or(MMIOERR_CHUNKNOTFOUND)
}

/// mmioAscend(h, ck, flags): to the end of the chunk (padded to even). A chunk made by
/// mmioCreateChunk (MMIO_DIRTY) gets its size from what was written, and a pad byte if odd.
fn mmioAscend(c: &mut Ctx) -> u32 {
    let (h, ck) = (c.a(0), c.a(1));
    let data = r32(ck + 12);
    with(h, |f| {
        if r32(ck + 16) & MMIO_DIRTY != 0 {
            let size = f.pos.saturating_sub(data);
            let at = f.pos;
            if size != r32(ck + 4) && data >= 4 {
                f.pos = data - 4;
                f.put_u32(size);
                f.pos = at;
                w32(ck + 4, size);
            }
            if size & 1 != 0 {
                let z = [0u8];
                if let (Data::File(v), true) = (&mut f.data, (at as u64) < MAX_WRITE) {
                    if v.len() <= at as usize {
                        v.resize(at as usize + 1, 0);
                    }
                    v[at as usize] = z[0];
                    f.dirty = true;
                }
            }
        }
        f.pos = data.saturating_add(((r32(ck + 4) as u64 + 1) & !1).min(u32::MAX as u64) as u32);
    })
    .map_or(MMIOERR_CHUNKNOTFOUND, |_| 0)
}

/// mmioCreateChunk(h, ck, flags): the chunk header (with fccType for CREATERIFF 0x20 /
/// CREATELIST 0x40) at the position; dwDataOffset after the 8 header bytes, MMIO_DIRTY set.
fn mmioCreateChunk(c: &mut Ctx) -> u32 {
    let (h, ck, flags) = (c.a(0), c.a(1), c.a(2));
    let list = match flags & 0x60 {
        0x20 => Some(FOURCC_RIFF),
        0x40 => Some(FOURCC_LIST),
        _ => None,
    };
    with(h, |f| {
        if f.out.is_none() && matches!(f.data, Data::File(_) | Data::Handle(..)) {
            return MMIOERR_CANNOTWRITE;
        }
        if let Some(id) = list {
            w32(ck, id);
        }
        let start = f.pos;
        f.put_u32(r32(ck));
        f.put_u32(r32(ck + 4));
        if list.is_some() {
            f.put_u32(r32(ck + 8));
        }
        w32(ck + 12, start.saturating_add(8));
        w32(ck + 16, MMIO_DIRTY);
        0
    })
    .unwrap_or(MMIOERR_INVALIDFILE)
}

/// mmioGetInfo(h, MMIOINFO*, flags): memory is its own buffer (pchNext at the position);
/// a file is unbuffered (lDiskOffset at the position)
fn mmioGetInfo(c: &mut Ctx) -> u32 {
    let (h, info) = (c.a(0), c.a(1));
    let mi = self::info();
    let Some((pos, mem_buf)) = with(h, |f| (f.pos, if let Data::Mem(a, n) = f.data { Some((a, n)) } else { None })) else { return MMIOERR_INVALIDFILE };
    mem::fill(info, mi.size, 0);
    if with(h, |f| f.buf.is_some()).unwrap_or(false) {
        with(h, |f| f.put_buffer(info));
        mem::wp(info + mi.hmmio, h);
        return 0;
    }
    match mem_buf {
        Some((a, n)) => {
            w32(info + 4, FOURCC_MEM);
            w32(info + mi.cch, n);
            mem::wp(info + mi.buf, a);
            mem::wp(info + mi.next, a + pos);
            mem::wp(info + mi.end_read, a + n);
            mem::wp(info + mi.end_read + mem::ps(), a + n);
        }
        None => w32(info + mi.disk, pos),
    }
    mem::wp(info + mi.hmmio, h);
    0
}
/// mmioSetInfo(h, MMIOINFO*, flags): the position where the program's reading through the buffer
/// got to (pchNext).
fn mmioSetInfo(c: &mut Ctx) -> u32 {
    let (h, info) = (c.a(0), c.a(1));
    if info == 0 {
        return 0;
    }
    with(h, |f| {
        if let Some(p) = f.pos_of(info) {
            f.pos = p;
        }
        0
    })
    .unwrap_or(MMIOERR_INVALIDFILE)
}
/// mmioAdvance(h, MMIOINFO*, MMIO_READ): the buffer refilled from where pchNext got to (the
/// DirectX SDK's WaveReadFile reads a file this way, byte by byte from pchNext); at the end of the
/// file it succeeds with pchNext == pchEndRead. A buffer is made if the file had none (Windows
/// answers MMIOERR_UNBUFFERED there; the programs then read nothing). Writing through the buffer
/// is not supported.
fn mmioAdvance(c: &mut Ctx) -> u32 {
    let (h, info, flags) = (c.a(0), c.a(1), c.a(2));
    if info == 0 {
        return MMIOERR_INVALIDFILE;
    }
    with(h, |f| {
        if flags & 2 != 0 {
            return MMIOERR_CANNOTWRITE;
        }
        let p = f.pos_of(info).unwrap_or(f.pos);
        let (b, n) = f.buffer(r32(info + self::info().cch));
        f.pos = p;
        let got = f.read(b, n);
        f.pos = p;
        f.buf_off = p;
        f.buf_len = got;
        f.put_buffer(info);
        0
    })
    .unwrap_or(MMIOERR_INVALIDFILE)
}
/// mmioStringToFOURCC(str, flags): the first four characters (upper-cased with MMIO_TOUPPER)
fn fourcc(c: &mut Ctx, wide: bool) -> u32 {
    let s = crate::k32::arg_str(c.a(0), wide);
    let mut b = [b' '; 4];
    for (i, ch) in s.bytes().take(4).enumerate() {
        b[i] = if c.a(1) & 0x10 != 0 { ch.to_ascii_uppercase() } else { ch };
    }
    u32::from_le_bytes(b)
}
fn mmioStringToFOURCCA(c: &mut Ctx) -> u32 {
    fourcc(c, false)
}
fn mmioStringToFOURCCW(c: &mut Ctx) -> u32 {
    fourcc(c, true)
}

pub fn init() {
    for (n, f) in [
        ("mmioOpenA", mmioOpenA as crate::api::Handler),
        ("mmioOpenW", mmioOpenW),
        ("mmioInstallIOProcA", mmioInstallIOProcA),
        ("mmioInstallIOProcW", mmioInstallIOProcA),
        ("mmioClose", mmioClose),
        ("mmioRead", mmioRead),
        ("mmioWrite", mmioWrite),
        ("mmioSeek", mmioSeek),
        ("mmioDescend", mmioDescend),
        ("mmioAscend", mmioAscend),
        ("mmioGetInfo", mmioGetInfo),
        ("mmioCreateChunk", mmioCreateChunk),
        ("mmioFlush", mmioFlush),
        ("mmioSetInfo", mmioSetInfo),
        ("mmioAdvance", mmioAdvance),
        ("mmioStringToFOURCCA", mmioStringToFOURCCA),
        ("mmioStringToFOURCCW", mmioStringToFOURCCW),
    ] {
        implement("winmm", n, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn handle_len_riff_and_rest() {
        // a RIFF: its size plus the 8 header bytes
        assert_eq!(handle_len(b"RIFF\x1c\0\0\0WAVE", 36), 36);
        // a RIFF claiming more than the file holds: the file's end
        assert_eq!(handle_len(b"RIFF\xff\xff\xff\xff", 100), 100);
        // anything else (an RGSS archive), or too short a head: the rest of the file
        assert_eq!(handle_len(b"RGSSAD\0\x01", 6_617_962), 6_617_962);
        assert_eq!(handle_len(b"RIF", 3), 3);
        assert_eq!(handle_len(b"", 0), 0);
        // past 4 GB: what a 32-bit position reaches (not cut to 64 MB as before)
        assert_eq!(handle_len(b"RGSSAD\0\x03", 5 << 30), u32::MAX);
        assert_eq!(handle_len(b"RGSSAD\0\x03", 300 << 20), 300 << 20);
    }

    #[test]
    fn part_past_the_end_is_empty() {
        assert_eq!(part(b"abcd", 1, 2), b"bc");
        assert_eq!(part(b"abcd", 3, 9), b"d");
        assert_eq!(part(b"abcd", 4, 1), b"");
        assert_eq!(part(b"abcd", 100, 0), b"");
        assert_eq!(part(b"abcd", u32::MAX, u32::MAX), b"");
    }

    proptest! {
        #[test]
        fn part_is_the_clamped_range(v in proptest::collection::vec(any::<u8>(), 0..64), pos in any::<u32>(), n in any::<u32>()) {
            let want: Vec<u8> = v.iter().copied().skip(pos as usize).take(n as usize).collect();
            prop_assert_eq!(part(&v, pos, n), &want[..]);
        }

        #[test]
        fn handle_len_never_passes_the_file(head in proptest::collection::vec(any::<u8>(), 0..12), rest in any::<u64>()) {
            let n = handle_len(&head, rest);
            prop_assert!(n as u64 <= rest);
            if !head.starts_with(b"RIFF") || head.len() < 8 {
                prop_assert_eq!(n as u64, rest.min(u32::MAX as u64));
            }
        }

        #[test]
        fn handle_len_riff_is_size_plus_header(size in any::<u32>(), rest in any::<u64>()) {
            let mut head = b"RIFF".to_vec();
            head.extend_from_slice(&size.to_le_bytes());
            let n = handle_len(&head, rest);
            prop_assert_eq!(n as u64, (size as u64 + 8).min(u32::MAX as u64).min(rest));
        }
    }

    use crate::test_guest::{call, enter};

    /// An mmio file of `bytes` (as mmioOpen of a file makes, writable with `out`).
    fn file(bytes: Vec<u8>, writable: bool) -> u32 {
        let h = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let out = writable.then(|| "C:\\mmio-test-never-flushed.wav".to_string());
        FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default).insert(h, Mmio { data: Data::File(bytes), pos: 0, out, dirty: false, buf: None, buf_off: 0, buf_len: 0 });
        h
    }
    fn chunk(id: &[u8; 4], size: u32, data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(data);
        v
    }
    fn ckinfo(id: &[u8; 4]) -> u32 {
        let ck = mem::heap_alloc(20, true);
        w32(ck, u32::from_le_bytes(*id));
        ck
    }

    #[test]
    fn chunk_after_never_comes_back() {
        assert_eq!(chunk_after(0, 4), Some(12));
        assert_eq!(chunk_after(12, 3), Some(24));
        // (0xfffffff8 + 8 wrapped to 0: the walk stood still at `at` for ever, the lock held)
        assert_eq!(chunk_after(0, 0xffff_fff8), None);
        assert_eq!(chunk_after(0, 0xffff_fff7), None);
        assert_eq!(chunk_after(0, u32::MAX), None);
        assert_eq!(chunk_after(16, 0xffff_ffe0), Some(0xffff_fff8));
        assert_eq!(chunk_after(u32::MAX - 7, 0), None);
    }

    #[test]
    fn descend_past_a_chunk_of_4_gb_ends() {
        let _g = enter();
        // "junk" claiming 0xfffffff8 bytes, then the chunk asked for: not found, not a hang
        let mut v = chunk(b"junk", 0xffff_fff8, b"");
        v.extend(chunk(b"fmt ", 2, b"ab"));
        let h = file(v, false);
        let ck = ckinfo(b"fmt ");
        let (tx, rx) = std::sync::mpsc::channel();
        let ckp = ck;
        std::thread::spawn(move || tx.send(call(mmioDescend, &[h, ckp, 0, 0x10])).unwrap());
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(10)), Ok(MMIOERR_CHUNKNOTFOUND));
        assert_eq!(call(mmioClose, &[h, 0]), 0);
        mem::heap_free(ck);
    }

    #[test]
    fn close_frees_the_io_buffer() {
        let _g = enter();
        let h = file(chunk(b"RIFF", 4, b"WAVE"), false);
        let (b, _) = with(h, |f| f.buffer(0)).unwrap();
        assert!(mem::heap_owns(b));
        assert_eq!(call(mmioClose, &[h, 0]), 0);
        // (it stayed in the guest heap: 8 KiB per sound the DirectX SDK's CWaveFile opened)
        assert!(!mem::heap_owns(b));
    }

    #[test]
    fn writes_past_the_limit_fail() {
        let _g = enter();
        let h = file(Vec::new(), true);
        let src = mem::heap_alloc(16, true);
        assert_eq!(call(mmioSeek, &[h, 0x7000_0000, 0]), 0x7000_0000);
        // (a 1.8 GB Vec::resize before)
        assert_eq!(call(mmioWrite, &[h, src, 16]), u32::MAX);
        assert_eq!(call(mmioSeek, &[h, 0, 0]), 0);
        assert_eq!(call(mmioWrite, &[h, src, 16]), 16);
        // an ascend of a dirty chunk whose data offset is below 4 (no header before it)
        let ck = ckinfo(b"data");
        w32(ck + 12, 2);
        w32(ck + 16, MMIO_DIRTY);
        assert_eq!(call(mmioAscend, &[h, ck, 0]), 0);
        FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default).remove(&h);
        mem::heap_free(src);
        mem::heap_free(ck);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Descend(u8, u32),
        Ascend,
        Read(u32),
        Seek(i32, u8),
        Write(u32),
        Create(u8),
        Advance,
    }
    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..4, any::<u32>()).prop_map(|(f, i)| Op::Descend(f, i)),
            Just(Op::Ascend),
            (0u32..300).prop_map(Op::Read),
            (any::<i32>(), 0u8..3).prop_map(|(o, w)| Op::Seek(o, w)),
            (0u32..64).prop_map(Op::Write),
            (0u8..3).prop_map(Op::Create),
            Just(Op::Advance),
        ]
    }
    /// RIFF-shaped bytes with any chunk sizes (as a damaged or hostile .wav has).
    fn arb_riff() -> impl Strategy<Value = Vec<u8>> {
        let ids = prop_oneof![Just(*b"RIFF"), Just(*b"LIST"), Just(*b"fmt "), Just(*b"data"), any::<[u8; 4]>()];
        let size = prop_oneof![0u32..64, Just(0xffff_fff8), Just(0xffff_fff7), Just(u32::MAX), any::<u32>()];
        prop::collection::vec((ids, size, prop::collection::vec(any::<u8>(), 0..40)), 0..8)
            .prop_map(|cs| cs.into_iter().flat_map(|(id, size, d)| chunk(&id, size, &d)).collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(150))]
        /// Any sequence of mmio calls on any RIFF-shaped file, read-only or writable: no panic
        /// (abort), every descend ends (each step of its walk past the last), the file never
        /// grows past MAX_WRITE, and a read gives no more than was asked and the file holds.
        #[test]
        fn any_calls_on_any_riff(bytes in arb_riff(), writable in any::<bool>(), ops in prop::collection::vec(arb_op(), 0..24)) {
            let _g = enter();
            let h = file(bytes, writable);
            let (ck, parent, buf, info) = (mem::heap_alloc(20, true), mem::heap_alloc(20, true), mem::heap_alloc(300, true), mem::heap_alloc(info().size, true));
            for op in ops {
                match op {
                    Op::Descend(f, id) => {
                        w32(ck, id);
                        w32(ck + 8, id.rotate_left(8));
                        let p = if f & 1 != 0 { parent } else { 0 };
                        let r = call(mmioDescend, &[h, ck, p, [0, 0x10, 0x20, 0x40][f as usize]]);
                        prop_assert!(r == 0 || r == MMIOERR_CHUNKNOTFOUND);
                        if r == 0 {
                            mem::copy(parent, ck, 20);
                        }
                    }
                    Op::Ascend => {
                        call(mmioAscend, &[h, ck, 0]);
                    }
                    Op::Read(n) => {
                        let (pos, len) = with(h, |f| (f.pos, f.len())).unwrap();
                        let got = call(mmioRead, &[h, buf, n]);
                        prop_assert!(got <= n && got <= len.saturating_sub(pos));
                    }
                    Op::Seek(o, w) => {
                        call(mmioSeek, &[h, o as u32, w as u32]);
                    }
                    Op::Write(n) => {
                        call(mmioWrite, &[h, buf, n]);
                    }
                    Op::Create(f) => {
                        w32(ck + 4, 0);
                        call(mmioCreateChunk, &[h, ck, [0, 0x20, 0x40][f as usize]]);
                    }
                    Op::Advance => {
                        call(mmioGetInfo, &[h, info, 0]);
                        call(mmioAdvance, &[h, info, 0]);
                    }
                }
                prop_assert!(with(h, |f| f.len() as u64).unwrap() <= MAX_WRITE.max(1 << 16));
            }
            // (not flushed: the test's file is never written out)
            let f = FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::default).remove(&h).unwrap();
            if let Some((b, _)) = f.buf {
                mem::heap_free(b);
            }
            for p in [ck, parent, buf, info] {
                mem::heap_free(p);
            }
        }
    }
}
