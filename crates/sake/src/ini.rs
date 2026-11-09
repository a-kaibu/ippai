//! kernel32's profile (.ini) functions over real files: GetPrivateProfileString/Int/Section,
//! WritePrivateProfileString, and the win.ini GetProfile* ones. Files are read as ANSI (or
//! UTF-16 with a BOM) and written back as ANSI.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::files;
use crate::k32::arg_str;
use crate::mem::{put_cstr, put_wstr};

/// A profile file as its lines: the ones before the first section, then each section's header
/// and lines. Writes change only the lines they touch, as Windows does (comments, odd spacing,
/// quotes and duplicate sections stay as they were).
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct Ini {
    pre: Vec<String>,
    secs: Vec<Sec>,
    /// read from UTF-16 (written back the same way)
    wide: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Sec {
    header: String,
    name: String,
    lines: Vec<String>,
}

/// A section line's key and value: blank lines and ';' comments are neither, a line without
/// '=' is a key with no value
fn entry(l: &str) -> Option<(&str, Option<&str>)> {
    let t = l.trim();
    if t.is_empty() || t.starts_with(';') {
        return None;
    }
    Some(match t.split_once('=') {
        Some((k, v)) => (k.trim(), Some(v.trim())),
        None => (t, None),
    })
}

fn unquote(v: &str) -> &str {
    if v.len() >= 2 && (v.starts_with('"') && v.ends_with('"') || v.starts_with('\'') && v.ends_with('\'')) {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

impl Ini {
    pub(crate) fn parse(text: &str) -> Ini {
        let mut ini = Ini::default();
        let mut lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        for l in lines {
            if let Some(h) = l.trim_start().strip_prefix('[') {
                let name = h.split(']').next().unwrap_or("").trim().to_string();
                ini.secs.push(Sec { header: l.to_string(), name, lines: Vec::new() });
            } else if let Some(s) = ini.secs.last_mut() {
                s.lines.push(l.to_string());
            } else {
                ini.pre.push(l.to_string());
            }
        }
        ini
    }

    pub(crate) fn text(&self) -> String {
        let mut s = String::new();
        for l in self.pre.iter().chain(self.secs.iter().flat_map(|x| std::iter::once(&x.header).chain(&x.lines))) {
            s += l;
            s += "\r\n";
        }
        s
    }

    /// The first section called `name` (later ones of the same name are never seen)
    fn sec(&self, name: &str) -> Option<usize> {
        self.secs.iter().position(|x| x.name.eq_ignore_ascii_case(name.trim()))
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.secs.iter().map(|x| x.name.clone()).collect()
    }

    /// The keys that have values, in file order (duplicates too)
    pub(crate) fn keys(&self, sec: &str) -> Option<Vec<String>> {
        let s = &self.secs[self.sec(sec)?];
        Some(s.lines.iter().filter_map(|l| entry(l)).filter(|e| e.1.is_some()).map(|e| e.0.to_string()).collect())
    }

    /// GetPrivateProfileSection's lines: "key=value" trimmed (quotes kept), bare keys as they are
    pub(crate) fn section(&self, sec: &str) -> Option<Vec<String>> {
        let s = &self.secs[self.sec(sec)?];
        Some(s.lines.iter().filter_map(|l| entry(l)).map(|(k, v)| v.map_or(k.to_string(), |v| format!("{k}={v}"))).collect())
    }

    /// The value of `key`: trimmed, and unquoted when quoted at both ends
    pub(crate) fn value(&self, sec: &str, key: &str) -> Option<String> {
        let s = &self.secs[self.sec(sec)?];
        s.lines.iter().filter_map(|l| entry(l)).find(|e| e.1.is_some() && e.0.eq_ignore_ascii_case(key.trim())).map(|e| unquote(e.1.unwrap()).to_string())
    }

    fn sec_or_new(&mut self, sec: &str) -> usize {
        self.sec(sec).unwrap_or_else(|| {
            self.secs.push(Sec { header: format!("[{sec}]"), name: sec.trim().to_string(), lines: Vec::new() });
            self.secs.len() - 1
        })
    }

    /// Sets `key` (a new one goes after the section's last line that is not blank), or deletes
    /// it when `val` is None
    pub(crate) fn set(&mut self, sec: &str, key: &str, val: Option<&str>) {
        let Some(val) = val else {
            if let Some(i) = self.sec(sec) {
                if let Some(j) = self.secs[i].lines.iter().position(|l| entry(l).is_some_and(|e| e.0.eq_ignore_ascii_case(key.trim()))) {
                    self.secs[i].lines.remove(j);
                }
            }
            return;
        };
        let i = self.sec_or_new(sec);
        let lines = &mut self.secs[i].lines;
        match lines.iter().position(|l| entry(l).is_some_and(|e| e.0.eq_ignore_ascii_case(key.trim()))) {
            Some(j) => {
                let k = entry(&lines[j]).unwrap().0.to_string();
                lines[j] = format!("{k}={val}");
            }
            None => {
                let at = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |j| j + 1);
                lines.insert(at, format!("{}={val}", key.trim()));
            }
        }
    }

    pub(crate) fn delete_section(&mut self, sec: &str) {
        if let Some(i) = self.sec(sec) {
            self.secs.remove(i);
        }
    }

    /// WritePrivateProfileSection: the section's lines become `lines`
    pub(crate) fn set_section(&mut self, sec: &str, lines: Vec<String>) {
        let i = self.sec_or_new(sec);
        self.secs[i].lines = lines;
    }
}

/// GetPrivateProfileStruct's text: the bytes in hex, then their sum's low byte
pub(crate) fn struct_hex(b: &[u8]) -> String {
    let sum = b.iter().fold(0u8, |s, &x| s.wrapping_add(x));
    b.iter().chain([sum].iter()).map(|x| format!("{x:02X}")).collect()
}

/// The `size` bytes of a struct value, or the Win32 error: 24 (ERROR_BAD_LENGTH) when the text
/// is not size+1 bytes of hex, 13 (ERROR_INVALID_DATA) when a digit or the checksum is wrong
pub(crate) fn struct_bytes(v: &str, size: usize) -> Result<Vec<u8>, u32> {
    if v.len() != 2 * (size + 1) {
        return Err(24);
    }
    let mut b = Vec::with_capacity(size + 1);
    for i in 0..=size {
        b.push(u8::from_str_radix(v.get(2 * i..2 * i + 2).ok_or(13u32)?, 16).map_err(|_| 13u32)?);
    }
    let sum = b.pop().unwrap();
    if sum != b.iter().fold(0u8, |s, &x| s.wrapping_add(x)) {
        return Err(13);
    }
    Ok(b)
}

/// GetPrivateProfileInt's number: leading digits (0x for hex) after an optional '-', wrapped to
/// 32 bits; None for an empty value (the default is returned)
pub(crate) fn int_of(v: &str) -> Option<u32> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    let (neg, d) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v),
    };
    let mut n: u32 = 0;
    if let Some(h) = d.strip_prefix("0x").or_else(|| d.strip_prefix("0X")) {
        for c in h.chars().map_while(|c| c.to_digit(16)) {
            n = n.wrapping_mul(16).wrapping_add(c);
        }
    } else {
        for c in d.chars().map_while(|c| c.to_digit(10)) {
            n = n.wrapping_mul(10).wrapping_add(c);
        }
    }
    Some(if neg { n.wrapping_neg() } else { n })
}

