//! A game's download for `ippai add`: zip archives (Shift-JIS names), InstallShield setups that
//! carry their files in the exe, zlib-stream setups (WILL's PrincessWaltz_WebTrial2.exe) and
//! self-extracting zips here; lzh, rar, 7z, Inno Setup and Windows Installer packages through the
//! tools when they are installed (7z, unar, unrar, innoextract, msiextract or msiinfo +
//! cabextract). Installers found inside are unpacked too, rather than run in the browser (an
//! installer writing a 1 GB package into the in-memory file system runs out of wasm memory).
//! The same steps as tests/corpus-prep.sh and its scripts.

use anyhow::{bail, Context, Result};
use flate2::bufread::ZlibDecoder;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// What `unpack` takes: an archive or an installer (not a folder).
pub fn is_package(p: &Path) -> bool {
    p.is_file() && ["zip", "exe", "lzh", "lha", "rar", "7z", "cab", "msi"].contains(&ext(p).as_str())
}

/// Unpacks `src` into the folder `out` (made here, empty), then the installers in it.
pub fn unpack(src: &Path, out: &Path) -> Result<()> {
    fs::create_dir_all(out)?;
    // (absolute: the tools run in the folder they unpack into)
    let (src, out) = (&src.canonicalize()?, &out.canonicalize()?);
    let e = ext(src);
    let ours: &[fn(&Path, &Path) -> Result<bool>] = match e.as_str() {
        "zip" => &[unzip],
        "exe" => &[installshield, zsfx, unzip],
        _ => &[],
    };
    let mut done = false;
    for f in ours {
        match f(src, out) {
            Ok(true) if !empty(out) => {
                done = true;
                break;
            }
            Ok(_) => {}
            Err(err) => eprintln!("  {}: {err:#}", src.display()),
        }
        clear(out)?;
    }
    if !done && !tools(src, out, &e)? {
        bail!("{}: not an archive or installer this can unpack (lzh, rar, 7z, Inno Setup and msi need 7z, unar, innoextract or msiextract installed)", src.display());
    }
    fix_names(out)?;
    nested(out)?;
    Ok(())
}

/// The program to start, relative to `root` with `/`: `hint` (also found one folder further
/// down, as `*/hint`), else the biggest .exe that is not a tool or installer; cs2.exe (CatSystem2's
/// game behind its launcher) over that.
pub fn pick_exe(root: &Path, hint: Option<&str>) -> Option<String> {
    let mut exes = vec![];
    walk(root, &mut |p| {
        if ext(p) == "exe" {
            let rel = p.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            exes.push((p.metadata().ok()?.len(), rel));
        }
        None::<()>
    });
    if let Some(h) = hint {
        let h = h.replace('\\', "/");
        let tail = format!("/{}", h.to_lowercase());
        return exes.iter().map(|(_, r)| r).find(|r| **r == h).or_else(|| exes.iter().map(|(_, r)| r).find(|r| r.to_lowercase().ends_with(&tail) || r.eq_ignore_ascii_case(&h))).cloned();
    }
    if let Some((_, r)) = exes.iter().find(|(_, r)| r.rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case("cs2.exe"))) {
        return Some(r.clone());
    }
    let tool = regex::Regex::new(r"(?i)unins|setup|install|config|WindowsInstaller|InstMsi|KB[0-9]{6}|vcredist|vc_redist|redist|directx|dxsetup|tool|設定|update|DXWeb|dotnet").unwrap();
    exes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // (by the file's name: setup.exe.is/Disk1/BGI.exe is a game)
    exes.into_iter().find(|(_, r)| !tool.is_match(r.rsplit('/').next().unwrap_or(r))).map(|(_, r)| r)
}

fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn empty(d: &Path) -> bool {
    fs::read_dir(d).map_or(true, |mut r| r.next().is_none())
}

fn clear(d: &Path) -> Result<()> {
    for e in fs::read_dir(d)? {
        let p = e?.path();
        if p.is_dir() { fs::remove_dir_all(&p)? } else { fs::remove_file(&p)? }
    }
    Ok(())
}

/// Calls `f` on every file under `d` (not following links); stops at the first Some.
fn walk<T>(d: &Path, f: &mut dyn FnMut(&Path) -> Option<T>) -> Option<T> {
    let mut v: Vec<_> = fs::read_dir(d).ok()?.filter_map(|e| e.ok()).collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let p = e.path();
        let t = e.file_type().ok()?;
        let r = if t.is_dir() { walk(&p, f) } else if t.is_file() { f(&p) } else { None };
        if r.is_some() {
            return r;
        }
    }
    None
}

