//! msi's AppSearch: the AppSearch table's properties set from what a signature finds
//! through CompLocator (an installed component), RegLocator (a registry value), IniLocator
//! (an .ini value) or DrLocator (a folder search), tried in that order. A signature listed in
//! the Signature table looks for a file (name, versions, sizes, languages); one that is not
//! looks for a folder.

use std::collections::HashSet;

use crate::msi_pkg::{query, Package};

/// msidbLocatorType: Directory 0, FileName 1, RawValue 2 (+ 0x10 64-bit).
const DIRECTORY: i32 = 0;
const FILENAME: i32 = 1;
const RAWVALUE: i32 = 2;

#[derive(Default)]
struct Sig {
    name: String,
    /// the file looked for (its long name); None: the signature is a folder
    file: Option<String>,
    min_ver: (u32, u32),
    max_ver: (u32, u32),
    min_size: u32,
    max_size: u32,
    langs: String,
}

fn q(s: &str) -> String {
    s.replace('\'', "")
}

/// "1.2.3.4" as (MS, LS).
pub fn parse_version(v: &str) -> (u32, u32) {
    let mut p = [0u32; 4];
    for (i, x) in v.split('.').take(4).enumerate() {
        p[i] = x.trim().parse::<u32>().unwrap_or(0) & 0xffff;
    }
    (p[0] << 16 | p[1], p[2] << 16 | p[3])
}

fn signature(pk: &Package, name: &str) -> Sig {
    let mut s = Sig { name: name.to_string(), ..Sig::default() };
    let Some(r) = query(&pk.db, &format!("SELECT * FROM `Signature` WHERE `Signature` = '{}'", q(name))).into_iter().next() else { return s };
    let f = r.get(2).text();
    if f.is_empty() {
        return s;
    }
    s.file = Some(f.rsplit_once('|').map_or(f.clone(), |x| x.1.to_string()));
    s.min_ver = parse_version(&r.get(3).text());
    s.max_ver = parse_version(&r.get(4).text());
    let int = |i: usize| match r.get(i) {
        msidb::record::Field::Int(v) if *v != msidb::NULL_INT && *v > 0 => *v as u32,
        _ => 0,
    };
    s.min_size = int(5);
    s.max_size = int(6);
    s.langs = r.get(9).text();
    s
}

/// Whether the Windows path exists: Some(is a folder).
fn kind(path: &str) -> Option<bool> {
    if path.is_empty() {
        return None;
    }
    let h = crate::files::resolve(&crate::files::full_path(path))?;
    Some(crate::files::is_dir(&h))
}

fn slash(p: &str) -> String {
    if p.ends_with('\\') { p.to_string() } else { format!("{p}\\") }
}

fn parent_dir(p: &str) -> String {
    let t = p.trim_end_matches('\\');
    match t.rfind('\\') {
        Some(i) => t[..=i].to_string(),
        None => String::new(),
    }
}

/// The file's version within the signature's bounds, and its languages among those named.
fn version_ok(sig: &Sig, path: &str) -> bool {
    let Some((ms, ls, langs)) = crate::misc::file_version(path) else { return false };
    let v = (ms, ls);
    if v < sig.min_ver {
        return false;
    }
    if sig.max_ver != (0, 0) && v > sig.max_ver {
        return false;
    }
    sig.langs.split(',').map(str::trim).filter(|x| !x.is_empty()).all(|x| {
        let id: u16 = x.parse().unwrap_or(0);
        id == 0 || langs.contains(&id)
    })
}

/// A file found by a locator (FileName type): itself when it matches the signature; for a
/// folder signature, the folder it is in.
fn check_file(sig: &Sig, path: &str) -> Option<String> {
    if sig.file.is_none() {
        let d = parent_dir(path);
        return (kind(&d) == Some(true)).then_some(d);
    }
    if kind(path)? {
        return None;
    }
    // (a file without a version resource matches; with one, its version must be in bounds)
    if crate::misc::file_version(path).is_none() {
        return Some(path.to_string());
    }
    let mut s2 = Sig { langs: String::new(), ..Sig::default() };
    s2.min_ver = sig.min_ver;
    s2.max_ver = sig.max_ver;
    version_ok(&s2, path).then(|| path.to_string())
}

fn size_of(path: &str) -> u32 {
    crate::files::resolve(&crate::files::full_path(path)).and_then(|h| kura::fs::metadata(h).ok()).map_or(0, |m| m.len() as u32)
}

