//! The identification and setup headers.

use crate::bits::{ilog, Bits};
use crate::codebook::Codebook;
use crate::Error;

pub struct Ident {
    pub channels: usize,
    pub rate: u32,
    /// short and long block sizes
    pub blocksize: [usize; 2],
}

pub struct Floor1 {
    pub partition_class: Vec<u8>,
    /// per class: dimensions, subclass bits, master book (-1: none), subclass books (-1: none)
    pub classes: Vec<(usize, u32, i32, Vec<i32>)>,
    pub multiplier: i32,
    /// the posts' x (in decoding order)
    pub x: Vec<i32>,
    /// for posts 2..: the low and high neighbours
    pub lo: Vec<usize>,
    pub hi: Vec<usize>,
    /// post indices in increasing x
    pub sorted: Vec<usize>,
}

pub struct Residue {
    pub kind: u16,
    pub begin: usize,
    pub end: usize,
    pub grouping: usize,
    pub classifications: usize,
    pub classbook: usize,
    /// per classification: the books of each stage (-1: none)
    pub books: Vec<[i32; 8]>,
    pub stages: usize,
}

pub struct Mapping {
    /// (magnitude, angle) channels
    pub coupling: Vec<(usize, usize)>,
    /// per channel: its submap
    pub mux: Vec<usize>,
    /// per submap: (floor, residue)
    pub submaps: Vec<(usize, usize)>,
}

pub struct Mode {
    pub blockflag: bool,
    pub mapping: usize,
}

pub struct Setup {
    pub ident: Ident,
    pub books: Vec<Codebook>,
    pub floors: Vec<Floor1>,
    pub residues: Vec<Residue>,
    pub mappings: Vec<Mapping>,
    pub modes: Vec<Mode>,
}

fn header(b: &mut Bits, kind: u32) -> Result<(), Error> {
    if b.read(8) != Some(kind) {
        return Err(Error::Header);
    }
    for &c in b"vorbis" {
        if b.read(8) != Some(c as u32) {
            return Err(Error::Header);
        }
    }
    Ok(())
}

impl Ident {
    pub fn read(packet: &[u8]) -> Result<Ident, Error> {
        let e = Error::Header;
        let mut b = Bits::new(packet);
        header(&mut b, 1)?;
        if b.read(32).ok_or(e)? != 0 {
            return Err(e);
        }
        let channels = b.read(8).ok_or(e)? as usize;
        let rate = b.read(32).ok_or(e)?;
        b.read(32).ok_or(e)?;
        b.read(32).ok_or(e)?;
        b.read(32).ok_or(e)?;
        let b0 = b.read(4).ok_or(e)?;
        let b1 = b.read(4).ok_or(e)?;
        if channels == 0 || rate == 0 || !(6..=13).contains(&b0) || !(6..=13).contains(&b1) || b0 > b1 || b.read(1) != Some(1) {
            return Err(e);
        }
        Ok(Ident { channels, rate, blocksize: [1 << b0, 1 << b1] })
    }
}

impl Floor1 {
    fn read(b: &mut Bits, books: usize) -> Result<Floor1, Error> {
        let e = Error::Header;
        let parts = b.read(5).ok_or(e)? as usize;
        let mut partition_class = Vec::with_capacity(parts);
        for _ in 0..parts {
            partition_class.push(b.read(4).ok_or(e)? as u8);
        }
        let nclasses = partition_class.iter().map(|&c| c as usize + 1).max().unwrap_or(0);
        let book = |v: u32| -> Result<i32, Error> {
            let v = v as i32 - 1;
            if v >= books as i32 {
                Err(e)
            } else {
                Ok(v)
            }
        };
        let mut classes = Vec::with_capacity(nclasses);
        for _ in 0..nclasses {
            let dim = b.read(3).ok_or(e)? as usize + 1;
            let sub = b.read(2).ok_or(e)?;
            let master = if sub > 0 { book(b.read(8).ok_or(e)? + 1)? } else { -1 };
            let mut subs = Vec::with_capacity(1 << sub);
            for _ in 0..1 << sub {
                subs.push(book(b.read(8).ok_or(e)?)?);
            }
            classes.push((dim, sub, master, subs));
        }
        let multiplier = b.read(2).ok_or(e)? as i32 + 1;
        let rangebits = b.read(4).ok_or(e)?;
        let mut x = vec![0, 1 << rangebits];
        for &c in &partition_class {
            for _ in 0..classes[c as usize].0 {
                x.push(b.read(rangebits).ok_or(e)? as i32);
            }
        }
        if x.len() > 65 {
            return Err(e);
        }
        let mut sorted: Vec<usize> = (0..x.len()).collect();
        sorted.sort_by_key(|&i| x[i]);
        if sorted.windows(2).any(|w| x[w[0]] == x[w[1]]) {
            return Err(e);
        }
        let (mut lo, mut hi) = (Vec::new(), Vec::new());
        for i in 2..x.len() {
            let (mut l, mut h) = (0usize, 1usize);
            let (mut lx, mut hx) = (0, 1 << 30);
            for j in 0..i {
                if x[j] > lx && x[j] < x[i] {
                    l = j;
                    lx = x[j];
                }
                if x[j] < hx && x[j] > x[i] {
                    h = j;
                    hx = x[j];
                }
            }
            lo.push(l);
            hi.push(h);
        }
        Ok(Floor1 { partition_class, classes, multiplier, x, lo, hi, sorted })
    }
}

