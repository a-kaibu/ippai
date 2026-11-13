//! Get/SetThreadContext (and Wow64Get/SetThreadContext, NtGet/SetContextThread) of any thread
//! of this process: the calling one as if the call had returned; another one (suspended) as
//! it stands: in an API call as if that call had returned, else where it stopped. Only the
//! parts ContextFlags names are read or written (kernel32:thread test_GetThreadContext,
//! test_SetThreadContext).
//!
//! A context set for another thread is kept until that thread goes on: at its API call's
//! return, or where a suspended thread stood still (k32::park_suspended).

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::mem::{self, r32};
use crate::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The byte ranges of a CONTEXT each ContextFlags bit names (x86, then x64)
fn ranges(x64: bool, flags: u32) -> Vec<(u32, u32)> {
    let mut v = vec![];
    let mut add = |bit: u32, r: &[(u32, u32)]| {
        if flags & bit != 0 {
            v.extend_from_slice(r);
        }
    };
    if x64 {
        add(1, &[(0x38, 0x3a), (0x42, 0x48), (0x98, 0xa0), (0xf8, 0x100)]); // CONTROL: Cs, Ss EFlags, Rsp, Rip
        add(2, &[(0x78, 0x98), (0xa0, 0xf8)]); // INTEGER
        add(4, &[(0x3a, 0x42)]); // SEGMENTS: Ds Es Fs Gs
        add(8, &[(0x34, 0x38), (0x100, 0x300)]); // FLOATING_POINT: MxCsr, FltSave
        add(0x10, &[(0x48, 0x78)]); // DEBUG_REGISTERS
    } else {
        add(1, &[(0xb4, 0xcc)]); // CONTROL: Ebp Eip SegCs EFlags Esp SegSs
        add(2, &[(0x9c, 0xb4)]); // INTEGER
        add(4, &[(0x8c, 0x9c)]); // SEGMENTS
        add(8, &[(0x1c, 0x8c)]); // FLOATING_POINT
        add(0x10, &[(0x04, 0x1c)]); // DEBUG_REGISTERS
        add(0x20, &[(0xcc, 0x2cc)]); // EXTENDED_REGISTERS
    }
    v
}

fn size(x64: bool) -> u32 {
    if x64 { crate::seh64::CONTEXT_SIZE } else { crate::seh::CONTEXT_SIZE }
}
fn flags_at(x64: bool) -> u32 {
    if x64 { 0x30 } else { 0 }
}

/// The context of `cpu` into `ctx` (whole): `api` the stack pointer and pop of the API call it
/// is in, reported as returned
fn capture(cpu: &mut koji::Cpu, api: (u32, u32), ctx: u32) {
    let x64 = mem::x64();
    if x64 {
        crate::seh64::save_context(cpu, ctx);
        if api.0 != 0 {
            mem::w64(ctx + 0xf8, mem::r64(api.0));
            mem::w64(ctx + 0x98, api.0 as u64 + api.1 as u64);
        }
    } else {
        crate::seh::save_context(cpu, ctx);
        if api.0 != 0 {
            mem::w32(ctx + 0xb8, r32(api.0));
            mem::w32(ctx + 0xc4, api.0 + api.1);
        }
    }
}

/// Copies the parts of `src` that `flags` names into `dst`
fn overlay(dst: u32, src: u32, flags: u32) {
    for (a, b) in ranges(mem::x64(), flags) {
        mem::copy(dst + a, src + a, b - a);
    }
}

/// Loads a whole context into `cpu`
fn load(cpu: &mut koji::Cpu, ctx: u32) {
    let x64 = mem::x64();
    // (all of it: the parts not set are the thread's own, captured)
    mem::w32(ctx + flags_at(x64), if x64 { 0x10_001f } else { 0x1003f });
    if x64 {
        crate::seh64::load_context(cpu, ctx);
    } else {
        crate::seh::load_context(cpu, ctx);
    }
}

