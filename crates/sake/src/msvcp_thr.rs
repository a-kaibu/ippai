//! msvcp140.dll's threads support, as the STL's stl/src (mutex.cpp, cond.cpp, cthread.cpp,
//! xtime.cpp, ppltasks.cpp, excptptr.cpp): the C-style _Mtx_* / _Cnd_* / _Thrd_* functions
//! std::mutex, std::condition_variable and std::thread call, the clocks, std::random_device,
//! the Concurrency task plumbing PPL tasks (std::async) use, exception_ptr, and the
//! std::system_error throwers.
//!
//! _Mtx_internal_imp_t (x64 80 bytes, x86 48): int _Type, then the critical section's storage
//! { void* _Unused, SRW lock } (64 / 36 bytes: its SRW lock word at +16 / +8), long _Thread_id
//! after it (-1 or 0: no owner), int _Count. Storage the program's constexpr std::mutex
//! constructor zeroed works without _Mtx_init_in_situ. _Cnd_internal_imp_t (72 / 40 bytes): the
//! CONDITION_VARIABLE after its first pointer is the wait's key, as the STL's
//! _Stl_condition_variable keeps it: the headers' inline waits call SleepConditionVariableSRW on
//! it (with the mutex's SRW lock), and _Cnd_broadcast must wake those too
//! (<__msvc_threads_core.hpp>, VS 2022 17.11).

#![allow(non_snake_case)]

use std::sync::Mutex;

use crate::api::{self, host_fn, Ctx, Handler};
use crate::k32::{cv_wait, cv_wake, srw_acquire, srw_release, srw_try};
use crate::mem::{self, ps, r32, r64, rp, w32, w64, wp};
use crate::msvcp::{vcall, A};

// _Thrd_result
const SUCCESS: u32 = 0;
const NOMEM: u32 = 1;
const TIMEDOUT: u32 = 2;
const BUSY: u32 = 3;

fn x64() -> bool {
    mem::x64()
}
fn MTX_SIZE() -> u32 {
    if x64() { 80 } else { 48 }
}
fn CND_SIZE() -> u32 {
    if x64() { 72 } else { 40 }
}
// _Mtx_type bits
const MTX_PLAIN: u32 = 0x1;
const MTX_TRY: u32 = 0x2;
const MTX_TIMED: u32 = 0x4;
const MTX_RECURSIVE: u32 = 0x100;

/// the SRW lock of the critical section (after int _Type and the critical section's _Unused)
fn LOCK() -> u32 {
    2 * ps()
}
/// the CONDITION_VARIABLE inside a _Cnd_internal_imp_t
fn CV() -> u32 {
    ps()
}
fn OWNER() -> u32 {
    if x64() { 72 } else { 40 }
}
fn COUNT() -> u32 {
    OWNER() + 4
}

fn tid() -> u32 {
    crate::cur().tid
}

/// Calls a kernel32 function (our trap) from here.
fn k32(c: &mut Ctx, name: &str, args: &[u32]) -> u32 {
    let f = api::export("kernel32", name);
    c.call(f, args)
}

// ---------------------------------------------------------------------------------------------
// mutexes

fn owner_is_me(m: u32) -> bool {
    r32(m + COUNT()) != 0 && r32(m + OWNER()) == tid()
}

/// How long `mtx_lock` may wait.
#[derive(Clone, Copy, PartialEq)]
enum Wait {
    Forever,
    /// _Mtx_trylock (a target time of exactly 0: busy when taken)
    Try,
    /// _Mtx_timedlock: up to so many ms (0: the target passed; timed out when taken)
    Ms(u32),
}

/// Locks `m` as the STL's mtx_do_lock (mutex.cpp, VS 2022 17.11): a plain mutex (type 1,
/// with or without the recursive bit) waits for it whatever is asked (a try too: Windows hangs
/// there, tests/winref host_msvcpsync), and the thread holding it counts again; the others
/// try or wait as asked, and the holder counts again only when it is recursive.
fn mtx_lock(m: u32, wait: Wait) -> u32 {
    let ty = r32(m);
    let me = owner_is_me(m);
    if ty & !MTX_RECURSIVE == MTX_PLAIN {
        if !me {
            srw_acquire(m + LOCK(), false);
            w32(m + OWNER(), tid());
        }
        w32(m + COUNT(), r32(m + COUNT()) + 1);
        return SUCCESS;
    }
    let got = me
        || match wait {
            Wait::Forever => {
                srw_acquire(m + LOCK(), false);
                true
            }
            Wait::Try | Wait::Ms(0) => srw_try(m + LOCK(), false),
            // (a wait on the lock word with a deadline, woken by the release)
            Wait::Ms(ms) => crate::k32::srw_acquire_for(m + LOCK(), false, ms),
        };
    if got {
        if me {
            if ty & MTX_RECURSIVE == 0 {
                // (not recursive: the count stays, and it is as if taken elsewhere)
                return if matches!(wait, Wait::Forever | Wait::Try) { BUSY } else { TIMEDOUT };
            }
            w32(m + COUNT(), r32(m + COUNT()) + 1);
        } else {
            w32(m + OWNER(), tid());
            w32(m + COUNT(), 1);
        }
        return SUCCESS;
    }
    if wait == Wait::Try { BUSY } else { TIMEDOUT }
}

