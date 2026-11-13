//! cabinet.dll's FDI (File Decompression Interface): FDICreate, FDIIsCabinet, FDICopy,
//! FDIDestroy. Self-extracting trial games (騎士姫クラリス's taiken exe, #124) carry a cabinet
//! and unpack it with these through their own file callbacks before they start the game.
//!
//! The cabinet format and the order of the notifications are in `Cab` / `copy` (no guest
//! memory: unit tests, property tests and the `fdi` fuzz target drive them); the exports read
//! the cabinet through the guest's pfnread and hand the files back through pfnwrite and the
//! notify callback, as Windows' FDI does (and Wine's dlls/cabinet/fdi.c, whose order is the
//! reference: fdintCABINET_INFO once, then per file fdintCOPY_FILE, its data, fdintCLOSE_FILE_INFO).
//! MSZIP and LZX folders and stored ones; Quantum is FDIERROR_BAD_COMPR_TYPE. A file continued
//! in the next cabinet of a set (spanning) is FDIERROR_CORRUPT_CABINET for now.
#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::mem::{self, w16, w32};

// FDIERROR
pub const FDIERROR_NONE: u32 = 0;
pub const FDIERROR_CABINET_NOT_FOUND: u32 = 1;
pub const FDIERROR_NOT_A_CABINET: u32 = 2;
pub const FDIERROR_UNKNOWN_CABINET_VERSION: u32 = 3;
pub const FDIERROR_CORRUPT_CABINET: u32 = 4;
pub const FDIERROR_ALLOC_FAIL: u32 = 5;
pub const FDIERROR_BAD_COMPR_TYPE: u32 = 6;
pub const FDIERROR_MDI_FAIL: u32 = 7;
pub const FDIERROR_TARGET_FILE: u32 = 8;
pub const FDIERROR_USER_ABORT: u32 = 11;

// FDINOTIFICATIONTYPE
pub const FDINT_CABINET_INFO: u32 = 0;
pub const FDINT_COPY_FILE: u32 = 2;
pub const FDINT_CLOSE_FILE_INFO: u32 = 3;

const CFHEADER: usize = 36;
const A_EXEC: u16 = 0x40;
const IFOLD_CONTINUED: u16 = 0xfffd; // 0xfffd from prev, 0xfffe to next, 0xffff both

