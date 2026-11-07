//! msvcp140.dll, the parts of the C++ standard library programs call out of line: the locale
//! machinery std::regex and the streams use (locale::_Init, locale::id, facet reference counts,
//! ctype<char>, _Locinfo, the "C" locale's _Strcoll / _Strxfrm) and the _X* functions that
//! throw the standard exceptions. Layouts are MSVC's (<xlocale>, <xlocinfo>, <exception>) with
//! pointer-sized fields; the thrown exceptions' ThrowInfo is the RVA form with base 0 (absolute
//! addresses), which is also x86's pointer form. Registered for 64-bit programs (a 32-bit one
//! needs thiscall glue first, issue #30).

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::api::{host_fn, implement, Ctx, Handler};
use crate::mem::{self, cstr, ps, r8, rp, w32, w64, w8, wp};

/// _X_CTYPE (LC_CTYPE): the category ctype<char>::_Getcat gives
pub(crate) const X_CTYPE: u32 = 2;

fn align(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}

/// A table of traps (a vtable).
fn table(name: &str, fs: &[Handler]) -> u32 {
    let t = mem::heap_alloc(ps() * fs.len() as u32, true);
    for (i, f) in fs.iter().enumerate() {
        wp(t + ps() * i as u32, host_fn(&format!("msvcp::{name}::{i}"), *f, 0));
    }
    t
}

// ---------------------------------------------------------------------------------------------
// facets: { vtable (scalar deleting destructor, _Incref, _Decref, ...), size_t _Myrefs }

fn refs(this: u32) -> u32 {
    this + ps()
}
/// ?_Incref@facet@locale@std@@UEAAXXZ
pub(crate) fn facet_incref(c: &mut Ctx) -> u32 {
    let p = refs(c.a(0));
    wp(p, rp(p).wrapping_add(1));
    0
}
/// ?_Decref@facet@locale@std@@UEAAPEAV_Facet_base@3@XZ: the facet to delete when the last
/// reference went
pub(crate) fn facet_decref(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = rp(refs(this)).wrapping_sub(1);
    wp(refs(this), n);
    if n == 0 { this } else { 0 }
}
/// the scalar deleting destructor of the facets made here (flags bit 0: free it)
pub(crate) fn facet_delete(c: &mut Ctx) -> u32 {
    let (this, flags) = (c.a(0), c.a(1));
    if flags & 1 != 0 {
        mem::heap_free(this);
    }
    this
}
fn facet_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| table("facet", &[facet_delete, facet_incref, facet_decref]))
}
/// ??0facet@locale@std@@IEAA@_K@Z (refs)
fn facet_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wp(this, facet_vtable());
    wp(refs(this), c.a(1));
    this
}
pub(crate) fn nothing(c: &mut Ctx) -> u32 {
    c.a(0)
}

// ---------------------------------------------------------------------------------------------
// locale: { _Locimp* }; _Locimp : facet { facet** _Facetvec, size_t _Facetcount, int _Catmask,
// bool _Xparent, _Yarn<char> _Name }. The global (and classic) one is the "C" locale with no
// facets of its own: use_facet makes each with its _Getcat and keeps it.

fn locimp() -> u32 {
    static L: OnceLock<u32> = OnceLock::new();
    *L.get_or_init(|| {
        let p = ps();
        let name_at = align(4 * p + 5, p);
        let l = mem::heap_alloc(name_at + 2 * p, true);
        wp(l, facet_vtable());
        // (never deleted)
        wp(refs(l), 1 << 30);
        w32(l + 4 * p, 0x3f);
        let name = mem::heap_alloc(2, true);
        w8(name, b'C');
        wp(l + name_at, name);
        l
    })
}
/// ?_Init@locale@std@@CAPEAV_Locimp@12@_N@Z (bool incref)
fn locale_init(c: &mut Ctx) -> u32 {
    let l = locimp();
    if c.a(0) & 0xff != 0 {
        wp(refs(l), rp(refs(l)).wrapping_add(1));
    }
    l
}
/// A locale object is 16 bytes: its two empty bases take the first 8 (MSVC lays a second
/// empty base out at offset 1), _Ptr is at 8.
pub const LOCALE_SIZE: u32 = 16;
const LOCALE_PTR: u32 = 8;