impl Residue {
    fn read(b: &mut Bits, kind: u16, books: &[Codebook]) -> Result<Residue, Error> {
        let e = Error::Header;
        let begin = b.read(24).ok_or(e)? as usize;
        let end = b.read(24).ok_or(e)? as usize;
        let grouping = b.read(24).ok_or(e)? as usize + 1;
        let classifications = b.read(6).ok_or(e)? as usize + 1;
        let classbook = b.read(8).ok_or(e)? as usize;
        let mut cascade = Vec::with_capacity(classifications);
        for _ in 0..classifications {
            let low = b.read(3).ok_or(e)?;
            let high = if b.flag().ok_or(e)? { b.read(5).ok_or(e)? } else { 0 };
            cascade.push(high << 3 | low);
        }
        let mut list = Vec::with_capacity(classifications);
        let mut stages = 0;
        for &c in &cascade {
            let mut s = [-1i32; 8];
            for (k, slot) in s.iter_mut().enumerate() {
                if c >> k & 1 == 1 {
                    let book = b.read(8).ok_or(e)? as usize;
                    if book >= books.len() || books[book].values.is_empty() {
                        return Err(e);
                    }
                    *slot = book as i32;
                }
            }
            stages = stages.max(ilog(c) as usize);
            list.push(s);
        }
        if classbook >= books.len() || books[classbook].dim == 0 {
            return Err(e);
        }
        Ok(Residue { kind, begin, end, grouping, classifications, classbook, books: list, stages })
    }
}

impl Setup {
    pub fn read(ident: Ident, packet: &[u8]) -> Result<Setup, Error> {
        let e = Error::Header;
        let mut b = Bits::new(packet);
        header(&mut b, 5)?;
        let nbooks = b.read(8).ok_or(e)? as usize + 1;
        let mut books = Vec::with_capacity(nbooks);
        for _ in 0..nbooks {
            books.push(Codebook::read(&mut b)?);
        }
        let ntimes = b.read(6).ok_or(e)? + 1;
        for _ in 0..ntimes {
            if b.read(16).ok_or(e)? != 0 {
                return Err(e);
            }
        }
        let nfloors = b.read(6).ok_or(e)? + 1;
        let mut floors = Vec::new();
        for _ in 0..nfloors {
            match b.read(16).ok_or(e)? {
                1 => floors.push(Floor1::read(&mut b, books.len())?),
                // floor 0 (no encoder has made it since 2002): left to the game's decoder
                0 => return Err(Error::Unsupported),
                _ => return Err(e),
            }
        }
        let nres = b.read(6).ok_or(e)? + 1;
        let mut residues = Vec::new();
        for _ in 0..nres {
            let kind = b.read(16).ok_or(e)? as u16;
            if kind > 2 {
                return Err(e);
            }
            residues.push(Residue::read(&mut b, kind, &books)?);
        }
        let nmaps = b.read(6).ok_or(e)? + 1;
        let ch = ident.channels;
        let mut mappings = Vec::new();
        for _ in 0..nmaps {
            if b.read(16).ok_or(e)? != 0 {
                return Err(e);
            }
            let submaps = if b.flag().ok_or(e)? { b.read(4).ok_or(e)? as usize + 1 } else { 1 };
            let mut coupling = Vec::new();
            if b.flag().ok_or(e)? {
                let steps = b.read(8).ok_or(e)? as usize + 1;
                let bits = ilog(ch as u32 - 1);
                for _ in 0..steps {
                    let m = b.read(bits).ok_or(e)? as usize;
                    let a = b.read(bits).ok_or(e)? as usize;
                    if m == a || m >= ch || a >= ch {
                        return Err(e);
                    }
                    coupling.push((m, a));
                }
            }
            if b.read(2).ok_or(e)? != 0 {
                return Err(e);
            }
            let mut mux = vec![0usize; ch];
            if submaps > 1 {
                for m in mux.iter_mut() {
                    *m = b.read(4).ok_or(e)? as usize;
                    if *m >= submaps {
                        return Err(e);
                    }
                }
            }
            let mut subs = Vec::with_capacity(submaps);
            for _ in 0..submaps {
                b.read(8).ok_or(e)?;
                let f = b.read(8).ok_or(e)? as usize;
                let r = b.read(8).ok_or(e)? as usize;
                if f >= floors.len() || r >= residues.len() {
                    return Err(e);
                }
                subs.push((f, r));
            }
            mappings.push(Mapping { coupling, mux, submaps: subs });
        }
        let nmodes = b.read(6).ok_or(e)? + 1;
        let mut modes = Vec::new();
        for _ in 0..nmodes {
            let blockflag = b.flag().ok_or(e)?;
            let window = b.read(16).ok_or(e)?;
            let transform = b.read(16).ok_or(e)?;
            let mapping = b.read(8).ok_or(e)? as usize;
            if window != 0 || transform != 0 || mapping >= mappings.len() {
                return Err(e);
            }
            modes.push(Mode { blockflag, mapping });
        }
        if b.read(1) != Some(1) {
            return Err(e);
        }
        Ok(Setup { ident, books, floors, residues, mappings, modes })
    }
}
