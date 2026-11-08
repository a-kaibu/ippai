//! x64 C++ exceptions: vcruntime140's _CxxThrowException, __CxxFrameHandler3,
//! vcruntime140_1's __CxxFrameHandler4 and __C_specific_handler (SEH __try), on seh64's
//! table-based dispatch. The data structures are MSVC's (ehdata.h / ehdata4.h: RVAs from the
//! image base); the algorithms follow Wine's msvcrt (except.c, handler4.c).
//!
//! A catch block runs as the unwind's consolidation callback: the C++ handler unwinds to its
//! frame with a STATUS_UNWIND_CONSOLIDATE record whose callback is CATCH (a trap), and seh64
//! calls it below the stack of the exception (so the thrown object, which may live in a frame
//! that was unwound, stays intact). CATCH registers the exception as the thread's current one
//! and jumps into the catch funclet, which returns to seh64's CONSOLIDATE_TRAP with where to go
//! on; catch_done then ends the catch (destroying the object unless it was rethrown). An
//! exception that leaves a catch block passes its CONSOLIDATE_TRAP frame: through_catch notes
//! a rethrow (dispatch) or abandons the catch (unwind), as Wine's filter and __finally around
//! its call of the funclet.
//!
//! _set_se_translator: a frame handler with try blocks that gets a structured exception (not
//! /EHs) jumps into the thread's translator with TRANSLATE_TRAP as the return address. A C++
//! exception it throws is dispatched on its own: seh64's walk goes from TRANSLATE_TRAP to the
//! handler's caller and from there into the frames of the structured exception, so any frame
//! from where that happened up can catch it. A translator that returns gets the handler called
//! again, which goes on without it (catch (...) only).

#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use crate::api::{implement, Ctx};
use crate::mem::{r32, r64, w32, w64};

const CXX_EXCEPTION: u32 = 0xe06d_7363;
const MAGIC_VC6: u32 = 0x1993_0520;
const MAGIC_VC8: u32 = 0x1993_0522;
const STATUS_UNWIND_CONSOLIDATE: u32 = 0x8000_0029;
const STATUS_LONGJUMP: u32 = 0x8000_0026;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;
const EXCEPTION_NONCONTINUABLE: u32 = 1;
const EXCEPTION_UNWINDING: u32 = 2;
const EXCEPTION_EXIT_UNWIND: u32 = 4;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const CONTINUE_EXECUTION: u32 = 0;
const CONTINUE_SEARCH: u32 = 1;
const RECORD_SIZE: u32 = 0x98;

const TYPE_FLAG_CONST: u32 = 1;
const TYPE_FLAG_VOLATILE: u32 = 2;
const TYPE_FLAG_REFERENCE: u32 = 8;
const CLASS_IS_SIMPLE_TYPE: u32 = 1;
const CLASS_HAS_VIRTUAL_BASE_CLASS: u32 = 4;

/// FuncInfo (FH3) flags
const FUNC_DESCR_SYNCHRONOUS: u32 = 1;
const FUNC_DESCR_NOEXCEPT: u32 = 4;

/// The consolidation records' parameter counts (FH3 as Wine's 11, FH4 10).
const FH3_PARAMS: u32 = 11;
const FH4_PARAMS: u32 = 10;

/// Where the current exception (record, context) is kept: the TEB's free page after errno.
const CUR_EXC_TEB: u32 = crate::crt::ERRNO_TEB + 0x10;

/// _set_se_translator's function, per thread (after the current exception)
const SE_TRANSLATOR_TEB: u32 = crate::cxx::SE_TRANSLATOR_TEB;
/// The exception (record u32, frame u64) whose translator returned: its handler, called again,
/// goes on without it.
const SE_TRANSLATED_TEB: u32 = SE_TRANSLATOR_TEB + 8;
/// Where the translator returns to (seh64's walk passes it too).
pub const TRANSLATE_TRAP: u32 = 0xffff_0060;
/// The translator's call below the handler's return address: TRANSLATE_TRAP, the home space,
/// EXCEPTION_POINTERS (record, context) at 0x30, the dispatcher context at 0x40 and the frame
/// at 0x48.
pub const TRANSLATE_FRAME: u32 = 0x50;

fn trace() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var_os("SAKE_TRACE_SEH").is_some())
}

// ---------------------------------------------------------------------------------------------
// records and the dispatcher context

fn code(rec: u32) -> u32 {
    r32(rec)
}
fn flags(rec: u32) -> u32 {
    r32(rec + 4)
}
fn nparams(rec: u32) -> u32 {
    r32(rec + 0x18)
}
fn info(rec: u32, i: u32) -> u64 {
    r64(rec + 0x20 + 8 * i)
}
fn is_cxx(rec: u32) -> bool {
    code(rec) == CXX_EXCEPTION && nparams(rec) == 4 && (info(rec, 0) as u32).wrapping_sub(MAGIC_VC6) <= 2
}

/// DISPATCHER_CONTEXT (x64)
struct Disp {
    pc: u64,
    base: u64,
    func: u32,
    target_ip: u64,
    ctx: u32,
    data: u32,
    at: u32,
}

impl Disp {
    fn read(d: u32) -> Disp {
        Disp { pc: r64(d), base: r64(d + 8), func: r64(d + 0x10) as u32, target_ip: r64(d + 0x20), ctx: r64(d + 0x28) as u32, data: r64(d + 0x38) as u32, at: d }
    }
    /// guest address of an RVA (images above 4G: their low alias)
    fn rva(&self, r: u32) -> u32 {
        (self.base as u32).wrapping_add(r)
    }
    /// the code address of an RVA (as code sees it)
    fn code(&self, r: u32) -> u64 {
        self.base + r as u64
    }
    fn func_begin(&self) -> u32 {
        r32(self.func)
    }
}

