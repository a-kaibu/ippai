//! msvcp140.dll's iostreams (x64), as Microsoft's STL has them (<xiosbase>, <ios>, <streambuf>,
//! <istream>, <ostream>; src/ios.cpp, iosptrs.cpp, iostream.cpp, cout.cpp, fiopen.cpp): ios_base,
//! basic_ios, basic_streambuf, basic_istream, basic_ostream and basic_iostream of char and
//! wchar_t, the standard streams and _Fiopen. Numbers are formatted and parsed as num_put /
//! num_get do in the "C" locale (no grouping, '.').
//!
//! Layouts (MSVC x64, /vd1; what clang's MS ABI lays out from the STL headers):
//! ios_base { vftable, size_t _Stdstr, int _Mystate, _Except, _Fmtfl, streamsize _Prec, _Wide,
//! _Iosarray* _Arr, _Fnarray* _Calls, locale* _Ploc } (72), basic_ios adds _Mystrbuf, _Tiestr,
//! _Elem _Fillch (96). basic_streambuf { vftable, _Gfirst, _Pfirst, _IGfirst, _IPfirst, _Gnext,
//! _Pnext, _IGnext, _IPnext, int _Gcount, _Pcount, int* _IGcount, _IPcount, locale* _Plocale }
//! (104). basic_istream { vbptr, streamsize _Chcount, vtordisp, basic_ios at 24 },
//! basic_ostream { vbptr, vtordisp, basic_ios at 16 }, basic_iostream { istream, ostream at 16,
//! vtordisp, basic_ios at 32 }. A class with the virtual base takes a hidden "most derived" int
//! after its constructor's arguments, and its destructor (like its virtual functions) gets the
//! basic_ios part. Programs mostly inline these and carry their own vftables, so everything here
//! calls virtual functions through the object's.
//!
//! Pointers stored into objects are widened (an object or buffer can be in an image mapped
//! above 4G).

#![allow(non_snake_case)]

use std::sync::{Mutex, OnceLock};

use crate::api::{host_fn, implement, Ctx, Handler};
use crate::mem::{self, r16, r32, r64, r8, rp, w16, w32, w64, w8};

// ---------------------------------------------------------------------------------------------
// constants (<xiosbase>)

const EOFBIT: u32 = 1;
const FAILBIT: u32 = 2;
const BADBIT: u32 = 4;
const STATMASK: u32 = 0x17;

const SKIPWS: u32 = 0x1;
const UNITBUF: u32 = 0x2;
const UPPERCASE: u32 = 0x4;
const SHOWBASE: u32 = 0x8;
const SHOWPOINT: u32 = 0x10;
const SHOWPOS: u32 = 0x20;
const LEFT: u32 = 0x40;
const INTERNAL: u32 = 0x100;
const DEC: u32 = 0x200;
const OCT: u32 = 0x400;
const HEX: u32 = 0x800;
const SCIENTIFIC: u32 = 0x1000;
const FIXED: u32 = 0x2000;
const BOOLALPHA: u32 = 0x4000;
const ADJUSTFIELD: u32 = 0x1c0;
const BASEFIELD: u32 = 0xe00;
const FLOATFIELD: u32 = 0x3000;
const FMTMASK: u32 = 0xffff;

const IN: u32 = 1;
const OUT: u32 = 2;
const SEEK_CUR: u32 = 1;

// ios_base / basic_ios fields
const STDSTR: u32 = 8;
const STATE: u32 = 16;
const EXCEPT: u32 = 20;
const FMTFL: u32 = 24;
const PREC: u32 = 32;
const WIDE: u32 = 40;
const ARR: u32 = 48;
const CALLS: u32 = 56;
const PLOC: u32 = 64;
const STRBUF: u32 = 72;
const TIESTR: u32 = 80;
const FILLCH: u32 = 88;
const IOS_SIZE: u32 = 96;

// basic_streambuf fields
const GFIRST: u32 = 8;
const PFIRST: u32 = 16;
const IGFIRST: u32 = 24;
const IPFIRST: u32 = 32;
const GNEXT: u32 = 40;
const PNEXT: u32 = 48;
const IGNEXT: u32 = 56;
const IPNEXT: u32 = 64;
const GCOUNT: u32 = 72;
const PCOUNT: u32 = 76;
const IGCOUNT: u32 = 80;
const IPCOUNT: u32 = 88;
const PLOCALE: u32 = 96;
const SB_SIZE: u32 = 104;

// basic_streambuf's virtual functions
const V_LOCK: u32 = 1;
const V_UNLOCK: u32 = 2;
const V_OVERFLOW: u32 = 3;
const V_PBACKFAIL: u32 = 4;
const V_SHOWMANYC: u32 = 5;
const V_UNDERFLOW: u32 = 6;
const V_UFLOW: u32 = 7;
const V_XSGETN: u32 = 8;
const V_XSPUTN: u32 = 9;
const V_SEEKOFF: u32 = 10;
const V_SEEKPOS: u32 = 11;
const V_SETBUF: u32 = 12;
const V_SYNC: u32 = 13;
const V_IMBUE: u32 = 14;

// the virtual base's place in each class standing alone
const ISTREAM_VB: u32 = 24;
const OSTREAM_VB: u32 = 16;
const IOSTREAM_VB: u32 = 32;
const CHCOUNT: u32 = 8;

// ---------------------------------------------------------------------------------------------
// helpers

/// A pointer as the program sees it.
fn wide(a: u32) -> u64 {
    mem::widen(a)
}
/// Stores pointer `v` (widened).
fn wq(a: u32, v: u32) {
    w64(a, wide(v));
}
fn i64_(a: u32) -> i64 {
    r64(a) as i64
}

fn es<const W: bool>() -> u32 {
    if W { 2 } else { 1 }
}
fn eof<const W: bool>() -> u32 {
    if W { 0xffff } else { u32::MAX }
}
/// An int_type a function returned (wint_t: its low half).
fn meta<const W: bool>(v: u64) -> u32 {
    if W { v as u32 & 0xffff } else { v as u32 }
}
fn rd<const W: bool>(a: u32) -> u32 {
    if W { r16(a) as u32 } else { r8(a) as u32 }
}
fn wr<const W: bool>(a: u32, v: u32) {
    if W { w16(a, v as u16) } else { w8(a, v as u8) }
}
/// to_char_type (an element from an argument register)
fn ch<const W: bool>(v: u32) -> u32 {
    if W { v & 0xffff } else { v & 0xff }
}

fn call(c: &mut Ctx, f: u32, args: &[u64]) -> u64 {
    // (nothing more runs once a throw left the trap)
    if c.jumped {
        return 0;
    }
    crate::call64(c.cpu, wide(f), args)
}
/// Calls virtual function `slot` of `obj`.
pub(crate) fn vcall(c: &mut Ctx, obj: u32, slot: u32, args: &[u64]) -> u64 {
    let f = rp(rp(obj) + 8 * slot);
    let mut a = Vec::with_capacity(args.len() + 1);
    a.push(wide(obj));
    a.extend_from_slice(args);
    call(c, f, &a)
}

/// A table of traps (a vtable).
fn table(name: &str, fs: &[Handler]) -> u32 {
    let t = mem::heap_alloc(8 * fs.len() as u32, true);
    for (i, f) in fs.iter().enumerate() {
        wq(t + 8 * i as u32, host_fn(&format!("msvcp::{name}::{i}"), *f, 0));
    }
    t
}

fn static_str(s: &str) -> u32 {
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, s.as_bytes());
    p
}

// ---------------------------------------------------------------------------------------------
// ios_base

fn state(ios: u32) -> u32 {
    r32(ios + STATE)
}
fn flags(ios: u32) -> u32 {
    r32(ios + FMTFL)
}
fn good(ios: u32) -> bool {
    state(ios) == 0
}
fn fail(ios: u32) -> bool {
    state(ios) & (BADBIT | FAILBIT) != 0
}

/// ios_base::clear(state, reraise): throws ios_base::failure for a state the exception mask
/// has (reraise: rethrows the exception being handled).
fn base_clear(c: &mut Ctx, ios: u32, st: u32, reraise: bool) {
    let st = st & STATMASK;
    w32(ios + STATE, st);
    let f = st & r32(ios + EXCEPT);
    if f != 0 {
        if reraise {
            crate::msvcp::rethrow(c);
            return;
        }
        let msg = if f & BADBIT != 0 {
            "ios_base::badbit set"
        } else if f & FAILBIT != 0 {
            "ios_base::failbit set"
        } else {
            "ios_base::eofbit set"
        };
        crate::msvcp::throw_failure(c, msg);
    }
}
/// basic_ios::clear: a stream without a buffer is bad.
fn ios_clear(c: &mut Ctx, ios: u32, st: u32, reraise: bool) {
    let st = st | if rp(ios + STRBUF) != 0 { 0 } else { BADBIT };
    base_clear(c, ios, st, reraise);
}
/// ios_base::setstate (no stream buffer check)
pub(crate) fn base_setstate(c: &mut Ctx, ios: u32, st: u32) {
    base_clear(c, ios, state(ios) | st, false);
}
fn setstate(c: &mut Ctx, ios: u32, st: u32) {
    if st != 0 || r32(ios + EXCEPT) & state(ios) != 0 {
        ios_clear(c, ios, state(ios) | st, false);
    }
}

/// _Iosarray { _Next, int _Index, long _Lo, void* _Vp } / _Fnarray { _Next, int _Index, _Pfn }
fn callfns(c: &mut Ctx, ios: u32, ev: u32) {
    let mut p = rp(ios + CALLS);
    while p != 0 {
        let f = rp(p + 16);
        call(c, f, &[ev as u64, wide(ios), r32(p + 8) as u64]);
        p = rp(p);
    }
}
fn findarr(ios: u32, idx: u32) -> u32 {
    let mut spare = 0;
    let mut p = rp(ios + ARR);
    while p != 0 {
        if r32(p + 8) == idx {
            return p;
        }
        if spare == 0 && r32(p + 12) == 0 && r64(p + 16) == 0 {
            spare = p;
        }
        p = rp(p);
    }
    if spare != 0 {
        w32(spare + 8, idx);
        return spare;
    }
    let n = mem::heap_alloc(24, true);
    wq(n, rp(ios + ARR));
    w32(n + 8, idx);
    wq(ios + ARR, n);
    n
}
fn tidy(c: &mut Ctx, ios: u32) {
    callfns(c, ios, 0);
    for head in [ARR, CALLS] {
        let mut p = rp(ios + head);
        while p != 0 {
            let next = rp(p);
            mem::heap_free(p);
            p = next;
        }
        w64(ios + head, 0);
    }
}
/// ios_base::_Init
fn base_init(c: &mut Ctx, ios: u32) {
    w64(ios + PLOC, 0);
    w64(ios + STDSTR, 0);
    w32(ios + EXCEPT, 0);
    w32(ios + FMTFL, SKIPWS | DEC);
    w64(ios + PREC, 6);
    w64(ios + WIDE, 0);
    w64(ios + ARR, 0);
    w64(ios + CALLS, 0);
    base_clear(c, ios, 0, false);
    wq(ios + PLOC, crate::msvcp::new_locale());
}

/// The standard streams' slots (iosptrs.cpp: [1, 8]) and open counts.
static STDSTR_TAB: Mutex<[(u32, u8); 10]> = Mutex::new([(0, 0); 10]);

fn addstd(ios: u32) {
    let mut t = STDSTR_TAB.lock().unwrap();
    let mut i = 1;
    while i < 8 && t[i].0 != 0 && t[i].0 != ios {
        i += 1;
    }
    t[i].0 = ios;
    t[i].1 = t[i].1.wrapping_add(1);
    w64(ios + STDSTR, i as u64);
}
/// ios_base::_Ios_base_dtor
fn base_dtor(c: &mut Ctx, ios: u32) {
    let i = r64(ios + STDSTR) as usize;
    if i > 0 && i < 10 {
        let mut t = STDSTR_TAB.lock().unwrap();
        t[i].1 = t[i].1.wrapping_sub(1);
        if t[i].1 > 0 {
            return;
        }
    }
    tidy(c, ios);
    let l = rp(ios + PLOC);
    if l != 0 {
        crate::msvcp::delete_locale(c, l);
    }
}

