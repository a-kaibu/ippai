//! MSVC C++ exceptions (x86): _CxxThrowException raises 0xE06D7363 with the object and its
//! throw info; __CxxFrameHandler{,2,3} is each C++ function's SEH handler (EAX: the function's
//! FuncInfo). It finds the try block covering the frame's state whose catch matches the thrown
//! type, copies the object into the catch's frame, unwinds (other frames, then this one's
//! destructors), jumps into the catch funclet and continues where it returns (below: the catch
//! block runs as guest code). As Wine's msvcrt (except_i386.c); _set_se_translator as MSVC's.

use std::cell::RefCell;

use crate::api::{implement, Ctx};
use crate::mem::{cstr, r32, w32};
use koji::{EAX, EBP, ESP};

const CXX_EXCEPTION: u32 = 0xe06d_7363;
const MAGIC_VC6: u32 = 0x1993_0520;
const MAGIC_VC8: u32 = 0x1993_0522;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;

thread_local! {
    /// exceptions being handled (object, throw info): what `throw;` rethrows
    static CURRENT: RefCell<Vec<(u32, u32)>> = const { RefCell::new(Vec::new()) };
}

/// No exception being handled on this thread (save states: snap.rs).
pub fn idle() -> bool {
    CURRENT.with(|c| c.borrow().is_empty())
}

/// guest stubs: call `fn` with EBP set (catch and unwind funclets; returns EAX), and thiscall
/// with 0, 1 or 2 stack arguments
fn stubs() -> &'static [u32; 4] {
    static S: std::sync::OnceLock<[u32; 4]> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        [
            crate::api::stub(&[
                0x55, 0x53, 0x56, 0x57, //       push ebp, ebx, esi, edi
                0x8b, 0x44, 0x24, 0x14, //       mov eax, [esp+20]
                0x8b, 0x6c, 0x24, 0x18, //       mov ebp, [esp+24]
                0xff, 0xd0, //                   call eax
                0x5f, 0x5e, 0x5b, 0x5d, //       pop edi, esi, ebx, ebp
                0xc2, 0x08, 0x00, //             ret 8
            ]),
            crate::api::stub(&[
                0x8b, 0x44, 0x24, 0x04, //       mov eax, [esp+4]
                0x8b, 0x4c, 0x24, 0x08, //       mov ecx, [esp+8]
                0xff, 0xd0, //                   call eax
                0xc2, 0x08, 0x00, //             ret 8
            ]),
            crate::api::stub(&[
                0x8b, 0x44, 0x24, 0x04, //       mov eax, [esp+4]
                0x8b, 0x4c, 0x24, 0x08, //       mov ecx, [esp+8]
                0xff, 0x74, 0x24, 0x0c, //       push dword [esp+12]
                0xff, 0xd0, //                   call eax
                0xc2, 0x0c, 0x00, //             ret 12
            ]),
            crate::api::stub(&[
                0x8b, 0x44, 0x24, 0x04, //       mov eax, [esp+4]
                0x8b, 0x4c, 0x24, 0x08, //       mov ecx, [esp+8]
                0xff, 0x74, 0x24, 0x10, //       push dword [esp+16]
                0xff, 0x74, 0x24, 0x10, //       push dword [esp+16] (the one at +12 before)
                0xff, 0xd0, //                   call eax
                0xc2, 0x10, 0x00, //             ret 16
            ]),
        ]
    })
}

/// _CxxThrowException(object, throw info); (0, 0): rethrow the exception being handled
fn throw(c: &mut Ctx) -> u32 {
    let (mut obj, mut info) = (c.a(0), c.a(1));
    if obj == 0 && info == 0 {
        if let Some(&(o, i)) = CURRENT.with(|v| v.borrow().last().copied()).as_ref() {
            (obj, info) = (o, i);
        }
    }
    let ret = r32(c.esp);
    c.cpu.eip = ret;
    c.cpu.regs[ESP] = c.esp + 4 + 8;
    crate::seh::raise(c.cpu, CXX_EXCEPTION, 1, ret, &[MAGIC_VC6, obj, info]);
    c.jumped = true;
    0
}