fn u16le(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32le(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
/// A NUL-terminated string at `o` (at most 256 bytes as CAB names are): (bytes, offset past the NUL).
fn sz(d: &[u8], o: usize) -> Option<(Vec<u8>, usize)> {
    let rest = d.get(o..)?;
    let n = rest.iter().take(257).position(|&b| b == 0)?;
    Some((rest[..n].to_vec(), o + n + 1))
}

/// The fixed CFHEADER fields (what FDIIsCabinet reports).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub cb_cabinet: u32,
    pub c_folders: u16,
    pub c_files: u16,
    pub set_id: u16,
    pub i_cabinet: u16,
    pub reserve: bool,
    pub has_prev: bool,
    pub has_next: bool,
}

/// The first 36 bytes: the header, or the FDIERROR they give.
pub fn header(d: &[u8]) -> Result<Header, u32> {
    if d.len() < CFHEADER {
        return Err(FDIERROR_NOT_A_CABINET);
    }
    if &d[0..4] != b"MSCF" {
        return Err(FDIERROR_NOT_A_CABINET);
    }
    let (minor, major) = (d[24], d[25]);
    if major > 1 || (major == 1 && minor > 3) {
        return Err(FDIERROR_UNKNOWN_CABINET_VERSION);
    }
    let flags = u16le(d, 30).unwrap();
    Ok(Header {
        cb_cabinet: u32le(d, 8).unwrap(),
        c_folders: u16le(d, 26).unwrap(),
        c_files: u16le(d, 28).unwrap(),
        set_id: u16le(d, 32).unwrap(),
        i_cabinet: u16le(d, 34).unwrap(),
        reserve: flags & 4 != 0,
        has_prev: flags & 1 != 0,
        has_next: flags & 2 != 0,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    /// the first CFDATA's offset in the cabinet
    pub data: u32,
    pub blocks: u16,
    pub comp: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub size: u32,
    /// the uncompressed offset in its folder
    pub offset: u32,
    pub folder: u16,
    pub date: u16,
    pub time: u16,
    pub attribs: u16,
    pub name: Vec<u8>,
}

/// A whole cabinet, parsed (not decompressed).
#[derive(Clone, Debug)]
pub struct Cab {
    pub head: Header,
    pub next_cab: Vec<u8>,
    pub next_disk: Vec<u8>,
    pub data_reserve: u8,
    pub folders: Vec<Folder>,
    pub files: Vec<File>,
}

impl Cab {
    pub fn parse(d: &[u8]) -> Result<Cab, u32> {
        const BAD: u32 = FDIERROR_CORRUPT_CABINET;
        let head = header(d)?;
        let mut o = CFHEADER;
        let (mut folder_reserve, mut data_reserve) = (0usize, 0u8);
        if head.reserve {
            let hr = u16le(d, o).ok_or(BAD)? as usize;
            folder_reserve = *d.get(o + 2).ok_or(BAD)? as usize;
            data_reserve = *d.get(o + 3).ok_or(BAD)?;
            o += 4 + hr;
        }
        if head.has_prev {
            o = sz(d, o).ok_or(BAD)?.1;
            o = sz(d, o).ok_or(BAD)?.1;
        }
        let (mut next_cab, mut next_disk) = (Vec::new(), Vec::new());
        if head.has_next {
            let (a, p) = sz(d, o).ok_or(BAD)?;
            let (b, p) = sz(d, p).ok_or(BAD)?;
            (next_cab, next_disk, o) = (a, b, p);
        }
        let mut folders = Vec::with_capacity(head.c_folders as usize);
        for _ in 0..head.c_folders {
            folders.push(Folder { data: u32le(d, o).ok_or(BAD)?, blocks: u16le(d, o + 4).ok_or(BAD)?, comp: u16le(d, o + 6).ok_or(BAD)? });
            o += 8 + folder_reserve;
        }
        let mut o = u32le(d, 16).unwrap() as usize;
        let mut files = Vec::with_capacity(head.c_files as usize);
        for _ in 0..head.c_files {
            let size = u32le(d, o).ok_or(BAD)?;
            let offset = u32le(d, o + 4).ok_or(BAD)?;
            let folder = u16le(d, o + 8).ok_or(BAD)?;
            let date = u16le(d, o + 10).ok_or(BAD)?;
            let time = u16le(d, o + 12).ok_or(BAD)?;
            let attribs = u16le(d, o + 14).ok_or(BAD)?;
            let (name, p) = sz(d, o + 16).ok_or(BAD)?;
            o = p;
            files.push(File { size, offset, folder, date, time, attribs, name });
        }
        Ok(Cab { head, next_cab, next_disk, data_reserve, folders, files })
    }
}

/// One folder's uncompressed bytes, block by block.
struct Decoder<'a> {
    d: &'a [u8],
    folder: &'a Folder,
    data_reserve: usize,
    kind: Kind,
    /// the next CFDATA's offset and index
    at: usize,
    block: u16,
    /// the current block's bytes and how far they are used
    buf: Vec<u8>,
    used: usize,
    /// uncompressed offset of buf[0]
    base: u64,
    /// MSZIP: the last 32 KB of output (a block's back references reach into it)
    hist: Vec<u8>,
}

enum Kind {
    Stored,
    MsZip,
    Lzx(Box<lzxd::Lzxd>),
}

impl<'a> Decoder<'a> {
    fn new(d: &'a [u8], folder: &'a Folder, data_reserve: u8) -> Result<Decoder<'a>, u32> {
        let kind = match folder.comp & 0xf {
            0 => Kind::Stored,
            1 => Kind::MsZip,
            3 => {
                let w = match (folder.comp >> 8) & 0x1f {
                    15 => lzxd::WindowSize::KB32,
                    16 => lzxd::WindowSize::KB64,
                    17 => lzxd::WindowSize::KB128,
                    18 => lzxd::WindowSize::KB256,
                    19 => lzxd::WindowSize::KB512,
                    20 => lzxd::WindowSize::MB1,
                    21 => lzxd::WindowSize::MB2,
                    _ => return Err(FDIERROR_BAD_COMPR_TYPE),
                };
                Kind::Lzx(Box::new(lzxd::Lzxd::new(w)))
            }
            _ => return Err(FDIERROR_BAD_COMPR_TYPE),
        };
        Ok(Decoder { d, folder, data_reserve: data_reserve as usize, kind, at: folder.data as usize, block: 0, buf: Vec::new(), used: 0, base: 0, hist: Vec::new() })
    }

    /// Uncompressed offset of the next byte `read` returns.
    fn pos(&self) -> u64 {
        self.base + self.used as u64
    }

    /// The next block into buf.
    fn next_block(&mut self) -> Result<(), u32> {
        const BAD: u32 = FDIERROR_CORRUPT_CABINET;
        if self.block >= self.folder.blocks {
            return Err(BAD);
        }
        let d = self.d;
        let cb = u16le(d, self.at + 4).ok_or(BAD)? as usize;
        let un = u16le(d, self.at + 6).ok_or(BAD)? as usize;
        let start = self.at + 8 + self.data_reserve;
        let src = d.get(start..start + cb).ok_or(BAD)?;
        // (0 uncompressed bytes: the block continues in the next cabinet)
        if un == 0 || un > 0x8000 + 6144 {
            return Err(BAD);
        }
        self.base += self.buf.len() as u64;
        self.buf.clear();
        self.used = 0;
        match &mut self.kind {
            Kind::Stored => {
                if cb != un {
                    return Err(BAD);
                }
                self.buf.extend_from_slice(src);
            }
            Kind::MsZip => {
                use miniz_oxide::inflate::core::{decompress, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF, DecompressorOxide};
                use miniz_oxide::inflate::TINFLStatus;
                if src.len() < 2 || &src[..2] != b"CK" {
                    return Err(FDIERROR_MDI_FAIL);
                }
                let h = self.hist.len();
                let mut out = std::mem::take(&mut self.hist);
                out.resize(h + un, 0);
                let mut r = DecompressorOxide::new();
                let (st, _, n) = decompress(&mut r, &src[2..], &mut out, h, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF);
                if st != TINFLStatus::Done || n != un {
                    return Err(FDIERROR_MDI_FAIL);
                }
                self.buf.extend_from_slice(&out[h..h + un]);
                let keep = out.len().saturating_sub(0x8000);
                out.drain(..keep);
                self.hist = out;
            }
            Kind::Lzx(l) => {
                let o = l.decompress_next(src, un).map_err(|_| FDIERROR_MDI_FAIL)?;
                if o.len() != un {
                    return Err(FDIERROR_MDI_FAIL);
                }
                self.buf.extend_from_slice(o);
            }
        }
        self.at = start + cb;
        self.block += 1;
        Ok(())
    }

    /// Up to `max` bytes at pos() (an empty slice never: a folder ending early is an error).
    fn read(&mut self, max: usize) -> Result<&[u8], u32> {
        if self.used == self.buf.len() {
            self.next_block()?;
        }
        let n = max.min(self.buf.len() - self.used);
        let s = &self.buf[self.used..self.used + n];
        self.used += n;
        Ok(s)
    }
}

/// What FDICopy tells the caller; the answers steer it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub cb: u32,
    pub psz1: Vec<u8>,
    pub psz2: Vec<u8>,
    pub hf: u32,
    pub date: u16,
    pub time: u16,
    pub attribs: u16,
    pub set_id: u16,
    pub i_cabinet: u16,
    pub i_folder: u16,
}

