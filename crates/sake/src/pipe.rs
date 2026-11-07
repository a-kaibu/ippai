//! Pipes for the program (kernel32's pipe functions): handles to the pipe ends of crate::kern,
//! and the I/O on them as kernelbase does it (OVERLAPPED, waiting, the errors).

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::api::{implement, Ctx};
use crate::k32::{arg_str, event_set, last_error};
use crate::kern::{self, Kern, NewPipe, PENDING, SUCCESS};
use crate::mem::{self, put32, r32};
use crate::obj::{self, Obj};

/// A handle's pipe end, and what the handle may do with it.
#[derive(Clone, Copy, Debug)]
pub struct Handle {
    pub end: u32,
    pub read: bool,
    pub write: bool,
    /// opened for overlapped I/O (FILE_FLAG_OVERLAPPED)
    pub ovl: bool,
}

pub fn handle(h: u32) -> Option<Handle> {
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Pipe(p), _)) => Some(*p),
        _ => None,
    })
}

/// The last handle object of an end closed (crate::obj::close).
pub fn closed(p: Handle) {
    kw(|k| k.release(p.end, crate::pid()));
}

fn fail(status: u32) -> u32 {
    last_error(kern::win32(status));
    0
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

/// CreateFile of a pipe path: a client end. (`name` from kern::pipe_name)
pub fn open(name: &str, access: u32, flags: u32) -> u32 {
    let read = access & (GENERIC_READ | GENERIC_ALL | 1) != 0;
    let write = access & (GENERIC_WRITE | GENERIC_ALL | 2 | 4) != 0;
    match kw(|k| k.open(name, read, write, crate::pid())) {
        Ok(end) => {
            last_error(0);
            obj::add(Obj::Pipe(Handle {
                end,
                read,
                write,
                ovl: flags & FILE_FLAG_OVERLAPPED != 0,
            }))
        }
        Err(s) => {
            fail(s);
            0xffff_ffff
        }
    }
}

/// CreatePipe(&read, &write, sa, size)
fn CreatePipe(c: &mut Ctx) -> u32 {
    let (r, w) = kw(|k| k.create_anon(c.a(3), crate::pid()));
    let hr = obj::add(Obj::Pipe(Handle {
        end: r,
        read: true,
        write: false,
        ovl: false,
    }));
    let hw = obj::add(Obj::Pipe(Handle {
        end: w,
        read: false,
        write: true,
        ovl: false,
    }));
    // SECURITY_ATTRIBUTES.bInheritHandle
    if c.a(2) != 0 && r32(c.a(2) + 2 * mem::ps()) != 0 {
        obj::set_inherit(hr, true);
        obj::set_inherit(hw, true);
    }
    mem::putp(c.a(0), hr);
    mem::putp(c.a(1), hw);
    1
}

/// CreateNamedPipe(name, open mode, pipe mode, max instances, out size, in size, default
/// timeout, sa)
fn create_named(c: &mut Ctx, wide: bool) -> u32 {
    let path = arg_str(c.a(0), wide);
    let (open, mode) = (c.a(1), c.a(2));
    let Some(name) = kern::pipe_name(&path) else {
        last_error(123);
        return 0xffff_ffff;
    };
    let p = NewPipe {
        access: open & 3,
        msg_type: mode & 4 != 0,
        msg_read: mode & 2 != 0,
        nonblock: mode & 1 != 0,
        max: c.a(3),
        outsize: c.a(4),
        insize: c.a(5),
        timeout: c.a(6),
        first: open & 0x8_0000 != 0,
    };
    match kw(|k| k.create(&name, &p, crate::pid())) {
        Ok(end) => {
            let h = obj::add(Obj::Pipe(Handle {
                end,
                read: open & 1 != 0,
                write: open & 2 != 0,
                ovl: open & FILE_FLAG_OVERLAPPED != 0,
            }));
            if c.a(7) != 0 && r32(c.a(7) + 2 * mem::ps()) != 0 {
                obj::set_inherit(h, true);
            }
            h
        }
        Err(s) => {
            fail(s);
            0xffff_ffff
        }
    }
}
fn CreateNamedPipeA(c: &mut Ctx) -> u32 {
    create_named(c, false)
}
fn CreateNamedPipeW(c: &mut Ctx) -> u32 {
    create_named(c, true)
}

// ---------------------------------------------------------------------------------------------
// I/O: done at once, waited for, or (overlapped) finished by a thread of its own

/// An operation on the kernel: Ok((bytes, SUCCESS or BUFFER_OVERFLOW)) when done, Err(PENDING)
/// while it waits, Err(status) when it failed. Called again until it is no longer pending.
type Op = Box<dyn FnMut(&mut Kern) -> Result<(u32, u32), u32> + Send>;

/// Overlapped I/O not done yet: the thread that changes what it waits for finishes it (as the
/// kernel does on Windows: a client's CreateFile completes the server's ConnectNamedPipe at
/// once), and a thread of its own watches for changes from elsewhere.
struct Pending {
    h: u32,
    tid: u32,
    done: Done,
    cancel: bool,
    op: Op,
}
/// Where finished I/O goes: an OVERLAPPED (kernelbase's calls), or an NT call's
/// IO_STATUS_BLOCK, event and APC
#[derive(Clone, Copy)]
enum Done {
    Ov(u32),
    Nt(NtIo),
}
/// NtReadFile / NtWriteFile's (iosb, event, APC routine, APC context)
#[derive(Clone, Copy)]
pub struct NtIo {
    pub iosb: u32,
    pub event: u32,
    pub apc: u32,
    pub ctx: u32,
}
static PENDING_IO: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
static WATCHING: AtomicBool = AtomicBool::new(false);

/// kern::with, then the overlapped I/O the change may have finished
fn kw<R>(f: impl FnOnce(&mut Kern) -> R) -> R {
    let r = kern::with(f).0;
    pump();
    r
}

/// Finishes the overlapped I/O that can be finished.
pub fn pump() {
    let mut done = Vec::new();
    {
        let mut g = PENDING_IO.lock().unwrap();
        if g.is_empty() {
            return;
        }
        kern::with(|k| {
            g.retain_mut(|p| {
                match if p.cancel {
                    Err(kern::CANCELLED)
                } else {
                    (p.op)(k)
                } {
                    Err(PENDING) => true,
                    r => {
                        done.push((p.done, p.tid, r));
                        false
                    }
                }
            })
        });
    }
    for (d, tid, r) in done {
        let (n, s) = r.unwrap_or_else(|e| (0, e));
        match d {
            Done::Ov(ov) => ov_done(ov, s, n),
            Done::Nt(nt) => nt_done(nt, tid, s, n),
        }
    }
}

fn watch() {
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| loop {
        let gen = kern::with(|_| ()).1;
        if PENDING_IO.lock().unwrap().is_empty() {
            WATCHING.store(false, Ordering::Release);
            if PENDING_IO.lock().unwrap().is_empty() || WATCHING.swap(true, Ordering::AcqRel) {
                return;
            }
        }
        kern::wait_change(gen, Some(Instant::now() + Duration::from_millis(100)));
        pump();
    });
}

