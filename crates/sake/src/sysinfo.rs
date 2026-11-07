//! What a hardware tool asks the system about the machine besides CPUID: ntdll's
//! NtQuerySystemInformation (basic, processor, per-processor times, code integrity), the
//! firmware tables (an SMBIOS describing sake's virtual machine; no ACPI tables) and the
//! installed memory. The machine matches GetSystemInfo / GlobalMemoryStatusEx: ncpu()
//! processors, PHYS bytes of memory.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::k32::{last_error, ncpu, ticks_ms, PHYS};
use crate::mem::{self, put32, w32};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

const STATUS_INVALID_INFO_CLASS: u32 = 0xc000_0003;
const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xc000_0004;

fn mask() -> u64 {
    (1u64 << ncpu()) - 1
}

/// The bytes of one information class, or None for a class sake does not answer. `ptr` is the
/// guest's pointer size.
fn system_information(class: u32, ptr: u32) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    let p = |v: &mut Vec<u8>, x: u64| v.extend_from_slice(&x.to_le_bytes()[..ptr as usize]);
    let u = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    match class {
        // SYSTEM_BASIC_INFORMATION
        0 => {
            u(&mut v, 0);
            u(&mut v, 156_250);
            u(&mut v, 0x1000);
            u(&mut v, (PHYS >> 12) as u32);
            u(&mut v, 1);
            u(&mut v, (PHYS >> 12) as u32 - 1);
            u(&mut v, 0x10000);
            if ptr == 8 {
                u(&mut v, 0);
            }
            p(&mut v, 0x10000);
            p(&mut v, if ptr == 8 { 0x7fff_fffe_ffff } else { 0x7ffe_ffff });
            p(&mut v, mask());
            v.push(ncpu() as u8);
            while v.len() % ptr as usize != 0 {
                v.push(0);
            }
        }
        // SYSTEM_PROCESSOR_INFORMATION: architecture, level, revision (as GetSystemInfo), the
        // processor count, feature bits
        1 => {
            v.extend_from_slice(&(if ptr == 8 { 9u16 } else { 0 }).to_le_bytes());
            v.extend_from_slice(&6u16.to_le_bytes());
            v.extend_from_slice(&0x0f01u16.to_le_bytes());
            v.extend_from_slice(&(ncpu() as u16).to_le_bytes());
            u(&mut v, 0);
        }
        // SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION per processor, in 100 ns units: idle,
        // kernel (idle included, as Windows counts it), user, DPC, interrupt, interrupts. The
        // processors share the elapsed time: half idle, a quarter user, a quarter kernel.
        8 => {
            let t = ticks_ms() as u64 * 10_000;
            for _ in 0..ncpu() {
                for x in [t / 2, t / 2 + t / 4, t / 4, 0, 0] {
                    v.extend_from_slice(&x.to_le_bytes());
                }
                u(&mut v, ticks_ms());
                u(&mut v, 0);
            }
        }
        // SYSTEM_CODEINTEGRITY_INFORMATION: enabled, nothing else (no test signing, no HVCI)
        0x67 => {
            u(&mut v, 8);
            u(&mut v, 1);
        }
        _ => return None,
    }
    Some(v)
}

/// NtQuerySystemInformation(class, buffer, length, return_length*)
fn NtQuerySystemInformation(c: &mut Ctx) -> u32 {
    let (class, buf, len, ret) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let ptr = if mem::x64() { 8 } else { 4 };
    let Some(data) = system_information(class, ptr) else {
        // (the process list and performance counters: k32.rs, from the running threads)
        let st = crate::k32::query_system_processes(c);
        if st == STATUS_INVALID_INFO_CLASS && crate::env_on!("SAKE_TRACE_SYSINFO") {
            eprintln!("sake: NtQuerySystemInformation class {class:#x} not answered");
        }
        return st;
    };
    let n = data.len() as u32;
    // (WoW64 gives a 32-bit program's per-processor times as many whole entries as fit, winref
    // sysinfo-32)
    if class == 8 && !mem::x64() && len < n && len >= 48 {
        let k = len / 48 * 48;
        put32(ret, k);
        mem::write(buf, &data[..k as usize]);
        return 0;
    }
    put32(ret, n);
    // fixed-size classes want the exact size; the processor's (1) and the per-processor one (8)
    // at least theirs (winref sysinfo)
    if len < n || (!matches!(class, 1 | 8) && len != n) {
        return STATUS_INFO_LENGTH_MISMATCH;
    }
    mem::write(buf, &data);
    0
}
fn ZwQuerySystemInformation(c: &mut Ctx) -> u32 {
    NtQuerySystemInformation(c)
}

