//! msvcrt.dll: the C runtime DLL games' own DLLs link against (ogg/vorbis, DxLib helpers...).
//! Heap calls share HeapAlloc's heap, files are k32's file objects, printf is misc's.
//! Data exports (`_adjust_fdiv`, `_iob`, ...) are guest variables made on first import.

#![allow(non_snake_case)]

use crate::HashMap;
use std::io::SeekFrom;
use std::sync::{Arc, Mutex, OnceLock};

use crate::api::{implement, Ctx};
use crate::files::File;
use crate::mem::{self, cstr, ps, putp, r16, r32, r8, rp, w16, w32, w8, wp, wstr};
use crate::misc::VaArgs;
use crt::Unit;
use crate::text::a2s;

macro_rules! reg {
    ($($f:ident),* $(,)?) => { $( implement("msvcrt", stringify!($f), $f); )* };
}

#[path = "crt_io.rs"]
mod io;

// ---------------------------------------------------------------------------------------------
// what differs between x86 and x64: argument positions, size_t, float results

/// A function's arguments in order: x86 stack slots (a 64-bit integer or a double takes two),
/// x64 arguments (RCX, RDX, R8, R9 or XMM0-3 by position, then the stack).
struct Args(u32);

impl Args {
    fn p(&mut self, c: &Ctx) -> u32 {
        self.0 += 1;
        c.a(self.0 - 1)
    }
    fn size(&mut self, c: &Ctx) -> u32 {
        self.0 += 1;
        size(c, self.0 - 1)
    }
    fn u64(&mut self, c: &Ctx) -> u64 {
        if mem::x64() {
            self.0 += 1;
            return c.a64(self.0 - 1);
        }
        self.0 += 2;
        c.a(self.0 - 2) as u64 | (c.a(self.0 - 1) as u64) << 32
    }
    fn f64(&mut self, c: &Ctx) -> f64 {
        if mem::x64() {
            self.0 += 1;
            return f64::from_bits(c.xmm(self.0 - 1));
        }
        f64::from_bits(self.u64(c))
    }
    fn f32(&mut self, c: &Ctx) -> f32 {
        self.0 += 1;
        f32::from_bits(if mem::x64() { c.xmm(self.0 - 1) as u32 } else { c.a(self.0 - 1) })
    }
}

/// size_t argument `i` (x64: saturated to 32 bits, so _TRUNCATE and SIZE_MAX stay the maximum)
fn size(c: &Ctx, i: u32) -> u32 {
    if mem::x64() { c.a64(i).min(u32::MAX as u64) as u32 } else { c.a(i) }
}
/// A float result: x86 ST0, x64 the low single of XMM0.
fn ret_f32(c: &mut Ctx, v: f32) -> u32 {
    c.st0 = Some(if mem::x64() { f64::from_bits(v.to_bits() as u64) } else { v as f64 });
    0
}
/// Whether the call went to the UCRT (ucrtbase, api-ms-win-crt-*) rather than msvcrt / msvcr*.
fn is_ucrt(c: &Ctx) -> bool {
    crate::api::dll_of(c.trap).is_some_and(|d| d == "ucrtbase")
}
/// The guest string at `p` (without its NUL) as units of T.
fn gstr<T: Unit>(p: u32) -> Vec<T> {
    if T::WIDE { wstr(p).into_iter().map(|u| T::from_code(u as u32)).collect() } else { cstr(p).into_iter().map(|b| T::from_code(b as u32)).collect() }
}
/// At most `n` units of the guest string at `p`.
fn gstrn<T: Unit>(p: u32, n: u32) -> Vec<T> {
    if T::WIDE {
        mem::wstrn(p, n).into_iter().map(|u| T::from_code(u as u32)).collect()
    } else {
        mem::cstrn(p, n).into_iter().map(|b| T::from_code(b as u32)).collect()
    }
}
fn unit<T: Unit>() -> u32 {
    if T::WIDE { 2 } else { 1 }
}
/// Writes `s` (no NUL added) at `p`.
fn put_units<T: Unit>(p: u32, s: &[T]) {
    if T::WIDE {
        for (i, u) in s.iter().enumerate() {
            w16(p + 2 * i as u32, u.code() as u16);
        }
    } else {
        let b: Vec<u8> = s.iter().map(|u| u.code() as u8).collect();
        mem::write(p, &b);
    }
}
fn put_nul<T: Unit>(p: u32) {
    if T::WIDE { w16(p, 0) } else { w8(p, 0) }
}
fn errno_of(e: i32) {
    if e != 0 {
        set_errno(e as u32);
    }
}

// ---------------------------------------------------------------------------------------------
// data exports

static DATA: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// The address of msvcrt's data export `name`, if it is one.
pub fn data(name: &str) -> Option<u32> {
    const NAMES: &[&str] = &["_adjust_fdiv", "_fmode", "_commode", "__argc", "__argv", "__wargv", "_environ", "_wenviron",
        "__initenv", "__winitenv", "_acmdln", "_wcmdln", "_osver", "_winver", "_winmajor", "_winminor", "__mb_cur_max",
        "_pctype", "_pwctype", "_timezone", "_daylight", "_dstbias", "_tzname", "_mbctype", "_HUGE", "_iob"];
    if !NAMES.contains(&name) {
        return None;
    }
    let mut g = DATA.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
    if let Some(&a) = m.get(name) {
        return Some(a);
    }
    let a = match name {
        "_iob" => iob(),
        "_HUGE" => {
            let p = mem::heap_alloc(8, true);
            mem::write(p, &f64::INFINITY.to_le_bytes());
            p
        }
        "__mb_cur_max" => var(if crate::text::is_dbcs() { 2 } else { 1 }),
        "_pctype" => var(ctype_table() + 2),
        "_pwctype" => var(ctype_table() + 2),
        "_mbctype" => {
            let t = mem::heap_alloc(257, true);
            for b in 0..=255u8 {
                if crate::text::is_lead(b) {
                    w8(t + 1 + b as u32, 4);
                }
            }
            t
        }
        "__argc" => var(args().0),
        "__argv" => var(args().1),
        "__wargv" => var(wargs().1),
        "_acmdln" => var(crate::k32::cmdline().0),
        "_wcmdln" => var(crate::k32::cmdline().1),
        "_environ" | "__initenv" => var(io::environ(false)),
        "_wenviron" | "__winitenv" => var(io::environ(true)),
        "_tzname" => io::tzname_array(),
        "_osver" => var(2600),
        "_winver" => var(0x501),
        "_winmajor" => var(5),
        "_winminor" => var(1),
        _ => var(0),
    };
    m.insert(name.to_string(), a);
    Some(a)
}

/// crtdll.dll's data exports: msvcrt's under their `_dll` names (`_osmajor_dll` is
/// `_winmajor`, `_osversion_dll` `_winver`, `_baseversion_dll` `_osver`), `_ctype` the table
/// itself (EOF first), and crtdll's own `_basemajor_dll` / `_baseminor_dll` / `_cpumode_dll` /
/// `_osmode_dll` (0).
pub fn crtdll_data(name: &str) -> Option<u32> {
    if name == "_ctype" {
        return Some(ctype_table());
    }
    let msvcrt = match name {
        "_osmajor_dll" => "_winmajor",
        "_osminor_dll" => "_winminor",
        "_osversion_dll" => "_winver",
        "_baseversion_dll" => "_osver",
        "_timezone_dll" => "_timezone",
        "_daylight_dll" => "_daylight",
        "_basemajor_dll" | "_baseminor_dll" | "_cpumode_dll" | "_osmode_dll" => {
            static OWN: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
            return Some(*OWN.lock().unwrap().get_or_insert_with(HashMap::default).entry(name.into()).or_insert_with(|| var(0)));
        }
        n => n.strip_suffix("_dll").unwrap_or(n),
    };
    data(msvcrt)
}

fn var(v: u32) -> u32 {
    let p = mem::heap_alloc(16, true);
    w32(p, v);
    p
}

/// msvcrt's ctype table (257 shorts, for EOF first).
fn ctype_table() -> u32 {
    static T: Mutex<u32> = Mutex::new(0);
    let mut g = T.lock().unwrap();
    if *g == 0 {
        let t = mem::heap_alloc(2 * 257, true);
        for c in 0..256u32 {
            let b = c as u8;
            let mut v = 0u16;
            if b.is_ascii_uppercase() {
                v |= 0x1 | 0x100;
            }
            if b.is_ascii_lowercase() {
                v |= 0x2 | 0x100;
            }
            if b.is_ascii_digit() {
                v |= 0x4;
            }
            if matches!(b, b' ' | 9..=13) {
                v |= 0x8;
            }
            if b.is_ascii_punctuation() {
                v |= 0x10;
            }
            if b < 0x20 || b == 0x7f {
                v |= 0x20;
            }
            if b == b' ' {
                v |= 0x40;
            }
            if b.is_ascii_hexdigit() {
                v |= 0x80;
            }
            w16(t + 2 + 2 * c, v);
        }
        *g = t;
    }
    *g
}

/// The command line split into words, as the CRT does (quotes group and are dropped).
fn split_words<T: Copy + PartialEq + From<u8>>(line: &[T]) -> Vec<Vec<T>> {
    let (quote, space, tab) = (T::from(b'"'), T::from(b' '), T::from(b'\t'));
    let mut words = Vec::new();
    let mut cur: Option<Vec<T>> = None;
    let mut quoted = false;
    for &b in line {
        if b == quote {
            quoted = !quoted;
            cur.get_or_insert_with(Vec::new);
        } else if (b == space || b == tab) && !quoted {
            words.extend(cur.take());
        } else {
            cur.get_or_insert_with(Vec::new).push(b);
        }
    }
    words.extend(cur);
    words
}

/// A NULL-terminated pointer array of the strings, each `unit` bytes per char.
fn string_array(words: Vec<Vec<u8>>, unit: u32) -> u32 {
    let argv = mem::heap_alloc(ps() * (words.len() as u32 + 1), true);
    for (i, w) in words.iter().enumerate() {
        let s = mem::heap_alloc(w.len() as u32 + unit, true);
        mem::write(s, w);
        wp(argv + ps() * i as u32, s);
    }
    argv
}

/// (argc, argv) parsed from the command line, as the CRT does.
fn args() -> (u32, u32) {
    static A: Mutex<(u32, u32)> = Mutex::new((0, 0));
    let mut g = A.lock().unwrap();
    if g.1 == 0 {
        let words = split_words(&cstr(crate::k32::cmdline().0));
        *g = (words.len() as u32, string_array(words, 1));
    }
    *g
}

/// (argc, wargv) parsed from the wide command line.
fn wargs() -> (u32, u32) {
    static A: Mutex<(u32, u32)> = Mutex::new((0, 0));
    let mut g = A.lock().unwrap();
    if g.1 == 0 {
        let words = split_words(&mem::wstr(crate::k32::cmdline().1));
        let n = words.len() as u32;
        let bytes = words.into_iter().map(|w| w.iter().flat_map(|c| c.to_le_bytes()).collect()).collect();
        *g = (n, string_array(bytes, 2));
    }
    *g
}

/// an empty environment block (one NULL pointer)
// ---------------------------------------------------------------------------------------------
// startup and exit

fn _initterm(c: &mut Ctx) -> u32 {
    let (mut p, end) = (c.a(0), c.a(1));
    while p < end {
        let f = rp(p);
        if f != 0 {
            c.call(f, &[]);
        }
        p += ps();
    }
    0
}
fn _initterm_e(c: &mut Ctx) -> u32 {
    let (mut p, end) = (c.a(0), c.a(1));
    while p < end {
        let f = rp(p);
        if f != 0 {
            let r = c.call(f, &[]);
            if r != 0 {
                return r;
            }
        }
        p += ps();
    }
    0
}

/// atexit functions, run by exit()
static ATEXIT: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn _onexit(c: &mut Ctx) -> u32 {
    ATEXIT.lock().unwrap().push(c.a(0));
    c.a(0)
}
fn atexit(c: &mut Ctx) -> u32 {
    ATEXIT.lock().unwrap().push(c.a(0));
    0
}
/// __dllonexit(func, &begin, &end): a DLL's own table; we keep them all in one list.
fn __dllonexit(c: &mut Ctx) -> u32 {
    ATEXIT.lock().unwrap().push(c.a(0));
    c.a(0)
}
fn exit(c: &mut Ctx) -> u32 {
    let code = c.a(0);
    loop {
        let Some(f) = ATEXIT.lock().unwrap().pop() else { break };
        c.call(f, &[]);
    }
    unsafe { libc::exit(code as i32) }
}
fn _exit(c: &mut Ctx) -> u32 {
    unsafe { libc::exit(c.a(0) as i32) }
}
fn abort(_: &mut Ctx) -> u32 {
    eprintln!("sake: abort()");
    unsafe { libc::exit(3) }
}
fn __getmainargs(c: &mut Ctx) -> u32 {
    let (argc, argv) = args();
    w32(c.a(0), argc);
    wp(c.a(1), argv);
    wp(c.a(2), rp(data("_environ").unwrap()));
    0
}
fn __set_app_type(_: &mut Ctx) -> u32 {
    0
}
fn __p__fmode(_: &mut Ctx) -> u32 {
    data("_fmode").unwrap()
}
fn __p__commode(_: &mut Ctx) -> u32 {
    data("_commode").unwrap()
}
fn __p___argc(_: &mut Ctx) -> u32 {
    data("__argc").unwrap()
}
fn __p___argv(_: &mut Ctx) -> u32 {
    data("__argv").unwrap()
}
fn __p___wargv(_: &mut Ctx) -> u32 {
    data("__wargv").unwrap()
}
fn __p__wcmdln(_: &mut Ctx) -> u32 {
    data("_wcmdln").unwrap()
}
fn __p__wenviron(_: &mut Ctx) -> u32 {
    data("_wenviron").unwrap()
}
fn __wgetmainargs(c: &mut Ctx) -> u32 {
    let (argc, argv) = wargs();
    w32(c.a(0), argc);
    wp(c.a(1), argv);
    wp(c.a(2), rp(data("_wenviron").unwrap()));
    0
}
fn __p__environ(_: &mut Ctx) -> u32 {
    data("_environ").unwrap()
}
fn __p___initenv(_: &mut Ctx) -> u32 {
    data("__initenv").unwrap()
}
fn __p__acmdln(_: &mut Ctx) -> u32 {
    data("_acmdln").unwrap()
}
fn __p___mb_cur_max(_: &mut Ctx) -> u32 {
    data("__mb_cur_max").unwrap()
}
fn ___mb_cur_max_func(_: &mut Ctx) -> u32 {
    r32(data("__mb_cur_max").unwrap())
}
fn __pctype_func(_: &mut Ctx) -> u32 {
    r32(data("_pctype").unwrap())
}
/// The CRT's control word (_MCW_EM | _MCW_RC | _MCW_PC | _MCW_IC bits) over the guest's own
/// x87 control word and MXCSR: x86 sets and reports the x87 one (and the SSE rounding and
/// masks with it), x64 only MXCSR (no precision control there: Windows' answer is 0x8001f).
const EM_BITS: [(u32, u32); 6] = [(0x10, 0), (0x80000, 1), (0x8, 2), (0x4, 3), (0x2, 4), (0x1, 5)];
pub fn fp_control(c: &Ctx) -> u32 {
    if mem::x64() {
        let m = c.cpu.mxcsr;
        let em = EM_BITS.iter().filter(|&&(_, b)| m >> (7 + b) & 1 != 0).fold(0, |a, &(f, _)| a | f);
        return em | (m >> 13 & 3) << 8;
    }
    let cw = c.cpu.fpu.cw as u32;
    let em = EM_BITS.iter().filter(|&&(_, b)| cw >> b & 1 != 0).fold(0, |a, &(f, _)| a | f);
    let pc = match cw >> 8 & 3 {
        0 => 0x20000,
        2 => 0x10000,
        _ => 0,
    };
    em | (cw >> 10 & 3) << 8 | pc | (cw >> 12 & 1) << 18
}
pub fn set_fp_control(c: &mut Ctx, new: u32, mask: u32) -> u32 {
    let v = (fp_control(c) & !mask) | (new & mask);
    let em = |shift: u32| EM_BITS.iter().filter(|&&(f, _)| v & f != 0).fold(0, |a, &(_, b)| a | 1 << (b + shift));
    let rc = v >> 8 & 3;
    c.cpu.mxcsr = (c.cpu.mxcsr & !(0x3f << 7 | 3 << 13)) | em(7) | rc << 13;
    if !mem::x64() {
        let pc = match v & 0x30000 {
            0x20000 => 0,
            0x10000 => 2,
            _ => 3,
        };
        c.cpu.fpu.cw = (c.cpu.fpu.cw & 0xe0c0) as u16 | em(0) as u16 | (pc << 8 | rc << 10 | (v >> 18 & 1) << 12) as u16 | 0x40;
    }
    fp_control(c)
}
fn _controlfp(c: &mut Ctx) -> u32 {
    // (the denormal mask stays as it is)
    let (new, mask) = (c.a(0), c.a(1) & !0x80000);
    set_fp_control(c, new, mask)
}
fn _control87(c: &mut Ctx) -> u32 {
    let (new, mask) = (c.a(0), c.a(1));
    set_fp_control(c, new, mask)
}
fn _clearfp(_: &mut Ctx) -> u32 {
    0
}
fn _purecall(_: &mut Ctx) -> u32 {
    eprintln!("sake: pure virtual call");
    unsafe { libc::exit(3) }
}
fn _XcptFilter(_: &mut Ctx) -> u32 {
    0
}
fn signal(_: &mut Ctx) -> u32 {
    0
}
fn _lock(_: &mut Ctx) -> u32 {
    0
}
fn _unlock(_: &mut Ctx) -> u32 {
    0
}

/// errno (and _doserrno after it) live in the TEB's free page, so fast::errno_ptr can find them
pub const ERRNO_TEB: u32 = 0x1000;

