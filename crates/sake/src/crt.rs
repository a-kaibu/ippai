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
fn clock(_: &mut Ctx) -> u32 {
    crate::k32::ticks_ms().wrapping_sub(60_000)
}

// ---------------------------------------------------------------------------------------------
// printf

fn sprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(1)), &mut VaArgs::stack(c.esp + 12), false);
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
    let s = format(&cstr(c.a(2)), &mut VaArgs::stack(c.esp + 16), false);
    snprintf_into(c.a(0), c.a(1), &s)
}
fn _vsnprintf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(2)), &mut VaArgs::stack(c.a(3)), false);
    snprintf_into(c.a(0), c.a(1), &s)
}
fn swprintf(c: &mut Ctx) -> u32 {
    let s = format_w(&wstr(c.a(1)), &mut VaArgs::stack(c.esp + 12));
    for (i, &u) in s.iter().chain([0u16].iter()).enumerate() {
        w16(c.a(0) + 2 * i as u32, u);
    }
    s.len() as u32
}
fn printf(c: &mut Ctx) -> u32 {
    let s = format(&cstr(c.a(0)), &mut VaArgs::stack(c.esp + 8), false);
    eprint!("{}", a2s(&s));
    s.len() as u32
}
fn sscanf(c: &mut Ctx) -> u32 {
    // the integer/string subset (%d %x %s %c %f)
    let s = cstr(c.a(0));
    let f = cstr(c.a(1));
    let mut va = VaArgs::stack(c.esp + 12);
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
    let (buf, size, n, fp) = (c.a(0), c.a(1), c.a(2), c.a(3));
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
    with_file(fp, |s| {
        let Some(f) = &s.f else { return u32::MAX };
        s.unget = None;
        s.eof = false;
        let pos = match whence {
            0 => SeekFrom::Start(off.max(0) as u64),
            1 => SeekFrom::Current(off),
            _ => SeekFrom::End(off),
        };
        if f.seek(pos).is_some() { 0 } else { u32::MAX }
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
    let s = format(&cstr(c.a(1)), &mut VaArgs::stack(c.esp + 12), false);
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
    for (i, v) in [r[vnx86::EBP], r[vnx86::EBX], r[vnx86::EDI], r[vnx86::ESI], c.esp + 4, r32(crate::cur().teb), r32(c.esp)].into_iter().enumerate() {
        w32(b + 4 * i as u32, v);
    }
    0
}
fn longjmp(c: &mut Ctx) -> u32 {
    let (b, v) = (c.a(0), c.a(1));
    let g = |i: u32| r32(b + 4 * i);
    c.cpu.regs[vnx86::EBP] = g(0);
    c.cpu.regs[vnx86::EBX] = g(1);
    c.cpu.regs[vnx86::EDI] = g(2);
    c.cpu.regs[vnx86::ESI] = g(3);
    c.cpu.regs[vnx86::ESP] = g(4);
    w32(crate::cur().teb, g(5));
    c.cpu.regs[vnx86::EAX] = if v == 0 { 1 } else { v };
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
        let l = mem::heap_alloc(64, true);
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
/// _locale_t: only ever the "C" locale
fn _create_locale(_: &mut Ctx) -> u32 {
    static LOC: OnceLock<u32> = OnceLock::new();
    *LOC.get_or_init(|| mem::heap_alloc(64, true))
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

pub fn init() {
    implement("msvcrt", "_strtof_l", strtod);
    implement("msvcrt", "_strtod_l", strtod);
    implement("msvcrt", "strtof", strtod);
    reg!(_wcsicmp, _wcsnicmp, wcschr, wcsrchr, wcscat, wcsncat, wcsncpy, wcsstr, wcsspn, wcscspn, wcspbrk);
    reg!(towlower, towupper, iswspace, iswdigit, iswalpha, wcstol, wcstoul, _wtoi, wcstod);
    implement("msvcrt", "_wtol", _wtoi);
    reg!(_wcsdup, _wcslwr, _wcsupr, wcstok_s, strtok_s, strtok, wcstok);
    // (ucrtbase's wcstok takes a context)
    implement("ucrtbase", "wcstok", wcstok_s);
    reg!(localeconv, setlocale);
    reg!(_putenv, fputc, __acrt_iob_func, _create_locale, _free_locale, _fdclass, _dclass, _fdsign, _dsign, strnlen);
    implement("ucrtbase", "__stdio_common_vsprintf", __stdio_common_vsprintf);
    implement("ucrtbase", "__stdio_common_vswprintf", __stdio_common_vswprintf);
    reg!(wcsncmp, getenv, _ismbblead, memcpy_s, _controlfp_s, __iob_func, fopen_s, _wfopen_s, _setjmp3, longjmp);
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
        sprintf, vsprintf, _snprintf, _vsnprintf, swprintf, printf, sscanf,
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
