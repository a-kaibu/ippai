//! Fibers (CreateFiber, SwitchToFiber, ConvertThreadToFiber, ...) and fiber local storage
//! (FlsAlloc / RtlFlsAlloc and the rest) in Windows 10's layout, which programs and Wine's
//! kernel32:fiber read directly.
//!
//! FLS: the process has a GLOBAL_FLS_DATA { FLS_INFO_CHUNK *callback_chunks[8]; LIST_ENTRY
//! head; ULONG high_index }, each fiber (TEB->FlsSlots) a TEB_FLS_DATA { LIST_ENTRY entry;
//! void **data_chunks[8] } linked into that list. Chunk j holds 0x10 << j indexes (index 0,
//! chunk 0's first, is reserved), 4079 indexes in all (1..=0xfef). A callback chunk is
//! { ULONG count (allocated indexes); { void *unknown; callback } [n] } and an index is free
//! while its callback is NULL (an index allocated without one has ~0); a data chunk is
//! void *[n + 1], index k's value at [k + 1]. All of it lives on the process heap (HeapSize
//! gives the sizes Windows does).
//!
//! A fiber is a heap block whose first pointer is its parameter (GetFiberData reads
//! *TEB->FiberData); the rest is what SwitchToFiber swaps in the TEB (exception list, stack
//! bounds, FLS data, activation context stack) and the fiber's registers as a CONTEXT.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, ps, r32, rp, w32, wp};
use std::sync::Mutex;

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_NO_MEMORY: u32 = 0xC000_0017;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_ALREADY_FIBER: u32 = 1280;
const ERROR_ALREADY_THREAD: u32 = 1281;
const FLS_OUT_OF_INDEXES: u32 = 0xffff_ffff;

/// The highest FLS index (the 8 chunks hold 0x10 + 0x20 + .. + 0x800 = 0xff0 with index 0)
pub const FLS_MAX: u32 = 0xfef;
const CHUNKS: u32 = 8;
/// An allocated index without a callback
const NO_CALLBACK: u32 = u32::MAX;

pub fn chunk_size(j: u32) -> u32 {
    0x10 << j
}

/// FLS index -> (chunk, index in the chunk)
pub fn chunk_of(index: u32) -> (u32, u32) {
    let (mut j, mut k) = (0, index);
    while k >= chunk_size(j) {
        k -= chunk_size(j);
        j += 1;
    }
    (j, k)
}

/// (the guest-visible lists are changed under this; callbacks run outside it)
static FLS: Mutex<u32> = Mutex::new(0);

fn callback_mark(cb: u32) -> u32 {
    if cb == 0 { NO_CALLBACK } else { cb }
}

/// The GLOBAL_FLS_DATA (made on first use)
fn global(g: &mut u32) -> u32 {
    if *g == 0 {
        let p = ps();
        let b = mem::heap_alloc(10 * p + 4, true);
        // the list head points at itself
        wp(b + 8 * p, b + 8 * p);
        wp(b + 9 * p, b + 8 * p);
        *g = b;
    }
    *g
}
fn head(g: u32) -> u32 {
    g + 8 * ps()
}
fn high_slot(g: u32) -> u32 {
    g + 10 * ps()
}
/// callbacks[k] of callback chunk `c`: (unknown, callback) addresses
fn cb_slot(c: u32, k: u32) -> u32 {
    let p = ps();
    c + p + 2 * p * k + p
}
/// Callback chunk j (0 none)
fn cb_chunk(g: u32, j: u32) -> u32 {
    rp(g + j * ps())
}

/// Reads a pointer-sized callback, as 32 bits (~0 stays ~0 on x64)
fn read_cb(a: u32) -> u32 {
    if mem::x64() {
        let v = mem::r64(a);
        if v == u64::MAX { NO_CALLBACK } else { v as u32 }
    } else {
        r32(a)
    }
}
fn write_cb(a: u32, v: u32) {
    if mem::x64() {
        mem::w64(a, if v == NO_CALLBACK { u64::MAX } else { v as u64 });
    } else {
        w32(a, v)
    }
}

