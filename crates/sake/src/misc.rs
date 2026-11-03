//! The smaller DLLs: printf formatting (wsprintf, msvcrt), the registry (advapi32, kept in
//! memory), version, ole32/oleaut32 basics, shell32 folders, comctl32, winmm timers.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::{implement, Ctx};
use crate::k32::{arg_str, ret_str};
use crate::mem::{self, cstr, put32, r16, r32, r8, w16, w32, wstr};
use crate::obj::{self, Obj};
use crate::text::{a2w, s2a, w2a};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

// ---------------------------------------------------------------------------------------------
// printf

/// Variable arguments on the guest stack.
pub struct VaArgs {
    at: u32,
}

impl VaArgs {
    /// A va_list (x64: 8-byte slots).
    pub fn stack(at: u32) -> VaArgs {
        VaArgs { at }
    }
    /// The variable arguments from argument `i` of the call in `c` on (x64: the register ones
    /// go to their home space first, as va_start does).
    pub fn arg(c: &crate::api::Ctx, i: u32) -> VaArgs {
        if crate::mem::x64() {
            for k in 0..4 {
                crate::mem::w64(c.esp + 8 + 8 * k, c.a64(k));
            }
            return VaArgs { at: c.esp + 8 + 8 * i };
        }
        VaArgs { at: c.esp + 4 + 4 * i }
    }
    pub fn u32(&mut self) -> u32 {
        let v = r32(self.at);
        self.at += crate::mem::ps();
        v
    }
    pub fn u64(&mut self) -> u64 {
        if crate::mem::x64() {
            let v = crate::mem::r64(self.at);
            self.at += 8;
            return v;
        }
        let lo = self.u32() as u64;
        lo | (self.u32() as u64) << 32
    }
    pub fn f64(&mut self) -> f64 {
        f64::from_bits(self.u64())
    }
}

/// ANSI printf: `%s` is ANSI unless `wide` (then `%s` is UTF-16, as in swprintf).
pub fn format(fmt: &[u8], va: &mut VaArgs, wide: bool) -> Vec<u8> {
    w2a(&format_units(&a2w(fmt), va, !wide))
}

/// UTF-16 printf (wsprintfW/swprintf): `%s` is UTF-16, `%S` ANSI.
pub fn format_w(fmt: &[u16], va: &mut VaArgs) -> Vec<u16> {
    format_units(fmt, va, false)
}

fn format_units(f: &[u16], va: &mut VaArgs, ansi: bool) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(f.len() + 16);
    let mut i = 0;
    let ch = |i: usize| f.get(i).copied().unwrap_or(0);
    while i < f.len() {
        let c = f[i];
        if c != b'%' as u16 {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if ch(i) == b'%' as u16 {
            out.push(c);
            i += 1;
            continue;
        }
        let (mut left, mut plus, mut space, mut zero, mut alt) = (false, false, false, false, false);
        loop {
            match ch(i) as u8 {
                b'-' => left = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'0' => zero = true,
                b'#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        if ch(i) == b'*' as u16 {
            let w = va.u32() as i32;
            if w < 0 {
                left = true;
            }
            width = w.unsigned_abs() as usize;
            i += 1;
        } else {
            while (b'0' as u16..=b'9' as u16).contains(&ch(i)) {
                width = width * 10 + (ch(i) - b'0' as u16) as usize;
                i += 1;
            }
        }
        let mut prec: Option<usize> = None;
        if ch(i) == b'.' as u16 {
            i += 1;
            let mut p = 0usize;
            if ch(i) == b'*' as u16 {
                p = (va.u32() as i32).max(0) as usize;
                i += 1;
            } else {
                while (b'0' as u16..=b'9' as u16).contains(&ch(i)) {
                    p = p * 10 + (ch(i) - b'0' as u16) as usize;
                    i += 1;
                }
            }
            prec = Some(p);
        }
        // size
        let mut long64 = false;
        let mut short = false;
        let mut wide_arg: Option<bool> = None;
        loop {
            let c = ch(i) as u8;
            match c {
                b'h' => {
                    short = true;
                    wide_arg = Some(false);
                }
                b'l' => {
                    if ch(i + 1) == b'l' as u16 {
                        long64 = true;
                        i += 1;
                    } else {
                        wide_arg = Some(true);
                    }
                }
                b'w' => wide_arg = Some(true),
                b'L' | b'z' | b'j' | b't' => {}
                b'I' => {
                    if ch(i + 1) == b'6' as u16 && ch(i + 2) == b'4' as u16 {
                        long64 = true;
                        i += 2;
                    } else if ch(i + 1) == b'3' as u16 && ch(i + 2) == b'2' as u16 {
                        i += 2;
                    }
                }
                _ => break,
            }
            i += 1;
        }
        let ty = ch(i) as u8;
        i += 1;
        let mut body: Vec<u16>;
        let mut sign = String::new();
        let mut numeric = true;
        match ty {
            b'd' | b'i' => {
                let v = if long64 { va.u64() as i64 } else if short { va.u32() as i16 as i64 } else { va.u32() as i32 as i64 };
                if v < 0 {
                    sign = "-".into();
                } else if plus {
                    sign = "+".into();
                } else if space {
                    sign = " ".into();
                }
                body = num(v.unsigned_abs(), 10, false, prec);
            }
            b'u' | b'x' | b'X' | b'o' => {
                let v = if long64 { va.u64() } else if short { va.u32() as u16 as u64 } else { va.u32() as u64 };
                let base = match ty {
                    b'u' => 10,
                    b'o' => 8,
                    _ => 16,
                };
                body = num(v, base, ty == b'X', prec);
                if alt && v != 0 && base == 16 {
                    sign = if ty == b'X' { "0X".into() } else { "0x".into() };
                }
            }
            b'p' => {
                body = num(va.u32() as u64, 16, true, Some(8));
            }
            b'c' | b'C' => {
                numeric = false;
                let v = va.u32();
                let w = wide_arg.unwrap_or((ty == b'C') == ansi);
                body = if w { vec![v as u16] } else { a2w(&[v as u8]) };
            }
            b's' | b'S' => {
                numeric = false;
                let p = va.u32();
                let w = wide_arg.unwrap_or((ty == b'S') == ansi);
                body = if p == 0 {
                    "(null)".encode_utf16().collect()
                } else if w {
                    wstr(p)
                } else {
                    a2w(&cstr(p))
                };
                if let Some(pr) = prec {
                    body.truncate(pr);
                }
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
                let v = va.f64();
                if v.is_sign_negative() && !v.is_nan() {
                    sign = "-".into();
                } else if plus {
                    sign = "+".into();
                } else if space {
                    sign = " ".into();
                }
                let s = float(v.abs(), ty, prec.unwrap_or(6), alt);
                body = s.encode_utf16().collect();
            }
            b'n' => {
                let p = va.u32();
                put32(p, out.len() as u32);
                continue;
            }
            _ => {
                body = vec![b'%' as u16, ty as u16];
                numeric = false;
            }
        }
        let s: Vec<u16> = sign.encode_utf16().collect();
        let len = s.len() + body.len();
        if len < width {
            let pad = width - len;
            if left {
                out.extend(&s);
                out.extend(&body);
                out.extend(std::iter::repeat_n(b' ' as u16, pad));
            } else if zero && numeric && prec.is_none() {
                out.extend(&s);
                out.extend(std::iter::repeat_n(b'0' as u16, pad));
                out.extend(&body);
            } else {
                out.extend(std::iter::repeat_n(b' ' as u16, pad));
                out.extend(&s);
                out.extend(&body);
            }
        } else {
            out.extend(&s);
            out.extend(&body);
        }
    }
    out
}

fn num(v: u64, base: u64, upper: bool, prec: Option<usize>) -> Vec<u16> {
    let digits = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut d = Vec::new();
    let mut x = v;
    while x > 0 {
        d.push(digits[(x % base) as usize] as u16);
        x /= base;
    }
    let min = prec.unwrap_or(1);
    while d.len() < min {
        d.push(b'0' as u16);
    }
    d.reverse();
    d
}

fn float(v: f64, ty: u8, prec: usize, alt: bool) -> String {
    if v.is_nan() {
        return "1.#QNAN0".into();
    }
    if v.is_infinite() {
        return "1.#INF00".into();
    }
    let s = match ty {
        b'f' | b'F' => format!("{v:.prec$}"),
        b'e' | b'E' => exp(v, prec),
        _ => {
            let p = prec.max(1);
            let e = if v == 0.0 { 0 } else { v.log10().floor() as i32 };
            let mut s = if e < -4 || e >= p as i32 { exp(v, p - 1) } else { format!("{v:.*}", (p as i32 - 1 - e).max(0) as usize) };
            if !alt && s.contains('.') {
                let (m, x) = match s.find('e') {
                    Some(k) => (s[..k].to_string(), s[k..].to_string()),
                    None => (s.clone(), String::new()),
                };
                let m = m.trim_end_matches('0').trim_end_matches('.').to_string();
                s = m + &x;
            }
            s
        }
    };
    if ty.is_ascii_uppercase() {
        s.to_uppercase()
    } else {
        s
    }
}

fn exp(v: f64, prec: usize) -> String {
    let s = format!("{v:.prec$e}");
    // Rust: 1.5e3 -> msvcrt: 1.5e+003
    match s.split_once('e') {
        Some((m, e)) => {
            let n: i32 = e.parse().unwrap_or(0);
            format!("{m}e{}{:03}", if n < 0 { '-' } else { '+' }, n.abs())
        }
        None => s,
    }
}

// ---------------------------------------------------------------------------------------------
// the registry (in memory)

pub struct Key {
    pub path: String,
}

#[derive(Default)]
struct KeyData {
    /// value name (lower case) -> (name, type, data)
    values: Vec<(String, u32, Vec<u8>)>,
}

static REG: Mutex<Option<HashMap<String, KeyData>>> = Mutex::new(None);

fn with_reg<R>(f: impl FnOnce(&mut HashMap<String, KeyData>) -> R) -> R {
    let mut g = REG.lock().unwrap();
    let m = g.get_or_insert_with(|| {
        let mut m = HashMap::default();
        let mut set = |k: &str, n: &str, v: &str| {
            let e: &mut KeyData = m.entry(k.to_lowercase()).or_default();
            let mut data: Vec<u8> = v.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            data.extend([0, 0]);
            e.values.push((n.to_string(), 1, data));
        };
        set("hklm\\software\\microsoft\\windows nt\\currentversion", "CurrentVersion", "6.1");
        set("hklm\\software\\microsoft\\windows nt\\currentversion", "ProductName", "Microsoft Windows 7");
        set("hklm\\software\\microsoft\\windows\\currentversion", "ProgramFilesDir", "C:\\Program Files");
        set("hklm\\software\\microsoft\\directx", "Version", "4.09.00.0904");
        // stand-ins for the usual Windows faces, used by DirectWrite when they are missing (its
        // last-resort font is Tahoma)
        for (face, with) in [("Tahoma", "MS UI Gothic"), ("Segoe UI", "MS UI Gothic"), ("Microsoft Sans Serif", "MS UI Gothic"),
            ("Arial", "MS PGothic"), ("Meiryo", "MS PGothic"), ("Meiryo UI", "MS UI Gothic"), ("Yu Gothic", "MS PGothic"),
            ("Yu Gothic UI", "MS UI Gothic"), ("Times New Roman", "MS PMincho"), ("Yu Mincho", "MS PMincho"), ("Courier New", "MS Gothic")]
        {
            set("hkcu\\software\\wine\\fonts\\replacements", face, with);
        }
        // the installed fonts (DirectWrite's system collection reads this list)
        if let Some(dir) = crate::files::resolve("C:\\windows\\Fonts\\") {
            let mut files: Vec<String> = vnhost::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
            files.sort();
            for f in files.iter().filter(|f| [".ttf", ".ttc", ".otf"].iter().any(|x| f.to_ascii_lowercase().ends_with(x))) {
                let stem = f.rsplit_once('.').map_or(f.as_str(), |x| x.0);
                set("hklm\\software\\microsoft\\windows nt\\currentversion\\fonts", &format!("{stem} (TrueType)"), f);
            }
        }
        m.entry("hkcu\\software".into()).or_default();
        m
    });
    f(m)
}

fn root_name(h: u32) -> Option<&'static str> {
    Some(match h {
        0x8000_0000 => "hkcr",
        0x8000_0001 => "hkcu",
        0x8000_0002 => "hklm",
        0x8000_0003 => "hku",
        0x8000_0005 => "hkcc",
        _ => return None,
    })
}

