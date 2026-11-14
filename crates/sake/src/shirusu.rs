//! SAKE_SHIRUSU=1: the guest's calls to a fixed set of Win32 APIs as lines on stderr, for a
//! shirusu Trace (github.com/a-kaibu/shirusu-poc's public Trace format) that tools/shirusu
//! builds from them and compares with a shirusu capture of the same game on real Windows
//! (.github/workflows/shirusu.yml, docs/shirusu.md).
//!
//! One line when a call goes in, one when it comes back (a call that never comes back, such as
//! ExitThread or a handler that jumps, has none: the Trace says it ended unknown):
//!
//!   shirusu E <tid> <call> <ms> <api> <arg,...|-> [s=<utf16 hex>] [d=<bytes hex>]
//!   shirusu L <tid> <call> <ms> <ret> <last error> [o=<u32>] [d=<bytes hex>]
//!
//! all numbers hex but <ms>; <call> counts per thread; <api> as shirusu's catalog names it
//! (kernel32!CreateFileW). The extras are what shirusu's capture reads of memory, within its
//! bounds (1024 UTF-16 units, 4096 bytes): CreateFileW's path, WriteFile's buffer before the
//! call, ReadFile's count and buffer after it, WriteFile's count after it.
//!
//! Only these APIs: the same set the Windows capture records (`--mode filtered --api`, the
//! list in tools/shirusu/apis.txt).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// (dll as shirusu's catalog names it, function): tools/shirusu/apis.txt
pub fn apis() -> &'static [(String, String)] {
    static A: OnceLock<Vec<(String, String)>> = OnceLock::new();
    A.get_or_init(|| {
        include_str!("../../../tools/shirusu/apis.txt")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .filter_map(|l| l.trim().split_once('!').map(|(d, f)| (d.to_string(), f.to_string())))
            .collect()
    })
}

const MAX_UNITS: u32 = 1024;
const MAX_BYTES: u32 = 4096;

pub fn on() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var("SAKE_SHIRUSU").is_ok_and(|v| v == "1"))
}

/// The catalog name of the API `name` of `dll` (sake's DLL key), when it is one of `apis()`.
/// (A-variants and the W ones sake implements under kernelbase or another host DLL count as
/// the DLL programs import them from.)
pub fn api_name(dll: &str, name: &str) -> Option<String> {
    let host = match dll {
        "kernelbase" | "kernel32" => "kernel32",
        "advapi32" => "advapi32",
        d => d,
    };
    apis().iter().find(|(d, f)| d == host && f == name).map(|(d, f)| format!("{d}!{f}"))
}

/// Per trap index: the catalog name, or None for an API not recorded.
fn name_of(i: u32) -> Option<String> {
    static M: Mutex<Option<HashMap<u32, Option<String>>>> = Mutex::new(None);
    let mut g = M.lock().unwrap();
    g.get_or_insert_with(HashMap::default)
        .entry(i)
        .or_insert_with(|| trap_api(crate::api::TRAP_LO + 4 * i))
        .clone()
}

/// The catalog name of the API at trap address `a` (api::name_of is "dll!name")
fn trap_api(a: u32) -> Option<String> {
    let full = crate::api::name_of(a)?;
    let (dll, name) = full.split_once('!')?;
    api_name(dll, name)
}