fn catch_trap() -> u32 {
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| crate::api::host_fn("cxx64::catch", catch_entry, 0))
}

fn is_consolidate(rec: u32, n: u32) -> bool {
    code(rec) == STATUS_UNWIND_CONSOLIDATE && nparams(rec) == n && info(rec, 0) == catch_trap() as u64
}

// ---------------------------------------------------------------------------------------------
// types: ThrowInfo { flags, destructor, custom handler, catchable type table }, CatchableType
// { flags, type_info, this offsets (3 ints), size, copy constructor }, type_info { vtable,
// spare, mangled name }

/// The image base that holds `addr` (the thrown type's RVAs are from it).
pub fn image_base_of(addr: u32) -> u64 {
    crate::pe::MODULES.read().unwrap().iter().find(|m| addr.wrapping_sub(m.base) < m.size).map_or(0, |m| crate::mem::widen(m.base))
}

fn mangled(ti: u32) -> Vec<u8> {
    crate::mem::cstr(ti + 16)
}

/// The this pointer of a base class at `type`'s offsets within `object`.
fn this_ptr(ty: u32, object: u64) -> u64 {
    if object == 0 {
        return 0;
    }
    let (this_off, vbase_descr, vbase_off) = (r32(ty + 8) as i32, r32(ty + 12) as i32, r32(ty + 16) as i32);
    let mut o = object;
    if vbase_descr >= 0 {
        o = o.wrapping_add(vbase_descr as i64 as u64);
        let offset_ptr = r64(o as u32).wrapping_add(vbase_off as i64 as u64);
        o = o.wrapping_add(r32(offset_ptr as u32) as i32 as i64 as u64);
    }
    o.wrapping_add(this_off as i64 as u64)
}

/// The catchable type of the thrown `exc_type` (at `exc_base`) a catch of `catch_ti` (0:
/// catch(...)) with `catch_flags` takes, if any.
fn find_caught_type(exc_type: u32, exc_base: u64, catch_ti: u32, catch_flags: u32) -> Option<u32> {
    let rva = |r: u32| (exc_base as u32).wrapping_add(r);
    let table = rva(r32(exc_type + 12));
    for i in 0..r32(table) {
        let ty = rva(r32(table + 4 + 4 * i));
        let ti = rva(r32(ty + 4));
        if catch_ti == 0 {
            return Some(ty);
        }
        if catch_ti != ti && mangled(catch_ti) != mangled(ti) {
            continue;
        }
        let ef = r32(exc_type);
        if ef & TYPE_FLAG_CONST != 0 && catch_flags & TYPE_FLAG_CONST == 0 {
            continue;
        }
        if ef & TYPE_FLAG_VOLATILE != 0 && catch_flags & TYPE_FLAG_VOLATILE == 0 {
            continue;
        }
        return Some(ty);
    }
    None
}

/// Copies the thrown object into the catch's variable at `dest`.
fn copy_exception(c: &mut Ctx, object: u64, dest: u32, catch_flags: u32, ty: u32, exc_base: u64) {
    let (tflags, size, ctor) = (r32(ty), r32(ty + 20), r32(ty + 24));
    if catch_flags & TYPE_FLAG_REFERENCE != 0 {
        w64(dest, this_ptr(ty, object));
    } else if tflags & CLASS_IS_SIMPLE_TYPE != 0 {
        crate::mem::copy(dest, object as u32, size);
        if size == 8 {
            w64(dest, this_ptr(ty, r64(dest)));
        }
    } else if ctor != 0 {
        let src = this_ptr(ty, object);
        let f = exc_base + ctor as u64;
        if tflags & CLASS_HAS_VIRTUAL_BASE_CLASS != 0 {
            crate::call64(c.cpu, f, &[dest as u64, src, 1]);
        } else {
            crate::call64(c.cpu, f, &[dest as u64, src]);
        }
    } else {
        crate::mem::copy(dest, this_ptr(ty, object) as u32, size);
    }
}

/// Runs the thrown object's destructor (record `rec`, a C++ one).
pub fn destroy(cpu: &mut koji::Cpu, rec: u32) {
    if !is_cxx(rec) || info(rec, 2) == 0 {
        return;
    }
    let (object, ti, base) = (info(rec, 1), info(rec, 2) as u32, info(rec, 3));
    let dtor = r32(ti + 4);
    if dtor != 0 {
        if trace() {
            eprintln!("sake: x64 C++: destroying the exception object {object:#x}");
        }
        crate::call64(cpu, base + dtor as u64, &[object]);
    }
}

// ---------------------------------------------------------------------------------------------
// the catches in progress on this thread

#[derive(Clone, Copy)]
enum Kind {
    /// FH3: the function's unwind help (state, try level), reset after the catch
    Fh3 { unwind_help: u32 },
    /// FH4: the try levels for a search / an unwind through the catch, its continuations
    Fh4 { search: i32, unwind: i32, ret: [u64; 2] },
}

#[derive(Clone, Copy)]
struct Active {
    /// the stack pointer at CONSOLIDATE_TRAP when the funclet returns
    key: u32,
    /// the exception (record and context) the catch handles
    rec: u32,
    ctx: u32,
    rethrown: bool,
    kind: Kind,
}

