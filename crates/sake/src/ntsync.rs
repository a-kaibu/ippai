#![allow(non_snake_case)]
//! ntdll's synchronization calls over kernel32's objects (k32.rs, obj.rs): NtCreate/Open/Set/
//! Reset/Clear/Pulse/QueryEvent, NtCreate/Open/Release/QueryMutant, NtCreate/Open/Release/
//! QuerySemaphore, NtWaitForSingleObject / NtWaitForMultipleObjects / NtSignalAndWaitFor
//! SingleObject / NtDelayExecution (a LARGE_INTEGER timeout: NULL forever, negative relative,
//! positive absolute, in 100 ns), the I/O completion calls (NtCreate/Set/RemoveIoCompletion(Ex),
//! iocp.rs) and kernel32's SignalObjectAndWait.
//!
//! Statuses follow Wine's ntdll sync and om tests (Windows' answers).

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, ps, r16, rp, w32, wp};
use crate::obj::{self, Obj};

pub const STATUS_SUCCESS: u32 = 0;
pub const STATUS_TIMEOUT: u32 = 0x102;
pub const STATUS_USER_APC: u32 = 0xc0;
pub const STATUS_OBJECT_NAME_EXISTS: u32 = 0x4000_0000;
pub const STATUS_INVALID_INFO_CLASS: u32 = 0xc000_0003;
pub const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xc000_0004;
pub const STATUS_INVALID_HANDLE: u32 = 0xc000_0008;
pub const STATUS_INVALID_PARAMETER: u32 = 0xc000_000d;
pub const STATUS_OBJECT_TYPE_MISMATCH: u32 = 0xc000_0024;
pub const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xc000_0034;
pub const STATUS_OBJECT_PATH_NOT_FOUND: u32 = 0xc000_003a;
pub const STATUS_OBJECT_PATH_SYNTAX_BAD: u32 = 0xc000_003b;
pub const STATUS_MUTANT_NOT_OWNED: u32 = 0xc000_0046;
pub const STATUS_SEMAPHORE_LIMIT_EXCEEDED: u32 = 0xc000_0047;

/// A kernel32 error from k32's object calls as the NTSTATUS ntdll returns
pub fn status_of(e: u32) -> u32 {
    match e {
        0 => STATUS_SUCCESS,
        183 => STATUS_OBJECT_NAME_EXISTS,
        2 => STATUS_OBJECT_NAME_NOT_FOUND,
        3 => STATUS_OBJECT_PATH_NOT_FOUND,
        6 => STATUS_OBJECT_TYPE_MISMATCH,
        87 => STATUS_INVALID_PARAMETER,
        288 => STATUS_MUTANT_NOT_OWNED,
        298 => STATUS_SEMAPHORE_LIMIT_EXCEEDED,
        e => 0xc000_0000 | e,
    }
}

/// An NT object name as kernel32 names it: "\BaseNamedObjects\x" and
/// "\Sessions\<n>\BaseNamedObjects\x" are "x" (Local\ and Global\ are the same namespace here);
/// a name under a directory handle is taken as under BaseNamedObjects. Err(status) for a
/// relative name without a directory.
pub fn nt_name(name: &str, root: bool) -> Result<String, u32> {
    if root {
        return Ok(name.to_string());
    }
    let Some(rest) = name.strip_prefix('\\') else {
        return Err(STATUS_OBJECT_PATH_SYNTAX_BAD);
    };
    if let Some(n) = rest.strip_prefix("BaseNamedObjects\\") {
        return Ok(n.to_string());
    }
    if let Some(s) = rest.strip_prefix("Sessions\\") {
        if let Some((num, tail)) = s.split_once('\\') {
            if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
                if let Some(n) = tail.strip_prefix("BaseNamedObjects\\") {
                    return Ok(n.to_string());
                }
            }
        }
    }
    Err(STATUS_OBJECT_PATH_NOT_FOUND)
}

