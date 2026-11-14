//! Enigma Virtual Box: some NW.js games (RPG Maker MV, e.g. 王女アイリスの世直し冒険記 体験版,
//! RJ186234) are one nw.exe whose app files (package.json, www/...) are in a file system boxed into
//! the exe, which the box's loader serves to the process from memory. The page runs NW.js games
//! as web pages (web/src/html.ts), which needs the files themselves: `masu pack` adds the box's
//! files next to the exe, as nwjs.rs does for a zip appended to nw.exe.
//!
//! The format (as Enigma Virtual Box writes it; evbunpack by mos9527 documents it): the PE's
//! .enigma1 section holds "EVB\0" + 60 bytes, then a tree of nodes in pre-order. A node is u32
//! size (of what follows it), 8 bytes, u32 count of children (folders), its name in UTF-16 up to a
//! 0 unit, then a type byte (0 the root, 2 a file, 3 a folder). The top folder "%DEFAULT FOLDER%"
//! is the exe's own; others (%SYSTEM FOLDER% and the like) are skipped. Two layouts:
//! - older boxes (before 2017, the one above): each file node is followed by its bytes; a node
//!   ends `size + 4` after it starts, the last 49 bytes of a file node holding its original size
//!   (u32 at 2) and stored size (u32 at 41).
//! - newer ones: the root's size spans the whole table, the nodes follow it from its last byte on,
//!   a file node's 53 bytes after the type byte hold the sizes (original at 2, stored at 49), a
//!   folder's 25 bytes, and the files' bytes follow the table in the nodes' order.
//! A file whose stored size is not its size is compressed: u32 n, u32, then n - 8 bytes of
//! 12-byte entries whose first u32 is a chunk's stored size, then the chunks, each aPLib packed.
//!
//! What the box says is not trusted: names that would leave the folder are skipped, a file past
//! the exe's end or a node past the end of the box is an error, and what it unpacks to is capped.

use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"EVB\0";
/// Files and folders a box may hold.
const MAX_ENTRIES: usize = 200_000;
/// Folder depth.
const MAX_DEPTH: usize = 64;
/// UTF-16 units of a name.
const MAX_NAME: usize = 1024;
/// A compressed file's chunk table.
const MAX_TABLE: u64 = 64 << 20;
/// Bytes it may unpack to: this many times the exe's size, at least `MIN_OUT`, at most `MAX_OUT`.
const RATIO: u64 = 8;
const MIN_OUT: u64 = 256 << 20;
const MAX_OUT: u64 = 16 << 30;
/// Bytes one compressed chunk may unpack to (real boxes' chunks are far smaller): a damaged
/// gamma that asks for 2^31 bytes is an error before it is built in memory.
const MAX_CHUNK: usize = 64 << 20;

/// A file of the box: its bytes at `offset..offset + stored` of the exe, `size` once unpacked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packed {
    pub offset: u64,
    pub stored: u64,
    pub size: u64,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Where the box of the PE whose first bytes are `head` starts: the "EVB\0" in the first 64 KiB
/// of its .enigma1 section (read with `read(offset, len)`). None for a PE without one.
pub fn locate(head: &[u8], read: &mut dyn FnMut(u64, usize) -> io::Result<Vec<u8>>) -> io::Result<Option<u64>> {
    if head.len() < 0x40 || &head[..2] != b"MZ" {
        return Ok(None);
    }
    let pe = u32_at(head, 0x3c) as usize;
    if pe.checked_add(24).is_none_or(|e| e > head.len()) || &head[pe..pe + 4] != b"PE\0\0" {
        return Ok(None);
    }
    let sections = u16::from_le_bytes([head[pe + 6], head[pe + 7]]) as usize;
    let opt = u16::from_le_bytes([head[pe + 20], head[pe + 21]]) as usize;
    for i in 0..sections {
        let s = pe + 24 + opt + 40 * i;
        let Some(sec) = head.get(s..s + 40) else { break };
        if &sec[..8] != b".enigma1" {
            continue;
        }
        let (raw_size, raw_at) = (u32_at(sec, 16) as u64, u32_at(sec, 20) as u64);
        let b = read(raw_at, raw_size.min(64 << 10) as usize)?;
        return Ok(b.windows(4).position(|w| w == MAGIC).map(|p| raw_at + p as u64));
    }
    Ok(None)
}

/// A node: its size field, children count, name and type, and where its fixed fields after the
/// type byte start.
struct Node {
    size: u64,
    count: u32,
    name: String,
    ty: u8,
    after: u64,
}