thread_local! {
    static ACTIVE: RefCell<Vec<Active>> = const { RefCell::new(Vec::new()) };
    /// FH4's try level for the next handler call (-2: from the IP), as Wine's FLS slot
    static TRYLEVEL4: Cell<i32> = const { Cell::new(-2) };
}

/// No exception being handled on this thread (save states: snap.rs).
pub fn idle() -> bool {
    ACTIVE.with(|a| a.borrow().is_empty()) && TRYLEVEL4.with(|t| t.get()) == -2
}

/// The EXCEPTION_RECORD of the exception the thread is handling (0: none), for exception_ptr.
pub fn current_record() -> u32 {
    ACTIVE.with(|a| a.borrow().last().map_or(0, |a| a.rec))
}

/// The thread's current exception (__current_exception / _context) after a change of catches.
fn set_current() {
    let top = ACTIVE.with(|a| a.borrow().last().copied());
    let teb = crate::cur().teb;
    w64(teb + CUR_EXC_TEB, top.map_or(0, |a| a.rec as u64));
    w64(teb + CUR_EXC_TEB + 8, top.map_or(0, |a| a.ctx as u64));
}

/// Ends catch `a` (taken off the list): its object goes unless rethrown or caught again.
fn end_catch(cpu: &mut koji::Cpu, a: Active) {
    let object = info(a.rec, 1);
    let in_use = ACTIVE.with(|l| l.borrow().iter().any(|o| is_cxx(o.rec) && info(o.rec, 1) == object));
    if !a.rethrown && !in_use {
        destroy(cpu, a.rec);
    }
    set_current();
}

/// CATCH: the consolidation callback (RCX: the record; the return address is seh64's
/// CONSOLIDATE_TRAP with the target context after it). Enters the catch funclet
/// (RDX: the frame).
fn catch_entry(c: &mut Ctx) -> u32 {
    let rec = c.a(0);
    let (frame, handler) = (info(rec, 1), info(rec, 5));
    let (prev, ctx) = (info(rec, 6) as u32, info(rec, 7) as u32);
    let kind = if nparams(rec) == FH3_PARAMS {
        let descr = info(rec, 2) as u32;
        Kind::Fh3 { unwind_help: (frame as u32).wrapping_add(r32(descr + 28)) }
    } else {
        Kind::Fh4 { search: info(rec, 2) as i32, unwind: info(rec, 3) as i32, ret: [info(rec, 8), info(rec, 9)] }
    };
    if trace() {
        eprintln!("sake: x64 C++: catch block {handler:#x} frame {frame:#x}");
    }
    ACTIVE.with(|a| a.borrow_mut().push(Active { key: c.esp + 8, rec: prev, ctx, rethrown: false, kind }));
    set_current();
    // (the funclet as Wine's call_exc_handler calls it: RCX the handler, RDX the frame)
    c.cpu.set_gpr(1, handler);
    c.cpu.set_gpr(2, frame);
    c.cpu.set_gpr(8, 0x100);
    c.cpu.eip = handler as u32;
    c.cpu.rip_hi = (handler >> 32) as u32;
    c.jumped = true;
    0
}

/// seh64's CONSOLIDATE_TRAP: a catch funclet returned `rax` with the stack at `sp`. Ends the
/// catch it was and gives where to continue.
pub fn catch_done(cpu: &mut koji::Cpu, sp: u32, rax: u64) -> u64 {
    let Some(a) = ACTIVE.with(|l| {
        let mut l = l.borrow_mut();
        if l.last().is_some_and(|a| a.key == sp) { l.pop() } else { None }
    }) else {
        return rax;
    };
    end_catch(cpu, a);
    match a.kind {
        Kind::Fh3 { unwind_help } => {
            w32(unwind_help, -2i32 as u32);
            w32(unwind_help + 4, -1i32 as u32);
            rax
        }
        Kind::Fh4 { ret, .. } => {
            TRYLEVEL4.with(|t| t.set(-2));
            if ret[1] != 0 {
                ret[(rax != 0) as usize]
            } else if ret[0] != 0 {
                ret[0]
            } else {
                rax
            }
        }
    }
}

/// seh64's walk passes the CONSOLIDATE_TRAP frame at `sp` (a catch block's caller) for the
/// exception `rec`: searching, a rethrow of the catch's object is noted; unwinding, the catch
/// is left (as the __finally around Wine's funclet call).
pub fn through_catch(cpu: &mut koji::Cpu, sp: u32, unwinding: bool, rec: u32) {
    let Some(i) = ACTIVE.with(|l| l.borrow().iter().rposition(|a| a.key == sp)) else { return };
    let a = ACTIVE.with(|l| l.borrow()[i]);
    if !unwinding {
        if let Kind::Fh4 { search, .. } = a.kind {
            TRYLEVEL4.with(|t| t.set(search));
        }
        if rec != 0 && is_cxx(rec) && is_cxx(a.rec) && info(rec, 1) == info(a.rec, 1) {
            ACTIVE.with(|l| l.borrow_mut()[i].rethrown = true);
        }
        return;
    }
    if let Kind::Fh4 { unwind, .. } = a.kind {
        TRYLEVEL4.with(|t| t.set(unwind));
    }
    let a = ACTIVE.with(|l| l.borrow_mut().remove(i));
    end_catch(cpu, a);
}

// ---------------------------------------------------------------------------------------------
// _CxxThrowException(object, ThrowInfo)