fn key_path(h: u32) -> Option<String> {
    if let Some(r) = root_name(h) {
        return Some(r.into());
    }
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Key(k), _)) => Some(k.path.clone()),
        _ => None,
    })
}

fn join(base: &str, sub: &str) -> String {
    let sub = sub.trim_matches('\\').to_lowercase();
    if sub.is_empty() {
        base.into()
    } else {
        format!("{base}\\{sub}")
    }
}

fn open_key(h: u32, sub: &str, create: bool, out: u32) -> u32 {
    let Some(base) = key_path(h) else { return 6 };
    let path = join(&base, sub);
    if crate::env_on!("SAKE_TRACE_REG") {
        eprintln!("sake reg: open {path}");
    }
    let exists = with_reg(|m| {
        if m.contains_key(&path) || root_name(h).is_some() && sub.trim_matches('\\').is_empty() {
            return true;
        }
        if create {
            // create the parents too
            let mut p = String::new();
            for part in path.split('\\') {
                p = if p.is_empty() { part.into() } else { format!("{p}\\{part}") };
                m.entry(p.clone()).or_default();
            }
            return true;
        }
        // a key with subkeys exists even when it has no data of its own
        let pre = format!("{path}\\");
        m.keys().any(|k| k.starts_with(&pre))
    });
    if !exists {
        put32(out, 0);
        return 2;
    }
    put32(out, obj::add(Obj::Key(Key { path })));
    0
}

fn RegOpenKeyExA(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), false) };
    open_key(c.a(0), &s, false, c.a(4))
}
fn RegOpenKeyExW(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), true) };
    open_key(c.a(0), &s, false, c.a(4))
}
fn RegOpenKeyA(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), false) };
    open_key(c.a(0), &s, false, c.a(2))
}
fn RegOpenKeyW(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), true) };
    open_key(c.a(0), &s, false, c.a(2))
}
fn RegCreateKeyExA(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), false);
    put32(c.a(8), 1);
    open_key(c.a(0), &s, true, c.a(7))
}
fn RegCreateKeyExW(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), true);
    put32(c.a(8), 1);
    open_key(c.a(0), &s, true, c.a(7))
}
fn RegCreateKeyA(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), false) };
    open_key(c.a(0), &s, true, c.a(2))
}
fn RegCreateKeyW(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), true) };
    open_key(c.a(0), &s, true, c.a(2))
}
fn RegCloseKey(c: &mut Ctx) -> u32 {
    if root_name(c.a(0)).is_none() {
        obj::close(c.a(0));
    }
    0
}
fn RegFlushKey(_: &mut Ctx) -> u32 {
    0
}

fn is_str(ty: u32) -> bool {
    matches!(ty, 1 | 2 | 7)
}

fn query_value(c: &mut Ctx, key: u32, name: &str, pty: u32, data: u32, plen: u32, wide: bool) -> u32 {
    let Some(path) = key_path(key) else { return 6 };
    let lname = name.to_lowercase();
    let v = with_reg(|m| m.get(&path).and_then(|k| k.values.iter().find(|v| v.0.to_lowercase() == lname).map(|v| (v.1, v.2.clone()))));
    let _ = c;
    if crate::env_on!("SAKE_TRACE_REG") {
        eprintln!("sake reg: query {path}\\{name}: {:?}", v.as_ref().map(|x| x.1.len()));
    }
    let Some((ty, mut bytes)) = v else { return 2 };
    if is_str(ty) && !wide {
        let u: Vec<u16> = bytes.chunks(2).map(|b| u16::from_le_bytes([b[0], *b.get(1).unwrap_or(&0)])).collect();
        bytes = w2a(&u);
    }
    put32(pty, ty);
    let have = if plen != 0 { r32(plen) } else { 0 };
    put32(plen, bytes.len() as u32);
    if data == 0 {
        return 0;
    }
    if have < bytes.len() as u32 {
        return 234;
    }
    mem::write(data, &bytes);
    0
}
fn RegQueryValueExA(c: &mut Ctx) -> u32 {
    let name = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), false) };
    let (k, t, d, l) = (c.a(0), c.a(3), c.a(4), c.a(5));
    query_value(c, k, &name, t, d, l, false)
}
fn RegQueryValueExW(c: &mut Ctx) -> u32 {
    let name = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), true) };
    let (k, t, d, l) = (c.a(0), c.a(3), c.a(4), c.a(5));
    query_value(c, k, &name, t, d, l, true)
}
fn RegQueryValueA(c: &mut Ctx) -> u32 {
    let (k, sub, d, l) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut h = k;
    if sub != 0 && r8(sub) != 0 {
        let tmp = mem::heap_alloc(4, true);
        let r = open_key(k, &arg_str(sub, false), false, tmp);
        h = r32(tmp);
        mem::heap_free(tmp);
        if r != 0 {
            return r;
        }
    }
    query_value(c, h, "", 0, d, l, false)
}

fn set_value(key: u32, name: &str, ty: u32, data: u32, len: u32, wide: bool) -> u32 {
    let Some(path) = key_path(key) else { return 6 };
    let mut bytes = mem::slice(data, len).to_vec();
    if is_str(ty) && !wide {
        let mut u: Vec<u16> = a2w(bytes.split(|&b| b == 0).next().unwrap_or(&[]));
        u.push(0);
        bytes = u.iter().flat_map(|c| c.to_le_bytes()).collect();
    }
    with_reg(|m| {
        let k = m.entry(path).or_default();
        let lname = name.to_lowercase();
        k.values.retain(|v| v.0.to_lowercase() != lname);
        k.values.push((name.to_string(), ty, bytes));
    });
    0
}
fn RegSetValueExA(c: &mut Ctx) -> u32 {
    let name = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), false) };
    set_value(c.a(0), &name, c.a(3), c.a(4), c.a(5), false)
}
fn RegSetValueExW(c: &mut Ctx) -> u32 {
    let name = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), true) };
    set_value(c.a(0), &name, c.a(3), c.a(4), c.a(5), true)
}
fn RegSetValueA(c: &mut Ctx) -> u32 {
    let (k, ty, d, n) = (c.a(0), c.a(2), c.a(3), c.a(4));
    set_value(k, "", ty, d, n + 1, false)
}
fn delete_value(key: u32, name: &str) -> u32 {
    let Some(path) = key_path(key) else { return 6 };
    let lname = name.to_lowercase();
    with_reg(|m| {
        let Some(k) = m.get_mut(&path) else { return 2 };
        let n = k.values.len();
        k.values.retain(|v| v.0.to_lowercase() != lname);
        if k.values.len() == n {
            2
        } else {
            0
        }
    })
}
fn RegDeleteValueA(c: &mut Ctx) -> u32 {
    let name = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), false) };
    delete_value(c.a(0), &name)
}
fn RegDeleteValueW(c: &mut Ctx) -> u32 {
    let name = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), true) };
    delete_value(c.a(0), &name)
}
fn delete_key(key: u32, sub: &str) -> u32 {
    let Some(base) = key_path(key) else { return 6 };
    let p = join(&base, sub);
    with_reg(|m| if m.remove(&p).is_some() { 0 } else { 2 })
}
fn RegDeleteKeyA(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), false);
    delete_key(c.a(0), &s)
}
fn RegDeleteKeyW(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), true);
    delete_key(c.a(0), &s)
}