fn _errno(_: &mut Ctx) -> u32 {
    crate::cur().teb + ERRNO_TEB
}
fn __doserrno(_: &mut Ctx) -> u32 {
    crate::cur().teb + ERRNO_TEB + 4
}
pub(crate) fn set_errno(v: u32) {
    w32(crate::cur().teb + ERRNO_TEB, v);
}

// ---------------------------------------------------------------------------------------------
// heap

fn malloc(c: &mut Ctx) -> u32 {
    mem::heap_alloc_small(c.a(0).max(1), false)
}
fn calloc(c: &mut Ctx) -> u32 {
    mem::heap_alloc(c.a(0).saturating_mul(c.a(1)).max(1), true)
}
fn realloc(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    if p == 0 {
        return mem::heap_alloc(n.max(1), false);
    }
    if n == 0 {
        mem::heap_free(p);
        return 0;
    }
    mem::heap_realloc(p, n, false)
}
fn free(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free_small(c.a(0));
    }
    0
}
/// _aligned_malloc(size, align): the block's start and size sit in the 8 bytes before it
fn aligned_alloc(n: u32, align: u32) -> u32 {
    let align = align.max(8).next_power_of_two();
    let raw = mem::heap_alloc(n.saturating_add(align + 8), false);
    if raw == 0 {
        return 0;
    }
    let p = (raw + 8 + align - 1) & !(align - 1);
    w32(p - 8, raw);
    w32(p - 4, n);
    p
}
fn _aligned_malloc(c: &mut Ctx) -> u32 {
    aligned_alloc(c.a(0), c.a(1))
}
fn _aligned_free(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(r32(c.a(0) - 8));
    }
    0
}
fn _aligned_realloc(c: &mut Ctx) -> u32 {
    let (p, n, align) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return aligned_alloc(n, align);
    }
    if n == 0 {
        mem::heap_free(r32(p - 8));
        return 0;
    }
    let q = aligned_alloc(n, align);
    if q != 0 {
        mem::copy(q, p, n.min(r32(p - 4)));
        mem::heap_free(r32(p - 8));
    }
    q
}
fn _aligned_msize(c: &mut Ctx) -> u32 {
    r32(c.a(0) - 4)
}
fn _mkdir(c: &mut Ctx) -> u32 {
    if crate::k32::mkdir(&crate::k32::arg_str(c.a(0), false)) != 0 { 0 } else { set_errno(17); u32::MAX }
}
fn _wmkdir(c: &mut Ctx) -> u32 {
    if crate::k32::mkdir(&crate::k32::arg_str(c.a(0), true)) != 0 { 0 } else { set_errno(17); u32::MAX }
}
fn _msize(c: &mut Ctx) -> u32 {
    mem::heap_size(c.a(0))
}

// ---------------------------------------------------------------------------------------------
// memory and strings

fn memcpy(c: &mut Ctx) -> u32 {
    let (d, s, n) = (c.a(0), c.a(1), c.a(2));
    if n != 0 {
        unsafe { std::ptr::copy(mem::slice(s, n).as_ptr(), mem::slice_mut(d, n).as_mut_ptr(), n as usize) };
    }
    d
}
fn memmove(c: &mut Ctx) -> u32 {
    memcpy(c)
}
fn memset(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), c.a(2), c.a(1) as u8);
    c.a(0)
}
fn memcmp(c: &mut Ctx) -> u32 {
    let n = c.a(2);
    mem::slice(c.a(0), n).cmp(mem::slice(c.a(1), n)) as i32 as u32
}
fn memchr(c: &mut Ctx) -> u32 {
    let (p, ch, n) = (c.a(0), c.a(1) as u8, c.a(2));
    mem::slice(p, n).iter().position(|&b| b == ch).map_or(0, |i| p + i as u32)
}
fn strlen(c: &mut Ctx) -> u32 {
    cstr(c.a(0)).len() as u32
}
fn strcpy(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(1));
    mem::write(c.a(0), &s);
    w8(c.a(0) + s.len() as u32, 0);
    c.a(0)
}
fn strncpy(c: &mut Ctx) -> u32 {
    let (d, n) = (c.a(0), c.a(2));
    let s = mem::cstrn(c.a(1), n);
    mem::write(d, &s);
    mem::fill(d + s.len() as u32, n - s.len() as u32, 0);
    d
}
fn strcat(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    let at = d + cstr(d).len() as u32;
    let s = cstr(c.a(1));
    mem::write(at, &s);
    w8(at + s.len() as u32, 0);
    d
}
fn strncat(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    let at = d + cstr(d).len() as u32;
    let s = mem::cstrn(c.a(1), c.a(2));
    mem::write(at, &s);
    w8(at + s.len() as u32, 0);
    d
}
fn cmp(a: &[u8], b: &[u8]) -> u32 {
    a.cmp(b) as i32 as u32
}
fn lower(s: Vec<u8>) -> Vec<u8> {
    s.to_ascii_lowercase()
}
fn strcmp(c: &mut Ctx) -> u32 {
    cmp(&cstr(c.a(0)), &cstr(c.a(1)))
}
fn strncmp(c: &mut Ctx) -> u32 {
    cmp(&mem::cstrn(c.a(0), c.a(2)), &mem::cstrn(c.a(1), c.a(2)))
}
/// Case-insensitive compare: Windows' x86 msvcrt gives -1/0/1, its x64 one the difference of
/// the first lowercased bytes that differ (_stricmp("a_", "A[") is 1 and 4)
fn icmp(a: &[u8], b: &[u8]) -> u32 {
    let (a, b) = (lower(a.to_vec()), lower(b.to_vec()));
    if !mem::x64() {
        return cmp(&a, &b);
    }
    let n = a.len().min(b.len());
    let k = (0..n).find(|&k| a[k] != b[k]).unwrap_or(n);
    (*a.get(k).unwrap_or(&0) as i32 - *b.get(k).unwrap_or(&0) as i32) as u32
}
fn _stricmp(c: &mut Ctx) -> u32 {
    icmp(&cstr(c.a(0)), &cstr(c.a(1)))
}
fn _strnicmp(c: &mut Ctx) -> u32 {
    icmp(&mem::cstrn(c.a(0), c.a(2)), &mem::cstrn(c.a(1), c.a(2)))
}
fn strchr(c: &mut Ctx) -> u32 {
    let (p, ch) = (c.a(0), c.a(1) as u8);
    let s = cstr(p);
    if ch == 0 {
        return p + s.len() as u32;
    }
    s.iter().position(|&b| b == ch).map_or(0, |i| p + i as u32)
}
fn strrchr(c: &mut Ctx) -> u32 {
    let (p, ch) = (c.a(0), c.a(1) as u8);
    let s = cstr(p);
    if ch == 0 {
        return p + s.len() as u32;
    }
    s.iter().rposition(|&b| b == ch).map_or(0, |i| p + i as u32)
}
/// strspn / strcspn: the length of the prefix made of (not made of) `set`'s bytes
fn span(c: &mut Ctx, inside: bool) -> u32 {
    let (s, set) = (cstr(c.a(0)), cstr(c.a(1)));
    s.iter().position(|b| set.contains(b) != inside).unwrap_or(s.len()) as u32
}
fn strspn(c: &mut Ctx) -> u32 {
    span(c, true)
}
fn strcspn(c: &mut Ctx) -> u32 {
    span(c, false)
}
fn strpbrk(c: &mut Ctx) -> u32 {
    let (s, set) = (cstr(c.a(0)), cstr(c.a(1)));
    s.iter().position(|b| set.contains(b)).map_or(0, |i| c.a(0) + i as u32)
}
fn strstr(c: &mut Ctx) -> u32 {
    let (p, n) = (cstr(c.a(0)), cstr(c.a(1)));
    if n.is_empty() {
        return c.a(0);
    }
    p.windows(n.len()).position(|w| w == n.as_slice()).map_or(0, |i| c.a(0) + i as u32)
}
fn _strdup(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return 0;
    }
    let s = cstr(c.a(0));
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, &s);
    p
}
fn _strlwr(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0)).to_ascii_lowercase();
    mem::write(c.a(0), &s);
    c.a(0)
}
fn _strupr(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0)).to_ascii_uppercase();
    mem::write(c.a(0), &s);
    c.a(0)
}
fn _wcsdup(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return 0;
    }
    let s = wstr(c.a(0));
    let p = mem::heap_alloc(2 * s.len() as u32 + 2, true);
    for (i, &u) in s.iter().enumerate() {
        w16(p + 2 * i as u32, u);
    }
    p
}
fn wcs_map(c: &mut Ctx, f: fn(char) -> char) -> u32 {
    let s = wstr(c.a(0));
    for (i, &u) in s.iter().enumerate() {
        let m = char::from_u32(u as u32).map_or(u, |ch| {
            let m = f(ch) as u32;
            if m < 0x10000 { m as u16 } else { u }
        });
        w16(c.a(0) + 2 * i as u32, m);
    }
    c.a(0)
}
fn _wcslwr(c: &mut Ctx) -> u32 {
    wcs_map(c, |ch| ch.to_lowercase().next().unwrap_or(ch))
}
fn _wcsupr(c: &mut Ctx) -> u32 {
    wcs_map(c, |ch| ch.to_uppercase().next().unwrap_or(ch))
}
/// strtok/wcstok (with or without a context): `w` is the character size
fn tok(w: u32, s: u32, delim: u32, ctx: u32) -> u32 {
    let rd = |a: u32| if w == 2 { r16(a) as u32 } else { r8(a) as u32 };
    let wr = |a: u32| if w == 2 { w16(a, 0) } else { w8(a, 0) };
    let ds: Vec<u32> = (0..).map(|i| rd(delim + w * i)).take_while(|&ch| ch != 0).collect();
    let mut p = if s != 0 { s } else { rp(ctx) };
    if p == 0 {
        return 0;
    }
    while rd(p) != 0 && ds.contains(&rd(p)) {
        p += w;
    }
    if rd(p) == 0 {
        wp(ctx, p);
        return 0;
    }
    let start = p;
    while rd(p) != 0 && !ds.contains(&rd(p)) {
        p += w;
    }
    if rd(p) != 0 {
        wr(p);
        p += w;
    }
    wp(ctx, p);
    start
}
static TOK_CTX: OnceLock<u32> = OnceLock::new();
fn tok_ctx() -> u32 {
    *TOK_CTX.get_or_init(|| mem::heap_alloc(16, true))
}
fn strtok(c: &mut Ctx) -> u32 {
    tok(1, c.a(0), c.a(1), tok_ctx())
}
fn strtok_s(c: &mut Ctx) -> u32 {
    tok(1, c.a(0), c.a(1), c.a(2))
}
fn wcstok(c: &mut Ctx) -> u32 {
    tok(2, c.a(0), c.a(1), tok_ctx() + 8)
}
fn wcstok_s(c: &mut Ctx) -> u32 {
    tok(2, c.a(0), c.a(1), c.a(2))
}
fn wcslen(c: &mut Ctx) -> u32 {
    wstr(c.a(0)).len() as u32
}
fn wcscpy(c: &mut Ctx) -> u32 {
    let s = wstr(c.a(1));
    for (i, &u) in s.iter().chain([0u16].iter()).enumerate() {
        w16(c.a(0) + 2 * i as u32, u);
    }
    c.a(0)
}
fn wcschr(c: &mut Ctx) -> u32 {
    let (p, ch) = (c.a(0), c.a(1) as u16);
    let s = wstr(p);
    if ch == 0 {
        return p + 2 * s.len() as u32;
    }
    s.iter().position(|&u| u == ch).map_or(0, |i| p + 2 * i as u32)
}
fn wcsrchr(c: &mut Ctx) -> u32 {
    let (p, ch) = (c.a(0), c.a(1) as u16);
    let s = wstr(p);
    if ch == 0 {
        return p + 2 * s.len() as u32;
    }
    s.iter().rposition(|&u| u == ch).map_or(0, |i| p + 2 * i as u32)
}
fn wput(dst: u32, s: &[u16]) {
    for (i, &u) in s.iter().chain([0u16].iter()).enumerate() {
        w16(dst + 2 * i as u32, u);
    }
}
fn wcscat(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    wput(d + 2 * wstr(d).len() as u32, &wstr(c.a(1)));
    d
}
fn wcsncat(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    let src: Vec<u16> = wstr(c.a(1)).into_iter().take(c.a(2) as usize).collect();
    wput(d + 2 * wstr(d).len() as u32, &src);
    d
}
fn wcsncpy(c: &mut Ctx) -> u32 {
    let (d, n) = (c.a(0), c.a(2));
    let src = wstr(c.a(1));
    for i in 0..n {
        w16(d + 2 * i, src.get(i as usize).copied().unwrap_or(0));
    }
    d
}
fn wcsstr(c: &mut Ctx) -> u32 {
    let (h, n) = (wstr(c.a(0)), wstr(c.a(1)));
    if n.is_empty() {
        return c.a(0);
    }
    h.windows(n.len()).position(|w| w == n.as_slice()).map_or(0, |i| c.a(0) + 2 * i as u32)
}
fn wcsspn(c: &mut Ctx) -> u32 {
    let set = wstr(c.a(1));
    wstr(c.a(0)).iter().take_while(|u| set.contains(u)).count() as u32
}
fn wcscspn(c: &mut Ctx) -> u32 {
    let set = wstr(c.a(1));
    wstr(c.a(0)).iter().take_while(|u| !set.contains(u)).count() as u32
}
fn wcspbrk(c: &mut Ctx) -> u32 {
    let set = wstr(c.a(1));
    wstr(c.a(0)).iter().position(|u| set.contains(u)).map_or(0, |i| c.a(0) + 2 * i as u32)
}
fn towlower(c: &mut Ctx) -> u32 {
    let u = c.a(0) & 0xffff;
    char::from_u32(u).and_then(|ch| ch.to_lowercase().next()).map_or(u, |l| if (l as u32) < 0x10000 { l as u32 } else { u })
}
fn towupper(c: &mut Ctx) -> u32 {
    let u = c.a(0) & 0xffff;
    char::from_u32(u).and_then(|ch| ch.to_uppercase().next()).map_or(u, |l| if (l as u32) < 0x10000 { l as u32 } else { u })
}
fn iswspace(c: &mut Ctx) -> u32 {
    char::from_u32(c.a(0) & 0xffff).is_some_and(|ch| ch.is_whitespace()) as u32
}
fn iswdigit(c: &mut Ctx) -> u32 {
    (b'0' as u32..=b'9' as u32).contains(&(c.a(0) & 0xffff)) as u32
}
fn iswalpha(c: &mut Ctx) -> u32 {
    char::from_u32(c.a(0) & 0xffff).is_some_and(|ch| ch.is_alphabetic()) as u32
}
/// strtol / strtoul and their 64-bit, wide and _l forms (str, end, base): `bits` of the result
fn strto_int<T: Unit>(c: &mut Ctx, signed: bool, bits: u32) -> u32 {
    let (s, end, base) = (c.a(0), c.a(1), c.a(2));
    let t = gstr::<T>(s);
    let legacy = !is_ucrt(c);
    let (v, used, e) = match (signed, legacy) {
        (true, false) => {
            let (v, n, e) = crt::num::strtol(&t, base, bits);
            (v as u64, n, e)
        }
        (true, true) => {
            let (v, n, e) = crt::num::strtol_legacy(&t, base, bits);
            (v as u64, n, e)
        }
        (false, false) => crt::num::strtoul(&t, base, bits),
        (false, true) => crt::num::strtoul_legacy(&t, base, bits),
    };
    putp(end, s + unit::<T>() * used as u32);
    errno_of(e);
    if bits == 64 { c.ret64(v) } else { v as u32 }
}
macro_rules! strto_ints {
    ($($name:ident = $t:ty, $signed:expr, $bits:expr;)*) => { $( fn $name(c: &mut Ctx) -> u32 { strto_int::<$t>(c, $signed, $bits) } )* };
}
strto_ints! {
    strtol = u8, true, 32; strtoul = u8, false, 32; _strtoi64 = u8, true, 64; _strtoui64 = u8, false, 64;
    wcstol = u16, true, 32; wcstoul = u16, false, 32; _wcstoi64 = u16, true, 64; _wcstoui64 = u16, false, 64;
}
/// atoi / atol (32-bit), _atoi64 / atoll (64-bit) and the wide forms
fn ato_int<T: Unit>(c: &mut Ctx, bits: u32) -> u32 {
    // (msvcrt.dll's atoi/atol wrap around, the later runtimes' are limited: Windows' answers)
    if bits == 32 && crate::api::dll_of(c.trap).as_deref() == Some("msvcrt") {
        return crt::num::atoi_wrap(&gstr::<T>(c.a(0))) as u32;
    }
    let (v, _, e) = crt::num::atoi(&gstr::<T>(c.a(0)), bits);
    errno_of(e);
    if bits == 64 { c.ret64(v as u64) } else { v as u32 }
}
fn _atoi64(c: &mut Ctx) -> u32 {
    ato_int::<u8>(c, 64)
}
fn _wtoi(c: &mut Ctx) -> u32 {
    ato_int::<u16>(c, 32)
}
fn _wtoi64(c: &mut Ctx) -> u32 {
    ato_int::<u16>(c, 64)
}
/// strtod / strtof and the wide and _l forms (str, end): the UCRT's C99 syntax, the legacy
/// runtimes' decimal one
fn strto_float<T: Unit>(c: &mut Ctx, kind: crt::num::Float, with_end: bool) -> u32 {
    let s = c.a(0);
    let (v, used, e) = crt::num::strtod(&gstr::<T>(s), kind, !is_ucrt(c));
    if with_end {
        putp(c.a(1), s + unit::<T>() * used as u32);
    }
    errno_of(e);
    match kind {
        crt::num::Float::F64 => {
            c.st0 = Some(v);
            0
        }
        crt::num::Float::F32 => ret_f32(c, v as f32),
    }
}
fn strtof(c: &mut Ctx) -> u32 {
    strto_float::<u8>(c, crt::num::Float::F32, true)
}
fn wcstod(c: &mut Ctx) -> u32 {
    strto_float::<u16>(c, crt::num::Float::F64, true)
}
fn wcstof(c: &mut Ctx) -> u32 {
    strto_float::<u16>(c, crt::num::Float::F32, true)
}
fn _wtof(c: &mut Ctx) -> u32 {
    strto_float::<u16>(c, crt::num::Float::F64, false)
}
fn wlower(s: Vec<u16>) -> Vec<u16> {
    s.into_iter().map(|u| if (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u }).collect()
}
fn _wcsicmp(c: &mut Ctx) -> u32 {
    wlower(wstr(c.a(0))).cmp(&wlower(wstr(c.a(1)))) as i32 as u32
}
fn _wcsnicmp(c: &mut Ctx) -> u32 {
    let n = c.a(2) as usize;
    let t = |a: u32| wlower(wstr(a).into_iter().take(n).collect());
    t(c.a(0)).cmp(&t(c.a(1))) as i32 as u32
}
fn wcscmp(c: &mut Ctx) -> u32 {
    wstr(c.a(0)).cmp(&wstr(c.a(1))) as i32 as u32
}
fn toupper(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if (b'a' as u32..=b'z' as u32).contains(&v) { v - 32 } else { v }
}
fn tolower(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if (b'A' as u32..=b'Z' as u32).contains(&v) { v + 32 } else { v }
}
fn ctype(c: &mut Ctx, mask: u16) -> u32 {
    let v = c.a(0);
    if v > 255 {
        return 0;
    }
    (r16(ctype_table() + 2 + 2 * v) & mask) as u32
}
fn isalpha(c: &mut Ctx) -> u32 {
    ctype(c, 0x103)
}
fn isupper(c: &mut Ctx) -> u32 {
    ctype(c, 1)
}
fn islower(c: &mut Ctx) -> u32 {
    ctype(c, 2)
}
fn isdigit(c: &mut Ctx) -> u32 {
    ctype(c, 4)
}
fn isspace(c: &mut Ctx) -> u32 {
    ctype(c, 8)
}
fn ispunct(c: &mut Ctx) -> u32 {
    ctype(c, 0x10)
}
fn isalnum(c: &mut Ctx) -> u32 {
    ctype(c, 0x107)
}
fn isxdigit(c: &mut Ctx) -> u32 {
    ctype(c, 0x80)
}
fn iscntrl(c: &mut Ctx) -> u32 {
    ctype(c, 0x20)
}
fn isgraph(c: &mut Ctx) -> u32 {
    ctype(c, 0x117)
}
fn isprint(c: &mut Ctx) -> u32 {
    ctype(c, 0x157)
}

