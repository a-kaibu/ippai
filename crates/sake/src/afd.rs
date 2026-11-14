//! \Device\Afd: the ancillary function driver's poll, as Rust's mio (tokio) and libuv wait for
//! sockets on Windows. They open "\Device\Afd\<name>" with NtCreateFile, tie it to a completion
//! port and ask IOCTL_AFD_POLL through NtDeviceIoControlFile for each socket; the answer comes
//! as a completion packet (ApcContext is its OVERLAPPED value) once one of the asked events is
//! true, or now when one already is. NtCancelIoFileEx ends a poll with STATUS_CANCELLED. Sockets
//! are ws2.rs's; a poll is looked at again whenever a socket changes (ws2's state signals the
//! waiters) and when a thread waits on the port (iocp.rs).
//!
//! AFD_POLL_INFO { LARGE_INTEGER Timeout; ULONG NumberOfHandles; ULONG Exclusive;
//!                 AFD_POLL_HANDLE_INFO { HANDLE Handle; ULONG Events; NTSTATUS Status } [] }

use crate::api::{implement, Ctx};
use crate::mem::{ps, r32, rp, w32, wp};
use crate::obj::{self, Obj};
use std::sync::Mutex;

pub const IOCTL_AFD_POLL: u32 = 0x0001_2024;
pub const RECEIVE: u32 = 0x1;
pub const RECEIVE_EXPEDITED: u32 = 0x2;
pub const SEND: u32 = 0x4;
pub const DISCONNECT: u32 = 0x8;
pub const ABORT: u32 = 0x10;
pub const LOCAL_CLOSE: u32 = 0x20;
pub const ACCEPT: u32 = 0x80;
pub const CONNECT_FAIL: u32 = 0x100;

const STATUS_PENDING: u32 = 0x103;
const STATUS_CANCELLED: u32 = 0xc000_0120;
const STATUS_NOT_FOUND: u32 = 0xc000_0225;
const STATUS_INVALID_PARAMETER: u32 = 0xc000_000d;
const STATUS_INSUFFICIENT_RESOURCES: u32 = 0xc000_009a;

/// A socket's state as the poll sees it (ws2.rs fills it in)
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SockState {
    pub rx: bool,
    pub oob: bool,
    pub listening_queue: bool,
    pub stream: bool,
    pub connected: bool,
    pub connect_failed: bool,
    pub eof: bool,
    pub reset: bool,
    pub bound: bool,
}

/// The AFD_POLL_* events now true of a socket (None: no such socket, LOCAL_CLOSE)
pub fn events(s: Option<SockState>) -> u32 {
    let Some(s) = s else { return LOCAL_CLOSE };
    let mut e = 0;
    if s.rx {
        e |= RECEIVE;
    }
    if s.oob {
        e |= RECEIVE_EXPEDITED;
    }
    if s.listening_queue {
        e |= ACCEPT;
    }
    if s.stream {
        if s.connected && !s.connect_failed && !s.reset {
            e |= SEND;
        }
        if s.connected && s.eof {
            e |= DISCONNECT;
        }
        if s.reset {
            e |= ABORT;
        }
        if s.connect_failed {
            e |= CONNECT_FAIL;
        }
    } else if s.bound {
        e |= SEND;
    }
    e
}

/// A poll asked of AFD handle `afd` that has not ended
#[derive(Clone, Debug)]
struct Poll {
    afd: u32,
    event: u32,
    ctx: u32,
    iosb: u32,
    out: u32,
    out_len: u32,
    /// (socket, events asked)
    handles: Vec<(u32, u32)>,
}

static POLLS: Mutex<Vec<Poll>> = Mutex::new(Vec::new());

/// Size of AFD_POLL_INFO with `n` handles (`ptr`: the pointer size)
/// (in 64 bits: `16 + n * 12` in u32 wrapped for a guest's n = 0x55555556 to 24, passed the
/// buffer check and the handles were then collected for 1.4 billion entries, review A-7)
pub fn info_size(n: u32, ptr: u32) -> u64 {
    16 + n as u64 * (ptr as u64 + 8).next_multiple_of(ptr as u64)
}

/// Most handles one poll takes (mio and libuv ask for one; Windows has no fixed limit, but the
/// list is held here per pending poll).
pub const MAX_HANDLES: u32 = 1 << 16;

/// The handles that fired (socket, events true & asked; LOCAL_CLOSE when gone) of `asked`
pub fn fired(asked: &[(u32, u32)], now: impl Fn(u32) -> u32) -> Vec<(u32, u32)> {
    asked
        .iter()
        .filter_map(|&(h, want)| {
            let e = now(h) & (want | LOCAL_CLOSE);
            (e != 0).then_some((h, e))
        })
        .collect()
}

fn is_afd(h: u32) -> bool {
    obj::with(|t| matches!(t.objs.get(&h), Some((Obj::Other("afd"), _))))
}