/// A file in `dir` (and folders under it, `depth` levels) matching the signature.
fn find_in(sig: &Sig, file: &str, dir: &str, depth: i32) -> Option<String> {
    if depth < 0 {
        return None;
    }
    let dir = slash(dir);
    let host = crate::files::resolve(&crate::files::full_path(&dir))?;
    let mut subdirs = Vec::new();
    let mut found = None;
    let mut names: Vec<(String, bool)> = kura::fs::read_dir(&host).into_iter().flatten().flatten().map(|e| (e.file_name().to_string_lossy().to_string(), kura::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()))).collect();
    names.sort();
    for (n, is_dir) in names {
        if is_dir {
            subdirs.push(n);
        } else if found.is_none() && crate::files::wild(&file.to_lowercase(), &n.to_lowercase()) {
            let p = format!("{dir}{n}");
            let size = size_of(&p);
            let ok = (sig.min_size == 0 || size >= sig.min_size)
                && (sig.max_size == 0 || size <= sig.max_size)
                && (sig.min_ver == (0, 0) && sig.max_ver == (0, 0) || version_ok(sig, &p));
            if ok {
                found = Some(p);
            }
        }
    }
    if found.is_some() {
        return found;
    }
    subdirs.into_iter().find_map(|d| find_in(sig, file, &format!("{dir}{d}"), depth - 1))
}