fn atoi(c: &mut Ctx) -> u32 {
    ato_int::<u8>(c, 32)
}
fn atof(c: &mut Ctx) -> u32 {
    strto_float::<u8>(c, crt::num::Float::F64, false)
}
fn strtod(c: &mut Ctx) -> u32 {
    strto_float::<u8>(c, crt::num::Float::F64, true)
}
/// _itoa / _ltoa / _ultoa / _i64toa / _ui64toa and the wide forms (value, buf, radix): the digits
/// (a radix outside 2..36: an empty string and EINVAL)
fn xtoa<T: Unit>(c: &mut Ctx, signed: bool, bits: u32) -> u32 {
    let mut a = Args(0);
    let v = if bits == 64 { a.u64(c) as i64 } else if signed { a.p(c) as i32 as i64 } else { a.p(c) as i64 };
    let (buf, radix) = (a.p(c), a.p(c));
    if buf == 0 {
        set_errno(22);
        return 0;
    }
    if !(2..=36).contains(&radix) {
        put_nul::<T>(buf);
        set_errno(22);
        return buf;
    }
    let d: Vec<T> = crt::num::to_radix(v, bits, signed, radix).into_iter().map(T::from_ascii).collect();
    put_units(buf, &d);
    put_nul::<T>(buf + unit::<T>() * d.len() as u32);
    buf
}
/// the _s forms (value, buf, size in units, radix): 0 or the error (crt::num::xtoa_s)
fn xtoa_s<T: Unit>(c: &mut Ctx, signed: bool, bits: u32) -> u32 {
    let mut a = Args(0);
    let v = if bits == 64 { a.u64(c) as i64 } else if signed { a.p(c) as i32 as i64 } else { a.p(c) as i64 };
    let (buf, n, radix) = (a.p(c), a.size(c), a.p(c));
    // (no result is longer than 66 units: a view that big decides it alike)
    let n = n.min(80);
    let mut v_buf: Vec<T> = if buf == 0 { Vec::new() } else { gstrn_raw::<T>(buf, n) };
    let e = crt::num::xtoa_s(v, bits, signed, (buf != 0).then_some(&mut v_buf[..]), radix);
    if buf != 0 {
        put_units(buf, &v_buf);
    }
    errno_of(e);
    e as u32
}
/// The `n` units at `p` as they are (NULs included).
fn gstrn_raw<T: Unit>(p: u32, n: u32) -> Vec<T> {
    (0..n).map(|i| T::from_code(if T::WIDE { r16(p + 2 * i) as u32 } else { r8(p + i) as u32 })).collect()
}
macro_rules! xtoas {
    ($($name:ident, $name_s:ident = $t:ty, $signed:expr, $bits:expr;)*) => { $(
        fn $name(c: &mut Ctx) -> u32 { xtoa::<$t>(c, $signed, $bits) }
        fn $name_s(c: &mut Ctx) -> u32 { xtoa_s::<$t>(c, $signed, $bits) }
    )* };
}
xtoas! {
    _itoa, _itoa_s = u8, true, 32; _ultoa, _ultoa_s = u8, false, 32;
    _i64toa, _i64toa_s = u8, true, 64; _ui64toa, _ui64toa_s = u8, false, 64;
    _itow, _itow_s = u16, true, 32; _ultow, _ultow_s = u16, false, 32;
    _i64tow, _i64tow_s = u16, true, 64; _ui64tow, _ui64tow_s = u16, false, 64;
}
fn abs(c: &mut Ctx) -> u32 {
    c.i(0).unsigned_abs()
}

/// msvcrt's own quicksort (median of three, insertion of the largest below 9 elements), in
/// place: games that sort equal keys get them in the order Windows leaves them.
fn qsort(c: &mut Ctx) -> u32 {
    let (base, num, w, f) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if num < 2 || w == 0 {
        return 0;
    }
    let cmp = |c: &mut Ctx, a: u32, b: u32| c.call(f, &[a, b]) as i32;
    let swap = |a: u32, b: u32| {
        if a != b {
            for k in 0..w {
                let (x, y) = (r8(a + k), r8(b + k));
                w8(a + k, y);
                w8(b + k, x);
            }
        }
    };
    let mut stack: Vec<(u32, u32)> = Vec::new();
    let (mut lo, mut hi) = (base, base + w * (num - 1));
    loop {
        let size = (hi - lo) / w + 1;
        if size <= 8 {
            let mut h = hi;
            while h > lo {
                let mut max = lo;
                let mut p = lo + w;
                while p <= h {
                    if cmp(c, p, max) > 0 {
                        max = p;
                    }
                    p += w;
                }
                swap(max, h);
                h -= w;
            }
        } else {
            let mut mid = lo + (size / 2) * w;
            if cmp(c, lo, mid) > 0 { swap(lo, mid); }
            if cmp(c, lo, hi) > 0 { swap(lo, hi); }
            if cmp(c, mid, hi) > 0 { swap(mid, hi); }
            let (mut loguy, mut higuy) = (lo, hi);
            loop {
                if mid > loguy {
                    loop {
                        loguy += w;
                        if !(loguy < mid && cmp(c, loguy, mid) <= 0) { break; }
                    }
                }
                if mid <= loguy {
                    loop {
                        loguy += w;
                        if !(loguy <= hi && cmp(c, loguy, mid) <= 0) { break; }
                    }
                }
                loop {
                    higuy -= w;
                    if !(higuy > mid && cmp(c, higuy, mid) > 0) { break; }
                }
                if higuy < loguy { break; }
                swap(loguy, higuy);
                if mid == higuy { mid = loguy; }
            }
            higuy += w;
            if mid < higuy {
                loop {
                    higuy -= w;
                    if !(higuy > mid && cmp(c, higuy, mid) == 0) { break; }
                }
            }
            if mid >= higuy {
                loop {
                    higuy -= w;
                    if !(higuy > lo && cmp(c, higuy, mid) == 0) { break; }
                }
            }
            if higuy.wrapping_sub(lo) as i32 >= hi.wrapping_sub(loguy) as i32 {
                if lo < higuy { stack.push((lo, higuy)); }
                if loguy < hi { lo = loguy; continue; }
            } else {
                if loguy < hi { stack.push((loguy, hi)); }
                if lo < higuy { hi = higuy; continue; }
            }
        }
        match stack.pop() {
            Some((l, h)) => (lo, hi) = (l, h),
            None => return 0,
        }
    }
}
/// msvcrt's bsearch (which of equal elements it finds follows its halving)
fn bsearch(c: &mut Ctx) -> u32 {
    let (key, base, mut num, w, f) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if num == 0 {
        return 0;
    }
    let (mut lo, mut hi) = (base as i64, base as i64 + (num as i64 - 1) * w as i64);
    while lo <= hi {
        let half = num / 2;
        if half != 0 {
            let mid = lo + (if num & 1 != 0 { half } else { half - 1 }) as i64 * w as i64;
            let r = c.call(f, &[key, mid as u32]) as i32;
            if r == 0 {
                return mid as u32;
            } else if r < 0 {
                hi = mid - w as i64;
                num = if num & 1 != 0 { half } else { half - 1 };
            } else {
                lo = mid + w as i64;
                num = half;
            }
        } else if num != 0 {
            return if c.call(f, &[key, lo as u32]) != 0 { 0 } else { lo as u32 };
        } else {
            break;
        }
    }
    0
}

static RAND: Mutex<u32> = Mutex::new(1);
fn srand(c: &mut Ctx) -> u32 {
    *RAND.lock().unwrap() = c.a(0);
    0
}
fn rand(_: &mut Ctx) -> u32 {
    let mut g = RAND.lock().unwrap();
    *g = g.wrapping_mul(214013).wrapping_add(2531011);
    (*g >> 16) & 0x7fff
}
/// time / _time32 (x86 msvcrt's time_t) and _time64 (x64's time)
fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
fn _time32(c: &mut Ctx) -> u32 {
    let t = now() as u32;
    mem::put32(c.a(0), t);
    t
}
fn time(c: &mut Ctx) -> u32 {
    if mem::x64() { _time64(c) } else { _time32(c) }
}
fn _time64(c: &mut Ctx) -> u32 {
    let t = now() as u64;
    if c.a(0) != 0 {
        mem::w64(c.a(0), t);
    }
    c.ret64(t)
}

/// _splitpath(_s) / _wsplitpath(_s): path -> drive, dir, fname, ext (each pointer may be 0;
/// the _s forms give each one's size after it)
fn splitpath(c: &mut Ctx, wide: bool, sized: bool) -> u32 {
    let path: Vec<u16> = if wide { wstr(c.a(0)) } else { crate::text::a2w(&cstr(c.a(0))) };
    let is_sep = |ch: u16| ch == b'\\' as u16 || ch == b'/' as u16;
    let drive = if path.len() >= 2 && path[1] == b':' as u16 { 2 } else { 0 };
    let rest = &path[drive..];
    let dir_end = rest.iter().rposition(|&ch| is_sep(ch)).map_or(0, |i| i + 1);
    let name = &rest[dir_end..];
    let dot = name.iter().rposition(|&ch| ch == b'.' as u16).unwrap_or(name.len());
    let parts: [&[u16]; 4] = [&path[..drive], &rest[..dir_end], &name[..dot], &name[dot..]];
    for (k, part) in parts.iter().enumerate() {
        let (ptr, cap) = if sized { (c.a(1 + 2 * k as u32), c.a(2 + 2 * k as u32)) } else { (c.a(1 + k as u32), 260) };
        if ptr == 0 {
            continue;
        }
        if wide {
            mem::put_wstr(ptr, cap, part);
        } else {
            mem::put_cstr(ptr, cap, &crate::text::w2a(part));
        }
    }
    0
}
fn _splitpath(c: &mut Ctx) -> u32 {
    splitpath(c, false, false)
}
fn _wsplitpath(c: &mut Ctx) -> u32 {
    splitpath(c, true, false)
}
fn _splitpath_s(c: &mut Ctx) -> u32 {
    splitpath(c, false, true)
}
fn _wsplitpath_s(c: &mut Ctx) -> u32 {
    splitpath(c, true, true)
}

/// _get_narrow/wide_winmain_command_line: the command line after the program name
fn winmain_cmdline(wide: bool) -> u32 {
    let (a, w) = crate::k32::cmdline();
    let (p, step) = if wide { (w, 2) } else { (a, 1) };
    let ch = |at: u32| if wide { r16(at) as u32 } else { r8(at) as u32 };
    let mut at = p;
    let mut quoted = false;
    while ch(at) != 0 && (quoted || ch(at) > b' ' as u32) {
        if ch(at) == b'"' as u32 {
            quoted = !quoted;
        }
        at += step;
    }
    while ch(at) != 0 && ch(at) <= b' ' as u32 {
        at += step;
    }
    at
}
fn _get_narrow_winmain_command_line(_: &mut Ctx) -> u32 {
    winmain_cmdline(false)
}
fn _get_wide_winmain_command_line(_: &mut Ctx) -> u32 {
    winmain_cmdline(true)
}

fn clock(_: &mut Ctx) -> u32 {
    crate::k32::ticks_ms().wrapping_sub(60_000)
}

// ---------------------------------------------------------------------------------------------
// printf

// The legacy runtimes' printf family (msvcrt, msvcr*): crt::fmt in the legacy dialect. A
// format error (an unknown conversion in the UCRT's dialect, a result past MAX_OUT) gives -1.

fn pf<T: Unit>(fmt: u32, va: &mut VaArgs, m: crt::fmt::Mode) -> Option<Vec<T>> {
    match crate::misc::printf(&gstr::<T>(fmt), va, m) {
        Ok(s) => Some(s),
        Err(_) => {
            set_errno(22);
            None
        }
    }
}
fn legacy<T: Unit>(fmt: u32, va: &mut VaArgs) -> Option<Vec<T>> {
    pf(fmt, va, crate::misc::legacy_mode())
}
/// sprintf / vsprintf / swprintf (no count): all of it and the NUL
fn put_all<T: Unit>(out: u32, s: Option<Vec<T>>) -> u32 {
    let Some(s) = s else { return u32::MAX };
    if out == 0 {
        set_errno(22);
        return u32::MAX;
    }
    put_units(out, &s);
    put_nul::<T>(out + unit::<T>() * s.len() as u32);
    s.len() as u32
}
/// _snprintf / _vsnprintf / _snwprintf (buf, count): shorter than count: it and a NUL; exactly
/// count: no NUL, the length; longer: count units, no NUL, -1
fn put_count<T: Unit>(out: u32, count: u32, s: Option<Vec<T>>) -> u32 {
    let Some(s) = s else { return u32::MAX };
    let n = s.len().min(count as usize);
    if out == 0 && n != 0 {
        set_errno(22);
        return u32::MAX;
    }
    put_units(out, &s[..n]);
    if s.len() < count as usize {
        put_nul::<T>(out + unit::<T>() * n as u32);
    }
    if s.len() <= count as usize { s.len() as u32 } else { u32::MAX }
}
/// the secure forms (buf, size[, count]): sprintf_s / swprintf_s without a count; _snprintf_s
/// with one, _TRUNCATE (-1) writing what fits. What does not fit: an empty buffer, ERANGE, -1
/// (_TRUNCATE: the truncated text, -1); count cuts short: that much, -1.
fn put_s<T: Unit>(out: u32, size: u32, count: Option<u32>, s: Option<Vec<T>>) -> u32 {
    if out == 0 || size == 0 {
        set_errno(22);
        return u32::MAX;
    }
    let Some(s) = s else {
        put_nul::<T>(out);
        return u32::MAX;
    };
    let truncate = count == Some(u32::MAX);
    let want = match count {
        Some(n) if !truncate => s.len().min(n as usize),
        _ => s.len(),
    };
    if want >= size as usize {
        if truncate {
            let n = size as usize - 1;
            put_units(out, &s[..n]);
            put_nul::<T>(out + unit::<T>() * n as u32);
        } else {
            put_nul::<T>(out);
            set_errno(34);
        }
        return u32::MAX;
    }
    put_units(out, &s[..want]);
    put_nul::<T>(out + unit::<T>() * want as u32);
    if want < s.len() { u32::MAX } else { want as u32 }
}
fn sprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(1), &mut VaArgs::arg(c, 2));
    put_all(c.a(0), s)
}
fn vsprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(1), &mut VaArgs::stack(c.a(2)));
    put_all(c.a(0), s)
}
fn _snprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(2), &mut VaArgs::arg(c, 3));
    put_count(c.a(0), size(c, 1), s)
}
fn _vsnprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(2), &mut VaArgs::stack(c.a(3)));
    put_count(c.a(0), size(c, 1), s)
}
fn swprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(1), &mut VaArgs::arg(c, 2));
    put_all(c.a(0), s)
}
// the secure and counted variants (msvcr80+): `cap` is the buffer's size in characters
/// sprintf_s(buf, size, fmt, ...)
fn sprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(2), &mut VaArgs::arg(c, 3));
    put_s(c.a(0), size(c, 1), None, s)
}
fn vsprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(2), &mut VaArgs::stack(c.a(3)));
    put_s(c.a(0), size(c, 1), None, s)
}
/// _snprintf_s(buf, size, count, fmt, ...)
fn _snprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(3), &mut VaArgs::arg(c, 4));
    put_s(c.a(0), size(c, 1), Some(size(c, 2)), s)
}
fn _vsnprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u8>(c.a(3), &mut VaArgs::stack(c.a(4)));
    put_s(c.a(0), size(c, 1), Some(size(c, 2)), s)
}
/// _vscprintf(fmt, va): the length
fn _vscprintf(c: &mut Ctx) -> u32 {
    legacy::<u8>(c.a(0), &mut VaArgs::stack(c.a(1))).map_or(u32::MAX, |s| s.len() as u32)
}
fn _scprintf(c: &mut Ctx) -> u32 {
    legacy::<u8>(c.a(0), &mut VaArgs::arg(c, 1)).map_or(u32::MAX, |s| s.len() as u32)
}
/// swprintf_s(buf, count, fmt, ...)
fn swprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(2), &mut VaArgs::arg(c, 3));
    put_s(c.a(0), size(c, 1), None, s)
}
fn vswprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(2), &mut VaArgs::stack(c.a(3)));
    put_s(c.a(0), size(c, 1), None, s)
}
/// _snwprintf(buf, count, fmt, ...) / _vsnwprintf
fn _snwprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(2), &mut VaArgs::arg(c, 3));
    put_count(c.a(0), size(c, 1), s)
}
fn _vsnwprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(2), &mut VaArgs::stack(c.a(3)));
    put_count(c.a(0), size(c, 1), s)
}
/// _snwprintf_s(buf, size, count, fmt, ...)
fn _snwprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(3), &mut VaArgs::arg(c, 4));
    put_s(c.a(0), size(c, 1), Some(size(c, 2)), s)
}
fn _vsnwprintf_s(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(3), &mut VaArgs::stack(c.a(4)));
    put_s(c.a(0), size(c, 1), Some(size(c, 2)), s)
}
fn vswprintf(c: &mut Ctx) -> u32 {
    let s = legacy::<u16>(c.a(1), &mut VaArgs::stack(c.a(2)));
    put_all(c.a(0), s)
}
fn _vscwprintf(c: &mut Ctx) -> u32 {
    legacy::<u16>(c.a(0), &mut VaArgs::stack(c.a(1))).map_or(u32::MAX, |s| s.len() as u32)
}
fn _scwprintf(c: &mut Ctx) -> u32 {
    legacy::<u16>(c.a(0), &mut VaArgs::arg(c, 1)).map_or(u32::MAX, |s| s.len() as u32)
}
fn printf(c: &mut Ctx) -> u32 {
    let Some(s) = legacy::<u8>(c.a(0), &mut VaArgs::arg(c, 1)) else { return u32::MAX };
    eprint!("{}", a2s(&s));
    s.len() as u32
}
fn vprintf(c: &mut Ctx) -> u32 {
    let Some(s) = legacy::<u8>(c.a(0), &mut VaArgs::stack(c.a(1))) else { return u32::MAX };
    eprint!("{}", a2s(&s));
    s.len() as u32
}
// The scanf family: crt::scan with the pointers from the guest's variable arguments.