fn subkeys(path: &str) -> Vec<String> {
    let pre = format!("{path}\\");
    let mut v: Vec<String> = with_reg(|m| {
        m.keys().filter_map(|k| k.strip_prefix(&pre)).map(|r| r.split('\\').next().unwrap_or("").to_string()).collect()
    });
    v.sort();
    v.dedup();
    v
}
fn enum_key(c: &mut Ctx, wide: bool, ex: bool) -> u32 {
    let (k, i, name) = (c.a(0), c.a(1), c.a(2));
    let Some(path) = key_path(k) else { return 6 };
    let Some(s) = subkeys(&path).into_iter().nth(i as usize) else { return 259 };
    if ex {
        let pn = c.a(3);
        let cap = r32(pn);
        if (s.len() as u32) >= cap {
            return 234;
        }
        let n = ret_str(name, cap, &s, wide);
        w32(pn, n);
    } else {
        ret_str(name, c.a(3), &s, wide);
    }
    0
}
fn RegEnumKeyExA(c: &mut Ctx) -> u32 {
    enum_key(c, false, true)
}
fn RegEnumKeyExW(c: &mut Ctx) -> u32 {
    enum_key(c, true, true)
}
fn RegEnumKeyA(c: &mut Ctx) -> u32 {
    enum_key(c, false, false)
}
fn RegEnumKeyW(c: &mut Ctx) -> u32 {
    enum_key(c, true, false)
}
fn enum_value(c: &mut Ctx, wide: bool) -> u32 {
    let (k, i, name, pn, pty, data, plen) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(5), c.a(6), c.a(7));
    let Some(path) = key_path(k) else { return 6 };
    let v = with_reg(|m| m.get(&path).and_then(|kd| kd.values.get(i as usize).map(|v| v.0.clone())));
    let Some(vn) = v else { return 259 };
    let cap = r32(pn);
    if (vn.len() as u32) >= cap {
        return 234;
    }
    w32(pn, ret_str(name, cap, &vn, wide));
    query_value(c, k, &vn, pty, data, plen, wide)
}
fn RegEnumValueA(c: &mut Ctx) -> u32 {
    enum_value(c, false)
}
fn RegEnumValueW(c: &mut Ctx) -> u32 {
    enum_value(c, true)
}
fn query_info(c: &mut Ctx) -> u32 {
    let k = c.a(0);
    let Some(path) = key_path(k) else { return 6 };
    let subs = subkeys(&path);
    let (nv, maxn, maxd) = with_reg(|m| {
        m.get(&path).map_or((0, 0, 0), |kd| {
            (kd.values.len(), kd.values.iter().map(|v| v.0.len()).max().unwrap_or(0), kd.values.iter().map(|v| v.2.len()).max().unwrap_or(0))
        })
    });
    put32(c.a(1), 0);
    put32(c.a(2), 0);
    put32(c.a(4), subs.len() as u32);
    put32(c.a(5), subs.iter().map(|s| s.len()).max().unwrap_or(0) as u32);
    put32(c.a(6), 0);
    put32(c.a(7), nv as u32);
    put32(c.a(8), maxn as u32);
    put32(c.a(9), maxd as u32);
    put32(c.a(10), 0);
    if c.a(11) != 0 {
        // a fixed last-write time, not 0 (DirectWrite builds its font set only when the Fonts
        // key is newer than what it has: zero at first)
        w32(c.a(11), 0);
        w32(c.a(11) + 4, 0x01d6_0000);
    }
    0
}
fn RegQueryInfoKeyA(c: &mut Ctx) -> u32 {
    query_info(c)
}
fn RegQueryInfoKeyW(c: &mut Ctx) -> u32 {
    query_info(c)
}

// ---------------------------------------------------------------------------------------------
// version.dll

/// The RT_VERSION resource of `path` (a loaded module or the file).
fn version_resource(path: &str) -> Option<Vec<u8>> {
    let full = crate::files::full_path(path).to_lowercase();
    let name = full.rsplit('\\').next().unwrap_or("").to_string();
    let base = crate::pe::MODULES
        .read()
        .unwrap()
        .iter()
        .find(|m| m.path.to_lowercase() == full || (!path.contains('\\') && m.name == name))
        .map(|m| m.base);
    // a DLL we implement (no resources): a Japanese Windows 7 one (RealLive checks kernel32's
    // language)
    let ours = || name.strip_suffix(".dll").filter(|d| crate::api::is_system(d)).map(|_| system_version_info(&name));
    let Some(base) = base else { return ours() };
    let Some(e) = crate::pe::find_resource(base, &crate::pe::ResId::Int(16), &crate::pe::ResId::Int(1))
        .or_else(|| crate::pe::first_resource(base, 16))
    else {
        return ours();
    };
    let (p, n) = crate::pe::resource_data(base, e);
    Some(mem::slice(p, n).to_vec())
}

/// One VS_VERSIONINFO node: header, key, value (`text`: UTF-16 string), children.
fn ver_node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0u8; 6];
    for u in key.encode_utf16().chain([0]) {
        v.extend(u.to_le_bytes());
    }
    let pad = |v: &mut Vec<u8>| v.resize((v.len() + 3) & !3, 0);
    pad(&mut v);
    v.extend(value);
    for ch in children {
        pad(&mut v);
        v.extend(ch);
    }
    let vlen = if text { value.len() / 2 } else { value.len() };
    let len = v.len() as u16;
    v[0..2].copy_from_slice(&len.to_le_bytes());
    v[2..4].copy_from_slice(&(vlen as u16).to_le_bytes());
    v[4..6].copy_from_slice(&(text as u16).to_le_bytes());
    v
}
fn system_version_info(file: &str) -> Vec<u8> {
    let w = |s: &str| -> Vec<u8> { s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect() };
    // VS_FIXEDFILEINFO, version 6.1.7601.17514
    let mut fixed = Vec::new();
    for d in [0xfeef04bdu32, 0x10000, 0x60001, 7601 << 16 | 17514, 0x60001, 7601 << 16 | 17514, 0x3f, 0, 0x40004, 2, 0, 0, 0] {
        fixed.extend(d.to_le_bytes());
    }
    let strings = ver_node(
        "041104b0",
        &[],
        true,
        &[
            ver_node("CompanyName", &w("Microsoft Corporation"), true, &[]),
            ver_node("FileVersion", &w("6.1.7601.17514"), true, &[]),
            ver_node("OriginalFilename", &w(file), true, &[]),
            ver_node("ProductVersion", &w("6.1.7601.17514"), true, &[]),
        ],
    );
    let sfi = ver_node("StringFileInfo", &[], true, &[strings]);
    let tr = ver_node("Translation", &[0x11, 0x04, 0xb0, 0x04], false, &[]);
    let vfi = ver_node("VarFileInfo", &[], true, &[tr]);
    ver_node("VS_VERSION_INFO", &fixed, false, &[sfi, vfi])
}

fn GetFileVersionInfoSizeA(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    version_resource(&arg_str(c.a(0), false)).map_or(0, |v| v.len() as u32 * 2)
}
fn GetFileVersionInfoSizeW(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    version_resource(&arg_str(c.a(0), true)).map_or(0, |v| v.len() as u32 * 2)
}
fn get_version_info(path: &str, len: u32, out: u32) -> u32 {
    let Some(v) = version_resource(path) else { return 0 };
    mem::fill(out, len, 0);
    let n = (v.len() as u32).min(len);
    mem::write(out, &v[..n as usize]);
    1
}
fn GetFileVersionInfoA(c: &mut Ctx) -> u32 {
    get_version_info(&arg_str(c.a(0), false), c.a(2), c.a(3))
}
fn GetFileVersionInfoW(c: &mut Ctx) -> u32 {
    get_version_info(&arg_str(c.a(0), true), c.a(2), c.a(3))
}

/// Finds `sub` ("\\StringFileInfo\\041104b0\\FileVersion") in a VS_VERSIONINFO block:
/// (value address, value length in its units, is text).
fn ver_find(block: u32, sub: &str) -> Option<(u32, u32, bool)> {
    let parts: Vec<String> = sub.split('\\').filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect();
    let mut node = block;
    for part in &parts {
        let len = r16(node) as u32;
        let vlen = r16(node + 2) as u32;
        let ty = r16(node + 4);
        let key_len = wstr(node + 6).len() as u32;
        let mut child = (node + 6 + 2 * (key_len + 1) + 3) & !3;
        child = (child + if ty == 1 { 2 * vlen } else { vlen } + 3) & !3;
        let end = node + len;
        let mut found = None;
        while child + 6 < end {
            let clen = r16(child) as u32;
            if clen == 0 {
                break;
            }
            let k = String::from_utf16_lossy(&wstr(child + 6)).to_lowercase();
            if &k == part {
                found = Some(child);
                break;
            }
            child = (child + clen + 3) & !3;
        }
        node = found?;
    }
    let vlen = r16(node + 2) as u32;
    let ty = r16(node + 4);
    let key_len = wstr(node + 6).len() as u32;
    let val = (node + 6 + 2 * (key_len + 1) + 3) & !3;
    Some((val, vlen, ty == 1))
}

fn ver_query(c: &mut Ctx, wide: bool) -> u32 {
    let (block, sub, pp, plen) = (c.a(0), arg_str(c.a(1), wide), c.a(2), c.a(3));
    let Some((val, n, text)) = ver_find(block, &sub) else { return 0 };
    if text && !wide {
        // the ANSI copy goes in the second half of the buffer (sized twice the resource)
        let total = r16(block) as u32;
        let at = block + total + (val - block) / 2;
        let s: Vec<u16> = wstr(val);
        let a = w2a(&s);
        mem::write(at, &a);
        mem::w8(at + a.len() as u32, 0);
        put32(pp, at);
        put32(plen, a.len() as u32 + 1);
    } else {
        put32(pp, val);
        put32(plen, n);
    }
    1
}
fn VerQueryValueA(c: &mut Ctx) -> u32 {
    ver_query(c, false)
}
fn VerQueryValueW(c: &mut Ctx) -> u32 {
    ver_query(c, true)
}

// ---------------------------------------------------------------------------------------------
// ole32 / oleaut32

