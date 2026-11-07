//! msvcp140.dll's locale facets beyond ctype<char>, for 64-bit programs: codecvt<char, char>
//! and codecvt<wchar_t, char> (what basic_filebuf converts with), ctype<wchar_t>,
//! time_put<wchar_t> and the "C" locale helpers under them (_Getwctype, _Towlower, _Mbrtowc,
//! _Wcrtomb, _Wcscoll, _Wcsxfrm, _Getcvt, __crtLCMapStringW). Ported from MS STL's <xlocale>,
//! <xloctime> and src/x*.cpp; the C++ locale is always "C" here (_Locinfo of "C"), so _Cvtvec
//! says _Isclocale and _Ctypevec has no locale name, which is what those sources then do.
//!
//! Layouts (x64, from clang's record dumps of the MS STL headers):
//! - facet { vftable, _Myrefs @8 }: codecvt<char, char> 16 bytes; codecvt<wchar_t, char>
//!   { _Cvtvec @16 } 64; ctype<wchar_t> { _Ctypevec @16, _Cvtvec @48 } 96; time_put<wchar_t>
//!   { _Timevec @16 } 24.
//! - _Ctypevec { u32 _Page, short* _Table @8, int _Delfl @16, wchar_t* _LocaleName @24 } 32;
//!   _Cvtvec { u32 _Page, u32 _Mbcurmax, int _Isclocale, u8 _Isleadbyte[32] } 44.
//! - vftables: facet's 3 (deleting dtor, _Incref, _Decref), then codecvt: do_always_noconv,
//!   do_max_length, do_encoding, do_in, do_out, do_unshift, do_length; ctype<wchar_t>: do_is
//!   (range), do_is, do_scan_is, do_scan_not, do_tolower (range), do_tolower, do_toupper x2,
//!   do_widen (range), do_widen, do_narrow (range), do_narrow; time_put: do_put.

#![allow(non_snake_case)]

use std::sync::OnceLock;

use crate::api::{host_fn, implement, Ctx, Handler};
use crate::mem::{self, r16, r32, r64, r8, w16, w32, w64, w8};
use crate::msvcp::{facet_decref, facet_delete, facet_incref, X_CTYPE};

/// _X_TIME (LC_TIME)
const X_TIME: u32 = 5;
/// codecvt_base::result
const OK: u32 = 0;
const PARTIAL: u32 = 1;
const ERROR: u32 = 2;
const NOCONV: u32 = 3;
const WEOF: u32 = 0xffff;

fn rq(a: u32) -> u32 {
    r64(a) as u32
}
/// Stores pointer `v` (widened).
fn wq(a: u32, v: u32) {
    w64(a, mem::widen(v));
}
/// A table of traps (a vtable, 8-byte entries).
fn table(name: &str, fs: &[Handler]) -> u32 {
    let t = mem::heap_alloc(8 * fs.len() as u32, true);
    for (i, f) in fs.iter().enumerate() {
        wq(
            t + 8 * i as u32,
            host_fn(&format!("msvcp_loc::{name}::{i}"), *f, 0),
        );
    }
    t
}
fn vcall(c: &mut Ctx, obj: u32, slot: u32, args: &[u64]) -> u64 {
    crate::msvcp_io::vcall(c, obj, slot, args)
}
/// A new facet of `size` bytes with `vft` and no references.
fn new_facet(size: u32, vft: u32) -> u32 {
    let f = mem::heap_alloc(size, true);
    wq(f, vft);
    f
}
/// The common _Getcat: a new facet into *pf when it is NULL.
fn getcat(c: &Ctx, make: fn() -> u32, cat: u32) -> u32 {
    let pf = c.a(0);
    if pf != 0 && rq(pf) == 0 {
        wq(pf, make());
    }
    cat
}

// ---------------------------------------------------------------------------------------------
// the "C" locale's character functions (src/xmbtowc.cpp, xwctomb.cpp, xgetwctype.cpp,
// xtowlower.cpp, xtowupper.cpp, xwcscoll.cpp, xwcsxfrm.cpp)