fn throw(c: &mut Ctx) -> u32 {
    let (object, ti) = (c.a64(0), c.a(1));
    let params: [u64; 4] = if object == 0 && ti == 0 {
        // throw; : the current exception again
        let Some(a) = ACTIVE.with(|l| l.borrow().last().copied()) else {
            crate::fatal(c.cpu, "C++ rethrow without an exception (terminate)");
        };
        [info(a.rec, 0), info(a.rec, 1), info(a.rec, 2), info(a.rec, 3)]
    } else {
        [MAGIC_VC6 as u64, object, ti as u64, image_base_of(ti)]
    };
    raise_cxx(c, params);
    0
}

/// throw; from a trap: the exception being handled again
pub fn rethrow(c: &mut Ctx) {
    let Some(a) = ACTIVE.with(|l| l.borrow().last().copied()) else {
        crate::fatal(c.cpu, "C++ rethrow without an exception (terminate)");
    };
    raise_cxx(c, [info(a.rec, 0), info(a.rec, 1), info(a.rec, 2), info(a.rec, 3)]);
}

/// Throws `object` of ThrowInfo `ti` from a trap (msvcp's _X* functions).
pub fn throw_object(c: &mut Ctx, object: u64, ti: u32) {
    raise_cxx(c, [MAGIC_VC6 as u64, object, ti as u64, image_base_of(ti)]);
}

fn raise_cxx(c: &mut Ctx, params: [u64; 4]) {
    if trace() {
        eprintln!("sake: x64 C++ throw object {:#x} type {:#x}", params[1], params[2]);
    }
    // the context: as if the call had returned
    let ret = r64(c.esp);
    c.cpu.eip = ret as u32;
    c.cpu.rip_hi = (ret >> 32) as u32;
    c.cpu.set_gpr(4, c.esp as u64 + 8);
    crate::seh64::raise(c.cpu, CXX_EXCEPTION, EXCEPTION_NONCONTINUABLE, ret as u32, &params);
    c.jumped = true;
}

/// Unwinds to `frame` and calls the catch funclet `handler` (the consolidation record built
/// below the handler's stack).
fn unwind_to_catch(c: &mut Ctx, frame: u64, d: &Disp, params: &[u64]) {
    let rec = ((c.esp - 0x100) & !15) - RECORD_SIZE;
    crate::mem::fill(rec, RECORD_SIZE, 0);
    w32(rec, STATUS_UNWIND_CONSOLIDATE);
    w32(rec + 4, EXCEPTION_NONCONTINUABLE);
    w32(rec + 0x18, params.len() as u32);
    for (i, p) in params.iter().enumerate() {
        w64(rec + 0x20 + 8 * i as u32, *p);
    }
    crate::seh64::unwind(c, frame, d.pc, rec, 0, 0);
}

// ---------------------------------------------------------------------------------------------
// __CxxFrameHandler3: FuncInfo { magic, unwind count, unwind map, try count, try map, ip map
// count, ip map, unwind help, expect list, flags }

struct Fh3 {
    magic: u32,
    unwind_count: u32,
    unwind_table: u32,
    try_count: u32,
    tryblocks: u32,
    ip_count: u32,
    ipmap: u32,
    unwind_help: i32,
    flags: u32,
}

impl Fh3 {
    fn read(d: &Disp, p: u32) -> Fh3 {
        Fh3 {
            magic: r32(p) & 0x1fff_ffff,
            unwind_count: r32(p + 4),
            unwind_table: d.rva(r32(p + 8)),
            try_count: r32(p + 12),
            tryblocks: d.rva(r32(p + 16)),
            ip_count: r32(p + 20),
            ipmap: d.rva(r32(p + 24)),
            unwind_help: r32(p + 28) as i32,
            flags: if r32(p) & 0x1fff_ffff >= MAGIC_VC8 { r32(p + 36) } else { 0 },
        }
    }
    fn ip_to_state(&self, d: &Disp, ip: u64) -> i32 {
        let mut ret = -1;
        for i in 0..self.ip_count {
            let e = self.ipmap + 8 * i;
            if d.code(r32(e)) > ip {
                break;
            }
            ret = r32(e + 4) as i32;
        }
        ret
    }
    /// try block i: (start, end, catch level, catch count, catch blocks)
    fn tryblock(&self, d: &Disp, i: u32) -> (i32, i32, i32, u32, u32) {
        let t = self.tryblocks + 20 * i;
        (r32(t) as i32, r32(t + 4) as i32, r32(t + 8) as i32, r32(t + 12), d.rva(r32(t + 16)))
    }
}

fn local_unwind3(c: &mut Ctx, frame: u64, d: &Disp, f: &Fh3, last: i32) {
    let help = (frame as u32).wrapping_add(f.unwind_help as u32);
    let mut level = r32(help) as i32;
    if level == -2 {
        level = f.ip_to_state(d, d.pc);
    }
    while level > last {
        if level < 0 || level as u32 >= f.unwind_count {
            crate::fatal(c.cpu, &format!("C++ unwind: invalid state {level} (terminate)"));
        }
        let e = f.unwind_table + 8 * level as u32;
        let h = r32(e + 4);
        if h != 0 {
            crate::call64(c.cpu, d.code(h), &[d.code(h), frame, 0x100]);
        }
        level = r32(e) as i32;
    }
    w32(help, level as u32);
}

