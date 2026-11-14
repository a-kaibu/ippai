//! The smaller DLLs: printf formatting (wsprintf, msvcrt), the registry (advapi32, kept in
//! memory), version, ole32/oleaut32 basics, shell32 folders, comctl32, winmm timers.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::{implement, Ctx, Handler};
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

/// The printf dialect of the API at hand: the legacy CRT's (msvcrt, msvcr*, user32's wsprintf)
/// unless the program called the UCRT (crt::fmt::Mode::ucrt with the call's options).
pub fn legacy_mode() -> crt::fmt::Mode {
    crt::fmt::Mode::legacy(crate::mem::x64())
}

/// ANSI printf (the legacy dialect): `%s` is ANSI unless `wide` (then UTF-16, as in swprintf).
/// An invalid format gives what came before it.
pub fn format(fmt: &[u8], va: &mut VaArgs, wide: bool) -> Vec<u8> {
    if wide {
        return w2a(&format_w(&a2w(fmt), va));
    }
    printf(fmt, va, legacy_mode()).unwrap_or_default()
}

/// UTF-16 printf (wsprintfW/swprintf): `%s` is UTF-16, `%S` ANSI.
pub fn format_w(fmt: &[u16], va: &mut VaArgs) -> Vec<u16> {
    printf(fmt, va, legacy_mode()).unwrap_or_default()
}

/// printf of `fmt` (ANSI bytes or UTF-16 units) with guest variable arguments.
pub fn printf<T: crt::Unit>(fmt: &[T], va: &mut VaArgs, m: crt::fmt::Mode) -> Result<Vec<T>, crt::fmt::Error> {
    if m.positional {
        return crt::fmt::format_p(fmt, m, &mut GuestArgs(va));
    }
    crt::fmt::format(fmt, m, &mut GuestArgs(va))
}

/// crt::fmt's arguments from a guest va_list (x86: 4-byte slots, 64-bit values and doubles in
/// two; x64: one 8-byte slot each) and the strings in guest memory.
pub struct GuestArgs<'a>(pub &'a mut VaArgs);

impl crt::fmt::Args for GuestArgs<'_> {
    fn int(&mut self, bytes: u32) -> u64 {
        if bytes == 8 { self.0.u64() } else { self.0.u32() as u64 }
    }
    fn float(&mut self) -> f64 {
        self.0.f64()
    }
    fn narrow(&mut self, p: u64, max: Option<usize>) -> Vec<u8> {
        match max {
            Some(n) => crate::mem::cstrn(p as u32, n.min(u32::MAX as usize) as u32),
            None => cstr(p as u32),
        }
    }
    fn wide(&mut self, p: u64, max: Option<usize>) -> Vec<u16> {
        match max {
            Some(n) => crate::mem::wstrn(p as u32, n.min(u32::MAX as usize) as u32),
            None => wstr(p as u32),
        }
    }
    fn counted(&mut self, p: u64) -> (usize, u64) {
        // ANSI_STRING / UNICODE_STRING: Length, MaximumLength, then the Buffer pointer
        let p = p as u32;
        (crate::mem::r16(p) as usize, crate::mem::rp(p + crate::mem::ps()) as u64)
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        a2w(s)
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        w2a(s)
    }
    fn store(&mut self, p: u64, bytes: u32, v: u64) {
        crate::mem::write(p as u32, &v.to_le_bytes()[..bytes.min(8) as usize]);
    }
}

// ---------------------------------------------------------------------------------------------
// the registry: in memory, what the game writes also kept in C:\users\user\sake-registry.tsv
// (a save root of the page, so settings and flags games keep in the registry survive restarts)

pub struct Key {
    pub path: String,
    /// opened in the 64-bit view (a 64-bit process's own, or KEY_WOW64_64KEY): wow64_value
    pub view64: bool,
}

#[derive(Default, Clone, PartialEq, Debug)]
struct KeyData {
    /// value name (lower case) -> (name, type, data)
    values: Vec<(String, u32, Vec<u8>)>,
    /// made or changed by the game (saved)
    ours: bool,
    /// the key's own name as created (paths are kept in lower case)
    name: String,
}

const REG_FILE: &str = "C:\\users\\user\\sake-registry.tsv";

fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(2 * b.len());
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

/// The saved keys: "key<TAB>original name" lines (old files just have "key"), then
/// "key<TAB>name<TAB>type<TAB>hex" for each value.
fn load_reg(m: &mut HashMap<String, KeyData>) -> bool {
    let Some(h) = crate::files::resolve(REG_FILE) else { return false };
    let Ok(t) = kura::fs::read(h) else { return false };
    let t = String::from_utf8_lossy(&t);
    parse_reg(&t, m);
    true
}

fn parse_reg(t: &str, m: &mut HashMap<String, KeyData>) {
    for l in t.lines() {
        if l.is_empty() { continue; }
        let f: Vec<&str> = l.split('\t').collect();
        if f[0].is_empty() {
            continue;
        }
        let k = m.entry(f[0].to_string()).or_default();
        k.ours = true;
        if let [_, name] = f[..] {
            k.name = name.to_string();
        }
        if let [_, name, ty, data] = f[..] {
            // (bytes, not chars: a damaged file read lossily has U+FFFD in it)
            let bytes = data.as_bytes().chunks_exact(2).filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok()).collect();
            let lname = name.to_lowercase();
            k.values.retain(|v| v.0.to_lowercase() != lname);
            k.values.push((name.to_string(), ty.parse().unwrap_or(3), bytes));
        }
    }
}

fn format_reg(m: &HashMap<String, KeyData>) -> String {
    let mut keys: Vec<&String> = m.iter().filter(|(_, k)| k.ours).map(|(p, _)| p).collect();
    keys.sort();
    let mut t = String::new();
    for p in keys {
        // (names with tabs or line breaks, and the empty one: not kept)
        if p.is_empty() || p.contains(['\t', '\n', '\r']) {
            continue;
        }
        let name = &m[p].name;
        if name.is_empty() || name.contains(['\t', '\n', '\r']) {
            t += &format!("{p}\n");
        } else {
            t += &format!("{p}\t{name}\n");
        }
        for (n, ty, d) in m[p].values.iter().filter(|v| !v.0.contains(['\t', '\n'])) {
            t += &format!("{p}\t{n}\t{ty}\t{}\n", hex(d));
        }
    }
    t
}

fn write_reg(t: &str) -> bool {
    #[cfg(test)]
    reg_cache_tests::WRITES.with(|n| n.set(n.get() + 1));
    let h = crate::files::resolve_new(REG_FILE);
    if let Some(d) = h.rsplit_once('/') {
        let _ = kura::fs::create_dir_all(d.0);
    }
    let temp = format!("{h}.{}.tmp", crate::pid());
    if kura::fs::write(&temp, t).is_err() { return false; }
    if kura::fs::rename(&temp, &h).is_err() { let _ = kura::fs::remove_file(&temp); return false; }
    true
}

/// A change to the registry (called inside `with_reg`): written to the file when the
/// transaction ends while other processes share the registry (unless in a `reg_batch`), a
/// little later otherwise (waits::later).
fn save_reg(_: &HashMap<String, KeyData>) -> bool {
    REG_CHANGED.store(true, Ordering::Relaxed);
    true
}

/// Set by `save_reg` inside a transaction (one transaction at a time: REG)
static REG_CHANGED: AtomicBool = AtomicBool::new(false);

/// How long a change of a process of its own waits for more before it is written (one file
/// write for a settings dialog's 50 values, an installer's 200): the page's look at the saves,
/// RegFlushKey, RegCloseKey, starting another process and the process's end write it sooner.
const REG_LATER: Duration = Duration::from_millis(500);

/// The registry of this process: the keys, and how they stand to the file
struct Reg {
    m: HashMap<String, KeyData>,
    /// the file's stamp (kura::fs::stamp) when last read or written: another process's change
    /// shows as a new one
    stamp: u64,
    /// what was written last (a write that changes nothing is not made)
    written: u64,
    /// the saved keys as the file had them when last read or written: what another process
    /// changed since is what differs from it in the file, what this one changed what differs
    /// from it here (`merge_reg`)
    base: HashMap<String, KeyData>,
}

impl waits::later::Store for Reg {
    fn write(&mut self) -> bool {
        let t = format_reg(&self.m);
        let sum = text_sum(&t);
        if sum != self.written {
            if !write_reg(&t) {
                return false;
            }
            self.written = sum;
        }
        self.stamp = kura::fs::stamp(crate::files::resolve_new(REG_FILE));
        self.base = saved_keys(&self.m);
        true
    }
}

/// The keys the file keeps
fn saved_keys(m: &HashMap<String, KeyData>) -> HashMap<String, KeyData> {
    m.iter().filter(|(_, k)| k.ours).map(|(p, k)| (p.clone(), k.clone())).collect()
}

/// Another process's changes (`file`, read now) with this one's not yet written, as Windows
/// keeps two processes' writes: what this process changed since `base` (a key made or deleted,
/// a value set or deleted, a key's spelling) stays as it is here, everything else becomes what
/// the file says; the built-in keys stay. Values are the unit: two processes setting different
/// values of one key both keep theirs, the same value is this process's (it writes last).
fn merge_reg(m: &mut HashMap<String, KeyData>, base: &HashMap<String, KeyData>, file: HashMap<String, KeyData>) {
    let mut paths: Vec<String> = file.keys().chain(base.keys()).cloned().collect();
    paths.extend(m.iter().filter(|(_, k)| k.ours).map(|(p, _)| p.clone()));
    paths.sort();
    paths.dedup();
    for p in paths {
        // (keys alike when their spelling and values are, in any order: a value set again
        // moves to the end in some paths)
        let here = m.get(&p).filter(|k| k.ours).cloned().map(|h| match base.get(&p) {
            Some(b) if same_key(&h, b) => b.clone(),
            _ => h,
        });
        let (b, f) = (base.get(&p), file.get(&p));
        let merged = match (here, b, f) {
            // (made there)
            (None, None, f) => f.cloned(),
            // (made here, perhaps there too)
            (Some(h), None, f) => Some(f.map_or(h.clone(), |f| merge_key(&h, &KeyData::default(), f))),
            // (deleted here)
            (None, Some(_), _) => None,
            // (deleted there: made again here only with values this process set, as a key
            // created again after another deleted it; deleting values of it changes nothing)
            (Some(h), Some(b), None) => Some(merge_key(&h, b, &KeyData { name: h.name.clone(), ..KeyData::default() })).filter(|k| !k.values.is_empty()),
            (Some(h), Some(b), Some(f)) => Some(merge_key(&h, b, f)),
        };
        match merged {
            Some(k) => {
                m.insert(p, k);
            }
            None => {
                if m.get(&p).is_some_and(|k| k.ours) {
                    m.remove(&p);
                }
            }
        }
    }
}

fn same_key(a: &KeyData, b: &KeyData) -> bool {
    fn sorted(k: &KeyData) -> Vec<(String, u32, &[u8])> {
        let mut v: Vec<(String, u32, &[u8])> = k.values.iter().map(|v| (v.0.to_lowercase(), v.1, v.2.as_slice())).collect();
        v.sort();
        v
    }
    a.name == b.name && sorted(a) == sorted(b)
}

/// One key both have: its values merged one by one (names without case), in this process's
/// order with the file's new ones after
fn merge_key(here: &KeyData, base: &KeyData, file: &KeyData) -> KeyData {
    let find = |k: &KeyData, n: &str| k.values.iter().find(|v| v.0.eq_ignore_ascii_case(n)).cloned();
    let mut names: Vec<String> = here.values.iter().chain(&file.values).chain(&base.values).map(|v| v.0.clone()).collect();
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.to_lowercase()));
    let values = names
        .iter()
        .filter_map(|n| {
            let (h, b) = (find(here, n), find(base, n));
            if h != b { h } else { find(file, n) }
        })
        .collect();
    let name = if here.name != base.name { here.name.clone() } else { file.name.clone() };
    KeyData { values, ours: true, name }
}

/// While shared: what other processes wrote since this one last looked (`merge_reg`)
fn reg_sync(r: &mut waits::later::Held<Reg>) {
    let stamp = kura::fs::stamp(crate::files::resolve_new(REG_FILE));
    // (0: a file just changed or not there: nothing tells, so it is read)
    if stamp != 0 && stamp == r.value.stamp {
        return;
    }
    let mut file = HashMap::default();
    if load_reg(&mut file) {
        let reg = &mut r.value;
        merge_reg(&mut reg.m, &reg.base, file.clone());
        reg.base = file;
    }
    r.value.stamp = stamp;
}

fn text_sum(t: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

static REG: waits::later::Later<Reg> = waits::later::Later::new();

thread_local! {
    /// reg_batch levels of this thread
    static BATCH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Runs `f` with the registry's changes written once at its end even while shared (msi's
/// standard actions: a package's thousand Registry rows are one file write, not a thousand).
/// Other processes see them at the end; this one at once.
pub(crate) fn reg_batch<R>(f: impl FnOnce() -> R) -> R {
    BATCH.with(|b| b.set(b.get() + 1));
    let r = f();
    if BATCH.with(|b| {
        b.set(b.get() - 1);
        b.get() == 0
    }) {
        reg_flush();
    }
    r
}

/// Writes the registry's changes now (RegFlushKey, RegCloseKey; before another process starts;
/// a batch's end). While shared: merged with other processes' first, under the kernel's lock.
pub(crate) fn reg_flush() {
    with_reg_eager(true, |_| ((), false));
}

/// The page looks at the saves: what waits is written first. Never waits for a guest thread
/// (the page's main thread runs it): a transaction going on writes it as it ends. While shared
/// nothing waits but a batch's changes, written at its end (and they can't be written here,
/// outside the kernel's lock).
fn reg_flush_for_page() {
    if !crate::kern::host::shared() {
        REG.flush_soon();
    }
}

extern "C" fn reg_flush_at_exit() {
    if crate::kern::host::shared() {
        reg_flush();
    } else {
        REG.flush_soon();
    }
}

/// Keys every Windows has, even empty (setup programs open them to tell whether they may
/// install: EXEpress CX opens ...\Uninstall and says "requires power user" when it can't;
/// winref regsys has Windows' answers). Their parents exist through them.
/// The values of Explorer\Shell Folders (HKCU when `true`, HKLM otherwise) with their CSIDL, as
/// on Windows 7. "User Shell Folders" holds the same names as REG_EXPAND_SZ.
pub(crate) const SHELL_FOLDERS: &[(&str, u32, bool)] = &[
    ("AppData", 0x1a, true),
    ("Cache", 0x20, true),
    ("Cookies", 0x21, true),
    ("Desktop", 0x10, true),
    ("Favorites", 0x06, true),
    ("History", 0x22, true),
    ("Local AppData", 0x1c, true),
    ("My Music", 0x0d, true),
    ("My Pictures", 0x27, true),
    ("My Video", 0x0e, true),
    ("NetHood", 0x13, true),
    ("Personal", 0x05, true),
    ("PrintHood", 0x1b, true),
    ("Programs", 0x02, true),
    ("Recent", 0x08, true),
    ("SendTo", 0x09, true),
    ("Start Menu", 0x0b, true),
    ("Startup", 0x07, true),
    ("Templates", 0x15, true),
    ("Fonts", 0x14, true),
    ("CD Burning", 0x3b, true),
    ("Administrative Tools", 0x30, true),
    ("Common AppData", 0x23, false),
    ("Common Desktop", 0x19, false),
    ("Common Documents", 0x2e, false),
    ("Common Programs", 0x17, false),
    ("Common Start Menu", 0x16, false),
    ("Common Startup", 0x18, false),
    ("Common Templates", 0x2d, false),
    ("Common Administrative Tools", 0x2f, false),
    ("CommonMusic", 0x35, false),
    ("CommonPictures", 0x36, false),
    ("CommonVideo", 0x37, false),
];

/// A folder as "User Shell Folders" spells it (%USERPROFILE%\..., %ProgramData%, %PUBLIC%\...);
/// None for the folders it doesn't list (Fonts lives under %windir%).
pub(crate) fn shell_folder_unexpanded(path: &str) -> Option<String> {
    for (pre, var) in [("C:\\users\\user", "%USERPROFILE%"), ("C:\\users\\Public", "%PUBLIC%"), ("C:\\ProgramData", "%ProgramData%")] {
        if let Some(rest) = path.strip_prefix(pre).filter(|r| r.is_empty() || r.starts_with('\\')) {
            return Some(format!("{var}{rest}"));
        }
    }
    None
}

const SYSTEM_KEYS: [&str; 8] = [
    // (the post-mortem debugger's key: crate::dbg)
    "hklm\\software\\Microsoft\\Windows NT\\CurrentVersion\\AeDebug",
    "hklm\\software\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
    "hklm\\software\\Microsoft\\Windows\\CurrentVersion\\App Paths",
    "hklm\\software\\Microsoft\\Windows\\CurrentVersion\\Run",
    "hklm\\software\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
    "hkcu\\software\\Microsoft\\Windows\\CurrentVersion\\Run",
    "hkcu\\software\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
    "hkcu\\Control Panel\\Desktop",
];

/// RPG Maker RTPs: (registry key, value, folder), as the official setups write them on a 32-bit
/// Windows (their folders: crates/masu/src/pack.rs RTP_DIRS, installed by rtp.rs). RGSS1-3 (XP,
/// VX, VX Ace) read the folder of the RTP their Game.ini names (RTP=RPGVXAce, RTP1=Standard)
/// from the key and stop with "RTP が必要です" without it; RPG_RT (2000, 2003) reads
/// RuntimePackagePath (the 2000 setup's script names Software\ASCII\RPG2000; the 2003 one's is
/// its company and product, Enterbrain\RPG2003), the user's key too.
pub(crate) const RTPS: &[(&str, &str, &str)] = &[
    ("hklm\\software\\enterbrain\\rgss3\\rtp", "RPGVXAce", "C:\\Program Files\\Common Files\\Enterbrain\\RGSS3\\RPGVXAce"),
    ("hklm\\software\\enterbrain\\rgss2\\rtp", "RPGVX", "C:\\Program Files\\Common Files\\Enterbrain\\RGSS2\\RPGVX"),
    ("hklm\\software\\enterbrain\\rgss\\rtp", "Standard", "C:\\Program Files\\Common Files\\Enterbrain\\RGSS\\Standard"),
    ("hklm\\software\\ascii\\rpg2000", "RuntimePackagePath", "C:\\Program Files\\ASCII\\RPG2000\\RTP"),
    ("hkcu\\software\\ascii\\rpg2000", "RuntimePackagePath", "C:\\Program Files\\ASCII\\RPG2000\\RTP"),
    ("hklm\\software\\enterbrain\\rpg2003", "RuntimePackagePath", "C:\\Program Files\\Enterbrain\\RPG2003\\RTP"),
    ("hkcu\\software\\enterbrain\\rpg2003", "RuntimePackagePath", "C:\\Program Files\\Enterbrain\\RPG2003\\RTP"),
];

/// A ShiinaRio game's installed state (shiina.rs), once the game's exe is loaded: the values its
/// SETUP.exe writes, where its INI's key has no InstPath yet.
fn shiina_installed() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    let Some(exe) = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.clone()) else { return };
    DONE.store(true, Ordering::Relaxed);
    let Some((dir, inis)) = crate::shiina::inis(&exe) else { return };
    let Some(key) = inis.iter().find_map(|i| {
        let t = kura::fs::read(crate::files::resolve(i)?).ok()?;
        crate::shiina::reg_key(&String::from_utf16_lossy(&crate::text::mb2w(932, &t)))
    }) else {
        return;
    };
    let path = join("hkcu", &key);
    with_reg(|m| {
        let k = m.entry(path.clone()).or_default();
        if k.values.iter().any(|v| v.0.eq_ignore_ascii_case("InstPath")) {
            return;
        }
        if crate::env_on!("SAKE_TRACE_REG") {
            eprintln!("sake reg: {path}: ShiinaRio's installed state for {dir}");
        }
        if k.name.is_empty() {
            k.name = key.clone();
        }
        for (n, ty, d) in crate::shiina::values(&dir) {
            k.values.push((n.to_string(), ty, d));
        }
        k.ours = true;
        save_reg(m);
    });
}

/// A string value (REG_SZ 1 / REG_EXPAND_SZ 2) in the built-in registry.
fn put_str(m: &mut HashMap<String, KeyData>, key: &str, name: &str, v: &str, ty: u32) {
    let e = m.entry(key.to_lowercase()).or_default();
    let mut data: Vec<u8> = v.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    data.extend([0, 0]);
    e.values.push((name.to_string(), ty, data));
}

fn reg_builtin() -> HashMap<String, KeyData> {
    let mut m = HashMap::default();
    put_str(&mut m, "hklm\\software\\microsoft\\windows nt\\currentversion", "CurrentVersion", "6.1", 1);
    put_str(&mut m, "hklm\\software\\microsoft\\windows nt\\currentversion", "ProductName", "Microsoft Windows 7", 1);
    // (ProgramFilesDir and CommonFilesDir as the key's view has them: wow64_value)
    for (n, v) in [("ProgramFilesDir", "C:\\Program Files"), ("ProgramFilesDir (x86)", "C:\\Program Files (x86)"), ("ProgramW6432Dir", "C:\\Program Files"),
        ("CommonFilesDir", "C:\\Program Files\\Common Files"), ("CommonFilesDir (x86)", "C:\\Program Files (x86)\\Common Files"), ("CommonW6432Dir", "C:\\Program Files\\Common Files")] {
        put_str(&mut m, "hklm\\software\\microsoft\\windows\\currentversion", n, v, 1);
    }
    put_str(&mut m, "hklm\\software\\microsoft\\directx", "Version", "4.09.00.0904", 1);
    // Explorer's folder lists (old programs read these instead of SHGetFolderPath)
    for (key, user) in [("hkcu", true), ("hklm", false)] {
        for &(name, csidl, for_user) in SHELL_FOLDERS {
            if for_user != user {
                continue;
            }
            let Some(path) = special_folder(csidl) else { continue };
            put_str(&mut m, &format!("{key}\\software\\microsoft\\windows\\currentversion\\explorer\\shell folders"), name, path, 1);
            if let Some(v) = shell_folder_unexpanded(path) {
                put_str(&mut m, &format!("{key}\\software\\microsoft\\windows\\currentversion\\explorer\\user shell folders"), name, &v, 2); // REG_EXPAND_SZ
            }
        }
    }
    // the installed fonts (DirectWrite's system collection reads this list)
    if let Some(dir) = crate::files::resolve("C:\\windows\\Fonts\\") {
        let mut files: Vec<String> = kura::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
        files.sort();
        for f in files.iter().filter(|f| [".ttf", ".ttc", ".otf"].iter().any(|x| f.to_ascii_lowercase().ends_with(x))) {
            let stem = f.rsplit_once('.').map_or(f.as_str(), |x| x.0);
            put_str(&mut m, "hklm\\software\\microsoft\\windows nt\\currentversion\\fonts", &format!("{stem} (TrueType)"), f, 1);
        }
    }
    // the RPG Maker RTPs in C:\ (the official ones, crates/masu/src/rtp.rs), as their setups
    // register them: the engine finds the RTP folder by this value
    for (key, name, dir) in RTPS {
        if crate::files::resolve(&format!("{dir}\\")).is_some() {
            put_str(&mut m, key, name, dir, 1);
        }
    }
    m.entry("hkcu\\software".into()).or_default();
    for path in SYSTEM_KEYS {
        let k = m.entry(path.to_lowercase()).or_default();
        k.name = path.rsplit('\\').next().unwrap_or(path).into();
    }
    // the event logs (eventlog.rs) and the event log service's own source
    for l in ["Application", "Security", "System", "System\\EventLog"] {
        let k = m.entry(format!("hklm\\system\\currentcontrolset\\services\\eventlog\\{}", l.to_lowercase())).or_default();
        k.name = l.rsplit('\\').next().unwrap_or(l).into();
    }
m
}

fn reg_init() -> Reg {
    let mut m = reg_builtin();
    let stamp = kura::fs::stamp(crate::files::resolve_new(REG_FILE));
    load_reg(&mut m);
    static HOOKS: std::sync::Once = std::sync::Once::new();
    HOOKS.call_once(|| {
        kura::vfs::before_export(reg_flush_for_page);
        unsafe {
            libc::atexit(reg_flush_at_exit);
        }
    });
    let base = saved_keys(&m);
    Reg { m, stamp, written: 0, base }
}

/// Runs `f` on the registry: a transaction (other threads, and other processes once they
/// share the registry, wait meanwhile). The file is read once; read again only when another
/// process changed it (its stamp is new), which only happens while the registry is shared.
fn with_reg<R>(f: impl FnOnce(&mut HashMap<String, KeyData>) -> R) -> R {
    with_reg_eager(false, |m| {
        REG_CHANGED.store(false, Ordering::Relaxed);
        let r = f(m);
        (r, REG_CHANGED.swap(false, Ordering::Relaxed))
    })
}

/// A transaction written at its end when `now`, or while shared and not in a batch
fn with_reg_eager<R>(now: bool, f: impl FnOnce(&mut HashMap<String, KeyData>) -> (R, bool)) -> R {
    let shared = crate::kern::host::shared();
    let eager = now || shared && BATCH.with(|b| b.get()) == 0;
    let run = || {
        REG.with(reg_init, eager, REG_LATER, |h| {
            if shared {
                reg_sync(h);
            }
            f(&mut h.value.m)
        })
    };
    if shared {
        // (the read/modify/write transaction across the game, SCM and helper processes, not
        // just across this process's Rust threads)
        crate::kern::with(|_| run()).0
    } else {
        run()
    }
}

/// Internal SCM / COM access to the same registry as guest Reg* calls. Refresh
/// saved entries so a newly installed service is visible in another process.
pub(crate) fn registry_value(path: &str, name: &str) -> Option<(u32, Vec<u8>)> {
    with_reg(|m| {
        let path = path.to_lowercase();
        let find = |key: &str| m.get(key)?.values.iter().find(|v| v.0.eq_ignore_ascii_case(name)).map(|v| (v.1, v.2.clone()));
        find(&path).or_else(|| path.strip_prefix("hkcr\\").and_then(|p|
            find(&format!("hkcu\\software\\classes\\{p}")).or_else(|| find(&format!("hklm\\software\\classes\\{p}")))))
    })
}
pub(crate) fn registry_values(path: &str, values: Vec<(String, u32, Vec<u8>)>) -> bool {
    with_reg(|m| {
        let k = m.entry(path.to_lowercase()).or_default();
        for (name, ty, value) in values {
            k.values.retain(|v| !v.0.eq_ignore_ascii_case(&name));
            k.values.push((name, ty, value));
        }
        k.ours = true;
        save_reg(m)
    })
}
pub(crate) fn registry_remove_tree(path: &str) -> bool {
    let path = path.to_lowercase();
    with_reg(|m| {
        m.retain(|k, _| k != &path && !k.starts_with(&(path.clone() + "\\")));
        save_reg(m)
    })
}
pub(crate) fn registry_string(path: &str, name: &str) -> Option<String> {
    let (ty, bytes) = registry_value(path, name)?;
    if !matches!(ty, 1 | 2) { return None; }
    let words: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).take_while(|&c| c != 0).collect();
    Some(String::from_utf16_lossy(&words))
}

/// Sets a value by key path ("hklm\\software\\x"; data as the W functions store it), creating
/// the key and its parents (msi's WriteRegistryValues, product registration).
pub fn reg_put(path: &str, name: &str, ty: u32, data: Vec<u8>) {
    let path = wow64_key(path.trim_matches('\\').to_lowercase());
    with_reg(|m| {
        let mut at = String::new();
        for part in path.split('\\') {
            at = if at.is_empty() { part.to_string() } else { format!("{at}\\{part}") };
            m.entry(at.clone()).or_default().ours = true;
        }
        let k = m.get_mut(&path).unwrap();
        let lname = name.to_lowercase();
        match k.values.iter_mut().find(|v| v.0.to_lowercase() == lname) {
            Some(v) => {
                v.1 = ty;
                v.2 = data;
            }
            None => k.values.push((name.to_string(), ty, data)),
        }
        save_reg(m);
    });
}

/// Creates a key by path (and its parents).
pub fn reg_create(path: &str) {
    let path = wow64_key(path.trim_matches('\\').to_lowercase());
    with_reg(|m| {
        let mut at = String::new();
        for part in path.split('\\') {
            at = if at.is_empty() { part.to_string() } else { format!("{at}\\{part}") };
            m.entry(at.clone()).or_default().ours = true;
        }
        save_reg(m);
    });
}

/// A value by key path: (type, data).
pub fn reg_get(path: &str, name: &str) -> Option<(u32, Vec<u8>)> {
    let path = wow64_key(path.trim_matches('\\').to_lowercase());
    let lname = name.to_lowercase();
    with_reg(|m| m.get(&path).and_then(|k| k.values.iter().find(|v| v.0.to_lowercase() == lname).map(|v| (v.1, v.2.clone()))))
}

/// The value names of a key path.
pub fn reg_values(path: &str) -> Vec<String> {
    let path = wow64_key(path.trim_matches('\\').to_lowercase());
    with_reg(|m| m.get(&path).map(|k| k.values.iter().map(|v| v.0.clone()).collect()).unwrap_or_default())
}

/// Deletes a value of a key path.
pub fn reg_delete_value(path: &str, name: &str) {
    let path = wow64_key(path.trim_matches('\\').to_lowercase());
    with_reg(|m| {
        if let Some(k) = m.get_mut(&path) {
            let before = k.values.len();
            k.values.retain(|v| !v.0.eq_ignore_ascii_case(name));
            if k.values.len() != before {
                save_reg(m);
            }
        }
    });
}

/// Deletes a key path and everything under it.
pub fn reg_delete_tree(path: &str) {
    let path = wow64_key(path.trim_matches('\\').to_lowercase());
    let pre = format!("{path}\\");
    with_reg(|m| {
        m.retain(|k, _| *k != path && !k.starts_with(&pre));
        save_reg(m);
    });
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
        wow64_key(format!("{base}\\{sub}"))
    }
}

/// The keys under which 64-bit Windows keeps the 32-bit view (registry redirection), as one with
/// the 64-bit view in sake: a program that opens SOFTWARE\Wow6432Node\<x> because it is on 64-bit
/// Windows finds what a 32-bit installer wrote to SOFTWARE\<x>, and KEY_WOW64_32KEY /
/// KEY_WOW64_64KEY open the same keys. (A 32-bit process's views of the two are the same keys
/// on Windows for most of what games keep: their own keys in the view they were written in.)
/// `p` is a lower-case path.
pub(crate) fn wow64_key(p: String) -> String {
    const VIEWS: [&str; 4] = ["hklm\\software\\classes", "hkcu\\software\\classes", "hklm\\software", "hkcr"];
    for v in VIEWS {
        if let Some(rest) = p.strip_prefix(v).and_then(|r| r.strip_prefix("\\wow6432node")) {
            if rest.is_empty() || rest.starts_with('\\') {
                return format!("{v}{rest}");
            }
        }
    }
    p
}

/// RegCreateKeyEx: opens or creates, and says which (REG_CREATED_NEW_KEY 1, REG_OPENED_EXISTING_KEY 2).
fn create_key_ex(h: u32, sub: &str, out: u32, disp: u32, sam: u32) -> u32 {
    let existed = key_path(h).is_some_and(|base| {
        let path = join(&base, sub);
        let pre = format!("{path}\\");
        root_name(h).is_some() && sub.trim_matches('\\').is_empty() || with_reg(|m| m.contains_key(&path) || m.keys().any(|k| k.starts_with(&pre)))
    });
    let r = open_key_view(h, sub, true, out, sam);
    if r == 0 {
        put32(disp, if existed { 2 } else { 1 });
    }
    r
}

fn open_key(h: u32, sub: &str, create: bool, out: u32) -> u32 {
    open_key_view(h, sub, create, out, 0)
}
/// The view a key opened from `h` with access `sam` is in: KEY_WOW64_64KEY (0x100) or
/// KEY_WOW64_32KEY (0x200) when given, else `h`'s, else the process's own.
fn view64(h: u32, sam: u32) -> bool {
    match sam & 0x300 {
        0x100 => true,
        0x200 => false,
        _ => obj::with(|t| match t.objs.get(&h) {
            Some((Obj::Key(k), _)) => Some(k.view64),
            _ => None,
        })
        .unwrap_or_else(mem::x64),
    }
}
fn open_key_view(h: u32, sub: &str, create: bool, out: u32, sam: u32) -> u32 {
    let Some(base) = key_path(h) else { return 6 };
    if sub.starts_with('\\') {
        mem::putp(out, 0);
        return 161; // ERROR_BAD_PATHNAME
    }
    let path = join(&base, sub);
    if crate::env_on!("SAKE_TRACE_REG") {
        eprintln!("sake reg: open {path}");
    }
    shiina_installed();
    let exists = with_reg(|m| {
        if m.contains_key(&path) || root_name(h).is_some() && sub.trim_matches('\\').is_empty() {
            return true;
        }
        if create {
            // create the parents too
            let mut p = String::new();
            let given: Vec<&str> = sub.split('\\').filter(|x| !x.is_empty()).collect();
            let parts: Vec<&str> = path.split('\\').collect();
            for (i, part) in parts.iter().enumerate() {
                p = if p.is_empty() { (*part).into() } else { format!("{p}\\{part}") };
                let e = m.entry(p.clone()).or_default();
                // (the name as the program spelled it, for RegEnumKey)
                let g = (i + given.len()).checked_sub(parts.len()).and_then(|j| given.get(j));
                if e.name.is_empty() {
                    e.name = g.map_or(part.to_string(), |g| g.to_string());
                }
            }
            if let Some(k) = m.get_mut(&path).filter(|k| !k.ours) {
                k.ours = true;
                save_reg(m);
            }
            return true;
        }
        // a key with subkeys exists even when it has no data of its own
        let pre = format!("{path}\\");
        m.keys().any(|k| k.starts_with(&pre))
    });
    if !exists {
        mem::putp(out, 0);
        return 2;
    }
    let view64 = view64(h, sam);
    mem::putp(out, obj::add(Obj::Key(Key { path, view64 })));
    0
}

fn RegOpenKeyExA(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), false) };
    open_key_view(c.a(0), &s, false, c.a(4), c.a(3))
}
fn RegOpenKeyExW(c: &mut Ctx) -> u32 {
    let s = if c.a(1) == 0 { String::new() } else { arg_str(c.a(1), true) };
    open_key_view(c.a(0), &s, false, c.a(4), c.a(3))
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
    create_key_ex(c.a(0), &s, c.a(7), c.a(8), c.a(5))
}
fn RegCreateKeyExW(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), true);
    create_key_ex(c.a(0), &s, c.a(7), c.a(8), c.a(5))
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
    if root_name(c.a(0)).is_none() && !obj::close(c.a(0)) {
        return 6;
    }
    // (what was written through the key reaches the file: a settings dialog's values once)
    reg_flush();
    0
}
fn RegFlushKey(_: &mut Ctx) -> u32 {
    reg_flush();
    0
}

/// The values 64-bit Windows keeps apart in its two views of the same key (wow64_key): the
/// Program Files folders of the view's kind (k32::wow64_env)
fn wow64_value(path: &str, lname: &str, view64: bool) -> Option<&'static str> {
    if path != "hklm\\software\\microsoft\\windows\\currentversion" {
        return None;
    }
    Some(match (lname, view64) {
        ("programfilesdir", true) => "C:\\Program Files",
        ("programfilesdir", false) => "C:\\Program Files (x86)",
        ("commonfilesdir", true) => "C:\\Program Files\\Common Files",
        ("commonfilesdir", false) => "C:\\Program Files (x86)\\Common Files",
        _ => return None,
    })
}
fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect()
}

fn is_str(ty: u32) -> bool {
    matches!(ty, 1 | 2 | 7)
}

