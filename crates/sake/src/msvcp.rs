//! msvcp140.dll, the parts of the C++ standard library programs call out of line: the locale
//! machinery std::regex and the streams use (locale::_Init, locale::id, facet reference counts,
//! ctype<char>, _Locinfo, the "C" locale's _Strcoll / _Strxfrm) and the _X* functions that
//! throw the standard exceptions. Layouts are MSVC's (<xlocale>, <xlocinfo>, <exception>) with
//! pointer-sized fields; the thrown exceptions' ThrowInfo is the RVA form with base 0 (absolute
//! addresses), which is also x86's pointer form.
//!
//! The functions are written for x64 (`this` and the arguments as c.a(i)) and serve 32-bit
//! programs too (issue #150): there the thiscall members get `this` from ECX as argument 0
//! (api::Ctx::this), the 64-bit integers take two stack slots (Ctx::q), calls back into the
//! program's virtual functions are thiscall (vcall), and each x64 name is registered under its
//! x86 spelling (x86_names) in msvcp140 and msvcp120 (VS 2013: the same layouts for what is
//! here, Wine's msvcp90.h), where those DLLs export it (specs.txt, from Wine's spec files).

#![allow(non_snake_case)]

use std::sync::OnceLock;

use crate::api::{host_fn_this, implement, Ctx, Handler};
use crate::mem::{self, cstr, ps, r8, rp, w32, w8, wp};

/// _X_CTYPE (LC_CTYPE): the category ctype<char>::_Getcat gives
pub(crate) const X_CTYPE: u32 = 2;

fn align(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}

// ---------------------------------------------------------------------------------------------
// the two ABIs

/// A table of traps (a vftable): each function with its x86 stack argument bytes (thiscall:
/// `this` in ECX, the callee pops the rest).
pub(crate) fn table(name: &str, fs: &[(Handler, u32)]) -> u32 {
    let t = mem::heap_alloc(ps() * fs.len() as u32, true);
    for (i, (f, bytes)) in fs.iter().enumerate() {
        wq(t + ps() * i as u32, host_fn_this(&format!("msvcp::{name}::{i}"), *f, *bytes));
    }
    t
}

/// Stores pointer `v` (x64: widened, as the program sees an address in an image above 4G).
pub(crate) fn wq(a: u32, v: u32) {
    if mem::x64() {
        mem::w64(a, mem::widen(v));
    } else {
        w32(a, v);
    }
}
/// Stores a size_t (x86: its low 32 bits).
pub(crate) fn wz(a: u32, v: u64) {
    if mem::x64() {
        mem::w64(a, v);
    } else {
        w32(a, v as u32);
    }
}
/// Copies the pointer-sized field at `src` to `dst` (all of it).
pub(crate) fn cpp(dst: u32, src: u32) {
    mem::copy(dst, src, ps());
}

/// An argument of a call into the program: a pointer (x64: widened), a 32-bit value, or a
/// 64-bit integer (x86: two stack slots).
#[derive(Clone, Copy)]
pub(crate) enum A {
    P(u32),
    I(u32),
    Q(u64),
    /// an object of so many bytes passed by value: x64 its address (a temporary copy of the
    /// caller's: the callee may change it), x86 its bytes on the stack
    V(u32, u32),
}
/// The x64 name `x64` as the program spells it (x86: one of its x86 names).
pub(crate) fn is_name(name: &str, x64: &str) -> bool {
    if mem::x64() { name == x64 } else { x86_names(x64).iter().any(|n| n == name) }
}