/// Room for a context below `esp` (the guest's stack below its pointer is free)
fn scratch(esp: u32) -> u32 {
    ((esp - 0x400) & !15) - size(mem::x64())
}

static ANY: AtomicBool = AtomicBool::new(false);
/// The contexts set for other threads, by thread id: (ContextFlags, the CONTEXT's bytes)
static PENDING: Mutex<Option<HashMap<u32, (u32, Vec<u8>)>>> = Mutex::new(None);

/// The calling thread goes on: a context another thread set for it is loaded now (`esp`: its
/// stack pointer, for the scratch)
pub fn apply_pending(cpu: &mut koji::Cpu) {
    if !ANY.load(Ordering::Acquire) {
        return;
    }
    let tid = crate::cur().tid;
    let Some((flags, bytes)) = PENDING.lock().unwrap().get_or_insert_with(HashMap::default).remove(&tid) else { return };
    let esp = cpu.gpr(4) as u32;
    let base = scratch(esp);
    let given = base - size(mem::x64()) - 16;
    capture(cpu, (0, 0), base);
    mem::write(given, &bytes);
    overlay(base, given, flags);
    load(cpu, base);
}

fn thread_of(h: u32) -> Option<u32> {
    crate::k32::any_thread_tid(h)
}

fn GetThreadContext(c: &mut Ctx) -> u32 {
    let (h, ctx) = (c.a(0), c.a(1));
    let Some(tid) = thread_of(h) else {
        crate::k32::last_error(6);
        return 0;
    };
    let x64 = mem::x64();
    let flags = r32(ctx + flags_at(x64));
    let base = scratch(c.esp);
    if tid == crate::cur().tid {
        let pop = if x64 { 8 } else { 12 };
        let esp = c.esp;
        capture(c.cpu, (esp, pop), base);
    } else {
        let Some((cpu, api)) = crate::k32::thread_cpu(tid) else {
            crate::k32::last_error(6);
            return 0;
        };
        capture(unsafe { &mut *cpu }, api, base);
    }
    overlay(ctx, base, flags);
    1
}

fn SetThreadContext(c: &mut Ctx) -> u32 {
    let (h, ctx) = (c.a(0), c.a(1));
    let Some(tid) = thread_of(h) else {
        crate::k32::last_error(6);
        return 0;
    };
    let x64 = mem::x64();
    let flags = r32(ctx + flags_at(x64));
    if tid == crate::cur().tid {
        // (the caller goes on in the context: as if the call had returned there)
        let base = scratch(c.esp);
        let pop = if x64 { 8 } else { 12 };
        let esp = c.esp;
        capture(c.cpu, (esp, pop), base);
        overlay(base, ctx, flags);
        load(c.cpu, base);
        c.jumped = true;
        return 1;
    }
    let bytes = mem::slice(ctx, size(x64)).to_vec();
    PENDING.lock().unwrap().get_or_insert_with(HashMap::default).insert(tid, (flags, bytes));
    ANY.store(true, Ordering::Release);
    1
}

/// The x86 descriptor of selector `sel` in thread `teb` (32-bit Windows' GDT: code 0x23,
/// data 0x2b, the TEB 0x53; index 0 is the null one): an LDT_ENTRY, or the error (an index
/// beyond: ERROR_NOACCESS; a value above 16 bits: ERROR_INVALID_THREAD_ID)
pub fn selector_entry(sel: u32, teb: u32) -> Result<[u8; 8], u32> {
    if sel > 0xffff {
        return Err(1444);
    }
    // (base, limit, type with the S bit, granularity)
    let (base, limit, ty, g) = match sel & !7 {
        0 => return Ok([0; 8]),
        0x20 => (0, 0xfffff, 0x1b, 1),
        0x28 => (0, 0xfffff, 0x13, 1),
        0x50 => (teb, 0xfff, 0x13, 0),
        _ => return Err(998),
    };
    let flags1 = ty | 3 << 5 | 1 << 7; // Type (accessed), Dpl 3, Pres
    let flags2 = (limit >> 16) as u8 | 1 << 6 | (g << 7) as u8; // LimitHi, Default_Big, Granularity
    Ok([limit as u8, (limit >> 8) as u8, base as u8, (base >> 8) as u8, (base >> 16) as u8, flags1 as u8, flags2, (base >> 24) as u8])
}

