#![allow(non_snake_case)]
//! IO completion ports and overlapped results (kernel32): CreateIoCompletionPort,
//! Get/PostQueuedCompletionStatus(Ex), GetOverlappedResult(Ex), CancelIo(Ex),
//! SetFileCompletionNotificationModes. Files complete their IO at once (ReadFile/WriteFile
//! post here when the handle has a port); sockets (ws2.rs) complete later too.
//!
//! OVERLAPPED { Internal (the NTSTATUS), InternalHigh (bytes), Offset, OffsetHigh, hEvent };
//! an hEvent with its low bit set asks for no completion packet.

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, ps, r32, rp, w32, wp};
use crate::obj::{self, Obj};
use crate::HashMap;
use std::collections::VecDeque;
use std::sync::Mutex;

pub const STATUS_PENDING: u32 = 0x103;

/// (bytes, key, overlapped, NTSTATUS)
type Packet = (u32, u64, u32, u32);

#[derive(Default)]
struct State {
    ports: HashMap<u32, VecDeque<Packet>>,
    /// handle -> (port, key)
    assoc: HashMap<u32, (u32, u64)>,
    /// handle -> FILE_SKIP_* flags
    modes: HashMap<u32, u32>,
    /// handles opened with FILE_FLAG_OVERLAPPED
    overlapped: std::collections::HashSet<u32>,
    /// handles BindIoCompletionCallback gave a callback (their port's pool thread calls it)
    bound: HashMap<u32, u32>,
    /// ports whose last handle was closed while waits still hold them (signaled)
    closed: std::collections::HashSet<u32>,
}
static ST: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(ST.lock().unwrap().get_or_insert_with(State::default))
}

/// NTSTATUS -> Win32 error, for the statuses IO here ends with
pub fn nt_to_win32(s: u32) -> u32 {
    match s {
        0 => 0,
        STATUS_PENDING => 997,
        0xC0000120 => 995,  // STATUS_CANCELLED: ERROR_OPERATION_ABORTED
        0x80000005 => 234,  // STATUS_BUFFER_OVERFLOW: ERROR_MORE_DATA
        0xC000020D => 64,   // STATUS_CONNECTION_RESET: ERROR_NETNAME_DELETED
        0xC000020C => 64,   // STATUS_CONNECTION_DISCONNECTED
        0xC0000236 => 1225, // STATUS_CONNECTION_REFUSED
        0xC000023C => 1231, // STATUS_NETWORK_UNREACHABLE
        0xC00000B5 => 121,  // STATUS_IO_TIMEOUT: ERROR_SEM_TIMEOUT
        0xC0000241 => 1236, // STATUS_CONNECTION_ABORTED
        0xC000000D => 87,
        0xC0000140 => 1229, // STATUS_INVALID_CONNECTION
        0xC00000B0 => 233,  // STATUS_PIPE_DISCONNECTED
        0xC0000011 => 38,   // STATUS_END_OF_FILE: ERROR_HANDLE_EOF
        0xC000009A => 1450, // STATUS_INSUFFICIENT_RESOURCES
        0xC0000008 => 6,
        0xC0000022 => 5,
        0xC00000A3 => 21, // STATUS_DEVICE_NOT_READY
        0xC000020A => 52, // STATUS_ADDRESS_ALREADY_EXISTS: ERROR_DUP_NAME
        0xC0000234 => 1232,
        0xC0000023 => 122, // STATUS_BUFFER_TOO_SMALL: ERROR_INSUFFICIENT_BUFFER
        0xC00000BB => 50,  // STATUS_NOT_SUPPORTED
        s => s,
    }
}

/// Queues a packet on `port` and wakes its waiters
pub fn post(port: u32, bytes: u32, key: u64, ov: u32, status: u32) {
    // (a completion that raced the port's close is dropped: making the queue again would
    // keep a dead port's queue for good and tell its waiters it lives)
    let live = with(|s| s.ports.get_mut(&port).map(|q| q.push_back((bytes, key, ov, status))).is_some());
    if live {
        obj::signal();
    }
}