/// A new locale object (locale(): the global locale, referenced).
pub fn new_locale() -> u32 {
    let p = mem::heap_alloc(LOCALE_SIZE, true);
    let l = locimp();
    wp(refs(l), rp(refs(l)).wrapping_add(1));
    wp(p + LOCALE_PTR, l);
    p
}
/// facet::_Incref / _Decref through the _Locimp's vftable
fn locimp_incref(c: &mut Ctx, l: u32) {
    if l != 0 {
        let f = rp(rp(l) + ps());
        c.call(f, &[l]);
    }
}
fn locimp_release(c: &mut Ctx, l: u32) {
    if l == 0 {
        return;
    }
    let f = rp(rp(l) + 2 * ps());
    let del = c.call(f, &[l]);
    if del != 0 {
        let d = rp(rp(del));
        c.call(d, &[del, 1]);
    }
}
/// ~locale of the locale object at `p` (its storage stays)
pub fn delete_locale_value(c: &mut Ctx, p: u32) {
    let l = rp(p + LOCALE_PTR);
    locimp_release(c, l);
    wp(p + LOCALE_PTR, 0);
}
/// delete of a locale object made with new
pub fn delete_locale(c: &mut Ctx, p: u32) {
    delete_locale_value(c, p);
    mem::heap_free(p);
}
/// locale(const locale&) at `dst` from the locale object at `src`
pub fn locale_copy(c: &mut Ctx, dst: u32, src: u32) {
    let l = rp(src + LOCALE_PTR);
    w64(dst, 0);
    w64(dst + LOCALE_PTR, mem::widen(l));
    locimp_incref(c, l);
}
/// locale::operator=
pub fn locale_assign(c: &mut Ctx, dst: u32, src: u32) {
    let (a, b) = (rp(dst + LOCALE_PTR), rp(src + LOCALE_PTR));
    if a != b {
        locimp_release(c, a);
        w64(dst + LOCALE_PTR, mem::widen(b));
        locimp_incref(c, b);
    }
}

/// ?_Getgloballocale@locale@std@@CAPEAV_Locimp@12@XZ
fn locale_global(_: &mut Ctx) -> u32 {
    locimp()
}
/// ??Bid@locale@std@@QEAA_KXZ: the id's index, given on first use
fn locale_id(c: &mut Ctx) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let this = c.a(0);
    if rp(this) == 0 {
        wp(this, NEXT.fetch_add(1, Ordering::Relaxed) + 1);
    }
    rp(this)
}
/// The locale::id data exports (one zero word each, numbered by locale_id).
pub fn data(name: &str) -> Option<u32> {
    if let Some(a) = crate::msvcp_io::data(name) {
        return Some(a);
    }
    static IDS: std::sync::Mutex<Vec<(String, u32)>> = std::sync::Mutex::new(Vec::new());
    if !(name.starts_with("?id@?$") && name.ends_with("@std@@2V0locale@2@A")) {
        return None;
    }
    let mut g = IDS.lock().unwrap();
    if let Some(&(_, a)) = g.iter().find(|(n, _)| n == name) {
        return Some(a);
    }
    let a = mem::heap_alloc(8, true);
    g.push((name.to_string(), a));
    Some(a)
}

// ---------------------------------------------------------------------------------------------
// _Lockit { int _Locktype } and _Locinfo { _Lockit, _Yarn<char> _Days, _Months, _Yarn<wchar_t>
// _W_Days, _W_Months, _Yarn<char> _Oldlocname, _Newlocname }: the "C" locale's