/// Throws `obj` of ThrowInfo `info` from a trap (msvcp's _X* functions): as if the call had
/// returned there.
pub fn throw_object(c: &mut Ctx, obj: u32, info: u32) {
    let ret = r32(c.esp);
    c.cpu.eip = ret;
    c.cpu.regs[ESP] = c.esp + 4;
    crate::seh::raise(c.cpu, CXX_EXCEPTION, 1, ret, &[MAGIC_VC6, obj, info]);
    c.jumped = true;
}
/// The C++ exception being handled (its object and ThrowInfo), if any.
pub fn current() -> Option<(u32, u32)> {
    CURRENT.with(|v| v.borrow().last().copied())
}
/// throw; from a trap: the exception being handled again
pub fn rethrow(c: &mut Ctx) {
    let Some((obj, info)) = current() else {
        crate::fatal(c.cpu, "C++ rethrow without an exception (terminate)");
    };
    throw_object(c, obj, info);
}

/// this pointer of a base class (PMD: this offset, vbase descriptor offset, vbase offset)
fn this_ptr(pmd: u32, obj: u32) -> u32 {
    if obj == 0 {
        return 0;
    }
    let (mdisp, pdisp, vdisp) = (r32(pmd), r32(pmd + 4) as i32, r32(pmd + 8));
    let mut o = obj;
    if pdisp >= 0 {
        o = o.wrapping_add(pdisp as u32);
        o = o.wrapping_add(r32(r32(o).wrapping_add(vdisp)));
    }
    o.wrapping_add(mdisp)
}

/// The catchable type of the thrown `info` that a catch of `catch_ti` (0: `...`) with
/// `catch_flags` takes.
fn caught_type(info: u32, catch_ti: u32, catch_flags: u32) -> Option<u32> {
    let table = r32(info + 12);
    for i in 0..r32(table) {
        let ty = r32(table + 4 + 4 * i);
        let ti = r32(ty + 4);
        if catch_ti == 0 {
            return Some(ty);
        }
        if catch_ti != ti && cstr(catch_ti + 8) != cstr(ti + 8) {
            continue;
        }
        let f = r32(info);
        if (f & 1 != 0 && catch_flags & 1 == 0) || (f & 2 != 0 && catch_flags & 2 == 0) {
            continue;
        }
        return Some(ty);
    }
    None
}

/// copies the thrown object to `dest` as the catch declares it
fn copy_object(c: &mut Ctx, obj: u32, dest: u32, catch_flags: u32, ty: u32) {
    let (flags, size, ctor) = (r32(ty), r32(ty + 20), r32(ty + 24));
    if catch_flags & 8 != 0 {
        w32(dest, this_ptr(ty + 8, obj)); // reference
    } else if flags & 1 != 0 {
        crate::mem::copy(dest, obj, size); // simple type
        if size == 4 {
            w32(dest, this_ptr(ty + 8, r32(dest)));
        }
    } else if ctor != 0 {
        let src = this_ptr(ty + 8, obj);
        let s = stubs();
        if flags & 4 != 0 {
            c.call(s[3], &[ctor, dest, src, 1]);
        } else {
            c.call(s[2], &[ctor, dest, src]);
        }
    } else {
        crate::mem::copy(dest, this_ptr(ty + 8, obj), size);
    }
}

/// runs the frame's unwind funclets from its state down to `last`
fn local_unwind(c: &mut Ctx, frame: u32, descr: u32, last: i32) {
    let (count, table) = (r32(descr + 4) as i32, r32(descr + 8));
    let mut level = r32(frame + 8) as i32;
    while level != last {
        if level < 0 || level >= count {
            break;
        }
        let e = table + 8 * level as u32;
        let f = r32(e + 4);
        // (the state first: a throw from a destructor must not run it again)
        level = r32(e) as i32;
        w32(frame + 8, level as u32);
        if f != 0 {
            c.call(stubs()[0], &[f, frame + 12]);
        }
    }
    w32(frame + 8, last as u32);
}