/// IO on handle `h` with OVERLAPPED `ov` ended (status, bytes): the OVERLAPPED gets them, the
/// event is set and a packet goes to the handle's port, as Windows does. `sync`: it ended
/// within the call (FILE_SKIP_COMPLETION_PORT_ON_SUCCESS skips the packet then).
pub fn complete(h: u32, ov: u32, status: u32, bytes: u32, sync: bool) {
    if ov == 0 {
        return;
    }
    let k = ps();
    wp(ov, status);
    wp(ov + k, bytes);
    let ev = rp(ov + 2 * k + 8);
    let (assoc, mode) = with(|s| (s.assoc.get(&h).copied(), s.modes.get(&h).copied().unwrap_or(0)));
    let mut told = false;
    if ev & !1 != 0 && !(sync && mode & 2 != 0) {
        crate::k32::set_event(ev & !1);
        told = true;
    }
    if let Some((port, key)) = assoc {
        if ev & 1 == 0 && !(sync && status == 0 && mode & 1 != 0) {
            post(port, bytes, key, ov, status);
            told = true;
        }
    }
    // (no event and no port: Windows signals the file handle, which GetOverlappedResult(bWait)
    // with hEvent NULL waits on; its wait here looks at the OVERLAPPED again when woken)
    if !told {
        obj::signal();
    }
}

/// Whether completing on `h` within the call skips the port (FILE_SKIP_COMPLETION_PORT_ON_SUCCESS)
pub fn skips_port(h: u32) -> bool {
    with(|s| s.modes.get(&h).is_some_and(|m| m & 1 != 0) && s.assoc.contains_key(&h))
}

/// Drops what is kept for a closed handle
pub fn forget(h: u32) {
    with(|s| {
        s.assoc.remove(&h);
        s.modes.remove(&h);
        s.ports.remove(&h);
        s.overlapped.remove(&h);
        s.bound.remove(&h);
    });
}

/// CreateFile with FILE_FLAG_OVERLAPPED made `h`
pub fn mark_overlapped(h: u32) {
    with(|s| s.overlapped.insert(h));
}
/// Whether CreateFile made `h` with FILE_FLAG_OVERLAPPED (its IO keeps no file pointer)
pub fn is_overlapped(h: u32) -> bool {
    with(|s| s.overlapped.contains(&h))
}

/// BindIoCompletionCallback(file, callback(error, bytes, overlapped), flags): the file's IO
/// completes on a pool thread that calls `callback`; the file must be overlapped and not
/// bound yet
fn BindIoCompletionCallback(c: &mut Ctx) -> u32 {
    let (h, f) = (c.a(0), c.a(1));
    if h == 0 || h == 0xffff_ffff {
        last_error(6);
        return 0;
    }
    if f == 0 || !with(|s| s.overlapped.contains(&h)) || with(|s| s.bound.contains_key(&h) || s.assoc.contains_key(&h)) {
        last_error(87);
        return 0;
    }
    // (one port for every bound file, its key the file: one IO thread for all of them hands
    // each completion to a pool worker; each file used to have a port and a guest thread of
    // its own, waking once a second)
    let port = bind_port();
    if port == 0 {
        last_error(8);
        return 0;
    }
    with(|s| {
        s.assoc.insert(h, (port, h as u64));
        s.bound.insert(h, f);
    });
    1
}

/// The port of BindIoCompletionCallback's files, its IO thread started (0: no thread).
fn bind_port() -> u32 {
    static PORT: Mutex<u32> = Mutex::new(0);
    static ENTRY: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let mut p = PORT.lock().unwrap();
    if *p == 0 {
        let port = new_port();
        let entry = *ENTRY.get_or_init(|| crate::api::host_fn("pool_io_thread", bind_io_thread, 4));
        if crate::spawn_service(entry, port).is_none() {
            obj::close(port);
            return 0;
        }
        *p = port;
    }
    *p
}

/// BindIoCompletionCallback's IO thread: callback(Win32 error, bytes, overlapped) of each
/// packet's file, on a pool worker
fn bind_io_thread(c: &mut Ctx) -> u32 {
    let port = c.a(0);
    loop {
        match wait_packet(c, port, u32::MAX, false) {
            Ok((bytes, key, ov, status)) => {
                let Some(f) = with(|s| s.bound.get(&(key as u32)).copied()) else { continue };
                let err = nt_to_win32(status);
                crate::pool::submit(Box::new(move |c| {
                    c.call(f, &[err, bytes, ov]);
                }));
            }
            Err(258) => {}
            Err(_) => return 0,
        }
    }
}