/// A folder search: the file in `path` (to `depth` levels), or the folder itself for a
/// folder signature. A path without a drive is tried on the fixed drives (C:).
fn search_dir(sig: &Sig, path: &str, depth: i32) -> Option<String> {
    let b = path.as_bytes();
    let full = (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') || path.starts_with("\\\\");
    let path = if full { path.to_string() } else { format!("C:\\{path}") };
    let v = match &sig.file {
        Some(f) => find_in(sig, f, &path, depth),
        None => (kind(&path) == Some(true)).then(|| path.clone()),
    }?;
    Some(if kind(&v) == Some(true) { slash(&v) } else { v })
}

fn search_comp(pk: &Package, sig: &Sig) -> Option<String> {
    let r = query(&pk.db, &format!("SELECT * FROM `CompLocator` WHERE `Signature_` = '{}'", q(&sig.name))).into_iter().next()?;
    let (id, ty) = (r.get(2).text(), r.get(3).int());
    let path = crate::msi_install::locate_component(&id)?;
    let is_dir = kind(&path)?;
    let present = sig.file.is_some();
    if ty != DIRECTORY && present && !is_dir {
        return check_file(sig, &path);
    }
    if !present && (ty != DIRECTORY || is_dir) {
        return Some(if ty == FILENAME { parent_dir(&path) } else { slash(&path) });
    }
    if present {
        let file = query(&pk.db, &format!("SELECT `FileName` FROM `Signature` WHERE `Signature` = '{}'", q(&sig.name))).first().map(|r| r.get(1).text()).unwrap_or_default();
        let p = format!("{}{file}", slash(&path));
        return (kind(&p) == Some(false)).then_some(p);
    }
    None
}

/// A registry value as AppSearch writes it: a string as is ("#" doubled first), a DWORD as
/// "#n", binary as "#x" and hex digits, a multi-string with its NULs.
fn raw_value(ty: u32, data: &[u8]) -> Option<String> {
    let text = || {
        let u: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let n = u.iter().position(|&c| c == 0).unwrap_or(u.len());
        String::from_utf16_lossy(&u[..n])
    };
    Some(match ty {
        1 => {
            let s = text();
            if s.starts_with('#') { format!("#{s}") } else { s }
        }
        2 => crate::k32::expand_env(&text()),
        4 if data.len() >= 4 => format!("#{}", i32::from_le_bytes(data[..4].try_into().unwrap())),
        3 => format!("#x{}", data.iter().map(|b| format!("{b:02X}")).collect::<String>()),
        // (a NUL first, then the strings with their NULs)
        7 => {
            let u: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            format!("\0{}", String::from_utf16_lossy(&u))
        }
        _ => return None,
    })
}

fn search_reg(pk: &Package, sig: &Sig) -> Option<String> {
    let r = query(&pk.db, &format!("SELECT * FROM `RegLocator` WHERE `Signature_` = '{}'", q(&sig.name))).into_iter().next()?;
    let (root, ty) = (r.get(2).int(), r.get(5).int());
    let root = match root {
        0 => "hkcr",
        1 => "hkcu",
        2 => "hklm",
        3 => "hku",
        _ => return None,
    };
    let key = format!("{root}\\{}", pk.format(&r.get(3).text()));
    let (rty, data) = crate::misc::reg_get(&key, &pk.format(&r.get(4).text()))?;
    if data.is_empty() {
        return None;
    }
    let ty = if matches!(ty, msidb::NULL_INT) { 0 } else { ty & 0x0f };
    if ty == RAWVALUE {
        return raw_value(rty, &data);
    }
    let mut s = match rty {
        1 | 2 => raw_value(rty, &data)?,
        _ => return None,
    };
    // (a quoted path: what is in the quotes)
    if let Some(a) = s.find('"') {
        if let Some(b) = s[a + 1..].find('"') {
            s = s[a + 1..a + 1 + b].to_string();
        }
    }
    match ty {
        DIRECTORY => search_dir(sig, &s, 0),
        FILENAME => check_file(sig, &s),
        _ => None,
    }
}

fn search_ini(pk: &Package, sig: &Sig) -> Option<String> {
    let r = query(&pk.db, &format!("SELECT * FROM `IniLocator` WHERE `Signature_` = '{}'", q(&sig.name))).into_iter().next()?;
    let field = match r.get(5) {
        msidb::record::Field::Int(v) if *v != msidb::NULL_INT => *v,
        _ => 0,
    };
    let ty = match r.get(6) {
        msidb::record::Field::Int(v) if *v != msidb::NULL_INT => *v & 0x0f,
        _ => 0,
    };
    let v = crate::ini::read_entry(&r.get(2).text(), &r.get(3).text(), &r.get(4).text()).filter(|v| !v.is_empty())?;
    match ty {
        DIRECTORY => search_dir(sig, &v, 0),
        FILENAME => check_file(sig, &v),
        RAWVALUE if field == 0 => Some(v),
        // (field n of a comma-separated value, spaces after the commas dropped)
        RAWVALUE => {
            let parts: Vec<&str> = v.split(',').collect();
            let i = (field as usize - 1).min(parts.len() - 1);
            Some(if i == 0 { parts[0].to_string() } else { parts[i].trim_start_matches(' ').to_string() })
        }
        _ => None,
    }
}

fn search_drl(pk: &Package, sig: &Sig, seen: &mut HashSet<String>) -> Option<String> {
    let r = query(&pk.db, &format!("SELECT * FROM `DrLocator` WHERE `Signature_` = '{}'", q(&sig.name))).into_iter().next()?;
    if !seen.insert(sig.name.clone()) {
        return None;
    }
    let (parent, path) = (r.get(2).text(), r.get(3).text());
    let depth = match r.get(4) {
        msidb::record::Field::Int(v) if *v != msidb::NULL_INT => *v,
        _ => 0,
    };
    let path = if path.is_empty() { String::new() } else { pk.format(path.rsplit_once('|').map_or(&path[..], |x| x.1)) };
    let full = if parent.is_empty() {
        path
    } else {
        let mut p = search(pk, &parent, seen)?;
        if kind(&p) == Some(false) {
            p = parent_dir(&p);
        }
        format!("{p}{path}")
    };
    search_dir(sig, &if full.is_empty() { full } else { slash(&full) }, depth)
}

fn search(pk: &Package, name: &str, seen: &mut HashSet<String>) -> Option<String> {
    let sig = signature(pk, name);
    search_comp(pk, &sig).or_else(|| search_reg(pk, &sig)).or_else(|| search_ini(pk, &sig)).or_else(|| search_drl(pk, &sig, seen))
}

/// AppSearch (and CCPSearch's signatures): the properties found, in the table's order.
pub fn app_search(pk: &mut Package) {
    for r in query(&pk.db, "SELECT `Property`, `Signature_` FROM `AppSearch`") {
        let (prop, sig) = (r.get(1).text(), r.get(2).text());
        let mut seen = HashSet::new();
        if let Some(v) = search(pk, &sig, &mut seen) {
            pk.set_prop(&prop, &v);
            if prop == "SourceDir" {
                pk.resolve_sources();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("1.2.3.4"), (0x10002, 0x30004));
        assert_eq!(parse_version("2.1"), (0x20001, 0));
        assert_eq!(parse_version(""), (0, 0));
        assert!(parse_version("1.10.0.0") > parse_version("1.9.65535.65535"));
    }

    fn w(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// RegLocator raw values as Windows writes them into the property (Wine's
    /// test_appsearch_reglocator).
    #[test]
    fn raw_values() {
        assert_eq!(raw_value(1, &w("regszdata\0")).as_deref(), Some("regszdata"));
        assert_eq!(raw_value(1, &w("#regszdata\0")).as_deref(), Some("##regszdata"));
        assert_eq!(raw_value(4, &42u32.to_le_bytes()).as_deref(), Some("#42"));
        assert_eq!(raw_value(4, &(-42i32).to_le_bytes()).as_deref(), Some("#-42"));
        assert_eq!(raw_value(3, &[0x12, 0x34, 0xab, 0xcd]).as_deref(), Some("#x1234ABCD"));
        assert_eq!(raw_value(2, &w("my%NOVAR_MASU%\0")).as_deref(), Some("my%NOVAR_MASU%"));
        assert_eq!(raw_value(7, &w("one\0two\0\0")).as_deref(), Some("\0one\0two\0\0"));
        assert_eq!(raw_value(11, &[0; 8]), None);
    }

    #[test]
    fn paths() {
        assert_eq!(parent_dir("C:\\a\\b.txt"), "C:\\a\\");
        assert_eq!(parent_dir("C:\\a\\"), "C:\\");
        assert_eq!(slash("C:\\a"), "C:\\a\\");
    }
}