fn query_value(c: &mut Ctx, key: u32, name: &str, pty: u32, data: u32, plen: u32, wide: bool) -> u32 {
    let Some(path) = key_path(key) else { return 6 };
    let lname = name.to_lowercase();
    let v = with_reg(|m| m.get(&path).and_then(|k| k.values.iter().find(|v| v.0.to_lowercase() == lname).map(|v| (v.1, v.2.clone()))));
    let v = v.map(|(ty, d)| wow64_value(&path, &lname, view64(key, 0)).map_or((ty, d), |s| (1, utf16z(s))));
    let _ = c;
    if crate::env_on!("SAKE_TRACE_REG") {
        eprintln!("sake reg: query {path}\\{name}: {:?}", v.as_ref().map(|x| x.1.len()));
    }
    let Some((ty, mut bytes)) = v else {
        // (Windows clears the type, and the size only of a size query)
        put32(pty, 0);
        if data == 0 {
            put32(plen, 0);
        }
        return 2;
    };
    let have = if plen != 0 { r32(plen) } else { 0 };
    if is_str(ty) && !wide {
        let u: Vec<u16> = bytes.chunks(2).map(|b| u16::from_le_bytes([b[0], *b.get(1).unwrap_or(&0)])).collect();
        // (the A function converts in place: past the text, the caller's buffer keeps the UTF-16
        // bytes that were there)
        if data != 0 && have as usize >= bytes.len() {
            mem::write(data, &bytes);
        }
        bytes = w2a(&u);
    }
    put32(pty, ty);
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
        let tmp = mem::heap_alloc(8, true);
        let r = open_key(k, &arg_str(sub, false), false, tmp);
        h = mem::rp(tmp);
        mem::heap_free(tmp);
        if r != 0 {
            return r;
        }
    }
    query_value(c, h, "", 0, d, l, false)
}

/// RegGetValue(key, subkey, value, flags, &type, data, &len) (the RRF_RT_ type filter is not
/// applied)
fn reg_get_value(c: &mut Ctx, wide: bool) -> u32 {
    let (k, sub, name, t, d, l) = (c.a(0), c.a(1), c.a(2), c.a(4), c.a(5), c.a(6));
    let name = if name == 0 { String::new() } else { arg_str(name, wide) };
    let mut h = k;
    if sub != 0 && r8(sub) != 0 {
        let tmp = mem::heap_alloc(8, true);
        // (RRF_SUBKEY_WOW6464KEY 0x10000, RRF_SUBKEY_WOW6432KEY 0x20000: KEY_WOW64_64KEY, _32KEY)
        let r = open_key_view(k, &arg_str(sub, wide), false, tmp, c.a(3) >> 8 & 0x300);
        h = mem::rp(tmp);
        mem::heap_free(tmp);
        if r != 0 {
            return r;
        }
    }
    // the RRF_RT_* filter: bit 1 << type for REG_NONE..REG_DWORD, 0x20 MULTI_SZ, 0x40 QWORD;
    // REG_SZ takes REG_EXPAND_SZ too unless RRF_NOEXPAND (0x10000000)
    let flags = c.a(3);
    let path = key_path(h);
    let lname = name.to_lowercase();
    let ty = path.and_then(|p| with_reg(|m| m.get(&p).and_then(|k| k.values.iter().find(|v| v.0.to_lowercase() == lname).map(|v| v.1))));
    if let Some(ty) = ty {
        let bit = match ty {
            0..=4 => 1 << ty,
            7 => 0x20,
            11 => 0x40,
            _ => 0,
        };
        let allowed = flags & 0xffff == 0xffff || flags & bit != 0 || ty == 2 && flags & 2 != 0 && flags & 0x1000_0000 == 0;
        if !allowed {
            return 1630; // ERROR_UNSUPPORTED_TYPE
        }
    }
    query_value(c, h, &name, t, d, l, wide)
}
fn RegGetValueA(c: &mut Ctx) -> u32 {
    reg_get_value(c, false)
}
fn RegGetValueW(c: &mut Ctx) -> u32 {
    reg_get_value(c, true)
}

fn set_value(key: u32, name: &str, ty: u32, data: u32, len: u32, wide: bool) -> u32 {
    let Some(path) = key_path(key) else { return 6 };
    let mut bytes = mem::slice(data, len).to_vec();
    if is_str(ty) && !wide {
        // all of it converted (MULTI_SZ: every string), with a NUL added when the text has none
        let nul = bytes.last().is_some_and(|&b| b != 0);
        let mut u: Vec<u16> = bytes.split(|&b| b == 0).enumerate().flat_map(|(i, s)| (i > 0).then_some(0).into_iter().chain(a2w(s))).collect();
        if nul {
            u.push(0);
        }
        bytes = u.iter().flat_map(|c| c.to_le_bytes()).collect();
    }
    if crate::env_on!("SAKE_TRACE_REG") {
        let shown = if is_str(ty) {
            let u: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
            format!("{:?}", String::from_utf16_lossy(&u))
        } else {
            hex(&bytes)
        };
        eprintln!("sake reg: set {path}\\{name} type {ty}: {shown}");
    }
    with_reg(|m| {
        let k = m.entry(path).or_default();
        let lname = name.to_lowercase();
        // (replacing keeps the name's spelling and place)
        match k.values.iter_mut().find(|v| v.0.to_lowercase() == lname) {
            Some(v) => {
                v.1 = ty;
                v.2 = bytes;
            }
            None => k.values.push((name.to_string(), ty, bytes)),
        }
        k.ours = true;
        save_reg(m);
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
/// RegSetValueA(key, subkey, REG_SZ, text, len): the default value of the subkey (made if
/// needed); the text's own NUL ends it
fn RegSetValueA(c: &mut Ctx) -> u32 {
    let (k, sub, ty, d) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut h = k;
    if sub != 0 && r8(sub) != 0 {
        let tmp = mem::heap_alloc(8, true);
        let r = open_key(k, &arg_str(sub, false), true, tmp);
        h = mem::rp(tmp);
        mem::heap_free(tmp);
        if r != 0 {
            return r;
        }
    }
    let r = set_value(h, "", ty, d, cstr(d).len() as u32 + 1, false);
    if h != k {
        obj::close(h);
    }
    r
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
            k.ours = true;
            save_reg(m);
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
    let pre = format!("{p}\\");
    with_reg(|m| match m.keys().any(|k| k.starts_with(&pre)) {
        // a key with subkeys stays (ERROR_ACCESS_DENIED): RegDeleteTree removes those
        true => 5,
        false => match m.remove(&p) {
        Some(k) => {
            if k.ours {
                save_reg(m);
            }
            0
        }
        None => 2,
        },
    })
}
/// RegDeleteTree: `sub` with everything under it; with no `sub`, the key's subkeys and values.
fn delete_tree(key: u32, sub: Option<&str>) -> u32 {
    let Some(base) = key_path(key) else { return 6 };
    let p = join(&base, sub.unwrap_or(""));
    let pre = format!("{p}\\");
    with_reg(|m| {
        let doomed: Vec<String> = m.keys().filter(|k| k.starts_with(&pre) || sub.is_some() && **k == p).cloned().collect();
        let exists = m.contains_key(&p) || !doomed.is_empty();
        if !exists {
            return 2;
        }
        for k in doomed {
            m.remove(&k);
        }
        if sub.is_none() {
            if let Some(k) = m.get_mut(&p) {
                k.values.clear();
                k.ours = true;
            }
        }
        save_reg(m);
        0
    })
}
fn RegDeleteTreeA(c: &mut Ctx) -> u32 {
    let s = (c.a(1) != 0).then(|| arg_str(c.a(1), false));
    delete_tree(c.a(0), s.as_deref())
}
fn RegDeleteTreeW(c: &mut Ctx) -> u32 {
    let s = (c.a(1) != 0).then(|| arg_str(c.a(1), true));
    delete_tree(c.a(0), s.as_deref())
}
fn RegDeleteKeyA(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), false);
    delete_key(c.a(0), &s)
}
fn RegDeleteKeyW(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), true);
    delete_key(c.a(0), &s)
}
/// RegDeleteKeyEx(key, subkey, samDesired, reserved): the WOW64 view flags do not matter
/// here (one registry view).
fn RegDeleteKeyExA(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), false);
    delete_key(c.a(0), &s)
}
fn RegDeleteKeyExW(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(1), true);
    delete_key(c.a(0), &s)
}

/// Whether a key (a lower-case full path such as "hklm\\software") exists
pub(crate) fn reg_key_exists(path: &str) -> bool {
    let pre = format!("{path}\\");
    with_reg(|m| m.contains_key(path) || m.keys().any(|k| k.starts_with(&pre)))
}

/// The subkeys of a key (a lower-case full path)
pub(crate) fn reg_subkeys(path: &str) -> Vec<String> {
    subkeys(path)
}

/// The subkeys' names as created, in Windows' order (case-insensitive, by UTF-16 upper case)
fn subkeys(path: &str) -> Vec<String> {
    let pre = format!("{path}\\");
    let mut v: Vec<String> = with_reg(|m| {
        let mut low: Vec<String> = m.keys().filter_map(|k| k.strip_prefix(&pre)).map(|r| r.split('\\').next().unwrap_or("").to_string()).collect();
        low.sort();
        low.dedup();
        low.into_iter().map(|l| m.get(&format!("{pre}{l}")).map(|k| k.name.clone()).filter(|n| !n.is_empty()).unwrap_or(l)).collect()
    });
    let key = |s: &String| -> Vec<u16> { s.to_uppercase().encode_utf16().collect() };
    v.sort_by_key(key);
    v
}
/// A name's length as the A or W function counts it
fn name_len(s: &str, wide: bool) -> u32 {
    if wide { s.encode_utf16().count() as u32 } else { crate::text::s2a(s).len() as u32 }
}
fn enum_key(c: &mut Ctx, wide: bool, ex: bool) -> u32 {
    let (k, i, name) = (c.a(0), c.a(1), c.a(2));
    let Some(path) = key_path(k) else { return 6 };
    let Some(s) = subkeys(&path).into_iter().nth(i as usize) else { return 259 };
    if ex {
        let pn = c.a(3);
        let cap = r32(pn);
        if name_len(&s, wide) >= cap {
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
    if name_len(&vn, wide) >= cap {
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
            (kd.values.len(), kd.values.iter().map(|v| v.0.encode_utf16().count()).max().unwrap_or(0), kd.values.iter().map(|v| v.2.len()).max().unwrap_or(0))
        })
    });
    put32(c.a(1), 0);
    put32(c.a(2), 0);
    put32(c.a(4), subs.len() as u32);
    put32(c.a(5), subs.iter().map(|s| s.encode_utf16().count()).max().unwrap_or(0) as u32);
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
    let Some(base) = base else {
        // Inspect the file as data: querying an installed runtime's version must
        // neither require it to be loaded nor execute its initialization code.
        if let Some(version) = crate::files::resolve(&full).and_then(|host| file_resource_of(&host, 16)) {
            return Some(version);
        }
        return ours();
    };
    let Some(e) = crate::pe::find_resource(base, &crate::pe::ResId::Int(16), &crate::pe::ResId::Int(1))
        .or_else(|| crate::pe::first_resource(base, 16))
    else {
        return ours();
    };
    let (p, n) = crate::pe::resource_data(base, e);
    Some(mem::slice(p, n).to_vec())
}

/// Resource `ty` of the PE file at host path `host`, its headers and resource section read by
/// offset (`pe::file_resource_read`); the whole file only when that section does not hold it
/// and the file is small.
fn file_resource_of(host: &str, ty: u32) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = kura::fs::File::open(host).ok()?;
    let mut read = |at: u64, n: usize| -> Option<Vec<u8>> {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut v = Vec::new();
        (&mut f).take(n as u64).read_to_end(&mut v).ok()?;
        Some(v)
    };
    if let Some(v) = crate::pe::file_resource_read(&mut read, ty) {
        return Some(v);
    }
    let len = kura::fs::metadata(host).ok()?.len();
    if len > 64 << 20 {
        return None;
    }
    let data = read(0, len as usize)?;
    crate::pe::file_resource(&data, ty).map(<[u8]>::to_vec)
}

/// A file's VS_FIXEDFILEINFO version (MS, LS) and its Translation languages (msi's
/// signatures).
pub(crate) fn file_version(path: &str) -> Option<(u32, u32, Vec<u16>)> {
    let host = crate::files::resolve(&crate::files::full_path(path))?;
    let v = file_resource_of(&host, 16)?;
    let v = &v[..];
    let at = v.windows(4).position(|w| w == [0xbd, 0x04, 0xef, 0xfe])?;
    let d = |k: usize| v.get(at + k..at + k + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let (ms, ls) = (d(8)?, d(12)?);
    let key: Vec<u8> = "Translation".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut langs = Vec::new();
    if let Some(k) = v.windows(key.len()).position(|w| w == key.as_slice()) {
        // (the key, its NUL, padding to 4: then LANGID/codepage pairs; wValueLength before)
        let start = (k + key.len() + 2 + 3) & !3;
        // (a key at the very start of the block has no header before it: no languages)
        let len = k.checked_sub(4).map_or(0, |h| u16::from_le_bytes([v[h], v[h + 1]]) as usize);
        for c in v.get(start..start + len).unwrap_or(&[]).chunks_exact(4) {
            langs.push(u16::from_le_bytes([c[0], c[1]]));
        }
    }
    Some((ms, ls, langs))
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

/// The version block of `path`, or the error: no file (2), no version resource (1813).
fn version_or_error(path: &str) -> Option<Vec<u8>> {
    let v = version_resource(path);
    if v.is_none() {
        crate::k32::last_error(if crate::files::resolve(&crate::files::full_path(path)).is_some() { 1813 } else { 2 });
    }
    v
}
fn GetFileVersionInfoSizeA(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    version_or_error(&arg_str(c.a(0), false)).map_or(0, |v| v.len() as u32 * 2)
}
fn GetFileVersionInfoSizeW(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    version_or_error(&arg_str(c.a(0), true)).map_or(0, |v| v.len() as u32 * 2)
}
fn get_version_info(path: &str, len: u32, out: u32) -> u32 {
    let Some(v) = version_or_error(path) else { return 0 };
    crate::k32::last_error(0);
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
        mem::putp(pp, at);
        put32(plen, a.len() as u32 + 1);
    } else {
        mem::putp(pp, val);
        put32(plen, n);
    }
    crate::k32::last_error(0);
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

/// Each thread's CoInitialize and OleInitialize counts (OleInitialize counts for both): a
/// repeated initialization is S_FALSE, and drag and drop needs both
static COM: Mutex<Option<HashMap<u32, (u32, u32)>>> = Mutex::new(None);
fn com<R>(f: impl FnOnce(&mut (u32, u32)) -> R) -> R {
    f(COM.lock().unwrap().get_or_insert_with(HashMap::default).entry(crate::cur().tid).or_default())
}
/// Whether the current thread has COM (or WinRT, RoInitialize) initialized.
pub fn com_initialized() -> bool {
    com(|n| n.0 > 0)
}
/// RoInitialize / RoUninitialize count as CoInitializeEx / CoUninitialize.
pub fn com_enter(c: &mut Ctx) -> u32 {
    CoInitialize(c)
}
pub fn com_leave(c: &mut Ctx) -> u32 {
    CoUninitialize(c)
}
/// CoInitializeSecurity's answer: CO_E_NOTINITIALIZED before the thread has COM, S_OK the
/// first time in the process, RPC_E_TOO_LATE after (the security is set once)
pub fn init_security_result(com_ready: bool, done_before: bool) -> u32 {
    if !com_ready {
        0x8004_01f0
    } else if done_before {
        0x8001_0119
    } else {
        0
    }
}
static SECURITY_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// CoInitializeSecurity(sd, auth count, services, reserved, authn level, imp level, list,
/// capabilities, reserved): sake has no calls between processes to secure (WMI is in the
/// process), so it is only remembered
fn CoInitializeSecurity(_: &mut Ctx) -> u32 {
    let ready = com_initialized();
    init_security_result(ready, ready && SECURITY_SET.swap(true, std::sync::atomic::Ordering::SeqCst))
}
/// CoSetProxyBlanket(proxy, authn, authz, principal, level, imp level, identity, caps): ole32
/// asks the proxy for IClientSecurity {0000013d-0000-0000-c000-000000000046} and answers its
/// QueryInterface's failure (E_NOINTERFACE for an object in the process, as on Windows). An
/// object that has it is one sake stands in for a remote one with (WMI's services: SDRT and the
/// WMI samples call this after ConnectServer): S_OK, there being no blanket to set.
fn CoSetProxyBlanket(c: &mut Ctx) -> u32 {
    let proxy = c.a(0);
    if proxy == 0 {
        return 0x8007_0057; // E_INVALIDARG
    }
    let iid = mem::heap_alloc(16 + 8, true);
    mem::write(iid, &[0x3d, 0x01, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    let out = iid + 16;
    let hr = c.call(mem::rp(mem::rp(proxy)), &[proxy, iid, out]);
    let cs = mem::rp(out);
    mem::heap_free(iid);
    if hr & 0x8000_0000 != 0 {
        return hr;
    }
    if cs != 0 {
        c.call(mem::rp(mem::rp(cs) + 2 * mem::ps()), &[cs]);
    }
    0
}
fn CoInitialize(_: &mut Ctx) -> u32 {
    com(|n| {
        n.0 += 1;
        (n.0 > 1) as u32
    })
}
fn CoInitializeEx(c: &mut Ctx) -> u32 {
    CoInitialize(c)
}
fn OleInitialize(_: &mut Ctx) -> u32 {
    com(|n| {
        n.0 += 1;
        n.1 += 1;
        (n.1 > 1) as u32
    })
}
fn CoUninitialize(_: &mut Ctx) -> u32 {
    com(|n| n.0 = n.0.saturating_sub(1));
    0
}
fn OleUninitialize(_: &mut Ctx) -> u32 {
    com(|n| {
        n.0 = n.0.saturating_sub(1);
        n.1 = n.1.saturating_sub(1);
    });
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
                ("szProcessorEnglish", Str("masu x86")),
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
                ("szDescription", Str("masu WebGPU")),
                ("szDeviceName", Str("\\\\.\\DISPLAY1")),
                ("szManufacturer", Str("masu")),
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
        let v = mem::heap_alloc(mem::ps() * m.len() as u32, true);
        for (i, (f, b)) in m.iter().enumerate() {
            mem::wp(v + mem::ps() * i as u32, crate::api::host_fn(&format!("IDxDiagContainer::{i}"), *f, *b));
        }
        v
    });
    let o = mem::heap_alloc(2 * mem::ps(), true);
    mem::wp(o, vt);
    DX_NODES.lock().unwrap().get_or_insert_with(HashMap::default).insert(o, path.to_string());
    o
}
fn dx_path(o: u32) -> String {
    DX_NODES.lock().unwrap().get_or_insert_with(HashMap::default).get(&o).cloned().unwrap_or_default()
}
fn dx_qi(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), c.a(0));
    0
}
fn dx_addref(_: &mut Ctx) -> u32 {
    1
}
fn dxp_initialize(_: &mut Ctx) -> u32 {
    0
}
fn dxp_root(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), dx_container(""));
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
        mem::putp(c.a(2), 0);
        return 0x8007_0057;
    }
    mem::putp(c.a(2), dx_container(&path));
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
    // (a VARIANT: the type, then the value at 8; 24 bytes on x64)
    mem::fill(v, 2 * mem::ps() + 8, 0);
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
            mem::wp(v + 8, bstr_alloc(&st.encode_utf16().collect::<Vec<u16>>()));
        }
    }
    0
}

/// CLSID_DxDiagProvider
fn dxdiag_provider() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        let m: &[(crate::api::Handler, u32)] = &[(dx_qi, 12), (dx_addref, 4), (dx_addref, 4), (dxp_initialize, 8), (dxp_root, 8)];
        let v = mem::heap_alloc(mem::ps() * m.len() as u32, true);
        for (i, (f, b)) in m.iter().enumerate() {
            mem::wp(v + mem::ps() * i as u32, crate::api::host_fn(&format!("IDxDiagProvider::{i}"), *f, *b));
        }
        v
    });
    let o = mem::heap_alloc(2 * mem::ps(), true);
    mem::wp(o, vt);
    o
}

fn CoCreateInstance(c: &mut Ctx) -> u32 {
    let (clsid, iid, out) = (c.a(0), c.a(3), c.a(4));
    // (the VMR-9 can be aggregated)
    if let Some(hr) = crate::vmr9::create_instance(c, clsid, c.a(1), iid, out) {
        return hr;
    }
    // (DirectMusic refuses an outer object)
    if let Some(hr) = crate::dmusic::create_instance(clsid, c.a(1), iid, out) {
        return hr;
    }
    if let Some(hr) = crate::dmime::create_instance(clsid, c.a(1), iid, out) {
        return hr;
    }
    let hr = create_object(c, clsid, iid, out);
    if hr == 0x8004_0154 {
        let (context, outer) = (c.a(2), c.a(1));
        crate::comrpc::create(c.cpu, clsid, iid, out, context, outer)
    } else { hr }
}
fn CoGetClassObject(c: &mut Ctx) -> u32 {
    let (clsid, context, reserved, iid, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if out == 0 || clsid == 0 || iid == 0 { return 0x8000_4003; }
    mem::putp(out, 0);
    if reserved != 0 { return 0x8007_0057; }
    let hr = crate::comrpc::get_class(c.cpu, clsid, iid, out, context);
    if hr != 0 && crate::api::trace() != 0 {
        eprintln!("sake: CoGetClassObject {} iid {} context {context:x}: {hr:08x}",
            crate::comrpc::text(mem::slice(clsid, 16)), crate::comrpc::text(mem::slice(iid, 16)));
    }
    hr
}

/// The object of class `clsid` into *out (what CoCreateInstance and the class factories of
/// DllGetClassObject make): S_OK, or REGDB_E_CLASSNOTREG with *out NULL.
fn create_object(c: &mut Ctx, clsid: u32, iid: u32, out: u32) -> u32 {
    if r32(clsid) == 0xa65b_8071 {
        mem::putp(out, dxdiag_provider());
        return 0;
    }
    if let Some(hr) = crate::shelllink::create_instance(clsid, iid, out) {
        return hr;
    }
    if let Some(hr) = crate::webbrowser::create_instance(clsid, iid, out) {
        return hr;
    }
    if let Some(obj) = crate::ddraw::create_instance(clsid, iid) {
        mem::putp(out, obj);
        return 0;
    }
    if let Some(obj) = crate::mf::create_instance(clsid, iid) {
        mem::putp(out, obj);
        return if obj == 0 { 0x8000_4002 } else { 0 };
    }
    if let Some(hr) = crate::vmr9::create_instance(c, clsid, 0, iid, out) {
        return hr;
    }
    if let Some(obj) = crate::dshow_mpeg::create_instance(clsid, iid) {
        mem::putp(out, obj);
        return if obj == 0 { 0x8000_4002 } else { 0 };
    }
    if let Some(obj) = crate::dshow::create_instance(clsid, iid) {
        mem::putp(out, obj);
        return 0;
    }
    if let Some(obj) = crate::amstream::create_instance(clsid, iid) {
        mem::putp(out, obj);
        return if obj == 0 { 0x8000_4002 } else { 0 };
    }
    if let Some(hr) = crate::dmusic::create_instance(clsid, 0, iid, out) {
        return hr;
    }
    if let Some(hr) = crate::dmime::create_instance(clsid, 0, iid, out) {
        return hr;
    }
    if let Some(obj) = crate::dinput::create_instance(clsid) {
        mem::putp(out, obj);
        return 0;
    }
    if let Some(obj) = crate::mmdevapi::create_instance(clsid) {
        mem::putp(c.a(4), obj);
        return 0;
    }
    if let Some(obj) = crate::com::create_instance(clsid) {
        mem::putp(out, obj);
        return 0;
    }
    if let Some(obj) = crate::xaudio2::create_instance(clsid) {
        mem::putp(out, obj);
        return 0;
    }
    if let Some(obj) = crate::xapo::create_instance(clsid) {
        mem::putp(out, obj);
        return 0;
    }
    if let Some(obj) = crate::jscript::create_instance(clsid) {
        mem::putp(out, obj);
        return 0;
    }
    if let Some(obj) = crate::sound::create_instance(c, clsid, iid) {
        mem::putp(out, obj);
        return 0;
    }
    mem::putp(out, 0);
    0x8004_0154
}

/// The classes each COM DLL serves from DllGetClassObject (first CLSID dwords; sake's
/// classes are told apart by them, as create_object does).
fn dll_classes(dll: &str) -> &'static [u32] {
    match dll {
        // CLSID_XAudio2, CLSID_XAudio2_Debug, CLSID_AudioVolumeMeter, CLSID_AudioReverb (2.7)
        "xaudio2_7" => &[0x5a50_8685, 0xdb05_ea35, 0xcac1_105f, 0x6a93_130e],
        // CLSID_WICImagingFactory, CLSID_WICImagingFactory2
        "windowscodecs" => &[crate::com::CLSID_WIC_FACTORY, crate::com::CLSID_WIC_FACTORY2],
        _ => &[],
    }
}

/// DllGetClassObject(rclsid, riid, ppv) of `dll`: an IClassFactory for one of its classes, or
/// CLASS_E_CLASSNOTAVAILABLE.
fn dll_get_class_object(c: &mut Ctx, dll: &str) -> u32 {
    let (clsid, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return 0x8000_4003; // E_POINTER
    }
    mem::putp(out, 0);
    if !dll_classes(dll).contains(&r32(clsid)) {
        return 0x8004_0111;
    }
    // IUnknown or IClassFactory {00000001-0000-0000-c000-000000000046}
    if !(matches!(r32(iid), 0 | 1) && r16(iid + 4) == 0 && r16(iid + 6) == 0 && r32(iid + 8) == 0xc0 && r32(iid + 12) == 0x4600_0000) {
        return 0x8000_4002; // E_NOINTERFACE
    }
    mem::putp(out, class_factory(clsid));
    0
}

/// An IClassFactory making objects of class `clsid` (CreateInstance = CoCreateInstance).
/// Layout: vtable, refs, the CLSID.
fn class_factory(clsid: u32) -> u32 {
    use crate::api::Handler;
    fn qi(c: &mut Ctx) -> u32 {
        let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
        let unk_or_cf = matches!(r32(iid), 0 | 1) && r16(iid + 4) == 0 && r16(iid + 6) == 0 && r32(iid + 8) == 0xc0 && r32(iid + 12) == 0x4600_0000;
        if !unk_or_cf {
            mem::putp(out, 0);
            return 0x8000_4002;
        }
        add_ref(c);
        mem::putp(out, this);
        0
    }
    fn add_ref(c: &mut Ctx) -> u32 {
        let r = c.a(0) + mem::ps();
        w32(r, r32(r) + 1);
        r32(r)
    }
    fn release(c: &mut Ctx) -> u32 {
        let r = c.a(0) + mem::ps();
        let n = r32(r).saturating_sub(1);
        w32(r, n);
        n
    }
    /// CreateInstance(this, outer, riid, ppv)
    fn create(c: &mut Ctx) -> u32 {
        let (this, outer, iid, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
        if out == 0 {
            return 0x8000_4003;
        }
        if outer != 0 {
            mem::putp(out, 0);
            return 0x8004_0110; // CLASS_E_NOAGGREGATION
        }
        create_object(c, this + mem::ps() + 4, iid, out)
    }
    fn lock(_: &mut Ctx) -> u32 {
        0
    }
    static VT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *VT.get_or_init(|| {
        let m: [(Handler, u32); 5] = [(qi, 12), (add_ref, 4), (release, 4), (create, 16), (lock, 8)];
        crate::api::vtable("IClassFactory", &m)
    });
    let o = mem::heap_alloc(mem::ps() + 4 + 16, true);
    mem::wp(o, vt);
    w32(o + mem::ps(), 1);
    for i in 0..4 {
        w32(o + mem::ps() + 4 + 4 * i, r32(clsid + 4 * i));
    }
    o
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
        // (x64: a table of 8-byte entries, the object a pointer to it)
        let ps = mem::ps();
        let v = mem::heap_alloc(ps * m.len() as u32, true);
        for (i, (f, n)) in m.iter().enumerate() {
            mem::wp(v + ps * i as u32, crate::api::host_fn(&format!("IMalloc::{i}"), *f, *n));
        }
        let o = mem::heap_alloc(ps, true);
        mem::wp(o, v);
        o
    })
}
fn IsUserAnAdmin(_: &mut Ctx) -> u32 {
    1
}
fn SHGetMalloc(c: &mut Ctx) -> u32 {
    mem::putp(c.a(0), malloc_obj());
    0
}
fn CoGetMalloc(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), malloc_obj());
    0
}
/// In-process marshaling (every apartment of sake's one process shares its objects, as the
/// free-threaded marshaler does): the stream carries a tag and the interface pointer, which
/// holds the reference the unmarshaling takes over.
const MARSHAL_TAG: u32 = 0x5341_4b45;
/// CoMarshalInterface(stream, riid, unknown, dest context, reserved, flags)
fn CoMarshalInterface(c: &mut Ctx) -> u32 {
    let (stm, iid, unk) = (c.a(0), c.a(1), c.a(2));
    if stm == 0 || unk == 0 {
        return 0x8007_0057;
    }
    let out = mem::heap_alloc(16, true);
    let hr = crate::d3dx9::com_call(c, unk, 0, &[iid, out]);
    if hr != 0 {
        mem::heap_free(out);
        return hr;
    }
    let p = mem::rp(out);
    w32(out, MARSHAL_TAG);
    w32(out + 4, p);
    w32(out + 8, (p as u64 >> 32) as u32);
    // IStream::Write(data, 12, NULL)
    let hr = crate::d3dx9::com_call(c, stm, 4, &[out, 12, 0]);
    if hr != 0 {
        crate::d3dx9::com_call(c, p, 2, &[]);
    }
    // (the stream has its copy: the block went with every call before)
    mem::heap_free(out);
    hr
}
/// The interface pointer CoMarshalInterface wrote to `stm` (it holds a reference), or an error
fn read_marshaled(c: &mut Ctx, stm: u32) -> Result<u32, u32> {
    if stm == 0 {
        return Err(0x8007_0057);
    }
    let buf = mem::heap_alloc(16, true);
    let hr = crate::d3dx9::com_call(c, stm, 3, &[buf, 12, buf + 12]);
    let (got, tag, p) = (mem::r32(buf + 12), mem::r32(buf), mem::r32(buf + 4));
    mem::heap_free(buf);
    if hr != 0 || got != 12 || tag != MARSHAL_TAG {
        // (STG_E_READFAULT)
        return Err(if hr != 0 { hr } else { 0x8003_001e });
    }
    Ok(p)
}
/// CoUnmarshalInterface(stream, riid, void**)
fn CoUnmarshalInterface(c: &mut Ctx) -> u32 {
    let (stm, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return 0x8000_4003;
    }
    mem::putp(out, 0);
    let p = match read_marshaled(c, stm) {
        Ok(p) => p,
        Err(hr) => return hr,
    };
    let hr = crate::d3dx9::com_call(c, p, 0, &[iid, out]);
    crate::d3dx9::com_call(c, p, 2, &[]);
    hr
}
/// CoReleaseMarshalData(stream): the marshaled reference goes
fn CoReleaseMarshalData(c: &mut Ctx) -> u32 {
    match read_marshaled(c, c.a(0)) {
        Ok(p) => {
            crate::d3dx9::com_call(c, p, 2, &[]);
            0
        }
        Err(hr) => hr,
    }
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
/// StringFromCLSID / StringFromIID: the "{...}" string in CoTaskMem
fn StringFromCLSID(c: &mut Ctx) -> u32 {
    let (g, out) = (c.a(0), c.a(1));
    let s: Vec<u16> = guid_string(g).encode_utf16().collect();
    let p = mem::heap_alloc(2 * s.len() as u32 + 2, false);
    mem::put_wstr(p, s.len() as u32 + 1, &s);
    mem::putp(out, p);
    0
}
fn StringFromIID(c: &mut Ctx) -> u32 {
    StringFromCLSID(c)
}
/// CLSIDFromString / IIDFromString: "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}" (ProgIDs are not
/// registered: CO_E_CLASSSTRING).
/// CLSIDFromString / IIDFromString as combase parses: NULL is GUID_NULL (S_OK), "" fails
/// leaving the GUID, a string not starting with '{' zeroes it, and a "{...}" one is read field
/// by field into the GUID, so a bad character leaves what came before it (as Windows does).
fn guid_from_string(c: &mut Ctx, err: u32) -> u32 {
    let (s, out) = (c.a(0), c.a(1));
    if s == 0 {
        mem::fill(out, 16, 0);
        return 0;
    }
    let w = wstr(s);
    if w.is_empty() {
        return err;
    }
    if w[0] != b'{' as u16 {
        mem::fill(out, 16, 0);
        return err;
    }
    let at = |i: usize| w.get(i).copied().unwrap_or(0);
    let hex = |i: usize| char::from_u32(at(i) as u32).and_then(|ch| ch.to_digit(16));
    let field = |from: usize, n: usize, put: &dyn Fn(u32)| -> bool {
        let mut v = 0u32;
        put(0);
        for i in from..from + n {
            let Some(d) = hex(i) else { return false };
            v = v << 4 | d;
            put(v);
        }
        true
    };
    if !field(1, 8, &|v| w32(out, v)) || at(9) != b'-' as u16 {
        return err;
    }
    if !field(10, 4, &|v| w16(out + 4, v as u16)) || at(14) != b'-' as u16 {
        return err;
    }
    if !field(15, 4, &|v| w16(out + 6, v as u16)) || at(19) != b'-' as u16 {
        return err;
    }
    let mut i = 20;
    for k in 0..8u32 {
        if i == 24 {
            if at(i) != b'-' as u16 {
                return err;
            }
            i += 1;
        }
        let (Some(h), Some(l)) = (hex(i), hex(i + 1)) else { return err };
        mem::w8(out + 8 + k, (h << 4 | l) as u8);
        i += 2;
    }
    if at(37) == b'}' as u16 && w.len() == 38 { 0 } else { err }
}
/// CLSIDFromProgID: the script engines' names (other ProgIDs are not registered).
fn CLSIDFromProgID(c: &mut Ctx) -> u32 {
    let name = String::from_utf16_lossy(&wstr(c.a(0)));
    match crate::jscript::progid(&name) {
        Some(g) => {
            mem::write(c.a(1), &g);
            0
        }
        None => {
            eprintln!("sake: CLSIDFromProgID {name}: not registered");
            0x8004_01f3 // CO_E_CLASSSTRING
        }
    }
}
fn CLSIDFromString(c: &mut Ctx) -> u32 {
    guid_from_string(c, 0x8004_01f3) // CO_E_CLASSSTRING
}
fn IIDFromString(c: &mut Ctx) -> u32 {
    guid_from_string(c, 0x8004_01f4) // CO_E_IIDSTRING
}
// rpcrt4's UUIDs (the same GUIDs; strings without braces, lower case)
fn UuidCreate(c: &mut Ctx) -> u32 {
    CoCreateGuid(c);
    let p = c.a(0);
    w16(p + 6, (r16(p + 6) & 0x0fff) | 0x4000);
    mem::w8(p + 8, (mem::r8(p + 8) & 0x3f) | 0x80);
    0
}
fn UuidCreateNil(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), 16, 0);
    0
}
fn uuid_text(p: u32) -> String {
    guid_string(p).trim_matches(['{', '}']).to_ascii_lowercase()
}
fn uuid_to_string(c: &mut Ctx, wide: bool) -> u32 {
    let (u, out) = (c.a(0), c.a(1));
    let t = uuid_text(u);
    let p = mem::heap_alloc(2 * (t.len() as u32 + 1), true);
    for (i, b) in t.bytes().enumerate() {
        if wide { w16(p + 2 * i as u32, b as u16) } else { mem::w8(p + i as u32, b) }
    }
    mem::wp(out, p);
    0
}
fn UuidToStringA(c: &mut Ctx) -> u32 {
    uuid_to_string(c, false)
}
fn UuidToStringW(c: &mut Ctx) -> u32 {
    uuid_to_string(c, true)
}
fn RpcStringFreeA(c: &mut Ctx) -> u32 {
    let pp = c.a(0);
    if pp != 0 && mem::rp(pp) != 0 {
        mem::heap_free(mem::rp(pp));
        mem::wp(pp, 0);
    }
    0
}
/// A UUID's text as rpcrt4 reads it: exactly 8-4-4-4-12 hex digits (either case), no braces,
/// nothing before or after. The 16 bytes as they are in memory.
pub fn parse_uuid(t: &[u16]) -> Option<[u8; 16]> {
    if t.len() != 36 {
        return None;
    }
    let mut digits = [0u8; 32];
    let mut n = 0;
    for (i, &ch) in t.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if ch != u16::from(b'-') {
                return None;
            }
            continue;
        }
        digits[n] = char::from_u32(ch.into()).and_then(|c| c.to_digit(16))? as u8;
        n += 1;
    }
    let byte = |k: usize| digits[2 * k] << 4 | digits[2 * k + 1];
    let mut u = [0u8; 16];
    // Data1 (4 bytes), Data2, Data3 little-endian; Data4 as written
    for (k, at) in [(0, 3), (1, 2), (2, 1), (3, 0), (4, 5), (5, 4), (6, 7), (7, 6)] {
        u[at] = byte(k);
    }
    for k in 8..16 {
        u[k] = byte(k);
    }
    Some(u)
}
fn uuid_from_string(c: &mut Ctx, wide: bool) -> u32 {
    let (s, out) = (c.a(0), c.a(1));
    if s == 0 {
        mem::fill(out, 16, 0);
        return 0;
    }
    let t: Vec<u16> = if wide { wstr(s) } else { cstr(s).into_iter().map(u16::from).collect() };
    match parse_uuid(&t) {
        Some(u) => {
            mem::write(out, &u);
            0
        }
        None => 1705, // RPC_S_INVALID_STRING_UUID
    }
}
fn UuidFromStringA(c: &mut Ctx) -> u32 {
    uuid_from_string(c, false)
}
fn UuidFromStringW(c: &mut Ctx) -> u32 {
    uuid_from_string(c, true)
}
/// UuidEqual(a, b, &status)
fn UuidEqual(c: &mut Ctx) -> u32 {
    put32(c.a(2), 0);
    let z = [0u8; 16];
    let (a, b) = (c.a(0), c.a(1));
    let va = if a == 0 { z.to_vec() } else { mem::slice(a, 16).to_vec() };
    let vb = if b == 0 { z.to_vec() } else { mem::slice(b, 16).to_vec() };
    (va == vb) as u32
}
fn UuidIsNil(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    (c.a(0) == 0 || mem::slice(c.a(0), 16).iter().all(|&b| b == 0)) as u32
}
// OLE drag and drop: a window's IDropTarget is its property "OleDropTargetInterface"
const DROP_PROP: &str = "oledroptargetinterface";
const DRAGDROP_S_DROP: u32 = 0x0004_0100;
const DRAGDROP_E_NOTREGISTERED: u32 = 0x8004_0100;
const DRAGDROP_E_ALREADYREGISTERED: u32 = 0x8004_0101;
const DRAGDROP_E_INVALIDHWND: u32 = 0x8004_0102;
const CO_E_NOTINITIALIZED: u32 = 0x8004_01f0;