/// ??0_Lockit@std@@QEAA@H@Z (kind): one thread's sake runs the C++ library calls in order
fn lockit_ctor(c: &mut Ctx) -> u32 {
    w32(c.a(0), c.a(1));
    c.a(0)
}
/// ??0_Locinfo@std@@QEAA@PEBD@Z (name)
fn locinfo_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let p = ps();
    mem::fill(this, p.max(4) + 12 * p, 0);
    this
}
/// ?_Getcoll@_Locinfo@std@@QEBA?AU_Collvec@@XZ: _Collvec { code page, locale name (NULL:
/// "C") } into the hidden result
fn locinfo_getcoll(c: &mut Ctx) -> u32 {
    let ret = c.a(1);
    w32(ret, 0);
    wp(ret + ps(), 0);
    ret
}
/// _Strcoll(s1, end1, s2, end2, collvec): the "C" locale's byte order
fn _Strcoll(c: &mut Ctx) -> u32 {
    let (a, ae, b, be) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let x = mem::slice(a, ae.saturating_sub(a));
    let y = mem::slice(b, be.saturating_sub(b));
    x.cmp(y) as i32 as u32
}
/// _Strxfrm(dst, dst end, src, src end, collvec): the "C" locale's identity; the length needed
fn _Strxfrm(c: &mut Ctx) -> u32 {
    let (d, de, s, se) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let n = se.saturating_sub(s);
    if n <= de.saturating_sub(d) {
        mem::copy(d, s, n);
    }
    n
}

// ---------------------------------------------------------------------------------------------
// ctype<char> : facet { _Ctypevec { code page, const short* table, int delfl, wchar_t* locale
// name } }; vtable (MSVC puts an overload set in reverse order of declaration): destructor,
// _Incref, _Decref, do_tolower(first, last), do_tolower(c), do_toupper x2 (the same),
// do_widen(first, last, dest), do_widen(c), do_narrow(first, last, dflt, dest),
// do_narrow(c, dflt)

fn lower(b: u8) -> u8 {
    b.to_ascii_lowercase()
}
fn upper(b: u8) -> u8 {
    b.to_ascii_uppercase()
}
fn map_range(c: &mut Ctx, f: fn(u8) -> u8) -> u32 {
    let (first, last) = (c.a(1), c.a(2));
    for p in first..last {
        w8(p, f(r8(p)));
    }
    last
}
fn ct_tolower(c: &mut Ctx) -> u32 {
    lower(c.a(1) as u8) as u32
}
fn ct_tolower_range(c: &mut Ctx) -> u32 {
    map_range(c, lower)
}
fn ct_toupper(c: &mut Ctx) -> u32 {
    upper(c.a(1) as u8) as u32
}
fn ct_toupper_range(c: &mut Ctx) -> u32 {
    map_range(c, upper)
}
fn ct_widen(c: &mut Ctx) -> u32 {
    c.a(1) & 0xff
}
fn ct_widen_range(c: &mut Ctx) -> u32 {
    let (first, last, dest) = (c.a(1), c.a(2), c.a(3));
    mem::copy(dest, first, last.saturating_sub(first));
    last
}
fn ct_narrow(c: &mut Ctx) -> u32 {
    c.a(1) & 0xff
}
fn ct_narrow_range(c: &mut Ctx) -> u32 {
    let (first, last, dest) = (c.a(1), c.a(2), c.a(4));
    mem::copy(dest, first, last.saturating_sub(first));
    last
}
fn ctype_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        table(
            "ctype",
            &[facet_delete, facet_incref, facet_decref, ct_tolower_range, ct_tolower, ct_toupper_range, ct_toupper, ct_widen_range, ct_widen, ct_narrow_range, ct_narrow],
        )
    })
}
/// ?_Getcat@?$ctype@D@std@@SA_KPEAPEBVfacet@locale@2@PEBV42@@Z (facet**, locale*): a new
/// ctype<char> of the "C" locale (0 references) when *pf is NULL
fn ctype_getcat(c: &mut Ctx) -> u32 {
    let pf = c.a(0);
    if pf != 0 && rp(pf) == 0 {
        let p = ps();
        let f = mem::heap_alloc(6 * p, true);
        wp(f, ctype_vtable());
        wp(f + 3 * p, crate::crt::data("_pctype").map_or(0, rp));
        wp(pf, f);
    }
    X_CTYPE
}
/// ctype<char>::tolower(c) / tolower(first, last): the virtual do_tolower
fn ctype_tolower(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let f = rp(rp(this) + 4 * ps());
    c.call(f, &[this, c.a(1)]) & 0xff
}
fn ctype_tolower_range(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let f = rp(rp(this) + 3 * ps());
    c.call(f, &[this, c.a(1), c.a(2)])
}