/// The name in an OBJECT_ATTRIBUTES { Length, RootDirectory, ObjectName, ... } (None: unnamed)
fn attr_name(attr: u32) -> Result<Option<String>, u32> {
    if attr == 0 {
        return Ok(None);
    }
    let (root, us) = (rp(attr + ps()), rp(attr + 2 * ps()));
    if us == 0 || r16(us) == 0 {
        return Ok(None);
    }
    let buf = rp(us + ps());
    let s: Vec<u16> = (0..r16(us) as u32 / 2).map(|i| r16(buf + 2 * i)).collect();
    nt_name(&String::from_utf16_lossy(&s), root != 0).map(Some)
}

fn create(ph: u32, attr: u32, o: Obj) -> u32 {
    if ph == 0 {
        return 0xc000_0005; // STATUS_ACCESS_VIOLATION
    }
    let name = match attr_name(attr) {
        Ok(n) => n,
        Err(s) => return s,
    };
    // (ntdll leaves the thread's last error alone)
    let saved = crate::k32::get_last_error();
    last_error(0);
    let h = crate::k32::create_named(name, o);
    let e = crate::k32::get_last_error();
    last_error(saved);
    if h == 0 {
        return status_of(if e == 0 { 6 } else { e });
    }
    wp(ph, h);
    status_of(e)
}

fn open(ph: u32, attr: u32, kind: u8) -> u32 {
    let name = match attr_name(attr) {
        Ok(Some(n)) => n,
        Ok(None) => return STATUS_OBJECT_PATH_SYNTAX_BAD,
        Err(s) => return s,
    };
    let saved = crate::k32::get_last_error();
    let h = crate::k32::open_sync(name, kind);
    let e = crate::k32::get_last_error();
    last_error(saved);
    if h == 0 {
        return status_of(e);
    }
    wp(ph, h);
    STATUS_SUCCESS
}

/// An event's (manual, set), a mutex's (owner, count, abandoned) or a semaphore's (count, max)
enum State {
    Event(bool, bool),
    Mutex(u32, u32, bool),
    Semaphore(i32, i32),
}

fn state(h: u32) -> Option<State> {
    use crate::kern::Sync;
    if let Some(id) = crate::k32::shared_id(h) {
        return crate::kern::with(|k| k.sync_state(id)).0.map(|s| match s {
            Sync::Event { manual, set } => State::Event(manual, set),
            Sync::Mutex {
                owner,
                count,
                abandoned,
                ..
            } => State::Mutex(owner, count, abandoned),
            Sync::Semaphore { count, max } => State::Semaphore(count, max),
        });
    }
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Event { manual, set }, _)) => Some(State::Event(*manual, *set)),
        Some((Obj::Mutex { owner, count }, _)) => {
            Some(State::Mutex(*owner, *count, *owner == obj::ABANDONED))
        }
        Some((Obj::Semaphore { count, max }, _)) => Some(State::Semaphore(*count, *max)),
        _ => None,
    })
}

/// An event handle's state, or the status for another handle
fn event(h: u32) -> Result<(bool, bool), u32> {
    match state(h) {
        Some(State::Event(m, s)) => Ok((m, s)),
        Some(_) => Err(STATUS_OBJECT_TYPE_MISMATCH),
        None if obj::with(|t| t.objs.get(&h).is_some()) => Err(STATUS_OBJECT_TYPE_MISMATCH),
        None => Err(STATUS_INVALID_HANDLE),
    }
}

/// NtSetEvent / NtResetEvent / NtPulseEvent (h, &previous state)
fn set_event(h: u32, prev: u32, to: Option<bool>) -> u32 {
    let (_, was) = match event(h) {
        Ok(s) => s,
        Err(e) => return e,
    };
    match to {
        Some(v) => {
            crate::k32::event_set(h, v);
        }
        None => {
            crate::k32::event_set(h, true);
            std::thread::yield_now();
            crate::k32::event_set(h, false);
        }
    }
    mem::put32(prev, was as u32);
    STATUS_SUCCESS
}