/// An argument of a COM call: a pointer or DWORD, or a POINTL by value (two DWORDs on x86,
/// one register on x64)
#[derive(Clone, Copy)]
enum Arg {
    P(u32),
    Pt(i32, i32),
}
/// Calls method `index` of COM object `obj`
fn method(c: &mut Ctx, obj: u32, index: u32, args: &[Arg]) -> u32 {
    let f = mem::rp(mem::rp(obj) + index * mem::ps());
    if mem::x64() {
        let mut a = vec![mem::widen(obj)];
        for &x in args {
            a.push(match x {
                Arg::P(p) => mem::widen(p),
                Arg::Pt(x, y) => x as u32 as u64 | (y as u32 as u64) << 32,
            });
        }
        return crate::call64(c.cpu, mem::widen(f), &a) as u32;
    }
    let mut a = vec![obj];
    for &x in args {
        match x {
            Arg::P(p) => a.push(p),
            Arg::Pt(x, y) => a.extend([x as u32, y as u32]),
        }
    }
    c.call(f, &a)
}

/// Drag and drop needs OleInitialize (without it E_OUTOFMEMORY, without COM
/// CO_E_NOTINITIALIZED, as Windows)
fn drag_ready() -> Result<(), u32> {
    let (com_n, ole_n) = com(|n| *n);
    if ole_n == 0 {
        Err(0x8007_000e)
    } else if com_n == 0 {
        Err(CO_E_NOTINITIALIZED)
    } else {
        Ok(())
    }
}
/// RegisterDragDrop(hwnd, IDropTarget*): the target, AddRef'd, as the window's property
fn RegisterDragDrop(c: &mut Ctx) -> u32 {
    let (h, t) = (c.a(0), c.a(1));
    if let Err(e) = drag_ready() {
        return e;
    }
    if t == 0 {
        return 0x8007_0057;
    }
    if h == 0 || !crate::user::is_window(h) {
        return DRAGDROP_E_INVALIDHWND;
    }
    if crate::user::prop(h, DROP_PROP).is_some() {
        return DRAGDROP_E_ALREADYREGISTERED;
    }
    method(c, t, 1, &[]);
    crate::user::set_prop(h, DROP_PROP, Some(t));
    0
}
/// RevokeDragDrop(hwnd): the target released
fn RevokeDragDrop(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h == 0 || !crate::user::is_window(h) {
        return DRAGDROP_E_INVALIDHWND;
    }
    let Some(t) = crate::user::set_prop(h, DROP_PROP, None) else { return DRAGDROP_E_NOTREGISTERED };
    method(c, t, 2, &[]);
    0
}
/// The MK_* state of the mouse buttons and modifier keys, and whether Escape is down
fn drag_keys() -> (u32, bool) {
    let k = crate::user::key_states();
    let down = |vk: usize| k[vk] & 0x80 != 0;
    let mut m = 0;
    for (vk, bit) in [(1, 1), (2, 2), (0x10, 4), (0x11, 8), (4, 0x10), (0x12, 0x20)] {
        if down(vk) {
            m |= bit;
        }
    }
    (m, down(0x1b))
}
/// DoDragDrop(IDataObject*, IDropSource*, allowed effects, &effect): until the source's
/// QueryContinueDrag says to drop or cancel, the target under the cursor gets DragEnter
/// (a new one; the one left DragLeave) and DragOver, the source GiveFeedback of what the
/// target accepts (within the allowed effects). On a drop the target gets Drop when it accepts
/// something, else DragLeave; its failure is the answer. As Windows (Wine's dragdrop test).
fn DoDragDrop(c: &mut Ctx) -> u32 {
    let (data, src, allowed, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if drag_ready().is_err() {
        return CO_E_NOTINITIALIZED;
    }
    if data == 0 || src == 0 || out == 0 {
        return 0x8007_0057;
    }
    // (first the data's formats: Windows enumerates them twice, then asks for each)
    let e = mem::heap_alloc(8 + 64 * 8, true);
    if method(c, data, 8, &[Arg::P(1), Arg::P(e)]) == 0 && mem::rp(e) != 0 {
        let en = mem::rp(e);
        let mut n = 0;
        for pass in 0..2 {
            if pass == 1 {
                method(c, en, 5, &[]);
            }
            n = 0;
            while n < 8 && method(c, en, 3, &[Arg::P(1), Arg::P(e + 8 + 64 * n), Arg::P(0)]) == 0 {
                n += 1;
            }
        }
        for i in 0..n {
            method(c, data, 5, &[Arg::P(e + 8 + 64 * i)]);
        }
        method(c, en, 2, &[]);
    }
    mem::heap_free(e);
    // (the mouse captured meanwhile, by the window the drag starts over)
    let (x0, y0) = crate::user::cursor_pos();
    let prev = crate::user::capture(crate::user::window_from_point(x0, y0));
    let ef = mem::heap_alloc(4, true);
    // (the target entered and what it accepts)
    let mut cur: Option<(u32, u32)> = None;
    let mut accepted = 0;
    let hr = loop {
        let (keys, esc) = drag_keys();
        let hr = method(c, src, 3, &[Arg::P(esc as u32), Arg::P(keys)]);
        let (x, y) = crate::user::cursor_pos();
        let pt = Arg::Pt(x, y);
        let at = crate::user::prop_at(x, y, DROP_PROP);
        if hr == 0 || hr == DRAGDROP_S_DROP {
            if cur.is_some() && cur != at {
                method(c, cur.unwrap().1, 5, &[]);
                cur = None;
            }
            if cur.is_none() {
                if let Some((_, t)) = at {
                    w32(ef, allowed);
                    method(c, t, 3, &[Arg::P(data), Arg::P(keys), pt, Arg::P(ef)]);
                    accepted = r32(ef) & allowed;
                    cur = at;
                } else {
                    accepted = 0;
                }
                method(c, src, 4, &[Arg::P(accepted)]);
            }
        }
        match hr {
            0 => {
                if let Some((_, t)) = cur {
                    w32(ef, allowed);
                    method(c, t, 4, &[Arg::P(keys), pt, Arg::P(ef)]);
                    accepted = r32(ef) & allowed;
                }
                method(c, src, 4, &[Arg::P(accepted)]);
                // (a save state taken meanwhile parks this thread here, as at any wait)
                kura::snap::sleep(Duration::from_millis(10));
                crate::snap::point();
            }
            DRAGDROP_S_DROP => {
                let mut effect = 0;
                let mut hr = DRAGDROP_S_DROP;
                if let Some((_, t)) = cur {
                    if accepted != 0 {
                        w32(ef, allowed);
                        let r = method(c, t, 6, &[Arg::P(data), Arg::P(keys), pt, Arg::P(ef)]);
                        effect = r32(ef) & allowed;
                        if r & 0x8000_0000 != 0 {
                            hr = r;
                        }
                    } else {
                        method(c, t, 5, &[]);
                    }
                }
                w32(out, effect);
                break hr;
            }
            _ => {
                if let Some((_, t)) = cur {
                    method(c, t, 5, &[]);
                }
                w32(out, 0);
                break hr;
            }
        }
    };
    mem::heap_free(ef);
    crate::user::capture(prev);
    hr
}

pub(crate) fn bstr_alloc(chars: &[u16]) -> u32 {
    let Some(p) = bstr_block(chars.len() as u64 * 2) else { return 0 };
    for (i, &c) in chars.iter().enumerate() {
        w16(p + 4 + 2 * i as u32, c);
    }
    p + 4
}
/// A BSTR's block for `bytes` bytes of text (its length word set, zeroed): its address, or None
/// past what a BSTR holds (0x7fffffff bytes less its header) or the heap gives.
fn bstr_block(bytes: u64) -> Option<u32> {
    if bytes > 0x7fff_fff0 - 6 {
        return None;
    }
    let p = mem::heap_alloc(bytes as u32 + 6, true);
    if p == 0 {
        return None;
    }
    w32(p, bytes as u32);
    Some(p)
}
/// A BSTR of `units` UTF-16 units copied from guest `src` (0: zeros): SysAllocStringLen.
fn bstr_from_guest(src: u32, units: u32) -> u32 {
    let Some(p) = bstr_block(units as u64 * 2) else { return 0 };
    if src != 0 {
        mem::copy(p + 4, src, 2 * units);
    }
    p + 4
}
pub(crate) fn bstr_free(b: u32) {
    if b != 0 {
        mem::heap_free(b - 4);
    }
}
/// VarBstrFromBool(value, lcid, flags, &out): "True" for any nonzero value, "False" for 0.
fn VarBstrFromBool(c: &mut Ctx) -> u32 {
    let (v, out) = (c.a(0) as u16, c.a(3));
    if out == 0 {
        return 0x8007_0057;
    }
    let s: Vec<u16> = if v != 0 { "True" } else { "False" }.encode_utf16().collect();
    mem::wp(out, bstr_alloc(&s));
    0
}
fn SysAllocString(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return 0;
    }
    let chars = wstr(c.a(0));
    let b = bstr_alloc(&chars);
    // Lengths only: Automation arguments can contain passwords or license tokens.
    if crate::env_on!("SAKE_TRACE_AUTOMATION") {
        eprintln!("sake: Automation allocate units={} bytes={} pointer={b:x}", chars.len(), r32(b - 4));
    }
    b
}
fn SysAllocStringLen(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    bstr_from_guest(p, n)
}
fn SysAllocStringByteLen(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let Some(b) = bstr_block(n as u64) else { return 0 };
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
    let nb = bstr_from_guest(s, n);
    if nb == 0 {
        return 0;
    }
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
        let len = r32(c.a(0) - 4) / 2;
        if crate::env_on!("SAKE_TRACE_AUTOMATION") {
            eprintln!("sake: Automation length units={len} pointer={:x}", c.a(0));
        }
        len
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
    // (a type that is no VARTYPE stays: DISP_E_BADVARTYPE)
    let t = r16(v) & 0x0fff;
    if t > 37 && !matches!(t, 64..=72 | 0xfff) {
        return 0x8002_0008;
    }
    variant_clear(c, v);
    0
}
/// Frees what VARIANT `v` owns (its BSTR; a reference to its interface) and empties it
fn variant_clear(c: &mut Ctx, v: u32) {
    match r16(v) {
        8 => bstr_free(mem::rp(v + 8)),
        // VT_DISPATCH, VT_UNKNOWN: the VARIANT's reference goes
        9 | 13 => {
            let o = mem::rp(v + 8);
            if o != 0 {
                let release = mem::rp(mem::rp(o) + 2 * mem::ps());
                c.call(release, &[o]);
            }
        }
        _ => {}
    }
    mem::fill(v, 16, 0);
}
/// PropVariantClear: frees what the value owns (CoTaskMem strings, BSTRs, blobs, CLSIDs and
/// interfaces) and empties it (a PROPVARIANT is 8 bytes of header and two pointers' worth)
fn PropVariantClear(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if v == 0 {
        return 0;
    }
    let p = mem::ps();
    let t = r16(v);
    match t {
        // VT_LPSTR, VT_LPWSTR, VT_CLSID, VT_CF
        30 | 31 | 72 | 71 => {
            let q = mem::rp(v + 8);
            if q != 0 {
                mem::heap_free(q);
            }
        }
        8 => bstr_free(mem::rp(v + 8)),
        // VT_BLOB, VT_BLOB_OBJECT: { ULONG size, BYTE* data }
        65 | 70 => {
            let q = mem::rp(v + 8 + p);
            if q != 0 {
                mem::heap_free(q);
            }
        }
        // VT_UNKNOWN, VT_DISPATCH, VT_STREAM, VT_STORAGE...: Release
        9 | 13 | 66..=69 => {
            let o = mem::rp(v + 8);
            if o != 0 {
                let release = mem::rp(mem::rp(o) + 2 * p);
                c.call(release, &[o]);
            }
        }
        // VT_VECTOR of numbers, dates, GUIDs (MF's blobs are VT_VECTOR | VT_UI1): { ULONG n,
        // elements* } (their elements need nothing freed)
        0x1002..=0x1007 | 0x100b | 0x1010..=0x1015 | 0x1040 | 0x1048 => {
            let q = mem::rp(v + 8 + p);
            if q != 0 {
                mem::heap_free(q);
            }
        }
        _ if t > 0x0fff && t & 0x4000 == 0 => {
            // (vectors and arrays: not made here)
            crate::com::warn_once(&format!("PropVariantClear: vt {t:#x} left as it is"));
        }
        _ => {}
    }
    mem::fill(v, 8 + 2 * p, 0);
    0
}
/// VariantCopy: `dst` cleared, then `src`'s value in it, with a reference of its own to an
/// interface (and a copy of a BSTR)
fn variant_copy(c: &mut Ctx, dst: u32, src: u32) -> u32 {
    if dst == src {
        return 0;
    }
    variant_clear(c, dst);
    let r = variant_copy_raw(dst, src);
    if r == 0 && matches!(r16(dst), 9 | 13) {
        let o = mem::rp(dst + 8);
        if o != 0 {
            let add = mem::rp(mem::rp(o) + mem::ps());
            c.call(add, &[o]);
        }
    }
    r
}
fn variant_copy_raw(dst: u32, src: u32) -> u32 {
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
    let (d, s) = (c.a(0), c.a(1));
    variant_copy(c, d, s)
}
fn VariantCopyInd(c: &mut Ctx) -> u32 {
    let (d, s) = (c.a(0), c.a(1));
    variant_copy(c, d, s)
}
/// What a VARIANT holds, for VariantChangeType.
enum Val {
    Empty,
    Int(i64),
    F(f64),
    Str(String),
}
fn variant_val(v: u32) -> Option<Val> {
    let ty = r16(v);
    let (ty, p) = if ty & 0x4000 != 0 { (ty & !0x4000, r32(v + 8)) } else { (ty, v + 8) };
    Some(match ty {
        0 | 1 => Val::Empty,
        2 => Val::Int(r16(p) as i16 as i64),
        3 | 10 | 22 => Val::Int(r32(p) as i32 as i64),
        4 => Val::F(f32::from_bits(r32(p)) as f64),
        5 | 7 => Val::F(f64::from_bits(mem::r64(p))),
        6 => Val::F(mem::r64(p) as i64 as f64 / 10000.0),
        8 => {
            let b = r32(p);
            let n = if b == 0 { 0 } else { r32(b - 4) / 2 };
            Val::Str(String::from_utf16_lossy(&(0..n).map(|i| r16(b + 2 * i)).collect::<Vec<_>>()))
        }
        11 => Val::Int(if r16(p) != 0 { -1 } else { 0 }),
        16 => Val::Int(r8(p) as i8 as i64),
        17 => Val::Int(r8(p) as i64),
        18 => Val::Int(r16(p) as i64),
        19 | 23 => Val::Int(r32(p) as i64),
        20 | 21 => Val::Int(mem::r64(p) as i64),
        _ => return None,
    })
}
/// A number in a string as VarParseNumFromStr reads it: spaces around, full-width digits,
/// thousands separators, "&H"/"&O" prefixes; no "0x"
fn parse_num(s: &str) -> Option<f64> {
    let t: String = s
        .chars()
        .map(|c| match c {
            '\u{ff10}'..='\u{ff19}' => char::from(b'0' + (c as u32 - 0xff10) as u8),
            '\u{ff0b}' => '+',
            '\u{ff0d}' => '-',
            '\u{ff0e}' => '.',
            '\u{ff0c}' => ',',
            c => c,
        })
        .collect();
    let t = t.trim();
    if let Some(h) = t.strip_prefix("&H").or_else(|| t.strip_prefix("&h")) {
        return i64::from_str_radix(h, 16).ok().map(|x| x as f64);
    }
    if let Some(o) = t.strip_prefix("&O").or_else(|| t.strip_prefix("&o")) {
        return i64::from_str_radix(o, 8).ok().map(|x| x as f64);
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let n = body.find(['.', 'e', 'E']).unwrap_or(body.len());
    let (int_part, rest) = body.split_at(n);
    // (separators only between digits of the integer part)
    if rest.contains(',') || int_part.starts_with(',') || int_part.ends_with(',') || int_part.contains(",,") {
        return None;
    }
    let plain: String = t.chars().filter(|&c| c != ',').collect();
    plain.parse::<f64>().ok()
}
/// A date in a string ("2024/03/05", "2024-3-5 12:30:00") as a DATE (days since 1899-12-30)
fn parse_date(s: &str) -> Option<f64> {
    let s = s.trim();
    let (d, t) = s.split_once(' ').map_or((s, ""), |(a, b)| (a, b.trim()));
    let f: Vec<i64> = d.split(['/', '-', '.']).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let [y, m, dd] = f[..] else { return None };
    if !(1..=12).contains(&m) || !(1..=31).contains(&dd) || !(100..=9999).contains(&y) {
        return None;
    }
    let mut v = (crate::k32::days_from_civil(y, m, dd) + 25569) as f64;
    if !t.is_empty() {
        let h: Vec<u32> = t.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
        let (hh, mm, ss) = (h[0], *h.get(1).unwrap_or(&0), *h.get(2).unwrap_or(&0));
        if h.len() > 3 || hh > 23 || mm > 59 || ss > 59 {
            return None;
        }
        v += (hh * 3600 + mm * 60 + ss) as f64 / 86400.0;
    }
    Some(v)
}
/// A double as VarBstrFromR8 writes it: 15 significant digits (7 for a float), trailing zeros
/// dropped, an exponent ("1E+20") past them
fn real_str(f: f64, digits: usize) -> String {
    // (not numbers: msvcrt's spellings, which oleaut32's "%.15G" gives; "inf" / "NaN" have no
    // 'e' for the split below, which panicked)
    if f.is_infinite() {
        return if f < 0.0 { "-1.#INF".into() } else { "1.#INF".into() };
    }
    if f.is_nan() {
        return if f.is_sign_negative() { "-1.#IND".into() } else { "1.#QNAN".into() };
    }
    let digits = digits.max(1);
    if f == 0.0 {
        return if f.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let e = format!("{:.*e}", digits - 1, f);
    let (m, x) = e.split_once('e').unwrap();
    let x: i32 = x.parse().unwrap();
    if x < -(digits as i32) || x >= digits as i32 {
        let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
        return format!("{m}E{}{:02}", if x < 0 { '-' } else { '+' }, x.abs());
    }
    let s = format!("{:.*}", (digits as i32 - 1 - x).max(0) as usize, f);
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}
/// VariantChangeType(dst, src, flags, vt): the numeric, boolean, date and string conversions.
fn variant_change(c: &mut Ctx, dst: u32, src: u32, flags: u32, vt: u16) -> u32 {
    let sty = r16(src) & !0x4000;
    if sty == vt || vt == 0x0c {
        return variant_copy(c, dst, src);
    }
    let Some(val) = variant_val(src) else { return 0x8002_0005 };
    let num = |v: &Val| -> Option<f64> {
        match v {
            Val::Empty => Some(0.0),
            Val::Int(i) => Some(*i as f64),
            Val::F(f) => Some(*f),
            // strings: dates only as dates, "True"/"False" only as booleans
            Val::Str(s) if vt == 7 => parse_date(s),
            Val::Str(s) => parse_num(s).or_else(|| {
                (vt == 11).then(|| match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "#true#" => Some(-1.0),
                    "false" | "#false#" => Some(0.0),
                    _ => None,
                })?
            }),
        }
    };
    let mut out = [0u8; 16];
    out[0..2].copy_from_slice(&vt.to_le_bytes());
    match vt {
        0 | 1 => {}
        8 => {
            let s = match &val {
                Val::Empty => String::new(),
                // (VARIANT_ALPHABOOL, VARIANT_LOCALBOOL: words; else -1 / 0)
                Val::Int(i) if sty == 11 && flags & 0x12 != 0 => if *i != 0 { "True".into() } else { "False".into() },
                Val::Int(i) => i.to_string(),
                Val::F(f) if sty == 4 => real_str(*f, 7),
                Val::F(f) => real_str(*f, 15),
                Val::Str(s) => s.clone(),
            };
            let w: Vec<u16> = s.encode_utf16().collect();
            out[8..12].copy_from_slice(&bstr_alloc(&w).to_le_bytes());
        }
        _ => {
            let Some(f) = num(&val) else { return 0x8002_0005 };
            if f.is_infinite() || vt == 4 && f.abs() > f32::MAX as f64 || vt == 7 && !(-657434.0..2958466.0).contains(&f) {
                return 0x8002_000a; // DISP_E_OVERFLOW
            }
            // (a boolean to a byte is its low byte: True is 255)
            let f = if sty == 11 && vt == 17 { (f as i64 & 0xff) as f64 } else { f };
            let r = |f: f64| f.round_ties_even();
            let i = r(f) as i64;
            let range = |lo: i64, hi: i64| (lo..=hi).contains(&i);
            let ok = match vt {
                2 => range(i16::MIN as i64, i16::MAX as i64),
                3 | 22 => range(i32::MIN as i64, i32::MAX as i64),
                16 => range(-128, 127),
                17 => range(0, 255),
                18 => range(0, 65535),
                19 | 23 => range(0, u32::MAX as i64),
                _ => true,
            };
            if !ok {
                return 0x8002_000a; // DISP_E_OVERFLOW
            }
            match vt {
                2 | 18 => out[8..10].copy_from_slice(&(i as u16).to_le_bytes()),
                3 | 19 | 22 | 23 => out[8..12].copy_from_slice(&(i as u32).to_le_bytes()),
                16 | 17 => out[8] = i as u8,
                20 | 21 => out[8..16].copy_from_slice(&(i as u64).to_le_bytes()),
                4 => out[8..12].copy_from_slice(&(f as f32).to_bits().to_le_bytes()),
                5 | 7 => out[8..16].copy_from_slice(&f.to_bits().to_le_bytes()),
                6 => out[8..16].copy_from_slice(&((f * 10000.0).round() as i64).to_le_bytes()),
                11 => out[8..10].copy_from_slice(&(if f != 0.0 { 0xffffu16 } else { 0 }).to_le_bytes()),
                _ => return 0x8002_0005,
            }
        }
    }
    // the old dst (which may be src) goes away only once the value was read
    if r16(dst) == 8 {
        bstr_free(r32(dst + 8));
    }
    mem::write(dst, &out);
    0
}
fn VariantChangeTypeEx(c: &mut Ctx) -> u32 {
    let (d, s, f, t) = (c.a(0), c.a(1), c.a(3), c.a(4) as u16);
    variant_change(c, d, s, f, t)
}
fn VariantChangeType(c: &mut Ctx) -> u32 {
    let (d, s, f, t) = (c.a(0), c.a(1), c.a(2), c.a(3) as u16);
    variant_change(c, d, s, f, t)
}
fn GetErrorInfo(c: &mut Ctx) -> u32 {
    crate::mem::putp(c.a(1), 0);
    1
}
fn SetErrorInfo(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// shell32

pub(crate) fn special_folder(csidl: u32) -> Option<&'static str> {
    Some(match csidl & 0xff {
        0 | 0x10 => "C:\\users\\user\\Desktop",
        0x02 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs",
        5 => "C:\\users\\user\\Documents",
        0x06 | 0x1f => "C:\\users\\user\\Favorites",
        0x07 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
        0x08 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Recent",
        0x09 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\SendTo",
        0x0b => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu",
        0x0d => "C:\\users\\user\\Music",
        0x0e => "C:\\users\\user\\Videos",
        0x13 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Network Shortcuts",
        0x14 => "C:\\windows\\Fonts",
        0x15 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Templates",
        0x16 => "C:\\ProgramData\\Microsoft\\Windows\\Start Menu",
        0x17 => "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs",
        0x18 => "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
        0x19 => "C:\\users\\Public\\Desktop",
        0x1a => "C:\\users\\user\\AppData\\Roaming",
        0x1b => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Printer Shortcuts",
        0x1c => "C:\\users\\user\\AppData\\Local",
        0x20 => "C:\\users\\user\\AppData\\Local\\Microsoft\\Windows\\Temporary Internet Files",
        0x21 => "C:\\users\\user\\AppData\\Local\\Microsoft\\Windows\\INetCookies",
        0x22 => "C:\\users\\user\\AppData\\Local\\Microsoft\\Windows\\History",
        0x23 => "C:\\ProgramData",
        0x24 => "C:\\windows",
        // (64-bit Windows: SYSTEMX86 is SysWOW64, PROGRAM_FILES(X86) and their COMMON ones the
        // 32-bit folders, PROGRAM_FILES the process's own kind's: k32::wow64_env)
        0x25 => "C:\\windows\\system32",
        0x29 => crate::k32::SYSWOW64,
        0x26 if mem::x64() => "C:\\Program Files",
        0x26 | 0x2a => "C:\\Program Files (x86)",
        0x27 => "C:\\users\\user\\Pictures",
        0x28 => "C:\\users\\user",
        0x2b if mem::x64() => "C:\\Program Files\\Common Files",
        0x2b | 0x2c => "C:\\Program Files (x86)\\Common Files",
        0x2d => "C:\\ProgramData\\Microsoft\\Windows\\Templates",
        0x2e => "C:\\users\\Public\\Documents",
        0x2f => "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Administrative Tools",
        0x30 => "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Administrative Tools",
        0x35 => "C:\\users\\Public\\Music",
        0x36 => "C:\\users\\Public\\Pictures",
        0x37 => "C:\\users\\Public\\Videos",
        0x38 => "C:\\windows\\resources",
        0x3b => "C:\\users\\user\\AppData\\Local\\Microsoft\\Windows\\Burn\\Burn",
        _ => return None,
    })
}

fn folder_path(csidl: u32, create: bool) -> Option<String> {
    let p = special_folder(csidl)?;
    // (made whether asked or not, as the folders of a Windows install are there; one look when
    // it is there already, not a resolve of each of its parents per call: engines ask for it on
    // every save path they build)
    let _ = create;
    if !crate::files::resolve(p).is_some_and(|h| crate::files::is_dir(&h)) {
        crate::files::make_dirs(p);
    }
    Some(p.to_string())
}

/// SHCreateDirectoryEx: the folder and any missing parents (ERROR_ALREADY_EXISTS if there).
fn sh_mkdir(path: &str) -> u32 {
    if crate::files::resolve(path).is_some() {
        return 183;
    }
    // (each missing parent made in the spelling of the existing ones, trailing separators or not)
    let h = crate::files::make_dirs(path);
    if crate::files::is_dir(&h) { 0 } else { 3 }
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
/// SHStrDupA/W(src, &copy): a wide copy in CoTaskMemAlloc memory
fn sh_str_dup(c: &mut Ctx, wide: bool) -> u32 {
    let (src, out) = (c.a(0), c.a(1));
    if out == 0 {
        return 0x8007_0057;
    }
    if src == 0 {
        mem::putp(out, 0);
        return 0x8007_0057;
    }
    let w: Vec<u16> = if wide { mem::wstr(src) } else { crate::text::a2w(&mem::cstr(src)) };
    let p = mem::heap_alloc(2 * w.len() as u32 + 2, true);
    for (i, v) in w.iter().enumerate() {
        mem::w16(p + 2 * i as u32, *v);
    }
    mem::putp(out, p);
    0
}
fn SHStrDupA(c: &mut Ctx) -> u32 {
    sh_str_dup(c, false)
}
fn SHStrDupW(c: &mut Ctx) -> u32 {
    sh_str_dup(c, true)
}
/// SHGetPropertyStoreForWindow(hwnd, riid, &store): no property stores (taskbar app ids)
fn SHGetPropertyStoreForWindow(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), 0);
    0x8000_4001
}
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
        // ProgramFilesX86, ProgramFilesCommonX86, SystemX86; ProgramFilesX64 and
        // ProgramFilesCommonX64 only for a 64-bit process
        0x7c5a_40ef => folder_path(0x2a, true),
        0xde97_4d24 => folder_path(0x2c, true),
        0xd652_31b0 => folder_path(0x29, true),
        0x6d80_9377 if mem::x64() => folder_path(0x26, true),
        0x6365_d5a7 if mem::x64() => folder_path(0x2b, true),
        0x33e2_8130 => folder_path(0x27, true),
        // Programs, CommonPrograms, StartMenu, CommonStartMenu, Startup, CommonStartup,
        // PublicDesktop, ProgramFilesCommon, Favorites, Music, Videos, SendTo, Recent, Templates,
        // PublicDocuments
        0xa77f_5d77 => folder_path(0x02, true),
        0x0139_d44e => folder_path(0x17, true),
        0x625b_53c3 => folder_path(0x0b, true),
        0xa411_5719 => folder_path(0x16, true),
        0xb97d_20bb => folder_path(0x07, true),
        0x82a5_ea35 => folder_path(0x18, true),
        0xc4aa_340d => folder_path(0x19, true),
        0xf7f1_ed05 => folder_path(0x2b, true),
        0x1777_f761 => folder_path(0x06, true),
        0x4bd8_d571 => folder_path(0x0d, true),
        0x1898_9b1d => folder_path(0x0e, true),
        0x8983_036c => folder_path(0x09, true),
        0xae50_c081 => folder_path(0x08, true),
        0xa632_93e8 => folder_path(0x15, true),
        0xed48_24af => folder_path(0x2e, true),
        // UserProgramFiles (a per-user install's Program Files)
        0x5cd7_aee2 => folder_path(0x1c, true).map(|p| {
            let p = format!("{p}\\Programs");
            crate::files::make_dirs(&p);
            p
        }),
        // Saved Games
        0x4c5c_32ff => folder_path(0x28, true).map(|p| {
            let p = format!("{p}\\Saved Games");
            crate::files::make_dirs(&p);
            p
        }),
        _ => None,
    };
    let Some(p) = path else {
        mem::putp(out, 0);
        return 0x8007_0002;
    };
    let w: Vec<u16> = p.encode_utf16().chain([0]).collect();
    let b = mem::heap_alloc(2 * w.len() as u32, false);
    for (i, &u) in w.iter().enumerate() {
        w16(b + 2 * i as u32, u);
    }
    mem::putp(out, b);
    0
}
// SHFileOperation: what installers do with it (NSIS's CopyFiles and RMDir on wildcards,
// electron-builder's installer copying the unpacked app into $INSTDIR), without UI.