// ---------------------------------------------------------------------------------------------
// the standard exceptions: { vtable (scalar deleting destructor, what), __std_exception_data
// { what, do_free } } (regex_error: then its error_type); thrown with a ThrowInfo { attributes,
// destructor, forward compat, catchable types } listing the class and its bases

struct Class {
    /// mangled names, the class first, then its bases
    names: &'static [&'static str],
    size: u32,
}

fn ex_what(c: &mut Ctx) -> u32 {
    let w = rp(c.a(0) + ps());
    if w != 0 {
        return w;
    }
    static UNKNOWN: OnceLock<u32> = OnceLock::new();
    *UNKNOWN.get_or_init(|| {
        let p = mem::heap_alloc(20, true);
        mem::write(p, b"Unknown exception\0");
        p
    })
}
fn ex_free_what(this: u32) {
    let d = this + ps();
    if r8(d + ps()) != 0 && rp(d) != 0 {
        mem::heap_free(rp(d));
    }
    wp(d, 0);
    wp(d + ps(), 0);
}
fn ex_delete(c: &mut Ctx) -> u32 {
    let (this, flags) = (c.a(0), c.a(1));
    ex_free_what(this);
    if flags & 1 != 0 {
        mem::heap_free(this);
    }
    this
}
/// the destructor ThrowInfo names
fn ex_dtor(c: &mut Ctx) -> u32 {
    ex_free_what(c.a(0));
    0
}
fn ex_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| table("exception", &[ex_delete, ex_what]))
}
/// the copy constructor (this, source): a do_free message is copied
fn copy_ctor(size: u32) -> impl Fn(&mut Ctx) -> u32 {
    move |c: &mut Ctx| {
        let (this, src) = (c.a(0), c.a(1));
        mem::copy(this, src, size);
        let w = rp(src + ps());
        if r8(src + 2 * ps()) != 0 && w != 0 {
            let s = cstr(w);
            let p = mem::heap_alloc(s.len() as u32 + 1, true);
            mem::write(p, &s);
            wp(this + ps(), p);
        }
        this
    }
}
fn copy24(c: &mut Ctx) -> u32 {
    copy_ctor(3 * ps())(c)
}
fn copy32(c: &mut Ctx) -> u32 {
    copy_ctor(3 * ps() + 8)(c)
}

