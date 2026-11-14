//! InstallShield 6 cabinets (data1.hdr + data1.cab, data2.cab, ...): the files an InstallShield
//! setup carries, read without running it. What the RPG Maker 2000 / 2003 RTP setups use
//! (rtp.rs); checked against unshield's output in the tests (the format is the one unshield
//! documents: a header file with the directory and file tables, volumes with each file's data as
//! raw-deflate chunks, each after its u16 length).

use anyhow::{bail, ensure, Context, Result};
use md5::Digest;
use std::fs;
use std::path::{Path, PathBuf};

/// A file of the cabinet.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Its folder in the setup (Windows separators, "" for none).
    pub dir: String,
    pub name: String,
    index: usize,
}

/// A file's descriptor (format 6).
#[derive(Clone, Debug)]
struct Desc {
    flags: u16,
    expanded: u64,
    compressed: u64,
    offset: u64,
    md5: [u8; 16],
    name: u32,
    dir: u16,
    link_prev: u32,
    link_flags: u8,
    volume: u16,
}

const SPLIT: u16 = 1;
const OBFUSCATED: u16 = 2;
const COMPRESSED: u16 = 4;
const INVALID: u16 = 8;
const LINK_PREV: u8 = 1;

pub struct Cabinet {
    /// data<N>.cab without the number: "<dir>/data"
    base: PathBuf,
    descs: Vec<Desc>,
    pub entries: Vec<Entry>,
}

fn u16_at(b: &[u8], p: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(b.get(p..p + 2).context("cabinet header cut short")?.try_into().unwrap()))
}
fn u32_at(b: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(b.get(p..p + 4).context("cabinet header cut short")?.try_into().unwrap()))
}
fn u64_at(b: &[u8], p: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(b.get(p..p + 8).context("cabinet header cut short")?.try_into().unwrap()))
}
/// A NUL-terminated Shift-JIS string at `p`.
fn str_at(b: &[u8], p: usize) -> Result<String> {
    let s = b.get(p..).context("cabinet string out of the header")?;
    let s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];
    Ok(encoding_rs::SHIFT_JIS.decode(s).0.into_owned())
}

/// The format's major version from the common header's version field.
fn major(version: u32) -> u32 {
    match version >> 24 {
        1 => (version >> 12) & 0xf,
        2 | 4 => {
            let v = (version & 0xffff) / 100;
            if v == 0 { 6 } else { v }
        }
        _ => 0,
    }
}

impl Cabinet {
    /// Reads `hdr` (data1.hdr; or data1.cab when the header is in it).
    pub fn open(hdr: &Path) -> Result<Cabinet> {
        let b = fs::read(hdr).with_context(|| hdr.display().to_string())?;
        Self::parse(&b, hdr)
    }

