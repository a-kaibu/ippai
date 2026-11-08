//! x64 exception handling: table-based, as ntdll's RtlDispatchException / RtlUnwindEx /
//! RtlVirtualUnwind do it. Each image's exception directory (.pdata) lists its functions'
//! RUNTIME_FUNCTIONs; their UNWIND_INFO says how to undo the prolog and which language handler
//! (__C_specific_handler, __CxxFrameHandler3/4, __GSHandlerCheck...) the function has.
//!
//! The dispatch loop walks frames in Rust and calls each language handler as guest code that
//! returns to SEH64_TRAP (no host frame stays behind: a handler that catches calls RtlUnwindEx,
//! which jumps to the target). A catch block is the unwind's "consolidation" callback (the
//! C++ runtime's), called the same way and returning to CONSOLIDATE_TRAP.

#![allow(non_snake_case)]

use std::cell::RefCell;

use koji::Cpu;

use crate::api::{implement, Ctx};
use crate::mem::{r16, r32, r64, r8, w32, w64};

pub const CONTEXT_SIZE: u32 = 0x4d0;
const RECORD_SIZE: u32 = 0x98;
const DISP_SIZE: u32 = 0x50;

const UNW_FLAG_EHANDLER: u32 = 1;
const UNW_FLAG_UHANDLER: u32 = 2;
const UNW_FLAG_CHAININFO: u32 = 4;

const EXCEPTION_UNWINDING: u32 = 2;
const EXCEPTION_EXIT_UNWIND: u32 = 4;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const STATUS_UNWIND: u32 = 0xc000_0027;
const STATUS_LONGJUMP: u32 = 0x8000_0026;
const STATUS_UNWIND_CONSOLIDATE: u32 = 0x8000_0029;

/// A catch block (the consolidation callback) returns here.
pub const CONSOLIDATE_TRAP: u32 = 0xffff_0040;
/// An unwind's call of a frame's handler returns here (`unwind_go`).
pub const UNWIND_TRAP: u32 = 0xffff_0050;

fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("SAKE_TRACE_SEH").is_some())
}

// ---------------------------------------------------------------------------------------------
// CONTEXT (x64): GPRs in encoding order at 0x78, RIP 0xf8, EFlags 0x44, MxCsr 0x34, the
// FXSAVE area at 0x100 (XMM0-15 at 0x1a0)

fn reg(ctx: u32, i: usize) -> u64 {
    r64(ctx + 0x78 + 8 * i as u32)
}
fn set_reg(ctx: u32, i: usize, v: u64) {
    w64(ctx + 0x78 + 8 * i as u32, v)
}
fn rsp(ctx: u32) -> u64 {
    reg(ctx, 4)
}
fn rip(ctx: u32) -> u64 {
    r64(ctx + 0xf8)
}
fn set_rip(ctx: u32, v: u64) {
    w64(ctx + 0xf8, v)
}

pub fn save_context(cpu: &mut Cpu, ctx: u32) {
    crate::mem::fill(ctx, CONTEXT_SIZE, 0);
    w32(ctx + 0x30, 0x10_001f); // CONTEXT_FULL | SEGMENTS | DEBUG... (AMD64)
    w32(ctx + 0x34, cpu.mxcsr);
    for (o, v) in [(0x38u32, 0x33u16), (0x3a, 0x2b), (0x3c, 0x2b), (0x3e, 0x53), (0x40, 0x2b), (0x42, 0x2b)] {
        crate::mem::w16(ctx + o, v);
    }
    w32(ctx + 0x44, cpu.flags());
    for i in 0..16 {
        set_reg(ctx, i, cpu.gpr(i));
    }
    set_rip(ctx, cpu.eip as u64 | (cpu.rip_hi as u64) << 32);
    let _ = koji::fxsave(cpu, &mut crate::seh::Guest, ctx + 0x100);
    for i in 0..16 {
        let x = cpu.xmm(i);
        w64(ctx + 0x1a0 + 16 * i as u32, x as u64);
        w64(ctx + 0x1a8 + 16 * i as u32, (x >> 64) as u64);
    }
}

pub fn load_context(cpu: &mut Cpu, ctx: u32) {
    for i in 0..16 {
        cpu.set_gpr(i, reg(ctx, i));
    }
    let ip = rip(ctx);
    cpu.eip = ip as u32;
    cpu.rip_hi = (ip >> 32) as u32;
    cpu.eflags = r32(ctx + 0x44) | 2;
    cpu.lazy.op = 0;
    if r32(ctx + 0x30) & 8 != 0 {
        let _ = koji::fxrstor(cpu, &mut crate::seh::Guest, ctx + 0x100);
        for i in 0..16 {
            let lo = r64(ctx + 0x1a0 + 16 * i as u32) as u128;
            let hi = r64(ctx + 0x1a8 + 16 * i as u32) as u128;
            cpu.set_xmm(i, lo | hi << 64);
        }
        cpu.mxcsr = r32(ctx + 0x34);
    }
}

// ---------------------------------------------------------------------------------------------
// function tables

/// Dynamic function tables (RtlAddFunctionTable): (table, count, base).
static DYNAMIC: std::sync::Mutex<Vec<(u32, u32, u32)>> = std::sync::Mutex::new(Vec::new());

