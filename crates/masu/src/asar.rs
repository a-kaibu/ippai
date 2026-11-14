//! Electron games ship their app as resources/app.asar (Tyrano's Electron builds, e.g. the
//! marginal trial RJ01094090). The page runs Electron games from resources/app/ (web/src/html.ts),
//! so `masu pack` unpacks the archive there, as nwjs.rs does for NW.js apps.
//!
//! The format (electron/asar): u32 4, u32 header buffer size, then that buffer (a Chromium
//! pickle: u32 payload size, u32 string length, the JSON header), then the files' bytes from
//! 8 + header buffer size on. The header is {"files": {name: entry}} with an entry either
//! {"files": {...}} (a folder), {"offset": "<decimal>", "size": n} (a file; offsets are strings,
//! counted from the data's start), {"unpacked": true, "size": n} (a file kept beside the archive
//! in app.asar.unpacked/) or {"link": "..."} (a symlink: skipped).
//!
//! What the header says is not trusted: names that would leave the folder are skipped, a file
//! past the archive's end is an error, and what it unpacks to is capped (entries, bytes) as for
//! NW.js zips.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Files and folders a header may list.
const MAX_ENTRIES: usize = 200_000;
/// The JSON header's size (Electron's own are a few hundred KiB at most).
const MAX_HEADER: u64 = 64 << 20;
/// Folder depth.
const MAX_DEPTH: usize = 64;
/// Bytes it may unpack to: this many times the archive's size, at least `MIN_OUT`, at most `MAX_OUT`.
const RATIO: u64 = 2;
const MIN_OUT: u64 = 256 << 20;
const MAX_OUT: u64 = 16 << 30;

/// A file of the archive: where it goes (relative), and where its bytes are.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// bytes `offset..offset + size` of the data part
    Packed { offset: u64, size: u64 },
    /// app.asar.unpacked/<path>
    Unpacked,
}

/// The header of an asar archive: where its data starts and its files (folders as entries with
/// no source). None when `head` (the archive's first bytes, at least the whole header) is not one.
pub fn parse(head: &[u8], len: u64) -> Result<Option<(u64, Vec<(PathBuf, Option<Source>)>)>> {
    let u32_at = |o: usize| head.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as u64);
    let (Some(4), Some(buf), Some(payload), Some(slen)) = (u32_at(0), u32_at(4), u32_at(8), u32_at(12)) else { return Ok(None) };
    // (the pickle: its payload is the string's length and the string, padded to 4)
    if buf > MAX_HEADER || payload + 4 > buf || slen + 4 > payload || 8 + buf > len {
        return Ok(None);
    }
    let Some(json) = head.get(16..16 + slen as usize) else { return Ok(None) };
    let Ok(v) = serde_json::from_slice::<Value>(json) else { return Ok(None) };
    let Some(root) = v.get("files").and_then(Value::as_object) else { return Ok(None) };
    let data = 8 + buf;
    let mut out = Vec::new();
    let mut todo = vec![(PathBuf::new(), root, 0usize)];
    while let Some((dir, files, depth)) = todo.pop() {
        if depth > MAX_DEPTH {
            bail!("folders nested over {MAX_DEPTH} deep");
        }
        for (name, e) in files {
            // (one plain name: no separators, no "." or "..", nothing Windows can't hold)
            if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0', ':']) {
                continue;
            }
            if out.len() >= MAX_ENTRIES {
                bail!("more than {MAX_ENTRIES} files");
            }
            let path = dir.join(name);
            if let Some(sub) = e.get("files").and_then(Value::as_object) {
                out.push((path.clone(), None));
                todo.push((path, sub, depth + 1));
            } else if e.get("unpacked").and_then(Value::as_bool) == Some(true) {
                out.push((path, Some(Source::Unpacked)));
            } else if e.get("link").is_some() {
                continue;
            } else {
                let offset = match e.get("offset") {
                    Some(Value::String(s)) => s.parse::<u64>().ok(),
                    Some(Value::Number(n)) => n.as_u64(),
                    _ => None,
                };
                let size = e.get("size").and_then(Value::as_u64);
                let (Some(offset), Some(size)) = (offset, size) else { bail!("{}: no offset or size", path.display()) };
                if offset.checked_add(size).and_then(|e| e.checked_add(data)).is_none_or(|end| end > len) {
                    bail!("{}: past the archive's end", path.display());
                }
                out.push((path, Some(Source::Packed { offset, size })));
            }
        }
    }
    Ok(Some((data, out)))
}

