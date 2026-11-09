//! msiexec.exe: a small program sake writes to C:\Windows\System32 (a PE that calls
//! msi!ippai_msiexec and exits with what it returns), so that installers and uninstall
//! strings that run "msiexec /i package.msi" or "MsiExec.exe /X{code}" start a process of
//! their own as on Windows. ippai_msiexec reads the command line:
//!   /i /package <msi>    install          /x /uninstall <msi | product code>  remove
//!   /qn /qb /qr /qf /quiet /passive        the UI level ("+", "-", "!" after it ignored)
//!   /norestart /promptrestart /forcerestart /l... <file>   accepted
//!   NAME=value ...       properties, passed on

use crate::api::{implement, Ctx};
use crate::msi_install::{install_product, set_ui_level, INSTALLUILEVEL_BASIC, INSTALLUILEVEL_FULL, INSTALLUILEVEL_NONE, INSTALLUILEVEL_REDUCED};

pub const PATH: &str = "C:\\Windows\\System32\\msiexec.exe";

/// The image: PE32, one section at RVA 0x1000 with the code and the imports of
/// msi!ippai_msiexec and kernel32!ExitProcess.
pub fn image() -> Vec<u8> {
    const BASE: u32 = 0x40_0000;
    const RVA: u32 = 0x1000;
    let mut f = vec![0u8; 0x400];
    let w16 = |f: &mut Vec<u8>, at: usize, v: u16| f[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let w32 = |f: &mut Vec<u8>, at: usize, v: u32| f[at..at + 4].copy_from_slice(&v.to_le_bytes());
    // DOS header, PE signature, file header
    f[0..2].copy_from_slice(b"MZ");
    w32(&mut f, 0x3c, 0x40);
    f[0x40..0x44].copy_from_slice(b"PE\0\0");
    w16(&mut f, 0x44, 0x14c);
    w16(&mut f, 0x46, 1);
    w16(&mut f, 0x54, 0xe0);
    w16(&mut f, 0x56, 0x0102);
    // optional header
    let o = 0x58;
    w16(&mut f, o, 0x10b);
    w32(&mut f, o + 4, 0x200);
    w32(&mut f, o + 16, RVA);
    w32(&mut f, o + 20, RVA);
    w32(&mut f, o + 28, BASE);
    w32(&mut f, o + 32, 0x1000);
    w32(&mut f, o + 36, 0x200);
    w16(&mut f, o + 40, 4);
    w16(&mut f, o + 48, 4);
    w32(&mut f, o + 56, 0x2000);
    w32(&mut f, o + 60, 0x200);
    w16(&mut f, o + 68, 2); // GUI: no console of its own
    w32(&mut f, o + 72, 0x10_0000);
    w32(&mut f, o + 76, 0x1000);
    w32(&mut f, o + 80, 0x10_0000);
    w32(&mut f, o + 84, 0x1000);
    w32(&mut f, o + 92, 16);
    // data directories: imports, IAT
    w32(&mut f, o + 96 + 8, RVA + 0x40);
    w32(&mut f, o + 96 + 12, 60);
    w32(&mut f, o + 96 + 96, RVA + 0x90);
    w32(&mut f, o + 96 + 100, 16);
    // section header
    let s = o + 0xe0;
    f[s..s + 5].copy_from_slice(b".text");
    w32(&mut f, s + 8, 0x200);
    w32(&mut f, s + 12, RVA);
    w32(&mut f, s + 16, 0x200);
    w32(&mut f, s + 20, 0x200);
    w32(&mut f, s + 36, 0xe000_0020);
    // the section (file offset 0x200 = RVA 0x1000)
    let t = 0x200usize;
    let iat_msi = BASE + RVA + 0x90;
    let iat_k32 = BASE + RVA + 0x98;
    // call [msi!ippai_msiexec]; push eax; call [kernel32!ExitProcess]; ret
    let mut code = vec![0xff, 0x15];
    code.extend(iat_msi.to_le_bytes());
    code.push(0x50);
    code.extend([0xff, 0x15]);
    code.extend(iat_k32.to_le_bytes());
    code.push(0xc3);
    f[t..t + code.len()].copy_from_slice(&code);
    // import descriptors (ILT, name, IAT), the null one last
    let desc = |f: &mut Vec<u8>, at: usize, ilt: u32, name: u32, iat: u32| {
        f[at..at + 4].copy_from_slice(&ilt.to_le_bytes());
        f[at + 12..at + 16].copy_from_slice(&name.to_le_bytes());
        f[at + 16..at + 20].copy_from_slice(&iat.to_le_bytes());
    };
    desc(&mut f, t + 0x40, RVA + 0x80, RVA + 0xe0, RVA + 0x90);
    desc(&mut f, t + 0x54, RVA + 0x88, RVA + 0xf0, RVA + 0x98);
    for (thunk, name) in [(0x80, 0xa0u32), (0x88, 0xc0), (0x90, 0xa0), (0x98, 0xc0)] {
        w32(&mut f, t + thunk, RVA + name);
    }
    f[t + 0xa2..t + 0xa2 + 13].copy_from_slice(b"ippai_msiexec");
    f[t + 0xc2..t + 0xc2 + 11].copy_from_slice(b"ExitProcess");
    f[t + 0xe0..t + 0xe7].copy_from_slice(b"msi.dll");
    f[t + 0xf0..t + 0xfc].copy_from_slice(b"kernel32.dll");
    f
}

/// Writes the program when it is not there (a file of the same name is left alone).
pub fn install() {
    let host = crate::files::resolve_new(PATH);
    if kura::fs::metadata(&host).is_ok() {
        return;
    }
    if let Some((d, _)) = host.rsplit_once('/') {
        let _ = kura::fs::create_dir_all(d);
    }
    let _ = kura::fs::write(&host, image());
}

/// The words of a command line: quotes group (and are dropped), "NAME=\"a b\"" stays one.
pub fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What a command line asks for: (remove, package or product code, UI level, properties).
pub fn parse(args: &str) -> Option<(bool, String, u32, String)> {
    let w = words(args);
    let mut remove = false;
    let mut target = None;
    let mut level = INSTALLUILEVEL_FULL;
    let mut props = Vec::new();
    let mut i = 0;
    while i < w.len() {
        let a = &w[i];
        let opt = a.strip_prefix('/').or_else(|| a.strip_prefix('-'));
        match opt.map(|o| o.to_ascii_lowercase()) {
            Some(o) if o == "i" || o == "package" || o == "x" || o == "uninstall" => {
                remove = o.starts_with('x') || o == "uninstall";
                i += 1;
                target = Some(w.get(i)?.clone());
            }
            // "/x{code}" and "/i{code}" written together
            Some(o) if (o.starts_with("x{") || o.starts_with("i{")) => {
                remove = o.starts_with('x');
                target = Some(a[2..].to_string());
            }
            Some(o) if o.starts_with('q') => {
                level = match o.trim_end_matches(['+', '-', '!']) {
                    "q" | "qn" => INSTALLUILEVEL_NONE,
                    "qb" => INSTALLUILEVEL_BASIC,
                    "qr" => INSTALLUILEVEL_REDUCED,
                    "qf" => INSTALLUILEVEL_FULL,
                    _ => INSTALLUILEVEL_NONE,
                };
            }
            Some(o) if o == "quiet" => level = INSTALLUILEVEL_NONE,
            Some(o) if o == "passive" => level = INSTALLUILEVEL_BASIC,
            Some(o) if o.starts_with('l') => i += 1,
            Some(_) => {}
            None => props.push(if a.contains(' ') { a.replacen('=', "=\"", 1) + "\"" } else { a.clone() }),
        }
        i += 1;
    }
    Some((remove, target?, level, props.join(" ")))
}

/// msi!ippai_msiexec: the command line run; its result is the exit code.
fn ippai_msiexec(c: &mut Ctx) -> u32 {
    let line = crate::k32::arg_str(crate::k32::cmdline().1, true);
    let args = crate::process::split_command(&line).1;
    let Some((remove, target, level, props)) = parse(&args) else { return 1639 }; // ERROR_INVALID_COMMAND_LINE
    set_ui_level(level);
    if remove {
        if target.starts_with('{') {
            return crate::msi_install::configure(c, &target, crate::msi_pkg::INSTALLSTATE_ABSENT, &props);
        }
        return install_product(c, &target, &format!("{props} REMOVE=ALL"));
    }
    if target.starts_with('{') {
        return crate::msi_install::configure(c, &target, crate::msi_pkg::INSTALLSTATE_DEFAULT, &props);
    }
    install_product(c, &target, &props)
}

pub fn init() {
    implement("msi", "ippai_msiexec", ippai_msiexec);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines() {
        assert_eq!(parse("/i \"C:\\a b\\x.msi\" /qn FOO=1"), Some((false, "C:\\a b\\x.msi".into(), INSTALLUILEVEL_NONE, "FOO=1".into())));
        assert_eq!(parse("/X{6A0F2E9B-5C1D-4E7A-9B3C-1D2E3F405162}"), Some((true, "{6A0F2E9B-5C1D-4E7A-9B3C-1D2E3F405162}".into(), INSTALLUILEVEL_FULL, String::new())));
        assert_eq!(parse("/x x.msi /qb!- /norestart"), Some((true, "x.msi".into(), INSTALLUILEVEL_BASIC, String::new())));
        assert_eq!(parse("/package x.msi /passive /l*v log.txt INSTALLDIR=\"C:\\My Game\""), Some((false, "x.msi".into(), INSTALLUILEVEL_BASIC, "INSTALLDIR=\"C:\\My Game\"".into())));
        assert_eq!(parse("/qn"), None);
        assert_eq!(parse("/i"), None);
    }

    #[test]
    fn image_parses() {
        let f = image();
        let pe = &f[0x40..];
        assert_eq!(&pe[..4], b"PE\0\0");
        // (the imports' names where the descriptors point)
        let name = |rva: usize| {
            let off = rva - 0x1000 + 0x200;
            f[off..].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect::<String>()
        };
        let d = 0x200 + 0x40;
        let rva = |at: usize| u32::from_le_bytes(f[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(name(rva(d + 12)), "msi.dll");
        assert_eq!(name(rva(d + 20 + 12)), "kernel32.dll");
        let at = |r: usize| r - 0x1000 + 0x200;
        assert_eq!(name(rva(at(rva(d))) + 2), "ippai_msiexec");
        assert_eq!(name(rva(at(rva(d + 20))) + 2), "ExitProcess");
        // (the code calls through the IATs: ImageBase + their RVAs)
        assert_eq!(&f[0x200..0x202], &[0xff, 0x15]);
        assert_eq!(rva(0x202), 0x40_0000 + rva(d + 16));
        assert_eq!(rva(d + 40), 0);
    }
}