/// A double-NUL-ended list of paths, full.
fn path_list(p: u32, wide: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = p;
    if p == 0 {
        return out;
    }
    loop {
        let s = if wide { String::from_utf16_lossy(&mem::wstr(at)) } else { crate::text::a2s(&mem::cstr(at)) };
        if s.is_empty() {
            break;
        }
        at += if wide { 2 * (s.encode_utf16().count() as u32 + 1) } else { crate::text::s2a(&s).len() as u32 + 1 };
        out.push(crate::files::full_path(&s));
    }
    out
}
/// '*' and '?' without regard to case ("*.*" any name, as Windows takes it)
pub(crate) fn wild_match(pat: &str, name: &str) -> bool {
    if pat == "*.*" {
        return true;
    }
    let (p, n): (Vec<char>, Vec<char>) = (pat.to_lowercase().chars().collect(), name.to_lowercase().chars().collect());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(st) = star {
            i = st + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}
/// The existing paths a source names: itself, or what its wildcard last part matches in its
/// folder (only files with FOF_FILESONLY); (path, is a folder)
fn sh_expand(src: &str, files_only: bool) -> Vec<(String, bool)> {
    let (dir, last) = src.rsplit_once('\\').unwrap_or(("", src));
    if !last.contains(['*', '?']) {
        return crate::files::resolve(src).and_then(|h| kura::fs::metadata(&h).ok()).map(|m| vec![(src.to_string(), m.is_dir())]).unwrap_or_default();
    }
    let Some(h) = crate::files::resolve(dir) else { return Vec::new() };
    let mut out: Vec<(String, bool)> = kura::fs::read_dir(&h).into_iter().flatten().flatten().filter_map(|e| {
        let name = e.file_name().into_string().ok()?;
        let is_dir = e.metadata().ok()?.is_dir();
        (wild_match(last, &name) && !(files_only && is_dir)).then(|| (format!("{dir}\\{name}"), is_dir))
    }).collect();
    out.sort();
    out
}
fn sh_copy(src: &str, is_dir: bool, to: &str, recurse: bool) -> bool {
    let Some(h) = crate::files::resolve(src) else { return false };
    if !is_dir {
        return kura::fs::copy(&h, crate::files::resolve_new(to)).is_ok();
    }
    crate::files::make_dirs(to);
    let mut ok = true;
    for e in kura::fs::read_dir(&h).into_iter().flatten().flatten() {
        let (Ok(name), Ok(m)) = (e.file_name().into_string(), e.metadata()) else { continue };
        if m.is_dir() && !recurse {
            continue;
        }
        ok &= sh_copy(&format!("{src}\\{name}"), m.is_dir(), &format!("{to}\\{name}"), recurse);
    }
    ok
}
fn sh_delete(src: &str, is_dir: bool) -> bool {
    let Some(h) = crate::files::resolve(src) else { return false };
    if !is_dir {
        return kura::fs::remove_file(&h).is_ok();
    }
    for e in kura::fs::read_dir(&h).into_iter().flatten().flatten() {
        let (Ok(name), Ok(m)) = (e.file_name().into_string(), e.metadata()) else { continue };
        sh_delete(&format!("{src}\\{name}"), m.is_dir());
    }
    kura::fs::remove_dir(&h).is_ok()
}
/// SHFileOperation(&SHFILEOPSTRUCT): FO_MOVE 1, FO_COPY 2, FO_DELETE 3, FO_RENAME 4 of the
/// double-NUL lists pFrom / pTo (FOF_MULTIDESTFILES 1: a destination per source; FOF_FILESONLY
/// 0x80 for wildcards; FOF_NORECURSION 0x1000). A copy into an existing folder goes under it; a
/// single source to a missing name takes that name; several (or a wildcard) to a missing folder
/// make it. 0 when done, else ERROR_FILE_NOT_FOUND (a source missing) or the DE_ code 0x7c
/// (DE_INVALIDFILES) for a list Windows rejects. The struct is packed to 1 byte in a 32-bit
/// process (shellapi.h).
fn sh_file_operation(c: &mut Ctx, wide: bool) -> u32 {
    let op = c.a(0);
    let x64 = mem::x64();
    let (func_at, from_at, to_at, flags_at, aborted_at) = if x64 { (8, 16, 24, 32, 36) } else { (4, 8, 12, 16, 18) };
    let func = r32(op + func_at);
    let from = path_list(mem::rp(op + from_at), wide);
    let to = path_list(mem::rp(op + to_at), wide);
    let flags = mem::r16(op + flags_at) as u32;
    put32(op + aborted_at, 0);
    if crate::env_on!("SAKE_TRACE_FILES") {
        eprintln!("sake: SHFileOperation {func} {from:?} -> {to:?} flags {flags:#x}");
    }
    if from.is_empty() || !(1..=4).contains(&func) {
        return 0x7c;
    }
    let (files_only, recurse, multi) = (flags & 0x80 != 0, flags & 0x1000 == 0, flags & 1 != 0);
    let mut missing = false;
    for (i, src) in from.iter().enumerate() {
        let wild = src.rsplit('\\').next().is_some_and(|l| l.contains(['*', '?']));
        let found = sh_expand(src, files_only);
        if found.is_empty() {
            missing |= !wild;
            continue;
        }
        if func == 3 {
            for (p, d) in &found {
                sh_delete(p, *d);
            }
            continue;
        }
        let Some(dest) = (if multi { to.get(i) } else { to.first() }).cloned() else { return 0x7c };
        if func == 4 {
            let (Some(h), new) = (crate::files::resolve(src), crate::files::resolve_new(&dest)) else { missing = true; continue };
            if kura::fs::rename(&h, &new).is_err() {
                return 5;
            }
            continue;
        }
        let dest_dir = crate::files::resolve(&dest).and_then(|h| kura::fs::metadata(&h).ok()).is_some_and(|m| m.is_dir());
        let into = dest_dir || wild || (!multi && from.len() > 1);
        for (p, d) in &found {
            let target = if into {
                crate::files::make_dirs(&dest);
                format!("{}\\{}", dest.trim_end_matches('\\'), p.rsplit('\\').next().unwrap_or(p))
            } else {
                dest.clone()
            };
            let moved = func == 1 && crate::files::resolve(p).is_some_and(|h| crate::files::resolve(&target).is_none() && kura::fs::rename(&h, crate::files::resolve_new(&target)).is_ok());
            if !moved {
                if !sh_copy(p, *d, &target, recurse) {
                    return 5;
                }
                if func == 1 {
                    sh_delete(p, *d);
                }
            }
        }
    }
    if missing { 2 } else { 0 }
}
fn SHFileOperationA(c: &mut Ctx) -> u32 {
    sh_file_operation(c, false)
}
fn SHFileOperationW(c: &mut Ctx) -> u32 {
    sh_file_operation(c, true)
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
/// The system image list SHGetFileInfo(SHGFI_SYSICONINDEX) answers with.
static SYS_IMAGELIST: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// SHGetFileInfoA/W(path, attrs, &info, size, flags): a file's or folder's display name,
/// type name, attributes, icon and system image list index (SHGFI_USEFILEATTRIBUTES: from
/// `attrs`, the file need not exist); SHGFI_EXETYPE: a PE's kind; SHGFI_PIDL is not taken.
/// Extensions of the file types a fresh Windows registers (HKCR\.ext): Explorer's default
/// "hide extensions for known file types" leaves them out of display names
const KNOWN_TYPES: &[&str] = &[
    "exe", "dll", "com", "bat", "cmd", "sys", "drv", "cpl", "scr", "ocx", "msi", "inf", "ini", "txt", "log", "xml", "htm", "html", "rtf", "hlp", "chm", "bmp", "dib", "jpg", "jpeg",
    "png", "gif", "ico", "cur", "tif", "tiff", "wav", "mp3", "mid", "midi", "wma", "avi", "mpg", "mpeg", "wmv", "mp4", "zip", "cab", "ttf", "otf", "fon", "lnk", "url",
];

/// SHGetFileInfo's display name of a file or folder: the name, less the extension of a known
/// type (Windows: "shfileinfo.txt" -> "shfileinfo", "none.bmp" -> "none"; a folder keeps its
/// dots)
fn display_name(base: &str, dir: bool) -> String {
    match base.rsplit_once('.') {
        Some((stem, ext)) if !dir && !stem.is_empty() && KNOWN_TYPES.contains(&ext.to_ascii_lowercase().as_str()) => stem.to_string(),
        _ => base.to_string(),
    }
}

fn sh_get_file_info(c: &mut Ctx, wide: bool) -> u32 {
    let (path, attrs, info, size, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    const ICON: u32 = 0x100;
    const DISPLAYNAME: u32 = 0x200;
    const TYPENAME: u32 = 0x400;
    const ATTRIBUTES: u32 = 0x800;
    const EXETYPE: u32 = 0x2000;
    const SYSICONINDEX: u32 = 0x4000;
    const USEFILEATTRIBUTES: u32 = 0x10;
    if path == 0 || flags & 8 != 0 {
        return 0;
    }
    let name = arg_str(path, wide);
    let ps = mem::ps();
    let need = if wide { ps + 8 + 340 * 2 } else { ps + 8 + 340 };
    if info != 0 && size < need {
        return 0;
    }
    if flags & EXETYPE != 0 {
        // 'PE' and the subsystem's Windows version for a GUI program, 'PE' alone for a console one
        let Some(host) = crate::files::resolve(&name) else { return 0 };
        let Ok(b) = kura::fs::read(&host) else { return 0 };
        let r32b = |o: usize| b.get(o..o + 4).map_or(0, |x| u32::from_le_bytes(x.try_into().unwrap()));
        let r16b = |o: usize| b.get(o..o + 2).map_or(0, |x| u16::from_le_bytes(x.try_into().unwrap()) as u32);
        if r16b(0) != 0x5a4d {
            return 0;
        }
        let pe = r32b(0x3c) as usize;
        if r32b(pe) != 0x4550 {
            return 0;
        }
        let opt = pe + 24;
        let (subsystem, major, minor) = (r16b(opt + 68), r16b(opt + 48), r16b(opt + 50));
        return if subsystem == 3 { 0x4550 } else { 0x4550 | (major << 8 | minor) << 16 };
    }
    let (exists, dir) = if flags & USEFILEATTRIBUTES != 0 {
        (true, attrs & 0x10 != 0)
    } else {
        match crate::files::resolve(&name).and_then(|h| kura::fs::metadata(&h).ok()) {
            Some(m) => (true, m.is_dir()),
            None => (false, false),
        }
    };
    if !exists {
        return 0;
    }
    let trimmed = name.trim_end_matches(['\\', '/']);
    let base = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed).to_string();
    let drive = trimmed.len() == 2 && trimmed.ends_with(':');
    let ext = if dir { String::new() } else { base.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default() };
    if info != 0 {
        mem::write(info, &vec![0u8; need as usize]);
        if flags & SYSICONINDEX != 0 || flags & ICON != 0 {
            // (indexes of the generic file, folder, drive and program icons)
            let i = if drive { 8 } else if dir { 3 } else if ext == "exe" { 2 } else { 0 };
            w32(info + ps, i);
        }
        if flags & ICON != 0 {
            let load = crate::export_once!("user32", "LoadIconW");
            let h = c.call(load, &[0, 32512]);
            mem::wp(info, h);
        }
        if flags & ATTRIBUTES != 0 {
            // SFGAO_FILESYSTEM | CANCOPY | CANMOVE | CANRENAME | CANDELETE | HASPROPSHEET, and
            // a folder's FOLDER | FILESYSANCESTOR | HASSUBFOLDER | STORAGE... (nothing for a
            // name taken with SHGFI_USEFILEATTRIBUTES: Windows answers no attributes then)
            let a = if flags & USEFILEATTRIBUTES != 0 {
                0
            } else if dir {
                0xf080_0177u32
            } else {
                0x4040_0177
            };
            w32(info + ps + 4, a);
        }
        let name_at = info + ps + 8;
        if flags & DISPLAYNAME != 0 {
            let shown = if drive { format!("Local Disk ({})", trimmed.to_ascii_uppercase()) } else { display_name(&base, dir) };
            ret_str(name_at, 260, &shown, wide);
        }
        if flags & TYPENAME != 0 {
            let t = if drive {
                "Local Disk".to_string()
            } else if dir {
                "File folder".to_string()
            } else {
                match ext.as_str() {
                    "" => "File".to_string(),
                    "exe" => "Application".to_string(),
                    "txt" => "Text Document".to_string(),
                    e => format!("{} File", e.to_ascii_uppercase()),
                }
            };
            ret_str(name_at + if wide { 520 } else { 260 }, 80, &t, wide);
        }
    }
    if flags & SYSICONINDEX != 0 {
        return *SYS_IMAGELIST.get_or_init(|| IMAGELISTS.fetch_add(4, Ordering::Relaxed));
    }
    1
}
/// SHAutoComplete(edit, flags): S_OK (no completion list is shown).
fn SHAutoComplete(_: &mut Ctx) -> u32 {
    0
}
fn SHGetFileInfoA(c: &mut Ctx) -> u32 {
    sh_get_file_info(c, false)
}
fn SHGetFileInfoW(c: &mut Ctx) -> u32 {
    sh_get_file_info(c, true)
}

/// IsOS(os): what Windows 10 Pro (a 64-bit workstation, not a domain member) answers; sake
/// is no WoW64 process.
/// DllGetVersion's answers (major, minor, build) as Windows 10 22H2's: shlwapi and shell32 6.0,
/// comctl32 5.82 (version 6 only for programs whose manifest asks for it). Programs check
/// them: 魔王戦記 (玉藻スタジオ) refuses to start without shlwapi 5 ("IE5以降が…").
pub const SHLWAPI_VERSION: (u32, u32, u32) = (6, 0, 19041);
pub const SHELL32_VERSION: (u32, u32, u32) = (6, 0, 19041);
pub const COMCTL32_VERSION: (u32, u32, u32) = (5, 82, 19041);

/// DllGetVersion(DLLVERSIONINFO *): cbSize 20 (DLLVERSIONINFO) or 32 (DLLVERSIONINFO2: dwFlags 0,
/// ullVersion major.minor.build.0 in 16-bit fields), else E_INVALIDARG; dwPlatformID
/// DLLVER_PLATFORM_NT. The dwords written (offset, value), all within cbSize.
pub fn dll_version_words(size: u32, (major, minor, build): (u32, u32, u32)) -> Result<Vec<(u32, u32)>, u32> {
    const E_INVALIDARG: u32 = 0x8007_0057;
    if size != 20 && size != 32 {
        return Err(E_INVALIDARG);
    }
    let mut w = vec![(4, major), (8, minor), (12, build), (16, 2)];
    if size == 32 {
        let ull = (major as u64 & 0xffff) << 48 | (minor as u64 & 0xffff) << 32 | (build as u64 & 0xffff) << 16;
        w.extend([(20, 0), (24, ull as u32), (28, (ull >> 32) as u32)]);
    }
    Ok(w)
}

pub fn dll_get_version(p: u32, v: (u32, u32, u32)) -> u32 {
    if p == 0 {
        return 0x8007_0057;
    }
    match dll_version_words(r32(p), v) {
        Ok(w) => {
            for (o, x) in w {
                w32(p + o, x);
            }
            0
        }
        Err(e) => e,
    }
}

fn IsOS(c: &mut Ctx) -> u32 {
    // OS_NT, NT4ORGREATER, WIN2000ORGREATER, WIN2000PRO, XPORGREATER, PROFESSIONAL,
    // PERSONALTERMINALSERVER, FASTUSERSWITCHING
    matches!(c.a(0), 1 | 3 | 7 | 8 | 18 | 20 | 25 | 26) as u32
}

fn SHGetSpecialFolderLocation(c: &mut Ctx) -> u32 {
    let (csidl, out) = (c.a(1), c.a(2));
    // an ID list of our own: a marker and the CSIDL, then the terminator
    let p = mem::heap_alloc(12, true);
    w16(p, 8);
    w16(p + 2, 0x7676);
    w32(p + 4, csidl);
    mem::putp(out, p);
    0
}
fn SHGetFolderLocation(c: &mut Ctx) -> u32 {
    let (csidl, out) = (c.a(1), c.a(4));
    let p = mem::heap_alloc(12, true);
    w16(p, 8);
    w16(p + 2, 0x7676);
    w32(p + 4, csidl);
    mem::putp(out, p);
    0
}
fn pidl_path(p: u32) -> Option<String> {
    if p == 0 {
        return None;
    }
    match r16(p + 2) {
        0x7676 => folder_path(r32(p + 4), false),
        PATH_PIDL => Some(String::from_utf16_lossy(&wstr(p + 4))),
        _ => None,
    }
}
/// The marker of an ID list sake makes for any folder (SHBrowseForFolder's): the path follows
/// in UTF-16.
const PATH_PIDL: u16 = 0x7677;
fn path_pidl(path: &str) -> u32 {
    let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let n = 4 + 2 * w.len() as u32;
    // (and the terminator: an item of 0 bytes)
    let p = mem::heap_alloc(n + 2, true);
    w16(p, n as u16);
    w16(p + 2, PATH_PIDL);
    for (i, &u) in w.iter().enumerate() {
        w16(p + 4 + 2 * i as u32, u);
    }
    p
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

/// SHBrowseForFolder's dialog: the program's callback (BROWSEINFO.lpfn) and the folder
/// selected in it.
struct Browse {
    lpfn: u32,
    lparam: u32,
    sel: String,
}
thread_local!(static BROWSE: std::cell::RefCell<Vec<Browse>> = const { std::cell::RefCell::new(Vec::new()) });
fn browse<R>(f: impl FnOnce(&mut Browse) -> R) -> Option<R> {
    BROWSE.with(|b| b.borrow_mut().last_mut().map(f))
}
/// (posted to the dialog by itself: the player presses OK)
const BROWSE_OK: u32 = 0xbff0;
/// BFFM_SELCHANGED to the callback: the selection's ID list, freed after.
fn browse_selchanged(c: &mut Ctx, h: u32) {
    let Some((f, l, sel)) = browse(|b| (b.lpfn, b.lparam, b.sel.clone())) else { return };
    if f != 0 {
        let p = path_pidl(&sel);
        c.call(f, &[h, 2, p, l]);
        mem::heap_free(p);
    }
}
fn browse_dlgproc(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let end = |c: &mut Ctx, r: u32| {
        let f = crate::export_once!("user32", "EndDialog");
        c.call(f, &[h, r]);
        1
    };
    match m {
        // WM_INITDIALOG: BFFM_INITIALIZED, then BFFM_SELCHANGED for the first selection (as
        // Windows' dialog); OK comes after whatever the callback posted meanwhile
        0x110 => {
            let (f, l) = browse(|b| (b.lpfn, b.lparam)).unwrap_or((0, 0));
            if f != 0 {
                c.call(f, &[h, 1, 0, l]);
                browse_selchanged(c, h);
            }
            crate::user::post(h, BROWSE_OK, 0, 0);
            1
        }
        // BFFM_SETSELECTIONA / W: a path (wParam TRUE) or an ID list; a folder that is not
        // there leaves the selection as it was
        0x466 | 0x467 => {
            let s = if wp != 0 { (lp != 0).then(|| arg_str(lp, m == 0x467)) } else { pidl_path(lp) };
            let s = s.map(|s| crate::files::full_path(&s)).filter(|s| crate::files::resolve(s).is_some_and(|h| kura::fs::metadata(h).is_ok_and(|m| m.is_dir())));
            if let Some(s) = s {
                browse(|b| b.sel = s);
                browse_selchanged(c, h);
            }
            1
        }
        // BFFM_SETSTATUSTEXT, BFFM_ENABLEOK, BFFM_SETOKTEXT, BFFM_SETEXPANDED: nothing to show
        0x464 | 0x465 | 0x468..=0x46a => 1,
        // IDOK, IDCANCEL (the callback may press them itself)
        0x111 if wp & 0xffff == 1 => end(c, 1),
        0x111 if wp & 0xffff == 2 => end(c, 0),
        BROWSE_OK => end(c, 1),
        _ => 0,
    }
}
/// SHBrowseForFolder(BROWSEINFO): the folder dialog, which nobody sees, the player taking the
/// folder selected when it opens: the root (BROWSEINFO.pidlRoot; none: the desktop), or the
/// one the callback selects (BFFM_SETSELECTION in BFFM_INITIALIZED, as programs do to offer a
/// default). The callback gets a real dialog window and its messages; IDCANCEL from it is a
/// cancel (NULL). Returns the folder's ID list (SHGetPathFromIDList reads it), its name in
/// pszDisplayName. (SHUFFLE! Essence+ asks where its saves go when SCRIPT\LL_APP.SYS is not
/// there and quits on NULL.)
fn sh_browse_for_folder(c: &mut Ctx, wide: bool) -> u32 {
    let bi = c.a(0);
    let ps = mem::ps();
    let (owner, root, name, lpfn, lparam) = (mem::rp(bi), mem::rp(bi + ps), mem::rp(bi + 2 * ps), mem::rp(bi + 5 * ps), mem::rp(bi + 6 * ps));
    let sel = pidl_path(root).or_else(|| folder_path(0, false)).unwrap_or_default();
    BROWSE.with(|b| b.borrow_mut().push(Browse { lpfn, lparam, sel }));
    static PROC: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let proc_ = *PROC.get_or_init(|| crate::api::host_fn("browse_dlgproc", browse_dlgproc, 16));
    let r = crate::user::hidden_dialog(c, owner, proc_, lparam);
    let b = BROWSE.with(|b| b.borrow_mut().pop()).unwrap();
    eprintln!("sake: SHBrowseForFolder: {}", if r == 1 { b.sel.as_str() } else { "(cancelled)" });
    if r != 1 {
        return 0;
    }
    if name != 0 {
        let leaf = b.sel.trim_end_matches('\\').rsplit('\\').next().unwrap_or("");
        ret_str(name, 260, leaf, wide);
    }
    path_pidl(&b.sel)
}
fn SHBrowseForFolderA(c: &mut Ctx) -> u32 {
    sh_browse_for_folder(c, false)
}
fn SHBrowseForFolderW(c: &mut Ctx) -> u32 {
    sh_browse_for_folder(c, true)
}
/// ShellExecute(verb, file, params, dir): "open" of one of the game's programs runs it
/// (k32::exec); anything else (pages, documents) is only logged.
fn shell_execute(verb: u32, file: u32, params: u32, dir: u32, wide: bool) -> u32 {
    let verb = if verb == 0 { "open".to_string() } else { arg_str(verb, wide) };
    let (f, p) = (arg_str(file, wide), if params == 0 { String::new() } else { arg_str(params, wide) });
    let d = (dir != 0).then(|| arg_str(dir, wide));
    if verb.eq_ignore_ascii_case("open") {
        crate::k32::exec(&f, &p, d.as_deref());
    }
    eprintln!("sake: ShellExecute {verb} {f} {p}");
    42
}
fn ShellExecuteA(c: &mut Ctx) -> u32 {
    shell_execute(c.a(1), c.a(2), c.a(3), c.a(4), false)
}
fn ShellExecuteW(c: &mut Ctx) -> u32 {
    shell_execute(c.a(1), c.a(2), c.a(3), c.a(4), true)
}
/// SHELLEXECUTEINFO: verb +12, file +16, parameters +20, directory +24, hInstApp +32
fn shell_execute_ex(c: &mut Ctx, wide: bool) -> u32 {
    let i = c.a(0);
    let r = shell_execute(r32(i + 12), r32(i + 16), r32(i + 20), r32(i + 24), wide);
    w32(i + 32, r);
    1
}
fn ShellExecuteExA(c: &mut Ctx) -> u32 {
    shell_execute_ex(c, false)
}
fn ShellExecuteExW(c: &mut Ctx) -> u32 {
    shell_execute_ex(c, true)
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
    // (an empty line: the program's own path)
    let args = if s.is_empty() { vec![crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.clone()).unwrap_or_default()] } else { crate::process::argv(&s) };
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
    let read = mem::rp(mem::rp(stream) + 3 * mem::ps());
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
    let Some(mut rest) = bitmap_rest(w, h, bpp) else { return false };
    // (read past in pieces: not one block of what the header claims)
    let tmp = mem::heap_alloc(0x10000, false);
    let mut ok = true;
    while ok && rest > 0 {
        let n = rest.min(0x10000);
        ok = stream_read(c, stream, tmp, n);
        rest -= n;
    }
    mem::heap_free(tmp);
    ok
}
/// The palette and pixel bytes after an image list bitmap's headers (w x h at bpp), or None
/// for one past 256 MB (the u32 products wrapped: a small read, the stream left mid-bitmap).
fn bitmap_rest(w: u32, h: u32, bpp: u32) -> Option<u32> {
    let pal = if bpp <= 8 { (1u64 << bpp) * 4 } else { 0 };
    let row = (w as u64 * bpp as u64).div_ceil(32) * 4;
    let n = (h as u64).checked_mul(row)?.checked_add(pal)?;
    (n <= 256 << 20).then_some(n as u32)
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
/// timeGetDevCaps(&caps, cb): TIMERR_NOCANDO for a TIMECAPS shorter than its 8 bytes
fn timeGetDevCaps(c: &mut Ctx) -> u32 {
    let (p, cb) = (c.a(0), c.a(1));
    if p == 0 || cb < 8 {
        return 97;
    }
    w32(p, 1);
    w32(p + 4, 1_000_000);
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
            kura::snap::sleep(next - now);
            if crate::snap::point() || std::time::Instant::now() < next {
                continue;
            }
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
fn midiOutGetNumDevs(_: &mut Ctx) -> u32 {
    0
}
fn auxGetNumDevs(_: &mut Ctx) -> u32 {
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
/// A path's characters (DBCS pairs as one in the A forms, each with its byte offset; the
/// offsets have one more entry, the end), shlwapi walking with CharNext.
struct PathChars {
    ch: Vec<u32>,
    off: Vec<u32>,
}
fn path_chars(p: u32, wide: bool) -> PathChars {
    let (mut ch, mut off) = (Vec::new(), Vec::new());
    if wide {
        for (i, u) in wstr(p).into_iter().enumerate() {
            ch.push(u as u32);
            off.push(2 * i as u32);
        }
        off.push(2 * ch.len() as u32);
    } else {
        let b = cstr(p);
        let mut i = 0;
        while i < b.len() {
            off.push(i as u32);
            if crate::text::is_lead(b[i]) && i + 1 < b.len() {
                ch.push((b[i] as u32) << 8 | b[i + 1] as u32);
                i += 2;
            } else {
                ch.push(b[i] as u32);
                i += 1;
            }
        }
        off.push(b.len() as u32);
    }
    PathChars { ch, off }
}
const BS: u32 = b'\\' as u32;
fn path_is_root(c: &[u32]) -> bool {
    match c {
        [] => false,
        [BS] => true,
        [BS, BS, rest @ ..] => rest.iter().filter(|&&x| x == BS).count() <= 1,
        [_, 0x3a, BS] => true,
        _ => false,
    }
}
/// PathFindFileName: past the last separator ('\\', '/', ':') that something other than a
/// separator follows ("C:\\dir\\" -> "dir\\", "C:\\" stays)
fn file_name_at(c: &[u32]) -> usize {
    let sep = |x: u32| x == BS || x == b'/' as u32;
    let mut last = 0;
    for i in 0..c.len() {
        if (sep(c[i]) || c[i] == b':' as u32) && c.get(i + 1).is_some_and(|&n| !sep(n)) {
            last = i + 1;
        }
    }
    last
}
/// PathRemoveFileSpec: where the cut goes (None: nothing to remove)
fn file_spec_at(c: &[u32]) -> Option<usize> {
    let mut i = 0;
    let mut spec = 0;
    for _ in 0..2 {
        if c.get(i) == Some(&BS) {
            i += 1;
            spec = i;
        }
    }
    while i < c.len() {
        if c[i] == BS {
            spec = i;
        } else if c[i] == b':' as u32 {
            i += 1;
            spec = i;
            if c.get(i) == Some(&BS) {
                spec += 1;
            }
            // (the character after the colon is stepped over)
        }
        i += 1;
    }
    (spec < c.len()).then_some(spec)
}
fn put_nul(p: u32, at: u32, wide: bool) {
    if wide {
        w16(p + at, 0);
    } else {
        crate::mem::w8(p + at, 0);
    }
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
/// UrlApplyScheme's answer: the URL with the scheme it gets, or None (S_FALSE: left alone).
/// URL_APPLY_DEFAULT 1 (http://), GUESSSCHEME 2 (by the host's first label, as the registry's
/// URL\Prefixes), GUESSFILE 4 (a drive path or a UNC name), FORCEAPPLY 8 (a URL that has a
/// scheme gets the default one in front all the same).
pub(crate) fn url_apply_scheme(url: &str, flags: u32) -> Option<String> {
    // (a scheme: a letter, then at least one more scheme character, then ':'; "u:" is a drive)
    let scheme = url.find(':').is_some_and(|n| n >= 2 && url[..n].chars().enumerate().all(|(i, ch)| ch.is_ascii_alphabetic() || (i > 0 && (ch.is_ascii_digit() || "+-.".contains(ch)))));
    if scheme {
        return (flags & 9 == 9).then(|| format!("http://{url}"));
    }
    if flags & 4 != 0 {
        let b = url.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return Some(format!("file:///{}", url.replace('\\', "/")));
        }
        if let Some(rest) = url.strip_prefix("\\\\") {
            return Some(format!("file://{}", rest.replace('\\', "/")));
        }
    }
    if flags & 2 != 0 {
        let first = url.split('.').next().unwrap_or("").to_ascii_lowercase();
        let guess = match first.as_str() {
            _ if !url.contains('.') => None,
            "www" | "home" | "mosaic" => Some("http"),
            "ftp" => Some("ftp"),
            "gopher" => Some("gopher"),
            _ => None,
        };
        if let Some(g) = guess {
            return Some(format!("{g}://{url}"));
        }
    }
    (flags & 1 != 0).then(|| format!("http://{url}"))
}
fn url_apply(c: &mut Ctx, wide: bool) -> u32 {
    let (url, out, len, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if url == 0 || out == 0 || len == 0 {
        return 0x8007_0057;
    }
    let Some(new) = url_apply_scheme(&arg_str(url, wide), flags) else { return 1 };
    let w: Vec<u16> = new.encode_utf16().collect();
    let a = w2a(&w);
    let n = if wide { w.len() } else { a.len() } as u32;
    if n + 1 > r32(len) {
        w32(len, n + 1);
        return 0x8000_4003;
    }
    if wide {
        mem::put_wstr(out, n + 1, &w);
    } else {
        mem::put_cstr(out, n + 1, &a);
    }
    w32(len, n);
    0
}
/// UrlGetPart's answer: Ok(Some(part)), Ok(None) for S_FALSE (an empty part, or no scheme for
/// URL_PART_SCHEME), Err(HRESULT). Parts: SCHEME 1, HOSTNAME 2, USERNAME 3, PASSWORD 4, PORT 5,
/// QUERY 6; URL_PARTFLAG_KEEPSCHEME 1 puts "scheme:" in front of 2..5 (not for file:).
pub(crate) fn url_part(url: &str, part: u32, flags: u32) -> Result<Option<String>, u32> {
    const E_FAIL: u32 = 0x8000_4005;
    const E_INVALIDARG: u32 = 0x8007_0057;
    let some = |s: &str| Ok((!s.is_empty()).then(|| s.to_string()));
    // (a scheme: a letter, at least one more of letters, digits, + - .; case folded)
    let scheme = url.find(':').filter(|&n| n >= 2 && url[..n].chars().enumerate().all(|(i, ch)| ch.is_ascii_alphabetic() || (i > 0 && (ch.is_ascii_digit() || "+-.".contains(ch))))).map(|n| url[..n].to_ascii_lowercase());
    let Some(scheme) = scheme else {
        return if part == 1 { Ok(None) } else { Err(E_FAIL) };
    };
    if part == 1 {
        return some(&scheme);
    }
    let rest = &url[scheme.len() + 1..];
    let file = scheme == "file";
    let hosted = matches!(scheme.as_str(), "file" | "ftp" | "gopher" | "http" | "https" | "news" | "nntp" | "snews" | "telnet");
    // (the authority after "//", where either slash may be a backslash, up to / \ ? #)
    let b = rest.as_bytes();
    let auth = (b.len() >= 2 && matches!(b[0], b'/' | b'\\') && matches!(b[1], b'/' | b'\\')).then(|| {
        let a = &rest[2..];
        &a[..a.find(['/', '\\', '?', '#']).unwrap_or(a.len())]
    });
    if part == 6 {
        let after = &rest[auth.map_or(0, |a| 2 + a.len())..];
        let q = after.find('?').map(|i| &after[i + 1..]).unwrap_or("");
        return some(&q[..q.find('#').unwrap_or(q.len())]);
    }
    if !hosted || !(2..=5).contains(&part) {
        return Err(E_FAIL);
    }
    let Some(auth) = auth else {
        return if file { Ok(None) } else { Err(E_FAIL) };
    };
    let keep = |s: &str| -> Result<Option<String>, u32> {
        if flags & 1 != 0 && !file { Ok(Some(format!("{scheme}:{s}"))) } else { some(s) }
    };
    if file {
        // (the whole authority is the host; a drive letter is none)
        if part != 2 {
            return Err(E_FAIL);
        }
        let drive = auth.len() == 2 && auth.as_bytes()[0].is_ascii_alphabetic() && auth.ends_with(':');
        return some(if drive { "" } else { auth });
    }
    let (user, hostport) = match auth.split_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, auth),
    };
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (hostport, None),
    };
    let v = match part {
        2 => host,
        3 => user.map(|u| u.split_once(':').map_or(u, |x| x.0)).ok_or(E_INVALIDARG)?,
        4 => user.and_then(|u| u.split_once(':')).map(|x| x.1).ok_or(E_INVALIDARG)?,
        _ => port.ok_or(E_INVALIDARG)?,
    };
    if v.is_empty() && !(part == 2 && flags & 1 != 0) {
        return Ok(None);
    }
    keep(v)
}
fn url_get_part(c: &mut Ctx, wide: bool) -> u32 {
    let (url, out, size, part, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if url == 0 || out == 0 || size == 0 || r32(size) == 0 {
        return 0x8007_0057;
    }
    let got = url_part(&arg_str(url, wide), part, flags);
    let v = match got {
        Err(e) => return e,
        Ok(None) => {
            if wide { w16(out, 0) } else { mem::w8(out, 0) }
            w32(size, 0);
            return if wide { 0 } else { 1 };
        }
        Ok(Some(v)) => v,
    };
    let w: Vec<u16> = v.encode_utf16().collect();
    let a = w2a(&w);
    let n = if wide { w.len() } else { a.len() } as u32;
    if n + 1 > r32(size) {
        w32(size, n + 1);
        return 0x8000_4003;
    }
    if wide { mem::put_wstr(out, n + 1, &w) } else { mem::put_cstr(out, n + 1, &a) };
    w32(size, n);
    0
}
fn UrlGetPartA(c: &mut Ctx) -> u32 {
    url_get_part(c, false)
}
fn UrlGetPartW(c: &mut Ctx) -> u32 {
    url_get_part(c, true)
}
fn UrlApplySchemeA(c: &mut Ctx) -> u32 {
    url_apply(c, false)
}
fn UrlApplySchemeW(c: &mut Ctx) -> u32 {
    url_apply(c, true)
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
/// PathRemoveBackslash: the trailing backslash off, except a root's ("C:\\"); returns the
/// last character (where the backslash was)
fn remove_backslash(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    let pc = path_chars(p, wide);
    let last = pc.ch.len().saturating_sub(1);
    if pc.ch.last() == Some(&BS) && !path_is_root(&pc.ch) {
        put_nul(p, pc.off[last], wide);
    }
    p + pc.off[last]
}
fn PathRemoveBackslashA(c: &mut Ctx) -> u32 {
    remove_backslash(c, false)
}
fn PathRemoveBackslashW(c: &mut Ctx) -> u32 {
    remove_backslash(c, true)
}
fn strip_path(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    let pc = path_chars(p, wide);
    let at = pc.off[file_name_at(&pc.ch)];
    let end = pc.off[pc.ch.len()] + if wide { 2 } else { 1 };
    mem::copy(p, p + at, end - at);
    0
}
fn PathStripPathA(c: &mut Ctx) -> u32 {
    strip_path(c, false)
}
fn PathStripPathW(c: &mut Ctx) -> u32 {
    strip_path(c, true)
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
fn IsNetworkAlive(c: &mut Ctx) -> u32 {
    put32(c.a(0), 1);
    1
}
fn remove_file_spec(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return 0;
    }
    let pc = path_chars(p, wide);
    let Some(at) = file_spec_at(&pc.ch) else { return 0 };
    put_nul(p, pc.off[at], wide);
    1
}
fn PathRemoveFileSpecA(c: &mut Ctx) -> u32 {
    remove_file_spec(c, false)
}
fn PathRemoveFileSpecW(c: &mut Ctx) -> u32 {
    remove_file_spec(c, true)
}
fn PathFileExistsA(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), false)).is_some() as u32
}
fn PathFileExistsW(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), true)).is_some() as u32
}
fn PathIsDirectoryA(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), false)).is_some_and(|h| crate::files::is_dir(&h)) as u32 * 16
}
fn PathIsDirectoryW(c: &mut Ctx) -> u32 {
    crate::files::resolve(&arg_str(c.a(0), true)).is_some_and(|h| crate::files::is_dir(&h)) as u32 * 16
}
fn is_relative(s: &str) -> bool {
    !(s.starts_with('\\') || s.as_bytes().get(1) == Some(&b':'))
}
/// PathCombine: the file alone when it is absolute, the directory's root plus a rooted file
/// ("\\x"), else directory\\file; then canonicalized. None: neither given.
fn combine(dir: Option<&str>, file: Option<&str>) -> Option<String> {
    let t = match (dir, file) {
        (None, None) => return None,
        (d, None | Some("")) => d.unwrap_or("").to_string(),
        (None | Some(""), Some(f)) => f.to_string(),
        (Some(d), Some(f)) if !is_relative(f) => {
            if f.starts_with('\\') && !f.starts_with("\\\\") {
                format!("{}{}", strip_to_root(d).unwrap_or_default().trim_end_matches('\\'), f)
            } else {
                f.to_string()
            }
        }
        (Some(d), Some(f)) => format!("{}{}{f}", d, if d.ends_with('\\') { "" } else { "\\" }),
    };
    Some(canonicalize(&t))
}
fn opt_str(p: u32, wide: bool) -> Option<String> {
    (p != 0).then(|| arg_str(p, wide))
}
/// PathAppend: PathCombine of the path and the more part without its leading backslashes
fn path_append(c: &mut Ctx, wide: bool) -> u32 {
    let (p, more) = (c.a(0), opt_str(c.a(1), wide));
    let (Some(more), true) = (more, p != 0) else { return 0 };
    let more = if more.starts_with("\\\\") { &more[..] } else { more.trim_start_matches('\\') };
    let Some(r) = combine(Some(&arg_str(p, wide)), Some(more)) else { return 0 };
    ret_str(p, 260, &r, wide);
    1
}
fn PathAppendA(c: &mut Ctx) -> u32 {
    path_append(c, false)
}
fn PathAppendW(c: &mut Ctx) -> u32 {
    path_append(c, true)
}
fn path_combine(c: &mut Ctx, wide: bool) -> u32 {
    let d = c.a(0);
    if d == 0 {
        return 0;
    }
    match combine(opt_str(c.a(1), wide).as_deref(), opt_str(c.a(2), wide).as_deref()) {
        Some(r) => {
            ret_str(d, 260, &r, wide);
            d
        }
        None => {
            put_nul(d, 0, wide);
            0
        }
    }
}
fn PathCombineA(c: &mut Ctx) -> u32 {
    path_combine(c, false)
}
fn PathCombineW(c: &mut Ctx) -> u32 {
    path_combine(c, true)
}
/// PathCanonicalize: "." and ".." parts resolved ("C:\\a\\..\\b" -> "C:\\b"; Unity builds its
/// _Data folder's path with it). As Windows: ".." stops at the root and at a UNC server,
/// a relative path backed up past its start becomes rooted ("a\\.." -> "\\"), doubled
/// backslashes stay, "..." is a name, a bare drive gets its backslash.
fn canonicalize(p: &str) -> String {
    if p.is_empty() {
        return "\\".into();
    }
    let (mut root, rest) = if p.as_bytes().get(1) == Some(&b':') {
        let n = if p.as_bytes().get(2) == Some(&b'\\') { 3 } else { 2 };
        p.split_at(n)
    } else if let Some(r) = p.strip_prefix("\\\\") {
        ("\\\\", r)
    } else if let Some(r) = p.strip_prefix('\\') {
        ("\\", r)
    } else {
        ("", p)
    };
    let unc = root == "\\\\";
    let mut out: Vec<&str> = Vec::new();
    let parts: Vec<&str> = rest.split('\\').collect();
    for (i, &part) in parts.iter().enumerate() {
        match part {
            "." => {}
            // (a trailing empty part keeps the trailing backslash; others are doubled ones)
            "" if i + 1 == parts.len() && out.is_empty() => {}
            ".." => {
                if !(unc && out.len() <= 1) {
                    out.pop();
                }
                if out.is_empty() && root.is_empty() {
                    root = "\\";
                }
            }
            x => out.push(x),
        }
    }
    let mut r = format!("{root}{}", out.join("\\"));
    if r.len() == 2 && r.ends_with(':') {
        r.push('\\');
    }
    r
}
fn PathCanonicalizeA(c: &mut Ctx) -> u32 {
    let (d, s) = (c.a(0), arg_str(c.a(1), false));
    ret_str(d, 260, &canonicalize(&s), false);
    1
}
fn PathCanonicalizeW(c: &mut Ctx) -> u32 {
    let (d, s) = (c.a(0), arg_str(c.a(1), true));
    ret_str(d, 260, &canonicalize(&s), true);
    1
}
fn find_file_name(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    let pc = path_chars(p, wide);
    p + pc.off[file_name_at(&pc.ch)]
}
fn PathFindFileNameA(c: &mut Ctx) -> u32 {
    find_file_name(c, false)
}
fn PathFindFileNameW(c: &mut Ctx) -> u32 {
    find_file_name(c, true)
}
/// PathFindExtension: the last '.' after the last backslash or space, else the end
fn find_extension(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    let pc = path_chars(p, wide);
    let mut dot = None;
    for (i, &x) in pc.ch.iter().enumerate() {
        if x == BS || x == b' ' as u32 {
            dot = None;
        } else if x == b'.' as u32 {
            dot = Some(i);
        }
    }
    p + pc.off[dot.unwrap_or(pc.ch.len())]
}
fn PathFindExtensionA(c: &mut Ctx) -> u32 {
    find_extension(c, false)
}
fn PathFindExtensionW(c: &mut Ctx) -> u32 {
    find_extension(c, true)
}
/// PathRenameExtensionA/W(path, ext): replaces (or appends) the extension; FALSE past MAX_PATH
fn PathRenameExtensionA(c: &mut Ctx) -> u32 {
    let (p, mut s, ext) = (c.a(0), cstr(c.a(0)), cstr(c.a(1)));
    let start = s.iter().rposition(|&b| b == b'\\').map_or(0, |k| k + 1);
    s.truncate(s[start..].iter().rposition(|&b| b == b'.').map_or(s.len(), |k| start + k));
    if s.len() + ext.len() >= 260 {
        return 0;
    }
    s.extend_from_slice(&ext);
    s.push(0);
    mem::write(p, &s);
    1
}
fn PathRenameExtensionW(c: &mut Ctx) -> u32 {
    let (p, mut s, ext) = (c.a(0), wstr(c.a(0)), wstr(c.a(1)));
    let start = s.iter().rposition(|&b| b == b'\\' as u16).map_or(0, |k| k + 1);
    s.truncate(s[start..].iter().rposition(|&b| b == b'.' as u16).map_or(s.len(), |k| start + k));
    if s.len() + ext.len() >= 260 {
        return 0;
    }
    s.extend_from_slice(&ext);
    s.push(0);
    for (i, v) in s.iter().enumerate() {
        w16(p + 2 * i as u32, *v);
    }
    1
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
impl Hasher {
    fn digest(&self) -> (crate::cng::Hash, Vec<u8>) {
        use crate::cng::Hash;
        match self {
            Self::Md5(h) => (Hash::Md5, h.clone().finalize().0.to_vec()),
            Self::Sha1(h) => (Hash::Sha1, h.clone().finish().to_vec()),
            Self::Sha256(h) => (Hash::Sha256, sha2::Digest::finalize(h.clone()).to_vec()),
        }
    }
}

// CryptoAPI PUBLICKEYBLOBs use little-endian RSA integers and signatures, unlike
// CNG. Keep the original blob for export; use CNG's real RSA implementation to
// verify signatures (including the DigestInfo OID). SDRT's UCManSvc imports these
// APIs; no runtime-specific result or key is substituted here.
#[derive(Clone)]
struct PublicRsa {
    key: crate::cng::Rsa,
    blob: Vec<u8>,
    alg: u32,
    bits: u32,
    exportable: bool,
}
impl PublicRsa {
    fn from_blob(b: &[u8]) -> Result<Self, u32> {
        use rsa::{BigUint, RsaPublicKey};
        let bad = crate::cng::NTE_BAD_DATA;
        if b.len() < 20 || b[..4] != [6, 2, 0, 0] {
            return Err(bad);
        }
        let word = |i| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let (alg, magic, bits, exponent) = (word(4), word(8), word(12), word(16));
        if !matches!(alg, 0x2400 | 0xa400) {
            return Err(0x8009_0008); // NTE_BAD_ALGID
        }
        if magic != 0x3141_5352 || !(384..=16384).contains(&bits) || bits % 8 != 0 || b.len() != 20 + bits as usize / 8 {
            return Err(bad);
        }
        let n = BigUint::from_bytes_le(&b[20..]);
        if n.bits() != bits as usize {
            return Err(bad);
        }
        let key = RsaPublicKey::new_with_max_size(n, BigUint::from(exponent), 16384).map_err(|_| bad)?;
        Ok(Self { key: crate::cng::Rsa::Public(key), blob: b.to_vec(), alg, bits, exportable: false })
    }
    fn generated(key: crate::capi::Key, spec: u32) -> Result<Self, u32> {
        use rsa::traits::PublicKeyParts;
        let public = key.rsa.public();
        let e = public.e().to_bytes_le();
        if e.len() > 4 { return Err(crate::cng::NTE_BAD_DATA); }
        let mut exponent = [0; 4]; exponent[..e.len()].copy_from_slice(&e);
        let alg: u32 = if matches!(spec, 1 | 0xa400) { 0xa400 } else { 0x2400 };
        let mut blob = vec![6, 2, 0, 0]; blob.extend(alg.to_le_bytes());
        blob.extend(0x3141_5352u32.to_le_bytes()); blob.extend(key.bits.to_le_bytes()); blob.extend(exponent);
        let n = public.n().to_bytes_le(); blob.extend(&n); blob.resize(20 + key.bits as usize / 8, 0);
        Ok(Self { key: key.rsa, blob, alg, bits: key.bits, exportable: key.exportable })
    }
    fn private_blob(&self) -> Result<Vec<u8>, u32> {
        if !self.exportable { return Err(crate::cng::NTE_BAD_KEY_STATE); }
        let full = self.key.to_blob(crate::cng::Blob::FullPrivate, self.bits)?;
        let e = u32::from_le_bytes(full[8..12].try_into().unwrap()) as usize;
        let n = self.bits as usize / 8; let half = n / 2;
        let mut result = self.blob.clone(); result[0] = 7; result[8..12].copy_from_slice(&0x3241_5352u32.to_le_bytes());
        let mut at = 24 + e + n;
        for size in [half,half,half,half,half,n] {
            result.extend(full.get(at..at+size).ok_or(crate::cng::NTE_BAD_DATA)?.iter().rev()); at += size;
        }
        Ok(result)
    }
    fn verify(&self, hash: crate::cng::Hash, digest: &[u8], signature: &[u8], flags: u32) -> Result<(), u32> {
        if flags & !1 != 0 {
            return Err(crate::cng::NTE_BAD_FLAGS);
        }
        if signature.len() != self.bits as usize / 8 {
            return Err(crate::cng::NTE_BAD_SIGNATURE);
        }
        let sig: Vec<u8> = signature.iter().rev().copied().collect();
        self.key.verify(crate::cng::Pad::Pkcs1((flags & 1 == 0).then_some(hash)), digest, &sig)
    }
}
static RSA_KEYS: Mutex<Option<HashMap<u32, PublicRsa>>> = Mutex::new(None);
fn crypto_result(result: Result<(), u32>) -> u32 {
    match result {
        Ok(()) => 1,
        Err(e) => {
            crate::k32::last_error(e);
            0
        }
    }
}
fn CryptImportKey(c: &mut Ctx) -> u32 {
    if let Err(e) = crate::capi::provider(c.a(0)) { return crypto_result(Err(e)); }
    if c.a(1) == 0 || c.a(5) == 0 {
        return crypto_result(Err(87));
    }
    if c.a(3) != 0 || c.a(4) != 0 {
        return crypto_result(Err(crate::cng::NTE_BAD_FLAGS));
    }
    match PublicRsa::from_blob(mem::slice(c.a(1), c.a(2))) {
        Ok(key) => {
            let id = NEXT_HASH.fetch_add(1, Ordering::Relaxed);
            RSA_KEYS.lock().unwrap().get_or_insert_with(HashMap::default).insert(id, key);
            mem::putp(c.a(5), id);
            1
        }
        Err(e) => crypto_result(Err(e)),
    }
}
fn CryptExportKey(c: &mut Ctx) -> u32 {
    if c.a(5) == 0 {
        return crypto_result(Err(87));
    }
    if c.a(1) != 0 || c.a(3) != 0 {
        return crypto_result(Err(crate::cng::NTE_BAD_FLAGS));
    }
    if !matches!(c.a(2), 6 | 7) {
        return crypto_result(Err(0x8009_000a)); // NTE_BAD_TYPE
    }
    let g = RSA_KEYS.lock().unwrap();
    let Some(k) = g.as_ref().and_then(|m| m.get(&c.a(0))) else {
        return crypto_result(Err(0x8009_0003)); // NTE_BAD_KEY
    };
    let blob = if c.a(2) == 7 { match k.private_blob() { Ok(b) => b, Err(e) => return crypto_result(Err(e)) } } else { k.blob.clone() };
    let (out, len) = (c.a(4), c.a(5));
    let cap = r32(len);
    put32(len, blob.len() as u32);
    if out != 0 {
        if cap < blob.len() as u32 {
            return crypto_result(Err(234));
        }
        mem::write(out, &blob);
    }
    1
}
fn CryptVerifySignatureA(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return crypto_result(Err(87));
    }
    if c.a(5) & !1 != 0 {
        return crypto_result(Err(crate::cng::NTE_BAD_FLAGS));
    }
    let keys = RSA_KEYS.lock().unwrap();
    let Some(k) = keys.as_ref().and_then(|m| m.get(&c.a(3))) else {
        return crypto_result(Err(0x8009_0003));
    };
    let (alg, digest) = {
        let mut hashes = HASHES.lock().unwrap();
        let Some((h, done)) = hashes.as_mut().and_then(|m| m.get_mut(&c.a(0))) else {
            return crypto_result(Err(0x8009_0002)); // NTE_BAD_HASH
        };
        *done = true;
        h.digest()
    };
    crypto_result(k.verify(alg, &digest, mem::slice(c.a(1), c.a(2)), c.a(5)))
}
fn CryptVerifySignatureW(c: &mut Ctx) -> u32 {
    CryptVerifySignatureA(c)
}