fn ios_base_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| table("ios_base", &[ios_base_delete]))
}
fn ios_base_delete(c: &mut Ctx) -> u32 {
    let (this, fl) = (c.a(0), c.a(1));
    base_dtor(c, this);
    if fl & 1 != 0 {
        mem::heap_free(this);
    }
    this
}
/// ??0ios_base@std@@IEAA@XZ: the members zeroed
fn ios_base_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    mem::fill(this + 8, 64, 0);
    wq(this, ios_base_vtable());
    this
}
fn ios_base_dtor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    base_dtor(c, this);
    0
}
fn Ios_base_dtor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    base_dtor(c, this);
    0
}
fn Addstd(c: &mut Ctx) -> u32 {
    addstd(c.a(0));
    0
}
fn base_Init(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    base_init(c, this);
    0
}
fn base_Tidy(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    tidy(c, this);
    0
}
fn base_Callfns(c: &mut Ctx) -> u32 {
    let (this, ev) = (c.a(0), c.a(1));
    callfns(c, this, ev);
    0
}
fn base_Findarr(c: &mut Ctx) -> u32 {
    findarr(c.a(0), c.a(1))
}
fn base_bool(c: &mut Ctx) -> u32 {
    !fail(c.a(0)) as u32
}
fn base_not(c: &mut Ctx) -> u32 {
    fail(c.a(0)) as u32
}
fn base_good(c: &mut Ctx) -> u32 {
    good(c.a(0)) as u32
}
fn base_eof(c: &mut Ctx) -> u32 {
    (state(c.a(0)) & EOFBIT != 0) as u32
}
fn base_fail(c: &mut Ctx) -> u32 {
    fail(c.a(0)) as u32
}
fn base_bad(c: &mut Ctx) -> u32 {
    (state(c.a(0)) & BADBIT != 0) as u32
}
fn base_rdstate(c: &mut Ctx) -> u32 {
    state(c.a(0))
}
fn base_clear2(c: &mut Ctx) -> u32 {
    let (this, st, re) = (c.a(0), c.a(1), c.a(2) & 0xff != 0);
    base_clear(c, this, st, re);
    0
}
fn base_clear1(c: &mut Ctx) -> u32 {
    let (this, st) = (c.a(0), c.a(1));
    base_clear(c, this, st, false);
    0
}
fn base_setstate2(c: &mut Ctx) -> u32 {
    let (this, st, re) = (c.a(0), c.a(1), c.a(2) & 0xff != 0);
    base_clear(c, this, state(this) | st, re);
    0
}
fn base_setstate1(c: &mut Ctx) -> u32 {
    let (this, st) = (c.a(0), c.a(1));
    base_clear(c, this, state(this) | st, false);
    0
}
fn base_exceptions_get(c: &mut Ctx) -> u32 {
    r32(c.a(0) + EXCEPT)
}
fn base_exceptions_set(c: &mut Ctx) -> u32 {
    let (this, e) = (c.a(0), c.a(1));
    w32(this + EXCEPT, e & STATMASK);
    base_clear(c, this, state(this), false);
    0
}
fn base_flags_get(c: &mut Ctx) -> u32 {
    flags(c.a(0))
}
fn base_flags_set(c: &mut Ctx) -> u32 {
    let (this, f) = (c.a(0), c.a(1));
    let old = flags(this);
    w32(this + FMTFL, f & FMTMASK);
    old
}
fn base_setf1(c: &mut Ctx) -> u32 {
    let (this, f) = (c.a(0), c.a(1));
    let old = flags(this);
    w32(this + FMTFL, old | (f & FMTMASK));
    old
}
fn base_setf2(c: &mut Ctx) -> u32 {
    let (this, f, m) = (c.a(0), c.a(1), c.a(2));
    let old = flags(this);
    w32(this + FMTFL, (old & !m) | (f & m & FMTMASK));
    old
}
fn base_unsetf(c: &mut Ctx) -> u32 {
    let (this, m) = (c.a(0), c.a(1));
    w32(this + FMTFL, flags(this) & !m);
    0
}
fn base_precision_get(c: &mut Ctx) -> u32 {
    let v = r64(c.a(0) + PREC);
    c.ret64(v)
}
fn base_precision_set(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a64(1));
    let old = r64(this + PREC);
    w64(this + PREC, v);
    c.ret64(old)
}
fn base_width_get(c: &mut Ctx) -> u32 {
    let v = r64(c.a(0) + WIDE);
    c.ret64(v)
}
fn base_width_set(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a64(1));
    let old = r64(this + WIDE);
    w64(this + WIDE, v);
    c.ret64(old)
}
/// getloc (this, result)
fn base_getloc(c: &mut Ctx) -> u32 {
    let (this, ret) = (c.a(0), c.a(1));
    crate::msvcp::locale_copy(c, ret, rp(this + PLOC));
    ret
}
/// ios_base::imbue (this, result, locale): the old one
fn base_imbue_at(c: &mut Ctx, this: u32, ret: u32, loc: u32) {
    let l = rp(this + PLOC);
    crate::msvcp::locale_copy(c, ret, l);
    crate::msvcp::locale_assign(c, l, loc);
    callfns(c, this, 1);
}
fn base_imbue(c: &mut Ctx) -> u32 {
    let (this, ret, loc) = (c.a(0), c.a(1), c.a(2));
    base_imbue_at(c, this, ret, loc);
    ret
}
fn base_iword(c: &mut Ctx) -> u32 {
    findarr(c.a(0), c.a(1)) + 12
}
fn base_pword(c: &mut Ctx) -> u32 {
    findarr(c.a(0), c.a(1)) + 16
}
fn base_register_callback(c: &mut Ctx) -> u32 {
    let (this, f, idx) = (c.a(0), c.a(1), c.a(2));
    let n = mem::heap_alloc(24, true);
    wq(n, rp(this + CALLS));
    w32(n + 8, idx);
    wq(n + 16, f);
    wq(this + CALLS, n);
    0
}
fn copyfmt_base(c: &mut Ctx, this: u32, o: u32) {
    if this == o {
        return;
    }
    tidy(c, this);
    crate::msvcp::locale_assign(c, rp(this + PLOC), rp(o + PLOC));
    w32(this + FMTFL, flags(o));
    w64(this + PREC, r64(o + PREC));
    w64(this + WIDE, r64(o + WIDE));
    let mut p = rp(o + ARR);
    while p != 0 {
        if r32(p + 12) != 0 || r64(p + 16) != 0 {
            let n = findarr(this, r32(p + 8));
            w32(n + 12, r32(p + 12));
            w64(n + 16, r64(p + 16));
        }
        p = rp(p);
    }
    let mut p = rp(o + CALLS);
    while p != 0 {
        let n = mem::heap_alloc(24, true);
        wq(n, rp(this + CALLS));
        w32(n + 8, r32(p + 8));
        w64(n + 16, r64(p + 16));
        wq(this + CALLS, n);
        p = rp(p);
    }
    callfns(c, this, 2);
    w32(this + EXCEPT, r32(o + EXCEPT) & STATMASK);
    base_clear(c, this, state(this), false);
}
fn base_copyfmt(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    copyfmt_base(c, this, o);
    this
}
fn base_swap_at(this: u32, o: u32) {
    if this == o {
        return;
    }
    for (off, n) in [(STATE, 4), (EXCEPT, 4), (FMTFL, 4), (PREC, 8), (WIDE, 8), (ARR, 8), (CALLS, 8), (PLOC, 8)] {
        let a = mem::slice(this + off, n).to_vec();
        mem::copy(this + off, o + off, n);
        mem::write(o + off, &a);
    }
}
fn base_swap(c: &mut Ctx) -> u32 {
    base_swap_at(c.a(0), c.a(1));
    0
}

static INDEX: Mutex<u32> = Mutex::new(0);
static SYNC: Mutex<bool> = Mutex::new(true);
static INIT_CNT: OnceLock<u32> = OnceLock::new();

fn base_xalloc(_: &mut Ctx) -> u32 {
    let mut g = INDEX.lock().unwrap();
    let i = *g;
    *g += 1;
    i
}
fn base_sync_with_stdio(c: &mut Ctx) -> u32 {
    let mut g = SYNC.lock().unwrap();
    let old = *g;
    *g = c.a(0) & 0xff != 0;
    old as u32
}
fn init_cnt() -> u32 {
    *INIT_CNT.get_or_init(|| {
        let p = mem::heap_alloc(4, true);
        w32(p, u32::MAX);
        p
    })
}
fn Init_ctor(_: &mut Ctx) -> u32 {
    let p = init_cnt();
    let n = r32(p) as i32;
    w32(p, if n >= 0 { n as u32 + 1 } else { 1 });
    0
}
fn Init_dtor(c: &mut Ctx) -> u32 {
    let p = init_cnt();
    let n = r32(p).wrapping_sub(1);
    w32(p, n);
    if n == 0 {
        for s in [std_stream(1), std_stream(3), std_stream(0)] {
            flush_at::<false>(c, s);
        }
    }
    0
}
fn Init_ctor_obj(c: &mut Ctx) -> u32 {
    Init_ctor(c);
    c.a(0)
}
fn Init_dtor_obj(c: &mut Ctx) -> u32 {
    Init_dtor(c);
    0
}
fn Init_cnt_func(_: &mut Ctx) -> u32 {
    init_cnt()
}

// ---------------------------------------------------------------------------------------------
// basic_streambuf

fn gptr(sb: u32) -> u32 {
    rp(rp(sb + IGNEXT))
}
fn eback(sb: u32) -> u32 {
    rp(rp(sb + IGFIRST))
}
fn gcount(sb: u32) -> i32 {
    r32(rp(sb + IGCOUNT)) as i32
}
fn pptr(sb: u32) -> u32 {
    rp(rp(sb + IPNEXT))
}
fn pbase(sb: u32) -> u32 {
    rp(rp(sb + IPFIRST))
}
fn pcount(sb: u32) -> i32 {
    r32(rp(sb + IPCOUNT)) as i32
}
fn egptr<const W: bool>(sb: u32) -> u32 {
    gptr(sb).wrapping_add((gcount(sb) as u32).wrapping_mul(es::<W>()))
}
fn epptr<const W: bool>(sb: u32) -> u32 {
    pptr(sb).wrapping_add((pcount(sb) as u32).wrapping_mul(es::<W>()))
}
fn gnavail(sb: u32) -> i64 {
    if gptr(sb) != 0 { gcount(sb) as i64 } else { 0 }
}
fn pnavail(sb: u32) -> i64 {
    if pptr(sb) != 0 { pcount(sb) as i64 } else { 0 }
}
fn gbump<const W: bool>(sb: u32, n: i32) {
    let gc = rp(sb + IGCOUNT);
    w32(gc, (r32(gc) as i32).wrapping_sub(n) as u32);
    let gn = rp(sb + IGNEXT);
    wq(gn, rp(gn).wrapping_add((n as u32).wrapping_mul(es::<W>())));
}
fn pbump<const W: bool>(sb: u32, n: i32) {
    let pc = rp(sb + IPCOUNT);
    w32(pc, (r32(pc) as i32).wrapping_sub(n) as u32);
    let pn = rp(sb + IPNEXT);
    wq(pn, rp(pn).wrapping_add((n as u32).wrapping_mul(es::<W>())));
}
fn setg<const W: bool>(sb: u32, first: u32, next: u32, last: u32) {
    wq(rp(sb + IGFIRST), first);
    wq(rp(sb + IGNEXT), next);
    w32(rp(sb + IGCOUNT), (last.wrapping_sub(next) as i32 / es::<W>() as i32) as u32);
}
fn setp<const W: bool>(sb: u32, first: u32, next: u32, last: u32) {
    wq(rp(sb + IPFIRST), first);
    wq(rp(sb + IPNEXT), next);
    w32(rp(sb + IPCOUNT), (last.wrapping_sub(next) as i32 / es::<W>() as i32) as u32);
}
/// _Init(): the buffer pointers point at the object's own fields, no buffers
fn sb_init<const W: bool>(sb: u32) {
    wq(sb + IGFIRST, sb + GFIRST);
    wq(sb + IPFIRST, sb + PFIRST);
    wq(sb + IGNEXT, sb + GNEXT);
    wq(sb + IPNEXT, sb + PNEXT);
    wq(sb + IGCOUNT, sb + GCOUNT);
    wq(sb + IPCOUNT, sb + PCOUNT);
    setp::<W>(sb, 0, 0, 0);
    setg::<W>(sb, 0, 0, 0);
}

fn sgetc<const W: bool>(c: &mut Ctx, sb: u32) -> u32 {
    if gnavail(sb) > 0 { rd::<W>(gptr(sb)) } else { meta::<W>(vcall(c, sb, V_UNDERFLOW, &[])) }
}
fn sbumpc<const W: bool>(c: &mut Ctx, sb: u32) -> u32 {
    if gnavail(sb) > 0 {
        let v = rd::<W>(gptr(sb));
        gbump::<W>(sb, 1);
        v
    } else {
        meta::<W>(vcall(c, sb, V_UFLOW, &[]))
    }
}
fn snextc<const W: bool>(c: &mut Ctx, sb: u32) -> u32 {
    if gnavail(sb) > 1 {
        gbump::<W>(sb, 1);
        rd::<W>(gptr(sb))
    } else if sbumpc::<W>(c, sb) == eof::<W>() {
        eof::<W>()
    } else {
        sgetc::<W>(c, sb)
    }
}
pub(crate) fn sputc<const W: bool>(c: &mut Ctx, sb: u32, v: u32) -> u32 {
    if pnavail(sb) > 0 {
        wr::<W>(pptr(sb), v);
        pbump::<W>(sb, 1);
        v
    } else {
        meta::<W>(vcall(c, sb, V_OVERFLOW, &[v as u64]))
    }
}
fn sputn(c: &mut Ctx, sb: u32, p: u32, n: i64) -> i64 {
    vcall(c, sb, V_XSPUTN, &[wide(p), n as u64]) as i64
}
fn sgetn(c: &mut Ctx, sb: u32, p: u32, n: i64) -> i64 {
    vcall(c, sb, V_XSGETN, &[wide(p), n as u64]) as i64
}
fn pubsync(c: &mut Ctx, sb: u32) -> i32 {
    vcall(c, sb, V_SYNC, &[]) as i32
}
fn sputbackc<const W: bool>(c: &mut Ctx, sb: u32, v: u32) -> u32 {
    let g = gptr(sb);
    if g != 0 && eback(sb) < g && rd::<W>(g - es::<W>()) == v {
        gbump::<W>(sb, -1);
        return rd::<W>(gptr(sb));
    }
    meta::<W>(vcall(c, sb, V_PBACKFAIL, &[v as u64]))
}
fn sungetc<const W: bool>(c: &mut Ctx, sb: u32) -> u32 {
    let g = gptr(sb);
    if g != 0 && eback(sb) < g {
        gbump::<W>(sb, -1);
        return rd::<W>(gptr(sb));
    }
    meta::<W>(vcall(c, sb, V_PBACKFAIL, &[eof::<W>() as u64]))
}
/// pubseekoff / pubseekpos into a fresh pos_type; its streamoff (_Myoff + _Fpos)
fn seekoff(c: &mut Ctx, sb: u32, off: i64, way: u32, mode: u32) -> i64 {
    let r = mem::heap_alloc(24, true);
    vcall(c, sb, V_SEEKOFF, &[wide(r), off as u64, way as u64, mode as u64]);
    let v = i64_(r).wrapping_add(i64_(r + 8));
    mem::heap_free(r);
    v
}
fn seekpos(c: &mut Ctx, sb: u32, pos: u32, mode: u32) -> i64 {
    let r = mem::heap_alloc(24, true);
    let arg = mem::heap_alloc(24, true);
    mem::copy(arg, pos, 24);
    vcall(c, sb, V_SEEKPOS, &[wide(r), wide(arg), mode as u64]);
    let v = i64_(r).wrapping_add(i64_(r + 8));
    mem::heap_free(r);
    mem::heap_free(arg);
    v
}
/// fpos(off) into `ret`
fn put_pos(ret: u32, off: i64) -> u32 {
    w64(ret, off as u64);
    w64(ret + 8, 0);
    w64(ret + 16, 0);
    ret
}

fn sb_vtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| {
        table(
            if W { "wstreambuf" } else { "streambuf" },
            &[
                sb_delete::<W>,
                nothing,
                nothing,
                sb_eof::<W>,
                sb_eof::<W>,
                sb_zero,
                sb_eof::<W>,
                sb_uflow::<W>,
                sb_xsgetn::<W>,
                sb_xsputn::<W>,
                sb_seekoff,
                sb_seekpos,
                sb_setbuf,
                sb_zero,
                nothing,
            ],
        )
    })
}
fn sb_dtor_at(c: &mut Ctx, sb: u32) {
    let l = rp(sb + PLOCALE);
    if l != 0 {
        crate::msvcp::delete_locale(c, l);
    }
}
fn sb_delete<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, fl) = (c.a(0), c.a(1));
    sb_dtor_at(c, this);
    if fl & 1 != 0 {
        mem::heap_free(this);
    }
    this
}
fn nothing(_: &mut Ctx) -> u32 {
    0
}
fn sb_eof<const W: bool>(_: &mut Ctx) -> u32 {
    eof::<W>()
}
fn sb_zero(c: &mut Ctx) -> u32 {
    c.ret64(0)
}
fn sb_uflow<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    if meta::<W>(vcall(c, this, V_UNDERFLOW, &[])) == eof::<W>() {
        return eof::<W>();
    }
    let v = rd::<W>(gptr(this));
    gbump::<W>(this, 1);
    v
}
fn sb_xsgetn<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, mut p, count) = (c.a(0), c.a(1), c.a64(2) as i64);
    let mut n = count;
    while n > 0 {
        let avail = gnavail(this);
        if avail > 0 {
            let k = avail.min(n);
            mem::copy(p, gptr(this), k as u32 * es::<W>());
            p += k as u32 * es::<W>();
            n -= k;
            gbump::<W>(this, k as i32);
        } else {
            let m = meta::<W>(vcall(c, this, V_UFLOW, &[]));
            if m == eof::<W>() {
                break;
            }
            wr::<W>(p, m);
            p += es::<W>();
            n -= 1;
        }
    }
    c.ret64((count - n) as u64)
}
fn sb_xsputn<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, mut p, count) = (c.a(0), c.a(1), c.a64(2) as i64);
    let mut n = count;
    while n > 0 {
        let avail = pnavail(this);
        if avail > 0 {
            let k = avail.min(n);
            mem::copy(pptr(this), p, k as u32 * es::<W>());
            p += k as u32 * es::<W>();
            n -= k;
            pbump::<W>(this, k as i32);
        } else if meta::<W>(vcall(c, this, V_OVERFLOW, &[rd::<W>(p) as u64])) == eof::<W>() {
            break;
        } else {
            p += es::<W>();
            n -= 1;
        }
    }
    c.ret64((count - n) as u64)
}
/// seekoff (this, result, ...) / seekpos: the bad position
fn sb_seekoff(c: &mut Ctx) -> u32 {
    put_pos(c.a(1), -1)
}
fn sb_seekpos(c: &mut Ctx) -> u32 {
    put_pos(c.a(1), -1)
}
fn sb_setbuf(c: &mut Ctx) -> u32 {
    c.a(0)
}

