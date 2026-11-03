//! NW.js games ship their app as a zip appended to nw.exe ("copy /b nw.exe+app.nw") or as
//! package.nw. The page runs such games as web pages (web/src/html.ts), which needs the files
//! themselves: `ippai pack` adds the zip's files next to the exe.

use anyhow::Result;
use std::fs::{self, File};
use std::io::{self, Cursor};
use std::path::Path;
use zip::ZipArchive;

/// Where the zip at the end of `bytes` starts: its central directory offsets count from there,
/// so they are off by the size of the exe in front of it.
fn zip_start(bytes: &[u8]) -> Option<usize> {
    let from = bytes.len().saturating_sub(22 + 0xffff);
    let eocd = from + bytes[from..].windows(4).rposition(|w| w == b"PK\x05\x06")?;
    let field = |at: usize| {
        let b = bytes.get(eocd + at..eocd + at + 4)?;
        Some(u32::from_le_bytes(b.try_into().ok()?) as usize)
    };
    let (size, offset) = (field(12)?, field(16)?);
    eocd.checked_sub(size)?.checked_sub(offset)
}

/// Unpacks the app zip in `file` into `dir` when it holds a package.json; returns whether it did.
pub fn unpack(file: &Path, dir: &Path) -> Result<bool> {
    let bytes = fs::read(file)?;
    let Some(start) = zip_start(&bytes) else { return Ok(false) };
    let Ok(mut zip) = ZipArchive::new(Cursor::new(&bytes[start..])) else { return Ok(false) };
    if zip.index_for_name("package.json").is_none() {
        return Ok(false);
    }
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
        io::copy(&mut e, &mut File::create(&out)?)?;
    }
    Ok(true)
}