/// Calls the program's function `f` (cdecl, or x64's one convention) with `args`; EDX:EAX.
pub(crate) fn gcall(c: &mut Ctx, f: u32, args: &[A]) -> u64 {
    // (nothing more runs once a throw left the trap)
    if c.jumped {
        return 0;
    }
    if mem::x64() {
        return crate::call64(c.cpu, mem::widen(f), &wide_args(args));
    }
    let r = c.call(f, &slots(args));
    r as u64 | (c.cpu.regs[koji::EDX] as u64) << 32
}
/// Calls virtual function `slot` of `obj` (x86: thiscall); EDX:EAX (x64: RAX).
pub(crate) fn vcall(c: &mut Ctx, obj: u32, slot: u32, args: &[A]) -> u64 {
    let f = rp(rp(obj) + ps() * slot);
    tcall(c, f, obj, args)
}
/// Calls member function `f` of `obj` (x86: thiscall).
pub(crate) fn tcall(c: &mut Ctx, f: u32, obj: u32, args: &[A]) -> u64 {
    if c.jumped {
        return 0;
    }
    if mem::x64() {
        let mut a = vec![mem::widen(obj)];
        a.extend(wide_args(args));
        return crate::call64(c.cpu, mem::widen(f), &a);
    }
    crate::call_thiscall(c.cpu, f, obj, &slots(args))
}
fn wide_args(args: &[A]) -> Vec<u64> {
    args.iter().map(|a| match *a {
        A::P(p) => mem::widen(p),
        A::I(v) => v as u64,
        A::Q(v) => v,
        A::V(p, _) => mem::widen(p),
    }).collect()
}
fn slots(args: &[A]) -> Vec<u32> {
    let mut v = Vec::with_capacity(args.len() + 2);
    for a in args {
        match *a {
            A::P(x) | A::I(x) => v.push(x),
            A::Q(q) => {
                v.push(q as u32);
                v.push((q >> 32) as u32);
            }
            A::V(p, n) => v.extend((0..n.div_ceil(4)).map(|i| mem::r32(p + 4 * i))),
        }
    }
    v
}
/// The argument index after a 64-bit integer argument at `i` (x86: two slots).
pub(crate) fn after_q(i: u32) -> u32 {
    if mem::x64() { i + 1 } else { i + 2 }
}