pub trait Sink {
    /// fdintCABINET_INFO, fdintCOPY_FILE, fdintCLOSE_FILE_INFO: the callback's answer (an int).
    fn notify(&mut self, kind: u32, n: &Note) -> i32;
    /// pfnwrite: the bytes written (fewer = the target failed).
    fn write(&mut self, hf: u32, data: &[u8]) -> u32;
}

/// FDICopy's work on a cabinet already in memory: Ok, or the FDIERROR it fails with.
pub fn copy(d: &[u8], sink: &mut dyn Sink) -> Result<(), u32> {
    let cab = Cab::parse(d)?;
    let info = Note {
        cb: 0,
        psz1: cab.next_cab.clone(),
        psz2: cab.next_disk.clone(),
        hf: 0,
        date: 0,
        time: 0,
        attribs: 0,
        set_id: cab.head.set_id,
        i_cabinet: cab.head.i_cabinet,
        i_folder: 0,
    };
    if sink.notify(FDINT_CABINET_INFO, &info) == -1 {
        return Err(FDIERROR_USER_ABORT);
    }
    let mut dec: Option<(u16, Decoder)> = None;
    for f in &cab.files {
        // (continued from the previous cabinet: that cabinet's FDICopy had it)
        if f.folder == IFOLD_CONTINUED || f.folder == 0xffff {
            continue;
        }
        if f.folder == 0xfffe {
            return Err(FDIERROR_CORRUPT_CABINET);
        }
        let folder = cab.folders.get(f.folder as usize).ok_or(FDIERROR_CORRUPT_CABINET)?;
        let mut n = Note { cb: f.size, psz1: f.name.clone(), psz2: Vec::new(), hf: 0, date: f.date, time: f.time, attribs: f.attribs, set_id: cab.head.set_id, i_cabinet: cab.head.i_cabinet, i_folder: f.folder };
        let hf = sink.notify(FDINT_COPY_FILE, &n);
        if hf == -1 {
            return Err(FDIERROR_USER_ABORT);
        }
        if hf == 0 {
            continue;
        }
        // the folder's decoder, from its start when the file lies behind it
        let fresh = match &dec {
            Some((i, dd)) => *i != f.folder || dd.pos() > f.offset as u64,
            None => true,
        };
        if fresh {
            dec = Some((f.folder, Decoder::new(d, folder, cab.data_reserve)?));
        }
        let dd = &mut dec.as_mut().unwrap().1;
        while dd.pos() < f.offset as u64 {
            let skip = (f.offset as u64 - dd.pos()).min(usize::MAX as u64) as usize;
            dd.read(skip)?;
        }
        let mut left = f.size as usize;
        while left > 0 {
            let s = dd.read(left)?;
            left -= s.len();
            if sink.write(hf as u32, s) != s.len() as u32 {
                return Err(FDIERROR_TARGET_FILE);
            }
        }
        n.hf = hf as u32;
        n.cb = (f.attribs & A_EXEC != 0) as u32;
        if sink.notify(FDINT_CLOSE_FILE_INFO, &n) == 0 {
            return Err(FDIERROR_USER_ABORT);
        }
    }
    Ok(())
}