/// OVERLAPPED: Internal, InternalHigh (ULONG_PTRs), Offset, OffsetHigh, hEvent
fn ov_event(ov: u32) -> u32 {
    mem::rp(ov + 2 * mem::ps() + 8)
}
fn ov_done(ov: u32, status: u32, n: u32) {
    mem::wp(ov + mem::ps(), n);
    mem::wp(ov, status);
    let ev = ov_event(ov);
    if ev != 0 {
        event_set(ev, true);
    }
}

/// Runs `op` for handle `h` as kernelbase's ReadFile and friends do: the result in `*out`
/// (0 for overlapped I/O that did not finish without an error), and TRUE, or FALSE with the
/// error (ERROR_IO_PENDING for overlapped I/O left to finish).
fn io(h: u32, p: Handle, ov: u32, out: u32, mut op: Op) -> u32 {
    put32(out, 0);
    if ov != 0 {
        mem::wp(ov, PENDING);
    }
    let mut r = kw(&mut op);
    if r == Err(PENDING) {
        if p.ovl && ov != 0 {
            PENDING_IO.lock().unwrap().push(Pending {
                h,
                tid: crate::cur().tid,
                done: Done::Ov(ov),
                cancel: false,
                op,
            });
            // (a change between the first try and now)
            pump();
            watch();
            last_error(997);
            return 0;
        }
        r = kern::block(None, &|| false, &mut op);
        pump();
    }
    match r {
        Ok((n, s)) => {
            if ov != 0 {
                ov_done(ov, s, n);
            }
            put32(out, if ov != 0 && s != SUCCESS { 0 } else { n });
            if s == SUCCESS {
                1
            } else {
                fail(s)
            }
        }
        // (a failure at once leaves the OVERLAPPED pending, as on Windows)
        Err(s) => fail(s),
    }
}

