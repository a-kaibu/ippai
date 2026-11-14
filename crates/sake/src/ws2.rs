#![allow(non_snake_case)]
//! ws2_32 / wsock32: Windows sockets, without a network. Sockets live in this process: TCP
//! connects to a listening socket here and UDP datagrams reach sockets bound here, over the
//! loopback addresses (127.x.x.x, ::1, and the any address as a destination for neither).
//! Anything else is unreachable, as on a machine without a connection. Blocking calls really
//! block (another thread's send wakes them); non-blocking ones, select, WSAPoll,
//! WSAEventSelect, WSAAsyncSelect, overlapped IO (events, completion routines, completion
//! ports) and the Microsoft extensions (AcceptEx, ConnectEx, ...) behave as Windows'.
//!
//! The rest of ws2_32 (names, address text, the catalog) is ws2name.rs.

use crate::api::{implement, Ctx};
use crate::iocp::{self, STATUS_PENDING};
use crate::mem::{self, ps, r16, r32, rp, w16, w32, wp};
use crate::obj::{self, Obj};
use crate::ws2name::{self as names, read_sa, write_sa, Addr, AF_INET, AF_INET6, AF_UNSPEC, WSAEAFNOSUPPORT, WSAEFAULT, WSAEINVAL, WSANOTINITIALISED};
use crate::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SOCKET_ERROR: u32 = u32::MAX;
pub const WSAEINTR: u32 = 10004;
pub const WSAEACCES: u32 = 10013;
pub const WSAEWOULDBLOCK: u32 = 10035;
pub const WSAEALREADY: u32 = 10037;
pub const WSAENOTSOCK: u32 = 10038;
pub const WSAEDESTADDRREQ: u32 = 10039;
pub const WSAEMSGSIZE: u32 = 10040;
pub const WSAEPROTOTYPE: u32 = 10041;
pub const WSAENOPROTOOPT: u32 = 10042;
pub const WSAEPROTONOSUPPORT: u32 = 10043;
pub const WSAESOCKTNOSUPPORT: u32 = 10044;
pub const WSAEOPNOTSUPP: u32 = 10045;
pub const WSAEADDRINUSE: u32 = 10048;
pub const WSAEADDRNOTAVAIL: u32 = 10049;
pub const WSAENETUNREACH: u32 = 10051;
pub const WSAECONNABORTED: u32 = 10053;
pub const WSAECONNRESET: u32 = 10054;
pub const WSAENOBUFS: u32 = 10055;
pub const WSAEISCONN: u32 = 10056;
pub const WSAENOTCONN: u32 = 10057;
pub const WSAESHUTDOWN: u32 = 10058;
pub const WSAETIMEDOUT: u32 = 10060;
pub const WSAECONNREFUSED: u32 = 10061;
const WSATRY_AGAIN: u32 = 11002;
pub const WSA_IO_PENDING: u32 = 997;
pub const WSA_IO_INCOMPLETE: u32 = 996;
pub const WSA_OPERATION_ABORTED: u32 = 995;

const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;

const FD_READ: u32 = 1;
const FD_WRITE: u32 = 2;
const FD_OOB: u32 = 4;
const FD_ACCEPT: u32 = 8;
const FD_CONNECT: u32 = 16;
const FD_CLOSE: u32 = 32;
const FD_ALL: u32 = 0x3ff;

const MSG_OOB: u32 = 1;
const MSG_PEEK: u32 = 2;
const MSG_WAITALL: u32 = 8;
const MSG_PARTIAL: u32 = 0x8000;
const MSG_TRUNC: u32 = 0x0100;

const STATUS_CANCELLED: u32 = 0xC0000120;
const STATUS_BUFFER_OVERFLOW: u32 = 0x80000005;
const STATUS_CONNECTION_RESET: u32 = 0xC000020D;

// ---- the thread's last error and startup ----

pub fn last(e: u32) {
    crate::k32::last_error(e);
}
pub fn get_last() -> u32 {
    r32(crate::cur().teb + crate::teb_last_error())
}
/// Sets the error and returns SOCKET_ERROR
pub fn err(e: u32) -> u32 {
    last(e);
    SOCKET_ERROR
}

static STARTED: AtomicU32 = AtomicU32::new(0);
pub fn started() -> bool {
    STARTED.load(Ordering::Relaxed) > 0
}

/// WSAStartup(version, &WSADATA): WSADATA { wVersion, wHighVersion, (x64: iMaxSockets,
/// iMaxUdpDg, lpVendorInfo,) szDescription[257], szSystemStatus[129], (x86: iMaxSockets,
/// iMaxUdpDg, lpVendorInfo) }
fn WSAStartup(c: &mut Ctx) -> u32 {
    let (v, d) = (c.a(0) & 0xffff, c.a(1));
    let (lo, hi) = (v & 0xff, v >> 8);
    // the version given back: 1.0 / 1.1, 2.0 - 2.2, else 2.2 (also for an unsupported 0.x)
    let ver = match lo {
        1 => if hi == 0 { 0x0001 } else { 0x0101 },
        2 if hi <= 2 => v,
        _ => 0x0202,
    };
    if d == 0 {
        return if lo == 0 { 10092 } else { WSAEFAULT };
    }
    // WSADATA: x86 { wVersion, wHighVersion, szDescription[257], szSystemStatus[129],
    // iMaxSockets, iMaxUdpDg, lpVendorInfo }; x64 puts the last three before the strings.
    // lpVendorInfo is left alone; the limits are only reported to 1.x callers.
    let (desc, status, limits) = if mem::x64() { (d + 16, d + 16 + 257, d + 4) } else { (d + 4, d + 4 + 257, d + 390) };
    mem::write(desc, &[0; 257]);
    mem::write(status, &[0; 129]);
    w16(d, ver as u16);
    w16(d + 2, 0x0202);
    mem::write(desc, b"WinSock 2.0\0");
    mem::write(status, b"Running\0");
    let v1 = lo == 1;
    w16(limits, if v1 { 32767 } else { 0 });
    w16(limits + 2, if v1 { 65467 } else { 0 });
    if lo == 0 {
        return 10092; // WSAVERNOTSUPPORTED
    }
    STARTED.fetch_add(1, Ordering::Relaxed);
    0
}
/// WSACleanup: the last one closes every socket
fn WSACleanup(_: &mut Ctx) -> u32 {
    let n = STARTED.load(Ordering::Relaxed);
    if n == 0 {
        return err(WSANOTINITIALISED);
    }
    STARTED.store(n - 1, Ordering::Relaxed);
    if n == 1 {
        let hs: Vec<u32> = with(|st, _| st.handles.keys().copied().collect());
        for h in hs {
            close_socket(h);
        }
    }
    last(0);
    0
}

// ---- sockets ----

/// Where a socket's network events go (WSAEventSelect / WSAAsyncSelect)
#[derive(Clone, Copy, PartialEq)]
enum Notify {
    None,
    Event(u32),
    Msg(u32, u32),
}

enum PendKind {
    /// WSARecv / WSARecvFrom: the buffers, &flags, &from, &fromlen
    Recv { bufs: Vec<(u32, u32)>, flags: u32, from: u32, fromlen: u32, peek: bool, msg: u32 },
    /// AcceptEx: the socket to accept into, its buffer (data length, local and remote sizes)
    Accept { acc: u32, buf: u32, data: u32, llen: u32, rlen: u32 },
    /// SIO_ADDRESS_LIST_CHANGE and the like: never completes (until cancelled)
    Forever,
}

struct Pend {
    kind: PendKind,
    ov: u32,
    routine: u32,
    tid: u32,
    handle: u32,
}

struct Sock {
    af: u32,
    ty: u32,
    proto: u32,
    nonblock: bool,
    local: Option<Addr>,
    peer: Option<Addr>,
    listening: bool,
    backlog: usize,
    /// connections waiting for accept (ids of their server-side sockets)
    queue: VecDeque<u32>,
    /// the other end of a TCP connection
    conn: Option<u32>,
    connected: bool,
    rx: VecDeque<u8>,
    /// (source, data, destination)
    dgrams: VecDeque<(Addr, Vec<u8>, Addr)>,
    /// the destination of the last datagram received (IP_PKTINFO's)
    last_dst: Option<Addr>,
    /// the urgent (MSG_OOB) byte waiting to be read apart, and whether one came inline
    /// (SO_OOBINLINE)
    oob: Option<u8>,
    oob_inline: bool,
    /// the other end shut its sending side or closed
    eof: bool,
    reset: bool,
    /// the other end is gone (a send after that resets)
    peer_gone: bool,
    shut_rd: bool,
    shut_wr: bool,
    so_error: u32,
    connect_failed: bool,
    /// the result FD_CONNECT reports, once
    connect_result: Option<u32>,
    udp_reset: bool,
    udp_connreset: bool,
    reuse: bool,
    exclusive: bool,
    v6only: bool,
    broadcast: bool,
    linger: (u16, u16),
    rcvtimeo: u32,
    sndtimeo: u32,
    rcvbuf: u32,
    sndbuf: u32,
    opts: HashMap<(u32, u32), u32>,
    notify: Notify,
    mask: u32,
    netev: u32,
    neterr: [u32; 10],
    reported: u32,
    pend: Vec<Pend>,
    connect_time: Option<Instant>,
    /// the handle that refers to it (its own id) and any duplicates
    nhandles: u32,
    /// bind order (the last bound UDP socket gets unicast datagrams)
    bind_seq: u64,
}

impl Sock {
    fn new(af: u32, ty: u32, proto: u32) -> Sock {
        Sock {
            af,
            ty,
            proto,
            nonblock: false,
            local: None,
            peer: None,
            listening: false,
            backlog: 0,
            queue: VecDeque::new(),
            conn: None,
            connected: false,
            rx: VecDeque::new(),
            dgrams: VecDeque::new(),
            last_dst: None,
            oob: None,
            oob_inline: false,
            eof: false,
            reset: false,
            peer_gone: false,
            shut_rd: false,
            shut_wr: false,
            so_error: 0,
            connect_failed: false,
            connect_result: None,
            udp_reset: false,
            udp_connreset: true,
            reuse: false,
            exclusive: false,
            v6only: true,
            broadcast: false,
            linger: (0, 0),
            rcvtimeo: 0,
            sndtimeo: 0,
            rcvbuf: 65536,
            sndbuf: 65536,
            opts: HashMap::default(),
            notify: Notify::None,
            mask: 0,
            netev: 0,
            neterr: [0; 10],
            reported: 0,
            pend: Vec::new(),
            connect_time: None,
            nhandles: 1,
            bind_seq: 0,
        }
    }
    fn stream(&self) -> bool {
        self.ty == SOCK_STREAM
    }
    fn readable(&self) -> bool {
        (self.listening && !self.queue.is_empty())
            || !self.rx.is_empty()
            || !self.dgrams.is_empty()
            || self.udp_reset
            || (self.stream() && self.connected && (self.eof || self.reset || self.shut_rd))
    }
    fn writable(&self) -> bool {
        if self.stream() {
            self.connected && !self.connect_failed
        } else {
            true
        }
    }
    /// the network events now true
    fn conditions(&self) -> u32 {
        let mut c = 0;
        if !self.rx.is_empty() || !self.dgrams.is_empty() {
            c |= FD_READ;
        }
        if self.listening && !self.queue.is_empty() {
            c |= FD_ACCEPT;
        }
        if (self.stream() && self.connected && !self.connect_failed) || (!self.stream() && self.local.is_some()) {
            c |= FD_WRITE;
        }
        if self.connect_result.is_some() {
            c |= FD_CONNECT;
        }
        if self.oob.is_some() {
            c |= FD_OOB;
        }
        if self.stream() && self.connected && (self.reset || (self.eof && self.rx.is_empty())) {
            c |= FD_CLOSE;
        }
        c
    }
    fn info(&self) -> (u32, u32, u32) {
        (self.af, self.ty, self.proto)
    }
}

/// What to do after the lock is dropped (these take other locks)
enum Act {
    SetEvent(u32),
    ResetEvent(u32),
    Post(u32, u32, u32, u32),
    Apc(u32, u32, [u32; 4]),
    Complete(u32, u32, u32, u32, bool),
    Close(u32),
}

#[derive(Default)]
struct State {
    socks: HashMap<u32, Sock>,
    /// handle -> socket id
    handles: HashMap<u32, u32>,
    next_port: u16,
    seq: u64,
    dirty: bool,
    /// OVERLAPPED -> the flags its WSARecv ended with
    ov_flags: HashMap<u32, u32>,
}

static ST: Mutex<Option<State>> = Mutex::new(None);
/// bumped whenever a socket changes (blocked calls wait for it to move)
static GEN: AtomicU64 = AtomicU64::new(0);

fn with<R>(f: impl FnOnce(&mut State, &mut Vec<Act>) -> R) -> R {
    let mut acts = Vec::new();
    let (r, dirty) = {
        let mut g = ST.lock().unwrap_or_else(|e| e.into_inner());
        let st = g.get_or_insert_with(|| State { next_port: 49152, ..Default::default() });
        let r = f(st, &mut acts);
        let d = std::mem::take(&mut st.dirty);
        if d {
            GEN.fetch_add(1, Ordering::SeqCst);
        }
        (r, d)
    };
    run(acts);
    if dirty {
        obj::signal();
    }
    r
}

fn run(acts: Vec<Act>) {
    for a in acts {
        match a {
            Act::SetEvent(h) => {
                crate::k32::set_event(h);
            }
            Act::ResetEvent(h) => {
                crate::k32::event_set(h, false);
            }
            Act::Post(w, m, a, b) => {
                crate::user::post(w, m, a, b);
            }
            Act::Apc(tid, f, a) => crate::apc::queue_args(tid, f, &a),
            Act::Complete(h, ov, status, bytes, sync) => iocp::complete(h, ov, status, bytes, sync),
            Act::Close(h) => {
                obj::close(h);
            }
        }
    }
}

/// Runs `f` until it gives a result, waiting for socket changes between tries; None on timeout.
/// With a guest context the wait is alertable: queued APCs run and the wait goes on, as in Windows.
fn block<R>(mut c: Option<&mut Ctx>, ms: u32, mut f: impl FnMut(&mut State, &mut Vec<Act>) -> Option<R>) -> Option<R> {
    let tid = crate::cur().tid;
    let deadline = (ms != u32::MAX).then(|| Instant::now() + Duration::from_millis(ms as u64));
    loop {
        let (r, g) = with(|st, acts| (f(st, acts), GEN.load(Ordering::SeqCst)));
        if r.is_some() {
            return r;
        }
        let alert = c.is_some();
        if let Some(c) = c.as_deref_mut() {
            if crate::apc::pending(tid) {
                crate::apc::run(c);
                continue;
            }
        }
        let left = match deadline {
            None => u32::MAX,
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    return None;
                }
                (d - now).as_millis().max(1) as u32
            }
        };
        obj::wait(&[], false, left, Some(&|| GEN.load(Ordering::SeqCst) != g || (alert && crate::apc::pending(tid))));
    }
}

/// A row of iphlpapi's TCP / UDP tables: one of this process's sockets
#[derive(Clone, Debug, PartialEq)]
pub struct ConnRow {
    pub tcp: bool,
    pub af: u32,
    pub local: Addr,
    pub remote: Option<Addr>,
    /// MIB_TCP_STATE_* (LISTEN 2, ESTAB 5, CLOSE_WAIT 8); 0 for UDP
    pub state: u32,
}

/// The sockets the TCP / UDP tables show: listening and connected TCP sockets, bound UDP
/// ones
pub fn conn_rows() -> Vec<ConnRow> {
    with(|st, _| {
        let mut v = Vec::new();
        for s in st.socks.values() {
            let Some(local) = s.local.clone() else { continue };
            if s.stream() {
                let state = if s.listening {
                    2
                } else if s.conn.is_some() || s.connected {
                    if s.eof { 8 } else { 5 }
                } else {
                    continue;
                };
                v.push(ConnRow { tcp: true, af: s.af, local, remote: if s.listening { None } else { s.peer.clone() }, state });
            } else if s.ty == SOCK_DGRAM {
                v.push(ConnRow { tcp: false, af: s.af, local, remote: None, state: 0 });
            }
        }
        v
    })
}

/// DuplicateHandle of a socket: another handle to it
pub fn dup_socket(h: u32) -> Option<u32> {
    let id = with(|st, _| st.id(h).ok())?;
    let n = new_handle();
    with(|st, _| {
        st.handles.insert(n, id);
        st.socks.get_mut(&id).unwrap().nhandles += 1;
    });
    Some(n)
}

/// A socket's state for an AFD poll (afd.rs); None: not a socket (closed)
pub fn afd_state(h: u32) -> Option<crate::afd::SockState> {
    with(|st, _| {
        let s = st.socks.get(&st.id(h).ok()?)?;
        Some(crate::afd::SockState {
            rx: !s.rx.is_empty() || !s.dgrams.is_empty() || s.udp_reset,
            oob: s.oob.is_some(),
            listening_queue: s.listening && !s.queue.is_empty(),
            stream: s.stream(),
            connected: s.connected,
            connect_failed: s.connect_failed,
            eof: s.eof || s.shut_rd,
            reset: s.reset,
            bound: s.local.is_some(),
        })
    })
}

pub fn is_socket(h: u32) -> bool {
    with(|st, _| st.handles.contains_key(&h))
}