/// ??0basic_streambuf (protected default)
fn sb_ctor<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    mem::fill(this + 8, SB_SIZE - 8, 0);
    wq(this, sb_vtable::<W>());
    wq(this + PLOCALE, crate::msvcp::new_locale());
    sb_init::<W>(this);
    this
}
/// basic_streambuf(_Uninitialized): nothing is set
fn sb_ctor_uninit(c: &mut Ctx) -> u32 {
    c.a(0)
}
/// basic_streambuf(const basic_streambuf&)
fn sb_ctor_copy<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    mem::fill(this + 8, SB_SIZE - 8, 0);
    wq(this, sb_vtable::<W>());
    let l = mem::heap_alloc(crate::msvcp::LOCALE_SIZE, true);
    crate::msvcp::locale_copy(c, l, rp(o + PLOCALE));
    wq(this + PLOCALE, l);
    sb_init::<W>(this);
    setp::<W>(this, pbase(o), pptr(o), epptr::<W>(o));
    setg::<W>(this, eback(o), gptr(o), egptr::<W>(o));
    this
}
fn sb_assign<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    if this != o {
        setp::<W>(this, pbase(o), pptr(o), epptr::<W>(o));
        setg::<W>(this, eback(o), gptr(o), egptr::<W>(o));
        let tmp = mem::heap_alloc(crate::msvcp::LOCALE_SIZE, true);
        pubimbue_at(c, this, tmp, rp(o + PLOCALE));
        crate::msvcp::delete_locale_value(c, tmp);
        mem::heap_free(tmp);
    }
    this
}
fn sb_swap<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    if this != o {
        let (pf, pn, pe) = (pbase(this), pptr(this), epptr::<W>(this));
        let (gf, gn, ge) = (eback(this), gptr(this), egptr::<W>(this));
        setp::<W>(this, pbase(o), pptr(o), epptr::<W>(o));
        setp::<W>(o, pf, pn, pe);
        setg::<W>(this, eback(o), gptr(o), egptr::<W>(o));
        setg::<W>(o, gf, gn, ge);
        let l = r64(this + PLOCALE);
        w64(this + PLOCALE, r64(o + PLOCALE));
        w64(o + PLOCALE, l);
    }
    0
}
fn sb_dtor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    sb_dtor_at(c, this);
    0
}
fn pubimbue_at(c: &mut Ctx, sb: u32, ret: u32, loc: u32) {
    let l = rp(sb + PLOCALE);
    crate::msvcp::locale_copy(c, ret, l);
    vcall(c, sb, V_IMBUE, &[wide(loc)]);
    crate::msvcp::locale_assign(c, l, loc);
}
fn sb_pubimbue(c: &mut Ctx) -> u32 {
    let (this, ret, loc) = (c.a(0), c.a(1), c.a(2));
    pubimbue_at(c, this, ret, loc);
    ret
}
fn sb_getloc(c: &mut Ctx) -> u32 {
    let (this, ret) = (c.a(0), c.a(1));
    crate::msvcp::locale_copy(c, ret, rp(this + PLOCALE));
    ret
}
fn sb_pubseekoff(c: &mut Ctx) -> u32 {
    let (this, ret, off, way, mode) = (c.a(0), c.a(1), c.a64(2), c.a(3), c.a(4));
    vcall(c, this, V_SEEKOFF, &[wide(ret), off, way as u64, mode as u64]);
    ret
}
fn sb_pubseekpos(c: &mut Ctx) -> u32 {
    let (this, ret, pos, mode) = (c.a(0), c.a(1), c.a(2), c.a(3));
    vcall(c, this, V_SEEKPOS, &[wide(ret), wide(pos), mode as u64]);
    ret
}
fn sb_pubsetbuf(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2));
    vcall(c, this, V_SETBUF, &[wide(p), n]) as u32
}
fn sb_pubsync(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    pubsync(c, this) as u32
}
fn sb_in_avail(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = gnavail(this);
    let v = if n > 0 { n as u64 } else { vcall(c, this, V_SHOWMANYC, &[]) };
    c.ret64(v)
}
fn sb_sbumpc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    sbumpc::<W>(c, this)
}
fn sb_sgetc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    sgetc::<W>(c, this)
}
fn sb_snextc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    snextc::<W>(c, this)
}
fn sb_sgetn<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2) as i64);
    let r = sgetn(c, this, p, n);
    c.ret64(r as u64)
}
fn sb_sputbackc<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), ch::<W>(c.a(1)));
    sputbackc::<W>(c, this, v)
}
fn sb_sungetc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    sungetc::<W>(c, this)
}
fn sb_stossc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    if gnavail(this) > 0 {
        gbump::<W>(this, 1);
    } else {
        vcall(c, this, V_UFLOW, &[]);
    }
    0
}
fn sb_sputc<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), ch::<W>(c.a(1)));
    sputc::<W>(c, this, v)
}
fn sb_sputn<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2) as i64);
    let r = sputn(c, this, p, n);
    c.ret64(r as u64)
}
fn sb_eback(c: &mut Ctx) -> u32 {
    eback(c.a(0))
}
fn sb_gptr(c: &mut Ctx) -> u32 {
    gptr(c.a(0))
}
fn sb_pbase(c: &mut Ctx) -> u32 {
    pbase(c.a(0))
}
fn sb_pptr(c: &mut Ctx) -> u32 {
    pptr(c.a(0))
}
fn sb_egptr<const W: bool>(c: &mut Ctx) -> u32 {
    egptr::<W>(c.a(0))
}
fn sb_epptr<const W: bool>(c: &mut Ctx) -> u32 {
    epptr::<W>(c.a(0))
}
fn sb_gbump<const W: bool>(c: &mut Ctx) -> u32 {
    gbump::<W>(c.a(0), c.a(1) as i32);
    0
}
fn sb_pbump<const W: bool>(c: &mut Ctx) -> u32 {
    pbump::<W>(c.a(0), c.a(1) as i32);
    0
}
fn sb_setg<const W: bool>(c: &mut Ctx) -> u32 {
    setg::<W>(c.a(0), c.a(1), c.a(2), c.a(3));
    0
}
fn sb_setp2<const W: bool>(c: &mut Ctx) -> u32 {
    setp::<W>(c.a(0), c.a(1), c.a(1), c.a(2));
    0
}
fn sb_setp3<const W: bool>(c: &mut Ctx) -> u32 {
    setp::<W>(c.a(0), c.a(1), c.a(2), c.a(3));
    0
}
fn sb_Gndec<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    gbump::<W>(this, -1);
    gptr(this)
}
fn sb_Gninc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let g = gptr(this);
    gbump::<W>(this, 1);
    g
}
fn sb_Gnpreinc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    gbump::<W>(this, 1);
    gptr(this)
}
fn sb_Gnavail(c: &mut Ctx) -> u32 {
    let n = gnavail(c.a(0));
    c.ret64(n as u64)
}
fn sb_Pninc<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let p = pptr(this);
    pbump::<W>(this, 1);
    p
}
fn sb_Pnavail(c: &mut Ctx) -> u32 {
    let n = pnavail(c.a(0));
    c.ret64(n as u64)
}
fn sb_Init0<const W: bool>(c: &mut Ctx) -> u32 {
    sb_init::<W>(c.a(0));
    0
}
/// _Init(gfirst, gnext, gcount, pfirst, pnext, pcount)
fn sb_Init6(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wq(this + IGFIRST, c.a(1));
    wq(this + IGNEXT, c.a(2));
    wq(this + IGCOUNT, c.a(3));
    wq(this + IPFIRST, c.a(4));
    wq(this + IPNEXT, c.a(5));
    wq(this + IPCOUNT, c.a(6));
    0
}

// ---------------------------------------------------------------------------------------------
// basic_ios

fn ios_vtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| table(if W { "wios" } else { "ios" }, &[ios_base_delete]))
}
/// basic_ios::init(streambuf, isstd)
fn ios_init<const W: bool>(c: &mut Ctx, ios: u32, sb: u32, isstd: bool) {
    base_init(c, ios);
    wq(ios + STRBUF, sb);
    w64(ios + TIESTR, 0);
    wr::<W>(ios + FILLCH, b' ' as u32);
    if sb == 0 {
        setstate(c, ios, BADBIT);
    }
    if isstd {
        addstd(ios);
    }
}
/// The basic_ios part of a new object: ios_base() and the basic_ios fields zeroed.
fn ios_construct<const W: bool>(ios: u32) {
    mem::fill(ios + 8, IOS_SIZE - 8, 0);
    wq(ios, ios_vtable::<W>());
}
fn ios_ctor<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    ios_construct::<W>(this);
    this
}
fn ios_ctor_sb<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, sb) = (c.a(0), c.a(1));
    ios_construct::<W>(this);
    ios_init::<W>(c, this, sb, false);
    this
}
fn ios_dtor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    base_dtor(c, this);
    0
}
fn ios_clear2(c: &mut Ctx) -> u32 {
    let (this, st, re) = (c.a(0), c.a(1), c.a(2) & 0xff != 0);
    ios_clear(c, this, st, re);
    0
}
fn ios_clear1(c: &mut Ctx) -> u32 {
    let (this, st) = (c.a(0), c.a(1));
    ios_clear(c, this, st, false);
    0
}
fn ios_setstate2(c: &mut Ctx) -> u32 {
    let (this, st, re) = (c.a(0), c.a(1), c.a(2) & 0xff != 0);
    ios_clear(c, this, state(this) | st, re);
    0
}
fn ios_setstate1(c: &mut Ctx) -> u32 {
    let (this, st) = (c.a(0), c.a(1));
    ios_clear(c, this, state(this) | st, false);
    0
}
fn ios_copyfmt<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    w64(this + TIESTR, r64(o + TIESTR));
    wr::<W>(this + FILLCH, rd::<W>(o + FILLCH));
    copyfmt_base(c, this, o);
    this
}
fn ios_tie_get(c: &mut Ctx) -> u32 {
    rp(c.a(0) + TIESTR)
}
fn ios_tie_set(c: &mut Ctx) -> u32 {
    let (this, t) = (c.a(0), c.a(1));
    let old = rp(this + TIESTR);
    wq(this + TIESTR, t);
    old
}
fn ios_rdbuf_get(c: &mut Ctx) -> u32 {
    rp(c.a(0) + STRBUF)
}
fn ios_rdbuf_set(c: &mut Ctx) -> u32 {
    let (this, sb) = (c.a(0), c.a(1));
    let old = rp(this + STRBUF);
    wq(this + STRBUF, sb);
    ios_clear(c, this, 0, false);
    old
}
fn ios_set_rdbuf(c: &mut Ctx) -> u32 {
    wq(c.a(0) + STRBUF, c.a(1));
    0
}
fn ios_imbue(c: &mut Ctx) -> u32 {
    let (this, ret, loc) = (c.a(0), c.a(1), c.a(2));
    base_imbue_at(c, this, ret, loc);
    let sb = rp(this + STRBUF);
    if sb != 0 {
        let tmp = mem::heap_alloc(crate::msvcp::LOCALE_SIZE, true);
        pubimbue_at(c, sb, tmp, loc);
        crate::msvcp::delete_locale_value(c, tmp);
        mem::heap_free(tmp);
    }
    ret
}
fn ios_fill_get<const W: bool>(c: &mut Ctx) -> u32 {
    rd::<W>(c.a(0) + FILLCH)
}
fn ios_fill_set<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), ch::<W>(c.a(1)));
    let old = rd::<W>(this + FILLCH);
    wr::<W>(this + FILLCH, v);
    old
}
/// narrow(ch, default) / widen(byte): the "C" locale's ctype (wchar_t above 0xff: default)
fn ios_narrow<const W: bool>(c: &mut Ctx) -> u32 {
    let (v, d) = (ch::<W>(c.a(1)), c.a(2) & 0xff);
    if v < 0x100 { v } else { d }
}
fn ios_widen(c: &mut Ctx) -> u32 {
    c.a(1) & 0xff
}
fn ios_swap_at<const W: bool>(this: u32, o: u32) {
    base_swap_at(this, o);
    let f = rd::<W>(this + FILLCH);
    wr::<W>(this + FILLCH, rd::<W>(o + FILLCH));
    wr::<W>(o + FILLCH, f);
    let t = r64(this + TIESTR);
    w64(this + TIESTR, r64(o + TIESTR));
    w64(o + TIESTR, t);
}
fn ios_swap<const W: bool>(c: &mut Ctx) -> u32 {
    ios_swap_at::<W>(c.a(0), c.a(1));
    0
}
fn ios_move<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    if this != o {
        w64(this + STRBUF, 0);
        w64(this + TIESTR, 0);
        ios_swap_at::<W>(this, o);
    }
    0
}
fn ios_init_fn<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, sb, isstd) = (c.a(0), c.a(1), c.a(2) & 0xff != 0);
    ios_init::<W>(c, this, sb, isstd);
    0
}

// ---------------------------------------------------------------------------------------------
// basic_istream / basic_ostream / basic_iostream: the virtual base through the vbtable

/// The basic_ios of the stream subobject at `this` (vbtable entry 1).
fn vb(this: u32) -> u32 {
    this.wrapping_add(r32(rp(this) + 4))
}
fn vbtable(off: u32) -> u32 {
    let t = mem::heap_alloc(8, true);
    w32(t + 4, off);
    t
}
fn istream_vbtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| vbtable(ISTREAM_VB))
}
fn ostream_vbtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| vbtable(OSTREAM_VB))
}
/// basic_iostream's two: of its istream part (at 0) and its ostream part (at 16)
fn iostream_vbtables<const W: bool>() -> (u32, u32) {
    static V: [OnceLock<(u32, u32)>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| (vbtable(IOSTREAM_VB), vbtable(IOSTREAM_VB - 16)))
}

/// The scalar deleting destructor in a stream class's vftable: `this` is the basic_ios part,
/// the object starts `static` (+ its vtordisp) before it.
fn stream_delete(c: &mut Ctx, st: u32) -> u32 {
    let (ios, fl) = (c.a(0), c.a(1));
    let start = ios.wrapping_sub(st).wrapping_sub(r32(ios - 4));
    base_dtor(c, ios);
    if fl & 1 != 0 {
        mem::heap_free(start);
    }
    start
}
fn istream_delete(c: &mut Ctx) -> u32 {
    stream_delete(c, ISTREAM_VB)
}
fn ostream_delete(c: &mut Ctx) -> u32 {
    stream_delete(c, OSTREAM_VB)
}
fn iostream_delete(c: &mut Ctx) -> u32 {
    stream_delete(c, IOSTREAM_VB)
}
fn istream_vtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| table(if W { "wistream" } else { "istream" }, &[istream_delete]))
}
fn ostream_vtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| table(if W { "wostream" } else { "ostream" }, &[ostream_delete]))
}
fn iostream_vtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| table(if W { "wiostream" } else { "iostream" }, &[iostream_delete]))
}

/// A stream constructor's own part: the vbtable (most derived: also the basic_ios), then the
/// class's vftable and vtordisp in the basic_ios.
fn stream_begin<const W: bool>(this: u32, vbt: u32, most_derived: bool, vft: u32, st: u32) -> u32 {
    if most_derived {
        wq(this, vbt);
        ios_construct::<W>(vb(this));
    }
    let ios = vb(this);
    wq(ios, vft);
    w32(ios - 4, (ios.wrapping_sub(this)).wrapping_sub(st));
    ios
}

