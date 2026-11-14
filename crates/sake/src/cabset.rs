//! Cabinet sets: a folder that goes on from one cabinet into the next (spanning), read straight
//! from the files without loading them. msi.dll's InstallFiles takes a package's cabinets
//! through it when they are a set: 天極姫's Data1.cab (402 MB, "next" Data11.cab) and
//! Data11.cab (958 MB), whose files cross from one into the other (the `cab` crate refuses
//! those: "File entry folder index out of bounds").
//!
//! The format (cabinet.rs's tables): each volume lists its own folders; when a volume has a
//! previous one, its folder 0 is the previous volume's last folder going on. A file whose data
//! crosses volumes is listed in both, with iFolder 0xFFFE (goes on into the next), 0xFFFD (came
//! from the previous) or 0xFFFF (both); its offset is in the whole folder. A CFDATA block can be
//! cut at a volume's end: that part has cbUncomp 0 and the next volume's first block of the
//! folder holds the rest, with the block's real cbUncomp.

use crate::cabinet::{Cab, Kind, FDIERROR_CORRUPT_CABINET};
use std::io::{Read, Seek, SeekFrom, Write};

const BAD: u32 = FDIERROR_CORRUPT_CABINET;

/// A file of the set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    /// the set's folder (the folders of every volume, a spanned one once)
    pub folder: usize,
    /// its offset in the folder's uncompressed bytes
    pub offset: u32,
    pub size: u32,
    pub attribs: u16,
}

/// One volume's part of a folder.
#[derive(Clone, Copy, Debug)]
struct Seg {
    vol: usize,
    data: u32,
    blocks: u16,
}

struct SetFolder {
    comp: u16,
    segs: Vec<Seg>,
}

/// A folder being decoded: where its next block is and what came out last.
struct Stream {
    folder: usize,
    kind: Kind,
    hist: Vec<u8>,
    seg: usize,
    block: u16,
    at: u64,
    buf: Vec<u8>,
    used: usize,
    base: u64,
}

pub struct Set<R> {
    vols: Vec<(R, u8)>,
    folders: Vec<SetFolder>,
    pub files: Vec<Entry>,
    cur: Option<Stream>,
}

/// A volume's tables: the start of the file read until they parse (they come before the data).
pub fn tables<R: Read + Seek>(r: &mut R) -> Result<Cab, u32> {
    let mut want: u64 = 1 << 16;
    loop {
        r.seek(SeekFrom::Start(0)).map_err(|_| BAD)?;
        let mut d = Vec::new();
        r.by_ref().take(want).read_to_end(&mut d).map_err(|_| BAD)?;
        match Cab::parse(&d) {
            Ok(c) => return Ok(c),
            // (cut short of the file table: read more of the file)
            Err(BAD) if d.len() as u64 == want && want < 1 << 28 => want *= 4,
            Err(e) => return Err(e),
        }
    }
}

impl<R: Read + Seek> Set<R> {
    /// The set of `vols` in order (the first cabinet first). A first volume that has a previous
    /// one: the files that came from it are left out (their start is not here).
    pub fn new(vols: Vec<R>) -> Result<Set<R>, u32> {
        let mut set = Set { vols: Vec::new(), folders: Vec::new(), files: Vec::new(), cur: None };
        for (vi, mut r) in vols.into_iter().enumerate() {
            let cab = tables(&mut r)?;
            let cont = cab.head.has_prev && !set.folders.is_empty();
            let first = if cont { set.folders.len() - 1 } else { set.folders.len() };
            for (k, fo) in cab.folders.iter().enumerate() {
                let seg = Seg { vol: vi, data: fo.data, blocks: fo.blocks };
                if k == 0 && cont {
                    set.folders[first].segs.push(seg);
                } else {
                    set.folders.push(SetFolder { comp: fo.comp, segs: vec![seg] });
                }
            }
            for f in &cab.files {
                let folder = match f.folder {
                    0xfffd | 0xffff if !cont => continue,
                    0xfffd | 0xffff => first,
                    0xfffe if cab.folders.is_empty() => return Err(BAD),
                    0xfffe => first + cab.folders.len() - 1,
                    i if (i as usize) < cab.folders.len() => first + i as usize,
                    _ => return Err(BAD),
                };
                let e = Entry { name: f.name.clone(), folder, offset: f.offset, size: f.size, attribs: f.attribs };
                // (a file that crosses volumes is listed in each)
                if !set.files.contains(&e) {
                    set.files.push(e);
                }
            }
            set.vols.push((r, cab.data_reserve));
        }
        Ok(set)
    }