/// The most a SO_RCVBUF / SO_SNDBUF counts for (programs set 1 GiB "to be safe"; sums of
/// two such values used to wrap to a tiny cap)
const MAX_BUF: u32 = 4 << 20;
/// The least (Windows' default 8 KB)
const MIN_BUF: u32 = 8192;
fn buf_size(v: u32) -> usize {
    v.clamp(MIN_BUF, MAX_BUF) as usize
}
/// How much a stream connection holds unread before its sender waits
pub(crate) fn pipe_cap(rcvbuf: u32, sndbuf: u32) -> usize {
    buf_size(rcvbuf) + buf_size(sndbuf)
}
/// Whether a datagram of `n` bytes still fits a receive queue holding `queued` bytes (Windows
/// drops datagrams once the receive buffer is full; at least one always fits)
pub(crate) fn dgram_fits(queued: usize, n: usize, rcvbuf: u32) -> bool {
    queued == 0 || queued + n <= buf_size(rcvbuf)
}
/// The most one send takes (a stream send of more sends this much; the program sends the rest)
const MAX_SEND: u32 = 16 << 20;
/// The most WSABUFs / WSAPOLLFDs one call takes
const MAX_BUFS: u32 = 1 << 16;
/// The most OVERLAPPED flags kept for WSAGetOverlappedResult (only non-zero ones are kept)
const MAX_OV_FLAGS: usize = 4096;

fn new_handle() -> u32 {
    obj::add(Obj::Other("socket"))
}

impl State {
    fn touch(&mut self) {
        self.dirty = true;
    }
    /// The flags an overlapped operation on `ov` ended with: only non-zero ones are kept (the
    /// map used to grow by every OVERLAPPED address ever used), at most MAX_OV_FLAGS
    fn set_ov_flags(&mut self, ov: u32, fl: u32) {
        if fl == 0 {
            self.ov_flags.remove(&ov);
            return;
        }
        if self.ov_flags.len() >= MAX_OV_FLAGS && !self.ov_flags.contains_key(&ov) {
            self.ov_flags.clear();
        }
        self.ov_flags.insert(ov, fl);
    }
    /// The first connection queued on listener `id` that still exists (one closed through its
    /// handle while queued is dropped from the queue, not unwrapped)
    fn queued(&mut self, id: u32, take: bool) -> Option<u32> {
        loop {
            let q = *self.socks.get(&id)?.queue.front()?;
            if self.socks.contains_key(&q) {
                if take {
                    self.socks.get_mut(&id)?.queue.pop_front();
                }
                return Some(q);
            }
            self.socks.get_mut(&id)?.queue.pop_front();
        }
    }
    fn id(&self, h: u32) -> Result<u32, u32> {
        self.handles.get(&h).copied().ok_or(WSAENOTSOCK)
    }
    fn port_free(&self, ty: u32, port: u16) -> bool {
        !self.socks.values().any(|s| s.ty == ty && s.local.as_ref().is_some_and(|l| l.port == port))
    }
    fn ephemeral(&mut self, ty: u32) -> u16 {
        for _ in 0..16384 {
            let p = self.next_port;
            self.next_port = if p >= 65535 { 49152 } else { p + 1 };
            if self.port_free(ty, p) {
                return p;
            }
        }
        0
    }

    /// Reports the events that became true to the socket's event or window (each once,
    /// until the call that re-enables it)
    /// Whether a stream socket's sends must wait: the other end holds as much unread data as
    /// its receive buffer and this send buffer take
    fn full(&self, id: u32) -> bool {
        let Some(s) = self.socks.get(&id) else { return false };
        match s.conn.and_then(|p| self.socks.get(&p)) {
            Some(o) if s.stream() => o.rx.len() >= pipe_cap(o.rcvbuf, s.sndbuf),
            _ => false,
        }
    }

    /// After a receive on `id`: the sender at the other end may write again
    fn drained(&mut self, id: u32, acts: &mut Vec<Act>) {
        if let Some(p) = self.socks.get(&id).and_then(|s| s.conn) {
            self.touch();
            self.refresh(p, acts);
        }
    }

    fn refresh(&mut self, id: u32, acts: &mut Vec<Act>) {
        let full = self.full(id);
        let Some(s) = self.socks.get_mut(&id) else { return };
        let new = s.conditions() & s.mask & !s.reported & if full { !FD_WRITE } else { !0 };
        if new == 0 || s.notify == Notify::None {
            return;
        }
        s.reported |= new;
        s.netev |= new;
        if new & FD_CONNECT != 0 {
            s.neterr[4] = s.connect_result.unwrap_or(0);
        }
        if new & FD_CLOSE != 0 {
            s.neterr[5] = if s.reset { WSAECONNRESET } else { 0 };
        }
        match s.notify {
            Notify::Event(e) => acts.push(Act::SetEvent(e)),
            Notify::Msg(w, m) => {
                for (bit, i) in [(FD_CONNECT, 4), (FD_ACCEPT, 3), (FD_WRITE, 1), (FD_READ, 0), (FD_OOB, 2), (FD_CLOSE, 5)] {
                    if new & bit != 0 {
                        acts.push(Act::Post(w, m, id, bit | s.neterr[i] << 16));
                    }
                }
            }
            Notify::None => {}
        }
    }

    /// Completes the socket's waiting overlapped receives (and accepts) that can complete now
    fn serve(&mut self, id: u32, acts: &mut Vec<Act>) {
        loop {
            let Some(s) = self.socks.get_mut(&id) else { return };
            let Some(i) = s.pend.iter().position(|p| !matches!(p.kind, PendKind::Forever)) else { return };
            match &s.pend[i].kind {
                PendKind::Recv { bufs, flags, from, fromlen, peek, msg } => {
                    let (bufs, flags, from, fromlen, peek, msg) = (bufs.clone(), *flags, *from, *fromlen, *peek, *msg);
                    let fl = if peek { MSG_PEEK } else { 0 };
                    match take_data(s, &bufs, fl) {
                        Ok(None) => return,
                        r => {
                            let p = s.pend.remove(i);
                            let (status, n, outfl) = match r {
                                Ok(Some((n, src, trunc))) => {
                                    if let (Some(a), true) = (src, from != 0) {
                                        let _ = write_sa(from, fromlen, &a);
                                    }
                                    let fl = recv_flags(s, msg, trunc);
                                    if fl != 0 { (STATUS_BUFFER_OVERFLOW, n, fl) } else { (0, n, 0) }
                                }
                                Err(e) => (wsa_to_nt(e), 0, 0),
                                Ok(None) => unreachable!(),
                            };
                            if flags != 0 {
                                w32(flags, outfl);
                            }
                            s.reported &= !FD_READ;
                if !s.readable() {
                    s.netev &= !FD_READ; // (nothing left: the recorded FD_READ is gone too)
                }
                            self.drained(id, acts);
                            // (WSARecvMsg reports its flags in the WSAMSG only)
                            let outfl = if msg != 0 { 0 } else { outfl };
                            if msg == 0 {
                                self.set_ov_flags(p.ov, outfl);
                            }
                            finish(&p, status, n, outfl, acts);
                            self.refresh(id, acts);
                        }
                    }
                }
                PendKind::Accept { .. } => {
                    if s.queue.is_empty() && !s.pend.is_empty() {
                        return;
                    }
                    let p = s.pend.remove(i);
                    let PendKind::Accept { acc, buf, data, llen, rlen } = p.kind else { unreachable!() };
                    let r = self.accept_into(id, acc, acts);
                    let (status, n) = match r {
                        Ok(()) => {
                            put_accept_addrs(self, acc, buf, data, llen, rlen);
                            (0, 0)
                        }
                        Err(e) => (wsa_to_nt(e), 0),
                    };
                    let p = Pend { kind: PendKind::Forever, ..p };
                    finish(&p, status, n, 0, acts);
                }
                PendKind::Forever => return,
            }
        }
    }

    /// Moves the first queued connection of listener `id` into socket handle `acc` (AcceptEx)
    fn accept_into(&mut self, id: u32, acc: u32, acts: &mut Vec<Act>) -> Result<(), u32> {
        let aid = self.id(acc)?;
        let Some(q) = self.queued(id, true) else { return Err(WSAEWOULDBLOCK) };
        if !self.socks.contains_key(&aid) {
            return Err(WSAENOTSOCK);
        }
        let Some(mut new) = self.socks.remove(&q) else { return Err(WSAEWOULDBLOCK) };
        // the accepted connection takes the place of the given socket
        let Some(old) = self.socks.remove(&aid) else { return Err(WSAENOTSOCK) };
        new.nhandles = old.nhandles;
        new.notify = old.notify;
        new.mask = old.mask;
        new.nonblock = old.nonblock;
        if let Some(c) = new.conn {
            if let Some(cl) = self.socks.get_mut(&c) {
                cl.conn = Some(aid);
            }
        }
        self.socks.insert(aid, new);
        // the server side was given a handle of its own when it was made; it goes now
        self.handles.remove(&q);
        acts.push(Act::Close(q));
        if let Some(l) = self.socks.get_mut(&id) {
            l.reported &= !FD_ACCEPT;
        }
        self.touch();
        self.refresh(id, acts);
        self.refresh(aid, acts);
        Ok(())
    }
}

/// The overlapped operation `p` ended: the OVERLAPPED, its event, the completion routine (as an
/// APC to the thread that asked) and the completion port
fn finish(p: &Pend, status: u32, bytes: u32, flags: u32, acts: &mut Vec<Act>) {
    acts.push(Act::Complete(p.handle, p.ov, status, bytes, false));
    if p.routine != 0 {
        acts.push(Act::Apc(p.tid, p.routine, [nt_to_wsa(status), bytes, p.ov, flags]));
    }
}

pub fn wsa_to_nt(e: u32) -> u32 {
    match e {
        0 => 0,
        WSAECONNRESET => STATUS_CONNECTION_RESET,
        WSAECONNABORTED => 0xC0000241,
        WSAEMSGSIZE => STATUS_BUFFER_OVERFLOW,
        WSA_OPERATION_ABORTED => STATUS_CANCELLED,
        WSAECONNREFUSED => 0xC0000236,
        WSAENETUNREACH => 0xC000023C,
        WSAESHUTDOWN => 0xC00000B0,
        WSAETIMEDOUT => 0xC00000B5,
        WSAENOTCONN => 0xC0000140,
        WSAEADDRINUSE => 0xC000020A,
        WSAEWOULDBLOCK => 0xC00000A3,
        WSAEINVAL => 0xC000000D,
        WSAEADDRNOTAVAIL => 0xC0000141,
        WSAEOPNOTSUPP => 0xC00000BB,
        WSAEFAULT => 0xC0000023,
        // (no status of its own: kept in the customer range, which nt_to_wsa reads back)
        e => 0xE0000001 | (e & 0x3fff) << 16,
    }
}
/// NTSTATUS -> Winsock error, by Windows' table (as WSAGetOverlappedResult reports it): other
/// failures are WSAEINVAL, successes no error
pub fn nt_to_wsa(s: u32) -> u32 {
    // (an error with no status of its own, from wsa_to_nt)
    if s >> 28 == 0xE && s & 0xffff == 1 {
        return s >> 16 & 0x3fff;
    }
    match s {
        0x00000103 => 996, // PENDING
        0x80000005 => 10040, // BUFFER_OVERFLOW
        0xC0000002 => 10045, // NOT_IMPLEMENTED
        0xC0000005 => 10014, // ACCESS_VIOLATION
        0xC0000007 => 10055, // PAGEFILE_QUOTA
        0xC0000008 => 10038, // INVALID_HANDLE
        0xC000000E => 10050, // NO_SUCH_DEVICE
        0xC000000F => 10050, // NO_SUCH_FILE
        0xC0000017 => 10055, // NO_MEMORY
        0xC0000018 => 10055, // CONFLICTING_ADDRESSES
        0xC0000022 => 10013, // ACCESS_DENIED
        0xC0000023 => 10014, // BUFFER_TOO_SMALL
        0xC0000024 => 10038, // OBJECT_TYPE_MISMATCH
        0xC0000034 => 10050, // OBJECT_NAME_NOT_FOUND
        0xC000003A => 10050, // OBJECT_PATH_NOT_FOUND
        0xC0000043 => 10048, // SHARING_VIOLATION
        0xC0000044 => 10055, // QUOTA_EXCEEDED
        0xC0000097 => 10055, // TOO_MANY_PAGING_FILES
        0xC000009A => 10055, // INSUFFICIENT_RESOURCES
        0xC00000A1 => 10055, // WORKING_SET_QUOTA
        0xC00000A3 => 10035, // DEVICE_NOT_READY
        0xC00000B0 => 10058, // PIPE_DISCONNECTED
        0xC00000B5 => 10060, // IO_TIMEOUT
        0xC00000BB => 10045, // NOT_SUPPORTED
        0xC00000BC => 10061, // REMOTE_NOT_LISTENING
        0xC00000BE => 10051, // BAD_NETWORK_PATH
        0xC00000BF => 10050, // NETWORK_BUSY
        0xC00000C3 => 10050, // INVALID_NETWORK_RESPONSE
        0xC00000C4 => 10050, // UNEXPECTED_NETWORK_ERROR
        0xC00000D0 => 10035, // REQUEST_NOT_ACCEPTED
        0xC0000120 => 995, // CANCELLED
        0xC000012D => 10055, // COMMITMENT_LIMIT
        0xC000013B => 10053, // LOCAL_DISCONNECT
        0xC000013C => 10054, // REMOTE_DISCONNECT
        0xC000013D => 10055, // REMOTE_RESOURCES
        0xC000013E => 10054, // LINK_FAILED
        0xC000013F => 10060, // LINK_TIMEOUT
        0xC0000140 => 10057, // INVALID_CONNECTION
        0xC0000141 => 10049, // INVALID_ADDRESS
        0xC0000206 => 10040, // INVALID_BUFFER_SIZE
        0xC0000207 => 10049, // INVALID_ADDRESS_COMPONENT
        0xC0000209 => 10055, // TOO_MANY_ADDRESSES
        0xC000020A => 10048, // ADDRESS_ALREADY_EXISTS
        0xC000020C => 10054, // CONNECTION_DISCONNECTED
        0xC000020D => 10054, // CONNECTION_RESET
        0xC000020F => 10053, // TRANSACTION_ABORTED
        0xC0000236 => 10061, // CONNECTION_REFUSED
        0xC0000237 => 10101, // GRACEFUL_DISCONNECT
        0xC000023B => 10056, // CONNECTION_ACTIVE
        0xC000023C => 10051, // NETWORK_UNREACHABLE
        0xC000023D => 10065, // HOST_UNREACHABLE
        0xC000023E => 10051, // PROTOCOL_UNREACHABLE
        0xC000023F => 10054, // PORT_UNREACHABLE
        0xC0000240 => 10004, // REQUEST_ABORTED
        0xC0000241 => 10053, // CONNECTION_ABORTED
        0xC00002C5 => 10014, // DATATYPE_MISALIGNMENT_ERROR
        0xC0000350 => 10064, // HOST_DOWN
        0x800703E4 => 996, // 0x80070000|IO_INCOMPLETE
        0xC00103E4 => 996, // 0xc0010000|IO_INCOMPLETE
        0xC00703E4 => 996, // 0xc0070000|IO_INCOMPLETE
        s if s >> 31 == 0 => 0,
        _ => WSAEINVAL,
    }
}

/// Takes received data for `bufs` (address, length): Ok(None) when there is none yet,
/// Ok(Some((bytes, sender, truncated))), or the error the receive ends with
fn take_data(s: &mut Sock, bufs: &[(u32, u32)], flags: u32) -> Result<Option<(u32, Option<Addr>, bool)>, u32> {
    let total: u32 = bufs.iter().map(|b| b.1).sum();
    let peek = flags & MSG_PEEK != 0;
    if s.stream() {
        if !s.connected {
            return Err(WSAENOTCONN);
        }
        if s.shut_rd {
            return Err(WSAESHUTDOWN);
        }
        if flags & MSG_OOB != 0 {
            // the urgent byte (none kept apart with SO_OOBINLINE, or none sent: WSAEINVAL)
            if s.opts.get(&(SOL_SOCKET, 0x100)).is_some_and(|&v| v != 0) || total == 0 {
                return Err(WSAEINVAL);
            }
            let Some(b) = (if peek { s.oob } else { s.oob.take() }) else { return Err(WSAEINVAL) };
            scatter(bufs, &[b]);
            return Ok(Some((1, None, false)));
        }
        if s.rx.is_empty() {
            if s.reset {
                return Err(WSAECONNRESET);
            }
            if s.eof {
                return Ok(Some((0, None, false)));
            }
            return Ok(None);
        }
        if flags & MSG_WAITALL != 0 && !peek && (s.rx.len() as u32) < total && !s.eof && !s.reset {
            return Ok(None);
        }
        let n = (s.rx.len() as u32).min(total);
        let data: Vec<u8> = if peek { s.rx.iter().take(n as usize).copied().collect() } else { s.rx.drain(..n as usize).collect() };
        scatter(bufs, &data);
        return Ok(Some((n, None, false))); // (a stream receive reports no source address)
    }
    if s.local.is_none() {
        return Err(WSAEINVAL);
    }
    if s.udp_reset {
        s.udp_reset = false;
        return Err(WSAECONNRESET);
    }
    let Some((from, d, dst)) = (if peek { s.dgrams.front().cloned() } else { s.dgrams.pop_front() }) else {
        return Ok(None);
    };
    s.last_dst = Some(dst);
    let n = (d.len() as u32).min(total);
    scatter(bufs, &d[..n as usize]);
    Ok(Some((n, Some(from), (d.len() as u32) > total)))
}

fn scatter(bufs: &[(u32, u32)], mut data: &[u8]) {
    for &(p, len) in bufs {
        if data.is_empty() {
            break;
        }
        let n = (len as usize).min(data.len());
        mem::write(p, &data[..n]);
        data = &data[n..];
    }
}

/// WSABUF array { ULONG len; CHAR *buf }
/// (a count past MAX_BUFS, or an array outside the guest, is taken as none: the call then
/// fails or sends nothing, not an allocation of the count)
/// Whether a WSABUF array of `n` at `p` can be read (wsabufs takes it)
fn bufs_ok(p: u32, n: u32) -> bool {
    n <= MAX_BUFS && (n == 0 || p != 0 && mem::in_guest(p, n * 2 * ps()))
}
fn wsabufs(p: u32, n: u32) -> Vec<(u32, u32)> {
    let k = ps();
    if !bufs_ok(p, n) {
        return Vec::new();
    }
    (0..n).map(|i| (rp(p + 2 * k * i + k), r32(p + 2 * k * i))).collect()
}