/// The TEB_FLS_DATA list entries, first to last
fn entries(g: u32) -> Vec<u32> {
    let h = head(g);
    let mut v = Vec::new();
    let mut e = rp(h);
    while e != h && e != 0 && v.len() < 1 << 20 {
        v.push(e);
        e = rp(e);
    }
    v
}

/// RtlFlsAlloc: the lowest free index gets `cb`
pub fn alloc(cb: u32) -> Result<u32, u32> {
    let mut g = FLS.lock().unwrap();
    let g = global(&mut g);
    let p = ps();
    for j in 0..CHUNKS {
        let n = chunk_size(j);
        let mut c = cb_chunk(g, j);
        if c == 0 {
            c = mem::heap_alloc(p + 2 * p * n, true);
            if c == 0 {
                return Err(STATUS_NO_MEMORY);
            }
            if j == 0 {
                // (index 0 is never handed out)
                write_cb(cb_slot(c, 0), NO_CALLBACK);
                w32(c, 1);
            }
            wp(g + j * p, c);
        }
        if r32(c) >= n {
            continue;
        }
        for k in 0..n {
            let s = cb_slot(c, k);
            if read_cb(s) == 0 {
                write_cb(s, callback_mark(cb));
                wp(s - p, 0);
                w32(c, r32(c) + 1);
                let index = (0..j).map(chunk_size).sum::<u32>() + k;
                if index > r32(high_slot(g)) {
                    w32(high_slot(g), index);
                }
                return Ok(index);
            }
        }
    }
    Err(STATUS_NO_MEMORY)
}