struct GuestSink(VaArgs);

impl crt::scan::Sink for GuestSink {
    fn ptr(&mut self) -> u64 {
        self.0.u32() as u64
    }
    fn size(&mut self) -> u64 {
        self.0.u32() as u64
    }
    fn int(&mut self, p: u64, bytes: u32, v: u64) {
        mem::write(p as u32, &v.to_le_bytes()[..bytes.min(8) as usize]);
    }
    fn float(&mut self, p: u64, bytes: u32, v: f64) {
        if bytes == 4 { mem::write(p as u32, &(v as f32).to_le_bytes()) } else { mem::write(p as u32, &v.to_le_bytes()) }
    }
    fn narrow(&mut self, p: u64, s: &[u8]) {
        mem::write(p as u32, s);
    }
    fn wide(&mut self, p: u64, s: &[u16]) {
        put_units(p as u32, s);
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        crate::text::a2w(s)
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        crate::text::w2a(s)
    }
}
fn scan_mode(secure: bool) -> crt::scan::Mode {
    crt::scan::Mode { ptr64: mem::x64(), secure, legacy: true, legacy_wide: true }
}
/// sscanf / swscanf and the _s forms (input, fmt, ...); vsscanf (input, fmt, va)
fn scanf<T: Unit>(input: u32, fmt: u32, va: VaArgs, m: crt::scan::Mode) -> u32 {
    if input == 0 || fmt == 0 {
        set_errno(22);
        return u32::MAX;
    }
    crt::scan::scan(&gstr::<T>(input), &gstr::<T>(fmt), m, &mut GuestSink(va)) as u32
}
fn sscanf(c: &mut Ctx) -> u32 {
    scanf::<u8>(c.a(0), c.a(1), VaArgs::arg(c, 2), scan_mode(false))
}
fn sscanf_s(c: &mut Ctx) -> u32 {
    scanf::<u8>(c.a(0), c.a(1), VaArgs::arg(c, 2), scan_mode(true))
}
fn swscanf(c: &mut Ctx) -> u32 {
    scanf::<u16>(c.a(0), c.a(1), VaArgs::arg(c, 2), scan_mode(false))
}
fn swscanf_s(c: &mut Ctx) -> u32 {
    scanf::<u16>(c.a(0), c.a(1), VaArgs::arg(c, 2), scan_mode(true))
}
fn vsscanf(c: &mut Ctx) -> u32 {
    scanf::<u8>(c.a(0), c.a(1), VaArgs::stack(c.a(2)), scan_mode(false))
}
fn vswscanf(c: &mut Ctx) -> u32 {
    scanf::<u16>(c.a(0), c.a(1), VaArgs::stack(c.a(2)), scan_mode(false))
}

// ---------------------------------------------------------------------------------------------
// math: doubles come on the stack and go back in ST0; the _CI* forms take ST0 (and ST1)

fn ret(c: &mut Ctx, v: f64) -> u32 {
    c.st0 = Some(v);
    0
}
macro_rules! math1 {
    ($($name:ident = $f:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { let x = c.f64(0); ret(c, $f(x)) } )* };
}
math1!(floor = f64::floor, ceil = f64::ceil, fabs = f64::abs);
/// msvcrt's errno for math: EDOM when a number gives NaN, ERANGE when finite numbers give an
/// infinity (log(0) too).
fn math_errno(args: &[f64], r: f64) {
    if r.is_nan() && !args.iter().any(|a| a.is_nan()) {
        set_errno(33);
    } else if r.is_infinite() && args.iter().all(|a| a.is_finite()) {
        set_errno(34);
    }
}
macro_rules! math1e {
    ($($name:ident = $f:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { let x = c.f64(0); let r = $f(x); math_errno(&[x], r); ret(c, r) } )* };
}
math1e!(sqrt = f64::sqrt, sin = f64::sin, cos = f64::cos, tan = f64::tan, asin = f64::asin, acos = f64::acos,
    atan = f64::atan, exp = f64::exp, log = f64::ln, log10 = f64::log10, sinh = f64::sinh, cosh = f64::cosh, tanh = f64::tanh);
fn pow(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, y) = (a.f64(c), a.f64(c));
    let r = x.powf(y);
    if x != 0.0 {
        math_errno(&[x, y], r);
    }
    ret(c, r)
}
fn atan2(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (y, x) = (a.f64(c), a.f64(c));
    ret(c, y.atan2(x))
}
fn fmod(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, y) = (a.f64(c), a.f64(c));
    let r = x % y;
    math_errno(&[x, y], r);
    ret(c, if r == 0.0 { 0f64.copysign(x) } else { r })
}
fn ldexp(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, e) = (a.f64(c), a.p(c) as i32);
    ret(c, scale(x, e))
}
/// x * 2^e, rounded once (as ldexp)
fn scale(x: f64, e: i32) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let (m, ex) = frexp_of(x);
    let t = (ex as i64 + e as i64).clamp(-1200, 1100);
    if t > 1024 {
        return f64::INFINITY.copysign(x);
    }
    if t >= -1021 {
        return f64::from_bits(m.to_bits() & !(0x7ff << 52) | ((t + 1022) as u64) << 52);
    }
    // (subnormal: an exact normal times the smallest subnormal, one rounding)
    let y = f64::from_bits(m.to_bits() & !(0x7ff << 52) | ((t + 1074 + 1022) as u64) << 52);
    y * f64::from_bits(1)
}
fn frexp(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, pe) = (a.f64(c), a.p(c));
    let (m, e) = frexp_of(x);
    mem::put32(pe, e as u32);
    ret(c, m)
}
/// (mantissa in [0.5, 1), exponent); 0, infinities and NaN: themselves and 0
fn frexp_of(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let (x, bias) = if x.is_normal() { (x, 0) } else { (x * 2f64.powi(64), -64) };
    let b = x.to_bits();
    let e = ((b >> 52) & 0x7ff) as i32 - 1022;
    (f64::from_bits(b & !(0x7ff << 52) | 1022 << 52), e + bias)
}
fn modf(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, p) = (a.f64(c), a.p(c));
    let i = x.trunc();
    mem::write(p, &i.to_le_bytes());
    // (infinities: a fraction of +-0; NaN: NaN)
    ret(c, if x.is_infinite() { 0f64.copysign(x) } else { (x - i).copysign(x) })
}
fn modff(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, p) = (a.f32(c), a.p(c));
    let i = x.trunc();
    mem::write(p, &i.to_le_bytes());
    ret_f32(c, if x.is_infinite() { 0f32.copysign(x) } else { (x - i).copysign(x) })
}
// the float forms (x64's ucrtbase exports them; x86 code mostly widens to double)
macro_rules! mathf1 {
    ($($name:ident = $f:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { let x = Args(0).f32(c); ret_f32(c, $f(x)) } )* };
}
mathf1!(floorf = f32::floor, ceilf = f32::ceil, sqrtf = f32::sqrt, sinf = f32::sin, cosf = f32::cos, tanf = f32::tan,
    asinf = f32::asin, acosf = f32::acos, atanf = f32::atan, expf = f32::exp, logf = f32::ln, log10f = f32::log10,
    fabsf = f32::abs, sinhf = f32::sinh, coshf = f32::cosh, tanhf = f32::tanh, log2f = f32::log2, exp2f = f32::exp2,
    roundf = f32::round, truncf = f32::trunc, cbrtf = f32::cbrt);
macro_rules! mathf2 {
    ($($name:ident = $f:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { let mut a = Args(0); let (x, y) = (a.f32(c), a.f32(c)); ret_f32(c, $f(x, y)) } )* };
}
mathf2!(powf = f32::powf, atan2f = f32::atan2, fmodf = |x: f32, y: f32| x % y, hypotf = f32::hypot,
    fmaxf = f32::max, fminf = f32::min, copysignf = f32::copysign);
// C99 doubles
math1!(round = f64::round, trunc = f64::trunc, log2 = f64::log2, exp2 = f64::exp2, cbrt = f64::cbrt,
    log1p = f64::ln_1p, expm1 = f64::exp_m1, asinh = f64::asinh, acosh = f64::acosh, atanh = f64::atanh);
macro_rules! math2 {
    ($($name:ident = $f:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { let mut a = Args(0); let (x, y) = (a.f64(c), a.f64(c)); ret(c, $f(x, y)) } )* };
}
math2!(hypot = f64::hypot, fmax = f64::max, fmin = f64::min, copysign = f64::copysign);
/// lround / lrint-likes: the long result (out of range: LONG_MIN, as the UCRT's)
fn lround(c: &mut Ctx) -> u32 {
    let x = Args(0).f64(c).round();
    if x >= -2147483648.0 && x < 2147483648.0 { x as i32 as u32 } else { 0x8000_0000 }
}
fn llround(c: &mut Ctx) -> u32 {
    let x = Args(0).f64(c).round();
    let v = if x >= -9.223372036854776e18 && x < 9.223372036854776e18 { x as i64 } else { i64::MIN };
    c.ret64(v as u64)
}

fn st_pop(c: &mut Ctx) -> f64 {
    let v = c.cpu.fpu.st(0);
    c.cpu.fpu.pop();
    v
}
macro_rules! ci1 {
    ($($name:ident = $f:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { let x = st_pop(c); ret(c, $f(x)) } )* };
}
ci1!(_CIsqrt = f64::sqrt, _CIsin = f64::sin, _CIcos = f64::cos, _CItan = f64::tan, _CIasin = f64::asin,
    _CIacos = f64::acos, _CIatan = f64::atan, _CIexp = f64::exp, _CIlog = f64::ln, _CIlog10 = f64::log10,
    _CIsinh = f64::sinh, _CIcosh = f64::cosh, _CItanh = f64::tanh);
/// _libm_sse2_*_precise: the argument and result are in xmm0 (pow: xmm0, xmm1)
fn xmm_f64(c: &mut Ctx, i: usize) -> f64 {
    f64::from_bits(c.cpu.xmm[i] as u64)
}
fn xmm_ret(c: &mut Ctx, v: f64) -> u32 {
    c.cpu.xmm[0] = (c.cpu.xmm[0] & !(u64::MAX as u128)) | v.to_bits() as u128;
    0
}
macro_rules! sse1 {
    ($($name:ident = $f:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { let x = xmm_f64(c, 0); xmm_ret(c, $f(x)) } )* };
}
sse1!(_libm_sse2_sin_precise = f64::sin, _libm_sse2_cos_precise = f64::cos, _libm_sse2_tan_precise = f64::tan,
    _libm_sse2_asin_precise = f64::asin, _libm_sse2_acos_precise = f64::acos, _libm_sse2_atan_precise = f64::atan,
    _libm_sse2_exp_precise = f64::exp, _libm_sse2_log_precise = f64::ln, _libm_sse2_log10_precise = f64::log10,
    _libm_sse2_sqrt_precise = f64::sqrt);
fn _libm_sse2_pow_precise(c: &mut Ctx) -> u32 {
    let (x, y) = (xmm_f64(c, 0), xmm_f64(c, 1));
    xmm_ret(c, x.powf(y))
}
fn _CIpow(c: &mut Ctx) -> u32 {
    let y = st_pop(c);
    let x = st_pop(c);
    ret(c, x.powf(y))
}
fn _CIatan2(c: &mut Ctx) -> u32 {
    let x = st_pop(c);
    let y = st_pop(c);
    ret(c, y.atan2(x))
}
fn _CIfmod(c: &mut Ctx) -> u32 {
    let y = st_pop(c);
    let x = st_pop(c);
    ret(c, x % y)
}
fn _ftol(c: &mut Ctx) -> u32 {
    let v = st_pop(c);
    let i = if v.is_finite() && v.abs() < 9.2e18 { v.trunc() as i64 } else { i64::MIN };
    c.ret64(i as u64)
}

// ---------------------------------------------------------------------------------------------
// stdio: a FILE is msvcrt's size of guest bytes that we only use as a key

/// msvcrt's FILE: 32 bytes (x64: 48)
fn file_size() -> u32 {
    if mem::x64() { 48 } else { 32 }
}

struct Stream {
    f: Option<Arc<File>>,
    /// its descriptor (_fileno)
    fd: u32,
    eof: bool,
    unget: Option<u8>,
}

static FILES: Mutex<Option<HashMap<u32, Stream>>> = Mutex::new(None);

fn iob() -> u32 {
    let p = mem::heap_alloc(file_size() * 20, true);
    let mut g = FILES.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
    for i in 0..3 {
        m.insert(p + file_size() * i, Stream { f: None, fd: i, eof: true, unget: None });
    }
    p
}

fn with_file<R>(fp: u32, f: impl FnOnce(&mut Stream) -> R) -> Option<R> {
    FILES.lock().unwrap().get_or_insert_with(HashMap::default).get_mut(&fp).map(f)
}