/// An NT call's I/O finished: its IO_STATUS_BLOCK (Status, Information), its event, its APC
/// queued for the thread that asked (routine(context, iosb, 0))
fn nt_done(nt: NtIo, tid: u32, status: u32, n: u32) {
    if nt.iosb != 0 {
        mem::wp(nt.iosb + mem::ps(), n);
        mem::wp(nt.iosb, status);
    }
    if nt.event != 0 {
        event_set(nt.event, true);
    }
    if nt.apc != 0 {
        crate::apc::queue_args(tid, nt.apc, &[nt.ctx, nt.iosb, 0]);
    }
}
/// `op` as NtReadFile / NtWriteFile run it: on a handle opened for overlapped I/O, I/O that
/// has to wait is STATUS_PENDING (finished later: iosb, event, APC), otherwise it is waited
/// for; the status. A failure at once leaves the iosb as it was.
fn nt_io(h: u32, p: Handle, nt: NtIo, mut op: Op) -> u32 {
    // (the event is reset first)
    if nt.event != 0 {
        event_set(nt.event, false);
    }
    let mut r = kw(&mut op);
    if r == Err(PENDING) {
        if p.ovl {
            PENDING_IO.lock().unwrap().push(Pending {
                h,
                tid: crate::cur().tid,
                done: Done::Nt(nt),
                cancel: false,
                op,
            });
            pump();
            watch();
            return PENDING;
        }
        r = kern::block(None, &|| false, &mut op);
        pump();
    }
    match r {
        Ok((n, s)) => {
            nt_done(nt, crate::cur().tid, s, n);
            s
        }
        Err(s) => s,
    }
}
/// NtReadFile on a pipe handle
pub fn nt_read(h: u32, p: Handle, nt: NtIo, buf: u32, n: u32) -> u32 {
    if !p.read {
        return kern::ACCESS_DENIED;
    }
    let e = p.end;
    nt_io(
        h,
        p,
        nt,
        Box::new(move |k| {
            let (d, more) = k.read(e, n)?;
            mem::write(buf, &d);
            Ok((
                d.len() as u32,
                if more { kern::BUFFER_OVERFLOW } else { SUCCESS },
            ))
        }),
    )
}
/// NtWriteFile on a pipe handle
pub fn nt_write(h: u32, p: Handle, nt: NtIo, buf: u32, n: u32) -> u32 {
    if !p.write {
        return kern::ACCESS_DENIED;
    }
    let (e, data) = (p.end, mem::slice(buf, n).to_vec());
    nt_io(
        h,
        p,
        nt,
        Box::new(move |k| k.write(e, &data).map(|n| (n, SUCCESS))),
    )
}

/// ReadFile on a pipe handle
pub fn read(c: &mut Ctx, h: u32, p: Handle) -> u32 {
    let (buf, n, out, ov) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if !p.read {
        put32(out, 0);
        return fail(kern::ACCESS_DENIED);
    }
    let e = p.end;
    io(
        h,
        p,
        ov,
        out,
        Box::new(move |k| {
            let (d, more) = k.read(e, n)?;
            mem::write(buf, &d);
            Ok((
                d.len() as u32,
                if more { kern::BUFFER_OVERFLOW } else { SUCCESS },
            ))
        }),
    )
}

/// WriteFile on a pipe handle
pub fn write(c: &mut Ctx, h: u32, p: Handle) -> u32 {
    let (buf, n, out, ov) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if !p.write {
        put32(out, 0);
        return fail(kern::ACCESS_DENIED);
    }
    let (e, data) = (p.end, mem::slice(buf, n).to_vec());
    io(
        h,
        p,
        ov,
        out,
        Box::new(move |k| k.write(e, &data).map(|n| (n, SUCCESS))),
    )
}