/// The ThrowInfo of `class` (made once per class).
fn throw_info(class: &Class) -> u32 {
    static MADE: std::sync::Mutex<Vec<(&'static str, u32)>> = std::sync::Mutex::new(Vec::new());
    let mut g = MADE.lock().unwrap();
    if let Some(&(_, t)) = g.iter().find(|(n, _)| *n == class.names[0]) {
        return t;
    }
    let p = ps();
    let dtor = host_fn("msvcp::exception::dtor", ex_dtor, 0);
    let copy = if class.size > 3 * p { host_fn("msvcp::exception::copy32", copy32, 0) } else { host_fn("msvcp::exception::copy24", copy24, 0) };
    let n = class.names.len() as u32;
    let array = mem::heap_alloc(4 + 4 * n, true);
    w32(array, n);
    for (i, name) in class.names.iter().enumerate() {
        // type_info { vtable, spare, ".?AV...@std@@" }
        let ti = mem::heap_alloc(2 * p + name.len() as u32 + 1, true);
        mem::write(ti + 2 * p, name.as_bytes());
        // CatchableType { flags, type, this displacement { mdisp, pdisp -1, vdisp }, size, copy }
        let ct = mem::heap_alloc(28, true);
        w32(ct + 4, ti);
        w32(ct + 12, -1i32 as u32);
        w32(ct + 20, class.size);
        w32(ct + 24, copy);
        w32(array + 4 + 4 * i as u32, ct);
    }
    let t = mem::heap_alloc(16, true);
    w32(t + 4, dtor);
    w32(t + 12, array);
    g.push((class.names[0], t));
    t
}

/// Throws a new `class` object with message `what` (static: do_free 0) and an int after it.
fn throw(c: &mut Ctx, class: &Class, what: u32, extra: u32) -> u32 {
    let o = mem::heap_alloc(class.size, true);
    wp(o, ex_vtable());
    wp(o + ps(), what);
    if class.size > 3 * ps() {
        w32(o + 3 * ps(), extra);
    }
    crate::cxx64::throw_object(c, o as u64, throw_info(class));
    0
}
fn static_str(s: &str) -> u32 {
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, s.as_bytes());
    p
}

const SYSTEM_ERROR: Class = Class { names: &[".?AVsystem_error@std@@", ".?AV_System_error@std@@", ".?AVruntime_error@std@@", ".?AVexception@std@@"], size: 40 };
const INVALID_ARGUMENT: Class = Class { names: &[".?AVinvalid_argument@std@@", ".?AVlogic_error@std@@", ".?AVexception@std@@"], size: 24 };
const BAD_FUNCTION_CALL: Class = Class { names: &[".?AVbad_function_call@std@@", ".?AVexception@std@@"], size: 24 };
const RUNTIME_ERROR: Class = Class { names: &[".?AVruntime_error@std@@", ".?AVexception@std@@"], size: 24 };
const OVERFLOW_ERROR: Class = Class { names: &[".?AVoverflow_error@std@@", ".?AVruntime_error@std@@", ".?AVexception@std@@"], size: 24 };

/// generic_category() (an error_category: vtable, _Addr): what system_error's error_code points
/// to; its virtual functions (name, message, ...) are not called by the programs here
fn generic_category() -> u32 {
    static C: OnceLock<u32> = OnceLock::new();
    *C.get_or_init(|| {
        let p = mem::heap_alloc(2 * ps(), true);
        wp(p, table("error_category", &[nothing, nothing, nothing, nothing, nothing, nothing]));
        wp(p + ps(), 3);
        p
    })
}
/// Throws system_error(error_code(errno, generic_category())) with message `what`
/// (runtime_error's data, then the error_code { int, const error_category * }).
/// iostream_category(): what ios_base::failure's error_code (io_errc::stream) points to
fn iostream_category() -> u32 {
    static C: OnceLock<u32> = OnceLock::new();
    *C.get_or_init(|| {
        let p = mem::heap_alloc(2 * ps(), true);
        wp(p, table("iostream_category", &[nothing, nothing, nothing, nothing, nothing, nothing]));
        wp(p + ps(), 5);
        p
    })
}
const FAILURE: Class = Class {
    names: &[".?AVfailure@ios_base@std@@", ".?AVsystem_error@std@@", ".?AV_System_error@std@@", ".?AVruntime_error@std@@", ".?AVexception@std@@"],
    size: 40,
};
/// Throws ios_base::failure(msg) (error_code(io_errc::stream, iostream_category()); what() as
/// system_error makes it: "msg: iostream stream error").
pub fn throw_failure(c: &mut Ctx, msg: &str) {
    let o = mem::heap_alloc(FAILURE.size, true);
    wp(o, ex_vtable());
    wp(o + ps(), static_str(&format!("{msg}: iostream stream error")));
    w32(o + 3 * ps(), 1);
    wp(o + 4 * ps(), iostream_category());
    crate::cxx64::throw_object(c, o as u64, throw_info(&FAILURE));
}
/// throw; (the exception being handled)
pub fn rethrow(c: &mut Ctx) {
    crate::cxx64::rethrow(c);
}

