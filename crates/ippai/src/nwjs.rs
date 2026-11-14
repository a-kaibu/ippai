//! NW.js games ship their app as a zip appended to nw.exe ("copy /b nw.exe+app.nw") or as
//! package.nw. The page runs such games as web pages (web/src/html.ts), which needs the files
//! themselves: `ippai pack` adds the zip's files next to the exe.
//!
//! The exe is read from disk as needed (only its tail to find the zip), and what the zip unpacks
//! to is capped (a zip bomb stops with an error instead of filling the disk).

use anyhow::{bail, Result};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use zip::ZipArchive;

/// Files a zip may hold.
const MAX_ENTRIES: usize = 200_000;
/// Bytes it may unpack to: this many times its own size, at least `MIN_OUT`, at most `MAX_OUT`.
const RATIO: u64 = 50;
const MIN_OUT: u64 = 256 << 20;
const MAX_OUT: u64 = 16 << 30;
/// Bytes its central directory may take (the zip crate reads all of it, names and all, before
/// anything can be counted: review b01). 200,000 entries with 300-byte names fit.
const MAX_DIRECTORY: u64 = 64 << 20;

/// Where the zip whose end of central directory is in `tail` (the last bytes of the file, which
/// starts `tail_at` bytes in) starts: its offsets count from there, so they are off by the size
/// of the exe in front of it.
#[cfg(test)]
fn zip_start(tail: &[u8], tail_at: u64) -> Option<u64> {
    zip_directory(tail, tail_at).map(|(start, _)| start)
}

/// [`zip_start`] and the size of the central directory.
fn zip_directory(tail: &[u8], tail_at: u64) -> Option<(u64, u64)> {
    let eocd = tail.windows(4).rposition(|w| w == b"PK\x05\x06")?;
    let field = |at: usize| {
        let b = tail.get(eocd + at..eocd + at + 4)?;
        Some(u32::from_le_bytes(b.try_into().ok()?) as u64)
    };
    let (size, offset) = (field(12)?, field(16)?);
    Some(((tail_at + eocd as u64).checked_sub(size)?.checked_sub(offset)?, size))
}

/// The part of a file from `start` on, as a file of its own.
struct Part {
    f: File,
    start: u64,
    len: u64,
}

impl Read for Part {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.f.read(buf)
    }
}

impl Seek for Part {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let at = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => (self.f.stream_position()? - self.start).checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.f.seek(SeekFrom::Start(self.start + at))? - self.start)
    }
}