// ---- the exports ----

/// An HFDI: the guest's callbacks and ERF.
#[derive(Clone, Copy)]
struct Fdi {
    alloc: u32,
    free: u32,
    open: u32,
    read: u32,
    write: u32,
    close: u32,
    erf: u32,
}

static FDIS: std::sync::Mutex<Vec<(u32, Fdi)>> = std::sync::Mutex::new(Vec::new());

fn fdi(h: u32) -> Option<Fdi> {
    FDIS.lock().unwrap().iter().find(|e| e.0 == h).map(|e| e.1)
}

fn set_erf(erf: u32, oper: u32) {
    if erf != 0 {
        w32(erf, oper);
        w32(erf + 4, 0);
        w32(erf + 8, 1);
    }
}

/// FDICreate(pfnalloc, pfnfree, pfnopen, pfnread, pfnwrite, pfnclose, pfnseek, cpuType, perf)
fn FDICreate(c: &mut Ctx) -> u32 {
    let f = Fdi { alloc: c.a(0), free: c.a(1), open: c.a(2), read: c.a(3), write: c.a(4), close: c.a(5), erf: c.a(8) };
    if f.alloc == 0 || f.open == 0 || f.read == 0 {
        set_erf(f.erf, FDIERROR_ALLOC_FAIL);
        return 0;
    }
    // (the HFDI is the caller's memory, as on Windows: its allocator sees one block per FDI)
    let h = c.call(f.alloc, &[64]);
    if h == 0 {
        set_erf(f.erf, FDIERROR_ALLOC_FAIL);
        return 0;
    }
    if f.erf != 0 {
        mem::fill(f.erf, 12, 0);
    }
    FDIS.lock().unwrap().push((h, f));
    h
}