fn mtx_unlock(m: u32) {
    let n = r32(m + COUNT()).wrapping_sub(1);
    w32(m + COUNT(), n);
    if n == 0 {
        w32(m + OWNER(), u32::MAX);
        srw_release(m + LOCK(), false);
    }
}

fn mtx_init(m: u32, ty: u32) {
    mem::fill(m, MTX_SIZE(), 0);
    w32(m, ty);
    w32(m + OWNER(), u32::MAX);
}

/// _Mtx_init_in_situ(mtx, type)
fn _Mtx_init_in_situ(c: &mut Ctx) -> u32 {
    mtx_init(c.a(0), c.a(1));
    0
}
/// _Mtx_init(&mtx, type): a new one
fn _Mtx_init(c: &mut Ctx) -> u32 {
    let m = mem::heap_alloc(MTX_SIZE(), true);
    if m == 0 {
        wp(c.a(0), 0);
        return NOMEM;
    }
    mtx_init(m, c.a(1));
    wp(c.a(0), m);
    SUCCESS
}
fn _Mtx_destroy(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}
fn _Mtx_lock(c: &mut Ctx) -> u32 {
    mtx_lock(c.a(0), Wait::Forever)
}
fn _Mtx_trylock(c: &mut Ctx) -> u32 {
    mtx_lock(c.a(0), Wait::Try)
}
/// _Mtx_timedlock(mtx, const _timespec64 *target)
fn _Mtx_timedlock(c: &mut Ctx) -> u32 {
    let m = c.a(0);
    let _ = MTX_TRY | MTX_TIMED;
    mtx_lock(m, Wait::Ms(ms_until(c.a(1))))
}
fn _Mtx_unlock(c: &mut Ctx) -> u32 {
    mtx_unlock(c.a(0));
    SUCCESS
}
fn _Mtx_current_owns(c: &mut Ctx) -> u32 {
    owner_is_me(c.a(0)) as u32
}
/// the critical section's storage
fn _Mtx_getconcrtcs(c: &mut Ctx) -> u32 {
    c.a(0) + ps()
}
fn _Mtx_clear_owner(c: &mut Ctx) -> u32 {
    let m = c.a(0);
    w32(m + OWNER(), u32::MAX);
    w32(m + COUNT(), r32(m + COUNT()).wrapping_sub(1));
    0
}
fn _Mtx_reset_owner(c: &mut Ctx) -> u32 {
    let m = c.a(0);
    w32(m + OWNER(), tid());
    w32(m + COUNT(), r32(m + COUNT()).wrapping_add(1));
    0
}

// ---------------------------------------------------------------------------------------------
// condition variables

fn cnd_new(c: &mut Ctx) -> u32 {
    let p = mem::heap_alloc(CND_SIZE(), true);
    wp(c.a(0), p);
    if p == 0 { NOMEM } else { SUCCESS }
}
fn cnd_free(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}
fn cnd_init_in_situ(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), CND_SIZE(), 0);
    0
}

/// cnd_wait's lock back: mutex `m` held `n` times
pub(crate) fn mtx_reacquire(m: u64, n: u64) {
    let m = m as u32;
    srw_acquire(m + LOCK(), false);
    w32(m + OWNER(), tid());
    w32(m + COUNT(), (n as u32).max(1));
}

/// Waits on `cv` with `m` (which the thread holds, possibly more than once) released.
fn cnd_wait(cv: u32, m: u32, ms: u32) -> u32 {
    let n = r32(m + COUNT());
    let woke = cv_wait(
        cv + CV(),
        ms,
        |m, _| {
            let m = m as u32;
            w32(m + OWNER(), u32::MAX);
            w32(m + COUNT(), 0);
            srw_release(m + LOCK(), false);
        },
        crate::snap::Reacquire::MsvcpMtx,
        m as u64,
        n as u64,
        SUCCESS,
    );
    if woke { SUCCESS } else { TIMEDOUT }
}
/// _Cnd_wait(cnd, mtx)
fn _Cnd_wait(c: &mut Ctx) -> u32 {
    cnd_wait(c.a(0), c.a(1), u32::MAX)
}
/// _Cnd_timedwait(cnd, mtx, const _timespec64 *target)
fn _Cnd_timedwait(c: &mut Ctx) -> u32 {
    let ms = ms_until(c.a(2));
    cnd_wait(c.a(0), c.a(1), ms)
}
/// _Cnd_timedwait_for(cnd, mtx, unsigned ms)
fn _Cnd_timedwait_for(c: &mut Ctx) -> u32 {
    cnd_wait(c.a(0), c.a(1), c.a(2))
}
/// _Cnd_signal / _Cnd_broadcast (a signal wakes all waiters: they recheck their predicate)
fn _Cnd_signal(c: &mut Ctx) -> u32 {
    cv_wake(c.a(0) + CV());
    SUCCESS
}