/// `out`/`name` for a name from a package (`\` or `/` separated), or None for one that would
/// land outside `out` (`..`, an absolute path, a drive).
fn safe_path(out: &Path, name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let mut p = out.to_path_buf();
    let mut any = false;
    for c in Path::new(&name).components() {
        match c {
            Component::Normal(s) if !s.as_bytes().contains(&b':') => {
                p.push(s);
                any = true;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    any.then_some(p)
}

/// Writes `r` to `out`/`name`; returns the size (None: a name outside `out`, skipped).
fn save(out: &Path, name: &str, r: &mut dyn Read) -> Result<Option<u64>> {
    let Some(p) = safe_path(out, name) else {
        eprintln!("  {name:?}: outside the folder, skipped");
        return Ok(None);
    };
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    let mut f = File::create(&p).with_context(|| p.display().to_string())?;
    Ok(Some(io::copy(r, &mut f)?))
}

fn cp932(b: &[u8]) -> String {
    encoding_rs::SHIFT_JIS.decode_without_bom_handling(b).0.into_owned()
}

// --- zip ---

/// A zip (also one at the end of an exe): names not in UTF-8 are Shift-JIS, as Japanese zips' are.
fn unzip(src: &Path, out: &Path) -> Result<bool> {
    let mut z = zip::ZipArchive::new(BufReader::new(File::open(src)?))?;
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        let raw = e.name_raw();
        let name = std::str::from_utf8(raw).map(String::from).unwrap_or_else(|_| cp932(raw));
        if name.ends_with('/') || name.ends_with('\\') {
            if let Some(p) = safe_path(out, &name) {
                fs::create_dir_all(p)?;
            }
            continue;
        }
        save(out, &name, &mut e)?;
    }
    Ok(true)
}

/// A self-extracting zip of a game: one that holds an .exe (not a program that only carries a
/// zip of its own data, as NW.js apps do).
fn game_sfx(src: &Path) -> bool {
    let Ok(z) = File::open(src).map(BufReader::new).map_err(anyhow::Error::from).and_then(|f| Ok(zip::ZipArchive::new(f)?)) else {
        return false;
    };
    let any = z.file_names().any(|n| n.to_lowercase().ends_with(".exe"));
    any
}

// --- InstallShield (tests/isextract.py) ---

/// Where a PE's image ends in the file (its overlay's start).
fn pe_end(f: &mut File) -> Option<u64> {
    let mut d = vec![0; 4096];
    f.seek(SeekFrom::Start(0)).ok()?;
    let n = f.read(&mut d).ok()?;
    d.truncate(n);
    let u16_at = |o: usize| Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?));
    if d.get(..2) != Some(b"MZ") {
        return None;
    }
    let pe = u32_at(0x3c)? as usize;
    if d.get(pe..pe + 4) != Some(b"PE\0\0") {
        return None;
    }
    let (ns, opt) = (u16_at(pe + 6)? as usize, u16_at(pe + 20)? as usize);
    let sec = pe + 24 + opt;
    (0..ns).map(|i| Some(u32_at(sec + 40 * i + 16)? as u64 + u32_at(sec + 40 * i + 20)? as u64)).try_fold(0, |m, e| Some(e?.max(m)))
}

/// InstallShield's obfuscation: byte b at offset i -> !(k ^ rotate4(b)), k the name-derived
/// key's byte (i % 1024) % len (the key restarts every 1024 bytes).
struct Unmask<R> {
    r: R,
    key: Vec<u8>,
    pos: usize,
}

const IS_MASK: [u8; 4] = [0x13, 0x35, 0x86, 0x07];

impl<R> Unmask<R> {
    fn new(r: R, name: &[u8]) -> Self {
        Unmask { r, key: name.iter().enumerate().map(|(i, c)| c ^ IS_MASK[i % 4]).collect(), pos: 0 }
    }
}

impl<R: Read> Read for Unmask<R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let n = self.r.read(b)?;
        for x in &mut b[..n] {
            let k = self.key[self.pos % 1024 % self.key.len()];
            *x = !(k ^ x.rotate_left(4));
            self.pos += 1;
        }
        Ok(n)
    }
}

/// One entry's data: unmasked when `flags & 6`, inflated when it then starts as zlib.
fn is_entry(f: &mut File, at: u64, len: u64, flags: u32, key: &[u8], out: &Path, name: &str) -> Result<()> {
    f.seek(SeekFrom::Start(at))?;
    let raw = f.take(len);
    let r: Box<dyn Read + '_> = if flags & 6 != 0 && !key.is_empty() { Box::new(Unmask::new(raw, key)) } else { Box::new(raw) };
    let mut r = BufReader::with_capacity(1 << 20, r);
    let zlib = r.fill_buf()?.starts_with(b"x\x9c");
    let n = if zlib { save(out, name, &mut ZlibDecoder::new(r))? } else { save(out, name, &mut r)? };
    eprintln!("  {name} {}", n.unwrap_or(0));
    Ok(())
}

fn installshield(src: &Path, out: &Path) -> Result<bool> {
    let mut f = File::open(src)?;
    let size = f.metadata()?.len();
    let Some(end) = pe_end(&mut f) else { return Ok(false) };
    if end >= size {
        return Ok(false);
    }
    let mut tail = vec![0; 4096];
    f.seek(SeekFrom::Start(end))?;
    let n = f.read(&mut tail)?;
    tail.truncate(n);
    let find = |s: &[u8]| tail.windows(s.len()).position(|w| w == s);
    // (some setups have debug info between the image and the stream)
    let at = [find(b"ISSetupStream\0"), find(b"InstallShield\0")].into_iter().flatten().min().unwrap_or(0) as u64;
    let mut h = [0u8; 46];
    f.seek(SeekFrom::Start(end + at))?;
    if f.read_exact(&mut h).is_err() {
        return Ok(false);
    }
    let count = u16::from_le_bytes([h[14], h[15]]);
    let mut pos = end + at + 46;
    let mut rd = |pos: u64, n: usize| -> Result<Vec<u8>> {
        let mut b = vec![0; n];
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut b).context("truncated setup")?;
        Ok(b)
    };
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let mut entries = vec![];
    if &h[..14] == b"ISSetupStream\0" {
        let ver = u16::from_le_bytes([h[16], h[17]]);
        for _ in 0..count {
            // (version 4 on: 64-bit sizes, 48-byte entry headers)
            let (hl, b) = if ver >= 4 { (48, rd(pos, 18)?) } else { (24, rd(pos, 14)?) };
            let (nl, flags) = (u32_at(&b, 0) as usize, u32_at(&b, 4));
            let len = if ver >= 4 { u64::from_le_bytes(b[10..18].try_into().unwrap()) } else { u32_at(&b, 10) as u64 };
            let nb = rd(pos + hl, nl)?;
            let u: Vec<u16> = nb.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let name = String::from_utf16_lossy(&u);
            let data = pos + hl + nl as u64;
            entries.push((data, len, flags, name.as_bytes().to_vec(), name));
            pos = data + len;
        }
    } else if &h[..14] == b"InstallShield\0" {
        for _ in 0..count {
            let a = rd(pos, 312)?;
            let raw = &a[..a[..260].iter().position(|&c| c == 0).unwrap_or(260)];
            let (flags, len) = (u32_at(&a, 260), u32_at(&a, 268) as u64);
            entries.push((pos + 312, len, flags, raw.to_vec(), cp932(raw)));
            pos += 312 + len;
        }
    } else {
        return is_plain(&mut f, end, size, out);
    }
    if pos > size {
        bail!("truncated setup");
    }
    for (at, len, flags, key, name) in entries {
        is_entry(&mut f, at, len, flags, &key, out, &name)?;
    }
    Ok(count > 0)
}