/// The Windows path of profile file `name`: a bare name lives in C:\windows.
fn path(name: &str) -> String {
    if name.contains('\\') || name.contains('/') {
        files::full_path(name)
    } else {
        format!("C:\\windows\\{name}")
    }
}

/// The file's contents, or None when it does not exist
fn load(name: &str) -> Option<Ini> {
    let h = files::resolve(&path(name))?;
    let b = kura::fs::read(h).ok()?;
    let mut wide = false;
    let text = if b.starts_with(&[0xff, 0xfe]) {
        wide = true;
        String::from_utf16_lossy(&b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
    } else if let Some(u) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(u).into_owned()
    } else {
        crate::text::a2s(&b)
    };
    let mut ini = Ini::parse(&text);
    ini.wide = wide;
    Some(ini)
}

fn save(name: &str, ini: &Ini) -> bool {
    let s = ini.text();
    let bytes = if ini.wide { [0xff, 0xfe].into_iter().chain(s.encode_utf16().flat_map(|u| u.to_le_bytes())).collect() } else { crate::text::s2a(&s) };
    let w = path(name);
    let h = files::resolve(&w).unwrap_or_else(|| files::resolve_new(&w));
    kura::fs::write(h, bytes).is_ok()
}

/// Copies `s` into a buffer of `cap` characters (truncated, NUL-terminated): returns the length
/// and whether it was cut (Windows counts filling the buffer exactly as cut).
fn put(dst: u32, cap: u32, s: &str, wide: bool) -> (u32, bool) {
    let full = if wide { s.encode_utf16().count() } else { crate::text::s2a(s).len() } as u32;
    if cap == 0 {
        return (0, true);
    }
    let n = if wide {
        let v: Vec<u16> = s.encode_utf16().take(cap as usize - 1).collect();
        put_wstr(dst, cap, &v);
        v.len() as u32
    } else {
        let mut v = crate::text::s2a(s);
        v.truncate(cap as usize - 1);
        put_cstr(dst, cap, &v);
        v.len() as u32
    };
    (n, full + 1 >= cap)
}