/// basic_istream(streambuf, isstd) [, most derived]
fn istream_ctor<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, sb, isstd, md) = (c.a(0), c.a(1), c.a(2) & 0xff != 0, c.a(3) != 0);
    let ios = stream_begin::<W>(this, istream_vbtable::<W>(), md, istream_vtable::<W>(), ISTREAM_VB);
    w64(this + CHCOUNT, 0);
    ios_init::<W>(c, ios, sb, isstd);
    this
}
/// basic_istream(streambuf, isstd, skip init) [, most derived]
fn istream_ctor3<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, sb, isstd, skip, md) = (c.a(0), c.a(1), c.a(2) & 0xff != 0, c.a(3) & 0xff != 0, c.a(4) != 0);
    let ios = stream_begin::<W>(this, istream_vbtable::<W>(), md, istream_vtable::<W>(), ISTREAM_VB);
    w64(this + CHCOUNT, 0);
    if !skip {
        ios_init::<W>(c, ios, sb, isstd);
    }
    this
}
/// basic_istream(_Uninitialized) [, most derived]
fn istream_ctor_uninit<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, md) = (c.a(0), c.a(2) != 0);
    let ios = stream_begin::<W>(this, istream_vbtable::<W>(), md, istream_vtable::<W>(), ISTREAM_VB);
    addstd(ios);
    this
}
/// basic_istream(basic_istream&&) [, most derived]
fn istream_ctor_move<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o, md) = (c.a(0), c.a(1), c.a(2) != 0);
    let ios = stream_begin::<W>(this, istream_vbtable::<W>(), md, istream_vtable::<W>(), ISTREAM_VB);
    w64(this + CHCOUNT, r64(o + CHCOUNT));
    ios_init::<W>(c, ios, 0, false);
    move_ios::<W>(ios, vb(o));
    w64(o + CHCOUNT, 0);
    this
}
fn move_ios<const W: bool>(ios: u32, o: u32) {
    if ios != o {
        w64(ios + STRBUF, 0);
        w64(ios + TIESTR, 0);
        ios_swap_at::<W>(ios, o);
    }
}
fn ostream_ctor<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, sb, isstd, md) = (c.a(0), c.a(1), c.a(2) & 0xff != 0, c.a(3) != 0);
    let ios = stream_begin::<W>(this, ostream_vbtable::<W>(), md, ostream_vtable::<W>(), OSTREAM_VB);
    ios_init::<W>(c, ios, sb, isstd);
    this
}
/// basic_ostream(_Uninitialized, addit) [, most derived]
fn ostream_ctor_uninit<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, addit, md) = (c.a(0), c.a(2) & 0xff != 0, c.a(3) != 0);
    let ios = stream_begin::<W>(this, ostream_vbtable::<W>(), md, ostream_vtable::<W>(), OSTREAM_VB);
    if addit {
        addstd(ios);
    }
    this
}
fn ostream_ctor_move<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o, md) = (c.a(0), c.a(1), c.a(2) != 0);
    let ios = stream_begin::<W>(this, ostream_vbtable::<W>(), md, ostream_vtable::<W>(), OSTREAM_VB);
    ios_init::<W>(c, ios, 0, false);
    move_ios::<W>(ios, vb(o));
    this
}
/// basic_iostream(streambuf) [, most derived]: istream(sb), ostream(uninitialized, no addstd)
fn iostream_begin<const W: bool>(this: u32, md: bool) -> u32 {
    let (vi, vo) = iostream_vbtables::<W>();
    if md {
        wq(this, vi);
        wq(this + 16, vo);
        ios_construct::<W>(vb(this));
    }
    let ios = vb(this);
    wq(ios, iostream_vtable::<W>());
    w32(ios - 4, (ios.wrapping_sub(this)).wrapping_sub(IOSTREAM_VB));
    w64(this + CHCOUNT, 0);
    ios
}
fn iostream_ctor<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, sb, md) = (c.a(0), c.a(1), c.a(2) != 0);
    let ios = iostream_begin::<W>(this, md);
    ios_init::<W>(c, ios, sb, false);
    this
}
fn iostream_ctor_move<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o, md) = (c.a(0), c.a(1), c.a(2) != 0);
    let ios = iostream_begin::<W>(this, md);
    ios_init::<W>(c, ios, 0, false);
    move_ios::<W>(ios, vb(o));
    w64(this + CHCOUNT, r64(o + CHCOUNT));
    w64(o + CHCOUNT, 0);
    this
}
/// ~basic_istream / ~basic_ostream / ~basic_iostream (this: the basic_ios part): nothing of
/// their own
fn stream_dtor(_: &mut Ctx) -> u32 {
    0
}
/// ??_D: the destructor of a complete object (this: the class), with its virtual base
fn stream_vbase_dtor(c: &mut Ctx) -> u32 {
    let ios = vb(c.a(0));
    base_dtor(c, ios);
    0
}
/// operator=(&&) / swap
fn istream_swap<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    ios_swap_at::<W>(vb(this), vb(o));
    let n = r64(this + CHCOUNT);
    w64(this + CHCOUNT, r64(o + CHCOUNT));
    w64(o + CHCOUNT, n);
    this
}
fn ostream_swap<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, o) = (c.a(0), c.a(1));
    if this != o {
        ios_swap_at::<W>(vb(this), vb(o));
    }
    this
}

// ---------------------------------------------------------------------------------------------
// sentries

fn lock(c: &mut Ctx, ios: u32) {
    let sb = rp(ios + STRBUF);
    if sb != 0 {
        vcall(c, sb, V_LOCK, &[]);
    }
}
fn unlock(c: &mut Ctx, ios: u32) {
    let sb = rp(ios + STRBUF);
    if sb != 0 {
        vcall(c, sb, V_UNLOCK, &[]);
    }
}
/// basic_ostream::flush of the stream whose basic_ios is `ios` (with its sentry)
fn flush_ios<const W: bool>(c: &mut Ctx, ios: u32) {
    let sb = rp(ios + STRBUF);
    if sb == 0 {
        return;
    }
    if osentry::<W>(c, ios) {
        let st = if pubsync(c, sb) == -1 { BADBIT } else { 0 };
        setstate(c, ios, st);
    }
    osentry_end(c, ios);
}
/// flush of an ostream object (0: none)
fn flush_at<const W: bool>(c: &mut Ctx, os: u32) {
    if os != 0 {
        flush_ios::<W>(c, vb(os));
    }
}
/// basic_ostream::sentry: locks, flushes the tied stream; true when the stream is good
fn osentry<const W: bool>(c: &mut Ctx, ios: u32) -> bool {
    lock(c, ios);
    if !good(ios) {
        return false;
    }
    let t = rp(ios + TIESTR);
    if t == 0 || vb(t) == ios {
        return true;
    }
    flush_ios::<W>(c, vb(t));
    good(ios)
}
/// ~sentry: _Osfx, unlock
fn osentry_end(c: &mut Ctx, ios: u32) {
    osfx(c, ios);
    unlock(c, ios);
}
fn osfx(c: &mut Ctx, ios: u32) {
    if good(ios) && flags(ios) & UNITBUF != 0 {
        let sb = rp(ios + STRBUF);
        if pubsync(c, sb) == -1 {
            setstate(c, ios, BADBIT);
        }
    }
}
/// _Ipfx(noskip): good, the tied stream flushed, whitespace skipped
fn ipfx<const W: bool>(c: &mut Ctx, ios: u32, noskip: bool) -> bool {
    if !good(ios) {
        setstate(c, ios, FAILBIT);
        return false;
    }
    let t = rp(ios + TIESTR);
    if t != 0 {
        flush_ios::<W>(c, vb(t));
    }
    if !noskip && flags(ios) & SKIPWS != 0 {
        let sb = rp(ios + STRBUF);
        let mut m = sgetc::<W>(c, sb);
        loop {
            if m == eof::<W>() {
                setstate(c, ios, EOFBIT | FAILBIT);
                break;
            }
            if !is_space(m) {
                break;
            }
            m = snextc::<W>(c, sb);
        }
    }
    good(ios)
}
fn is_space(v: u32) -> bool {
    matches!(v, 0x20 | 0x09..=0x0d)
}
/// basic_istream::sentry
fn isentry<const W: bool>(c: &mut Ctx, ios: u32, noskip: bool) -> bool {
    lock(c, ios);
    ipfx::<W>(c, ios, noskip)
}

// ---------------------------------------------------------------------------------------------
// basic_ostream

fn os_Osfx(c: &mut Ctx) -> u32 {
    let ios = vb(c.a(0));
    osfx(c, ios);
    0
}
fn os_opfx<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let ios = vb(this);
    if !good(ios) {
        return 0;
    }
    let t = rp(ios + TIESTR);
    if t == 0 || t == this {
        return 1;
    }
    flush_ios::<W>(c, vb(t));
    good(ios) as u32
}
fn os_flush<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    flush_ios::<W>(c, vb(this));
    this
}
fn os_put<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), ch::<W>(c.a(1)));
    let ios = vb(this);
    let st = if !osentry::<W>(c, ios) {
        BADBIT
    } else if sputc::<W>(c, rp(ios + STRBUF), v) == eof::<W>() {
        BADBIT
    } else {
        0
    };
    osentry_end(c, ios);
    setstate(c, ios, st);
    this
}
fn os_write<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2) as i64);
    let ios = vb(this);
    let st = if !osentry::<W>(c, ios) {
        BADBIT
    } else if n > 0 && sputn(c, rp(ios + STRBUF), p, n) != n {
        BADBIT
    } else {
        0
    };
    osentry_end(c, ios);
    setstate(c, ios, st);
    this
}
fn os_seekp_pos(c: &mut Ctx) -> u32 {
    let (this, pos) = (c.a(0), c.a(1));
    let ios = vb(this);
    lock(c, ios);
    if !fail(ios) {
        let st = if seekpos(c, rp(ios + STRBUF), pos, OUT) == -1 { FAILBIT } else { 0 };
        setstate(c, ios, st);
    }
    osentry_end(c, ios);
    this
}
fn os_seekp_off(c: &mut Ctx) -> u32 {
    let (this, off, way) = (c.a(0), c.a64(1) as i64, c.a(2));
    let ios = vb(this);
    lock(c, ios);
    if !fail(ios) {
        let st = if seekoff(c, rp(ios + STRBUF), off, way, OUT) == -1 { FAILBIT } else { 0 };
        setstate(c, ios, st);
    }
    osentry_end(c, ios);
    this
}
fn os_tellp(c: &mut Ctx) -> u32 {
    let (this, ret) = (c.a(0), c.a(1));
    let ios = vb(this);
    lock(c, ios);
    put_pos(ret, -1);
    if !fail(ios) {
        let sb = rp(ios + STRBUF);
        vcall(c, sb, V_SEEKOFF, &[wide(ret), 0, SEEK_CUR as u64, OUT as u64]);
    }
    osentry_end(c, ios);
    ret
}
/// operator<<(manipulator): calls it with the stream (basic_ios / ios_base ones get the
/// basic_ios part)
fn os_manip_self(c: &mut Ctx) -> u32 {
    let (this, f) = (c.a(0), c.a(1));
    call(c, f, &[wide(this)]) as u32
}
fn manip_ios(c: &mut Ctx) -> u32 {
    let (this, f) = (c.a(0), c.a(1));
    call(c, f, &[wide(vb(this))]);
    this
}
/// operator<<(basic_streambuf*): copies until its end
fn os_from_sb<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, src) = (c.a(0), c.a(1));
    let ios = vb(this);
    let mut st = 0;
    let mut copied = false;
    if osentry::<W>(c, ios) && src != 0 {
        let mut m = eof::<W>();
        loop {
            m = if m == eof::<W>() { sgetc::<W>(c, src) } else { snextc::<W>(c, src) };
            if m == eof::<W>() {
                break;
            }
            if sputc::<W>(c, rp(ios + STRBUF), m) == eof::<W>() {
                st |= BADBIT;
                break;
            }
            copied = true;
        }
    }
    osentry_end(c, ios);
    w64(ios + WIDE, 0);
    let st = if src == 0 { BADBIT } else if copied { st } else { st | FAILBIT };
    setstate(c, ios, st);
    this
}

/// Puts `field` (ASCII) padded to the stream's width as num_put does: the first `prefix`
/// characters (sign, 0x) before internal fill. False when the buffer refused a character.
fn put_field<const W: bool>(c: &mut Ctx, ios: u32, field: &[u8], prefix: usize) -> bool {
    let w = r64(ios + WIDE) as i64;
    let mut fill = if w <= 0 || w as usize <= field.len() { 0 } else { w as usize - field.len() };
    let fc = rd::<W>(ios + FILLCH);
    let sb = rp(ios + STRBUF);
    let adj = flags(ios) & ADJUSTFIELD;
    let mut ok = true;
    let mut put = |c: &mut Ctx, v: u32| {
        if ok && sputc::<W>(c, sb, v) == eof::<W>() {
            ok = false;
        }
    };
    if adj != LEFT && adj != INTERNAL {
        for _ in 0..fill {
            put(c, fc);
        }
        fill = 0;
        for &b in &field[..prefix] {
            put(c, b as u32);
        }
    } else if adj == INTERNAL {
        for &b in &field[..prefix] {
            put(c, b as u32);
        }
        for _ in 0..fill {
            put(c, fc);
        }
        fill = 0;
    } else {
        for &b in &field[..prefix] {
            put(c, b as u32);
        }
    }
    for &b in &field[prefix..] {
        put(c, b as u32);
    }
    w64(ios + WIDE, 0);
    for _ in 0..fill {
        put(c, fc);
    }
    ok
}

/// The printf arguments of one conversion.
struct Args(Vec<u64>, Vec<f64>);
impl crt::fmt::Args for Args {
    fn int(&mut self, bytes: u32) -> u64 {
        let v = if self.0.is_empty() { 0 } else { self.0.remove(0) };
        if bytes >= 8 { v } else { v & ((1u64 << (8 * bytes)) - 1) }
    }
    fn float(&mut self) -> f64 {
        if self.1.is_empty() { 0.0 } else { self.1.remove(0) }
    }
    fn narrow(&mut self, _: u64, _: Option<usize>) -> Vec<u8> {
        Vec::new()
    }
    fn wide(&mut self, _: u64, _: Option<usize>) -> Vec<u16> {
        Vec::new()
    }
    fn counted(&mut self, _: u64) -> (usize, u64) {
        (0, 0)
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        s.iter().map(|&b| b as u16).collect()
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        s.iter().map(|&u| u as u8).collect()
    }
    fn store(&mut self, _: u64, _: u32, _: u64) {}
}
fn sprintf(fmt: &str, ints: Vec<u64>, floats: Vec<f64>) -> Vec<u8> {
    crt::fmt::format(fmt.as_bytes(), crt::fmt::Mode::ucrt(true, 0), &mut Args(ints, floats)).unwrap_or_default()
}