/// The hashes and whether HP_HASHVAL has finished them (more data then fails with NTE_BAD_HASH_STATE)
static HASHES: Mutex<Option<HashMap<u32, (Hasher, bool)>>> = Mutex::new(None);
static HASH_PROVIDERS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
static NEXT_HASH: AtomicU32 = AtomicU32::new(0x4a00_0000);

/// The provider's default key length (bits): 40 for the Base provider asked by name, else 128
/// (no name is the Strong provider since Vista: SRPG Studio)
fn acquire(c: &mut Ctx, wide: bool) -> u32 {
    let container = (c.a(1) != 0).then(|| arg_str(c.a(1), wide));
    let name = (c.a(2) != 0).then(|| arg_str(c.a(2), wide));
    if c.a(0) == 0 && c.a(4) & 16 == 0 { return crypto_result(Err(87)); }
    match crate::capi::acquire(container.as_deref(), name.as_deref(), c.a(3), c.a(4)) {
        Ok(id) => { if c.a(4) & 16 == 0 { mem::putp(c.a(0), id); } 1 }, Err(e) => crypto_result(Err(e)),
    }
}
fn CryptAcquireContextA(c: &mut Ctx) -> u32 {
    acquire(c, false)
}
fn CryptAcquireContextW(c: &mut Ctx) -> u32 {
    acquire(c, true)
}
fn CryptReleaseContext(c: &mut Ctx) -> u32 {
    crypto_result(crate::capi::release(c.a(0), c.a(1)))
}
fn add_rsa(c: &mut Ctx, generated: bool) -> u32 {
    let (provider, spec, out) = (c.a(0), c.a(1), if generated { c.a(3) } else { c.a(2) });
    if out == 0 { return crypto_result(Err(87)); }
    let key = if generated { crate::capi::generate(provider, spec, c.a(2)) } else { crate::capi::user_key(provider, spec) };
    match key.and_then(|k| PublicRsa::generated(k, spec)) {
        Ok(key) => { let id = NEXT_HASH.fetch_add(1, Ordering::Relaxed);
            RSA_KEYS.lock().unwrap().get_or_insert_with(HashMap::default).insert(id, key); mem::putp(out, id); 1 },
        Err(e) => crypto_result(Err(e)),
    }
}
fn CryptGenKey(c: &mut Ctx) -> u32 { add_rsa(c, true) }
fn CryptGetUserKey(c: &mut Ctx) -> u32 { add_rsa(c, false) }
fn CryptSignHashA(c: &mut Ctx) -> u32 {
    if c.a(5) == 0 { return crypto_result(Err(87)); }
    if c.a(3) & !1 != 0 { return crypto_result(Err(crate::cng::NTE_BAD_FLAGS)); }
    let provider = HASH_PROVIDERS.lock().unwrap().as_ref().and_then(|m| m.get(&c.a(0))).copied();
    let Some(provider) = provider else { return crypto_result(Err(0x8009_0002)); };
    let key = match crate::capi::user_key(provider, c.a(1)) { Ok(k) => k, Err(e) => return crypto_result(Err(e)) };
    let (hash, digest) = { let mut g = HASHES.lock().unwrap();
        let Some((h, done)) = g.as_mut().and_then(|m| m.get_mut(&c.a(0))) else { return crypto_result(Err(0x8009_0002)); };
        *done = true; h.digest()
    };
    let (out, len) = (c.a(4), c.a(5)); let cap = r32(len); put32(len, key.bits / 8);
    if out == 0 { return 1; }
    if cap < key.bits / 8 { return crypto_result(Err(234)); }
    match key.rsa.sign(crate::cng::Pad::Pkcs1((c.a(3) & 1 == 0).then_some(hash)), &digest) {
        Ok(mut signature) => { signature.reverse(); mem::write(out, &signature); 1 }, Err(e) => crypto_result(Err(e)),
    }
}
fn CryptSignHashW(c: &mut Ctx) -> u32 { CryptSignHashA(c) }
fn CryptGenRandom(c: &mut Ctx) -> u32 {
    if let Err(e) = crate::capi::provider(c.a(0)) { return crypto_result(Err(e)); }
    // (the host's random bytes, as RtlGenRandom)
    let (n, p) = (c.a(1), c.a(2));
    random_into(p, n) as u32
}
fn CryptCreateHash(c: &mut Ctx) -> u32 {
    if let Err(e) = crate::capi::provider(c.a(0)) { return crypto_result(Err(e)); }
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
    HASHES.lock().unwrap().get_or_insert_with(HashMap::default).insert(id, (h, false));
    HASH_PROVIDERS.lock().unwrap().get_or_insert_with(HashMap::default).insert(id, c.a(0));
    mem::putp(c.a(4), id);
    1
}
fn CryptHashData(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(1), c.a(2));
    let mut g = HASHES.lock().unwrap();
    let Some((h, done)) = g.get_or_insert_with(HashMap::default).get_mut(&c.a(0)) else {
        crate::k32::last_error(87);
        return 0;
    };
    if *done {
        crate::k32::last_error(0x8009_000c);
        return 0;
    }
    match h {
        Hasher::Md5(h) => h.consume(data),
        Hasher::Sha1(h) => h.update(data),
        Hasher::Sha256(h) => sha2::Digest::update(h, data),
    }
    1
}
fn CryptGetHashParam(c: &mut Ctx) -> u32 {
    let (h, param, out, plen) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut g = HASHES.lock().unwrap();
    let Some((hs, done)) = g.get_or_insert_with(HashMap::default).get_mut(&h) else {
        crate::k32::last_error(87);
        return 0;
    };
    let (alg, size) = match &*hs {
        Hasher::Md5(_) => (0x8003u32, 16u32),
        Hasher::Sha1(_) => (0x8004, 20),
        Hasher::Sha256(_) => (0x800c, 32),
    };
    // (the digest only when it is asked for: not for HP_ALGID or HP_HASHSIZE)
    let val: Vec<u8> = match param {
        1 => alg.to_le_bytes().to_vec(),
        2 => hs.digest().1,
        4 => size.to_le_bytes().to_vec(),
        _ => return 0,
    };
    if out != 0 {
        if r32(plen) < val.len() as u32 {
            put32(plen, val.len() as u32);
            w32(crate::cur().teb + crate::teb_last_error(), 234);
            return 0;
        }
        mem::write(out, &val);
        *done |= param == 2;
    }
    put32(plen, val.len() as u32);
    1
}
fn CryptDestroyHash(c: &mut Ctx) -> u32 {
    if let Some(m) = HASH_PROVIDERS.lock().unwrap().as_mut() { m.remove(&c.a(0)); }
    let ok = HASHES.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0)).is_some();
    if !ok {
        crate::k32::last_error(87);
    }
    ok as u32
}

/// RC4 keys (CALG_RC4) made with CryptDeriveKey from a hash: the key's bytes and the cipher
/// state, which a final CryptEncrypt/CryptDecrypt resets (SRPG Studio's data.dts)
struct Rc4 {
    /// the key's length in bits (KP_KEYLEN: without the salt)
    bits: u32,
    key: Vec<u8>,
    s: [u8; 256],
    i: u8,
    j: u8,
}
impl Rc4 {
    fn new(key: Vec<u8>, bits: u32) -> Rc4 {
        let mut r = Rc4 { bits, key, s: [0; 256], i: 0, j: 0 };
        r.reset();
        r
    }
    fn reset(&mut self) {
        for (i, x) in self.s.iter_mut().enumerate() {
            *x = i as u8;
        }
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(self.s[i]).wrapping_add(self.key[i % self.key.len()]);
            self.s.swap(i, j as usize);
        }
        (self.i, self.j) = (0, 0);
    }
    fn apply(&mut self, d: &mut [u8]) {
        for b in d {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            *b ^= self.s[self.s[self.i as usize].wrapping_add(self.s[self.j as usize]) as usize];
        }
    }
}
static KEYS: Mutex<Option<HashMap<u32, Rc4>>> = Mutex::new(None);
/// CryptDeriveKey(prov, alg, hash, flags, &key): key length in the flags' high word (else the
/// provider's default); a 40-bit key gets 11 zero bytes of salt unless CRYPT_NO_SALT (0x10)
fn CryptDeriveKey(c: &mut Ctx) -> u32 {
    let (alg, hash, flags, out) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if alg != 0x6801 {
        eprintln!("sake: CryptDeriveKey: algorithm {alg:#x}");
        w32(crate::cur().teb + crate::teb_last_error(), 0x8009_0008);
        return 0;
    }
    let digest = {
        let g = HASHES.lock().unwrap();
        match g.as_ref().and_then(|m| m.get(&hash)) {
            Some((Hasher::Md5(x), _)) => x.clone().finalize().0.to_vec(),
            Some((Hasher::Sha1(x), _)) => x.clone().finish().to_vec(),
            Some((Hasher::Sha256(x), _)) => sha2::Digest::finalize(x.clone()).to_vec(),
            None => {
                crate::k32::last_error(87);
                return 0;
            }
        }
    };
    let bits = match flags >> 16 {
        0 => match crate::capi::provider(c.a(0)) { Ok(p) => p.rc4_bits, Err(e) => return crypto_result(Err(e)) },
        b => b,
    };
    let mut key = digest[..((bits / 8) as usize).clamp(5, digest.len())].to_vec();
    let bits = 8 * key.len() as u32;
    if key.len() == 5 && flags & 0x10 == 0 {
        key.resize(16, 0);
    }
    let id = NEXT_HASH.fetch_add(1, Ordering::Relaxed);
    KEYS.lock().unwrap().get_or_insert_with(HashMap::default).insert(id, Rc4::new(key, bits));
    mem::putp(out, id);
    1
}
/// CryptEncrypt / CryptDecrypt(key, hash, final, flags, data, &len[, cap]): RC4 in place
fn rc4_call(c: &mut Ctx) -> u32 {
    let (key, fin, data, plen) = (c.a(0), c.a(2) != 0, c.a(4), c.a(5));
    let n = r32(plen);
    let mut g = KEYS.lock().unwrap();
    let Some(k) = g.get_or_insert_with(HashMap::default).get_mut(&key) else {
        crate::k32::last_error(87);
        return 0;
    };
    // (in pieces: not a copy of the whole buffer out and back)
    let mut at = 0u32;
    let mut buf = [0u8; 4096];
    while data != 0 && at < n {
        let m = (n - at).min(4096);
        let piece = &mut buf[..m as usize];
        piece.copy_from_slice(mem::slice(data.wrapping_add(at), m));
        k.apply(piece);
        mem::write(data.wrapping_add(at), piece);
        at += m;
    }
    if fin {
        k.reset();
    }
    1
}
fn CryptDecrypt(c: &mut Ctx) -> u32 {
    rc4_call(c)
}
fn CryptEncrypt(c: &mut Ctx) -> u32 {
    rc4_call(c)
}
fn CryptDestroyKey(c: &mut Ctx) -> u32 {
    let rsa = RSA_KEYS.lock().unwrap().as_mut().is_some_and(|m| m.remove(&c.a(0)).is_some());
    let ok = rsa || KEYS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0)).is_some();
    if !ok {
        crate::k32::last_error(87);
    }
    ok as u32
}
/// CryptGetKeyParam(key, param, out, &len, flags): KP_ALGID (7), KP_BLOCKLEN (8, 0 for a
/// stream cipher), KP_KEYLEN (9)
fn CryptGetKeyParam(c: &mut Ctx) -> u32 {
    let (key, param, out, plen) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if plen == 0 {
        return crypto_result(Err(87));
    }
    if c.a(4) != 0 {
        return crypto_result(Err(crate::cng::NTE_BAD_FLAGS));
    }
    let rsa = RSA_KEYS.lock().unwrap().as_ref().and_then(|m| m.get(&key)).map(|k| (k.alg, k.bits, k.bits));
    let rc4 = || KEYS.lock().unwrap().get_or_insert_with(HashMap::default).get(&key).map(|k| (0x6801, 0, k.bits));
    let Some((alg, block, bits)) = rsa.or_else(rc4) else {
        crate::k32::last_error(87);
        return 0;
    };
    let v = match param {
        7 => alg,
        8 => block,
        9 => bits,
        _ => {
            crate::k32::last_error(0x8009_0015); // NTE_BAD_TYPE
            return 0;
        }
    };
    if out != 0 {
        if r32(plen) < 4 {
            put32(plen, 4);
            crate::k32::last_error(234);
            return 0;
        }
        w32(out, v);
    }
    put32(plen, 4);
    1
}

/// SHA-1 (CALG_SHA1) as it goes: the sha1 crate's state, 64 bytes buffered at most (it held
/// everything hashed and hashed it at the end: a launcher checking a 500 MB archive kept it
/// twice in sake's heap).
#[derive(Clone, Default)]
struct Sha1 {
    h: sha1::Sha1,
}
impl Sha1 {
    fn update(&mut self, d: &[u8]) {
        sha1::Digest::update(&mut self.h, d);
    }
    fn finish(self) -> [u8; 20] {
        sha1::Digest::finalize(self.h).into()
    }
}

fn WinVerifyTrust(_: &mut Ctx) -> u32 {
    0
}

/// XInput: each DLL's exports as Windows has them (xinput9_1_0 has no XInputEnable, xinputuap
/// none of xinput1_4's undocumented ordinals)
const XINPUT: &[(&str, &[&str])] = &[
    ("xinput1_1", &["XInputEnable", "XInputGetCapabilities", "XInputGetDSoundAudioDeviceGuids", "XInputSetState", "XInputGetState"]),
    ("xinput1_2", &["XInputEnable", "XInputGetCapabilities", "XInputGetDSoundAudioDeviceGuids", "XInputSetState", "XInputGetState"]),
    ("xinput1_3", &["XInputEnable", "XInputGetCapabilities", "XInputGetDSoundAudioDeviceGuids", "XInputSetState", "XInputGetState", "XInputGetBatteryInformation", "XInputGetKeystroke", "XInputGetStateEx"]),
    ("xinput1_4", &["XInputEnable", "XInputGetCapabilities", "XInputSetState", "XInputGetState", "XInputGetBatteryInformation", "XInputGetKeystroke", "XInputGetAudioDeviceIds", "XInputGetStateEx", "XInputWaitForGuideButton", "XInputCancelGuideButtonWait", "XInputPowerOffController", "XInputGetBaseBusInformation", "XInputGetCapabilitiesEx"]),
    ("xinput9_1_0", &["XInputGetCapabilities", "XInputGetDSoundAudioDeviceGuids", "XInputSetState", "XInputGetState"]),
    ("xinputuap", &["XInputEnable", "XInputGetCapabilities", "XInputSetState", "XInputGetState", "XInputGetBatteryInformation", "XInputGetKeystroke", "XInputGetAudioDeviceIds"]),
];

/// What an XInput call answers with no controller attached (winref xinput): every user index
/// ERROR_DEVICE_NOT_CONNECTED, one past 3 ERROR_BAD_ARGUMENTS; XINPUTUAP (the Windows 10 one,
/// over Windows.Gaming.Input) first wants COM / WinRT on the thread: CO_E_NOTINITIALIZED.
fn xinput_answer(uap: bool, user: u32, com: bool) -> u32 {
    if uap && !com {
        0x8004_01F0
    } else if user > 3 {
        160
    } else {
        1167
    }
}
fn xinput_none(c: &mut Ctx) -> u32 {
    xinput_answer(false, c.a(0), true)
}
fn xinputuap_none(c: &mut Ctx) -> u32 {
    xinput_answer(true, c.a(0), com_initialized())
}
fn xinput_ok(_: &mut Ctx) -> u32 {
    0
}

/// GetFileTitle's answer for a path (comdlg32; as Wine): the part after the last '\\', '/' or
/// ':'; None (-1) for an empty path, one with wildcards or brackets, or one ending in a separator.
/// Extensions are kept (Windows hides them only when Explorer hides known extensions).
pub(crate) fn file_title(path: &[u16]) -> Option<&[u16]> {
    let sep = |c: u16| c == b'\\' as u16 || c == b'/' as u16 || c == b':' as u16;
    if path.is_empty() || path.iter().any(|&c| c == b'*' as u16 || c == b'[' as u16 || c == b']' as u16) || sep(*path.last()?) {
        return None;
    }
    let start = path.iter().rposition(|&c| sep(c)).map_or(0, |i| i + 1);
    Some(&path[start..])
}
/// GetFileTitleA/W(lpszFile, lpszTitle, cchSize): 0, or the size it needs (with the
/// terminator) when cchSize is short, or -1. The ANSI form works on the wide text, so a
/// Shift-JIS trail byte 0x5c is no separator.
fn get_file_title(c: &mut Ctx, wide: bool) -> u32 {
    let (file, out, cap) = (c.a(0), c.a(1), c.a(2) & 0xffff);
    if file == 0 || out == 0 {
        return u32::MAX;
    }
    let w: Vec<u16> = if wide { wstr(file) } else { a2w(&cstr(file)) };
    let Some(t) = file_title(&w) else { return u32::MAX };
    let t = String::from_utf16_lossy(t);
    let need = if wide { t.encode_utf16().count() } else { s2a(&t).len() } as u32 + 1;
    if cap < need {
        return need;
    }
    ret_str(out, cap, &t, wide);
    0
}
fn GetFileTitleA(c: &mut Ctx) -> u32 {
    get_file_title(c, false)
}
fn GetFileTitleW(c: &mut Ctx) -> u32 {
    get_file_title(c, true)
}
pub fn init() {
    let _ = (s2a, r8);
    implement("comdlg32", "GetFileTitleA", GetFileTitleA);
    implement("comdlg32", "GetFileTitleW", GetFileTitleW);
    wts_init();
    implement("shell32", "SHGetKnownFolderPath", SHGetKnownFolderPath);
    // what installers (Inno Setup's uninstaller) call around their windows: no themes, no
    // composition, no shutdown to block, nothing for Explorer to refresh
    for n in ["BufferedPaintInit", "BufferedPaintUnInit"] {
        implement("uxtheme", n, |_| 0);
    }
    implement("uxtheme", "IsAppThemed", |_| 0);
    implement("dwmapi", "DwmIsCompositionEnabled", |c| {
        let p = c.a(0);
        if p == 0 {
            return 0x8007_0057;
        }
        w32(p, 0);
        0
    });
    // (hwnd, attribute, out, size): with composition off (DwmIsCompositionEnabled above, as on
    // Windows 7 with the basic theme) every attribute is DWM_E_COMPOSITIONDISABLED and `out` is
    // left alone. Answering S_OK without writing gave Unity 6 a garbage
    // DWMWA_EXTENDED_FRAME_BOUNDS and its window a client area at x -1037 (逆討伐 体験版).
    implement("dwmapi", "DwmGetWindowAttribute", |_| 0x8026_3001);
    implement("user32", "DisableProcessWindowsGhosting", |_| 0);
    for n in ["ShutdownBlockReasonCreate", "ShutdownBlockReasonDestroy"] {
        implement("user32", n, |_| 1);
    }
    // (CHANGEFILTERSTRUCT: cbSize, ExtStatus = MSGFLTINFO_NONE)
    implement("user32", "ChangeWindowMessageFilterEx", |c| {
        let p = c.a(3);
        if p != 0 {
            w32(p + 4, 0);
        }
        1
    });
    implement("shell32", "SHChangeNotify", |_| 0);
    implement("shell32", "SHCreateItemFromParsingName", |c| {
        if c.a(3) != 0 {
            w32(c.a(3), 0);
        }
        0x8000_4001
    });
    // (xinputuap: the Windows 10 one, AdvHD imports it)
    for &(d, names) in XINPUT {
        for &n in names {
            implement(d, n, if n == "XInputEnable" { xinput_ok } else if d == "xinputuap" { xinputuap_none } else { xinput_none });
        }
    }
    reg!("advapi32";
        RegOpenKeyExA, RegOpenKeyExW, RegOpenKeyA, RegOpenKeyW, RegCreateKeyExA, RegCreateKeyExW, RegCreateKeyA,
        RegCreateKeyW, RegCloseKey, RegFlushKey, RegQueryValueExA, RegQueryValueExW, RegQueryValueA, RegGetValueA, RegGetValueW,
        RegSetValueExA, RegSetValueExW, RegSetValueA, RegDeleteValueA, RegDeleteValueW, RegDeleteKeyA, RegDeleteTreeA, RegDeleteTreeW,
        RegDeleteKeyW, RegDeleteKeyExA, RegDeleteKeyExW, RegEnumKeyExA, RegEnumKeyExW, RegEnumKeyA, RegEnumKeyW, RegEnumValueA, RegEnumValueW,
        RegQueryInfoKeyA, RegQueryInfoKeyW, CryptAcquireContextA, CryptAcquireContextW, CryptReleaseContext,
        CryptGenRandom, CryptCreateHash, CryptHashData, CryptGetHashParam, CryptDestroyHash, CryptDeriveKey,
        CryptDecrypt, CryptEncrypt, CryptDestroyKey, CryptGetKeyParam,
        CryptImportKey, CryptExportKey, CryptVerifySignatureA, CryptVerifySignatureW,
        CryptGenKey, CryptGetUserKey, CryptSignHashA, CryptSignHashW,
    );
    reg!("wintrust"; WinVerifyTrust);
    reg!("version"; GetFileVersionInfoSizeA, GetFileVersionInfoSizeW, GetFileVersionInfoA, GetFileVersionInfoW,
        VerQueryValueA, VerQueryValueW);
    reg!("rpcrt4"; UuidCreate, UuidCreateNil, UuidToStringA, UuidToStringW, RpcStringFreeA, UuidFromStringA, UuidFromStringW,
        UuidEqual, UuidIsNil);
    implement("rpcrt4", "UuidCreateSequential", UuidCreate);
    // the COM DLLs' class objects (CoCreateInstance's classes; programs that load the DLL
    // themselves, or check for it, get them here)
    implement("xaudio2_7", "DllGetClassObject", |c| dll_get_class_object(c, "xaudio2_7"));
    implement("windowscodecs", "DllGetClassObject", |c| dll_get_class_object(c, "windowscodecs"));
    implement("olepro32", "DllGetClassObject", |c| dll_get_class_object(c, "olepro32"));
    for d in ["xaudio2_7", "windowscodecs", "olepro32", "ksproxy", "hhctrl", "msdmo"] {
        implement(d, "DllCanUnloadNow", |_| 1); // S_FALSE
        implement(d, "DllRegisterServer", |_| 0);
        implement(d, "DllUnregisterServer", |_| 0);
    }
    implement("rpcrt4", "RpcStringFreeW", RpcStringFreeA);
    // (ole32 exports them too: mingw's libole32 imports them from there)
    implement("ole32", "GetErrorInfo", GetErrorInfo);
    implement("ole32", "SetErrorInfo", SetErrorInfo);
    reg!("ole32"; CoInitialize, CoInitializeEx, OleInitialize, CoUninitialize, OleUninitialize, CoCreateInstance, CoGetClassObject,
        CoTaskMemAlloc, CoTaskMemRealloc, CoTaskMemFree, PropVariantClear, CoGetMalloc, CoCreateGuid, CoMarshalInterface, CoUnmarshalInterface, CoReleaseMarshalData, IsEqualGUID, StringFromGUID2, StringFromCLSID, StringFromIID, CLSIDFromString, CLSIDFromProgID, IIDFromString,
        RegisterDragDrop, RevokeDragDrop, DoDragDrop, CoInitializeSecurity, CoSetProxyBlanket);
    implement("user32", "IsImmersiveProcess", |_| 0);
    reg!("oleaut32"; VarBstrFromBool);
    reg!("oleaut32"; SysAllocString, SysAllocStringLen, SysAllocStringByteLen, SysReAllocString,
        SysReAllocStringLen, SysFreeString, SysStringLen, SysStringByteLen, VariantInit, VariantClear, VariantCopy,
        VariantCopyInd, VariantChangeTypeEx, VariantChangeType, GetErrorInfo, SetErrorInfo);
    reg!("oleaut32"; SafeArrayCreate, SafeArrayCreateEx, SafeArrayCreateVector, SafeArrayAllocDescriptor,
        SafeArrayAllocDescriptorEx, SafeArrayAllocData, SafeArrayGetDim, SafeArrayGetElemsize, SafeArrayGetVartype,
        SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayLock, SafeArrayUnlock, SafeArrayAccessData,
        SafeArrayUnaccessData, SafeArrayPtrOfIndex, SafeArrayPutElement, SafeArrayGetElement, SafeArrayDestroyData,
        SafeArrayDestroyDescriptor, SafeArrayDestroy, SafeArrayRedim);
    reg!("oleaut32"; VarAdd, VarSub, VarMul, VarDiv, VarIdiv, VarMod, VarNeg, VarCmp, VarBstrCmp, VarBstrCat);
    reg!("shfolder"; SHGetFolderPathA, SHGetFolderPathW);
    reg!("shell32"; SHFileOperationA, SHFileOperationW, SHCreateDirectoryExA, SHCreateDirectoryExW, SHCreateDirectory, SHGetSpecialFolderPathA, SHGetSpecialFolderPathW, SHGetFolderPathA, SHGetFolderPathW,
        SHGetSpecialFolderLocation, SHGetFolderLocation, SHGetPathFromIDListA, SHGetPathFromIDListW, ILFree, SHGetMalloc, SHBrowseForFolderA, SHBrowseForFolderW, IsUserAnAdmin,
        ShellExecuteA, ShellExecuteW, ShellExecuteExA, ShellExecuteExW, DragAcceptFiles, DragFinish,
        DragQueryFileA, DragQueryFileW, CommandLineToArgvW);
    reg!("comctl32"; InitCommonControls, InitCommonControlsEx, ImageList_Create, ImageList_Read, ImageList_Destroy, ImageList_Add,
        ImageList_GetImageCount, ImageList_SetBkColor, ImageList_GetBkColor, ImageList_GetIconSize,
        ImageList_SetIconSize, ImageList_Remove, ImageList_Draw, ImageList_DrawEx, ImageList_ReplaceIcon);
    reg!("winmm"; timeGetTime, timeBeginPeriod, timeEndPeriod, timeGetDevCaps, timeGetSystemTime, timeSetEvent,
        timeKillEvent, joyGetNumDevs, joyGetPosEx, joyGetPos, joyGetDevCapsA, joyGetDevCapsW,
        midiOutGetNumDevs, auxGetNumDevs);
    reg!("shlwapi"; PathStripToRootA, PathStripToRootW, PathIsUNCA, PathIsUNCW, UrlApplySchemeA, UrlApplySchemeW, UrlGetPartA, UrlGetPartW);
    reg!("shlwapi"; PathRemoveFileSpecA, PathRemoveFileSpecW, PathFileExistsA, PathFileExistsW, PathIsDirectoryA,
        PathIsDirectoryW, PathAppendA, PathAppendW, PathCombineA, PathCombineW, PathCanonicalizeA, PathCanonicalizeW, PathFindFileNameA,
        PathFindFileNameW, PathFindExtensionA, PathFindExtensionW, PathRenameExtensionA, PathRenameExtensionW, PathIsRelativeA, PathIsRelativeW, PathUnquoteSpacesA,
        PathUnquoteSpacesW, PathAddBackslashA, PathAddBackslashW, PathRemoveBackslashA, PathRemoveBackslashW, PathStripPathA, PathStripPathW, PathAddExtensionA, PathAddExtensionW);
    reg!("shlwapi"; StrStrA, StrStrW, StrStrIA, StrStrIW, StrRStrIA, StrRStrIW, SHStrDupA, SHStrDupW);
    reg!("shell32"; SHGetPropertyStoreForWindow, SHGetFileInfoA, SHGetFileInfoW);
    implement("shell32", "SHBrowseForFolder", SHBrowseForFolderA);
    // (shell32 exports the ANSI one under its plain name too: matsuba_trial_1_setup.exe imports it)
    implement("shell32", "SHGetPathFromIDList", SHGetPathFromIDListA);
    reg!("shlwapi"; IsOS, SHAutoComplete);
    implement("shlwapi", "DllGetVersion", |c| dll_get_version(c.a(0), SHLWAPI_VERSION));
    implement("shell32", "DllGetVersion", |c| dll_get_version(c.a(0), SHELL32_VERSION));
    implement("comctl32", "DllGetVersion", |c| dll_get_version(c.a(0), COMCTL32_VERSION));
    reg!("sensapi"; IsNetworkAlive);
    // ws2_32 lives in ws2.rs
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

/// StrRStrI's search over characters (UTF-16 units, or an ANSI string's characters with a DBCS
/// one as lead << 8 | trail): the start of the last match of `needle` (compared folded) that
/// starts before `last` and within the string; shlwapi lets a match that starts before `last`
/// run past it by up to the needle's length - 1 (Wine's tests of StrRStrI).
pub(crate) fn rstr_i(hay: &[u16], last: usize, needle: &[u16], fold: impl Fn(u16) -> u16) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let len = needle.len();
    let last = last.min(hay.len());
    let end = last + (len - 1).min(hay.len() - last);
    (0..hay.len()).take_while(|&i| i + len <= end).filter(|&i| hay[i..i + len].iter().zip(needle).all(|(&a, &b)| fold(a) == fold(b))).last()
}
/// An ANSI string's characters (a DBCS one as lead << 8 | trail) and where each starts
fn ansi_chars(b: &[u8]) -> (Vec<u16>, Vec<usize>) {
    let (mut chars, mut at, mut i) = (Vec::new(), Vec::new(), 0);
    while i < b.len() {
        at.push(i);
        if crate::text::is_lead(b[i]) && i + 1 < b.len() {
            chars.push((b[i] as u16) << 8 | b[i + 1] as u16);
            i += 2;
        } else {
            chars.push(b[i] as u16);
            i += 1;
        }
    }
    (chars, at)
}
/// StrRStrIA/W(source, last, search): the last match of search before `last` (NULL: the end),
/// ignoring case; NULL for a NULL or empty search or none found. Flash's player (悪魔の石板, #209)
fn str_rstr_i(c: &mut Ctx, wide: bool) -> u32 {
    let (s, last, n) = (c.a(0), c.a(1), c.a(2));
    if s == 0 || n == 0 {
        return 0;
    }
    if wide {
        let (hs, ns) = (wstr(s), wstr(n));
        let last = if last == 0 { hs.len() } else { (last.wrapping_sub(s) / 2) as usize };
        return rstr_i(&hs, last, &ns, crate::text::upper16).map_or(0, |i| s + 2 * i as u32);
    }
    let (hb, nb) = (cstr(s), cstr(n));
    let ((hs, at), (ns, _)) = (ansi_chars(&hb), ansi_chars(&nb));
    // (`last` as a character index: the first character starting at or after it)
    let last = if last == 0 { hs.len() } else { at.iter().position(|&b| b >= last.wrapping_sub(s) as usize).unwrap_or(hs.len()) };
    let fold = |u: u16| if (0x61..=0x7a).contains(&u) { u - 32 } else { u };
    rstr_i(&hs, last, &ns, fold).map_or(0, |i| s + at[i] as u32)
}
fn StrRStrIA(c: &mut Ctx) -> u32 {
    str_rstr_i(c, false)
}
fn StrRStrIW(c: &mut Ctx) -> u32 {
    str_rstr_i(c, true)
}