/// A port handle was closed: its packets go, its waiters end (ERROR_ABANDONED_WAIT_0)
/// The port's last handle was closed: its waiters end (GetQueuedCompletionStatus:
/// ERROR_ABANDONED_WAIT_0; a wait on the port: signaled)
pub fn closed(h: u32) {
    with(|s| {
        s.ports.remove(&h);
        s.assoc.retain(|_, (p, _)| *p != h);
        s.closed.insert(h);
    });
    obj::signal();
}
/// The port object went (the last wait on it ended too)
pub fn gone(h: u32) {
    with(|s| {
        s.ports.remove(&h);
        s.assoc.retain(|_, (p, _)| *p != h);
        s.closed.remove(&h);
    });
}

/// The port and key handle `h` is tied to
pub fn association(h: u32) -> Option<(u32, u64)> {
    with(|s| s.assoc.get(&h).copied())
}

/// Whether a port has packets (a port handle is signaled then)
pub fn has_packets(h: u32) -> bool {
    with(|s| s.closed.contains(&h) || s.ports.get(&h).is_some_and(|q| !q.is_empty()))
}

/// A new port's handle
pub fn new_port() -> u32 {
    let p = obj::add(Obj::Other("iocp"));
    with(|s| s.ports.insert(p, VecDeque::new()));
    p
}

fn is_port(h: u32) -> bool {
    obj::with(|t| matches!(t.objs.get(&h), Some((Obj::Other("iocp"), _))))
}

/// CreateIoCompletionPort(file, port, key, threads)
fn CreateIoCompletionPort(c: &mut Ctx) -> u32 {
    let (file, port, key) = (c.a(0), c.a(1), if mem::x64() { c.a64(2) } else { c.a(2) as u64 });
    let invalid = file == u32::MAX;
    if invalid && port != 0 {
        last_error(87);
        return 0;
    }
    if port != 0 && !is_port(port) {
        last_error(6);
        return 0;
    }
    if !invalid && obj::with(|t| t.objs.get(&file).is_none()) && !crate::ws2::is_socket(file) {
        last_error(6);
        return 0;
    }
    let port = if port == 0 {
        new_port()
    } else {
        port
    };
    if !invalid {
        with(|s| s.assoc.insert(file, (port, key)));
    }
    port
}

fn take(port: u32) -> Option<Packet> {
    with(|s| s.ports.get_mut(&port).and_then(|q| q.pop_front()))
}

/// Waits for a packet on `port` (or an APC when alertable): Ok(packet), Err(win32 error)
fn wait_packet(c: &mut Ctx, port: u32, ms: u32, alertable: bool) -> Result<Packet, u32> {
    if !is_port(port) {
        return Err(6);
    }
    let tid = crate::cur().tid;
    let deadline = (ms != u32::MAX).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms as u64));
    loop {
        // (AFD polls end here: a socket that changed is looked at by the port's waiter)
        crate::afd::check();
        if let Some(p) = take(port) {
            return Ok(p);
        }
        // (closed while waiting)
        if !with(|s| s.ports.contains_key(&port)) {
            return Err(735); // ERROR_ABANDONED_WAIT_0
        }
        if alertable && crate::apc::run(c) {
            return Err(crate::apc::WAIT_IO_COMPLETION);
        }
        let left = match deadline {
            None => u32::MAX,
            Some(d) => {
                let now = std::time::Instant::now();
                if now >= d {
                    return Err(258); // WAIT_TIMEOUT
                }
                (d - now).as_millis().max(1) as u32
            }
        };
        let ready = || with(|s| s.ports.get(&port).is_none_or(|q| !q.is_empty())) || (alertable && crate::apc::pending(tid));
        // (AFD polls waiting: ws2 ends them as sockets change; a look now and then besides, as a
        // safety net: every 10 ms kept an idle network program's waiters from ever sleeping)
        let left = if crate::afd::pending() { left.min(250) } else { left };
        obj::wait(&[], false, left, Some(&ready));
    }
}

/// GetQueuedCompletionStatus(port, &bytes, &key, &overlapped, ms)
fn GetQueuedCompletionStatus(c: &mut Ctx) -> u32 {
    let (port, bp, kp, op, ms) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    match wait_packet(c, port, ms, false) {
        Ok((bytes, key, ov, status)) => {
            mem::put32(bp, bytes);
            if kp != 0 {
                if mem::x64() {
                    mem::w64(kp, key);
                } else {
                    w32(kp, key as u32);
                }
            }
            if op != 0 {
                wp(op, ov);
            }
            if status != 0 {
                last_error(nt_to_win32(status));
                return 0;
            }
            1
        }
        Err(e) => {
            if op != 0 {
                wp(op, 0);
            }
            last_error(e);
            0
        }
    }
}