/// NtCreateFile of "\Device\Afd..." (path without the "\??\"): a handle, or None for another name
pub fn open(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    (n == "\\device\\afd" || n.starts_with("\\device\\afd\\")).then(|| obj::add(Obj::Other("afd")))
}

/// Writes the answer of a poll that ended (status, the fired handles) and tells its waiter
fn finish(p: &Poll, status: u32, got: &[(u32, u32)]) {
    let k = ps();
    let mut info = 0;
    if status == 0 {
        // (Timeout and Exclusive as they were asked)
        let n = got.len() as u32;
        let size = info_size(n, k);
        if p.out != 0 && p.out_len as u64 >= size {
            w32(p.out + 8, n);
            let es = (k + 8).next_multiple_of(k);
            for (i, &(h, e)) in got.iter().enumerate() {
                let at = p.out + 16 + i as u32 * es;
                wp(at, h);
                if k == 8 {
                    w32(at + 4, 0);
                }
                w32(at + k, e);
                w32(at + k + 4, 0);
            }
            info = size as u32;
        }
    }
    if p.iosb != 0 {
        wp(p.iosb, status);
        wp(p.iosb + k, info);
    }
    if p.event != 0 {
        crate::k32::set_event(p.event);
    }
    if let Some((port, key)) = crate::iocp::association(p.afd) {
        if p.ctx != 0 {
            crate::iocp::post(port, info, key, p.ctx, status);
        }
    }
}

/// Ends the polls whose sockets have an asked event now; true if any did
pub fn check() -> bool {
    let done: Vec<(Poll, Vec<(u32, u32)>)> = {
        let mut ps = POLLS.lock().unwrap();
        if ps.is_empty() {
            return false;
        }
        let mut done = Vec::new();
        ps.retain(|p| {
            if !is_afd(p.afd) {
                return false;
            }
            let got = fired(&p.handles, |h| events(crate::ws2::afd_state(h)));
            if got.is_empty() {
                return true;
            }
            done.push((p.clone(), got));
            false
        });
        done
    };
    for (p, got) in &done {
        finish(p, 0, got);
    }
    !done.is_empty()
}

/// Whether polls wait (a port's waiter looks at them)
pub fn pending() -> bool {
    !POLLS.lock().unwrap().is_empty()
}

/// NtDeviceIoControlFile(h, event, apc, apcctx, iosb, code, in, inlen, out, outlen) on an AFD handle
pub fn ioctl(c: &mut Ctx) -> Option<u32> {
    let h = c.a(0);
    if !is_afd(h) {
        return None;
    }
    let (event, ctx, iosb, code, inp, inlen, out, outlen) = (c.a(1), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7), c.a(8), c.a(9));
    let k = ps();
    if code != IOCTL_AFD_POLL || inp == 0 || inlen < 16 {
        return Some(STATUS_INVALID_PARAMETER);
    }
    let n = r32(inp + 8);
    if info_size(n, k) > inlen as u64 || n == 0 {
        return Some(STATUS_INVALID_PARAMETER);
    }
    if n > MAX_HANDLES {
        return Some(STATUS_INSUFFICIENT_RESOURCES);
    }
    let es = (k + 8).next_multiple_of(k);
    let handles = (0..n).map(|i| (rp(inp + 16 + i * es), r32(inp + 16 + i * es + k))).collect();
    let p = Poll { afd: h, event, ctx, iosb, out, out_len: outlen, handles };
    if event != 0 {
        crate::k32::reset_event(event);
    }
    let got = fired(&p.handles, |s| events(crate::ws2::afd_state(s)));
    if !got.is_empty() {
        finish(&p, 0, &got);
        return Some(0);
    }
    if iosb != 0 {
        wp(iosb, STATUS_PENDING);
    }
    POLLS.lock().unwrap().push(p);
    // (a socket that became ready since `fired` above: ws2's check ran before the poll was
    // here; formal/tla/AfdPoll.tla)
    check();
    Some(STATUS_PENDING)
}

/// Ends polls of `afd` with STATUS_CANCELLED: the one with that IO_STATUS_BLOCK, or all (0)
fn cancel(afd: u32, iosb: u32) -> bool {
    let gone: Vec<Poll> = {
        let mut ps = POLLS.lock().unwrap();
        let (gone, keep) = ps.drain(..).partition(|p| p.afd == afd && (iosb == 0 || p.iosb == iosb));
        *ps = keep;
        gone
    };
    for p in &gone {
        finish(p, STATUS_CANCELLED, &[]);
    }
    !gone.is_empty()
}

/// The AFD handle was closed: its polls end as cancelled
pub fn closed(h: u32) {
    cancel(h, 0);
}