fn find_catch3(c: &mut Ctx, rec: u32, ctx: u32, frame: u64, d: &Disp, f: &Fh3, descr: u32, exc_type: u32, orig_frame: u64) {
    let exc_base = if nparams(rec) == 4 { info(rec, 3) } else { 0 };
    let object = info(rec, 1);
    let mut level = f.ip_to_state(d, d.pc);
    let mut in_catch = None;
    for i in (0..f.try_count).rev() {
        let t = f.tryblock(d, i);
        if level > t.1 && level <= t.2 {
            in_catch = Some(t);
            break;
        }
    }
    let help = (orig_frame as u32).wrapping_add(f.unwind_help as u32);
    if level > r32(help + 4) as i32 {
        w32(help, level as u32);
        w32(help + 4, level as u32);
    } else {
        level = r32(help + 4) as i32;
    }
    for i in 0..f.try_count {
        let (start, end, _, count, blocks) = f.tryblock(d, i);
        if level < start || level > end {
            continue;
        }
        if let Some(ic) = in_catch {
            if start <= ic.1 || end > ic.2 {
                continue;
            }
        }
        for j in 0..count {
            let cb = blocks + 20 * j;
            let (cflags, cti, offset, handler) = (r32(cb), r32(cb + 4), r32(cb + 8) as i32, r32(cb + 12));
            let catch_ti = if cti != 0 { d.rva(cti) } else { 0 };
            if exc_type != 0 {
                let Some(ty) = find_caught_type(exc_type, exc_base, catch_ti, cflags) else { continue };
                if catch_ti != 0 && crate::mem::r8(catch_ti + 16) != 0 && offset != 0 {
                    copy_exception(c, object, (orig_frame as u32).wrapping_add(offset as u32), cflags, ty, exc_base);
                }
            } else if cti != 0 {
                continue;
            }
            let params = [
                catch_trap() as u64,
                orig_frame,
                descr as u64,
                start as i64 as u64,
                0,
                d.code(handler),
                rec as u64,
                ctx as u64,
                0,
                0,
                u64::MAX,
            ];
            unwind_to_catch(c, frame, d, &params);
            return;
        }
    }
}

fn frame_handler3(c: &mut Ctx) -> u32 {
    let (rec, frame, ctx, dp) = (c.a(0), c.a64(1), c.a(2), c.a(3));
    let d = Disp::read(dp);
    let descr = d.rva(r32(d.data));
    let f = Fh3::read(&d, descr);
    if !(MAGIC_VC6..=MAGIC_VC8).contains(&f.magic) {
        return CONTINUE_SEARCH;
    }
    if f.magic >= MAGIC_VC8
        && f.flags & FUNC_DESCR_SYNCHRONOUS != 0
        && code(rec) != CXX_EXCEPTION
        && !is_consolidate(rec, FH3_PARAMS)
        && code(rec) != STATUS_LONGJUMP
    {
        return CONTINUE_SEARCH;
    }
    let level = f.ip_to_state(&d, d.pc);
    let mut orig_frame = frame;
    let mut unwind_level = -1;
    // a nested exception (from a catch funclet): the frame of the function it belongs to
    let mut throw_func = None;
    for i in (0..f.try_count).rev() {
        let (_, end, catch_level, count, blocks) = f.tryblock(&d, i);
        if level > end && level <= catch_level {
            let tf = *throw_func.get_or_insert_with(|| crate::seh64::lookup(d.pc as u32).map(|(e, b)| b.wrapping_add(r32(e))));
            for j in 0..count {
                let cb = blocks + 20 * j;
                if Some(d.rva(r32(cb + 12))) == tf {
                    unwind_level = end;
                    orig_frame = r64((frame as u32).wrapping_add(r32(cb + 16)));
                }
            }
        }
    }
    if flags(rec) & (EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND) != 0 {
        let last = if flags(rec) & EXCEPTION_TARGET_UNWIND != 0 {
            if is_consolidate(rec, FH3_PARAMS) { info(rec, 3) as i32 } else { level }
        } else {
            unwind_level
        };
        local_unwind3(c, orig_frame, &d, &f, last);
        return CONTINUE_SEARCH;
    }
    let nested = orig_frame != frame;
    let noexcept = |c: &mut Ctx| {
        if !nested && code(rec) == CXX_EXCEPTION && f.magic >= MAGIC_VC8 && f.flags & FUNC_DESCR_NOEXCEPT != 0 {
            crate::fatal(c.cpu, "noexcept function propagating an exception (terminate)");
        }
    };
    if f.try_count == 0 {
        noexcept(c);
        return CONTINUE_SEARCH;
    }
    let exc_type = if code(rec) == CXX_EXCEPTION { info(rec, 2) as u32 } else { 0 };
    if exc_type == 0 && code(rec) != CXX_EXCEPTION && translate(c, rec, frame, ctx, dp) {
        return 0;
    }
    find_catch3(c, rec, ctx, frame, &d, &f, descr, exc_type, orig_frame);
    if c.jumped {
        return 0;
    }
    noexcept(c);
    CONTINUE_SEARCH
}

// ---------------------------------------------------------------------------------------------
// __CxxFrameHandler4: FuncInfo4, compressed (ehdata4.h)

const FH4_IS_CATCH: u8 = 0x01;
const FH4_IS_SEPARATED: u8 = 0x02;
const FH4_BBT: u8 = 0x04;
const FH4_UNWIND_MAP: u8 = 0x08;
const FH4_TRYBLOCK_MAP: u8 = 0x10;
const FH4_EHS: u8 = 0x20;
const FH4_NO_EXCEPT: u8 = 0x40;