const CVTVEC: u32 = 44;
/// _Getcvt for "C": code page 0, one byte a character, _Isclocale
fn put_cvtvec(p: u32) {
    mem::fill(p, CVTVEC, 0);
    w32(p + 4, 1);
    w32(p + 8, 1);
}
/// _Ctypevec for "C": the CRT's classification table, not ours to free, no locale name
fn put_ctypevec(p: u32) {
    mem::fill(p, 32, 0);
    wq(p + 8, crate::crt::data("_pctype").map_or(0, mem::rp));
}
/// _Mbrtowc in the "C" locale: a byte is its own code (-1 never; 0 for NUL)
fn mbrtowc_c(b: u8) -> (u16, i32) {
    (b as u16, if b == 0 { 0 } else { 1 })
}
/// _Wcrtomb in the "C" locale: codes above 255 do not convert (EILSEQ)
fn wcrtomb_c(w: u16) -> Option<u8> {
    (w <= 255).then_some(w as u8)
}
/// _Getwctype: GetStringTypeW's CT_CTYPE1 flags
fn wctype(w: u16) -> u16 {
    crate::k32::ctype1(w)
}
/// _Towlower / _Towupper with no locale name: ASCII letters only below 256, LCMapStringW
/// above (WEOF stays)
fn towlower(w: u16) -> u16 {
    if w as u32 == WEOF {
        w
    } else if w < 256 {
        if (0x41..=0x5a).contains(&w) {
            w + 32
        } else {
            w
        }
    } else {
        crate::text::lower16(w)
    }
}
fn towupper(w: u16) -> u16 {
    if w as u32 == WEOF {
        w
    } else if w < 256 {
        if (0x61..=0x7a).contains(&w) {
            w - 32
        } else {
            w
        }
    } else {
        crate::text::upper16(w)
    }
}
fn wslice(a: u32, end: u32) -> Vec<u16> {
    (0..end.saturating_sub(a) / 2)
        .map(|i| r16(a + 2 * i))
        .collect()
}

/// _Mbrtowc(pwc, s, n, state, cvtvec)
fn _Mbrtowc(c: &mut Ctx) -> u32 {
    let (pwc, s, n) = (c.a(0), c.a(1), c.a64(2));
    if n == 0 {
        return 0;
    }
    let (w, r) = mbrtowc_c(r8(s));
    w16(pwc, w);
    r as u32
}
/// _Wcrtomb(s, wc, state, cvtvec)
fn _Wcrtomb(c: &mut Ctx) -> u32 {
    let (s, w) = (c.a(0), c.a(1) as u16);
    match wcrtomb_c(w) {
        Some(b) => {
            w8(s, b);
            1
        }
        None => {
            crate::crt::set_errno(42);
            u32::MAX
        }
    }
}
/// _Getcvt(): the _Cvtvec into the hidden result
fn _Getcvt(c: &mut Ctx) -> u32 {
    put_cvtvec(c.a(0));
    c.a(0)
}
/// _Getctype(): the _Ctypevec into the hidden result
fn _Getctype(c: &mut Ctx) -> u32 {
    put_ctypevec(c.a(0));
    c.a(0)
}
/// ?_Getcvt@_Locinfo@std@@QEBA?AU_Cvtvec@@XZ (this, result)
fn locinfo_getcvt(c: &mut Ctx) -> u32 {
    put_cvtvec(c.a(1));
    c.a(1)
}
/// ?_Getctype@_Locinfo@std@@QEBA?AU_Ctypevec@@XZ (this, result)
fn locinfo_getctype(c: &mut Ctx) -> u32 {
    put_ctypevec(c.a(1));
    c.a(1)
}
/// _Getcoll(): _Collvec { code page, locale name (NULL: "C") } into the hidden result
fn _Getcoll(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), 16, 0);
    c.a(0)
}
/// _Getwctype(wc, ctypevec)
fn _Getwctype(c: &mut Ctx) -> u32 {
    wctype(c.a(0) as u16) as u32
}
/// _Getwctypes(first, last, dest, ctypevec): last
fn _Getwctypes(c: &mut Ctx) -> u32 {
    let (first, last, dest) = (c.a(0), c.a(1), c.a(2));
    classify(first, last, dest)
}
fn classify(first: u32, last: u32, dest: u32) -> u32 {
    for (i, w) in wslice(first, last).into_iter().enumerate() {
        w16(dest + 2 * i as u32, wctype(w));
    }
    last
}
fn _Towlower(c: &mut Ctx) -> u32 {
    towlower(c.a(0) as u16) as u32
}
fn _Towupper(c: &mut Ctx) -> u32 {
    towupper(c.a(0) as u16) as u32
}
/// _Wcscoll(s1, end1, s2, end2, collvec): wmemcmp order, then the shorter first
fn _Wcscoll(c: &mut Ctx) -> u32 {
    let (a, ae, b, be) = (c.a(0), c.a(1), c.a(2), c.a(3));
    wslice(a, ae).cmp(&wslice(b, be)) as i32 as u32
}
/// _Wcsxfrm(dst, dst end, src, src end, collvec): the "C" locale's identity (copied when it
/// fits); the length needed
fn _Wcsxfrm(c: &mut Ctx) -> u32 {
    let (d, de, s, se) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let n = se.saturating_sub(s) / 2;
    if n <= de.saturating_sub(d) / 2 {
        mem::copy(d, s, 2 * n);
    }
    n
}
/// __crtLCMapStringW(locale name, flags, src, n, dst, cap): LCMapStringEx's slots
fn __crtLCMapStringW(c: &mut Ctx) -> u32 {
    crate::k32::LCMapStringW(c)
}