pub fn init() {
    // NtCancelIoFileEx(h, iosb, &status_iosb), NtCancelIoFile(h, &status_iosb)
    implement("ntdll", "NtCancelIoFileEx", |c| {
        let (h, io, st) = (c.a(0), c.a(1), c.a(2));
        let s = if !is_afd(h) {
            c.call(crate::export_once!("kernel32", "CancelIoEx"), &[h, 0]);
            0
        } else if cancel(h, io) {
            0
        } else {
            STATUS_NOT_FOUND
        };
        if st != 0 && s == 0 {
            wp(st, 0);
            wp(st + ps(), 0);
        }
        s
    });
    implement("ntdll", "NtCancelIoFile", |c| {
        let (h, st) = (c.a(0), c.a(1));
        if is_afd(h) {
            cancel(h, 0);
        } else {
            c.call(crate::export_once!("kernel32", "CancelIo"), &[h]);
        }
        if st != 0 {
            wp(st, 0);
            wp(st + ps(), 0);
        }
        0
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn layout() {
        // (x86: 12-byte entries; x64: 16)
        assert_eq!(info_size(1, 4), 28);
        assert_eq!(info_size(2, 4), 40);
        assert_eq!(info_size(1, 8), 32);
        assert_eq!(info_size(3, 8), 64);
        // (wrapped to 24 in u32)
        assert_eq!(info_size(0x5555_5556, 4), 16 + 0x5555_5556u64 * 12);
        assert_eq!(info_size(u32::MAX, 8), 16 + u32::MAX as u64 * 16);
    }

    #[test]
    fn socket_events() {
        let s = SockState { stream: true, connected: true, ..Default::default() };
        assert_eq!(events(Some(s)), SEND);
        assert_eq!(events(Some(SockState { rx: true, ..s })), SEND | RECEIVE);
        assert_eq!(events(Some(SockState { eof: true, ..s })), SEND | DISCONNECT);
        assert_eq!(events(Some(SockState { reset: true, ..s })), ABORT);
        assert_eq!(events(Some(SockState { stream: true, connect_failed: true, ..Default::default() })), CONNECT_FAIL);
        assert_eq!(events(Some(SockState { stream: true, listening_queue: true, ..Default::default() })), ACCEPT);
        // an unconnected stream socket: nothing; a bound datagram one can send
        assert_eq!(events(Some(SockState { stream: true, ..Default::default() })), 0);
        assert_eq!(events(Some(SockState { bound: true, ..Default::default() })), SEND);
        assert_eq!(events(None), LOCAL_CLOSE);
    }

    fn state() -> impl Strategy<Value = Option<SockState>> {
        prop::option::of(
            (any::<[bool; 9]>()).prop_map(|b| SockState {
                rx: b[0],
                oob: b[1],
                listening_queue: b[2],
                stream: b[3],
                connected: b[4],
                connect_failed: b[5],
                eof: b[6],
                reset: b[7],
                bound: b[8],
            }),
        )
    }

    proptest! {
        // a poll reports only what was asked (and LOCAL_CLOSE, which needs no asking), every
        // asked event that is true, and the handles in the order asked
        #[test]
        fn fired_is_asked_and_true(asks in prop::collection::vec((0u32..8, 0u32..0x200), 1..8),
                                   states in prop::collection::vec(state(), 8)) {
            let now = |h: u32| events(states[h as usize]);
            let got = fired(&asks, now);
            let mut i = 0;
            for &(h, want) in &asks {
                let e = now(h) & (want | LOCAL_CLOSE);
                if e != 0 {
                    prop_assert_eq!(got[i], (h, e));
                    i += 1;
                }
            }
            prop_assert_eq!(i, got.len());
        }

        // SEND and ABORT never together; a gone socket is LOCAL_CLOSE alone
        #[test]
        fn events_consistent(s in state()) {
            let e = events(s);
            prop_assert!(e & SEND == 0 || e & ABORT == 0);
            prop_assert_eq!(s.is_none(), e == LOCAL_CLOSE);
        }

        // the entries fit the size for any count, both pointer sizes
        #[test]
        fn entries_fit(n in 1u32..1000, x64 in any::<bool>()) {
            let k = if x64 { 8 } else { 4 };
            let es = (k + 8u32).next_multiple_of(k);
            prop_assert_eq!(16 + n as u64 * es as u64, info_size(n, k));
            prop_assert!(es >= k + 8);
        }

        /// Review A-7: for every count the guest can pass, the size does not wrap: a buffer of
        /// `inlen` bytes passes the check only when it holds all `n` entries.
        #[test]
        fn size_never_wraps(n: u32, inlen: u32, x64 in any::<bool>()) {
            let k = if x64 { 8 } else { 4 };
            let size = info_size(n, k);
            prop_assert!(size >= 16 + n as u64 * 12);
            if size <= inlen as u64 {
                prop_assert!(n as u64 * (k as u64 + 8) <= inlen as u64);
            }
        }
    }
}