fn with_pipe(c: &Ctx, f: impl FnOnce(u32, Handle) -> u32) -> u32 {
    match handle(c.a(0)) {
        Some(p) => f(c.a(0), p),
        None => fail(kern::INVALID_HANDLE),
    }
}

/// PeekNamedPipe(h, buf, size, &read, &avail, &left this message)
fn PeekNamedPipe(c: &mut Ctx) -> u32 {
    let (buf, size, pread, pavail, pleft) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    with_pipe(c, |_, p| {
        match kw(|k| k.peek(p.end, if buf == 0 { 0 } else { size })) {
            Ok(pk) => {
                mem::write(buf, &pk.data);
                let n = pk.data.len() as u32;
                put32(pread, n);
                put32(pavail, pk.avail);
                put32(pleft, pk.msg_len.wrapping_sub(n));
                1
            }
            Err(s) => fail(s),
        }
    })
}

/// ConnectNamedPipe(h, ov)
fn ConnectNamedPipe(c: &mut Ctx) -> u32 {
    let ov = c.a(1);
    with_pipe(c, |h, p| {
        let e = p.end;
        let mut started = false;
        io(
            h,
            p,
            ov,
            0,
            Box::new(move |k| {
                let s = if started { k.connected(e) } else { k.listen(e) };
                started = true;
                if s == SUCCESS {
                    Ok((0, SUCCESS))
                } else {
                    Err(s)
                }
            }),
        )
    })
}

/// DisconnectNamedPipe(h)
fn DisconnectNamedPipe(c: &mut Ctx) -> u32 {
    with_pipe(c, |_, p| match kw(|k| k.disconnect(p.end)) {
        SUCCESS => 1,
        s => fail(s),
    })
}

/// TransactNamedPipe(h, in, in size, out, out size, &read, ov)
fn TransactNamedPipe(c: &mut Ctx) -> u32 {
    let (inp, n, outp, cap, pread, ov) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    with_pipe(c, |h, p| {
        let (e, data) = (p.end, mem::slice(inp, n).to_vec());
        let mut sent = false;
        io(
            h,
            p,
            ov,
            pread,
            Box::new(move |k| {
                if !sent {
                    let s = k.transact(e, &data);
                    if s != SUCCESS {
                        return Err(s);
                    }
                    sent = true;
                }
                let (d, more) = k.read(e, cap)?;
                mem::write(outp, &d);
                Ok((
                    d.len() as u32,
                    if more { kern::BUFFER_OVERFLOW } else { SUCCESS },
                ))
            }),
        )
    })
}

/// WaitNamedPipe(name, ms): 0 is the pipe's own default wait, -1 forever
fn wait_named(c: &mut Ctx, wide: bool) -> u32 {
    let Some(name) = kern::pipe_name(&arg_str(c.a(0), wide)) else {
        last_error(161); // ERROR_BAD_PATHNAME
        return 0;
    };
    (wait_named_for(&name, c.a(1)) == SUCCESS) as u32
}
fn WaitNamedPipeA(c: &mut Ctx) -> u32 {
    wait_named(c, false)
}
fn WaitNamedPipeW(c: &mut Ctx) -> u32 {
    wait_named(c, true)
}

/// GetNamedPipeHandleState(h, &state, &instances, &max collection, &timeout, user, user size)
fn handle_state(c: &mut Ctx, _wide: bool) -> u32 {
    let (pstate, pinst, pmax, ptime) = (c.a(1), c.a(2), c.a(3), c.a(4));
    with_pipe(c, |_, p| match kern::with(|k| k.info(p.end)).0 {
        Ok(i) => {
            put32(pstate, (i.nonblock as u32) | (i.msg_read as u32) << 1);
            if pinst != 0 {
                match i.instances {
                    Some(n) => put32(pinst, n),
                    None => return fail(kern::PIPE_DISCONNECTED),
                }
            }
            put32(pmax, 0);
            put32(ptime, 0);
            1
        }
        Err(s) => fail(s),
    })
}
fn GetNamedPipeHandleStateA(c: &mut Ctx) -> u32 {
    handle_state(c, false)
}
fn GetNamedPipeHandleStateW(c: &mut Ctx) -> u32 {
    handle_state(c, true)
}

