//! msvcrt.dll: the C runtime DLL games' own DLLs link against (ogg/vorbis, DxLib helpers...).
//! Heap calls share HeapAlloc's heap, files are k32's file objects, printf is misc's.
//! Data exports (`_adjust_fdiv`, `_iob`, ...) are guest variables made on first import.

#![allow(non_snake_case)]

use crate::HashMap;
use std::io::SeekFrom;
use std::sync::{Arc, Mutex, OnceLock};

use crate::api::{implement, Ctx};
use crate::files::File;
use crate::mem::{self, cstr, r16, r32, r8, w16, w32, w8, wstr};
use crate::misc::{format, format_w, VaArgs};
use crate::text::a2s;

macro_rules! reg {
    ($($f:ident),* $(,)?) => { $( implement("msvcrt", stringify!($f), $f); )* };
}

// ---------------------------------------------------------------------------------------------
// data exports

static DATA: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// The address of msvcrt's data export `name`, if it is one.
pub fn data(name: &str) -> Option<u32> {
    const NAMES: &[&str] = &["_adjust_fdiv", "_fmode", "_commode", "__argc", "__argv", "__wargv", "_environ", "_wenviron",
        "__initenv", "__winitenv", "_acmdln", "_wcmdln", "_osver", "_winver", "_winmajor", "_winminor", "__mb_cur_max",
        "_pctype", "_pwctype", "_timezone", "_daylight", "_mbctype", "_HUGE", "_iob"];
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
        "_acmdln" => var(crate::k32::cmdline().0),
        "_wcmdln" => var(crate::k32::cmdline().1),
        "_environ" | "__initenv" => var(env()),
        "_wenviron" | "__winitenv" => var(mem::heap_alloc(4, true)),
        "_osver" => var(2600),
        "_winver" => var(0x501),
        "_winmajor" => var(5),
        "_winminor" => var(1),
        _ => var(0),
    };
    m.insert(name.to_string(), a);
    Some(a)
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

/// (argc, argv) parsed from the command line, as the CRT does.
fn args() -> (u32, u32) {
    static A: Mutex<(u32, u32)> = Mutex::new((0, 0));
    let mut g = A.lock().unwrap();
    if g.1 == 0 {
        let line = cstr(crate::k32::cmdline().0);
        let mut words: Vec<Vec<u8>> = Vec::new();
        let mut cur: Option<Vec<u8>> = None;
        let mut quoted = false;
        for &b in &line {
            match b {
                b'"' => {
                    quoted = !quoted;
                    cur.get_or_insert_with(Vec::new);
                }
                b' ' | b'\t' if !quoted => {
                    if let Some(w) = cur.take() {
                        words.push(w);
                    }
                }
                _ => cur.get_or_insert_with(Vec::new).push(b),
            }
        }
        words.extend(cur);
        let argv = mem::heap_alloc(4 * (words.len() as u32 + 1), true);
        for (i, w) in words.iter().enumerate() {
            let s = mem::heap_alloc(w.len() as u32 + 1, true);
            mem::write(s, w);
            w32(argv + 4 * i as u32, s);
        }
        *g = (words.len() as u32, argv);
    }
    *g
}

fn env() -> u32 {
    let p = mem::heap_alloc(4, true);
    p
}

// ---------------------------------------------------------------------------------------------
// startup and exit