fn CoInitialize(_: &mut Ctx) -> u32 {
    0
}
fn CoInitializeEx(_: &mut Ctx) -> u32 {
    0
}
fn OleInitialize(_: &mut Ctx) -> u32 {
    0
}
fn CoUninitialize(_: &mut Ctx) -> u32 {
    0
}
fn OleUninitialize(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// dxdiagn: IDxDiagProvider / IDxDiagContainer with what games check (the DirectX version,
// the OS, the display)

#[derive(Clone)]
enum DxProp {
    U4(u32),
    U8(u64),
    Str(&'static str),
    Bool(bool),
}

/// node path -> (children, properties)
fn dx_node(path: &str) -> Option<(Vec<&'static str>, Vec<(&'static str, DxProp)>)> {
    use DxProp::*;
    let (sw, sh) = crate::user::screen();
    Some(match path {
        "" => (vec!["DxDiag_SystemInfo", "DxDiag_DisplayDevices", "DxDiag_DirectSound"], vec![]),
        "DxDiag_SystemInfo" => (
            vec![],
            vec![
                ("dwDirectXVersionMajor", U4(9)),
                ("dwDirectXVersionMinor", U4(0)),
                ("szDirectXVersionLetter", Str("c")),
                ("szDirectXVersionEnglish", Str("4.09.0000.0904")),
                ("szDirectXVersionLongEnglish", Str("DirectX 9.0c")),
                ("dwOSMajorVersion", U4(6)),
                ("dwOSMinorVersion", U4(1)),
                ("dwOSBuildNumber", U4(7601)),
                ("dwOSPlatformID", U4(2)),
                ("ullPhysicalMemory", U8(4 << 30)),
                ("ullAvailPhysMem", U8(3 << 30)),
                ("szPhysicalMemoryEnglish", Str("4096MB RAM")),
                ("szProcessorEnglish", Str("vnvm x86")),
                ("szOSEnglish", Str("Windows 7")),
                ("szOSExEnglish", Str("Windows 7 Professional 32-bit (6.1, Build 7601)")),
                ("szLanguagesEnglish", Str("Japanese")),
                ("bDebug", Bool(false)),
                ("bNECPC98", Bool(false)),
            ],
        ),
        "DxDiag_DisplayDevices" => (vec!["0"], vec![]),
        "DxDiag_DisplayDevices.0" => (
            vec![],
            vec![
                ("szDescription", Str("vnvm WebGPU")),
                ("szDeviceName", Str("\\\\.\\DISPLAY1")),
                ("szManufacturer", Str("vnvm")),
                ("szChipType", Str("WebGPU")),
                ("szDisplayMemoryEnglish", Str("512.0 MB")),
                ("szDisplayMemoryLocalized", Str("512.0 MB")),
                ("dwWidth", U4(sw as u32)),
                ("dwHeight", U4(sh as u32)),
                ("dwBpp", U4(32)),
                ("dwRefreshRate", U4(60)),
                ("szDriverVersion", Str("9.18.13.0000")),
                ("bDDAccelerationEnabled", Bool(true)),
                ("b3DAccelerationExists", Bool(true)),
                ("b3DAccelerationEnabled", Bool(true)),
                ("dwVendorId", U4(0x10de)),
                ("dwDeviceId", U4(0x0a20)),
            ],
        ),
        "DxDiag_DirectSound" => (vec![], vec![]),
        _ => return None,
    })
}

static DX_NODES: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

fn dx_container(path: &str) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        let m: &[(crate::api::Handler, u32)] = &[
            (dx_qi, 12),
            (dx_addref, 4),
            (dx_addref, 4),
            (dxc_num_children, 8),
            (dxc_enum_children, 16),
            (dxc_get_child, 12),
            (dxc_num_props, 8),
            (dxc_enum_props, 16),
            (dxc_get_prop, 12),
        ];
        let v = mem::heap_alloc(4 * m.len() as u32, true);
        for (i, (f, b)) in m.iter().enumerate() {
            w32(v + 4 * i as u32, crate::api::host_fn(&format!("IDxDiagContainer::{i}"), *f, *b));
        }
        v
    });
    let o = mem::heap_alloc(8, true);
    w32(o, vt);
    DX_NODES.lock().unwrap().get_or_insert_with(HashMap::default).insert(o, path.to_string());
    o
}
fn dx_path(o: u32) -> String {
    DX_NODES.lock().unwrap().get_or_insert_with(HashMap::default).get(&o).cloned().unwrap_or_default()
}
fn dx_qi(c: &mut Ctx) -> u32 {
    put32(c.a(2), c.a(0));
    0
}
fn dx_addref(_: &mut Ctx) -> u32 {
    1
}
fn dxp_initialize(_: &mut Ctx) -> u32 {
    0
}
fn dxp_root(c: &mut Ctx) -> u32 {
    put32(c.a(1), dx_container(""));
    0
}
fn dxc_num_children(c: &mut Ctx) -> u32 {
    let n = dx_node(&dx_path(c.a(0))).map_or(0, |n| n.0.len());
    put32(c.a(1), n as u32);
    0
}
fn put_wname(buf: u32, cch: u32, s: &str) -> u32 {
    let u: Vec<u16> = s.encode_utf16().collect();
    if (u.len() as u32) >= cch {
        return 0x8007_007a;
    }
    mem::put_wstr(buf, cch, &u);
    0
}
fn dxc_enum_children(c: &mut Ctx) -> u32 {
    let Some((ch, _)) = dx_node(&dx_path(c.a(0))) else { return 0x8007_0057 };
    match ch.get(c.a(1) as usize) {
        Some(n) => put_wname(c.a(2), c.a(3), n),
        None => 0x8007_0057,
    }
}
fn dxc_get_child(c: &mut Ctx) -> u32 {
    let base = dx_path(c.a(0));
    let name = String::from_utf16_lossy(&wstr(c.a(1)));
    let path = if base.is_empty() { name } else { format!("{base}.{name}") };
    if dx_node(&path).is_none() {
        put32(c.a(2), 0);
        return 0x8007_0057;
    }
    put32(c.a(2), dx_container(&path));
    0
}
fn dxc_num_props(c: &mut Ctx) -> u32 {
    let n = dx_node(&dx_path(c.a(0))).map_or(0, |n| n.1.len());
    put32(c.a(1), n as u32);
    0
}
fn dxc_enum_props(c: &mut Ctx) -> u32 {
    let Some((_, props)) = dx_node(&dx_path(c.a(0))) else { return 0x8007_0057 };
    match props.get(c.a(1) as usize) {
        Some((n, _)) => put_wname(c.a(2), c.a(3), n),
        None => 0x8007_0057,
    }
}
fn dxc_get_prop(c: &mut Ctx) -> u32 {
    let name = String::from_utf16_lossy(&wstr(c.a(1)));
    let v = c.a(2);
    let Some((_, props)) = dx_node(&dx_path(c.a(0))) else { return 0x8007_0057 };
    let Some((_, p)) = props.iter().find(|(n, _)| *n == name) else { return 0x8007_0057 };
    mem::fill(v, 16, 0);
    match p {
        DxProp::U4(x) => {
            w16(v, 19);
            w32(v + 8, *x);
        }
        DxProp::U8(x) => {
            w16(v, 21);
            w32(v + 8, *x as u32);
            w32(v + 12, (*x >> 32) as u32);
        }
        DxProp::Bool(b) => {
            w16(v, 11);
            w16(v + 8, if *b { 0xffff } else { 0 });
        }
        DxProp::Str(st) => {
            w16(v, 8);
            w32(v + 8, bstr_alloc(&st.encode_utf16().collect::<Vec<u16>>()));
        }
    }
    0
}

/// CLSID_DxDiagProvider
fn dxdiag_provider() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        let m: &[(crate::api::Handler, u32)] = &[(dx_qi, 12), (dx_addref, 4), (dx_addref, 4), (dxp_initialize, 8), (dxp_root, 8)];
        let v = mem::heap_alloc(4 * m.len() as u32, true);
        for (i, (f, b)) in m.iter().enumerate() {
            w32(v + 4 * i as u32, crate::api::host_fn(&format!("IDxDiagProvider::{i}"), *f, *b));
        }
        v
    });
    let o = mem::heap_alloc(8, true);
    w32(o, vt);
    o
}

fn CoCreateInstance(c: &mut Ctx) -> u32 {
    let clsid = c.a(0);
    let iid = c.a(3);
    let g = |p: u32| -> String {
        if p == 0 {
            return "null".into();
        }
        format!("{:08x}-{:04x}-{:04x}", r32(p), r16(p + 4), r16(p + 6))
    };
    if r32(clsid) == 0xa65b_8071 {
        put32(c.a(4), dxdiag_provider());
        return 0;
    }
    if let Some(obj) = crate::ddraw::create_instance(clsid, iid) {
        mem::putp(c.a(4), obj);
        return 0;
    }
    if let Some(obj) = crate::dshow::create_instance(clsid, iid) {
        mem::putp(c.a(4), obj);
        return 0;
    }
    if let Some(obj) = crate::dinput::create_instance(clsid) {
        mem::putp(c.a(4), obj);
        return 0;
    }
    if let Some(obj) = crate::com::create_instance(clsid) {
        mem::putp(c.a(4), obj);
        return 0;
    }
    if let Some(obj) = crate::xaudio2::create_instance(clsid) {
        put32(c.a(4), obj);
        return 0;
    }
    if let Some(obj) = crate::sound::create_instance(c, clsid, iid) {
        mem::putp(c.a(4), obj);
        return 0;
    }
    eprintln!("sake: CoCreateInstance {{{}}} iid {{{}}}: not registered", g(clsid), g(iid));
    mem::putp(c.a(4), 0);
    0x8004_0154
}
fn CoTaskMemAlloc(c: &mut Ctx) -> u32 {
    mem::heap_alloc(c.a(0), false)
}
fn CoTaskMemRealloc(c: &mut Ctx) -> u32 {
    mem::heap_realloc(c.a(0), c.a(1), false)
}
fn CoTaskMemFree(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}
/// The task allocator as an IMalloc object (one, shared).
fn malloc_obj() -> u32 {
    static O: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *O.get_or_init(|| {
        fn qi(c: &mut Ctx) -> u32 {
            put32(c.a(2), c.a(0));
            0
        }
        fn one(_: &mut Ctx) -> u32 {
            1
        }
        fn alloc(c: &mut Ctx) -> u32 {
            mem::heap_alloc(c.a(1), false)
        }
        fn realloc(c: &mut Ctx) -> u32 {
            mem::heap_realloc(c.a(1), c.a(2), false)
        }
        fn free(c: &mut Ctx) -> u32 {
            if c.a(1) != 0 {
                mem::heap_free(c.a(1));
            }
            0
        }
        fn size(c: &mut Ctx) -> u32 {
            if c.a(1) == 0 { u32::MAX } else { mem::heap_size(c.a(1)) }
        }
        fn did(c: &mut Ctx) -> u32 {
            mem::heap_owns(c.a(1)) as u32
        }
        fn nothing(_: &mut Ctx) -> u32 {
            0
        }
        let m: [(crate::api::Handler, u32); 9] =
            [(qi, 12), (one, 4), (one, 4), (alloc, 8), (realloc, 12), (free, 8), (size, 8), (did, 8), (nothing, 4)];
        let v = mem::heap_alloc(4 * m.len() as u32, true);
        for (i, (f, n)) in m.iter().enumerate() {
            w32(v + 4 * i as u32, crate::api::host_fn(&format!("IMalloc::{i}"), *f, *n));
        }
        let o = mem::heap_alloc(4, true);
        w32(o, v);
        o
    })
}
fn IsUserAnAdmin(_: &mut Ctx) -> u32 {
    1
}
fn SHGetMalloc(c: &mut Ctx) -> u32 {
    put32(c.a(0), malloc_obj());
    0
}
fn CoGetMalloc(c: &mut Ctx) -> u32 {
    put32(c.a(1), malloc_obj());
    0
}
fn CoCreateGuid(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    static SEED: AtomicU32 = AtomicU32::new(0x1234_5678);
    for i in 0..4 {
        let mut x = SEED.fetch_add(0x9e37_79b9, Ordering::Relaxed) ^ crate::k32::ticks_ms();
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        w32(p + 4 * i, x);
    }
    0
}
fn IsEqualGUID(c: &mut Ctx) -> u32 {
    (mem::slice(c.a(0), 16) == mem::slice(c.a(1), 16)) as u32
}
fn guid_string(p: u32) -> String {
    let b = mem::slice(p + 8, 8);
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        r32(p),
        r16(p + 4),
        r16(p + 6),
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7]
    )
}
fn StringFromGUID2(c: &mut Ctx) -> u32 {
    let (g, out, n) = (c.a(0), c.a(1), c.a(2));
    let s: Vec<u16> = guid_string(g).encode_utf16().collect();
    if (n as usize) <= s.len() {
        return 0;
    }
    mem::put_wstr(out, n, &s) + 1
}
/// CLSIDFromString / IIDFromString: "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}" (ProgIDs are not
/// registered: CO_E_CLASSSTRING).
fn guid_from_string(c: &mut Ctx) -> u32 {
    let (s, out) = (c.a(0), c.a(1));
    let t: String = crate::k32::arg_str(s, true).chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
    if t.len() != 32 || !crate::k32::arg_str(s, true).starts_with('{') {
        mem::fill(out, 16, 0);
        return 0x8004_01f3;
    }
    let hex = |a: usize, n: usize| u32::from_str_radix(&t[a..a + n], 16).unwrap_or(0);
    w32(out, hex(0, 8));
    w16(out + 4, hex(8, 4) as u16);
    w16(out + 6, hex(12, 4) as u16);
    for i in 0..8 {
        mem::w8(out + 8 + i as u32, hex(16 + 2 * i, 2) as u8);
    }
    0
}
fn CLSIDFromString(c: &mut Ctx) -> u32 {
    guid_from_string(c)
}
fn IIDFromString(c: &mut Ctx) -> u32 {
    guid_from_string(c)
}
fn RegisterDragDrop(_: &mut Ctx) -> u32 {
    0
}
fn RevokeDragDrop(_: &mut Ctx) -> u32 {
    0
}