/// FuncInfo4's compressed unsigned integers (1 to 4 bytes, the low bits say how many).
fn decode_uint(p: &mut u32) -> u32 {
    let b = |i: u32| crate::mem::r8(*p + i) as u32;
    let (v, n) = match b(0) {
        x if x & 1 == 0 => (x >> 1, 1),
        x if x & 3 == 1 => ((x >> 2) + (b(1) << 6), 2),
        x if x & 7 == 3 => ((x >> 3) + (b(1) << 5) + (b(2) << 13), 3),
        x if x & 15 == 7 => ((x >> 4) + (b(1) << 4) + (b(2) << 12) + (b(3) << 20), 4),
        _ => (r32(*p + 1), 5),
    };
    *p += n;
    v
}
fn read_rva(p: &mut u32) -> u32 {
    let v = r32(*p);
    *p += 4;
    v
}

struct Fh4 {
    header: u8,
    unwind_count: u32,
    unwind_map: u32,
    try_count: u32,
    try_map: u32,
    ip_count: u32,
    ip_map: u32,
    frame: u32,
}

/// An unwind map entry: (type, the previous entry, handler, object offset)
fn read_unwind4(p: &mut u32) -> (u32, u32, u32, u32) {
    let start = *p;
    let t = decode_uint(p);
    let prev = start.wrapping_sub(t >> 2);
    let (handler, object) = match t & 3 {
        1 | 2 => {
            let h = read_rva(p);
            (h, decode_uint(p))
        }
        3 => (read_rva(p), 0),
        _ => (0, 0),
    };
    (t & 3, prev, handler, object)
}

/// A try block: (start, end, catch level, catch count, catch blocks)
fn read_try4(p: &mut u32, d: &Disp) -> (i32, i32, i32, u32, u32) {
    let (start, end, catch_level) = (decode_uint(p) as i32, decode_uint(p) as i32, decode_uint(p) as i32);
    let cb = read_rva(p);
    if cb == 0 {
        return (start, end, catch_level, 0, 0);
    }
    let mut q = d.rva(cb);
    let n = decode_uint(&mut q);
    (start, end, catch_level, n, q)
}

/// A catch block: (flags, type_info, offset, handler, continuations); None: a form not known
fn read_catch4(p: &mut u32, func_rva: u32) -> Option<(u32, u32, u32, u32, [u32; 2])> {
    let h = crate::mem::r8(*p);
    *p += 1;
    if h & !0x3f != 0 || h & 0x30 == 0x30 {
        return None;
    }
    let flags = if h & 1 != 0 { decode_uint(p) } else { 0 };
    let ti = if h & 2 != 0 { read_rva(p) } else { 0 };
    let offset = if h & 4 != 0 { decode_uint(p) } else { 0 };
    let handler = read_rva(p);
    let mut ret = [0u32; 2];
    let n = match h & 0x30 {
        0x10 => 1,
        0x20 => 2,
        _ => 0,
    };
    for r in ret.iter_mut().take(n) {
        *r = if h & 8 != 0 { read_rva(p) } else { decode_uint(p) + func_rva };
    }
    Some((flags, ti, offset, handler, ret))
}

impl Fh4 {
    fn read(d: &Disp) -> Option<Fh4> {
        let mut p = d.rva(r32(d.data));
        let header = crate::mem::r8(p);
        p += 1;
        if header & FH4_BBT != 0 {
            return None;
        }
        let mut f = Fh4 { header, unwind_count: 0, unwind_map: 0, try_count: 0, try_map: 0, ip_count: 0, ip_map: 0, frame: 0 };
        if header & FH4_UNWIND_MAP != 0 {
            let mut q = d.rva(read_rva(&mut p));
            f.unwind_count = decode_uint(&mut q);
            f.unwind_map = q;
        }
        if header & FH4_TRYBLOCK_MAP != 0 {
            let mut q = d.rva(read_rva(&mut p));
            f.try_count = decode_uint(&mut q);
            f.try_map = q;
        }
        let mut ip = read_rva(&mut p);
        if header & FH4_IS_SEPARATED != 0 {
            let mut q = d.rva(ip);
            let n = decode_uint(&mut q);
            let begin = d.func_begin();
            let mut found = None;
            for _ in 0..n {
                let func = read_rva(&mut q);
                let map = read_rva(&mut q);
                if func == begin {
                    found = Some(map);
                    break;
                }
            }
            ip = found?;
        }
        let mut q = d.rva(ip);
        f.ip_count = decode_uint(&mut q);
        f.ip_map = q;
        if header & FH4_IS_CATCH != 0 {
            f.frame = decode_uint(&mut p);
        }
        Some(f)
    }
    fn ip_to_state(&self, d: &Disp, ip: u64) -> i32 {
        let mut p = self.ip_map;
        let mut at = d.code(d.func_begin());
        let mut ret = -1;
        for _ in 0..self.ip_count {
            at += decode_uint(&mut p) as u64;
            let state = decode_uint(&mut p) as i32 - 1;
            if ip < at {
                break;
            }
            ret = state;
        }
        ret
    }
}

fn local_unwind4(c: &mut Ctx, frame: u64, d: &Disp, f: &Fh4, level: i32, last: i32) {
    let level = if level == -2 { f.ip_to_state(d, d.pc) } else { level };
    if level < -1 || level >= f.unwind_count as i32 {
        crate::fatal(c.cpu, &format!("C++ unwind (FH4): invalid state {level} (terminate)"));
    }
    if level <= last {
        return;
    }
    let mut p = f.unwind_map;
    let mut stop = p.wrapping_sub(1);
    for i in 0..level {
        let at = p;
        read_unwind4(&mut p);
        if i == last {
            stop = at;
        }
    }
    while p > stop {
        let mut q = p;
        let (kind, prev, handler, object) = read_unwind4(&mut q);
        p = prev;
        if handler != 0 {
            let mut obj = frame.wrapping_add(object as u64);
            if kind == 2 {
                obj = r64(obj as u32);
            }
            crate::call64(c.cpu, d.code(handler), &[obj, frame]);
        }
    }
}