/// Whether a bound address `l` (of a socket with `v6only`) takes traffic for `dst`
fn takes(l: &Addr, v6only: bool, dst: &Addr) -> bool {
    if l.port != dst.port {
        return false;
    }
    match (l.is_v6(), dst.is_v6()) {
        (false, false) => l.any() || l.ip == dst.ip,
        (true, true) => l.any() || l.ip == dst.ip || (!v6only && l.as_v4().is_some() && l.ip == dst.ip),
        (true, false) => !v6only && (l.any() || l.as_v4() == dst.as_v4()),
        (false, true) => dst.as_v4().is_some_and(|d| l.any() || l.ip == d),
    }
}

/// `a` as a socket of family `af` sees it (IPv4 addresses mapped for IPv6 sockets)
fn seen_as(a: &Addr, af: u32) -> Addr {
    match (af, a.is_v6()) {
        (AF_INET6, false) => Addr::mapped(a.ip[..4].try_into().unwrap(), a.port),
        (AF_INET, true) => a.as_v4().map_or(a.clone(), |v| Addr::v4(v, a.port)),
        _ => a.clone(),
    }
}

/// Whether an address can be bound here
fn bindable(a: &Addr, v6only: bool) -> bool {
    if a.any() || a.loopback() {
        return a.is_v6() || !a.is_v6() || !v6only;
    }
    false
}

/// The local address a connection or datagram to `dst` comes from, for socket `s`
fn source_for(s: &Sock, dst: &Addr) -> Addr {
    let ip = match s.local.as_ref() {
        Some(l) if !l.any() => l.clone(),
        _ => {
            if dst.is_v6() {
                if let Some(v4) = dst.as_v4() {
                    Addr::mapped(if v4[0] == 127 { v4 } else { [127, 0, 0, 1] }, 0)
                } else {
                    let mut lo = [0u8; 16];
                    lo[15] = 1;
                    Addr::v6(lo, 0)
                }
            } else {
                Addr::v4(if dst.ip[0] == 127 { dst.ip[..4].try_into().unwrap() } else { [127, 0, 0, 1] }, 0)
            }
        }
    };
    let mut ip = ip;
    ip.port = s.local.as_ref().map_or(0, |l| l.port);
    ip
}

// ---- creating and closing ----

/// socket / WSASocket: Ok(handle)
fn create(af: u32, ty: u32, proto: u32) -> Result<u32, u32> {
    if !started() {
        return Err(WSANOTINITIALISED);
    }
    // Windows' order of checks (ws2_32's test_WSASocket table): AF_UNSPEC with no protocol,
    // then the type, the family, and the protocol for the type
    let (mut af, mut ty, mut proto) = (af, ty, proto);
    if af == AF_UNSPEC && proto == 0 {
        return Err(WSAEINVAL);
    }
    if (af as i32) < 0 {
        return Err(WSAEAFNOSUPPORT);
    }
    match ty {
        0..=2 => {}
        3 if matches!(af, AF_INET | AF_INET6) => return Err(WSAEACCES), // raw sockets need an administrator
        3..=5 => return Err(WSAESOCKTNOSUPPORT),
        _ => return Err(WSAESOCKTNOSUPPORT),
    }
    match af {
        AF_INET | AF_INET6 => {}
        AF_UNSPEC => af = AF_INET,
        _ => return Err(WSAEAFNOSUPPORT),
    }
    if ty == 0 {
        ty = match proto {
            6 | 0 => SOCK_STREAM,
            17 => SOCK_DGRAM,
            _ => return Err(WSAEPROTONOSUPPORT),
        };
    }
    let want = if ty == SOCK_STREAM { 6 } else { 17 };
    if proto == 0 {
        proto = want;
    } else if proto != want {
        return Err(WSAEPROTONOSUPPORT);
    }
    let h = new_handle();
    with(|st, _| {
        st.socks.insert(h, Sock::new(af, ty, proto));
        st.handles.insert(h, h);
        st.touch();
    });
    Ok(h)
}

fn ret_socket(c: &mut Ctx, r: Result<u32, u32>) -> u32 {
    match r {
        Ok(h) => {
            last(0);
            h
        }
        Err(e) => {
            last(e);
            if mem::x64() { c.ret64(u64::MAX) } else { u32::MAX }
        }
    }
}

fn socket(c: &mut Ctx) -> u32 {
    let r = create(c.a(0), c.a(1), c.a(2));
    ret_socket(c, r)
}

/// WSASocketA/W(af, type, protocol, protocolInfo, group, flags); a protocol info from
/// WSADuplicateSocket gives another handle to that socket
fn WSASocket(c: &mut Ctx) -> u32 {
    let (mut af, mut ty, mut proto, info) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if info != 0 {
        let dup = r32(info + 112);
        if dup != 0 {
            let id = with(|st, _| st.id(dup).ok());
            if let Some(id) = id {
                let h = new_handle();
                with(|st, _| {
                    st.handles.insert(h, id);
                    st.socks.get_mut(&id).unwrap().nhandles += 1;
                });
                return h;
            }
        }
        const FROM: u32 = u32::MAX; // FROM_PROTOCOL_INFO
        if af == FROM || af == 0 {
            af = r32(info + 76);
        }
        if ty == FROM || ty == 0 {
            ty = r32(info + 88);
        }
        if proto == FROM || proto == 0 {
            proto = r32(info + 92);
        }
    }
    let r = create(af, ty, proto);
    ret_socket(c, r)
}

/// Ends socket `id`: waiting IO is cancelled, the other end of a connection sees the close
/// (a reset when unread data or a zero linger is left), queued connections are reset
fn teardown(st: &mut State, id: u32, acts: &mut Vec<Act>) {
    let Some(mut s) = st.socks.remove(&id) else { return };
    for p in s.pend.drain(..) {
        finish(&p, STATUS_CANCELLED, 0, 0, acts);
    }
    if let Some(c) = s.conn {
        let rst = !s.rx.is_empty() || (s.linger.0 != 0 && s.linger.1 == 0);
        if let Some(o) = st.socks.get_mut(&c) {
            if rst {
                o.reset = true;
            } else {
                o.eof = true;
            }
            o.peer_gone = true;
            o.conn = None;
        }
        st.serve(c, acts);
        st.refresh(c, acts);
    }
    for q in s.queue.drain(..) {
        if let Some(srv) = st.socks.remove(&q) {
            if let Some(cl) = srv.conn.and_then(|c| st.socks.get_mut(&c)) {
                cl.reset = true;
                cl.peer_gone = true;
                cl.conn = None;
            }
            if let Some(c) = srv.conn {
                st.serve(c, acts);
                st.refresh(c, acts);
            }
            st.handles.remove(&q);
            acts.push(Act::Close(q));
        }
    }
    st.touch();
}

pub fn close_socket(h: u32) -> u32 {
    let r = with(|st, acts| {
        let id = st.id(h)?;
        st.handles.remove(&h);
        let s = st.socks.get_mut(&id).unwrap();
        s.nhandles -= 1;
        if s.nhandles == 0 {
            teardown(st, id, acts);
        }
        st.touch();
        Ok(())
    });
    match r {
        Ok(()) => {
            iocp::forget(h);
            obj::close(h);
            0
        }
        Err(e) => err(e),
    }
}

fn closesocket(c: &mut Ctx) -> u32 {
    if !started() {
        return err(WSANOTINITIALISED);
    }
    close_socket(c.a(0))
}

/// Runs `f` on the socket behind handle `h` (WSANOTINITIALISED / WSAENOTSOCK first)
fn on<R>(h: u32, f: impl FnOnce(&mut State, u32, &mut Vec<Act>) -> Result<R, u32>) -> Result<R, u32> {
    if !started() {
        return Err(WSANOTINITIALISED);
    }
    with(|st, acts| {
        let id = st.id(h)?;
        f(st, id, acts)
    })
}

/// A result as a return value: errors set the last error, and success clears it (as Windows'
/// ws2_32 does)
fn res(r: Result<u32, u32>) -> u32 {
    match r {
        Ok(v) => {
            last(0);
            v
        }
        Err(e) => err(e),
    }
}

// ---- bind, listen, connect, accept ----

/// Whether binding `n` (the newcomer's address and options) clashes with `o` already bound
/// to the same port, by Windows' rules (ws2_32's test_reuseaddr pins them down). IPv4-mapped
/// addresses count as IPv4 ones. The same address clashes unless both sockets set
/// SO_REUSEADDR (only the newcomer: WSAEACCES). A wildcard over a specific address (a
/// dual-stack :: covers IPv4 too) clashes only through SO_EXCLUSIVEADDRUSE: on the holder's
/// wildcard it is WSAEACCES, on the newcomer's WSAEADDRINUSE.
fn bind_clash(o: &Addr, o_v6only: bool, o_reuse: bool, o_excl: bool, n: &Addr, n_v6only: bool, n_reuse: bool, n_excl: bool) -> Result<(), u32> {
    let norm = |a: &Addr| match a.as_v4() {
        Some(v) => (false, v.to_vec()),
        None => (true, a.ip.clone()),
    };
    let ((o6, oip), (n6, nip)) = (norm(o), norm(n));
    let any = |ip: &[u8]| ip.iter().all(|&b| b == 0);
    if o6 == n6 && oip == nip {
        if o_reuse && n_reuse {
            return Ok(());
        }
        return Err(if n_reuse && !o_reuse { WSAEACCES } else { WSAEADDRINUSE });
    }
    let covers = |w6: bool, wip: &[u8], w_v6only: bool, x6: bool| any(wip) && (w6 == x6 || (w6 && !w_v6only && !x6));
    if covers(o6, &oip, o_v6only, n6) && o_excl {
        return Err(WSAEACCES);
    }
    if covers(n6, &nip, n_v6only, o6) && n_excl {
        return Err(WSAEADDRINUSE);
    }
    Ok(())
}

fn do_bind(st: &mut State, id: u32, a: Addr) -> Result<(), u32> {
    let s = &st.socks[&id];
    if s.local.is_some() {
        return Err(WSAEINVAL);
    }
    let a = match (s.af, a.is_v6()) {
        (AF_INET, false) | (AF_INET6, true) => a,
        _ => return Err(WSAEFAULT),
    };
    if !bindable(&a, s.v6only) {
        return Err(WSAEADDRNOTAVAIL);
    }
    let (ty, reuse, v6only, s_excl) = (s.ty, s.reuse, s.v6only, s.exclusive);
    let mut a = a;
    if a.port == 0 {
        a.port = st.ephemeral(ty);
    } else {
        for (oid, o) in &st.socks {
            if *oid == id || o.ty != ty {
                continue;
            }
            let Some(l) = &o.local else { continue };
            if l.port != a.port {
                continue;
            }
            bind_clash(l, o.v6only, o.reuse, o.exclusive, &a, v6only, reuse, s_excl)?;
        }
    }
    st.seq += 1;
    let seq = st.seq;
    let s = st.socks.get_mut(&id).unwrap();
    s.local = Some(a);
    s.bind_seq = seq;
    st.touch();
    Ok(())
}

fn bind(c: &mut Ctx) -> u32 {
    let (h, sa, len) = (c.a(0), c.a(1), c.a(2));
    res(on(h, |st, id, acts| {
        let a = read_sa(sa, len).map_err(|e| if e == WSAEAFNOSUPPORT { WSAEAFNOSUPPORT } else { WSAEFAULT })?;
        do_bind(st, id, a)?;
        st.refresh(id, acts);
        Ok(0)
    }))
}

fn listen(c: &mut Ctx) -> u32 {
    let (h, backlog) = (c.a(0), c.a(1) as i32);
    res(on(h, |st, id, _| {
        let s = st.socks.get_mut(&id).unwrap();
        if !s.stream() {
            return Err(WSAEOPNOTSUPP);
        }
        if s.connected {
            return Err(WSAEISCONN);
        }
        if s.local.is_none() {
            return Err(WSAEINVAL);
        }
        s.listening = true;
        s.backlog = if backlog <= 0 { 1 } else { (backlog as usize).min(200) };
        st.touch();
        Ok(0)
    }))
}

/// Connects stream socket `id` to `dst`: Ok when connected, or the error
fn do_connect(st: &mut State, id: u32, dst: Addr, srv_h: u32, acts: &mut Vec<Act>) -> Result<(), u32> {
    let s = &st.socks[&id];
    if s.listening {
        return Err(WSAEINVAL);
    }
    if s.connected {
        return Err(WSAEISCONN);
    }
    if dst.any() || dst.port == 0 {
        return Err(WSAEADDRNOTAVAIL);
    }
    let dst = match (s.af, dst.is_v6()) {
        (AF_INET, false) | (AF_INET6, true) => dst,
        _ => return Err(WSAEFAULT),
    };
    if !dst.loopback() {
        return Err(WSAENETUNREACH);
    }
    // the listener
    let l = st
        .socks
        .iter()
        .filter(|(_, o)| o.listening && o.ty == SOCK_STREAM && o.local.as_ref().is_some_and(|l| takes(l, o.v6only, &dst)))
        .max_by_key(|(_, o)| (o.local.as_ref().is_some_and(|l| !l.any()), std::cmp::Reverse(o.bind_seq)))
        .map(|(k, _)| *k);
    let Some(lid) = l else { return Err(WSAECONNREFUSED) };
    if st.socks[&lid].queue.len() >= st.socks[&lid].backlog {
        return Err(WSAECONNREFUSED);
    }
    // bind the client if it is not yet
    if st.socks[&id].local.is_none() {
        let mut lo = if dst.is_v6() { Addr::v6([0; 16], 0) } else { Addr::v4([0; 4], 0) };
        lo.port = 0;
        do_bind(st, id, lo)?;
    }
    let src = source_for(&st.socks[&id], &dst);
    let lsock = &st.socks[&lid];
    let mut srv = Sock::new(lsock.af, SOCK_STREAM, 6);
    srv.local = Some(seen_as(&Addr { port: lsock.local.as_ref().unwrap().port, ..dst.clone() }, lsock.af));
    srv.peer = Some(seen_as(&src, lsock.af));
    srv.connected = true;
    srv.conn = Some(id);
    srv.connect_time = Some(Instant::now());
    srv.reuse = lsock.reuse;
    srv.v6only = lsock.v6only;
    srv.rcvbuf = lsock.rcvbuf;
    srv.sndbuf = lsock.sndbuf;
    srv.opts = lsock.opts.clone();
    srv.linger = lsock.linger;
    st.socks.insert(srv_h, srv);
    st.handles.insert(srv_h, srv_h);
    st.socks.get_mut(&lid).unwrap().queue.push_back(srv_h);
    let s = st.socks.get_mut(&id).unwrap();
    s.local = Some(src);
    s.peer = Some(dst);
    s.connected = true;
    s.conn = Some(srv_h);
    s.connect_time = Some(Instant::now());
    s.so_error = 0;
    s.connect_failed = false;
    st.touch();
    st.serve(lid, acts);
    st.refresh(lid, acts);
    Ok(())
}

/// connect / WSAConnect for any socket type
fn connect_to(h: u32, sa: u32, len: u32) -> u32 {
    if !started() {
        return err(WSANOTINITIALISED);
    }
    let srv_h = new_handle();
    let r = with(|st, acts| -> Result<bool, u32> {
        let id = st.id(h)?;
        let s = &st.socks[&id];
        if !s.stream() {
            // UDP: sets (or with AF_UNSPEC clears) the default peer
            if sa != 0 && len >= 2 && r16(sa) as u32 == AF_UNSPEC {
                st.socks.get_mut(&id).unwrap().peer = None;
                st.touch();
                return Ok(false);
            }
            let dst = read_sa(sa, len)?;
            if dst.any() || dst.port == 0 {
                return Err(WSAEADDRNOTAVAIL);
            }
            if st.socks[&id].local.is_none() {
                let lo = if st.socks[&id].af == AF_INET6 { Addr::v6([0; 16], 0) } else { Addr::v4([0; 4], 0) };
                do_bind(st, id, lo)?;
            }
            let s = st.socks.get_mut(&id).unwrap();
            let src = source_for(s, &dst);
            if s.local.as_ref().is_some_and(|l| l.any()) && dst.loopback() {
                s.local = Some(src);
            }
            s.peer = Some(dst);
            st.touch();
            st.refresh(id, acts);
            return Ok(false);
        }
        let nonblock = s.nonblock;
        let dst = read_sa(sa, len)?;
        let r = do_connect(st, id, dst, srv_h, acts);
        let s = st.socks.get_mut(&id).unwrap();
        match r {
            Ok(()) => {
                s.connect_result = Some(0);
                st.refresh(id, acts);
                if nonblock { Err(WSAEWOULDBLOCK) } else { Ok(true) }
            }
            Err(e @ (WSAECONNREFUSED | WSAENETUNREACH | WSAETIMEDOUT)) if nonblock => {
                s.so_error = e;
                s.connect_failed = true;
                s.connect_result = Some(e);
                st.touch();
                st.refresh(id, acts);
                Err(WSAEWOULDBLOCK)
            }
            Err(e) => Err(e),
        }
    });
    let used = matches!(r, Ok(true)) || (matches!(r, Err(WSAEWOULDBLOCK)) && with(|st, _| st.socks.contains_key(&srv_h)));
    if !used {
        obj::close(srv_h);
    }
    match r {
        Ok(_) => 0,
        Err(e) => err(e),
    }
}