pub fn throw_system_error(c: &mut Ctx, errno: u32, what: u32) -> u32 {
    let o = mem::heap_alloc(SYSTEM_ERROR.size, true);
    wp(o, ex_vtable());
    wp(o + ps(), what);
    w32(o + 3 * ps(), errno);
    wp(o + 4 * ps(), generic_category());
    crate::cxx64::throw_object(c, o as u64, throw_info(&SYSTEM_ERROR));
    0
}
fn x_invalid_argument(c: &mut Ctx) -> u32 {
    let w = c.a(0);
    throw(c, &INVALID_ARGUMENT, w, 0)
}
fn x_runtime_error(c: &mut Ctx) -> u32 {
    let w = c.a(0);
    throw(c, &RUNTIME_ERROR, w, 0)
}
fn x_overflow_error(c: &mut Ctx) -> u32 {
    let w = c.a(0);
    throw(c, &OVERFLOW_ERROR, w, 0)
}
fn x_bad_function_call(c: &mut Ctx) -> u32 {
    throw(c, &BAD_FUNCTION_CALL, static_str("bad function call"), 0)
}

const BAD_ALLOC: Class = Class { names: &[".?AVbad_alloc@std@@", ".?AVexception@std@@"], size: 24 };
const LENGTH_ERROR: Class = Class { names: &[".?AVlength_error@std@@", ".?AVlogic_error@std@@", ".?AVexception@std@@"], size: 24 };
const OUT_OF_RANGE: Class = Class { names: &[".?AVout_of_range@std@@", ".?AVlogic_error@std@@", ".?AVexception@std@@"], size: 24 };
const REGEX_ERROR: Class = Class { names: &[".?AVregex_error@std@@", ".?AVruntime_error@std@@", ".?AVexception@std@@"], size: 32 };

/// ?_Xbad_alloc@std@@YAXXZ
fn x_bad_alloc(c: &mut Ctx) -> u32 {
    static W: OnceLock<u32> = OnceLock::new();
    let w = *W.get_or_init(|| static_str("bad allocation"));
    throw(c, &BAD_ALLOC, w, 0)
}
/// ?_Xlength_error@std@@YAXPEBD@Z (message)
fn x_length_error(c: &mut Ctx) -> u32 {
    let w = c.a(0);
    throw(c, &LENGTH_ERROR, w, 0)
}
/// ?_Xout_of_range@std@@YAXPEBD@Z (message)
fn x_out_of_range(c: &mut Ctx) -> u32 {
    let w = c.a(0);
    throw(c, &OUT_OF_RANGE, w, 0)
}
/// ?_Xregex_error@std@@YAXW4error_type@regex_constants@1@@Z (error_type): the messages of
/// MSVC's regex_error
fn x_regex_error(c: &mut Ctx) -> u32 {
    const MSGS: [&str; 15] = [
        "regex_error(error_collate): The expression contained an invalid collating element name.",
        "regex_error(error_ctype): The expression contained an invalid character class name.",
        "regex_error(error_escape): The expression contained an invalid escaped character, or a trailing escape.",
        "regex_error(error_backref): The expression contained an invalid back reference.",
        "regex_error(error_brack): The expression contained mismatched [ and ].",
        "regex_error(error_paren): The expression contained mismatched ( and ).",
        "regex_error(error_brace): The expression contained mismatched { and }.",
        "regex_error(error_badbrace): The expression contained an invalid range in a { expression }.",
        "regex_error(error_range): The expression contained an invalid character range, such as [b-a] in most encodings.",
        "regex_error(error_space): There was insufficient memory to convert the expression into a finite state machine.",
        "regex_error(error_badrepeat): One of *?+{ was not preceded by a valid regular expression.",
        "regex_error(error_complexity): The complexity of an attempted match against a regular expression exceeded a pre-set level.",
        "regex_error(error_stack): There was insufficient memory to determine whether the regular expression could match the specified character sequence.",
        "regex_error(error_parse)",
        "regex_error(error_syntax)",
    ];
    static W: OnceLock<Vec<u32>> = OnceLock::new();
    let ws = W.get_or_init(|| MSGS.iter().map(|m| static_str(m)).collect());
    let e = c.a(0);
    let w = *ws.get(e as usize).unwrap_or(&ws[14]);
    throw(c, &REGEX_ERROR, w, e)
}