fn fopen_mode(path: &str, mode: &[u8]) -> Option<File> {
    let (access, disp) = match mode.first() {
        Some(b'r') if mode.contains(&b'+') => (0xc000_0000, 3),
        Some(b'r') => (0x8000_0000, 3),
        Some(b'w') => (0xc000_0000, 2),
        Some(b'a') => (0xc000_0000, 4),
        _ => return None,
    };
    let f = crate::files::open(path, access, disp).ok()?;
    if mode.first() == Some(&b'a') {
        f.seek(SeekFrom::End(0));
    }
    Some(f)
}
fn new_stream(f: File) -> u32 {
    let f = Arc::new(f);
    let fd = io::fd_of_file(f.clone());
    new_stream_of(f, fd)
}
/// A stream over descriptor `fd`'s file (fclose closes the descriptor too).
fn new_stream_of(f: Arc<File>, fd: u32) -> u32 {
    let fp = mem::heap_alloc(file_size(), true);
    FILES.lock().unwrap().get_or_insert_with(HashMap::default).insert(fp, Stream { f: Some(f), fd, eof: false, unget: None });
    fp
}
/// _get_stream_buffer_pointers(stream, &base, &ptr, &cnt): the FILE's buffer fields (UCRT
/// order _ptr, _base, _cnt); they stay 0, so std::basic_filebuf always reads through the CRT
fn _get_stream_buffer_pointers(c: &mut Ctx) -> u32 {
    let fp = c.a(0);
    for (out, off) in [(c.a(1), ps()), (c.a(2), 0), (c.a(3), 2 * ps())] {
        putp(out, fp + off);
    }
    0
}
fn fopen(c: &mut Ctx) -> u32 {
    let path = a2s(&cstr(c.a(0)));
    match fopen_mode(&path, &cstr(c.a(1))) {
        Some(f) => new_stream(f),
        None => {
            set_errno(2);
            0
        }
    }
}
fn _wfopen(c: &mut Ctx) -> u32 {
    let path = String::from_utf16_lossy(&wstr(c.a(0)));
    let mode: Vec<u8> = wstr(c.a(1)).iter().map(|&u| u as u8).collect();
    match fopen_mode(&path, &mode) {
        Some(f) => new_stream(f),
        None => {
            set_errno(2);
            0
        }
    }
}
fn remove(c: &mut Ctx) -> u32 {
    let path = a2s(&cstr(c.a(0)));
    let Some(path) = crate::files::resolve(&path) else {
        set_errno(2);
        return u32::MAX;
    };
    if kura::fs::remove_file(path).is_ok() { 0 } else { set_errno(13); u32::MAX }
}
fn rename(c: &mut Ctx) -> u32 {
    let from = a2s(&cstr(c.a(0)));
    let to = a2s(&cstr(c.a(1)));
    let Some(from) = crate::files::resolve(&from) else {
        set_errno(2);
        return u32::MAX;
    };
    if crate::files::resolve(&to).is_some() {
        set_errno(17); // EEXIST
        return u32::MAX;
    }
    if kura::fs::rename(from, crate::files::resolve_new(&to)).is_ok() { 0 } else { set_errno(13); u32::MAX }
}
fn fclose(c: &mut Ctx) -> u32 {
    let fp = c.a(0);
    let had = FILES.lock().unwrap().get_or_insert_with(HashMap::default).remove(&fp);
    if let Some(s) = had {
        if s.fd > 2 {
            io::close_fd(s.fd);
        }
        mem::heap_free(fp);
        0
    } else {
        u32::MAX
    }
}
fn fread(c: &mut Ctx) -> u32 {
    fread_to(c.a(0), c.a(1), c.a(2), c.a(3))
}
/// fread_s(buf, bufsize, size, n, fp): fread of what fits in bufsize
fn fread_s(c: &mut Ctx) -> u32 {
    let (buf, cap, size, n, fp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if size == 0 {
        return 0;
    }
    fread_to(buf, size, n.min(cap / size), fp)
}
fn fread_to(buf: u32, size: u32, n: u32, fp: u32) -> u32 {
    let total = size.saturating_mul(n);
    if total == 0 {
        return 0;
    }
    with_file(fp, |s| {
        let Some(f) = s.f.clone() else { return 0 };
        let mut at = buf;
        let mut got = 0;
        if let Some(b) = s.unget.take() {
            w8(at, b);
            at += 1;
            got = 1;
        }
        got += f.read_into(at, total - got);
        if got < total {
            s.eof = true;
        }
        got / size
    })
    .unwrap_or(0)
}
fn fwrite(c: &mut Ctx) -> u32 {
    let (buf, size, n, fp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let total = size.saturating_mul(n);
    if total == 0 {
        return 0;
    }
    with_file(fp, |s| match &s.f {
        Some(f) => f.write_from(buf, total) / size,
        None => {
            eprint!("{}", a2s(mem::slice(buf, total)));
            n
        }
    })
    .unwrap_or(0)
}
fn fseek(c: &mut Ctx) -> u32 {
    seek(c.a(0), c.i(1) as i64, c.a(2))
}
fn seek(fp: u32, off: i64, whence: u32) -> u32 {
    // (msvcrt clears the end-of-file flag first, also when the seek then fails)
    with_file(fp, |s| s.eof = false);
    if fp == 0 || whence > 2 || (whence == 0 && off < 0) {
        set_errno(22); // EINVAL
        return u32::MAX;
    }
    with_file(fp, |s| {
        let Some(f) = &s.f else { return u32::MAX };
        let pos = match whence {
            0 => SeekFrom::Start(off as u64),
            1 => SeekFrom::Current(off - s.unget.is_some() as i64),
            _ => SeekFrom::End(off),
        };
        if f.seek(pos).is_some() {
            s.unget = None;
            s.eof = false;
            0
        } else {
            set_errno(22);
            u32::MAX
        }
    })
    .unwrap_or(u32::MAX)
}
fn ftell(c: &mut Ctx) -> u32 {
    with_file(c.a(0), |s| {
        let Some(f) = &s.f else { return u32::MAX };
        let p = f.seek(SeekFrom::Current(0)).unwrap_or(0) as u32;
        p - s.unget.is_some() as u32
    })
    .unwrap_or(u32::MAX)
}
/// fgetpos(stream, fpos_t*) / fsetpos: the position as a 64-bit offset
fn fgetpos(c: &mut Ctx) -> u32 {
    let p = ftell(c);
    if p == u32::MAX {
        return u32::MAX;
    }
    mem::w64(c.a(1), p as u64);
    0
}
/// _ftelli64(stream) / _fseeki64(stream, offset (64-bit), whence)
fn _ftelli64(c: &mut Ctx) -> u32 {
    let p = ftell(c);
    c.ret64(if p == u32::MAX { u64::MAX } else { p as u64 })
}
fn _fseeki64(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (fp, off, whence) = (a.p(c), a.u64(c) as i64, a.p(c));
    seek(fp, off, whence)
}
fn fsetpos(c: &mut Ctx) -> u32 {
    let (fp, off) = (c.a(0), mem::r64(c.a(1)));
    with_file(fp, |s| {
        let Some(f) = &s.f else { return u32::MAX };
        s.unget = None;
        s.eof = false;
        if f.seek(SeekFrom::Start(off)).is_some() { 0 } else { u32::MAX }
    })
    .unwrap_or(u32::MAX)
}
fn ___lc_collate_cp_func(_: &mut Ctx) -> u32 {
    932
}
// ---------------------------------------------------------------------------------------------
// _findfirst / _findnext: a FindFirstFile handle; the data's layout by time and size widths

/// (time 64-bit, size 64-bit, wide)
fn find_write(p: u32, (t64, s64, wide): (bool, bool, bool), name: &str, m: &kura::fs::Metadata) {
    let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let size = if m.is_dir() { 0 } else { m.len() };
    w32(p, crate::files::attrs(m) & 0x37);
    let mut o = if t64 { 8 } else { 4 };
    for _ in 0..3 {
        w32(p + o, t as u32);
        if t64 {
            w32(p + o + 4, (t >> 32) as u32);
        }
        o += if t64 { 8 } else { 4 };
    }
    if s64 {
        o = (o + 7) & !7;
        w32(p + o, size as u32);
        w32(p + o + 4, (size >> 32) as u32);
        o += 8;
    } else {
        w32(p + o, size as u32);
        o += 4;
    }
    if wide {
        let w: Vec<u16> = name.encode_utf16().collect();
        mem::put_wstr(p + o, 260, &w);
    } else {
        mem::put_cstr(p + o, 260, &crate::text::s2a(name));
    }
}
fn find_first(c: &mut Ctx, kind: (bool, bool, bool)) -> u32 {
    let pat = if kind.2 { String::from_utf16_lossy(&wstr(c.a(0))) } else { a2s(&cstr(c.a(0))) };
    match crate::files::find(&pat) {
        Some(mut f) if !f.entries.is_empty() => {
            let (n, m) = &f.entries[0];
            find_write(c.a(1), kind, n, m);
            f.at = 1;
            crate::obj::add(crate::obj::Obj::Find(Arc::new(Mutex::new(f))))
        }
        _ => {
            set_errno(2);
            u32::MAX
        }
    }
}
fn find_next(c: &mut Ctx, kind: (bool, bool, bool)) -> u32 {
    let f = crate::obj::with(|t| match t.objs.get(&c.a(0)) {
        Some((crate::obj::Obj::Find(f), _)) => Some(f.clone()),
        _ => None,
    });
    let Some(f) = f else { return u32::MAX };
    let mut f = f.lock().unwrap();
    if f.at >= f.entries.len() {
        set_errno(2);
        return u32::MAX;
    }
    let (n, m) = &f.entries[f.at];
    find_write(c.a(1), kind, n, m);
    f.at += 1;
    0
}
fn _findclose(c: &mut Ctx) -> u32 {
    if crate::obj::close(c.a(0)) { 0 } else { u32::MAX }
}
macro_rules! finds {
    ($($first:ident, $next:ident = $k:expr;)*) => { $(
        fn $first(c: &mut Ctx) -> u32 { find_first(c, $k) }
        fn $next(c: &mut Ctx) -> u32 { find_next(c, $k) }
    )* };
}
finds! {
    _findfirst32, _findnext32 = (false, false, false);
    _findfirst32i64, _findnext32i64 = (false, true, false);
    _findfirst64i32, _findnext64i32 = (true, false, false);
    _findfirst64, _findnext64 = (true, true, false);
    _wfindfirst32, _wfindnext32 = (false, false, true);
    _wfindfirst32i64, _wfindnext32i64 = (false, true, true);
    _wfindfirst64i32, _wfindnext64i32 = (true, false, true);
    _wfindfirst64, _wfindnext64 = (true, true, true);
}

fn rewind(c: &mut Ctx) -> u32 {
    with_file(c.a(0), |s| {
        s.unget = None;
        s.eof = false;
        if let Some(f) = &s.f {
            f.seek(SeekFrom::Start(0));
        }
    });
    0
}
fn feof(c: &mut Ctx) -> u32 {
    with_file(c.a(0), |s| s.eof as u32).unwrap_or(1)
}
fn ferror(_: &mut Ctx) -> u32 {
    0
}
fn fflush(_: &mut Ctx) -> u32 {
    0
}
fn getc_of(fp: u32) -> u32 {
    with_file(fp, |s| {
        if let Some(b) = s.unget.take() {
            return b as u32;
        }
        let Some(f) = s.f.clone() else { return u32::MAX };
        let tmp = mem::heap_alloc(4, false);
        let got = f.read_into(tmp, 1);
        let b = r8(tmp);
        mem::heap_free(tmp);
        if got == 0 {
            s.eof = true;
            u32::MAX
        } else {
            b as u32
        }
    })
    .unwrap_or(u32::MAX)
}
fn fgetc(c: &mut Ctx) -> u32 {
    getc_of(c.a(0))
}
fn ungetc(c: &mut Ctx) -> u32 {
    let (ch, fp) = (c.a(0), c.a(1));
    if ch == u32::MAX {
        return ch;
    }
    with_file(fp, |s| {
        s.unget = Some(ch as u8);
        s.eof = false;
        ch
    })
    .unwrap_or(u32::MAX)
}
fn fgets(c: &mut Ctx) -> u32 {
    let (buf, n, fp) = (c.a(0), c.a(1), c.a(2));
    if n == 0 {
        return 0;
    }
    let mut i = 0;
    while i + 1 < n {
        let ch = getc_of(fp);
        if ch == u32::MAX {
            break;
        }
        w8(buf + i, ch as u8);
        i += 1;
        if ch == b'\n' as u32 {
            break;
        }
    }
    if i == 0 {
        return 0;
    }
    w8(buf + i, 0);
    buf
}
fn fputs(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0));
    with_file(c.a(1), |st| match &st.f {
        Some(f) => f.write_from(c.a(0), s.len() as u32),
        None => {
            eprint!("{}", a2s(&s));
            0
        }
    });
    0
}
/// Writes `s` to the stream (stdout / stderr: the log).
fn write_stream(fp: u32, s: &[u8]) {
    let tmp = mem::heap_alloc(s.len() as u32 + 1, false);
    mem::write(tmp, s);
    with_file(fp, |st| match &st.f {
        Some(f) => {
            f.write_from(tmp, s.len() as u32);
        }
        None => eprint!("{}", a2s(s)),
    });
    mem::heap_free(tmp);
}
fn fprintf(c: &mut Ctx) -> u32 {
    let Some(s) = legacy::<u8>(c.a(1), &mut VaArgs::arg(c, 2)) else { return u32::MAX };
    write_stream(c.a(0), &s);
    s.len() as u32
}
fn vfprintf(c: &mut Ctx) -> u32 {
    let Some(s) = legacy::<u8>(c.a(1), &mut VaArgs::stack(c.a(2))) else { return u32::MAX };
    write_stream(c.a(0), &s);
    s.len() as u32
}

// the VC8+ CRT's pointer encoding (identity: nothing to protect here) and debug hooks
fn _encode_pointer(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn _encoded_null(_: &mut Ctx) -> u32 {
    0
}
fn _crt_debugger_hook(_: &mut Ctx) -> u32 {
    0
}
fn _configthreadlocale(_: &mut Ctx) -> u32 {
    // _DISABLE_PER_THREAD_LOCALE
    2
}
fn ___mb_cur_max_l_func(_: &mut Ctx) -> u32 {
    if crate::text::acp() == 932 { 2 } else { 1 }
}

fn wcsncmp(c: &mut Ctx) -> u32 {
    let n = c.a(2) as usize;
    let a: Vec<u16> = wstr(c.a(0)).into_iter().chain([0]).take(n).collect();
    let b: Vec<u16> = wstr(c.a(1)).into_iter().chain([0]).take(n).collect();
    a.cmp(&b) as i32 as u32
}
fn getenv(c: &mut Ctx) -> u32 {
    // the CRT's environment (io::env_get), kept in a guest buffer per name
    static VALS: Mutex<Option<HashMap<Vec<u8>, u32>>> = Mutex::new(None);
    let name = cstr(c.a(0));
    let Some(v) = io::env_get(&a2s(&name)) else { return 0 };
    let bytes = crate::text::s2a(&v);
    let mut g = VALS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
    if let Some(&p) = m.get(&name.to_ascii_uppercase()) {
        mem::heap_free(p);
    }
    let p = mem::heap_alloc(bytes.len() as u32 + 1, true);
    mem::write(p, &bytes);
    m.insert(name.to_ascii_uppercase(), p);
    p
}
fn _ismbblead(c: &mut Ctx) -> u32 {
    let b = c.a(0) & 0xff;
    (crate::text::acp() == 932 && matches!(b, 0x81..=0x9f | 0xe0..=0xfc)) as u32
}
/// memcpy_s / memmove_s (dst, dst size, src, count): crt::sec (memcpy_s zeroes the
/// destination on an error, memmove_s leaves it)
fn memcpy_s(c: &mut Ctx) -> u32 {
    mem_s(c, crt::sec::memcpy_s)
}
fn memmove_s(c: &mut Ctx) -> u32 {
    mem_s(c, crt::sec::memmove_s)
}
fn mem_s(c: &mut Ctx, f: fn(Option<&mut [u8]>, Option<&[u8]>, usize) -> i32) -> u32 {
    let (d, dn, s, n) = (c.a(0), size(c, 1), c.a(2), size(c, 3));
    let dn = dn.min(0u32.wrapping_sub(d.max(1)));
    // (the source as a copy: memmove_s's overlap is then no matter)
    let src = (s != 0).then(|| mem::slice(s, n.min(dn.saturating_add(1)).min(0u32.wrapping_sub(s))).to_vec());
    let e = f((d != 0).then(|| mem::slice_mut(d, dn)), src.as_deref(), n as usize);
    errno_of(e);
    e as u32
}
fn _controlfp_s(c: &mut Ctx) -> u32 {
    let (out, new, mask) = (c.a(0), c.a(1), c.a(2) & !0x80000);
    let v = set_fp_control(c, new, mask);
    if out != 0 {
        w32(out, v);
    }
    0
}
fn __iob_func(_: &mut Ctx) -> u32 {
    data("_iob").unwrap_or(0)
}
fn __p__iob(c: &mut Ctx) -> u32 {
    __iob_func(c)
}
fn fopen_s(c: &mut Ctx) -> u32 {
    let (pf, path, mode) = (c.a(0), a2s(&cstr(c.a(1))), cstr(c.a(2)));
    if pf == 0 {
        return 22;
    }
    let fp = fopen_mode(&path, &mode).map_or(0, new_stream);
    wp(pf, fp);
    if fp == 0 { 2 } else { 0 }
}
fn _wfopen_s(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return 22;
    }
    let path = String::from_utf16_lossy(&wstr(c.a(1)));
    let mode: Vec<u8> = wstr(c.a(2)).iter().map(|&u| u as u8).collect();
    let fp = fopen_mode(&path, &mode).map_or(0, new_stream);
    wp(c.a(0), fp);
    if fp == 0 { 2 } else { 0 }
}

// setjmp/longjmp: jmp_buf = ebp, ebx, edi, esi, esp, SEH frame, eip (as msvcrt's first fields)
// setjmp/longjmp: jmp_buf = ebp, ebx, edi, esi, esp, SEH frame, eip (as msvcrt's first fields);
// x64: seh64's (_JUMP_BUFFER, unwinding)
fn _setjmp3(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return crate::seh64::setjmp(c);
    }
    let b = c.a(0);
    let r = &c.cpu.regs;
    for (i, v) in [r[koji::EBP], r[koji::EBX], r[koji::EDI], r[koji::ESI], c.esp + 4, r32(crate::cur().teb), r32(c.esp)].into_iter().enumerate() {
        w32(b + 4 * i as u32, v);
    }
    0
}
fn longjmp(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return crate::seh64::longjmp(c);
    }
    let (b, v) = (c.a(0), c.a(1));
    let g = |i: u32| r32(b + 4 * i);
    c.cpu.regs[koji::EBP] = g(0);
    c.cpu.regs[koji::EBX] = g(1);
    c.cpu.regs[koji::EDI] = g(2);
    c.cpu.regs[koji::ESI] = g(3);
    c.cpu.regs[koji::ESP] = g(4);
    w32(crate::cur().teb, g(5));
    c.cpu.regs[koji::EAX] = if v == 0 { 1 } else { v };
    c.cpu.eip = g(6);
    c.jumped = true;
    0
}

// ucrtbase's printf core: (options (u64), buf, len, format, locale, va_list)
// ucrtbase's stdio cores: (options (u64), buf, count, format, locale, va_list) and friends. The
// options pick the dialect (crt::fmt::Mode::ucrt) and the buffer rules, as the UCRT's
// common_vsprintf: no buffer: the length; it fits: it and a NUL; it does not: LEGACY_VSPRINTF_
// NULL_TERMINATION (1) _vsnprintf's (exactly count: no NUL, the length; more: -1), else the
// last unit a NUL and STANDARD_SNPRINTF_BEHAVIOR (2) the length, otherwise -2 (-1 for count 0).
fn ucrt_pf<T: Unit>(c: &mut Ctx, sized: bool, secure: bool) -> u32 {
    let mut a = Args(0);
    let opts = a.u64(c);
    let buf = a.p(c);
    let size = a.size(c);
    let count = if sized { a.size(c) } else { u32::MAX };
    let fmt = a.p(c);
    let _locale = a.p(c);
    let va = a.p(c);
    if fmt == 0 {
        set_errno(22);
        return u32::MAX;
    }
    let s = pf::<T>(fmt, &mut VaArgs::stack(va), crt::fmt::Mode::ucrt(mem::x64(), opts));
    if secure {
        return put_s(buf, size, sized.then_some(count), s);
    }
    let Some(s) = s else {
        if buf != 0 && size != 0 {
            put_nul::<T>(buf);
        }
        return u32::MAX;
    };
    if buf == 0 {
        if size != 0 {
            set_errno(22);
            return u32::MAX;
        }
        return s.len() as u32;
    }
    let n = s.len().min(size as usize);
    put_units(buf, &s[..n]);
    if s.len() < size as usize {
        put_nul::<T>(buf + unit::<T>() * n as u32);
        return s.len() as u32;
    }
    if opts & 1 != 0 {
        return if s.len() == size as usize { s.len() as u32 } else { u32::MAX };
    }
    if size != 0 {
        put_nul::<T>(buf + unit::<T>() * (size - 1));
    }
    if opts & 2 != 0 {
        s.len() as u32
    } else if size != 0 {
        -2i32 as u32
    } else {
        u32::MAX
    }
}

