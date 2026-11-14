//! InstallShield 6 cabinets (data1.hdr + data1.cab, data2.cab, ...): the files an InstallShield
//! setup carries, read without running it. What the RPG Maker 2000 / 2003 RTP setups use
//! (rtp.rs); checked against unshield's output in the tests (the format is the one unshield
//! documents: a header file with the directory and file tables, volumes with each file's data as
//! raw-deflate chunks, each after its u16 length).

use anyhow::{bail, ensure, Context, Result};
use md5::Digest;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
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
        // (as many as the header can hold: a damaged count reserves nothing)
        let mut descs = Vec::with_capacity(files.min(b.len() / 0x57));
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

    /// Volume `n`: its header, the open file and its length.
    fn open_volume(&self, n: u16) -> Result<(VolumeHeader, fs::File, u64)> {
        let mut f = fs::File::open(self.volume(n))?;
        let len = f.metadata()?.len();
        let mut head = Vec::new();
        (&mut f).take(VolumeHeader::LEN).read_to_end(&mut head)?;
        Ok((VolumeHeader::read(&head)?, f, len))
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
        // (each volume opened once, and only the file's span of it read: not the whole
        // dataN.cab per file)
        let mut raw = Vec::new();
        let mut vol = d.volume.max(1);
        let mut at = d.offset;
        let mut cur = self.open_volume(vol).with_context(|| self.volume(vol).display().to_string())?;
        while (raw.len() as u64) < stored {
            let h = &cur.0;
            // what of the file this volume holds: up to its end, or the part before the split
            let mut take = stored - raw.len() as u64;
            if d.flags & SPLIT != 0 && h.last_file == i as u32 && h.last_size != 0 && at == h.last_offset {
                take = take.min(h.last_size);
            }
            ensure!(at.checked_add(take).is_some_and(|end| end <= cur.2), "{}: data out of {}", e.name, self.volume(vol).display());
            cur.1.seek(SeekFrom::Start(at))?;
            let from = raw.len();
            raw.resize(from + take as usize, 0);
            cur.1.read_exact(&mut raw[from..]).with_context(|| format!("{}: data out of {}", e.name, self.volume(vol).display()))?;
            if (raw.len() as u64) < stored {
                vol += 1;
                cur = self.open_volume(vol).with_context(|| format!("{}: continues in {}", e.name, self.volume(vol).display()))?;
                ensure!(cur.0.first_file == i as u32, "{}: volume {vol} does not continue it", e.name);
                at = cur.0.first_offset;
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
    /// the bytes `read` looks at
    const LEN: u64 = 20 + 64;
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
    // (what the chunks can give, not what a damaged header claims)
    let mut out = Vec::with_capacity(expanded.min(raw.len().saturating_mul(4)).min(64 << 20));
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

    /// A format-6 cabinet as an InstallShield setup lays it out: data1.hdr with the tables and
    /// the volumes data1.cab, data2.cab ... of `vol` bytes at most each (a file that crosses a
    /// volume's end is split: SPLIT, the volume's last_file/last_offset/last_size, the next one's
    /// first_file/first_offset). Files: (name, bytes, compressed, obfuscated).
    fn build(dir: &Path, files: &[(String, Vec<u8>, bool, bool)], vol: usize) {
        let mut stored: Vec<Vec<u8>> = Vec::new();
        for (_, data, zip, obf) in files {
            let mut raw = if *zip {
                let mut r = Vec::new();
                for c in data.chunks(0x8000) {
                    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
                    std::io::Write::write_all(&mut e, c).unwrap();
                    let z = e.finish().unwrap();
                    r.extend((z.len() as u16).to_le_bytes());
                    r.extend(z);
                }
                r
            } else {
                data.clone()
            };
            if *obf {
                // (the inverse of deobfuscate)
                for (i, x) in raw.iter_mut().enumerate() {
                    *x = x.wrapping_add((i % 0x47) as u8).rotate_left(2) ^ 0xd5;
                }
            }
            stored.push(raw);
        }
        // volumes: a 20 + 64 byte header, then the files' bytes
        const HEAD: usize = 84;
        let mut vols: Vec<Vec<u8>> = vec![vec![0; HEAD]];
        let mut place = vec![(0u16, 0u64, false); files.len()];
        let mut firsts: Vec<(u32, u64)> = vec![(0, 0)];
        let mut lasts: Vec<(u32, u64, u64)> = vec![(0, 0, 0)];
        for (i, raw) in stored.iter().enumerate() {
            let mut left = &raw[..];
            let mut first = true;
            loop {
                let v = vols.len() - 1;
                let room = vol.saturating_sub(vols[v].len()).max(1);
                let n = left.len().min(room);
                let at = vols[v].len() as u64;
                if first {
                    place[i] = (v as u16 + 1, at, false);
                } else {
                    firsts[v] = (i as u32, at);
                }
                vols[v].extend_from_slice(&left[..n]);
                left = &left[n..];
                first = false;
                if left.is_empty() {
                    break;
                }
                place[i].2 = true;
                lasts[v] = (i as u32, at, n as u64);
                vols.push(vec![0; HEAD]);
                firsts.push((0, 0));
                lasts.push((0, 0, 0));
            }
        }
        for (v, b) in vols.iter_mut().enumerate() {
            b[..4].copy_from_slice(b"ISc(");
            b[28..32].copy_from_slice(&firsts[v].0.to_le_bytes());
            b[32..36].copy_from_slice(&lasts[v].0.to_le_bytes());
            b[36..44].copy_from_slice(&firsts[v].1.to_le_bytes());
            b[60..68].copy_from_slice(&lasts[v].1.to_le_bytes());
            b[76..84].copy_from_slice(&lasts[v].2.to_le_bytes());
            fs::write(dir.join(format!("data{}.cab", v + 1)), b).unwrap();
        }
        // the header: common header, cd at 0x200 (table at cd + 0x100: dir offset, names, descs)
        let (cd, ft_off) = (0x200usize, 0x100usize);
        let table = cd + ft_off;
        let ft2 = 0x40usize;
        let mut names = Vec::new();
        let names_at = ft2 + files.len() * 0x57;
        let mut b = vec![0u8; table + names_at];
        b[..4].copy_from_slice(b"ISc(");
        b[4..8].copy_from_slice(&0x0100600cu32.to_le_bytes());
        b[12..16].copy_from_slice(&(cd as u32).to_le_bytes());
        b[cd + 0x0c..cd + 0x10].copy_from_slice(&(ft_off as u32).to_le_bytes());
        b[cd + 0x1c..cd + 0x20].copy_from_slice(&1u32.to_le_bytes());
        b[cd + 0x28..cd + 0x2c].copy_from_slice(&(files.len() as u32).to_le_bytes());
        b[cd + 0x2c..cd + 0x30].copy_from_slice(&(ft2 as u32).to_le_bytes());
        let name = |names: &mut Vec<u8>, s: &str| {
            let at = names_at + names.len();
            names.extend(s.as_bytes());
            names.push(0);
            at as u32
        };
        let d0 = name(&mut names, "rtp");
        b[table..table + 4].copy_from_slice(&d0.to_le_bytes());
        for (i, (n, data, zip, obf)) in files.iter().enumerate() {
            let p = table + ft2 + i * 0x57;
            let flags = (*zip as u16) * COMPRESSED | (*obf as u16) * OBFUSCATED | (place[i].2 as u16) * SPLIT;
            b[p..p + 2].copy_from_slice(&flags.to_le_bytes());
            b[p + 2..p + 10].copy_from_slice(&(data.len() as u64).to_le_bytes());
            b[p + 10..p + 18].copy_from_slice(&(stored[i].len() as u64).to_le_bytes());
            b[p + 18..p + 26].copy_from_slice(&place[i].1.to_le_bytes());
            b[p + 26..p + 42].copy_from_slice(&<[u8; 16]>::from(md5::Md5::digest(data)));
            let at = name(&mut names, n);
            b[p + 58..p + 62].copy_from_slice(&at.to_le_bytes());
            b[p + 85..p + 87].copy_from_slice(&place[i].0.to_le_bytes());
        }
        b.extend(names);
        fs::write(dir.join("data1.hdr"), b).unwrap();
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ippai-iscab-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Files read back from a cabinet over several volumes, split ones too; a volume cut
    /// short or a damaged size is an error, not a read of everything
    #[test]
    fn files_across_volumes() {
        let dir = scratch();
        let files: Vec<(String, Vec<u8>, bool, bool)> = (0..7u32)
            .map(|k| (format!("f{k}.bin"), (0..(k * 37_000 + 5)).map(|i| (i * (k + 3) % 253) as u8 ^ (i >> 11) as u8).collect(), k % 2 == 0, k % 3 == 0))
            .collect();
        build(&dir, &files, 100_000);
        assert!(dir.join("data3.cab").exists(), "several volumes");
        let c = Cabinet::open(&dir.join("data1.hdr")).unwrap();
        assert_eq!(c.entries.len(), 7);
        for (e, f) in c.entries.iter().zip(&files) {
            assert_eq!((e.dir.as_str(), e.name.as_str()), ("rtp", f.0.as_str()));
            assert_eq!(c.read(e).unwrap(), f.1, "{}", f.0);
        }
        // the last volume cut: the files in it fail, the ones before still read
        let last = (1..).map(|v| dir.join(format!("data{v}.cab"))).take_while(|p| p.exists()).last().unwrap();
        let v = fs::read(&last).unwrap();
        fs::write(&last, &v[..v.len() - 10]).unwrap();
        assert!(c.read(&c.entries[6]).is_err());
        assert_eq!(c.read(&c.entries[0]).unwrap(), files[0].1);
        // a size of 4 GiB - 1 in the table: an error before anything that size is allocated
        let mut h = fs::read(dir.join("data1.hdr")).unwrap();
        let p = 0x300 + 0x40 + 1 * 0x57;
        h[p + 10..p + 18].copy_from_slice(&0xffff_fff0u64.to_le_bytes());
        h[p + 2..p + 10].copy_from_slice(&0xffff_fff0u64.to_le_bytes());
        fs::write(dir.join("data1.hdr"), h).unwrap();
        let c = Cabinet::open(&dir.join("data1.hdr")).unwrap();
        assert!(c.read(&c.entries[1]).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        /// any files, any volume size: every file reads back as it was
        #[test]
        fn any_layout_reads_back(sizes in proptest::collection::vec((0usize..60_000, any::<bool>(), any::<bool>(), any::<u8>()), 1..8), vol in 200usize..80_000) {
            let dir = scratch();
            let files: Vec<(String, Vec<u8>, bool, bool)> = sizes.iter().enumerate().map(|(k, &(n, zip, obf, seed))| {
                (format!("f{k}"), (0..n).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(seed as u32).to_le_bytes()[(i / 700) % 4]).collect(), zip, obf)
            }).collect();
            build(&dir, &files, vol);
            let c = Cabinet::open(&dir.join("data1.hdr")).unwrap();
            prop_assert_eq!(c.entries.len(), files.len());
            for (e, f) in c.entries.iter().zip(&files) {
                prop_assert_eq!(c.read(e).unwrap(), f.1.clone());
            }
            fs::remove_dir_all(dir).unwrap();
        }
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
