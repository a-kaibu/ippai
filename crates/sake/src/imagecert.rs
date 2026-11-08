//! ImageHlp certificate-table access. These functions read actual WIN_CERTIFICATE
//! records; they do not assert that a certificate or file signature is trusted.
#![allow(non_snake_case)]
use std::io::{Read, Seek, SeekFrom};
use crate::api::{implement, Ctx};
use crate::mem::{self, r32, w32};

fn certificates(data: &[u8]) -> Option<Vec<&[u8]>> {
    let word = |o: usize| Some(u16::from_le_bytes(data.get(o..o.checked_add(2)?)?.try_into().ok()?));
    let dword = |o: usize| Some(u32::from_le_bytes(data.get(o..o.checked_add(4)?)?.try_into().ok()?));
    if data.get(..2)? != b"MZ" { return None; }
    let pe = dword(0x3c)? as usize;
    if data.get(pe..pe.checked_add(4)?)? != b"PE\0\0" { return None; }
    let opt = pe.checked_add(24)?;
    let dirs = match word(opt)? { 0x10b => 96, 0x20b => 112, _ => return None };
    if (word(pe + 20)? as usize) < dirs + 40 || dword(opt + dirs - 4)? < 5 { return Some(vec![]); }
    let (at, size) = (dword(opt + dirs + 32)? as usize, dword(opt + dirs + 36)? as usize);
    if at == 0 && size == 0 { return Some(vec![]); }
    let table = data.get(at..at.checked_add(size)?)?;
    let mut result = Vec::new(); let mut at = 0usize;
    while at < table.len() {
        let n = u32::from_le_bytes(table.get(at..at.checked_add(4)?)?.try_into().ok()?) as usize;
        if n < 8 { return None; }
        result.push(table.get(at..at.checked_add(n)?)?);
        at = at.checked_add(n)?.checked_add(7)? & !7;
        if at > table.len() { return None; }
    }
    Some(result)
}
fn read_file(h: u32) -> Result<Vec<u8>, u32> {
    let file = crate::obj::file(h).ok_or(6u32)?;
    let mut f = file.f.lock().unwrap();
    let position = f.stream_position().map_err(|_| 87u32)?;
    let result = (|| { f.seek(SeekFrom::Start(0))?; let mut b = vec![]; f.read_to_end(&mut b)?; Ok::<_,std::io::Error>(b) })();
    f.seek(SeekFrom::Start(position)).map_err(|_| 87u32)?;
    result.map_err(|_| 5)
}
fn fail(error: u32) -> u32 { crate::k32::last_error(error); 0 }
fn ImageGetCertificateHeader(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 { return fail(87); }
    let data = match read_file(c.a(0)) { Ok(b) => b, Err(e) => return fail(e) };
    let Some(entries) = certificates(&data) else { return fail(193) };
    let Some(certificate) = entries.get(c.a(1) as usize) else { return fail(87) };
    mem::write(c.a(2), &certificate[..8]); 1
}
fn ImageGetCertificateData(c: &mut Ctx) -> u32 {
    if c.a(3) == 0 { return fail(87); }
    let data = match read_file(c.a(0)) { Ok(b) => b, Err(e) => return fail(e) };
    let Some(entries) = certificates(&data) else { return fail(193) };
    let Some(certificate) = entries.get(c.a(1) as usize) else { return fail(87) };
    let capacity = r32(c.a(3)); w32(c.a(3), certificate.len() as u32);
    if c.a(2) == 0 || capacity < certificate.len() as u32 { return fail(122); }
    mem::write(c.a(2), certificate); 1
}
fn ImageEnumerateCertificates(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 { return fail(87); }
    let data = match read_file(c.a(0)) { Ok(b) => b, Err(e) => return fail(e) };
    let Some(entries) = certificates(&data) else { return fail(193) };
    let filter = c.a(1) as u16;
    let indexes: Vec<u32> = entries.iter().enumerate().filter(|(_, b)| filter == 0xff || u16::from_le_bytes([b[6],b[7]]) == filter)
        .map(|(i, _)| i as u32).collect();
    w32(c.a(2), indexes.len() as u32);
    if c.a(3) != 0 {
        if c.a(4) < indexes.len() as u32 { return fail(122); }
        for (i, value) in indexes.into_iter().enumerate() { w32(c.a(3) + i as u32 * 4, value); }
    }
    1
}

