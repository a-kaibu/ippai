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
const FOURCC_MEM: u32 = u32::from_le_bytes(*b"MEM ");
const FOURCC_RIFF: u32 = u32::from_le_bytes(*b"RIFF");
const FOURCC_LIST: u32 = u32::from_le_bytes(*b"LIST");

enum Data {
    File(Vec<u8>),
    /// guest memory: address, size
    Mem(u32, u32),
}

struct Mmio {
    data: Data,
    pos: u32,
    /// the file to write back to (opened for writing) and whether there is something to write
    out: Option<String>,
    dirty: bool,
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

impl Mmio {
    fn len(&self) -> u32 {
        match &self.data {
            Data::File(v) => v.len() as u32,
            Data::Mem(_, n) => *n,
        }
    }
    /// copies up to `n` bytes at the position to guest `dst`
    fn read(&mut self, dst: u32, n: u32) -> u32 {
        let n = n.min(self.len().saturating_sub(self.pos));
        match &self.data {
            Data::File(v) => mem::write(dst, &v[self.pos as usize..(self.pos + n) as usize]),
            Data::Mem(a, _) => mem::copy(dst, a + self.pos, n),
        }
        self.pos += n;
        n
    }
    /// writes guest bytes at the position (a file grows; memory stops at its end)
    fn write(&mut self, src: u32, n: u32) -> u32 {
        let n = match &mut self.data {
            Data::File(v) => {
                let end = (self.pos + n) as usize;
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
        };
        self.pos += n;
        self.dirty = true;
        n
    }
    fn put_u32(&mut self, v: u32) {
        let t = v.to_le_bytes();
        match &mut self.data {
            Data::File(d) => {
                let end = self.pos as usize + 4;
                if d.len() < end {
                    d.resize(end, 0);
                }
                d[self.pos as usize..end].copy_from_slice(&t);
            }
            Data::Mem(a, len) => {
                if self.pos + 4 <= *len {
                    w32(*a + self.pos, v);
                }
            }
        }
        self.pos += 4;
        self.dirty = true;
    }
    fn flush(&mut self) -> bool {
        if let (Some(p), true, Data::File(v)) = (&self.out, self.dirty, &self.data) {
            let h = crate::files::resolve(p).unwrap_or_else(|| crate::files::resolve_new(p));
            if kura::fs::write(h, v.clone()).is_err() {
                return false;
            }
            self.dirty = false;
        }
        true
    }
    fn u32_at(&self, off: u32) -> Option<u32> {
        if off + 4 > self.len() {
            return None;
        }
        Some(match &self.data {
            Data::File(v) => u32::from_le_bytes(v[off as usize..off as usize + 4].try_into().unwrap()),
            Data::Mem(a, _) => r32(a + off),
        })
    }
}

static FILES: Mutex<Option<HashMap<u32, Mmio>>> = Mutex::new(None);
static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x6d00);

fn with<R>(h: u32, f: impl FnOnce(&mut Mmio) -> R) -> Option<R> {
    FILES.lock().unwrap().get_or_insert_with(HashMap::default).get_mut(&h).map(f)
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
    } else if name == 0 && info != 0 && r32(info + 4) == 0 && r32(info + mi.adw) != 0 {
        // adwInfo[0]: an open file's handle, read from where it is (a RIFF in an archive)
        let Some(v) = from_handle(r32(info + mi.adw)) else {
            return fail(MMIOERR_CANNOTOPEN);
        };
        Data::File(v)
    } else if info != 0 && r32(info + 4) == FOURCC_MEM {
        Data::Mem(mem::rp(info + mi.buf), r32(info + mi.cch))
    } else {
        if name == 0 {
            return fail(MMIOERR_CANNOTOPEN);
        }
        let path = crate::k32::arg_str(name, wide);
        let old = crate::files::resolve(&path).and_then(|h| kura::fs::read(h).ok());
        if flags & 0x1000 != 0 {
            // MMIO_CREATE: a new, empty file
            let h = crate::files::resolve(&path).unwrap_or_else(|| crate::files::resolve_new(&path));
            if kura::fs::write(h, Vec::new()).is_err() {
                return fail(MMIOERR_CANNOTOPEN);
            }
            out = Some(path);
            Data::File(Vec::new())
        } else {
            let Some(v) = old else { return fail(MMIOERR_CANNOTOPEN) };
            if flags & 0x4000 != 0 {
                // MMIO_EXIST: only whether it is there
                return 1;
            }
            if writing {
                out = Some(path);
            }
            Data::File(v)
        }
    };
    let h = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FILES.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, Mmio { data, pos: 0, out, dirty: false });
    if info != 0 {
        w32(info + mi.err, 0);
        mem::wp(info + mi.hmmio, h);
    }
    h
}
/// The RIFF at an open file's position (or the rest of the file, up to 64 MB)
fn from_handle(h: u32) -> Option<Vec<u8>> {
    use std::io::SeekFrom;
    let f = crate::obj::file(h)?;
    let at = f.seek(SeekFrom::Current(0))?;
    let tmp = mem::alloc(8)?;
    let got = f.read_into(tmp, 8);
    let n = if got == 8 && r32(tmp) == FOURCC_RIFF { r32(tmp + 4).saturating_add(8) } else { (f.size().saturating_sub(at)).min(64 << 20) as u32 };
    mem::release(tmp);
    f.seek(SeekFrom::Start(at))?;
    let buf = mem::alloc(n.max(1))?;
    let got = f.read_into(buf, n);
    let v = mem::slice(buf, got).to_vec();
    mem::release(buf);
    Some(v)
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
        if f != 0 && f != FOURCC_MEM {
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
        v.extend_from_slice(mem::slice(tmp, n as u32));
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
    let f = FILES.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0));
    match f {
        Some(mut f) => {
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
    with(h, |f| if f.out.is_none() && matches!(f.data, Data::File(_)) { u32::MAX } else { f.write(buf, n.max(0) as u32) }).unwrap_or(u32::MAX)
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
    let end_limit = if parent != 0 { r32(parent + 12) + r32(parent + 4) } else { u32::MAX };
    with(h, |f| {
        loop {
            let at = f.pos;
            if at + 8 > end_limit.min(f.len()) {
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
            f.pos = at + 8 + ((size + 1) & !1);
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
            if size != r32(ck + 4) {
                f.pos = data - 4;
                f.put_u32(size);
                f.pos = at;
                w32(ck + 4, size);
            }
            if size & 1 != 0 {
                let z = [0u8];
                if let Data::File(v) = &mut f.data {
                    if v.len() <= at as usize {
                        v.resize(at as usize + 1, 0);
                    }
                    v[at as usize] = z[0];
                    f.dirty = true;
                }
            }
        }
        f.pos = data + ((r32(ck + 4) + 1) & !1);
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
        if f.out.is_none() && matches!(f.data, Data::File(_)) {
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
        w32(ck + 12, start + 8);
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
fn ok(_: &mut Ctx) -> u32 {
    0
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
        ("mmioSetInfo", ok),
        ("mmioStringToFOURCCA", mmioStringToFOURCCA),
        ("mmioStringToFOURCCW", mmioStringToFOURCCW),
    ] {
        implement("winmm", n, f);
    }
}