// ---------------------------------------------------------------------------------------------
// wtsapi32 (Remote Desktop sessions: FVP's and Yuka's trials ask), bcryptprimitives

/// The one session: 1, the console, active, as a desktop user's on Windows.
const SESSION: u32 = 1;

/// An ASCII string in a new heap block (as WTSFreeMemory frees): UTF-16 or ANSI.
fn wts_str(s: &str, wide: bool) -> (u32, u32) {
    let n = s.len() as u32 + 1;
    let p = mem::heap_alloc(if wide { 2 * n } else { n }, true);
    for (i, b) in s.bytes().enumerate() {
        if wide { w16(p + 2 * i as u32, b as u16) } else { mem::write(p + i as u32, &[b]) }
    }
    (p, if wide { 2 * n } else { n })
}

/// WTSQuerySessionInformation(server, session, class, &buf, &bytes): the console session's
/// (as Windows answers for it: empty client strings, zero client numbers); other sessions don't
/// exist.
fn wts_query(c: &mut Ctx, wide: bool) -> u32 {
    let (session, class, out, bytes) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if out == 0 || bytes == 0 {
        crate::k32::last_error(87);
        return 0;
    }
    // (WTS_CURRENT_SESSION: -1)
    if session != SESSION && session != u32::MAX {
        crate::k32::last_error(7022); // ERROR_CTX_WINSTATION_NOT_FOUND
        return 0;
    }
    let zeros = |n: u32| (mem::heap_alloc(n, true), n);
    let (p, n) = match class {
        // initial program, application name, working directory, OEM id, client name, client directory
        0 | 1 | 2 | 3 | 10 | 11 => wts_str("", wide),
        4 => {
            let (p, n) = zeros(4);
            w32(p, SESSION);
            (p, n)
        }
        5 => wts_str("user", wide),
        6 => wts_str("Console", wide),
        7 => wts_str("WORKGROUP", wide),
        // connect state (WTSActive), client build number, client hardware id
        8 | 9 | 13 => zeros(4),
        // client product id, protocol type (the console: 0)
        12 | 16 => zeros(2),
        // WTS_CLIENT_ADDRESS, WTS_CLIENT_DISPLAY
        14 => zeros(24),
        15 => zeros(12),
        _ => {
            crate::k32::last_error(87);
            return 0;
        }
    };
    mem::putp(out, p);
    w32(bytes, n);
    1
}

fn WTSQuerySessionInformationA(c: &mut Ctx) -> u32 {
    wts_query(c, false)
}
fn WTSQuerySessionInformationW(c: &mut Ctx) -> u32 {
    wts_query(c, true)
}

/// WTSEnumerateSessions(server, 0, 1, &info, &count): the one session
/// (WTS_SESSION_INFO {SessionId, pWinStationName, State}).
fn wts_enum(c: &mut Ctx, wide: bool) -> u32 {
    let (out, count) = (c.a(3), c.a(4));
    if c.a(1) != 0 || c.a(2) != 1 || out == 0 || count == 0 {
        crate::k32::last_error(87);
        return 0;
    }
    let ps = mem::ps();
    let info = mem::heap_alloc(3 * ps, true);
    w32(info, SESSION);
    mem::putp(info + ps, wts_str("Console", wide).0);
    w32(info + 2 * ps, 0);
    mem::putp(out, info);
    w32(count, 1);
    1
}
fn WTSEnumerateSessionsA(c: &mut Ctx) -> u32 {
    wts_enum(c, false)
}
fn WTSEnumerateSessionsW(c: &mut Ctx) -> u32 {
    wts_enum(c, true)
}

/// WTSFreeMemory(p): the blocks above (their strings are kept: a few bytes per call).
fn WTSFreeMemory(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}

/// ProcessIdToSessionId(pid, &session): every process is in the one session.
fn ProcessIdToSessionId(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        crate::k32::last_error(87);
        return 0;
    }
    w32(c.a(1), SESSION);
    1
}

/// ProcessPrng(buf, len): always TRUE (Chromium's base calls it for every random byte).
fn ProcessPrng(c: &mut Ctx) -> u32 {
    random_into(c.a(0), c.a(1));
    1
}
/// The host's random bytes into guest [p, p + n), 256 at a time (not a host buffer of n bytes
/// first: a guest n up to 4 GB). Whether the host gave them.
fn random_into(p: u32, n: u32) -> bool {
    let mut b = [0u8; 256];
    let mut at = 0u32;
    while at < n {
        let k = (n - at).min(256);
        if unsafe { libc::getentropy(b.as_mut_ptr().cast(), k as usize) } != 0 {
            return false;
        }
        mem::write(p.wrapping_add(at), &b[..k as usize]);
        at += k;
    }
    true
}

// ---------------------------------------------------------------------------------------------
// secur32 (SSPI): no security packages. libcurl's global init (Unity 6 runs it at startup and
// gives up without it) wants the function table only; every package call fails.

/// SecurityFunctionTable (version 1) entries, in order; None: reserved.
const SSPI_TABLE: [Option<&str>; 26] = [
    Some("EnumerateSecurityPackages"), Some("QueryCredentialsAttributes"), Some("AcquireCredentialsHandle"),
    Some("FreeCredentialsHandle"), None, Some("InitializeSecurityContext"), Some("AcceptSecurityContext"),
    Some("CompleteAuthToken"), Some("DeleteSecurityContext"), Some("ApplyControlToken"), Some("QueryContextAttributes"),
    Some("ImpersonateSecurityContext"), Some("RevertSecurityContext"), Some("MakeSignature"), Some("VerifySignature"),
    Some("FreeContextBuffer"), Some("QuerySecurityPackageInfo"), None, None, Some("ExportSecurityContext"),
    Some("ImportSecurityContext"), Some("AddCredentials"), None, Some("QuerySecurityContextToken"),
    Some("EncryptMessage"), Some("DecryptMessage"),
];
/// The A/W-less names of [`SSPI_TABLE`] that have no A and W forms
const SSPI_PLAIN: [&str; 14] = [
    "FreeCredentialsHandle", "AcceptSecurityContext", "CompleteAuthToken", "DeleteSecurityContext", "ApplyControlToken",
    "ImpersonateSecurityContext", "RevertSecurityContext", "MakeSignature", "VerifySignature", "FreeContextBuffer",
    "ExportSecurityContext", "QuerySecurityContextToken", "EncryptMessage", "DecryptMessage",
];

/// The exported name of table entry `n` in the A or W table.
fn sspi_name(n: &str, wide: bool) -> String {
    if SSPI_PLAIN.contains(&n) { n.to_string() } else { format!("{n}{}", if wide { 'W' } else { 'A' }) }
}

fn sspi_init() {
    for n in SSPI_TABLE.iter().flatten() {
        for wide in [false, true] {
            // SEC_E_UNSUPPORTED_FUNCTION (FreeContextBuffer: nothing was handed out to free)
            implement("secur32", &sspi_name(n, wide), if *n == "FreeContextBuffer" { |_| 0 } else { |_| 0x8009_0302 });
        }
    }
    implement("secur32", "InitSecurityInterfaceA", |_| sspi_table(false));
    implement("secur32", "InitSecurityInterfaceW", |_| sspi_table(true));
}

/// The A or W function table (made once; Windows' is static too).
fn sspi_table(wide: bool) -> u32 {
    static TABLES: Mutex<[u32; 2]> = Mutex::new([0; 2]);
    let mut t = TABLES.lock().unwrap();
    if t[wide as usize] == 0 {
        let ps = mem::ps();
        let p = mem::heap_alloc(ps * (1 + SSPI_TABLE.len() as u32), true);
        mem::w32(p, 1); // SECURITY_SUPPORT_PROVIDER_INTERFACE_VERSION
        // (the addresses GetProcAddress gives for secur32's exports: programs compare them)
        let module = crate::pe::by_name("secur32.dll");
        for (i, n) in SSPI_TABLE.iter().enumerate() {
            if let Some(n) = n {
                let name = sspi_name(n, wide);
                let f = module.and_then(|m| crate::pe::proc_address(m, crate::pe::Import::Name(&name)));
                mem::wp(p + ps * (1 + i as u32), f.unwrap_or_else(|| crate::api::export("secur32", &name)));
            }
        }
        t[wide as usize] = p;
    }
    t[wide as usize]
}

fn wts_init() {
    // session change notifications: registered, and none come (the session never changes)
    for n in ["WTSRegisterSessionNotification", "WTSUnRegisterSessionNotification", "WTSRegisterSessionNotificationEx", "WTSUnRegisterSessionNotificationEx"] {
        implement("wtsapi32", n, |_| 1);
    }
    reg!("wtsapi32"; WTSQuerySessionInformationA, WTSQuerySessionInformationW, WTSEnumerateSessionsA, WTSEnumerateSessionsW, WTSFreeMemory);
    // no user tokens or other servers' processes
    for n in ["WTSQueryUserToken", "WTSEnumerateProcessesA", "WTSEnumerateProcessesW"] {
        implement("wtsapi32", n, |_| {
            crate::k32::last_error(1314); // ERROR_PRIVILEGE_NOT_HELD
            0
        });
    }
    implement("wtsapi32", "WTSOpenServerA", |_| 0);
    implement("wtsapi32", "WTSOpenServerW", |_| 0);
    implement("wtsapi32", "WTSCloseServer", |_| 0);
    for d in ["kernel32", "kernelbase"] {
        implement(d, "ProcessIdToSessionId", ProcessIdToSessionId);
    }
    implement("kernel32", "WTSGetActiveConsoleSessionId", |_| SESSION);
    reg!("bcryptprimitives"; ProcessPrng);
    sspi_init();
    // wmvcore (Windows Media Format SDK): the readers are wmvcore.rs's; the writers, editors and
    // the rest can't be made
    fn not_made(c: &mut Ctx, out: u32) -> u32 {
        if c.a(out) != 0 {
            mem::putp(c.a(out), 0);
        }
        0x8000_4001 // E_NOTIMPL
    }
    let outs: [(Handler, &[&str]); 3] = [
        (|c| not_made(c, 0), &["WMCreateEditor", "WMCreateProfileManager", "WMCreateIndexer", "WMCreateWriterFileSink", "WMCreateWriterNetworkSink", "WMCreateWriterPushSink"]),
        (|c| not_made(c, 1), &["WMCreateWriter", "WMCreateBackupRestorer"]),
        (|c| not_made(c, 2), &[]),
    ];
    for (f, names) in outs {
        for n in names {
            implement("wmvcore", n, f);
        }
    }
    // AMGetErrorText(hr, buf, chars): a DirectShow error's text, for krmovie's messages
    implement("quartz", "AMGetErrorTextA", |c| {
        let t = format!("DirectShow error 0x{:08X}", c.a(0));
        if c.a(1) == 0 || (c.a(2) as usize) <= t.len() {
            return 0;
        }
        mem::write(c.a(1), t.as_bytes());
        mem::write(c.a(1) + t.len() as u32, &[0]);
        t.len() as u32
    });
    implement("quartz", "AMGetErrorTextW", |c| {
        let t = format!("DirectShow error 0x{:08X}", c.a(0));
        if c.a(1) == 0 || (c.a(2) as usize) <= t.len() {
            return 0;
        }
        for (i, b) in t.bytes().chain([0]).enumerate() {
            w16(c.a(1) + 2 * i as u32, b as u16);
        }
        t.len() as u32
    });
    // WMIsContentProtected(file, &protected): no DRM'd content here (NS_E_DRM... not, S_FALSE)
    implement("wmvcore", "WMIsContentProtected", |c| {
        if c.a(1) != 0 {
            w32(c.a(1), 0);
        }
        1 // S_FALSE
    });
}

#[cfg(test)]
mod file_title_tests {
    use super::file_title;
    use proptest::prelude::*;

    fn t(s: &str) -> Option<String> {
        let w: Vec<u16> = s.encode_utf16().collect();
        file_title(&w).map(String::from_utf16_lossy)
    }

    /// Wine's comdlg32 tests (filedlg: test_GetFileTitle) and the LZH self-extractor's call.
    #[test]
    fn as_wine() {
        assert_eq!(t("C:\\dir\\file.txt").as_deref(), Some("file.txt"));
        assert_eq!(t("file.txt").as_deref(), Some("file.txt"));
        assert_eq!(t("c:file").as_deref(), Some("file"));
        assert_eq!(t("a/b/c.lzh").as_deref(), Some("c.lzh"));
        assert_eq!(t("Z:\\ippai\\game\\地っ球.exe").as_deref(), Some("地っ球.exe"));
        assert_eq!(t(""), None);
        assert_eq!(t("C:\\dir\\"), None);
        assert_eq!(t("C:"), None);
        assert_eq!(t("C:\\*.txt"), None);
        assert_eq!(t("C:\\[a].txt"), None);
    }

    proptest! {
        /// dir + separator + name gives name; the title never holds a separator and is a suffix.
        #[test]
        fn name_after_any_dir(dir in "[A-Za-z0-9 .\\\\/:\u{3042}-\u{3093}]{0,20}", sep in prop::sample::select(vec!['\\', '/', ':']), name in "[A-Za-z0-9 ._\u{30a2}-\u{30f3}]{1,16}") {
            let p = format!("{dir}{sep}{name}");
            prop_assert_eq!(t(&p), Some(name.clone()));
            let w: Vec<u16> = p.encode_utf16().collect();
            let got = file_title(&w).unwrap();
            prop_assert!(w.ends_with(got));
            prop_assert!(!got.iter().any(|&c| c == 92 || c == 47 || c == 58));
        }
    }
}

/// For fuzz/fuzz_targets/registry.rs: the registry file's text and the merge of two
/// processes' changes on any bytes.
#[doc(hidden)]
pub mod reg_fuzz {
    use super::*;

    fn parsed(t: &str) -> HashMap<String, KeyData> {
        let mut m = HashMap::default();
        parse_reg(t, &mut m);
        m
    }

    /// `data`: up to three texts split at 0xff bytes (base, here, file). Parsing never panics
    /// and writing what was read reads back the same; the merge keeps the keys this process did
    /// not change as the file has them, leaves this process's alone when the file did not
    /// change, and with nothing changed here is the file.
    pub fn run(data: &[u8]) {
        let mut parts = data.splitn(3, |&b| b == 0xff).map(|p| String::from_utf8_lossy(p).into_owned());
        let (b, h, f) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
        let (base, here, file) = (parsed(&b), parsed(&h), parsed(&f));
        for m in [&base, &here, &file] {
            let t = format_reg(m);
            assert_eq!(format_reg(&parsed(&t)), t, "written text reads back");
        }
        // (round trips first: what the file can hold)
        let (base, here, file) = (parsed(&format_reg(&base)), parsed(&format_reg(&here)), parsed(&format_reg(&file)));
        let mut m = here.clone();
        merge_reg(&mut m, &base, file.clone());
        for (p, k) in &file {
            let mine = here.get(p);
            let unchanged = match (mine, base.get(p)) {
                (Some(a), Some(b)) => same_key(a, b),
                (None, None) => true,
                _ => false,
            };
            if unchanged {
                assert!(m.get(p).is_some_and(|x| same_key(x, k)), "{p}: not changed here, the file's");
            }
        }
        if same_saved(&file, &base) {
            assert!(same_saved(&m, &here), "the file did not change: ours as they are");
        }
        if same_saved(&here, &base) {
            assert!(same_saved(&m, &file), "nothing changed here: the file");
        }
        // (merging again with what came out changes nothing)
        let mut again = m.clone();
        merge_reg(&mut again, &file, file.clone());
        assert!(same_saved(&again, &m));
    }

    fn same_saved(a: &HashMap<String, KeyData>, b: &HashMap<String, KeyData>) -> bool {
        fn keys(m: &HashMap<String, KeyData>) -> Vec<&String> {
            let mut v: Vec<&String> = m.iter().filter(|(_, k)| k.ours).map(|(p, _)| p).collect();
            v.sort();
            v
        }
        keys(a) == keys(b) && keys(a).iter().all(|p| same_key(&a[*p], &b[*p]))
    }
}

#[cfg(test)]
pub(crate) mod reg_cache_tests {
    //! The registry kept in memory and written later (with_reg / Reg::flush). The tests that
    //! look at the file are one test: other tests of this binary change the same registry, but
    //! only ever add to what the file holds.
    use super::*;
    use proptest::prelude::*;

    const HKEY_CURRENT_USER: u32 = 0x8000_0001;