fn find_catch4(c: &mut Ctx, rec: u32, ctx: u32, frame: u64, d: &Disp, f: &Fh4, exc_type: u32, orig_frame: u64, level: i32) {
    let exc_base = if nparams(rec) == 4 { info(rec, 3) } else { 0 };
    let level = if level == -2 { f.ip_to_state(d, d.pc) } else { level };
    let mut tp = f.try_map;
    for _ in 0..f.try_count {
        let (start, end, catch_level, count, blocks) = read_try4(&mut tp, d);
        if level < start || level > end {
            continue;
        }
        let mut cp = blocks;
        for _ in 0..count {
            let Some((cflags, cti, offset, handler, ret)) = read_catch4(&mut cp, d.func_begin()) else { return };
            let catch_ti = if cti != 0 { d.rva(cti) } else { 0 };
            if exc_type != 0 {
                let Some(ty) = find_caught_type(exc_type, exc_base, catch_ti, cflags) else { continue };
                if catch_ti != 0 && crate::mem::r8(catch_ti + 16) != 0 && offset != 0 {
                    copy_exception(c, info(rec, 1), (orig_frame as u32).wrapping_add(offset), cflags, ty, exc_base);
                }
            } else if cti != 0 {
                continue;
            }
            let params = [
                catch_trap() as u64,
                orig_frame,
                catch_level as i64 as u64,
                start as i64 as u64,
                0,
                d.code(handler),
                rec as u64,
                ctx as u64,
                if ret[0] != 0 { d.code(ret[0]) } else { 0 },
                if ret[1] != 0 { d.code(ret[1]) } else { 0 },
            ];
            unwind_to_catch(c, frame, d, &params);
            return;
        }
    }
}

fn frame_handler4(c: &mut Ctx) -> u32 {
    let (rec, frame, ctx, dp) = (c.a(0), c.a64(1), c.a(2), c.a(3));
    let d = Disp::read(dp);
    let level = TRYLEVEL4.with(|t| t.replace(-2));
    let header = crate::mem::r8(d.rva(r32(d.data)));
    if header & FH4_EHS != 0 && code(rec) != CXX_EXCEPTION && !is_consolidate(rec, FH4_PARAMS) && code(rec) != STATUS_LONGJUMP {
        return CONTINUE_SEARCH;
    }
    let Some(f) = Fh4::read(&d) else {
        eprintln!("sake: __CxxFrameHandler4: FuncInfo4 header {header:#x} not supported");
        return CONTINUE_SEARCH;
    };
    let orig_frame = if f.header & FH4_IS_CATCH != 0 { r64((frame as u32).wrapping_add(f.frame)) } else { frame };
    if flags(rec) & (EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND) != 0 {
        let mut last = -1;
        if flags(rec) & EXCEPTION_TARGET_UNWIND != 0 {
            if is_consolidate(rec, FH4_PARAMS) {
                last = info(rec, 3) as i32;
            } else if code(rec) == STATUS_LONGJUMP {
                last = f.ip_to_state(&d, d.target_ip);
            }
        }
        local_unwind4(c, orig_frame, &d, &f, level, last);
        return CONTINUE_SEARCH;
    }
    let noexcept = |c: &mut Ctx| {
        if f.header & FH4_IS_CATCH == 0 && code(rec) == CXX_EXCEPTION && f.header & FH4_NO_EXCEPT != 0 {
            crate::fatal(c.cpu, "noexcept function propagating an exception (terminate)");
        }
    };
    if f.try_map == 0 {
        noexcept(c);
        return CONTINUE_SEARCH;
    }
    let exc_type = if code(rec) == CXX_EXCEPTION { info(rec, 2) as u32 } else { 0 };
    if code(rec) != CXX_EXCEPTION && translate(c, rec, frame, ctx, dp) {
        // (the handler runs again: the catch funclet's state with it)
        TRYLEVEL4.with(|t| t.set(level));
        return 0;
    }
    find_catch4(c, rec, ctx, frame, &d, &f, exc_type, orig_frame, level);
    if c.jumped {
        return 0;
    }
    noexcept(c);
    CONTINUE_SEARCH
}

// ---------------------------------------------------------------------------------------------
// _set_se_translator

fn _set_se_translator(c: &mut Ctx) -> u32 {
    let at = crate::cur().teb + SE_TRANSLATOR_TEB;
    let old = r64(at);
    w64(at, c.a64(0));
    c.edx = Some((old >> 32) as u32);
    old as u32
}

/// A frame handler with try blocks has the structured exception `rec`: calls the thread's
/// translator (code, EXCEPTION_POINTERS*) in its place, unless there is none, the exception is
/// a breakpoint, or it returned for this frame already. Whether it jumped there.
fn translate(c: &mut Ctx, rec: u32, frame: u64, ctx: u32, dp: u32) -> bool {
    let teb = crate::cur().teb;
    let f = r64(teb + SE_TRANSLATOR_TEB);
    if r32(teb + SE_TRANSLATED_TEB) == rec && r64(teb + SE_TRANSLATED_TEB + 8) == frame {
        w32(teb + SE_TRANSLATED_TEB, 0);
        return false;
    }
    if f == 0 || code(rec) == STATUS_BREAKPOINT {
        return false;
    }
    if trace() {
        eprintln!("sake: x64 SE translator {f:#x} for {:08x} (frame {frame:#x})", code(rec));
    }
    let sp = c.esp - TRANSLATE_FRAME;
    w64(sp, TRANSLATE_TRAP as u64);
    w64(sp + 0x30, rec as u64);
    w64(sp + 0x38, ctx as u64);
    w64(sp + 0x40, dp as u64);
    w64(sp + 0x48, frame);
    c.cpu.set_gpr(1, code(rec) as u64);
    c.cpu.set_gpr(2, sp as u64 + 0x30);
    c.cpu.set_gpr(4, sp as u64);
    c.cpu.eip = f as u32;
    c.cpu.rip_hi = (f >> 32) as u32;
    c.jumped = true;
    true
}