/// num_put's integer field: _Ifmt's conversion (spec "ld", "lu", "Ld", "Lu")
fn int_field(fl: u32, v: u64, signed: bool, long64: bool) -> (Vec<u8>, usize) {
    let mut f = String::from("%");
    if fl & SHOWPOS != 0 {
        f.push('+');
    }
    if fl & SHOWBASE != 0 {
        f.push('#');
    }
    f.push_str(if long64 { "I64" } else { "l" });
    let base = fl & BASEFIELD;
    f.push(if base == OCT {
        'o'
    } else if base != HEX {
        if signed { 'd' } else { 'u' }
    } else if fl & UPPERCASE != 0 {
        'X'
    } else {
        'x'
    });
    let s = sprintf(&f, vec![v], vec![]);
    let mut prefix = (!s.is_empty() && (s[0] == b'+' || s[0] == b'-')) as usize;
    if base == HEX && prefix + 2 <= s.len() && s[prefix] == b'0' && (s[prefix + 1] | 0x20) == b'x' {
        prefix += 2;
    }
    (s, prefix)
}
/// num_put's floating-point field (_Ffmt, _Fput_v3)
fn float_field(fl: u32, prec: i64, v: f64) -> (Vec<u8>, usize) {
    let ff = fl & FLOATFIELD;
    let hex = ff == FIXED | SCIENTIFIC;
    let prec = if hex { -1 } else { prec };
    let fl = if v.is_finite() { fl } else { fl & !SHOWPOINT };
    let mut f = String::from("%");
    if fl & SHOWPOS != 0 {
        f.push('+');
    }
    if fl & SHOWPOINT != 0 {
        f.push('#');
    }
    f.push_str(".*");
    let up = fl & UPPERCASE != 0;
    f.push(match (ff, up) {
        (FIXED, _) => 'f',
        (_, true) if hex => 'A',
        (_, false) if hex => 'a',
        (SCIENTIFIC, true) => 'E',
        (SCIENTIFIC, false) => 'e',
        (_, true) => 'G',
        _ => 'g',
    });
    let s = sprintf(&f, vec![prec as i32 as u32 as u64], vec![v]);
    let mut prefix = (!s.is_empty() && (s[0] == b'+' || s[0] == b'-')) as usize;
    if hex && prefix + 2 <= s.len() && s[prefix] == b'0' && (s[prefix + 1] | 0x20) == b'x' {
        prefix += 2;
    }
    (s, prefix)
}

/// operator<<(number) after the sentry: the field put, badbit when that failed
fn insert<const W: bool>(c: &mut Ctx, this: u32, field: impl FnOnce(u32, u32) -> (Vec<u8>, usize)) -> u32 {
    let ios = vb(this);
    let mut st = 0;
    if osentry::<W>(c, ios) {
        let (s, prefix) = field(ios, flags(ios));
        if !put_field::<W>(c, ios, &s, prefix) {
            st = BADBIT;
        }
    }
    osentry_end(c, ios);
    setstate(c, ios, st);
    this
}
/// short / int: as unsigned for oct and hex
fn os_short<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1) as u16);
    insert::<W>(c, this, |_, fl| {
        let b = fl & BASEFIELD;
        let x = if b == OCT || b == HEX { v as u64 } else { v as i16 as i64 as u64 };
        int_field(fl, x, true, false)
    })
}
fn os_ushort<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1) & 0xffff);
    insert::<W>(c, this, |_, fl| int_field(fl, v as u64, false, false))
}
fn os_int<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1));
    insert::<W>(c, this, |_, fl| {
        let b = fl & BASEFIELD;
        let x = if b == OCT || b == HEX { v as u64 } else { v as i32 as i64 as u64 };
        int_field(fl, x, true, false)
    })
}
fn os_uint<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1));
    insert::<W>(c, this, |_, fl| int_field(fl, v as u64, false, false))
}
fn os_long<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1));
    insert::<W>(c, this, |_, fl| int_field(fl, v as i32 as i64 as u64, true, false))
}
fn os_i64<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a64(1));
    insert::<W>(c, this, |_, fl| int_field(fl, v, true, true))
}
fn os_u64<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a64(1));
    insert::<W>(c, this, |_, fl| int_field(fl, v, false, true))
}
fn os_bool<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1) & 0xff != 0);
    insert::<W>(c, this, |_, fl| {
        if fl & BOOLALPHA != 0 {
            ((if v { "true" } else { "false" }).as_bytes().to_vec(), 0)
        } else {
            int_field(fl, v as u64, true, false)
        }
    })
}
fn os_float<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), f32::from_bits(c.xmm(1) as u32) as f64);
    insert::<W>(c, this, |ios, fl| float_field(fl, r64(ios + PREC) as i64, v))
}
fn os_double<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), f64::from_bits(c.xmm(1)));
    insert::<W>(c, this, |ios, fl| float_field(fl, r64(ios + PREC) as i64, v))
}
fn os_ptr<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a64(1));
    insert::<W>(c, this, |_, _| (sprintf("%p", vec![v], vec![]), 0))
}

// ---------------------------------------------------------------------------------------------
// basic_istream

fn chcount_inc(this: u32) {
    let n = r64(this + CHCOUNT);
    if n != i64::MAX as u64 {
        w64(this + CHCOUNT, n + 1);
    }
}
fn is_Ipfx<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, noskip) = (c.a(0), c.a(1) & 0xff != 0);
    ipfx::<W>(c, vb(this), noskip) as u32
}
fn is_isfx(_: &mut Ctx) -> u32 {
    0
}
fn is_gcount(c: &mut Ctx) -> u32 {
    let n = r64(c.a(0) + CHCOUNT);
    c.ret64(n)
}
/// get(): a metacharacter
fn get_meta<const W: bool>(c: &mut Ctx, this: u32) -> u32 {
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    let m = if !isentry::<W>(c, ios, true) {
        eof::<W>()
    } else {
        let sb = rp(ios + STRBUF);
        let m = sgetc::<W>(c, sb);
        if m == eof::<W>() {
            st = EOFBIT | FAILBIT;
        } else {
            sbumpc::<W>(c, sb);
            w64(this + CHCOUNT, 1);
        }
        m
    };
    unlock(c, ios);
    setstate(c, ios, st);
    m
}
fn is_get0<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    get_meta::<W>(c, this)
}
fn is_get_ch<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    let m = get_meta::<W>(c, this);
    if m != eof::<W>() {
        wr::<W>(p, m);
    }
    this
}
/// get(str, count, delim)
fn get_str<const W: bool>(c: &mut Ctx, this: u32, mut p: u32, mut n: i64, delim: u32) -> u32 {
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    if isentry::<W>(c, ios, true) && n > 0 {
        let sb = rp(ios + STRBUF);
        let mut m = sgetc::<W>(c, sb);
        loop {
            n -= 1;
            if n <= 0 {
                break;
            }
            if m == eof::<W>() {
                st |= EOFBIT;
                break;
            } else if m == delim {
                break;
            }
            wr::<W>(p, m);
            p += es::<W>();
            chcount_inc(this);
            m = snextc::<W>(c, sb);
        }
    }
    unlock(c, ios);
    if r64(this + CHCOUNT) == 0 {
        st |= FAILBIT;
    }
    setstate(c, ios, st);
    wr::<W>(p, 0);
    this
}
fn is_get_str2<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2) as i64);
    get_str::<W>(c, this, p, n, b'\n' as u32)
}
fn is_get_str3<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n, d) = (c.a(0), c.a(1), c.a64(2) as i64, ch::<W>(c.a(3)));
    get_str::<W>(c, this, p, n, d)
}
/// get(streambuf&, delim)
fn get_sb<const W: bool>(c: &mut Ctx, this: u32, dst: u32, delim: u32) -> u32 {
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    if isentry::<W>(c, ios, true) {
        let sb = rp(ios + STRBUF);
        let mut m = sgetc::<W>(c, sb);
        loop {
            if m == eof::<W>() {
                st |= EOFBIT;
                break;
            }
            if m == delim || sputc::<W>(c, dst, m) == eof::<W>() {
                break;
            }
            chcount_inc(this);
            m = snextc::<W>(c, sb);
        }
    }
    unlock(c, ios);
    if r64(this + CHCOUNT) == 0 {
        st |= FAILBIT;
    }
    setstate(c, ios, st);
    this
}
fn is_get_sb1<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, dst) = (c.a(0), c.a(1));
    get_sb::<W>(c, this, dst, b'\n' as u32)
}
fn is_get_sb2<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, dst, d) = (c.a(0), c.a(1), ch::<W>(c.a(2)));
    get_sb::<W>(c, this, dst, d)
}
fn getline<const W: bool>(c: &mut Ctx, this: u32, mut p: u32, mut n: i64, delim: u32) -> u32 {
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    if isentry::<W>(c, ios, true) && n > 0 {
        let sb = rp(ios + STRBUF);
        let mut m = sgetc::<W>(c, sb);
        loop {
            if m == eof::<W>() {
                st |= EOFBIT;
                break;
            } else if m == delim {
                chcount_inc(this);
                sbumpc::<W>(c, sb);
                break;
            }
            n -= 1;
            if n <= 0 {
                st |= FAILBIT;
                break;
            }
            wr::<W>(p, m);
            p += es::<W>();
            chcount_inc(this);
            m = snextc::<W>(c, sb);
        }
    }
    unlock(c, ios);
    wr::<W>(p, 0);
    if r64(this + CHCOUNT) == 0 {
        st |= FAILBIT;
    }
    setstate(c, ios, st);
    this
}
fn is_getline2<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2) as i64);
    getline::<W>(c, this, p, n, b'\n' as u32)
}
fn is_getline3<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n, d) = (c.a(0), c.a(1), c.a64(2) as i64, ch::<W>(c.a(3)));
    getline::<W>(c, this, p, n, d)
}
/// ignore(count, delim metacharacter)
fn is_ignore<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, mut n, d) = (c.a(0), c.a64(1) as i64, c.a(2));
    let d = if W { d & 0xffff } else { d };
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    if isentry::<W>(c, ios, true) && n > 0 {
        let sb = rp(ios + STRBUF);
        loop {
            if n != i64::MAX {
                n -= 1;
                if n < 0 {
                    break;
                }
            }
            let m = sbumpc::<W>(c, sb);
            if m == eof::<W>() {
                st |= EOFBIT;
                break;
            }
            chcount_inc(this);
            if m == d {
                break;
            }
        }
    }
    unlock(c, ios);
    setstate(c, ios, st);
    this
}
fn read_at<const W: bool>(c: &mut Ctx, this: u32, p: u32, n: i64) {
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    if isentry::<W>(c, ios, true) && n > 0 {
        let got = sgetn(c, rp(ios + STRBUF), p, n);
        w64(this + CHCOUNT, got as u64);
        if got != n {
            st |= EOFBIT | FAILBIT;
        }
    }
    unlock(c, ios);
    setstate(c, ios, st);
}
fn is_read<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2) as i64);
    read_at::<W>(c, this, p, n);
    this
}
fn is_readsome<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a64(2) as i64);
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    let ok = isentry::<W>(c, ios, true);
    unlock(c, ios);
    if !ok {
        st |= FAILBIT;
    } else {
        let sb = rp(ios + STRBUF);
        let g = gnavail(sb);
        let avail = if g > 0 { g } else { vcall(c, sb, V_SHOWMANYC, &[]) as i64 };
        if avail < 0 {
            st |= EOFBIT;
        } else if n > 0 && avail > 0 {
            read_at::<W>(c, this, p, avail.min(n));
        }
    }
    setstate(c, ios, st);
    let r = r64(this + CHCOUNT);
    c.ret64(r)
}
fn is_peek<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    let m = if !isentry::<W>(c, ios, true) {
        eof::<W>()
    } else {
        let m = sgetc::<W>(c, rp(ios + STRBUF));
        if m == eof::<W>() {
            st |= EOFBIT;
        }
        m
    };
    unlock(c, ios);
    setstate(c, ios, st);
    m
}
/// putback(ch) / unget()
fn put_back<const W: bool>(c: &mut Ctx, this: u32, v: Option<u32>) -> u32 {
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let old = state(ios);
    ios_clear(c, ios, old & !EOFBIT, false);
    let mut st = 0;
    if isentry::<W>(c, ios, true) {
        let sb = rp(ios + STRBUF);
        let m = match v {
            Some(v) => sputbackc::<W>(c, sb, v),
            None => sungetc::<W>(c, sb),
        };
        if m == eof::<W>() {
            st |= BADBIT | old;
        }
    }
    unlock(c, ios);
    setstate(c, ios, st);
    this
}
fn is_putback<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), ch::<W>(c.a(1)));
    put_back::<W>(c, this, Some(v))
}
fn is_unget<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    put_back::<W>(c, this, None)
}
fn is_sync<const W: bool>(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let ios = vb(this);
    isentry::<W>(c, ios, true);
    let sb = rp(ios + STRBUF);
    let r = if sb == 0 {
        u32::MAX
    } else if pubsync(c, sb) == -1 {
        setstate(c, ios, BADBIT);
        u32::MAX
    } else {
        0
    };
    unlock(c, ios);
    r
}
fn is_seekg_pos<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, pos) = (c.a(0), c.a(1));
    let ios = vb(this);
    let old = state(ios);
    ios_clear(c, ios, old & !EOFBIT, false);
    isentry::<W>(c, ios, true);
    let mut st = 0;
    if !fail(ios) && seekpos(c, rp(ios + STRBUF), pos, IN) == -1 {
        st |= FAILBIT;
    }
    unlock(c, ios);
    setstate(c, ios, st);
    this
}
fn is_seekg_off<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, off, way) = (c.a(0), c.a64(1) as i64, c.a(2));
    let ios = vb(this);
    let old = state(ios);
    ios_clear(c, ios, old & !EOFBIT, false);
    isentry::<W>(c, ios, true);
    let mut st = 0;
    if !fail(ios) && seekoff(c, rp(ios + STRBUF), off, way, IN) == -1 {
        st |= FAILBIT;
    }
    unlock(c, ios);
    setstate(c, ios, st);
    this
}
fn is_tellg<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, ret) = (c.a(0), c.a(1));
    let ios = vb(this);
    isentry::<W>(c, ios, true);
    put_pos(ret, -1);
    if !fail(ios) {
        let sb = rp(ios + STRBUF);
        vcall(c, sb, V_SEEKOFF, &[wide(ret), 0, SEEK_CUR as u64, IN as u64]);
    }
    unlock(c, ios);
    ret
}
/// operator>>(basic_streambuf*): copies until end of file
fn is_to_sb<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, dst) = (c.a(0), c.a(1));
    let ios = vb(this);
    w64(this + CHCOUNT, 0);
    let mut st = 0;
    if isentry::<W>(c, ios, true) && dst != 0 {
        let sb = rp(ios + STRBUF);
        let mut m = sgetc::<W>(c, sb);
        loop {
            if m == eof::<W>() {
                st |= EOFBIT;
                break;
            }
            if sputc::<W>(c, dst, m) == eof::<W>() {
                break;
            }
            chcount_inc(this);
            m = snextc::<W>(c, sb);
        }
    }
    unlock(c, ios);
    if r64(this + CHCOUNT) == 0 {
        st |= FAILBIT;
    }
    setstate(c, ios, st);
    this
}