fn read_node<R: Read + Seek>(r: &mut R, at: u64) -> Result<Node> {
    r.seek(SeekFrom::Start(at))?;
    let mut h = [0u8; 16];
    r.read_exact(&mut h)?;
    let mut units = Vec::new();
    loop {
        let mut u = [0u8; 2];
        r.read_exact(&mut u)?;
        let u = u16::from_le_bytes(u);
        if u == 0 {
            break;
        }
        if units.len() >= MAX_NAME {
            bail!("a name over {MAX_NAME} units at {at:#x}");
        }
        units.push(u);
    }
    let mut ty = [0u8];
    r.read_exact(&mut ty)?;
    Ok(Node {
        size: u32_at(&h, 0) as u64,
        count: u32_at(&h, 12),
        name: String::from_utf16_lossy(&units),
        ty: ty[0],
        after: at + 16 + 2 * units.len() as u64 + 3,
    })
}

fn read_u32s<R: Read + Seek>(r: &mut R, at: u64, offs: &[u64]) -> Result<Vec<u64>> {
    offs.iter()
        .map(|&o| {
            r.seek(SeekFrom::Start(at + o))?;
            let mut b = [0u8; 4];
            r.read_exact(&mut b)?;
            Ok(u32::from_le_bytes(b) as u64)
        })
        .collect()
}

/// The files and folders (no source) of the box at `at` of a file of `len` bytes; paths are
/// relative to the exe's folder.
pub fn parse<R: Read + Seek>(r: &mut R, at: u64, len: u64) -> Result<Vec<(PathBuf, Option<Packed>)>> {
    r.seek(SeekFrom::Start(at))?;
    let mut hdr = [0u8; 64];
    r.read_exact(&mut hdr)?;
    if &hdr[..4] != MAGIC {
        bail!("no EVB\\0 at {at:#x}");
    }
    let root = read_node(r, at + 64)?;
    // (an older box's root node is just itself: 8 + 4 bytes, an empty name, its type)
    let legacy = root.size == 15 && root.name.is_empty() && root.ty == 0;
    let (mut pos, mut data) = if legacy {
        (at + 64 + root.size + 4, 0)
    } else {
        (at + 64 + 15, (at + 80 + root.size).checked_sub(12).context("a table before its box")?)
    };
    let mut out = Vec::new();
    // (folder to add to, None when skipped; children left)
    let mut stack: Vec<(Option<PathBuf>, u64)> = vec![(Some(PathBuf::new()), root.count as u64)];
    let mut nodes = 0usize;
    while let Some(top) = stack.last_mut() {
        if top.1 == 0 {
            stack.pop();
            continue;
        }
        top.1 -= 1;
        let dir = top.0.clone();
        let depth = stack.len();
        nodes += 1;
        if nodes > MAX_ENTRIES {
            bail!("more than {MAX_ENTRIES} files");
        }
        let n = read_node(r, pos)?;
        let ok_name = !n.name.is_empty() && n.name != "." && n.name != ".." && !n.name.contains(['/', '\\', '\0', ':']);
        match n.ty {
            2 => {
                let (size, stored, start, next) = if legacy {
                    let end = pos + n.size + 4;
                    if end < n.after + 49 {
                        bail!("a file node too short at {pos:#x}");
                    }
                    let v = read_u32s(r, end - 49, &[2, 41])?;
                    (v[0], v[1], end, end + v[1])
                } else {
                    let v = read_u32s(r, n.after, &[2, 49])?;
                    let start = data;
                    data += v[1];
                    (v[0], v[1], start, n.after + 53)
                };
                if start.checked_add(stored).is_none_or(|e| e > len) {
                    bail!("{}: past the exe's end", n.name);
                }
                if let (Some(d), true) = (dir, ok_name) {
                    out.push((d.join(&n.name), Some(Packed { offset: start, stored, size })));
                }
                pos = next;
            }
            3 => {
                pos = if legacy {
                    if pos + n.size + 4 < n.after {
                        bail!("a folder node too short at {pos:#x}");
                    }
                    pos + n.size + 4
                } else {
                    n.after + 25
                };
                if depth > MAX_DEPTH {
                    bail!("folders nested over {MAX_DEPTH} deep");
                }
                // (the exe's folder is the top "%DEFAULT FOLDER%"; other % folders are not in it)
                let sub = match dir {
                    Some(d) if depth == 1 && n.name == "%DEFAULT FOLDER%" => Some(d),
                    Some(_) if depth == 1 && n.name.starts_with('%') => None,
                    Some(d) if ok_name => {
                        out.push((d.join(&n.name), None));
                        Some(d.join(&n.name))
                    }
                    _ => None,
                };
                stack.push((sub, n.count as u64));
            }
            t => bail!("a node of type {t} at {pos:#x}"),
        }
    }
    Ok(out)
}

/// Writes `p`'s bytes (unpacked) to `w`, at most `left` of them; returns how many.
pub fn extract<R: Read + Seek, W: Write>(r: &mut R, p: &Packed, w: &mut W, left: u64) -> Result<u64> {
    extract_chunked(r, p, w, left, MAX_CHUNK)
}