thread_local! {
    static NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// A call going in: what its leave line needs.
pub struct Open {
    call: u64,
    func: String,
    /// (ReadFile: buffer, capacity, count's address; WriteFile: count's address)
    buf: u32,
    cap: u32,
    count_at: u32,
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(2 * b.len());
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// At most `n` readable bytes from `a`.
fn bytes(a: u32, n: u32) -> Vec<u8> {
    if a == 0 {
        return Vec::new();
    }
    let n = crate::mem::accessible(a, n.min(MAX_BYTES), false, true);
    crate::mem::slice(a, n).to_vec()
}

/// A NUL-ended UTF-16 string from `a`, at most MAX_UNITS (+1: shirusu marks one longer cut),
/// as bytes; None when `a` is NULL or unreadable.
fn wide(a: u32) -> Option<Vec<u8>> {
    if a == 0 || crate::mem::accessible(a, 2, false, true) < 2 {
        return None;
    }
    let mut v = Vec::new();
    for k in 0..=MAX_UNITS {
        let p = a + 2 * k;
        if crate::mem::accessible(p, 2, false, true) < 2 {
            break;
        }
        let c = crate::mem::r16(p);
        if c == 0 {
            v.extend_from_slice(&[0, 0]);
            break;
        }
        v.extend_from_slice(&c.to_le_bytes());
    }
    Some(v)
}

fn ms() -> u32 {
    crate::k32::ticks_ms().wrapping_sub(60_000)
}

/// The enter line of a call to trap index `i` (arguments `args`), if its API is recorded.
pub fn enter(i: u32, args: &[u64]) -> Option<Open> {
    let api = name_of(i)?;
    let call = NEXT.with(|n| {
        let c = n.get();
        n.set(c + 1);
        c
    });
    let func = api.split_once('!')?.1.to_string();
    let a = |k: usize| args.get(k).copied().unwrap_or(0);
    let mut open = Open { call, func: func.clone(), buf: 0, cap: 0, count_at: 0 };
    let (mut s, mut d) = (None, None);
    match func.as_str() {
        "CreateFileW" => s = wide(a(0) as u32),
        "WriteFile" => {
            d = Some(bytes(a(1) as u32, a(2) as u32));
            open.count_at = a(3) as u32;
        }
        "ReadFile" => {
            open.buf = a(1) as u32;
            open.cap = a(2) as u32;
            open.count_at = a(3) as u32;
        }
        _ => {}
    }
    eprintln!("{}", enter_line(crate::cur().tid, call, ms(), &api, args, s.as_deref(), d.as_deref()));
    Some(open)
}

/// `shirusu E ...` (tools/shirusu's parse reads it)
fn enter_line(tid: u32, call: u64, ms: u32, api: &str, args: &[u64], s: Option<&[u8]>, d: Option<&[u8]>) -> String {
    let list: Vec<String> = args.iter().map(|v| format!("{v:x}")).collect();
    // ("-": no arguments, so the extras that follow are not taken for them)
    let list = if list.is_empty() { "-".to_string() } else { list.join(",") };
    let mut line = format!("shirusu E {tid:x} {call:x} {ms} {api} {list}");
    if let Some(s) = s {
        line += &format!(" s={}", hex(s));
    }
    if let Some(d) = d {
        line += &format!(" d={}", hex(d));
    }
    line
}

/// `shirusu L ...`
fn leave_line(tid: u32, call: u64, ms: u32, ret: u64, err: u32, o: Option<u32>, d: Option<&[u8]>) -> String {
    let mut line = format!("shirusu L {tid:x} {call:x} {ms} {ret:x} {err:x}");
    if let Some(n) = o {
        line += &format!(" o={n:x}");
    }
    if let Some(d) = d {
        line += &format!(" d={}", hex(d));
    }
    line
}

/// The leave line: the call's result (RAX/EAX; with EDX:EAX for a 64-bit result) and the
/// thread's last error after it; the count at the call's count pointer, ReadFile's buffer.
pub fn leave(o: Open, ret: u64) {
    let err = crate::k32::get_last_error();
    let count = (o.count_at != 0 && crate::mem::accessible(o.count_at, 4, false, true) == 4).then(|| crate::mem::r32(o.count_at));
    let d = (o.func == "ReadFile" && ret as u32 != 0).then_some(count).flatten().map(|n| bytes(o.buf, n.min(o.cap)));
    eprintln!("{}", leave_line(crate::cur().tid, o.call, ms(), ret, err, count, d.as_deref()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the names programs import, as the catalog has them; others are not recorded
    #[test]
    fn names() {
        assert_eq!(api_name("kernel32", "CreateFileW").as_deref(), Some("kernel32!CreateFileW"));
        assert_eq!(api_name("kernelbase", "ReadFile").as_deref(), Some("kernel32!ReadFile"));
        assert_eq!(api_name("advapi32", "RegOpenKeyExA").as_deref(), Some("advapi32!RegOpenKeyExA"));
        assert_eq!(api_name("d3d9", "Direct3DCreate9").as_deref(), Some("d3d9!Direct3DCreate9"));
        assert_eq!(api_name("kernel32", "GetTickCount"), None);
        assert_eq!(api_name("user32", "CreateFileW"), None);
    }

    /// the traps programs call land on their catalog names (the trap table's names are
    /// "dll!name": taken for the bare name, nothing was ever recorded)
    #[test]
    fn traps() {
        fn a(d: &str, n: &str) -> Option<String> {
            trap_api(crate::api::export(d, n))
        }
        assert_eq!(a("kernel32.dll", "CreateFileW").as_deref(), Some("kernel32!CreateFileW"));
        assert_eq!(a("KERNEL32", "ReadFile").as_deref(), Some("kernel32!ReadFile"));
        assert_eq!(a("kernelbase.dll", "WriteFile").as_deref(), Some("kernel32!WriteFile"));
        assert_eq!(a("advapi32.dll", "RegQueryValueExW").as_deref(), Some("advapi32!RegQueryValueExW"));
        assert_eq!(a("user32.dll", "CreateWindowExW").as_deref(), Some("user32!CreateWindowExW"));
        assert_eq!(a("kernel32.dll", "GetTickCount"), None);
        // every API of the list, through the table
        for (d, n) in apis() {
            assert_eq!(a(&format!("{d}.dll"), n), Some(format!("{d}!{n}")), "{d}!{n}");
        }
        assert_eq!(trap_api(0), None);
    }

    /// no API twice (the converter keys the Trace's API set by name)
    #[test]
    fn unique() {
        let mut v: Vec<_> = apis().to_vec();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), apis().len());
        assert!(v.len() > 40);
    }

    /// the lines as tools/shirusu's tests/convert.rs `sake_lines` reads them (same strings)
    #[test]
    fn lines() {
        let path: Vec<u8> = "a.dat\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(
            enter_line(0x20, 1, 1500, "kernel32!CreateFileW", &[0x401000, 0x80000000, 1, 0, 3, 0x80, 0], Some(&path), None),
            "shirusu E 20 1 1500 kernel32!CreateFileW 401000,80000000,1,0,3,80,0 s=61002e006400610074000000"
        );
        assert_eq!(enter_line(0x24, 2, 7, "kernel32!GetLogicalDrives", &[], None, None), "shirusu E 24 2 7 kernel32!GetLogicalDrives -");
        assert_eq!(leave_line(0x20, 1, 1501, 0xffff_ffff, 2, None, None), "shirusu L 20 1 1501 ffffffff 2");
        assert_eq!(leave_line(0x20, 3, 9, 1, 0, Some(4), Some(&[1, 2, 0xab, 4])), "shirusu L 20 3 9 1 0 o=4 d=0102ab04");
    }

    #[test]
    fn hex_bytes() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(hex(&[]), "");
    }
}