/// The characters of the field num_get reads, from the stream buffer (sgetc / snextc).
struct In<'a, 'b, const W: bool> {
    c: &'a mut Ctx<'b>,
    sb: u32,
    cur: u32,
}
impl<const W: bool> In<'_, '_, W> {
    fn peek(&self) -> Option<u32> {
        (self.cur != eof::<W>()).then_some(self.cur)
    }
    fn next(&mut self) {
        self.cur = snextc::<W>(self.c, self.sb);
    }
    fn is(&self, b: u8) -> bool {
        self.peek() == Some(b as u32)
    }
}
const SRC: &[u8] = b"0123456789ABCDEFabcdef-+XxPp";
fn atom(v: u32, limit: usize) -> Option<usize> {
    SRC.iter().take(limit).position(|&b| b as u32 == v)
}
/// _Parse_int_with_locale ("C" locale): the field and its base (None: no digits)
fn parse_int<const W: bool>(r: &mut In<W>, fl: u32) -> (Vec<u8>, Option<u32>) {
    let mut ac = Vec::new();
    if r.is(b'+') {
        ac.push(b'+');
        r.next();
    } else if r.is(b'-') {
        ac.push(b'-');
        r.next();
    }
    let mut base = match fl & BASEFIELD {
        OCT => 8,
        HEX => 16,
        0 => 0,
        _ => 10,
    };
    let mut seen = false;
    let mut nonzero = false;
    if r.is(b'0') {
        seen = true;
        r.next();
        if (r.is(b'x') || r.is(b'X')) && (base == 0 || base == 16) {
            base = 16;
            seen = false;
            r.next();
        } else if base == 0 {
            base = 8;
        }
    }
    let dlen = match base {
        0 | 10 => 10,
        8 => 8,
        _ => 22,
    };
    while let Some(v) = r.peek() {
        let Some(i) = atom(v, dlen) else { break };
        let d = SRC[i];
        if (nonzero || d != b'0') && ac.len() < 31 {
            ac.push(d);
            nonzero = true;
        }
        seen = true;
        r.next();
    }
    if !seen {
        return (ac, None);
    }
    if !nonzero {
        ac.push(b'0');
    }
    (ac, Some(base))
}
/// _Parse_fp_with_locale ("C" locale): the field for strtod (None: no digits)
fn parse_fp<const W: bool>(r: &mut In<W>) -> Option<Vec<u8>> {
    const MAX_SIG: i32 = 768;
    let mut ac = Vec::new();
    if r.is(b'+') {
        ac.push(b'+');
        r.next();
    } else if r.is(b'-') {
        ac.push(b'-');
        r.next();
    }
    ac.push(b'0');
    let mut hex = false;
    let mut seen = false;
    if r.is(b'0') {
        r.next();
        if r.peek().is_none() {
            return Some(ac);
        }
        if r.is(b'x') || r.is(b'X') {
            hex = true;
            r.next();
            ac.push(b'x');
        } else {
            seen = true;
        }
    }
    let mut lost = false;
    let mut sig = 0;
    let mut pow: i64 = 0;
    let dend = if hex { 22 } else { 10 };
    while let Some(i) = r.peek().and_then(|v| atom(v, dend)) {
        if sig >= MAX_SIG {
            pow += 1;
            if i > 0 {
                lost = true;
            }
        } else if i != 0 || sig != 0 {
            ac.push(SRC[i]);
            sig += 1;
        }
        seen = true;
        r.next();
    }
    if hex && seen && sig == 0 {
        ac.push(b'0');
    }
    if r.is(b'.') {
        ac.push(b'.');
        r.next();
    }
    if sig == 0 {
        while r.is(b'0') {
            pow -= 1;
            seen = true;
            r.next();
        }
    }
    while let Some(i) = r.peek().and_then(|v| atom(v, dend)) {
        if sig < MAX_SIG {
            ac.push(SRC[i]);
            sig += 1;
        } else if i > 0 {
            lost = true;
        }
        seen = true;
        r.next();
    }
    if lost {
        let k = if ac.last() == Some(&b'.') { ac.len() - 2 } else { ac.len() - 1 };
        if ac[k] == b'0' || ac[k] == if hex { b'8' } else { b'5' } {
            ac[k] += 1;
        }
    }
    let (lo, up) = if hex { (b'p', b'P') } else { (b'e', b'E') };
    let mut exp: i64 = 0;
    if seen && (r.is(lo) || r.is(up)) {
        r.next();
        seen = false;
        let mut neg = false;
        if r.is(b'+') {
            r.next();
        } else if r.is(b'-') {
            neg = true;
            r.next();
        }
        while r.is(b'0') {
            seen = true;
            r.next();
        }
        while let Some(i) = r.peek().and_then(|v| atom(v, 10)) {
            exp = if exp < i64::MAX / 10 || (exp == i64::MAX / 10 && i as i64 <= i64::MAX % 10) { exp * 10 + i as i64 } else { i64::MAX };
            seen = true;
            r.next();
        }
        if neg {
            exp = -exp;
        }
    }
    if !seen {
        return None;
    }
    let (bound, rep_bound) = if hex { (4200i64, 1050i64) } else { (1100, 1100) };
    let mut adj = pow;
    loop {
        if exp >= 0 && adj >= 0 && (exp >= bound || adj >= rep_bound) {
            exp = bound;
            break;
        } else if exp <= 0 && adj <= 0 && (exp <= -bound || adj <= -rep_bound) {
            exp = -bound;
            break;
        } else if exp.abs() <= bound && adj.abs() <= rep_bound {
            exp += if hex { adj * 4 } else { adj };
            exp = exp.clamp(-bound, bound);
            break;
        } else {
            let up = if hex { (exp.abs() - 1) / 4 + 1 } else { exp.abs() };
            let k = up.min(pow.abs());
            if exp >= 0 {
                exp -= if hex { k * 4 } else { k };
                adj += k;
            } else {
                exp += if hex { k * 4 } else { k };
                adj -= k;
            }
        }
    }
    if exp != 0 {
        ac.push(if hex { b'p' } else { b'e' });
        ac.extend_from_slice(exp.to_string().as_bytes());
    }
    Some(ac)
}

/// What a numeric extraction stores.
#[derive(Clone, Copy)]
enum Num {
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    I64,
    U64,
    Bool,
    Float,
    Double,
    Ptr,
}
/// operator>>(number&): the sentry, the field parsed and converted as num_get::do_get (and
/// the narrowing basic_istream does for short and int)
fn extract<const W: bool>(c: &mut Ctx, kind: Num) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    let ios = vb(this);
    let mut st = 0;
    if isentry::<W>(c, ios, false) {
        let sb = rp(ios + STRBUF);
        let cur = sgetc::<W>(c, sb);
        let fl = flags(ios);
        let mut r = In::<W> { c, sb, cur };
        st = get_num(&mut r, fl, kind, p);
        if r.peek().is_none() {
            st |= EOFBIT;
        }
        let c = r.c;
        unlock(c, ios);
        setstate(c, ios, st);
    } else {
        unlock(c, ios);
        setstate(c, ios, st);
    }
    this
}
fn get_num<const W: bool>(r: &mut In<W>, fl: u32, kind: Num, p: u32) -> u32 {
    use crt::num::{strtod, strtol, strtoul, Float};
    match kind {
        Num::Float | Num::Double => {
            let Some(ac) = parse_fp(r) else {
                if matches!(kind, Num::Float) { w32(p, 0) } else { w64(p, 0) }
                return FAILBIT;
            };
            let (v, used, err) = strtod(&ac, if matches!(kind, Num::Float) { Float::F32 } else { Float::F64 }, false);
            match kind {
                Num::Float => w32(p, (v as f32).to_bits()),
                _ => w64(p, v.to_bits()),
            }
            if used == 0 || err != 0 { FAILBIT } else { 0 }
        }
        Num::Bool if fl & BOOLALPHA != 0 => {
            // "false" / "true": the longest match of the two names
            let names: [&[u8]; 2] = [b"false", b"true"];
            let mut alive = [true, true];
            let mut k = 0;
            let mut hit = None;
            loop {
                for (i, n) in names.iter().enumerate() {
                    if alive[i] && k == n.len() {
                        hit = Some(i);
                        alive[i] = false;
                    }
                }
                let Some(v) = r.peek() else { break };
                let mut any = false;
                for (i, n) in names.iter().enumerate() {
                    alive[i] = alive[i] && k < n.len() && n[k] as u32 == v;
                    any |= alive[i];
                }
                if !any {
                    break;
                }
                k += 1;
                r.next();
            }
            if alive.iter().any(|&a| a) {
                hit = None;
            }
            match hit {
                Some(i) => {
                    w8(p, i as u8);
                    0
                }
                None => {
                    w8(p, 0);
                    FAILBIT
                }
            }
        }
        _ => {
            let fl = if matches!(kind, Num::Ptr) { HEX } else { fl };
            let (ac, base) = parse_int(r, fl);
            let Some(base) = base else {
                match kind {
                    Num::Short | Num::UShort => w16(p, 0),
                    Num::Bool => w8(p, 0),
                    Num::I64 | Num::U64 | Num::Ptr => w64(p, 0),
                    _ => w32(p, 0),
                }
                return FAILBIT;
            };
            let signed = |bits: u32| strtol(&ac, base, bits);
            match kind {
                Num::Short | Num::Int | Num::Long | Num::Bool => {
                    let (v, used, err) = signed(32);
                    let bad = used == 0 || err != 0;
                    match kind {
                        Num::Short => {
                            let (x, f) = if bad {
                                (v, true)
                            } else if v < i16::MIN as i64 {
                                (i16::MIN as i64, true)
                            } else if v > i16::MAX as i64 {
                                (i16::MAX as i64, true)
                            } else {
                                (v, false)
                            };
                            w16(p, x as u16);
                            if f { FAILBIT } else { 0 }
                        }
                        Num::Bool => {
                            if bad {
                                w8(p, 1);
                                FAILBIT
                            } else {
                                w8(p, (v != 0) as u8);
                                if v != 0 && v != 1 { FAILBIT } else { 0 }
                            }
                        }
                        _ => {
                            w32(p, v as u32);
                            if bad { FAILBIT } else { 0 }
                        }
                    }
                }
                Num::UShort => {
                    let minus = ac.first() == Some(&b'-');
                    let digits = if minus { &ac[1..] } else { &ac[..] };
                    let (v, used, err) = strtoul(digits, base, 32);
                    if used == 0 || err != 0 || v > 0xffff {
                        w16(p, 0xffff);
                        FAILBIT
                    } else {
                        w16(p, if minus { (v as u16).wrapping_neg() } else { v as u16 });
                        0
                    }
                }
                Num::UInt | Num::ULong => {
                    let (v, used, err) = strtoul(&ac, base, 32);
                    w32(p, v as u32);
                    if used == 0 || err != 0 { FAILBIT } else { 0 }
                }
                Num::I64 => {
                    let (v, used, err) = signed(64);
                    w64(p, v as u64);
                    if used == 0 || err != 0 { FAILBIT } else { 0 }
                }
                _ => {
                    let (v, used, err) = strtoul(&ac, base, 64);
                    if matches!(kind, Num::Ptr) && (used == 0 || err != 0) {
                        w64(p, 0);
                        return FAILBIT;
                    }
                    w64(p, v);
                    if used == 0 || err != 0 { FAILBIT } else { 0 }
                }
            }
        }
    }
}
macro_rules! extractor {
    ($name:ident, $kind:expr) => {
        fn $name<const W: bool>(c: &mut Ctx) -> u32 {
            extract::<W>(c, $kind)
        }
    };
}
extractor!(is_short, Num::Short);
extractor!(is_ushort, Num::UShort);
extractor!(is_int, Num::Int);
extractor!(is_uint, Num::UInt);
extractor!(is_long, Num::Long);
extractor!(is_ulong, Num::ULong);
extractor!(is_i64, Num::I64);
extractor!(is_u64, Num::U64);
extractor!(is_bool, Num::Bool);
extractor!(is_float, Num::Float);
extractor!(is_double, Num::Double);
extractor!(is_ptr, Num::Ptr);

// ---------------------------------------------------------------------------------------------
// the standard streams: cin, cout, cerr, clog and the wide ones on console buffers

/// The standard streams' buffers: like MS STL's filebuf over _cpp_stdin / _cpp_stdout /
/// _cpp_stderr (cout.cpp, cin.cpp ...), each character through the CRT's FILE (so a program's
/// freopen of stdout carries cout along, and cout and printf keep their order). { streambuf,
/// int stdio index @SB_SIZE }. The wide ones convert with the "C" locale's codecvt<wchar_t,
/// char>: a byte is its own code and codes above 255 do not convert (a write fails).
const STDIO_FD: u32 = SB_SIZE;
fn stdio_file(c: &mut Ctx, this: u32) -> u64 {
    let f = crate::api::export("ucrtbase", "__acrt_iob_func");
    call(c, f, &[r32(this + STDIO_FD) as u64])
}
fn crt(c: &mut Ctx, name: &str, args: &[u64]) -> i32 {
    let f = crate::api::export("ucrtbase", name);
    call(c, f, args) as i32
}
fn stdio_overflow<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1));
    let v = if W { v & 0xffff } else { v };
    if v == eof::<W>() {
        // (not eof: nothing to write)
        return 0;
    }
    if W && v > 255 {
        return eof::<W>();
    }
    let f = stdio_file(c, this);
    if crt(c, "fputc", &[(v & 0xff) as u64, f]) == -1 { eof::<W>() } else { v }
}
/// The next byte, left in the FILE (underflow) or taken (uflow)
fn stdio_get<const W: bool>(c: &mut Ctx, take: bool) -> u32 {
    let this = c.a(0);
    let f = stdio_file(c, this);
    let ch = crt(c, "fgetc", &[f]);
    if ch == -1 {
        return eof::<W>();
    }
    if !take {
        crt(c, "ungetc", &[ch as u32 as u64, f]);
    }
    ch as u32 & 0xff
}
fn stdio_underflow<const W: bool>(c: &mut Ctx) -> u32 {
    stdio_get::<W>(c, false)
}
fn stdio_uflow<const W: bool>(c: &mut Ctx) -> u32 {
    stdio_get::<W>(c, true)
}
fn stdio_pbackfail<const W: bool>(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1));
    let v = if W { v & 0xffff } else { v };
    if v == eof::<W>() || (W && v > 255) {
        return eof::<W>();
    }
    let f = stdio_file(c, this);
    if crt(c, "ungetc", &[(v & 0xff) as u64, f]) == -1 { eof::<W>() } else { v }
}
fn stdio_sync(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let f = stdio_file(c, this);
    if crt(c, "fflush", &[f]) == 0 { 0 } else { u32::MAX }
}
fn stdio_vtable<const W: bool>() -> u32 {
    static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
    *V[W as usize].get_or_init(|| {
        table(
            if W { "wstdiobuf" } else { "stdiobuf" },
            &[
                sb_delete::<W>,
                nothing,
                nothing,
                stdio_overflow::<W>,
                stdio_pbackfail::<W>,
                sb_zero,
                stdio_underflow::<W>,
                stdio_uflow::<W>,
                sb_xsgetn::<W>,
                sb_xsputn::<W>,
                sb_seekoff,
                sb_seekpos,
                sb_setbuf,
                stdio_sync,
                nothing,
            ],
        )
    })
}
fn new_stdiobuf<const W: bool>(fd: u32) -> u32 {
    let sb = mem::heap_alloc(SB_SIZE + 8, true);
    wq(sb, stdio_vtable::<W>());
    wq(sb + PLOCALE, crate::msvcp::new_locale());
    sb_init::<W>(sb);
    w32(sb + STDIO_FD, fd);
    sb
}

/// The standard streams, made on first use: 0 cout, 1 cerr, 2 cin, 3 clog, then the wide ones
/// (4 + the same). cin and cerr are tied to cout, cerr is unit-buffered.
fn std_stream(i: usize) -> u32 {
    static S: OnceLock<[u32; 8]> = OnceLock::new();
    S.get_or_init(|| {
        let mut s = [0; 8];
        for (k, v) in s.iter_mut().enumerate() {
            *v = make_std(k)
        }
        // ties and flags
        for w in [0, 4] {
            for k in [1, 2] {
                wq(vb(s[w + k]) + TIESTR, s[w]);
            }
            let e = vb(s[w + 1]);
            w32(e + FMTFL, flags(e) | UNITBUF);
        }
        s
    })[i]
}
fn make_std(k: usize) -> u32 {
    let wide_ = k >= 4;
    let input = k % 4 == 2;
    let (size, vbt, vft, st) = match (input, wide_) {
        (true, false) => (ISTREAM_VB + IOS_SIZE, istream_vbtable::<false>(), istream_vtable::<false>(), ISTREAM_VB),
        (true, true) => (ISTREAM_VB + IOS_SIZE, istream_vbtable::<true>(), istream_vtable::<true>(), ISTREAM_VB),
        (false, false) => (OSTREAM_VB + IOS_SIZE, ostream_vbtable::<false>(), ostream_vtable::<false>(), OSTREAM_VB),
        (false, true) => (OSTREAM_VB + IOS_SIZE, ostream_vbtable::<true>(), ostream_vtable::<true>(), OSTREAM_VB),
    };
    let o = mem::heap_alloc(size, true);
    let ios = if wide_ { stream_begin::<true>(o, vbt, true, vft, st) } else { stream_begin::<false>(o, vbt, true, vft, st) };
    // (cout: stdout, cerr and clog: stderr, cin: stdin)
    let fd = [1, 2, 0, 2][k % 4];
    let sb = if wide_ { new_stdiobuf::<true>(fd) } else { new_stdiobuf::<false>(fd) };
    // (basic_ios::init without a Ctx: nothing can throw or call back here)
    w32(ios + FMTFL, SKIPWS | DEC);
    w64(ios + PREC, 6);
    wq(ios + PLOC, crate::msvcp::new_locale());
    wq(ios + STRBUF, sb);
    if wide_ { w16(ios + FILLCH, b' ' as u16) } else { w8(ios + FILLCH, b' ') };
    addstd(ios);
    o
}
fn ptr_cell(i: usize) -> u32 {
    static P: OnceLock<[u32; 8]> = OnceLock::new();
    P.get_or_init(|| {
        let mut s = [0; 8];
        for (k, v) in s.iter_mut().enumerate() {
            *v = mem::heap_alloc(8, true);
            wq(*v, std_stream(k));
        }
        s
    })[i]
}