// ---------------------------------------------------------------------------------------------
// firmware tables

const RSMB: u32 = u32::from_be_bytes(*b"RSMB");

/// One SMBIOS structure: the formatted part (type, its length, a handle, `body`) and the
/// strings it refers to by number
fn structure(out: &mut Vec<u8>, ty: u8, handle: u16, body: &[u8], strings: &[&str]) {
    out.push(ty);
    out.push(4 + body.len() as u8);
    out.extend_from_slice(&handle.to_le_bytes());
    out.extend_from_slice(body);
    for s in strings {
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    }
    if strings.is_empty() {
        out.push(0);
    }
    out.push(0);
}

/// SMBIOS 3.0 tables of sake's virtual machine: BIOS (0), system (1), baseboard (2), memory
/// device (17), end (127)
fn smbios() -> Vec<u8> {
    let mut t = Vec::new();
    // BIOS: vendor 1, version 2, start segment, date 3, ROM size, characteristics (BIOS
    // characteristics not supported), extension bytes, release 1.0
    let mut b = vec![1, 2, 0x00, 0xe0, 3, 0];
    b.extend_from_slice(&8u64.to_le_bytes());
    b.extend_from_slice(&[0, 0, 1, 0, 0xff, 0xff]);
    structure(&mut t, 0, 0, &b, &["vnvm", "sake", "11/07/2026"]);
    // system: manufacturer 1, product 2, version 3, serial 0, UUID, wake-up type (power switch),
    // SKU 0, family 0
    let mut b = vec![1, 2, 3, 0];
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(&[6, 0, 0]);
    structure(&mut t, 1, 1, &b, &["vnvm", "sake virtual machine", "1.0"]);
    // baseboard: manufacturer 1, product 2, version 3, serial 0, asset 0, features (hosting
    // board), location 0, chassis handle, type (motherboard), no contained objects
    let b = [1, 2, 3, 0, 0, 1, 0, 3, 0, 10, 0];
    structure(&mut t, 2, 2, &b, &["vnvm", "sake virtual board", "1.0"]);
    // memory device: one module of PHYS bytes, DDR4 DIMM, 64-bit, speed unknown
    let mb = (PHYS >> 20) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(&0xfffeu16.to_le_bytes()); // physical memory array: none described
    b.extend_from_slice(&0xfffeu16.to_le_bytes()); // error information: none
    b.extend_from_slice(&64u16.to_le_bytes()); // total width
    b.extend_from_slice(&64u16.to_le_bytes()); // data width
    b.extend_from_slice(&0x7fffu16.to_le_bytes()); // size: in the extended size field
    b.extend_from_slice(&[9, 0, 1, 0, 0x1a]); // DIMM, not in a set, locator 1, bank 0, DDR4
    b.extend_from_slice(&0x80u16.to_le_bytes()); // type detail: synchronous
    b.extend_from_slice(&0u16.to_le_bytes()); // speed unknown
    b.extend_from_slice(&[2, 0, 0, 0, 0]); // manufacturer 2, serial, asset, part, attributes
    b.extend_from_slice(&mb.to_le_bytes()); // extended size in MB
    b.extend_from_slice(&0u16.to_le_bytes()); // configured speed
    structure(&mut t, 17, 17, &b, &["DIMM 0", "vnvm"]);
    structure(&mut t, 127, 127, &[], &[]);
    t
}
fn firmware_table(provider: u32, id: u32) -> Option<Vec<u8>> {
    // (RSMB has the one table, whatever the id: winref sysinfo)
    let _ = id;
    if provider != RSMB {
        return None;
    }
    let t = smbios();
    // RawSMBIOSData: calling method, major, minor, DMI revision, the table's length
    let mut v = vec![0, 3, 0, 0];
    v.extend_from_slice(&(t.len() as u32).to_le_bytes());
    v.extend_from_slice(&t);
    Some(v)
}
/// GetSystemFirmwareTable(provider, id, buffer, size): the bytes written, or the size needed
/// when it is bigger than `size` (nothing written), 0 for a table that is not there
fn GetSystemFirmwareTable(c: &mut Ctx) -> u32 {
    let (provider, id, buf, size) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(t) = firmware_table(provider, id) else {
        last_error(1168); // ERROR_NOT_FOUND
        return 0;
    };
    let n = t.len() as u32;
    if size < n || buf == 0 {
        last_error(122);
        return n;
    }
    mem::write(buf, &t);
    n
}
/// EnumSystemFirmwareTables(provider, buffer, size): the ids, as DWORDs (RSMB has the one, 0)
fn EnumSystemFirmwareTables(c: &mut Ctx) -> u32 {
    let (provider, buf, size) = (c.a(0), c.a(1), c.a(2));
    if provider != RSMB {
        return 0;
    }
    if size >= 4 && buf != 0 {
        w32(buf, 0);
    }
    4
}
/// GetPhysicallyInstalledSystemMemory(KB*)
fn GetPhysicallyInstalledSystemMemory(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        last_error(87);
        return 0;
    }
    let kb = PHYS >> 10;
    w32(p, kb as u32);
    w32(p + 4, (kb >> 32) as u32);
    1
}
// ---------------------------------------------------------------------------------------------
// power schemes (powrprof): Windows' three, in the registry's order, Balanced active