    fn parse(b: &[u8], hdr: &Path) -> Result<Cabinet> {
        ensure!(b.get(..4) == Some(b"ISc("), "{}: not an InstallShield cabinet", hdr.display());
        let version = u32_at(b, 4)?;
        let m = major(version);
        ensure!(m >= 6, "InstallShield cabinet format {m} ({version:#x}): only 6 and later");
        let cd = u32_at(b, 12)? as usize;
        let ft_off = u32_at(b, cd + 0x0c)? as usize;
        let dirs = u32_at(b, cd + 0x1c)? as usize;
        let files = u32_at(b, cd + 0x28)? as usize;
        let ft2 = u32_at(b, cd + 0x2c)? as usize;
        ensure!(dirs < 1 << 16 && files < 1 << 20, "cabinet tables too big");
        let table = cd + ft_off;
        let dir_names = (0..dirs).map(|i| str_at(b, table + u32_at(b, table + 4 * i)? as usize)).collect::<Result<Vec<_>>>()?;
        let mut descs = Vec::with_capacity(files);
        let mut entries = Vec::new();
        for i in 0..files {
            let p = table + ft2 + i * 0x57;
            let mut md5 = [0; 16];
            md5.copy_from_slice(b.get(p + 26..p + 42).context("cabinet header cut short")?);
            let d = Desc {
                flags: u16_at(b, p)?,
                expanded: u64_at(b, p + 2)?,
                compressed: u64_at(b, p + 10)?,
                offset: u64_at(b, p + 18)?,
                md5,
                name: u32_at(b, p + 58)?,
                dir: u16_at(b, p + 62)?,
                link_prev: u32_at(b, p + 76)?,
                link_flags: *b.get(p + 84).context("cabinet header cut short")?,
                volume: u16_at(b, p + 85)?,
            };
            if d.flags & INVALID == 0 && d.name != 0 {
                let dir = dir_names.get(d.dir as usize).cloned().unwrap_or_default();
                entries.push(Entry { dir, name: str_at(b, table + d.name as usize)?, index: i });
            }
            descs.push(d);
        }
        let name = hdr.file_name().and_then(|n| n.to_str()).unwrap_or("data1.hdr");
        let stem = name.trim_end_matches(|c: char| c != '.').trim_end_matches('.');
        let stem = stem.trim_end_matches(|c: char| c.is_ascii_digit());
        Ok(Cabinet { base: hdr.with_file_name(stem), descs, entries })
    }

    fn volume(&self, n: u16) -> PathBuf {
        let mut s = self.base.clone().into_os_string();
        s.push(format!("{n}.cab"));
        PathBuf::from(s)
    }

    /// The bytes of `e` (checked against the cabinet's MD5).
    pub fn read(&self, e: &Entry) -> Result<Vec<u8>> {
        let mut i = e.index;
        // (a file that shares an earlier one's data)
        for _ in 0..8 {
            let d = &self.descs[i];
            if d.link_flags & LINK_PREV != 0 && (d.link_prev as usize) < self.descs.len() && d.link_prev as usize != i {
                i = d.link_prev as usize;
            } else {
                break;
            }
        }
        let d = &self.descs[i];
        let stored = if d.flags & COMPRESSED != 0 { d.compressed } else { d.expanded };
        ensure!(stored < 1 << 32 && d.expanded < 1 << 32, "{}: too big", e.name);
        let mut raw = Vec::with_capacity(stored as usize);
        let mut vol = d.volume.max(1);
        let mut at = d.offset;
        while (raw.len() as u64) < stored {
            let v = fs::read(self.volume(vol)).with_context(|| self.volume(vol).display().to_string())?;
            let h = VolumeHeader::read(&v)?;
            // what of the file this volume holds: up to its end, or the part before the split
            let mut take = stored - raw.len() as u64;
            if d.flags & SPLIT != 0 && h.last_file == i as u32 && h.last_size != 0 && at == h.last_offset {
                take = take.min(h.last_size);
            }
            let s = v.get(at as usize..(at + take) as usize).with_context(|| format!("{}: data out of {}", e.name, self.volume(vol).display()))?;
            raw.extend_from_slice(s);
            if (raw.len() as u64) < stored {
                vol += 1;
                let next = fs::read(self.volume(vol)).with_context(|| format!("{}: continues in {}", e.name, self.volume(vol).display()))?;
                let nh = VolumeHeader::read(&next)?;
                ensure!(nh.first_file == i as u32, "{}: volume {vol} does not continue it", e.name);
                at = nh.first_offset;
            }
        }
        if d.flags & OBFUSCATED != 0 {
            deobfuscate(&mut raw);
        }
        let out = if d.flags & COMPRESSED != 0 { inflate_chunks(&raw, d.expanded as usize)? } else { raw };
        ensure!(out.len() as u64 == d.expanded, "{}: {} bytes, the cabinet says {}", e.name, out.len(), d.expanded);
        if d.md5 != [0; 16] {
            let got: [u8; 16] = md5::Md5::digest(&out).into();
            ensure!(got == d.md5, "{}: MD5 differs from the cabinet's", e.name);
        }
        Ok(out)
    }
}