fn _initterm(c: &mut Ctx) -> u32 {
    let (mut p, end) = (c.a(0), c.a(1));
    while p < end {
        let f = r32(p);
        if f != 0 {
            c.call(f, &[]);
        }
        p += 4;
    }
    0
}
fn _initterm_e(c: &mut Ctx) -> u32 {
    let (mut p, end) = (c.a(0), c.a(1));
    while p < end {
        let f = r32(p);
        if f != 0 {
            let r = c.call(f, &[]);
            if r != 0 {
                return r;
            }
        }
        p += 4;
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
    w32(c.a(1), argv);
    w32(c.a(2), env());
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
fn _controlfp(_: &mut Ctx) -> u32 {
    0x9001f
}
fn _control87(_: &mut Ctx) -> u32 {
    0x9001f
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
fn set_errno(v: u32) {
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
fn _stricmp(c: &mut Ctx) -> u32 {
    cmp(&lower(cstr(c.a(0))), &lower(cstr(c.a(1))))
}
fn _strnicmp(c: &mut Ctx) -> u32 {
    cmp(&lower(mem::cstrn(c.a(0), c.a(2))), &lower(mem::cstrn(c.a(1), c.a(2))))
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
    let mut p = if s != 0 { s } else { r32(ctx) };
    if p == 0 {
        return 0;
    }
    while rd(p) != 0 && ds.contains(&rd(p)) {
        p += w;
    }
    if rd(p) == 0 {
        w32(ctx, p);
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
    w32(ctx, p);
    start
}
static TOK_CTX: OnceLock<u32> = OnceLock::new();
fn tok_ctx() -> u32 {
    *TOK_CTX.get_or_init(|| mem::heap_alloc(8, true))
}
fn strtok(c: &mut Ctx) -> u32 {
    tok(1, c.a(0), c.a(1), tok_ctx())
}
fn strtok_s(c: &mut Ctx) -> u32 {
    tok(1, c.a(0), c.a(1), c.a(2))
}
fn wcstok(c: &mut Ctx) -> u32 {
    tok(2, c.a(0), c.a(1), tok_ctx() + 4)
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
/// a wide string as bytes for parse_int (one byte per character, non-ASCII as 0xff)
fn wbytes(p: u32) -> Vec<u8> {
    wstr(p).into_iter().map(|u| if u < 0x80 { u as u8 } else { 0xff }).collect()
}
fn wcstol(c: &mut Ctx) -> u32 {
    let (v, n, _) = parse_int(&wbytes(c.a(0)), c.a(2));
    if c.a(1) != 0 {
        w32(c.a(1), c.a(0) + 2 * n as u32);
    }
    v.clamp(i32::MIN as i64, i32::MAX as i64) as i32 as u32
}
fn wcstoul(c: &mut Ctx) -> u32 {
    let (v, n, _) = parse_int(&wbytes(c.a(0)), c.a(2));
    if c.a(1) != 0 {
        w32(c.a(1), c.a(0) + 2 * n as u32);
    }
    v as u32
}
fn _wtoi(c: &mut Ctx) -> u32 {
    parse_int(&wbytes(c.a(0)), 10).0 as i32 as u32
}
fn wcstod(c: &mut Ctx) -> u32 {
    let (v, n) = parse_float(&wbytes(c.a(0)));
    if c.a(1) != 0 {
        w32(c.a(1), c.a(0) + 2 * n as u32);
    }
    c.st0 = Some(v);
    0
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
fn isprint(c: &mut Ctx) -> u32 {
    ctype(c, 0x157)
}

/// strtol-style parsing: (value, bytes used)
fn parse_int(s: &[u8], base: u32) -> (i64, usize, bool) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | 9..=13) {
        i += 1;
    }
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut base = base;
    if (base == 0 || base == 16) && s.get(i) == Some(&b'0') && matches!(s.get(i + 1), Some(b'x' | b'X')) {
        i += 2;
        base = 16;
    } else if base == 0 {
        base = if s.get(i) == Some(&b'0') { 8 } else { 10 };
    }
    let start = i;
    let mut v: i64 = 0;
    while let Some(d) = s.get(i).and_then(|&b| (b as char).to_digit(base)) {
        v = v.saturating_mul(base as i64).saturating_add(d as i64);
        i += 1;
    }
    if i == start {
        return (0, 0, false);
    }
    (if neg { -v } else { v }, i, neg)
}
fn atoi(c: &mut Ctx) -> u32 {
    parse_int(&cstr(c.a(0)), 10).0 as i32 as u32
}
fn strtol(c: &mut Ctx) -> u32 {
    let (v, n, _) = parse_int(&cstr(c.a(0)), c.a(2));
    if c.a(1) != 0 {
        w32(c.a(1), c.a(0) + n as u32);
    }
    v.clamp(i32::MIN as i64, i32::MAX as i64) as i32 as u32
}
fn strtoul(c: &mut Ctx) -> u32 {
    let (v, n, _) = parse_int(&cstr(c.a(0)), c.a(2));
    if c.a(1) != 0 {
        w32(c.a(1), c.a(0) + n as u32);
    }
    v as u32
}
/// (value, bytes used)
fn parse_float(s: &[u8]) -> (f64, usize) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | 9..=13) {
        i += 1;
    }
    let start = i;
    let mut best = (0.0, 0);
    let mut j = i;
    while j < s.len() && matches!(s[j], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
        j += 1;
        if let Ok(v) = std::str::from_utf8(&s[start..j]).unwrap_or("").parse::<f64>() {
            best = (v, j);
        }
    }
    best
}
fn atof(c: &mut Ctx) -> u32 {
    c.st0 = Some(parse_float(&cstr(c.a(0))).0);
    0
}
fn strtod(c: &mut Ctx) -> u32 {
    let (v, n) = parse_float(&cstr(c.a(0)));
    if c.a(1) != 0 {
        w32(c.a(1), c.a(0) + n as u32);
    }
    c.st0 = Some(v);
    0
}
fn itoa_common(v: i64, out: u32, base: u32) -> u32 {
    let mut s = Vec::new();
    let (neg, mut u) = if base == 10 && v < 0 { (true, (-v) as u64) } else { (false, v as u32 as u64) };
    loop {
        s.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(u % base as u64) as usize]);
        u /= base as u64;
        if u == 0 {
            break;
        }
    }
    if neg {
        s.push(b'-');
    }
    s.reverse();
    s.push(0);
    mem::write(out, &s);
    out
}
fn _itoa(c: &mut Ctx) -> u32 {
    itoa_common(c.i(0) as i64, c.a(1), c.a(2).clamp(2, 36))
}
fn digits(mut u: u64, base: u64) -> Vec<u8> {
    let mut s = Vec::new();
    loop {
        s.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(u % base) as usize]);
        u /= base;
        if u == 0 {
            break;
        }
    }
    s.reverse();
    s
}
/// _itoa_s / _ltoa_s / _ultoa_s (v, buf, size, radix) and the 64-bit _i64toa_s / _ui64toa_s
/// (v as two words): 0, or ERANGE (34) when it does not fit
fn toa_s(v: u64, neg: bool, out: u32, size: u32, base: u32) -> u32 {
    let mut s = digits(v, base.clamp(2, 36) as u64);
    if neg {
        s.insert(0, b'-');
    }
    if out == 0 || s.len() as u32 + 1 > size {
        if out != 0 && size > 0 {
            w8(out, 0);
        }
        return 34;
    }
    mem::write(out, &s);
    w8(out + s.len() as u32, 0);
    0
}
fn _itoa_s(c: &mut Ctx) -> u32 {
    let v = c.i(0) as i64;
    let neg = c.a(3) == 10 && v < 0;
    toa_s(if neg { v.unsigned_abs() } else { c.a(0) as u64 }, neg, c.a(1), c.a(2), c.a(3))
}
fn _ultoa_s(c: &mut Ctx) -> u32 {
    toa_s(c.a(0) as u64, false, c.a(1), c.a(2), c.a(3))
}
fn _i64toa_s(c: &mut Ctx) -> u32 {
    let v = (c.a(0) as u64 | (c.a(1) as u64) << 32) as i64;
    let neg = c.a(4) == 10 && v < 0;
    toa_s(if neg { v.unsigned_abs() } else { v as u64 }, neg, c.a(2), c.a(3), c.a(4))
}
fn _ui64toa_s(c: &mut Ctx) -> u32 {
    toa_s(c.a(0) as u64 | (c.a(1) as u64) << 32, false, c.a(2), c.a(3), c.a(4))
}
fn abs(c: &mut Ctx) -> u32 {
    c.i(0).unsigned_abs()
}