/// __CxxFrameHandler{,2,3}(rec, frame, context, dispatch), EAX = FuncInfo
fn frame_handler(c: &mut Ctx) -> u32 {
    let descr = c.cpu.regs[EAX];
    let (rec, frame) = (c.a(0), c.a(1));
    let magic = r32(descr) & 0x1fff_ffff;
    if !(MAGIC_VC6..=MAGIC_VC8).contains(&magic) {
        return 1;
    }
    let code = r32(rec);
    let cxx = code == CXX_EXCEPTION && r32(rec + 16) == 3;
    // (/EHs: C++ exceptions only)
    if magic >= MAGIC_VC8 && r32(descr + 32) & 1 != 0 && !cxx {
        return 1;
    }
    if r32(rec + 4) & 6 != 0 {
        if r32(descr + 4) != 0 {
            local_unwind(c, frame, descr, -1);
        }
        return 1;
    }
    if r32(descr + 12) == 0 {
        return 1;
    }
    if !cxx && translate(c, rec, frame, descr) {
        return 0;
    }
    if find_catch(c, rec, frame, descr) {
        return 0;
    }
    1
}

/// Catches exception `rec` in `frame` (FuncInfo `descr`) if one of its try blocks takes it:
/// unwinds, calls the catch funclet and jumps where it returns. Whether it did.
fn find_catch(c: &mut Ctx, rec: u32, frame: u32, descr: u32) -> bool {
    let cxx = r32(rec) == CXX_EXCEPTION && r32(rec + 16) == 3;
    let (tries, try_table) = (r32(descr + 12), r32(descr + 16));
    let (obj, info) = if cxx { (r32(rec + 24), r32(rec + 28)) } else { (0, 0) };
    let level = r32(frame + 8) as i32;
    for i in 0..tries {
        let t = try_table + 20 * i;
        let (start, end) = (r32(t) as i32, r32(t + 4) as i32);
        if level < start || level > end {
            continue;
        }
        let (n, blocks) = (r32(t + 12), r32(t + 16));
        let mut handler = 0;
        for k in 0..n {
            let b = blocks + 16 * k;
            let (cflags, cti, off, h) = (r32(b), r32(b + 4), r32(b + 8), r32(b + 12));
            if cxx && info != 0 {
                let Some(ty) = caught_type(info, cti, cflags) else { continue };
                if cti != 0 && crate::mem::r8(cti + 8) != 0 && off != 0 {
                    copy_object(c, obj, frame + 12 + off, cflags, ty);
                }
            } else if cti != 0 {
                continue; // not a C++ exception: catch (...) only
            }
            handler = h;
            break;
        }
        if handler == 0 {
            continue;
        }
        if crate::env_on!("SAKE_TRACE_SEH") {
            eprintln!("sake: C++ catch in frame {frame:#x} (level {level}, try {start}..{end}) at {handler:#x}");
        }
        // unwind the frames above, this one down to the try block, then call the catch
        let unwind = crate::api::export("kernel32", "RtlUnwind");
        let before = UNWIND.with(|u| u.replace((frame, t)));
        c.call(unwind, &[frame, 0, rec, 0]);
        UNWIND.with(|u| u.set(before));
        relink_kept();
        local_unwind(c, frame, descr, start);
        w32(frame + 8, (end + 1) as u32);
        enter_catch(c, handler, frame, t, obj, info);
        return true;
    }
    false
}

// ---------------------------------------------------------------------------------------------
// the catch block, run as guest code
//
// The catch funclet is entered by a jump, not a nested host call: a catch block that throws
// or rethrows (`catch (...) { cleanup(); throw; }`) to a handler further out leaves no host
// frame behind (a nested run that never returned kept find_catch's frame on the host stack,
// and its exception counted as being handled for good; inside a window procedure, the
// callback's return later ended that stale run and went on in its old registers). While the
// catch block runs, a registration of CATCH_GUARD on fs:[0] sees an exception leave it.

