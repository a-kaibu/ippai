//! cabinet.dll's FDI (File Decompression Interface): FDICreate, FDIIsCabinet, FDICopy,
//! FDIDestroy. Self-extracting trial games (騎士姫クラリス's taiken exe, #124) carry a cabinet
//! and unpack it with these through their own file callbacks before they start the game.
//!
//! The cabinet format and the order of the notifications are in `Cab` / `copy` (no guest
//! memory: unit tests, property tests and the `fdi` fuzz target drive them); the exports read
//! the cabinet through the guest's pfnread and hand the files back through pfnwrite and the
//! notify callback, as Windows' FDI does (and Wine's dlls/cabinet/fdi.c, whose order is the
//! reference: fdintCABINET_INFO once, then per file fdintCOPY_FILE, its data, fdintCLOSE_FILE_INFO).
//! The cabinet is not read whole: its tables first, then each CFDATA block when a file
//! needs it (pfnseek between folders), so a self-extractor's 600 MB cabinet costs a block.
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

/// One folder's uncompressed bytes, block by block, each CFDATA read from the cabinet when
/// it is reached (a block is at most 32 KB + 6 KB of compressed bytes).
struct Decoder<'a> {
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
    /// the current CFDATA's compressed bytes
    raw: Vec<u8>,
}

pub(crate) enum Kind {
    Stored,
    MsZip,
    Lzx(Box<lzxd::Lzxd>),
}