/// The RUNTIME_FUNCTION covering `pc` and its image's base: (entry, base).
pub fn lookup(pc: u32) -> Option<(u32, u32)> {
    let found = crate::pe::MODULES.read().unwrap().iter().find(|m| pc.wrapping_sub(m.base) < m.size).map(|m| (m.base, m.pdata));
    let mut tables: Vec<(u32, u32, u32)> = Vec::new();
    if let Some((base, (t, n))) = found {
        tables.push((t, n, base));
    }
    tables.extend(DYNAMIC.lock().unwrap().iter().copied());
    for (t, n, base) in tables {
        let rva = pc.wrapping_sub(base);
        let (mut lo, mut hi) = (0u32, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let e = t + 12 * mid;
            if rva < r32(e) {
                hi = mid;
            } else if rva >= r32(e + 4) {
                lo = mid + 1;
            } else {
                return Some((e, base));
            }
        }
    }
    None
}

/// RtlVirtualUnwind: undoes the prolog of the function `f` (at `pc`) in `ctx`. Returns the
/// language handler of kind `kind` (UNW_FLAG_E/UHANDLER) if it has one, with its data, and
/// the frame (establisher) pointer.
fn virtual_unwind(kind: u32, base: u32, pc: u64, f: u32, ctx: u32) -> (u64, u32, u64) {
    let mut f = f;
    let func_start = base.wrapping_add(r32(f));
    let mut frame = rsp(ctx);
    let mut first = true;
    let mut mach = false;
    let mut handler = (0u64, 0u32);
    loop {
        let info = base + r32(f + 8);
        let (vf, prolog, count, fr) = (r8(info) as u32, r8(info + 1) as u32, r8(info + 2) as u32, r8(info + 3) as u32);
        let flags = vf >> 3;
        let (freg, foff) = ((fr & 15) as usize, fr >> 4);
        // inside the prolog: only the codes for the instructions already executed
        let off = (pc as u32).wrapping_sub(base.wrapping_add(r32(f)));
        let in_prolog = first && off < prolog;
        if first && freg != 0 {
            // the frame register is set up once its SET_FPREG code has run
            let mut set = !in_prolog;
            let mut i = 0;
            while i < count {
                let c = r16(info + 4 + 2 * i);
                if c >> 8 & 15 == 3 && (c & 0xff) as u32 <= off {
                    set = true;
                }
                i += op_slots(c);
            }
            if set {
                frame = reg(ctx, freg).wrapping_sub(foff as u64 * 16);
            }
        }
        let mut i = 0;
        while i < count {
            let c = r16(info + 4 + 2 * i);
            let (at, op, opi) = ((c & 0xff) as u32, (c >> 8 & 15) as u32, (c >> 12) as usize);
            let next = |k: u32| r16(info + 4 + 2 * (i + k)) as u64;
            let n = op_slots(c);
            if in_prolog && at > off {
                i += n;
                continue;
            }
            let sp = rsp(ctx);
            match op {
                0 => {
                    // PUSH_NONVOL
                    set_reg(ctx, opi, r64(sp as u32));
                    set_reg(ctx, 4, sp + 8);
                }
                1 => {
                    // ALLOC_LARGE
                    let size = if opi == 0 { next(1) * 8 } else { next(1) | next(2) << 16 };
                    set_reg(ctx, 4, sp + size);
                }
                2 => set_reg(ctx, 4, sp + (opi as u64 + 1) * 8), // ALLOC_SMALL
                3 => set_reg(ctx, 4, frame),                     // SET_FPREG
                4 => set_reg(ctx, opi, r64((frame + next(1) * 8) as u32)), // SAVE_NONVOL
                5 => set_reg(ctx, opi, r64((frame + (next(1) | next(2) << 16)) as u32)), // SAVE_NONVOL_FAR
                8 | 9 => {
                    // SAVE_XMM128(_FAR)
                    let a = if op == 8 { frame + next(1) * 16 } else { frame + (next(1) | next(2) << 16) };
                    let x = ctx + 0x1a0 + 16 * opi as u32;
                    w64(x, r64(a as u32));
                    w64(x + 8, r64(a as u32 + 8));
                }
                10 => {
                    // PUSH_MACHFRAME
                    let s = sp + if opi != 0 { 8 } else { 0 };
                    set_rip(ctx, r64(s as u32));
                    set_reg(ctx, 4, r64(s as u32 + 24));
                    mach = true;
                }
                _ => {}
            }
            i += n;
        }
        if first && flags & (UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER) & kind != 0 && flags & UNW_FLAG_CHAININFO == 0 {
            let h = info + 4 + 2 * ((count + 1) & !1);
            handler = (crate::mem::widen(base + r32(h)), h + 4);
        }
        if flags & UNW_FLAG_CHAININFO == 0 {
            break;
        }
        f = info + 4 + 2 * ((count + 1) & !1);
        first = false;
    }
    let _ = func_start;
    if !mach {
        let sp = rsp(ctx);
        set_rip(ctx, r64(sp as u32));
        set_reg(ctx, 4, sp + 8);
    }
    (handler.0, handler.1, frame)
}

/// Number of 16-bit slots an unwind code takes.
fn op_slots(c: u16) -> u32 {
    match (c >> 8 & 15, c >> 12) {
        (1, 0) => 2,
        (1, _) => 3,
        (4, _) => 2,
        (5, _) => 3,
        (8, _) => 2,
        (9, _) => 3,
        _ => 1,
    }
}