/// The catch funclet's frame, below the frame handler's stack: stubs()[0]'s return
/// (CATCH_RET), its arguments (the funclet, EBP), then at CATCH_REG the registration while the
/// catch block runs: next, CATCH_GUARD, the function's EH frame, its saved ESP, the object, its
/// ThrowInfo and the try block's entry (tryLow, tryHigh, catchHigh, ...).
const CATCH_REG: u32 = 12;
const CATCH_FRAME: u32 = CATCH_REG + 28;

thread_local! {
    /// RtlUnwind of find_catch: the EH frame and the try block entry that catch the exception
    static UNWIND: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
    /// catch guards that unwind passed but whose catch block goes on (find_catch puts them back)
    static KEPT: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// Whether try block `inner` (an entry of the try map) lies in the catch blocks of try block
/// `outer` of the same function: a try written inside a catch block has states after the
/// try's and up to its catch blocks' last.
fn inside_catch(outer: u32, inner: u32) -> bool {
    let (hi, catch_hi) = (r32(outer + 4) as i32, r32(outer + 8) as i32);
    let (lo2, hi2) = (r32(inner) as i32, r32(inner + 4) as i32);
    hi < lo2 && hi2 <= catch_hi
}

/// After find_catch's RtlUnwind: the catch guards it passed whose catch block is not left (the
/// exception is caught inside it) go back on fs:[0], above the catching frame, in their order.
fn relink_kept() {
    let kept = KEPT.with(|k| std::mem::take(&mut *k.borrow_mut()));
    let teb = crate::cur().teb;
    for reg in kept.into_iter().rev() {
        w32(reg, r32(teb));
        w32(teb, reg);
    }
}

fn catch_ret_fn() -> u32 {
    static F: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *F.get_or_init(|| crate::api::host_fn("cxx::catch_ret", catch_ret, 0))
}
fn catch_guard_fn() -> u32 {
    static F: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *F.get_or_init(|| crate::api::host_fn("cxx::catch_guard", catch_guard, 0))
}

/// Jumps into catch funclet `handler` of EH frame `frame` (registration at `frame`, EBP at
/// frame + 12) and try block entry `t`, which handles (`obj`, `info`; 0, 0: not a C++
/// exception).
fn enter_catch(c: &mut Ctx, handler: u32, frame: u32, t: u32, obj: u32, info: u32) {
    let teb = crate::cur().teb;
    let sp = (c.cpu.regs[ESP] - CATCH_FRAME) & !3;
    let reg = sp + CATCH_REG;
    w32(sp, catch_ret_fn());
    w32(sp + 4, handler);
    w32(sp + 8, frame + 12);
    for (i, v) in [r32(teb), catch_guard_fn(), frame, r32(frame - 4), obj, info, t].into_iter().enumerate() {
        w32(reg + 4 * i as u32, v);
    }
    w32(teb, reg);
    CURRENT.with(|v| v.borrow_mut().push((obj, info)));
    c.cpu.regs[ESP] = sp;
    c.cpu.eip = stubs()[0];
    c.jumped = true;
}

/// Whether an outer catch block still handles `obj` (a `throw;` caught again inside it): its
/// end destroys it, not this one's.
fn in_use(obj: u32) -> bool {
    obj != 0 && CURRENT.with(|v| v.borrow().iter().any(|&(o, _)| o == obj))
}

/// The catch block is done with the exception (`obj` of `info`): its destructor, unless an
/// outer catch block still has it (the catch has its own copy).
fn destroy(c: &mut Ctx, obj: u32, info: u32) {
    if info != 0 && r32(info + 4) != 0 && !in_use(obj) {
        c.call(stubs()[1], &[r32(info + 4), obj]);
    }
}

/// CATCH_RET: the catch funclet returned where the function goes on (EAX), the stack at its
/// registration. Off fs:[0], the exception destroyed, then a jump there with the function's
/// ESP and EBP.
fn catch_ret(c: &mut Ctx) -> u32 {
    let reg = c.cpu.regs[ESP];
    let cont = c.cpu.regs[EAX];
    let teb = crate::cur().teb;
    let [next, _, frame, save_esp, obj, info, _] = std::array::from_fn(|i| r32(reg + 4 * i as u32));
    w32(teb, next);
    CURRENT.with(|v| v.borrow_mut().pop());
    w32(frame - 4, save_esp);
    destroy(c, obj, info);
    c.cpu.regs[ESP] = save_esp;
    c.cpu.regs[EBP] = frame + 12;
    c.cpu.eip = cont;
    c.jumped = true;
    0
}

/// CATCH_GUARD (rec, registration, context, dispatcher): an exception leaves the catch block
/// (a handler further out unwinds past it): the exception it handled is no longer being
/// handled, and goes unless the one leaving is that very object (`throw;`). A try block inside
/// the catch block catching it (its function's frame is below this registration on fs:[0])
/// leaves the catch block running: the guard stays.
fn catch_guard(c: &mut Ctx) -> u32 {
    let (rec, reg) = (c.a(0), c.a(1));
    if r32(rec + 4) & 6 == 0 {
        return 1;
    }
    let (to, t) = UNWIND.with(|u| u.get());
    if to != 0 && to == r32(reg + 8) && inside_catch(r32(reg + 24), t) {
        KEPT.with(|k| k.borrow_mut().push(reg));
        return 1;
    }
    let (obj, info) = (r32(reg + 16), r32(reg + 20));
    // (the function's saved ESP as before the catch block: its funclet keeps its own there, which
    // a catch further out in the same function would continue with)
    w32(r32(reg + 8) - 4, r32(reg + 12));
    CURRENT.with(|v| v.borrow_mut().pop());
    let rethrown = r32(rec) == CXX_EXCEPTION && r32(rec + 16) == 3 && r32(rec + 24) == obj && obj != 0;
    if !rethrown {
        destroy(c, obj, info);
    }
    1
}

// ---------------------------------------------------------------------------------------------
// _set_se_translator

/// _set_se_translator's function, per thread (the TEB's free page; x64 the same).
pub const SE_TRANSLATOR_TEB: u32 = crate::crt::ERRNO_TEB + 0x20;
/// The exception (record, frame) whose translator returned: its handler, called again, goes
/// on without it.
const SE_TRANSLATED_TEB: u32 = SE_TRANSLATOR_TEB + 8;
/// The translator's call below the handler's return address: TRANSLATE_TRAP, its arguments
/// (code, EXCEPTION_POINTERS*), at 0x10 a registration of the guard (next, handler, then the
/// FuncInfo and the frame at 0x20), at 0x18 the EXCEPTION_POINTERS, at 0x28 the handler's EBX,
/// EBP, ESI and EDI (the dispatcher's, which the guard gives back).
const TRANSLATE_FRAME: u32 = 0x40;
const SAVED: [usize; 4] = [koji::EBX, EBP, koji::ESI, koji::EDI];

fn _set_se_translator(c: &mut Ctx) -> u32 {
    let at = crate::cur().teb + SE_TRANSLATOR_TEB;
    let old = r32(at);
    w32(at, c.a(0));
    old
}

/// As cxx64's: the frame handler of `frame` (with try blocks) has the structured exception
/// `rec`: jumps into the thread's translator, returning to TRANSLATE_TRAP, with the guard on
/// top of the chain (a C++ exception it throws is looked for in `frame` alone, as MSVC's
/// TranslatorGuardHandler does). Whether it jumped.
fn translate(c: &mut Ctx, rec: u32, frame: u32, descr: u32) -> bool {
    let teb = crate::cur().teb;
    let f = r32(teb + SE_TRANSLATOR_TEB);
    if r32(teb + SE_TRANSLATED_TEB) == rec && r32(teb + SE_TRANSLATED_TEB + 4) == frame {
        w32(teb + SE_TRANSLATED_TEB, 0);
        return false;
    }
    if f == 0 || r32(rec) == STATUS_BREAKPOINT {
        return false;
    }
    if crate::env_on!("SAKE_TRACE_SEH") {
        eprintln!("sake: SE translator {f:#x} for {:08x} (frame {frame:#x})", r32(rec));
    }
    let sp = c.esp - TRANSLATE_FRAME;
    let guard = sp + 0x10;
    w32(sp, crate::cxx64::TRANSLATE_TRAP);
    w32(sp + 4, r32(rec));
    w32(sp + 8, sp + 0x18);
    w32(guard, r32(teb));
    w32(guard + 4, guard_fn());
    w32(sp + 0x18, rec);
    w32(sp + 0x1c, c.a(2));
    w32(sp + 0x20, descr);
    w32(sp + 0x24, frame);
    for (i, r) in SAVED.iter().enumerate() {
        w32(sp + 0x28 + 4 * i as u32, c.cpu.regs[*r]);
    }
    w32(teb, guard);
    c.cpu.regs[ESP] = sp;
    c.cpu.eip = f;
    c.jumped = true;
    true
}

fn guard_fn() -> u32 {
    static G: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *G.get_or_init(|| crate::api::host_fn("cxx::se_guard", guard, 0))
}

/// Takes the guard off the chain and continues in the frame handler that called the
/// translator, at its return (`verdict`) or (None) at its start again.
fn back_to_handler(cpu: &mut koji::Cpu, guard: u32, verdict: Option<u32>) {
    let sp = guard - 0x10;
    let (rec, frame) = (r32(sp + 0x18), r32(sp + 0x24));
    let teb = crate::cur().teb;
    w32(teb, r32(guard));
    let entry = sp + TRANSLATE_FRAME;
    for (i, r) in SAVED.iter().enumerate() {
        cpu.regs[*r] = r32(sp + 0x28 + 4 * i as u32);
    }
    match verdict {
        Some(v) => {
            cpu.regs[EAX] = v;
            cpu.eip = r32(entry);
            cpu.regs[ESP] = entry + 4;
        }
        None => {
            w32(teb + SE_TRANSLATED_TEB, rec);
            w32(teb + SE_TRANSLATED_TEB + 4, frame);
            // (the registration's handler: the function's thunk that loads EAX and jumps to
            // __CxxFrameHandler)
            cpu.regs[ESP] = entry;
            cpu.eip = r32(frame + 4);
        }
    }
}

/// TRANSLATE_TRAP: the translator returned (ESP past its return address): the handler again.
pub fn translated(cpu: &mut koji::Cpu) {
    let sp = cpu.regs[ESP] - 4;
    back_to_handler(cpu, sp + 0x10, None);
}

/// The guard's handler (rec, registration, context, dispatcher): a C++ exception from the
/// translator is caught in the frame the translator was called for, or else forgotten: the
/// translator's frames are unwound and that frame's handler says "continue search" for the
/// structured exception.
fn guard(c: &mut Ctx) -> u32 {
    let (rec, reg) = (c.a(0), c.a(1));
    if r32(rec + 4) & 6 != 0 || r32(rec) != CXX_EXCEPTION {
        return 1;
    }
    let (descr, frame) = (r32(reg + 0x10), r32(reg + 0x14));
    if find_catch(c, rec, frame, descr) {
        return 0;
    }
    let unwind = crate::api::export("kernel32", "RtlUnwind");
    c.call(unwind, &[reg, 0, rec, 0]);
    let info = r32(rec + 28);
    if r32(rec + 16) == 3 && info != 0 && r32(info + 4) != 0 {
        c.call(stubs()[1], &[r32(info + 4), r32(rec + 24)]);
    }
    back_to_handler(c.cpu, reg, Some(1));
    c.jumped = true;
    0
}

pub fn init() {
    if crate::mem::x64() {
        return crate::cxx64::register();
    }
    for d in ["msvcrt", "ucrtbase"] {
        implement(d, "_CxxThrowException", throw);
        for n in ["__CxxFrameHandler", "__CxxFrameHandler2", "__CxxFrameHandler3"] {
            implement(d, n, frame_handler);
        }
    }
    for d in ["msvcrt", "msvcr70", "msvcr71", "msvcr80", "msvcr90", "msvcr100", "msvcr110", "msvcr120"] {
        implement(d, "?_set_se_translator@@YAP6AXIPAU_EXCEPTION_POINTERS@@@ZP6AXI0@Z@Z", _set_se_translator);
    }
    for d in ["ucrtbase", "vcruntime140"] {
        implement(d, "_set_se_translator", _set_se_translator);
    }
}