/// ?uncaught_exceptions@std@@YAHXZ: exceptions thrown and not yet caught (none: the dispatch
/// runs to its catch before the program goes on)
fn uncaught(_: &mut Ctx) -> u32 {
    0
}

pub fn init() {
    if !mem::x64() {
        return;
    }
    crate::msvcp_thr::init();
    crate::msvcp_io::init();
    crate::msvcp_loc::init();
    for (name, f) in [
        ("??0_Lockit@std@@QEAA@H@Z", lockit_ctor as Handler),
        ("??1_Lockit@std@@QEAA@XZ", nothing),
        ("??0_Locinfo@std@@QEAA@PEBD@Z", locinfo_ctor),
        ("??1_Locinfo@std@@QEAA@XZ", nothing),
        ("?_Getcoll@_Locinfo@std@@QEBA?AU_Collvec@@XZ", locinfo_getcoll),
        ("??0facet@locale@std@@IEAA@_K@Z", facet_ctor),
        ("??1facet@locale@std@@MEAA@XZ", nothing),
        ("?_Incref@facet@locale@std@@UEAAXXZ", facet_incref),
        ("?_Decref@facet@locale@std@@UEAAPEAV_Facet_base@3@XZ", facet_decref),
        ("??Bid@locale@std@@QEAA_KXZ", locale_id),
        ("?_Init@locale@std@@CAPEAV_Locimp@12@_N@Z", locale_init),
        ("?_Getgloballocale@locale@std@@CAPEAV_Locimp@12@XZ", locale_global),
        ("?_Getcat@?$ctype@D@std@@SA_KPEAPEBVfacet@locale@2@PEBV42@@Z", ctype_getcat),
        ("?tolower@?$ctype@D@std@@QEBADD@Z", ctype_tolower),
        ("?tolower@?$ctype@D@std@@QEBAPEBDPEADPEBD@Z", ctype_tolower_range),
        ("_Strcoll", _Strcoll),
        ("_Strxfrm", _Strxfrm),
        ("?_Xbad_alloc@std@@YAXXZ", x_bad_alloc),
        ("?_Xlength_error@std@@YAXPEBD@Z", x_length_error),
        ("?_Xout_of_range@std@@YAXPEBD@Z", x_out_of_range),
        ("?_Xregex_error@std@@YAXW4error_type@regex_constants@1@@Z", x_regex_error),
        ("?_Xinvalid_argument@std@@YAXPEBD@Z", x_invalid_argument),
        ("?_Xruntime_error@std@@YAXPEBD@Z", x_runtime_error),
        ("?_Xoverflow_error@std@@YAXPEBD@Z", x_overflow_error),
        ("?_Xbad_function_call@std@@YAXXZ", x_bad_function_call),
        ("?uncaught_exceptions@std@@YAHXZ", uncaught),
        ("?uncaught_exception@std@@YA_NXZ", uncaught),
        ("?_Facet_Register@std@@YAXPEAV_Facet_base@1@@Z", nothing),
    ] {
        implement("msvcp140", name, f);
    }
}