/// (GUID, name, description)
const SCHEMES: [([u8; 16], &str, &str); 3] = [
    // {381b4222-f694-41f0-9685-ff5bb260df2e}
    (
        [0x22, 0x42, 0x1b, 0x38, 0x94, 0xf6, 0xf0, 0x41, 0x96, 0x85, 0xff, 0x5b, 0xb2, 0x60, 0xdf, 0x2e],
        "Balanced",
        "Automatically balances performance with energy consumption on capable hardware.",
    ),
    // {8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c}
    (
        [0xda, 0x7f, 0x5e, 0x8c, 0xbf, 0xe8, 0x96, 0x4a, 0x9a, 0x85, 0xa6, 0xe2, 0x3a, 0x8c, 0x63, 0x5c],
        "High performance",
        "Favors performance, but may use more energy.",
    ),
    // {a1841308-3541-4fab-bc81-f71556f20b4a}
    (
        [0x08, 0x13, 0x84, 0xa1, 0x41, 0x35, 0xab, 0x4f, 0xbc, 0x81, 0xf7, 0x15, 0x56, 0xf2, 0x0b, 0x4a],
        "Power saver",
        "Saves energy by reducing your computer's performance where possible.",
    ),
];
const ERROR_MORE_DATA: u32 = 234;
const ERROR_NO_MORE_ITEMS: u32 = 259;
const ERROR_FILE_NOT_FOUND: u32 = 2;

