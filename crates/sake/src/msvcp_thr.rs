//! msvcp140.dll's threads support (x64), as the STL's stl/src (mutex.cpp, cond.cpp, cthread.cpp,
//! xtime.cpp, ppltasks.cpp, excptptr.cpp): the C-style _Mtx_* / _Cnd_* / _Thrd_* functions
//! std::mutex, std::condition_variable and std::thread call, the clocks, std::random_device,
//! the Concurrency task plumbing PPL tasks (std::async) use, exception_ptr, and the
//! std::system_error throwers.
//!
//! _Mtx_internal_imp_t (80 bytes): int _Type, then the critical section's storage (its SRW lock
//! word at +16), long _Thread_id at +72 (-1 or 0: no owner), int _Count at +76. Storage the
//! program's constexpr std::mutex constructor zeroed works without _Mtx_init_in_situ.
//! _Cnd_internal_imp_t (72 bytes): only its address is used (the wait generation's key).

#![allow(non_snake_case)]

use std::sync::Mutex;

use crate::api::{self, host_fn, implement, Ctx, Handler};
use crate::k32::{cv_wait, cv_wake, srw_acquire, srw_release, srw_try};
use crate::mem::{self, r32, r64, rp, w32, w64, wp};

// _Thrd_result
const SUCCESS: u32 = 0;
const NOMEM: u32 = 1;
const TIMEDOUT: u32 = 2;
const BUSY: u32 = 3;

const MTX_SIZE: u32 = 80;
const CND_SIZE: u32 = 72;
// _Mtx_type bits
const MTX_PLAIN: u32 = 0x1;
const MTX_TRY: u32 = 0x2;
const MTX_TIMED: u32 = 0x4;
const MTX_RECURSIVE: u32 = 0x100;

