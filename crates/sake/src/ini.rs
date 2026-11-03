//! kernel32's profile (.ini) functions over real files: GetPrivateProfileString/Int/Section,
//! WritePrivateProfileString, and the win.ini GetProfile* ones. Files are read as ANSI (or
//! UTF-16 with a BOM) and written back as ANSI.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::files;
use crate::k32::arg_str;
use crate::mem::{put_cstr, put_wstr};

type Ini = Vec<(String, Vec<(String, String)>)>;

/// The Windows path of profile file `name`: a bare name lives in C:\windows.
fn path(name: &str) -> String {
    if name.contains('\\') || name.contains('/') {
        files::full_path(name)
    } else {
        format!("C:\\windows\\{name}")
    }
}

fn load(name: &str) -> Ini {
    let Some(h) = files::resolve(&path(name)) else { return Vec::new() };
    let Ok(b) = kura::fs::read(h) else { return Vec::new() };
    let text = if b.starts_with(&[0xff, 0xfe]) {
        String::from_utf16_lossy(&b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
    } else if let Some(u) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(u).into_owned()
    } else {
        crate::text::a2s(&b)
    };
    let mut ini: Ini = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(s) = l.strip_prefix('[') {
            ini.push((s.split(']').next().unwrap_or("").trim().to_string(), Vec::new()));
        } else if l.is_empty() || l.starts_with(';') {
        } else if let Some(sec) = ini.last_mut() {
            match l.split_once('=') {
                Some((k, v)) => sec.1.push((k.trim().to_string(), unquote(v.trim()).to_string())),
                None => sec.1.push((l.to_string(), String::new())),
            }
        }
    }
    ini
}

fn unquote(v: &str) -> &str {
    if v.len() >= 2 && (v.starts_with('"') && v.ends_with('"') || v.starts_with('\'') && v.ends_with('\'')) {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

fn save(name: &str, ini: &Ini) -> bool {
    let mut s = String::new();
    for (sec, keys) in ini {
        s += &format!("[{sec}]\r\n");
        for (k, v) in keys {
            s += &format!("{k}={v}\r\n");
        }
        s += "\r\n";
    }
    let w = path(name);
    let h = files::resolve(&w).unwrap_or_else(|| files::resolve_new(&w));
    kura::fs::write(h, crate::text::s2a(&s)).is_ok()
}

fn section<'a>(ini: &'a Ini, s: &str) -> Option<&'a Vec<(String, String)>> {
    ini.iter().find(|x| x.0.eq_ignore_ascii_case(s)).map(|x| &x.1)
}

fn value(ini: &Ini, s: &str, k: &str) -> Option<String> {
    section(ini, s)?.iter().find(|x| x.0.eq_ignore_ascii_case(k)).map(|x| x.1.clone())
}

/// Copies `s` into a buffer of `cap` characters (truncated, NUL-terminated): returns the length.
fn put(dst: u32, cap: u32, s: &str, wide: bool) -> u32 {
    if cap == 0 {
        return 0;
    }
    if wide {
        let v: Vec<u16> = s.encode_utf16().take(cap as usize - 1).collect();
        put_wstr(dst, cap, &v);
        v.len() as u32
    } else {
        let mut v = crate::text::s2a(s);
        v.truncate(cap as usize - 1);
        put_cstr(dst, cap, &v);
        v.len() as u32
    }
}

/// A list of NUL-separated strings ending in a double NUL, as much as fits: returns the length
/// without the final NUL (cap - 2 when cut).
fn put_list(dst: u32, cap: u32, items: &[String], wide: bool) -> u32 {
    let mut out: Vec<u16> = Vec::new();
    let mut a: Vec<u8> = Vec::new();
    for i in items {
        if wide {
            out.extend(i.encode_utf16());
            out.push(0);
        } else {
            a.extend(crate::text::s2a(i));
            a.push(0);
        }
    }
    let n = if wide { out.len() } else { a.len() } as u32;
    if cap < 2 {
        return 0;
    }
    let (len, cut) = if n + 1 > cap { (cap - 2, true) } else { (n, false) };
    for i in 0..len {
        if wide {
            crate::mem::w16(dst + 2 * i, out[i as usize]);
        } else {
            crate::mem::w8(dst + i, a[i as usize]);
        }
    }
    let z = |at: u32| if wide { crate::mem::w16(dst + 2 * at, 0) } else { crate::mem::w8(dst + at, 0) };
    z(len);
    z(len + 1);
    if cut { cap - 2 } else { len }
}

fn get_string(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let (sec, key, def, buf, cap) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let ini = load(file);
    if sec == 0 {
        let v: Vec<String> = ini.iter().map(|x| x.0.clone()).collect();
        return put_list(buf, cap, &v, wide);
    }
    let s = arg_str(sec, wide);
    if key == 0 {
        let v: Vec<String> = section(&ini, &s).map(|k| k.iter().map(|x| x.0.clone()).collect()).unwrap_or_default();
        return put_list(buf, cap, &v, wide);
    }
    let v = value(&ini, &s, &arg_str(key, wide)).unwrap_or_else(|| if def != 0 { arg_str(def, wide).trim_end().to_string() } else { String::new() });
    put(buf, cap, &v, wide)
}