/// PowerEnumerate(root, scheme, subgroup, access, index, buffer, size*): the schemes (access
/// ACCESS_SCHEME 16 with no scheme given); subgroups and settings are not described
fn PowerEnumerate(c: &mut Ctx) -> u32 {
    let (scheme, access, index, buf, sizep) = (c.a(1), c.a(3), c.a(4), c.a(5), c.a(6));
    if access != 16 || scheme != 0 || index as usize >= SCHEMES.len() {
        return ERROR_NO_MORE_ITEMS;
    }
    if buf == 0 {
        // (the size query is ERROR_MORE_DATA, winref sysinfo)
        put32(sizep, 16);
        return ERROR_MORE_DATA;
    }
    if sizep == 0 || mem::r32(sizep) < 16 {
        put32(sizep, 16);
        return ERROR_MORE_DATA;
    }
    mem::write(buf, &SCHEMES[index as usize].0);
    w32(sizep, 16);
    0
}
/// PowerGetActiveScheme(root, GUID**): a LocalAlloc'ed GUID
fn PowerGetActiveScheme(c: &mut Ctx) -> u32 {
    let g = mem::heap_alloc(16, false);
    mem::write(g, &SCHEMES[0].0);
    mem::putp(c.a(1), g);
    0
}
/// the scheme's name or description into (buffer, size*) as UTF-16 with its NUL, sizes in bytes
fn power_text(c: &mut Ctx, description: bool) -> u32 {
    let (scheme, subgroup, setting, buf, sizep) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if scheme == 0 || subgroup != 0 || setting != 0 {
        return ERROR_FILE_NOT_FOUND;
    }
    let mut g = [0u8; 16];
    for (i, b) in g.iter_mut().enumerate() {
        *b = mem::r8(scheme + i as u32);
    }
    let Some(s) = SCHEMES.iter().find(|s| s.0 == g) else {
        return ERROR_FILE_NOT_FOUND;
    };
    let text = if description { s.2 } else { s.1 };
    let w: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let n = 2 * w.len() as u32;
    if buf == 0 {
        put32(sizep, n);
        return 0;
    }
    if sizep == 0 || mem::r32(sizep) < n {
        put32(sizep, n);
        return ERROR_MORE_DATA;
    }
    for (i, ch) in w.iter().enumerate() {
        mem::w16(buf + 2 * i as u32, *ch);
    }
    w32(sizep, n);
    0
}
fn PowerReadFriendlyName(c: &mut Ctx) -> u32 {
    power_text(c, false)
}
fn PowerReadDescription(c: &mut Ctx) -> u32 {
    power_text(c, true)
}
/// the Windows XP scheme calls: no legacy schemes to list or report (FALSE)
fn EnumPwrSchemes(_: &mut Ctx) -> u32 {
    last_error(ERROR_FILE_NOT_FOUND);
    0
}
fn GetActivePwrScheme(_: &mut Ctx) -> u32 {
    last_error(ERROR_FILE_NOT_FOUND);
    0
}

pub fn init() {
    reg!("ntdll"; NtQuerySystemInformation, ZwQuerySystemInformation);
    reg!("kernel32"; GetSystemFirmwareTable, EnumSystemFirmwareTables, GetPhysicallyInstalledSystemMemory);
    reg!("powrprof"; PowerEnumerate, PowerGetActiveScheme, PowerReadFriendlyName, PowerReadDescription, EnumPwrSchemes, GetActivePwrScheme);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smbios_structures_walk_to_the_end() {
        let t = smbios();
        let mut o = 0;
        let mut types = Vec::new();
        while o < t.len() {
            types.push(t[o]);
            let mut s = o + t[o + 1] as usize;
            // the strings end at a double NUL
            while !(t[s] == 0 && t[s + 1] == 0) {
                s += 1;
            }
            o = s + 2;
        }
        assert_eq!(o, t.len());
        assert_eq!(types, [0, 1, 2, 17, 127]);
    }

    #[test]
    fn basic_information_sizes() {
        assert_eq!(system_information(0, 4).unwrap().len(), 44);
        assert_eq!(system_information(0, 8).unwrap().len(), 64);
        assert_eq!(system_information(1, 4).unwrap().len(), 12);
        assert_eq!(system_information(0x67, 4).unwrap(), [8, 0, 0, 0, 1, 0, 0, 0]);
    }
}