/// WSAConnectByNameA/W(s, node, service, &localLen, local, &remoteLen, remote, timeout,
/// reserved): looks the name up and connects to the first address that answers; the local
/// address is given back without its port
fn connect_by_name(c: &mut Ctx, wide: bool) -> u32 {
    let (h, node, serv, llp, la, rlp, ra, reserved) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(8));
    let fail = |e: u32| {
        last(e);
        0
    };
    let text = |p: u32| if wide { String::from_utf16_lossy(&mem::wstr(p)) } else { crate::text::a2s(&mem::cstr(p)) };
    let info = match on(h, |st, id, _| Ok(st.socks[&id].info())) {
        Ok(i) => i,
        Err(e) => return fail(e),
    };
    if reserved != 0 || node == 0 || serv == 0 || info.1 != SOCK_STREAM {
        return fail(WSAEINVAL);
    }
    let room = if info.0 == AF_INET6 { 28 } else { 16 };
    for (p, lp) in [(la, llp), (ra, rlp)] {
        if p != 0 && lp != 0 && r32(lp) < room {
            return fail(WSAEFAULT);
        }
    }
    let hints = names::Hints { flags: 0, family: info.0, socktype: SOCK_STREAM, protocol: 6 };
    let addrs = match names::addrinfo(Some(&text(node)), Some(&text(serv)), &hints) {
        Ok((_, v)) => v,
        Err(e) => return fail(e),
    };
    let buf = mem::heap_alloc(28, true);
    let mut r = 10065; // WSAEHOSTUNREACH
    for (a, ..) in &addrs {
        mem::write(buf, &a.bytes());
        if connect_to(h, buf, a.size()) == 0 {
            r = 0;
            break;
        }
        r = get_last();
    }
    mem::heap_free(buf);
    if r != 0 {
        return fail(r);
    }
    let (local, peer) = with(|st, _| st.id(h).ok().map(|id| (st.socks[&id].local.clone(), st.socks[&id].peer.clone()))).unwrap_or_default();
    for (p, lp, a) in [(la, llp, local.map(|mut l| {
        l.port = 0;
        l
    })), (ra, rlp, peer)] {
        let Some(a) = a else { continue };
        if lp != 0 {
            if p != 0 {
                mem::write(p, &a.bytes());
            }
            w32(lp, a.size());
        }
    }
    last(0);
    1
}

fn connect(c: &mut Ctx) -> u32 {
    connect_to(c.a(0), c.a(1), c.a(2))
}

/// accept(s, &addr, &len) / WSAAccept(s, &addr, &len, condition, data)
fn accept(c: &mut Ctx, wsa: bool) -> u32 {
    let (h, sa, lenp) = (c.a(0), c.a(1), c.a(2));
    let (cond, data) = if wsa { (c.a(3), c.a(4)) } else { (0, 0) };
    let pre = on(h, |st, id, _| {
        let s = &st.socks[&id];
        if !s.listening {
            return Err(WSAEINVAL);
        }
        if sa != 0 && (lenp == 0 || (r32(lenp) as i32) < if s.af == AF_INET6 { 28 } else { 16 }) {
            return Err(WSAEFAULT);
        }
        Ok(s.nonblock)
    });
    let nonblock = match pre {
        Ok(n) => n,
        Err(e) => {
            last(e);
            return if mem::x64() { c.ret64(u64::MAX) } else { u32::MAX };
        }
    };
    let take = |st: &mut State, acts: &mut Vec<Act>| -> Option<Result<u32, u32>> {
        // (closed while waiting: WSAEINTR, as in Windows)
            let Ok(id) = st.id(h) else { return Some(Err(10004)) };
        if !st.socks.get(&id)?.listening {
            return Some(Err(WSAEINVAL));
        }
        let q = st.queued(id, true)?;
        let l = st.socks.get_mut(&id)?;
        let (nb, notify, mask) = (l.nonblock, l.notify, l.mask);
        l.reported &= !FD_ACCEPT;
        let s = st.socks.get_mut(&q)?;
        s.nonblock = nb;
        s.notify = notify;
        s.mask = mask;
        if let (true, Some(peer)) = (sa != 0, s.peer.as_ref()) {
            let _ = write_sa(sa, lenp, peer);
        }
        st.touch();
        st.refresh(id, acts);
        st.refresh(q, acts);
        Some(Ok(q))
    };
    // WSAAccept's condition function decides first, on the connection at the head of the queue
    if wsa && cond != 0 {
        let peek = |st: &mut State, _: &mut Vec<Act>| -> Option<Result<(u32, Addr, Addr), u32>> {
            // (closed while waiting: WSAEINTR, as in Windows)
            let Ok(id) = st.id(h) else { return Some(Err(10004)) };
            if !st.socks.get(&id)?.listening {
                return Some(Err(WSAEINVAL));
            }
            let q = st.queued(id, false)?;
            let s = &st.socks[&q];
            let (Some(peer), Some(local)) = (s.peer.clone(), s.local.clone()) else { return Some(Err(WSAECONNRESET)) };
            Some(Ok((q, peer, local)))
        };
        let got = if nonblock { with(peek).unwrap_or(Err(WSAEWOULDBLOCK)) } else { block(Some(c), u32::MAX, peek).unwrap() };
        let (q, peer, local) = match got {
            Ok(x) => x,
            Err(e) => return ret_socket(c, Err(e)),
        };
        // lpCallerId / lpCalleeId: WSABUFs of the two addresses; then the GROUP
        let k = ps();
        let m = mem::heap_alloc(4 * k + 2 * 28 + 8, true);
        let (bufs, sas, group) = (m, m + 4 * k, m + 4 * k + 56);
        for (i, a) in [&peer, &local].into_iter().enumerate() {
            let sa = sas + 28 * i as u32;
            let len = 28u32;
            let lp = mem::heap_alloc(4, true);
            mem::put32(lp, len);
            let _ = write_sa(sa, lp, a);
            mem::put32(bufs + 2 * k * i as u32, r32(lp));
            mem::wp(bufs + 2 * k * i as u32 + k, sa);
            mem::heap_free(lp);
        }
        let v = c.call(cond, &[bufs, 0, 0, 0, bufs + 2 * k, 0, group, data]);
        mem::heap_free(m);
        match v {
            0 => {}
            // CF_DEFER: the connection stays queued
            2 => return ret_socket(c, Err(WSATRY_AGAIN)),
            // CF_REJECT (or anything else): the connection is refused
            _ => {
                with(|st, _| {
                    if let Some(l) = st.id(h).ok().and_then(|id| st.socks.get_mut(&id)) {
                        l.queue.retain(|&x| x != q);
                    }
                });
                close_socket(q);
                return ret_socket(c, Err(WSAECONNREFUSED));
            }
        }
    }
    let r = if nonblock {
        with(|st, acts| take(st, acts)).unwrap_or(Err(WSAEWOULDBLOCK))
    } else {
        block(Some(c), u32::MAX, take).unwrap()
    };
    ret_socket(c, r)
}

// ---- data ----

/// Sends `data` on socket `id` (to `to` for a datagram socket): the bytes sent
fn send_data(st: &mut State, id: u32, data: &[u8], to: Option<Addr>, flags: u32, acts: &mut Vec<Act>) -> Result<u32, u32> {
    let s = st.socks.get_mut(&id).unwrap();
    if s.stream() {
        if !s.connected || s.connect_failed {
            return Err(WSAENOTCONN);
        }
        if s.shut_wr {
            return Err(WSAESHUTDOWN);
        }
        if s.reset {
            return Err(WSAECONNRESET);
        }
        let Some(p) = s.conn else {
            // the other end closed: this send goes, and the reset that answers it ends the connection
            if s.peer_gone {
                s.reset = true;
                st.touch();
                return Ok(data.len() as u32);
            }
            return Err(WSAENOTCONN);
        };
        // a full pipe: a blocking send waits (SEND_WAIT), a non-blocking one takes what fits
        // or fails with WSAEWOULDBLOCK (FD_WRITE is reported again once there is room)
        let nonblock = s.nonblock;
        let (cap, used) = {
            let o = &st.socks[&p];
            (pipe_cap(o.rcvbuf, st.socks[&id].sndbuf), o.rx.len())
        };
        let room = cap.saturating_sub(used);
        let n = if room == 0 {
            if nonblock {
                st.socks.get_mut(&id).unwrap().reported &= !FD_WRITE;
                return Err(WSAEWOULDBLOCK);
            }
            return Err(SEND_WAIT);
        } else if nonblock {
            data.len().min(room)
        } else {
            data.len()
        };
        let o = st.socks.get_mut(&p).unwrap();
        if o.shut_rd {
            // data for a closed receiving side resets the connection
            o.reset = true;
        } else if flags & MSG_OOB != 0 && n > 0 {
            // the last byte is urgent: read apart (recv MSG_OOB), or inline with SO_OOBINLINE
            if o.opts.get(&(SOL_SOCKET, 0x100)).is_some_and(|&v| v != 0) {
                o.rx.extend(&data[..n]);
                o.oob_inline = true;
            } else {
                o.rx.extend(&data[..n - 1]);
                o.oob = Some(data[n - 1]);
            }
        } else {
            o.rx.extend(&data[..n]);
        }
        st.touch();
        st.serve(p, acts);
        st.refresh(p, acts);
        return Ok(n as u32);
    }
    if s.shut_wr {
        return Err(WSAESHUTDOWN);
    }
    let dst = match to.or_else(|| s.peer.clone()) {
        Some(d) => d,
        None => return Err(WSAENOTCONN),
    };
    let dst = match (s.af, dst.is_v6()) {
        (AF_INET, false) | (AF_INET6, true) => dst,
        _ => return Err(WSAEFAULT),
    };
    if data.len() > if s.af == AF_INET6 { 65527 } else { 65507 } {
        return Err(WSAEMSGSIZE);
    }
    if dst.any() {
        return Err(WSAEADDRNOTAVAIL);
    }
    if dst.port == 0 {
        // (nothing listens on port 0: the datagram goes nowhere, and the send succeeds)
        if s.local.is_none() {
            let lo = if s.af == AF_INET6 { Addr::v6([0; 16], 0) } else { Addr::v4([0; 4], 0) };
            do_bind(st, id, lo)?;
        }
        return Ok(data.len() as u32);
    }
    let v4 = dst.as_v4();
    let bcast = v4.is_some_and(|a| a == [255; 4] || a[3] == 255 && a[0] == 127);
    let mcast = v4.is_some_and(|a| (224..240).contains(&a[0])) || (dst.as_v4().is_none() && dst.ip[0] == 0xff);
    if bcast && !s.broadcast {
        return Err(WSAEACCES);
    }
    if !dst.loopback() && !bcast && !mcast {
        return Err(WSAENETUNREACH);
    }
    if s.local.is_none() {
        let lo = if s.af == AF_INET6 { Addr::v6([0; 16], 0) } else { Addr::v4([0; 4], 0) };
        do_bind(st, id, lo)?;
    }
    let src = source_for(&st.socks[&id], &dst);
    let targets: Vec<u32> = {
        let mut v: Vec<(u64, u32)> = st
            .socks
            .iter()
            .filter(|(_, o)| {
                o.ty == SOCK_DGRAM
                    && !o.shut_rd
                    && o.local.as_ref().is_some_and(|l| if bcast || mcast { l.port == dst.port && (l.any() || bcast) } else { takes(l, o.v6only, &dst) })
                    && o.peer.as_ref().is_none_or(|p| seen_as(p, AF_INET6) == seen_as(&src, AF_INET6))
            })
            .map(|(k, o)| (o.bind_seq, *k))
            .collect();
        v.sort();
        if bcast || mcast { v.into_iter().map(|x| x.1).collect() } else { v.last().map(|x| x.1).into_iter().collect() }
    };
    if targets.is_empty() && !bcast && !mcast {
        // the port unreachable reply: the next receive fails with WSAECONNRESET
        let s = st.socks.get_mut(&id).unwrap();
        if s.udp_connreset {
            s.udp_reset = true;
        }
    }
    for t in targets {
        let Some(o) = st.socks.get_mut(&t) else { continue };
        // (a full receive buffer: the datagram is lost, as on Windows)
        let queued: usize = o.dgrams.iter().map(|d| d.1.len()).sum();
        if !dgram_fits(queued, data.len(), o.rcvbuf) {
            continue;
        }
        o.dgrams.push_back((seen_as(&src, o.af), data.to_vec(), seen_as(&dst, o.af)));
        st.serve(t, acts);
        st.refresh(t, acts);
    }
    st.touch();
    Ok(data.len() as u32)
}

/// The bytes of the buffers, MAX_SEND at most; WSAEFAULT for a buffer outside the guest
fn gather(bufs: &[(u32, u32)]) -> Result<Vec<u8>, u32> {
    let mut left = MAX_SEND;
    let mut v = Vec::new();
    for &(p, n) in bufs {
        let n = n.min(left);
        if n != 0 && !mem::in_guest(p, n) {
            return Err(WSAEFAULT);
        }
        v.extend_from_slice(mem::slice(p, n));
        left -= n;
    }
    Ok(v)
}

/// send_data's "the pipe is full, wait for room" (not a Winsock error)
const SEND_WAIT: u32 = 0xffff_fffe;

fn send_common(mut c: Option<&mut Ctx>, h: u32, bufs: &[(u32, u32)], to: u32, tolen: u32, flags: u32) -> Result<u32, u32> {
    let data = gather(bufs)?;
    loop {
        let r = on(h, |st, id, acts| {
            let to = if to != 0 && !st.socks[&id].stream() { Some(read_sa(to, tolen)?) } else { None };
            send_data(st, id, &data, to, flags, acts)
        });
        if r != Err(SEND_WAIT) {
            return r;
        }
        block(c.as_deref_mut(), u32::MAX, |st, _| match st.id(h) {
            Ok(id) if st.full(id) && st.socks[&id].conn.is_some() && !st.socks[&id].reset => None,
            _ => Some(()),
        });
    }
}

fn send(c: &mut Ctx) -> u32 {
    let (h, b, n) = (c.a(0), c.a(1), c.a(2));
    let fl = c.a(3);
    res(send_common(Some(c), h, &[(b, n)], 0, 0, fl))
}
fn sendto(c: &mut Ctx) -> u32 {
    let (h, b, n, to, tolen) = (c.a(0), c.a(1), c.a(2), c.a(4), c.a(5));
    let fl = c.a(3);
    res(send_common(Some(c), h, &[(b, n)], to, tolen, fl))
}

/// A receive: (bytes, sender, truncated) or the error; blocks unless non-blocking
fn recv_common(c: Option<&mut Ctx>, h: u32, bufs: &[(u32, u32)], flags: u32) -> Result<(u32, Option<Addr>, bool), u32> {
    let (nonblock, timeo) = on(h, |st, id, _| {
        let s = &st.socks[&id];
        Ok((s.nonblock, s.rcvtimeo))
    })?;
    let f = |st: &mut State, acts: &mut Vec<Act>| -> Option<Result<(u32, Option<Addr>, bool), u32>> {
        let Ok(id) = st.id(h) else { return Some(Err(WSAEINTR)) };
        let s = st.socks.get_mut(&id).unwrap();
        match take_data(s, bufs, flags) {
            Ok(None) => None,
            Ok(Some(r)) => {
                s.reported &= !FD_READ;
                if !s.readable() {
                    s.netev &= !FD_READ; // (nothing left: the recorded FD_READ is gone too)
                }
                st.touch();
                st.refresh(id, acts);
                st.drained(id, acts);
                Some(Ok(r))
            }
            Err(e) => Some(Err(e)),
        }
    };
    if nonblock {
        return with(|st, acts| f(st, acts)).unwrap_or(Err(WSAEWOULDBLOCK));
    }
    block(c, if timeo == 0 { u32::MAX } else { timeo }, f).unwrap_or(Err(WSAETIMEDOUT))
}