fn qsort(c: &mut Ctx) -> u32 {
    let (base, n, w, f) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if n < 2 || w == 0 {
        return 0;
    }
    // sort a copy by index, comparing the copy's elements, then write the result back
    let tmp = mem::heap_alloc(n * w, false);
    mem::copy(tmp, base, n * w);
    let mut idx: Vec<u32> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        let r = c.call(f, &[tmp + a * w, tmp + b * w]) as i32;
        r.cmp(&0)
    });
    for (i, &k) in idx.iter().enumerate() {
        mem::copy(base + i as u32 * w, tmp + k * w, w);
    }
    mem::heap_free(tmp);
    0
}
fn bsearch(c: &mut Ctx) -> u32 {
    let (key, base, n, w, f) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let r = c.call(f, &[key, base + mid * w]) as i32;
        match r.cmp(&0) {
            std::cmp::Ordering::Equal => return base + mid * w,
            std::cmp::Ordering::Less => hi = mid,
            std::cmp::Ordering::Greater => lo = mid + 1,
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
fn time(c: &mut Ctx) -> u32 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as u32;
    if c.a(0) != 0 {
        w32(c.a(0), t);
    }
    t
}
fn _time64(c: &mut Ctx) -> u32 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if c.a(0) != 0 {
        w32(c.a(0), t as u32);
        w32(c.a(0) + 4, (t >> 32) as u32);
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

fn sprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(1)), &mut VaArgs::arg(c, 2), false);
    mem::write(c.a(0), &s);
    w8(c.a(0) + s.len() as u32, 0);
    s.len() as u32
}
fn vsprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(1)), &mut VaArgs::stack(c.a(2)), false);
    mem::write(c.a(0), &s);
    w8(c.a(0) + s.len() as u32, 0);
    s.len() as u32
}
fn snprintf_into(out: u32, cap: u32, s: &[u8]) -> u32 {
    let n = s.len().min(cap as usize);
    mem::write(out, &s[..n]);
    if s.len() < cap as usize {
        w8(out + n as u32, 0);
        s.len() as u32
    } else {
        u32::MAX
    }
}
fn _snprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(2)), &mut VaArgs::arg(c, 3), false);
    snprintf_into(c.a(0), c.a(1), &s)
}
fn _vsnprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(2)), &mut VaArgs::stack(c.a(3)), false);
    snprintf_into(c.a(0), c.a(1), &s)
}
fn swprintf(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(1)), &mut VaArgs::arg(c, 2));
    for (i, &u) in s.iter().chain([0u16].iter()).enumerate() {
        w16(c.a(0) + 2 * i as u32, u);
    }
    s.len() as u32
}
// the secure and counted variants (msvcr80+): `cap` is the buffer's size in characters
fn put_w(out: u32, cap: u32, s: &[u16], truncate: bool) -> u32 {
    if cap == 0 {
        return u32::MAX;
    }
    let n = s.len().min(cap as usize - 1);
    for (i, &u) in s[..n].iter().enumerate() {
        w16(out + 2 * i as u32, u);
    }
    w16(out + 2 * n as u32, 0);
    if n < s.len() && truncate { u32::MAX } else { n as u32 }
}
fn put_a(out: u32, cap: u32, s: &[u8], truncate: bool) -> u32 {
    if cap == 0 {
        return u32::MAX;
    }
    let n = s.len().min(cap as usize - 1);
    mem::write(out, &s[..n]);
    w8(out + n as u32, 0);
    if n < s.len() && truncate { u32::MAX } else { n as u32 }
}
/// sprintf_s(buf, size, fmt, ...)
fn sprintf_s(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(2)), &mut VaArgs::arg(c, 3), false);
    put_a(c.a(0), c.a(1), &s, false)
}
fn vsprintf_s(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(2)), &mut VaArgs::stack(c.a(3)), false);
    put_a(c.a(0), c.a(1), &s, false)
}
/// _snprintf_s(buf, size, count, fmt, ...): at most count characters (_TRUNCATE: -1 when cut)
fn _snprintf_s(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(3)), &mut VaArgs::arg(c, 4), false);
    put_a(c.a(0), c.a(1).min(c.a(2).saturating_add(1)), &s, true)
}
fn _vsnprintf_s(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(3)), &mut VaArgs::stack(c.a(4)), false);
    put_a(c.a(0), c.a(1).min(c.a(2).saturating_add(1)), &s, true)
}
/// _vscprintf(fmt, va): the length
fn _vscprintf(c: &mut Ctx) -> u32 {
    format(&cstr(c.a(0)), &mut VaArgs::stack(c.a(1)), false).len() as u32
}
fn _scprintf(c: &mut Ctx) -> u32 {
    format(&cstr(c.a(0)), &mut VaArgs::arg(c, 1), false).len() as u32
}
/// swprintf_s(buf, count, fmt, ...) (also msvcr80+'s swprintf with a count: _swprintf_c)
fn swprintf_s(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(2)), &mut VaArgs::arg(c, 3));
    put_w(c.a(0), c.a(1), &s, false)
}
fn vswprintf_s(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(2)), &mut VaArgs::stack(c.a(3)));
    put_w(c.a(0), c.a(1), &s, false)
}
/// _snwprintf(buf, count, fmt, ...) / _vsnwprintf: -1 when cut
fn _snwprintf(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(2)), &mut VaArgs::arg(c, 3));
    let n = s.len().min(c.a(1) as usize);
    for (i, &u) in s[..n].iter().enumerate() {
        w16(c.a(0) + 2 * i as u32, u);
    }
    if n < c.a(1) as usize {
        w16(c.a(0) + 2 * n as u32, 0);
        n as u32
    } else {
        u32::MAX
    }
}
fn _vsnwprintf(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(2)), &mut VaArgs::stack(c.a(3)));
    let n = s.len().min(c.a(1) as usize);
    for (i, &u) in s[..n].iter().enumerate() {
        w16(c.a(0) + 2 * i as u32, u);
    }
    if n < c.a(1) as usize {
        w16(c.a(0) + 2 * n as u32, 0);
        n as u32
    } else {
        u32::MAX
    }
}
/// _snwprintf_s(buf, size, count, fmt, ...)
fn _snwprintf_s(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(3)), &mut VaArgs::arg(c, 4));
    put_w(c.a(0), c.a(1).min(c.a(2).saturating_add(1)), &s, true)
}
fn _vsnwprintf_s(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(3)), &mut VaArgs::stack(c.a(4)));
    put_w(c.a(0), c.a(1).min(c.a(2).saturating_add(1)), &s, true)
}
fn vswprintf(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(1)), &mut VaArgs::stack(c.a(2)));
    for (i, &u) in s.iter().chain([0u16].iter()).enumerate() {
        w16(c.a(0) + 2 * i as u32, u);
    }
    s.len() as u32
}
fn _vscwprintf(c: &mut Ctx) -> u32 {
    format_w(&wstr(c.a(0)), &mut VaArgs::stack(c.a(1))).len() as u32
}
fn printf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(0)), &mut VaArgs::arg(c, 1), false);
    eprint!("{}", a2s(&s));
    s.len() as u32
}
fn sscanf(c: &mut Ctx) -> u32 {
    // the integer/string subset (%d %x %s %c %f)
    let s = cstr(c.a(0));
    let f = cstr(c.a(1));
    let mut va = VaArgs::arg(c, 2);
    let (mut i, mut k, mut n) = (0, 0, 0u32);
    while k < f.len() {
        let ch = f[k];
        if ch == b'%' && k + 1 < f.len() {
            k += 1;
            while k < f.len() && (f[k].is_ascii_digit() || f[k] == b'l' || f[k] == b'h') {
                k += 1;
            }
            let conv = f.get(k).copied().unwrap_or(0);
            k += 1;
            if conv != b'c' {
                while i < s.len() && s[i].is_ascii_whitespace() {
                    i += 1;
                }
            }
            match conv {
                b'd' | b'i' | b'u' | b'x' | b'X' => {
                    let (v, used, _) = parse_int(&s[i..], if matches!(conv, b'x' | b'X') { 16 } else { 10 });
                    if used == 0 {
                        break;
                    }
                    i += used;
                    w32(va.u32(), v as u32);
                }
                b'f' | b'g' | b'e' => {
                    let (v, used) = parse_float(&s[i..]);
                    if used == 0 {
                        break;
                    }
                    i += used;
                    mem::write(va.u32(), &(v as f32).to_le_bytes());
                }
                b's' => {
                    let st = i;
                    while i < s.len() && !s[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if i == st {
                        break;
                    }
                    let out = va.u32();
                    mem::write(out, &s[st..i]);
                    w8(out + (i - st) as u32, 0);
                }
                b'c' => {
                    let Some(&b) = s.get(i) else { break };
                    i += 1;
                    w8(va.u32(), b);
                }
                _ => break,
            }
            n += 1;
        } else if ch.is_ascii_whitespace() {
            while i < s.len() && s[i].is_ascii_whitespace() {
                i += 1;
            }
            k += 1;
        } else {
            if s.get(i) != Some(&ch) {
                break;
            }
            i += 1;
            k += 1;
        }
    }
    n
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
math1!(floor = f64::floor, ceil = f64::ceil, sqrt = f64::sqrt, sin = f64::sin, cos = f64::cos, tan = f64::tan,
    asin = f64::asin, acos = f64::acos, atan = f64::atan, exp = f64::exp, log = f64::ln, log10 = f64::log10,
    fabs = f64::abs, sinh = f64::sinh, cosh = f64::cosh, tanh = f64::tanh);
fn pow(c: &mut Ctx) -> u32 {
    let (x, y) = (c.f64(0), c.f64(2));
    ret(c, x.powf(y))
}
fn atan2(c: &mut Ctx) -> u32 {
    let (y, x) = (c.f64(0), c.f64(2));
    ret(c, y.atan2(x))
}
fn fmod(c: &mut Ctx) -> u32 {
    let (x, y) = (c.f64(0), c.f64(2));
    ret(c, x % y)
}
fn ldexp(c: &mut Ctx) -> u32 {
    let (x, e) = (c.f64(0), c.i(2));
    ret(c, x * 2f64.powi(e))
}
fn frexp(c: &mut Ctx) -> u32 {
    let (x, pe) = (c.f64(0), c.a(2));
    if x == 0.0 || !x.is_finite() {
        w32(pe, 0);
        return ret(c, x);
    }
    let mut e = x.abs().log2().floor() as i32 + 1;
    let mut m = x / 2f64.powi(e);
    if m.abs() >= 1.0 {
        m /= 2.0;
        e += 1;
    } else if m.abs() < 0.5 {
        m *= 2.0;
        e -= 1;
    }
    w32(pe, e as u32);
    ret(c, m)
}
fn modf(c: &mut Ctx) -> u32 {
    let (x, p) = (c.f64(0), c.a(2));
    mem::write(p, &x.trunc().to_le_bytes());
    ret(c, x.fract())
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
// stdio: a FILE is 32 guest bytes (as msvcrt's) that we only use as a key

const FILE_SIZE: u32 = 32;

struct Stream {
    f: Option<Arc<File>>,
    eof: bool,
    unget: Option<u8>,
}

static FILES: Mutex<Option<HashMap<u32, Stream>>> = Mutex::new(None);

fn iob() -> u32 {
    let p = mem::heap_alloc(FILE_SIZE * 20, true);
    let mut g = FILES.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
    for i in 0..3 {
        m.insert(p + FILE_SIZE * i, Stream { f: None, eof: true, unget: None });
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
    let fp = mem::heap_alloc(FILE_SIZE, true);
    FILES.lock().unwrap().get_or_insert_with(HashMap::default).insert(fp, Stream { f: Some(Arc::new(f)), eof: false, unget: None });
    fp
}
/// _get_stream_buffer_pointers(stream, &base, &ptr, &cnt): the FILE's buffer fields (UCRT
/// order _ptr, _base, _cnt); they stay 0, so std::basic_filebuf always reads through the CRT
fn _get_stream_buffer_pointers(c: &mut Ctx) -> u32 {
    let fp = c.a(0);
    for (out, off) in [(c.a(1), 4), (c.a(2), 0), (c.a(3), 8)] {
        if out != 0 {
            w32(out, fp + off);
        }
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
        set_errno(13);
        return u32::MAX;
    }
    if kura::fs::rename(from, crate::files::resolve_new(&to)).is_ok() { 0 } else { set_errno(13); u32::MAX }
}
fn fclose(c: &mut Ctx) -> u32 {
    let fp = c.a(0);
    let had = FILES.lock().unwrap().get_or_insert_with(HashMap::default).remove(&fp).is_some();
    if had {
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
    let (fp, off, whence) = (c.a(0), c.i(1) as i64, c.a(2));
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
    w32(c.a(1), p);
    w32(c.a(1) + 4, 0);
    0
}
fn fsetpos(c: &mut Ctx) -> u32 {
    let (fp, off) = (c.a(0), r32(c.a(1)) as u64);
    with_file(fp, |s| {
        let Some(f) = &s.f else { return u32::MAX };
        s.unget = None;
        s.eof = false;
        if f.seek(SeekFrom::Start(off)).is_some() { 0 } else { u32::MAX }
    })
    .unwrap_or(u32::MAX)
}
/// strerror(errno): one fixed message
fn strerror(_: &mut Ctx) -> u32 {
    static S: OnceLock<u32> = OnceLock::new();
    *S.get_or_init(|| {
        let p = mem::heap_alloc(16, true);
        mem::write(p, b"Unknown error\0");
        p
    })
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
fn fprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(1)), &mut VaArgs::arg(c, 2), false);
    let tmp = mem::heap_alloc(s.len() as u32 + 1, false);
    mem::write(tmp, &s);
    with_file(c.a(0), |st| match &st.f {
        Some(f) => {
            f.write_from(tmp, s.len() as u32);
        }
        None => eprint!("{}", a2s(&s)),
    });
    mem::heap_free(tmp);
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
    // the process environment (GetEnvironmentVariableA), kept in a guest buffer per name
    static VALS: Mutex<Option<HashMap<Vec<u8>, u32>>> = Mutex::new(None);
    let name = cstr(c.a(0));
    let f = crate::api::export("kernel32", "GetEnvironmentVariableA");
    let n = c.call(f, &[c.a(0), 0, 0]);
    if n == 0 {
        return 0;
    }
    let mut g = VALS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
    if let Some(&p) = m.get(&name) {
        mem::heap_free(p);
    }
    let p = mem::heap_alloc(n + 1, true);
    m.insert(name, p);
    drop(g);
    c.call(f, &[c.a(0), p, n + 1]);
    p
}
fn _ismbblead(c: &mut Ctx) -> u32 {
    let b = c.a(0) & 0xff;
    (crate::text::acp() == 932 && matches!(b, 0x81..=0x9f | 0xe0..=0xfc)) as u32
}
fn memcpy_s(c: &mut Ctx) -> u32 {
    let (d, dn, s, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if n == 0 {
        return 0;
    }
    if d == 0 || s == 0 || dn < n {
        if d != 0 {
            mem::fill(d, dn, 0);
        }
        return 22;
    }
    mem::copy(d, s, n);
    0
}
fn _controlfp_s(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        w32(c.a(0), 0x9001f);
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
    let fp = fopen_mode(&path, &mode).map_or(0, new_stream);
    mem::put32(pf, fp);
    if fp == 0 { 2 } else { 0 }
}
fn _wfopen_s(c: &mut Ctx) -> u32 {
    let path = String::from_utf16_lossy(&wstr(c.a(1)));
    let mode: Vec<u8> = wstr(c.a(2)).iter().map(|&u| u as u8).collect();
    let fp = fopen_mode(&path, &mode).map_or(0, new_stream);
    mem::put32(c.a(0), fp);
    if fp == 0 { 2 } else { 0 }
}

// setjmp/longjmp: jmp_buf = ebp, ebx, edi, esi, esp, SEH frame, eip (as msvcrt's first fields)
fn _setjmp3(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let r = &c.cpu.regs;
    for (i, v) in [r[koji::EBP], r[koji::EBX], r[koji::EDI], r[koji::ESI], c.esp + 4, r32(crate::cur().teb), r32(c.esp)].into_iter().enumerate() {
        w32(b + 4 * i as u32, v);
    }
    0
}
fn longjmp(c: &mut Ctx) -> u32 {
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
fn common_out(opts: u32, buf: u32, len: u32, n: usize, put: impl Fn(u32, usize)) -> u32 {
    if buf == 0 {
        return n as u32;
    }
    if n < len as usize {
        put(buf, n);
        return n as u32;
    }
    if len == 0 {
        return u32::MAX;
    }
    // _CRT_INTERNAL_PRINTF_STANDARD_SNPRINTF_BEHAVIOR: C99 snprintf, else _snprintf's -1
    put(buf, len as usize - 1);
    if opts & 2 != 0 { n as u32 } else { u32::MAX }
}
fn __stdio_common_vsprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(4)), &mut VaArgs::stack(c.a(6)), false);
    common_out(c.a(0), c.a(2), c.a(3), s.len(), |b, n| {
        mem::write(b, &s[..n]);
        w8(b + n as u32, 0);
    })
}
/// the _s forms: a result that doesn't fit leaves an empty string and returns -1
fn __stdio_common_vsprintf_s(c: &mut Ctx) -> u32 {
    let (buf, len) = (c.a(2), c.a(3));
    let s = format(&cstr(c.a(4)), &mut VaArgs::stack(c.a(6)), false);
    if buf == 0 || s.len() >= len as usize {
        if buf != 0 && len != 0 {
            w8(buf, 0);
        }
        return u32::MAX;
    }
    mem::write(buf, &s);
    w8(buf + s.len() as u32, 0);
    s.len() as u32
}
fn __stdio_common_vswprintf_s(c: &mut Ctx) -> u32 {
    let (buf, len) = (c.a(2), c.a(3));
    let s = format_w(&wstr(c.a(4)), &mut VaArgs::stack(c.a(6)));
    if buf == 0 || s.len() >= len as usize {
        if buf != 0 && len != 0 {
            w16(buf, 0);
        }
        return u32::MAX;
    }
    mem::put_wstr(buf, len, &s);
    s.len() as u32
}
/// strncpy_s(dst, size, src, count): count _TRUNCATE (-1) copies what fits
fn strncpy_s(c: &mut Ctx) -> u32 {
    let (d, size, src, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if d == 0 || size == 0 {
        return 22;
    }
    let s = mem::cstrn(src, n);
    if s.len() >= size as usize && n != u32::MAX {
        w8(d, 0);
        return 34;
    }
    mem::put_cstr(d, size, &s);
    if s.len() >= size as usize { 80 } else { 0 }
}
/// strcpy_s / wcscpy_s / strcat_s / wcscat_s / wcsncpy_s (dst, size, src[, count]): 0, or 22 /
/// 34 (ERANGE) with an empty dst when it doesn't fit
fn copy_s(d: u32, size: u32, src: Vec<u16>, wide: bool, append: bool, truncate: bool) -> u32 {
    if d == 0 || size == 0 {
        return 22;
    }
    let mut v: Vec<u16> = if append { if wide { wstr(d) } else { cstr(d).into_iter().map(|b| b as u16).collect() } } else { Vec::new() };
    v.extend(src);
    if v.len() >= size as usize && !truncate {
        if wide { w16(d, 0) } else { w8(d, 0) }
        return 34;
    }
    if wide {
        mem::put_wstr(d, size, &v);
    } else {
        let b: Vec<u8> = v.into_iter().map(|u| u as u8).collect();
        mem::put_cstr(d, size, &b);
    }
    0
}
fn strcpy_s(c: &mut Ctx) -> u32 {
    copy_s(c.a(0), c.a(1), cstr(c.a(2)).into_iter().map(|b| b as u16).collect(), false, false, false)
}
fn wcscpy_s(c: &mut Ctx) -> u32 {
    copy_s(c.a(0), c.a(1), wstr(c.a(2)), true, false, false)
}
fn strcat_s(c: &mut Ctx) -> u32 {
    copy_s(c.a(0), c.a(1), cstr(c.a(2)).into_iter().map(|b| b as u16).collect(), false, true, false)
}
fn wcscat_s(c: &mut Ctx) -> u32 {
    copy_s(c.a(0), c.a(1), wstr(c.a(2)), true, true, false)
}
fn wcsncpy_s(c: &mut Ctx) -> u32 {
    let n = c.a(3);
    let mut s = wstr(c.a(2));
    s.truncate(n as usize);
    copy_s(c.a(0), c.a(1), s, true, false, n == u32::MAX)
}
fn ___lc_codepage_func(_: &mut Ctx) -> u32 {
    932
}
/// the locale names by category (LC_ALL..LC_TIME): all "ja-JP"
fn ___lc_locale_name_func(_: &mut Ctx) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| {
        let p = mem::heap_alloc(6 * 4 + 16, true);
        mem::put_wstr(p + 24, 8, &"ja-JP".encode_utf16().collect::<Vec<u16>>());
        for i in 0..6 {
            w32(p + 4 * i, p + 24);
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
fn __stdio_common_vswprintf(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(4)), &mut VaArgs::stack(c.a(6)));
    common_out(c.a(0), c.a(2), c.a(3), s.len(), |b, n| {
        for (i, &u) in s[..n].iter().chain([0u16].iter()).enumerate() {
            w16(b + 2 * i as u32, u);
        }
    })
}

/// setlocale: only "C"
fn setlocale(_: &mut Ctx) -> u32 {
    static C: OnceLock<u32> = OnceLock::new();
    *C.get_or_init(|| {
        let p = mem::heap_alloc(2, true);
        w8(p, b'C');
        p
    })
}
/// the "C" locale's struct lconv
fn localeconv(_: &mut Ctx) -> u32 {
    static L: OnceLock<u32> = OnceLock::new();
    *L.get_or_init(|| {
        // (msvcr100+ / the UCRT add 8 UTF-16 strings after the C fields: _W_decimal_point at 0x30,
        // read by std::numpunct<wchar_t>)
        let l = mem::heap_alloc(0x50, true);
        let (dot, empty) = (mem::heap_alloc(2, true), mem::heap_alloc(2, true));
        w8(dot, b'.');
        w32(l, dot);
        for i in 1..10 {
            w32(l + 4 * i, empty);
        }
        // int_frac_digits.. n_sign_posn: CHAR_MAX
        for i in 40..48 {
            w8(l + i, 127);
        }
        let (wdot, wempty) = (mem::heap_alloc(4, true), mem::heap_alloc(4, true));
        w16(wdot, b'.' as u16);
        w32(l + 0x30, wdot);
        for i in 1..8 {
            w32(l + 0x30 + 4 * i, wempty);
        }
        l
    })
}
fn _putenv(_: &mut Ctx) -> u32 {
    0
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
    data("_iob").unwrap_or(0) + FILE_SIZE * c.a(0)
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
    fp_class(c.a(0) as u64, 8, 23)
}
fn _dclass(c: &mut Ctx) -> u32 {
    fp_class(c.a(0) as u64 | (c.a(1) as u64) << 32, 11, 52)
}
fn _fdsign(c: &mut Ctx) -> u32 {
    (c.a(0) >> 31) << 15
}
fn _dsign(c: &mut Ctx) -> u32 {
    (c.a(1) >> 31) << 15
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
    r32(data("_wenviron").unwrap())
}
fn ucrt_zero(_: &mut Ctx) -> u32 {
    0
}
/// _onexit_table_t: { first, last, end } (plain function pointers here)
fn _initialize_onexit_table(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    if t == 0 {
        return u32::MAX;
    }
    if r32(t) == 0 {
        mem::fill(t, 12, 0);
    }
    0
}
fn _register_onexit_function(c: &mut Ctx) -> u32 {
    let (t, f) = (c.a(0), c.a(1));
    if t == 0 {
        return u32::MAX;
    }
    let (first, last, end) = (r32(t), r32(t + 4), r32(t + 8));
    if first == 0 || last >= end {
        let n = if first == 0 { 32 } else { (end - first) / 4 * 2 };
        let p = mem::heap_alloc(n * 4, true);
        if first != 0 {
            mem::write(p, &mem::slice(first, last - first).to_vec());
            mem::heap_free(first);
        }
        w32(t, p);
        w32(t + 4, p + (last - first));
        w32(t + 8, p + n * 4);
    }
    let last = r32(t + 4);
    w32(last, f);
    w32(t + 4, last + 4);
    0
}
/// runs the table's functions, last registered first, and empties it
fn _execute_onexit_table(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    if t == 0 {
        return u32::MAX;
    }
    let (first, last) = (r32(t), r32(t + 4));
    mem::fill(t, 12, 0);
    let mut p = last;
    while p > first {
        p -= 4;
        let f = r32(p);
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
    eprintln!("sake: terminate() from {:#x}", r32(c.esp));
    unsafe { libc::exit(3) }
}
fn wcsnlen(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    (0..n).find(|&i| mem::r16(p + 2 * i) == 0).unwrap_or(n)
}
/// __std_exception_data: { what, do_free }
fn __std_exception_copy(c: &mut Ctx) -> u32 {
    let (src, dst) = (c.a(0), c.a(1));
    let what = r32(src);
    if mem::r8(src + 4) != 0 && what != 0 {
        let s = cstr(what);
        let p = mem::heap_alloc(s.len() as u32 + 1, true);
        mem::write(p, &s);
        w32(dst, p);
        w32(dst + 4, 1);
    } else {
        w32(dst, what);
        w32(dst + 4, 0);
    }
    0
}
fn __std_exception_destroy(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    if mem::r8(d + 4) != 0 && r32(d) != 0 {
        mem::heap_free(r32(d));
    }
    w32(d, 0);
    w32(d + 4, 0);
    0
}
/// __std_type_info_data: { undecorated name, decorated name... }
fn __std_type_info_compare(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    if a == b {
        return 0;
    }
    cstr(a + 5).cmp(&cstr(b + 5)) as i32 as u32
}
fn __std_type_info_hash(c: &mut Ctx) -> u32 {
    // FNV-1a, as the UCRT
    let mut h: u32 = 0x811c9dc5;
    for b in cstr(c.a(0) + 5) {
        h = (h ^ b as u32).wrapping_mul(0x01000193);
    }
    h
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
/// struct tm from seconds since 1970 (UTC or local)
fn tm_of(t: i64, local: bool) -> [i32; 9] {
    let t = if local { t + crate::k32::tz_bias() } else { t };
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let yday = days - days_from_civil(y, 1, 1);
    [(rem % 60) as i32, (rem / 60 % 60) as i32, (rem / 3600) as i32, d as i32, (m - 1) as i32, (y - 1900) as i32,
        (days + 4).rem_euclid(7) as i32, yday as i32, 0]
}
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
fn time_tm(c: &mut Ctx, wide: bool, local: bool) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return 0;
    }
    let t = if wide { (r32(p) as u64 | (r32(p + 4) as u64) << 32) as i64 } else { r32(p) as i32 as i64 };
    if t < 0 {
        return 0;
    }
    let b = tm_buf();
    write_tm(b, &tm_of(t, local));
    b
}
fn localtime32(c: &mut Ctx) -> u32 {
    time_tm(c, false, true)
}
fn _localtime64(c: &mut Ctx) -> u32 {
    time_tm(c, true, true)
}
fn gmtime32(c: &mut Ctx) -> u32 {
    time_tm(c, false, false)
}
fn _gmtime64(c: &mut Ctx) -> u32 {
    time_tm(c, true, false)
}
/// the _s forms: (tm*, time_t*)
fn time_tm_s(c: &mut Ctx, wide: bool, local: bool) -> u32 {
    let (out, p) = (c.a(0), c.a(1));
    if out == 0 || p == 0 {
        return 22;
    }
    let t = if wide { (r32(p) as u64 | (r32(p + 4) as u64) << 32) as i64 } else { r32(p) as i32 as i64 };
    write_tm(out, &tm_of(t, local));
    0
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
/// mktime: normalizes the tm (local time) and gives its time
fn mktime_of(p: u32) -> i64 {
    let tm = read_tm(p);
    let (mut y, mut mon) = (tm[5] as i64 + 1900, tm[4] as i64);
    y += mon.div_euclid(12);
    mon = mon.rem_euclid(12);
    let days = days_from_civil(y, mon + 1, 1) + tm[3] as i64 - 1;
    let t = days * 86400 + tm[2] as i64 * 3600 + tm[1] as i64 * 60 + tm[0] as i64 - crate::k32::tz_bias();
    write_tm(p, &tm_of(t, true));
    t
}
fn mktime32(c: &mut Ctx) -> u32 {
    mktime_of(c.a(0)) as u32
}
fn _mktime64(c: &mut Ctx) -> u32 {
    let t = mktime_of(c.a(0));
    c.ret64(t as u64)
}
fn difftime32(c: &mut Ctx) -> u32 {
    c.st0 = Some(c.a(0) as i32 as f64 - c.a(1) as i32 as f64);
    0
}
fn _difftime64(c: &mut Ctx) -> u32 {
    let a = (c.a(0) as u64 | (c.a(1) as u64) << 32) as i64;
    let b = (c.a(2) as u64 | (c.a(3) as u64) << 32) as i64;
    c.st0 = Some((a - b) as f64);
    0
}

/// strftime's output for `fmt` (C locale names, as the UCRT's default)
fn strftime_of(fmt: &[u16], tm: &[i32; 9]) -> Vec<u16> {
    let mut o = String::new();
    let mut i = 0;
    let hour12 = |h: i32| if h % 12 == 0 { 12 } else { h % 12 };
    while i < fmt.len() {
        let ch = fmt[i];
        i += 1;
        if ch != b'%' as u16 || i >= fmt.len() {
            o.push(char::from_u32(ch as u32).unwrap_or('?'));
            continue;
        }
        let mut f = fmt[i];
        // ("%#d": no leading zeros)
        let alt = f == b'#' as u16;
        if alt && i + 1 < fmt.len() {
            i += 1;
            f = fmt[i];
        }
        i += 1;
        let n2 = |v: i32| if alt { format!("{v}") } else { format!("{v:02}") };
        let (wd, mon) = (tm[6].rem_euclid(7) as usize, tm[4].rem_euclid(12) as usize);
        match f as u8 {
            b'a' => o += &DAYS[wd][..3],
            b'A' => o += DAYS[wd],
            b'b' | b'h' => o += &MONTHS[mon][..3],
            b'B' => o += MONTHS[mon],
            b'c' => o += &format!("{}/{}/{} {}:{}:{}", n2(tm[4] + 1), n2(tm[3]), n2(tm[5] % 100), n2(tm[2]), n2(tm[1]), n2(tm[0])),
            b'd' => o += &n2(tm[3]),
            b'e' => o += &format!("{:2}", tm[3]),
            b'H' => o += &n2(tm[2]),
            b'I' => o += &n2(hour12(tm[2])),
            b'j' => o += &if alt { format!("{}", tm[7] + 1) } else { format!("{:03}", tm[7] + 1) },
            b'm' => o += &n2(tm[4] + 1),
            b'M' => o += &n2(tm[1]),
            b'p' => o += if tm[2] < 12 { "AM" } else { "PM" },
            b'S' => o += &n2(tm[0]),
            b'U' => o += &n2((tm[7] + 7 - tm[6]) / 7),
            b'W' => o += &n2((tm[7] + 7 - (tm[6] + 6) % 7) / 7),
            b'w' => o += &format!("{}", tm[6]),
            b'x' | b'D' => o += &format!("{}/{}/{}", n2(tm[4] + 1), n2(tm[3]), n2(tm[5] % 100)),
            b'X' | b'T' => o += &format!("{}:{}:{}", n2(tm[2]), n2(tm[1]), n2(tm[0])),
            b'R' => o += &format!("{}:{}", n2(tm[2]), n2(tm[1])),
            b'F' => o += &format!("{}-{:02}-{:02}", tm[5] + 1900, tm[4] + 1, tm[3]),
            b'y' => o += &n2(tm[5] % 100),
            b'Y' => o += &format!("{}", tm[5] + 1900),
            b'z' => o += if crate::k32::tz_bias() == 9 * 3600 { "+0900" } else { "+0000" },
            b'Z' => o += if crate::k32::tz_bias() == 9 * 3600 { "Tokyo Standard Time" } else { "Coordinated Universal Time" },
            b'n' => o.push('\n'),
            b't' => o.push('\t'),
            b'%' => o.push('%'),
            _ => {}
        }
    }
    o.encode_utf16().collect()
}
fn strftime_into(c: &mut Ctx, wide: bool) -> u32 {
    let (buf, max, fmt, tm) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if buf == 0 || fmt == 0 || tm == 0 {
        return 0;
    }
    let f = if wide { wstr(fmt) } else { crate::text::a2w(&cstr(fmt)) };
    let out = strftime_of(&f, &read_tm(tm));
    if wide {
        if out.len() >= max as usize {
            if max != 0 {
                w16(buf, 0);
            }
            return 0;
        }
        mem::put_wstr(buf, max, &out);
        out.len() as u32
    } else {
        let a = crate::text::w2a(&out);
        if a.len() >= max as usize {
            if max != 0 {
                w8(buf, 0);
            }
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
/// UTF-16, locale name), C locale
fn _Gettnames(_: &mut Ctx) -> u32 {
    let mut strs: Vec<String> = Vec::new();
    strs.extend(DAYS.iter().map(|d| d[..3].to_string()));
    strs.extend(DAYS.iter().map(|d| d.to_string()));
    strs.extend(MONTHS.iter().map(|m| m[..3].to_string()));
    strs.extend(MONTHS.iter().map(|m| m.to_string()));
    strs.extend(["AM", "PM", "MM/dd/yy", "dddd, MMMM dd, yyyy", "HH:mm:ss"].map(String::from));
    let head = 43 * 4 + 8 + 43 * 4 + 4;
    let size = head + strs.iter().map(|s| 3 * (s.len() as u32 + 1)).sum::<u32>() + 4;
    let p = mem::heap_alloc(size, true);
    let mut at = p + head;
    for (i, s) in strs.iter().enumerate() {
        w32(p + 4 * i as u32, at);
        mem::write(at, s.as_bytes());
        at += s.len() as u32 + 1;
    }
    at = (at + 1) & !1;
    for (i, s) in strs.iter().enumerate() {
        w32(p + 43 * 4 + 8 + 4 * i as u32, at);
        let u: Vec<u16> = s.encode_utf16().collect();
        mem::put_wstr(at, u.len() as u32 + 1, &u);
        at += 2 * (u.len() as u32 + 1);
    }
    w32(p + 43 * 4 + 4, 1);
    // (locale name "C")
    w32(p + 43 * 8 + 8, at);
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
fn _ultoa(c: &mut Ctx) -> u32 {
    itoa_common(c.a(0) as i64, c.a(1), c.a(2).clamp(2, 36))
}
fn i64_to_a(v: u64, neg: bool, out: u32, base: u32) -> u32 {
    let mut s = digits(v, base.clamp(2, 36) as u64);
    if neg {
        s.insert(0, b'-');
    }
    s.push(0);
    mem::write(out, &s);
    out
}
fn _i64toa(c: &mut Ctx) -> u32 {
    let v = (c.a(0) as u64 | (c.a(1) as u64) << 32) as i64;
    let neg = c.a(3) == 10 && v < 0;
    i64_to_a(if neg { v.unsigned_abs() } else { v as u64 }, neg, c.a(2), c.a(3))
}
fn _ui64toa(c: &mut Ctx) -> u32 {
    i64_to_a(c.a(0) as u64 | (c.a(1) as u64) << 32, false, c.a(2), c.a(3))
}
fn _atoi64(c: &mut Ctx) -> u32 {
    let v = parse_int(&cstr(c.a(0)), 10).0;
    c.ret64(v as u64)
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
fn _ecvt(c: &mut Ctx) -> u32 {
    let (v, n) = (c.f64(0), c.i(2).max(0) as usize);
    let (d, dec) = ecvt_digits(v, n);
    cvt_out(c, &d, dec, v.is_sign_negative() && v != 0.0, c.a(3), c.a(4))
}
fn _fcvt(c: &mut Ctx) -> u32 {
    let (v, n) = (c.f64(0), c.i(2).clamp(0, 300) as usize);
    let s = format!("{:.*}", n, v.abs());
    let (ip, fp) = s.split_once('.').unwrap_or((&s, ""));
    let mut d: Vec<u8> = ip.bytes().chain(fp.bytes()).collect();
    let mut dec = ip.len() as i32;
    while d.len() > 1 && d[0] == b'0' && dec > 0 {
        d.remove(0);
        dec -= 1;
    }
    if ip == "0" {
        // 0.00123: "123" with the point 2 digits to its left
        let z = d.iter().take_while(|&&b| b == b'0').count();
        if z == d.len() {
            d.clear();
            dec = -(n as i32);
            if v == 0.0 { dec = 0; }
        } else {
            d.drain(..z);
            dec = -(z as i32);
        }
    }
    cvt_out(c, &d, dec, v.is_sign_negative() && v != 0.0, c.a(3), c.a(4))
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
    implement("msvcrt", "_strtof_l", strtod);
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
        ("_localtime32", localtime32),
        ("gmtime", gmtime32),
        ("_gmtime32", gmtime32),
        ("mktime", mktime32),
        ("_mktime32", mktime32),
        ("difftime", difftime32),
        ("_difftime32", difftime32),
        ("_Strftime", strftime),
        ("_Wcsftime", wcsftime),
        ("_W_Gettnames", _Gettnames),
    ] {
        implement("msvcrt", alias, f);
    }
    implement("msvcrt", "_strtod_l", strtod);
    implement("msvcrt", "strtof", strtod);
    reg!(_wcsicmp, _wcsnicmp, wcschr, wcsrchr, wcscat, wcsncat, wcsncpy, wcsstr, wcsspn, wcscspn, wcspbrk);
    reg!(towlower, towupper, iswspace, iswdigit, iswalpha, wcstol, wcstoul, _wtoi, wcstod);
    implement("msvcrt", "_wtol", _wtoi);
    reg!(_wcsdup, _wcslwr, _wcsupr, wcstok_s, strtok_s, strtok, wcstok);
    // (ucrtbase's wcstok takes a context)
    implement("ucrtbase", "wcstok", wcstok_s);
    reg!(localeconv, setlocale);
    reg!(strcpy_s, wcscpy_s, strcat_s, wcscat_s, wcsncpy_s);
    reg!(_itoa_s, _ultoa_s, _i64toa_s, _ui64toa_s);
    reg!(puts, putchar, setvbuf, _strrev, _ultoa, _i64toa, _ui64toa, _atoi64, div, _isnan, _finite, _fpclass, _hypot,
        _ecvt, _fcvt, _mbslen, _mbschr, _mbsrchr, _mbsinc, _mbsnextc, _ismbcspace, _mbctohira, _mbctokata,
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
    reg!(fgetpos, fsetpos, strerror, ___lc_collate_cp_func);
    implement("msvcrt", "_fsopen", fopen);
    implement("msvcrt", "_wfsopen", _wfopen);
    reg!(_aligned_malloc, _aligned_free, _aligned_realloc, _aligned_msize, _mkdir, _wmkdir);
    reg!(fread_s, _time64, _splitpath, _wsplitpath, _splitpath_s, _wsplitpath_s);
    reg!(_get_narrow_winmain_command_line, _get_wide_winmain_command_line);
    implement("msvcrt", "_time32", time);
    reg!(_putenv, fputc, __acrt_iob_func, _create_locale, _free_locale, _fdclass, _dclass, _fdsign, _dsign, strnlen);
    implement("ucrtbase", "__stdio_common_vsprintf", __stdio_common_vsprintf);
    implement("ucrtbase", "__stdio_common_vswprintf", __stdio_common_vswprintf);
    reg!(wcsncmp, getenv, _ismbblead, memcpy_s, _controlfp_s, __iob_func, __p__iob, fopen_s, _wfopen_s, _setjmp3, longjmp, remove, rename);
    implement("msvcrt", "_setjmp", _setjmp3);
    reg!(_encode_pointer, _encoded_null, _crt_debugger_hook, _configthreadlocale, ___mb_cur_max_l_func);
    reg!(_initterm, _initterm_e, _onexit, atexit, __dllonexit, exit, _exit, abort, __getmainargs, __set_app_type,
        __p__fmode, __p__commode, __p___argc, __p___argv, __p__environ, __p___initenv, __p__acmdln,
        __p___mb_cur_max, ___mb_cur_max_func, __pctype_func, _controlfp, _control87, _clearfp, _purecall,
        _XcptFilter, signal, _lock, _unlock, _errno, __doserrno,
        malloc, calloc, realloc, free, _msize,
        memcpy, memmove, memset, memcmp, memchr, strlen, strcpy, strncpy, strcat, strncat, strcmp, strncmp,
        _stricmp, _strnicmp, strchr, strrchr, strstr, _strdup, _strlwr, _strupr, wcslen, wcscpy, wcscmp,
        toupper, tolower, isalpha, isupper, islower, isdigit, isspace, ispunct, isalnum, isxdigit, isprint,
        atoi, strtol, strtoul, atof, strtod, _itoa, abs, qsort, bsearch, srand, rand, time, clock,
        sprintf, vsprintf, _snprintf, _vsnprintf, swprintf, printf, sscanf, sprintf_s, vsprintf_s, _snprintf_s,
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