/// Older setups (ojyonana_trial.exe): name, path, version and decimal size as NUL-ended
/// strings, then the file as is; the path (Disk1\AdvHD.exe) is where the file goes.
fn is_plain(f: &mut File, end: u64, size: u64, out: &Path) -> Result<bool> {
    let mut pos = end;
    while pos < size {
        let mut h = vec![0; 1024];
        f.seek(SeekFrom::Start(pos))?;
        let n = f.read(&mut h)?;
        let parts: Vec<&[u8]> = h[..n].splitn(5, |&c| c == 0).collect();
        if parts.len() < 5 || parts[3].is_empty() || !parts[3].iter().all(u8::is_ascii_digit) || parts[1].is_empty() {
            return Ok(pos > end);
        }
        let len: u64 = std::str::from_utf8(parts[3])?.parse()?;
        pos += parts[..4].iter().map(|p| p.len() as u64 + 1).sum::<u64>();
        if pos + len > size {
            bail!("truncated setup");
        }
        let name = cp932(parts[1]);
        f.seek(SeekFrom::Start(pos))?;
        save(out, &name, &mut (&mut *f).take(len))?;
        eprintln!("  {name} {len}");
        pos += len;
    }
    Ok(pos > end)
}

// --- zlib-stream setups (tests/zsfx.py) ---

/// The next offset at or after `pos` where `pat` starts.
fn find_from(f: &mut File, mut pos: u64, pat: &[u8]) -> Result<Option<u64>> {
    let mut b = vec![0; 1 << 20];
    loop {
        f.seek(SeekFrom::Start(pos))?;
        let n = f.read(&mut b)?;
        if n < pat.len() {
            return Ok(None);
        }
        if let Some(i) = b[..n].windows(pat.len()).position(|w| w == pat) {
            return Ok(Some(pos + i as u64));
        }
        pos += (n - pat.len() + 1) as u64;
    }
}