fn recv(c: &mut Ctx) -> u32 {
    let (h, b, n, fl) = (c.a(0), c.a(1), c.a(2), c.a(3));
    match recv_common(Some(c), h, &[(b, n)], fl) {
        Ok((n, _, false)) => res(Ok(n)),
        Ok((_, _, true)) => err(WSAEMSGSIZE),
        Err(e) => err(e),
    }
}
fn recvfrom(c: &mut Ctx) -> u32 {
    let (h, b, n, fl, from, lenp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if from != 0 && (lenp == 0 || (r32(lenp) as i32) < 16) {
        if is_socket(h) {
            return err(WSAEFAULT);
        }
    }
    match recv_common(Some(c), h, &[(b, n)], fl) {
        Ok((n, src, trunc)) => {
            if let (Some(a), true) = (src, from != 0) {
                if write_sa(from, lenp, &a).is_err() {
                    return err(WSAEFAULT);
                }
            }
            if trunc { err(WSAEMSGSIZE) } else { res(Ok(n)) }
        }
        Err(e) => err(e),
    }
}

/// WSASend / WSASendTo(s, bufs, n, &sent, flags, [to, tolen,] ov, routine): sending never
/// waits, so an overlapped send completes at once
fn wsa_send(c: &mut Ctx, to: bool) -> u32 {
    let (h, bp, n, sentp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (top, tolen, ov, routine) = if to { (c.a(5), c.a(6), c.a(7), c.a(8)) } else { (0, 0, c.a(5), c.a(6)) };
    ov_start(ov);
    let bufs = wsabufs(bp, n);
    let fl = c.a(4);
    // (an array that is not there: WSAEFAULT, not a send of nothing)
    let r = if !bufs_ok(bp, n) { Err(WSAEFAULT) } else { send_common(Some(c), h, &bufs, top, tolen, fl) };
    complete_now(h, ov, routine, r.map(|n| (n, 0)), sentp)
}

/// Reports a call that ended at once: Ok((bytes, flags)) or the error; with an OVERLAPPED, it
/// completes (event, routine, port) too
fn complete_now(h: u32, ov: u32, routine: u32, r: Result<(u32, u32), u32>, bytesp: u32) -> u32 {
    match r {
        Ok((n, fl)) => {
            mem::put32(bytesp, n);
            if ov != 0 {
                let p = Pend { kind: PendKind::Forever, ov, routine, tid: crate::cur().tid, handle: h };
                let mut acts = Vec::new();
                with(|st, _| st.set_ov_flags(ov, fl));
                acts.push(Act::Complete(h, ov, 0, n, true));
                if routine != 0 {
                    acts.push(Act::Apc(p.tid, routine, [0, n, ov, fl]));
                }
                run(acts);
            }
            0
        }
        Err(e) => {
            if ov != 0 && e != WSAENOTSOCK && e != WSANOTINITIALISED && e != WSAEFAULT && e != WSAEWOULDBLOCK {
                let k = ps();
                wp(ov, wsa_to_nt(e));
                wp(ov + k, 0);
            }
            err(e)
        }
    }
}

/// The flags a receive reports: MSG_PARTIAL for a datagram cut short; for WSARecvMsg (`msg`,
/// its WSAMSG) the control data is written too, and MSG_CTRUNC if it did not fit
fn recv_flags(s: &Sock, msg: u32, trunc: bool) -> u32 {
    let mut fl = if trunc { if msg != 0 { MSG_TRUNC } else { MSG_PARTIAL } } else { 0 };
    if msg != 0 && write_control(msg, &control_msgs(s)) {
        fl |= MSG_CTRUNC;
    }
    fl
}

const MSG_CTRUNC: u32 = 0x0200;

/// The control messages the socket's IP_PKTINFO / IP_RECVTTL / IP_RECVTOS (IPv6:
/// IPV6_PKTINFO / IPV6_HOPLIMIT / IPV6_RECVTCLASS) ask for, for the last datagram:
/// (level, type, data)
fn control_msgs(s: &Sock) -> Vec<(u32, u32, Vec<u8>)> {
    let mut v = Vec::new();
    if s.stream() {
        return v;
    }
    let on = |level: u32, opt: u32| s.opts.get(&(level, opt)).copied().unwrap_or(0) != 0;
    let dst = s.last_dst.clone().unwrap_or_else(|| s.local.clone().unwrap_or(Addr::v4([0; 4], 0)));
    let ifindex = 1u32.to_le_bytes(); // (the loopback interface)
    if s.af == AF_INET6 {
        if on(41, 19) {
            let mut d = seen_as(&dst, AF_INET6).ip.clone();
            d.extend_from_slice(&ifindex);
            v.push((41, 19, d));
        }
        if on(41, 21) {
            v.push((41, 21, 128u32.to_le_bytes().to_vec()));
        }
        if on(41, 40) {
            v.push((41, 39, 0u32.to_le_bytes().to_vec()));
        }
    } else {
        if on(0, 19) {
            let mut d = dst.ip[..4].to_vec();
            d.extend_from_slice(&ifindex);
            v.push((0, 19, d));
        }
        if on(0, 21) {
            v.push((0, 4, 128u32.to_le_bytes().to_vec()));
        }
        if on(0, 40) {
            v.push((0, 3, 0u32.to_le_bytes().to_vec()));
        }
    }
    v
}

/// Writes control messages into WSAMSG `m`'s Control buffer as WSACMSGHDRs { SIZE_T len;
/// INT level, type; data } (each aligned to a pointer); Control.len becomes the bytes used.
/// Returns whether some did not fit.
fn write_control(m: u32, msgs: &[(u32, u32, Vec<u8>)]) -> bool {
    let k = ps();
    let (lenp, bufp) = if k == 8 { (m + 32, m + 40) } else { (m + 16, m + 20) };
    let (room, buf) = (r32(lenp), rp(bufp));
    let hdr = if k == 8 { 16 } else { 12 };
    let align = |n: u32| (n + k - 1) & !(k - 1);
    let mut at = 0;
    let mut cut = false;
    for (level, ty, data) in msgs {
        let len = hdr + data.len() as u32;
        if buf == 0 || at + len > room {
            cut = true;
            break;
        }
        wp(buf + at, len);
        w32(buf + at + k, *level);
        w32(buf + at + k + 4, *ty);
        mem::write(buf + at + hdr, data);
        at += align(len);
    }
    w32(lenp, at.min(room));
    cut
}

/// WSARecv / WSARecvFrom(s, bufs, n, &got, &flags, [from, &fromlen,] ov, routine)
fn wsa_recv(c: &mut Ctx, from: bool) -> u32 {
    let (h, bp, n, gotp, flp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let (fromp, lenp, ov, routine) = if from { (c.a(5), c.a(6), c.a(7), c.a(8)) } else { (0, 0, c.a(5), c.a(6)) };
    if !bufs_ok(bp, n) {
        return err(WSAEFAULT);
    }
    recv_ex(c, h, &wsabufs(bp, n), gotp, flp, fromp, lenp, ov, routine, 0)
}

/// An overlapped operation starting: its event is reset (as Windows does)
fn ov_start(ov: u32) {
    if ov != 0 {
        let ev = rp(ov + 2 * ps() + 8) & !1;
        if ev != 0 {
            crate::k32::event_set(ev, false);
        }
    }
}

/// A receive into `bufs` (`flp`: the in/out flags, `msg`: WSARecvMsg's WSAMSG or 0)
#[allow(clippy::too_many_arguments)]
fn recv_ex(c: &mut Ctx, h: u32, bufs: &[(u32, u32)], gotp: u32, flp: u32, fromp: u32, lenp: u32, ov: u32, routine: u32, msg: u32) -> u32 {
    let flags = if flp != 0 { r32(flp) } else { 0 };
    let tid = crate::cur().tid;
    ov_start(ov);
    // done now if there is data; else a blocking call waits, an overlapped one is left pending
    let r = if ov == 0 {
        recv_common(Some(c), h, bufs, flags).map(|(n, src, trunc)| Some((n, src, with(|st, _| st.id(h).map(|id| recv_flags(&st.socks[&id], msg, trunc)).unwrap_or(0)))))
    } else {
        on(h, |st, id, acts| {
            let s = st.socks.get_mut(&id).unwrap();
            let busy = s.pend.iter().any(|p| matches!(p.kind, PendKind::Recv { .. }));
            let now = if busy { Ok(None) } else { take_data(s, bufs, flags) };
            match now {
                Ok(None) => {
                    let k = ps();
                    wp(ov, STATUS_PENDING);
                    wp(ov + k, 0);
                    s.pend.push(Pend { kind: PendKind::Recv { bufs: bufs.to_vec(), flags: flp, from: fromp, fromlen: lenp, peek: flags & MSG_PEEK != 0, msg }, ov, routine, tid, handle: h });
                    st.touch();
                    Ok(None)
                }
                Ok(Some((n, src, trunc))) => {
                    let fl = recv_flags(s, msg, trunc);
                    s.reported &= !FD_READ;
                if !s.readable() {
                    s.netev &= !FD_READ; // (nothing left: the recorded FD_READ is gone too)
                }
                    st.touch();
                    st.refresh(id, acts);
                    st.drained(id, acts);
                    Ok(Some((n, src, fl)))
                }
                Err(e) => Err(e),
            }
        })
    };
    match r {
        Ok(None) => err(WSA_IO_PENDING),
        Ok(Some((n, src, fl))) => {
            if let (Some(a), true) = (src, fromp != 0) {
                let _ = write_sa(fromp, lenp, &a);
            }
            if flp != 0 {
                w32(flp, fl);
            }
            mem::put32(gotp, n);
            if fl == 0 {
                return if ov == 0 { res(Ok(0)) } else { complete_now(h, ov, routine, Ok((n, 0)), gotp) };
            }
            if ov == 0 {
                return err(WSAEMSGSIZE);
            }
            // it did not all fit: an overlapped receive completes with STATUS_BUFFER_OVERFLOW
            // (a warning, so the call itself reports it pending)
            let mut acts = Vec::new();
            let p = Pend { kind: PendKind::Forever, ov, routine, tid, handle: h };
            let fl = if msg != 0 { 0 } else { fl };
            if msg == 0 {
                with(|st, _| st.set_ov_flags(ov, fl));
            }
            finish(&p, STATUS_BUFFER_OVERFLOW, n, fl, &mut acts);
            run(acts);
            err(WSA_IO_PENDING)
        }
        Err(e) => {
            if ov == 0 {
                err(e)
            } else {
                complete_now(h, ov, routine, Err(e), gotp)
            }
        }
    }
}

/// WSAGetOverlappedResult(s, ov, &bytes, wait, &flags)
fn WSAGetOverlappedResult(c: &mut Ctx) -> u32 {
    let (h, ov, bp, wait, flp) = (c.a(0), c.a(1), c.a(2), c.a(3) != 0, c.a(4));
    if !is_socket(h) {
        last(WSAENOTSOCK);
        return 0;
    }
    if ov == 0 || bp == 0 || flp == 0 {
        last(WSAEFAULT);
        return 0;
    }
    if rp(ov) == STATUS_PENDING {
        if !wait {
            last(WSA_IO_INCOMPLETE);
            return 0;
        }
        iocp::wait_overlapped(c, ov, u32::MAX, false);
    }
    let k = ps();
    w32(bp, rp(ov + k));
    w32(flp, with(|st, _| st.ov_flags.get(&ov).copied().unwrap_or(0)));
    // NT_SUCCESS: done (no error); else the status's error
    let s = rp(ov);
    let e = nt_to_wsa(s);
    last(e);
    (s >> 31 == 0) as u32
}

/// Cancels IO waiting on socket handle `h` (`ov` 0: all of it; `mine`: only this thread's)
pub fn cancel(h: u32, ov: u32, mine: bool) -> bool {
    let tid = crate::cur().tid;
    with(|st, acts| {
        let Ok(id) = st.id(h) else { return false };
        let s = st.socks.get_mut(&id).unwrap();
        let mut found = false;
        let mut i = 0;
        while i < s.pend.len() {
            let p = &s.pend[i];
            if (ov == 0 || p.ov == ov) && (!mine || p.tid == tid) && p.handle == h {
                let p = s.pend.remove(i);
                finish(&p, STATUS_CANCELLED, 0, 0, acts);
                found = true;
            } else {
                i += 1;
            }
        }
        if found {
            st.touch();
        }
        found
    })
}

fn shutdown(c: &mut Ctx) -> u32 {
    let (h, how) = (c.a(0), c.a(1));
    res(on(h, |st, id, acts| {
        if how > 2 {
            return Err(WSAEINVAL);
        }
        let s = st.socks.get_mut(&id).unwrap();
        if s.stream() && (!s.connected || s.connect_failed) {
            return Err(WSAENOTCONN);
        }
        if how != 1 {
            s.shut_rd = true;
        }
        if how != 0 && !s.shut_wr {
            s.shut_wr = true;
            if let Some(p) = s.conn {
                if let Some(o) = st.socks.get_mut(&p) {
                    o.eof = true;
                }
                st.serve(p, acts);
                st.refresh(p, acts);
            }
        }
        st.touch();
        st.serve(id, acts);
        Ok(0)
    }))
}

fn getsockname(c: &mut Ctx) -> u32 {
    let (h, sa, lenp) = (c.a(0), c.a(1), c.a(2));
    res(on(h, |st, id, _| {
        let s = &st.socks[&id];
        let Some(l) = &s.local else { return Err(WSAEINVAL) };
        write_sa(sa, lenp, l)?;
        Ok(0)
    }))
}
fn getpeername(c: &mut Ctx) -> u32 {
    let (h, sa, lenp) = (c.a(0), c.a(1), c.a(2));
    res(on(h, |st, id, _| {
        let s = &st.socks[&id];
        let p = match &s.peer {
            Some(p) if !s.stream() || (s.connected && !s.connect_failed) => p,
            _ => return Err(WSAENOTCONN),
        };
        write_sa(sa, lenp, p)?;
        Ok(0)
    }))
}

// ---- select, poll, events ----

/// fd_set { u_int fd_count; SOCKET fd_array[64] }
fn fd_list(p: u32) -> Vec<u32> {
    if p == 0 {
        return Vec::new();
    }
    let k = ps();
    let n = r32(p).min(4096); // (FD_SETSIZE is the caller's choice)
    (0..n).map(|i| rp(p + k + k * i)).collect()
}
fn fd_put(p: u32, v: &[u32]) {
    if p == 0 {
        return;
    }
    let k = ps();
    w32(p, v.len() as u32);
    for (i, &h) in v.iter().enumerate() {
        wp(p + k + k * i as u32, h);
        if k == 8 {
            w32(p + k + k * i as u32 + 4, 0);
        }
    }
}

fn select(c: &mut Ctx) -> u32 {
    let (rd, wr, ex, tv) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if !started() {
        return err(WSANOTINITIALISED);
    }
    let (r, w, e) = (fd_list(rd), fd_list(wr), fd_list(ex));
    if r.is_empty() && w.is_empty() && e.is_empty() {
        if tv == 0 {
            return err(WSAEINVAL);
        }
        let ms = r32(tv).saturating_mul(1000).saturating_add(r32(tv + 4) / 1000);
        // (a frame sleep of ported code: in slices, so a save state's pause parks the thread)
        let end = std::time::Instant::now() + Duration::from_millis(ms.min(60_000) as u64);
        while let Some(d) = end.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero()) {
            kura::snap::sleep(d.min(Duration::from_millis(50)));
            crate::snap::point();
        }
        return err(WSAEINVAL);
    }
    let bad = with(|st, _| r.iter().chain(&w).chain(&e).any(|h| st.id(*h).is_err()));
    if bad {
        return err(WSAENOTSOCK);
    }
    let ms = if tv == 0 { u32::MAX } else { r32(tv).saturating_mul(1000).saturating_add(r32(tv + 4).div_ceil(1000)) };
    // one poll entry per set member (read, then write, then except); the ready ones go back
    // into their sets, each socket once per set (the sets may be the same memory: all are
    // cleared first), and the count is of what was added
    let ents: Vec<(u32, u8, u32)> = r.iter().map(|&h| (h, 0, rd)).chain(w.iter().map(|&h| (h, 1, wr))).chain(e.iter().map(|&h| (h, 2, ex))).collect();
    let check = |st: &mut State, _: &mut Vec<Act>| -> Option<Vec<bool>> {
        let ready: Vec<bool> = ents
            .iter()
            .map(|&(h, kind, _)| {
                let s = st.id(h).ok().and_then(|id| st.socks.get(&id));
                // (a socket closed meanwhile is ready: readable in the read set, an error in
                // the others)
                match kind {
                    0 => s.is_none_or(|s| s.readable()),
                    1 => s.is_none_or(|s| s.writable()) && !st.id(h).is_ok_and(|id| st.full(id)),
                    _ => s.is_none_or(|s| s.connect_failed || s.oob.is_some()),
                }
            })
            .collect();
        ready.iter().any(|&b| b).then_some(ready)
    };
    let got = if ms == 0 { with(check) } else { block(Some(c), ms, check) };
    let ready = got.unwrap_or_else(|| vec![false; ents.len()]);
    // a closed socket goes into the read set if it was asked for there, else it is an error
    let closed: Vec<bool> = with(|st, _| ents.iter().map(|&(h, ..)| st.id(h).is_err()).collect());
    let gone = ents.iter().zip(&ready).zip(&closed).any(|((&(h, kind, _), &rd_), &c)| rd_ && c && kind != 0 && !r.contains(&h));
    if gone {
        for p in [rd, wr, ex] {
            if p != 0 {
                w32(p, 0);
            }
        }
        return err(WSAENOTSOCK);
    }
    let mut out: Vec<(u32, Vec<u32>)> = Vec::new();
    for p in [rd, wr, ex] {
        if p != 0 && !out.iter().any(|o| o.0 == p) {
            out.push((p, Vec::new()));
        }
    }
    let mut n = 0;
    for ((&(h, kind, p), _), &c) in ents.iter().zip(&ready).zip(&closed).filter(|x| *x.0 .1) {
        let p = if c && kind != 0 { rd } else { p };
        let set = &mut out.iter_mut().find(|o| o.0 == p).unwrap().1;
        if !set.contains(&h) {
            set.push(h);
            n += 1;
        }
    }
    for (p, v) in &out {
        fd_put(*p, v);
    }
    res(Ok(n))
}

/// __WSAFDIsSet(s, set)
fn WSAFDIsSet(c: &mut Ctx) -> u32 {
    fd_list(c.a(1)).contains(&c.a(0)) as u32
}

/// WSAPoll(fds, n, timeout): WSAPOLLFD { SOCKET fd; SHORT events; SHORT revents }
fn WSAPoll(c: &mut Ctx) -> u32 {
    let (fds, n, ms) = (c.a(0), c.a(1), c.a(2) as i32);
    if !started() {
        return err(WSANOTINITIALISED);
    }
    if fds == 0 {
        return err(WSAEFAULT);
    }
    if n == 0 {
        return err(WSAEINVAL);
    }
    let k = ps();
    let size = 2 * k;
    if n > MAX_BUFS || !mem::in_guest(fds, n * size) {
        return err(WSAEFAULT);
    }
    let ents: Vec<(u32, u16)> = (0..n).map(|i| (rp(fds + size * i), r16(fds + size * i + k))).collect();
    if ents.iter().any(|e| e.1 & !0x0337 != 0) {
        return err(WSAEINVAL);
    }
    let check = |st: &mut State, _: &mut Vec<Act>| -> Option<Vec<u16>> {
        let v: Vec<u16> = ents
            .iter()
            .map(|&(h, ev)| {
                let full = st.id(h).is_ok_and(|id| st.full(id));
                let Some(s) = st.id(h).ok().and_then(|id| st.socks.get(&id)) else { return 4 }; // POLLNVAL
                let mut r = 0;
                if s.readable() && !(s.stream() && s.connected && (s.eof || s.reset) && s.rx.is_empty()) {
                    r |= ev & 0x0300;
                }
                if s.writable() && !full && !(s.stream() && s.connected && s.reset) {
                    r |= ev & 0x0010;
                }
                if s.connect_failed {
                    r |= 1; // POLLERR
                }
                if s.stream() && s.connected && (s.eof || s.reset) {
                    r |= 2 | (ev & 0x0100); // POLLHUP (and what is left to read)
                    if s.reset {
                        r |= 1;
                    }
                }
                if s.stream() && !s.connected && !s.listening && !s.connect_failed {
                    r |= 2; // never connected: POLLHUP
                    r |= ev & 0x0010;
                }
                r
            })
            .collect();
        v.iter().any(|&r| r != 0).then_some(v)
    };
    let wait = if ms < 0 { u32::MAX } else { ms as u32 };
    let got = if wait == 0 { with(check) } else { block(Some(c), wait, check) };
    let v = got.unwrap_or_else(|| vec![0; ents.len()]);
    for (i, r) in v.iter().enumerate() {
        w16(fds + size * i as u32 + k + 2, *r);
    }
    v.iter().filter(|&&r| r != 0).count() as u32
}

fn set_notify(h: u32, n: Notify, mask: u32) -> u32 {
    res(on(h, |st, id, acts| {
        let s = st.socks.get_mut(&id).unwrap();
        s.notify = if mask == 0 { Notify::None } else { n };
        s.mask = mask & FD_ALL;
        s.netev = 0;
        s.neterr = [0; 10];
        // (FD_WRITE, once reported, comes again only after a send that would have blocked)
        s.reported &= FD_WRITE;
        s.nonblock = true;
        st.touch();
        st.refresh(id, acts);
        Ok(0)
    }))
}

/// WSAEventSelect(s, event, mask)
fn WSAEventSelect(c: &mut Ctx) -> u32 {
    let (h, ev, mask) = (c.a(0), c.a(1), c.a(2));
    set_notify(h, if ev == 0 { Notify::None } else { Notify::Event(ev) }, if ev == 0 { 0 } else { mask })
}
/// WSAAsyncSelect(s, hwnd, msg, mask)
fn WSAAsyncSelect(c: &mut Ctx) -> u32 {
    let (h, w, m, mask) = (c.a(0), c.a(1), c.a(2), c.a(3));
    set_notify(h, Notify::Msg(w, m), mask)
}

/// WSAEnumNetworkEvents(s, event, &WSANETWORKEVENTS { lNetworkEvents, iErrorCode[10] })
fn WSAEnumNetworkEvents(c: &mut Ctx) -> u32 {
    let (h, ev, out) = (c.a(0), c.a(1), c.a(2));
    res(on(h, |st, id, acts| {
        if out == 0 {
            return Err(WSAEFAULT);
        }
        let s = st.socks.get_mut(&id).unwrap();
        w32(out, s.netev);
        // (the error codes of events not reported are left alone)
        for i in 0..10 {
            if s.netev & (1 << i) != 0 {
                w32(out + 4 + 4 * i, s.neterr[i as usize]);
            }
        }
        if s.netev & FD_CONNECT != 0 {
            s.connect_result = None;
        }
        s.netev = 0;
        s.neterr = [0; 10];
        if ev != 0 {
            acts.push(Act::ResetEvent(ev));
        }
        Ok(0)
    }))
}

fn WSAWaitForMultipleEvents(c: &mut Ctx) -> u32 {
    let (n, hs, all, ms, alertable) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3), c.a(4) != 0);
    if n == 0 || n > 64 || hs == 0 {
        last(87);
        return u32::MAX;
    }
    let k = ps();
    let v: Vec<u32> = (0..n).map(|i| rp(hs + k * i)).collect();
    if alertable { crate::apc::wait(c, &v, all, ms) } else { obj::wait(&v, all, ms, None) }
}