/// The query calls' common checks: class 0, the exact length; writes the return length
fn query_info(class: u32, len: u32, ret: u32, want: u32) -> Result<(), u32> {
    if class != 0 {
        return Err(STATUS_INVALID_INFO_CLASS);
    }
    if len != want {
        return Err(STATUS_INFO_LENGTH_MISMATCH);
    }
    mem::put32(ret, want);
    Ok(())
}

/// Milliseconds for an NT timeout (NULL: INFINITE); rounds up so a short wait still waits.
pub fn timeout_ms(p: u32) -> u32 {
    if p == 0 {
        return u32::MAX;
    }
    let v = mem::r64(p) as i64;
    let ticks = if v < 0 {
        v.unsigned_abs()
    } else {
        let now = crate::files::filetime(std::time::SystemTime::now()) as i64;
        (v - now).max(0) as u64
    };
    ticks.div_ceil(10_000).min(u32::MAX as u64 - 1) as u32
}

/// A wait's result as an NTSTATUS (they share their numbers; a bad handle is a status here)
fn wait_status(r: u32) -> u32 {
    match r {
        obj::WAIT_FAILED => STATUS_INVALID_HANDLE,
        crate::apc::WAIT_IO_COMPLETION => STATUS_USER_APC,
        r => r,
    }
}

fn wait(c: &mut Ctx, hs: &[u32], all: bool, alertable: bool, ms: u32) -> u32 {
    if alertable {
        wait_status(crate::apc::wait(c, hs, all, ms))
    } else {
        wait_status(obj::wait(hs, all, ms, None))
    }
}

/// Signals an object for SignalObjectAndWait: Err(status) for one that cannot be signaled
fn signal(h: u32) -> Result<(), u32> {
    match state(h) {
        Some(State::Event(..)) => {
            crate::k32::event_set(h, true);
            Ok(())
        }
        Some(State::Mutex(..)) => crate::k32::release_mutex(h).map_err(status_of),
        Some(State::Semaphore(..)) => crate::k32::release_semaphore(h, 1)
            .map(|_| ())
            .map_err(status_of),
        None if obj::with(|t| t.objs.get(&h).is_some()) => Err(STATUS_OBJECT_TYPE_MISMATCH),
        None => Err(STATUS_INVALID_HANDLE),
    }
}

