//! The credential manager (advapi32 Cred*): a store of the logon session (nothing is kept
//! when the process ends) and the "@@" marshaled credential names.
//!
//! - CredWrite/Read/Delete/Enumerate/ReadDomainCredentials (A and W) over `STORE`, keyed by
//!   type and target name (case-insensitive). Domain password credentials keep their blob
//!   but never give it back (as Windows: the password is only for the LSA).
//! - CredMarshalCredential / CredUnmarshalCredential / CredIsMarshaledCredential: "@@", the
//!   type as one character, then the data 6 bits a character, least significant bits first,
//!   in the alphabet A-Z a-z 0-9 # -. `marshal`/`unmarshal` are pure and tested below.

#![allow(non_snake_case)]

use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, cstr, r32, rp, w32, wstr};
use crate::text::{a2w, w2a};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INVALID_FLAGS: u32 = 1004;
const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_BAD_USERNAME: u32 = 2202;

/// CRED_TYPE_GENERIC .. CRED_TYPE_DOMAIN_EXTENDED
const TYPE_MAX: u32 = 7;
const DOMAIN_PASSWORD: u32 = 2;
/// CRED_PERSIST_SESSION .. CRED_PERSIST_ENTERPRISE
const PERSIST_MAX: u32 = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Cred {
    pub flags: u32,
    pub ty: u32,
    pub target: Vec<u16>,
    pub comment: Option<Vec<u16>>,
    pub written: u64,
    pub blob: Vec<u8>,
    pub persist: u32,
    /// (keyword, flags, value)
    pub attrs: Vec<(Vec<u16>, u32, Vec<u8>)>,
    pub alias: Option<Vec<u16>>,
    pub user: Option<Vec<u16>>,
}

static STORE: Mutex<Vec<Cred>> = Mutex::new(Vec::new());

fn lower(s: &[u16]) -> Vec<u16> {
    s.iter().map(|&c| crate::text::lower16(c)).collect()
}
fn domain_type(t: u32) -> bool {
    (2..=6).contains(&t) && t != 5
}

/// Why CredWrite refuses a credential, if it does
pub fn check(c: &Cred) -> Result<(), u32> {
    if c.ty == 0 || c.ty >= TYPE_MAX || c.target.is_empty() || c.persist == 0 || c.persist > PERSIST_MAX {
        return Err(ERROR_INVALID_PARAMETER);
    }
    if domain_type(c.ty) && c.user.as_ref().is_none_or(|u| u.is_empty()) {
        return Err(ERROR_BAD_USERNAME);
    }
    Ok(())
}