/// TRANSLATE_TRAP: the translator returned. The handler again (the dispatcher context's), with
/// its arguments and stack as the dispatcher called it.
pub fn translated(cpu: &mut koji::Cpu) {
    let sp = cpu.gpr(4) as u32 - 8;
    let (rec, ctx, dp, frame) = (r64(sp + 0x30), r64(sp + 0x38), r64(sp + 0x40), r64(sp + 0x48));
    let teb = crate::cur().teb;
    w32(teb + SE_TRANSLATED_TEB, rec as u32);
    w64(teb + SE_TRANSLATED_TEB + 8, frame);
    for (r, v) in [(1, rec), (2, frame), (8, ctx), (9, dp)] {
        cpu.set_gpr(r, v);
    }
    cpu.set_gpr(4, (sp + TRANSLATE_FRAME) as u64);
    let h = r64(dp as u32 + 0x30);
    cpu.eip = h as u32;
    cpu.rip_hi = (h >> 32) as u32;
}

// ---------------------------------------------------------------------------------------------
// __C_specific_handler: SCOPE_TABLE { count, { begin, end, handler (filter / __finally),
// target (0: a __finally) }[] }

fn C_specific_handler(c: &mut Ctx) -> u32 {
    let (rec, frame, ctx, dp) = (c.a(0), c.a64(1), c.a(2), c.a(3));
    let d = Disp::read(dp);
    let table = d.data;
    let n = r32(table);
    let scope_index = dp + 0x48;
    let within = |i: u32, ip: u64| {
        let s = table + 4 + 16 * i;
        ip >= d.code(r32(s)) && ip < d.code(r32(s + 4))
    };
    if flags(rec) & (EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND) != 0 {
        for i in r32(scope_index)..n {
            let s = table + 4 + 16 * i;
            if !within(i, d.pc) || r32(s + 12) != 0 {
                continue;
            }
            if flags(rec) & EXCEPTION_TARGET_UNWIND != 0 && within(i, d.target_ip) {
                break;
            }
            w32(scope_index, i + 1);
            crate::call64(c.cpu, d.code(r32(s + 8)), &[1, frame]);
        }
        return CONTINUE_SEARCH;
    }
    for i in r32(scope_index)..n {
        let s = table + 4 + 16 * i;
        if !within(i, d.pc) || r32(s + 12) == 0 {
            continue;
        }
        let h = r32(s + 8);
        if h != 1 {
            // the filter: (EXCEPTION_POINTERS*, frame)
            let ptrs = ((c.esp - 0x40) & !15) - 16;
            w64(ptrs, rec as u64);
            w64(ptrs + 8, ctx as u64);
            match crate::call64(c.cpu, d.code(h), &[ptrs as u64, frame]) as i32 {
                1 => {}
                0 => continue,
                _ => return CONTINUE_EXECUTION,
            }
        }
        crate::seh64::unwind(c, frame, d.code(r32(s + 12)), rec, code(rec) as u64, d.ctx);
        return 0;
    }
    let _ = d.at;
    CONTINUE_SEARCH
}

// ---------------------------------------------------------------------------------------------

/// __current_exception / __current_exception_context: the thread's EXCEPTION_RECORD* and
/// CONTEXT* slots
fn __current_exception(_: &mut Ctx) -> u32 {
    crate::cur().teb + CUR_EXC_TEB
}
fn __current_exception_context(_: &mut Ctx) -> u32 {
    crate::cur().teb + CUR_EXC_TEB + 8
}
/// __uncaught_exceptions: none in flight between a throw and its catch outside a dispatch
fn __uncaught_exceptions(_: &mut Ctx) -> u32 {
    0
}

pub fn register() {
    for d in ["msvcrt", "ucrtbase", "vcruntime140", "vcruntime140_1"] {
        implement(d, "_CxxThrowException", throw);
        implement(d, "__CxxFrameHandler3", frame_handler3);
        implement(d, "__CxxFrameHandler", frame_handler3);
        implement(d, "__CxxFrameHandler4", frame_handler4);
        implement(d, "__C_specific_handler", C_specific_handler);
        implement(d, "__current_exception", __current_exception);
        implement(d, "__current_exception_context", __current_exception_context);
        implement(d, "__uncaught_exceptions", __uncaught_exceptions);
    }
    implement("vcruntime140", "_set_se_translator", _set_se_translator);
    implement("ucrtbase", "_set_se_translator", _set_se_translator);
    for d in ["msvcrt", "msvcr70", "msvcr71", "msvcr80", "msvcr90", "msvcr100", "msvcr110", "msvcr120"] {
        implement(d, "?_set_se_translator@@YAP6AXIPEAU_EXCEPTION_POINTERS@@@ZP6AXI0@Z@Z", _set_se_translator);
    }
    implement("ntdll", "__C_specific_handler", C_specific_handler);
    let _ = catch_trap();
}