pub fn init() {
    let d = "ntdll";
    implement(d, "NtCreateEvent", |c| {
        if c.a(3) > 1 {
            return STATUS_INVALID_PARAMETER;
        }
        // (type 0: a notification (manual-reset) event, 1: a synchronization one)
        create(
            c.a(0),
            c.a(2),
            Obj::Event {
                manual: c.a(3) == 0,
                set: c.a(4) & 0xff != 0,
            },
        )
    });
    implement(d, "NtOpenEvent", |c| open(c.a(0), c.a(2), 1));
    implement(d, "NtSetEvent", |c| set_event(c.a(0), c.a(1), Some(true)));
    implement(d, "NtResetEvent", |c| {
        set_event(c.a(0), c.a(1), Some(false))
    });
    implement(d, "NtClearEvent", |c| set_event(c.a(0), 0, Some(false)));
    implement(d, "NtPulseEvent", |c| set_event(c.a(0), c.a(1), None));
    implement(d, "NtSetEventBoostPriority", |c| {
        set_event(c.a(0), 0, Some(true))
    });
    // NtQueryEvent(h, class, EVENT_BASIC_INFORMATION { type, state }, length, &length)
    implement(d, "NtQueryEvent", |c| {
        let (h, class, buf, len, ret) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
        let (manual, set) = match event(h) {
            Ok(s) => s,
            Err(e) => return e,
        };
        if let Err(e) = query_info(class, len, ret, 8) {
            return e;
        }
        w32(buf, !manual as u32);
        w32(buf + 4, set as u32);
        STATUS_SUCCESS
    });
    implement(d, "NtCreateMutant", |c| {
        let own = c.a(3) & 0xff != 0;
        let tid = crate::cur().tid;
        create(
            c.a(0),
            c.a(2),
            Obj::Mutex {
                owner: if own { tid } else { 0 },
                count: own as u32,
            },
        )
    });
    implement(d, "NtOpenMutant", |c| open(c.a(0), c.a(2), 2));
    // NtReleaseMutant(h, &previous count): the count is 1 free, 0 held once, -1 twice...
    implement(d, "NtReleaseMutant", |c| {
        let (h, prev) = (c.a(0), c.a(1));
        let count = match state(h) {
            Some(State::Mutex(_, n, _)) => n,
            Some(_) => return STATUS_OBJECT_TYPE_MISMATCH,
            None => return STATUS_INVALID_HANDLE,
        };
        match crate::k32::release_mutex(h) {
            Ok(()) => {
                mem::put32(prev, 1 - count as i32 as u32);
                STATUS_SUCCESS
            }
            Err(e) => status_of(e),
        }
    });
    // NtQueryMutant(h, class, MUTANT_BASIC_INFORMATION { count, owned by caller, abandoned }, ...)
    implement(d, "NtQueryMutant", |c| {
        let (h, class, buf, len, ret) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
        let (owner, count, abandoned) = match state(h) {
            Some(State::Mutex(o, n, a)) => (o, n, a),
            Some(_) => return STATUS_OBJECT_TYPE_MISMATCH,
            None => return STATUS_INVALID_HANDLE,
        };
        if let Err(e) = query_info(class, len, ret, 8) {
            return e;
        }
        w32(buf, (1 - count as i32) as u32);
        mem::w8(buf + 4, (owner == crate::cur().tid && count > 0) as u8);
        mem::w8(buf + 5, abandoned as u8);
        STATUS_SUCCESS
    });
    implement(d, "NtCreateSemaphore", |c| {
        let (init, max) = (c.a(3) as i32, c.a(4) as i32);
        if max <= 0 || init < 0 || init > max {
            return STATUS_INVALID_PARAMETER;
        }
        create(c.a(0), c.a(2), Obj::Semaphore { count: init, max })
    });
    implement(d, "NtOpenSemaphore", |c| open(c.a(0), c.a(2), 3));
    implement(d, "NtReleaseSemaphore", |c| {
        let (h, n, prev) = (c.a(0), c.a(1) as i32, c.a(2));
        match state(h) {
            Some(State::Semaphore(..)) => {}
            Some(_) => return STATUS_OBJECT_TYPE_MISMATCH,
            None => return STATUS_INVALID_HANDLE,
        }
        match crate::k32::release_semaphore(h, n) {
            Ok(p) => {
                mem::put32(prev, p as u32);
                STATUS_SUCCESS
            }
            Err(e) => status_of(e),
        }
    });
    // NtQuerySemaphore(h, class, SEMAPHORE_BASIC_INFORMATION { count, max }, ...)
    implement(d, "NtQuerySemaphore", |c| {
        let (h, class, buf, len, ret) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
        let (count, max) = match state(h) {
            Some(State::Semaphore(n, m)) => (n, m),
            Some(_) => return STATUS_OBJECT_TYPE_MISMATCH,
            None => return STATUS_INVALID_HANDLE,
        };
        if let Err(e) = query_info(class, len, ret, 8) {
            return e;
        }
        w32(buf, count as u32);
        w32(buf + 4, max as u32);
        STATUS_SUCCESS
    });
    implement(d, "NtWaitForSingleObject", |c| {
        let (h, alertable, ms) = (c.a(0), c.a(1) & 0xff != 0, timeout_ms(c.a(2)));
        wait(c, &[h], false, alertable, ms)
    });
    // NtWaitForMultipleObjects(count, handles, WaitAll 0 / WaitAny 1, alertable, timeout)
    implement(d, "NtWaitForMultipleObjects", |c| {
        let (n, p, ty, alertable, ms) = (
            c.a(0),
            c.a(1),
            c.a(2),
            c.a(3) & 0xff != 0,
            timeout_ms(c.a(4)),
        );
        if n == 0 || n > 64 || ty > 1 {
            return STATUS_INVALID_PARAMETER;
        }
        let hs: Vec<u32> = (0..n).map(|i| rp(p + ps() * i)).collect();
        wait(c, &hs, ty == 0, alertable, ms)
    });
    implement(d, "NtSignalAndWaitForSingleObject", |c| {
        let (s, h, alertable, ms) = (c.a(0), c.a(1), c.a(2) & 0xff != 0, timeout_ms(c.a(3)));
        if let Err(e) = signal(s) {
            return e;
        }
        wait(c, &[h], false, alertable, ms)
    });
    // NtDelayExecution(alertable, timeout)
    implement(d, "NtDelayExecution", |c| {
        let (alertable, ms) = (c.a(0) & 0xff != 0, timeout_ms(c.a(1)));
        if alertable {
            let r = crate::apc::wait(c, &[], false, ms);
            return if r == crate::apc::WAIT_IO_COMPLETION {
                STATUS_USER_APC
            } else {
                STATUS_SUCCESS
            };
        }
        if ms == 0 {
            std::thread::yield_now();
        } else {
            crate::k32::sleep_ms(ms);
        }
        STATUS_SUCCESS
    });
    // SignalObjectAndWait(signal, wait, ms, alertable): WAIT_FAILED and the error for a bad one
    implement("kernel32", "SignalObjectAndWait", |c| {
        let (s, h, ms, alertable) = (c.a(0), c.a(1), c.a(2), c.a(3) != 0);
        if let Err(e) = signal(s) {
            last_error(match e {
                STATUS_INVALID_HANDLE | STATUS_OBJECT_TYPE_MISMATCH => 6,
                STATUS_MUTANT_NOT_OWNED => 288,
                STATUS_SEMAPHORE_LIMIT_EXCEEDED => 298,
                _ => 87,
            });
            return obj::WAIT_FAILED;
        }
        if alertable {
            crate::apc::wait(c, &[h], false, ms)
        } else {
            obj::wait(&[h], false, ms, None)
        }
    });
    crate::iocp::init_nt();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn names() {
        assert_eq!(nt_name("\\BaseNamedObjects\\ev", false), Ok("ev".into()));
        assert_eq!(
            nt_name("\\Sessions\\1\\BaseNamedObjects\\Local\\x", false),
            Ok("Local\\x".into())
        );
        assert_eq!(nt_name("ev", false), Err(STATUS_OBJECT_PATH_SYNTAX_BAD));
        assert_eq!(nt_name("ev", true), Ok("ev".into()));
        assert_eq!(
            nt_name("\\Nowhere\\ev", false),
            Err(STATUS_OBJECT_PATH_NOT_FOUND)
        );
        assert_eq!(
            nt_name("\\Sessions\\x\\BaseNamedObjects\\ev", false),
            Err(STATUS_OBJECT_PATH_NOT_FOUND)
        );
    }

    #[test]
    fn statuses() {
        assert_eq!(status_of(0), STATUS_SUCCESS);
        assert_eq!(status_of(183), STATUS_OBJECT_NAME_EXISTS);
        assert_eq!(status_of(6), STATUS_OBJECT_TYPE_MISMATCH);
        assert_eq!(status_of(298), STATUS_SEMAPHORE_LIMIT_EXCEEDED);
        // a wait's results are already statuses
        assert_eq!(wait_status(0x102), STATUS_TIMEOUT);
        assert_eq!(wait_status(0x81), 0x81);
        assert_eq!(wait_status(obj::WAIT_FAILED), STATUS_INVALID_HANDLE);
    }

    proptest! {
        #[test]
        fn session_paths_map_to_their_name(n in 0u32..100, name in "[A-Za-z0-9_]{1,12}") {
            prop_assert_eq!(nt_name(&format!("\\Sessions\\{n}\\BaseNamedObjects\\{name}"), false), Ok(name.clone()));
            prop_assert_eq!(nt_name(&format!("\\BaseNamedObjects\\{name}"), false), Ok(name));
        }
    }
}