/// One frame up: (handler, data, frame, entry, base) with `ctx` unwound to the caller. A leaf
/// function (no entry) just returns. None: no more frames (a host frame or garbage).
/// `seen`: dispatcher frames passed so far (a handler's return to SEH64_TRAP leads to the
/// context its exception happened in, the n-th dispatch from the top).
fn step(cpu: &mut Cpu, kind: u32, ctx: u32, seen: &mut usize, rec: u32) -> Option<(u64, u32, u64, u32, u32)> {
    let pc = rip(ctx);
    if pc == crate::api::SEH64_TRAP as u64 {
        let sp = rsp(ctx);
        let d = STACK.with(|s| {
            let s = s.borrow();
            s.len().checked_sub(*seen + 1).map(|i| s[i])
        })?;
        *seen += 1;
        crate::mem::copy(ctx, d.orig, CONTEXT_SIZE);
        return Some((0, 0, sp, 0, 0));
    }
    if pc == CONSOLIDATE_TRAP as u64 {
        // a catch block's caller: the frame it was called for (the C++ runtime notes the
        // exception leaving the catch)
        let sp = rsp(ctx);
        crate::cxx64::through_catch(cpu, sp as u32, kind == UNW_FLAG_UHANDLER, rec);
        crate::mem::copy(ctx, consolidate_saved(sp as u32), CONTEXT_SIZE);
        return Some((0, 0, sp, 0, 0));
    }
    if pc == crate::cxx64::TRANSLATE_TRAP as u64 {
        // an SE translator's caller: the frame handler that called it, whose caller is the
        // dispatcher (SEH64_TRAP, next)
        let sp = rsp(ctx);
        let entry = sp - 8 + crate::cxx64::TRANSLATE_FRAME as u64;
        set_rip(ctx, r64(entry as u32));
        set_reg(ctx, 4, entry + 8);
        return Some((0, 0, sp, 0, 0));
    }
    if pc == UNWIND_TRAP as u64 {
        let sp = rsp(ctx);
        if let Some(orig) = UNWINDS.with(|u| u.borrow().iter().rev().find(|e| e.handler_sp == sp).map(|e| e.orig)) {
            crate::mem::copy(ctx, orig, CONTEXT_SIZE);
            return Some((0, 0, sp, 0, 0));
        }
    }
    if (pc as u32) >= crate::api::TRAP_LO && pc >> 32 == 0 {
        return None;
    }
    match lookup(pc as u32) {
        Some((f, base)) => {
            let (h, d, frame) = virtual_unwind(kind, base, pc, f, ctx);
            Some((h, d, frame, f, base))
        }
        None => {
            let sp = rsp(ctx);
            if !(crate::mem::LO as u64..crate::mem::HI as u64).contains(&sp) {
                return None;
            }
            set_rip(ctx, r64(sp as u32));
            set_reg(ctx, 4, sp + 8);
            Some((0, 0, sp, 0, 0))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// dispatch

/// A dispatch in progress on this thread: guest memory blocks (below the faulting RSP).
#[derive(Clone, Copy)]
struct Dispatch {
    rec: u32,
    /// the context the exception happened in
    orig: u32,
    /// the frame being looked at (before its unwind), and the one above
    cur: u32,
    next: u32,
    disp: u32,
    /// stack bounds: frames outside are not walked
    limit: u64,
    /// a C++ exception an SE translator threw (the walk passed TRANSLATE_TRAP): the frame
    /// whose handler called the translator. As vcruntime140 does, only that frame looks for a
    /// catch; past it the C++ exception is dropped and the structured exception goes on to the
    /// frames above (whose handlers call the translator again). 0: none
    guard: u64,
}

thread_local! {
    static STACK: RefCell<Vec<Dispatch>> = const { RefCell::new(Vec::new()) };
    /// Unwinds in progress on this thread (each waiting for a handler's return to UNWIND_TRAP).
    /// An exception raised in a handler goes on past it to the frames above RtlUnwindEx's
    /// caller (libgcc's cleanups raise STATUS_GCC_UNWIND from their unwind handler, for the
    /// frame being unwound to catch); an unwind that one starts leaves this one behind.
    static UNWINDS: RefCell<Vec<Unwind>> = const { RefCell::new(Vec::new()) };
}

/// No exception dispatch or unwind under way on this thread (save states: snap.rs).
pub fn idle() -> bool {
    STACK.with(|s| s.borrow().is_empty()) && UNWINDS.with(|u| u.borrow().is_empty())
}

/// Raises exception `code` at `addr` with `cpu` as the context, and starts the dispatch: the
/// next guest code to run is the first handler (or the context again).
pub fn raise(cpu: &mut Cpu, code: u32, flags: u32, addr: u32, params: &[u64]) {
    let a = cpu.eip as u64 | (cpu.rip_hi as u64) << 32;
    let a = if a as u32 == addr { a } else { crate::mem::widen(addr) };
    let sp = cpu.gpr(4) as u32;
    let rec = ((sp - 0x200) & !15) - RECORD_SIZE;
    crate::mem::fill(rec, RECORD_SIZE, 0);
    w32(rec, code);
    w32(rec + 4, flags);
    w64(rec + 0x10, a);
    w32(rec + 0x18, params.len().min(15) as u32);
    for (i, p) in params.iter().take(15).enumerate() {
        w64(rec + 0x20 + 8 * i as u32, *p);
    }
    let orig = (rec - CONTEXT_SIZE) & !15;
    save_context(cpu, orig);
    dispatch(cpu, rec, orig);
}

/// Dispatches the exception `rec` that happened in context `orig` (both in guest memory, which
/// stays live until the dispatch is over).
fn dispatch(cpu: &mut Cpu, rec: u32, orig: u32) {
    if trace() {
        crate::api::dump_ring("the exception");
        eprintln!(
            "sake: x64 exception {:08x} at {:#x} params {:x} {:x} {:x} (rsp {:#x})",
            r32(rec),
            r64(rec + 0x10),
            r64(rec + 0x20),
            r64(rec + 0x28),
            r64(rec + 0x30),
            rsp(orig)
        );
    }
    // vectored handlers first: EXCEPTION_CONTINUE_EXECUTION (-1) resumes the context
    let veh = crate::seh::veh_list();
    if !veh.is_empty() {
        let ptrs = (orig - 0x20) & !15;
        w64(ptrs, rec as u64);
        w64(ptrs + 8, orig as u64);
        cpu.set_gpr(4, ptrs as u64 - 0x40);
        for f in veh {
            if crate::call(cpu, f, &[ptrs]) == 0xffff_ffff {
                crate::seh::continue_handlers(cpu, ptrs);
                load_context(cpu, orig);
                return;
            }
        }
    }
    let cur = (orig - 0x40 - CONTEXT_SIZE) & !15;
    let next = cur - CONTEXT_SIZE;
    let disp = (next - DISP_SIZE) & !15;
    crate::mem::copy(cur, orig, CONTEXT_SIZE);
    let teb = crate::cur().teb;
    let limit = r64(teb + 8);
    STACK.with(|s| s.borrow_mut().push(Dispatch { rec, orig, cur, next, disp, limit, guard: 0 }));
    walk(cpu);
}

/// Goes up the frames of the topmost dispatch until one has a handler to call (it is called:
/// SEH64_TRAP continues here) or none is left (unhandled).
fn walk(cpu: &mut Cpu) {
    let Some(mut d) = STACK.with(|s| s.borrow().last().copied()) else {
        crate::fatal(cpu, "x64 exception walk without a dispatch");
    };
    loop {
        crate::mem::copy(d.next, d.cur, CONTEXT_SIZE);
        let pc = rip(d.cur);
        if pc == crate::cxx64::TRANSLATE_TRAP as u64 && d.guard == 0 {
            // (cxx64::translate's frame: the translator returns here, the frame argument above)
            d.guard = r64(rsp(d.cur) as u32 - 8 + 0x48);
            STACK.with(|s| s.borrow_mut().last_mut().unwrap().guard = d.guard);
        }
        let Some((h, data, frame, f, base)) = step(cpu, UNW_FLAG_EHANDLER, d.next, &mut 1, d.rec) else {
            if trace() {
                eprintln!("sake: x64 walk: no frame above pc {pc:#x}");
            }
            break;
        };
        if frame >= d.limit || rsp(d.next) > d.limit {
            if trace() {
                eprintln!("sake: x64 walk: frame {frame:#x} (pc {pc:#x}, next rsp {:#x}) past the stack base {:#x}", rsp(d.next), d.limit);
            }
            break;
        }
        if d.guard != 0 && frame > d.guard {
            translation_dropped(cpu, d);
            return;
        }
        if h != 0 && guarded(d.guard, frame) {
            if trace() {
                eprintln!("sake: x64 frame {frame:#x} (pc {pc:#x}): handler {h:#x}");
            }
            // DISPATCHER_CONTEXT
            crate::mem::fill(d.disp, DISP_SIZE, 0);
            w64(d.disp, pc);
            w64(d.disp + 8, crate::mem::widen(base));
            w64(d.disp + 0x10, f as u64);
            w64(d.disp + 0x18, frame);
            w64(d.disp + 0x28, d.cur as u64);
            w64(d.disp + 0x30, h);
            w64(d.disp + 0x38, data as u64);
            // the handler's call: (record, frame, context, dispatcher context) -> SEH64_TRAP
            let sp = ((d.disp - 0x40) & !15) - 8;
            w64(sp, crate::api::SEH64_TRAP as u64);
            cpu.set_gpr(4, sp as u64);
            cpu.set_gpr(1, d.rec as u64);
            cpu.set_gpr(2, frame);
            cpu.set_gpr(8, d.orig as u64);
            cpu.set_gpr(9, d.disp as u64);
            cpu.eip = h as u32;
            cpu.rip_hi = (h >> 32) as u32;
            // (the frame to go on with when it says "continue search")
            return;
        }
        if trace() {
            eprintln!("sake: x64 walk: pc {pc:#x} -> {:#x} rsp {:#x} (frame {frame:#x})", rip(d.next), rsp(d.next));
        }
        crate::mem::copy(d.cur, d.next, CONTEXT_SIZE);
    }
    unhandled(cpu, d);
}

/// Whether a frame's handler is called for the dispatch's C++ exception: any, or (an SE
/// translator's) only the frame that translated it (`guard`).
fn guarded(guard: u64, frame: u64) -> bool {
    guard == 0 || frame == guard
}

/// The C++ exception `d` an SE translator threw found no catch in the translating frame: it
/// is destroyed and the structured exception goes on past that frame, as if its handler had
/// said "continue search".
fn translation_dropped(cpu: &mut Cpu, d: Dispatch) {
    if trace() {
        eprintln!("sake: x64 translated exception not caught in frame {:#x}: dropped", d.guard);
    }
    crate::cxx64::destroy(cpu, d.rec);
    STACK.with(|s| s.borrow_mut().pop());
    let Some(o) = STACK.with(|s| s.borrow().last().copied()) else {
        crate::fatal(cpu, "x64 translated exception without the structured one");
    };
    crate::mem::copy(o.cur, o.next, CONTEXT_SIZE);
    walk(cpu);
}

/// SEH64_TRAP: a language handler returned its verdict (EAX).
pub fn handler_done(cpu: &mut Cpu) {
    let verdict = cpu.regs[0];
    let Some(d) = STACK.with(|s| s.borrow().last().copied()) else {
        crate::fatal(cpu, "x64 handler return without a dispatch");
    };
    if trace() {
        eprintln!("sake: x64 handler says {verdict}");
    }
    match verdict {
        0 => {
            // ExceptionContinueExecution
            STACK.with(|s| s.borrow_mut().pop());
            load_context(cpu, d.orig);
        }
        _ => {
            // ExceptionContinueSearch (nested/collided: the same)
            crate::mem::copy(d.cur, d.next, CONTEXT_SIZE);
            walk(cpu);
        }
    }
}

fn unhandled(cpu: &mut Cpu, d: Dispatch) {
    STACK.with(|s| s.borrow_mut().pop());
    let filter = crate::seh::TOP_FILTER.load(std::sync::atomic::Ordering::Relaxed);
    if filter != 0 {
        let ptrs = (d.disp - 0x20) & !15;
        w64(ptrs, d.rec as u64);
        w64(ptrs + 8, d.orig as u64);
        cpu.set_gpr(4, ptrs as u64 - 0x40);
        if crate::call(cpu, filter, &[ptrs]) == 0xffff_ffff {
            load_context(cpu, d.orig);
            return;
        }
    }
    let msg = format!(
        "unhandled x64 exception {:08x} at {:#x} (params {:x} {:x}), rsp {:#x}",
        r32(d.rec),
        r64(d.rec + 0x10),
        r64(d.rec + 0x20),
        r64(d.rec + 0x28),
        rsp(d.orig)
    );
    load_context(cpu, d.orig);
    crate::fatal(cpu, &msg);
}

/// Drops the dispatches whose blocks are below `sp` (the program unwound past them).
fn drop_below(sp: u64) {
    STACK.with(|s| s.borrow_mut().retain(|d| (d.rec as u64) > sp));
    UNWINDS.with(|u| u.borrow_mut().retain(|e| e.handler_sp > sp));
}

// ---------------------------------------------------------------------------------------------
// the API

/// The caller's context (as if the trap had returned).
fn caller_context(c: &mut Ctx, ctx: u32) {
    save_context(c.cpu, ctx);
    set_rip(ctx, r64(c.esp));
    set_reg(ctx, 4, c.esp as u64 + 8);
}

/// RtlCaptureContext(CONTEXT*)
fn RtlCaptureContext(c: &mut Ctx) -> u32 {
    let ctx = c.a(0);
    caller_context(c, ctx);
    0
}

/// RtlLookupFunctionEntry(pc, *base, history) -> RUNTIME_FUNCTION*
fn RtlLookupFunctionEntry(c: &mut Ctx) -> u32 {
    match lookup(c.a(0)) {
        Some((f, base)) => {
            crate::mem::put64(c.a(1), crate::mem::widen(base));
            f
        }
        None => {
            crate::mem::put64(c.a(1), 0);
            0
        }
    }
}

/// RtlVirtualUnwind(type, base, pc, entry, CONTEXT*, *handler data, *frame, ctx pointers)
fn RtlVirtualUnwind(c: &mut Ctx) -> u32 {
    let (kind, base, pc, f, ctx) = (c.a(0), c.a(1), c.a64(2), c.a(3), c.a(4));
    let (h, data, frame) = virtual_unwind(kind, base, pc, f, ctx);
    crate::mem::put64(c.a(5), data as u64);
    crate::mem::put64(c.a(6), frame);
    c.ret64(h)
}

/// RtlPcToFileHeader(pc, *base) -> base
fn RtlPcToFileHeader(c: &mut Ctx) -> u32 {
    let b = crate::pe::containing(c.a(0)).unwrap_or(0);
    crate::mem::put64(c.a(1), if b != 0 { crate::mem::widen(b) } else { 0 });
    b
}

/// RtlAddFunctionTable(table, count, base)
fn RtlAddFunctionTable(c: &mut Ctx) -> u32 {
    DYNAMIC.lock().unwrap().push((c.a(0), c.a(1), c.a(2)));
    1
}

/// RtlAddGrowableFunctionTable(&handle, table, count, max count, range base, range end) ->
/// NTSTATUS: a table JIT compilers (mono) append sorted entries to; the handle is the table.
fn RtlAddGrowableFunctionTable(c: &mut Ctx) -> u32 {
    let (out, t, n, base) = (c.a(0), c.a(1), c.a(2), c.a(4));
    if out == 0 || t == 0 {
        return 0xc000_000d; // STATUS_INVALID_PARAMETER
    }
    DYNAMIC.lock().unwrap().push((t, n, base));
    crate::mem::put64(out, crate::mem::widen(t));
    0
}
/// RtlGrowFunctionTable(handle, new count)
fn RtlGrowFunctionTable(c: &mut Ctx) -> u32 {
    let (t, n) = (c.a(0), c.a(1));
    if let Some(e) = DYNAMIC.lock().unwrap().iter_mut().find(|e| e.0 == t) {
        e.1 = n;
    }
    0
}
/// RtlDeleteGrowableFunctionTable(handle)
fn RtlDeleteGrowableFunctionTable(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    DYNAMIC.lock().unwrap().retain(|e| e.0 != t);
    0
}
fn RtlDeleteFunctionTable(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    let mut g = DYNAMIC.lock().unwrap();
    let n = g.len();
    g.retain(|e| e.0 != t);
    (g.len() != n) as u32
}

/// RtlUnwindEx(target frame, target ip, record, return value, CONTEXT*, history)
fn RtlUnwindEx(c: &mut Ctx) -> u32 {
    let (target, ip, rec_in, retval, ctx_in) = (c.a64(0), c.a64(1), c.a(2), c.a64(3), c.a(4));
    unwind(c, target, ip, rec_in, retval, ctx_in);
    0
}

/// RtlUnwind(target frame, target ip, record, return value)
fn RtlUnwind(c: &mut Ctx) -> u32 {
    let (target, ip, rec_in, retval) = (c.a64(0), c.a64(1), c.a(2), c.a64(3));
    unwind(c, target, ip, rec_in, retval, 0);
    0
}

/// An unwind in progress (its blocks in guest memory below RtlUnwindEx's caller).
#[derive(Clone, Copy)]
struct Unwind {
    target: u64,
    ip: u64,
    rec: u32,
    retval: u64,
    /// the frame being unwound, the one above, the dispatcher context
    ctx: u32,
    next: u32,
    disp: u32,
    /// RtlUnwindEx's caller
    orig: u32,
    seen: usize,
    /// the frame whose handler was called
    frame: u64,
    /// RSP after the handler's return to UNWIND_TRAP
    handler_sp: u64,
}

pub fn unwind(c: &mut Ctx, target: u64, ip: u64, rec_in: u32, retval: u64, ctx_in: u32) {
    // scratch below the caller's stack
    let sp = c.esp;
    let ctx = ((sp - 0x200) & !15) - CONTEXT_SIZE;
    let next = ctx - CONTEXT_SIZE;
    let disp = (next - DISP_SIZE) & !15;
    let recb = (disp - RECORD_SIZE) & !15;
    let orig = (recb - CONTEXT_SIZE) & !15;
    caller_context(c, ctx);
    crate::mem::copy(orig, ctx, CONTEXT_SIZE);
    let rec = if rec_in != 0 {
        rec_in
    } else {
        crate::mem::fill(recb, RECORD_SIZE, 0);
        w32(recb, STATUS_UNWIND);
        w64(recb + 0x10, rip(ctx));
        recb
    };
    w32(rec + 4, r32(rec + 4) | if target == 0 { EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND } else { EXCEPTION_UNWINDING });
    if trace() {
        eprintln!("sake: x64 unwind to frame {target:#x} ip {ip:#x} code {:08x} from {:#x}", r32(rec), rip(ctx));
    }
    let _ = ctx_in;
    let u = Unwind { target, ip, rec, retval, ctx, next, disp, orig, seen: 0, frame: 0, handler_sp: 0 };
    UNWINDS.with(|s| s.borrow_mut().push(u));
    c.jumped = true;
    unwind_go(c.cpu, false);
}

/// UNWIND_TRAP: the handler of the topmost unwind's frame returned.
pub fn unwind_handler_done(cpu: &mut Cpu) {
    unwind_go(cpu, true);
}

/// Goes on with the topmost unwind: up the frames until one has a handler (called, returning
/// to UNWIND_TRAP) or the target is reached (its context loaded).
fn unwind_go(cpu: &mut Cpu, resumed: bool) {
    let Some(mut u) = UNWINDS.with(|s| s.borrow().last().copied()) else {
        crate::fatal(cpu, "x64 unwind trap without an unwind");
    };
    let limit = r64(crate::cur().teb + 8);
    let (ctx, next, rec) = (u.ctx, u.next, u.rec);
    // (resumed: past the frame whose handler returned)
    let mut done = resumed && u.frame == u.target;
    if resumed && !done {
        crate::mem::copy(ctx, next, CONTEXT_SIZE);
    }
    while !done {
        crate::mem::copy(next, ctx, CONTEXT_SIZE);
        let pc = rip(ctx);
        let Some((h, data, frame, f, base)) = step(cpu, UNW_FLAG_UHANDLER, next, &mut u.seen, rec) else {
            crate::fatal(cpu, &format!("x64 unwind to {:#x}: ran out of frames at {pc:#x}", u.target));
        };
        if frame > limit || (u.target != 0 && frame > u.target) {
            crate::fatal(cpu, &format!("x64 unwind to {:#x}: frame {frame:#x} beyond it", u.target));
        }
        if frame == u.target {
            w32(rec + 4, r32(rec + 4) | EXCEPTION_TARGET_UNWIND);
        }
        u.frame = frame;
        if h != 0 {
            let disp = u.disp;
            crate::mem::fill(disp, DISP_SIZE, 0);
            w64(disp, pc);
            w64(disp + 8, crate::mem::widen(base));
            w64(disp + 0x10, f as u64);
            w64(disp + 0x18, frame);
            w64(disp + 0x20, u.ip);
            w64(disp + 0x28, ctx as u64);
            w64(disp + 0x30, h);
            w64(disp + 0x38, data as u64);
            if trace() {
                eprintln!("sake: x64 unwind: frame {frame:#x} (pc {pc:#x}) handler {h:#x}");
            }
            // the handler's call: (record, frame, context, dispatcher context) -> UNWIND_TRAP
            let sp = ((u.orig - 0x40) & !15) - 8;
            w64(sp, UNWIND_TRAP as u64);
            u.handler_sp = sp as u64 + 8;
            UNWINDS.with(|s| *s.borrow_mut().last_mut().unwrap() = u);
            cpu.set_gpr(4, sp as u64);
            cpu.set_gpr(1, rec as u64);
            cpu.set_gpr(2, frame);
            cpu.set_gpr(8, ctx as u64);
            cpu.set_gpr(9, disp as u64);
            cpu.eip = h as u32;
            cpu.rip_hi = (h >> 32) as u32;
            return;
        }
        done = frame == u.target;
        if !done {
            crate::mem::copy(ctx, next, CONTEXT_SIZE);
        }
    }
    UNWINDS.with(|s| s.borrow_mut().pop());
    set_reg(ctx, 0, u.retval);
    set_rip(ctx, u.ip);
    // (below everything the unwind used)
    cpu.set_gpr(4, u.orig as u64 - 0x40);
    restore_cpu(cpu, ctx, rec);
}

/// RtlRestoreContext(CONTEXT*, record)
fn RtlRestoreContext(c: &mut Ctx) -> u32 {
    let (ctx, rec) = (c.a(0), c.a(1));
    restore(c, ctx, rec);
    0
}

/// Continues at `ctx` (a longjmp's buffer, or a catch block first, as `rec` says).
fn restore(c: &mut Ctx, ctx: u32, rec: u32) {
    restore_cpu(c.cpu, ctx, rec);
    c.jumped = true;
}

fn restore_cpu(cpu: &mut Cpu, ctx: u32, rec: u32) {
    let code = if rec != 0 { r32(rec) } else { 0 };
    // (below everything the unwind still uses: the exception's record and object)
    let deep = cpu.gpr(4) as u32;
    if code == STATUS_LONGJUMP && r32(rec + 0x18) >= 1 {
        // _JUMP_BUFFER: Frame, Rbx, Rsp, Rbp, Rsi, Rdi, R12-R15, Rip, MxCsr, FpCsr, Spare, Xmm6-15
        let jb = r64(rec + 0x20) as u32;
        for (k, r) in [(1u32, 3usize), (2, 4), (3, 5), (4, 6), (5, 7), (6, 12), (7, 13), (8, 14), (9, 15)] {
            set_reg(ctx, r, r64(jb + 8 * k));
        }
        set_rip(ctx, r64(jb + 0x50));
        for i in 0..10u32 {
            let x = ctx + 0x1a0 + 16 * (6 + i);
            w64(x, r64(jb + 0x60 + 16 * i));
            w64(x + 8, r64(jb + 0x68 + 16 * i));
        }
    }
    drop_below(rsp(ctx));
    load_context(cpu, ctx);
    if code == STATUS_UNWIND_CONSOLIDATE && r32(rec + 0x18) >= 1 {
        // the callback (a catch block) runs below the stack the unwind was called on (the
        // thrown object may be in a frame unwound) and returns where to go on; its return
        // address is CONSOLIDATE_TRAP with the target context above it, its pointer past the
        // callee's home space (MSVC's code keeps registers there: rbx, rcx)
        let cb = r64(rec + 0x20);
        let sp = deep.min(rsp(ctx) as u32);
        let saved = ((sp - 0x100) & !15) - CONTEXT_SIZE;
        crate::mem::copy(saved, ctx, CONTEXT_SIZE);
        // (RSP+8 16-byte aligned at its entry, as after a call)
        let frame = saved - 0x38;
        w64(frame, CONSOLIDATE_TRAP as u64);
        w64(frame + 8 + HOME, saved as u64);
        cpu.set_gpr(4, frame as u64);
        cpu.set_gpr(1, rec as u64);
        cpu.eip = cb as u32;
        cpu.rip_hi = (cb >> 32) as u32;
        if trace() {
            eprintln!("sake: x64 consolidate: callback {cb:#x}");
        }
    }
}

/// The x64 home space of four registers above a return address (the callee's to write)
const HOME: u32 = 0x20;
/// The target context of the catch block that returns to CONSOLIDATE_TRAP with the stack at
/// `sp` (past the return address)
fn consolidate_saved(sp: u32) -> u32 {
    r64(sp + HOME) as u32
}

/// CONSOLIDATE_TRAP: the catch block returned where to continue (RAX).
pub fn consolidated(cpu: &mut Cpu) {
    let sp = cpu.gpr(4) as u32; // (past the return address)
    let saved = consolidate_saved(sp);
    let to = crate::cxx64::catch_done(cpu, sp, cpu.gpr(0));
    set_rip(saved, to);
    load_context(cpu, saved);
    if trace() {
        eprintln!("sake: x64 catch block done: continuing at {to:#x}");
    }
}

/// RaiseException(code, flags, nargs, args)
pub fn raise_exception(c: &mut Ctx) -> u32 {
    let (code, flags, n, args) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let n = if args == 0 { 0 } else { n };
    let params: Vec<u64> = (0..n.min(15)).map(|i| r64(args + 8 * i)).collect();
    // the context: as if RaiseException had returned
    let ret = r64(c.esp);
    c.cpu.eip = ret as u32;
    c.cpu.rip_hi = (ret >> 32) as u32;
    c.cpu.set_gpr(4, c.esp as u64 + 8);
    // (x64 Windows marks a software exception: EXCEPTION_SOFTWARE_ORIGINATE 0x80)
    raise(c.cpu, code, flags & 1 | 0x80, ret as u32, &params);
    c.jumped = true;
    0
}

/// RtlRaiseException(record)
fn RtlRaiseException(c: &mut Ctx) -> u32 {
    let r = c.a(0);
    let sp = c.esp;
    let rec = ((sp - 0x200) & !15) - RECORD_SIZE;
    crate::mem::copy(rec, r, RECORD_SIZE);
    let orig = (rec - CONTEXT_SIZE) & !15;
    caller_context(c, orig);
    if r64(rec + 0x10) == 0 {
        w64(rec + 0x10, rip(orig));
    }
    dispatch(c.cpu, rec, orig);
    c.jumped = true;
    0
}

/// _setjmp(jmp_buf, frame) / _setjmpex: the caller's callee-saved state (_JUMP_BUFFER)
pub fn setjmp(c: &mut Ctx) -> u32 {
    let (jb, frame) = (c.a(0), c.a64(1));
    let cpu = &*c.cpu;
    w64(jb, frame);
    for (k, r) in [(1u32, 3usize), (3, 5), (4, 6), (5, 7), (6, 12), (7, 13), (8, 14), (9, 15)] {
        w64(jb + 8 * k, cpu.gpr(r));
    }
    w64(jb + 0x10, c.esp as u64 + 8);
    w64(jb + 0x50, r64(c.esp));
    w32(jb + 0x58, cpu.mxcsr);
    crate::mem::w16(jb + 0x5c, cpu.fpu.cw);
    for i in 0..10u32 {
        let x = cpu.xmm(6 + i as usize);
        w64(jb + 0x60 + 16 * i, x as u64);
        w64(jb + 0x68 + 16 * i, (x >> 64) as u64);
    }
    0
}

/// longjmp(jmp_buf, value): unwinds to the buffer's frame (running __finally blocks and
/// destructors), or just restores it when it has none
pub fn longjmp(c: &mut Ctx) -> u32 {
    let (jb, v) = (c.a(0), c.a(1));
    let v = if v == 0 { 1 } else { v } as u64;
    let rec = ((c.esp - 0x800) & !15) - RECORD_SIZE;
    crate::mem::fill(rec, RECORD_SIZE, 0);
    w32(rec, STATUS_LONGJUMP);
    w32(rec + 0x18, 1);
    w64(rec + 0x20, jb as u64);
    let frame = r64(jb);
    if frame != 0 {
        unwind(c, frame, r64(jb + 0x50), rec, v, 0);
    } else {
        let ctx = (rec - CONTEXT_SIZE) & !15;
        caller_context(c, ctx);
        set_reg(ctx, 0, v);
        restore(c, ctx, rec);
    }
    0
}

#[allow(non_snake_case)]
pub fn register() {
    if !crate::mem::x64() {
        return;
    }
    for d in ["ntdll", "kernelbase"] {
        implement(d, "RtlAddGrowableFunctionTable", RtlAddGrowableFunctionTable);
        implement(d, "RtlGrowFunctionTable", RtlGrowFunctionTable);
        implement(d, "RtlDeleteGrowableFunctionTable", RtlDeleteGrowableFunctionTable);
    }
    for d in ["ntdll", "kernel32"] {
        implement(d, "RtlCaptureContext", RtlCaptureContext);
        implement(d, "RtlLookupFunctionEntry", RtlLookupFunctionEntry);
        implement(d, "RtlVirtualUnwind", RtlVirtualUnwind);
        implement(d, "RtlPcToFileHeader", RtlPcToFileHeader);
        implement(d, "RtlUnwindEx", RtlUnwindEx);
        implement(d, "RtlUnwind", RtlUnwind);
        implement(d, "RtlRestoreContext", RtlRestoreContext);
        implement(d, "RtlRaiseException", RtlRaiseException);
        implement(d, "RtlAddFunctionTable", RtlAddFunctionTable);
        implement(d, "RtlDeleteFunctionTable", RtlDeleteFunctionTable);
    }
    implement("kernel32", "RaiseException", raise_exception);
    implement("ntdll", "_setjmp", setjmp);
    implement("ntdll", "_setjmpex", setjmp);
    implement("ntdll", "longjmp", longjmp);
    // (what MSVC calls for setjmp: the same jump buffer, vcruntime140's longjmp)
    for d in ["vcruntime140", "msvcrt"] {
        implement(d, "__intrinsic_setjmp", setjmp);
        implement(d, "__intrinsic_setjmpex", setjmp);
    }
    implement("vcruntime140", "longjmp", longjmp);
    implement("vcruntime140", "_setjmp", setjmp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // An SE translator's C++ exception is looked for in the translating frame alone (Windows'
    // answers: host/tests/expected/msvcseh.txt, one translator call per frame with try blocks)
    #[test]
    fn guarded_calls_only_the_translating_frame() {
        assert!(guarded(0, 0x1000));
        assert!(guarded(0x2000, 0x2000));
        assert!(!guarded(0x2000, 0x1ff0));
    }

    proptest! {
        #[test]
        fn guarded_is_any_frame_or_exactly_the_guard(guard in any::<u64>(), frame in any::<u64>()) {
            prop_assert_eq!(guarded(guard, frame), guard == 0 || frame == guard);
            prop_assert!(guarded(0, frame));
            prop_assert!(guarded(frame, frame));
        }
    }
}