/// Unpacks the app zip in `file` into `dir` when it holds a package.json; returns whether it did.
pub fn unpack(file: &Path, dir: &Path) -> Result<bool> {
    let mut f = File::open(file)?;
    let len = f.metadata()?.len();
    let tail_at = len.saturating_sub(22 + 0xffff);
    let mut tail = Vec::new();
    f.seek(SeekFrom::Start(tail_at))?;
    (&mut f).take(len - tail_at).read_to_end(&mut tail)?;
    let Some((start, directory)) = zip_directory(&tail, tail_at) else { return Ok(false) };
    if directory > MAX_DIRECTORY {
        bail!("{}: its app zip's directory takes {} MiB (at most {} MiB)", file.display(), directory >> 20, MAX_DIRECTORY >> 20);
    }
    let Ok(mut zip) = ZipArchive::new(Part { f, start, len: len - start }) else { return Ok(false) };
    if zip.index_for_name("package.json").is_none() {
        return Ok(false);
    }
    if zip.len() > MAX_ENTRIES {
        bail!("{}: {} files in its app zip (at most {MAX_ENTRIES})", file.display(), zip.len());
    }
    let cap = ((len - start).saturating_mul(RATIO)).clamp(MIN_OUT, MAX_OUT);
    let mut left = cap;
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        // (names that would leave the folder are skipped)
        let Some(name) = e.enclosed_name() else { continue };
        let out = dir.join(name);
        if e.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            fs::create_dir_all(p)?;
        }
        // (the sizes in the zip are not trusted: what comes out is counted)
        let n = io::copy(&mut (&mut e).take(left + 1), &mut File::create(&out)?)?;
        if n > left {
            bail!("{}: its app zip unpacks to more than {} MiB", file.display(), cap >> 20);
        }
        left -= n;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn exe_with_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in files {
            z.start_file(*name, o).unwrap();
            z.write_all(data).unwrap();
        }
        let zip = z.finish().unwrap().into_inner();
        [b"MZ fake exe in front".repeat(100), zip].concat()
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ippai-nwjs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unpacks_the_zip_after_the_exe() {
        let d = tmp("ok");
        fs::write(d.join("Game.exe"), exe_with_zip(&[("package.json", b"{\"main\":\"index.html\"}"), ("www/index.html", b"<html>")])).unwrap();
        assert!(unpack(&d.join("Game.exe"), &d.join("out")).unwrap());
        assert_eq!(fs::read(d.join("out/www/index.html")).unwrap(), b"<html>");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn leaves_other_exes_alone() {
        let d = tmp("plain");
        fs::write(d.join("a.exe"), b"MZ no zip here".repeat(10)).unwrap();
        assert!(!unpack(&d.join("a.exe"), &d.join("out")).unwrap());
        fs::write(d.join("b.exe"), exe_with_zip(&[("readme.txt", b"no package.json")])).unwrap();
        assert!(!unpack(&d.join("b.exe"), &d.join("out")).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn finds_the_zip_start() {
        // an end of central directory record: size 0x30 at 12, offset 0x100 at 16
        let mut tail = vec![0u8; 10];
        tail.extend_from_slice(b"PK\x05\x06");
        tail.extend_from_slice(&[0; 8]);
        tail.extend_from_slice(&0x30u32.to_le_bytes());
        tail.extend_from_slice(&0x100u32.to_le_bytes());
        tail.extend_from_slice(&[0; 2]);
        assert_eq!(zip_start(&tail, 0x1000), Some(0x1000 + 10 - 0x30 - 0x100));
        assert_eq!(zip_start(&tail, 0x100), None, "offsets before the file");
        assert_eq!(zip_start(&tail[..tail.len() - 4], 0x1000), None, "cut short");
        assert_eq!(zip_start(b"no record", 0), None);
    }

    /// Review b01: a zip whose central directory says it is huge is refused before the zip
    /// crate reads (and holds) it.
    #[test]
    fn refuses_a_huge_directory_before_reading_it() {
        let d = tmp("dir");
        let mut exe = b"MZ".repeat(1000);
        let size = (MAX_DIRECTORY + 1) as u32;
        // (the directory "starts" at the exe's first byte: offset 0, it ends where the record is)
        let mut rec = b"PK\x05\x06".to_vec();
        rec.extend_from_slice(&[0; 4]);
        rec.extend_from_slice(&0xffffu16.to_le_bytes());
        rec.extend_from_slice(&0xffffu16.to_le_bytes());
        rec.extend_from_slice(&size.to_le_bytes());
        rec.extend_from_slice(&0u32.to_le_bytes());
        rec.extend_from_slice(&[0; 2]);
        exe.resize(size as usize, 0);
        exe.extend_from_slice(&rec);
        fs::write(d.join("Game.exe"), &exe).unwrap();
        let e = unpack(&d.join("Game.exe"), &d.join("out")).unwrap_err();
        assert!(e.to_string().contains("directory takes"), "{e}");
        let _ = fs::remove_dir_all(&d);
    }

    proptest::proptest! {
        /// Any tail: no panic, and a start found lies before the record, the directory with it.
        #[test]
        fn zip_directory_on_any_tail(tail in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64), at in 0u64..1 << 40, eocd in 0usize..40, size in proptest::num::u32::ANY, offset in proptest::num::u32::ANY) {
            let mut t = tail.clone();
            if eocd + 22 <= t.len() {
                t[eocd..eocd + 4].copy_from_slice(b"PK\x05\x06");
                t[eocd + 12..eocd + 16].copy_from_slice(&size.to_le_bytes());
                t[eocd + 16..eocd + 20].copy_from_slice(&offset.to_le_bytes());
            }
            if let Some((start, dir)) = zip_directory(&t, at) {
                let rec = at + t.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap() as u64;
                proptest::prop_assert!(start + dir <= rec);
            }
        }
    }

    #[test]
    fn stops_a_zip_bomb() {
        let d = tmp("bomb");
        // 300 MiB of zeros compress to well under a MiB: past MIN_OUT and RATIO
        let zeros = vec![0u8; 300 << 20];
        fs::write(d.join("Game.exe"), exe_with_zip(&[("package.json", b"{}"), ("big.bin", &zeros)])).unwrap();
        let e = unpack(&d.join("Game.exe"), &d.join("out")).unwrap_err();
        assert!(e.to_string().contains("unpacks to more than"), "{e}");
        let _ = fs::remove_dir_all(&d);
    }
}