/// `extract`, each compressed chunk unpacking to at most `chunk_max` bytes (streamed to `w`
/// chunk by chunk).
fn extract_chunked<R: Read + Seek, W: Write>(r: &mut R, p: &Packed, w: &mut W, left: u64, chunk_max: usize) -> Result<u64> {
    if p.size > left {
        bail!("unpacks to more than allowed");
    }
    r.seek(SeekFrom::Start(p.offset))?;
    if p.stored == p.size {
        let n = io::copy(&mut r.take(p.stored), w)?;
        if n != p.stored {
            bail!("cut short");
        }
        return Ok(n);
    }
    let mut h = [0u8; 8];
    r.read_exact(&mut h)?;
    let table = u32_at(&h, 0) as u64;
    if table < 8 || table > p.stored || table > MAX_TABLE {
        bail!("a chunk table of {table} bytes");
    }
    let mut t = vec![0u8; (table - 8) as usize];
    r.read_exact(&mut t)?;
    // (12 bytes an entry, the last one possibly only its first 4)
    let chunks: Vec<u64> = t.chunks(12).filter(|c| c.len() >= 4).map(|c| u32_at(c, 0) as u64).collect();
    let mut rest = p.stored - table;
    let mut n = 0u64;
    let mut buf = Vec::new();
    for c in chunks {
        if rest == 0 {
            break;
        }
        let c = c.min(rest);
        buf.resize(c as usize, 0);
        r.read_exact(&mut buf)?;
        rest -= c;
        let un = depack(&buf, ((p.size - n) as usize).min(chunk_max))?;
        w.write_all(&un)?;
        n += un.len() as u64;
    }
    if rest != 0 || n != p.size {
        bail!("unpacked to {n} bytes, not {}", p.size);
    }
    Ok(n)
}

/// aPLib's depacker (no header): at most `max` bytes out, an error for data that reads past its
/// end or copies from before the output's start.
pub fn depack(src: &[u8], max: usize) -> Result<Vec<u8>> {
    struct Bits<'a> {
        src: &'a [u8],
        at: usize,
        tag: u8,
        left: u8,
    }
    impl Bits<'_> {
        fn byte(&mut self) -> Result<u8> {
            let b = *self.src.get(self.at).context("aPLib data cut short")?;
            self.at += 1;
            Ok(b)
        }
        fn bit(&mut self) -> Result<u32> {
            if self.left == 0 {
                self.tag = self.byte()?;
                self.left = 8;
            }
            self.left -= 1;
            let b = (self.tag >> 7) as u32;
            self.tag <<= 1;
            Ok(b)
        }
        fn gamma(&mut self) -> Result<u32> {
            let mut v = 1u32;
            loop {
                v = v.checked_mul(2).context("aPLib gamma too long")? + self.bit()?;
                if self.bit()? == 0 {
                    return Ok(v);
                }
            }
        }
    }
    let mut out: Vec<u8> = Vec::new();
    if src.is_empty() {
        return Ok(out);
    }
    let mut b = Bits { src, at: 0, tag: 0, left: 0 };
    let put = |out: &mut Vec<u8>, x: u8| -> Result<()> {
        if out.len() >= max {
            bail!("aPLib data unpacks to more than {max} bytes");
        }
        out.push(x);
        Ok(())
    };
    let copy = |out: &mut Vec<u8>, off: usize, len: usize| -> Result<()> {
        if off == 0 || off > out.len() {
            bail!("aPLib match before the start");
        }
        if out.len() + len > max {
            bail!("aPLib data unpacks to more than {max} bytes");
        }
        for _ in 0..len {
            out.push(out[out.len() - off]);
        }
        Ok(())
    };
    let first = b.byte()?;
    put(&mut out, first)?;
    let mut lwm = false;
    let mut r0 = 0usize;
    loop {
        if b.bit()? == 0 {
            let x = b.byte()?;
            put(&mut out, x)?;
            lwm = false;
        } else if b.bit()? == 0 {
            let g = b.gamma()? as usize;
            if !lwm && g == 2 {
                let len = b.gamma()? as usize;
                copy(&mut out, r0, len)?;
            } else {
                let hi = g - if lwm { 2 } else { 3 };
                let off = hi.checked_mul(256).context("aPLib offset too big")? + b.byte()? as usize;
                let mut len = b.gamma()? as usize;
                if off >= 32000 {
                    len += 1;
                }
                if off >= 1280 {
                    len += 1;
                }
                if off < 128 {
                    len += 2;
                }
                copy(&mut out, off, len)?;
                r0 = off;
            }
            lwm = true;
        } else if b.bit()? == 0 {
            let x = b.byte()? as usize;
            let (off, len) = (x >> 1, 2 + (x & 1));
            if off == 0 {
                return Ok(out);
            }
            copy(&mut out, off, len)?;
            r0 = off;
            lwm = true;
        } else {
            let mut off = 0usize;
            for _ in 0..4 {
                off = (off << 1) | b.bit()? as usize;
            }
            if off == 0 {
                put(&mut out, 0)?;
            } else {
                copy(&mut out, off, 1)?;
            }
            lwm = false;
        }
    }
}