/// SetNamedPipeHandleState(h, &mode, &max collection, &timeout)
fn SetNamedPipeHandleState(c: &mut Ctx) -> u32 {
    let pmode = c.a(1);
    with_pipe(c, |_, p| {
        if pmode == 0 {
            return 1;
        }
        let m = r32(pmode);
        if m & !3 != 0 {
            return fail(kern::INVALID_PARAMETER);
        }
        match kw(|k| k.set_mode(p.end, Some(m & 2 != 0), Some(m & 1 != 0))) {
            SUCCESS => 1,
            s => fail(s),
        }
    })
}

/// GetNamedPipeInfo(h, &flags, &out size, &in size, &max instances)
fn GetNamedPipeInfo(c: &mut Ctx) -> u32 {
    let (pf, po, pi, pm) = (c.a(1), c.a(2), c.a(3), c.a(4));
    with_pipe(c, |_, p| match kern::with(|k| k.info(p.end)).0 {
        Ok(i) if i.instances.is_none() => fail(kern::PIPE_DISCONNECTED),
        Ok(i) => {
            put32(pf, i.server as u32 | (i.msg_type as u32) << 2);
            put32(po, i.outsize);
            put32(pi, i.insize);
            put32(pm, i.max);
            1
        }
        Err(s) => fail(s),
    })
}

/// CallNamedPipe(name, in, in size, out, out size, &read, timeout): open, message mode,
/// transact, close
fn call_named(c: &mut Ctx, wide: bool) -> u32 {
    let path = arg_str(c.a(0), wide);
    let Some(name) = kern::pipe_name(&path) else {
        last_error(161);
        return 0;
    };
    let mut h = open(&name, GENERIC_READ | GENERIC_WRITE, 0);
    if h == 0xffff_ffff {
        if wait_named_for(&name, c.a(6)) != SUCCESS {
            return 0;
        }
        h = open(&name, GENERIC_READ | GENERIC_WRITE, 0);
        if h == 0xffff_ffff {
            return 0;
        }
    }
    let p = handle(h).unwrap();
    let r = match kw(|k| k.set_mode(p.end, Some(true), None)) {
        SUCCESS => {
            let (inp, n, outp, cap, pread) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
            let data = mem::slice(inp, n).to_vec();
            let e = p.end;
            let mut sent = false;
            io(
                h,
                p,
                0,
                pread,
                Box::new(move |k| {
                    if !sent {
                        let s = k.transact(e, &data);
                        if s != SUCCESS {
                            return Err(s);
                        }
                        sent = true;
                    }
                    let (d, more) = k.read(e, cap)?;
                    mem::write(outp, &d);
                    Ok((
                        d.len() as u32,
                        if more { kern::BUFFER_OVERFLOW } else { SUCCESS },
                    ))
                }),
            )
        }
        s => fail(s),
    };
    obj::close(h);
    r
}
/// Waits for an instance of `name` to listen (`ms` 0: the pipe's default, -1: forever):
/// SUCCESS, or the failure (its error set).
fn wait_named_for(name: &str, ms: u32) -> u32 {
    let mut until: Option<Option<Instant>> = None;
    loop {
        let (r, gen) = kern::with(|k| k.wait_named(name));
        let def = match r {
            Ok(None) => return SUCCESS,
            Ok(Some(def)) => def,
            Err(s) => {
                fail(s);
                return s;
            }
        };
        let until = *until.get_or_insert_with(|| match if ms == 0 { def } else { ms } {
            0xffff_ffff => None,
            w => Some(Instant::now() + Duration::from_millis(w as u64)),
        });
        if until.is_some_and(|u| Instant::now() >= u) {
            fail(kern::IO_TIMEOUT);
            return kern::IO_TIMEOUT;
        }
        kern::wait_change(gen, until);
    }
}
fn CallNamedPipeA(c: &mut Ctx) -> u32 {
    call_named(c, false)
}
fn CallNamedPipeW(c: &mut Ctx) -> u32 {
    call_named(c, true)
}