fn bstr_alloc(chars: &[u16]) -> u32 {
    let p = mem::heap_alloc(4 + 2 * chars.len() as u32 + 2, true);
    w32(p, 2 * chars.len() as u32);
    for (i, &c) in chars.iter().enumerate() {
        w16(p + 4 + 2 * i as u32, c);
    }
    p + 4
}
fn bstr_free(b: u32) {
    if b != 0 {
        mem::heap_free(b - 4);
    }
}
fn SysAllocString(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return 0;
    }
    bstr_alloc(&wstr(c.a(0)))
}
fn SysAllocStringLen(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let v: Vec<u16> = if p == 0 { vec![0; n as usize] } else { (0..n).map(|i| r16(p + 2 * i)).collect() };
    bstr_alloc(&v)
}
fn SysAllocStringByteLen(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let b = mem::heap_alloc(4 + n + 2, true);
    w32(b, n);
    if p != 0 {
        mem::copy(b + 4, p, n);
    }
    b + 4
}
fn SysReAllocString(c: &mut Ctx) -> u32 {
    let (pb, s) = (c.a(0), c.a(1));
    let nb = bstr_alloc(&wstr(s));
    bstr_free(r32(pb));
    w32(pb, nb);
    1
}
fn SysReAllocStringLen(c: &mut Ctx) -> u32 {
    let (pb, s, n) = (c.a(0), c.a(1), c.a(2));
    let v: Vec<u16> = if s == 0 { vec![0; n as usize] } else { (0..n).map(|i| r16(s + 2 * i)).collect() };
    let nb = bstr_alloc(&v);
    bstr_free(r32(pb));
    w32(pb, nb);
    1
}
fn SysFreeString(c: &mut Ctx) -> u32 {
    bstr_free(c.a(0));
    0
}
fn SysStringLen(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        0
    } else {
        r32(c.a(0) - 4) / 2
    }
}
fn SysStringByteLen(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        0
    } else {
        r32(c.a(0) - 4)
    }
}
fn VariantInit(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), 16, 0);
    0
}
fn VariantClear(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if r16(v) == 8 {
        bstr_free(r32(v + 8));
    }
    mem::fill(v, 16, 0);
    0
}
fn variant_copy(dst: u32, src: u32) -> u32 {
    let mut s = src;
    if r16(s) & 0x4000 != 0 {
        // VT_BYREF: copy what it points at
        let ty = r16(s) & !0x4000;
        let p = r32(s + 8);
        let tmp = [0u8; 16];
        mem::write(dst, &tmp);
        w16(dst, ty);
        match ty {
            8 => w32(dst + 8, bstr_alloc(&wstr(r32(p)))),
            5 | 7 | 20 | 21 => mem::copy(dst + 8, p, 8),
            12 => mem::copy(dst, p, 16),
            _ => mem::copy(dst + 8, p, 4),
        }
        return 0;
    }
    if dst != s {
        mem::copy(dst, s, 16);
        if r16(s) == 8 && r32(s + 8) != 0 {
            let b = r32(s + 8);
            let n = r32(b - 4) / 2;
            let v: Vec<u16> = (0..n).map(|i| r16(b + 2 * i)).collect();
            w32(dst + 8, bstr_alloc(&v));
        }
    }
    s = 0;
    let _ = s;
    0
}
fn VariantCopy(c: &mut Ctx) -> u32 {
    variant_copy(c.a(0), c.a(1))
}
fn VariantCopyInd(c: &mut Ctx) -> u32 {
    variant_copy(c.a(0), c.a(1))
}
fn VariantChangeTypeEx(c: &mut Ctx) -> u32 {
    let (dst, src, vt) = (c.a(0), c.a(1), c.a(4) as u16);
    if r16(src) == vt {
        return variant_copy(dst, src);
    }
    0x8002_0005
}
fn VariantChangeType(c: &mut Ctx) -> u32 {
    let (dst, src, vt) = (c.a(0), c.a(1), c.a(3) as u16);
    if r16(src) == vt {
        return variant_copy(dst, src);
    }
    0x8002_0005
}
fn GetErrorInfo(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    1
}
fn SetErrorInfo(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// shell32

fn special_folder(csidl: u32) -> Option<&'static str> {
    Some(match csidl & 0xff {
        0 | 0x10 => "C:\\users\\user\\Desktop",
        5 => "C:\\users\\user\\Documents",
        0x1a => "C:\\users\\user\\AppData\\Roaming",
        0x1c => "C:\\users\\user\\AppData\\Local",
        0x23 => "C:\\ProgramData",
        0x2e => "C:\\users\\Public\\Documents",
        0x14 => "C:\\windows\\Fonts",
        0x24 => "C:\\windows",
        0x25 | 0x29 => "C:\\windows\\system32",
        0x26 | 0x2a => "C:\\Program Files",
        0x28 => "C:\\users\\user",
        0x20 => "C:\\users\\user\\AppData\\Local\\Microsoft\\Windows\\Temporary Internet Files",
        0x21 => "C:\\users\\user\\AppData\\Local\\Microsoft\\Windows\\INetCookies",
        0x22 => "C:\\users\\user\\AppData\\Local\\Microsoft\\Windows\\History",
        0x27 => "C:\\users\\user\\Pictures",
        0x0d => "C:\\users\\user\\Music",
        0x0e => "C:\\users\\user\\Videos",
        0x02 | 0x07 | 0x0b | 0x15 | 0x08 | 0x06 | 0x09 | 0x13 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows",
        _ => return None,
    })
}

fn folder_path(csidl: u32, create: bool) -> Option<String> {
    let p = special_folder(csidl)?;
    if create || csidl & 0x8000 != 0 || true {
        let _ = vnhost::fs::create_dir_all(crate::files::to_host(p));
    }
    Some(p.to_string())
}