/// ImageHlp's stream is not the Authenticode flat-file hashing algorithm:
/// DOS/NT/section headers precede code sections, followed by other sections.
/// Normalize only checksum and the security-directory entry for level 6.
/// Other levels need additional import/resource/debug normalization; reject
/// them until independently verified instead of supplying an incorrect hash.
fn digest_parts(data: &[u8], level: u32) -> Result<Vec<Vec<u8>>, u32> {
    if level != 6 { return Err(50); }
    let invalid = 193u32;
    let word = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(data.get(o..o.checked_add(2)?)?.try_into().ok()?)) };
    let dword = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(data.get(o..o.checked_add(4)?)?.try_into().ok()?)) };
    if data.get(..2) != Some(b"MZ") { return Err(invalid); }
    let pe = dword(0x3c).ok_or(invalid)? as usize;
    if pe < 64 || data.get(pe..pe.checked_add(4).ok_or(invalid)?) != Some(b"PE\0\0") { return Err(invalid); }
    let opt = pe.checked_add(24).ok_or(invalid)?;
    let optional_size = word(pe + 20).ok_or(invalid)? as usize;
    let dirs = match word(opt).ok_or(invalid)? { 0x10b => 96, 0x20b => 112, _ => return Err(invalid) };
    if optional_size < dirs { return Err(invalid); }
    let sections = opt.checked_add(optional_size).ok_or(invalid)?;
    let count = word(pe + 6).ok_or(invalid)? as usize;
    let end = sections.checked_add(count.checked_mul(40).ok_or(invalid)?).ok_or(invalid)?;
    let headers = data.get(..end).ok_or(invalid)?;
    let mut nt = headers[pe..sections].to_vec();
    nt[24 + 64..24 + 68].fill(0);
    if dword(opt + dirs - 4).ok_or(invalid)? > 4 {
        if optional_size < dirs + 40 { return Err(invalid); }
        nt[24 + dirs + 32..24 + dirs + 40].fill(0);
    }
    let mut result = vec![headers[..pe].to_vec(), nt, headers[sections..].to_vec()];
    let mut code = Vec::new(); let mut remaining = Vec::new();
    for i in 0..count {
        let h = sections + i * 40;
        let size = dword(h + 16).ok_or(invalid)? as usize;
        if size == 0 { continue; }
        // Debug bytes need filtering even when embedded in a non-.debug
        // section. Fail closed until that normalization is implemented.
        if data.get(h..h + 8).is_some_and(|name| name == b".debug\0\0") { return Err(50); }
        let at = dword(h + 20).ok_or(invalid)? as usize;
        let bytes = data.get(at..at.checked_add(size).ok_or(invalid)?).ok_or(invalid)?.to_vec();
        if dword(h + 36).ok_or(invalid)? & 0x20 != 0 { code.push(bytes); } else { remaining.push(bytes); }
    }
    result.extend(code); result.extend(remaining); Ok(result)
}
fn ImageGetDigestStream(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 { return fail(87); }
    let data = match read_file(c.a(0)) { Ok(b) => b, Err(e) => return fail(e) };
    let parts = match digest_parts(&data, c.a(1)) { Ok(p) => p, Err(e) => return fail(e) };
    let (callback, handle) = (c.a(2), c.a(3));
    // No file/registry/kernel locks may survive guest callbacks.
    for part in parts {
        if part.is_empty() { continue; }
        let buffer = mem::heap_alloc(part.len() as u32, false);
        if buffer == 0 { return fail(8); }
        mem::write(buffer, &part);
        let ok = c.call(callback, &[handle, buffer, part.len() as u32]);
        mem::heap_free(buffer);
        if ok == 0 { return 0; }
    }
    1
}
pub fn init() {
    implement("imagehlp", "ImageGetCertificateHeader", ImageGetCertificateHeader);
    implement("imagehlp", "ImageGetCertificateData", ImageGetCertificateData);
    implement("imagehlp", "ImageEnumerateCertificates", ImageEnumerateCertificates);
    implement("imagehlp", "ImageGetDigestStream", ImageGetDigestStream);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_headers_are_rejected() {
        for n in 0..256 { assert!(certificates(&vec![0; n]).is_none()); }
    }
}