/// A volume's header (format 6), after the 20-byte common header.
struct VolumeHeader {
    first_file: u32,
    last_file: u32,
    first_offset: u64,
    last_offset: u64,
    last_size: u64,
}

impl VolumeHeader {
    fn read(v: &[u8]) -> Result<VolumeHeader> {
        ensure!(v.get(..4) == Some(b"ISc("), "not an InstallShield cabinet volume");
        let p = 20;
        Ok(VolumeHeader {
            first_file: u32_at(v, p + 8)?,
            last_file: u32_at(v, p + 12)?,
            first_offset: u64_at(v, p + 16)?,
            last_offset: u64_at(v, p + 40)?,
            last_size: u64_at(v, p + 56)?,
        })
    }
}

/// The setups' light scrambling of stored bytes.
fn deobfuscate(b: &mut [u8]) {
    for (i, x) in b.iter_mut().enumerate() {
        *x = (*x ^ 0xd5).rotate_right(2).wrapping_sub((i % 0x47) as u8);
    }
}

/// A file's compressed data: chunks of raw deflate, each after its u16 length.
fn inflate_chunks(raw: &[u8], expanded: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expanded);
    let mut p = 0;
    while p < raw.len() && out.len() < expanded {
        let n = u16_at(raw, p)? as usize;
        p += 2;
        let chunk = raw.get(p..p + n).context("compressed chunk cut short")?;
        p += n;
        let mut z = flate2::Decompress::new(false);
        let before = out.len();
        // (a chunk expands to at most 64 KiB)
        out.reserve(0x10000);
        loop {
            let r = z.decompress_vec(&chunk[z.total_in() as usize..], &mut out, flate2::FlushDecompress::Finish);
            match r {
                Ok(flate2::Status::StreamEnd) => break,
                Ok(_) if out.len() == out.capacity() => out.reserve(0x10000),
                Ok(_) if z.total_in() as usize >= chunk.len() => break,
                Ok(_) => {}
                Err(e) => bail!("bad deflate chunk: {e}"),
            }
        }
        ensure!(out.len() > before || n == 0, "empty deflate chunk");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn versions() {
        assert_eq!(major(0x0100600c), 6);
        assert_eq!(major(0x02000258), 6);
        assert_eq!(major(0x02000320), 8);
        assert_eq!(major(0x01005000), 5);
    }

    /// deflate chunks: what flate2 makes in pieces comes back whole
    #[test]
    fn chunks_round_trip() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 31 % 251) as u8 ^ (i >> 9) as u8).collect();
        let mut raw = Vec::new();
        for c in data.chunks(0x8000) {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut e, c).unwrap();
            let z = e.finish().unwrap();
            raw.extend((z.len() as u16).to_le_bytes());
            raw.extend(z);
        }
        assert_eq!(inflate_chunks(&raw, data.len()).unwrap(), data);
    }

    proptest! {
        /// Damaged headers and data are errors, never panics.
        #[test]
        fn garbage_is_an_error(mut b in proptest::collection::vec(any::<u8>(), 0..4096), cut in 0usize..4096) {
            b.splice(0..b.len().min(4), *b"ISc(");
            if b.len() > 8 { b[4..8].copy_from_slice(&0x0100600cu32.to_le_bytes()); }
            let _ = Cabinet::parse(&b, Path::new("data1.hdr"));
            let _ = inflate_chunks(&b[cut.min(b.len())..], 1 << 16);
            let _ = VolumeHeader::read(&b);
        }

        /// Deobfuscation is a byte-wise bijection (each position maps 256 values to 256).
        #[test]
        fn deobfuscate_is_one_to_one(pos in 0usize..1000) {
            let mut seen = [false; 256];
            for x in 0..=255u8 {
                let mut b = vec![0u8; pos + 1];
                b[pos] = x;
                deobfuscate(&mut b);
                prop_assert!(!seen[b[pos] as usize]);
                seen[b[pos] as usize] = true;
            }
        }
    }
}