/// GetOverlappedResult(h, ov, &n, wait)
fn GetOverlappedResult(c: &mut Ctx) -> u32 {
    overlapped_result(c.a(1), c.a(2), if c.a(3) != 0 { 0xffff_ffff } else { 0 })
}
/// GetOverlappedResultEx(h, ov, &n, ms, alertable)
fn GetOverlappedResultEx(c: &mut Ctx) -> u32 {
    overlapped_result(c.a(1), c.a(2), c.a(3))
}
fn overlapped_result(ov: u32, pn: u32, ms: u32) -> u32 {
    let until = (ms != 0xffff_ffff).then(|| Instant::now() + Duration::from_millis(ms as u64));
    while mem::rp(ov) == PENDING {
        if ms == 0 {
            last_error(996); // ERROR_IO_INCOMPLETE
            return 0;
        }
        if until.is_some_and(|t| Instant::now() >= t) {
            last_error(258); // WAIT_TIMEOUT
            return 0;
        }
        match ov_event(ov) {
            0 => std::thread::sleep(Duration::from_millis(1)),
            ev => {
                obj::wait(&[ev], false, 20, None);
            }
        }
    }
    let s = mem::rp(ov);
    put32(pn, mem::rp(ov + mem::ps()));
    if s == SUCCESS {
        // (Windows clears the last error then: tests/winref/golden/pipes-32)
        last_error(0);
        1
    } else {
        fail(s)
    }
}

/// Cancels the pending I/O of handle `h` that `pick` picks; whether there was any.
fn cancel(h: u32, pick: impl Fn(&Pending) -> bool) -> bool {
    let mut any = false;
    for p in PENDING_IO
        .lock()
        .unwrap()
        .iter_mut()
        .filter(|p| p.h == h && pick(p))
    {
        p.cancel = true;
        any = true;
    }
    pump();
    any
}
/// CancelIo / CancelIoEx of a pipe handle (iocp.rs's handle sockets too): this thread's I/O,
/// or anyone's (only that OVERLAPPED's, if given); whether there was any
pub fn cancel_io(h: u32, ov: u32, this_thread: bool) -> bool {
    let tid = crate::cur().tid;
    cancel(h, |p| {
        if this_thread {
            p.tid == tid
        } else {
            ov == 0 || matches!(p.done, Done::Ov(o) if o == ov)
        }
    })
}
/// CancelIo(h): this thread's I/O on it
fn CancelIo(c: &mut Ctx) -> u32 {
    let tid = crate::cur().tid;
    cancel(c.a(0), |p| p.tid == tid);
    1
}
/// CancelIoEx(h, ov): anyone's (that one's, if given)
fn CancelIoEx(c: &mut Ctx) -> u32 {
    let ov = c.a(1);
    if cancel(c.a(0), |p| {
        ov == 0 || matches!(p.done, Done::Ov(o) if o == ov)
    }) {
        1
    } else {
        last_error(1168); // ERROR_NOT_FOUND
        0
    }
}

fn GetNamedPipeClientProcessId(c: &mut Ctx) -> u32 {
    put32(c.a(1), crate::pid());
    with_pipe(c, |_, _| 1)
}

pub fn register() {
    macro_rules! reg {
        ($($f:ident),* $(,)?) => { $( implement("kernel32", stringify!($f), $f); )* };
    }
    reg!(
        CreatePipe,
        CreateNamedPipeA,
        CreateNamedPipeW,
        PeekNamedPipe,
        ConnectNamedPipe,
        DisconnectNamedPipe,
        TransactNamedPipe,
        WaitNamedPipeA,
        WaitNamedPipeW,
        GetNamedPipeHandleStateA,
        GetNamedPipeHandleStateW,
        SetNamedPipeHandleState,
        GetNamedPipeInfo,
        CallNamedPipeA,
        CallNamedPipeW,
        GetOverlappedResult,
        GetOverlappedResultEx,
        CancelIo,
        CancelIoEx,
        GetNamedPipeClientProcessId,
    );
    implement(
        "kernel32",
        "GetNamedPipeServerProcessId",
        GetNamedPipeClientProcessId,
    );
}