fn __stdio_common_vsprintf(c: &mut Ctx) -> u32 {
    ucrt_pf::<u8>(c, false, false)
}
fn __stdio_common_vswprintf(c: &mut Ctx) -> u32 {
    ucrt_pf::<u16>(c, false, false)
}
/// the _s forms: what does not fit leaves an empty string, ERANGE and -1
fn __stdio_common_vsprintf_s(c: &mut Ctx) -> u32 {
    ucrt_pf::<u8>(c, false, true)
}
fn __stdio_common_vswprintf_s(c: &mut Ctx) -> u32 {
    ucrt_pf::<u16>(c, false, true)
}
/// (options, buf, size, count, format, locale, va_list)
fn __stdio_common_vsnprintf_s(c: &mut Ctx) -> u32 {
    ucrt_pf::<u8>(c, true, true)
}
fn __stdio_common_vsnwprintf_s(c: &mut Ctx) -> u32 {
    ucrt_pf::<u16>(c, true, true)
}
/// __stdio_common_vfprintf(options, FILE*, format, locale, va_list)
fn ucrt_fpf<T: Unit>(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let opts = a.u64(c);
    let (fp, fmt, _locale, va) = (a.p(c), a.p(c), a.p(c), a.p(c));
    let Some(s) = pf::<T>(fmt, &mut VaArgs::stack(va), crt::fmt::Mode::ucrt(mem::x64(), opts)) else { return u32::MAX };
    let bytes: Vec<u8> = if T::WIDE { crate::text::w2a(&s.iter().map(|u| u.code() as u16).collect::<Vec<u16>>()) } else { s.iter().map(|u| u.code() as u8).collect() };
    write_stream(fp, &bytes);
    s.len() as u32
}
fn __stdio_common_vfprintf(c: &mut Ctx) -> u32 {
    ucrt_fpf::<u8>(c)
}
fn __stdio_common_vfwprintf(c: &mut Ctx) -> u32 {
    ucrt_fpf::<u16>(c)
}
/// __stdio_common_vsscanf(options, input, input count (-1: to its NUL), format, locale, va_list):
/// options 1 _CRT_INTERNAL_SCANF_SECURECRT, 2 LEGACY_WIDE_SPECIFIERS, 4 LEGACY_MSVCRT_COMPATIBILITY
fn ucrt_scan<T: Unit>(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let opts = a.u64(c);
    let (input, n, fmt, _locale, va) = (a.p(c), a.size(c), a.p(c), a.p(c), a.p(c));
    if input == 0 || fmt == 0 {
        set_errno(22);
        return u32::MAX;
    }
    let text = if n == u32::MAX { gstr::<T>(input) } else { gstrn::<T>(input, n) };
    let m = crt::scan::Mode { ptr64: mem::x64(), secure: opts & 1 != 0, legacy: opts & 4 != 0, legacy_wide: opts & 2 != 0 };
    crt::scan::scan(&text, &gstr::<T>(fmt), m, &mut GuestSink(VaArgs::stack(va))) as u32
}
fn __stdio_common_vsscanf(c: &mut Ctx) -> u32 {
    ucrt_scan::<u8>(c)
}
fn __stdio_common_vswscanf(c: &mut Ctx) -> u32 {
    ucrt_scan::<u16>(c)
}
// The secure string copies: crt::sec (proved against its model with Kani) over guest memory.
// The destination is viewed as `size` units; the source is read as a string.

/// A view of the destination: narrow ones in place, wide ones as a copy written back after.
fn sec_call<T: Unit>(d: u32, size: u32, need: u32, f: impl FnOnce(Option<&mut [T]>) -> i32) -> u32 {
    let e = if d == 0 {
        f(None)
    } else {
        // (beyond what the call can reach (the string there, the source and a NUL) nothing
        // changes, so a view that long decides it alike)
        let n = size.min(need).min(0u32.wrapping_sub(d) / unit::<T>());
        let mut v = gstrn_raw::<T>(d, n);
        let e = f(Some(&mut v));
        put_units(d, &v);
        e
    };
    errno_of(e);
    e as u32
}
/// How much of a destination of `size` units a copy of `src_len` units can touch.
fn reach<T: Unit>(d: u32, size: u32, src_len: usize, append: bool) -> u32 {
    let have = if append && d != 0 { gstrn::<T>(d, size).len() as u32 } else { 0 };
    have.saturating_add(src_len as u32).saturating_add(2)
}

fn copy_s<T: Unit>(c: &mut Ctx, append: bool) -> u32 {
    let (d, sz, s) = (c.a(0), size(c, 1), c.a(2));
    let src = (s != 0).then(|| gstr::<T>(s));
    let need = reach::<T>(d, sz, src.as_ref().map_or(0, |v| v.len()), append);
    sec_call::<T>(d, sz, need, |dst| if append { crt::sec::strcat_s(dst, src.as_deref()) } else { crt::sec::strcpy_s(dst, src.as_deref()) })
}
fn ncopy_s<T: Unit>(c: &mut Ctx, append: bool) -> u32 {
    let (d, sz, s, n) = (c.a(0), size(c, 1), c.a(2), size(c, 3));
    let count = if n == u32::MAX { crt::sec::TRUNCATE } else { n as usize };
    let src = (s != 0).then(|| gstrn::<T>(s, n.min(sz.saturating_add(1))));
    if d == 0 && sz != 0 {
        set_errno(22);
        return 22;
    }
    let need = reach::<T>(d, sz, src.as_ref().map_or(0, |v| v.len()), append);
    sec_call::<T>(d, sz, need, |dst| if append { crt::sec::strncat_s(dst, src.as_deref(), count) } else { crt::sec::strncpy_s(dst, src.as_deref(), count) })
}
/// strncpy_s(dst, size, src, count): count _TRUNCATE (-1) copies what fits
fn strncpy_s(c: &mut Ctx) -> u32 {
    ncopy_s::<u8>(c, false)
}
fn wcsncpy_s(c: &mut Ctx) -> u32 {
    ncopy_s::<u16>(c, false)
}
fn strncat_s(c: &mut Ctx) -> u32 {
    ncopy_s::<u8>(c, true)
}
fn wcsncat_s(c: &mut Ctx) -> u32 {
    ncopy_s::<u16>(c, true)
}
fn strcpy_s(c: &mut Ctx) -> u32 {
    copy_s::<u8>(c, false)
}
fn wcscpy_s(c: &mut Ctx) -> u32 {
    copy_s::<u16>(c, false)
}
fn strcat_s(c: &mut Ctx) -> u32 {
    copy_s::<u8>(c, true)
}
fn wcscat_s(c: &mut Ctx) -> u32 {
    copy_s::<u16>(c, true)
}
fn ___lc_codepage_func(_: &mut Ctx) -> u32 {
    932
}
/// the locale names by category (LC_ALL..LC_TIME): all "ja-JP"
fn ___lc_locale_name_func(_: &mut Ctx) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| {
        let p = mem::heap_alloc(6 * 8 + 16, true);
        mem::put_wstr(p + 48, 8, &"ja-JP".encode_utf16().collect::<Vec<u16>>());
        for i in 0..6 {
            wp(p + ps() * i, p + 48);
        }
        p
    })
}
/// the locale handles (LCIDs) by category (LC_ALL..LC_TIME): all 0x411 (ja-JP), as
/// ___lc_locale_name_func's names (msvcr80/90's isctype and toupper read LC_CTYPE's)
fn ___lc_handle_func(_: &mut Ctx) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| {
        let p = mem::heap_alloc(6 * 4, true);
        for i in 0..6 {
            w32(p + 4 * i, 0x411);
        }
        p
    })
}
/// _seh_filter_exe(code, pointers): no signal handlers here: search on
fn _seh_filter_exe(_: &mut Ctx) -> u32 {
    0
}

/// _setmaxstdio(n): the new limit (msvcrt allows 20..=2048), else -1
fn _setmaxstdio(c: &mut Ctx) -> u32 {
    let n = c.a(0);
    if (20..=2048).contains(&n) { n } else { u32::MAX }
}

/// the "C" locale's struct lconv: 10 char* fields, 8 chars (CHAR_MAX), then (msvcr100+ / the
/// UCRT) 8 wchar_t* fields, _W_decimal_point first (read by std::numpunct<wchar_t>)
fn localeconv(_: &mut Ctx) -> u32 {
    static L: OnceLock<u32> = OnceLock::new();
    *L.get_or_init(|| {
        let s = ps();
        let l = mem::heap_alloc(0x90, true);
        let (dot, empty) = (mem::heap_alloc(2, true), mem::heap_alloc(2, true));
        w8(dot, b'.');
        wp(l, dot);
        for i in 1..10 {
            wp(l + s * i, empty);
        }
        // int_frac_digits.. n_sign_posn: CHAR_MAX
        for i in 0..8 {
            w8(l + 10 * s + i, 127);
        }
        let w = 10 * s + 8;
        let (wdot, wempty) = (mem::heap_alloc(4, true), mem::heap_alloc(4, true));
        w16(wdot, b'.' as u16);
        wp(l + w, wdot);
        for i in 1..8 {
            wp(l + w + s * i, wempty);
        }
        l
    })
}
fn fputc(c: &mut Ctx) -> u32 {
    let ch = c.a(0) & 0xff;
    let tmp = mem::heap_alloc(1, false);
    mem::write(tmp, &[ch as u8]);
    with_file(c.a(1), |st| match &st.f {
        Some(f) => {
            f.write_from(tmp, 1);
        }
        None => eprint!("{}", ch as u8 as char),
    });
    mem::heap_free(tmp);
    ch
}
fn __acrt_iob_func(c: &mut Ctx) -> u32 {
    data("_iob").unwrap_or(0) + file_size() * c.a(0)
}
/// _locale_t: always the process's (ja-JP, code page 932) as { threadlocinfo*, threadmbcinfo* }
/// with the fields the CRT's inline code reads (msvcr80/90 layout): locinfo's code page,
/// handles, mb_cur_max and ctype tables; mbcinfo's code page and lead-byte table (AdvHD's
/// statically linked _Mbrtowc reads it).
fn _create_locale(_: &mut Ctx) -> u32 {
    static LOC: OnceLock<u32> = OnceLock::new();
    *LOC.get_or_init(|| {
        let cp = crate::text::acp();
        let loc = mem::heap_alloc(8, true);
        let li = mem::heap_alloc(0x100, true);
        w32(li, 1);
        w32(li + 4, cp);
        w32(li + 8, cp);
        for i in 0..6 {
            w32(li + 0xc + 4 * i, 0x411);
        }
        w32(li + 0xac, if crate::text::is_dbcs() { 2 } else { 1 });
        w32(li + 0xc8, r32(data("_pctype").unwrap()));
        // pclmap / pcumap: ASCII case tables
        let (lo, up) = (mem::heap_alloc(256, true), mem::heap_alloc(256, true));
        for b in 0..=255u8 {
            w8(lo + b as u32, b.to_ascii_lowercase());
            w8(up + b as u32, b.to_ascii_uppercase());
        }
        w32(li + 0xcc, lo);
        w32(li + 0xd0, up);
        let mb = mem::heap_alloc(0x220, true);
        w32(mb, 1);
        w32(mb + 4, cp);
        w32(mb + 8, crate::text::is_dbcs() as u32);
        w32(mb + 12, 0x411);
        // mbctype[257] at 0x1c (index 0 is EOF), then mbcasemap[256]
        let t = data("_mbctype").unwrap();
        mem::copy(mb + 0x1c, t, 257);
        for b in 0..=255u8 {
            w8(mb + 0x11d + b as u32, b);
        }
        w32(loc, li);
        w32(loc + 4, mb);
        loc
    })
}
fn _free_locale(_: &mut Ctx) -> u32 {
    0
}
fn fp_class(bits: u64, exp_bits: u32, man_bits: u32) -> u32 {
    let exp = (bits >> man_bits) & ((1 << exp_bits) - 1);
    let man = bits & ((1u64 << man_bits) - 1);
    // FP_INFINITE 1, FP_NAN 2, FP_NORMAL -1, FP_SUBNORMAL -2, FP_ZERO 0
    match (exp, man) {
        (0, 0) => 0,
        (0, _) => -2i32 as u32,
        (e, 0) if e == (1 << exp_bits) - 1 => 1,
        (e, _) if e == (1 << exp_bits) - 1 => 2,
        _ => -1i32 as u32,
    }
}
fn _fdclass(c: &mut Ctx) -> u32 {
    fp_class(Args(0).f32(c).to_bits() as u64, 8, 23)
}
fn _dclass(c: &mut Ctx) -> u32 {
    fp_class(Args(0).f64(c).to_bits(), 11, 52)
}
fn _fdsign(c: &mut Ctx) -> u32 {
    (Args(0).f32(c).to_bits() >> 31) << 15
}
fn _dsign(c: &mut Ctx) -> u32 {
    ((Args(0).f64(c).to_bits() >> 63) as u32) << 15
}
fn strnlen(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    (0..n).find(|&i| mem::r8(p + i) == 0).unwrap_or(n)
}

// ---------------------------------------------------------------------------------------------
// ucrtbase / vcruntime140 (VC++ 2015+): startup, onexit tables, std::exception, time