/// A list of NUL-separated strings ending in a double NUL, as much as fits: returns the length
/// without the final NUL (cap - 2 when cut) and whether it was cut.
fn put_list(dst: u32, cap: u32, items: &[String], wide: bool) -> (u32, bool) {
    let mut out: Vec<u16> = Vec::new();
    for i in items {
        if wide {
            out.extend(i.encode_utf16());
        } else {
            out.extend(crate::text::s2a(i).into_iter().map(u16::from));
        }
        out.push(0);
    }
    let n = out.len() as u32;
    if cap < 2 {
        return (0, true);
    }
    let cut = n + 1 > cap;
    let len = if cut { cap - 2 } else { n };
    let z = |at: u32, v: u16| if wide { crate::mem::w16(dst + 2 * at, v) } else { crate::mem::w8(dst + at, v as u8) };
    for i in 0..len {
        z(i, out[i as usize]);
    }
    z(len, 0);
    // (a list that fits ends in its last string's NUL and one more; an empty one is one NUL)
    if cut {
        z(len + 1, 0);
    }
    (len, cut)
}

fn list_result(r: (u32, bool)) -> u32 {
    crate::k32::last_error(if r.1 { 234 } else { 0 });
    r.0
}

fn get_string(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let (sec, key, def, buf, cap) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let ini = load(file).unwrap_or_default();
    if sec == 0 {
        return list_result(put_list(buf, cap, &ini.names(), wide));
    }
    let s = arg_str(sec, wide);
    if key == 0 {
        return list_result(put_list(buf, cap, &ini.keys(&s).unwrap_or_default(), wide));
    }
    match ini.value(&s, &arg_str(key, wide)) {
        Some(v) => {
            let (n, cut) = put(buf, cap, &v, wide);
            crate::k32::last_error(if cut { 234 } else { 0 });
            n
        }
        None => {
            let v = if def != 0 { arg_str(def, wide).trim_end().to_string() } else { String::new() };
            crate::k32::last_error(2);
            put(buf, cap, &v, wide).0
        }
    }
}

fn get_int(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let ini = load(file).unwrap_or_default();
    match ini.value(&arg_str(c.a(0), wide), &arg_str(c.a(1), wide)) {
        Some(v) => {
            crate::k32::last_error(0);
            int_of(&v).unwrap_or(c.a(2))
        }
        None => {
            crate::k32::last_error(2);
            c.a(2)
        }
    }
}

/// Changes the file with `f` (creating it when it changes something), and writes it back
fn modify(file: &str, f: impl FnOnce(&mut Ini)) -> u32 {
    let old = load(file);
    let mut ini = old.clone().unwrap_or_default();
    f(&mut ini);
    if old.as_ref() == Some(&ini) || old.is_none() && ini == Ini::default() {
        return 1;
    }
    save(file, &ini) as u32
}

fn write_string(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let (sec, key, val) = (c.a(0), c.a(1), c.a(2));
    if sec == 0 {
        // (flushes the profile cache, which sake does not have)
        crate::k32::last_error(2);
        return 0;
    }
    let s = arg_str(sec, wide);
    let k = (key != 0).then(|| arg_str(key, wide));
    let v = (val != 0).then(|| arg_str(val, wide));
    modify(file, |ini| match k {
        None => ini.delete_section(&s),
        Some(k) => ini.set(&s, &k, v.as_deref()),
    })
}