/// GetQueuedCompletionStatusEx(port, entries, count, &removed, ms, alertable):
/// OVERLAPPED_ENTRY { key, overlapped, Internal, bytes }
fn GetQueuedCompletionStatusEx(c: &mut Ctx) -> u32 {
    let (port, ents, count, rp_, ms, alertable) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5) != 0);
    if count == 0 || ents == 0 {
        last_error(87);
        return 0;
    }
    let k = ps();
    match wait_packet(c, port, ms, alertable) {
        Ok(first) => {
            let mut n = 0;
            let mut p = Some(first);
            while let Some((bytes, key, ov, status)) = p {
                let e = ents + 4 * k * n;
                if mem::x64() {
                    mem::w64(e, key);
                } else {
                    w32(e, key as u32);
                }
                wp(e + k, ov);
                wp(e + 2 * k, status);
                w32(e + 3 * k, bytes);
                n += 1;
                p = if n < count { take(port) } else { None };
            }
            mem::put32(rp_, n);
            1
        }
        Err(e) => {
            mem::put32(rp_, 0);
            last_error(e);
            0
        }
    }
}

/// PostQueuedCompletionStatus(port, bytes, key, overlapped)
fn PostQueuedCompletionStatus(c: &mut Ctx) -> u32 {
    let (port, bytes, key, ov) = (c.a(0), c.a(1), if mem::x64() { c.a64(2) } else { c.a(2) as u64 }, c.a(3));
    if !is_port(port) {
        last_error(6);
        return 0;
    }
    post(port, bytes, key, ov, 0);
    1
}

/// Waits until the OVERLAPPED is no longer pending (on its event when it has one)
pub fn wait_overlapped(c: &mut Ctx, ov: u32, ms: u32, alertable: bool) -> u32 {
    let k = ps();
    let ev = rp(ov + 2 * k + 8) & !1;
    if rp(ov) != STATUS_PENDING {
        return 0;
    }
    let done = || rp(ov) != STATUS_PENDING;
    let evs = [ev];
    let hs: &[u32] = if ev != 0 { &evs } else { &[] };
    let r = if alertable {
        crate::apc::wait(c, hs, false, ms)
    } else if ev != 0 {
        obj::wait(&[ev], false, ms, Some(&done))
    } else {
        obj::wait(&[], false, ms, Some(&done))
    };
    if rp(ov) != STATUS_PENDING {
        0
    } else {
        r
    }
}

/// GetOverlappedResult(h, ov, &bytes, wait) / GetOverlappedResultEx(h, ov, &bytes, ms, alertable)
fn overlapped_result(c: &mut Ctx, ex: bool) -> u32 {
    let (ov, bp) = (c.a(1), c.a(2));
    let (ms, alertable) = if ex { (c.a(3), c.a(4) != 0) } else { (if c.a(3) != 0 { u32::MAX } else { 0 }, false) };
    if ov == 0 {
        last_error(87);
        return 0;
    }
    if rp(ov) == STATUS_PENDING {
        if ms == 0 {
            last_error(996); // ERROR_IO_INCOMPLETE
            return 0;
        }
        let r = wait_overlapped(c, ov, ms, alertable);
        if rp(ov) == STATUS_PENDING {
            last_error(match r {
                crate::apc::WAIT_IO_COMPLETION => crate::apc::WAIT_IO_COMPLETION,
                0x102 => 258,
                _ => 996,
            });
            return 0;
        }
    }
    let k = ps();
    mem::put32(bp, rp(ov + k));
    let s = rp(ov);
    if s != 0 {
        last_error(nt_to_win32(s));
        return 0;
    }
    // (success clears the last error: tests/winref/golden/pipes-32)
    last_error(0);
    1
}

/// A wait_packet error as an NTSTATUS
fn status_of(e: u32) -> u32 {
    match e {
        735 => 0x80,  // STATUS_ABANDONED_WAIT_0
        258 => 0x102, // STATUS_TIMEOUT
        6 => 0xc000_0008,
        e => e,
    }
}

