//! winmm's multimedia file I/O (mmio*), reading only: RIFF files (WAVE, AVI headers) a game
//! opens by name, or a block of its memory (FOURCC_MEM). A file is read whole at open.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::mem::{self, r32, w32};

const MMIOERR_CANNOTOPEN: u32 = 257;
const MMIOERR_CHUNKNOTFOUND: u32 = 265;
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
    let fail = |e: u32| {
        if info != 0 {
            w32(info + 12, e);
        }
        0
    };
    // writing, creating, deleting, parsing: not supported
    if flags & 0x1303 != 0 {
        return fail(MMIOERR_CANNOTOPEN);
    }
    if crate::env_on!("SAKE_TRACE_FILES") && info != 0 {
        eprintln!("sake: mmioOpen({name:#x}, {info:#x}, flags {flags:#x}) {:x?}", (0..18).map(|i| r32(info + 4 * i)).collect::<Vec<_>>());
    }
    let proc = io_proc(name, info, wide);
    let data = if let Some(p) = proc {
        match read_via(c, p, name, info, flags) {
            Some(v) => Data::File(v),
            None => return fail(MMIOERR_CANNOTOPEN),
        }
    } else if name == 0 && info != 0 && r32(info + 4) == 0 && r32(info + 48) != 0 {
        // adwInfo[0]: an open file's handle, read from where it is (a RIFF in an archive)
        let Some(v) = from_handle(r32(info + 48)) else {
            return fail(MMIOERR_CANNOTOPEN);
        };
        Data::File(v)
    } else if info != 0 && r32(info + 4) == FOURCC_MEM {
        Data::Mem(r32(info + 24), r32(info + 20))
    } else {
        if name == 0 {
            return fail(MMIOERR_CANNOTOPEN);
        }
        let path = crate::k32::arg_str(name, wide);
        let Some(v) = crate::files::resolve(&path).and_then(|h| kura::fs::read(h).ok()) else { return fail(MMIOERR_CANNOTOPEN) };
        Data::File(v)
    };
    let h = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FILES.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, Mmio { data, pos: 0 });
    if info != 0 {
        w32(info + 12, 0);
        w32(info + 68, h);
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
    FILES.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0));
    0
}
fn mmioRead(c: &mut Ctx) -> u32 {
    let (h, buf, n) = (c.a(0), c.a(1), c.i(2));
    with(h, |f| f.read(buf, n.max(0) as u32)).unwrap_or(u32::MAX)
}
fn mmioWrite(_: &mut Ctx) -> u32 {
    u32::MAX
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
        if p < 0 {
            return u32::MAX;
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

/// mmioAscend(h, ck, flags): to the end of the chunk (padded to even)
fn mmioAscend(c: &mut Ctx) -> u32 {
    let (h, ck) = (c.a(0), c.a(1));
    let end = r32(ck + 12) + ((r32(ck + 4) + 1) & !1);
    with(h, |f| f.pos = end).map_or(MMIOERR_CHUNKNOTFOUND, |_| 0)
}

/// mmioGetInfo(h, MMIOINFO*, flags): no buffered I/O; the handle and position
fn mmioGetInfo(c: &mut Ctx) -> u32 {
    let (h, info) = (c.a(0), c.a(1));
    let Some(pos) = with(h, |f| f.pos) else { return MMIOERR_CANNOTOPEN };
    mem::fill(info, 72, 0);
    w32(info + 44, pos);
    w32(info + 68, h);
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
        ("mmioSetInfo", ok),
        ("mmioStringToFOURCCA", mmioStringToFOURCCA),
        ("mmioStringToFOURCCW", mmioStringToFOURCCW),
    ] {
        implement("winmm", n, f);
    }
}