/// WritePrivateProfileString(section, key, value, file) for sake's own callers (msi's IniFile).
pub fn write_entry(file: &str, sec: &str, key: &str, val: &str) -> bool {
    modify(file, |ini| ini.set(sec, key, Some(val))) != 0
}

/// A key's value (msi's IniLocator).
pub fn read_entry(file: &str, sec: &str, key: &str) -> Option<String> {
    load(file)?.value(sec, key)
}

/// Deletes a key (msi's RemoveIniValues).
pub fn delete_entry(file: &str, sec: &str, key: &str) -> bool {
    modify(file, |ini| ini.set(sec, key, None)) != 0
}

/// A double-NUL-terminated list of strings at `p`
fn arg_list(p: u32, wide: bool) -> Vec<String> {
    let mut v = Vec::new();
    let mut at = p;
    loop {
        let s = arg_str(at, wide);
        let n = if wide { s.encode_utf16().count() } else { crate::text::s2a(&s).len() } as u32;
        if n == 0 {
            return v;
        }
        at += (n + 1) * if wide { 2 } else { 1 };
        v.push(s);
    }
}

fn write_section(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let s = arg_str(c.a(0), wide);
    let lines = (c.a(1) != 0).then(|| arg_list(c.a(1), wide));
    modify(file, |ini| match lines {
        None => ini.delete_section(&s),
        Some(l) => ini.set_section(&s, l),
    })
}

fn get_section(c: &mut Ctx, wide: bool, file: &str) -> u32 {
    let ini = load(file).unwrap_or_default();
    put_list(c.a(1), c.a(2), &ini.section(&arg_str(c.a(0), wide)).unwrap_or_default(), wide).0
}

fn get_struct(c: &mut Ctx, wide: bool) -> u32 {
    let (sec, key, buf, size) = (arg_str(c.a(0), wide), arg_str(c.a(1), wide), c.a(2), c.a(3));
    let file = if c.a(4) != 0 { arg_str(c.a(4), wide) } else { "win.ini".to_string() };
    let Some(v) = load(&file).unwrap_or_default().value(&sec, &key) else {
        crate::k32::last_error(2);
        return 0;
    };
    match struct_bytes(&v, size as usize) {
        Ok(b) => {
            crate::mem::write(buf, &b);
            1
        }
        Err(e) => {
            crate::k32::last_error(e);
            0
        }
    }
}

fn write_struct(c: &mut Ctx, wide: bool) -> u32 {
    let (sec, key, buf, size) = (arg_str(c.a(0), wide), arg_str(c.a(1), wide), c.a(2), c.a(3));
    let file = if c.a(4) != 0 { arg_str(c.a(4), wide) } else { "win.ini".to_string() };
    let v = (buf != 0).then(|| struct_hex(crate::mem::slice(buf, size)));
    modify(&file, |ini| ini.set(&sec, &key, v.as_deref()))
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
    put_list(c.a(0), c.a(1), &load(&f).unwrap_or_default().names(), false).0
}
fn GetPrivateProfileSectionNamesW(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(2), true);
    put_list(c.a(0), c.a(1), &load(&f).unwrap_or_default().names(), true).0
}
fn WritePrivateProfileSectionA(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(2), false);
    write_section(c, false, &f)
}
fn WritePrivateProfileSectionW(c: &mut Ctx) -> u32 {
    let f = arg_str(c.a(2), true);
    write_section(c, true, &f)
}
fn GetPrivateProfileStructA(c: &mut Ctx) -> u32 {
    get_struct(c, false)
}
fn GetPrivateProfileStructW(c: &mut Ctx) -> u32 {
    get_struct(c, true)
}
fn WritePrivateProfileStructA(c: &mut Ctx) -> u32 {
    write_struct(c, false)
}
fn WritePrivateProfileStructW(c: &mut Ctx) -> u32 {
    write_struct(c, true)
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
        ("WritePrivateProfileSectionA", WritePrivateProfileSectionA),
        ("WritePrivateProfileSectionW", WritePrivateProfileSectionW),
        ("GetPrivateProfileStructA", GetPrivateProfileStructA),
        ("GetPrivateProfileStructW", GetPrivateProfileStructW),
        ("WritePrivateProfileStructA", WritePrivateProfileStructA),
        ("WritePrivateProfileStructW", WritePrivateProfileStructW),
    ] {
        implement("kernel32", n, f);
    }
}

#[cfg(test)]
#[path = "ini_tests.rs"]
mod tests;