fn get_int(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let ini = load(file);
    let Some(v) = value(&ini, &arg_str(c.a(0), wide), &arg_str(c.a(1), wide)) else { return c.a(2) };
    let v = v.trim();
    let (neg, d) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v),
    };
    let n: i64 = if let Some(h) = d.strip_prefix("0x").or_else(|| d.strip_prefix("0X")) {
        i64::from_str_radix(&h.chars().take_while(|c| c.is_ascii_hexdigit()).collect::<String>(), 16).unwrap_or(0)
    } else {
        d.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)
    };
    (if neg { -n } else { n }) as i32 as u32
}

fn write_string(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let (sec, key, val) = (c.a(0), c.a(1), c.a(2));
    if sec == 0 {
        return 1;
    }
    let s = arg_str(sec, wide);
    let mut ini = load(file);
    let i = match ini.iter().position(|x| x.0.eq_ignore_ascii_case(&s)) {
        Some(i) => i,
        None => {
            ini.push((s, Vec::new()));
            ini.len() - 1
        }
    };
    if key == 0 {
        ini.remove(i);
    } else {
        let k = arg_str(key, wide);
        let keys = &mut ini[i].1;
        let at = keys.iter().position(|x| x.0.eq_ignore_ascii_case(&k));
        match (at, val) {
            (Some(j), 0) => {
                keys.remove(j);
            }
            (None, 0) => {}
            (Some(j), v) => keys[j].1 = arg_str(v, wide),
            (None, v) => keys.push((k, arg_str(v, wide))),
        }
    }
    save(file, &ini) as u32
}

fn get_section(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let ini = load(file);
    let v: Vec<String> = section(&ini, &arg_str(c.a(0), wide)).map(|k| k.iter().map(|(k, v)| format!("{k}={v}")).collect()).unwrap_or_default();
    put_list(c.a(1), c.a(2), &v, wide)
}

fn GetPrivateProfileStringA(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(5), false);
    get_string(c, false, &f)
}
fn GetPrivateProfileStringW(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(5), true);
    get_string(c, true, &f)
}
fn GetProfileStringA(c: &mut Ctx) -> u32 {
    get_string(c, false, "win.ini")
}
fn GetProfileStringW(c: &mut Ctx) -> u32 {
    get_string(c, true, "win.ini")
}
fn GetPrivateProfileIntA(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(3), false);
    get_int(c, false, &f)
}
fn GetPrivateProfileIntW(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(3), true);
    get_int(c, true, &f)
}
fn GetProfileIntA(c: &mut Ctx) -> u32 {
    get_int(c, false, "win.ini")
}
fn GetProfileIntW(c: &mut Ctx) -> u32 {
    get_int(c, true, "win.ini")
}
fn WritePrivateProfileStringA(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(3), false);
    write_string(c, false, &f)
}
fn WritePrivateProfileStringW(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(3), true);
    write_string(c, true, &f)
}
fn WriteProfileStringA(c: &mut Ctx) -> u32 {
    write_string(c, false, "win.ini")
}
fn WriteProfileStringW(c: &mut Ctx) -> u32 {
    write_string(c, true, "win.ini")
}
fn GetPrivateProfileSectionA(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(3), false);
    get_section(c, false, &f)
}
fn GetPrivateProfileSectionW(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(3), true);
    get_section(c, true, &f)
}
fn GetPrivateProfileSectionNamesA(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(2), false);
    let v: Vec<String> = load(&f).into_iter().map(|x| x.0).collect();
    put_list(c.a(0), c.a(1), &v, false)
}
fn GetPrivateProfileSectionNamesW(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(2), true);
    let v: Vec<String> = load(&f).into_iter().map(|x| x.0).collect();
    put_list(c.a(0), c.a(1), &v, true)
}

pub fn init() {
    for (n, f) in [
        ("GetPrivateProfileStringA", GetPrivateProfileStringA as fn(&mut Ctx) -> u32),
        ("GetPrivateProfileStringW", GetPrivateProfileStringW),
        ("GetProfileStringA", GetProfileStringA),
        ("GetProfileStringW", GetProfileStringW),
        ("GetPrivateProfileIntA", GetPrivateProfileIntA),
        ("GetPrivateProfileIntW", GetPrivateProfileIntW),
        ("GetProfileIntA", GetProfileIntA),
        ("GetProfileIntW", GetProfileIntW),
        ("WritePrivateProfileStringA", WritePrivateProfileStringA),
        ("WritePrivateProfileStringW", WritePrivateProfileStringW),
        ("WriteProfileStringA", WriteProfileStringA),
        ("WriteProfileStringW", WriteProfileStringW),
        ("GetPrivateProfileSectionA", GetPrivateProfileSectionA),
        ("GetPrivateProfileSectionW", GetPrivateProfileSectionW),
        ("GetPrivateProfileSectionNamesA", GetPrivateProfileSectionNamesA),
        ("GetPrivateProfileSectionNamesW", GetPrivateProfileSectionNamesW),
    ] {
        implement("kernel32", n, f);
    }
}