/// SHCreateDirectoryEx: the folder and any missing parents (ERROR_ALREADY_EXISTS if there).
fn sh_mkdir(path: &str) -> u32 {
    if crate::files::resolve(path).is_some() {
        return 183;
    }
    let h = crate::files::resolve_new(path);
    if vnhost::fs::create_dir_all(&h).is_ok() { 0 } else { 3 }
}
fn SHCreateDirectoryExA(c: &mut Ctx) -> u32 {
    sh_mkdir(&arg_str(c.a(1), false))
}
fn SHCreateDirectoryExW(c: &mut Ctx) -> u32 {
    sh_mkdir(&arg_str(c.a(1), true))
}
fn SHCreateDirectory(c: &mut Ctx) -> u32 {
    sh_mkdir(&arg_str(c.a(1), true))
}
fn SHGetSpecialFolderPathA(c: &mut Ctx) -> u32 {
    let (out, csidl, create) = (c.a(1), c.a(2), c.a(3));
    match folder_path(csidl, create != 0) {
        Some(p) => {
            ret_str(out, 260, &p, false);
            1
        }
        None => 0,
    }
}
fn SHGetSpecialFolderPathW(c: &mut Ctx) -> u32 {
    let (out, csidl, create) = (c.a(1), c.a(2), c.a(3));
    match folder_path(csidl, create != 0) {
        Some(p) => {
            ret_str(out, 260, &p, true);
            1
        }
        None => 0,
    }
}
fn SHGetFolderPathA(c: &mut Ctx) -> u32 {
    let (csidl, out) = (c.a(1), c.a(4));
    match folder_path(csidl, true) {
        Some(p) => {
            ret_str(out, 260, &p, false);
            0
        }
        None => 0x8000_4005,
    }
}
/// SHGetKnownFolderPath(rfid, flags, token, PWSTR *out): the known folders that have a CSIDL.
fn SHGetKnownFolderPath(c: &mut Ctx) -> u32 {
    let (id, out) = (c.a(0), c.a(3));
    let path = match r32(id) {
        0x3eb6_85db => folder_path(0x1a, true),
        0xf1b3_2785 | 0xa520_a1a4 => folder_path(0x1c, true),
        0xfdd3_9ad0 => folder_path(5, true),
        0x62ab_5d82 => folder_path(0x23, true),
        0xb4bf_cc3a => folder_path(0x10, true),
        0x5e6c_858f => folder_path(0x28, true),
        0xfd22_8cb7 => folder_path(0x14, true),
        0x1ac1_4e77 => folder_path(0x25, true),
        0xf38b_f404 => folder_path(0x24, true),
        0x905e_63b6 => folder_path(0x26, true),
        0x33e2_8130 => folder_path(0x27, true),
        // Saved Games
        0x4c5c_32ff => folder_path(0x28, true).map(|p| {
            let p = format!("{p}\\Saved Games");
            let _ = vnhost::fs::create_dir_all(crate::files::to_host(&p));
            p
        }),
        _ => None,
    };
    let Some(p) = path else {
        put32(out, 0);
        return 0x8007_0002;
    };
    let w: Vec<u16> = p.encode_utf16().chain([0]).collect();
    let b = mem::heap_alloc(2 * w.len() as u32, false);
    for (i, &u) in w.iter().enumerate() {
        w16(b + 2 * i as u32, u);
    }
    put32(out, b);
    0
}
fn SHGetFolderPathW(c: &mut Ctx) -> u32 {
    let (csidl, out) = (c.a(1), c.a(4));
    match folder_path(csidl, true) {
        Some(p) => {
            ret_str(out, 260, &p, true);
            0
        }
        None => 0x8000_4005,
    }
}
fn SHGetSpecialFolderLocation(c: &mut Ctx) -> u32 {
    let (csidl, out) = (c.a(1), c.a(2));
    // an ID list of our own: a marker and the CSIDL, then the terminator
    let p = mem::heap_alloc(12, true);
    w16(p, 8);
    w16(p + 2, 0x7676);
    w32(p + 4, csidl);
    put32(out, p);
    0
}
fn SHGetFolderLocation(c: &mut Ctx) -> u32 {
    let (csidl, out) = (c.a(1), c.a(4));
    let p = mem::heap_alloc(12, true);
    w16(p, 8);
    w16(p + 2, 0x7676);
    w32(p + 4, csidl);
    put32(out, p);
    0
}
fn pidl_path(p: u32) -> Option<String> {
    if p == 0 || r16(p + 2) != 0x7676 {
        return None;
    }
    folder_path(r32(p + 4), false)
}
fn SHGetPathFromIDListA(c: &mut Ctx) -> u32 {
    match pidl_path(c.a(0)) {
        Some(s) => {
            ret_str(c.a(1), 260, &s, false);
            1
        }
        None => 0,
    }
}
fn SHGetPathFromIDListW(c: &mut Ctx) -> u32 {
    match pidl_path(c.a(0)) {
        Some(s) => {
            ret_str(c.a(1), 260, &s, true);
            1
        }
        None => 0,
    }
}
fn ILFree(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}
fn ShellExecuteA(c: &mut Ctx) -> u32 {
    eprintln!("sake: ShellExecute {} {}", arg_str(c.a(1), false), arg_str(c.a(2), false));
    42
}
fn ShellExecuteW(c: &mut Ctx) -> u32 {
    eprintln!("sake: ShellExecute {} {}", arg_str(c.a(1), true), arg_str(c.a(2), true));
    42
}
fn ShellExecuteExA(_: &mut Ctx) -> u32 {
    1
}
fn ShellExecuteExW(_: &mut Ctx) -> u32 {
    1
}
fn DragAcceptFiles(_: &mut Ctx) -> u32 {
    0
}
fn DragFinish(_: &mut Ctx) -> u32 {
    0
}
fn DragQueryFileA(_: &mut Ctx) -> u32 {
    0
}
fn DragQueryFileW(_: &mut Ctx) -> u32 {
    0
}
fn CommandLineToArgvW(c: &mut Ctx) -> u32 {
    let s = String::from_utf16_lossy(&wstr(c.a(0)));
    let mut args: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut q, mut any) = (false, false);
    for ch in s.chars() {
        match ch {
            '"' => {
                q = !q;
                any = true;
            }
            ' ' | '\t' if !q => {
                if any || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            _ => cur.push(ch),
        }
    }
    if any || !cur.is_empty() {
        args.push(cur);
    }
    let n = args.len() as u32;
    let size: u32 = 4 * (n + 1) + args.iter().map(|a| 2 * (a.encode_utf16().count() as u32 + 1)).sum::<u32>();
    let p = mem::heap_alloc(size, true);
    let mut at = p + 4 * (n + 1);
    for (i, a) in args.iter().enumerate() {
        let u: Vec<u16> = a.encode_utf16().collect();
        w32(p + 4 * i as u32, at);
        mem::put_wstr(at, u.len() as u32 + 1, &u);
        at += 2 * (u.len() as u32 + 1);
    }
    put32(c.a(1), n);
    p
}

// ---------------------------------------------------------------------------------------------
// comctl32

static IMAGELISTS: AtomicU32 = AtomicU32::new(0x0a7e_0000);

fn InitCommonControls(_: &mut Ctx) -> u32 {
    0
}
fn InitCommonControlsEx(_: &mut Ctx) -> u32 {
    1
}
fn ImageList_Create(_: &mut Ctx) -> u32 {
    IMAGELISTS.fetch_add(4, Ordering::Relaxed)
}
/// Reads from an IStream (IStream::Read); whether all `n` bytes came.
fn stream_read(c: &mut Ctx, stream: u32, buf: u32, n: u32) -> bool {
    let read = r32(r32(stream) + 12);
    let got = mem::heap_alloc(4, true);
    let hr = c.call(read, &[stream, buf, n, got]);
    let k = r32(got);
    mem::heap_free(got);
    hr == 0 && k == n
}

/// Skips one bitmap of an image list stream (BITMAPFILEHEADER, BITMAPINFOHEADER, colors, bits).
fn skip_bitmap(c: &mut Ctx, stream: u32, buf: u32) -> bool {
    if !stream_read(c, stream, buf, 14 + 40) || r16(buf) != 0x4d42 {
        return false;
    }
    let bi = buf + 14;
    let (w, h, bpp) = (r32(bi + 4), (r32(bi + 8) as i32).unsigned_abs(), r16(bi + 14) as u32);
    let pal = if bpp <= 8 { (1u32 << bpp) * 4 } else { 0 };
    let bits = h * ((w * bpp + 31) / 32) * 4;
    let rest = pal + bits;
    let tmp = mem::heap_alloc(rest.max(4), false);
    let ok = stream_read(c, stream, tmp, rest);
    mem::heap_free(tmp);
    ok
}