/// notify_all_at_thread_exit's registrations: (thread, cnd, mtx, int *done)
static AT_EXIT: Mutex<Vec<(u32, u32, u32, u32)>> = Mutex::new(Vec::new());
fn _Cnd_register_at_thread_exit(c: &mut Ctx) -> u32 {
    AT_EXIT.lock().unwrap().push((tid(), c.a(0), c.a(1), c.a(2)));
    0
}
fn _Cnd_unregister_at_thread_exit(c: &mut Ctx) -> u32 {
    let m = c.a(0);
    AT_EXIT.lock().unwrap().retain(|e| e.2 != m);
    0
}
/// A thread ended without its CRT's exit (TerminateThread, ExitThread): its registrations go
/// without running, as on Windows (they waited for a thread id that a later thread could get).
pub fn thread_gone(tid: u32) {
    AT_EXIT.lock().unwrap().retain(|e| e.0 != tid);
}
/// At the thread's exit: each registration's flag set, its mutex unlocked, its waiters woken.
fn _Cnd_do_broadcast_at_thread_exit(_: &mut Ctx) -> u32 {
    let me = tid();
    let mine: Vec<_> = {
        let mut g = AT_EXIT.lock().unwrap();
        let (mine, rest) = g.drain(..).partition(|e| e.0 == me);
        *g = rest;
        mine
    };
    for (_, cnd, m, done) in mine {
        if done != 0 {
            w32(done, 1);
        }
        mtx_unlock(m);
        cv_wake(cnd + CV());
    }
    0
}

// ---------------------------------------------------------------------------------------------
// threads and time

/// 100 ns ticks since 1970 (_Xtime_get_ticks)
fn ticks() -> u64 {
    crate::files::filetime(std::time::SystemTime::now()).wrapping_sub(116_444_736_000_000_000)
}

/// Milliseconds from now to the _timespec64 { __time64_t sec; long nsec } at `ts` (0 when past).
fn ms_until(ts: u32) -> u32 {
    let target = (r64(ts) as i64 as i128) * 1_000_000_000 + r32(ts + 8) as i32 as i128;
    let now = ticks() as i128 * 100;
    let ms = (target - now).max(0) / 1_000_000;
    ms.min(u32::MAX as i128 - 1) as u32
}

fn _Thrd_id(_: &mut Ctx) -> u32 {
    tid()
}
/// _Thrd_join(_Thrd_t {handle, id} (by value: 16 bytes, so a pointer on x64; x86 the 8 bytes
/// on the stack), int *result)
fn _Thrd_join(c: &mut Ctx) -> u32 {
    let (t, res) = (c.byval(0), c.a(if x64() { 1 } else { 2 }));
    let h = rp(t);
    if k32(c, "WaitForSingleObject", &[h, u32::MAX]) == u32::MAX {
        return 4;
    }
    if res != 0 {
        let code = mem::heap_alloc(4, true);
        if code != 0 {
            k32(c, "GetExitCodeThread", &[h, code]);
            w32(res, r32(code));
            mem::heap_free(code);
        }
    }
    k32(c, "CloseHandle", &[h]);
    SUCCESS
}
fn _Thrd_detach(c: &mut Ctx) -> u32 {
    let h = rp(c.byval(0));
    if k32(c, "CloseHandle", &[h]) != 0 { SUCCESS } else { 4 }
}
/// _Thrd_sleep(const _timespec64 *until)
fn _Thrd_sleep(c: &mut Ctx) -> u32 {
    let ms = ms_until(c.a(0));
    k32(c, "Sleep", &[ms]);
    0
}
/// _Thrd_sleep_for(unsigned long ms)
fn _Thrd_sleep_for(c: &mut Ctx) -> u32 {
    let ms = c.a(0);
    k32(c, "Sleep", &[ms]);
    0
}
fn _Thrd_yield(c: &mut Ctx) -> u32 {
    k32(c, "Sleep", &[0]);
    0
}
fn _Thrd_hardware_concurrency(_: &mut Ctx) -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

fn _Query_perf_counter(c: &mut Ctx) -> u32 {
    c.ret64(crate::k32::qpc())
}
fn _Query_perf_frequency(c: &mut Ctx) -> u32 {
    c.ret64(10_000_000)
}
fn _Xtime_get_ticks(c: &mut Ctx) -> u32 {
    c.ret64(ticks())
}
/// xtime_get / _Xtime_get2 style: _timespec64_get(ts, TIME_UTC) and _Timespec64_get_sys(ts)
fn timespec_get(c: &mut Ctx) -> u32 {
    let (ts, base) = (c.a(0), c.a(1));
    let t = ticks();
    w64(ts, t / 10_000_000);
    w32(ts + 8, (t % 10_000_000 * 100) as u32);
    if base == 0 { 1 } else { base }
}
/// _Xtime_diff_to_millis2(const _timespec64 *a, const _timespec64 *b): a - b, 0 when negative
fn _Xtime_diff_to_millis2(c: &mut Ctx) -> u32 {
    let ns = |p: u32| (r64(p) as i64 as i128) * 1_000_000_000 + r32(p + 8) as i32 as i128;
    let d = (ns(c.a(0)) - ns(c.a(1))).max(0);
    // (rounded up, as the STL)
    ((d + 999_999) / 1_000_000).min(u32::MAX as i128) as u32
}