// ---------------------------------------------------------------------------------------------
// codecvt_base and codecvt<char, char, _Mbstatet>: no conversion

fn yes(_: &mut Ctx) -> u32 {
    1
}
fn no(_: &mut Ctx) -> u32 {
    0
}
/// do_in / do_out (this, state, from, from end, from next&, to, to end, to next&): noconv
fn cc_noconv(c: &mut Ctx) -> u32 {
    wq(c.a(4), c.a(2));
    wq(c.a(7), c.a(5));
    NOCONV
}
/// do_unshift (this, state, to, to end, to next&)
fn cc_unshift(c: &mut Ctx) -> u32 {
    wq(c.a(4), c.a(2));
    NOCONV
}
/// do_length (this, state, from, end, max): min(end - from, max)
fn cc_length(c: &mut Ctx) -> u32 {
    let n = c.a64(3).saturating_sub(c.a64(2)).min(c.a64(4));
    n.min(i32::MAX as u64) as u32
}
fn cc_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        table(
            "codecvt",
            &[
                facet_delete,
                facet_incref,
                facet_decref,
                yes,
                yes,
                yes,
                cc_noconv,
                cc_noconv,
                cc_unshift,
                cc_length,
            ],
        )
    })
}
fn new_cc() -> u32 {
    new_facet(16, cc_vtable())
}
fn cc_getcat(c: &mut Ctx) -> u32 {
    getcat(c, new_cc, X_CTYPE)
}
/// ??0?$codecvt@DDU_Mbstatet@@@std@@QEAA@_K@Z and the _Locinfo one (this, [locinfo,] refs)
fn cc_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, cc_vtable());
    w64(this + 8, c.a64(1));
    this
}
fn cc_ctor_li(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, cc_vtable());
    w64(this + 8, c.a64(2));
    this
}

// the members that call the virtual functions
fn v3(c: &mut Ctx) -> u32 {
    vcall(c, c.a(0), 3, &[]) as u32 & 0xff
}
fn v4(c: &mut Ctx) -> u32 {
    vcall(c, c.a(0), 4, &[]) as u32
}
fn v5(c: &mut Ctx) -> u32 {
    vcall(c, c.a(0), 5, &[]) as u32
}
/// in / out (this, state, from, from end, from next&, to, to end, to next&)
fn call7(c: &mut Ctx, slot: u32) -> u32 {
    let a: Vec<u64> = (1..8).map(|i| c.a64(i)).collect();
    vcall(c, c.a(0), slot, &a) as u32
}
fn cv_in(c: &mut Ctx) -> u32 {
    call7(c, 6)
}
fn cv_out(c: &mut Ctx) -> u32 {
    call7(c, 7)
}
fn cv_unshift(c: &mut Ctx) -> u32 {
    let a: Vec<u64> = (1..5).map(|i| c.a64(i)).collect();
    vcall(c, c.a(0), 8, &a) as u32
}
fn cv_length(c: &mut Ctx) -> u32 {
    let a: Vec<u64> = (1..5).map(|i| c.a64(i)).collect();
    vcall(c, c.a(0), 9, &a) as u32
}

// ---------------------------------------------------------------------------------------------
// codecvt<wchar_t, char, _Mbstatet>: bytes <-> wchar_t through _Mbrtowc / _Wcrtomb

/// do_in (this, state, from, from end, from next&, to, to end, to next&)
fn wc_in(c: &mut Ctx) -> u32 {
    let (mut f, fe, fnext, mut t, te, tnext) = (c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    let r = loop {
        if f == fe {
            break OK;
        }
        if t >= te {
            break PARTIAL;
        }
        let (w, _) = mbrtowc_c(r8(f));
        w16(t, w);
        f += 1;
        t += 2;
    };
    wq(fnext, f);
    wq(tnext, t);
    r
}
/// do_out (this, state, from, from end, from next&, to, to end, to next&)
fn wc_out(c: &mut Ctx) -> u32 {
    let (mut f, fe, fnext, mut t, te, tnext) = (c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    let mut r = OK;
    while f < fe && t != te {
        match wcrtomb_c(r16(f)) {
            Some(b) => {
                w8(t, b);
                f += 2;
                t += 1;
            }
            None => {
                r = ERROR;
                break;
            }
        }
    }
    if r == OK && f < fe {
        r = PARTIAL;
    }
    wq(fnext, f);
    wq(tnext, t);
    r
}
/// do_unshift: "C" has no shift state (L'\0' is one byte, nothing to write)
fn wc_unshift(c: &mut Ctx) -> u32 {
    wq(c.a(4), c.a(2));
    OK
}
/// do_length (this, state, from, end, max): one byte a character
fn wc_length(c: &mut Ctx) -> u32 {
    cc_length(c)
}
/// do_max_length / do_encoding: _Cvt._Mbcurmax (1) and _Mbcurmax == 1
fn wc_max_length(c: &mut Ctx) -> u32 {
    r32(c.a(0) + 16 + 4)
}
fn wc_encoding(c: &mut Ctx) -> u32 {
    (r32(c.a(0) + 16 + 4) == 1) as u32
}
fn wc_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        table(
            "wcodecvt",
            &[
                facet_delete,
                facet_incref,
                facet_decref,
                no,
                wc_max_length,
                wc_encoding,
                wc_in,
                wc_out,
                wc_unshift,
                wc_length,
            ],
        )
    })
}
fn new_wc() -> u32 {
    let f = new_facet(64, wc_vtable());
    put_cvtvec(f + 16);
    f
}
fn wc_getcat(c: &mut Ctx) -> u32 {
    getcat(c, new_wc, X_CTYPE)
}
fn wc_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, wc_vtable());
    w64(this + 8, c.a64(1));
    put_cvtvec(this + 16);
    this
}
fn wc_ctor_li(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, wc_vtable());
    w64(this + 8, c.a64(2));
    put_cvtvec(this + 16);
    this
}
/// ?_Init@?$codecvt@...@IEAAXAEBV_Locinfo@2@@Z (this, locinfo)
fn wc_init(c: &mut Ctx) -> u32 {
    put_cvtvec(c.a(0) + 16);
    0
}