    /// The next whole block of the stream's folder: (compressed bytes, uncompressed size), the
    /// parts of a block cut at a volume's end joined.
    fn raw_block(&mut self) -> Result<(Vec<u8>, usize), u32> {
        let mut st = self.cur.take().ok_or(BAD)?;
        let r = (|| {
            let mut src = Vec::new();
            loop {
                let segs = &self.folders[st.folder].segs;
                if st.block >= segs[st.seg].blocks {
                    st.seg += 1;
                    let s = *segs.get(st.seg).ok_or(BAD)?;
                    st.block = 0;
                    st.at = s.data as u64;
                    continue;
                }
                let (r, reserve) = &mut self.vols[segs[st.seg].vol];
                let mut h = [0u8; 8];
                r.seek(SeekFrom::Start(st.at)).map_err(|_| BAD)?;
                r.read_exact(&mut h).map_err(|_| BAD)?;
                let cb = u16::from_le_bytes([h[4], h[5]]) as usize;
                let un = u16::from_le_bytes([h[6], h[7]]) as usize;
                r.seek(SeekFrom::Current(*reserve as i64)).map_err(|_| BAD)?;
                let n = src.len();
                src.resize(n + cb, 0);
                r.read_exact(&mut src[n..]).map_err(|_| BAD)?;
                st.at += 8 + *reserve as u64 + cb as u64;
                st.block += 1;
                if un != 0 {
                    return Ok((src, un));
                }
                // (cut here: the rest is the next volume's first block of the folder)
                if st.block < segs[st.seg].blocks {
                    return Err(BAD);
                }
            }
        })();
        self.cur = Some(st);
        r
    }

    fn next_block(&mut self) -> Result<(), u32> {
        let (src, un) = self.raw_block()?;
        let st = self.cur.as_mut().unwrap();
        st.base += st.buf.len() as u64;
        st.used = 0;
        let mut buf = std::mem::take(&mut st.buf);
        let r = st.kind.unpack(&mut st.hist, &src, un, &mut buf);
        st.buf = if r.is_ok() { buf } else { Vec::new() };
        r
    }

    /// Writes file `i` (an index into `files`) to `w`. Files in folder order read the folder once.
    pub fn read_file(&mut self, i: usize, w: &mut dyn Write) -> Result<(), u32> {
        let e = self.files.get(i).ok_or(BAD)?.clone();
        let fresh = self.cur.as_ref().is_none_or(|s| s.folder != e.folder || s.base + s.used as u64 > e.offset as u64);
        if fresh {
            let f = self.folders.get(e.folder).ok_or(BAD)?;
            let at = f.segs[0].data as u64;
            self.cur = Some(Stream { folder: e.folder, kind: Kind::of(f.comp)?, hist: Vec::new(), seg: 0, block: 0, at, buf: Vec::new(), used: 0, base: 0 });
        }
        let (mut skip, mut left) = (e.offset as u64 - self.pos(), e.size as u64);
        while skip > 0 || left > 0 {
            if self.cur.as_ref().is_some_and(|s| s.used == s.buf.len()) {
                if let Err(x) = self.next_block() {
                    self.cur = None;
                    return Err(x);
                }
            }
            let st = self.cur.as_mut().unwrap();
            let have = (st.buf.len() - st.used) as u64;
            if skip > 0 {
                let n = skip.min(have);
                st.used += n as usize;
                skip -= n;
                continue;
            }
            let n = left.min(have) as usize;
            w.write_all(&st.buf[st.used..st.used + n]).map_err(|_| crate::cabinet::FDIERROR_TARGET_FILE)?;
            st.used += n;
            left -= n as u64;
        }
        Ok(())
    }

    fn pos(&self) -> u64 {
        self.cur.as_ref().map_or(0, |s| s.base + s.used as u64)
    }
}

#[cfg(test)]
#[path = "cabset_tests.rs"]
mod tests;