/// The x86 spellings an x64 name of msvcp140 can have (`_K`, size_t on x64, is `I` on x86 or
/// stays unsigned __int64): the member function's `<access>E<cv>A` (__ptr64 this, cdecl) is
/// `<access><cv>E` (thiscall; or `<access><cv>A`, the few __cdecl members like num_put::_Iput),
/// the pointers and references lose their __ptr64 (`PEA` `PA`, `AEB` `AB`, `$$QEA` `$$QA`), a
/// pointer variable's trailing `EA` is `A`. Not derivable (and not implemented): a `_K` that
/// was a back reference on x64 (Concurrency::_Concurrent_vector_base_v4), and callbacks that
/// are __stdcall on x86 (_Execute_once).
pub fn x86_names(n: &str) -> Vec<String> {
    let b = n.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    // (where the member's calling convention is)
    let mut member = None;
    let mut i = 0;
    while i < b.len() {
        if member.is_none() && b[i..].starts_with(b"@@") && i + 5 < b.len() && b[i + 2].is_ascii_uppercase() && b[i + 3] == b'E' && matches!(b[i + 4], b'A' | b'B') && b[i + 5] == b'A' {
            out.extend_from_slice(&[b'@', b'@', b[i + 2], b[i + 4], b'E']);
            member = Some(out.len() - 1);
            i += 6;
            continue;
        }
        if matches!(b[i], b'P' | b'Q' | b'A' | b'R' | b'S') && i + 2 < b.len() && b[i + 1] == b'E' && matches!(b[i + 2], b'A'..=b'D') {
            out.push(b[i]);
            i += 2;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    // (a variable: its type's trailing __ptr64 `E`; a function ends in `Z`)
    if !out.ends_with(b"Z") && out.len() >= 2 && out[out.len() - 2] == b'E' && matches!(out[out.len() - 1], b'A' | b'B') {
        out.remove(out.len() - 2);
    }
    let s = String::from_utf8(out).unwrap();
    // (then the __cdecl spellings of a member: its `this` is stack argument 0)
    let mut bases = vec![s.clone()];
    if let Some(at) = member {
        bases.push(format!("{}A{}", &s[..at], &s[at + 1..]));
    }
    bases.iter().flat_map(|s| k_choices(s)).collect()
}
/// Every choice of `I` / `_K` for the `_K`s of `s`, the ones keeping more `_K`s first (an
/// operator<<(unsigned __int64) is `_K` on x86 too, beside the `I` of operator<<(unsigned)).
fn k_choices(s: &str) -> Vec<String> {
    let parts: Vec<&str> = s.split("_K").collect();
    let mut v = vec![(0, parts[0].to_string())];
    for p in &parts[1..] {
        v = v.into_iter().flat_map(|(k, x)| [(k, format!("{x}I{p}")), (k + 1, format!("{x}_K{p}"))]).collect();
    }
    v.sort_by_key(|x| std::cmp::Reverse(x.0));
    v.into_iter().map(|x| x.1).collect()
}

/// The x64 names reg_in found no x86 export for (tests: nothing the game libraries need).
#[cfg(test)]
pub(crate) static MISSED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Registers an x64 msvcp140 function (x86: under its x86 names that msvcp140 exports, and
/// msvcp120 too with `old`).
pub(crate) fn reg_in(old: bool, name: &str, f: Handler) {
    if mem::x64() {
        implement("msvcp140", name, f);
        return;
    }
    let names = x86_names(name);
    let mut done = false;
    for dll in if old { &["msvcp140", "msvcp120"][..] } else { &["msvcp140"][..] } {
        if let Some(n) = names.iter().find(|n| crate::api::exports(dll, n)) {
            implement(dll, n, f);
            done = true;
        }
    }
    // (a C or global cdecl function Wine's spec lacks: one spelling, nothing to pop)
    if !done && names.len() == 1 && (!name.starts_with('?') || name.contains("@@YA") || name.contains("@@SA") || name.contains("@@CA")) {
        implement("msvcp140", &names[0], f);
        return;
    }
    #[cfg(test)]
    if !done {
        MISSED.lock().unwrap().push(name.to_string());
    }
}
/// reg_in for msvcp140 and msvcp120
pub(crate) fn reg(name: &str, f: Handler) {
    reg_in(true, name, f);
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
    *V.get_or_init(|| table("facet", &[(facet_delete, 4), (facet_incref, 0), (facet_decref, 0)]))
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
/// A locale object is two pointers (x64 16 bytes, x86 8): its two empty bases take the first
/// (MSVC lays a second empty base out at offset 1), _Ptr is the second.
pub fn locale_size() -> u32 {
    2 * ps()
}
#[allow(non_snake_case)]
fn LOCALE_PTR() -> u32 {
    ps()
}

/// ?classic@locale@std@@SAAEBV12@XZ: the "C" locale object (static, never destroyed)
fn locale_classic(_c: &mut Ctx) -> u32 {
    static L: OnceLock<u32> = OnceLock::new();
    *L.get_or_init(new_locale)
}

/// A new locale object (locale(): the global locale, referenced).
pub fn new_locale() -> u32 {
    let p = mem::heap_alloc(locale_size(), true);
    let l = locimp();
    wp(refs(l), rp(refs(l)).wrapping_add(1));
    wp(p + LOCALE_PTR(), l);
    p
}
/// facet::_Incref / _Decref through the _Locimp's vftable
fn locimp_incref(c: &mut Ctx, l: u32) {
    if l != 0 {
        vcall(c, l, 1, &[]);
    }
}
fn locimp_release(c: &mut Ctx, l: u32) {
    if l == 0 {
        return;
    }
    let del = vcall(c, l, 2, &[]) as u32;
    if del != 0 {
        vcall(c, del, 0, &[A::I(1)]);
    }
}
/// ~locale of the locale object at `p` (its storage stays)
pub fn delete_locale_value(c: &mut Ctx, p: u32) {
    let l = rp(p + LOCALE_PTR());
    locimp_release(c, l);
    wp(p + LOCALE_PTR(), 0);
}
/// delete of a locale object made with new
pub fn delete_locale(c: &mut Ctx, p: u32) {
    delete_locale_value(c, p);
    mem::heap_free(p);
}
/// locale(const locale&) at `dst` from the locale object at `src`
pub fn locale_copy(c: &mut Ctx, dst: u32, src: u32) {
    let l = rp(src + LOCALE_PTR());
    wq(dst, 0);
    wq(dst + LOCALE_PTR(), l);
    locimp_incref(c, l);
}
/// locale::operator=
pub fn locale_assign(c: &mut Ctx, dst: u32, src: u32) {
    let (a, b) = (rp(dst + LOCALE_PTR()), rp(src + LOCALE_PTR()));
    if a != b {
        locimp_release(c, a);
        wq(dst + LOCALE_PTR(), b);
        locimp_incref(c, b);
    }
}

/// ?_Getgloballocale@locale@std@@CAPEAV_Locimp@12@XZ
fn locale_global(_: &mut Ctx) -> u32 {
    locimp()
}
/// ?_Id_cnt@id@locale@std@@0HA: the int the ids are numbered from. Microsoft's headers inline
/// locale::id's conversion (`if (_Id == 0) _Id = ++_Id_cnt;` under a _Lockit), so a program
/// increments it itself: a data export (it was a trap address, written to by the program;
/// msvcstream crashed reading it where that page is not memory: sake-grow).
fn id_cnt() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| mem::heap_alloc(4, true))
}
/// ??Bid@locale@std@@QEAA_KXZ: the id's index, given on first use (from _Id_cnt, as the
/// headers' inline copy does)
fn locale_id(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    if rp(this) == 0 {
        let n = mem::r32(id_cnt()).wrapping_add(1);
        w32(id_cnt(), n);
        wp(this, n);
    }
    rp(this)
}
/// The constants: std::_BADOFF (streamoff -1) and the <ymath.h> _Dconst bit patterns (msvcp120:
/// _Inf, _Nan...; long double is double), each in 16 zeroed bytes as the _Dconst union.
fn constant(name: &str) -> Option<u32> {
    let bits: u64 = match name {
        "?_BADOFF@std@@3_JB" => u64::MAX,
        "_FDenorm" | "_Denorm" | "_LDenorm" => 1,
        "_FInf" => 0x7f80_0000,
        "_Inf" | "_LInf" | "_Hugeval" => 0x7ff0 << 48,
        "_FNan" => 0x7fc0_0000,
        "_Nan" | "_LNan" => 0x7ff8 << 48,
        "_FSnan" => 0x7f80_0001,
        "_Snan" | "_LSnan" => (0x7ff0 << 48) + 1,
        "_FEps" => f32::EPSILON.to_bits() as u64,
        "_Eps" | "_LEps" => f64::EPSILON.to_bits(),
        "_LZero" | "_FZero" | "_Zero" => 0,
        _ => return None,
    };
    static CELLS: std::sync::Mutex<Vec<(String, u32)>> = std::sync::Mutex::new(Vec::new());
    let mut g = CELLS.lock().unwrap();
    if let Some(&(_, a)) = g.iter().find(|(n, _)| n == name) {
        return Some(a);
    }
    let a = mem::heap_alloc(16, true);
    mem::w64(a, bits);
    g.push((name.to_string(), a));
    Some(a)
}
/// The locale::id data exports (one zero word each, numbered by locale_id).
pub fn data(name: &str) -> Option<u32> {
    if let Some(a) = crate::msvcp_io::data(name) {
        return Some(a);
    }
    if name == "?_Id_cnt@id@locale@std@@0HA" {
        return Some(id_cnt());
    }
    if let Some(a) = constant(name) {
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
            &[
                (facet_delete, 4),
                (facet_incref, 0),
                (facet_decref, 0),
                (ct_tolower_range, 8),
                (ct_tolower, 4),
                (ct_toupper_range, 8),
                (ct_toupper, 4),
                (ct_widen_range, 12),
                (ct_widen, 4),
                (ct_narrow_range, 16),
                (ct_narrow, 8),
            ],
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
/// ctype<char>::is(mask, c): the classification table's bits for `c` (no virtual call: MSVC's
/// header inlines it the same way)
fn ctype_is(c: &mut Ctx) -> u32 {
    let (this, mask, ch) = (c.a(0), c.a(1) & 0xffff, c.a(2) & 0xff);
    let table = rp(this + 3 * ps());
    (table != 0 && mem::r16(table + 2 * ch) as u32 & mask != 0) as u32
}
/// _Yarn<char>::c_str(): { char* _Myptr, char _Nul }: the string, or "" (the _Nul) for none
fn yarn_c_str(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let p = rp(this);
    if p != 0 { p } else { this + ps() }
}
/// ctype<char>::tolower(c) / tolower(first, last): the virtual do_tolower
fn ctype_tolower(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 4, &[A::I(c.a(1))]) as u32 & 0xff
}
fn ctype_tolower_range(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 3, &[A::P(c.a(1)), A::P(c.a(2))]) as u32
}

fn ctype_toupper(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 6, &[A::I(c.a(1))]) as u32 & 0xff
}
fn ctype_toupper_range(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 5, &[A::P(c.a(1)), A::P(c.a(2))]) as u32
}
fn ctype_widen(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 8, &[A::I(c.a(1))]) as u32 & 0xff
}
fn ctype_widen_range(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 7, &[A::P(c.a(1)), A::P(c.a(2)), A::P(c.a(3))]) as u32
}
fn ctype_narrow(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 10, &[A::I(c.a(1)), A::I(c.a(2))]) as u32 & 0xff
}
fn ctype_narrow_range(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    vcall(c, this, 9, &[A::P(c.a(1)), A::P(c.a(2)), A::I(c.a(3)), A::P(c.a(4))]) as u32
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
/// ThrowInfo's destructor of an exception sake threw (`throw`, `throw_failure`,
/// `throw_system_error`): the message, then the block.
/// An exception_ptr still holding it (`hold_thrown`) puts that off to its last release.
pub(crate) fn thrown_dtor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let free = {
        let mut g = THROWN.lock().unwrap_or_else(|e| e.into_inner());
        let t = g.get_or_insert_with(Default::default);
        match t.get_mut(&this) {
            Some(h) if h.holds > 0 => {
                h.dead = true;
                false
            }
            Some(_) => {
                t.remove(&this);
                true
            }
            // (not one of sake's blocks: only its message)
            None => {
                ex_free_what(this);
                false
            }
        }
    };
    if free {
        free_thrown(this);
    }
    0
}
fn free_thrown(o: u32) {
    ex_free_what(o);
    if mem::heap_owns(o) {
        mem::heap_free(o);
    }
}
/// A block sake threw: how many exception_ptrs hold it, whether its catch is done with it.
#[derive(Default)]
struct Thrown {
    holds: u32,
    dead: bool,
}
/// The exception objects sake threw that are not freed yet (exception_ptr keeps the thrown
/// object itself, not a copy: the catch's end must not free it under one).
static THROWN: std::sync::Mutex<Option<std::collections::HashMap<u32, Thrown>>> = std::sync::Mutex::new(None);
/// `o`, a block of sake's to throw
pub(crate) fn thrown(o: u32) -> u32 {
    THROWN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(o, Thrown::default());
    o
}
/// An exception_ptr holds `obj` (no-op unless sake threw it).
pub(crate) fn hold_thrown(obj: u64) {
    let mut t = THROWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(h) = t.as_mut().and_then(|t| t.get_mut(&(obj as u32))) {
        h.holds += 1;
    }
}
/// An exception_ptr lets `obj` go: freed when it was the last and the catch is done.
pub(crate) fn release_thrown(obj: u64) {
    let o = obj as u32;
    let free = {
        let mut t = THROWN.lock().unwrap_or_else(|e| e.into_inner());
        let Some(m) = t.as_mut() else { return };
        match m.get_mut(&o) {
            Some(h) => {
                h.holds = h.holds.saturating_sub(1);
                let gone = h.holds == 0 && h.dead;
                if gone {
                    m.remove(&o);
                }
                gone
            }
            None => false,
        }
    };
    if free {
        free_thrown(o);
    }
}
/// Whether sake's thrown block `o` is not freed yet
#[cfg(test)]
pub(crate) fn thrown_live(o: u32) -> bool {
    THROWN.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|m| m.contains_key(&o))
}
fn ex_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| table("exception", &[(ex_delete, 4), (ex_what, 0)]))
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
    // (x86: thiscall, the copy constructor's source on the stack)
    // (the object thrown is sake's block: its destructor frees it too, when the catch is done
    // with it, as the thrower's stack frame would go on Windows; it stayed, a block a throw)
    let dtor = host_fn_this("msvcp::exception::thrown_dtor", thrown_dtor, 0);
    let copy = if class.size > 3 * p { host_fn_this("msvcp::exception::copy32", copy32, 4) } else { host_fn_this("msvcp::exception::copy24", copy24, 4) };
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
    thrown(o);
    wp(o, ex_vtable());
    wp(o + ps(), what);
    if class.size > 3 * ps() {
        w32(o + 3 * ps(), extra);
    }
    throw_object(c, o, throw_info(class));
    0
}
/// Throws `o` (ThrowInfo `ti`) from this trap.
pub(crate) fn throw_object(c: &mut Ctx, o: u32, ti: u32) {
    if mem::x64() {
        crate::cxx64::throw_object(c, o as u64, ti);
    } else {
        crate::cxx::throw_object(c, o, ti);
    }
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

/// error_category's virtual functions: deleting destructor, name, message(result, int),
/// default_error_condition(result, int), equivalent(int, const error_condition&),
/// equivalent(const error_code&, int)
const CATEGORY: [(Handler, u32); 6] = [(nothing, 4), (nothing, 0), (nothing, 8), (nothing, 8), (nothing, 8), (nothing, 8)];

/// generic_category() (an error_category: vtable, _Addr): what system_error's error_code points
/// to; its virtual functions (name, message, ...) are not called by the programs here
fn generic_category() -> u32 {
    static C: OnceLock<u32> = OnceLock::new();
    *C.get_or_init(|| {
        let p = mem::heap_alloc(2 * ps(), true);
        wp(p, table("error_category", &CATEGORY));
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
        wp(p, table("iostream_category", &CATEGORY));
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
    thrown(o);
    wp(o, ex_vtable());
    wp(o + ps(), static_str(&format!("{msg}: iostream stream error")));
    // (do_free: the message goes with the object)
    mem::w8(o + 2 * ps(), 1);
    w32(o + 3 * ps(), 1);
    wp(o + 4 * ps(), iostream_category());
    throw_object(c, o, throw_info(&FAILURE));
}
/// throw; (the exception being handled)
pub fn rethrow(c: &mut Ctx) {
    if mem::x64() {
        crate::cxx64::rethrow(c);
    } else {
        crate::cxx::rethrow(c);
    }
}

pub fn throw_system_error(c: &mut Ctx, errno: u32, what: u32) -> u32 {
    let o = mem::heap_alloc(SYSTEM_ERROR.size, true);
    thrown(o);
    wp(o, ex_vtable());
    wp(o + ps(), what);
    w32(o + 3 * ps(), errno);
    wp(o + 4 * ps(), generic_category());
    throw_object(c, o, throw_info(&SYSTEM_ERROR));
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
    // (one message for every throw: a new one a throw stayed, do_free unset)
    static W: OnceLock<u32> = OnceLock::new();
    let w = *W.get_or_init(|| static_str("bad function call"));
    throw(c, &BAD_FUNCTION_CALL, w, 0)
}

const BAD_ALLOC: Class = Class { names: &[".?AVbad_alloc@std@@", ".?AVexception@std@@"], size: 24 };
const LENGTH_ERROR: Class = Class { names: &[".?AVlength_error@std@@", ".?AVlogic_error@std@@", ".?AVexception@std@@"], size: 24 };
const OUT_OF_RANGE: Class = Class { names: &[".?AVout_of_range@std@@", ".?AVlogic_error@std@@", ".?AVexception@std@@"], size: 24 };
const REGEX_ERROR: Class = Class { names: &[".?AVregex_error@std@@", ".?AVruntime_error@std@@", ".?AVexception@std@@"], size: 32 };

const BAD_CAST: Class = Class { names: &[".?AVbad_cast@std@@", ".?AVexception@std@@"], size: 24 };
/// Throws std::bad_cast("Bad dynamic_cast!") from this trap: a failed dynamic_cast of a reference
pub fn throw_bad_cast(c: &mut Ctx) -> u32 {
    static W: OnceLock<u32> = OnceLock::new();
    let w = *W.get_or_init(|| static_str("Bad dynamic_cast!"));
    throw(c, &BAD_CAST, w, 0)
}

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
    crate::msvcp_thr::init();
    crate::msvcp_fs::init();
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
        ("?classic@locale@std@@SAAEBV12@XZ", locale_classic),
        ("?_Getcat@?$ctype@D@std@@SA_KPEAPEBVfacet@locale@2@PEBV42@@Z", ctype_getcat),
        ("?tolower@?$ctype@D@std@@QEBADD@Z", ctype_tolower),
        ("?is@?$ctype@D@std@@QEBA_NFD@Z", ctype_is),
        ("?c_str@?$_Yarn@D@std@@QEBAPEBDXZ", yarn_c_str),
        ("?tolower@?$ctype@D@std@@QEBAPEBDPEADPEBD@Z", ctype_tolower_range),
        ("?toupper@?$ctype@D@std@@QEBADD@Z", ctype_toupper),
        ("?toupper@?$ctype@D@std@@QEBAPEBDPEADPEBD@Z", ctype_toupper_range),
        ("?widen@?$ctype@D@std@@QEBADD@Z", ctype_widen),
        ("?widen@?$ctype@D@std@@QEBAPEBDPEBD0PEAD@Z", ctype_widen_range),
        ("?narrow@?$ctype@D@std@@QEBADDD@Z", ctype_narrow),
        ("?narrow@?$ctype@D@std@@QEBAPEBDPEBD0DPEAD@Z", ctype_narrow_range),
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
        reg(name, f);
    }
}

#[cfg(test)]
mod thrown_tests {
    use super::*;
    use crate::test_guest::{call, enter};

    /// A std exception sake threw: its catch's end frees the message and the block (both
    /// stayed for good, a block a throw), and another object's only its message.
    #[test]
    fn the_catch_end_frees_a_thrown_block() {
        let _g = enter();
        let o = thrown(mem::heap_alloc(FAILURE.size, true));
        let what = static_str("x: iostream stream error");
        wp(o, ex_vtable());
        wp(o + ps(), what);
        w8(o + 2 * ps(), 1);
        assert!(thrown_live(o));
        call(thrown_dtor, &[o]);
        assert!(!thrown_live(o));
        assert!(!mem::heap_owns(o), "block freed");
        assert!(!mem::heap_owns(what), "message freed");
        // (the guest's own object: its message only, the block is the guest's)
        let g = mem::heap_alloc(24, true);
        let w = static_str("guest");
        wp(g + ps(), w);
        w8(g + 2 * ps(), 1);
        call(thrown_dtor, &[g]);
        assert!(mem::heap_owns(g), "not sake's to free");
        assert_eq!(rp(g + ps()), 0);
        mem::heap_free(g);
    }

    /// The ThrowInfo of every class sake throws names the destructor that frees the block.
    #[test]
    fn throw_infos_name_the_freeing_destructor() {
        let _g = enter();
        let want = host_fn_this("msvcp::exception::thrown_dtor", thrown_dtor, 0);
        for class in [&FAILURE, &SYSTEM_ERROR, &INVALID_ARGUMENT, &RUNTIME_ERROR, &OVERFLOW_ERROR, &BAD_FUNCTION_CALL] {
            assert_eq!(mem::r32(throw_info(class) + 4), want, "{:?}", class.names[0]);
        }
    }
}