/// GetThreadSelectorEntry(thread, selector, &entry): x86 only (x64: ERROR_NOT_SUPPORTED)
fn GetThreadSelectorEntry(c: &mut Ctx) -> u32 {
    let (h, sel, out) = (c.a(0), c.a(1), c.a(2));
    if mem::x64() {
        crate::k32::last_error(50);
        return 0;
    }
    let Some(tid) = thread_of(h) else {
        crate::k32::last_error(6);
        return 0;
    };
    let teb = if tid == crate::cur().tid { crate::cur().teb } else { crate::k32::teb_of(tid).unwrap_or(0) };
    match selector_entry(sel, teb) {
        Ok(e) => {
            mem::write(out, &e);
            1
        }
        Err(e) => {
            crate::k32::last_error(e);
            0
        }
    }
}

/// NtGetContextThread / NtSetContextThread: the same, NTSTATUS
fn NtGetContextThread(c: &mut Ctx) -> u32 {
    if GetThreadContext(c) != 0 { 0 } else { 0xC000_0008 }
}
fn NtSetContextThread(c: &mut Ctx) -> u32 {
    if SetThreadContext(c) != 0 { 0 } else { 0xC000_0008 }
}

pub fn init() {
    for dll in ["kernel32", "kernelbase"] {
        implement(dll, "GetThreadContext", GetThreadContext);
        implement(dll, "SetThreadContext", SetThreadContext);
        implement(dll, "GetThreadSelectorEntry", GetThreadSelectorEntry);
    }
    for n in ["NtGetContextThread", "ZwGetContextThread"] {
        implement("ntdll", n, NtGetContextThread);
    }
    for n in ["NtSetContextThread", "ZwSetContextThread"] {
        implement("ntdll", n, NtSetContextThread);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The descriptors kernel32:thread test_GetThreadSelectorEntry reads from Windows
    #[test]
    fn selectors() {
        let fs = selector_entry(0x53, 0x7fec_0000).unwrap();
        assert_eq!(fs, [0xff, 0x0f, 0, 0, 0xec, 0xf3, 0x40, 0x7f]);
        let cs = selector_entry(0x23, 0).unwrap();
        assert_eq!(cs, [0xff, 0xff, 0, 0, 0, 0xfb, 0xcf, 0]);
        let ds = selector_entry(0x2b, 0).unwrap();
        assert_eq!(ds[5] & 0x1e, 0x12, "data, read/write");
        assert_eq!(selector_entry(0, 0), Ok([0; 8]));
        assert_eq!(selector_entry(3, 0), Ok([0; 8]));
        assert_eq!(selector_entry(0xdeadbeef, 0), Err(1444));
        assert_eq!(selector_entry(0x12b, 0), Err(998));
    }

    /// The parts never overlap, stay inside the CONTEXT, and all of them leave out only
    /// ContextFlags (and on x64 the home registers before it).
    #[test]
    fn context_parts() {
        for x64 in [false, true] {
            let mut all: Vec<(u32, u32)> = ranges(x64, 0xff);
            all.sort();
            for w in all.windows(2) {
                assert!(w[0].1 <= w[1].0, "{x64}: {:?} overlaps {:?}", w[0], w[1]);
            }
            assert!(all.iter().all(|&(a, b)| a < b && b <= size(x64)));
            let first = all[0].0;
            assert_eq!(first, if x64 { 0x34 } else { 4 });
            assert!(!all.iter().any(|&(a, b)| a <= flags_at(x64) && flags_at(x64) < b));
        }
    }
}