// ---------------------------------------------------------------------------------------------
// ctype<wchar_t>

/// do_is(first, last, dest)
fn ct_is_range(c: &mut Ctx) -> u32 {
    classify(c.a(1), c.a(2), c.a(3))
}
/// do_is(mask, wc)
fn ct_is(c: &mut Ctx) -> u32 {
    (wctype(c.a(2) as u16) & c.a(1) as u16 != 0) as u32
}
fn scan(c: &mut Ctx, want: bool) -> u32 {
    let (m, mut p, last) = (c.a(1) as u16, c.a(2), c.a(3));
    while p < last && (wctype(r16(p)) & m != 0) != want {
        p += 2;
    }
    p.min(last)
}
/// do_scan_is(mask, first, last) / do_scan_not
fn ct_scan_is(c: &mut Ctx) -> u32 {
    scan(c, true)
}
fn ct_scan_not(c: &mut Ctx) -> u32 {
    scan(c, false)
}
fn map(c: &mut Ctx, f: fn(u16) -> u16) -> u32 {
    let (first, last) = (c.a(1), c.a(2));
    let mut p = first;
    while p < last {
        w16(p, f(r16(p)));
        p += 2;
    }
    last
}
fn ct_tolower_range(c: &mut Ctx) -> u32 {
    map(c, towlower)
}
fn ct_tolower(c: &mut Ctx) -> u32 {
    towlower(c.a(1) as u16) as u32
}
fn ct_toupper_range(c: &mut Ctx) -> u32 {
    map(c, towupper)
}
fn ct_toupper(c: &mut Ctx) -> u32 {
    towupper(c.a(1) as u16) as u32
}
/// _Dowiden: _Mbrtowc of the byte (a negative result would be WEOF)
fn widen(b: u8) -> u16 {
    mbrtowc_c(b).0
}
/// _Donarrow: _Wcrtomb, or the default
fn narrow(w: u16, dflt: u8) -> u8 {
    wcrtomb_c(w).unwrap_or(dflt)
}
/// do_widen(first, last, dest)
fn ct_widen_range(c: &mut Ctx) -> u32 {
    let (first, last, dest) = (c.a(1), c.a(2), c.a(3));
    for i in 0..last.saturating_sub(first) {
        w16(dest + 2 * i, widen(r8(first + i)));
    }
    last
}
fn ct_widen(c: &mut Ctx) -> u32 {
    widen(c.a(1) as u8) as u32
}
/// do_narrow(first, last, dflt, dest)
fn ct_narrow_range(c: &mut Ctx) -> u32 {
    let (first, last, dflt, dest) = (c.a(1), c.a(2), c.a(3) as u8, c.a(4));
    for (i, w) in wslice(first, last).into_iter().enumerate() {
        w8(dest + i as u32, narrow(w, dflt));
    }
    last
}
fn ct_narrow(c: &mut Ctx) -> u32 {
    narrow(c.a(1) as u16, c.a(2) as u8) as u32
}
fn ct_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        table(
            "wctype",
            &[
                facet_delete,
                facet_incref,
                facet_decref,
                ct_is_range,
                ct_is,
                ct_scan_is,
                ct_scan_not,
                ct_tolower_range,
                ct_tolower,
                ct_toupper_range,
                ct_toupper,
                ct_widen_range,
                ct_widen,
                ct_narrow_range,
                ct_narrow,
            ],
        )
    })
}
fn ct_fill(f: u32) {
    put_ctypevec(f + 16);
    put_cvtvec(f + 48);
}
fn new_ct() -> u32 {
    let f = new_facet(96, ct_vtable());
    ct_fill(f);
    f
}
fn ct_getcat(c: &mut Ctx) -> u32 {
    getcat(c, new_ct, X_CTYPE)
}
fn ct_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, ct_vtable());
    w64(this + 8, c.a64(1));
    ct_fill(this);
    this
}
fn ct_ctor_li(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, ct_vtable());
    w64(this + 8, c.a64(2));
    ct_fill(this);
    this
}
fn ct_init(c: &mut Ctx) -> u32 {
    ct_fill(c.a(0));
    0
}
// the members: through the vftable (a program may derive its own ctype<wchar_t>)
fn vargs(c: &mut Ctx, slot: u32, n: u32) -> u32 {
    let a: Vec<u64> = (1..=n).map(|i| c.a64(i)).collect();
    vcall(c, c.a(0), slot, &a) as u32
}
fn m_is_range(c: &mut Ctx) -> u32 {
    vargs(c, 3, 3)
}
fn m_is(c: &mut Ctx) -> u32 {
    vargs(c, 4, 2) & 0xff
}
fn m_scan_is(c: &mut Ctx) -> u32 {
    vargs(c, 5, 3)
}
fn m_scan_not(c: &mut Ctx) -> u32 {
    vargs(c, 6, 3)
}
fn m_tolower_range(c: &mut Ctx) -> u32 {
    vargs(c, 7, 2)
}
fn m_tolower(c: &mut Ctx) -> u32 {
    vargs(c, 8, 1) & 0xffff
}
fn m_toupper_range(c: &mut Ctx) -> u32 {
    vargs(c, 9, 2)
}
fn m_toupper(c: &mut Ctx) -> u32 {
    vargs(c, 10, 1) & 0xffff
}
fn m_widen_range(c: &mut Ctx) -> u32 {
    vargs(c, 11, 3)
}
fn m_widen(c: &mut Ctx) -> u32 {
    vargs(c, 12, 1) & 0xffff
}
fn m_narrow_range(c: &mut Ctx) -> u32 {
    vargs(c, 13, 4)
}
fn m_narrow(c: &mut Ctx) -> u32 {
    vargs(c, 14, 2) & 0xff
}
/// _Dowiden / _Donarrow (non-virtual)
fn m_dowiden(c: &mut Ctx) -> u32 {
    widen(c.a(1) as u8) as u32
}
fn m_donarrow(c: &mut Ctx) -> u32 {
    narrow(c.a(1) as u16, c.a(2) as u8) as u32
}