fn ImageList_Read(c: &mut Ctx) -> u32 {
    let stream = c.a(0);
    let buf = mem::heap_alloc(64, true);
    let ok = stream_read(c, stream, buf, 28) && r16(buf) == 0x4c49 && {
        let flags = r16(buf + 18);
        skip_bitmap(c, stream, buf) && (flags & 1 == 0 || skip_bitmap(c, stream, buf))
    };
    mem::heap_free(buf);
    if ok {
        IMAGELISTS.fetch_add(4, Ordering::Relaxed)
    } else {
        0
    }
}
fn ImageList_Destroy(_: &mut Ctx) -> u32 {
    1
}
fn ImageList_Add(_: &mut Ctx) -> u32 {
    0
}
fn ImageList_GetImageCount(_: &mut Ctx) -> u32 {
    0
}
fn ImageList_SetBkColor(_: &mut Ctx) -> u32 {
    0xffff_ffff
}
fn ImageList_GetBkColor(_: &mut Ctx) -> u32 {
    0xffff_ffff
}
fn ImageList_GetIconSize(c: &mut Ctx) -> u32 {
    put32(c.a(1), 16);
    put32(c.a(2), 16);
    1
}
fn ImageList_SetIconSize(_: &mut Ctx) -> u32 {
    1
}
fn ImageList_Remove(_: &mut Ctx) -> u32 {
    1
}
fn ImageList_Draw(_: &mut Ctx) -> u32 {
    1
}
fn ImageList_DrawEx(_: &mut Ctx) -> u32 {
    1
}
fn ImageList_ReplaceIcon(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// winmm

fn timeGetTime(_: &mut Ctx) -> u32 {
    crate::k32::ticks_ms()
}
fn timeBeginPeriod(_: &mut Ctx) -> u32 {
    0
}
fn timeEndPeriod(_: &mut Ctx) -> u32 {
    0
}
fn timeGetDevCaps(c: &mut Ctx) -> u32 {
    w32(c.a(0), 1);
    w32(c.a(0) + 4, 1_000_000);
    0
}
fn timeGetSystemTime(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    w32(p, 1);
    w32(p + 4, crate::k32::ticks_ms());
    0
}

struct MmTimer {
    delay: u32,
    proc_: u32,
    user: u32,
    flags: u32,
    stop: AtomicBool,
}

static TIMERS: Mutex<Vec<(u32, Arc<MmTimer>)>> = Mutex::new(Vec::new());
static NEXT_TIMER: AtomicU32 = AtomicU32::new(1);

/// The thread of one multimedia timer (its start "function" is this trap).
fn mm_timer_thread(c: &mut Ctx) -> u32 {
    let id = c.a(0);
    let Some(t) = TIMERS.lock().unwrap().iter().find(|x| x.0 == id).map(|x| x.1.clone()) else { return 0 };
    let period = Duration::from_millis(t.delay.max(1) as u64);
    let mut next = std::time::Instant::now() + period;
    loop {
        let now = std::time::Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        if t.stop.load(Ordering::Relaxed) {
            break;
        }
        if t.flags & 0x10 != 0 {
            crate::k32::set_event(t.proc_);
        } else if t.flags & 0x20 != 0 {
            crate::k32::pulse_event(t.proc_);
        } else {
            c.call(t.proc_, &[id, 0, t.user, 0, 0]);
        }
        if t.flags & 1 == 0 {
            break;
        }
        next += period;
        let now = std::time::Instant::now();
        if next + period * 4 < now {
            next = now + period;
        }
    }
    TIMERS.lock().unwrap().retain(|x| x.0 != id);
    crate::k32::exit_thread(0)
}

fn timeSetEvent(c: &mut Ctx) -> u32 {
    let (delay, _res, proc_, user, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let id = NEXT_TIMER.fetch_add(1, Ordering::Relaxed);
    let t = Arc::new(MmTimer { delay, proc_, user, flags, stop: AtomicBool::new(false) });
    TIMERS.lock().unwrap().push((id, t));
    static ENTRY: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let entry = *ENTRY.get_or_init(|| crate::api::host_fn("mm_timer_thread", mm_timer_thread, 4));
    let h = obj::new_thread_obj(0);
    crate::spawn_gated(entry, id, 0x10000, h, false);
    id
}
fn timeKillEvent(c: &mut Ctx) -> u32 {
    let id = c.a(0);
    match TIMERS.lock().unwrap().iter().find(|x| x.0 == id) {
        Some((_, t)) => {
            t.stop.store(true, Ordering::Relaxed);
            0
        }
        None => 97,
    }
}
fn joyGetNumDevs(_: &mut Ctx) -> u32 {
    0
}
fn joyGetPosEx(_: &mut Ctx) -> u32 {
    167
}
fn joyGetPos(_: &mut Ctx) -> u32 {
    167
}
fn joyGetDevCapsA(_: &mut Ctx) -> u32 {
    167
}
fn joyGetDevCapsW(_: &mut Ctx) -> u32 {
    167
}
fn mciSendStringA(_: &mut Ctx) -> u32 {
    263
}
fn mciSendStringW(_: &mut Ctx) -> u32 {
    263
}
fn mciSendCommandA(_: &mut Ctx) -> u32 {
    263
}
fn midiOutGetNumDevs(_: &mut Ctx) -> u32 {
    0
}
fn auxGetNumDevs(_: &mut Ctx) -> u32 {
    0
}
fn mixerGetNumDevs(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// shlwapi (a few path helpers)

fn path_op(c: &mut Ctx, wide: bool, f: impl Fn(&str) -> Option<String>) -> u32 {
    let p = c.a(0);
    let s = arg_str(p, wide);
    match f(&s) {
        Some(o) => {
            ret_str(p, 260, &o, wide);
            1
        }
        None => 0,
    }
}
fn remove_spec(s: &str) -> Option<String> {
    let k = s.rfind('\\')?;
    Some(if k == 2 && s.as_bytes().get(1) == Some(&b':') { s[..3].to_string() } else { s[..k].to_string() })
}
/// "C:\\x\\y" -> "C:\\", "\\\\srv\\share\\x" -> "\\\\srv\\share"
fn strip_to_root(s: &str) -> Option<String> {
    if s.as_bytes().get(1) == Some(&b':') {
        return Some(format!("{}\\", &s[..2]));
    }
    if let Some(rest) = s.strip_prefix("\\\\") {
        let n = rest.match_indices('\\').nth(1).map_or(rest.len(), |x| x.0);
        return Some(format!("\\\\{}", &rest[..n]));
    }
    s.starts_with('\\').then(|| "\\".to_string())
}
fn PathStripToRootA(c: &mut Ctx) -> u32 {
    path_op(c, false, strip_to_root)
}
fn PathStripToRootW(c: &mut Ctx) -> u32 {
    path_op(c, true, strip_to_root)
}
fn PathIsUNCA(c: &mut Ctx) -> u32 {
    arg_str(c.a(0), false).starts_with("\\\\") as u32
}
fn PathIsUNCW(c: &mut Ctx) -> u32 {
    arg_str(c.a(0), true).starts_with("\\\\") as u32
}
fn unquote(s: &str) -> Option<String> {
    (s.len() >= 2 && s.starts_with('"') && s.ends_with('"')).then(|| s[1..s.len() - 1].to_string())
}
fn PathUnquoteSpacesA(c: &mut Ctx) -> u32 {
    path_op(c, false, unquote)
}
fn PathUnquoteSpacesW(c: &mut Ctx) -> u32 {
    path_op(c, true, unquote)
}
fn add_backslash(s: &str) -> Option<String> {
    Some(if s.ends_with('\\') || s.is_empty() { s.to_string() } else { format!("{s}\\") })
}
fn PathAddBackslashA(c: &mut Ctx) -> u32 {
    path_op(c, false, add_backslash);
    c.a(0) + crate::mem::cstr(c.a(0)).len() as u32
}
fn PathAddBackslashW(c: &mut Ctx) -> u32 {
    path_op(c, true, add_backslash);
    c.a(0) + 2 * wstr(c.a(0)).len() as u32
}
fn strip_path(s: &str) -> Option<String> {
    Some(s.rsplit(['\\', '/']).next().unwrap_or(s).to_string())
}
fn PathStripPathA(c: &mut Ctx) -> u32 {
    path_op(c, false, strip_path);
    0
}
fn PathStripPathW(c: &mut Ctx) -> u32 {
    path_op(c, true, strip_path);
    0
}
fn path_add_extension(c: &mut Ctx, wide: bool) -> u32 {
    let ext = if c.a(1) == 0 { ".exe".to_string() } else { arg_str(c.a(1), wide) };
    path_op(c, wide, |s| {
        let name = s.rsplit('\\').next().unwrap_or(s);
        (!name.contains('.')).then(|| format!("{s}{ext}"))
    });
    1
}
fn PathAddExtensionA(c: &mut Ctx) -> u32 {
    path_add_extension(c, false)
}
fn PathAddExtensionW(c: &mut Ctx) -> u32 {
    path_add_extension(c, true)
}
/// WSAStartup(version, WSADATA*): Winsock 2.2 without a network (sockets fail later). Lua's
/// socket library checks the version it gets and stops the game otherwise.
fn WSAStartup(c: &mut Ctx) -> u32 {
    let d = c.a(1);
    if d == 0 {
        return 10014; // WSAEFAULT
    }
    let want = c.a(0) & 0xffff;
    let v = if (want & 0xff, want >> 8) >= (2, 2) { 0x0202 } else { want };
    for i in 0..400 {
        crate::mem::w8(d + i, 0);
    }
    w16(d, v as u16);
    w16(d + 2, 0x0202);
    for (i, b) in b"WinSock 2.0".iter().enumerate() {
        crate::mem::w8(d + 4 + i as u32, *b);
    }
    for (i, b) in b"Running".iter().enumerate() {
        crate::mem::w8(d + 261 + i as u32, *b);
    }
    0
}
fn WSACleanup(_: &mut Ctx) -> u32 {
    0
}
fn IsNetworkAlive(c: &mut Ctx) -> u32 {
    put32(c.a(0), 1);
    1
}
fn PathRemoveFileSpecA(c: &mut Ctx) -> u32 {
    path_op(c, false, remove_spec)
}
fn PathRemoveFileSpecW(c: &mut Ctx) -> u32 {
    path_op(c, true, remove_spec)
}
fn PathFileExistsA(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), false)).is_some() as u32
}
fn PathFileExistsW(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), true)).is_some() as u32
}
fn PathIsDirectoryA(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), false)).is_some_and(|h| std::path::Path::new(&h).is_dir()) as u32 * 16
}
fn PathIsDirectoryW(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), true)).is_some_and(|h| std::path::Path::new(&h).is_dir()) as u32 * 16
}
fn append(a: &str, b: &str) -> String {
    if a.is_empty() {
        return b.into();
    }
    format!("{}\\{}", a.trim_end_matches('\\'), b.trim_start_matches('\\'))
}
fn PathAppendA(c: &mut Ctx) -> u32 {
    let b = arg_str(c.a(1), false);
    path_op(c, false, |a| Some(append(a, &b)))
}
fn PathAppendW(c: &mut Ctx) -> u32 {
    let b = arg_str(c.a(1), true);
    path_op(c, true, |a| Some(append(a, &b)))
}
fn PathCombineA(c: &mut Ctx) -> u32 {
    let (d, a, b) = (c.a(0), arg_str(c.a(1), false), arg_str(c.a(2), false));
    ret_str(d, 260, &append(&a, &b), false);
    d
}
fn PathCombineW(c: &mut Ctx) -> u32 {
    let (d, a, b) = (c.a(0), arg_str(c.a(1), true), arg_str(c.a(2), true));
    ret_str(d, 260, &append(&a, &b), true);
    d
}
fn PathFindFileNameA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let s = cstr(p);
    let k = s.iter().rposition(|&b| b == b'\\' || b == b'/').map_or(0, |k| k + 1);
    p + k as u32
}
fn PathFindFileNameW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let s = wstr(p);
    let k = s.iter().rposition(|&b| b == b'\\' as u16 || b == b'/' as u16).map_or(0, |k| k + 1);
    p + 2 * k as u32
}
fn PathFindExtensionA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let s = cstr(p);
    let start = s.iter().rposition(|&b| b == b'\\').map_or(0, |k| k + 1);
    p + s[start..].iter().rposition(|&b| b == b'.').map_or(s.len(), |k| start + k) as u32
}
fn PathFindExtensionW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let s = wstr(p);
    let start = s.iter().rposition(|&b| b == b'\\' as u16).map_or(0, |k| k + 1);
    p + 2 * s[start..].iter().rposition(|&b| b == b'.' as u16).map_or(s.len(), |k| start + k) as u32
}
fn PathIsRelativeA(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0));
    !(s.first() == Some(&b'\\') || s.get(1) == Some(&b':')) as u32
}
fn PathIsRelativeW(c: &mut Ctx) -> u32 {
    let s = wstr(c.a(0));
    !(s.first() == Some(&(b'\\' as u16)) || s.get(1) == Some(&(b':' as u16))) as u32
}

// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// advapi32 hashing (CryptoAPI): MD5, SHA-1, SHA-256

enum Hasher {
    Md5(md5::Context),
    Sha1(Sha1),
    Sha256(sha2::Sha256),
}

static HASHES: Mutex<Option<HashMap<u32, Hasher>>> = Mutex::new(None);
static NEXT_HASH: AtomicU32 = AtomicU32::new(0x4a00_0000);