fn _set_fmode(c: &mut Ctx) -> u32 {
    w32(data("_fmode").unwrap(), c.a(0));
    0
}
fn _get_initial_narrow_environment(_: &mut Ctx) -> u32 {
    r32(data("_environ").unwrap())
}
fn _get_initial_wide_environment(_: &mut Ctx) -> u32 {
    rp(data("_wenviron").unwrap())
}
fn ucrt_zero(_: &mut Ctx) -> u32 {
    0
}
/// _onexit_table_t: { first, last, end } (pointers to plain function pointers)
fn _initialize_onexit_table(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    if t == 0 {
        return u32::MAX;
    }
    if rp(t) == 0 {
        mem::fill(t, 3 * ps(), 0);
    }
    0
}
fn _register_onexit_function(c: &mut Ctx) -> u32 {
    let (t, f) = (c.a(0), c.a(1));
    if t == 0 {
        return u32::MAX;
    }
    let s = ps();
    let (first, last, end) = (rp(t), rp(t + s), rp(t + 2 * s));
    if first == 0 || last >= end {
        let n = if first == 0 { 32 } else { (end - first) / s * 2 };
        let p = mem::heap_alloc(n * s, true);
        if first != 0 {
            mem::copy(p, first, last - first);
            mem::heap_free(first);
        }
        wp(t, p);
        wp(t + s, p + (last - first));
        wp(t + 2 * s, p + n * s);
    }
    let last = rp(t + s);
    wp(last, f);
    wp(t + s, last + s);
    0
}
/// runs the table's functions, last registered first, and empties it
fn _execute_onexit_table(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    if t == 0 {
        return u32::MAX;
    }
    let s = ps();
    let (first, last) = (rp(t), rp(t + s));
    mem::fill(t, 3 * s, 0);
    let mut p = last;
    while p > first {
        p -= s;
        let f = rp(p);
        if f != 0 {
            c.call(f, &[]);
        }
    }
    if first != 0 {
        mem::heap_free(first);
    }
    0
}
fn _cexit(c: &mut Ctx) -> u32 {
    loop {
        let Some(f) = ATEXIT.lock().unwrap().pop() else { break };
        c.call(f, &[]);
    }
    0
}
fn _invalid_parameter_noinfo_noreturn(_: &mut Ctx) -> u32 {
    eprintln!("sake: invalid parameter (noreturn)");
    unsafe { libc::exit(0xc0000417u32 as i32) }
}
fn terminate(c: &mut Ctx) -> u32 {
    let from = if mem::x64() { mem::r64(c.esp) } else { r32(c.esp) as u64 };
    eprintln!("sake: terminate() from {from:#x}");
    unsafe { libc::exit(3) }
}
fn wcsnlen(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    (0..n).find(|&i| mem::r16(p + 2 * i) == 0).unwrap_or(n)
}
/// __std_exception_data: { what, do_free } (pointer-sized)
fn __std_exception_copy(c: &mut Ctx) -> u32 {
    let (src, dst) = (c.a(0), c.a(1));
    let s = ps();
    let what = rp(src);
    if mem::r8(src + s) != 0 && what != 0 {
        let t = cstr(what);
        let p = mem::heap_alloc(t.len() as u32 + 1, true);
        mem::write(p, &t);
        wp(dst, p);
        wp(dst + s, 1);
    } else {
        wp(dst, what);
        wp(dst + s, 0);
    }
    0
}
fn __std_exception_destroy(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    let s = ps();
    if mem::r8(d + s) != 0 && rp(d) != 0 {
        mem::heap_free(rp(d));
    }
    wp(d, 0);
    wp(d + s, 0);
    0
}
/// __std_type_info_data: { undecorated name, decorated name[] } (the name after a pointer, past
/// its leading '.')
fn __std_type_info_compare(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    if a == b {
        return 0;
    }
    cstr(a + ps() + 1).cmp(&cstr(b + ps() + 1)) as i32 as u32
}
fn __std_type_info_hash(c: &mut Ctx) -> u32 {
    // FNV-1a, as the UCRT (x64: the 64-bit one)
    let name = cstr(c.a(0) + ps() + 1);
    if mem::x64() {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in name {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        return c.ret64(h);
    }
    let mut h: u32 = 0x811c9dc5;
    for b in name {
        h = (h ^ b as u32).wrapping_mul(0x01000193);
    }
    h
}

use crt::time::{DAYS, MONTHS};

fn read_tm(p: u32) -> [i32; 9] {
    std::array::from_fn(|i| r32(p + 4 * i as u32) as i32)
}
fn write_tm(p: u32, tm: &[i32; 9]) {
    for (i, v) in tm.iter().enumerate() {
        w32(p + 4 * i as u32, *v as u32);
    }
}
/// localtime/gmtime's buffer (one, as msvcrt's per-thread one)
fn tm_buf() -> u32 {
    static B: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let b = B.load(std::sync::atomic::Ordering::Relaxed);
    if b != 0 {
        return b;
    }
    let p = mem::heap_alloc(36, true);
    B.store(p, std::sync::atomic::Ordering::Relaxed);
    p
}
/// the time_t at `p`: 64-bit or 32-bit
fn read_time(p: u32, t64: bool) -> i64 {
    if t64 { mem::r64(p) as i64 } else { r32(p) as i32 as i64 }
}
/// (localtime | gmtime)(&t): the shared buffer, NULL (EINVAL) outside the range
fn time_tm(c: &mut Ctx, t64: bool, local: bool) -> u32 {
    let p = c.a(0);
    if p == 0 {
        set_errno(22);
        return 0;
    }
    let (t, bits) = (read_time(p, t64), if t64 { 64 } else { 32 });
    let tm = if local { crt::time::localtime(t, bits, crate::k32::tz_bias()) } else { crt::time::gmtime(t, bits) };
    let Some(tm) = tm else {
        set_errno(22);
        return 0;
    };
    let b = tm_buf();
    write_tm(b, &tm);
    b
}
fn localtime32(c: &mut Ctx) -> u32 {
    time_tm(c, mem::x64(), true)
}
fn _localtime64(c: &mut Ctx) -> u32 {
    time_tm(c, true, true)
}
fn gmtime32(c: &mut Ctx) -> u32 {
    time_tm(c, mem::x64(), false)
}
fn _localtime32(c: &mut Ctx) -> u32 {
    time_tm(c, false, true)
}
fn _gmtime32(c: &mut Ctx) -> u32 {
    time_tm(c, false, false)
}
fn _gmtime64(c: &mut Ctx) -> u32 {
    time_tm(c, true, false)
}
/// the _s forms (tm*, time_t*): 0, or EINVAL with every field -1
fn time_tm_s(c: &mut Ctx, t64: bool, local: bool) -> u32 {
    let (out, p) = (c.a(0), c.a(1));
    if out == 0 {
        set_errno(22);
        return 22;
    }
    let bits = if t64 { 64 } else { 32 };
    let tm = if p == 0 {
        None
    } else {
        let t = read_time(p, t64);
        if local { crt::time::localtime(t, bits, crate::k32::tz_bias()) } else { crt::time::gmtime(t, bits) }
    };
    match tm {
        Some(tm) => {
            write_tm(out, &tm);
            0
        }
        None => {
            write_tm(out, &[-1; 9]);
            set_errno(22);
            22
        }
    }
}
fn _localtime32_s(c: &mut Ctx) -> u32 {
    time_tm_s(c, false, true)
}
fn _localtime64_s(c: &mut Ctx) -> u32 {
    time_tm_s(c, true, true)
}
fn _gmtime32_s(c: &mut Ctx) -> u32 {
    time_tm_s(c, false, false)
}
fn _gmtime64_s(c: &mut Ctx) -> u32 {
    time_tm_s(c, true, false)
}
/// mktime: normalizes the tm (local time) and gives its time (-1 out of range)
fn mktime_of(p: u32, bits: u32) -> i64 {
    if p == 0 {
        set_errno(22);
        return -1;
    }
    let mut tm = read_tm(p);
    match crt::time::mktime(&mut tm, bits, crate::k32::tz_bias()) {
        Some(t) => {
            write_tm(p, &tm);
            t
        }
        None => {
            set_errno(22);
            -1
        }
    }
}
fn mktime32(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return _mktime64(c);
    }
    _mktime32(c)
}
fn _mktime32(c: &mut Ctx) -> u32 {
    mktime_of(c.a(0), 32) as u32
}
fn _mktime64(c: &mut Ctx) -> u32 {
    let t = mktime_of(c.a(0), 64);
    c.ret64(t as u64)
}
fn difftime32(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return _difftime64(c);
    }
    _difftime32(c)
}
fn _difftime32(c: &mut Ctx) -> u32 {
    c.st0 = Some(c.a(0) as i32 as f64 - c.a(1) as i32 as f64);
    0
}
fn _difftime64(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, y) = (a.u64(c) as i64, a.u64(c) as i64);
    c.st0 = Some(x as f64 - y as f64);
    0
}


/// wcsftime's C locale text of `fmt` for the tm at `tm` (None: an invalid field)
pub(crate) fn strftime_w(fmt: &[u16], tm: u32) -> Option<Vec<u16>> {
    let bias = crate::k32::tz_bias();
    let name = if bias == 9 * 3600 { "Tokyo Standard Time" } else { "Coordinated Universal Time" };
    crt::time::strftime(fmt, &read_tm(tm), (bias, name))
}
/// strftime / wcsftime (buf, max, fmt, tm): crt::time's C locale output and its length, or 0
/// with an empty buffer when it does not fit (an invalid field or conversion: EINVAL too)
fn strftime_into(c: &mut Ctx, wide: bool) -> u32 {
    let (buf, max, fmt, tm) = (c.a(0), size(c, 1), c.a(2), c.a(3));
    if buf == 0 || max == 0 || fmt == 0 || tm == 0 {
        if buf != 0 && max != 0 {
            if wide { w16(buf, 0) } else { w8(buf, 0) }
        }
        set_errno(22);
        return 0;
    }
    let f = if wide { wstr(fmt) } else { crate::text::a2w(&cstr(fmt)) };
    let bias = crate::k32::tz_bias();
    let name = if bias == 9 * 3600 { "Tokyo Standard Time" } else { "Coordinated Universal Time" };
    let Some(out) = crt::time::strftime(&f, &read_tm(tm), (bias, name)) else {
        if wide { w16(buf, 0) } else { w8(buf, 0) }
        set_errno(22);
        return 0;
    };
    let fits = |n: usize| n < max as usize;
    if wide {
        if !fits(out.len()) {
            w16(buf, 0);
            set_errno(34);
            return 0;
        }
        mem::put_wstr(buf, max, &out);
        out.len() as u32
    } else {
        let a = crate::text::w2a(&out);
        if !fits(a.len()) {
            w8(buf, 0);
            set_errno(34);
            return 0;
        }
        mem::put_cstr(buf, max, &a);
        a.len() as u32
    }
}
fn strftime(c: &mut Ctx) -> u32 {
    strftime_into(c, false)
}
fn wcsftime(c: &mut Ctx) -> u32 {
    strftime_into(c, true)
}
/// _Getdays / _Getmonths: ":Sun:Sunday:Mon:Monday..." in a malloc'd string
fn names(list: &[&str]) -> String {
    list.iter().map(|n| format!(":{}:{n}", &n[..3])).collect()
}
fn _Getdays(_: &mut Ctx) -> u32 {
    let s = names(&DAYS);
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, s.as_bytes());
    p
}
fn _Getmonths(_: &mut Ctx) -> u32 {
    let s = names(&MONTHS);
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, s.as_bytes());
    p
}
fn _W_Getdays(_: &mut Ctx) -> u32 {
    let s: Vec<u16> = names(&DAYS).encode_utf16().collect();
    let p = mem::heap_alloc(2 * s.len() as u32 + 2, true);
    mem::put_wstr(p, s.len() as u32 + 1, &s);
    p
}
fn _W_Getmonths(_: &mut Ctx) -> u32 {
    let s: Vec<u16> = names(&MONTHS).encode_utf16().collect();
    let p = mem::heap_alloc(2 * s.len() as u32 + 2, true);
    mem::put_wstr(p, s.len() as u32 + 1, &s);
    p
}
/// _Gettnames: a malloc'd __lc_time_data (the UCRT's: 43 names, unk, refcount, the same 43 in
/// UTF-16, locale name), C locale; pointer-sized slots
fn _Gettnames(_: &mut Ctx) -> u32 {
    let mut strs: Vec<String> = Vec::new();
    strs.extend(DAYS.iter().map(|d| d[..3].to_string()));
    strs.extend(DAYS.iter().map(|d| d.to_string()));
    strs.extend(MONTHS.iter().map(|m| m[..3].to_string()));
    strs.extend(MONTHS.iter().map(|m| m.to_string()));
    strs.extend(["AM", "PM", "MM/dd/yy", "dddd, MMMM dd, yyyy", "HH:mm:ss"].map(String::from));
    let s = ps();
    // (names, ww_sdatefmt.. lcid etc. as one int pair, refcount, wide names, locale name)
    let (unk, refc, wnames) = (43 * s, 43 * s + 4, 43 * s + 8);
    let wnames = (wnames + s - 1) & !(s - 1);
    let lname = wnames + 43 * s;
    let head = lname + s;
    let size = head + strs.iter().map(|x| 3 * (x.len() as u32 + 1)).sum::<u32>() + 8;
    let p = mem::heap_alloc(size, true);
    let mut at = p + head;
    for (i, x) in strs.iter().enumerate() {
        wp(p + s * i as u32, at);
        mem::write(at, x.as_bytes());
        at += x.len() as u32 + 1;
    }
    at = (at + 1) & !1;
    for (i, x) in strs.iter().enumerate() {
        wp(p + wnames + s * i as u32, at);
        let u: Vec<u16> = x.encode_utf16().collect();
        mem::put_wstr(at, u.len() as u32 + 1, &u);
        at += 2 * (u.len() as u32 + 1);
    }
    let _ = unk;
    w32(p + refc, 1);
    // (locale name "C")
    wp(p + lname, at);
    w16(at, b'C' as u16);
    p
}

// ---------------------------------------------------------------------------------------------
// found missing by tests/winref (behaviour as Windows' msvcrt prints it there)

fn puts(c: &mut Ctx) -> u32 {
    let mut s = cstr(c.a(0));
    s.push(b'\n');
    eprint!("{}", a2s(&s));
    0
}
fn putchar(c: &mut Ctx) -> u32 {
    let ch = c.a(0) & 0xff;
    eprint!("{}", a2s(&[ch as u8]));
    ch
}
fn setvbuf(_: &mut Ctx) -> u32 {
    0
}
fn _strrev(c: &mut Ctx) -> u32 {
    let mut s = cstr(c.a(0));
    s.reverse();
    mem::write(c.a(0), &s);
    c.a(0)
}
fn div(c: &mut Ctx) -> u32 {
    let (a, b) = (c.i(0), c.i(1));
    if b == 0 {
        return 0;
    }
    c.ret64((a.wrapping_div(b) as u32 as u64) | (a.wrapping_rem(b) as u32 as u64) << 32)
}
fn _isnan(c: &mut Ctx) -> u32 {
    c.f64(0).is_nan() as u32
}
fn _finite(c: &mut Ctx) -> u32 {
    c.f64(0).is_finite() as u32
}
fn _fpclass(c: &mut Ctx) -> u32 {
    let x = c.f64(0);
    let neg = x.is_sign_negative();
    use std::num::FpCategory::*;
    match x.classify() {
        Nan => if x.to_bits() & (1 << 51) != 0 { 2 } else { 1 },
        Infinite => if neg { 4 } else { 0x200 },
        Normal => if neg { 8 } else { 0x100 },
        Subnormal => if neg { 0x10 } else { 0x80 },
        Zero => if neg { 0x20 } else { 0x40 },
    }
}
fn _hypot(c: &mut Ctx) -> u32 {
    let (x, y) = (c.f64(0), c.f64(2));
    ret(c, x.hypot(y))
}
/// _ecvt/_fcvt's buffer (one, as msvcrt's per-thread one)
fn cvt_buf() -> u32 {
    static B: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let b = B.load(std::sync::atomic::Ordering::Relaxed);
    if b != 0 {
        return b;
    }
    let p = mem::heap_alloc(352, true);
    B.store(p, std::sync::atomic::Ordering::Relaxed);
    p
}
/// (digits, decimal point position) of |v| with `n` significant digits
fn ecvt_digits(v: f64, n: usize) -> (Vec<u8>, i32) {
    let n = n.clamp(1, 340);
    if v == 0.0 {
        return (vec![b'0'; n], 0);
    }
    let s = format!("{:.*e}", n - 1, v.abs());
    let (m, e) = s.split_once('e').unwrap();
    (m.bytes().filter(|b| b.is_ascii_digit()).collect(), e.parse::<i32>().unwrap() + 1)
}
fn cvt_out(c: &mut Ctx, d: &[u8], dec: i32, neg: bool, pdec: u32, psign: u32) -> u32 {
    let b = cvt_buf();
    mem::write(b, &d[..d.len().min(350)]);
    w8(b + d.len().min(350) as u32, 0);
    w32(pdec, dec as u32);
    w32(psign, neg as u32);
    let _ = c;
    b
}
/// (value, digits, &dec, &sign) of _ecvt/_fcvt, x86 or x64
fn cvt_args(c: &Ctx) -> (f64, i32, u32, u32) {
    let mut a = Args(0);
    (a.f64(c), a.p(c) as i32, a.p(c), a.p(c))
}
fn _ecvt(c: &mut Ctx) -> u32 {
    let (v, n, pdec, psign) = cvt_args(c);
    let (d, dec) = ecvt_digits(v, n.max(0) as usize);
    cvt_out(c, &d, dec, v.is_sign_negative() && v != 0.0, pdec, psign)
}
fn _fcvt(c: &mut Ctx) -> u32 {
    let (v, n, pdec, psign) = cvt_args(c);
    let n = n.clamp(0, 300) as usize;
    let s = format!("{:.*}", n, v.abs());
    let (ip, fp) = s.split_once('.').unwrap_or((&s, ""));
    let mut d: Vec<u8> = ip.bytes().chain(fp.bytes()).collect();
    let mut dec = ip.len() as i32;
    while d.len() > 1 && d[0] == b'0' && dec > 0 {
        d.remove(0);
        dec -= 1;
    }
    if ip == "0" && v != 0.0 {
        // 0.00123: "123" with the point 2 digits to its left
        let z = d.iter().take_while(|&&b| b == b'0').count();
        if z == d.len() {
            d.clear();
            dec = -(n as i32);
        } else {
            d.drain(..z);
            dec = -(z as i32);
        }
    }
    if v == 0.0 {
        // (0: n zeros, the point before them)
        d = vec![b'0'; n];
        dec = 0;
    }
    cvt_out(c, &d, dec, v.is_sign_negative() && v != 0.0, pdec, psign)
}
/// _gcvt(value, digits, buffer): msvcrt's own %g ("1.", "1.23e-004", "1.e+020")
fn _gcvt(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (v, n, out) = (a.f64(c), (a.p(c) as i32).clamp(1, 340) as usize, a.p(c));
    let mut s = String::new();
    if v.is_sign_negative() && v != 0.0 {
        s.push('-');
    }
    let (mut d, dec) = ecvt_digits(v, n);
    while d.len() > 1 && d.last() == Some(&b'0') {
        d.pop();
    }
    let d = String::from_utf8(d).unwrap();
    if v == 0.0 {
        s.push_str("0.");
    } else if dec < -1 || dec > n as i32 {
        let e = dec - 1;
        s.push_str(&format!("{}.{}e{}{:03}", &d[..1], &d[1..], if e < 0 { '-' } else { '+' }, e.abs()));
    } else if dec <= 0 {
        s.push_str(&format!("0.{}{}", "0".repeat((-dec) as usize), d));
    } else {
        let k = dec as usize;
        let ip = if d.len() >= k { d[..k].to_string() } else { format!("{d}{}", "0".repeat(k - d.len())) };
        s.push_str(&format!("{ip}.{}", if d.len() > k { &d[k..] } else { "" }));
    }
    s.push('\0');
    mem::write(out, s.as_bytes());
    out
}
fn _mbslen(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0));
    let (mut i, mut n) = (0, 0);
    while i < s.len() {
        i += if crate::text::is_lead(s[i]) && i + 1 < s.len() { 2 } else { 1 };
        n += 1;
    }
    n
}
/// (offset, character) of each character of an ANSI string
fn mb_chars(s: &[u8]) -> Vec<(usize, u32)> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if crate::text::is_lead(s[i]) && i + 1 < s.len() {
            v.push((i, (s[i] as u32) << 8 | s[i + 1] as u32));
            i += 2;
        } else {
            v.push((i, s[i] as u32));
            i += 1;
        }
    }
    v
}
fn _mbschr(c: &mut Ctx) -> u32 {
    let (p, ch) = (c.a(0), c.a(1) & 0xffff);
    let s = cstr(p);
    if ch == 0 {
        return p + s.len() as u32;
    }
    mb_chars(&s).iter().find(|x| x.1 == ch).map_or(0, |x| p + x.0 as u32)
}
fn _mbsrchr(c: &mut Ctx) -> u32 {
    let (p, ch) = (c.a(0), c.a(1) & 0xffff);
    let s = cstr(p);
    if ch == 0 {
        return p + s.len() as u32;
    }
    mb_chars(&s).iter().rev().find(|x| x.1 == ch).map_or(0, |x| p + x.0 as u32)
}
fn _mbsinc(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    p + if crate::text::is_lead(r8(p)) && r8(p + 1) != 0 { 2 } else { 1 }
}
fn _mbsnextc(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let b = r8(p);
    if crate::text::is_lead(b) && r8(p + 1) != 0 { (b as u32) << 8 | r8(p + 1) as u32 } else { b as u32 }
}
fn _ismbcspace(c: &mut Ctx) -> u32 {
    let ch = c.a(0);
    (matches!(ch, 0x09..=0x0d | 0x20) || ch == 0x8140 && crate::text::acp() == 932) as u32
}
fn _mbctohira(c: &mut Ctx) -> u32 {
    match c.a(0) {
        k @ 0x8340..=0x837e => k - 0x8340 + 0x829f,
        k @ 0x8380..=0x8393 => k - 0x8380 + 0x82de,
        k => k,
    }
}
fn _mbctokata(c: &mut Ctx) -> u32 {
    match c.a(0) {
        h @ 0x829f..=0x82dd => h - 0x829f + 0x8340,
        h @ 0x82de..=0x82f1 => h - 0x82de + 0x8380,
        h => h,
    }
}
fn _mbcjistojms(c: &mut Ctx) -> u32 {
    let ch = c.a(0);
    let (hi, lo) = (ch >> 8, ch & 0xff);
    if !(0x21..=0x7e).contains(&hi) || !(0x21..=0x7e).contains(&lo) {
        return 0;
    }
    let s1 = (hi + 1) / 2 + if hi <= 0x5e { 0x70 } else { 0xb0 };
    let s2 = if hi & 1 != 0 { lo + if lo <= 0x5f { 0x1f } else { 0x20 } } else { lo + 0x7e };
    s1 << 8 | s2
}
fn _mbcjmstojis(c: &mut Ctx) -> u32 {
    let ch = c.a(0);
    let (s1, s2) = (ch >> 8, ch & 0xff);
    if !matches!(s1, 0x81..=0x9f | 0xe0..=0xef) || !matches!(s2, 0x40..=0x7e | 0x80..=0xfc) {
        return 0;
    }
    let mut hi = (if s1 <= 0x9f { s1 - 0x70 } else { s1 - 0xb0 }) * 2;
    let lo = if s2 < 0x9f {
        hi -= 1;
        s2 - if s2 > 0x7f { 0x20 } else { 0x1f }
    } else {
        s2 - 0x7e
    };
    hi << 8 | lo
}
fn _mbsupr(c: &mut Ctx) -> u32 {
    let mut s = cstr(c.a(0));
    crate::text::upper_a(&mut s);
    mem::write(c.a(0), &s);
    c.a(0)
}
fn _mbslwr(c: &mut Ctx) -> u32 {
    let mut s = cstr(c.a(0));
    crate::text::lower_a(&mut s);
    mem::write(c.a(0), &s);
    c.a(0)
}
static MBCP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);
fn _setmbcp(c: &mut Ctx) -> u32 {
    let cp = match c.i(0) {
        -3 | -4 => crate::text::acp(), // _MB_CP_ANSI, _MB_CP_LOCALE
        -2 => crate::text::acp(),      // _MB_CP_OEM
        cp => cp as u32,
    };
    MBCP.store(cp, std::sync::atomic::Ordering::Relaxed);
    0
}
fn _getmbcp(_: &mut Ctx) -> u32 {
    match MBCP.load(std::sync::atomic::Ordering::Relaxed) {
        u32::MAX => crate::text::acp(),
        cp => cp,
    }
}
fn asctime(c: &mut Ctx) -> u32 {
    let tm = read_tm(c.a(0));
    const DAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MON: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let s = format!("{} {} {:02} {:02}:{:02}:{:02} {}\n\0", DAY[tm[6].rem_euclid(7) as usize],
        MON[tm[4].rem_euclid(12) as usize], tm[3], tm[2], tm[1], tm[0], tm[5] + 1900);
    let b = cvt_buf();
    mem::write(b, s.as_bytes());
    b
}