// ---------------------------------------------------------------------------------------------
// time_put<char> and time_put<wchar_t> (on ostreambuf_iterator): ostreambuf_iterator { bool
// _Failed, streambuf* _Strbuf @8 }, passed by address (16 bytes) and returned through the
// hidden result

fn ch<const W: bool>(p: u32) -> u16 {
    if W {
        r16(p)
    } else {
        r8(p) as u16
    }
}
const fn es<const W: bool>() -> u32 {
    if W {
        2
    } else {
        1
    }
}
/// *it++ = v
fn it_put<const W: bool>(c: &mut Ctx, it: u32, v: u16) {
    let sb = rq(it + 8);
    let eof = if W { WEOF } else { u32::MAX };
    if sb == 0 || crate::msvcp_io::sputc::<W>(c, sb, v as u32) == eof {
        w8(it, 1);
    }
}
/// do_put(this, result, it, ios_base&, fill, tm*, specifier, modifier): one field with
/// _Strftime / _Wcsftime ("%x" / "%Ex"), badbit when it is not one
fn tp_do_put<const W: bool>(c: &mut Ctx) -> u32 {
    let (ret, it, ios, tm, spec, modi) =
        (c.a(1), c.a(2), c.a(3), c.a(5), c.a(6) as u8, c.a(7) as u8);
    mem::copy(ret, it, 16);
    let mut fmt = vec![b'%' as u16];
    if modi != 0 {
        fmt.push(modi as u16);
    }
    fmt.push(spec as u16);
    match crate::crt::strftime_w(&fmt, tm) {
        Some(out) => {
            let out: Vec<u16> = if W {
                out
            } else {
                crate::text::w2a(&out).into_iter().map(u16::from).collect()
            };
            for v in out {
                it_put::<W>(c, ret, v);
            }
        }
        None => crate::msvcp_io::base_setstate(c, ios, 4),
    }
    ret
}
fn tp_vtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| {
        table(
            if W { "wtime_put" } else { "time_put" },
            &[facet_delete, facet_incref, facet_decref, tp_do_put::<W>],
        )
    })
}
fn new_tp<const W: bool>() -> u32 {
    new_facet(24, tp_vtable::<W>())
}
fn tp_getcat<const W: bool>(c: &mut Ctx) -> u32 {
    getcat(c, new_tp::<W>, X_TIME)
}
fn tp_ctor<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, tp_vtable::<W>());
    w64(this + 8, c.a64(1));
    this
}
fn tp_ctor_li<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this, tp_vtable::<W>());
    w64(this + 8, c.a64(2));
    this
}
/// put(this, result, it, ios, fill, tm, specifier, modifier): do_put
fn tp_put(c: &mut Ctx) -> u32 {
    vargs(c, 3, 7);
    c.a(1)
}
/// put(this, result, it, ios, fill, tm, pattern, pattern end): literal text, and each %[EOQ#]x
/// through do_put (<xloctime>'s loop: a trailing % or %E is copied as it is)
fn tp_put_pattern<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, ret, it, ios, fill, tm, mut p, end) = (
        c.a(0),
        c.a(1),
        c.a(2),
        c.a(3),
        c.a64(4),
        c.a64(5),
        c.a(6),
        c.a(7),
    );
    let e = es::<W>();
    let mut cur = mem::heap_alloc(16, false);
    mem::copy(cur, it, 16);
    let nar = |v: u16| narrow(v, 0);
    while p < end {
        let v = ch::<W>(p);
        if nar(v) != b'%' {
            it_put::<W>(c, cur, v);
        } else if p + e >= end {
            it_put::<W>(c, cur, v);
            break;
        } else {
            p += e;
            let raw = ch::<W>(p);
            let (mut spec, mut modi) = (nar(raw), 0u8);
            if matches!(spec, b'E' | b'O' | b'Q' | b'#') {
                if p + e >= end {
                    it_put::<W>(c, cur, v);
                    it_put::<W>(c, cur, raw);
                    break;
                }
                p += e;
                modi = spec;
                spec = nar(ch::<W>(p));
            }
            let out = mem::heap_alloc(16, false);
            vcall(
                c,
                this,
                3,
                &[
                    mem::widen(out),
                    mem::widen(cur),
                    mem::widen(ios),
                    fill,
                    tm,
                    spec as u64,
                    modi as u64,
                ],
            );
            mem::heap_free(cur);
            cur = out;
            if c.jumped {
                break;
            }
        }
        p += e;
    }
    mem::copy(ret, cur, 16);
    mem::heap_free(cur);
    ret
}