// ---- ioctls ----

const FIONBIO: u32 = 0x8004667e;
const FIONREAD: u32 = 0x4004667f;
const SIOCATMARK: u32 = 0x40047307;

#[allow(clippy::too_many_arguments)]
fn ioctl(st: &mut State, id: u32, h: u32, code: u32, inp: u32, inlen: u32, out: u32, outlen: u32, retp: u32) -> Result<u32, u32> {
    let k = ps();
    let s = st.socks.get_mut(&id).unwrap();
    let out32 = |v: u32| -> Result<u32, u32> {
        if out == 0 || outlen < 4 {
            return Err(WSAEFAULT);
        }
        w32(out, v);
        Ok(4)
    };
    match code {
        FIONBIO => {
            if inp == 0 || inlen < 4 {
                return Err(WSAEFAULT);
            }
            let v = r32(inp) != 0;
            if !v && s.notify != Notify::None {
                return Err(WSAEINVAL);
            }
            s.nonblock = v;
            Ok(0)
        }
        FIONREAD => out32(if s.stream() { s.rx.len() as u32 } else { s.dgrams.iter().map(|d| d.1.len() as u32).sum() }),
        // (FALSE: urgent data is waiting)
        SIOCATMARK => out32((s.oob.is_none() && !s.oob_inline) as u32),
        // SIO_GET_EXTENSION_FUNCTION_POINTER
        0xc8000006 => {
            if inp == 0 || inlen < 16 || out == 0 || outlen < k {
                return Err(WSAEFAULT);
            }
            let g = mem::slice(inp, 16).to_vec();
            let f = EXTENSIONS.iter().find(|e| e.0 == g.as_slice()).ok_or(WSAEINVAL)?;
            wp(out, crate::api::host_fn(f.1, f.2, f.3));
            Ok(k)
        }
        // SIO_ADDRESS_LIST_QUERY: SOCKET_ADDRESS_LIST { iAddressCount, Address[] }; only
        // non-loopback addresses count, and there are none. The size needed is reported when
        // the buffer is missing or short (a buffer shorter than the count is WSAEINVAL)
        0x48000016 => {
            let need = k;
            if out != 0 && outlen != 0 && outlen < 4 {
                mem::put32(retp, 0);
                return Err(WSAEINVAL);
            }
            if out == 0 || outlen < need {
                mem::put32(retp, need);
                return Err(WSAEFAULT);
            }
            mem::write(out, &vec![0; need as usize]);
            Ok(need)
        }
        // SIO_GET_INTERFACE_LIST: INTERFACE_INFO { iiFlags, iiAddress, iiBroadcastAddress,
        // iiNetmask } (sockaddr_gen: 24 bytes each), the loopback interface (no broadcast)
        0x4004747f => {
            if out == 0 || outlen < 76 {
                return Err(WSAEFAULT);
            }
            mem::write(out, &[0; 76]);
            w32(out, 1 | 4 | 0x10); // IFF_UP | IFF_LOOPBACK | IFF_MULTICAST
            mem::write(out + 4, &Addr::v4([127, 0, 0, 1], 0).bytes());
            mem::write(out + 52, &Addr::v4([255, 0, 0, 0], 0).bytes());
            Ok(76)
        }
        // SIO_KEEPALIVE_VALS: tcp_keepalive { onoff, time, interval }
        0x98000004 => {
            if inp == 0 || inlen < 12 {
                return Err(WSAEFAULT);
            }
            Ok(0)
        }
        // SIO_ROUTING_INTERFACE_QUERY(destination) -> the local address used
        0xc8000014 => {
            if inp == 0 || inlen < 16 {
                return Err(WSAEINVAL);
            }
            let fam = mem::r16(inp) as u32;
            if fam != AF_INET && fam != AF_INET6 {
                return Err(WSAEAFNOSUPPORT);
            }
            let d = read_sa(inp, inlen).map_err(|_| WSAEINVAL)?;
            if d.ip.iter().all(|&b| b == 0) {
                return Err(WSAEINVAL);
            }
            if !d.loopback() {
                return Err(WSAENETUNREACH);
            }
            let src = source_for(s, &d);
            let a = Addr { port: 0, ..src };
            if out == 0 {
                return Err(WSAEFAULT);
            }
            if outlen < a.size() {
                mem::put32(retp, a.size());
                return Err(WSAEFAULT);
            }
            mem::write(out, &a.bytes());
            Ok(a.size())
        }
        // SIO_ADDRESS_LIST_CHANGE: no change ever comes
        0x28000017 => Err(WSAEWOULDBLOCK),
        // SIO_BASE_HANDLE, SIO_BSP_HANDLE(_SELECT, _POLL)
        0x48000022 | 0x4800001b | 0x4800001c | 0x4800001d => {
            if out == 0 || outlen < k {
                return Err(WSAEFAULT);
            }
            wp(out, h);
            Ok(k)
        }
        // SIO_UDP_CONNRESET
        0x9800000c => {
            if inp == 0 || inlen < 4 {
                return Err(WSAEFAULT);
            }
            s.udp_connreset = r32(inp) != 0;
            Ok(0)
        }
        // SIO_IDEAL_SEND_BACKLOG_QUERY
        0x4004747b => {
            if !s.stream() {
                return Err(WSAEOPNOTSUPP);
            }
            if !s.connected {
                return Err(WSAENOTCONN);
            }
            out32(0x10000)
        }
        // SIO_LOOPBACK_FAST_PATH, SIO_TCP_SET_ACK_FREQUENCY, SIO_ENABLE_CIRCULAR_QUEUEING
        0x98000010 | 0x98000017 | 0x28000002 => Ok(0),
        // SIO_UDP_NETRESET
        0x9800000f => {
            if inp == 0 || inlen < 4 {
                return Err(WSAEFAULT);
            }
            Ok(0)
        }
        _ => Err(WSAEOPNOTSUPP),
    }
}

fn ioctlsocket(c: &mut Ctx) -> u32 {
    let (h, code, p) = (c.a(0), c.a(1), c.a(2));
    res(on(h, |st, id, acts| {
        if bad_ptr(p) {
            return Err(WSAEFAULT);
        }
        let r = match code {
            FIONBIO => ioctl(st, id, h, code, p, 4, 0, 0, 0),
            FIONREAD | SIOCATMARK => ioctl(st, id, h, code, 0, 0, p, 4, 0),
            _ => Err(WSAEINVAL),
        };
        // (a query changes nothing: no wakeup of every waiting thread, per frame per socket)
        if code == FIONBIO {
            st.touch();
        }
        st.refresh(id, acts);
        r.map(|_| 0)
    }))
}

/// WSAIoctl codes that only read (FIONREAD, SIOCATMARK, the extension functions, address and
/// interface lists, the base handles): they wake nobody
pub(crate) fn query_only(code: u32) -> bool {
    matches!(code, FIONREAD | SIOCATMARK | 0xc8000006 | 0x48000016 | 0x4004747b | 0x48000022 | 0x4800001b | 0x4800001c | 0x4800001d)
}

/// WSAIoctl(s, code, in, inlen, out, outlen, &returned, ov, routine)
fn WSAIoctl(c: &mut Ctx) -> u32 {
    let (h, code, inp, inlen, out, outlen, retp, ov, routine) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7), c.a(8));
    if !started() {
        return err(WSANOTINITIALISED);
    }
    if !is_socket(h) {
        return err(WSAENOTSOCK);
    }
    // the count of bytes returned must have somewhere to go
    if retp == 0 {
        return err(WSAEFAULT);
    }
    let tid = crate::cur().tid;
    // Windows answers some codes in ws2_32 itself (at once, an OVERLAPPED completing with no
    // bytes) and passes the others to the driver (an OVERLAPPED call is pending and completes
    // with the result, error or not); the handle codes are ws2_32's only without one
    let base = matches!(code, 0x48000022 | 0x4800001b | 0x4800001c | 0x4800001d);
    let driver = !matches!(
        code,
        FIONBIO | FIONREAD | SIOCATMARK | 0xc8000006 | 0x48000016 | 0x98000004 | 0xc8000014 | 0x9800000c | 0x9800000f | 0x4004747b | 0x98000010
            | 0x98000017 | 0x28000002 | 0x28000017
    ) && !(base && ov == 0);
    if code == 0x28000017 && ov != 0 {
        // SIO_ADDRESS_LIST_CHANGE: pending until cancelled (no change ever comes)
        ov_start(ov);
        return with(|st, _| {
            // (closed on another thread since is_socket)
            let Some(s) = st.id(h).ok().and_then(|id| st.socks.get_mut(&id)) else { return err(WSAENOTSOCK) };
            let k = ps();
            wp(ov, STATUS_PENDING);
            wp(ov + k, 0);
            s.pend.push(Pend { kind: PendKind::Forever, ov, routine, tid, handle: h });
            st.touch();
            err(WSA_IO_PENDING)
        });
    }
    let r = if base && ov != 0 {
        Err(WSAEOPNOTSUPP)
    } else {
        on(h, |st, id, acts| {
            let r = ioctl(st, id, h, code, inp, inlen, out, outlen, retp);
            if !query_only(code) {
                st.touch();
            }
            st.refresh(id, acts);
            r
        })
    };
    if driver && ov != 0 {
        ov_start(ov);
        let (status, n) = match r {
            Ok(n) => (0, n),
            Err(e) => (wsa_to_nt(e), 0),
        };
        let mut acts = vec![Act::Complete(h, ov, status, n, false)];
        if routine != 0 {
            acts.push(Act::Apc(tid, routine, [nt_to_wsa(status), n, ov, 0]));
        }
        run(acts);
        return err(WSA_IO_PENDING);
    }
    match r {
        Ok(n) => {
            // (SIO_KEEPALIVE_VALS leaves the count of an OVERLAPPED call alone)
            if !(code == 0x98000004 && ov != 0) {
                mem::put32(retp, n);
            }
            if ov != 0 {
                ov_start(ov);
                let mut acts = vec![Act::Complete(h, ov, 0, 0, true)];
                if routine != 0 {
                    acts.push(Act::Apc(tid, routine, [0, 0, ov, 0]));
                }
                run(acts);
            }
            last(0);
            0
        }
        Err(e) => {
            // the driver's failures report no bytes
            if driver || (code == 0x98000004 && ov == 0) {
                mem::put32(retp, 0);
            }
            err(e)
        }
    }
}

// ---- options ----

const SOL_SOCKET: u32 = 0xffff;
const SO_DONTLINGER: u32 = !0x80;
const SO_EXCLUSIVEADDRUSE: u32 = !0x4;

/// How an option's value travels (the shapes ws2_32's test_set_getsockopt pins down):
/// `short`: it may be passed / read in fewer than 4 bytes (as one byte); `sizes`: the length
/// getsockopt reports for a 1-, 2- and 4-byte buffer (0 for a short buffer: it is cleared);
/// `boolean`: any non-zero value reads back as 1; `large`: values over 255 are taken (else
/// WSAEINVAL); `wide2`: a 2-byte buffer gets two bytes (still reported as 1); `zero`: a short
/// buffer is zeroed before the WSAEFAULT
#[derive(Clone, Copy)]
struct Shape {
    short: bool,
    sizes: [u32; 3],
    boolean: bool,
    large: bool,
    wide2: bool,
    zero: bool,
}

const fn shape(short: bool, sizes: [u32; 3], boolean: bool, large: bool, wide2: bool, zero: bool) -> Shape {
    Shape { short, sizes, boolean, large, wide2, zero }
}
/// a BOOLEAN: one byte
const BYTE: Shape = shape(true, [1, 1, 1], false, true, false, false);
/// a SOL_SOCKET BOOL
const BOOL_SOCKET: Shape = shape(true, [1, 1, 4], true, true, true, false);
/// an IP-level BOOL
const BOOL_IP: Shape = shape(true, [1, 1, 4], true, true, false, false);
/// a DWORD under 256 (TTLs, hop limits, TOS)
const SMALL: Shape = shape(true, [1, 1, 4], false, false, false, false);
/// a SOL_SOCKET DWORD (the buffer sizes and SO_LINGER zero a short buffer)
const INT: Shape = shape(false, [1, 2, 4], false, true, false, false);
const INT_ZERO: Shape = shape(false, [1, 2, 4], false, true, false, true);
/// a DWORD of the other levels
const INT4: Shape = shape(false, [0, 0, 4], false, true, false, false);
const BOOL4: Shape = shape(false, [0, 0, 4], true, true, false, false);

fn shape_of(level: u32, opt: u32) -> Shape {
    match (level, opt) {
        (SOL_SOCKET, 0x0020 | SO_DONTLINGER | 0x0100 | 0x0004 | SO_EXCLUSIVEADDRUSE) => BOOL_SOCKET,
        (SOL_SOCKET, 0x0008 | 0x0010) => BYTE,
        (SOL_SOCKET, 0x0080 | 0x1001 | 0x1002) => INT_ZERO,
        (SOL_SOCKET, _) => INT,
        (6, 1) => BYTE,
        (6, 16) => shape(false, [0, 0, 4], false, false, false, false),
        (0, 11 | 14) | (41, 11 | 14) => BOOL_IP,
        (0, 3 | 4 | 10) | (41, 4 | 10) => SMALL,
        (0, 19 | 21 | 40) | (41, 19 | 21 | 40) => BOOL4,
        (41, 27) => BYTE,
        _ => INT4,
    }
}

/// a guest pointer that can't be valid (NULL and the first 64K)
fn bad_ptr(p: u32) -> bool {
    p < 0x10000
}

/// setsockopt's value as the option's shape reads it from (val, len)
fn opt_value(level: u32, opt: u32, val: u32, len: i32) -> Result<u32, u32> {
    let sh = shape_of(level, opt);
    if bad_ptr(val) {
        return Err(WSAEFAULT);
    }
    let v = match len {
        l if l < 0 => {
            if sh.sizes[2] == 1 || level != SOL_SOCKET {
                return Err(WSAENOBUFS);
            }
            r32(val)
        }
        0 => {
            if level == SOL_SOCKET && sh.boolean {
                return Ok(0);
            }
            return Err(WSAEFAULT);
        }
        1..=3 if !sh.short => return Err(WSAEFAULT),
        1..=3 => mem::r8(val) as u32,
        _ if sh.sizes[2] == 1 => mem::r8(val) as u32,
        _ => r32(val),
    };
    if !sh.large && v > 255 {
        return Err(WSAEINVAL);
    }
    Ok(if sh.boolean { (v != 0) as u32 } else { v })
}