/// Inflates the zlib stream at `pos` into `w`; the compressed size, None when it's no stream.
fn inflate_at(f: &mut File, pos: u64, w: &mut dyn Write) -> Result<Option<u64>> {
    f.seek(SeekFrom::Start(pos))?;
    let mut z = ZlibDecoder::new(BufReader::with_capacity(1 << 20, &mut *f));
    match io::copy(&mut z, w) {
        Ok(_) => Ok(Some(z.total_in())),
        Err(e) if e.kind() == io::ErrorKind::InvalidInput || e.kind() == io::ErrorKind::InvalidData || e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The first `cap` bytes written, and the count.
struct Head {
    buf: Vec<u8>,
    cap: usize,
}

impl Write for Head {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = b.len().min(self.cap - self.buf.len());
        self.buf.extend_from_slice(&b[..n]);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const ZLIB9: &[u8] = b"\x78\xda";

/// A setup.exe that carries its files as zlib streams after an INI header (Ver=, AppName=,
/// FileCount=, FileN=name,dir,No,size,...): one stream per file, in FileN order, each as big
/// as the header says.
fn zsfx(src: &Path, out: &Path) -> Result<bool> {
    let mut f = File::open(src)?;
    let mut pos = 0;
    let ini = loop {
        let Some(i) = find_from(&mut f, pos, ZLIB9)? else { return Ok(false) };
        let mut h = Head { buf: vec![], cap: 1 << 20 };
        match inflate_at(&mut f, i, &mut h)? {
            Some(n) => {
                pos = i + n;
                if h.buf.windows(10).any(|w| w == b"FileCount=") && h.buf.windows(7).any(|w| w == b"\nFile0=") {
                    break cp932(&h.buf);
                }
            }
            None => pos = i + 2,
        }
    };
    // sub folders made with MAKESUBDIR: their last path part
    let sub: Vec<String> = regex::Regex::new(r#"(?m)^Val\d+=.*?MAKESUBDIR,"([^"]*)""#)
        .unwrap()
        .captures_iter(&ini)
        .map(|c| c[1].trim_end_matches('"').rsplit('\\').next().unwrap_or("").to_string())
        .collect();
    let files: Vec<(String, u64)> = regex::Regex::new(r"(?m)^File\d+=([^,\r\n]*),([^,\r\n]*),[^,\r\n]*,(\d+),")
        .unwrap()
        .captures_iter(&ini)
        .map(|c| {
            let name = if c[2].contains("MAKESUBDIR") && !sub.is_empty() { format!("{}/{}", sub[0], &c[1]) } else { c[1].to_string() };
            (name, c[3].parse().unwrap_or(u64::MAX))
        })
        .collect();
    for (name, size) in &files {
        let Some(p) = safe_path(out, name) else { bail!("{name:?}: outside the folder") };
        fs::create_dir_all(p.parent().unwrap())?;
        loop {
            let Some(i) = find_from(&mut f, pos, ZLIB9)? else { bail!("{name}: no stream left") };
            let mut w = io::BufWriter::new(File::create(&p)?);
            match inflate_at(&mut f, i, &mut w)? {
                Some(n) => {
                    w.flush()?;
                    let got = p.metadata()?.len();
                    if got != *size {
                        bail!("{name}: {got} bytes, the list says {size}");
                    }
                    pos = i + n;
                    eprintln!("  {name} {size}");
                    break;
                }
                None => pos = i + 2,
            }
        }
    }
    Ok(!files.is_empty())
}

// --- the tools ---

/// Runs a tool (missing: false); true when it exited 0.
fn run(tool: &str, args: &[&OsStr], cwd: &Path) -> bool {
    Command::new(tool)
        .args(args)
        .current_dir(cwd)
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Unpacks with the installed tools; true when one left files in `out`.
fn tools(src: &Path, out: &Path, e: &str) -> Result<bool> {
    let src = &src.canonicalize()?;
    let s = src.as_os_str();
    let o = out.as_os_str();
    let mut o_slash = o.to_os_string();
    o_slash.push("/");
    let o7 = {
        let mut x = std::ffi::OsString::from("-o");
        x.push(o);
        x
    };
    let sz = || run("7z", &["x".as_ref(), "-y".as_ref(), o7.as_os_str(), s], out);
    // (on an exe only a 7z self-extractor: 7z otherwise unpacks any PE's sections)
    let sz_sfx = || run("7z", &["x".as_ref(), "-y".as_ref(), "-t7z".as_ref(), o7.as_os_str(), s], out);
    let unar = |cp: bool| {
        let mut a: Vec<&OsStr> = vec!["-q".as_ref(), "-f".as_ref()];
        if cp {
            a.extend::<[&OsStr; 2]>(["-e".as_ref(), "cp932".as_ref()]);
        }
        a.extend::<[&OsStr; 3]>(["-o".as_ref(), o, s]);
        run("unar", &a, out)
    };
    let unrar = || run("unrar", &["x".as_ref(), "-y".as_ref(), s, o_slash.as_os_str()], out);
    let inno = || run("innoextract", &["-s".as_ref(), "-q".as_ref(), "-e".as_ref(), "-d".as_ref(), o, s], out);
    // (an archive with a file or two failing, e.g. a damaged Thumbs.db, still gives a game; an
    // exe has to unpack without errors)
    let steps: Vec<(Box<dyn Fn() -> bool + '_>, bool)> = match e {
        // (unar leaves some -lh5- members of PrismRhythm_trial_Web.lzh empty; 7z reads them)
        "lzh" | "lha" => vec![(Box::new(sz), false), (Box::new(|| unar(true)), false)],
        "zip" => vec![(Box::new(|| unar(true)), false), (Box::new(sz), false)],
        "rar" | "7z" | "cab" => vec![(Box::new(|| unar(false)), false), (Box::new(sz), false), (Box::new(unrar), false)],
        "exe" => vec![(Box::new(inno), true), (Box::new(|| unar(true)), true), (Box::new(sz_sfx), true), (Box::new(unrar), true)],
        "msi" => return msi(src, out),
        _ => vec![],
    };
    for (step, strict) in steps {
        let ok = step();
        if (ok || !strict) && !empty(out) {
            return Ok(true);
        }
        clear(out)?;
    }
    Ok(false)
}

/// A Windows Installer package: msiextract, else (packages split over several cabinets, which
/// msiextract can't do) every embedded cabinet through cabextract and each file moved to its
/// install path from the Directory/Component/File tables (tests/msiunpack.py).
fn msi(src: &Path, out: &Path) -> Result<bool> {
    if run("msiextract", &["-C".as_ref(), out.as_os_str(), src.as_os_str()], out) && !empty(out) {
        return Ok(true);
    }
    clear(out)?;
    let table = |name: &str| -> Vec<Vec<String>> {
        let Ok(o) = Command::new("msiinfo").args(["export".as_ref(), src.as_os_str(), name.as_ref()]).env("LC_ALL", "C.UTF-8").stderr(Stdio::null()).output() else { return vec![] };
        if !o.status.success() {
            return vec![];
        }
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n").lines().skip(3).filter(|l| !l.is_empty()).map(|l| l.split('\t').map(String::from).collect()).collect()
    };
    let long = |n: &str| n.rsplit('|').next().unwrap_or("").to_string();
    let dirs: HashMap<String, (String, String)> = table("Directory").into_iter().filter(|r| r.len() >= 3).map(|r| (r[0].clone(), (r[1].clone(), r[2].clone()))).collect();
    let comps: HashMap<String, String> = table("Component").into_iter().filter(|r| r.len() >= 3).map(|r| (r[0].clone(), r[2].clone())).collect();
    let files: HashMap<String, (String, String)> = table("File").into_iter().filter(|r| r.len() >= 3).map(|r| (r[0].clone(), (r[1].clone(), long(&r[2])))).collect();
    fn path(d: &str, dirs: &HashMap<String, (String, String)>, depth: usize, long: &dyn Fn(&str) -> String) -> String {
        let Some((parent, default)) = dirs.get(d) else { return String::new() };
        if depth > 64 || d == "TARGETDIR" || parent.is_empty() || parent == d {
            return String::new();
        }
        // the system folders, named as msiextract names them
        let sys = [("ProgramFilesFolder", "Program Files"), ("LocalAppDataFolder", "LocalAppData"), ("AppDataFolder", "AppData"), ("CommonAppDataFolder", "CommonAppData")];
        if let Some((_, s)) = sys.iter().find(|(k, _)| *k == d) {
            return s.to_string();
        }
        // DefaultDir: "target:source" -> target
        let name = long(default.split(':').next().unwrap_or(""));
        let p = path(parent, dirs, depth + 1, long);
        if name == "." || name == "SourceDir" { p } else if p.is_empty() { name } else { format!("{p}/{name}") }
    }
    let Ok(o) = Command::new("msiinfo").args(["streams".as_ref(), src.as_os_str()]).output() else { return Ok(false) };
    let tmp = out.join(".msi-cabs");
    fs::create_dir_all(tmp.join("files"))?;
    // (a cabinet set spans several streams: keep their names so cabextract can follow the set,
    // and let the ones that continue a set fail on their own)
    let cabs: Vec<String> = String::from_utf8_lossy(&o.stdout).split_whitespace().filter(|s| s.to_lowercase().ends_with(".cab") && !s.contains('/')).map(String::from).collect();
    for c in &cabs {
        let f = File::create(tmp.join(c))?;
        Command::new("msiinfo").args(["extract".as_ref(), src.as_os_str(), c.as_ref()]).stdout(f).status()?;
    }
    for c in &cabs {
        run("cabextract", &["-q".as_ref(), "-d".as_ref(), tmp.join("files").as_os_str(), tmp.join(c).as_os_str()], out);
    }
    let mut n = 0;
    for e in fs::read_dir(tmp.join("files"))? {
        let e = e?;
        let Some((comp, name)) = e.file_name().to_str().and_then(|k| files.get(k)) else { continue };
        let rel = path(comps.get(comp).map_or("", |s| s), &dirs, 0, &long);
        let Some(dst) = safe_path(out, &if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") }) else { continue };
        fs::create_dir_all(dst.parent().unwrap())?;
        fs::rename(e.path(), dst)?;
        n += 1;
    }
    fs::remove_dir_all(&tmp)?;
    Ok(n > 0)
}

// --- after unpacking ---

/// Names some tools leave in Shift-JIS bytes -> UTF-8.
fn fix_names(d: &Path) -> Result<()> {
    for e in fs::read_dir(d)? {
        let e = e?;
        let mut p = e.path();
        let n = e.file_name();
        if std::str::from_utf8(n.as_bytes()).is_err() {
            let to = d.join(cp932(n.as_bytes()));
            if !to.exists() {
                fs::rename(&p, &to)?;
                p = to;
            }
        }
        if e.file_type()?.is_dir() {
            fix_names(&p)?;
        }
    }
    Ok(())
}

/// Installers inside (vnbox ran the installed game): InstallShield setups, Inno Setup
/// installers and self-extracting zips of a game unpacked next to them, as NAME.is, NAME.inno and
/// NAME.zip; Windows Installer packages as NAME.x. The BGI install media marker goes.
fn nested(root: &Path) -> Result<()> {
    let mut files = vec![];
    walk(root, &mut |p| {
        files.push(p.to_path_buf());
        None::<()>
    });
    let with = |p: &Path, s: &str| {
        let mut o = p.as_os_str().to_os_string();
        o.push(s);
        PathBuf::from(o)
    };
    for s in files.iter().filter(|p| ext(p) == "exe") {
        let tries: [(&str, &dyn Fn(&Path, &Path) -> Result<bool>); 3] = [
            (".is", &installshield),
            (".inno", &|s: &Path, o: &Path| Ok(run("innoextract", &["-s".as_ref(), "-q".as_ref(), "-e".as_ref(), "-d".as_ref(), o.as_os_str(), s.as_os_str()], o))),
            (".zip", &|s: &Path, o: &Path| if game_sfx(s) { unzip(s, o) } else { Ok(false) }),
        ];
        for (suffix, f) in tries {
            let o = with(s, suffix);
            fs::create_dir_all(&o)?;
            if matches!(f(s, &o), Ok(true)) && !empty(&o) {
                eprintln!("{}: unpacked into {}", s.display(), o.display());
                fs::remove_file(s)?;
                fix_names(&o)?;
                break;
            }
            fs::remove_dir_all(&o)?;
        }
        // BGI install media: a 16-byte "0123456789ABCDEF" file next to BGIForInstalling.exe
        // marks the media, and the game run from there quits at once
        if s.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("BGIForInstalling.exe")) {
            for e in fs::read_dir(s.parent().unwrap())?.flatten() {
                let p = e.path();
                if p.is_file() && fs::read(&p).is_ok_and(|b| b.strip_suffix(b"\n").unwrap_or(&b).strip_suffix(b"\r").map_or(b.as_slice() == b"0123456789ABCDEF", |x| x == b"0123456789ABCDEF")) && e.metadata().is_ok_and(|m| m.len() == 16) {
                    eprintln!("{}: the install media's marker, removed", p.display());
                    fs::remove_file(&p)?;
                }
            }
        }
    }
    // (again: setups unpacked above hold packages)
    let mut files = vec![];
    walk(root, &mut |p| {
        files.push(p.to_path_buf());
        None::<()>
    });
    for m in files.iter().filter(|p| ext(p) == "msi") {
        let o = with(m, ".x");
        fs::create_dir_all(&o)?;
        if msi(m, &o).unwrap_or(false) {
            eprintln!("{}: unpacked into {}", m.display(), o.display());
            fs::remove_file(m)?;
        } else {
            eprintln!("{}: left as is (needs msiextract, or msiinfo and cabextract)", m.display());
            fs::remove_dir_all(&o)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use proptest::prelude::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ippai-unpack-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A PE with one section ending at 0x400, then `overlay`.
    fn pe(overlay: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 0x400];
        d[..2].copy_from_slice(b"MZ");
        d[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        d[0x80..0x84].copy_from_slice(b"PE\0\0");
        d[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        d[0x94..0x96].copy_from_slice(&0xe0u16.to_le_bytes());
        let sec = 0x80 + 24 + 0xe0;
        d[sec + 16..sec + 20].copy_from_slice(&0x200u32.to_le_bytes());
        d[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
        d.extend_from_slice(overlay);
        d
    }

    /// The masking setups apply (the inverse of Unmask): x = rotate4(!y ^ k).
    fn mask(data: &[u8], name: &[u8]) -> Vec<u8> {
        let key: Vec<u8> = name.iter().enumerate().map(|(i, c)| c ^ IS_MASK[i % 4]).collect();
        data.iter().enumerate().map(|(i, &y)| (!y ^ key[i % 1024 % key.len()]).rotate_left(4)).collect()
    }

    fn zlib(d: &[u8], level: u32) -> Vec<u8> {
        let mut e = ZlibEncoder::new(vec![], Compression::new(level));
        e.write_all(d).unwrap();
        e.finish().unwrap()
    }

    fn read_tree(d: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v = vec![];
        walk(d, &mut |p| {
            v.push((p.strip_prefix(d).unwrap().to_string_lossy().into_owned(), fs::read(p).unwrap()));
            None::<()>
        });
        v.sort();
        v
    }

    #[test]
    fn unmask_matches_the_table() {
        // tests/isextract.py's table: T[k][b] = ~(k ^ (b rotated by 4))
        for k in 0..=255u8 {
            for b in 0..=255u8 {
                let mut u = Unmask { r: &[b][..], key: vec![k], pos: 0 };
                let mut o = [0];
                u.read_exact(&mut o).unwrap();
                assert_eq!(o[0], !(k ^ ((b << 4) | (b >> 4))));
            }
        }
    }

    #[test]
    fn safe_paths() {
        let o = Path::new("/o");
        assert_eq!(safe_path(o, "a\\b/c.txt"), Some(PathBuf::from("/o/a/b/c.txt")));
        assert_eq!(safe_path(o, "./a"), Some(PathBuf::from("/o/a")));
        for bad in ["../a", "a/../../b", "/etc/x", "\\x", "C:\\x", "", "."] {
            assert_eq!(safe_path(o, bad), None, "{bad}");
        }
    }

    #[derive(Debug, Clone)]
    enum Is {
        Stream(u16),
        Old,
        Plain,
    }

    fn installshield_setup(kind: &Is, files: &[(String, Vec<u8>, bool)]) -> Vec<u8> {
        let mut o = vec![];
        match kind {
            Is::Stream(ver) => {
                o.extend_from_slice(b"ISSetupStream\0");
                o.extend_from_slice(&(files.len() as u16).to_le_bytes());
                o.extend_from_slice(&ver.to_le_bytes());
                o.resize(46, 0);
                for (name, data, z) in files {
                    let mut d = if *z { zlib(data, 6) } else { data.clone() };
                    d = mask(&d, name.as_bytes());
                    let n: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
                    o.extend_from_slice(&(n.len() as u32).to_le_bytes());
                    o.extend_from_slice(&6u32.to_le_bytes());
                    o.extend_from_slice(&[0, 0]);
                    if *ver >= 4 {
                        o.extend_from_slice(&(d.len() as u64).to_le_bytes());
                        o.extend_from_slice(&[0; 30]);
                    } else {
                        o.extend_from_slice(&(d.len() as u32).to_le_bytes());
                        o.extend_from_slice(&[0; 10]);
                    }
                    o.extend_from_slice(&n);
                    o.extend_from_slice(&d);
                }
            }
            Is::Old => {
                o.extend_from_slice(b"InstallShield\0");
                o.extend_from_slice(&(files.len() as u16).to_le_bytes());
                o.resize(46, 0);
                for (name, data, z) in files {
                    let raw = encoding_rs::SHIFT_JIS.encode(name).0;
                    let d = mask(&if *z { zlib(data, 6) } else { data.clone() }, &raw);
                    let mut a = vec![0u8; 312];
                    a[..raw.len()].copy_from_slice(&raw);
                    a[260..264].copy_from_slice(&6u32.to_le_bytes());
                    a[268..272].copy_from_slice(&(d.len() as u32).to_le_bytes());
                    o.extend_from_slice(&a);
                    o.extend_from_slice(&d);
                }
            }
            Is::Plain => {
                for (name, data, _) in files {
                    // (name, then the path the file goes to)
                    o.extend_from_slice(b"file.bin\0");
                    o.extend_from_slice(&encoding_rs::SHIFT_JIS.encode(name).0);
                    o.extend_from_slice(format!("\x001.0\0{}\0", data.len()).as_bytes());
                    o.extend_from_slice(data);
                }
            }
        }
        pe(&o)
    }

    fn names() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(vec!["data", "体験版", "Game.exe", "sub\\x.dat", "a b", "ｶﾅ", "音声"]), 1..3).prop_map(|v| v.join("_"))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        #[test]
        fn installshield_round_trip(
            kind in prop_oneof![Just(Is::Stream(2)), Just(Is::Stream(4)), Just(Is::Old), Just(Is::Plain)],
            files in prop::collection::btree_map(names(), (prop::collection::vec(any::<u8>(), 0..3000), any::<bool>()), 1..4),
        ) {
            let files: Vec<(String, Vec<u8>, bool)> = files.into_iter().map(|(n, (d, z))| (n, d, z)).collect();
            // (plain entries with no data look like the end)
            let files: Vec<_> = files.into_iter().filter(|f| !matches!(kind, Is::Plain) || !f.1.is_empty()).collect();
            prop_assume!(!files.is_empty());
            // a zlib-looking start of uncompressed data would be inflated, as in the setups
            prop_assume!(files.iter().all(|(_, d, z)| *z || !d.starts_with(b"x\x9c")));
            let d = tmp("is");
            let exe = d.join("setup.exe");
            fs::write(&exe, installshield_setup(&kind, &files)).unwrap();
            let out = d.join("out");
            fs::create_dir_all(&out).unwrap();
            prop_assert!(installshield(&exe, &out).unwrap());
            let mut want: Vec<(String, Vec<u8>)> = files.iter().map(|(n, d, _)| (n.replace('\\', "/"), d.clone())).collect();
            want.sort();
            prop_assert_eq!(read_tree(&out), want);
            fs::remove_dir_all(&d).unwrap();
        }
    }

    #[test]
    fn not_installshield() {
        let d = tmp("notis");
        for (name, body) in [("plain.exe", pe(b"")), ("junk.exe", pe(b"\0\0\0\0abc")), ("text.exe", b"hello".to_vec())] {
            let exe = d.join(name);
            fs::write(&exe, body).unwrap();
            let out = d.join(format!("{name}.out"));
            fs::create_dir_all(&out).unwrap();
            assert!(!installshield(&exe, &out).unwrap_or(false), "{name}");
        }
        // a truncated stream setup is an error, not a partial game
        let mut s = installshield_setup(&Is::Stream(2), &[("a.dat".into(), vec![1; 5000], false)]);
        s.truncate(s.len() - 10);
        fs::write(d.join("cut.exe"), s).unwrap();
        fs::create_dir_all(d.join("cut")).unwrap();
        assert!(installshield(&d.join("cut.exe"), &d.join("cut")).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    fn zsfx_setup(files: &[(&str, Vec<u8>)], sub: bool) -> Vec<u8> {
        let mut ini = String::from("[Setup]\r\nVer=1\r\nAppName=テスト\r\n");
        if sub {
            ini += "Val0=MAKESUBDIR,\"C:\\Games\\ゲーム\"\r\n";
        }
        ini += &format!("FileCount={}\r\n", files.len());
        for (i, (n, d)) in files.iter().enumerate() {
            ini += &format!("File{i}={n},{},1,{},0\r\n", if sub && i > 0 { "MAKESUBDIR" } else { "APP" }, d.len());
        }
        // an exe before it, with a zlib-looking pair that is no stream
        let mut o = pe(b"junk\x78\xda\xff\xff\xffmore");
        o.extend(zlib(&encoding_rs::SHIFT_JIS.encode(&ini).0, 9));
        for (_, d) in files {
            o.extend(b"pad");
            o.extend(zlib(d, 9));
        }
        o
    }

    #[test]
    fn zsfx_files() {
        let d = tmp("zsfx");
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let files = [("LsUninst.tmp", b"u".to_vec()), ("Game.exe", big.clone()), ("データ.dat", vec![])];
        for sub in [false, true] {
            let exe = d.join("setup.exe");
            fs::write(&exe, zsfx_setup(&files, sub)).unwrap();
            let out = d.join(format!("out{sub}"));
            fs::create_dir_all(&out).unwrap();
            assert!(zsfx(&exe, &out).unwrap());
            let p = if sub { "ゲーム/" } else { "" };
            let mut want = vec![("LsUninst.tmp".to_string(), b"u".to_vec()), (format!("{p}Game.exe"), big.clone()), (format!("{p}データ.dat"), vec![])];
            want.sort();
            assert_eq!(read_tree(&out), want);
        }
        // a size that doesn't match the list: an error
        let mut s = pe(&zlib(b"[S]\r\nFileCount=1\r\nFile0=a,APP,1,4,0\r\n", 9));
        s.extend(zlib(b"abc", 9));
        fs::write(d.join("bad.exe"), &s).unwrap();
        fs::create_dir_all(d.join("bad")).unwrap();
        assert!(zsfx(&d.join("bad.exe"), &d.join("bad")).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    /// A zip whose names are stored as the given bytes (Shift-JIS, no UTF-8 flag), optionally
    /// after an exe (a self-extracting zip).
    fn zip_with(names: &[(&[u8], &[u8])], prefix: &[u8]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(vec![]));
        let ph: Vec<String> = (0..names.len()).map(|i| ((b'A' + i as u8) as char).to_string().repeat(names[i].0.len())).collect();
        for (i, (_, d)) in names.iter().enumerate() {
            w.start_file(ph[i].clone(), zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(d).unwrap();
        }
        let mut z = w.finish().unwrap().into_inner();
        // the names aren't checksummed: put the raw bytes in place of the placeholders
        for (i, (n, _)) in names.iter().enumerate() {
            let p = ph[i].as_bytes();
            let mut at = 0;
            while let Some(j) = z[at..].windows(p.len()).position(|x| x == p) {
                z[at + j..at + j + p.len()].copy_from_slice(n);
                at += j + p.len();
            }
        }
        let mut o = prefix.to_vec();
        o.extend(z);
        o
    }

    #[test]
    fn zip_names_and_sfx() {
        let d = tmp("zip");
        let sj = encoding_rs::SHIFT_JIS.encode("体験版/ゲーム.exe").0.into_owned();
        let z = zip_with(&[(&sj, b"MZ game"), (b"readme.txt", b"hi"), (b"../evil.txt", b"x")], b"");
        fs::write(d.join("g.zip"), &z).unwrap();
        let out = d.join("out");
        unpack(&d.join("g.zip"), &out).unwrap();
        assert_eq!(read_tree(&out), vec![("readme.txt".into(), b"hi".to_vec()), ("体験版/ゲーム.exe".into(), b"MZ game".to_vec())]);
        assert!(!d.join("evil.txt").exists());
        assert_eq!(pick_exe(&out, None).as_deref(), Some("体験版/ゲーム.exe"));

        // a self-extracting zip of the game inside a zip: unpacked next to it
        let sfx = zip_with(&[(b"Game.exe", b"MZ the game"), (b"data.xp3", b"xp3")], &pe(b""));
        let outer = zip_with(&[(b"trial/trial.exe", &sfx)], b"");
        fs::write(d.join("outer.zip"), &outer).unwrap();
        let out = d.join("out2");
        unpack(&d.join("outer.zip"), &out).unwrap();
        assert_eq!(read_tree(&out), vec![("trial/trial.exe.zip/Game.exe".into(), b"MZ the game".to_vec()), ("trial/trial.exe.zip/data.xp3".into(), b"xp3".to_vec())]);
        // a program carrying a zip of its own data (NW.js) stays
        let nw = zip_with(&[(b"package.json", b"{}")], &pe(b""));
        fs::write(d.join("nw.zip"), zip_with(&[(b"Game.exe", &nw)], b"")).unwrap();
        let out = d.join("out3");
        unpack(&d.join("nw.zip"), &out).unwrap();
        assert_eq!(read_tree(&out), vec![("Game.exe".into(), nw.clone())]);
        // an exe that is the self-extracting zip itself
        fs::write(d.join("sfx.exe"), &sfx).unwrap();
        let out = d.join("out4");
        unpack(&d.join("sfx.exe"), &out).unwrap();
        assert_eq!(pick_exe(&out, None).as_deref(), Some("Game.exe"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn nested_installshield_and_bgi_marker() {
        let d = tmp("nested");
        let setup = installshield_setup(&Is::Stream(4), &[("Disk1\\BGI.exe".into(), b"MZ bgi".to_vec(), true)]);
        let z = zip_with(&[(b"setup.exe", &setup), (b"media/BGIForInstalling.exe", b"MZ inst"), (b"media/mark", b"0123456789ABCDEF"), (b"media/other", b"0123456789ABCDEX")], b"");
        fs::write(d.join("g.zip"), &z).unwrap();
        let out = d.join("out");
        unpack(&d.join("g.zip"), &out).unwrap();
        let names: Vec<String> = read_tree(&out).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["media/BGIForInstalling.exe", "media/other", "setup.exe.is/Disk1/BGI.exe"]);
        assert_eq!(pick_exe(&out, None).as_deref(), Some("setup.exe.is/Disk1/BGI.exe"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn shift_jis_names_renamed() {
        let d = tmp("names");
        let raw = encoding_rs::SHIFT_JIS.encode("体験版").0.into_owned();
        let dir = d.join(OsStr::from_bytes(&raw));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(OsStr::from_bytes(&encoding_rs::SHIFT_JIS.encode("起動.exe").0)), b"MZ").unwrap();
        fix_names(&d).unwrap();
        assert!(d.join("体験版/起動.exe").is_file());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn picks_the_game() {
        let d = tmp("pick");
        for (p, n) in [("Setup.exe", 900), ("game/Game.exe", 500), ("game/unins000.exe", 800), ("game/config.exe", 700), ("game/sub/tool.exe", 600), ("small.exe", 10)] {
            fs::create_dir_all(d.join(p).parent().unwrap()).unwrap();
            fs::write(d.join(p), vec![0; n]).unwrap();
        }
        assert_eq!(pick_exe(&d, None).as_deref(), Some("game/Game.exe"));
        assert_eq!(pick_exe(&d, Some("small.exe")).as_deref(), Some("small.exe"));
        // one folder further down, any case
        assert_eq!(pick_exe(&d, Some("GAME.EXE")).as_deref(), Some("game/Game.exe"));
        assert_eq!(pick_exe(&d, Some("missing.exe")), None);
        fs::write(d.join("game/cs2.exe"), b"MZ").unwrap();
        assert_eq!(pick_exe(&d, None).as_deref(), Some("game/cs2.exe"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn unknown_input_fails() {
        let d = tmp("bad");
        fs::write(d.join("x.exe"), pe(b"nothing here")).unwrap();
        assert!(unpack(&d.join("x.exe"), &d.join("out")).is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