    thread_local! {
        /// registry file writes made on this thread
        pub(super) static WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn file() -> String {
        kura::fs::read(crate::files::resolve_new(REG_FILE)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
    }

    fn line(path: &str, name: &str, data: &[u8]) -> String {
        format!("{path}\t{name}\t3\t{}\n", hex(data))
    }

    fn put(path: &str, name: &str, data: &[u8]) {
        reg_put(path, name, 3, data.to_vec());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn registry_written_later_and_never_lost() {
        use crate::test_guest::{call, enter, guest_thread};
        let _g = enter();
        guest_thread();
        let me = format!("hkcu\\software\\regcache{}", std::process::id());
        // 200 values (an installer's) are one write or two, not 200
        let w0 = WRITES.with(|n| n.get());
        for i in 0..200u32 {
            put(&me, &format!("v{i}"), &i.to_le_bytes());
        }
        assert!(WRITES.with(|n| n.get()) - w0 <= 2, "{} writes", WRITES.with(|n| n.get()) - w0);
        assert_eq!(reg_get(&me, "v199"), Some((3, 199u32.to_le_bytes().to_vec())));
        // the page's look at the saves writes them first
        kura::vfs::vnhost_vfs_changes();
        let t = file();
        for i in 0..200u32 {
            assert!(t.contains(&line(&me, &format!("v{i}"), &i.to_le_bytes())), "v{i}");
        }
        // RegFlushKey and RegCloseKey write; nothing changed: no write
        put(&me, "flushed", b"f");
        assert_eq!(call(RegFlushKey, &[HKEY_CURRENT_USER]), 0);
        assert!(file().contains(&line(&me, "flushed", b"f")));
        put(&me, "closed", b"c");
        let w1 = WRITES.with(|n| n.get());
        assert_eq!(call(RegCloseKey, &[HKEY_CURRENT_USER]), 0);
        assert!(file().contains(&line(&me, "closed", b"c")));
        put(&me, "closed", b"c");
        reg_flush();
        assert!(WRITES.with(|n| n.get()) - w1 <= 1);
        // a change waiting longer than REG_LATER is written by the next transaction
        put(&me, "late", b"l");
        std::thread::sleep(REG_LATER + Duration::from_millis(50));
        let _ = reg_get(&me, "late");
        assert!(file().contains(&line(&me, "late", b"l")));
        // shared: another process's change to the file is seen and merged with ours not yet
        // written (a batch's): a key it made stays, the key we changed stays ours
        reg_flush();
        let theirs = format!("{me}\\theirs");
        REG.with(reg_init, false, Duration::MAX, |h| {
            h.value.m.get_mut(&me).unwrap().values.push(("mine".into(), 3, b"m".to_vec()));
            let other = format!("{}{}{}", file(), line(&me, "lost", b"x"), line(&theirs, "t", b"t"));
            // (the same value set here and there: ours, written last)
            h.value.m.get_mut(&me).unwrap().values.retain(|v| v.0 != "v0");
            h.value.m.get_mut(&me).unwrap().values.push(("v0".into(), 3, b"ours".to_vec()));
            let other = format!("{other}{}", line(&me, "v0", b"theirs"));
            assert!(write_reg(&other));
            reg_sync(h);
            let m = &h.value.m;
            assert!(m[&me].values.iter().any(|v| v.0 == "mine"));
            assert!(m[&me].values.iter().any(|v| v.0 == "lost"), "another value of the same key: both kept");
            assert_eq!(m[&theirs].values, vec![("t".to_string(), 3, b"t".to_vec())]);
            ((), true)
        });
        reg_flush();
        let t = file();
        assert!(t.contains(&line(&theirs, "t", b"t")) && t.contains(&line(&me, "lost", b"x")) && t.contains(&line(&me, "mine", b"m")));
        assert!(t.contains(&line(&me, "v0", b"ours")) && !t.contains(&line(&me, "v0", b"theirs")));
        assert!(file().contains(&line(&me, "mine", b"m")));
        // threads writing while the page looks: every value reaches the file
        let ts: Vec<_> = (0..6)
            .map(|t| {
                let me = me.clone();
                std::thread::spawn(move || {
                    for i in 0..40u32 {
                        put(&me, &format!("t{t}i{i}"), &i.to_le_bytes());
                    }
                })
            })
            .collect();
        let page = std::thread::spawn(|| {
            for _ in 0..50 {
                kura::vfs::vnhost_vfs_changes();
                std::thread::yield_now();
            }
        });
        for t in ts {
            t.join().unwrap();
        }
        page.join().unwrap();
        REG.flush_soon();
        let t = file();
        for th in 0..6 {
            for i in 0..40u32 {
                assert!(t.contains(&line(&me, &format!("t{th}i{i}"), &i.to_le_bytes())), "t{th}i{i}");
            }
        }
        reg_delete_tree(&me);
        reg_flush();
        assert!(!file().contains(&me));
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum RegOp {
        Set(u8, u8, u8),
        DelValue(u8, u8),
        DelKey(u8),
        Sync,
        Write,
    }

    fn reg_op() -> impl Strategy<Value = (usize, RegOp)> {
        (
            0usize..2,
            prop_oneof![
                3 => (0u8..3, 0u8..3, 0u8..4).prop_map(|(k, n, d)| RegOp::Set(k, n, d)),
                1 => (0u8..3, 0u8..3).prop_map(|(k, n)| RegOp::DelValue(k, n)),
                1 => (0u8..3).prop_map(RegOp::DelKey),
                1 => Just(RegOp::Sync),
                2 => Just(RegOp::Write),
            ],
        )
    }

    proptest! {
        /// A damaged file (any text, lossy UTF-8 included) never stops the registry: it parses
        #[test]
        fn parse_any_text(t in "\\PC{0,200}", b in prop::collection::vec(any::<u8>(), 0..200)) {
            let mut m = HashMap::default();
            parse_reg(&t, &mut m);
            parse_reg(&String::from_utf8_lossy(&b), &mut m);
            parse_reg(&format!("k\tn\t3\t0\u{fffd}1"), &mut m);
        }

        /// What is written reads back: names, types and bytes
        #[test]
        fn format_parse_roundtrip(keys in prop::collection::vec(("[a-z0-9\\\\ ]{1,12}", prop::collection::vec(("[A-Za-z0-9 ]{0,8}", 0u32..12, prop::collection::vec(any::<u8>(), 0..16)), 0..4)), 0..6)) {
            let mut m: HashMap<String, KeyData> = HashMap::default();
            for (p, vals) in &keys {
                let k = m.entry(p.clone()).or_default();
                k.ours = true;
                for (n, ty, d) in vals {
                    k.values.retain(|v| !v.0.eq_ignore_ascii_case(n));
                    k.values.push((n.clone(), *ty, d.clone()));
                }
            }
            let mut back = HashMap::default();
            parse_reg(&format_reg(&m), &mut back);
            for (p, k) in &m {
                prop_assert_eq!(&back[p].values, &k.values);
            }
            prop_assert_eq!(back.len(), m.len());
        }

        /// Differential: two processes sharing the file, each changing its copy (values set and
        /// deleted, keys deleted) and syncing or writing in any order. After each write the file
        /// is what a reference of single cells says: every key's existence and every value is
        /// the one written last by a process that changed it since it last looked (a value set
        /// makes its key exist; a deleted key has no values). No process's change is lost.
        #[test]
        fn two_processes_lose_no_change(ops in prop::collection::vec(reg_op(), 1..40)) {
            type Cells = std::collections::BTreeMap<(u8, Option<u8>), Option<u8>>;
            let cells = |m: &HashMap<String, KeyData>| -> Cells {
                let mut c = Cells::new();
                for k in 0..3u8 {
                    let key = m.get(&format!("k{k}")).filter(|x| x.ours);
                    c.insert((k, None), key.map(|_| 1));
                    for n in 0..3u8 {
                        c.insert((k, Some(n)), key.and_then(|x| x.values.iter().find(|v| v.0 == format!("v{n}"))).map(|v| v.2[0]));
                    }
                }
                c
            };
            let mut file: HashMap<String, KeyData> = HashMap::default();
            let mut procs: Vec<(HashMap<String, KeyData>, HashMap<String, KeyData>)> = vec![(HashMap::default(), HashMap::default()); 2];
            let mut want = cells(&file);
            for op in ops {
                let (i, what) = op;
                let (m, base) = &mut procs[i];
                match what {
                    RegOp::Set(k, n, d) => {
                        let key = m.entry(format!("k{k}")).or_default();
                        key.ours = true;
                        key.name = format!("k{k}");
                        key.values.retain(|v| v.0 != format!("v{n}"));
                        key.values.push((format!("v{n}"), 3, vec![d]));
                    }
                    RegOp::DelValue(k, n) => {
                        if let Some(key) = m.get_mut(&format!("k{k}")) {
                            key.values.retain(|v| v.0 != format!("v{n}"));
                        }
                    }
                    RegOp::DelKey(k) => {
                        m.remove(&format!("k{k}"));
                    }
                    RegOp::Sync | RegOp::Write => {
                        let (mine, before) = (cells(m), cells(base));
                        merge_reg(m, base, file.clone());
                        *base = file.clone();
                        if what == RegOp::Write {
                            for (c, v) in &mine {
                                if before[c] != *v {
                                    want.insert(*c, *v);
                                }
                            }
                            for k in 0..3u8 {
                                let set_here = (0..3u8).any(|n| mine[&(k, Some(n))].is_some() && before[&(k, Some(n))] != mine[&(k, Some(n))]);
                                if set_here {
                                    want.insert((k, None), Some(1));
                                }
                                if want[&(k, None)].is_none() {
                                    for n in 0..3u8 {
                                        want.insert((k, Some(n)), None);
                                    }
                                }
                            }
                            // (the file holds what the process writes: its saved keys)
                            let mut text = HashMap::default();
                            parse_reg(&format_reg(m), &mut text);
                            file = text;
                            *base = file.clone();
                            prop_assert_eq!(cells(&file), want.clone());
                        }
                    }
                }
            }
        }

        /// The fuzz target's checks (reg_fuzz) on random registry texts
        #[test]
        fn reg_fuzz_checks(b in "[a-z\\\\\t0-9\n]{0,80}", h in "[a-z\\\\\t0-9\n]{0,80}", f in "[a-z\\\\\t0-9\n]{0,80}") {
            let mut data = b.into_bytes();
            data.push(0xff);
            data.extend(h.bytes());
            data.push(0xff);
            data.extend(f.bytes());
            reg_fuzz::run(&data);
        }

        #[test]
        fn hex_is_two_digits_a_byte(b in prop::collection::vec(any::<u8>(), 0..64)) {
            let want: String = b.iter().map(|x| format!("{x:02x}")).collect();
            prop_assert_eq!(hex(&b), want);
        }
    }
}

#[cfg(test)]
mod tests {
    // Windows' display names (tests/winref/golden/host_shfileinfo-32)
    #[test]
    fn display_names_hide_known_extensions() {
        assert_eq!(super::display_name("shfileinfo.txt", false), "shfileinfo");
        assert_eq!(super::display_name("host_shfileinfo-32.exe", false), "host_shfileinfo-32");
        assert_eq!(super::display_name("none.bmp", false), "none");
        assert_eq!(super::display_name("Folder", true), "Folder");
        assert_eq!(super::display_name("a.txt", true), "a.txt");
        assert_eq!(super::display_name("data.xp3", false), "data.xp3");
        assert_eq!(super::display_name(".txt", false), ".txt");
        assert_eq!(super::display_name("Setup.EXE", false), "Setup");
    }

    proptest::proptest! {
        /// a display name is the name or the name less one known extension, never anything else
        #[test]
        fn a_display_name_is_a_prefix_of_the_name(stem in "[a-zA-Z0-9 _.-]{0,12}", ext in "(txt|exe|xp3|Bmp|dat|)", dir: bool) {
            let base = if ext.is_empty() { stem.clone() } else { format!("{stem}.{ext}") };
            let d = super::display_name(&base, dir);
            proptest::prop_assert!(base.starts_with(&d));
            proptest::prop_assert!(d == base || (!dir && base.len() == d.len() + 1 + base.rsplit_once('.').unwrap().1.len()));
        }

        /// DllGetVersion: only cbSize 20 and 32 are answered, every dword written lies inside
        /// cbSize past cbSize itself, and DLLVERSIONINFO2's ullVersion carries the same
        /// major.minor.build (16-bit fields, QFE 0).
        #[test]
        fn dll_version_words_stay_in_the_struct(size in 0u32..80, major in 0u32..0x10000, minor in 0u32..0x10000, build in 0u32..0x10000) {
            use super::*;
            match dll_version_words(size, (major, minor, build)) {
                Err(e) => proptest::prop_assert!(e == 0x8007_0057 && size != 20 && size != 32),
                Ok(w) => {
                    proptest::prop_assert!(size == 20 || size == 32);
                    proptest::prop_assert!(w.iter().all(|&(o, _)| o >= 4 && o + 4 <= size && o % 4 == 0));
                    let get = |o: u32| w.iter().find(|x| x.0 == o).map(|x| x.1);
                    proptest::prop_assert_eq!((get(4), get(8), get(12), get(16)), (Some(major), Some(minor), Some(build), Some(2)));
                    if size == 32 {
                        let ull = (get(28).unwrap() as u64) << 32 | get(24).unwrap() as u64;
                        proptest::prop_assert_eq!((ull >> 48, ull >> 32 & 0xffff, ull >> 16 & 0xffff, ull & 0xffff), (major as u64, minor as u64, build as u64, 0));
                        proptest::prop_assert_eq!(get(20), Some(0));
                    }
                }
            }
        }
    }

    /// The versions Windows 10 answers: programs that check for IE5's shlwapi (5.0) or later go on.
    #[test]
    fn dll_versions_are_windows_10s() {
        use super::*;
        assert!(SHLWAPI_VERSION >= (5, 0, 0) && SHLWAPI_VERSION.0 == 6);
        assert_eq!((SHELL32_VERSION.0, SHELL32_VERSION.1), (6, 0));
        assert_eq!((COMCTL32_VERSION.0, COMCTL32_VERSION.1), (5, 82));
    }

    /// The keys every Windows has exist from the start with their parents, and enumerate under
    /// their own spelling (EXEpress CX gave up on a missing ...\\Uninstall)
    #[test]
    fn system_keys_exist() {
        use super::*;
        for path in SYSTEM_KEYS {
            let low = path.to_lowercase();
            let parts: Vec<&str> = low.split('\\').collect();
            for n in 2..=parts.len() {
                assert!(reg_key_exists(&parts[..n].join("\\")), "{}", parts[..n].join("\\"));
            }
            let (parent, name) = path.rsplit_once('\\').unwrap();
            assert!(reg_subkeys(&parent.to_lowercase()).iter().any(|k| k == name), "{name} under {parent}");
        }
        assert!(!reg_key_exists("hklm\\software\\microsoft\\windows\\currentversion\\no such key"));
    }

    #[test]
    fn registry_refresh_keeps_key_spelling() {
        use super::*;
        let mut registry = HashMap::default();
        parse_reg("hkcu\\software\\alpha\tAlpha\nhkcu\\software\\alpha\tValue\t3\t0102\n", &mut registry);
        let serialized = format_reg(&registry);
        let mut refreshed = HashMap::default();
        parse_reg(&serialized, &mut refreshed);
        let key = &refreshed["hkcu\\software\\alpha"];
        assert_eq!(key.name, "Alpha");
        assert_eq!(key.values, vec![("Value".to_string(), 3, vec![1, 2])]);
        parse_reg("hkcu\\software\\old\nhkcu\\software\\old\tname\t3\tff\n", &mut refreshed);
        assert_eq!(refreshed["hkcu\\software\\old"].values[0].2, vec![255]);
    }

    /// Windows' answers (Wine's shlwapi:url TEST_APPLY table, which passes on Windows): None is
    /// S_FALSE
    /// Windows' answers (Wine's shlwapi:url test_UrlGetPart rows): None is S_FALSE
    #[test]
    fn url_get_part_as_windows() {
        const FAIL: Result<Option<String>, u32> = Err(0x8000_4005);
        const INVAL: Result<Option<String>, u32> = Err(0x8007_0057);
        let ok = |s: &str| Ok(Some(s.to_string()));
        let (sch, host, user, pass, port, query) = (1, 2, 3, 4, 5, 6);
        let full = "http://foo:bar@localhost:21/internal.php?query=x&return=y";
        for (url, part, flags, want) in [
            (full, host, 0, ok("localhost")),
            (full, port, 0, ok("21")),
            (full, query, 0, ok("query=x&return=y")),
            ("http://foo:bar@localhost:21/internal.php#anchor", query, 0, Ok(None)),
            (full, sch, 1, ok("http")),
            (full, user, 1, ok("http:foo")),
            (full, pass, 1, ok("http:bar")),
            (full, host, 1, ok("http:localhost")),
            (full, port, 1, ok("http:21")),
            (full, query, 1, ok("query=x&return=y")),
            ("http://localhost/", user, 0, INVAL),
            ("http://localhost/", pass, 0, INVAL),
            ("http://localhost/", port, 0, INVAL),
            ("http://localhost/", query, 0, Ok(None)),
            ("http://localhost:port/", port, 0, ok("port")),
            ("http://:", host, 0, Ok(None)),
            ("http://:", port, 0, Ok(None)),
            ("http://user@localhost", user, 0, ok("user")),
            ("http://user@localhost", pass, 0, INVAL),
            ("http://@", user, 0, Ok(None)),
            ("http://@", pass, 0, INVAL),
            ("http://@", host, 0, Ok(None)),
            ("http://:@", user, 0, Ok(None)),
            ("http://:@", pass, 0, Ok(None)),
            ("http://host:port:q", port, 0, ok("port:q")),
            ("http://user:pass:q@host", pass, 0, ok("pass:q")),
            ("http://user@host@q", host, 0, ok("host@q")),
            ("http:localhost/index.html", host, 0, FAIL),
            ("http:/localhost/index.html", host, 0, FAIL),
            ("http://localhost\\index.html", host, 0, ok("localhost")),
            ("http:/\\localhost/index.html", host, 0, ok("localhost")),
            ("ftp:\\/localhost/index.html", host, 0, ok("localhost")),
            ("http://host?a:b@c:d", host, 0, ok("host")),
            ("http://host?a:b@c:d", query, 0, ok("a:b@c:d")),
            ("http://host#a:b@c:d", query, 0, Ok(None)),
            ("http:///index.html", host, 0, Ok(None)),
            ("http:///index.html", host, 1, ok("http:")),
            ("file://h o s t/c:/windows/file", host, 1, ok("h o s t")),
            ("file://foo:bar@localhost:21/file?query=x", user, 0, FAIL),
            ("file://foo:bar@localhost:21/file?query=x", host, 0, ok("foo:bar@localhost:21")),
            ("file://foo:bar@localhost:21/file?query=x", port, 0, FAIL),
            ("file://foo:bar@localhost:21/file?query=x", query, 0, ok("query=x")),
            ("http://user:pass 123@www.wine hq.org", pass, 0, ok("pass 123")),
            ("about:blank", sch, 0, ok("about")),
            ("about:blank", host, 0, FAIL),
            ("x-excid://36C00000/guid:{048B4E89}/Message.htm", sch, 0, ok("x-excid")),
            ("x-excid://36C00000/guid:{048B4E89}/Message.htm", query, 0, Ok(None)),
            ("ascheme:", sch, 0, ok("ascheme")),
            ("file:///index.html", host, 1, Ok(None)),
            ("file://c:\\index.htm", host, 0, Ok(None)),
            ("file:some text", host, 0, Ok(None)),
            ("index.htm", host, 0, FAIL),
            ("sChEmE-.+:", sch, 0, ok("scheme-.+")),
            ("scheme_:", sch, 0, Ok(None)),
            ("sch eme:", sch, 0, Ok(None)),
            (":", sch, 0, Ok(None)),
            ("a:", sch, 0, Ok(None)),
            ("0:", sch, 0, Ok(None)),
            ("ab:", sch, 0, ok("ab")),
            ("about://hostname/", host, 0, FAIL),
            ("javascript://hostname/", host, 0, FAIL),
            ("snews://hostname/", host, 1, ok("snews:hostname")),
            ("file://hostname/", host, 1, ok("hostname")),
        ] {
            assert_eq!(super::url_part(url, part, flags), want, "{url} {part} {flags}");
        }
    }

    #[test]
    fn url_apply_scheme_as_windows() {
        let (d, s, f, force) = (1, 2, 4, 8);
        for (url, flags, want) in [
            ("www.winehq.org", s | d, Some("http://www.winehq.org")),
            ("www.winehq.org", s, Some("http://www.winehq.org")),
            ("www.winehq.org", d, Some("http://www.winehq.org")),
            ("ftp.winehq.org", s | d, Some("ftp://ftp.winehq.org")),
            ("ftp.winehq.org", s, Some("ftp://ftp.winehq.org")),
            ("ftp.winehq.org", d, Some("http://ftp.winehq.org")),
            ("winehq.org", s | d, Some("http://winehq.org")),
            ("winehq.org", s, None),
            ("winehq.org", d, Some("http://winehq.org")),
            ("http://www.winehq.org", s, None),
            ("http://www.winehq.org", s | force, None),
            ("http://www.winehq.org", s | force | d, Some("http://http://www.winehq.org")),
            ("http://www.winehq.org", s | d, None),
            ("", s | d, Some("http://")),
            ("", s, None),
            ("", d, Some("http://")),
            ("u:\\windows", f | d, Some("file:///u:/windows")),
            ("u:\\windows", f, Some("file:///u:/windows")),
            ("u:\\windows", d, Some("http://u:\\windows")),
            ("file:///c:/windows", f, None),
            ("aa:\\windows", f, None),
            ("\\\\server\\share", d, Some("http://\\\\server\\share")),
            ("\\\\server\\share", f, Some("file://server/share")),
            ("\\\\server\\share", s, None),
            ("file://server/share", f, None),
            ("file://server/share", s, None),
        ] {
            assert_eq!(super::url_apply_scheme(url, flags).as_deref(), want, "{url} {flags}");
        }
    }
    use super::*;

    /// UuidToString's layout of the 16 bytes (Data1-3 little-endian)
    fn uuid_text_of(u: &[u8; 16], upper: bool) -> String {
        let d1 = u32::from_le_bytes([u[0], u[1], u[2], u[3]]);
        let d2 = u16::from_le_bytes([u[4], u[5]]);
        let d3 = u16::from_le_bytes([u[6], u[7]]);
        let t = format!("{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{}", u[8], u[9], u[10..].iter().map(|b| format!("{b:02x}")).collect::<String>());
        if upper { t.to_ascii_uppercase() } else { t }
    }
    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// tests/winref/probes/xinput.c played on the table and the answers, line by line against
    /// Windows' (the probe calls without COM on its thread, then with CoInitializeEx and
    /// RoInitialize where Windows' answers have those lines)
    fn xinput_probe(com: bool) -> Vec<String> {
        let mut out = vec![];
        for (dll, file) in [("xinput1_4", "xinput1_4.dll"), ("xinput9_1_0", "xinput9_1_0.dll"), ("xinputuap", "XINPUTUAP.dll")] {
            let names = XINPUT.iter().find(|e| e.0 == dll).unwrap().1;
            let has = |n: &str| names.contains(&n) as u32;
            out.push(format!("{file}: loaded"));
            out.push(format!("  exports: GetState {} SetState {} GetCapabilities {} Enable {}", has("XInputGetState"), has("XInputSetState"), has("XInputGetCapabilities"), has("XInputEnable")));
            for pass in ["enabled", "disabled"] {
                for i in 0..5 {
                    let r = xinput_answer(dll == "xinputuap", i, com);
                    out.push(format!("  {pass} user {i}: GetState {r} SetState {r} GetCapabilities {r}"));
                }
            }
        }
        out
    }
    #[test]
    fn xinput_as_windows() {
        for golden in [include_str!("../../../tests/winref/golden/xinput-32/stdout.txt"), include_str!("../../../tests/winref/golden/xinput-64/stdout.txt")] {
            let lines: Vec<&str> = golden.lines().collect();
            // the part without COM, then (once Windows answered them) the "com:" part
            let (plain, with_com) = match lines.iter().position(|l| l.starts_with("com:")) {
                Some(at) => (&lines[..at], Some(&lines[at + 1..])),
                None => (&lines[..], None),
            };
            assert_eq!(xinput_probe(false), plain.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            if let Some(w) = with_com {
                assert_eq!(xinput_probe(true), w.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            }
        }
        // every DLL's exports are in specs.txt (what the loader binds) and implemented
        let specs = include_str!("specs.txt");
        for &(d, names) in XINPUT {
            for n in names {
                assert!(specs.lines().any(|l| l.starts_with(&format!("{d} {n} "))), "{d}!{n} not in specs.txt");
            }
        }
    }

    proptest::proptest! {
        /// the old XInput DLLs never look at COM; XINPUTUAP answers CO_E_NOTINITIALIZED for
        /// every user index without it, and as the others with it
        #[test]
        fn xinput_answers(user in proptest::prelude::any::<u32>(), com in proptest::prelude::any::<bool>()) {
            let plain = xinput_answer(false, user, com);
            proptest::prop_assert_eq!(plain, if user > 3 { 160 } else { 1167 });
            proptest::prop_assert_eq!(xinput_answer(false, user, !com), plain);
            proptest::prop_assert_eq!(xinput_answer(true, user, com), if com { plain } else { 0x8004_01F0 });
        }
    }

    // tests/winref/probes/crypt.c (the lenient parser took any 32 hex digits among anything)
    #[test]
    fn uuid_strings_parse_strictly() {
        let u = parse_uuid(&w("6B29FC40-CA47-1067-B31D-00DD010662DA")).unwrap();
        assert_eq!(u, [0x40, 0xfc, 0x29, 0x6b, 0x47, 0xca, 0x67, 0x10, 0xb3, 0x1d, 0, 0xdd, 1, 6, 0x62, 0xda]);
        assert_eq!(parse_uuid(&w("6b29fc40-ca47-1067-b31d-00dd010662da")), Some(u));
        for bad in [
            "{6B29FC40-CA47-1067-B31D-00DD010662DA}",
            "6B29FC40-CA47-1067-B31D-00DD010662D",
            "6B29FC40CA47-1067-B31D-00DD010662DA",
            "6B29FC40-CA47-1067-B31D-00DD010662DAX",
            "G B29FC40-CA47-1067-B31D-00DD010662DA",
            "6B29FC40-CA47-1067-B31D_00DD010662DA",
            "+B29FC40-CA47-1067-B31D-00DD010662DA",
            "",
        ] {
            assert_eq!(parse_uuid(&w(bad)), None, "{bad}");
        }
    }

    proptest::proptest! {
        #[test]
        fn uuid_text_round_trips(u in proptest::array::uniform16(proptest::prelude::any::<u8>()), upper in proptest::prelude::any::<bool>()) {
            proptest::prop_assert_eq!(parse_uuid(&w(&uuid_text_of(&u, upper))), Some(u));
        }

        /// one character changed to anything else: a dash that is no longer one, a digit that
        /// is no longer hex, or a dash where a digit was, is refused; another hex digit is a
        /// different UUID
        #[test]
        fn uuid_text_one_change(u in proptest::array::uniform16(proptest::prelude::any::<u8>()), at in 0usize..36, ch in proptest::prelude::any::<u16>()) {
            let mut t = w(&uuid_text_of(&u, false));
            let was = t[at];
            proptest::prop_assume!(ch != was && !(was != u16::from(b'-') && (ch as u8 as u16 == ch) && (ch as u8).is_ascii_hexdigit() && (ch as u8).to_ascii_lowercase() as u16 == was));
            t[at] = ch;
            let is_hex = ch < 0x80 && (ch as u8).is_ascii_hexdigit();
            match parse_uuid(&t) {
                None => proptest::prop_assert!(was == u16::from(b'-') || !is_hex),
                Some(v) => {
                    proptest::prop_assert!(was != u16::from(b'-') && is_hex);
                    proptest::prop_assert_ne!(v, u);
                }
            }
        }

        #[test]
        fn uuid_text_other_lengths(t in proptest::collection::vec(proptest::prelude::any::<u16>(), 0..80)) {
            if t.len() != 36 {
                proptest::prop_assert_eq!(parse_uuid(&t), None);
            }
        }
    }

    // the answers of Windows (tests/winref/golden/shell-*)
    #[test]
    fn path_strings_follow_windows() {
        assert_eq!(canonicalize("C:\\a\\.\\b\\..\\c"), "C:\\a\\c");
        assert_eq!(canonicalize("C:\\..\\.."), "C:\\");
        assert_eq!(canonicalize("\\a\\..\\..\\b"), "\\b");
        assert_eq!(canonicalize("a\\.."), "\\");
        assert_eq!(canonicalize("C:\\a\\...\\b"), "C:\\a\\...\\b");
        assert_eq!(canonicalize("C:\\a\\."), "C:\\a");
        assert_eq!(canonicalize("\\\\s\\h\\..\\x"), "\\\\s\\x");
        assert_eq!(canonicalize("C:\\a\\\\b"), "C:\\a\\\\b");
        assert_eq!(canonicalize("C:\\dir\\"), "C:\\dir\\");
        let c = |a: Option<&str>, b: Option<&str>| combine(a, b);
        assert_eq!(c(Some("C:\\dir\\"), Some("\\file")).unwrap(), "C:\\file");
        assert_eq!(c(Some("C:\\dir"), Some("..\\x")).unwrap(), "C:\\x");
        assert_eq!(c(Some("C:\\dir"), Some("D:\\abs")).unwrap(), "D:\\abs");
        assert_eq!(c(Some("C:\\dir"), Some("")).unwrap(), "C:\\dir");
        assert_eq!(c(None, Some("file")).unwrap(), "file");
        assert_eq!(c(Some("C:\\a\\b"), Some(".\\..\\..\\..\\c")).unwrap(), "C:\\c");
        assert_eq!(c(Some("rel"), Some("x")).unwrap(), "rel\\x");
        assert!(c(None, None).is_none());
        let ch = |s: &str| s.bytes().map(u32::from).collect::<Vec<_>>();
        assert_eq!(file_name_at(&ch("C:\\dir\\")), 3);
        assert_eq!(file_name_at(&ch("C:\\")), 0);
        assert_eq!(file_name_at(&ch("\\\\srv\\share")), 6);
        assert_eq!(file_spec_at(&ch("C:\\")), None);
        assert_eq!(file_spec_at(&ch("file")), Some(0));
        assert_eq!(file_spec_at(&ch("\\\\srv\\share")), Some(5));
        assert_eq!(file_spec_at(&ch("\\rooted\\x")), Some(7));
        assert!(path_is_root(&ch("\\\\srv\\share")) && !path_is_root(&ch("\\\\srv\\share\\a")));
    }

    #[test]
    fn variant_strings_follow_windows() {
        assert_eq!(parse_num(" 12 "), Some(12.0));
        assert_eq!(parse_num("1,234.5"), Some(1234.5));
        assert_eq!(parse_num("0x10"), None);
        assert_eq!(parse_num("&H10"), Some(16.0));
        assert_eq!(parse_num("abc"), None);
        assert_eq!(parse_num(""), None);
        assert_eq!(parse_num("True"), None);
        assert_eq!(parse_num("\u{ff11}\u{ff12}"), Some(12.0));
        assert_eq!(parse_num("1e400"), Some(f64::INFINITY));
        assert_eq!(parse_num("1,,2"), None);
        assert_eq!(parse_num("1.5,0"), None);
        assert_eq!(parse_date("2024/03/05"), Some(45356.0));
        assert_eq!(parse_date("123"), None);
        assert_eq!(real_str(1.0 / 3.0, 15), "0.333333333333333");
        assert_eq!(real_str(1e10, 15), "10000000000");
        assert_eq!(real_str(45000.75, 15), "45000.75");
        assert_eq!(real_str(-2.5, 15), "-2.5");
        assert_eq!(real_str(0.1, 15), "0.1");
        assert_eq!(real_str(1e20, 15), "1E+20");
    }
}

/// SAFEARRAY (oleaut32): {cDims u16, fFeatures u16, cbElements u32, cLocks u32, pvData ptr,
/// rgsabound[cDims] of {cElements, lLbound}} with the bounds stored right to left (struct
/// bound 0 is the last dimension), the first index varying fastest, and the vartype in the
/// hidden dword 4 bytes before the struct (16 bytes hold an IID for interface arrays).
mod safearray {
    use super::*;
    const BADINDEX: u32 = 0x8002_000b;
    const LOCKED: u32 = 0x8002_000d;
    const INVALIDARG: u32 = 0x8007_0057;
    const UNEXPECTED: u32 = 0x8000_ffff;
    const F_BSTR: u16 = 0x100;
    const F_UNKNOWN: u16 = 0x200;
    const F_DISPATCH: u16 = 0x400;
    const F_VARIANT: u16 = 0x800;
    const F_HAVEIID: u16 = 0x40;
    const F_HAVEVARTYPE: u16 = 0x80;
    const F_FIXEDSIZE: u16 = 0x10;
    const F_CREATEVECTOR: u16 = 0x2000;

    fn data_at(sa: u32) -> u32 {
        sa + 12 + (mem::ps() - 4)
    }
    fn bounds(sa: u32) -> u32 {
        data_at(sa) + mem::ps()
    }
    fn dims(sa: u32) -> u32 {
        r16(sa) as u32
    }
    /// The bound of dimension `d` (1-based, left to right).
    fn bound(sa: u32, d: u32) -> u32 {
        bounds(sa) + 8 * (dims(sa) - d)
    }
    pub(super) fn elem_size(vt: u32) -> u32 {
        match vt {
            16 | 17 => 1,
            2 | 18 | 11 => 2,
            3 | 19 | 4 | 10 | 22 | 23 => 4,
            5 | 6 | 7 | 20 | 21 => 8,
            8 | 9 | 13 => mem::ps(),
            12 => 8 + 2 * mem::ps(),
            14 => 16,
            _ => 0,
        }
    }
    fn cells(sa: u32) -> u32 {
        (0..dims(sa)).map(|i| r32(bounds(sa) + 8 * i)).fold(1u32, u32::wrapping_mul)
    }
    /// The largest data block a safe array gets (the guest's heap is below 2 GB).
    const MAX_DATA: u64 = 0x7fff_0000;
    /// The bytes of `cells` elements of `es` bytes, or None past MAX_DATA (the u32 product
    /// wrapped: a 65536 x 65536 array of 4-byte cells allocated 1 byte, then was written whole).
    fn data_size(cells: u64, es: u32) -> Option<u32> {
        let n = cells.checked_mul(es as u64)?;
        (n <= MAX_DATA).then_some(n as u32)
    }
    /// The cells of the array's bounds, counted without wrapping.
    fn cells_wide(sa: u32) -> u64 {
        (0..dims(sa)).map(|i| r32(bounds(sa) + 8 * i) as u64).fold(1u64, |a, b| a.saturating_mul(b))
    }
    fn alloc_desc(vt: u32, n: u32, extra: u32) -> u32 {
        let size = 16 + 4 * mem::ps() + 8 * n.max(1);
        let p = mem::heap_alloc(size + extra, true);
        let sa = p + 16;
        w16(sa, n as u16);
        let f = match vt {
            8 => F_BSTR | F_HAVEVARTYPE,
            13 => F_UNKNOWN | F_HAVEIID,
            9 => F_DISPATCH | F_HAVEIID,
            12 => F_VARIANT | F_HAVEVARTYPE,
            _ => F_HAVEVARTYPE,
        };
        w16(sa + 2, f);
        w32(sa + 4, elem_size(vt));
        w32(sa - 4, vt);
        if f & F_HAVEIID != 0 {
            // IID_IUnknown / IID_IDispatch
            let iid: [u8; 16] = [if vt == 9 { 4 } else { 0 }, if vt == 9 { 4 } else { 0 }, 2, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
            mem::write(sa - 16, &iid);
        }
        sa
    }
    /// Frees elements `from..` of `sa`: BSTRs, VARIANTs (VariantClear) and interface
    /// references
    fn free_elems(c: &mut Ctx, sa: u32, from: u32) {
        let (f, es, n) = (r16(sa + 2), r32(sa + 4), cells(sa));
        let d = mem::rp(data_at(sa));
        if d == 0 {
            return;
        }
        for i in from..n {
            let e = d + i * es;
            if f & F_BSTR != 0 {
                bstr_free(mem::rp(e));
                mem::wp(e, 0);
            } else if f & F_VARIANT != 0 {
                variant_clear(c, e);
                mem::fill(e, es, 0);
            } else if f & (F_UNKNOWN | F_DISPATCH) != 0 {
                let o = mem::rp(e);
                if o != 0 {
                    com_call(c, o, 2);
                }
                mem::wp(e, 0);
            }
        }
    }
    fn index(sa: u32, ix: u32) -> Option<u32> {
        let n = dims(sa);
        let mut cell = 0u32;
        let mut stride = 1u32;
        for d in 1..=n {
            let b = bound(sa, d);
            let (cnt, lb) = (r32(b), r32(b + 4) as i32);
            let i = r32(ix + 4 * (d - 1)) as i32;
            if i < lb || (i as i64) >= lb as i64 + cnt as i64 {
                return None;
            }
            cell += (i - lb) as u32 * stride;
            stride *= cnt;
        }
        Some(mem::rp(data_at(sa)) + cell * r32(sa + 4))
    }
    fn create(vt: u32, n: u32, sab: u32) -> u32 {
        let es = elem_size(vt);
        if es == 0 || n == 0 || n > 0xffff {
            return 0;
        }
        let sa = alloc_desc(vt, n, 0);
        for d in 1..=n {
            mem::copy(bound(sa, d), sab + 8 * (d - 1), 8);
        }
        let data = data_size(cells_wide(sa), es).map_or(0, |n| mem::heap_alloc(n.max(1), true));
        if data == 0 {
            mem::heap_free(sa - 16);
            return 0;
        }
        mem::wp(data_at(sa), data);
        sa
    }
    pub(super) fn SafeArrayCreate(c: &mut Ctx) -> u32 {
        if c.a(2) == 0 {
            return 0;
        }
        create(c.a(0), c.a(1), c.a(2))
    }
    pub(super) fn SafeArrayCreateEx(c: &mut Ctx) -> u32 {
        SafeArrayCreate(c)
    }
    pub(super) fn SafeArrayCreateVector(c: &mut Ctx) -> u32 {
        let (vt, lb, n) = (c.a(0), c.a(1), c.a(2));
        let es = elem_size(vt);
        if es == 0 {
            return 0;
        }
        let Some(bytes) = data_size(n as u64, es).filter(|&b| b < MAX_DATA as u32 - 0x100) else { return 0 };
        let sa = alloc_desc(vt, 1, bytes);
        if sa == 16 {
            return 0;
        }
        w16(sa + 2, r16(sa + 2) | F_CREATEVECTOR);
        w32(bounds(sa), n);
        w32(bounds(sa) + 4, lb);
        mem::wp(data_at(sa), bounds(sa) + 8);
        sa
    }
    pub(super) fn SafeArrayAllocDescriptor(c: &mut Ctx) -> u32 {
        let (n, out) = (c.a(0), c.a(1));
        if n == 0 || n > 0xffff || out == 0 {
            return INVALIDARG;
        }
        let sa = alloc_desc(0, n, 0);
        w32(sa + 2, 0);
        w16(sa, n as u16);
        w32(sa + 4, 0);
        w32(sa - 4, 0);
        mem::wp(out, sa);
        0
    }
    pub(super) fn SafeArrayAllocDescriptorEx(c: &mut Ctx) -> u32 {
        let (vt, n, out) = (c.a(0), c.a(1), c.a(2));
        if n == 0 || n > 0xffff || out == 0 {
            return INVALIDARG;
        }
        let es = elem_size(vt);
        if es == 0 {
            return INVALIDARG;
        }
        mem::wp(out, alloc_desc(vt, n, 0));
        0
    }
    pub(super) fn SafeArrayAllocData(c: &mut Ctx) -> u32 {
        let sa = c.a(0);
        if sa == 0 {
            return INVALIDARG;
        }
        let d = data_size(cells_wide(sa), r32(sa + 4)).map_or(0, |n| mem::heap_alloc(n.max(1), true));
        if d == 0 {
            return 0x8007_000e;
        }
        mem::wp(data_at(sa), d);
        0
    }
    pub(super) fn SafeArrayGetDim(c: &mut Ctx) -> u32 {
        if c.a(0) == 0 { 0 } else { dims(c.a(0)) }
    }
    pub(super) fn SafeArrayGetElemsize(c: &mut Ctx) -> u32 {
        if c.a(0) == 0 { 0 } else { r32(c.a(0) + 4) }
    }
    pub(super) fn SafeArrayGetVartype(c: &mut Ctx) -> u32 {
        let (sa, out) = (c.a(0), c.a(1));
        if sa == 0 || out == 0 {
            return INVALIDARG;
        }
        let f = r16(sa + 2);
        let vt = if f & F_HAVEVARTYPE != 0 {
            r32(sa - 4)
        } else if f & F_BSTR != 0 {
            8
        } else if f & F_VARIANT != 0 {
            12
        } else if f & F_DISPATCH != 0 {
            9
        } else if f & F_UNKNOWN != 0 {
            13
        } else {
            return INVALIDARG;
        };
        w16(out, vt as u16);
        0
    }
    fn get_bound(c: &mut Ctx, upper: bool) -> u32 {
        let (sa, d, out) = (c.a(0), c.a(1), c.a(2));
        if sa == 0 || out == 0 {
            return INVALIDARG;
        }
        if d == 0 || d > dims(sa) {
            return BADINDEX;
        }
        let b = bound(sa, d);
        let lb = r32(b + 4);
        w32(out, if upper { lb.wrapping_add(r32(b)).wrapping_sub(1) } else { lb });
        0
    }
    pub(super) fn SafeArrayGetLBound(c: &mut Ctx) -> u32 {
        get_bound(c, false)
    }
    pub(super) fn SafeArrayGetUBound(c: &mut Ctx) -> u32 {
        get_bound(c, true)
    }
    pub(super) fn SafeArrayLock(c: &mut Ctx) -> u32 {
        let sa = c.a(0);
        if sa == 0 {
            return INVALIDARG;
        }
        let n = r32(sa + 8) + 1;
        if n > 0xffff {
            return UNEXPECTED;
        }
        w32(sa + 8, n);
        0
    }
    pub(super) fn SafeArrayUnlock(c: &mut Ctx) -> u32 {
        let sa = c.a(0);
        if sa == 0 {
            return INVALIDARG;
        }
        let n = r32(sa + 8);
        if n == 0 {
            return UNEXPECTED;
        }
        w32(sa + 8, n - 1);
        0
    }
    pub(super) fn SafeArrayAccessData(c: &mut Ctx) -> u32 {
        let (sa, out) = (c.a(0), c.a(1));
        if sa == 0 || out == 0 {
            return INVALIDARG;
        }
        let r = SafeArrayLock(c);
        if r != 0 {
            mem::wp(out, 0);
            return r;
        }
        mem::wp(out, mem::rp(data_at(sa)));
        0
    }
    pub(super) fn SafeArrayUnaccessData(c: &mut Ctx) -> u32 {
        SafeArrayUnlock(c)
    }
    pub(super) fn SafeArrayPtrOfIndex(c: &mut Ctx) -> u32 {
        let (sa, ix, out) = (c.a(0), c.a(1), c.a(2));
        if sa == 0 || ix == 0 || out == 0 {
            return INVALIDARG;
        }
        match index(sa, ix) {
            Some(p) => {
                mem::wp(out, p);
                0
            }
            None => BADINDEX,
        }
    }
    pub(super) fn SafeArrayPutElement(c: &mut Ctx) -> u32 {
        let (sa, ix, v) = (c.a(0), c.a(1), c.a(2));
        if sa == 0 || ix == 0 {
            return INVALIDARG;
        }
        let Some(p) = index(sa, ix) else { return BADINDEX };
        let (f, es) = (r16(sa + 2), r32(sa + 4));
        if f & F_BSTR != 0 {
            bstr_free(mem::rp(p));
            let n = if v == 0 { 0 } else { r32(v - 4) / 2 };
            let s: Vec<u16> = (0..n).map(|i| r16(v + 2 * i)).collect();
            mem::wp(p, if v == 0 { 0 } else { bstr_alloc(&s) });
        } else if f & F_VARIANT != 0 {
            variant_clear(c, p);
            mem::fill(p, es, 0);
            variant_copy(c, p, v);
        } else if f & (F_UNKNOWN | F_DISPATCH) != 0 {
            let old = mem::rp(p);
            if v != 0 {
                com_call(c, v, 1);
            }
            if old != 0 {
                com_call(c, old, 2);
            }
            mem::wp(p, v);
        } else {
            mem::copy(p, v, es);
        }
        0
    }
    pub(super) fn SafeArrayGetElement(c: &mut Ctx) -> u32 {
        let (sa, ix, v) = (c.a(0), c.a(1), c.a(2));
        if sa == 0 || ix == 0 || v == 0 {
            return INVALIDARG;
        }
        let Some(p) = index(sa, ix) else { return BADINDEX };
        let (f, es) = (r16(sa + 2), r32(sa + 4));
        if f & F_BSTR != 0 {
            let b = mem::rp(p);
            let n = if b == 0 { 0 } else { r32(b - 4) / 2 };
            let s: Vec<u16> = (0..n).map(|i| r16(b + 2 * i)).collect();
            mem::wp(v, if b == 0 { 0 } else { bstr_alloc(&s) });
        } else if f & F_VARIANT != 0 {
            mem::fill(v, es, 0);
            variant_copy(c, v, p);
        } else {
            mem::copy(v, p, es);
            if f & (F_UNKNOWN | F_DISPATCH) != 0 && mem::rp(p) != 0 {
                com_call(c, mem::rp(p), 1);
            }
        }
        0
    }
    pub(super) fn SafeArrayDestroyData(c: &mut Ctx) -> u32 {
        let sa = c.a(0);
        if sa == 0 {
            return 0;
        }
        if r32(sa + 8) != 0 {
            return LOCKED;
        }
        free_elems(c, sa, 0);
        let f = r16(sa + 2);
        if f & F_CREATEVECTOR != 0 {
            mem::fill(bounds(sa) + 8, cells(sa) * r32(sa + 4), 0);
        } else {
            let d = mem::rp(data_at(sa));
            if d != 0 {
                mem::heap_free(d);
            }
            mem::wp(data_at(sa), 0);
        }
        0
    }
    pub(super) fn SafeArrayDestroyDescriptor(c: &mut Ctx) -> u32 {
        let sa = c.a(0);
        if sa == 0 {
            return 0;
        }
        if r32(sa + 8) != 0 {
            return LOCKED;
        }
        mem::heap_free(sa - 16);
        0
    }
    pub(super) fn SafeArrayDestroy(c: &mut Ctx) -> u32 {
        let sa = c.a(0);
        if sa == 0 {
            return 0;
        }
        if r32(sa + 8) != 0 {
            return LOCKED;
        }
        SafeArrayDestroyData(c);
        SafeArrayDestroyDescriptor(c)
    }
    /// SafeArrayRedim changes the last (rightmost) dimension, keeping the elements that fit.
    pub(super) fn SafeArrayRedim(c: &mut Ctx) -> u32 {
        let (sa, nb) = (c.a(0), c.a(1));
        if sa == 0 || nb == 0 {
            return INVALIDARG;
        }
        if r32(sa + 8) != 0 {
            return LOCKED;
        }
        if r16(sa + 2) & F_FIXEDSIZE != 0 {
            return 0x8002_000c; // DISP_E_ARRAYISLOCKED's neighbour: FADF_FIXEDSIZE refuses
        }
        let es = r32(sa + 4);
        let old_cells = cells(sa);
        let b = bounds(sa);
        let (old_n, new_n) = (r32(b), r32(nb));
        let new_cells = if old_n == 0 { 0 } else { (old_cells / old_n) as u64 * new_n as u64 };
        let Some(new_size) = data_size(new_cells, es) else { return 0x8007_000e };
        let new_cells = new_cells as u32;
        if new_cells < old_cells {
            free_elems_from(c, sa, new_cells);
        }
        let old = mem::rp(data_at(sa));
        let nd = mem::heap_alloc(new_size.max(1), true);
        if nd == 0 {
            return 0x8007_000e;
        }
        if old != 0 {
            mem::copy(nd, old, old_cells.min(new_cells) * es);
            if r16(sa + 2) & F_CREATEVECTOR == 0 {
                mem::heap_free(old);
            }
        }
        mem::wp(data_at(sa), nd);
        let f = r16(sa + 2) & !F_CREATEVECTOR;
        w16(sa + 2, f);
        mem::copy(b, nb, 8);
        0
    }
    fn free_elems_from(c: &mut Ctx, sa: u32, from: u32) {
        free_elems(c, sa, from)
    }
    /// AddRef (1) or Release (2) of an interface pointer, guest or sake object alike.
    fn com_call(c: &mut Ctx, obj: u32, slot: u32) {
        let f = mem::rp(mem::rp(obj) + slot * mem::ps());
        c.call(f, &[obj]);
    }
}
use safearray::*;

/// oleaut32 variant arithmetic (VarAdd, VarSub, VarMul, VarDiv, VarIdiv, VarMod, VarNeg),
/// VarCmp and the BSTR helpers, with Windows' result types: the wider of the two operands
/// (UI1 < I2/BOOL < I4 < I8 < R4 < R8; a string is an R8, two strings concatenate in VarAdd)
/// and an integer result that overflows moving up UI1 -> I2 -> I4 -> R8.
mod vararith {
    use super::*;
    const OVERFLOW: u32 = 0x8002_000a;
    const DIVBYZERO: u32 = 0x8002_0012;
    const TYPEMISMATCH: u32 = 0x8002_0005;
    const BADVARTYPE: u32 = 0x8002_0008;

    #[derive(Clone, Copy, PartialEq, PartialOrd)]
    enum Rank {
        U1,
        I2,
        I4,
        I8,
        R4,
        R8,
    }
    struct Num {
        rank: Rank,
        i: i64,
        f: f64,
    }
    fn vt_of(v: u32) -> u16 {
        r16(v) & !0x4000
    }
    fn num(v: u32) -> Result<Option<Num>, u32> {
        let vt = vt_of(v);
        if vt == 1 {
            return Ok(None); // VT_NULL
        }
        let rank = match vt {
            0 | 2 | 11 | 16 => Rank::I2,
            17 => Rank::U1,
            3 | 18 | 22 | 10 => Rank::I4,
            19 | 23 | 20 | 21 => Rank::I8,
            4 => Rank::R4,
            5 | 6 | 7 | 8 => Rank::R8,
            _ => return Err(BADVARTYPE),
        };
        Ok(Some(match variant_val(v).ok_or(BADVARTYPE)? {
            Val::Empty => Num { rank, i: 0, f: 0.0 },
            Val::Int(i) => Num { rank, i, f: i as f64 },
            Val::F(f) => Num { rank, i: f.round_ties_even() as i64, f },
            Val::Str(s) => {
                let f = parse_num(&s).ok_or(TYPEMISMATCH)?;
                Num { rank, i: f.round_ties_even() as i64, f }
            }
        }))
    }
    fn put(dst: u32, vt: u16, bits: u64) {
        mem::fill(dst, 16, 0);
        w16(dst, vt);
        mem::write(dst + 8, &bits.to_le_bytes());
    }
    fn put_int(dst: u32, rank: Rank, i: i64) {
        let mut r = rank;
        loop {
            let fits = match r {
                Rank::U1 => (0..=255).contains(&i),
                Rank::I2 => (i16::MIN as i64..=i16::MAX as i64).contains(&i),
                Rank::I4 => (i32::MIN as i64..=i32::MAX as i64).contains(&i),
                _ => true,
            };
            if fits {
                break;
            }
            r = match r {
                Rank::U1 => Rank::I2,
                Rank::I2 => Rank::I4,
                _ => Rank::R8,
            };
        }
        match r {
            Rank::U1 => put(dst, 17, i as u8 as u64),
            Rank::I2 => put(dst, 2, i as i16 as u16 as u64),
            Rank::I4 => put(dst, 3, i as i32 as u32 as u64),
            Rank::I8 => put(dst, 20, i as u64),
            _ => put(dst, 5, (i as f64).to_bits()),
        }
    }
    fn put_real(dst: u32, rank: Rank, f: f64) -> u32 {
        if !f.is_finite() {
            return OVERFLOW;
        }
        if rank == Rank::R4 {
            put(dst, 4, (f as f32).to_bits() as u64);
        } else {
            put(dst, 5, f.to_bits());
        }
        0
    }
    fn bstr_of(v: u32) -> Vec<u16> {
        let b = r32(v + 8);
        let b = if r16(v) & 0x4000 != 0 { r32(b) } else { b };
        if b == 0 {
            return Vec::new();
        }
        (0..r32(b - 4) / 2).map(|i| r16(b + 2 * i)).collect()
    }
    /// The result rank of two operands: a 4-byte or wider integer with a float is an R8.
    fn rank(a: &Num, b: &Num) -> Rank {
        let hi = if a.rank > b.rank { a.rank } else { b.rank };
        let lo = if a.rank > b.rank { b.rank } else { a.rank };
        if hi == Rank::R4 && (lo == Rank::I4 || lo == Rank::I8) {
            Rank::R8
        } else {
            hi
        }
    }
    #[derive(Clone, Copy)]
    enum Op {
        Add,
        Sub,
        Mul,
    }
    fn arith(c: &mut Ctx, op: Op) -> u32 {
        let (l, r, out) = (c.a(0), c.a(1), c.a(2));
        if matches!(op, Op::Add) && vt_of(l) == 8 && vt_of(r) == 8 {
            let mut s = bstr_of(l);
            s.extend(bstr_of(r));
            put(out, 8, bstr_alloc(&s) as u64);
            return 0;
        }
        let (a, b) = match (num(l), num(r)) {
            (Err(e), _) | (_, Err(e)) => return e,
            (Ok(Some(a)), Ok(Some(b))) => (a, b),
            _ => {
                put(out, 1, 0);
                return 0;
            }
        };
        let k = rank(&a, &b);
        if k >= Rank::R4 {
            let f = match op {
                Op::Add => a.f + b.f,
                Op::Sub => a.f - b.f,
                Op::Mul => a.f * b.f,
            };
            return put_real(out, k, f);
        }
        let i = match op {
            Op::Add => a.i.checked_add(b.i),
            Op::Sub => a.i.checked_sub(b.i),
            Op::Mul => a.i.checked_mul(b.i),
        };
        match i {
            Some(i) => put_int(out, k, i),
            None => return OVERFLOW,
        }
        0
    }
    pub(super) fn VarAdd(c: &mut Ctx) -> u32 {
        arith(c, Op::Add)
    }
    pub(super) fn VarSub(c: &mut Ctx) -> u32 {
        arith(c, Op::Sub)
    }
    pub(super) fn VarMul(c: &mut Ctx) -> u32 {
        arith(c, Op::Mul)
    }
    /// VarDiv: always a real (R4 for two R4s); x/0 is DISP_E_OVERFLOW (0/0 too)
    pub(super) fn VarDiv(c: &mut Ctx) -> u32 {
        let (l, r, out) = (c.a(0), c.a(1), c.a(2));
        let (a, b) = match (num(l), num(r)) {
            (Err(e), _) | (_, Err(e)) => return e,
            (Ok(Some(a)), Ok(Some(b))) => (a, b),
            _ => {
                put(out, 1, 0);
                return 0;
            }
        };
        if b.f == 0.0 {
            // (x86's oleaut32 divides on the x87 and sees an overflow; x64's checks first)
            return if mem::x64() { DIVBYZERO } else { OVERFLOW };
        }
        let k = if a.rank == Rank::R4 && b.rank == Rank::R4 { Rank::R4 } else { Rank::R8 };
        put_real(out, k, a.f / b.f)
    }
    /// VarIdiv / VarMod: both rounded to integers (I4, I8 if either is), DISP_E_DIVBYZERO
    fn int_div(c: &mut Ctx, rem: bool) -> u32 {
        let (l, r, out) = (c.a(0), c.a(1), c.a(2));
        let (a, b) = match (num(l), num(r)) {
            (Err(e), _) | (_, Err(e)) => return e,
            (Ok(Some(a)), Ok(Some(b))) => (a, b),
            _ => {
                put(out, 1, 0);
                return 0;
            }
        };
        if b.i == 0 {
            return DIVBYZERO;
        }
        let v = if rem { a.i.wrapping_rem(b.i) } else { a.i.wrapping_div(b.i) };
        let k = if a.rank == Rank::I8 || b.rank == Rank::I8 {
            Rank::I8
        } else if a.rank <= Rank::I2 && b.rank <= Rank::I2 && a.rank == b.rank {
            a.rank
        } else {
            Rank::I4
        };
        put_int(out, k, v);
        0
    }
    pub(super) fn VarIdiv(c: &mut Ctx) -> u32 {
        int_div(c, false)
    }
    pub(super) fn VarMod(c: &mut Ctx) -> u32 {
        int_div(c, true)
    }
    pub(super) fn VarNeg(c: &mut Ctx) -> u32 {
        let (v, out) = (c.a(0), c.a(1));
        let a = match num(v) {
            Err(e) => return e,
            Ok(None) => {
                put(out, 1, 0);
                return 0;
            }
            Ok(Some(a)) => a,
        };
        match vt_of(v) {
            4 | 5 | 8 => put_real(out, if vt_of(v) == 4 { Rank::R4 } else { Rank::R8 }, -a.f),
            7 => {
                put(out, 7, (-a.f).to_bits());
                0
            }
            11 => {
                put(out, 2, (-a.i) as i16 as u16 as u64);
                0
            }
            _ => {
                put_int(out, if a.rank == Rank::U1 { Rank::I2 } else { a.rank }, -a.i);
                0
            }
        }
    }
    /// Strings as CompareString orders them for these: case-insensitively first, then lower
    /// before upper (NORM_IGNORECASE 1 stops there). VARCMP_LT 0 / EQ 1 / GT 2.
    fn cmp_str(a: &[u16], b: &[u16], flags: u32) -> u32 {
        let lower = |s: &[u16]| String::from_utf16_lossy(s).to_lowercase();
        let o = lower(a).cmp(&lower(b)).then_with(|| {
            if flags & 1 != 0 {
                std::cmp::Ordering::Equal
            } else {
                // a lower-case letter sorts before its capital
                let inv = |s: &[u16]| s.iter().map(|&c| if (b'a' as u16..=b'z' as u16).contains(&c) { 0 } else { 1 }).collect::<Vec<u8>>();
                inv(a).cmp(&inv(b))
            }
        });
        o as i32 as u32 + 1
    }
    pub(super) fn VarBstrCmp(c: &mut Ctx) -> u32 {
        let s = |b: u32| -> Vec<u16> { if b == 0 { Vec::new() } else { (0..r32(b - 4) / 2).map(|i| r16(b + 2 * i)).collect() } };
        cmp_str(&s(c.a(0)), &s(c.a(1)), c.a(3))
    }
    pub(super) fn VarBstrCat(c: &mut Ctx) -> u32 {
        let s = |b: u32| -> Vec<u16> { if b == 0 { Vec::new() } else { (0..r32(b - 4) / 2).map(|i| r16(b + 2 * i)).collect() } };
        let out = c.a(2);
        if out == 0 {
            return 0x8007_0057;
        }
        let mut v = s(c.a(0));
        v.extend(s(c.a(1)));
        mem::wp(out, bstr_alloc(&v));
        0
    }
    /// VarCmp(l, r, lcid, flags): two strings compare as strings, a string is greater than a
    /// number, numbers by value; VARCMP_NULL 3 with a NULL
    pub(super) fn VarCmp(c: &mut Ctx) -> u32 {
        let (l, r, flags) = (c.a(0), c.a(1), c.a(3));
        let (sl, sr) = (vt_of(l) == 8, vt_of(r) == 8);
        if sl && sr {
            return cmp_str(&bstr_of(l), &bstr_of(r), flags);
        }
        if vt_of(l) == 1 || vt_of(r) == 1 {
            return 3;
        }
        if sl != sr {
            return if sl { 2 } else { 0 };
        }
        let (a, b) = match (num(l), num(r)) {
            (Ok(Some(a)), Ok(Some(b))) => (a, b),
            (Err(e), _) | (_, Err(e)) => return e,
            _ => return 3,
        };
        match a.f.partial_cmp(&b.f) {
            Some(std::cmp::Ordering::Less) => 0,
            Some(std::cmp::Ordering::Equal) => 1,
            _ => 2,
        }
    }
}
use vararith::*;

#[cfg(test)]
mod sspi_tests {
    use super::*;

    /// Every entry of the A and W tables is a secur32 export Windows has (specs.txt), and the
    /// table has the layout of SecurityFunctionTable version 1 (26 slots after dwVersion; the
    /// reserved ones where sspi.h has them).
    #[test]
    fn table_entries_are_exports() {
        let names = crate::api::spec_names("secur32");
        for n in SSPI_TABLE.iter().flatten() {
            for wide in [false, true] {
                let e = sspi_name(n, wide);
                assert!(names.contains(&e), "{e}");
            }
        }
        let reserved: Vec<usize> = SSPI_TABLE.iter().enumerate().filter(|e| e.1.is_none()).map(|e| e.0).collect();
        assert_eq!(reserved, [4, 17, 18, 22]);
        assert_eq!(SSPI_TABLE[15], Some("FreeContextBuffer"));
        assert_eq!(SSPI_TABLE[25], Some("DecryptMessage"));
        assert_eq!(sspi_name("InitializeSecurityContext", true), "InitializeSecurityContextW");
        assert_eq!(sspi_name("MakeSignature", true), "MakeSignature");
    }
}

#[cfg(test)]
mod idlist_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    fn read_a(p: u32) -> Vec<u8> {
        (0..).map(|i| mem::r8(p + i)).take_while(|&b| b != 0).collect()
    }

    proptest! {
        /// SHGetPathFromIDList (the plain export) answers as SHGetPathFromIDListA for any folder
        /// ID list, the W form gives the same path in UTF-16, and an unknown or null list is 0
        #[test]
        fn plain_export_is_the_ansi_one(path in "[A-Za-z]:\\\\([A-Za-z0-9 _.]{1,8}|体験版|セーブ){0,4}") {
            let _g = enter();
            init();
            let plain = crate::api::find_impl("shell32", "SHGetPathFromIDList").expect("exported");
            let pidl = path_pidl(&path);
            let (a, b, w) = (mem::heap_alloc(520, true), mem::heap_alloc(520, true), mem::heap_alloc(520, true));
            prop_assert_eq!(call(plain, &[pidl, a]), 1);
            prop_assert_eq!(call(SHGetPathFromIDListA, &[pidl, b]), 1);
            prop_assert_eq!(read_a(a), read_a(b));
            prop_assert_eq!(call(SHGetPathFromIDListW, &[pidl, w]), 1);
            prop_assert_eq!(String::from_utf16_lossy(&wstr(w)), path.clone());
            if path.is_ascii() {
                prop_assert_eq!(read_a(a), path.as_bytes().to_vec());
            }
            prop_assert_eq!(call(plain, &[0, a]), 0);
            w16(pidl + 2, 0x1234);
            prop_assert_eq!(call(plain, &[pidl, a]), 0);
            for p in [pidl, a, b, w] { mem::heap_free(p); }
        }
    }
}

/// 64-bit Windows' registry views and folders as a 32-bit (WoW64) process sees them (#183)
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod wow64_tests {
    use super::*;
    use crate::test_guest::{call, enter as enter_mem, guest_thread};
    use proptest::prelude::*;

    fn enter() -> std::sync::MutexGuard<'static, ()> {
        let g = enter_mem();
        guest_thread();
        g
    }
    fn arg(s: &str) -> u32 {
        let p = mem::heap_alloc(s.len() as u32 + 1, true);
        for (i, b) in s.bytes().chain([0]).enumerate() {
            mem::w8(p + i as u32, b);
        }
        p
    }
    const HKLM: u32 = 0x8000_0002;
    /// RegQueryValueExA of a REG_SZ: (status, the text)
    fn query(key: u32, name: &str) -> (u32, String) {
        let (ty, data, len) = (mem::heap_alloc(4, true), mem::heap_alloc(520, true), mem::heap_alloc(4, true));
        w32(len, 520);
        let r = call(RegQueryValueExA, &[key, arg(name), 0, ty, data, len]);
        (r, crate::text::a2s(&mem::cstr(data)))
    }
    fn open(root: u32, sub: &str, sam: u32) -> u32 {
        let out = mem::heap_alloc(4, true);
        assert_eq!(call(RegOpenKeyExA, &[root, arg(sub), 0, 0x2_0019 | sam, out]), 0, "{sub}");
        r32(out)
    }

    /// What a 32-bit installer writes is where a program that knows it is on 64-bit Windows
    /// looks for it (SOFTWARE\Wow6432Node\...), and the other way round; KEY_WOW64_32KEY and
    /// KEY_WOW64_64KEY open the same keys.
    #[test]
    fn wow6432node_is_the_same_key() {
        let _g = enter();
        let brand = format!("SakeWow64Test{}", std::process::id());
        let (out, disp) = (mem::heap_alloc(4, true), mem::heap_alloc(4, true));
        let sub = format!("SOFTWARE\\{brand}\\Game");
        assert_eq!(call(RegCreateKeyExA, &[HKLM, arg(&sub), 0, 0, 0, 0xf003f, 0, out, disp]), 0);
        let data: Vec<u8> = "C:\\Game".bytes().chain([0]).collect();
        let d = mem::heap_alloc(data.len() as u32, true);
        mem::write(d, &data);
        assert_eq!(call(RegSetValueExA, &[r32(out), arg("InstPath"), 0, 1, d, data.len() as u32]), 0);
        for (path, sam) in [
            (format!("SOFTWARE\\Wow6432Node\\{brand}\\Game"), 0),
            (format!("Software\\WOW6432NODE\\{brand}\\Game\\"), 0x100),
            (format!("SOFTWARE\\{brand}\\Game"), 0x200),
            (format!("SOFTWARE\\{brand}\\Game"), 0x100),
        ] {
            let k = open(HKLM, &path, sam);
            assert_eq!(query(k, "InstPath"), (0, "C:\\Game".to_string()), "{path} {sam:#x}");
            call(RegCloseKey, &[k]);
        }
        // (one key: made through Wow6432Node, it is there without it)
        let sub2 = format!("SOFTWARE\\Wow6432Node\\{brand}\\Other");
        assert_eq!(call(RegCreateKeyExA, &[HKLM, arg(&sub2), 0, 0, 0, 0xf003f, 0, out, disp]), 0);
        assert_eq!(r32(disp), 1);
        assert!(reg_key_exists(&format!("hklm\\software\\{}\\other", brand.to_lowercase())));
        // (and the Wow6432Node key itself opens)
        call(RegCloseKey, &[open(HKLM, "SOFTWARE\\Wow6432Node", 0)]);
        reg_delete_tree(&format!("hklm\\software\\{brand}"));
    }

    /// ProgramFilesDir and CommonFilesDir are the view's: a 32-bit process's own the x86
    /// folders, KEY_WOW64_64KEY (NSIS's $PROGRAMFILES64, SetRegView 64) the 64-bit ones.
    #[test]
    fn program_files_by_view() {
        let _g = enter();
        let cv = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion";
        let k32 = open(HKLM, cv, 0);
        let k64 = open(HKLM, cv, 0x100);
        assert_eq!(query(k32, "ProgramFilesDir").1, "C:\\Program Files (x86)");
        assert_eq!(query(k64, "ProgramFilesDir").1, "C:\\Program Files");
        assert_eq!(query(k32, "CommonFilesDir").1, "C:\\Program Files (x86)\\Common Files");
        assert_eq!(query(k64, "CommonFilesDir").1, "C:\\Program Files\\Common Files");
        for k in [k32, k64] {
            assert_eq!(query(k, "ProgramFilesDir (x86)").1, "C:\\Program Files (x86)");
            assert_eq!(query(k, "ProgramW6432Dir").1, "C:\\Program Files");
        }
        // (a key opened from one of them keeps its view; KEY_WOW64_32KEY gives the 32-bit one)
        let sub = mem::heap_alloc(4, true);
        assert_eq!(call(RegOpenKeyExA, &[k64, 0, 0, 0x2_0019, sub]), 0);
        assert_eq!(query(r32(sub), "ProgramFilesDir").1, "C:\\Program Files");
        call(RegCloseKey, &[r32(sub)]);
        assert_eq!(query(open(HKLM, cv, 0x200), "ProgramFilesDir").1, "C:\\Program Files (x86)");
        // (the root's own view is the process's)
        let (ty, data, len) = (mem::heap_alloc(4, true), mem::heap_alloc(520, true), mem::heap_alloc(4, true));
        w32(len, 520);
        assert_eq!(call(RegGetValueA, &[HKLM, arg(cv), arg("ProgramFilesDir"), 0xffff, ty, data, len]), 0);
        assert_eq!(crate::text::a2s(&mem::cstr(data)), "C:\\Program Files (x86)");
        w32(len, 520);
        assert_eq!(call(RegGetValueA, &[HKLM, arg(cv), arg("ProgramFilesDir"), 0x1_ffff, ty, data, len]), 0);
        assert_eq!(crate::text::a2s(&mem::cstr(data)), "C:\\Program Files", "RRF_SUBKEY_WOW6464KEY");
    }

    #[test]
    fn special_folders_of_a_wow64_process() {
        let _g = enter();
        assert_eq!(special_folder(0x25), Some("C:\\windows\\system32"));
        assert_eq!(special_folder(0x29), Some("C:\\windows\\SysWOW64"));
        assert_eq!(special_folder(0x26), Some("C:\\Program Files (x86)"));
        assert_eq!(special_folder(0x2a), Some("C:\\Program Files (x86)"));
        assert_eq!(special_folder(0x2b), Some("C:\\Program Files (x86)\\Common Files"));
        assert_eq!(special_folder(0x2c), Some("C:\\Program Files (x86)\\Common Files"));
        let buf = mem::heap_alloc(520, true);
        assert_eq!(call(SHGetFolderPathA, &[0, 0x29, 0, 0, buf]), 0);
        assert_eq!(crate::text::a2s(&mem::cstr(buf)), "C:\\windows\\SysWOW64");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]
        /// wow64_key drops one Wow6432Node right under a redirected root and nothing else: the
        /// result never has it there, it is idempotent, and other paths are given back as they are.
        #[test]
        fn wow64_key_paths(
            root in prop_oneof![Just("hklm\\software"), Just("hkcr"), Just("hklm\\software\\classes"),
                Just("hkcu\\software\\classes"), Just("hkcu\\software"), Just("hklm\\system")],
            node in prop_oneof![Just("wow6432node"), Just("wow6432nodex"), Just("wow6432"), Just("x")],
            rest in prop::collection::vec("[a-z0-9 ]{1,8}", 0..3),
        ) {
            let tail: String = rest.iter().map(|r| format!("\\{r}")).collect();
            let p = format!("{root}\\{node}{tail}");
            let got = wow64_key(p.clone());
            let redirected = matches!(root, "hklm\\software" | "hkcr" | "hklm\\software\\classes" | "hkcu\\software\\classes");
            if redirected && node == "wow6432node" {
                prop_assert_eq!(&got, &format!("{root}{tail}"));
            } else {
                prop_assert_eq!(&got, &p);
            }
            prop_assert_eq!(wow64_key(got.clone()), got.clone());
        }
    }
}

/// SHFileOperation as installers use it (electron-builder's NSIS installer copies the unpacked
/// app with it, #183)
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod sh_file_op_tests {
    use super::*;
    use crate::test_guest::{call, enter as enter_mem, guest_thread};
    use proptest::prelude::*;

    fn enter() -> std::sync::MutexGuard<'static, ()> {
        let g = enter_mem();
        guest_thread();
        g
    }
    /// a double-NUL list
    fn list(items: &[&str]) -> u32 {
        let mut b: Vec<u8> = Vec::new();
        for i in items {
            b.extend(crate::text::s2a(i));
            b.push(0);
        }
        b.push(0);
        let p = mem::heap_alloc(b.len() as u32, true);
        mem::write(p, &b);
        p
    }
    fn op(func: u32, from: &[&str], to: &[&str], flags: u16) -> u32 {
        let s = mem::heap_alloc(30, true);
        w32(s + 4, func);
        w32(s + 8, list(from));
        w32(s + 12, if to.is_empty() { 0 } else { list(to) });
        mem::w16(s + 16, flags);
        w32(s + 18, 0xdead);
        let r = call(SHFileOperationA, &[s]);
        assert_eq!(r32(s + 18), 0, "fAnyOperationsAborted");
        r
    }
    fn put(w: &str, data: &str) {
        crate::files::make_dirs(w.rsplit_once('\\').unwrap().0);
        kura::fs::write(crate::files::resolve_new(w), data).unwrap();
    }
    fn get(w: &str) -> Option<String> {
        crate::files::resolve(w).and_then(|h| kura::fs::read(h).ok()).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    #[test]
    fn copy_move_delete_rename() {
        let _g = enter();
        let base = format!("C:\\shop-{}", std::process::id());
        let (src, dst) = (format!("{base}\\src"), format!("{base}\\dst"));
        put(&format!("{src}\\a.txt"), "a");
        put(&format!("{src}\\sub\\b.dat"), "b");
        put(&format!("{src}\\c.txt"), "c");
        // (NSIS CopyFiles "$PLUGINSDIR\app\*.*" "$INSTDIR": a missing destination folder is made)
        assert_eq!(op(2, &[&format!("{src}\\*.*")], &[&dst], 0x614), 0);
        assert_eq!(get(&format!("{dst}\\a.txt")).as_deref(), Some("a"));
        assert_eq!(get(&format!("{dst}\\sub\\b.dat")).as_deref(), Some("b"));
        // (FOF_FILESONLY: the wildcard takes no folders; FOF_NORECURSION copies none either)
        let d2 = format!("{base}\\files");
        assert_eq!(op(2, &[&format!("{src}\\*")], &[&d2], 0x80), 0);
        assert!(get(&format!("{d2}\\a.txt")).is_some() && get(&format!("{d2}\\sub\\b.dat")).is_none());
        // (one file to a missing name takes that name; into an existing folder, goes under it)
        assert_eq!(op(2, &[&format!("{src}\\a.txt")], &[&format!("{base}\\renamed.txt")], 0), 0);
        assert_eq!(get(&format!("{base}\\renamed.txt")).as_deref(), Some("a"));
        assert_eq!(op(2, &[&format!("{src}\\c.txt")], &[&d2], 0), 0);
        assert_eq!(get(&format!("{d2}\\c.txt")).as_deref(), Some("c"));
        // (a folder copied to a missing name is that folder)
        assert_eq!(op(2, &[&src], &[&format!("{base}\\copy")], 0), 0);
        assert_eq!(get(&format!("{base}\\copy\\sub\\b.dat")).as_deref(), Some("b"));
        // (move: gone from the source)
        assert_eq!(op(1, &[&format!("{base}\\renamed.txt")], &[&format!("{base}\\moved.txt")], 0), 0);
        assert!(get(&format!("{base}\\renamed.txt")).is_none());
        assert_eq!(get(&format!("{base}\\moved.txt")).as_deref(), Some("a"));
        // (rename)
        assert_eq!(op(4, &[&format!("{base}\\moved.txt")], &[&format!("{base}\\r.txt")], 0), 0);
        assert_eq!(get(&format!("{base}\\r.txt")).as_deref(), Some("a"));
        // (a missing source: ERROR_FILE_NOT_FOUND; an empty list DE_INVALIDFILES)
        assert_eq!(op(2, &[&format!("{base}\\nothing.txt")], &[&d2], 0), 2);
        assert_eq!(op(2, &[], &[&d2], 0), 0x7c);
        // (delete: folders with what is in them, wildcards)
        assert_eq!(op(3, &[&format!("{d2}\\*.txt")], &[], 0x14), 0);
        assert!(get(&format!("{d2}\\a.txt")).is_none() && crate::files::resolve(&d2).is_some());
        assert_eq!(op(3, &[&base], &[], 0x14), 0);
        assert!(crate::files::resolve(&base).is_none());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]
        /// wild_match agrees with the plain recursive definition ('*' any run, '?' one
        /// character), without regard to case
        #[test]
        fn wildcards_as_defined(pat in "[ab*?.]{0,6}", name in "[abAB.]{0,8}") {
            fn by_def(p: &[char], n: &[char]) -> bool {
                match p.first() {
                    None => n.is_empty(),
                    Some('*') => (0..=n.len()).any(|k| by_def(&p[1..], &n[k..])),
                    Some(&c) => !n.is_empty() && (c == '?' || c.eq_ignore_ascii_case(&n[0])) && by_def(&p[1..], &n[1..]),
                }
            }
            let (p, n): (Vec<char>, Vec<char>) = (pat.chars().collect(), name.chars().collect());
            let want = pat == "*.*" || by_def(&p, &n);
            prop_assert_eq!(wild_match(&pat, &name), want);
        }
    }
}

#[cfg(test)]
mod shell_folder_tests {
    use super::*;

    /// #162: every Explorer\Shell Folders name has a folder, the ones under the profile have a
    /// "User Shell Folders" spelling that expands back to it, and no name is listed twice
    #[test]
    fn shell_folders_table() {
        let vars = [("%USERPROFILE%", "C:\\users\\user"), ("%PUBLIC%", "C:\\users\\Public"), ("%ProgramData%", "C:\\ProgramData")];
        let mut seen = std::collections::HashSet::new();
        for &(name, csidl, user) in SHELL_FOLDERS {
            assert!(seen.insert((name.to_lowercase(), user)), "{name} twice");
            let p = special_folder(csidl).unwrap_or_else(|| panic!("{name}: no folder for csidl {csidl:#x}"));
            match shell_folder_unexpanded(p) {
                Some(u) => {
                    let (var, val) = vars.iter().find(|v| u.starts_with(v.0)).unwrap();
                    assert_eq!(format!("{val}{}", &u[var.len()..]), p);
                }
                None => assert_eq!(name, "Fonts"),
            }
            // (the user's folders are under the profile, the common ones aren't)
            assert_eq!(user && name != "Fonts", p.starts_with("C:\\users\\user"), "{name}: {p}");
        }
        assert_eq!(shell_folder_unexpanded("C:\\users\\username"), None);
        assert_eq!(shell_folder_unexpanded("C:\\users\\user").as_deref(), Some("%USERPROFILE%"));
    }
}

#[cfg(test)]
mod rstr_tests {
    use super::{ansi_chars, rstr_i};
    use proptest::prelude::*;

    fn up(u: u16) -> u16 {
        crate::text::upper16(u)
    }
    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// CoInitializeSecurity: once per process, after COM
    #[test]
    fn init_security_once() {
        use super::init_security_result;
        assert_eq!(init_security_result(false, false), 0x8004_01f0);
        assert_eq!(init_security_result(true, false), 0);
        assert_eq!(init_security_result(true, true), 0x8001_0119);
    }

    /// the rule as Wine's shlwapi has it (its tests of StrRStrIW pass on Windows)
    #[test]
    fn wine_cases() {
        let hay = w("abcabcABC");
        assert_eq!(rstr_i(&hay, hay.len(), &w("abc"), up), Some(6));
        assert_eq!(rstr_i(&hay, hay.len(), &w("ABC"), up), Some(6));
        assert_eq!(rstr_i(&hay, hay.len(), &w("x"), up), None);
        assert_eq!(rstr_i(&hay, hay.len(), &w(""), up), None);
        // (`last` at 6: a match may start at 5 and run past it; at 6 it starts at `last`)
        assert_eq!(rstr_i(&hay, 6, &w("abc"), up), Some(3));
        assert_eq!(rstr_i(&hay, 4, &w("abc"), up), Some(3));
        assert_eq!(rstr_i(&hay, 3, &w("abc"), up), Some(0));
        assert_eq!(rstr_i(&hay, 0, &w("abc"), up), None);
        // full-width and accented letters fold as CharUpper does
        assert_eq!(rstr_i(&w("ＡbＣ"), 3, &w("ａBｃ"), up), Some(0));
        assert_eq!(rstr_i(&w("Éé"), 2, &w("É"), up), Some(1));
    }

    /// A DBCS character is one character: its trail byte never starts a match
    #[test]
    fn dbcs_trail_bytes_do_not_match() {
        // "ア" = 83 41: the trail byte 'A' is not the letter A
        let (h, at) = ansi_chars(&[0x83, 0x41, b'a']);
        assert_eq!(at, vec![0, 2]);
        assert_eq!(rstr_i(&h, h.len(), &[b'A' as u16], |u| if (0x61..=0x7a).contains(&u) { u - 32 } else { u }), Some(1));
        let (h, _) = ansi_chars(&[0x83, 0x61]);
        let (n, _) = ansi_chars(&[0x83, 0x41]);
        assert_eq!(rstr_i(&h, h.len(), &n, |u| if (0x61..=0x7a).contains(&u) { u - 32 } else { u }), None);
    }

    proptest! {
        /// the answer is the model's: the greatest start i with i < last + len (i + len <= the
        /// end allowed) where the folded slices are equal
        #[test]
        fn matches_the_model(hay in "[aAbB\u{ff21}\u{ff41}]{0,24}", needle in "[aAbB\u{ff21}]{0,4}", last in 0usize..30) {
            let (h, n) = (w(&hay), w(&needle));
            let got = rstr_i(&h, last, &n, up);
            let want = if n.is_empty() { None } else {
                let l = last.min(h.len());
                let end = l + (n.len() - 1).min(h.len() - l);
                (0..h.len()).filter(|&i| i + n.len() <= end && (0..n.len()).all(|k| up(h[i + k]) == up(n[k]))).max()
            };
            prop_assert_eq!(got, want);
            if let Some(i) = got {
                prop_assert!(i < last.max(1) + n.len() && i + n.len() <= h.len());
            }
        }

        /// the characters of an ANSI string cover its bytes in order, each 1 or 2 long
        #[test]
        fn ansi_chars_cover_the_bytes(b in prop::collection::vec(1u8.., 0..40)) {
            let (chars, at) = ansi_chars(&b);
            prop_assert_eq!(chars.len(), at.len());
            for k in 0..at.len() {
                let next = if k + 1 < at.len() { at[k + 1] } else { b.len() };
                prop_assert!(next - at[k] == 1 || next - at[k] == 2);
            }
            prop_assert!(at.first().is_none_or(|&f| f == 0));
        }
    }
}

#[cfg(test)]
mod b08_tests {
    use super::*;
    use crate::test_guest::{call, enter};

    #[test]
    fn image_list_bitmaps_of_any_size() {
        assert_eq!(bitmap_rest(16, 16, 32), Some(1024));
        assert_eq!(bitmap_rest(3, 2, 8), Some(1024 + 8));
        assert_eq!(bitmap_rest(1, 1, 1), Some(8 + 4));
        // (65536 x 65536 x 32 bits wrapped to 0 in u32)
        assert_eq!(bitmap_rest(65536, 65536, 32), None);
        assert_eq!(bitmap_rest(u32::MAX, 1, 0xffff), None);
    }

    proptest::proptest! {
        #[test]
        fn bitmap_rest_is_the_exact_size(w in proptest::prelude::any::<u32>(), h in proptest::prelude::any::<u32>(), bpp in 0u32..0x10000) {
            let exact = if bpp <= 8 { 4u128 << bpp } else { 0 } + h as u128 * ((w as u128 * bpp as u128 + 31) / 32) * 4;
            match bitmap_rest(w, h, bpp) {
                Some(n) => proptest::prop_assert_eq!(n as u128, exact),
                None => proptest::prop_assert!(exact > 256 << 20),
            }
        }
    }

    #[test]
    fn bstr_lengths_past_a_bstr_are_null() {
        let _g = enter();
        // (n * 2 + 6 wrapped in u32: a 4-byte block, then n UTF-16 units read into a host Vec)
        for n in [0x8000_0000u32, 0xffff_fffd, u32::MAX, 0x3fff_fffd] {
            assert_eq!(call(SysAllocStringLen, &[0, n]), 0, "{n:#x}");
        }
        assert_eq!(call(SysAllocStringByteLen, &[0, u32::MAX - 2]), 0);
        let src = bstr_alloc(&[0x41, 0x42, 0x43]);
        let b = call(SysAllocStringLen, &[src, 2]);
        assert_eq!((r32(b - 4), r16(b), r16(b + 2), r16(b + 4)), (4, 0x41, 0x42, 0));
        // a failed SysReAllocStringLen leaves the old string
        let pb = mem::heap_alloc(4, true);
        w32(pb, b);
        assert_eq!(call(SysReAllocStringLen, &[pb, 0, u32::MAX]), 0);
        assert_eq!(r32(pb), b);
        assert_eq!(call(SysReAllocStringLen, &[pb, src, 3]), 1);
        assert_eq!(r32(r32(pb) - 4), 6);
        bstr_free(r32(pb));
        bstr_free(src);
        mem::heap_free(pb);
    }

    proptest::proptest! {
        /// Safe arrays of any bounds (counts up to 2^32 - 1 in up to 3 dimensions): either
        /// refused (NULL) or a data block holding every cell; never one smaller than its
        /// cells (the u32 product wrapped).
        #[test]
        fn safe_array_data_holds_every_cell(dims in proptest::collection::vec(proptest::prop_oneof![0u32..5, 0x8000u32..0x1_0001, proptest::prelude::any::<u32>()], 1..4), vt in proptest::prop_oneof![proptest::strategy::Just(2u32), proptest::strategy::Just(3), proptest::strategy::Just(5), proptest::strategy::Just(12)]) {
            // (between 64 MB and the limit the test's guest heap may run out before Windows'
            // would: not what this is about)
            let cells = dims.iter().fold(1u64, |a, &b| a.saturating_mul(b as u64));
            proptest::prop_assume!(cells.saturating_mul(16) <= 64 << 20 || cells > 0x7fff_0000);
            let _g = enter();
            let sab = mem::heap_alloc(8 * dims.len() as u32, true);
            for (i, &n) in dims.iter().enumerate() {
                w32(sab + 8 * i as u32, n);
            }
            let sa = call(safearray::SafeArrayCreate, &[vt, dims.len() as u32, sab]);
            if sa != 0 {
                let want = cells.saturating_mul(r32(sa + 4) as u64);
                let data = mem::rp(sa + 12);
                proptest::prop_assert!(mem::heap_size(data) as u64 >= want.max(1));
                call(safearray::SafeArrayDestroy, &[sa]);
            } else {
                proptest::prop_assert!(cells > 0x7fff_0000 / 16, "a small array refused: {:?}", dims);
            }
            mem::heap_free(sab);
        }
    }

    #[test]
    fn reals_that_are_no_numbers_as_strings() {
        assert_eq!(real_str(f64::INFINITY, 15), "1.#INF");
        assert_eq!(real_str(f64::NEG_INFINITY, 7), "-1.#INF");
        assert_eq!(real_str(f64::NAN, 15), "1.#QNAN");
        assert_eq!(real_str(-f64::NAN, 15), "-1.#IND");
        assert_eq!(real_str(1e20, 15), "1E+20");
        assert_eq!(real_str(0.5, 15), "0.5");
    }

    proptest::proptest! {
        /// Any double (bits: NaNs of every payload, infinities, subnormals) at any precision:
        /// no panic, and a finite one reads back within its digits.
        #[test]
        fn real_str_of_any_double(bits in proptest::prelude::any::<u64>(), digits in 1usize..18) {
            let f = f64::from_bits(bits);
            let t = real_str(f, digits);
            if f.is_finite() {
                let back: f64 = t.replace("E+", "e").replace("E-", "e-").parse().unwrap();
                let tol = f.abs() * 10f64.powi(1 - digits as i32) + f64::MIN_POSITIVE;
                proptest::prop_assert!((back - f).abs() <= tol, "{} -> {} -> {}", f, t, back);
            }
        }
    }

/// The SHA-1 sake had (everything kept, hashed at the end): the model the streaming one must match.
#[derive(Clone, Default)]
struct ModelSha1 {
    buf: Vec<u8>,
}
impl ModelSha1 {
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


    #[test]
    fn sha1_known_answers() {
        let one = |d: &[u8]| {
            let mut h = Sha1::default();
            h.update(d);
            h.finish().iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        assert_eq!(one(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(one(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(one(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }

    proptest::proptest! {
        /// Any data in any pieces, with digests taken between (CryptGetHashParam on a clone):
        /// the streaming SHA-1 answers what the kept-everything one did.
        #[test]
        fn sha1_streaming_is_the_model(pieces in proptest::collection::vec(proptest::collection::vec(proptest::prelude::any::<u8>(), 0..200), 0..12)) {
            let (mut h, mut m) = (Sha1::default(), ModelSha1::default());
            for p in &pieces {
                h.update(p);
                m.update(p);
                proptest::prop_assert_eq!(h.clone().finish(), m.clone().finish());
            }
            proptest::prop_assert_eq!(h.finish(), m.finish());
        }
    }
}