/// getsockopt's answer `v` written as the option's shape writes it into (val, *lenp)
fn opt_put(level: u32, opt: u32, val: u32, lenp: u32, v: u32) -> Result<u32, u32> {
    let sh = shape_of(level, opt);
    let mut len = r32(lenp) as i32;
    if len < 0 && level == SOL_SOCKET && opt == 0x7008 {
        len = 4; // (SO_OPENTYPE takes any length)
    }
    let n = match len {
        l if l <= 0 => return Err(WSAEFAULT),
        1..=3 if !sh.short => {
            if sh.zero {
                mem::write(val, &vec![0; len as usize]);
            }
            if sh.sizes[0] == 0 {
                w32(lenp, 0);
            }
            return Err(WSAEFAULT);
        }
        1..=3 => {
            let n = if len >= 2 && sh.wide2 { 2 } else { 1 };
            mem::write(val, &v.to_le_bytes()[..n]);
            w32(lenp, 1);
            return Ok(0);
        }
        _ => sh.sizes[2],
    };
    mem::write(val, &v.to_le_bytes()[..n as usize]);
    w32(lenp, n);
    Ok(0)
}

/// SO_OPENTYPE: a per-thread setting (whether new sockets are synchronous), whatever the
/// socket named
static OPENTYPE: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
fn opentype(set: Option<u32>) -> u32 {
    let tid = crate::cur().tid;
    let mut g = OPENTYPE.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
    if let Some(v) = set {
        m.insert(tid, v);
    }
    m.get(&tid).copied().unwrap_or(0)
}

fn getsockopt(c: &mut Ctx) -> u32 {
    let (h, level, opt, val, lenp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if level == SOL_SOCKET && opt == 0x7008 {
        if !started() {
            return err(WSANOTINITIALISED);
        }
        if bad_ptr(val) || bad_ptr(lenp) {
            return err(WSAEFAULT);
        }
        return res(opt_put(level, opt, val, lenp, opentype(None)));
    }
    res(on(h, |st, id, _| {
        if bad_ptr(val) || bad_ptr(lenp) {
            return Err(WSAEFAULT);
        }
        let len = r32(lenp) as i32;
        let s = st.socks.get_mut(&id).unwrap();
        let put = |v: u32, _size: u32| opt_put(level, opt, val, lenp, v);
        match level {
            SOL_SOCKET => match opt {
                0x0002 => put(s.listening as u32, 4),
                0x0004 => put(s.reuse as u32, 4),
                SO_EXCLUSIVEADDRUSE => put(s.exclusive as u32, 4),
                0x0020 => put(s.broadcast as u32, 4),
                0x0080 => {
                    if !s.stream() {
                        return Err(WSAENOPROTOOPT);
                    }
                    put(s.linger.0 as u32 | (s.linger.1 as u32) << 16, 4)
                }
                SO_DONTLINGER => {
                    if !s.stream() {
                        return Err(WSAENOPROTOOPT);
                    }
                    put((s.linger.0 == 0) as u32, 4)
                }
                0x1001 => put(s.sndbuf, 4),
                0x1002 => put(s.rcvbuf, 4),
                0x1005 => put(s.sndtimeo, 4),
                0x1006 => put(s.rcvtimeo, 4),
                0x1007 => {
                    let e = s.so_error;
                    s.so_error = 0;
                    put(e, 4)
                }
                0x1008 => put(s.ty, 4),
                0x2001 | 0x2002 => put(0, 4),
                0x2003 => {
                    if s.stream() {
                        return Err(WSAENOPROTOOPT);
                    }
                    put(if s.af == AF_INET6 { 65527 } else { 65507 }, 4)
                }
                0x2004 | 0x2005 => {
                    let wide = opt == 0x2005;
                    let size = if wide { 628 } else { 372 };
                    if len < size {
                        w32(lenp, size as u32);
                        return Err(WSAEFAULT);
                    }
                    let e = names::catalog_entry(s.af, s.ty, s.proto).ok_or(WSAENOPROTOOPT)?;
                    names::protocol_info(val, e, wide, 0);
                    Ok(0)
                }
                0x700c => put(s.connect_time.map_or(u32::MAX, |t| t.elapsed().as_secs() as u32), 4),
                0x0001 | 0x0008 | 0x0010 | 0x0100 | 0x3002 | 0x3005 | 0x3006 | 0x3007 => put(s.opts.get(&(level, opt)).copied().unwrap_or(0), 4),
                0x0040 | 0x1003 | 0x1004 => Err(WSAENOPROTOOPT),
                _ => Err(WSAENOPROTOOPT),
            },
            6 => {
                if !s.stream() {
                    return Err(WSAENOPROTOOPT);
                }
                match opt {
                    1 | 2 | 10 | 11 | 12 | 13 | 15 | 18 => put(s.opts.get(&(6, opt)).copied().unwrap_or(0), 4),
                    3 => put(s.opts.get(&(6, 3)).copied().unwrap_or(7200), 4),
                    4 => {
                        if !s.connected {
                            return Err(WSAENOTCONN);
                        }
                        put(65495, 4)
                    }
                    5 => put(s.opts.get(&(6, 5)).copied().unwrap_or(0), 4),
                    16 => put(s.opts.get(&(6, 16)).copied().unwrap_or(10), 4),
                    17 => put(s.opts.get(&(6, 17)).copied().unwrap_or(1), 4),
                    _ => Err(WSAENOPROTOOPT),
                }
            }
            0 | 41 => {
                let v6 = level == 41;
                if v6 != (s.af == AF_INET6) {
                    return Err(WSAEINVAL);
                }
                let stream = s.stream();
                let dgram_only: &[u32] = if v6 { &[9, 10, 11, 19, 21, 24, 25, 38, 40, 47, 50, 51, 60, 70, 75] } else { &[9, 10, 11, 19, 21, 22, 24, 25, 38, 40, 47, 50, 51, 60, 70, 75] };
                if stream && dgram_only.contains(&opt) {
                    return Err(WSAEINVAL);
                }
                match opt {
                    2 => Err(WSAEINVAL),        // HDRINCL: raw sockets only
                    12 | 13 => Err(WSAENOPROTOOPT), // ADD/DROP_MEMBERSHIP: set only
                    33 => Err(WSAEINVAL),       // GET_IFLIST
                    72 | 73 if !(s.connected || s.peer.is_some()) => Err(WSAENOTCONN),
                    72 | 73 => put(65535, 4),
                    4 => put(s.opts.get(&(level, 4)).copied().unwrap_or(128), 4),
                    10 => put(s.opts.get(&(level, 10)).copied().unwrap_or(1), 4),
                    11 => put(s.opts.get(&(level, 11)).copied().unwrap_or(1), 4),
                    27 if v6 => put(s.v6only as u32, 4),
                    1 => {
                        // IP_OPTIONS: none
                        w32(lenp, 0);
                        Ok(0)
                    }
                    3 | 9 | 14 | 19 | 21 | 22 | 23 | 24 | 25 | 28 | 31 | 32 | 38 | 39 | 40 | 47 | 50 | 51 | 60 | 70 | 71 | 75 | 76 => put(s.opts.get(&(level, opt)).copied().unwrap_or(0), 4),
                    _ => Err(WSAENOPROTOOPT),
                }
            }
            _ => Err(WSAEINVAL),
        }
    }))
}

fn setsockopt(c: &mut Ctx) -> u32 {
    let (h, level, opt, val, len) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4) as i32);
    if level == SOL_SOCKET && opt == 0x7008 {
        if !started() {
            return err(WSANOTINITIALISED);
        }
        return res(opt_value(level, opt, val, len).map(|v| {
            opentype(Some(v));
            0
        }));
    }
    res(on(h, |st, id, acts| {
        let s = st.socks.get_mut(&id).unwrap();
        let get = || opt_value(level, opt, val, len);
        let r = match level {
            SOL_SOCKET => match opt {
                0x0004 => {
                    let v = get()? != 0;
                    if v && s.exclusive {
                        return Err(WSAEINVAL);
                    }
                    s.reuse = v;
                    Ok(0)
                }
                SO_EXCLUSIVEADDRUSE => {
                    let v = get()? != 0;
                    if v && s.reuse {
                        return Err(WSAEINVAL);
                    }
                    s.exclusive = v;
                    Ok(0)
                }
                0x0020 => {
                    s.broadcast = get()? != 0;
                    Ok(0)
                }
                0x0080 => {
                    if !s.stream() {
                        return Err(WSAENOPROTOOPT);
                    }
                    let v = get()?;
                    s.linger = (v as u16, (v >> 16) as u16);
                    Ok(0)
                }
                SO_DONTLINGER => {
                    if !s.stream() {
                        return Err(WSAENOPROTOOPT);
                    }
                    s.linger.0 = (get()? == 0) as u16;
                    Ok(0)
                }
                0x1001 => {
                    s.sndbuf = get()?;
                    Ok(0)
                }
                0x1002 => {
                    s.rcvbuf = get()?;
                    Ok(0)
                }
                0x1005 => {
                    s.sndtimeo = get()?;
                    Ok(0)
                }
                0x1006 => {
                    s.rcvtimeo = get()?;
                    Ok(0)
                }
                0x1007 => {
                    s.so_error = get()?;
                    Ok(0)
                }
                0x1008 | 0x0002 | 0x2003 | 0x2004 | 0x2005 | 0x700c | 0x2001 => Err(WSAENOPROTOOPT),
                0x700b | 0x7010 => Ok(0), // SO_UPDATE_ACCEPT/CONNECT_CONTEXT
                0x0001 | 0x0008 | 0x0010 | 0x0100 | 0x2002 | 0x3002 | 0x3005 | 0x3006 | 0x3007 => {
                    let v = get()?;
                    s.opts.insert((level, opt), if opt == 0x2002 { v } else { (v != 0) as u32 });
                    Ok(0)
                }
                _ => Err(WSAENOPROTOOPT),
            },
            6 => {
                if !s.stream() {
                    return Err(WSAENOPROTOOPT);
                }
                match opt {
                    1 | 2 | 10 | 11 | 12 | 13 | 15 | 18 => {
                        let v = get()?;
                        s.opts.insert((6, opt), (v != 0) as u32);
                        Ok(0)
                    }
                    3 | 5 | 16 | 17 => {
                        let v = get()?;
                        s.opts.insert((6, opt), v);
                        Ok(0)
                    }
                    4 => Ok(0),
                    _ => Err(WSAENOPROTOOPT),
                }
            }
            0 | 41 => {
                let v6 = level == 41;
                if v6 != (s.af == AF_INET6) {
                    return Err(WSAEINVAL);
                }
                let dgram_only: &[u32] = if v6 { &[9, 10, 11, 19, 21, 24, 25, 38, 40, 47, 50, 51, 60, 70, 75] } else { &[9, 10, 11, 19, 21, 22, 24, 25, 38, 40, 47, 50, 51, 60, 70, 75] };
                if s.stream() && dgram_only.contains(&opt) {
                    return Err(WSAEINVAL);
                }
                match opt {
                    2 => Err(WSAEINVAL),
                    12 | 13 => {
                        if val == 0 || len < if v6 { 20 } else { 8 } {
                            return Err(WSAEFAULT);
                        }
                        Ok(0)
                    }
                    33 => Err(WSAEINVAL),
                    71 => {
                        let v = get()?;
                        if v == 0 || v > 3 {
                            return Err(WSAEINVAL);
                        }
                        s.opts.insert((level, 71), v);
                        Ok(0)
                    }
                    72 | 73 => Err(WSAENOPROTOOPT),
                    27 if v6 => {
                        if s.local.is_some() {
                            return Err(WSAEINVAL);
                        }
                        s.v6only = get()? != 0;
                        Ok(0)
                    }
                    1 => Ok(0),
                    3 | 4 | 9 | 10 | 11 | 14 | 19 | 21 | 22 | 23 | 24 | 25 | 28 | 31 | 32 | 38 | 39 | 40 | 47 | 50 | 51 | 60 | 70 | 75 | 76 => {
                        let v = get()?;
                        s.opts.insert((level, opt), v);
                        Ok(0)
                    }
                    _ => Err(WSAENOPROTOOPT),
                }
            }
            _ => Err(WSAEINVAL),
        };
        if r.is_ok() {
            st.touch();
        }
        st.refresh(id, acts);
        r
    }))
}

// ---- Microsoft extensions ----

/// AcceptEx's buffer: the received data, then the local and remote addresses, each in a slot
/// of `llen` / `rlen` bytes as { int length; sockaddr }
fn put_accept_addrs(st: &State, acc: u32, buf: u32, data: u32, llen: u32, rlen: u32) {
    let Ok(id) = st.id(acc) else { return };
    let s = &st.socks[&id];
    let put = |at: u32, room: u32, a: &Option<Addr>| {
        if let Some(a) = a {
            if room >= a.size() + 4 {
                w32(at, a.size());
                mem::write(at + 4, &a.bytes());
            }
        }
    };
    put(buf + data, llen, &s.local);
    put(buf + data + llen, rlen, &s.peer);
}

/// AcceptEx(listen, accept, buf, dataLen, localLen, remoteLen, &received, ov)
fn AcceptEx(c: &mut Ctx) -> u32 {
    let (lh, ah, buf, data, llen, rlen, recvp, ov) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    ov_start(ov);
    let tid = crate::cur().tid;
    let r = on(lh, |st, id, acts| {
        let l = &st.socks[&id];
        if !l.listening {
            return Err(WSAEINVAL);
        }
        let aid = st.id(ah)?;
        let a = &st.socks[&aid];
        if a.local.is_some() || a.connected {
            return Err(WSAEINVAL);
        }
        if ov == 0 {
            return Err(WSAEINVAL);
        }
        let min = if l.af == AF_INET6 { 44 } else { 32 };
        if llen < min || rlen < min {
            return Err(WSAEINVAL);
        }
        let now = !st.socks[&id].queue.is_empty() && st.socks[&id].pend.iter().all(|p| !matches!(p.kind, PendKind::Accept { .. })) && data == 0;
        if now {
            st.accept_into(id, ah, acts)?;
            put_accept_addrs(st, ah, buf, data, llen, rlen);
            return Ok(true);
        }
        let k = ps();
        wp(ov, STATUS_PENDING);
        wp(ov + k, 0);
        st.socks.get_mut(&id).unwrap().pend.push(Pend { kind: PendKind::Accept { acc: ah, buf, data, llen, rlen }, ov, routine: 0, tid, handle: lh });
        st.touch();
        st.serve(id, acts);
        Ok(false)
    });
    match r {
        Ok(true) => {
            mem::put32(recvp, 0);
            iocp::complete(lh, ov, 0, 0, true);
            1
        }
        Ok(false) => {
            last(WSA_IO_PENDING);
            0
        }
        Err(e) => {
            last(e);
            0
        }
    }
}