pub fn init() {
    // crtdll's __GetMainArgs(argc, argv, envp, expand): __getmainargs without the startup info
    implement("crtdll", "__GetMainArgs", __getmainargs);
    io::init();
    io::init2();
    implement("msvcrt", "_strtof_l", strtof);
    reg!(strtof, wcstof, _strtoi64, _strtoui64, _wcstoi64, _wcstoui64, _atoi64, _wtoi64, _wtof);
    for (alias, f) in [
        ("strtoll", _strtoi64 as crate::api::Handler),
        ("strtoull", _strtoui64),
        ("wcstoll", _wcstoi64),
        ("wcstoull", _wcstoui64),
        ("atoll", _atoi64),
        ("_wtoll", _wtoi64),
        ("_strtol_l", strtol),
        ("_strtoul_l", strtoul),
        ("_strtoi64_l", _strtoi64),
        ("_strtoui64_l", _strtoui64),
        ("_strtoll_l", _strtoi64),
        ("_strtoull_l", _strtoui64),
        ("_wcstol_l", wcstol),
        ("_wcstoul_l", wcstoul),
        ("_wcstoi64_l", _wcstoi64),
        ("_wcstoui64_l", _wcstoui64),
        ("_wcstod_l", wcstod),
        ("_wcstof_l", wcstof),
        ("_atoi_l", atoi),
        ("_atol_l", atoi),
        ("_atof_l", atof),
        ("_wtoi_l", _wtoi),
        ("_wtol_l", _wtoi),
        ("_wtof_l", _wtof),
        ("_atoi64_l", _atoi64),
        ("_ltow", _itow),
        ("_ltow_s", _itow_s),
        ("_ultow", _ultow),
        ("_ultow_s", _ultow_s),
        ("_scwprintf", _scwprintf),
        ("_mktime32", _mktime32),
        ("_localtime32", _localtime32),
        ("_gmtime32", _gmtime32),
        ("_difftime32", _difftime32),
    ] {
        implement("msvcrt", alias, f);
    }
    reg!(_itow, _itow_s, _ultow, _ultow_s, _i64tow, _i64tow_s, _ui64tow, _ui64tow_s, _i64toa, _ui64toa, _ultoa);
    reg!(sscanf_s, swscanf, swscanf_s, vsscanf, vswscanf, vprintf, vfprintf, strncat_s, wcsncat_s, _ftelli64, _fseeki64);
    reg!(modff, floorf, ceilf, sqrtf, sinf, cosf, tanf, asinf, acosf, atanf, expf, logf, log10f, fabsf, sinhf, coshf,
        tanhf, log2f, exp2f, roundf, truncf, cbrtf, powf, atan2f, fmodf, hypotf, fmaxf, fminf, copysignf,
        round, trunc, log2, exp2, cbrt, log1p, expm1, asinh, acosh, atanh, hypot, fmax, fmin, copysign, lround, llround);
    for (alias, f) in [("_hypot", hypot as crate::api::Handler), ("_copysign", copysign), ("_hypotf", hypotf), ("_copysignf", copysignf)] {
        implement("msvcrt", alias, f);
    }
    for (name, f) in [
        ("__stdio_common_vsnprintf_s", __stdio_common_vsnprintf_s as crate::api::Handler),
        ("__stdio_common_vsnwprintf_s", __stdio_common_vsnwprintf_s),
        ("__stdio_common_vfprintf", __stdio_common_vfprintf),
        ("__stdio_common_vfwprintf", __stdio_common_vfwprintf),
        ("__stdio_common_vsscanf", __stdio_common_vsscanf),
        ("__stdio_common_vswscanf", __stdio_common_vswscanf),
    ] {
        implement("ucrtbase", name, f);
    }
    reg!(_set_fmode, _get_initial_narrow_environment, _get_initial_wide_environment, _initialize_onexit_table,
        _register_onexit_function, _execute_onexit_table, _cexit, _invalid_parameter_noinfo_noreturn, terminate,
        wcsnlen, __std_exception_copy, __std_exception_destroy, __std_type_info_compare, __std_type_info_hash,
        _localtime64, _gmtime64, _localtime32_s, _localtime64_s, _gmtime32_s, _gmtime64_s, _mktime64, _difftime64,
        strftime, wcsftime, _Getdays, _Getmonths, _W_Getdays, _W_Getmonths, _Gettnames);
    for (alias, f) in [
        ("_set_app_type", ucrt_zero as crate::api::Handler),
        ("_set_new_mode", ucrt_zero),
        ("_configure_narrow_argv", ucrt_zero),
        ("_configure_wide_argv", ucrt_zero),
        ("_initialize_narrow_environment", ucrt_zero),
        ("_initialize_wide_environment", ucrt_zero),
        ("_register_thread_local_exe_atexit_callback", ucrt_zero),
        ("__setusermatherr", ucrt_zero),
        ("_callnewh", ucrt_zero),
        ("_lock_locales", ucrt_zero),
        ("_unlock_locales", ucrt_zero),
        ("_lock_file", ucrt_zero),
        ("_unlock_file", ucrt_zero),
        ("__uncaught_exception", ucrt_zero),
        ("__uncaught_exceptions", ucrt_zero),
        ("__std_type_info_destroy_list", ucrt_zero),
        ("__telemetry_main_invoke_trigger", ucrt_zero),
        ("__telemetry_main_return_trigger", ucrt_zero),
        ("_c_exit", ucrt_zero),
        ("_crt_atexit", atexit),
        ("_seh_filter_dll", _seh_filter_exe),
        ("__std_terminate", terminate),
        ("__strncnt", strnlen),
        ("localtime", localtime32),
        ("gmtime", gmtime32),
        ("mktime", mktime32),
        ("difftime", difftime32),
        ("_Strftime", strftime),
        ("_Wcsftime", wcsftime),
        ("_W_Gettnames", _Gettnames),
    ] {
        implement("msvcrt", alias, f);
    }
    implement("msvcrt", "_strtod_l", strtod);
    reg!(_wcsicmp, _wcsnicmp, wcschr, wcsrchr, wcscat, wcsncat, wcsncpy, wcsstr, wcsspn, wcscspn, wcspbrk);
    reg!(towlower, towupper, iswspace, iswdigit, iswalpha, wcstol, wcstoul, _wtoi, wcstod);
    implement("msvcrt", "_wtol", _wtoi);
    reg!(_wcsdup, _wcslwr, _wcsupr, wcstok_s, strtok_s, strtok, wcstok);
    // (ucrtbase's wcstok takes a context)
    implement("ucrtbase", "wcstok", wcstok_s);
    reg!(localeconv);
    reg!(strcpy_s, wcscpy_s, strcat_s, wcscat_s, wcsncpy_s);
    reg!(_itoa_s, _ultoa_s, _i64toa_s, _ui64toa_s);
    reg!(puts, putchar, setvbuf, _strrev, _ultoa, _i64toa, _ui64toa, _atoi64, div, _isnan, _finite, _fpclass, _hypot,
        _ecvt, _fcvt, _gcvt, _mbslen, _mbschr, _mbsrchr, _mbsinc, _mbsnextc, _ismbcspace, _mbctohira, _mbctokata,
        _mbcjistojms, _mbcjmstojis, _mbsupr, _mbslwr, _setmbcp, _getmbcp, asctime);
    implement("msvcrt", "ldiv", div);
    reg!(strncpy_s, ___lc_codepage_func, ___lc_locale_name_func, ___lc_handle_func, _seh_filter_exe);
    implement("ucrtbase", "__stdio_common_vsprintf_s", __stdio_common_vsprintf_s);
    implement("ucrtbase", "__stdio_common_vswprintf_s", __stdio_common_vswprintf_s);
    reg!(_libm_sse2_sin_precise, _libm_sse2_cos_precise, _libm_sse2_tan_precise, _libm_sse2_asin_precise,
        _libm_sse2_acos_precise, _libm_sse2_atan_precise, _libm_sse2_exp_precise, _libm_sse2_log_precise,
        _libm_sse2_log10_precise, _libm_sse2_sqrt_precise, _libm_sse2_pow_precise);
    reg!(_findclose, _findfirst32, _findnext32, _findfirst32i64, _findnext32i64, _findfirst64i32, _findnext64i32,
        _findfirst64, _findnext64, _wfindfirst32, _wfindnext32, _wfindfirst32i64, _wfindnext32i64,
        _wfindfirst64i32, _wfindnext64i32, _wfindfirst64, _wfindnext64);
    // (msvcrt's old names: 32-bit time)
    implement("msvcrt", "_findfirst", _findfirst32);
    implement("msvcrt", "_findnext", _findnext32);
    implement("msvcrt", "_findfirsti64", _findfirst32i64);
    implement("msvcrt", "_findnexti64", _findnext32i64);
    implement("msvcrt", "_wfindfirst", _wfindfirst32);
    implement("msvcrt", "_wfindnext", _wfindnext32);
    implement("msvcrt", "_wfindfirsti64", _wfindfirst32i64);
    implement("msvcrt", "_wfindnexti64", _wfindnext32i64);
    reg!(_get_stream_buffer_pointers);
    reg!(fgetpos, fsetpos, ___lc_collate_cp_func);
    implement("msvcrt", "_fsopen", fopen);
    implement("msvcrt", "_wfsopen", _wfopen);
    reg!(_aligned_malloc, _aligned_free, _aligned_realloc, _aligned_msize, _mkdir, _wmkdir);
    reg!(fread_s, _time64, _splitpath, _wsplitpath, _splitpath_s, _wsplitpath_s);
    reg!(_get_narrow_winmain_command_line, _get_wide_winmain_command_line);
    implement("msvcrt", "_time32", _time32);
    reg!(fputc, __acrt_iob_func, _create_locale, _free_locale, _fdclass, _dclass, _fdsign, _dsign, strnlen);
    implement("ucrtbase", "__stdio_common_vsprintf", __stdio_common_vsprintf);
    implement("ucrtbase", "__stdio_common_vswprintf", __stdio_common_vswprintf);
    reg!(_setmaxstdio);
    reg!(wcsncmp, getenv, _ismbblead, memcpy_s, memmove_s, _controlfp_s, __iob_func, __p__iob, fopen_s, _wfopen_s, _setjmp3, longjmp, remove, rename);
    implement("msvcrt", "_setjmp", _setjmp3);
    reg!(_encode_pointer, _encoded_null, _crt_debugger_hook, _configthreadlocale, ___mb_cur_max_l_func);
    reg!(_initterm, _initterm_e, _onexit, atexit, __dllonexit, exit, _exit, abort, __getmainargs, __set_app_type,
        __p__fmode, __p__commode, __p___argc, __p___argv, __p___wargv, __p__wcmdln, __p__wenviron, __wgetmainargs,
        __p__environ, __p___initenv, __p__acmdln,
        __p___mb_cur_max, ___mb_cur_max_func, __pctype_func, _controlfp, _control87, _clearfp, _purecall,
        _XcptFilter, signal, _lock, _unlock, _errno, __doserrno,
        malloc, calloc, realloc, free, _msize,
        memcpy, memmove, memset, memcmp, memchr, strlen, strcpy, strncpy, strcat, strncat, strcmp, strncmp,
        _stricmp, _strnicmp, strchr, strrchr, strstr, strspn, strcspn, strpbrk, _strdup, _strlwr, _strupr, wcslen, wcscpy, wcscmp,
        toupper, tolower, isalpha, isupper, islower, isdigit, isspace, ispunct, isalnum, isxdigit, isprint, iscntrl, isgraph,
        atoi, strtol, strtoul, atof, strtod, _itoa, abs, qsort, bsearch, srand, rand, time, clock,
        sprintf, vsprintf, _snprintf, _vsnprintf, swprintf, printf, vprintf, sscanf, sprintf_s, vsprintf_s, _snprintf_s,
        _vsnprintf_s, _vscprintf, _scprintf, swprintf_s, vswprintf_s, _snwprintf, _vsnwprintf, _snwprintf_s,
        _vsnwprintf_s, vswprintf, _vscwprintf,
        floor, ceil, sqrt, sin, cos, tan, asin, acos, atan, exp, log, log10, fabs, sinh, cosh, tanh, pow, atan2,
        fmod, ldexp, frexp, modf,
        _CIsqrt, _CIsin, _CIcos, _CItan, _CIasin, _CIacos, _CIatan, _CIexp, _CIlog, _CIlog10, _CIsinh, _CIcosh,
        _CItanh, _CIpow, _CIatan2, _CIfmod, _ftol,
        fopen, _wfopen, fclose, fread, fwrite, fseek, ftell, rewind, feof, ferror, fflush, fgetc, ungetc, fgets,
        fputs, fprintf);
    for (alias, f) in [
        ("_strcmpi", _stricmp as crate::api::Handler),
        ("_strdup", _strdup),
        ("atol", atoi),
        ("labs", abs),
        ("getc", fgetc),
        ("_ltoa", _itoa),
        ("_ltoa_s", _itoa_s),
        ("_ftol2", _ftol),
        ("_decode_pointer", _encode_pointer),
        ("_malloc_crt", malloc),
        ("_malloc_base", malloc),
        ("_calloc_base", calloc),
        ("_realloc_base", realloc),
        ("_free_base", free),
        ("_calloc_crt", calloc),
        ("_realloc_crt", realloc),
        ("_invalid_parameter_noinfo", _crt_debugger_hook),
        ("_invoke_watson", _crt_debugger_hook),
        ("__clean_type_info_names_internal", _crt_debugger_hook),
        ("_ftol2_sse", _ftol),
        ("_mbscmp", strcmp),
        ("_mbsicmp", _stricmp),
        ("??2@YAPAXI@Z", malloc),
        ("??_U@YAPAXI@Z", malloc),
        ("??3@YAXPAX@Z", free),
        ("??_V@YAXPAX@Z", free),
    ] {
        implement("msvcrt", alias, f);
    }
}

#[cfg(test)]
mod tests {
    use super::split_words;
    use proptest::prelude::*;

    #[test]
    fn split_like_crt() {
        let w = split_words(br#"a.exe  "b c"  d"e"f """#);
        assert_eq!(w, [b"a.exe".to_vec(), b"b c".to_vec(), b"def".to_vec(), b"".to_vec()]);
    }

    proptest! {
        // The narrow and wide splitters agree on ASCII, so __argv and __wargv hold the same words.
        #[test]
        fn wide_matches_narrow(line in "[ a-z\"\t]{0,40}") {
            let n = split_words(line.as_bytes());
            let w: Vec<u16> = line.encode_utf16().collect();
            let w = split_words(&w);
            let back: Vec<Vec<u8>> = w.iter().map(|s| s.iter().map(|&c| c as u8).collect()).collect();
            prop_assert_eq!(n, back);
        }
        // Words never hold an unquoted separator, and joining quoted words gives them back.
        #[test]
        fn quoted_round_trip(words in prop::collection::vec("[a-z ]{0,6}", 1..6)) {
            let line: String = words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" ");
            let got: Vec<String> = split_words(line.as_bytes()).into_iter().map(|w| String::from_utf8(w).unwrap()).collect();
            prop_assert_eq!(got, words);
        }
    }
}