// ---------------------------------------------------------------------------------------------
// <iomanip>'s manipulators (iomanip.cpp): _Smanip { void (*)(ios_base&, arg), arg } into the
// hidden result

fn swfun(c: &mut Ctx) -> u32 {
    w64(c.a(0) + WIDE, c.a64(1));
    0
}
fn spfun(c: &mut Ctx) -> u32 {
    w64(c.a(0) + PREC, c.a64(1));
    0
}
fn sbfun(c: &mut Ctx) -> u32 {
    let (ios, b) = (c.a(0), c.a(1));
    let f = match b {
        8 => OCT,
        10 => DEC,
        16 => HEX,
        _ => 0,
    };
    w32(ios + FMTFL, (flags(ios) & !BASEFIELD) | f);
    0
}
fn sifun(c: &mut Ctx) -> u32 {
    let (ios, m) = (c.a(0), c.a(1));
    w32(ios + FMTFL, (flags(ios) & !m) | (FMTMASK & m));
    0
}
fn rsfun(c: &mut Ctx) -> u32 {
    let (ios, m) = (c.a(0), c.a(1));
    w32(ios + FMTFL, flags(ios) & !m);
    0
}
fn smanip(c: &mut Ctx, name: &str, f: Handler) -> u32 {
    let (ret, v) = (c.a(0), c.a64(1));
    wq(ret, host_fn(name, f, 0));
    w64(ret + 8, v);
    ret
}
fn setw(c: &mut Ctx) -> u32 {
    smanip(c, "msvcp::swfun", swfun)
}
fn setprecision(c: &mut Ctx) -> u32 {
    smanip(c, "msvcp::spfun", spfun)
}
fn setbase(c: &mut Ctx) -> u32 {
    smanip(c, "msvcp::sbfun", sbfun)
}
fn setiosflags(c: &mut Ctx) -> u32 {
    smanip(c, "msvcp::sifun", sifun)
}
fn resetiosflags(c: &mut Ctx) -> u32 {
    smanip(c, "msvcp::rsfun", rsfun)
}

// ---------------------------------------------------------------------------------------------
// _Fiopen

/// _Fiopen(name, openmode, prot): fopen's mode for the openmode (fiopen.cpp), _fsopen /
/// _wfsopen, at the end for ate.
fn fiopen(c: &mut Ctx, wide_name: bool) -> u32 {
    const VALID: [u32; 20] = [1, 2, 0x12, 0xa, 0x21, 0x22, 0x32, 0x2a, 3, 0x13, 0xb, 0x23, 0x33, 0x2b, 0x82, 0x92, 0x93, 0xa2, 0xb2, 0xb3];
    const MODS: [&str; 20] = ["r", "w", "w", "a", "rb", "wb", "wb", "ab", "r+", "w+", "a+", "r+b", "w+b", "a+b", "wx", "wx", "w+x", "wbx", "wbx", "w+bx"];
    let (name, mut mode, prot) = (c.a(0), c.a(1), c.a(2));
    let at_end = mode & 4 != 0;
    if mode & 0x40 != 0 {
        mode |= IN;
    }
    if mode & 8 != 0 {
        mode |= OUT;
    }
    mode &= !(4 | 0x40);
    let Some(n) = VALID.iter().position(|&v| v == mode) else { return 0 };
    let m = MODS[n];
    let ms = if wide_name {
        let p = mem::heap_alloc(2 * m.len() as u32 + 2, true);
        for (i, b) in m.bytes().enumerate() {
            w16(p + 2 * i as u32, b as u16);
        }
        p
    } else {
        static_str(m)
    };
    let open = crate::api::export("ucrtbase", if wide_name { "_wfsopen" } else { "_fsopen" });
    let fp = call(c, open, &[wide(name), wide(ms), prot as u64]) as u32;
    mem::heap_free(ms);
    if fp == 0 {
        return 0;
    }
    if at_end {
        let seek = crate::api::export("ucrtbase", "fseek");
        if call(c, seek, &[wide(fp), 0, 2]) as u32 != 0 {
            let close = crate::api::export("ucrtbase", "fclose");
            call(c, close, &[wide(fp)]);
            return 0;
        }
    }
    fp
}
fn Fiopen_a(c: &mut Ctx) -> u32 {
    fiopen(c, false)
}
fn Fiopen_w(c: &mut Ctx) -> u32 {
    fiopen(c, true)
}

// ---------------------------------------------------------------------------------------------
// data exports and registration

/// The iostream data exports: the standard streams, their pointers, the vftables and vbtables,
/// ios_base's statics.
pub fn data(name: &str) -> Option<u32> {
    const STD: [&str; 4] = ["cout", "cerr", "cin", "clog"];
    for (k, s) in STD.iter().enumerate() {
        for (w, e) in [(0, "D"), (4, "_W")] {
            let cls = if *s == "cin" { "basic_istream" } else { "basic_ostream" };
            let pre = if w == 0 { "" } else { "w" };
            if name == format!("?{pre}{s}@std@@3V?${cls}@{e}U?$char_traits@{e}@std@@@1@A") {
                return Some(std_stream(w + k));
            }
            if name == format!("?_Ptr_{pre}{s}@std@@3PEAV?${cls}@{e}U?$char_traits@{e}@std@@@1@EA") {
                return Some(ptr_cell(w + k));
            }
        }
    }
    for (e, w) in [("D", false), ("_W", true)] {
        let t = format!("{e}U?$char_traits@{e}@std@@@std@@");
        let v = |cls: &str| format!("??_7?$basic_{cls}@{t}6B@");
        let table_of = Some;
        if name == v("ios") {
            return table_of(if w { ios_vtable::<true>() } else { ios_vtable::<false>() });
        }
        if name == v("streambuf") {
            return table_of(if w { sb_vtable::<true>() } else { sb_vtable::<false>() });
        }
        if name == v("istream") {
            return table_of(if w { istream_vtable::<true>() } else { istream_vtable::<false>() });
        }
        if name == v("ostream") {
            return table_of(if w { ostream_vtable::<true>() } else { ostream_vtable::<false>() });
        }
        if name == v("iostream") {
            return table_of(if w { iostream_vtable::<true>() } else { iostream_vtable::<false>() });
        }
        if name == format!("??_8?$basic_istream@{t}7B@") {
            return Some(if w { istream_vbtable::<true>() } else { istream_vbtable::<false>() });
        }
        if name == format!("??_8?$basic_ostream@{t}7B@") {
            return Some(if w { ostream_vbtable::<true>() } else { ostream_vbtable::<false>() });
        }
        let (vi, vo) = if w { iostream_vbtables::<true>() } else { iostream_vbtables::<false>() };
        if name == format!("??_8?$basic_iostream@{t}7B?$basic_istream@{t}1@@") {
            return Some(vi);
        }
        if name == format!("??_8?$basic_iostream@{t}7B?$basic_ostream@{t}1@@") {
            return Some(vo);
        }
    }
    match name {
        "??_7ios_base@std@@6B@" => Some(ios_base_vtable()),
        "?_Init_cnt@Init@ios_base@std@@0HA" => Some(init_cnt()),
        "?_Index@ios_base@std@@0HA" | "?_Sync@ios_base@std@@0_NA" => {
            static P: OnceLock<u32> = OnceLock::new();
            let p = *P.get_or_init(|| {
                let p = mem::heap_alloc(8, true);
                w8(p + 4, 1);
                p
            });
            Some(if name.contains("_Sync") { p + 4 } else { p })
        }
        _ => None,
    }
}