/// Unpacks the box of `exe` into `dir` when it holds an NW.js app (a package.json in the exe's
/// folder); returns whether it did.
pub fn unpack(exe: &Path, dir: &Path) -> Result<bool> {
    unpack_capped(exe, dir, None)
}

fn unpack_capped(exe: &Path, dir: &Path, cap: Option<u64>) -> Result<bool> {
    let f = File::open(exe)?;
    let len = f.metadata()?.len();
    let mut r = BufReader::new(f);
    let mut head = vec![0u8; 4096.min(len as usize)];
    r.read_exact(&mut head)?;
    let mut read = |at: u64, n: usize| -> io::Result<Vec<u8>> {
        let mut b = Vec::new();
        r.seek(SeekFrom::Start(at))?;
        (&mut r).take(n as u64).read_to_end(&mut b)?;
        Ok(b)
    };
    let Some(at) = locate(&head, &mut read)? else { return Ok(false) };
    let files = parse(&mut r, at, len).with_context(|| exe.display().to_string())?;
    if !files.iter().any(|(p, s)| s.is_some() && p.as_os_str().eq_ignore_ascii_case("package.json")) {
        return Ok(false);
    }
    let mut left = cap.unwrap_or_else(|| len.saturating_mul(RATIO).clamp(MIN_OUT, MAX_OUT));
    for (path, src) in files {
        let out = dir.join(&path);
        match src {
            None => fs::create_dir_all(&out)?,
            Some(p) => {
                if let Some(d) = out.parent() {
                    fs::create_dir_all(d)?;
                }
                let mut w = io::BufWriter::new(File::create(&out)?);
                let n = extract(&mut r, &p, &mut w, left).with_context(|| format!("{}: {}", exe.display(), path.display()))?;
                w.flush()?;
                left -= n;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    /// aPLib packing of `data` using every code (literals, the 4-bit single bytes, short and long
    /// matches, the repeated offset), picked by `pick` where several fit: what `depack` must undo.
    pub fn pack(data: &[u8], pick: &mut dyn FnMut(usize) -> usize) -> Vec<u8> {
        struct W {
            out: Vec<u8>,
            tag: usize,
            left: u8,
        }
        impl W {
            fn bit(&mut self, b: u32) {
                if self.left == 0 {
                    self.tag = self.out.len();
                    self.out.push(0);
                    self.left = 8;
                }
                self.left -= 1;
                if b != 0 {
                    self.out[self.tag] |= 1 << self.left;
                }
            }
            fn gamma(&mut self, v: u32) {
                assert!(v >= 2);
                let top = 31 - v.leading_zeros();
                for i in (0..top).rev() {
                    self.bit((v >> i) & 1);
                    self.bit((i != 0) as u32);
                }
            }
        }
        let mut w = W { out: Vec::new(), tag: 0, left: 0 };
        if data.is_empty() {
            return w.out;
        }
        w.out.push(data[0]);
        let (mut i, mut lwm, mut r0) = (1usize, false, 0usize);
        // (where each 3 bytes were seen: the far offsets to try)
        let mut seen: std::collections::HashMap<&[u8], Vec<usize>> = std::collections::HashMap::new();
        let mut indexed = 0usize;
        while i < data.len() {
            while indexed + 3 <= i {
                seen.entry(&data[indexed..indexed + 3]).or_default().push(indexed);
                indexed += 1;
            }
            let mut offs: Vec<usize> = (1..=i.min(64)).collect();
            if let Some(ps) = data.get(i..i + 3).and_then(|k| seen.get(k)) {
                offs.extend(ps.iter().rev().take(4).map(|p| i - p).filter(|&o| o > 64));
            }
            // what fits here: (code, offset, length)
            let mut ways: Vec<(u8, usize, usize)> = vec![(0, 0, 1)];
            for off in offs {
                let mut l = 0;
                while i + l < data.len() && data[i + l - off] == data[i + l] && l < 600 {
                    l += 1;
                }
                if l == 0 {
                    continue;
                }
                if off <= 15 {
                    ways.push((3, off, 1));
                }
                if off <= 127 && l >= 2 {
                    ways.push((2, off, 2 + (l >= 3) as usize));
                }
                let min = if off < 128 { 4 } else if off < 1280 { 2 } else if off < 32000 { 3 } else { 4 };
                if l >= min {
                    ways.push((1, off, l));
                }
                if !lwm && off == r0 && l >= 2 {
                    ways.push((4, off, l));
                }
            }
            if data[i] == 0 {
                ways.push((5, 0, 1));
            }
            let (code, off, len) = ways[pick(ways.len())];
            match code {
                0 => {
                    w.bit(0);
                    w.out.push(data[i]);
                    lwm = false;
                }
                1 => {
                    w.bit(1);
                    w.bit(0);
                    w.gamma((off >> 8) as u32 + if lwm { 2 } else { 3 });
                    w.out.push(off as u8);
                    let adj = (off >= 32000) as usize + (off >= 1280) as usize + if off < 128 { 2 } else { 0 };
                    w.gamma((len - adj) as u32);
                    r0 = off;
                    lwm = true;
                }
                2 => {
                    w.bit(1);
                    w.bit(1);
                    w.bit(0);
                    w.out.push((off << 1 | (len - 2)) as u8);
                    r0 = off;
                    lwm = true;
                }
                3 | 5 => {
                    w.bit(1);
                    w.bit(1);
                    w.bit(1);
                    for k in (0..4).rev() {
                        w.bit(((off >> k) & 1) as u32);
                    }
                    lwm = false;
                }
                _ => {
                    w.bit(1);
                    w.bit(0);
                    w.gamma(2);
                    w.gamma(len as u32);
                    lwm = true;
                }
            }
            i += len;
        }
        w.bit(1);
        w.bit(1);
        w.bit(0);
        w.out.push(0);
        w.out
    }

    /// A tree to box: (path, bytes; "dir/" a folder), compressed in chunks of `chunk` bytes or stored.
    pub struct Spec<'a> {
        pub files: &'a [(&'a str, &'a [u8])],
        pub legacy: bool,
        pub chunk: Option<usize>,
        /// a folder beside %DEFAULT FOLDER% (e.g. %SYSTEM FOLDER%) holding a file
        pub other: bool,
    }

    fn stored(b: &[u8], chunk: Option<usize>, seed: &mut u64) -> Vec<u8> {
        let Some(c) = chunk else { return b.to_vec() };
        let mut pick = |n: usize| {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*seed >> 33) as usize % n
        };
        let parts: Vec<Vec<u8>> = b.chunks(c.max(1)).map(|p| pack(p, &mut pick)).collect();
        let mut table = Vec::new();
        for (k, p) in parts.iter().enumerate() {
            table.extend((p.len() as u32).to_le_bytes());
            if k + 1 < parts.len() {
                table.extend((b.len() as u32).to_le_bytes());
                table.extend(0u32.to_le_bytes());
            }
        }
        let mut v = Vec::new();
        v.extend((table.len() as u32 + 8).to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v.extend(table);
        for p in parts {
            v.extend(p);
        }
        // (stored and unpacked sizes equal would read as not compressed)
        if v.len() == b.len() {
            return b.to_vec();
        }
        v
    }

    enum T {
        Dir(String, Vec<T>),
        File(String, Vec<u8>),
    }

    fn insert(into: &mut Vec<T>, parts: &[&str], data: Option<&[u8]>) {
        if parts.len() == 1 {
            match data {
                Some(d) => into.push(T::File(parts[0].into(), d.to_vec())),
                None => {
                    if !into.iter().any(|t| matches!(t, T::Dir(n, _) if n == parts[0])) {
                        into.push(T::Dir(parts[0].into(), vec![]));
                    }
                }
            }
            return;
        }
        if !into.iter().any(|t| matches!(t, T::Dir(n, _) if n == parts[0])) {
            into.push(T::Dir(parts[0].into(), vec![]));
        }
        for t in into.iter_mut() {
            if let T::Dir(n, sub) = t {
                if n == parts[0] {
                    return insert(sub, &parts[1..], data);
                }
            }
        }
    }

    fn name(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        v.extend([0, 0]);
        v
    }

    /// A PE with a .enigma1 section holding a box of `spec`, as Enigma Virtual Box lays it out.
    pub fn exe(spec: &Spec) -> Vec<u8> {
        let mut top = Vec::new();
        for (p, b) in spec.files {
            let parts: Vec<&str> = p.trim_end_matches('/').split('/').collect();
            insert(&mut top, &parts, if p.ends_with('/') { None } else { Some(b) });
        }
        let mut roots = vec![T::Dir("%DEFAULT FOLDER%".into(), top)];
        if spec.other {
            roots.push(T::Dir("%SYSTEM FOLDER%".into(), vec![T::File("evil.dll".into(), b"MZ".to_vec())]));
        }
        let mut seed = 7u64;
        let mut table = Vec::new();
        let mut data = Vec::new();
        fn walk(ts: &[T], legacy: bool, chunk: Option<usize>, seed: &mut u64, table: &mut Vec<u8>, data: &mut Vec<u8>) {
            for t in ts {
                match t {
                    T::Dir(n, sub) => {
                        let nm = name(n);
                        if legacy {
                            let size = 12 + nm.len() + 1 + 25;
                            table.extend((size as u32).to_le_bytes());
                            table.extend([0; 8]);
                            table.extend((sub.len() as u32).to_le_bytes());
                            table.extend(&nm);
                            table.push(3);
                            table.extend([0; 25]);
                        } else {
                            table.extend(0u32.to_le_bytes());
                            table.extend([0; 8]);
                            table.extend((sub.len() as u32).to_le_bytes());
                            table.extend(&nm);
                            table.push(3);
                            table.extend([0; 25]);
                        }
                        walk(sub, legacy, chunk, seed, table, data);
                    }
                    T::File(n, b) => {
                        let s = stored(b, chunk, seed);
                        let nm = name(n);
                        if legacy {
                            let size = 12 + nm.len() + 1 + 49;
                            table.extend((size as u32).to_le_bytes());
                            table.extend([0; 8]);
                            table.extend(0u32.to_le_bytes());
                            table.extend(&nm);
                            table.push(2);
                            let mut opt = [0u8; 49];
                            opt[2..6].copy_from_slice(&(b.len() as u32).to_le_bytes());
                            opt[41..45].copy_from_slice(&(s.len() as u32).to_le_bytes());
                            table.extend(opt);
                            table.extend(&s);
                        } else {
                            table.extend(0u32.to_le_bytes());
                            table.extend([0; 8]);
                            table.extend(0u32.to_le_bytes());
                            table.extend(&nm);
                            table.push(2);
                            let mut opt = [0u8; 53];
                            opt[2..6].copy_from_slice(&(b.len() as u32).to_le_bytes());
                            opt[49..53].copy_from_slice(&(s.len() as u32).to_le_bytes());
                            table.extend(opt);
                            data.extend(&s);
                        }
                    }
                }
            }
        }
        walk(&roots, spec.legacy, spec.chunk, &mut seed, &mut table, &mut data);
        let mut evb = Vec::new();
        evb.extend(MAGIC);
        evb.extend([0x40, 0, 0, 0]);
        evb.extend([0; 56]);
        if spec.legacy {
            evb.extend(15u32.to_le_bytes());
            evb.extend([0; 8]);
            evb.extend((roots.len() as u32).to_le_bytes());
            evb.extend([0, 0, 0]);
            evb.extend(table);
        } else {
            // (the nodes start at the root's last byte; the data at the root's size - 12 after it)
            let size = 12 + table.len() - 1;
            evb.extend((size as u32).to_le_bytes());
            evb.extend([0; 8]);
            evb.extend((roots.len() as u32).to_le_bytes());
            evb.pop();
            evb.extend(table);
            evb.extend(data);
        }
        // the PE: headers, a .text section, then .enigma1 (its first 0xa0 bytes a loader header)
        let mut pe = vec![0u8; 0x400];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x86..0x88].copy_from_slice(&2u16.to_le_bytes());
        pe[0x94..0x96].copy_from_slice(&0xe0u16.to_le_bytes());
        let sec = 0x80 + 24 + 0xe0;
        let mut text = vec![0x90u8; 0x200];
        text[0x10..0x14].copy_from_slice(MAGIC); // (not the box: outside .enigma1)
        let enigma_at = 0x400 + text.len();
        for (i, (n, at, size)) in [(&b".text\0\0\0"[..], 0x400usize, text.len()), (&b".enigma1"[..], enigma_at, 0xa0 + evb.len())].into_iter().enumerate() {
            let s = sec + 40 * i;
            pe[s..s + 8].copy_from_slice(n);
            pe[s + 16..s + 20].copy_from_slice(&(size as u32).to_le_bytes());
            pe[s + 20..s + 24].copy_from_slice(&(at as u32).to_le_bytes());
        }
        pe.extend(text);
        pe.extend([0u8; 0xa0]);
        pe.extend(evb);
        pe
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("masu-evb-{name}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn tree(d: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut todo = vec![d.to_path_buf()];
        while let Some(x) = todo.pop() {
            for e in fs::read_dir(&x).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    todo.push(p);
                } else {
                    out.push((p.strip_prefix(d).unwrap().to_string_lossy().replace('\\', "/"), fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    const APP: &[(&str, &[u8])] = &[
        ("package.json", b"{\"main\":\"www/index.html\"}"),
        ("www/index.html", b"<html>"),
        ("www/js/rpg_core.js", b"var Graphics = {}; var Graphics = {};"),
        ("www/img/titles1/タイトル.png", b"\x89PNG\r\n\x1a\n\0\0\0\0\0\0\0\0"),
        ("www/save/", b""),
    ];

    fn want() -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = APP.iter().filter(|f| !f.0.ends_with('/')).map(|(n, b)| (n.to_string(), b.to_vec())).collect();
        v.sort();
        v
    }

    #[test]
    fn unpacks_both_layouts_stored_and_compressed() {
        for legacy in [true, false] {
            for chunk in [None, Some(5), Some(1 << 16)] {
                let d = tmp("ok");
                fs::write(d.join("game.exe"), exe(&Spec { files: APP, legacy, chunk, other: true })).unwrap();
                assert!(unpack(&d.join("game.exe"), &d.join("app")).unwrap(), "{legacy} {chunk:?}");
                assert_eq!(tree(&d.join("app")), want(), "{legacy} {chunk:?}");
                assert!(d.join("app/www/save").is_dir());
                let _ = fs::remove_dir_all(&d);
            }
        }
    }

    #[test]
    fn not_an_nwjs_box_or_not_a_box() {
        let d = tmp("no");
        fs::write(d.join("a.exe"), exe(&Spec { files: &[("Game.exe", b"MZ"), ("data.xp3", b"XP3")], legacy: true, chunk: None, other: false })).unwrap();
        assert!(!unpack(&d.join("a.exe"), &d.join("out")).unwrap());
        // ("EVB\0" outside .enigma1 is not a box)
        let mut e = exe(&Spec { files: APP, legacy: false, chunk: None, other: false });
        let s = 0x80 + 24 + 0xe0 + 40;
        e[s..s + 8].copy_from_slice(b".rsrc\0\0\0");
        fs::write(d.join("b.exe"), &e).unwrap();
        assert!(!unpack(&d.join("b.exe"), &d.join("out")).unwrap());
        fs::write(d.join("c.exe"), b"MZ").unwrap();
        assert!(!unpack(&d.join("c.exe"), &d.join("out")).unwrap());
        assert!(!d.join("out").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn names_that_leave_the_folder_are_skipped() {
        let files: &[(&str, &[u8])] = &[("package.json", b"{}"), ("..", b"x"), ("C:x", b"x"), ("a\\..\\..\\b", b"x"), ("ok.txt", b"ok")];
        for legacy in [true, false] {
            let d = tmp("evil");
            fs::write(d.join("game.exe"), exe(&Spec { files, legacy, chunk: None, other: true })).unwrap();
            assert!(unpack(&d.join("game.exe"), &d.join("x/app")).unwrap());
            let got: Vec<String> = tree(&d).into_iter().map(|t| t.0).collect();
            assert_eq!(got, ["game.exe", "x/app/ok.txt", "x/app/package.json"]);
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn depack_known_streams() {
        // (by hand: "a", then literal "b", the single-byte code copying "a" (offset 2), then 110
        // with offset 0: the end)
        let src = [b'a', 0b0111_0010, b'b', 0b1100_0000, 0];
        assert_eq!(depack(&src, 100).unwrap(), b"aba");
        assert!(depack(&[b'a', 0b1110_0000], 100).is_err()); // cut short
        assert!(depack(&[b'a', 0b1100_0000, 0x08], 100).is_err()); // offset 4 of 1 byte
        assert_eq!(depack(&[], 100).unwrap(), b"");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        /// Every way of packing any bytes depacks to them, and a cap below their size is an error.
        #[test]
        fn depack_undoes_pack(data in prop::collection::vec(prop::sample::select(vec![0u8, 1, b'a', b'b', 0xff]), 0..400), seed in any::<u64>()) {
            let mut s = seed;
            let p = pack(&data, &mut |n| { s = s.wrapping_mul(6364136223846793005).wrapping_add(1); (s >> 33) as usize % n });
            prop_assert_eq!(depack(&p, data.len()).unwrap(), data.clone());
            if !data.is_empty() {
                prop_assert!(depack(&p, data.len() - 1).is_err());
            }
        }

        /// Long offsets (the 1280 and 32000 length adjustments) round-trip too.
        #[test]
        fn depack_far_matches(head in prop::collection::vec(any::<u8>(), 1..40), gap in 100usize..40000, seed in any::<u64>()) {
            let mut data = head.clone();
            data.extend((0..gap).map(|i| (i * 7 % 251) as u8));
            data.extend(&head);
            data.extend(&head);
            let mut s = seed;
            // (prefer the longest match: the far one)
            let p = pack(&data, &mut |n| { s = s.wrapping_mul(6364136223846793005).wrapping_add(1); if s >> 62 == 0 { (s >> 33) as usize % n } else { n - 1 } });
            prop_assert_eq!(depack(&p, data.len()).unwrap(), data);
        }

        /// Any tree round-trips through both layouts, stored or compressed.
        #[test]
        fn round_trip(files in prop::collection::btree_map("[a-zあ-お]{1,6}(/[a-z]{1,6}){0,2}", prop::collection::vec(any::<u8>(), 0..300), 1..8), legacy in any::<bool>(), chunk in prop::option::of(1usize..200)) {
            let mut files: Vec<(String, Vec<u8>)> = files.into_iter().collect();
            let names: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
            files.retain(|(n, _)| !names.iter().any(|m| m.starts_with(&format!("{n}/"))) && n != "package.json");
            files.push(("package.json".into(), b"{}".to_vec()));
            files.sort();
            let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
            let d = tmp("rt");
            fs::write(d.join("game.exe"), exe(&Spec { files: &refs, legacy, chunk, other: true })).unwrap();
            prop_assert!(unpack(&d.join("game.exe"), &d.join("app")).unwrap());
            let got = tree(&d.join("app"));
            let _ = fs::remove_dir_all(&d);
            prop_assert_eq!(got, files);
        }

        /// Damaged boxes: an error or a result, never a panic or a file outside the folder.
        #[test]
        fn damaged(edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..8), cut in any::<usize>(), legacy in any::<bool>(), compressed in any::<bool>()) {
            let mut e = exe(&Spec { files: APP, legacy, chunk: compressed.then_some(7), other: true });
            let box_at = 0x600 + 0xa0;
            for (at, b) in edits {
                let i = box_at + at % (e.len() - box_at);
                e[i] = b;
            }
            e.truncate(cut % (e.len() + 1));
            let d = tmp("dmg");
            fs::write(d.join("game.exe"), &e).unwrap();
            // (a damaged gamma can ask for 2^31 bytes: capped low to keep the case fast)
            let _ = unpack_capped(&d.join("game.exe"), &d.join("x/app"), Some(1 << 20));
            let outside: Vec<String> = tree(&d).into_iter().map(|t| t.0).filter(|n| n != "game.exe" && !n.starts_with("x/app/")).collect();
            let _ = fs::remove_dir_all(&d);
            prop_assert!(outside.is_empty(), "{:?}", outside);
        }
    }

    /// A chunk that unpacks to more than one chunk may is an error (a damaged box's gamma can
    /// ask for 2^31 bytes, built byte by byte in memory before); one within it unpacks
    #[test]
    fn chunks_are_capped() {
        let big: Vec<u8> = (0..20_000u32).map(|i| (i % 7) as u8).collect();
        let files: &[(&str, &[u8])] = &[("big.bin", &big)];
        let e = exe(&Spec { files, legacy: false, chunk: Some(20_000), other: false });
        let at = locate(&e[..e.len().min(4096)], &mut |o, l| Ok(e[o as usize..(o as usize + l).min(e.len())].to_vec())).unwrap().unwrap();
        let mut c = Cursor::new(&e);
        let list = parse(&mut c, at, e.len() as u64).unwrap();
        let (_, p) = list.iter().find(|f| f.0.as_os_str() == "big.bin").unwrap();
        let p = p.as_ref().unwrap();
        assert!(p.stored < p.size, "compressed");
        let mut out = Vec::new();
        assert!(extract_chunked(&mut c, p, &mut out, 1 << 30, 10_000).is_err());
        out.clear();
        assert_eq!(extract_chunked(&mut c, p, &mut out, 1 << 30, 20_000).unwrap(), 20_000);
        assert_eq!(out, big);
        out.clear();
        assert_eq!(extract(&mut c, p, &mut out, 1 << 30).unwrap(), 20_000);
    }

    /// The boxes Enigma Virtual Box itself made, when a folder of them is given (EVB_SAMPLES: a
    /// folder of evbunpack's tests/, whose *_packed_*.exe box README.txt, README_packed.txt's
    /// bytes): each unpacks to them.
    #[test]
    fn real_boxes() {
        let Ok(dir) = std::env::var("EVB_SAMPLES") else { return };
        let readme = fs::read(Path::new(&dir).join("README_packed.txt")).unwrap();
        let mut n = 0;
        for e in fs::read_dir(&dir).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.contains("_packed_") {
                continue;
            }
            let b = fs::read(e.path()).unwrap();
            let mut c = Cursor::new(&b);
            let at = locate(&b[..4096], &mut |o, l| Ok(b[o as usize..(o as usize + l).min(b.len())].to_vec())).unwrap().expect(&name);
            let files = parse(&mut c, at, b.len() as u64).expect(&name);
            let (_, p) = files.iter().find(|f| f.0.as_os_str() == "README.txt").expect(&name);
            let mut out = Vec::new();
            extract(&mut c, p.as_ref().unwrap(), &mut out, 1 << 20).unwrap();
            assert_eq!(out, readme, "{name}");
            n += 1;
        }
        assert!(n > 0);
    }
}