/// FDIDestroy(hfdi)
fn FDIDestroy(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let f = {
        let mut g = FDIS.lock().unwrap();
        let Some(i) = g.iter().position(|e| e.0 == h) else { return 0 };
        g.remove(i).1
    };
    if f.free != 0 {
        c.call(f.free, &[h]);
    }
    1
}

/// Reads up to `n` bytes from the guest file `hf` through pfnread (a scratch buffer in the
/// guest heap); short at the end of the file.
fn guest_read(c: &mut Ctx, f: &Fdi, hf: u32, n: usize, out: &mut Vec<u8>) -> bool {
    const CHUNK: u32 = 0x10000;
    let buf = mem::heap_alloc(CHUNK, false);
    if buf == 0 {
        return false;
    }
    let mut left = n;
    let mut ok = true;
    while left > 0 {
        let want = (left as u32).min(CHUNK);
        let got = c.call(f.read, &[hf, buf, want]);
        if got == u32::MAX || got > want {
            ok = false;
            break;
        }
        if got == 0 {
            break;
        }
        out.extend_from_slice(mem::slice(buf, got));
        left -= got as usize;
    }
    mem::heap_free(buf);
    ok
}

/// FDIIsCabinet(hfdi, hf, pfdici)
fn FDIIsCabinet(c: &mut Ctx) -> u32 {
    let (h, hf, info) = (c.a(0), c.a(1), c.a(2));
    let Some(f) = fdi(h) else { return 0 };
    let mut d = Vec::new();
    if !guest_read(c, &f, hf, CFHEADER, &mut d) {
        set_erf(f.erf, FDIERROR_NOT_A_CABINET);
        return 0;
    }
    match header(&d) {
        Ok(hd) => {
            if info != 0 {
                w32(info, hd.cb_cabinet);
                w16(info + 4, hd.c_folders);
                w16(info + 6, hd.c_files);
                w16(info + 8, hd.set_id);
                w16(info + 10, hd.i_cabinet);
                w32(info + 12, hd.reserve as u32);
                w32(info + 16, hd.has_prev as u32);
                w32(info + 20, hd.has_next as u32);
            }
            1
        }
        Err(e) => {
            set_erf(f.erf, e);
            0
        }
    }
}

/// FDINOTIFICATION in guest memory (x86 and x64 layouts) and the callbacks behind a `Sink`.
struct Guest<'c, 'a> {
    c: &'c mut Ctx<'a>,
    f: Fdi,
    notify: u32,
    pv: u32,
    path: Vec<u8>,
    note: u32,
    strs: [u32; 3],
}

impl Guest<'_, '_> {
    fn put_str(&mut self, slot: usize, s: &[u8]) -> u32 {
        let p = self.strs[slot];
        let n = s.len().min(255);
        mem::write(p, &s[..n]);
        mem::w8(p + n as u32, 0);
        p
    }
}