/// The member functions of the four stream templates for element `e` (D / _W, int_type `i`).
fn members<const W: bool>() -> Vec<(String, Handler)> {
    let (e, i) = if W { ("_W", "G") } else { ("D", "H") };
    let t = format!("{e}U?$char_traits@{e}@std@@@std@@");
    let sb = format!("?$basic_streambuf@{e}U?$char_traits@{e}@std@@@");
    let ios = format!("?$basic_ios@{t}");
    let is = format!("?$basic_istream@{t}");
    let os = format!("?$basic_ostream@{t}");
    let io = format!("?$basic_iostream@{t}");
    let s = format!("?$basic_streambuf@{t}");
    let fpos = "?AV?$fpos@U_Mbstatet@@@2@";
    let l: Vec<(String, Handler)> = vec![
        // basic_streambuf
        (format!("??0{s}IEAA@XZ"), sb_ctor::<W>),
        (format!("??0{s}IEAA@W4_Uninitialized@1@@Z"), sb_ctor_uninit),
        (format!("??0{s}IEAA@AEBV01@@Z"), sb_ctor_copy::<W>),
        (format!("??1{s}UEAA@XZ"), sb_dtor),
        (format!("??4{s}IEAAAEAV01@AEBV01@@Z"), sb_assign::<W>),
        (format!("?swap@{s}IEAAXAEAV12@@Z"), sb_swap::<W>),
        (format!("?_Gnavail@{s}IEBA_JXZ"), sb_Gnavail),
        (format!("?_Gndec@{s}IEAAPEA{e}XZ"), sb_Gndec::<W>),
        (format!("?_Gninc@{s}IEAAPEA{e}XZ"), sb_Gninc::<W>),
        (format!("?_Gnpreinc@{s}IEAAPEA{e}XZ"), sb_Gnpreinc::<W>),
        (format!("?_Init@{s}IEAAXPEAPEA{e}0PEAH001@Z"), sb_Init6),
        (format!("?_Init@{s}IEAAXXZ"), sb_Init0::<W>),
        (format!("?_Lock@{s}UEAAXXZ"), nothing),
        (format!("?_Unlock@{s}UEAAXXZ"), nothing),
        (format!("?_Pnavail@{s}IEBA_JXZ"), sb_Pnavail),
        (format!("?_Pninc@{s}IEAAPEA{e}XZ"), sb_Pninc::<W>),
        (format!("?eback@{s}IEBAPEA{e}XZ"), sb_eback),
        (format!("?egptr@{s}IEBAPEA{e}XZ"), sb_egptr::<W>),
        (format!("?epptr@{s}IEBAPEA{e}XZ"), sb_epptr::<W>),
        (format!("?gptr@{s}IEBAPEA{e}XZ"), sb_gptr),
        (format!("?pbase@{s}IEBAPEA{e}XZ"), sb_pbase),
        (format!("?pptr@{s}IEBAPEA{e}XZ"), sb_pptr),
        (format!("?gbump@{s}IEAAXH@Z"), sb_gbump::<W>),
        (format!("?pbump@{s}IEAAXH@Z"), sb_pbump::<W>),
        (format!("?setg@{s}IEAAXPEA{e}00@Z"), sb_setg::<W>),
        (format!("?setp@{s}IEAAXPEA{e}00@Z"), sb_setp3::<W>),
        (format!("?setp@{s}IEAAXPEA{e}0@Z"), sb_setp2::<W>),
        (format!("?getloc@{s}QEBA?AVlocale@2@XZ"), sb_getloc),
        (format!("?pubimbue@{s}QEAA?AVlocale@2@AEBV32@@Z"), sb_pubimbue),
        (format!("?imbue@{s}MEAAXAEBVlocale@2@@Z"), nothing),
        (format!("?in_avail@{s}QEAA_JXZ"), sb_in_avail),
        (format!("?overflow@{s}MEAA{i}{i}@Z"), sb_eof::<W>),
        (format!("?pbackfail@{s}MEAA{i}{i}@Z"), sb_eof::<W>),
        (format!("?underflow@{s}MEAA{i}XZ"), sb_eof::<W>),
        (format!("?uflow@{s}MEAA{i}XZ"), sb_uflow::<W>),
        (format!("?showmanyc@{s}MEAA_JXZ"), sb_zero),
        (format!("?xsgetn@{s}MEAA_JPEA{e}_J@Z"), sb_xsgetn::<W>),
        (format!("?xsputn@{s}MEAA_JPEB{e}_J@Z"), sb_xsputn::<W>),
        (format!("?seekoff@{s}MEAA{fpos}_JHH@Z"), sb_seekoff),
        (format!("?seekpos@{s}MEAA{fpos}V32@H@Z"), sb_seekpos),
        (format!("?setbuf@{s}MEAAPEAV12@PEA{e}_J@Z"), sb_setbuf),
        (format!("?sync@{s}MEAAHXZ"), sb_zero),
        (format!("?pubseekoff@{s}QEAA{fpos}_JHH@Z"), sb_pubseekoff),
        (format!("?pubseekoff@{s}QEAA{fpos}_JII@Z"), sb_pubseekoff),
        (format!("?pubseekpos@{s}QEAA{fpos}V32@H@Z"), sb_pubseekpos),
        (format!("?pubseekpos@{s}QEAA{fpos}V32@I@Z"), sb_pubseekpos),
        (format!("?pubsetbuf@{s}QEAAPEAV12@PEA{e}_J@Z"), sb_pubsetbuf),
        (format!("?pubsync@{s}QEAAHXZ"), sb_pubsync),
        (format!("?sbumpc@{s}QEAA{i}XZ"), sb_sbumpc::<W>),
        (format!("?sgetc@{s}QEAA{i}XZ"), sb_sgetc::<W>),
        (format!("?snextc@{s}QEAA{i}XZ"), sb_snextc::<W>),
        (format!("?sgetn@{s}QEAA_JPEA{e}_J@Z"), sb_sgetn::<W>),
        (format!("?sputbackc@{s}QEAA{i}{e}@Z"), sb_sputbackc::<W>),
        (format!("?sungetc@{s}QEAA{i}XZ"), sb_sungetc::<W>),
        (format!("?stossc@{s}QEAAXXZ"), sb_stossc::<W>),
        (format!("?sputc@{s}QEAA{i}{e}@Z"), sb_sputc::<W>),
        (format!("?sputn@{s}QEAA_JPEB{e}_J@Z"), sb_sputn::<W>),
        // basic_ios
        (format!("??0{ios}IEAA@XZ"), ios_ctor::<W>),
        (format!("??0{ios}QEAA@PEAV{sb}1@@Z"), ios_ctor_sb::<W>),
        (format!("??1{ios}UEAA@XZ"), ios_dtor),
        (format!("?clear@{ios}QEAAXH_N@Z"), ios_clear2),
        (format!("?clear@{ios}QEAAXI@Z"), ios_clear1),
        (format!("?setstate@{ios}QEAAXH_N@Z"), ios_setstate2),
        (format!("?setstate@{ios}QEAAXI@Z"), ios_setstate1),
        (format!("?copyfmt@{ios}QEAAAEAV12@AEBV12@@Z"), ios_copyfmt::<W>),
        (format!("?fill@{ios}QEAA{e}{e}@Z"), ios_fill_set::<W>),
        (format!("?fill@{ios}QEBA{e}XZ"), ios_fill_get::<W>),
        (format!("?imbue@{ios}QEAA?AVlocale@2@AEBV32@@Z"), ios_imbue),
        (format!("?init@{ios}IEAAXPEAV{sb}2@_N@Z"), ios_init_fn::<W>),
        (format!("?move@{ios}QEAAX$$QEAV12@@Z"), ios_move::<W>),
        (format!("?move@{ios}QEAAXAEAV12@@Z"), ios_move::<W>),
        (format!("?narrow@{ios}QEBAD{e}D@Z"), ios_narrow::<W>),
        (format!("?widen@{ios}QEBA{e}D@Z"), ios_widen),
        (format!("?rdbuf@{ios}QEAAPEAV{sb}2@PEAV32@@Z"), ios_rdbuf_set),
        (format!("?rdbuf@{ios}QEBAPEAV{sb}2@XZ"), ios_rdbuf_get),
        (format!("?set_rdbuf@{ios}QEAAXPEAV{sb}2@@Z"), ios_set_rdbuf),
        (format!("?swap@{ios}QEAAXAEAV12@@Z"), ios_swap::<W>),
        (format!("?tie@{ios}QEAAPEAV?$basic_ostream@{e}U?$char_traits@{e}@std@@@2@PEAV32@@Z"), ios_tie_set),
        (format!("?tie@{ios}QEBAPEAV?$basic_ostream@{e}U?$char_traits@{e}@std@@@2@XZ"), ios_tie_get),
        // basic_istream
        (format!("??0{is}QEAA@PEAV{sb}1@_N@Z"), istream_ctor::<W>),
        (format!("??0{is}QEAA@PEAV{sb}1@_N1@Z"), istream_ctor3::<W>),
        (format!("??0{is}QEAA@W4_Uninitialized@1@@Z"), istream_ctor_uninit::<W>),
        (format!("??0{is}IEAA@$$QEAV01@@Z"), istream_ctor_move::<W>),
        (format!("??1{is}UEAA@XZ"), stream_dtor),
        (format!("??_D{is}QEAAXXZ"), stream_vbase_dtor),
        (format!("??4{is}IEAAAEAV01@$$QEAV01@@Z"), istream_swap::<W>),
        (format!("?swap@{is}IEAAXAEAV12@@Z"), istream_swap::<W>),
        (format!("?_Ipfx@{is}QEAA_N_N@Z"), is_Ipfx::<W>),
        (format!("?ipfx@{is}QEAA_N_N@Z"), is_Ipfx::<W>),
        (format!("?isfx@{is}QEAAXXZ"), is_isfx),
        (format!("?gcount@{is}QEBA_JXZ"), is_gcount),
        (format!("?get@{is}QEAA{i}XZ"), is_get0::<W>),
        (format!("?get@{is}QEAAAEAV12@AEA{e}@Z"), is_get_ch::<W>),
        (format!("?get@{is}QEAAAEAV12@PEA{e}_J@Z"), is_get_str2::<W>),
        (format!("?get@{is}QEAAAEAV12@PEA{e}_J{e}@Z"), is_get_str3::<W>),
        (format!("?get@{is}QEAAAEAV12@AEAV{sb}2@@Z"), is_get_sb1::<W>),
        (format!("?get@{is}QEAAAEAV12@AEAV{sb}2@{e}@Z"), is_get_sb2::<W>),
        (format!("?getline@{is}QEAAAEAV12@PEA{e}_J@Z"), is_getline2::<W>),
        (format!("?getline@{is}QEAAAEAV12@PEA{e}_J{e}@Z"), is_getline3::<W>),
        (format!("?ignore@{is}QEAAAEAV12@_J{i}@Z"), is_ignore::<W>),
        (format!("?read@{is}QEAAAEAV12@PEA{e}_J@Z"), is_read::<W>),
        (format!("?readsome@{is}QEAA_JPEA{e}_J@Z"), is_readsome::<W>),
        (format!("?peek@{is}QEAA{i}XZ"), is_peek::<W>),
        (format!("?putback@{is}QEAAAEAV12@{e}@Z"), is_putback::<W>),
        (format!("?unget@{is}QEAAAEAV12@XZ"), is_unget::<W>),
        (format!("?sync@{is}QEAAHXZ"), is_sync::<W>),
        (format!("?seekg@{is}QEAAAEAV12@V?$fpos@U_Mbstatet@@@2@@Z"), is_seekg_pos::<W>),
        (format!("?seekg@{is}QEAAAEAV12@_JH@Z"), is_seekg_off::<W>),
        (format!("?tellg@{is}QEAA{fpos}XZ"), is_tellg::<W>),
        (format!("??5{is}QEAAAEAV01@AEAF@Z"), is_short::<W>),
        (format!("??5{is}QEAAAEAV01@AEAG@Z"), is_ushort::<W>),
        (format!("??5{is}QEAAAEAV01@AEAH@Z"), is_int::<W>),
        (format!("??5{is}QEAAAEAV01@AEAI@Z"), is_uint::<W>),
        (format!("??5{is}QEAAAEAV01@AEAJ@Z"), is_long::<W>),
        (format!("??5{is}QEAAAEAV01@AEAK@Z"), is_ulong::<W>),
        (format!("??5{is}QEAAAEAV01@AEAM@Z"), is_float::<W>),
        (format!("??5{is}QEAAAEAV01@AEAN@Z"), is_double::<W>),
        (format!("??5{is}QEAAAEAV01@AEAO@Z"), is_double::<W>),
        (format!("??5{is}QEAAAEAV01@AEAPEAX@Z"), is_ptr::<W>),
        (format!("??5{is}QEAAAEAV01@AEA_J@Z"), is_i64::<W>),
        (format!("??5{is}QEAAAEAV01@AEA_K@Z"), is_u64::<W>),
        (format!("??5{is}QEAAAEAV01@AEA_N@Z"), is_bool::<W>),
        (format!("??5{is}QEAAAEAV01@P6AAEAV01@AEAV01@@Z@Z"), os_manip_self),
        (format!("??5{is}QEAAAEAV01@P6AAEAV{ios}1@AEAV21@@Z@Z"), manip_ios),
        (format!("??5{is}QEAAAEAV01@P6AAEAVios_base@1@AEAV21@@Z@Z"), manip_ios),
        (format!("??5{is}QEAAAEAV01@PEAV{sb}1@@Z"), is_to_sb::<W>),
        // basic_ostream
        (format!("??0{os}QEAA@PEAV{sb}1@_N@Z"), ostream_ctor::<W>),
        (format!("??0{os}QEAA@W4_Uninitialized@1@_N@Z"), ostream_ctor_uninit::<W>),
        (format!("??0{os}IEAA@$$QEAV01@@Z"), ostream_ctor_move::<W>),
        (format!("??1{os}UEAA@XZ"), stream_dtor),
        (format!("??_D{os}QEAAXXZ"), stream_vbase_dtor),
        (format!("??4{os}IEAAAEAV01@$$QEAV01@@Z"), ostream_swap::<W>),
        (format!("?swap@{os}IEAAXAEAV12@@Z"), ostream_swap::<W>),
        (format!("?_Osfx@{os}QEAAXXZ"), os_Osfx),
        (format!("?osfx@{os}QEAAXXZ"), os_Osfx),
        (format!("?opfx@{os}QEAA_NXZ"), os_opfx::<W>),
        (format!("?flush@{os}QEAAAEAV12@XZ"), os_flush::<W>),
        (format!("?put@{os}QEAAAEAV12@{e}@Z"), os_put::<W>),
        (format!("?write@{os}QEAAAEAV12@PEB{e}_J@Z"), os_write::<W>),
        (format!("?seekp@{os}QEAAAEAV12@V?$fpos@U_Mbstatet@@@2@@Z"), os_seekp_pos),
        (format!("?seekp@{os}QEAAAEAV12@_JH@Z"), os_seekp_off),
        (format!("?tellp@{os}QEAA{fpos}XZ"), os_tellp),
        (format!("??6{os}QEAAAEAV01@F@Z"), os_short::<W>),
        (format!("??6{os}QEAAAEAV01@G@Z"), os_ushort::<W>),
        (format!("??6{os}QEAAAEAV01@H@Z"), os_int::<W>),
        (format!("??6{os}QEAAAEAV01@I@Z"), os_uint::<W>),
        (format!("??6{os}QEAAAEAV01@J@Z"), os_long::<W>),
        (format!("??6{os}QEAAAEAV01@K@Z"), os_uint::<W>),
        (format!("??6{os}QEAAAEAV01@M@Z"), os_float::<W>),
        (format!("??6{os}QEAAAEAV01@N@Z"), os_double::<W>),
        (format!("??6{os}QEAAAEAV01@O@Z"), os_double::<W>),
        (format!("??6{os}QEAAAEAV01@PEBX@Z"), os_ptr::<W>),
        (format!("??6{os}QEAAAEAV01@_J@Z"), os_i64::<W>),
        (format!("??6{os}QEAAAEAV01@_K@Z"), os_u64::<W>),
        (format!("??6{os}QEAAAEAV01@_N@Z"), os_bool::<W>),
        (format!("??6{os}QEAAAEAV01@P6AAEAV01@AEAV01@@Z@Z"), os_manip_self),
        (format!("??6{os}QEAAAEAV01@P6AAEAV{ios}1@AEAV21@@Z@Z"), manip_ios),
        (format!("??6{os}QEAAAEAV01@P6AAEAVios_base@1@AEAV21@@Z@Z"), manip_ios),
        (format!("??6{os}QEAAAEAV01@PEAV{sb}1@@Z"), os_from_sb::<W>),
        // basic_iostream
        (format!("??0{io}QEAA@PEAV{sb}1@@Z"), iostream_ctor::<W>),
        (format!("??0{io}IEAA@$$QEAV01@@Z"), iostream_ctor_move::<W>),
        (format!("??1{io}UEAA@XZ"), stream_dtor),
        (format!("??_D{io}QEAAXXZ"), stream_vbase_dtor),
        (format!("??4{io}IEAAAEAV01@$$QEAV01@@Z"), istream_swap::<W>),
        (format!("?swap@{io}IEAAXAEAV12@@Z"), istream_swap::<W>),
    ];
    l
}

pub fn init() {
    let base: [(&str, Handler); 47] = [
        ("??0ios_base@std@@IEAA@XZ", ios_base_ctor),
        ("??1ios_base@std@@UEAA@XZ", ios_base_dtor),
        ("?_Ios_base_dtor@ios_base@std@@CAXPEAV12@@Z", Ios_base_dtor),
        ("?_Addstd@ios_base@std@@SAXPEAV12@@Z", Addstd),
        ("?_Init@ios_base@std@@IEAAXXZ", base_Init),
        ("?_Tidy@ios_base@std@@AEAAXXZ", base_Tidy),
        ("?_Callfns@ios_base@std@@AEAAXW4event@12@@Z", base_Callfns),
        ("?_Findarr@ios_base@std@@AEAAAEAU_Iosarray@12@H@Z", base_Findarr),
        ("??7ios_base@std@@QEBA_NXZ", base_not),
        ("??Bios_base@std@@QEBA_NXZ", base_bool),
        ("?bad@ios_base@std@@QEBA_NXZ", base_bad),
        ("?eof@ios_base@std@@QEBA_NXZ", base_eof),
        ("?fail@ios_base@std@@QEBA_NXZ", base_fail),
        ("?good@ios_base@std@@QEBA_NXZ", base_good),
        ("?rdstate@ios_base@std@@QEBAHXZ", base_rdstate),
        ("?clear@ios_base@std@@QEAAXH@Z", base_clear1),
        ("?clear@ios_base@std@@QEAAXH_N@Z", base_clear2),
        ("?clear@ios_base@std@@QEAAXI@Z", base_clear1),
        ("?setstate@ios_base@std@@QEAAXH@Z", base_setstate1),
        ("?setstate@ios_base@std@@QEAAXH_N@Z", base_setstate2),
        ("?setstate@ios_base@std@@QEAAXI@Z", base_setstate1),
        ("?copyfmt@ios_base@std@@QEAAAEAV12@AEBV12@@Z", base_copyfmt),
        ("?exceptions@ios_base@std@@QEAAXH@Z", base_exceptions_set),
        ("?exceptions@ios_base@std@@QEAAXI@Z", base_exceptions_set),
        ("?exceptions@ios_base@std@@QEBAHXZ", base_exceptions_get),
        ("?flags@ios_base@std@@QEAAHH@Z", base_flags_set),
        ("?flags@ios_base@std@@QEBAHXZ", base_flags_get),
        ("?setf@ios_base@std@@QEAAHH@Z", base_setf1),
        ("?setf@ios_base@std@@QEAAHHH@Z", base_setf2),
        ("?unsetf@ios_base@std@@QEAAXH@Z", base_unsetf),
        ("?precision@ios_base@std@@QEAA_J_J@Z", base_precision_set),
        ("?precision@ios_base@std@@QEBA_JXZ", base_precision_get),
        ("?width@ios_base@std@@QEAA_J_J@Z", base_width_set),
        ("?width@ios_base@std@@QEBA_JXZ", base_width_get),
        ("?getloc@ios_base@std@@QEBA?AVlocale@2@XZ", base_getloc),
        ("?imbue@ios_base@std@@QEAA?AVlocale@2@AEBV32@@Z", base_imbue),
        ("?iword@ios_base@std@@QEAAAEAJH@Z", base_iword),
        ("?pword@ios_base@std@@QEAAAEAPEAXH@Z", base_pword),
        ("?register_callback@ios_base@std@@QEAAXP6AXW4event@12@AEAV12@H@ZH@Z", base_register_callback),
        ("?swap@ios_base@std@@QEAAXAEAV12@@Z", base_swap),
        ("?xalloc@ios_base@std@@SAHXZ", base_xalloc),
        ("?sync_with_stdio@ios_base@std@@SA_N_N@Z", base_sync_with_stdio),
        ("??0Init@ios_base@std@@QEAA@XZ", Init_ctor_obj),
        ("??1Init@ios_base@std@@QEAA@XZ", Init_dtor_obj),
        ("?_Init_ctor@Init@ios_base@std@@CAXPEAV123@@Z", Init_ctor),
        ("?_Init_dtor@Init@ios_base@std@@CAXPEAV123@@Z", Init_dtor),
        ("?_Init_cnt_func@Init@ios_base@std@@CAAEAHXZ", Init_cnt_func),
    ];
    for (n, f) in base {
        implement("msvcp140", n, f);
    }
    for (n, f) in [
        ("?setw@std@@YA?AU?$_Smanip@_J@1@_J@Z", setw as Handler),
        ("?setprecision@std@@YA?AU?$_Smanip@_J@1@_J@Z", setprecision),
        ("?setbase@std@@YA?AU?$_Smanip@H@1@H@Z", setbase),
        ("?setiosflags@std@@YA?AU?$_Smanip@H@1@H@Z", setiosflags),
        ("?resetiosflags@std@@YA?AU?$_Smanip@H@1@H@Z", resetiosflags),
    ] {
        implement("msvcp140", n, f);
    }
    implement("msvcp140", "?_Fiopen@std@@YAPEAU_iobuf@@PEBDHH@Z", Fiopen_a);
    implement("msvcp140", "?_Fiopen@std@@YAPEAU_iobuf@@PEB_WHH@Z", Fiopen_w);
    implement("msvcp140", "?_Fiopen@std@@YAPEAU_iobuf@@PEBGHH@Z", Fiopen_w);
    for (n, f) in members::<false>().into_iter().chain(members::<true>()) {
        implement("msvcp140", &n, f);
    }
}