/// RtlFlsFree: every fiber's value of `index` goes, through the callback when it has one.
/// Returns the (callback, value) calls to make.
pub fn free(index: u32) -> Result<Vec<(u32, u32)>, u32> {
    if index == 0 || index > FLS_MAX {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let mut g = FLS.lock().unwrap();
    let g = global(&mut g);
    let (j, k) = chunk_of(index);
    let c = cb_chunk(g, j);
    if c == 0 || read_cb(cb_slot(c, k)) == 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let cb = read_cb(cb_slot(c, k));
    let mut calls = Vec::new();
    for e in entries(g) {
        let d = rp(e + (2 + j) * ps());
        if d == 0 {
            continue;
        }
        let a = d + (k + 1) * ps();
        let v = rp(a);
        if v != 0 {
            if cb != NO_CALLBACK {
                calls.push((cb, v));
            }
            wp(a, 0);
        }
    }
    write_cb(cb_slot(c, k), 0);
    w32(c, r32(c).saturating_sub(1));
    Ok(calls)
}

/// RtlFlsGetValue with the TEB's FlsSlots at `slots`
pub fn get(slots: u32, index: u32) -> Result<u32, u32> {
    if index == 0 || index > FLS_MAX {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let f = rp(slots);
    if f == 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let (j, k) = chunk_of(index);
    let d = rp(f + (2 + j) * ps());
    if d == 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    Ok(rp(d + (k + 1) * ps()))
}

/// RtlFlsSetValue with the TEB's FlsSlots at `slots` (the fiber's TEB_FLS_DATA and the chunk
/// are made as needed)
pub fn set(slots: u32, index: u32, v: u32) -> Result<(), u32> {
    if index == 0 || index > FLS_MAX {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let p = ps();
    let mut f = rp(slots);
    if f == 0 {
        f = mem::heap_alloc((2 + CHUNKS) * p, true);
        if f == 0 {
            return Err(STATUS_NO_MEMORY);
        }
        let mut g = FLS.lock().unwrap();
        let g = global(&mut g);
        // InsertTailList
        let h = head(g);
        let last = rp(h + p);
        wp(f, h);
        wp(f + p, last);
        wp(last, f);
        wp(h + p, f);
        wp(slots, f);
    }
    let (j, k) = chunk_of(index);
    let mut d = rp(f + (2 + j) * p);
    if d == 0 {
        d = mem::heap_alloc((chunk_size(j) + 1) * p, true);
        if d == 0 {
            return Err(STATUS_NO_MEMORY);
        }
        wp(f + (2 + j) * p, d);
    }
    wp(d + (k + 1) * p, v);
    Ok(())
}

/// RtlProcessFlsData(fls, flags): 1 takes it off the list and returns its callbacks to make
/// (its values cleared), 2 frees it.
pub fn process(f: u32, flags: u32) -> Vec<(u32, u32)> {
    let mut calls = Vec::new();
    if f == 0 {
        return calls;
    }
    let p = ps();
    if flags & 1 != 0 {
        let mut g = FLS.lock().unwrap();
        let g = global(&mut g);
        // RemoveEntryList (the entry's own links stay)
        let (next, prev) = (rp(f), rp(f + p));
        if next != 0 && prev != 0 {
            wp(prev, next);
            wp(next + p, prev);
        }
        for j in 0..CHUNKS {
            let (d, c) = (rp(f + (2 + j) * p), cb_chunk(g, j));
            if d == 0 || c == 0 {
                continue;
            }
            for k in 0..chunk_size(j) {
                let a = d + (k + 1) * p;
                let v = rp(a);
                let cb = read_cb(cb_slot(c, k));
                if v != 0 && cb != 0 && cb != NO_CALLBACK {
                    calls.push((cb, v));
                    wp(a, 0);
                }
            }
        }
    }
    if flags & 2 != 0 {
        for j in 0..CHUNKS {
            let d = rp(f + (2 + j) * p);
            if d != 0 {
                mem::heap_free(d);
            }
        }
        mem::heap_free(f);
    }
    calls
}

fn run_callbacks(c: &mut Ctx, calls: Vec<(u32, u32)>) {
    for (cb, v) in calls {
        c.call(cb, &[v]);
    }
}

fn teb() -> u32 {
    crate::cur().teb
}

struct Teb {
    fiber_data: u32,
    stack_base: u32,
    stack_limit: u32,
    dealloc: u32,
    fls: u32,
    actctx: u32,
    flags: u32,
}
fn offs() -> Teb {
    if mem::x64() {
        Teb { fiber_data: 0x20, stack_base: 8, stack_limit: 0x10, dealloc: 0x1478, fls: 0x17c8, actctx: 0x2c8, flags: 0x17ee }
    } else {
        Teb { fiber_data: 0x10, stack_base: 4, stack_limit: 8, dealloc: 0xe0c, fls: 0xfb4, actctx: 0x1a8, flags: 0xfca }
    }
}
/// SameTebFlags.HasFiberData
const HAS_FIBER_DATA: u16 = 4;

/// TEB FlsSlots of the running thread
fn slots() -> u32 {
    teb() + offs().fls
}

/// The fiber block: param, exception list, stack base, limit, deallocation stack, FLS data,
/// activation context stack, its own stack's allocation (0: a converted thread's), then the
/// CONTEXT at CTX.
const F_EXCEPT: u32 = 1;
const F_BASE: u32 = 2;
const F_LIMIT: u32 = 3;
const F_DEALLOC: u32 = 4;
const F_FLS: u32 = 5;
const F_ACTCTX: u32 = 6;
const F_OWN: u32 = 7;
const CTX: u32 = 0x50;

fn ctx_size() -> u32 {
    if mem::x64() { crate::seh64::CONTEXT_SIZE } else { crate::seh::CONTEXT_SIZE }
}
fn new_block() -> u32 {
    mem::heap_alloc(CTX + ctx_size(), true)
}

pub fn is_fiber() -> bool {
    mem::r16(teb() + offs().flags) & HAS_FIBER_DATA != 0
}
fn set_fiber_flag(on: bool) {
    let a = teb() + offs().flags;
    let v = mem::r16(a);
    mem::w16(a, if on { v | HAS_FIBER_DATA } else { v & !HAS_FIBER_DATA });
}

/// The TEB's state into fiber `f`
fn save_teb(f: u32) {
    let (t, o, p) = (teb(), offs(), ps());
    if !mem::x64() {
        wp(f + F_EXCEPT * p, r32(t));
    }
    wp(f + F_BASE * p, rp(t + o.stack_base));
    wp(f + F_LIMIT * p, rp(t + o.stack_limit));
    wp(f + F_DEALLOC * p, rp(t + o.dealloc));
    wp(f + F_FLS * p, rp(t + o.fls));
    wp(f + F_ACTCTX * p, rp(t + o.actctx));
}
fn load_teb(f: u32) {
    let (t, o, p) = (teb(), offs(), ps());
    if !mem::x64() {
        w32(t, rp(f + F_EXCEPT * p));
    }
    wp(t + o.stack_base, rp(f + F_BASE * p));
    wp(t + o.stack_limit, rp(f + F_LIMIT * p));
    wp(t + o.dealloc, rp(f + F_DEALLOC * p));
    wp(t + o.fls, rp(f + F_FLS * p));
    wp(t + o.actctx, rp(f + F_ACTCTX * p));
    wp(t + o.fiber_data, f);
}

/// ConvertThreadToFiberEx(param, flags)
fn ConvertThreadToFiberEx(c: &mut Ctx) -> u32 {
    if is_fiber() {
        last_error(ERROR_ALREADY_FIBER);
        return 0;
    }
    let f = new_block();
    if f == 0 {
        last_error(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    }
    wp(f, c.a(0));
    save_teb(f);
    wp(teb() + offs().fiber_data, f);
    set_fiber_flag(true);
    f
}
fn ConvertThreadToFiber(c: &mut Ctx) -> u32 {
    ConvertThreadToFiberEx(c)
}

/// ConvertFiberToThread: the running fiber's block goes (its stack, FLS data and activation
/// contexts stay the thread's)
fn ConvertFiberToThread(_: &mut Ctx) -> u32 {
    if !is_fiber() {
        last_error(ERROR_ALREADY_THREAD);
        return 0;
    }
    let a = teb() + offs().fiber_data;
    let f = rp(a);
    wp(a, 0);
    set_fiber_flag(false);
    if f != 0 {
        mem::heap_free(f);
    }
    1
}

/// CreateFiberEx(commit, reserve, flags, start, param): a fiber that runs start(param) on its
/// own stack the first time it is switched to (returning from it ends the thread)
fn CreateFiberEx(c: &mut Ctx) -> u32 {
    let (reserve, start, param) = (c.a(1).max(c.a(0)), c.a(3), c.a(4));
    create(c, reserve, start, param)
}
/// CreateFiber(stack size, start, param)
fn CreateFiber(c: &mut Ctx) -> u32 {
    let (size, start, param) = (c.a(0), c.a(1), c.a(2));
    create(c, size, start, param)
}

fn create(c: &mut Ctx, size: u32, start: u32, param: u32) -> u32 {
    let size = (if size == 0 { 0x10_0000 } else { size.max(0x1_0000) }).saturating_add(0xffff) & !0xffff;
    let Some(st) = mem::reserve(0, size, 4, 0x20000, true) else {
        last_error(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    };
    mem::commit(st, 4);
    let f = new_block();
    if f == 0 {
        mem::release(st);
        last_error(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    }
    let (p, top) = (ps(), st + size);
    wp(f, param);
    if !mem::x64() {
        wp(f + F_EXCEPT * p, 0xffff_ffff);
    }
    wp(f + F_BASE * p, top);
    wp(f + F_LIMIT * p, st);
    wp(f + F_DEALLOC * p, st);
    wp(f + F_OWN * p, st);
    // the registers: the creator's (floating-point state included), entering start(param)
    // with the thread's exit as the return address
    let ctx = f + CTX;
    if mem::x64() {
        crate::seh64::save_context(c.cpu, ctx);
        let sp = top - 0x28;
        mem::w64(sp, crate::api::EXIT_TRAP as u64);
        mem::w64(ctx + 0x80, param as u64); // Rcx
        mem::w64(ctx + 0x98, sp as u64); // Rsp
        mem::w64(ctx + 0xa0, 0); // Rbp
        mem::w64(ctx + 0xf8, start as u64); // Rip
    } else {
        crate::seh::save_context(c.cpu, ctx);
        let sp = top - 16;
        w32(sp, crate::api::EXIT_TRAP);
        w32(sp + 4, param);
        w32(ctx + 0xb4, 0); // Ebp
        w32(ctx + 0xb8, start); // Eip
        w32(ctx + 0xc4, sp); // Esp
    }
    f
}

/// SwitchToFiber(fiber): the running fiber stops here (it goes on by returning from this
/// call) and `fiber` goes on where it stopped
fn SwitchToFiber(c: &mut Ctx) -> u32 {
    let to = c.a(0);
    let o = offs();
    let cur = rp(teb() + o.fiber_data);
    if to == cur || to == 0 {
        return 0;
    }
    if cur != 0 && cur != 0x1e00 {
        save_teb(cur);
        let ctx = cur + CTX;
        if mem::x64() {
            crate::seh64::save_context(c.cpu, ctx);
            mem::w64(ctx + 0xf8, mem::r64(c.esp)); // Rip: the return address
            mem::w64(ctx + 0x98, c.esp as u64 + 8); // Rsp
        } else {
            crate::seh::save_context(c.cpu, ctx);
            w32(ctx + 0xb8, r32(c.esp));
            w32(ctx + 0xc4, c.esp + 8);
        }
    }
    load_teb(to);
    if mem::x64() {
        crate::seh64::load_context(c.cpu, to + CTX);
    } else {
        crate::seh::load_context(c.cpu, to + CTX);
    }
    c.jumped = true;
    0
}

/// DeleteFiber(fiber): the running one ends the thread; another one's FLS values go through
/// their callbacks, then its stack and block
fn DeleteFiber(c: &mut Ctx) -> u32 {
    let f = c.a(0);
    if f == 0 {
        return 0;
    }
    if f == rp(teb() + offs().fiber_data) {
        crate::k32::exit_thread(1);
    }
    let p = ps();
    let calls = process(rp(f + F_FLS * p), 1);
    run_callbacks(c, calls);
    process(rp(f + F_FLS * p), 2);
    let own = rp(f + F_OWN * p);
    if own != 0 {
        mem::release(own);
    }
    mem::heap_free(f);
    0
}

fn IsThreadAFiber(_: &mut Ctx) -> u32 {
    is_fiber() as u32
}

/// The stack ExitThread gives back: the running fiber's (a thread that ends on a fiber of its
/// own keeps the stack it began on for the fiber it converted into)
pub fn exit_stack(thread_stack: u32) -> u32 {
    let d = rp(teb() + offs().dealloc);
    if d != 0 { d } else { thread_stack }
}

// ---------------------------------------------------------------------------------------------
// FLS entry points

fn RtlFlsAlloc(c: &mut Ctx) -> u32 {
    let (cb, out) = (c.a(0), c.a(1));
    match alloc(cb) {
        Ok(i) => {
            if out != 0 {
                w32(out, i);
            }
            0
        }
        Err(s) => s,
    }
}
fn RtlFlsFree(c: &mut Ctx) -> u32 {
    match free(c.a(0)) {
        Ok(calls) => {
            run_callbacks(c, calls);
            0
        }
        Err(s) => s,
    }
}
fn RtlFlsGetValue(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    match get(slots(), c.a(0)) {
        Ok(v) => {
            if out != 0 {
                wp(out, v);
            }
            0
        }
        Err(s) => s,
    }
}
fn RtlFlsSetValue(c: &mut Ctx) -> u32 {
    match set(slots(), c.a(0), c.a(1)) {
        Ok(()) => 0,
        Err(s) => s,
    }
}
fn RtlProcessFlsData(c: &mut Ctx) -> u32 {
    let (f, flags) = (c.a(0), c.a(1));
    let calls = process(f, flags & 1);
    run_callbacks(c, calls);
    process(f, flags & 2);
    0
}

fn FlsAlloc(c: &mut Ctx) -> u32 {
    match alloc(c.a(0)) {
        Ok(i) => i,
        Err(_) => {
            last_error(ERROR_NOT_ENOUGH_MEMORY);
            FLS_OUT_OF_INDEXES
        }
    }
}
fn FlsFree(c: &mut Ctx) -> u32 {
    match free(c.a(0)) {
        Ok(calls) => {
            run_callbacks(c, calls);
            1
        }
        Err(_) => {
            last_error(ERROR_INVALID_PARAMETER);
            0
        }
    }
}
fn FlsGetValue(c: &mut Ctx) -> u32 {
    match get(slots(), c.a(0)) {
        Ok(v) => {
            last_error(0);
            v
        }
        Err(_) => {
            last_error(ERROR_INVALID_PARAMETER);
            0
        }
    }
}
fn FlsSetValue(c: &mut Ctx) -> u32 {
    match set(slots(), c.a(0), c.a(1)) {
        Ok(()) => 1,
        Err(_) => {
            last_error(ERROR_INVALID_PARAMETER);
            0
        }
    }
}

pub fn init() {
    for dll in ["kernel32", "kernelbase"] {
        implement(dll, "ConvertThreadToFiber", ConvertThreadToFiber);
        implement(dll, "ConvertThreadToFiberEx", ConvertThreadToFiberEx);
        implement(dll, "ConvertFiberToThread", ConvertFiberToThread);
        implement(dll, "CreateFiber", CreateFiber);
        implement(dll, "CreateFiberEx", CreateFiberEx);
        implement(dll, "SwitchToFiber", SwitchToFiber);
        implement(dll, "DeleteFiber", DeleteFiber);
        implement(dll, "IsThreadAFiber", IsThreadAFiber);
        implement(dll, "FlsAlloc", FlsAlloc);
        implement(dll, "FlsFree", FlsFree);
        implement(dll, "FlsGetValue", FlsGetValue);
        implement(dll, "FlsSetValue", FlsSetValue);
    }
    reg!("ntdll"; RtlFlsAlloc, RtlFlsFree, RtlFlsGetValue, RtlFlsSetValue, RtlProcessFlsData);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    #[test]
    fn chunks_cover_the_indexes() {
        assert_eq!(chunk_of(0), (0, 0));
        assert_eq!(chunk_of(15), (0, 15));
        assert_eq!(chunk_of(16), (1, 0));
        assert_eq!(chunk_of(FLS_MAX), (7, chunk_size(7) - 1));
        assert_eq!((0..CHUNKS).map(chunk_size).sum::<u32>(), FLS_MAX + 1);
    }

    proptest! {
        /// chunk_of is the inverse of the chunks' running offsets
        #[test]
        fn chunk_of_round_trips(i in 0u32..=FLS_MAX) {
            let (j, k) = chunk_of(i);
            prop_assert!(j < CHUNKS && k < chunk_size(j));
            prop_assert_eq!((0..j).map(chunk_size).sum::<u32>() + k, i);
        }
    }

    #[derive(Debug, Clone)]
    enum Op {
        Alloc(bool),
        Free(usize),
        Set(usize, usize, u32),
        Get(usize, usize),
        /// a fiber's data processed (callbacks) and freed
        Drop(usize),
    }
    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            any::<bool>().prop_map(Op::Alloc),
            (0usize..8).prop_map(Op::Free),
            (0usize..3, 0usize..8, 0u32..4).prop_map(|(f, i, v)| Op::Set(f, i, v)),
            (0usize..3, 0usize..8).prop_map(|(f, i)| Op::Get(f, i)),
            (0usize..3).prop_map(Op::Drop),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        /// FLS against a model: indexes are the lowest free ones, values are per fiber,
        /// freeing an index (or a fiber's data) hands every nonzero value to its callback
        /// once, and a fresh fiber reads nothing
        #[test]
        fn fls_matches_a_model(ops in proptest::collection::vec(op(), 1..60)) {
            let _g = crate::test_guest::enter();
            // (a fresh global, as a process starts with)
            *FLS.lock().unwrap() = 0;
            let fibers: Vec<u32> = (0..3).map(|_| mem::heap_alloc(4, true)).collect();
            let mut live: Vec<(u32, u32)> = Vec::new(); // (index, callback)
            let mut vals: HashMap<(usize, u32), u32> = HashMap::new();
            for o in ops {
                match o {
                    Op::Alloc(with_cb) => {
                        let cb = if with_cb { 0x5000 } else { 0 };
                        let want = (1..=FLS_MAX).find(|i| !live.iter().any(|l| l.0 == *i)).unwrap();
                        let got = alloc(cb).unwrap();
                        prop_assert_eq!(got, want);
                        live.push((got, cb));
                    }
                    Op::Free(n) => {
                        let Some(&(i, cb)) = live.get(n) else { continue };
                        let calls = free(i).unwrap();
                        let mut want: Vec<(u32, u32)> = Vec::new();
                        for f in 0..3 {
                            if let Some(v) = vals.remove(&(f, i)) {
                                if v != 0 && cb != 0 {
                                    want.push((cb, v));
                                }
                            }
                        }
                        let (mut a, mut b) = (calls, want);
                        a.sort();
                        b.sort();
                        prop_assert_eq!(a, b);
                        live.remove(n);
                        prop_assert_eq!(free(i), Err(STATUS_INVALID_PARAMETER));
                    }
                    Op::Set(f, n, v) => {
                        let Some(&(i, _)) = live.get(n) else { continue };
                        set(fibers[f], i, v).unwrap();
                        vals.insert((f, i), v);
                    }
                    Op::Get(f, n) => {
                        let Some(&(i, _)) = live.get(n) else { continue };
                        match get(fibers[f], i) {
                            Ok(v) => prop_assert_eq!(v, vals.get(&(f, i)).copied().unwrap_or(0)),
                            Err(_) => prop_assert!(!vals.contains_key(&(f, i)) || rp(fibers[f]) == 0),
                        }
                    }
                    Op::Drop(f) => {
                        let d = rp(fibers[f]);
                        let mut a = process(d, 1);
                        process(d, 2);
                        wp(fibers[f], 0);
                        let mut b: Vec<(u32, u32)> = live.iter().filter_map(|&(i, cb)| {
                            let v = vals.remove(&(f, i))?;
                            (v != 0 && cb != 0).then_some((cb, v))
                        }).collect();
                        vals.retain(|k, _| k.0 != f);
                        a.sort();
                        b.sort();
                        prop_assert_eq!(a, b);
                    }
                }
            }
            prop_assert!(get(fibers[0], 0).is_err());
            prop_assert!(set(fibers[0], FLS_MAX + 1, 1).is_err());
            // (what a later case starts from: nothing left over)
            for f in 0..3 {
                process(rp(fibers[f]), 2);
                mem::heap_free(fibers[f]);
            }
            *FLS.lock().unwrap() = 0;
        }
    }
}