const LOCK: u32 = 16;
const OWNER: u32 = 72;
const COUNT: u32 = 76;

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
    r32(m + COUNT) != 0 && r32(m + OWNER) == tid()
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
            srw_acquire(m + LOCK, false);
            w32(m + OWNER, tid());
        }
        w32(m + COUNT, r32(m + COUNT) + 1);
        return SUCCESS;
    }
    let got = me
        || match wait {
            Wait::Forever => {
                srw_acquire(m + LOCK, false);
                true
            }
            Wait::Try | Wait::Ms(0) => srw_try(m + LOCK, false),
            Wait::Ms(ms) => {
                let end = std::time::Instant::now() + std::time::Duration::from_millis(ms as u64);
                loop {
                    if srw_try(m + LOCK, false) {
                        break true;
                    }
                    if std::time::Instant::now() >= end {
                        break false;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        };
    if got {
        if me {
            if ty & MTX_RECURSIVE == 0 {
                // (not recursive: the count stays, and it is as if taken elsewhere)
                return if matches!(wait, Wait::Forever | Wait::Try) { BUSY } else { TIMEDOUT };
            }
            w32(m + COUNT, r32(m + COUNT) + 1);
        } else {
            w32(m + OWNER, tid());
            w32(m + COUNT, 1);
        }
        return SUCCESS;
    }
    if wait == Wait::Try { BUSY } else { TIMEDOUT }
}

fn mtx_unlock(m: u32) {
    let n = r32(m + COUNT).saturating_sub(1);
    w32(m + COUNT, n);
    if n == 0 {
        w32(m + OWNER, u32::MAX);
        srw_release(m + LOCK, false);
    }
}

fn mtx_init(m: u32, ty: u32) {
    mem::fill(m, MTX_SIZE, 0);
    w32(m, ty);
    w32(m + OWNER, u32::MAX);
}

/// _Mtx_init_in_situ(mtx, type)
fn _Mtx_init_in_situ(c: &mut Ctx) -> u32 {
    mtx_init(c.a(0), c.a(1));
    0
}
/// _Mtx_init(&mtx, type): a new one
fn _Mtx_init(c: &mut Ctx) -> u32 {
    let m = mem::heap_alloc(MTX_SIZE, true);
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
    c.a(0) + 8
}
fn _Mtx_clear_owner(c: &mut Ctx) -> u32 {
    let m = c.a(0);
    w32(m + OWNER, u32::MAX);
    w32(m + COUNT, r32(m + COUNT).wrapping_sub(1));
    0
}
fn _Mtx_reset_owner(c: &mut Ctx) -> u32 {
    let m = c.a(0);
    w32(m + OWNER, tid());
    w32(m + COUNT, r32(m + COUNT).wrapping_add(1));
    0
}

// ---------------------------------------------------------------------------------------------
// condition variables

fn cnd_new(c: &mut Ctx) -> u32 {
    let p = mem::heap_alloc(CND_SIZE, true);
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
    mem::fill(c.a(0), CND_SIZE, 0);
    0
}

/// Waits on `cv` with `m` (which the thread holds, possibly more than once) released.
fn cnd_wait(cv: u32, m: u32, ms: u32) -> u32 {
    let n = r32(m + COUNT);
    let woke = cv_wait(
        cv,
        ms,
        |m, _| {
            let m = m as u32;
            w32(m + OWNER, u32::MAX);
            w32(m + COUNT, 0);
            srw_release(m + LOCK, false);
        },
        |m, n| {
            let m = m as u32;
            srw_acquire(m + LOCK, false);
            w32(m + OWNER, tid());
            w32(m + COUNT, (n as u32).max(1));
        },
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
    cv_wake(c.a(0));
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
        cv_wake(cnd);
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
/// _Thrd_join(_Thrd_t {handle, id} (by value: 16 bytes, so a pointer on x64), int *result)
fn _Thrd_join(c: &mut Ctx) -> u32 {
    let (t, res) = (c.a(0), c.a(1));
    let h = rp(t);
    if k32(c, "WaitForSingleObject", &[h, u32::MAX]) == u32::MAX {
        return 4;
    }
    if res != 0 {
        let code = mem::heap_alloc(4, true);
        k32(c, "GetExitCodeThread", &[h, code]);
        w32(res, r32(code));
        mem::heap_free(code);
    }
    k32(c, "CloseHandle", &[h]);
    SUCCESS
}
fn _Thrd_detach(c: &mut Ctx) -> u32 {
    let h = rp(c.a(0));
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
    let (f, data) = (rp(chore + 8), rp(chore + 16));
    c.call(f, &[data]);
    0
}
/// ?_Schedule_chore@details@Concurrency@@YAHPEAU_Threadpool_chore@12@@Z: a new work item for
/// the chore, submitted; 0, or the error CreateThreadpoolWork left (taskscheduler.cpp: the
/// tasks throw invalid_operation on anything else than 0)
fn schedule_chore(c: &mut Ctx) -> u32 {
    let chore = c.a(0);
    let cb = host_fn("msvcp::chore", chore_run, 0);
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
/// _ContextCallback { size_t _M_context }: 1 = capture deferred; captured: 0 (no apartment)
fn ctx_capture(c: &mut Ctx) -> u32 {
    wp(c.a(0), 0);
    0
}
/// task_continuation_context() : _ContextCallback(deferred), _M_RunInline(false)
fn continuation_ctor(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    wp(this, 1);
    mem::write(this + 8, &[0]);
    this
}
/// _ContextCallback::_CallInContext(std::function<void()> f (a pointer to the caller's copy, which
/// the callee destroys), bool): f() here. std::function: its _Impl pointer is the last of 8
/// pointers; _Func_base's vtable: _Copy, _Move, _Do_call, _Target_type, _Delete_this, _Get.
fn call_in_context(c: &mut Ctx) -> u32 {
    let f = c.a(1);
    let imp = rp(f + 56);
    if imp != 0 {
        let vt = rp(imp);
        c.call(rp(vt + 16), &[imp]);
        let local = (imp == f) as u32;
        c.call(rp(vt + 32), &[imp, (local == 0) as u32]);
        wp(f + 56, 0);
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

const RECORD: u32 = 152;

fn eptr_create(c: &mut Ctx) -> u32 {
    w64(c.a(0), 0);
    w64(c.a(0) + 8, 0);
    0
}
fn eptr_copy(c: &mut Ctx) -> u32 {
    let (d, s) = (c.a(0), c.a(1));
    w64(d, r64(s));
    w64(d + 8, r64(s + 8));
    0
}
fn eptr_to_bool(c: &mut Ctx) -> u32 {
    (r64(c.a(0)) != 0) as u32
}
fn eptr_compare(c: &mut Ctx) -> u32 {
    (r64(c.a(0)) == r64(c.a(1))) as u32
}
fn eptr_swap(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    for o in [0, 8] {
        let t = r64(a + o);
        w64(a + o, r64(b + o));
        w64(b + o, t);
    }
    0
}
/// __ExceptionPtrCurrentException(dst): the exception being handled, if any
fn eptr_current(c: &mut Ctx) -> u32 {
    let d = c.a(0);
    let rec = crate::cxx64::current_record();
    if rec == 0 {
        w64(d, 0);
        w64(d + 8, 0);
        return 0;
    }
    let copy = mem::heap_alloc(RECORD, true);
    mem::copy(copy, rec, RECORD);
    w64(d, copy as u64);
    w64(d + 8, 1);
    0
}
/// __ExceptionPtrRethrow(src): the C++ exception again (its object and ThrowInfo)
fn eptr_rethrow(c: &mut Ctx) -> u32 {
    let rec = r64(c.a(0)) as u32;
    if rec == 0 {
        // (rethrow_exception of a null exception_ptr: bad_exception)
        crate::fatal(c.cpu, "std::rethrow_exception of a null exception_ptr");
    }
    let (obj, ti) = (r64(rec + 0x20 + 8), r64(rec + 0x20 + 16) as u32);
    crate::cxx64::throw_object(c, obj, ti);
    0
}
/// __ExceptionPtrCopyException(dst, object, ThrowInfo): make_exception_ptr's
fn eptr_copy_exception(c: &mut Ctx) -> u32 {
    let (d, obj, ti) = (c.a(0), c.a64(1), c.a(2));
    let rec = mem::heap_alloc(RECORD, true);
    w32(rec, 0xe06d_7363);
    w32(rec + 4, 1);
    w32(rec + 0x18, 4);
    w64(rec + 0x20, 0x1993_0520);
    w64(rec + 0x28, obj);
    w64(rec + 0x30, ti as u64);
    w64(rec + 0x38, crate::cxx64::image_base_of(ti));
    w64(d, rec as u64);
    w64(d + 8, 1);
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
    match c.a(0) {
        2 | 3 => 2,
        5 => 13,
        6 => 9,
        8 | 14 => 12,
        32 | 33 | 170 => 16,
        80 | 183 => 17,
        87 => 22,
        112 => 28,
        1460 => 138,
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
        ("?__ExceptionPtrDestroy@@YAXPEAX@Z", nothing),
        ("?__ExceptionPtrCopy@@YAXPEAXPEBX@Z", eptr_copy),
        ("?__ExceptionPtrAssign@@YAXPEAXPEBX@Z", eptr_copy),
        ("?__ExceptionPtrToBool@@YA_NPEBX@Z", eptr_to_bool),
        ("?__ExceptionPtrCompare@@YA_NPEBX0@Z", eptr_compare),
        ("?__ExceptionPtrSwap@@YAXPEAX0@Z", eptr_swap),
        ("?__ExceptionPtrCurrentException@@YAXPEAX@Z", eptr_current),
        ("?__ExceptionPtrRethrow@@YAXPEBX@Z", eptr_rethrow),
        ("?__ExceptionPtrCopyException@@YAXPEAXPEBX1@Z", eptr_copy_exception),
        ("?_Syserror_map@std@@YAPEBDH@Z", _Syserror_map),
        ("?_Winerror_map@std@@YAHH@Z", _Winerror_map),
        ("?_Throw_Cpp_error@std@@YAXH@Z", _Throw_Cpp_error),
        ("?_Throw_C_error@std@@YAXH@Z", _Throw_C_error),
    ];
    for (n, f) in fs {
        implement("msvcp140", n, *f);
    }
    // the PPL task event logger (ETW): nothing logged
    for n in ["_LogScheduleTask@_TaskEventLogger@details@Concurrency@@QEAAX_N@Z", "_LogCancelTask", "_LogTaskCompleted", "_LogTaskExecutionCompleted", "_LogWorkItemCompleted", "_LogWorkItemStarted"] {
        let name = if n.contains('@') { format!("?{n}") } else { format!("?{n}@_TaskEventLogger@details@Concurrency@@QEAAXXZ") };
        implement("msvcp140", &name, nothing);
    }
}