/// GetAcceptExSockaddrs(buf, dataLen, localLen, remoteLen, &local, &localSize, &remote, &remoteSize)
fn GetAcceptExSockaddrs(c: &mut Ctx) -> u32 {
    let (buf, data, llen, _rlen, lp, lsp, rpp, rsp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    let l = buf + data;
    let r = l + llen;
    if lp != 0 {
        wp(lp, l + 4);
    }
    mem::put32(lsp, r32(l));
    if rpp != 0 {
        wp(rpp, r + 4);
    }
    mem::put32(rsp, r32(r));
    0
}

/// ConnectEx(s, name, namelen, sendbuf, sendlen, &sent, ov): the socket must be bound
fn ConnectEx(c: &mut Ctx) -> u32 {
    let (h, sa, len, sb, sl, sentp, ov) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    ov_start(ov);
    let pre = on(h, |st, id, _| {
        let s = &st.socks[&id];
        if !s.stream() {
            return Err(WSAEINVAL);
        }
        if s.local.is_none() {
            return Err(WSAEINVAL);
        }
        if s.connected {
            return Err(WSAEISCONN);
        }
        if ov == 0 {
            return Err(WSAEINVAL);
        }
        Ok(())
    });
    if let Err(e) = pre {
        last(e);
        return 0;
    }
    let srv_h = new_handle();
    let r = with(|st, acts| {
        let id = st.id(h)?;
        let dst = read_sa(sa, len)?;
        let r = do_connect(st, id, dst, srv_h, acts);
        if r.is_ok() {
            st.socks.get_mut(&id).unwrap().connect_result = Some(0);
            st.refresh(id, acts);
        }
        r
    });
    if r.is_err() {
        obj::close(srv_h);
    }
    match r {
        Ok(()) => {
            let n = if sb != 0 && sl != 0 { send_common(Some(c), h, &[(sb, sl)], 0, 0, 0).unwrap_or(0) } else { 0 };
            mem::put32(sentp, n);
            iocp::complete(h, ov, 0, n, true);
            1
        }
        Err(e) => {
            // a refused connection completes with the error
            if matches!(e, WSAECONNREFUSED | WSAENETUNREACH) {
                iocp::complete(h, ov, wsa_to_nt(e), 0, false);
                last(WSA_IO_PENDING);
                return 0;
            }
            last(e);
            0
        }
    }
}

/// DisconnectEx(s, ov, flags, reserved)
fn DisconnectEx(c: &mut Ctx) -> u32 {
    let (h, ov) = (c.a(0), c.a(1));
    let r = on(h, |st, id, acts| {
        let s = st.socks.get_mut(&id).unwrap();
        if !s.stream() || !s.connected {
            return Err(WSAENOTCONN);
        }
        s.shut_wr = true;
        if let Some(p) = s.conn {
            if let Some(o) = st.socks.get_mut(&p) {
                o.eof = true;
            }
            st.serve(p, acts);
            st.refresh(p, acts);
        }
        st.touch();
        Ok(())
    });
    match r {
        Ok(()) => {
            iocp::complete(h, ov, 0, 0, true);
            1
        }
        Err(e) => {
            last(e);
            0
        }
    }
}

/// TransmitFile(s, file, bytes, perSend, ov, buffers, flags): TRANSMIT_FILE_BUFFERS { Head,
/// HeadLength, Tail, TailLength }
fn TransmitFile(c: &mut Ctx) -> u32 {
    let (h, file, n, ov, tb) = (c.a(0), c.a(1), c.a(2), c.a(4), c.a(5));
    ov_start(ov);
    let k = ps();
    // (sent piece by piece as it is read, as sends that wait for room: the file is not read
    // whole into memory first)
    let r = (|| -> Result<u32, u32> {
        // (a stream socket that is connected: else WSAENOTCONN, as before anything is read)
        if !on(h, |st, id, _| Ok(st.socks[&id].stream() && (st.socks[&id].conn.is_some() || st.socks[&id].peer_gone)))? {
            return Err(WSAENOTCONN);
        }
        let mut sent = 0u32;
        let mut put = |c: &mut Ctx, p: u32, l: u32| -> Result<(), u32> {
            if p != 0 && l != 0 {
                sent = sent.saturating_add(send_common(Some(c), h, &[(p, l)], 0, 0, 0)?);
            }
            Ok(())
        };
        if tb != 0 {
            put(c, rp(tb), r32(tb + k))?;
        }
        if file != 0 {
            let Some(f) = obj::file(file) else { return Err(WSAEINVAL) };
            if ov != 0 {
                f.seek(std::io::SeekFrom::Start(r32(ov + 2 * k) as u64 | (r32(ov + 2 * k + 4) as u64) << 32));
            }
            let mut left = if n == 0 { u32::MAX } else { n };
            let tmp = mem::heap_alloc(65536, false);
            if tmp == 0 {
                return Err(WSAENOBUFS);
            }
            let r: Result<(), u32> = (|| {
                while left > 0 {
                    let got = f.read_into(tmp, left.min(65536));
                    if got == 0 {
                        break;
                    }
                    put(c, tmp, got)?;
                    left -= got;
                }
                Ok(())
            })();
            mem::heap_free(tmp);
            r?;
        }
        if tb != 0 {
            put(c, rp(tb + 2 * k), r32(tb + 3 * k))?;
        }
        Ok(sent)
    })();
    match r {
        Ok(n) => {
            iocp::complete(h, ov, 0, n, true);
            1
        }
        Err(e) => {
            last(e);
            0
        }
    }
}

/// WSARecvMsg(s, &WSAMSG, &got, ov, routine): WSAMSG { name, namelen, lpBuffers, dwBufferCount,
/// Control { len, buf }, dwFlags }
fn WSARecvMsg(c: &mut Ctx) -> u32 {
    let (h, m, gotp, ov, routine) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let k = ps();
    if bad_ptr(m) {
        return err(WSAEFAULT);
    }
    // WSAMSG { name, namelen, lpBuffers, dwBufferCount, Control { len, buf }, dwFlags }
    let (name, namelenp) = (rp(m), m + k);
    let bufs = wsabufs(rp(m + 2 * k), r32(m + 3 * k));
    let flags_at = if k == 8 { m + 48 } else { m + 24 };
    recv_ex(c, h, &bufs, gotp, flags_at, name, if name != 0 { namelenp } else { 0 }, ov, routine, m)
}

/// WSASendMsg(s, &WSAMSG, flags, &sent, ov, routine)
fn WSASendMsg(c: &mut Ctx) -> u32 {
    let (h, m, sentp, ov, routine) = (c.a(0), c.a(1), c.a(3), c.a(4), c.a(5));
    let k = ps();
    if m == 0 {
        return err(WSAEFAULT);
    }
    let (name, namelen) = (rp(m), r32(m + k));
    let bufs = wsabufs(rp(m + 2 * k), r32(m + 3 * k));
    let r = send_common(Some(c), h, &bufs, name, namelen, 0);
    complete_now(h, ov, routine, r.map(|n| (n, 0)), sentp)
}

type Ext = (&'static [u8], &'static str, crate::api::Handler, u32);
/// The SIO_GET_EXTENSION_FUNCTION_POINTER functions: (GUID, name, handler, stdcall bytes)
static EXTENSIONS: &[Ext] = &[
    (&[0xf1, 0x7d, 0x36, 0xb5, 0xac, 0xcb, 0xcf, 0x11, 0x95, 0xca, 0x00, 0x80, 0x5f, 0x48, 0xa1, 0x92], "ws2_AcceptEx", AcceptEx, 32),
    (&[0xb9, 0x07, 0xa2, 0x25, 0xf3, 0xdd, 0x60, 0x46, 0x8e, 0xe9, 0x76, 0xe5, 0x8c, 0x74, 0x06, 0x3e], "ws2_ConnectEx", ConnectEx, 28),
    (&[0x11, 0x2e, 0xda, 0x7f, 0x30, 0x86, 0x6f, 0x43, 0xa0, 0x31, 0xf5, 0x36, 0xa6, 0xee, 0xc1, 0x57], "ws2_DisconnectEx", DisconnectEx, 16),
    (&[0xf2, 0x7d, 0x36, 0xb5, 0xac, 0xcb, 0xcf, 0x11, 0x95, 0xca, 0x00, 0x80, 0x5f, 0x48, 0xa1, 0x92], "ws2_GetAcceptExSockaddrs", GetAcceptExSockaddrs, 32),
    (&[0xf0, 0x7d, 0x36, 0xb5, 0xac, 0xcb, 0xcf, 0x11, 0x95, 0xca, 0x00, 0x80, 0x5f, 0x48, 0xa1, 0x92], "ws2_TransmitFile", TransmitFile, 28),
    (&[0xc8, 0xd7, 0x89, 0xf6, 0x1f, 0x6f, 0x6b, 0x43, 0x8a, 0x53, 0xe5, 0x4f, 0xe3, 0x51, 0xc3, 0x22], "ws2_WSARecvMsg", WSARecvMsg, 20),
    (&[0x12, 0xe7, 0x41, 0xa4, 0x4f, 0x75, 0xca, 0x43, 0x84, 0xa7, 0x0d, 0xee, 0x44, 0xcf, 0x60, 0x6d], "ws2_WSASendMsg", WSASendMsg, 24),
];

/// WSADuplicateSocketA/W(s, pid, &info): a protocol info naming this socket
fn duplicate(c: &mut Ctx, wide: bool) -> u32 {
    let (h, pid, info) = (c.a(0), c.a(1), c.a(2));
    res(on(h, |st, id, _| {
        // (only this process: there are no others to give it to)
        if pid != crate::pid() {
            return Err(WSAEINVAL);
        }
        if bad_ptr(info) {
            return Err(WSAEFAULT);
        }
        let (af, ty, proto) = st.socks[&id].info();
        let e = names::catalog_entry(af, ty, proto).ok_or(WSAEINVAL)?;
        names::protocol_info(info, e, wide, h);
        Ok(0)
    }))
}

/// ReadFile / WriteFile on a socket handle: a recv / send
pub fn file_io(h: u32, buf: u32, n: u32, write: bool) -> Result<u32, u32> {
    if write {
        send_common(None, h, &[(buf, n)], 0, 0, 0)
    } else {
        recv_common(None, h, &[(buf, n)], 0).map(|r| r.0)
    }
}

pub fn init() {
    for d in ["ws2_32", "wsock32"] {
        implement(d, "WSAStartup", WSAStartup);
        implement(d, "WSACleanup", WSACleanup);
        implement(d, "WSAGetLastError", |_| get_last());
        implement(d, "WSASetLastError", |c| {
            last(c.a(0));
            0
        });
        implement(d, "socket", socket);
        implement(d, "closesocket", closesocket);
        implement(d, "bind", bind);
        implement(d, "listen", listen);
        implement(d, "connect", connect);
        implement(d, "accept", |c| accept(c, false));
        implement(d, "send", send);
        implement(d, "sendto", sendto);
        implement(d, "recv", recv);
        implement(d, "recvfrom", recvfrom);
        implement(d, "shutdown", shutdown);
        implement(d, "getsockname", getsockname);
        implement(d, "getpeername", getpeername);
        implement(d, "getsockopt", getsockopt);
        implement(d, "setsockopt", setsockopt);
        implement(d, "ioctlsocket", ioctlsocket);
        implement(d, "select", select);
        implement(d, "__WSAFDIsSet", WSAFDIsSet);
        implement(d, "WSAAsyncSelect", WSAAsyncSelect);
        implement(d, "WSAIsBlocking", |_| 0);
        implement(d, "WSACancelBlockingCall", |_| 0);
        implement(d, "WSASetBlockingHook", |c| c.a(0));
        implement(d, "WSAUnhookBlockingHook", |_| 0);
        implement(d, "AcceptEx", AcceptEx);
        implement(d, "GetAcceptExSockaddrs", GetAcceptExSockaddrs);
        implement(d, "TransmitFile", TransmitFile);
    }
    let d = "ws2_32";
    implement(d, "WSASocketA", WSASocket);
    implement(d, "WSASocketW", WSASocket);
    implement(d, "WSAAccept", |c| accept(c, true));
    implement(d, "WSAConnect", |c| connect_to(c.a(0), c.a(1), c.a(2)));
    implement(d, "WSASend", |c| wsa_send(c, false));
    implement(d, "WSASendTo", |c| wsa_send(c, true));
    implement(d, "WSARecv", |c| wsa_recv(c, false));
    implement(d, "WSARecvFrom", |c| wsa_recv(c, true));
    implement(d, "WSASendMsg", WSASendMsg);
    implement(d, "WSAGetOverlappedResult", WSAGetOverlappedResult);
    implement(d, "WSAPoll", WSAPoll);
    implement(d, "WSAEventSelect", WSAEventSelect);
    implement(d, "WSAEnumNetworkEvents", WSAEnumNetworkEvents);
    implement(d, "WSACreateEvent", |_| obj::add(Obj::Event { manual: true, set: false }));
    implement(d, "WSACloseEvent", |c| obj::close(c.a(0)) as u32);
    implement(d, "WSASetEvent", |c| {
        crate::k32::set_event(c.a(0));
        1
    });
    implement(d, "WSAResetEvent", |c| {
        crate::k32::event_set(c.a(0), false);
        1
    });
    implement(d, "WSAWaitForMultipleEvents", WSAWaitForMultipleEvents);
    implement(d, "WSAIoctl", WSAIoctl);
    implement(d, "WSADuplicateSocketA", |c| duplicate(c, false));
    implement(d, "WSADuplicateSocketW", |c| duplicate(c, true));
    implement(d, "WSARecvDisconnect", |c| {
        let h = c.a(0);
        res(on(h, |st, id, _| {
            st.socks.get_mut(&id).unwrap().shut_rd = true;
            st.touch();
            Ok(0)
        }))
    });
    implement(d, "WSASendDisconnect", |c| {
        let h = c.a(0);
        let mut x = [h, 1];
        let _ = &mut x;
        res(on(h, |st, id, acts| {
            let s = st.socks.get_mut(&id).unwrap();
            s.shut_wr = true;
            if let Some(p) = s.conn {
                if let Some(o) = st.socks.get_mut(&p) {
                    o.eof = true;
                }
                st.refresh(p, acts);
            }
            st.touch();
            Ok(0)
        }))
    });
    implement(d, "WSAConnectByNameA", |c| connect_by_name(c, false));
    implement(d, "WSAConnectByNameW", |c| connect_by_name(c, true));
    implement(d, "WSAProviderConfigChange", |c| {
        if c.a(0) == 0 {
            return err(WSAEFAULT);
        }
        // a handle that would complete when the provider catalog changes (it never does)
        if mem::rp(c.a(0)) == 0 {
            wp(c.a(0), obj::add(Obj::Other("provider change")));
        }
        res(Ok(0))
    });
    let _ = (FD_OOB, MSG_TRUNC, WSAEDESTADDRREQ, WSAEPROTOTYPE, WSAEALREADY, WSAENOBUFS, WSAESOCKTNOSUPPORT, WSAEINTR);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// any two SO_RCVBUF / SO_SNDBUF values: the pipe holds between 16 KB and 8 MB, never
        /// wraps (two 0x8000_0000s used to sum to 0), and grows with each value
        #[test]
        fn pipe_cap_is_bounded(r in any::<u32>(), s in any::<u32>(), d in 0u32..1 << 20) {
            let c = pipe_cap(r, s);
            prop_assert!((2 * MIN_BUF as usize..=2 * MAX_BUF as usize).contains(&c));
            prop_assert!(pipe_cap(r.saturating_add(d), s) >= c);
        }

        /// datagrams sent to an unread socket: the queue stays within its receive buffer (or
        /// one datagram), whatever the sizes
        #[test]
        fn datagram_queue_is_bounded(rcv in any::<u32>(), sizes in proptest::collection::vec(0usize..70_000, 1..400)) {
            let mut queued = 0usize;
            let mut kept = 0;
            for n in sizes {
                if dgram_fits(queued, n, rcv) {
                    queued += n;
                    kept += 1;
                }
            }
            prop_assert!(kept >= 1);
            prop_assert!(queued <= buf_size(rcv).max(70_000));
        }
    }

    #[test]
    fn ov_flags_keep_only_nonzero() {
        let mut st = State::default();
        for ov in 0..100_000u32 {
            st.set_ov_flags(ov * 32, 0);
        }
        assert!(st.ov_flags.is_empty());
        st.set_ov_flags(64, 2);
        assert_eq!(st.ov_flags.get(&64), Some(&2));
        // (reused for an operation that ended with no flags: 0 again)
        st.set_ov_flags(64, 0);
        assert_eq!(st.ov_flags.get(&64), None);
        for ov in 0..100_000u32 {
            st.set_ov_flags(ov * 32, 1);
        }
        assert!(st.ov_flags.len() <= MAX_OV_FLAGS);
    }

    #[test]
    fn queries_wake_nobody() {
        for c in [FIONREAD, SIOCATMARK, 0xc8000006, 0x48000016] {
            assert!(query_only(c), "{c:x}");
        }
        for c in [FIONBIO, 0x98000004, 0x9800000c, 0x28000017] {
            assert!(!query_only(c), "{c:x}");
        }
    }

    /// A listener whose queued connection was closed through its handle: accept skips it
    #[test]
    fn closed_queued_connections_are_skipped() {
        let mut st = State::default();
        let mut l = Sock::new(AF_INET, SOCK_STREAM, 6);
        l.listening = true;
        l.queue.extend([7, 8, 9]);
        st.socks.insert(1, l);
        st.socks.insert(9, Sock::new(AF_INET, SOCK_STREAM, 6));
        assert_eq!(st.queued(1, false), Some(9));
        assert_eq!(st.socks[&1].queue.len(), 1);
        assert_eq!(st.queued(1, true), Some(9));
        assert_eq!(st.queued(1, true), None);
        assert_eq!(st.queued(2, true), None);
    }

    #[test]
    fn matching() {
        let any4 = Addr::v4([0; 4], 80);
        let lo4 = Addr::v4([127, 0, 0, 1], 80);
        assert!(takes(&any4, true, &lo4));
        assert!(!takes(&Addr::v4([127, 0, 0, 2], 80), true, &lo4));
        let any6 = Addr::v6([0; 16], 80);
        assert!(!takes(&any6, true, &lo4));
        assert!(takes(&any6, false, &lo4));
        assert_eq!(seen_as(&lo4, AF_INET6), Addr::mapped([127, 0, 0, 1], 80));
        assert_eq!(nt_to_wsa(wsa_to_nt(WSAECONNRESET)), WSAECONNRESET);
    }

    /// Windows' NTSTATUS -> Winsock table (ws2_32's test_WSAGetOverlappedResult)
    #[test]
    fn status_table() {
        assert_eq!(nt_to_wsa(0), 0);
        assert_eq!(nt_to_wsa(STATUS_PENDING), WSA_IO_INCOMPLETE);
        assert_eq!(nt_to_wsa(0x8000_0005), WSAEMSGSIZE);
        assert_eq!(nt_to_wsa(0xC000_0120), WSA_OPERATION_ABORTED);
        assert_eq!(nt_to_wsa(0xC000_0023), WSAEFAULT);
        assert_eq!(nt_to_wsa(0xC000_00BB), WSAEOPNOTSUPP);
        assert_eq!(nt_to_wsa(0x8007_03E4), WSA_IO_INCOMPLETE);
        // other successes report nothing, other failures WSAEINVAL
        assert_eq!(nt_to_wsa(0x4000_0001), 0);
        assert_eq!(nt_to_wsa(0x0000_0102), 0);
        assert_eq!(nt_to_wsa(0xD000_0000), WSAEINVAL);
        assert_eq!(nt_to_wsa(0xC007_0001), WSAEINVAL);
        assert_eq!(nt_to_wsa(0x8000_0001), WSAEINVAL);
    }

    /// Every Winsock error survives the trip through an OVERLAPPED's status
    #[test]
    fn errors_round_trip() {
        for e in (10000..12000).chain([WSA_OPERATION_ABORTED]) {
            assert_eq!(nt_to_wsa(wsa_to_nt(e)), e, "{e}");
        }
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod issue88 {
    use super::*;
    use crate::test_guest::enter;
    #[test]
    fn overlapped_resets_only_its_event_and_masks_the_low_bit() {
        let _g = enter();
        for tag in [0, 1] {
            let ev = crate::obj::add(crate::obj::Obj::Event { manual: true, set: true });
            let p = mem::heap_alloc(32, true);
            mem::wp(p + 16, ev | tag);
            ov_start(p);
            assert!(crate::obj::with(|t| matches!(t.objs.get(&ev), Some((crate::obj::Obj::Event { set: false, .. }, _)))));
            mem::heap_free(p);
            assert!(crate::obj::close(ev));
        }
        ov_start(0);
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
pub(crate) fn test_bound_udp(f: impl FnOnce()) {
    with(|st, _| {
        for i in 0..3u32 {
            let mut sock = Sock::new(AF_INET, SOCK_DGRAM, 17);
            sock.local = Some(Addr::v4([127, 0, 0, 1], (41000 + i) as u16));
            st.socks.insert(0xffff0000 + i, sock);
        }
    });
    f();
    with(|st, _| { for i in 0..3 { st.socks.remove(&(0xffff0000 + i)); } });
}
