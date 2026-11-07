//! combase's Windows Runtime basics (api-ms-win-core-winrt-*): HSTRINGs and activation.
//! Unity 2021+ initializes the Windows Runtime at startup and asks for a few runtime classes
//! (gamepads, UI settings); none is registered here, which Unity takes as an older Windows.
//!
//! An HSTRING (NULL is the empty string) points at: flags (1: a reference string living in
//! the caller's HSTRING_HEADER), length in UTF-16 units, the characters' address, and for the
//! strings made here a reference count, then the characters and a terminating 0.
#![allow(non_snake_case)]
use crate::api::{implement, Ctx};
use crate::mem::{self, r32, w32};

const S_OK: u32 = 0;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_POINTER: u32 = 0x8000_4003;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;
const REF: u32 = 1;

/// A new HSTRING holding `s` (empty: NULL)
fn create(s: &[u16]) -> Option<u32> {
    if s.is_empty() {
        return Some(0);
    }
    let n = u32::try_from(s.len()).ok().filter(|&n| n < 0x3fff_fff0)?;
    let h = mem::heap_alloc(16 + 2 * (n + 1), false);
    if h == 0 {
        return None;
    }
    w32(h, 0);
    w32(h + 4, n);
    w32(h + 8, h + 16);
    w32(h + 12, 1);
    let b: Vec<u8> = s.iter().chain(&[0]).flat_map(|c| c.to_le_bytes()).collect();
    mem::write(h + 16, &b);
    Some(h)
}

fn len(h: u32) -> u32 {
    if h == 0 { 0 } else { r32(h + 4) }
}

fn chars(h: u32) -> Vec<u16> {
    if h == 0 { Vec::new() } else { mem::wstrn(r32(h + 8), r32(h + 4)) }
}

/// WindowsCreateString(src, length, &string)
fn WindowsCreateString(c: &mut Ctx) -> u32 {
    let (src, n, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_INVALIDARG;
    }
    if src == 0 && n != 0 {
        return E_POINTER;
    }
    match create(&if n == 0 { Vec::new() } else { mem::wstrn(src, n) }) {
        Some(h) => {
            mem::putp(out, h);
            S_OK
        }
        None => E_OUTOFMEMORY,
    }
}

/// WindowsCreateStringReference(src, length, &header, &string): the string stays the caller's
/// (it has to end in a 0 at `length`)
fn WindowsCreateStringReference(c: &mut Ctx) -> u32 {
    let (src, n, hdr, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 || hdr == 0 {
        return E_INVALIDARG;
    }
    if n == 0 {
        mem::putp(out, 0);
        return S_OK;
    }
    if src == 0 {
        return E_POINTER;
    }
    if mem::r16(src + 2 * n) != 0 {
        return E_INVALIDARG;
    }
    w32(hdr, REF);
    w32(hdr + 4, n);
    w32(hdr + 8, src);
    w32(hdr + 12, 0);
    mem::putp(out, hdr);
    S_OK
}

fn WindowsDeleteString(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h != 0 && r32(h) & REF == 0 && mem::atomic_rmw32(h + 12, |v| v.saturating_sub(1)) == 1 {
        mem::heap_free(h);
    }
    S_OK
}

/// WindowsDuplicateString(string, &copy): one more reference, or a copy of a reference string
fn WindowsDuplicateString(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_INVALIDARG;
    }
    let d = if h == 0 {
        0
    } else if r32(h) & REF != 0 {
        match create(&chars(h)) {
            Some(d) => d,
            None => return E_OUTOFMEMORY,
        }
    } else {
        mem::atomic_rmw32(h + 12, |v| v + 1);
        h
    };
    mem::putp(out, d);
    S_OK
}

/// WindowsGetStringRawBuffer(string, &length): the characters (an empty string's: "")
fn WindowsGetStringRawBuffer(c: &mut Ctx) -> u32 {
    static EMPTY: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let (h, n) = (c.a(0), c.a(1));
    if n != 0 {
        w32(n, len(h));
    }
    if h == 0 { *EMPTY.get_or_init(|| mem::heap_alloc(4, true)) } else { r32(h + 8) }
}

fn WindowsGetStringLen(c: &mut Ctx) -> u32 {
    len(c.a(0))
}

fn WindowsIsStringEmpty(c: &mut Ctx) -> u32 {
    (len(c.a(0)) == 0) as u32
}

/// Ordinal (code unit by code unit) order of two strings: -1, 0 or 1
pub fn ordinal_cmp(a: &[u16], b: &[u16]) -> i32 {
    a.cmp(b) as i32
}

/// WindowsCompareStringOrdinal(a, b, &result)
fn WindowsCompareStringOrdinal(c: &mut Ctx) -> u32 {
    let (a, b, r) = (c.a(0), c.a(1), c.a(2));
    if r == 0 {
        return E_INVALIDARG;
    }
    w32(r, ordinal_cmp(&chars(a), &chars(b)) as u32);
    S_OK
}

fn class_name(h: u32) -> String {
    String::from_utf16_lossy(&chars(h))
}

/// RoGetActivationFactory(class, iid, &factory) / RoActivateInstance(class, &instance): no
/// runtime class is registered
fn RoGetActivationFactory(c: &mut Ctx) -> u32 {
    if crate::env_on!("SAKE_TRACE_COM") {
        eprintln!("sake: RoGetActivationFactory {}: not registered", class_name(c.a(0)));
    }
    if c.a(2) != 0 {
        mem::putp(c.a(2), 0);
    }
    REGDB_E_CLASSNOTREG
}
fn RoActivateInstance(c: &mut Ctx) -> u32 {
    if crate::env_on!("SAKE_TRACE_COM") {
        eprintln!("sake: RoActivateInstance {}: not registered", class_name(c.a(0)));
    }
    if c.a(1) != 0 {
        mem::putp(c.a(1), 0);
    }
    REGDB_E_CLASSNOTREG
}

pub fn init() {
    // (RoInitialize: S_OK; a second call on the thread would be S_FALSE, which callers treat alike)
    implement("combase", "RoInitialize", |_| S_OK);
    implement("combase", "RoUninitialize", |_| 0);
    for (n, f) in [
        ("WindowsCreateString", WindowsCreateString as crate::api::Handler),
        ("WindowsCreateStringReference", WindowsCreateStringReference),
        ("WindowsDeleteString", WindowsDeleteString),
        ("WindowsDuplicateString", WindowsDuplicateString),
        ("WindowsGetStringRawBuffer", WindowsGetStringRawBuffer),
        ("WindowsGetStringLen", WindowsGetStringLen),
        ("WindowsIsStringEmpty", WindowsIsStringEmpty),
        ("WindowsCompareStringOrdinal", WindowsCompareStringOrdinal),
        ("RoGetActivationFactory", RoGetActivationFactory),
        ("RoActivateInstance", RoActivateInstance),
    ] {
        implement("combase", n, f);
    }
}

#[cfg(test)]
mod tests {
    use super::ordinal_cmp;

    #[test]
    fn ordinal_order() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(ordinal_cmp(&w("abc"), &w("abc")), 0);
        assert_eq!(ordinal_cmp(&w("ab"), &w("abc")), -1);
        assert_eq!(ordinal_cmp(&w("b"), &w("abc")), 1);
        // by code unit, not by culture: "B" (0x42) before "a" (0x61)
        assert_eq!(ordinal_cmp(&w("B"), &w("a")), -1);
        assert_eq!(ordinal_cmp(&[], &w("a")), -1);
    }
}