/// Unpacks `file` (an app.asar) into `dir` when it is an Electron app (a package.json at its
/// top); returns whether it did.
pub fn unpack(file: &Path, dir: &Path) -> Result<bool> {
    let mut f = File::open(file)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 16];
    if f.read_exact(&mut head).is_err() {
        return Ok(false);
    }
    let buf = u32::from_le_bytes(head[4..8].try_into().unwrap()) as u64;
    if u32::from_le_bytes(head[..4].try_into().unwrap()) != 4 || buf > MAX_HEADER || 8 + buf > len {
        return Ok(false);
    }
    let mut whole = vec![0u8; 8 + buf as usize];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut whole)?;
    let Some((data, files)) = parse(&whole, len).with_context(|| file.display().to_string())? else { return Ok(false) };
    if !files.iter().any(|(p, s)| s.is_some() && p.as_os_str() == "package.json") {
        return Ok(false);
    }
    let beside = {
        let mut s = file.as_os_str().to_os_string();
        s.push(".unpacked");
        PathBuf::from(s)
    };
    let cap = len.saturating_mul(RATIO).clamp(MIN_OUT, MAX_OUT);
    let mut left = cap;
    for (path, src) in files {
        let out = dir.join(&path);
        match src {
            None => fs::create_dir_all(&out)?,
            Some(src) => {
                if let Some(p) = out.parent() {
                    fs::create_dir_all(p)?;
                }
                let n = match src {
                    Source::Packed { offset, size } => {
                        if size > left {
                            bail!("{}: unpacks to more than {} MiB", file.display(), cap >> 20);
                        }
                        f.seek(SeekFrom::Start(data + offset))?;
                        io::copy(&mut (&mut f).take(size), &mut File::create(&out)?)?
                    }
                    // (missing beside the archive: left out, as Electron would fail to read it)
                    Source::Unpacked => match File::open(beside.join(&path)) {
                        Ok(mut u) => io::copy(&mut (&mut u).take(left + 1), &mut File::create(&out)?)?,
                        Err(_) => continue,
                    },
                };
                if n > left {
                    bail!("{}: unpacks to more than {} MiB", file.display(), cap >> 20);
                }
                left -= n;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// An asar archive of `files` (path, bytes; "dir/" ends a folder), as electron/asar writes it.
    pub fn asar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut root = serde_json::Map::new();
        let mut data = Vec::new();
        for (p, b) in files {
            let parts: Vec<&str> = p.trim_end_matches('/').split('/').collect();
            let mut m = &mut root;
            for d in &parts[..parts.len() - 1] {
                let e = m.entry(d.to_string()).or_insert_with(|| serde_json::json!({"files": {}}));
                m = e.get_mut("files").unwrap().as_object_mut().unwrap();
            }
            let last = parts[parts.len() - 1].to_string();
            if p.ends_with('/') {
                m.entry(last).or_insert_with(|| serde_json::json!({"files": {}}));
            } else {
                m.insert(last, serde_json::json!({"size": b.len(), "offset": data.len().to_string()}));
                data.extend_from_slice(b);
            }
        }
        with_header(&serde_json::json!({"files": root}).to_string(), &data)
    }

    pub fn with_header(json: &str, data: &[u8]) -> Vec<u8> {
        let s = json.as_bytes();
        let padded = (s.len() + 3) & !3;
        let payload = 4 + padded;
        let buf = 4 + payload;
        let mut v = Vec::new();
        v.extend(4u32.to_le_bytes());
        v.extend((buf as u32).to_le_bytes());
        v.extend((payload as u32).to_le_bytes());
        v.extend((s.len() as u32).to_le_bytes());
        v.extend(s);
        v.resize(8 + buf, 0);
        v.extend(data);
        v
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("masu-asar-{name}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn tree(d: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut todo = vec![d.to_path_buf()];
        while let Some(x) = todo.pop() {
            for e in fs::read_dir(&x).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    todo.push(p);
                } else {
                    out.push((p.strip_prefix(d).unwrap().to_string_lossy().into_owned(), fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn unpacks_an_electron_app() {
        let d = tmp("ok");
        let a = asar(&[("package.json", b"{\"main\":\"main.js\"}"), ("index.html", b"<html>"), ("tyrano/libs/a.js", b"js"), ("empty/", b"")]);
        fs::write(d.join("app.asar"), &a).unwrap();
        assert!(unpack(&d.join("app.asar"), &d.join("app")).unwrap());
        assert_eq!(tree(&d.join("app")), vec![
            ("index.html".into(), b"<html>".to_vec()),
            ("package.json".into(), b"{\"main\":\"main.js\"}".to_vec()),
            ("tyrano/libs/a.js".into(), b"js".to_vec()),
        ]);
        assert!(d.join("app/empty").is_dir());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unpacked_files_come_from_beside_it() {
        let d = tmp("beside");
        let json = r#"{"files":{"package.json":{"size":2,"offset":"0"},"bin":{"files":{"x.node":{"size":3,"unpacked":true},"gone.node":{"size":1,"unpacked":true}}},"l":{"link":"../x"}}}"#;
        fs::write(d.join("app.asar"), with_header(json, b"{}")).unwrap();
        fs::create_dir_all(d.join("app.asar.unpacked/bin")).unwrap();
        fs::write(d.join("app.asar.unpacked/bin/x.node"), b"abc").unwrap();
        assert!(unpack(&d.join("app.asar"), &d.join("app")).unwrap());
        assert_eq!(tree(&d.join("app")), vec![("bin/x.node".into(), b"abc".to_vec()), ("package.json".into(), b"{}".to_vec())]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn not_an_app_or_not_an_archive() {
        let d = tmp("no");
        fs::write(d.join("a.asar"), asar(&[("readme.txt", b"no package.json")])).unwrap();
        assert!(!unpack(&d.join("a.asar"), &d.join("out")).unwrap());
        fs::write(d.join("b.asar"), b"PK\x03\x04 not asar").unwrap();
        assert!(!unpack(&d.join("b.asar"), &d.join("out")).unwrap());
        fs::write(d.join("c.asar"), b"\x04\0\0\0").unwrap();
        assert!(!unpack(&d.join("c.asar"), &d.join("out")).unwrap());
        assert!(!d.join("out").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn names_that_leave_the_folder_are_skipped() {
        let d = tmp("evil");
        let json = r#"{"files":{"package.json":{"size":2,"offset":"0"},"..":{"files":{"evil.txt":{"size":1,"offset":"0"}}},"a/../../b":{"size":1,"offset":"0"},"c\\..\\d":{"size":1,"offset":"0"},"C:x":{"size":1,"offset":"0"}}}"#;
        fs::write(d.join("app.asar"), with_header(json, b"{}")).unwrap();
        assert!(unpack(&d.join("app.asar"), &d.join("x/app")).unwrap());
        assert_eq!(tree(&d), vec![("app.asar".into(), with_header(json, b"{}")), ("x/app/package.json".into(), b"{}".to_vec())]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_past_the_end_is_an_error() {
        let d = tmp("past");
        let json = r#"{"files":{"package.json":{"size":2,"offset":"0"},"big":{"size":100,"offset":"0"}}}"#;
        fs::write(d.join("app.asar"), with_header(json, b"{}")).unwrap();
        assert!(unpack(&d.join("app.asar"), &d.join("app")).is_err());
        let json = r#"{"files":{"package.json":{"size":2,"offset":"18446744073709551615"}}}"#;
        fs::write(d.join("app.asar"), with_header(json, b"{}")).unwrap();
        assert!(unpack(&d.join("app.asar"), &d.join("app")).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        /// Any set of files round-trips: what comes out is what went in.
        #[test]
        fn round_trip(files in prop::collection::btree_map("[a-z]{1,6}(/[a-z]{1,6}){0,2}", prop::collection::vec(any::<u8>(), 0..300), 1..8)) {
            // (a path can't be both a file and a folder)
            let mut files: Vec<(String, Vec<u8>)> = files.into_iter().collect();
            let names: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
            files.retain(|(n, _)| !names.iter().any(|m| m.starts_with(&format!("{n}/"))));
            files.push(("package.json".into(), b"{}".to_vec()));
            files.sort();
            files.dedup_by(|a, b| a.0 == b.0);
            let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
            let d = tmp("rt");
            fs::write(d.join("app.asar"), asar(&refs)).unwrap();
            prop_assert!(unpack(&d.join("app.asar"), &d.join("app")).unwrap());
            let got = tree(&d.join("app"));
            let _ = fs::remove_dir_all(&d);
            prop_assert_eq!(got, files);
        }

        /// Damaged archives: an error or a result, never a panic or a file outside the folder.
        #[test]
        fn damaged(edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..8), cut in any::<usize>()) {
            let mut a = asar(&[("package.json", b"{}"), ("www/index.html", b"<html>"), ("www/js/a.js", b"x")]);
            for (at, b) in edits {
                let i = at % a.len();
                a[i] = b;
            }
            a.truncate(cut % (a.len() + 1));
            let d = tmp("dmg");
            fs::write(d.join("app.asar"), &a).unwrap();
            let _ = unpack(&d.join("app.asar"), &d.join("x/app"));
            let outside: Vec<String> = tree(&d).into_iter().map(|t| t.0).filter(|n| n != "app.asar" && !n.starts_with("x/app/")).collect();
            let _ = fs::remove_dir_all(&d);
            prop_assert!(outside.is_empty(), "{:?}", outside);
        }
    }
}