fn CryptAcquireContextA(c: &mut Ctx) -> u32 {
    put32(c.a(0), 0x4a7f_0001);
    1
}
fn CryptAcquireContextW(c: &mut Ctx) -> u32 {
    CryptAcquireContextA(c)
}
fn CryptReleaseContext(_: &mut Ctx) -> u32 {
    1
}
fn CryptGenRandom(c: &mut Ctx) -> u32 {
    let (n, p) = (c.a(1), c.a(2));
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
    for i in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        mem::w8(p + i, x as u8);
    }
    1
}
fn CryptCreateHash(c: &mut Ctx) -> u32 {
    let h = match c.a(1) {
        0x8003 => Hasher::Md5(md5::Context::new()),
        0x8004 => Hasher::Sha1(Sha1::default()),
        0x800c => Hasher::Sha256(<sha2::Sha256 as sha2::Digest>::new()),
        alg => {
            eprintln!("sake: CryptCreateHash: algorithm {alg:#x}");
            return 0;
        }
    };
    let id = NEXT_HASH.fetch_add(1, Ordering::Relaxed);
    HASHES.lock().unwrap().get_or_insert_with(HashMap::default).insert(id, h);
    put32(c.a(4), id);
    1
}
fn CryptHashData(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(1), c.a(2));
    let mut g = HASHES.lock().unwrap();
    match g.get_or_insert_with(HashMap::default).get_mut(&c.a(0)) {
        Some(Hasher::Md5(h)) => h.consume(data),
        Some(Hasher::Sha1(h)) => h.update(data),
        Some(Hasher::Sha256(h)) => sha2::Digest::update(h, data),
        None => return 0,
    }
    1
}
fn CryptGetHashParam(c: &mut Ctx) -> u32 {
    let (h, param, out, plen) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut g = HASHES.lock().unwrap();
    let Some(hs) = g.get_or_insert_with(HashMap::default).get(&h) else { return 0 };
    let (alg, digest): (u32, Vec<u8>) = match hs {
        Hasher::Md5(x) => (0x8003, x.clone().finalize().0.to_vec()),
        Hasher::Sha1(x) => (0x8004, x.clone().finish().to_vec()),
        Hasher::Sha256(x) => (0x800c, sha2::Digest::finalize(x.clone()).to_vec()),
    };
    let val: Vec<u8> = match param {
        1 => alg.to_le_bytes().to_vec(),
        2 => digest,
        4 => (digest.len() as u32).to_le_bytes().to_vec(),
        _ => return 0,
    };
    if out != 0 {
        if r32(plen) < val.len() as u32 {
            put32(plen, val.len() as u32);
            w32(crate::cur().teb + crate::teb_last_error(), 234);
            return 0;
        }
        mem::write(out, &val);
    }
    put32(plen, val.len() as u32);
    1
}
fn CryptDestroyHash(c: &mut Ctx) -> u32 {
    HASHES.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0)).is_some() as u32
}

/// SHA-1 (no crate for it here; small enough to keep).
#[derive(Clone, Default)]
struct Sha1 {
    buf: Vec<u8>,
}
impl Sha1 {
    fn update(&mut self, d: &[u8]) {
        self.buf.extend_from_slice(d);
    }
    fn finish(mut self) -> [u8; 20] {
        let bits = (self.buf.len() as u64) * 8;
        self.buf.push(0x80);
        while self.buf.len() % 64 != 56 {
            self.buf.push(0);
        }
        self.buf.extend_from_slice(&bits.to_be_bytes());
        let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
        for chunk in self.buf.chunks(64) {
            let mut w = [0u32; 80];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
            }
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }
            let [mut a, mut b, mut c, mut d, mut e] = h;
            for (i, &wi) in w.iter().enumerate() {
                let (f, k) = match i {
                    0..=19 => ((b & c) | (!b & d), 0x5A827999),
                    20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                    40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                    _ => (b ^ c ^ d, 0xCA62C1D6),
                };
                let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = t;
            }
            for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
                *x = x.wrapping_add(y);
            }
        }
        let mut out = [0u8; 20];
        for (i, v) in h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }
}

fn WinVerifyTrust(_: &mut Ctx) -> u32 {
    0
}

/// XInput: no controller is ever connected.
fn xinput_none(_: &mut Ctx) -> u32 {
    1167 // ERROR_DEVICE_NOT_CONNECTED
}
fn xinput_ok(_: &mut Ctx) -> u32 {
    0
}

pub fn init() {
    let _ = (s2a, r8);
    implement("shell32", "SHGetKnownFolderPath", SHGetKnownFolderPath);
    for d in ["xinput1_1", "xinput1_2", "xinput1_3", "xinput1_4", "xinput9_1_0"] {
        for n in ["XInputGetState", "XInputGetStateEx", "XInputSetState", "XInputGetCapabilities", "XInputGetKeystroke", "XInputGetBatteryInformation", "XInputGetDSoundAudioDeviceGuids"] {
            implement(d, n, xinput_none);
        }
        implement(d, "XInputEnable", xinput_ok);
    }
    for n in ["XInputGetAudioDeviceIds", "XInputWaitForGuideButton", "XInputCancelGuideButtonWait", "XInputPowerOffController", "XInputGetBaseBusInformation", "XInputGetCapabilitiesEx"] {
        implement("xinput1_4", n, xinput_none);
    }
    reg!("advapi32";
        RegOpenKeyExA, RegOpenKeyExW, RegOpenKeyA, RegOpenKeyW, RegCreateKeyExA, RegCreateKeyExW, RegCreateKeyA,
        RegCreateKeyW, RegCloseKey, RegFlushKey, RegQueryValueExA, RegQueryValueExW, RegQueryValueA,
        RegSetValueExA, RegSetValueExW, RegSetValueA, RegDeleteValueA, RegDeleteValueW, RegDeleteKeyA,
        RegDeleteKeyW, RegEnumKeyExA, RegEnumKeyExW, RegEnumKeyA, RegEnumKeyW, RegEnumValueA, RegEnumValueW,
        RegQueryInfoKeyA, RegQueryInfoKeyW, CryptAcquireContextA, CryptAcquireContextW, CryptReleaseContext,
        CryptGenRandom, CryptCreateHash, CryptHashData, CryptGetHashParam, CryptDestroyHash,
    );
    reg!("wintrust"; WinVerifyTrust);
    reg!("version"; GetFileVersionInfoSizeA, GetFileVersionInfoSizeW, GetFileVersionInfoA, GetFileVersionInfoW,
        VerQueryValueA, VerQueryValueW);
    reg!("ole32"; CoInitialize, CoInitializeEx, OleInitialize, CoUninitialize, OleUninitialize, CoCreateInstance,
        CoTaskMemAlloc, CoTaskMemRealloc, CoTaskMemFree, CoGetMalloc, CoCreateGuid, IsEqualGUID, StringFromGUID2, CLSIDFromString, IIDFromString,
        RegisterDragDrop, RevokeDragDrop);
    reg!("oleaut32"; SysAllocString, SysAllocStringLen, SysAllocStringByteLen, SysReAllocString,
        SysReAllocStringLen, SysFreeString, SysStringLen, SysStringByteLen, VariantInit, VariantClear, VariantCopy,
        VariantCopyInd, VariantChangeTypeEx, VariantChangeType, GetErrorInfo, SetErrorInfo);
    reg!("shfolder"; SHGetFolderPathA, SHGetFolderPathW);
    reg!("shell32"; SHCreateDirectoryExA, SHCreateDirectoryExW, SHCreateDirectory, SHGetSpecialFolderPathA, SHGetSpecialFolderPathW, SHGetFolderPathA, SHGetFolderPathW,
        SHGetSpecialFolderLocation, SHGetFolderLocation, SHGetPathFromIDListA, SHGetPathFromIDListW, ILFree, SHGetMalloc, IsUserAnAdmin,
        ShellExecuteA, ShellExecuteW, ShellExecuteExA, ShellExecuteExW, DragAcceptFiles, DragFinish,
        DragQueryFileA, DragQueryFileW, CommandLineToArgvW);
    reg!("comctl32"; InitCommonControls, InitCommonControlsEx, ImageList_Create, ImageList_Read, ImageList_Destroy, ImageList_Add,
        ImageList_GetImageCount, ImageList_SetBkColor, ImageList_GetBkColor, ImageList_GetIconSize,
        ImageList_SetIconSize, ImageList_Remove, ImageList_Draw, ImageList_DrawEx, ImageList_ReplaceIcon);
    reg!("winmm"; timeGetTime, timeBeginPeriod, timeEndPeriod, timeGetDevCaps, timeGetSystemTime, timeSetEvent,
        timeKillEvent, joyGetNumDevs, joyGetPosEx, joyGetPos, joyGetDevCapsA, joyGetDevCapsW, mciSendStringA,
        mciSendStringW, mciSendCommandA, midiOutGetNumDevs, auxGetNumDevs, mixerGetNumDevs);
    reg!("shlwapi"; PathStripToRootA, PathStripToRootW, PathIsUNCA, PathIsUNCW);
    reg!("shlwapi"; PathRemoveFileSpecA, PathRemoveFileSpecW, PathFileExistsA, PathFileExistsW, PathIsDirectoryA,
        PathIsDirectoryW, PathAppendA, PathAppendW, PathCombineA, PathCombineW, PathFindFileNameA,
        PathFindFileNameW, PathFindExtensionA, PathFindExtensionW, PathIsRelativeA, PathIsRelativeW, PathUnquoteSpacesA,
        PathUnquoteSpacesW, PathAddBackslashA, PathAddBackslashW, PathStripPathA, PathStripPathW, PathAddExtensionA, PathAddExtensionW);
    reg!("shlwapi"; StrStrA, StrStrW, StrStrIA, StrStrIW);
    reg!("sensapi"; IsNetworkAlive);
    reg!("ws2_32"; WSAStartup, WSACleanup);
}

/// StrStr(I)A/W(haystack, needle): the first match (by UTF-16 unit, case folded for I), or 0
fn str_str(c: &mut Ctx, wide: bool, fold: bool) -> u32 {
    let (h, n) = (c.a(0), c.a(1));
    if h == 0 || n == 0 {
        return 0;
    }
    let get = |p: u32| if wide { wstr(p) } else { cstr(p).into_iter().map(|b| b as u16).collect() };
    let low = |v: Vec<u16>| -> Vec<u16> {
        if fold { v.into_iter().map(|u| if (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u }).collect() } else { v }
    };
    let (hs, ns) = (low(get(h)), low(get(n)));
    if ns.is_empty() {
        return h;
    }
    // (A: byte-wise, a DBCS trail byte can't start a match)
    let mut i = 0;
    while i + ns.len() <= hs.len() {
        if hs[i..i + ns.len()] == ns[..] {
            return h + i as u32 * if wide { 2 } else { 1 };
        }
        i += if !wide && crate::text::is_lead(hs[i] as u8) { 2 } else { 1 };
    }
    0
}
fn StrStrA(c: &mut Ctx) -> u32 {
    str_str(c, false, false)
}
fn StrStrW(c: &mut Ctx) -> u32 {
    str_str(c, true, false)
}
fn StrStrIA(c: &mut Ctx) -> u32 {
    str_str(c, false, true)
}
fn StrStrIW(c: &mut Ctx) -> u32 {
    str_str(c, true, true)
}