pub fn write(c: Cred) -> Result<(), u32> {
    check(&c)?;
    let mut s = STORE.lock().unwrap();
    let key = lower(&c.target);
    s.retain(|o| !(o.ty == c.ty && lower(&o.target) == key));
    s.push(c);
    Ok(())
}
pub fn read(target: &[u16], ty: u32) -> Option<Cred> {
    let key = lower(target);
    STORE.lock().unwrap().iter().find(|o| o.ty == ty && lower(&o.target) == key).cloned()
}
pub fn delete(target: &[u16], ty: u32) -> bool {
    let key = lower(target);
    let mut s = STORE.lock().unwrap();
    let n = s.len();
    s.retain(|o| !(o.ty == ty && lower(&o.target) == key));
    s.len() != n
}
/// The credentials a CredEnumerate filter selects: all, a name, or a prefix ending in '*'
pub fn select(filter: Option<&[u16]>) -> Vec<Cred> {
    let s = STORE.lock().unwrap();
    let Some(f) = filter else { return s.clone() };
    let f = lower(f);
    s.iter()
        .filter(|c| {
            let t = lower(&c.target);
            match f.split_last() {
                Some((&last, pre)) if last == '*' as u16 => t.starts_with(pre),
                _ => t == f,
            }
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Guest CREDENTIAL structures

fn ps() -> u32 {
    mem::ps()
}
/// CREDENTIAL's size and the offsets of (Comment, LastWritten, BlobSize, Blob, Persist,
/// AttributeCount, Attributes, TargetAlias, UserName); TargetName is at 8
fn layout() -> (u32, [u32; 9]) {
    if ps() == 8 { (80, [16, 24, 32, 40, 48, 52, 56, 64, 72]) } else { (52, [12, 16, 24, 28, 32, 36, 40, 44, 48]) }
}

fn text_in(p: u32, wide: bool) -> Option<Vec<u16>> {
    if p == 0 {
        None
    } else if wide {
        Some(wstr(p))
    } else {
        Some(a2w(&cstr(p)))
    }
}
fn text_bytes(s: &[u16], wide: bool) -> Vec<u8> {
    if wide { s.iter().chain(&[0]).flat_map(|c| c.to_le_bytes()).collect() } else { w2a(s).into_iter().chain([0]).collect() }
}

fn cred_in(p: u32, wide: bool) -> Cred {
    let (_, o) = layout();
    let ac = r32(p + o[5]);
    let ap = rp(p + o[6]);
    let ae = if ps() == 8 { 32 } else { 16 };
    let attrs = (0..ac)
        .map(|i| {
            let a = ap + i * ae;
            let (kw, fl, n, v) = (rp(a), r32(a + ps()), r32(a + ps() + 4), rp(a + ps() + 8));
            (text_in(kw, wide).unwrap_or_default(), fl, if v != 0 { mem::slice(v, n).to_vec() } else { Vec::new() })
        })
        .collect();
    let n = r32(p + o[2]);
    let b = rp(p + o[3]);
    Cred {
        flags: r32(p),
        ty: r32(p + 4),
        target: text_in(rp(p + 8), wide).unwrap_or_default(),
        comment: text_in(rp(p + o[0]), wide),
        written: 0,
        blob: if b != 0 { mem::slice(b, n).to_vec() } else { Vec::new() },
        persist: r32(p + o[4]),
        attrs,
        alias: text_in(rp(p + o[7]), wide),
        user: text_in(rp(p + o[8]), wide),
    }
}

/// Lays out credentials in one block: the array of pointers to them (when `array`), the
/// CREDENTIALs, then their strings and data. Returns the block (and the first CREDENTIAL).
fn creds_out(cs: &[Cred], wide: bool, array: bool) -> u32 {
    let (size, o) = layout();
    let p = ps() as usize;
    let head = if array { p * cs.len() } else { 0 };
    let mut b = vec![0u8; head + size as usize * cs.len()];
    let mut ptrs: Vec<(usize, usize)> = Vec::new();
    let add = |b: &mut Vec<u8>, data: &[u8]| -> usize {
        while b.len() % 8 != 0 {
            b.push(0);
        }
        let at = b.len();
        b.extend_from_slice(data);
        at
    };
    for (i, c) in cs.iter().enumerate() {
        let at = head + i * size as usize;
        if array {
            ptrs.push((i * p, at));
        }
        let put32 = |b: &mut Vec<u8>, off: u32, v: u32| b[at + off as usize..at + off as usize + 4].copy_from_slice(&v.to_le_bytes());
        put32(&mut b, 0, c.flags);
        put32(&mut b, 4, c.ty);
        b[at + o[1] as usize..at + o[1] as usize + 8].copy_from_slice(&c.written.to_le_bytes());
        put32(&mut b, o[4], c.persist);
        put32(&mut b, o[5], c.attrs.len() as u32);
        let mut s = |b: &mut Vec<u8>, off: u32, v: &Option<Vec<u16>>| {
            if let Some(v) = v {
                let x = add(b, &text_bytes(v, wide));
                ptrs.push((at + off as usize, x));
            }
        };
        s(&mut b, 8, &Some(c.target.clone()));
        s(&mut b, o[0], &c.comment);
        s(&mut b, o[7], &c.alias);
        s(&mut b, o[8], &c.user);
        // (a domain password stays with the LSA: no blob comes back)
        if c.ty != DOMAIN_PASSWORD && !c.blob.is_empty() {
            put32(&mut b, o[2], c.blob.len() as u32);
            let x = add(&mut b, &c.blob);
            ptrs.push((at + o[3] as usize, x));
        }
        if !c.attrs.is_empty() {
            let ae = if p == 8 { 32 } else { 16 };
            let x = add(&mut b, &vec![0; ae * c.attrs.len()]);
            ptrs.push((at + o[6] as usize, x));
            for (k, (kw, fl, v)) in c.attrs.iter().enumerate() {
                let e = x + k * ae;
                b[e + p..e + p + 4].copy_from_slice(&fl.to_le_bytes());
                b[e + p + 4..e + p + 8].copy_from_slice(&(v.len() as u32).to_le_bytes());
                let kx = add(&mut b, &text_bytes(kw, wide));
                ptrs.push((e, kx));
                if !v.is_empty() {
                    let vx = add(&mut b, v);
                    ptrs.push((e + p + 8, vx));
                }
            }
        }
    }
    let blk = mem::heap_alloc(b.len().max(1) as u32, true);
    mem::write(blk, &b);
    for (at, x) in ptrs {
        mem::wp(blk + at as u32, blk + x as u32);
    }
    blk
}

fn now() -> u64 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    ms * 10_000 + 116_444_736_000_000_000
}

fn fail(e: u32) -> u32 {
    last_error(e);
    0
}

/// CredWrite(credential, flags)
fn cred_write(c: &mut Ctx, wide: bool) -> u32 {
    let (p, flags) = (c.a(0), c.a(1));
    if p == 0 || rp(p + 8) == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    // CRED_PRESERVE_CREDENTIAL_BLOB
    if flags & !1 != 0 {
        return fail(ERROR_INVALID_FLAGS);
    }
    let mut cr = cred_in(p, wide);
    cr.written = now();
    if flags & 1 != 0 {
        if let Some(old) = read(&cr.target, cr.ty) {
            cr.blob = old.blob;
        }
    }
    match write(cr) {
        Ok(()) => {
            last_error(0);
            1
        }
        Err(e) => fail(e),
    }
}
fn CredWriteA(c: &mut Ctx) -> u32 {
    cred_write(c, false)
}
fn CredWriteW(c: &mut Ctx) -> u32 {
    cred_write(c, true)
}

/// CredRead(target, type, flags, &credential)
fn cred_read(c: &mut Ctx, wide: bool) -> u32 {
    let (t, ty, flags, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if ty == 0 || ty >= TYPE_MAX {
        return fail(ERROR_INVALID_PARAMETER);
    }
    if flags != 0 {
        return fail(ERROR_INVALID_FLAGS);
    }
    if t == 0 || out == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    match read(&text_in(t, wide).unwrap(), ty) {
        Some(cr) => {
            mem::putp(out, creds_out(&[cr], wide, false));
            1
        }
        None => fail(ERROR_NOT_FOUND),
    }
}
fn CredReadA(c: &mut Ctx) -> u32 {
    cred_read(c, false)
}
fn CredReadW(c: &mut Ctx) -> u32 {
    cred_read(c, true)
}

/// CredDelete(target, type, flags)
fn cred_delete(c: &mut Ctx, wide: bool) -> u32 {
    let (t, ty, flags) = (c.a(0), c.a(1), c.a(2));
    if ty == 0 || ty >= TYPE_MAX {
        return fail(ERROR_INVALID_PARAMETER);
    }
    if flags != 0 {
        return fail(ERROR_INVALID_FLAGS);
    }
    if t == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    if delete(&text_in(t, wide).unwrap(), ty) { 1 } else { fail(ERROR_NOT_FOUND) }
}
fn CredDeleteA(c: &mut Ctx) -> u32 {
    cred_delete(c, false)
}
fn CredDeleteW(c: &mut Ctx) -> u32 {
    cred_delete(c, true)
}

/// CredEnumerate(filter, flags, &count, &credentials): an array of pointers
fn cred_enumerate(c: &mut Ctx, wide: bool) -> u32 {
    let (f, flags, countp, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    // CRED_ENUMERATE_ALL_CREDENTIALS (1) goes with no filter only
    if flags & !1 != 0 || flags & 1 != 0 && f != 0 {
        return fail(ERROR_INVALID_FLAGS);
    }
    if countp == 0 || out == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let cs = select(text_in(f, wide).as_deref());
    if cs.is_empty() {
        w32(countp, 0);
        mem::putp(out, 0);
        return fail(ERROR_NOT_FOUND);
    }
    w32(countp, cs.len() as u32);
    mem::putp(out, creds_out(&cs, wide, true));
    1
}
fn CredEnumerateA(c: &mut Ctx) -> u32 {
    cred_enumerate(c, false)
}
fn CredEnumerateW(c: &mut Ctx) -> u32 {
    cred_enumerate(c, true)
}

/// CredReadDomainCredentials(target info, flags, &count, &credentials): the domain
/// credentials whose target is one of the info's names
fn cred_read_domain(c: &mut Ctx, wide: bool) -> u32 {
    let (info, flags, countp, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if info == 0 || countp == 0 || out == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    w32(countp, 0);
    mem::putp(out, 0);
    if flags & !1 != 0 {
        return fail(ERROR_INVALID_FLAGS);
    }
    // TargetName, NetbiosServerName, DnsServerName, NetbiosDomainName, DnsDomainName
    let names: Vec<Vec<u16>> = (0..5).filter_map(|i| text_in(rp(info + i * ps()), wide)).map(|n| lower(&n)).collect();
    let cs: Vec<Cred> = STORE.lock().unwrap().iter().filter(|c| domain_type(c.ty) && names.contains(&lower(&c.target))).cloned().collect();
    if cs.is_empty() {
        return fail(ERROR_NOT_FOUND);
    }
    w32(countp, cs.len() as u32);
    mem::putp(out, creds_out(&cs, wide, true));
    1
}
fn CredReadDomainCredentialsA(c: &mut Ctx) -> u32 {
    cred_read_domain(c, false)
}
fn CredReadDomainCredentialsW(c: &mut Ctx) -> u32 {
    cred_read_domain(c, true)
}

fn CredFree(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}

/// CredGetSessionTypes(count, persists): every type persists up to the enterprise
fn CredGetSessionTypes(c: &mut Ctx) -> u32 {
    let (n, out) = (c.a(0), c.a(1));
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    for i in 0..n {
        w32(out + 4 * i, if i == 0 || i >= TYPE_MAX { 0 } else { PERSIST_MAX });
    }
    1
}

// ---------------------------------------------------------------------------------------------
// Marshaled credentials

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789#-";
pub const CERT: u32 = 1;
pub const USERNAME: u32 = 2;
pub const BLOB: u32 = 3;

fn enc(data: &[u8]) -> String {
    let mut s = String::new();
    let (mut acc, mut bits) = (0u32, 0);
    for &b in data {
        acc |= (b as u32) << bits;
        bits += 8;
        while bits >= 6 {
            s.push(ALPHABET[(acc & 63) as usize] as char);
            acc >>= 6;
            bits -= 6;
        }
    }
    if bits > 0 {
        s.push(ALPHABET[(acc & 63) as usize] as char);
    }
    s
}
fn val(c: u8) -> Option<u32> {
    ALPHABET.iter().position(|&a| a == c).map(|v| v as u32)
}
/// `n` bytes from exactly the characters that hold them
fn dec(s: &[u8], n: usize) -> Option<Vec<u8>> {
    if s.len() != (8 * n).div_ceil(6) {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s {
        acc |= val(c)? << bits;
        bits += 6;
        if bits >= 8 && out.len() < n {
            out.push(acc as u8);
            acc >>= 8;
            bits -= 8;
        }
    }
    Some(out)
}

/// What a credential marshals: a certificate's SHA-1 hash, a user name, or a blob
#[derive(Clone, Debug, PartialEq)]
pub enum Marshaled {
    Cert([u8; 20]),
    User(Vec<u16>),
    Blob(Vec<u8>),
}

pub fn marshal(m: &Marshaled) -> String {
    let sized = |ty: u32, data: &[u8]| format!("@@{}{}{}", ALPHABET[ty as usize] as char, enc(&(data.len() as u32).to_le_bytes()), enc(data));
    match m {
        Marshaled::Cert(h) => format!("@@B{}", enc(h)),
        Marshaled::User(u) => sized(USERNAME, &u.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<_>>()),
        Marshaled::Blob(b) => sized(BLOB, b),
    }
}

/// The type (its character's value, 0 when there is none) and what it holds, if valid
pub fn unmarshal(s: &[u8]) -> (u32, Option<Marshaled>) {
    if s.len() < 3 || &s[..2] != b"@@" {
        return (0, None);
    }
    let ty = val(s[2]).unwrap_or(0);
    let rest = &s[3..];
    let m = match ty {
        CERT => dec(rest, 20).map(|h| Marshaled::Cert(h.try_into().unwrap())),
        USERNAME | BLOB => (|| {
            if rest.len() < 6 {
                return None;
            }
            let n = u32::from_le_bytes(dec(&rest[..6], 4)?.try_into().ok()?) as usize;
            if n == 0 || n > rest.len() * 6 / 8 || ty == USERNAME && n % 2 != 0 {
                return None;
            }
            let d = dec(&rest[6..], n)?;
            Some(if ty == USERNAME { Marshaled::User(d.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()) } else { Marshaled::Blob(d) })
        })(),
        _ => None,
    };
    (ty, m)
}

/// CredMarshalCredential(type, credential, &out)
fn marshal_in(ty: u32, p: u32) -> Option<Marshaled> {
    if p == 0 {
        return None;
    }
    match ty {
        // CERT_CREDENTIAL_INFO {cbSize; rgbHashOfCert[20]}
        CERT if r32(p) >= 24 => Some(Marshaled::Cert(mem::slice(p + 4, 20).try_into().unwrap())),
        // USERNAME_TARGET_CREDENTIAL_INFO {UserName}
        USERNAME => {
            let u = rp(p);
            (u != 0 && mem::r16(u) != 0).then(|| Marshaled::User(wstr(u)))
        }
        // BINARY_BLOB_CREDENTIAL_INFO {cbBlob; pbBlob}
        BLOB => {
            let (n, b) = (r32(p), rp(p + ps()));
            (n != 0 && b != 0).then(|| Marshaled::Blob(mem::slice(b, n).to_vec()))
        }
        _ => None,
    }
}
fn cred_marshal(c: &mut Ctx, wide: bool) -> u32 {
    let (ty, p, out) = (c.a(0), c.a(1), c.a(2));
    let Some(m) = marshal_in(ty, p).filter(|_| out != 0) else {
        return fail(ERROR_INVALID_PARAMETER);
    };
    let s = marshal(&m);
    let w: Vec<u16> = s.encode_utf16().collect();
    let b = text_bytes(&w, wide);
    let blk = mem::heap_alloc(b.len() as u32, false);
    mem::write(blk, &b);
    mem::putp(out, blk);
    1
}
fn CredMarshalCredentialA(c: &mut Ctx) -> u32 {
    cred_marshal(c, false)
}
fn CredMarshalCredentialW(c: &mut Ctx) -> u32 {
    cred_marshal(c, true)
}

/// CredUnmarshalCredential(text, &type, &credential)
fn cred_unmarshal(c: &mut Ctx, wide: bool) -> u32 {
    let (t, typ, out) = (c.a(0), c.a(1), c.a(2));
    if t == 0 || typ == 0 || out == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let s = w2a(&text_in(t, wide).unwrap());
    let (ty, m) = unmarshal(&s);
    w32(typ, ty);
    let Some(m) = m else {
        mem::putp(out, 0);
        return fail(ERROR_INVALID_PARAMETER);
    };
    let p = ps();
    let blk = match m {
        Marshaled::Cert(h) => {
            let b = mem::heap_alloc(24, true);
            w32(b, 24);
            mem::write(b + 4, &h);
            b
        }
        Marshaled::User(u) => {
            let b = mem::heap_alloc(p + 2 * u.len() as u32 + 2, true);
            mem::wp(b, b + p);
            mem::write(b + p, &text_bytes(&u, true));
            b
        }
        Marshaled::Blob(d) => {
            let b = mem::heap_alloc(2 * p + d.len() as u32, true);
            w32(b, d.len() as u32);
            mem::wp(b + p, b + 2 * p);
            mem::write(b + 2 * p, &d);
            b
        }
    };
    mem::putp(out, blk);
    1
}
fn CredUnmarshalCredentialA(c: &mut Ctx) -> u32 {
    cred_unmarshal(c, false)
}
fn CredUnmarshalCredentialW(c: &mut Ctx) -> u32 {
    cred_unmarshal(c, true)
}
fn cred_is_marshaled(c: &mut Ctx, wide: bool) -> u32 {
    let t = c.a(0);
    let ok = t != 0 && unmarshal(&w2a(&text_in(t, wide).unwrap())).1.is_some();
    if !ok {
        last_error(ERROR_INVALID_PARAMETER);
    }
    ok as u32
}
fn CredIsMarshaledCredentialA(c: &mut Ctx) -> u32 {
    cred_is_marshaled(c, false)
}
fn CredIsMarshaledCredentialW(c: &mut Ctx) -> u32 {
    cred_is_marshaled(c, true)
}

pub fn init() {
    reg!("advapi32";
        CredWriteA, CredWriteW, CredReadA, CredReadW, CredDeleteA, CredDeleteW, CredEnumerateA, CredEnumerateW,
        CredReadDomainCredentialsA, CredReadDomainCredentialsW, CredFree, CredGetSessionTypes,
        CredMarshalCredentialA, CredMarshalCredentialW, CredUnmarshalCredentialA, CredUnmarshalCredentialW,
        CredIsMarshaledCredentialA, CredIsMarshaledCredentialW,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn marshaled_as_windows() {
        let mut h = [0u8; 20];
        assert_eq!(marshal(&Marshaled::Cert(h)), "@@BAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        h[0] = 255;
        assert_eq!(marshal(&Marshaled::Cert(h)), "@@B-DAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(marshal(&Marshaled::Cert([0xff; 20])), "@@B--------------------------P");
        assert_eq!(marshal(&Marshaled::User(w("t"))), "@@CCAAAAA0BA");
        assert_eq!(marshal(&Marshaled::User(w("test1"))), "@@CKAAAAA0BQZAMHA0BQMAA");
        for (s, ty, ok) in [
            ("", 0, false),
            ("@@@", 0, false),
            ("@@E", 4, false),
            ("@@-", 63, false),
            ("@@BAAAAAAAAAAAAAAAAAAAAAAAAAA", 1, false),
            ("@@BAAAAAAAAAAAAAAAAAAAAAAAAAAAA", 1, false),
            ("@@BXlmblBAAAAAAAAAAAAAAAAAAAAA", 1, true),
            ("@@CAAAAAA0BA", 2, false),
            ("@@CEAAAAA0BAd", 2, false),
            ("@@CEAAAAA0BQZAw", 2, true),
            ("@@CEAAAAA0BQZAAA", 2, false),
            ("@@CCAAAAA0BAA", 2, false),
            ("@@CBAAAAA0BAA", 2, false),
            ("@@CAgAAAA0BAA", 2, false),
            ("@@CA-----0BQZAMHA0BA", 2, false),
            ("@@BAAAAAAAAAAAAAAAAAAAAAAAAA+", 1, false),
        ] {
            let (t, m) = unmarshal(s.as_bytes());
            assert_eq!((t, m.is_some()), (ty, ok), "{s}");
        }
        assert_eq!(unmarshal(b"@@CEAAAAA0BQZAg").1, Some(Marshaled::User(w("te"))));
    }

    #[test]
    fn store_rules() {
        let c = Cred { flags: 0, ty: 1, target: w("Credtest.example"), comment: None, written: 0, blob: vec![1, 2], persist: 1, attrs: vec![], alias: None, user: None };
        assert_eq!(check(&Cred { ty: 2, ..c.clone() }), Err(ERROR_BAD_USERNAME));
        assert_eq!(check(&Cred { persist: 4, ..c.clone() }), Err(ERROR_INVALID_PARAMETER));
        assert_eq!(check(&Cred { target: vec![], ..c.clone() }), Err(ERROR_INVALID_PARAMETER));
        write(c.clone()).unwrap();
        assert_eq!(read(&w("CREDTEST.example"), 1).map(|c| c.blob), Some(vec![1, 2]));
        assert!(read(&w("credtest.example"), 2).is_none());
        assert_eq!(select(Some(&w("credtest*"))).len(), 1);
        assert_eq!(select(Some(&w("credtest"))).len(), 0);
        assert!(delete(&w("credtest.example"), 1) && !delete(&w("credtest.example"), 1));
    }

    proptest! {
        /// Every marshaled credential unmarshals to itself, and is the only text that does
        #[test]
        fn marshal_round_trips(h in any::<[u8; 20]>(), u in proptest::collection::vec(1u16.., 1..40), b in proptest::collection::vec(any::<u8>(), 1..80)) {
            for m in [Marshaled::Cert(h), Marshaled::User(u.clone()), Marshaled::Blob(b.clone())] {
                let s = marshal(&m);
                let (ty, back) = unmarshal(s.as_bytes());
                prop_assert_eq!(back, Some(m.clone()));
                prop_assert!(ty >= 1 && ty <= 3);
                // one more character or one fewer is not a credential
                let mut longer = s.clone().into_bytes();
                longer.push(b'A');
                prop_assert!(unmarshal(&longer).1.is_none());
                prop_assert!(unmarshal(&s.as_bytes()[..s.len() - 1]).1.is_none());
            }
        }
        /// Any text: no panic
        #[test]
        fn unmarshal_any(s in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut t = b"@@".to_vec();
            t.extend(s);
            let _ = unmarshal(&t);
        }
    }
}