impl Kind {
    /// The decompressor of a CFFOLDER's typeCompress (Quantum and others: FDIERROR_BAD_COMPR_TYPE).
    pub(crate) fn of(comp: u16) -> Result<Kind, u32> {
        Ok(match comp & 0xf {
            0 => Kind::Stored,
            1 => Kind::MsZip,
            3 => {
                let w = match (comp >> 8) & 0x1f {
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
        })
    }

    /// One CFDATA block's `src` (its parts in a cabinet set joined) to its `un` bytes in `out`
    /// (cleared first); `hist`: MSZIP's last 32 KB of output, which a block's back references reach.
    pub(crate) fn unpack(&mut self, hist: &mut Vec<u8>, src: &[u8], un: usize, out: &mut Vec<u8>) -> Result<(), u32> {
        const BAD: u32 = FDIERROR_CORRUPT_CABINET;
        // (0 uncompressed bytes: the block continues in the next cabinet)
        if un == 0 || un > 0x8000 + 6144 {
            return Err(BAD);
        }
        out.clear();
        match self {
            Kind::Stored => {
                if src.len() != un {
                    return Err(BAD);
                }
                out.extend_from_slice(src);
            }
            Kind::MsZip => {
                use miniz_oxide::inflate::core::{decompress, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF, DecompressorOxide};
                use miniz_oxide::inflate::TINFLStatus;
                if src.len() < 2 || &src[..2] != b"CK" {
                    return Err(FDIERROR_MDI_FAIL);
                }
                let h = hist.len();
                let mut o = std::mem::take(hist);
                o.resize(h + un, 0);
                let mut r = DecompressorOxide::new();
                let (st, _, n) = decompress(&mut r, &src[2..], &mut o, h, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF);
                if st != TINFLStatus::Done || n != un {
                    return Err(FDIERROR_MDI_FAIL);
                }
                out.extend_from_slice(&o[h..h + un]);
                let keep = o.len().saturating_sub(0x8000);
                o.drain(..keep);
                *hist = o;
            }
            Kind::Lzx(l) => {
                let o = l.decompress_next(src, un).map_err(|_| FDIERROR_MDI_FAIL)?;
                if o.len() != un {
                    return Err(FDIERROR_MDI_FAIL);
                }
                out.extend_from_slice(o);
            }
        }
        Ok(())
    }
}

impl<'a> Decoder<'a> {
    fn new(folder: &'a Folder, data_reserve: u8) -> Result<Decoder<'a>, u32> {
        let kind = Kind::of(folder.comp)?;
        Ok(Decoder { folder, data_reserve: data_reserve as usize, kind, at: folder.data as usize, block: 0, buf: Vec::new(), used: 0, base: 0, hist: Vec::new(), raw: Vec::new() })
    }

    /// Uncompressed offset of the next byte `read` returns.
    fn pos(&self) -> u64 {
        self.base + self.used as u64
    }

    /// The next block into buf.
    fn next_block(&mut self, io: &mut dyn Io) -> Result<(), u32> {
        const BAD: u32 = FDIERROR_CORRUPT_CABINET;
        if self.block >= self.folder.blocks {
            return Err(BAD);
        }
        let mut raw = std::mem::take(&mut self.raw);
        if !io.read_at(self.at as u64, 8, &mut raw) {
            return Err(BAD);
        }
        let cb = u16le(&raw, 4).ok_or(BAD)? as usize;
        let un = u16le(&raw, 6).ok_or(BAD)? as usize;
        let start = self.at + 8 + self.data_reserve;
        if !io.read_at(start as u64, cb, &mut raw) {
            return Err(BAD);
        }
        let r = self.decode(&raw, un);
        self.raw = raw;
        r?;
        self.at = start + cb;
        self.block += 1;
        Ok(())
    }

    /// One CFDATA's bytes `src`, `un` once decompressed, into buf.
    fn decode(&mut self, src: &[u8], un: usize) -> Result<(), u32> {
        self.base += self.buf.len() as u64;
        self.used = 0;
        let mut buf = std::mem::take(&mut self.buf);
        let r = self.kind.unpack(&mut self.hist, src, un, &mut buf);
        self.buf = if r.is_ok() { buf } else { Vec::new() };
        r
    }

    /// Up to `max` bytes at pos() (an empty slice never: a folder ending early is an error).
    fn read(&mut self, io: &mut dyn Io, max: usize) -> Result<&[u8], u32> {
        if self.used == self.buf.len() {
            self.next_block(io)?;
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

/// A Sink that also reads the cabinet (pfnseek + pfnread).
pub trait Io: Sink {
    /// Exactly `n` bytes of the cabinet at `off` into `out` (replacing what it held): false
    /// when they are not all there.
    fn read_at(&mut self, off: u64, n: usize, out: &mut Vec<u8>) -> bool;
}

/// A cabinet in memory and the Sink its files go to.
struct Mem<'a> {
    d: &'a [u8],
    sink: &'a mut dyn Sink,
}
impl Sink for Mem<'_> {
    fn notify(&mut self, kind: u32, n: &Note) -> i32 {
        self.sink.notify(kind, n)
    }
    fn write(&mut self, hf: u32, data: &[u8]) -> u32 {
        self.sink.write(hf, data)
    }
}
impl Io for Mem<'_> {
    fn read_at(&mut self, off: u64, n: usize, out: &mut Vec<u8>) -> bool {
        out.clear();
        let Some(s) = usize::try_from(off).ok().and_then(|o| self.d.get(o..o.checked_add(n)?)) else { return false };
        out.extend_from_slice(s);
        true
    }
}

/// The bytes of a cabinet of `cb` bytes (at least its header) that hold every table `parse`
/// reads: the header with its reserve and set names, CFFOLDERs, then the CFFILEs at coffFiles.
/// The data blocks after them are left in the file.
pub fn tables_len(head36: &[u8], cb: u64) -> u64 {
    let Ok(h) = header(head36) else { return cb.min(CFHEADER as u64) };
    let coff = u32le(head36, 16).unwrap_or(0) as u64;
    let names = 4 * 257;
    let folders = h.c_folders as u64 * (8 + 255);
    let files = h.c_files as u64 * (16 + 257);
    (CFHEADER as u64 + 4 + 0xffff + names + folders).max(coff + files).min(cb)
}

/// The part of the cabinet's [off, off + n) that `tables` (its first bytes, already read)
/// holds, into `out` (cleared first); the offset and the count left to read from the file.
pub fn from_tables(tables: &[u8], off: u64, n: usize, out: &mut Vec<u8>) -> (u64, usize) {
    out.clear();
    let t = tables.len() as u64;
    if off >= t {
        return (off, n);
    }
    let end = (off + n as u64).min(t);
    out.extend_from_slice(&tables[off as usize..end as usize]);
    (end, n - (end - off) as usize)
}

/// FDICopy's work on a cabinet already in memory: Ok, or the FDIERROR it fails with.
pub fn copy(d: &[u8], sink: &mut dyn Sink) -> Result<(), u32> {
    let n = tables_len(d, d.len() as u64) as usize;
    stream(&d[..n], &mut Mem { d, sink })
}

/// FDICopy's work: the tables (`tables`, the cabinet's first tables_len bytes) parsed, then
/// each file's blocks read through `io` as they are reached. Ok, or the FDIERROR.
pub fn stream(tables: &[u8], io: &mut dyn Io) -> Result<(), u32> {
    let sink = &mut *io;
    let cab = Cab::parse(tables)?;
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
            dec = Some((f.folder, Decoder::new(folder, cab.data_reserve)?));
        }
        let dd = &mut dec.as_mut().unwrap().1;
        while dd.pos() < f.offset as u64 {
            let skip = (f.offset as u64 - dd.pos()).min(usize::MAX as u64) as usize;
            dd.read(sink, skip)?;
        }
        let mut left = f.size as usize;
        while left > 0 {
            let s = dd.read(sink, left)?;
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
    seek: u32,
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
    let f = Fdi { alloc: c.a(0), free: c.a(1), open: c.a(2), read: c.a(3), write: c.a(4), close: c.a(5), seek: c.a(6), erf: c.a(8) };
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
    let read = guest_read(c, &f, hf, CFHEADER, &mut d);
    match is_cabinet(read.then_some(&d[..])) {
        Err(None) => 0,
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
        Err(Some(e)) => {
            set_erf(f.erf, e);
            0
        }
    }
}

/// FDIIsCabinet's answer for the header read (None: the read came short): the header, or the
/// ERF error to set. A file that is not a cabinet (short, no "MSCF") is FALSE with the ERF left
/// alone, as on Windows (tests/winref/probes/fdi.c): 天極姫's InstallShield launcher asks about
/// its .msi with FDICreate's ERF in a frame that has returned, and an fError there was the
/// launcher's saved ebp.
pub fn is_cabinet(d: Option<&[u8]>) -> Result<Header, Option<u32>> {
    match header(d.ok_or(None)?) {
        Ok(h) => Ok(h),
        Err(FDIERROR_NOT_A_CABINET) => Err(None),
        Err(e) => Err(Some(e)),
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
    /// the cabinet's first bytes (read for the tables), its file, the file's pointer and the
    /// cabinet's size (cbCabinet), and a 64 KB read buffer
    tables: Vec<u8>,
    hf: u32,
    pos: u64,
    cb: u64,
    rbuf: u32,
}

/// The guest read buffer of FDICopy
const RBUF: u32 = 0x10000;

impl Io for Guest<'_, '_> {
    /// pfnseek when the bytes are not next (as Windows' FDI moves between folders), then
    /// pfnread in 64 KB pieces; nothing past cbCabinet
    fn read_at(&mut self, off: u64, n: usize, out: &mut Vec<u8>) -> bool {
        out.clear();
        if off + n as u64 > self.cb {
            return false;
        }
        let (off, n) = from_tables(&self.tables, off, n, out);
        if n == 0 {
            return true;
        }
        let n = out.len() + n;
        if off != self.pos {
            if self.f.seek != 0 {
                // (SEEK_SET; a long: cbCabinet is below 2 GB)
                if self.c.call(self.f.seek, &[self.hf, off as u32, 0]) != off as u32 {
                    return false;
                }
                self.pos = off;
            } else if off > self.pos {
                // (no pfnseek: read up to it)
                let mut skip = Vec::new();
                while self.pos < off {
                    let k = (off - self.pos).min(RBUF as u64) as usize;
                    let at = self.pos;
                    if !self.read_at(at, k, &mut skip) {
                        return false;
                    }
                }
            } else {
                return false;
            }
        }
        out.reserve(n);
        while out.len() < n {
            let want = ((n - out.len()) as u32).min(RBUF);
            let got = self.c.call(self.f.read, &[self.hf, self.rbuf, want]);
            if got == 0 || got == u32::MAX || got > want {
                return false;
            }
            out.extend_from_slice(mem::slice(self.rbuf, got));
            self.pos += got as u64;
        }
        true
    }
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
    if sbuf == 0 {
        set_erf(f.erf, FDIERROR_ALLOC_FAIL);
        return 0;
    }
    mem::write(sbuf, &full);
    let hf = c.call(f.open, &[sbuf, 0x8000, 0x180]);
    mem::heap_free(sbuf);
    if hf == u32::MAX {
        set_erf(f.erf, FDIERROR_CABINET_NOT_FOUND);
        return 0;
    }
    // the header and the tables are read first; the data blocks one by one as files need them
    let mut d = Vec::new();
    let mut r = if guest_read(c, &f, hf, CFHEADER, &mut d) { header(&d).map(|h| h.cb_cabinet) } else { Err(FDIERROR_NOT_A_CABINET) };
    if let Ok(cb) = r {
        let rest = tables_len(&d, cb as u64).saturating_sub(CFHEADER as u64) as usize;
        if !guest_read(c, &f, hf, rest, &mut d) {
            r = Err(FDIERROR_CORRUPT_CABINET);
        }
    }
    let r = r.and_then(|cb| {
        // the FDINOTIFICATION, its three strings, a write buffer (32 KB) and the read buffer
        // (64 KB) in one block
        let block = mem::heap_alloc(64 + 3 * 256 + 256 + 0x8000 + RBUF, true);
        if block == 0 {
            return Err(FDIERROR_ALLOC_FAIL);
        }
        let strs = [block + 64, block + 64 + 256, block + 64 + 512];
        let rbuf = block + 64 + 3 * 256 + 256 + 0x8000;
        let pos = d.len() as u64;
        let mut g = Guest { c: &mut *c, f, notify, pv, path: path_s, note: block, strs, tables: d.clone(), hf, pos, cb: cb as u64, rbuf };
        let r = stream(&d, &mut g);
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