impl Sink for Guest<'_, '_> {
    fn notify(&mut self, kind: u32, n: &Note) -> i32 {
        let p = self.note;
        let x64 = mem::x64();
        let ps = if x64 { 8 } else { 4 };
        mem::fill(p, 64, 0);
        let s1 = self.put_str(0, &n.psz1);
        let s2 = self.put_str(1, &n.psz2);
        let s3 = if kind == FDINT_CABINET_INFO { let path = self.path.clone(); self.put_str(2, &path) } else { 0 };
        w32(p, n.cb);
        let ptrs = [s1, s2, s3, self.pv, n.hf];
        for (i, v) in ptrs.into_iter().enumerate() {
            mem::put_addr(p + ps * (1 + i as u32), v);
        }
        let o = ps * 6;
        w16(p + o, n.date);
        w16(p + o + 2, n.time);
        w16(p + o + 4, n.attribs);
        w16(p + o + 6, n.set_id);
        w16(p + o + 8, n.i_cabinet);
        w16(p + o + 10, n.i_folder);
        w32(p + o + 12, 0);
        // (x64: the INT_PTR answer's low half; -1 stays -1)
        self.c.call(self.notify, &[kind, p]) as i32
    }
    fn write(&mut self, hf: u32, data: &[u8]) -> u32 {
        const CHUNK: usize = 0x8000;
        let buf = self.strs[2] + 256;
        let mut done = 0u32;
        for part in data.chunks(CHUNK) {
            mem::write(buf, part);
            let w = self.c.call(self.f.write, &[hf, buf, part.len() as u32]);
            if w != part.len() as u32 {
                return done;
            }
            done += w;
        }
        done
    }
}

/// FDICopy(hfdi, pszCabinet, pszCabPath, flags, pfnfdin, pfnfdid, pvUser)
fn FDICopy(c: &mut Ctx) -> u32 {
    let (h, name, path, notify, pv) = (c.a(0), c.a(1), c.a(2), c.a(4), c.a(6));
    let Some(f) = fdi(h) else { return 0 };
    let mut full = if path != 0 { mem::cstr(path) } else { Vec::new() };
    let path_s = full.clone();
    if name != 0 {
        full.extend_from_slice(&mem::cstr(name));
    }
    // (_O_RDONLY | _O_BINARY, _S_IREAD | _S_IWRITE, as Windows opens it)
    let sbuf = mem::heap_alloc(full.len() as u32 + 1, true);
    mem::write(sbuf, &full);
    let hf = c.call(f.open, &[sbuf, 0x8000, 0x180]);
    mem::heap_free(sbuf);
    if hf == u32::MAX {
        set_erf(f.erf, FDIERROR_CABINET_NOT_FOUND);
        return 0;
    }
    let mut d = Vec::new();
    let mut r = if guest_read(c, &f, hf, CFHEADER, &mut d) { header(&d).map(|h| h.cb_cabinet) } else { Err(FDIERROR_NOT_A_CABINET) };
    if let Ok(cb) = r {
        let rest = (cb as usize).saturating_sub(CFHEADER).min(0x7fff_ffff);
        if !guest_read(c, &f, hf, rest, &mut d) {
            r = Err(FDIERROR_CORRUPT_CABINET);
        }
    }
    let r = r.and_then(|_| {
        // the FDINOTIFICATION, its three strings and a write buffer (32 KB) in one block
        let block = mem::heap_alloc(64 + 3 * 256 + 256 + 0x8000, true);
        if block == 0 {
            return Err(FDIERROR_ALLOC_FAIL);
        }
        let strs = [block + 64, block + 64 + 256, block + 64 + 512];
        let mut g = Guest { c: &mut *c, f, notify, pv, path: path_s, note: block, strs };
        let r = copy(&d, &mut g);
        mem::heap_free(block);
        r
    });
    if f.close != 0 {
        c.call(f.close, &[hf]);
    }
    match r {
        Ok(()) => 1,
        Err(e) => {
            set_erf(f.erf, e);
            0
        }
    }
}

/// FDITruncateCabinet: not supported (FALSE, as for a cabinet it cannot change).
fn FDITruncateCabinet(_c: &mut Ctx) -> u32 {
    0
}

pub fn init() {
    implement("cabinet", "FDICreate", FDICreate);
    implement("cabinet", "FDIIsCabinet", FDIIsCabinet);
    implement("cabinet", "FDICopy", FDICopy);
    implement("cabinet", "FDIDestroy", FDIDestroy);
    implement("cabinet", "FDITruncateCabinet", FDITruncateCabinet);
}

#[cfg(test)]
#[path = "cabinet_tests.rs"]
mod tests;