// ---------------------------------------------------------------------------------------------

pub fn init() {
    const CC: &str = "?$codecvt@DDU_Mbstatet@@@std@@";
    const WC: &str = "?$codecvt@_WDU_Mbstatet@@@std@@";
    const CT: &str = "?$ctype@_W@std@@";
    const TP: &str = "?$time_put@_WV?$ostreambuf_iterator@_WU?$char_traits@_W@std@@@std@@@std@@";
    const GETCAT: &str = "SA_KPEAPEBVfacet@locale@2@PEBV42@@Z";
    const IN: &str = "QEBAHAEAU_Mbstatet@@PEBD1AEAPEBDPEAD3AEAPEAD@Z";
    const WIN: &str = "QEBAHAEAU_Mbstatet@@PEBD1AEAPEBDPEA_W3AEAPEA_W@Z";
    const WOUT: &str = "QEBAHAEAU_Mbstatet@@PEB_W1AEAPEB_WPEAD3AEAPEAD@Z";
    const UNSHIFT: &str = "QEBAHAEAU_Mbstatet@@PEAD1AEAPEAD@Z";
    const LENGTH: &str = "QEBAHAEAU_Mbstatet@@PEBD1_K@Z";
    let m = |s: &str| s.replacen("QEBA", "MEBA", 1);
    let mut list: Vec<(String, Handler)> = vec![
        ("_Mbrtowc".into(), _Mbrtowc as Handler),
        ("_Wcrtomb".into(), _Wcrtomb),
        ("_Getcvt".into(), _Getcvt),
        ("_Getctype".into(), _Getctype),
        ("_Getcoll".into(), _Getcoll),
        (
            "?_Getcvt@_Locinfo@std@@QEBA?AU_Cvtvec@@XZ".into(),
            locinfo_getcvt,
        ),
        (
            "?_Getctype@_Locinfo@std@@QEBA?AU_Ctypevec@@XZ".into(),
            locinfo_getctype,
        ),
        ("_Getwctype".into(), _Getwctype),
        ("_Getwctypes".into(), _Getwctypes),
        ("_Towlower".into(), _Towlower),
        ("_Towupper".into(), _Towupper),
        ("_Wcscoll".into(), _Wcscoll),
        ("_Wcsxfrm".into(), _Wcsxfrm),
        ("__crtLCMapStringW".into(), __crtLCMapStringW),
        ("__crtLCMapStringEx".into(), __crtLCMapStringW),
        // codecvt_base
        ("?always_noconv@codecvt_base@std@@QEBA_NXZ".into(), v3),
        ("?max_length@codecvt_base@std@@QEBAHXZ".into(), v4),
        ("?encoding@codecvt_base@std@@QEBAHXZ".into(), v5),
        ("?do_always_noconv@codecvt_base@std@@MEBA_NXZ".into(), yes),
        ("?do_max_length@codecvt_base@std@@MEBAHXZ".into(), yes),
        ("?do_encoding@codecvt_base@std@@MEBAHXZ".into(), yes),
        // codecvt<char, char>
        (format!("?_Getcat@{CC}{GETCAT}"), cc_getcat),
        (format!("??0{CC}QEAA@_K@Z"), cc_ctor),
        (format!("??0{CC}QEAA@AEBV_Locinfo@1@_K@Z"), cc_ctor_li),
        (format!("??1{CC}MEAA@XZ"), crate::msvcp::nothing),
        (format!("?_Init@{CC}IEAAXAEBV_Locinfo@2@@Z"), no),
        (format!("?in@{CC}{IN}"), cv_in),
        (format!("?out@{CC}{IN}"), cv_out),
        (format!("?unshift@{CC}{UNSHIFT}"), cv_unshift),
        (format!("?length@{CC}{LENGTH}"), cv_length),
        (format!("?do_always_noconv@{CC}MEBA_NXZ"), yes),
        (format!("?do_in@{CC}{}", m(IN)), cc_noconv),
        (format!("?do_out@{CC}{}", m(IN)), cc_noconv),
        (format!("?do_unshift@{CC}{}", m(UNSHIFT)), cc_unshift),
        (format!("?do_length@{CC}{}", m(LENGTH)), cc_length),
        // codecvt<wchar_t, char>
        (format!("?_Getcat@{WC}{GETCAT}"), wc_getcat),
        (format!("??0{WC}QEAA@_K@Z"), wc_ctor),
        (format!("??0{WC}QEAA@AEBV_Locinfo@1@_K@Z"), wc_ctor_li),
        (format!("??1{WC}MEAA@XZ"), crate::msvcp::nothing),
        (format!("?_Init@{WC}IEAAXAEBV_Locinfo@2@@Z"), wc_init),
        (format!("?in@{WC}{WIN}"), cv_in),
        (format!("?out@{WC}{WOUT}"), cv_out),
        (format!("?unshift@{WC}{UNSHIFT}"), cv_unshift),
        (format!("?length@{WC}{LENGTH}"), cv_length),
        (format!("?do_always_noconv@{WC}MEBA_NXZ"), no),
        (format!("?do_max_length@{WC}MEBAHXZ"), wc_max_length),
        (format!("?do_encoding@{WC}MEBAHXZ"), wc_encoding),
        (format!("?do_in@{WC}{}", m(WIN)), wc_in),
        (format!("?do_out@{WC}{}", m(WOUT)), wc_out),
        (format!("?do_unshift@{WC}{}", m(UNSHIFT)), wc_unshift),
        (format!("?do_length@{WC}{}", m(LENGTH)), wc_length),
        // ctype<wchar_t>
        (format!("?_Getcat@{CT}{GETCAT}"), ct_getcat),
        (format!("??0{CT}QEAA@_K@Z"), ct_ctor),
        (format!("??0{CT}QEAA@AEBV_Locinfo@1@_K@Z"), ct_ctor_li),
        (format!("??1{CT}MEAA@XZ"), crate::msvcp::nothing),
        (format!("?_Init@{CT}IEAAXAEBV_Locinfo@2@@Z"), ct_init),
        (format!("?is@{CT}QEBAPEB_WPEB_W0PEAF@Z"), m_is_range),
        (format!("?is@{CT}QEBA_NF_W@Z"), m_is),
        (format!("?scan_is@{CT}QEBAPEB_WFPEB_W0@Z"), m_scan_is),
        (format!("?scan_not@{CT}QEBAPEB_WFPEB_W0@Z"), m_scan_not),
        (
            format!("?tolower@{CT}QEBAPEB_WPEA_WPEB_W@Z"),
            m_tolower_range,
        ),
        (format!("?tolower@{CT}QEBA_W_W@Z"), m_tolower),
        (
            format!("?toupper@{CT}QEBAPEB_WPEA_WPEB_W@Z"),
            m_toupper_range,
        ),
        (format!("?toupper@{CT}QEBA_W_W@Z"), m_toupper),
        (format!("?widen@{CT}QEBAPEBDPEBD0PEA_W@Z"), m_widen_range),
        (format!("?widen@{CT}QEBA_WD@Z"), m_widen),
        (
            format!("?narrow@{CT}QEBAPEB_WPEB_W0DPEAD@Z"),
            m_narrow_range,
        ),
        (format!("?narrow@{CT}QEBAD_WD@Z"), m_narrow),
        (format!("?_Dowiden@{CT}IEBA_WD@Z"), m_dowiden),
        (format!("?_Donarrow@{CT}IEBAD_WD@Z"), m_donarrow),
        (format!("?do_is@{CT}MEBAPEB_WPEB_W0PEAF@Z"), ct_is_range),
        (format!("?do_is@{CT}MEBA_NF_W@Z"), ct_is),
        (format!("?do_scan_is@{CT}MEBAPEB_WFPEB_W0@Z"), ct_scan_is),
        (format!("?do_scan_not@{CT}MEBAPEB_WFPEB_W0@Z"), ct_scan_not),
        (
            format!("?do_tolower@{CT}MEBAPEB_WPEA_WPEB_W@Z"),
            ct_tolower_range,
        ),
        (format!("?do_tolower@{CT}MEBA_W_W@Z"), ct_tolower),
        (
            format!("?do_toupper@{CT}MEBAPEB_WPEA_WPEB_W@Z"),
            ct_toupper_range,
        ),
        (format!("?do_toupper@{CT}MEBA_W_W@Z"), ct_toupper),
        (
            format!("?do_widen@{CT}MEBAPEBDPEBD0PEA_W@Z"),
            ct_widen_range,
        ),
        (format!("?do_widen@{CT}MEBA_WD@Z"), ct_widen),
        (
            format!("?do_narrow@{CT}MEBAPEB_WPEB_W0DPEAD@Z"),
            ct_narrow_range,
        ),
        (format!("?do_narrow@{CT}MEBAD_WD@Z"), ct_narrow),
    ];
    for (w, tp, it, el) in [
        (
            false,
            "?$time_put@DV?$ostreambuf_iterator@DU?$char_traits@D@std@@@std@@@std@@",
            "?AV?$ostreambuf_iterator@DU?$char_traits@D@std@@@2@V32@",
            "D",
        ),
        (
            true,
            TP,
            "?AV?$ostreambuf_iterator@_WU?$char_traits@_W@std@@@2@V32@",
            "_W",
        ),
    ] {
        let g = |n: Handler, m: Handler| if w { m } else { n };
        list.push((
            format!("?_Getcat@{tp}{GETCAT}"),
            g(tp_getcat::<false>, tp_getcat::<true>),
        ));
        list.push((
            format!("??0{tp}QEAA@_K@Z"),
            g(tp_ctor::<false>, tp_ctor::<true>),
        ));
        list.push((
            format!("??0{tp}QEAA@AEBV_Locinfo@1@_K@Z"),
            g(tp_ctor_li::<false>, tp_ctor_li::<true>),
        ));
        list.push((format!("??1{tp}MEAA@XZ"), crate::msvcp::nothing));
        list.push((format!("?_Init@{tp}IEAAXAEBV_Locinfo@2@@Z"), no));
        list.push((
            format!("?put@{tp}QEBA{it}AEAVios_base@2@{el}PEBUtm@@DD@Z"),
            tp_put,
        ));
        let pat = if w { "PEB_W4" } else { "PEBD3" };
        list.push((
            format!("?put@{tp}QEBA{it}AEAVios_base@2@{el}PEBUtm@@{pat}@Z"),
            g(tp_put_pattern::<false>, tp_put_pattern::<true>),
        ));
        list.push((
            format!("?do_put@{tp}MEBA{it}AEAVios_base@2@{el}PEBUtm@@DD@Z"),
            g(tp_do_put::<false>, tp_do_put::<true>),
        ));
    }
    for (name, f) in list {
        implement("msvcp140", Box::leak(name.into_boxed_str()), f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_locale_conversions() {
        for b in 0..=255u8 {
            let (w, n) = mbrtowc_c(b);
            assert_eq!(w, b as u16);
            assert_eq!(n, (b != 0) as i32);
            assert_eq!(wcrtomb_c(w), Some(b));
            assert_eq!(narrow(widen(b), b'?'), b);
        }
        assert_eq!(wcrtomb_c(0x100), None);
        assert_eq!(narrow(0x3042, b'?'), b'?');
    }

    #[test]
    fn c_locale_case() {
        assert_eq!(towlower(b'A' as u16), b'a' as u16);
        assert_eq!(towupper(b'z' as u16), b'Z' as u16);
        // (Latin-1 letters keep their case below 256 in "C")
        assert_eq!(towlower(0xc0), 0xc0);
        assert_eq!(towlower(WEOF as u16), WEOF as u16);
        for w in 0..128u16 {
            let ch = w as u8 as char;
            assert_eq!(towlower(w), ch.to_ascii_lowercase() as u16);
            assert_eq!(towupper(w), ch.to_ascii_uppercase() as u16);
        }
    }
}