/// ntdll's completion calls: NtCreateIoCompletion(&h, access, attr, threads),
/// NtSetIoCompletion(port, key, value, status, information),
/// NtRemoveIoCompletion(port, &key, &value, iosb, timeout),
/// NtRemoveIoCompletionEx(port, FILE_IO_COMPLETION_INFORMATION[], count, &removed, timeout,
/// alertable): { key, value, iosb { status, information } }
pub fn init_nt() {
    let d = "ntdll";
    implement(d, "NtCreateIoCompletion", |c| {
        if c.a(0) == 0 {
            return 0xc000_0005;
        }
        wp(c.a(0), new_port());
        0
    });
    let set = |c: &mut Ctx| {
        let (port, key, value, status, info) = (c.a(0), if mem::x64() { c.a64(1) } else { c.a(1) as u64 }, c.a(2), c.a(3), c.a(4));
        if !is_port(port) {
            return 0xc000_0008;
        }
        post(port, info, key, value, status);
        0
    };
    implement(d, "NtSetIoCompletion", set);
    implement(d, "NtRemoveIoCompletion", |c| {
        let (port, pkey, pval, iosb, ms) = (c.a(0), c.a(1), c.a(2), c.a(3), crate::ntsync::timeout_ms(c.a(4)));
        match wait_packet(c, port, ms, false) {
            Ok((bytes, key, ov, status)) => {
                wp(pkey, key as u32);
                if mem::x64() {
                    mem::w64(pkey, key);
                }
                wp(pval, ov);
                wp(iosb, status);
                wp(iosb + ps(), bytes);
                0
            }
            Err(e) => status_of(e),
        }
    });
    implement(d, "NtRemoveIoCompletionEx", |c| {
        let (port, info, count, removed, ms, alertable) = (c.a(0), c.a(1), c.a(2), c.a(3), crate::ntsync::timeout_ms(c.a(4)), c.a(5) & 0xff != 0);
        let k = ps();
        match wait_packet(c, port, ms, alertable) {
            Ok(first) => {
                let (mut n, mut p) = (0, Some(first));
                while let Some((bytes, key, ov, status)) = p {
                    let e = info + 4 * k * n;
                    wp(e, key as u32);
                    if mem::x64() {
                        mem::w64(e, key);
                    }
                    wp(e + k, ov);
                    wp(e + 2 * k, status);
                    wp(e + 3 * k, bytes);
                    n += 1;
                    p = if n < count { take(port) } else { None };
                }
                mem::put32(removed, n);
                0
            }
            Err(e) => {
                mem::put32(removed, 0);
                if e == crate::apc::WAIT_IO_COMPLETION { 0xc0 } else { status_of(e) }
            }
        }
    });
}

pub fn init() {
    let d = "kernel32";
    implement(d, "CreateIoCompletionPort", CreateIoCompletionPort);
    implement(d, "BindIoCompletionCallback", BindIoCompletionCallback);
    implement(d, "GetQueuedCompletionStatus", GetQueuedCompletionStatus);
    implement(d, "GetQueuedCompletionStatusEx", GetQueuedCompletionStatusEx);
    implement(d, "PostQueuedCompletionStatus", PostQueuedCompletionStatus);
    implement(d, "GetOverlappedResult", |c| overlapped_result(c, false));
    implement(d, "GetOverlappedResultEx", |c| overlapped_result(c, true));
    // CancelIo(h) / CancelIoEx(h, ov): sockets' and pipes' IO waits
    implement(d, "CancelIo", |c| {
        let h = c.a(0);
        crate::ws2::cancel(h, 0, true);
        crate::pipe::cancel_io(h, 0, true);
        1
    });
    implement(d, "CancelIoEx", |c| {
        let (h, ov) = (c.a(0), c.a(1));
        if crate::ws2::cancel(h, ov, false) | crate::pipe::cancel_io(h, ov, false) {
            1
        } else {
            last_error(1168); // ERROR_NOT_FOUND
            0
        }
    });
    implement(d, "SetFileCompletionNotificationModes", |c| {
        let (h, f) = (c.a(0), c.a(1));
        if f & !3 != 0 {
            last_error(87);
            return 0;
        }
        // (only a handle that is one: a garbage value kept here would hand its FILE_SKIP_*
        // flags to the next real handle of that number)
        if obj::with(|t| t.objs.get(&h).is_none()) && !crate::ws2::is_socket(h) {
            last_error(6);
            return 0;
        }
        with(|s| *s.modes.entry(h).or_default() |= f);
        1
    });
    let _ = r32;
}