/// ?_Random_device@std@@YAIXZ: a random 32-bit number
fn _Random_device(_: &mut Ctx) -> u32 {
    let mut b = [0u8; 4];
    unsafe { libc::getentropy(b.as_mut_ptr().cast(), 4) };
    u32::from_le_bytes(b)
}

// ---------------------------------------------------------------------------------------------
// Concurrency (ppltasks.cpp, desktop: no COM apartments): chores on the thread pool

/// _Threadpool_chore { PTP_WORK _M_work; void (*_M_callback)(void *); void *_M_data }: the work
/// item's callback (instance, chore, work) calls callback(data)
fn chore_run(c: &mut Ctx) -> u32 {
    let chore = c.a(1);
    let (f, data) = (rp(chore + ps()), rp(chore + 2 * ps()));
    c.call(f, &[data]);
    0
}
/// ?_Schedule_chore@details@Concurrency@@YAHPEAU_Threadpool_chore@12@@Z: a new work item for
/// the chore, submitted; 0, or the error CreateThreadpoolWork left (taskscheduler.cpp: the
/// tasks throw invalid_operation on anything else than 0)
fn schedule_chore(c: &mut Ctx) -> u32 {
    let chore = c.a(0);
    // (a PTP_WORK_CALLBACK: x86 stdcall, three arguments)
    let cb = host_fn("msvcp::chore", chore_run, 12);
    let w = k32(c, "CreateThreadpoolWork", &[cb, chore, 0]);
    wp(chore, w);
    if w == 0 {
        return k32(c, "GetLastError", &[]);
    }
    reschedule_chore(c)
}
/// ?_Reschedule_chore@details@Concurrency@@YAHPEBU_Threadpool_chore@12@@Z: submits its work
/// item again; 0
fn reschedule_chore(c: &mut Ctx) -> u32 {
    let w = rp(c.a(0));
    k32(c, "SubmitThreadpoolWork", &[w]);
    0
}
/// ?_Release_chore@details@Concurrency@@YAXPEAU_Threadpool_chore@12@@Z
fn release_chore(c: &mut Ctx) -> u32 {
    let chore = c.a(0);
    let w = rp(chore);
    if w != 0 {
        k32(c, "CloseThreadpoolWork", &[w]);
        wp(chore, 0);
    }
    0
}
/// _ContextCallback { size_t _M_context }: 0 (no apartment); _Capture, _Reset and _Assign
/// leave it as it is (Windows' answers in Wine's msvcp140 tests)
fn ctx_capture(_: &mut Ctx) -> u32 {
    0
}
/// task_continuation_context(): _M_context 0, _M_RunInline false
fn continuation_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wp(this, 0);
    mem::write(this + ps(), &[0]);
    this
}
/// _ContextCallback::_CallInContext(std::function<void()> f (by value: x64 a pointer to the
/// caller's copy, x86 the copy on the stack; the callee destroys it), bool): f() here.
/// std::function: its _Impl pointer is the last of 6 + 16 / sizeof(void*) pointers; _Func_base's
/// vtable: _Copy, _Move, _Do_call, _Target_type, _Delete_this, _Get.
fn call_in_context(c: &mut Ctx) -> u32 {
    let f = c.byval(1);
    let at = f + (5 + 16 / ps()) * ps();
    let imp = rp(at);
    if imp != 0 {
        vcall(c, imp, 2, &[]);
        let local = (imp == f) as u32;
        vcall(c, imp, 4, &[A::I((local == 0) as u32)]);
        wp(at, 0);
    }
    0
}
fn unobserved(c: &mut Ctx) -> u32 {
    crate::fatal(c.cpu, "a PPL task's exception was never observed (Concurrency::details::_ReportUnobservedException)")
}
fn unhandled(c: &mut Ctx) -> u32 {
    crate::fatal(c.cpu, "a PPL task's exception was never handled (_ExceptionHolder::ReportUnhandledError)")
}
fn nothing(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// exception_ptr (excptptr.cpp): two pointers, the EXCEPTION_RECORD (a copy) and a non-zero
// "control block"; records are not freed (an exception_ptr is rare and small)

/// EXCEPTION_RECORD: its size, where its parameters are and how big each is
fn RECORD() -> u32 {
    if x64() { 152 } else { 80 }
}
fn PARAMS() -> u32 {
    if x64() { 0x20 } else { 0x14 }
}
fn param(rec: u32, i: u32) -> u32 {
    rp(rec + PARAMS() + ps() * i)
}

fn eptr_create(c: &mut Ctx) -> u32 {
    wp(c.a(0), 0);
    wp(c.a(0) + ps(), 0);
    0
}
// An exception_ptr is {record, its reference count}: the count a word just past sake's
// records (RECORD() + 4 bytes); the last Destroy frees the record (it was never freed:
// std::async / promise::set_exception / PPL tasks made one per exception for good).

/// The count word of the exception_ptr at `p` when it holds a record of sake's (0: none).
fn eptr_count(p: u32) -> u32 {
    let (rec, cnt) = (rp(p), rp(p + ps()));
    if rec != 0 && cnt == rec.wrapping_add(RECORD()) { cnt } else { 0 }
}
fn eptr_ref(p: u32) {
    let k = eptr_count(p);
    if k != 0 {
        w32(k, r32(k) + 1);
    }
}
fn eptr_unref(p: u32) {
    let k = eptr_count(p);
    if k == 0 {
        return;
    }
    let n = r32(k).saturating_sub(1);
    w32(k, n);
    if n == 0 {
        free_record(rp(p));
    }
}
/// `d` holds record `rec` (made with its one reference), or nothing.
fn eptr_set(d: u32, rec: u32) {
    wp(d, rec);
    wp(d + ps(), if rec != 0 { rec + RECORD() } else { 0 });
}
#[cfg(test)]
static LIVE_RECORDS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
fn free_record(rec: u32) {
    #[cfg(test)]
    LIVE_RECORDS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    mem::heap_free(rec);
}
/// A record block (RECORD() bytes and the count, 1), 0 when the guest heap is full.
fn alloc_record() -> u32 {
    let rec = mem::heap_alloc(RECORD() + 4, true);
    if rec != 0 {
        w32(rec + RECORD(), 1);
        #[cfg(test)]
        LIVE_RECORDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    rec
}
/// __ExceptionPtrDestroy
fn eptr_destroy(c: &mut Ctx) -> u32 {
    eptr_unref(c.a(0));
    0
}
/// __ExceptionPtrCopy (the copy constructor: `d` holds nothing yet)
fn eptr_copy(c: &mut Ctx) -> u32 {
    let (d, s) = (c.a(0), c.a(1));
    mem::copy(d, s, 2 * ps());
    eptr_ref(d);
    0
}
/// __ExceptionPtrAssign: `d` lets its record go and shares `s`'s
fn eptr_assign(c: &mut Ctx) -> u32 {
    let (d, s) = (c.a(0), c.a(1));
    if d == s {
        return 0;
    }
    eptr_ref(s);
    eptr_unref(d);
    mem::copy(d, s, 2 * ps());
    0
}
fn eptr_to_bool(c: &mut Ctx) -> u32 {
    (rp(c.a(0)) != 0) as u32
}
fn eptr_compare(c: &mut Ctx) -> u32 {
    (rp(c.a(0)) == rp(c.a(1))) as u32
}
fn eptr_swap(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    let t = mem::slice(a, 2 * ps()).to_vec();
    mem::copy(a, b, 2 * ps());
    mem::write(b, &t);
    0
}
/// A C++ exception's record (x64: its image base the fourth parameter); 0 when the guest heap
/// is full (it wrote the record over address 0 and the exception_ptr said it held one).
fn new_record(obj: u64, ti: u32) -> u32 {
    let rec = alloc_record();
    if rec == 0 {
        return 0;
    }
    w32(rec, 0xe06d_7363);
    w32(rec + 4, 1);
    if x64() {
        w32(rec + 0x18, 4);
        w64(rec + 0x20, 0x1993_0520);
        w64(rec + 0x28, obj);
        w64(rec + 0x30, ti as u64);
        w64(rec + 0x38, crate::cxx64::image_base_of(ti));
    } else {
        w32(rec + 0x10, 3);
        w32(rec + 0x14, 0x1993_0520);
        w32(rec + 0x18, obj as u32);
        w32(rec + 0x1c, ti);
    }
    rec
}
/// __ExceptionPtrCurrentException(dst): the exception being handled, if any
fn eptr_current(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    let copy = if x64() {
        let rec = crate::cxx64::current_record();
        (rec != 0).then(|| {
            let copy = alloc_record();
            if copy != 0 {
                mem::copy(copy, rec, RECORD());
            }
            copy
        })
    } else {
        crate::cxx::current().map(|(obj, ti)| new_record(obj as u64, ti))
    };
    eptr_set(d, copy.unwrap_or(0));
    0
}
/// __ExceptionPtrRethrow(src): the C++ exception again (its object and ThrowInfo)
fn eptr_rethrow(c: &mut Ctx) -> u32 {
    let rec = rp(c.a(0));
    if rec == 0 {
        // (rethrow_exception of a null exception_ptr: bad_exception)
        crate::fatal(c.cpu, "std::rethrow_exception of a null exception_ptr");
    }
    if x64() {
        let (obj, ti) = (r64(rec + 0x20 + 8), r64(rec + 0x20 + 16) as u32);
        crate::cxx64::throw_object(c, obj, ti);
    } else {
        crate::cxx::throw_object(c, param(rec, 1), param(rec, 2));
    }
    0
}
/// __ExceptionPtrCopyException(dst, object, ThrowInfo): make_exception_ptr's
fn eptr_copy_exception(c: &mut Ctx) -> u32 {
    let (d, obj, ti) = (c.a(0), if x64() { c.a64(1) } else { c.a(1) as u64 }, c.a(2));
    eptr_set(d, new_record(obj, ti));
    0
}

// ---------------------------------------------------------------------------------------------
// system_error

/// errc values and their messages (generic_category's, as the STL's _Syserror_map)
const ERRNO_MSGS: &[(u32, &str)] = &[
    (1, "operation not permitted"),
    (2, "no such file or directory"),
    (3, "no such process"),
    (4, "interrupted"),
    (5, "io error"),
    (11, "resource unavailable try again"),
    (12, "not enough memory"),
    (13, "permission denied"),
    (16, "device or resource busy"),
    (17, "file exists"),
    (22, "invalid argument"),
    (24, "too many files open"),
    (28, "no space on device"),
    (36, "resource deadlock would occur"),
    (40, "function not supported"),
    (138, "timed out"),
];

fn errno_msg(e: u32) -> u32 {
    // every unknown code shares one "unknown error" string, as the STL's table does
    let e = if ERRNO_MSGS.iter().any(|(k, _)| *k == e) { e } else { u32::MAX };
    static STRS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
    let mut g = STRS.lock().unwrap();
    if let Some(&(_, p)) = g.iter().find(|(k, _)| *k == e) {
        return p;
    }
    let s = ERRNO_MSGS.iter().find(|(k, _)| *k == e).map_or("unknown error", |(_, m)| m);
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, s.as_bytes());
    g.push((e, p));
    p
}
/// ?_Syserror_map@std@@YAPEBDH@Z
fn _Syserror_map(c: &mut Ctx) -> u32 {
    errno_msg(c.a(0))
}
/// ?_Winerror_map@std@@YAHH@Z: a Windows error's errno (0: none)
fn _Winerror_map(c: &mut Ctx) -> u32 {
    winerror_errno(c.a(0))
}
/// ?_Winerror_map@std@@YAPEBDH@Z (VS 2013's): the message of a Windows error's errno (Wine's)
fn _Winerror_map_str(c: &mut Ctx) -> u32 {
    errno_msg(winerror_errno(c.a(0)))
}
/// The STL's Windows error -> errno table (values as the Windows CRT's errno)
fn winerror_errno(e: u32) -> u32 {
    match e {
        1 => 40,
        2 | 3 | 123 => 2,
        4 => 24,
        5 | 12 | 16 | 19 | 32 | 82 | 998 | 10013 => 13,
        6 | 87 | 131 | 267 | 4390 | 10022 => 22,
        8 | 14 => 12,
        15 | 20 | 55 => 19,
        17 => 18,
        21 | 1237 => 11,
        25 | 29 | 30 | 110 | 1011 | 1012 | 1013 => 5,
        33 | 212 => 39,
        39 | 112 => 28,
        50 => 129,
        80 | 183 => 17,
        111 | 10063 => 38,
        142 | 170 | 2401 | 2404 => 16,
        145 => 41,
        995 => 105,
        1460 | 10060 => 138,
        10004 => 4,
        10009 => 9,
        10014 => 14,
        10024 => 24,
        10035 => 140,
        10036 => 112,
        10037 => 103,
        10038 => 128,
        10039 => 109,
        10040 => 115,
        10041 => 136,
        10042 => 123,
        10043 => 135,
        10045 => 130,
        10047 => 102,
        10048 => 100,
        10049 => 101,
        10050 => 116,
        10051 => 118,
        10052 => 117,
        10053 => 106,
        10054 => 108,
        10055 => 119,
        10056 => 113,
        10057 => 126,
        10061 => 107,
        10065 => 110,
        _ => 0,
    }
}
/// ?_Throw_Cpp_error@std@@YAXH@Z: system_error(make_error_code(errc)) for one of the
/// _Cpp_error codes (busy, invalid argument, no such process, not enough memory, operation not
/// permitted, resource deadlock would occur, resource unavailable try again)
fn _Throw_Cpp_error(c: &mut Ctx) -> u32 {
    const ERRC: [u32; 7] = [16, 22, 3, 12, 1, 36, 11];
    let e = *ERRC.get(c.a(0) as usize).unwrap_or(&22);
    crate::msvcp::throw_system_error(c, e, errno_msg(e))
}
/// ?_Throw_C_error@std@@YAXH@Z (a _Thrd_result): as _Throw_Cpp_error
fn _Throw_C_error(c: &mut Ctx) -> u32 {
    let e = match c.a(0) {
        1 => 12,
        2 => 138,
        3 => 16,
        _ => 22,
    };
    crate::msvcp::throw_system_error(c, e, errno_msg(e))
}

pub fn init() {
    let fs: &[(&str, Handler)] = &[
        ("_Mtx_init_in_situ", _Mtx_init_in_situ),
        ("_Mtx_destroy_in_situ", nothing),
        ("_Mtx_init", _Mtx_init),
        ("_Mtx_destroy", _Mtx_destroy),
        ("_Mtx_lock", _Mtx_lock),
        ("_Mtx_trylock", _Mtx_trylock),
        ("_Mtx_timedlock", _Mtx_timedlock),
        ("_Mtx_unlock", _Mtx_unlock),
        ("_Mtx_current_owns", _Mtx_current_owns),
        ("_Mtx_getconcrtcs", _Mtx_getconcrtcs),
        ("_Mtx_clear_owner", _Mtx_clear_owner),
        ("_Mtx_reset_owner", _Mtx_reset_owner),
        ("_Cnd_init_in_situ", cnd_init_in_situ),
        ("_Cnd_destroy_in_situ", nothing),
        ("_Cnd_init", cnd_new),
        ("_Cnd_destroy", cnd_free),
        ("_Cnd_wait", _Cnd_wait),
        ("_Cnd_timedwait", _Cnd_timedwait),
        ("_Cnd_timedwait_for", _Cnd_timedwait_for),
        ("_Cnd_signal", _Cnd_signal),
        ("_Cnd_broadcast", _Cnd_signal),
        ("_Cnd_register_at_thread_exit", _Cnd_register_at_thread_exit),
        ("_Cnd_unregister_at_thread_exit", _Cnd_unregister_at_thread_exit),
        ("_Cnd_do_broadcast_at_thread_exit", _Cnd_do_broadcast_at_thread_exit),
        ("_Thrd_id", _Thrd_id),
        ("_Thrd_join", _Thrd_join),
        ("_Thrd_detach", _Thrd_detach),
        ("_Thrd_sleep", _Thrd_sleep),
        ("_Thrd_sleep_for", _Thrd_sleep_for),
        ("_Thrd_yield", _Thrd_yield),
        ("_Thrd_hardware_concurrency", _Thrd_hardware_concurrency),
        ("_Query_perf_counter", _Query_perf_counter),
        ("_Query_perf_frequency", _Query_perf_frequency),
        ("_Xtime_get_ticks", _Xtime_get_ticks),
        ("xtime_get", timespec_get),
        ("_Timespec64_get_sys", timespec_get),
        ("_Xtime_diff_to_millis2", _Xtime_diff_to_millis2),
        ("?_Random_device@std@@YAIXZ", _Random_device),
        ("?_Schedule_chore@details@Concurrency@@YAHPEAU_Threadpool_chore@12@@Z", schedule_chore),
        ("?_Reschedule_chore@details@Concurrency@@YAHPEBU_Threadpool_chore@12@@Z", reschedule_chore),
        ("?_Release_chore@details@Concurrency@@YAXPEAU_Threadpool_chore@12@@Z", release_chore),
        ("?GetCurrentThreadId@platform@details@Concurrency@@YAJXZ", _Thrd_id),
        ("?_Capture@_ContextCallback@details@Concurrency@@AEAAXXZ", ctx_capture),
        ("?_Reset@_ContextCallback@details@Concurrency@@AEAAXXZ", nothing),
        ("?_CallInContext@_ContextCallback@details@Concurrency@@QEBAXV?$function@$$A6AXXZ@std@@_N@Z", call_in_context),
        ("??0task_continuation_context@Concurrency@@AEAA@XZ", continuation_ctor),
        ("?_ReportUnobservedException@details@Concurrency@@YAXXZ", unobserved),
        ("?ReportUnhandledError@_ExceptionHolder@details@Concurrency@@AEAAXXZ", unhandled),
        ("?_IsNonBlockingThread@_Task_impl_base@details@Concurrency@@SA_NXZ", nothing),
        ("?__ExceptionPtrCreate@@YAXPEAX@Z", eptr_create),
        ("?__ExceptionPtrDestroy@@YAXPEAX@Z", eptr_destroy),
        ("?__ExceptionPtrCopy@@YAXPEAXPEBX@Z", eptr_copy),
        ("?__ExceptionPtrAssign@@YAXPEAXPEBX@Z", eptr_assign),
        ("?__ExceptionPtrToBool@@YA_NPEBX@Z", eptr_to_bool),
        ("?__ExceptionPtrCompare@@YA_NPEBX0@Z", eptr_compare),
        ("?__ExceptionPtrSwap@@YAXPEAX0@Z", eptr_swap),
        ("?__ExceptionPtrCurrentException@@YAXPEAX@Z", eptr_current),
        ("?__ExceptionPtrRethrow@@YAXPEBX@Z", eptr_rethrow),
        ("?__ExceptionPtrCopyException@@YAXPEAXPEBX1@Z", eptr_copy_exception),
        ("?_Syserror_map@std@@YAPEBDH@Z", _Syserror_map),
        ("?_Winerror_map@std@@YAHH@Z", _Winerror_map),
        ("?_Winerror_map@std@@YAPEBDH@Z", _Winerror_map_str),
        ("?_Throw_Cpp_error@std@@YAXH@Z", _Throw_Cpp_error),
        ("?_Throw_C_error@std@@YAXH@Z", _Throw_C_error),
    ];
    for (n, f) in fs {
        // (msvcp120 has these with VS 2013's layouts: only its messages and exception_ptr here)
        let old = n.contains("_map@") || n.contains("ExceptionPtr");
        crate::msvcp::reg_in(old, n, *f);
    }
    // the PPL task event logger (ETW): nothing logged
    for n in ["_LogScheduleTask@_TaskEventLogger@details@Concurrency@@QEAAX_N@Z", "_LogCancelTask", "_LogTaskCompleted", "_LogTaskExecutionCompleted", "_LogWorkItemCompleted", "_LogWorkItemStarted"] {
        let name = if n.contains('@') { format!("?{n}") } else { format!("?{n}@_TaskEventLogger@details@Concurrency@@QEAAXXZ") };
        crate::msvcp::reg_in(false, &name, nothing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows' table, as Wine's msvcp140 test_Winerror_map checks it on Windows
    #[test]
    fn winerror_map_matches_windows() {
        let want: &[(u32, u32)] = &[
            (0, 0), (1, 40), (2, 2), (3, 2), (4, 24), (5, 13), (6, 22), (7, 0), (8, 12), (12, 13),
            (14, 12), (15, 19), (16, 13), (17, 18), (19, 13), (20, 19), (21, 11), (25, 5), (29, 5),
            (30, 5), (32, 13), (33, 39), (39, 28), (50, 129), (55, 19), (80, 17), (82, 13),
            (87, 22), (110, 5), (111, 38), (112, 28), (123, 2), (131, 22), (142, 16), (145, 41),
            (170, 16), (183, 17), (212, 39), (267, 22), (995, 105), (998, 13), (1011, 5),
            (1237, 11), (1460, 138), (2401, 16), (2404, 16), (4390, 22), (10004, 4), (10013, 13),
            (10035, 140), (10054, 108), (10060, 138), (10061, 107), (10063, 38), (10065, 110),
            (10066, 0), (0xffff, 0),
        ];
        for &(e, n) in want {
            assert_eq!(winerror_errno(e), n, "error {e}");
        }
    }
}

#[cfg(test)]
mod eptr_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;
    use std::sync::atomic::Ordering::Relaxed;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        /// Four exception_ptr variables and the calls msvcp's inline code makes on them
        /// (make_exception_ptr, the copy constructor into a fresh one, operator=, swap, the
        /// destructor) in any order, against a model of shared ownership: each holds the
        /// model's record, and a record is freed exactly when its last holder lets it go.
        #[test]
        fn records_live_while_held(ops in proptest::collection::vec((0u8..5, 0usize..4, 0usize..4), 1..60)) {
            let _g = enter();
            let start = LIVE_RECORDS.load(Relaxed);
            let vars = mem::heap_alloc(32, true);
            let v = |i: usize| vars + 8 * i as u32;
            // (model: which record each holds, by a number of the model's; None: empty)
            let mut holds: [Option<u32>; 4] = [None; 4];
            let mut made = 0u32;
            let mut ti = 0x1000u32;
            for (op, a, b) in ops {
                match op {
                    0 => {
                        // (a fresh exception_ptr gets the new one: the old value destroyed first)
                        call(eptr_destroy, &[v(a)]);
                        ti += 4;
                        call(eptr_copy_exception, &[v(a), 0x4000, ti]);
                        made += 1;
                        holds[a] = Some(ti);
                    }
                    // (a copy constructed from itself is no C++)
                    1 if a != b => {
                        call(eptr_destroy, &[v(a)]);
                        call(eptr_copy, &[v(a), v(b)]);
                        holds[a] = holds[b];
                    }
                    1 => {}
                    2 => { call(eptr_assign, &[v(a), v(b)]); holds[a] = holds[b]; }
                    3 => { call(eptr_swap, &[v(a), v(b)]); holds.swap(a, b); }
                    _ => { call(eptr_destroy, &[v(a)]); call(eptr_create, &[v(a)]); holds[a] = None; }
                }
                for i in 0..4 {
                    let rec = mem::rp(v(i));
                    prop_assert_eq!(call(eptr_to_bool, &[v(i)]) != 0, holds[i].is_some());
                    if let Some(t) = holds[i] { prop_assert_eq!(param(rec, 2), t, "var {}", i); }
                }
                let mut live: Vec<u32> = holds.iter().flatten().copied().collect();
                live.sort();
                live.dedup();
                prop_assert_eq!(LIVE_RECORDS.load(Relaxed) - start, live.len() as i64, "records alive");
            }
            for i in 0..4 { call(eptr_destroy, &[v(i)]); }
            prop_assert_eq!(LIVE_RECORDS.load(Relaxed), start);
            prop_assert!(made > 0 || true);
            mem::heap_free(vars);
        }
    }

    /// A registration of a thread that ended without its CRT's exit is dropped, not run.
    #[test]
    fn registrations_of_an_ended_thread_go() {
        AT_EXIT.lock().unwrap().push((0x7777, 1, 2, 3));
        AT_EXIT.lock().unwrap().push((0x7778, 1, 2, 3));
        thread_gone(0x7777);
        let left: Vec<u32> = AT_EXIT.lock().unwrap().iter().map(|e| e.0).filter(|&t| t >= 0x7777 && t <= 0x7778).collect();
        assert_eq!(left, vec![0x7778]);
        thread_gone(0x7778);
    }
}
