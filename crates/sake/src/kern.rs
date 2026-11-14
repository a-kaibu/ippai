//! The part of the kernel more than one process can reach: pipes (named ones, and the anonymous
//! ones, which are named pipes with a made-up name as on Windows).
//!
//! Wine's model (server/named_pipe.c): each pipe end has a state (listening, connected,
//! disconnected, closing), the end it is connected to, and the messages waiting for it to read.
//! A server end is one instance of a named pipe; opening the name connects a new client end to
//! the first listening instance. Checked against Windows by tests/winref/probes/pipes.c.
//!
//! Pure: no guest memory and no handles. Operations return NTSTATUS codes; PENDING means "not
//! yet" (the caller waits for the generation to change, `wait_change`, and asks again).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

pub mod enc;
pub mod host;
pub mod services;
pub mod com;
pub mod debug;
use enc::Enc;

pub const SUCCESS: u32 = 0;
pub const PENDING: u32 = 0x103;
pub const BUFFER_OVERFLOW: u32 = 0x8000_0005;
pub const INVALID_HANDLE: u32 = 0xc000_0008;
pub const INVALID_PARAMETER: u32 = 0xc000_000d;
pub const ACCESS_DENIED: u32 = 0xc000_0022;
pub const OBJECT_NAME_INVALID: u32 = 0xc000_0033;
pub const OBJECT_NAME_NOT_FOUND: u32 = 0xc000_0034;
pub const INSTANCE_NOT_AVAILABLE: u32 = 0xc000_00ab;
pub const PIPE_NOT_AVAILABLE: u32 = 0xc000_00ac;
pub const INVALID_PIPE_STATE: u32 = 0xc000_00ad;
pub const PIPE_BUSY: u32 = 0xc000_00ae;
pub const ILLEGAL_FUNCTION: u32 = 0xc000_00af;
pub const PIPE_DISCONNECTED: u32 = 0xc000_00b0;
pub const PIPE_CLOSING: u32 = 0xc000_00b1;
pub const PIPE_CONNECTED: u32 = 0xc000_00b2;
pub const PIPE_LISTENING: u32 = 0xc000_00b3;
pub const INVALID_READ_MODE: u32 = 0xc000_00b4;
pub const IO_TIMEOUT: u32 = 0xc000_00b5;
pub const PIPE_EMPTY: u32 = 0xc000_00d9;
pub const CANCELLED: u32 = 0xc000_0120;
pub const QUOTA_EXCEEDED: u32 = 0xc000_0044;
pub const PIPE_BROKEN: u32 = 0xc000_014b;

/// The Win32 error for an NTSTATUS (RtlNtStatusToDosError, for the ones used here).
pub fn win32(status: u32) -> u32 {
    match status {
        SUCCESS => 0,
        PENDING => 997,
        BUFFER_OVERFLOW => 234,
        INVALID_HANDLE => 6,
        INVALID_PARAMETER => 87,
        ACCESS_DENIED => 5,
        OBJECT_NAME_INVALID => 123,
        OBJECT_NAME_NOT_FOUND => 2,
        INSTANCE_NOT_AVAILABLE | PIPE_NOT_AVAILABLE | PIPE_BUSY => 231,
        INVALID_PIPE_STATE | INVALID_READ_MODE => 230,
        ILLEGAL_FUNCTION => 1,
        PIPE_DISCONNECTED => 233,
        PIPE_CLOSING | PIPE_EMPTY => 232,
        PIPE_CONNECTED => 535,
        PIPE_LISTENING => 536,
        IO_TIMEOUT => 121,
        CANCELLED => 995,
        PIPE_BROKEN => 109,
        QUOTA_EXCEEDED => 1816,
        _ => 317,
    }
}

/// FILE_PIPE_*_STATE
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Disconnected = 1,
    Listening = 2,
    Connected = 3,
    Closing = 4,
}

struct Msg {
    data: Vec<u8>,
    pos: usize,
    /// the write it came from (its writer end's count): the write is done once the messages up
    /// to it fit the reader's buffer (End::done)
    id: u64,
}
impl Msg {
    fn left(&self) -> usize {
        self.data.len() - self.pos
    }
}

pub struct End {
    pub server: bool,
    /// the pipe's name; None for a client its server disconnected
    pub pipe: Option<String>,
    pub state: State,
    conn: Option<u32>,
    /// what was written to this end, for it to read
    queue: VecDeque<Msg>,
    /// the pipe's type and sizes (kept for queries after the pipe is gone)
    pub msg_type: bool,
    pub outsize: u32,
    pub insize: u32,
    pub max: u32,
    /// PIPE_READMODE_MESSAGE, PIPE_NOWAIT: this end's own modes
    pub msg_read: bool,
    pub nonblock: bool,
    /// handle objects of each process that refer to it: (process id, count)
    refs: Vec<(u32, u32)>,
    /// the writes to the connected end: the last one's id, and the last one that is done
    sent: u64,
    done: u64,
}

struct Pipe {
    max: u32,
    instances: u32,
    /// FILE_SHARE_READ (1): clients may read; FILE_SHARE_WRITE (2): clients may write
    sharing: u32,
    msg_type: bool,
    outsize: u32,
    insize: u32,
    /// WaitNamedPipe's default wait, ms
    timeout: u32,
    /// the server ends waiting for a client, oldest first
    listeners: Vec<u32>,
}

/// What PeekNamedPipe sees.
#[derive(Debug, PartialEq, Eq)]
pub struct Peek {
    pub data: Vec<u8>,
    pub avail: u32,
    /// what is left of the first message (message-type pipes; 0 for byte pipes)
    pub msg_len: u32,
}

/// What GetNamedPipeInfo and GetNamedPipeHandleState see.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Info {
    pub server: bool,
    pub msg_type: bool,
    pub outsize: u32,
    pub insize: u32,
    pub max: u32,
    /// current instances (None: a client whose server disconnected it)
    pub instances: Option<u32>,
    pub msg_read: bool,
    pub nonblock: bool,
}

/// Another process's object that a handle refers to: what a child inherits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KRef {
    /// a pipe end, and the handle's rights (read, write, overlapped)
    Pipe {
        end: u32,
        read: bool,
        write: bool,
        ovl: bool,
    },
    Process {
        pid: u32,
        access: u32,
    },
    /// a named event, mutex or semaphore
    Sync {
        id: u32,
    },
}

/// A named event, mutex or semaphore: these are the kernel's (other processes open them by
/// name); unnamed ones stay in their process (crate::obj).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sync {
    Event {
        manual: bool,
        set: bool,
    },
    /// owner: the thread holding it (0: nobody) and its process
    Mutex {
        owner: u32,
        pid: u32,
        count: u32,
        abandoned: bool,
    },
    Semaphore {
        count: i32,
        max: i32,
    },
}
impl Sync {
    /// the kind, for a name taken by another kind (crate::k32's numbers)
    pub fn kind(&self) -> u8 {
        match self {
            Sync::Event { .. } => 1,
            Sync::Mutex { .. } => 2,
            Sync::Semaphore { .. } => 3,
        }
    }
}

pub struct KObj {
    pub sync: Sync,
    name: String,
    refs: Vec<(u32, u32)>,
}

#[derive(Clone, Debug, Default)]
pub struct Proc {
    pub parent: u32,
    /// the program (Windows path) and its command line, as CreateProcess got it
    pub image: String,
    pub cmdline: String,
    pub cwd: String,
    /// the exit code, once it ended
    pub exit: Option<u32>,
    /// TerminateProcess's code, until it ends
    pub kill: Option<u32>,
    /// its first thread's suspend count (CREATE_SUSPENDED: 1 until ResumeThread)
    pub suspend: u32,
    /// the handles it starts with (inherited): (handle number, object)
    pub inherit: Vec<(u32, KRef)>,
    /// STARTF_USESTDHANDLES's input, output, error (else None)
    pub std: Option<(u32, u32, u32)>,
    /// SCM-launched process: the service's canonical name.
    pub service: Option<String>,
    /// its environment (CreateProcess's lpEnvironment, else its parent's), None: sake's own
    pub env: Option<Vec<(String, String)>>,
}

/// A top-level window of a process, for the others (FindWindow, messages to it)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KWin {
    pub hwnd: u32,
    pub pid: u32,
    pub tid: u32,
    /// its class's name in lower case, and its title
    pub class: String,
    pub title: String,
}

/// A message to another process's window: posted, or sent (`send`: the sender waits for
/// `reply`). WM_COPYDATA's data comes along: (dwData, the bytes).
#[derive(Clone, Debug, Default)]
pub struct XMsg {
    pub id: u32,
    pub to: u32,
    pub hwnd: u32,
    pub msg: u32,
    pub w: u32,
    pub l: u32,
    pub data: Option<(u64, Vec<u8>)>,
    pub send: bool,
}

/// The first process's id; the others are 0x10000 apart (each numbers its threads from its
/// own id up, so the ids of all the threads of all processes differ).
pub const ROOT_PID: u32 = 0x10;
pub const PID_STEP: u32 = 0x1_0000;

#[derive(Default)]
pub struct Kern {
    /// changes with every change anyone may wait for
    pub gen: u64,
    next: u32,
    ends: BTreeMap<u32, End>,
    pipes: BTreeMap<String, Pipe>,
    pub procs: BTreeMap<u32, Proc>,
    next_pid: u32,
    sync: BTreeMap<u32, KObj>,
    sync_names: BTreeMap<String, u32>,
    /// the host's list of processes that ended: how many of them are done here
    pub dead_seen: u32,
    wins: Vec<KWin>,
    xmsgs: Vec<XMsg>,
    replies: BTreeMap<u32, u32>,
    next_msg: u32,
    pub services: BTreeMap<String, services::Service>,
    pub com: com::Broker,
    /// another process's memory: what was asked of each process (crate::xmem), and the answers
    vm: Vec<VmReq>,
    vm_replies: BTreeMap<u32, VmReply>,
    /// the threads started for other processes (CreateRemoteThread): their exit codes once
    /// they ended
    rthreads: BTreeMap<u32, Option<u32>>,
    /// which process debugs which, and the debug events (crate::dbg)
    pub dbg: debug::Debugs,
}

/// A request for process `to`'s memory (crate::xmem's operation `op` on `args`, `data`)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VmReq {
    pub id: u32,
    pub to: u32,
    pub op: u32,
    pub args: Vec<u32>,
    pub data: Vec<u8>,
}

/// Its answer: `status` 0 or the error, values and bytes; GONE if the process ended first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VmReply {
    pub status: u32,
    pub vals: Vec<u32>,
    pub data: Vec<u8>,
}
pub const VM_GONE: u32 = u32::MAX;

/// A pipe path's name (lower case, without the `\\.\pipe\` part); None if it is not a pipe path.
pub fn pipe_name(path: &str) -> Option<String> {
    let p = path.replace('/', "\\").to_ascii_lowercase();
    ["\\\\.\\pipe\\", "\\\\?\\pipe\\", "\\??\\pipe\\", "\\device\\namedpipe\\"].iter().find_map(|pre| p.strip_prefix(pre)).map(str::to_string)
}

/// PIPE_UNLIMITED_INSTANCES
pub const UNLIMITED: u32 = 255;
/// What may wait in all pipes together (Kern::write)
pub const PIPE_QUOTA: usize = 8 << 20;
/// The least of a byte-mode write that goes into the reader's queue at once, whatever its buffer
const PIPE_PIECE: usize = 4096;

/// A WriteFile's progress (Kern::write): its bytes in the reader's queue so far, and its last
/// message's id there (0: nothing yet)
#[derive(Default, Clone, Copy, Debug)]
pub struct Write {
    queued: usize,
    id: u64,
}

pub struct NewPipe {
    /// PIPE_ACCESS_INBOUND (1: the server reads), OUTBOUND (2), DUPLEX (3)
    pub access: u32,
    pub msg_type: bool,
    pub msg_read: bool,
    pub nonblock: bool,
    pub max: u32,
    pub outsize: u32,
    pub insize: u32,
    pub timeout: u32,
    /// FILE_FLAG_FIRST_PIPE_INSTANCE
    pub first: bool,
}

impl Kern {
    pub(crate) fn changed(&mut self) {
        self.gen += 1;
    }
    fn new_end(&mut self, e: End) -> u32 {
        self.next += 1;
        let id = self.next;
        self.ends.insert(id, e);
        self.changed();
        id
    }
    fn end(&mut self, e: u32) -> Result<&mut End, u32> {
        self.ends.get_mut(&e).ok_or(INVALID_HANDLE)
    }

    /// CreateNamedPipe: a new server end (instance) of pipe `name`, listening.
    pub fn create(&mut self, name: &str, p: &NewPipe, pid: u32) -> Result<u32, u32> {
        if name.is_empty() {
            return Err(OBJECT_NAME_INVALID);
        }
        if p.max == 0 || p.max > UNLIMITED || p.access & 3 == 0 || (p.msg_read && !p.msg_type) {
            return Err(INVALID_PARAMETER);
        }
        // (the clients' sharing: an inbound pipe's clients write, an outbound one's read)
        let sharing = (p.access & 2 != 0) as u32 | ((p.access & 1 != 0) as u32) << 1;
        if let Some(pipe) = self.pipes.get(name) {
            if pipe.max != UNLIMITED && pipe.instances >= pipe.max {
                return Err(INSTANCE_NOT_AVAILABLE);
            }
            if pipe.sharing != sharing || p.first {
                return Err(ACCESS_DENIED);
            }
        } else {
            let timeout = if p.timeout == 0 { 50 } else { p.timeout };
            let pipe = Pipe { max: p.max, instances: 0, sharing, msg_type: p.msg_type, outsize: p.outsize, insize: p.insize, timeout, listeners: Vec::new() };
            self.pipes.insert(name.to_string(), pipe);
        }
        let pipe = &self.pipes[name];
        let e = End {
            server: true,
            pipe: Some(name.to_string()),
            state: State::Listening,
            conn: None,
            queue: VecDeque::new(),
            msg_type: pipe.msg_type,
            outsize: pipe.outsize,
            insize: pipe.insize,
            max: pipe.max,
            msg_read: p.msg_read,
            nonblock: p.nonblock,
            refs: vec![(pid, 1)],
            sent: 0,
            done: 0,
        };
        let id = self.new_end(e);
        let pipe = self.pipes.get_mut(name).unwrap();
        pipe.instances += 1;
        pipe.listeners.push(id);
        Ok(id)
    }

    /// Opening pipe `name` (CreateFile): a client end connected to its first listening instance.
    pub fn open(&mut self, name: &str, read: bool, write: bool, pid: u32) -> Result<u32, u32> {
        let Some(pipe) = self.pipes.get(name) else { return Err(OBJECT_NAME_NOT_FOUND) };
        let Some(&server) = pipe.listeners.first() else { return Err(PIPE_NOT_AVAILABLE) };
        if (read && pipe.sharing & 1 == 0) || (write && pipe.sharing & 2 == 0) {
            return Err(ACCESS_DENIED);
        }
        let e = End {
            server: false,
            pipe: Some(name.to_string()),
            state: State::Connected,
            conn: Some(server),
            queue: VecDeque::new(),
            msg_type: pipe.msg_type,
            outsize: pipe.outsize,
            insize: pipe.insize,
            max: pipe.max,
            msg_read: false,
            nonblock: false,
            refs: vec![(pid, 1)],
            sent: 0,
            done: 0,
        };
        let id = self.new_end(e);
        self.pipes.get_mut(name).unwrap().listeners.remove(0);
        let s = self.ends.get_mut(&server).unwrap();
        s.state = State::Connected;
        s.conn = Some(id);
        Ok(id)
    }

    /// CreatePipe: (read end, write end) of a new byte pipe of `size` bytes (0: 4096).
    pub fn create_anon(&mut self, size: u32, pid: u32) -> (u32, u32) {
        let size = if size == 0 { 4096 } else { size };
        // (Windows' own names for them)
        let name = format!("win32pipes.{pid:08x}.{:08x}", self.next + 1);
        let p = NewPipe { access: 1, msg_type: false, msg_read: false, nonblock: false, max: 1, outsize: size, insize: size, timeout: 0, first: true };
        let r = self.create(&name, &p, pid).expect("a new name");
        let w = self.open(&name, false, true, pid).expect("a listening pipe");
        (r, w)
    }

    /// Another handle object referring to end `e` (an inherited or duplicated handle in `pid`).
    pub fn add_ref(&mut self, e: u32, pid: u32) {
        if let Some(end) = self.ends.get_mut(&e) {
            match end.refs.iter_mut().find(|r| r.0 == pid) {
                Some(r) => r.1 += 1,
                None => end.refs.push((pid, 1)),
            }
        }
    }

    /// A handle object of `pid` referring to end `e` is gone; the end goes with its last one.
    pub fn release(&mut self, e: u32, pid: u32) {
        let Some(end) = self.ends.get_mut(&e) else { return };
        if let Some(i) = end.refs.iter().position(|r| r.0 == pid) {
            end.refs[i].1 -= 1;
            if end.refs[i].1 == 0 {
                end.refs.remove(i);
            }
        }
        if end.refs.is_empty() {
            self.destroy(e);
        }
    }

    /// Process `pid` ended: its references go.
    pub fn release_process(&mut self, pid: u32) {
        let ids: Vec<u32> = self.ends.iter().filter(|(_, e)| e.refs.iter().any(|r| r.0 == pid)).map(|(&i, _)| i).collect();
        for e in ids {
            let end = self.ends.get_mut(&e).unwrap();
            end.refs.retain(|r| r.0 != pid);
            if end.refs.is_empty() {
                self.destroy(e);
            }
        }
    }

    fn destroy(&mut self, e: u32) {
        self.disconnect_end(e, PIPE_BROKEN);
        let end = self.ends.remove(&e).unwrap();
        if end.server {
            if let Some(name) = &end.pipe {
                if let Some(p) = self.pipes.get_mut(name) {
                    p.instances -= 1;
                    p.listeners.retain(|&x| x != e);
                    if p.instances == 0 {
                        self.pipes.remove(name);
                    }
                }
            }
        }
        self.changed();
    }

    /// Wine's pipe_end_disconnect: `e` and the end it is connected to stop being connected;
    /// DISCONNECTED (DisconnectNamedPipe) drops what they had to read, BROKEN (an end closed)
    /// leaves it to read.
    fn disconnect_end(&mut self, e: u32, status: u32) {
        let Some(end) = self.ends.get_mut(&e) else { return };
        let conn = end.conn.take();
        if status == PIPE_DISCONNECTED {
            end.state = State::Disconnected;
            end.queue.clear();
        } else {
            end.state = State::Closing;
        }
        if let Some(c) = conn {
            if let Some(ce) = self.ends.get_mut(&c) {
                ce.conn = None;
            }
            self.disconnect_end(c, status);
        }
        self.changed();
    }

    /// ReadFile of up to `max` bytes: (data, whether more of the message is left).
    pub fn read(&mut self, e: u32, max: u32) -> Result<(Vec<u8>, bool), u32> {
        let end = self.end(e)?;
        match end.state {
            State::Connected if end.nonblock && end.queue.is_empty() => return Err(PIPE_EMPTY),
            State::Connected => {}
            State::Disconnected => return Err(PIPE_DISCONNECTED),
            State::Listening => return Err(PIPE_LISTENING),
            State::Closing if end.queue.is_empty() => return Err(PIPE_BROKEN),
            State::Closing => {}
        }
        if end.queue.is_empty() {
            return Err(PENDING);
        }
        let max = max as usize;
        let mut out = Vec::new();
        let mut more = false;
        if end.msg_read {
            let m = end.queue.front_mut().unwrap();
            let n = max.min(m.left());
            out.extend_from_slice(&m.data[m.pos..m.pos + n]);
            m.pos += n;
            if m.left() == 0 {
                end.queue.pop_front();
            } else {
                more = true;
            }
        } else {
            while let Some(m) = end.queue.front_mut() {
                let n = (max - out.len()).min(m.left());
                out.extend_from_slice(&m.data[m.pos..m.pos + n]);
                m.pos += n;
                if m.left() == 0 {
                    end.queue.pop_front();
                }
                if out.len() == max {
                    break;
                }
            }
        }
        self.settle(e);
        self.changed();
        Ok((out, more))
    }

    /// The buffer of reader end `e`: what may wait for it before its writer waits (Wine's
    /// buffer_size: the server's in size, a client's out size).
    fn room(end: &End) -> usize {
        (if end.server { end.insize } else { end.outsize }) as usize
    }

    /// The bytes waiting in every pipe (bounded by PIPE_QUOTA).
    fn queued(&self) -> usize {
        self.ends.values().map(|e| e.queue.iter().map(Msg::left).sum::<usize>()).sum()
    }

    /// Reader end `e`'s queue changed: the writes of its writer that now fit its buffer are done
    /// (Wine's reselect_write_queue: a message is done once what is left of it and of the ones
    /// before it fits, or it was read; an empty one is done when it is written).
    fn settle(&mut self, e: u32) {
        let Some(end) = self.ends.get(&e) else { return };
        let room = Self::room(end);
        let mut done = 0;
        let mut avail = 0;
        for m in &end.queue {
            avail += m.left();
            if avail > room {
                break;
            }
            done = m.id;
        }
        // (and everything before the first message still there: read)
        let first = end.queue.front().map(|m| m.id.saturating_sub(1));
        let Some(w) = end.conn else { return };
        if let Some(we) = self.ends.get_mut(&w) {
            let read = first.unwrap_or(we.sent);
            we.done = we.done.max(done).max(read);
        }
    }

    /// WriteFile of `data`: Ok(bytes) once it is done, PENDING while it waits for the reader
    /// to make room, as on Windows (the reader's buffer, `room`). `w` is the write's own state:
    /// ask again with the same `w` until it is no longer PENDING.
    ///
    /// A byte-mode write goes into the reader's queue a buffer's worth at a time; a message goes
    /// whole and the write waits until it fits. A PIPE_NOWAIT end's writes are done at once.
    /// What waits in all pipes together is at most PIPE_QUOTA (beyond: QUOTA_EXCEEDED), so the
    /// kernel's state (the processes' shared buffer) stays small.
    pub fn write(&mut self, e: u32, data: &[u8], w: &mut Write) -> Result<u32, u32> {
        let end = self.end(e)?;
        if w.queued == 0 && w.id == 0 {
            match end.state {
                State::Connected => {}
                State::Disconnected => return Err(PIPE_DISCONNECTED),
                State::Listening => return Err(PIPE_LISTENING),
                State::Closing => return Err(PIPE_CLOSING),
            }
            if !end.msg_type && data.is_empty() {
                return Ok(0);
            }
        }
        let (msg_type, nonblock, conn, state) = (end.msg_type, end.nonblock, end.conn, end.state);
        if w.queued < data.len() || w.id == 0 {
            let Some(c) = conn.filter(|_| state == State::Connected) else { return Err(PIPE_BROKEN) };
            let reader = &self.ends[&c];
            let room = Self::room(reader).max(PIPE_PIECE);
            let avail: usize = reader.queue.iter().map(Msg::left).sum();
            let whole = msg_type || nonblock;
            let n = if whole {
                data.len()
            } else {
                // (a buffer of 0 bytes still passes PIPE_PIECE at a time: the write then waits
                // for it to be read)
                (data.len() - w.queued).min(room.saturating_sub(avail)).min(PIPE_QUOTA / 8)
            };
            if n == 0 && !whole {
                return Err(PENDING);
            }
            if self.queued() + n > PIPE_QUOTA {
                if w.id == 0 {
                    return Err(QUOTA_EXCEEDED);
                }
                return Err(PENDING);
            }
            let end = self.ends.get_mut(&e).unwrap();
            end.sent += 1;
            w.id = end.sent;
            let piece = data[w.queued..w.queued + n].to_vec();
            w.queued += n;
            self.ends.get_mut(&c).unwrap().queue.push_back(Msg { data: piece, pos: 0, id: w.id });
            self.settle(c);
            self.changed();
            // (an empty message is done at once, wherever it is in the queue)
            if nonblock || data.is_empty() {
                return Ok(data.len() as u32);
            }
        }
        let end = &self.ends[&e];
        if end.done >= w.id && w.queued == data.len() {
            return Ok(data.len() as u32);
        }
        if end.done >= w.id {
            // (that piece fits: the next one goes in)
            return self.write(e, data, w);
        }
        if end.state != State::Connected {
            return Err(PIPE_BROKEN);
        }
        Err(PENDING)
    }

    /// PeekNamedPipe of up to `max` bytes.
    pub fn peek(&mut self, e: u32, max: u32) -> Result<Peek, u32> {
        let end = self.end(e)?;
        match end.state {
            State::Connected => {}
            State::Closing if !end.queue.is_empty() => {}
            State::Closing => return Err(PIPE_BROKEN),
            _ => return Err(if end.pipe.is_some() { INVALID_PIPE_STATE } else { PIPE_DISCONNECTED }),
        }
        let avail: usize = end.queue.iter().map(Msg::left).sum();
        let mut n = (max as usize).min(avail);
        let mut msg_len = 0;
        if avail != 0 && end.msg_type {
            msg_len = end.queue[0].left();
            n = n.min(msg_len);
        }
        let mut data = Vec::with_capacity(n);
        for m in &end.queue {
            let k = (n - data.len()).min(m.left());
            data.extend_from_slice(&m.data[m.pos..m.pos + k]);
            if data.len() == n {
                break;
            }
        }
        Ok(Peek { data, avail: avail as u32, msg_len: msg_len as u32 })
    }

    /// ConnectNamedPipe's start: PENDING while waiting for a client (`connected` then says when
    /// one came).
    pub fn listen(&mut self, e: u32) -> u32 {
        let Ok(end) = self.end(e) else { return INVALID_HANDLE };
        if !end.server {
            return ILLEGAL_FUNCTION;
        }
        match end.state {
            State::Listening => {}
            State::Disconnected => {
                end.state = State::Listening;
                let name = end.pipe.clone();
                if let Some(p) = name.and_then(|n| self.pipes.get_mut(&n)) {
                    p.listeners.push(e);
                }
                self.changed();
            }
            State::Connected => return PIPE_CONNECTED,
            State::Closing => return PIPE_CLOSING,
        }
        if self.ends[&e].nonblock {
            PIPE_LISTENING
        } else {
            PENDING
        }
    }
    /// Whether the ConnectNamedPipe that `listen` started is done.
    pub fn connected(&mut self, e: u32) -> u32 {
        match self.end(e).map(|x| x.state) {
            Ok(State::Connected | State::Closing) => SUCCESS,
            Ok(State::Listening) => PENDING,
            Ok(State::Disconnected) => PIPE_DISCONNECTED,
            Err(s) => s,
        }
    }

    /// DisconnectNamedPipe
    pub fn disconnect(&mut self, e: u32) -> u32 {
        let Ok(end) = self.end(e) else { return INVALID_HANDLE };
        if !end.server {
            return ILLEGAL_FUNCTION;
        }
        match end.state {
            State::Connected => {
                // (the client is cut off from the pipe: everything it does fails from now on)
                let c = end.conn.unwrap();
                self.ends.get_mut(&c).unwrap().pipe = None;
            }
            State::Closing => {}
            State::Listening => return PIPE_LISTENING,
            State::Disconnected => return PIPE_DISCONNECTED,
        }
        self.disconnect_end(e, PIPE_DISCONNECTED);
        SUCCESS
    }

    /// TransactNamedPipe's write (its read is an ordinary message read after it).
    pub fn transact(&mut self, e: u32, data: &[u8]) -> u32 {
        let Ok(end) = self.end(e) else { return INVALID_HANDLE };
        let Some(c) = end.conn else {
            return if end.pipe.is_some() { INVALID_PIPE_STATE } else { PIPE_DISCONNECTED };
        };
        if !end.msg_read {
            return INVALID_READ_MODE;
        }
        if !end.queue.is_empty() {
            return PIPE_BUSY;
        }
        if self.queued() + data.len() > PIPE_QUOTA {
            return QUOTA_EXCEEDED;
        }
        let end = self.ends.get_mut(&e).unwrap();
        end.sent += 1;
        let id = end.sent;
        self.ends.get_mut(&c).unwrap().queue.push_back(Msg { data: data.to_vec(), pos: 0, id });
        self.settle(c);
        self.changed();
        SUCCESS
    }

    /// SetNamedPipeHandleState's mode (None: unchanged)
    pub fn set_mode(&mut self, e: u32, msg_read: Option<bool>, nonblock: Option<bool>) -> u32 {
        let Ok(end) = self.end(e) else { return INVALID_HANDLE };
        if msg_read == Some(true) && !end.msg_type {
            return INVALID_PARAMETER;
        }
        if let Some(m) = msg_read {
            end.msg_read = m;
        }
        if let Some(n) = nonblock {
            end.nonblock = n;
        }
        self.changed();
        SUCCESS
    }

    pub fn info(&self, e: u32) -> Result<Info, u32> {
        let end = self.ends.get(&e).ok_or(INVALID_HANDLE)?;
        let instances = end.pipe.as_ref().map(|n| self.pipes.get(n).map_or(0, |p| p.instances));
        Ok(Info { server: end.server, msg_type: end.msg_type, outsize: end.outsize, insize: end.insize, max: end.max, instances, msg_read: end.msg_read, nonblock: end.nonblock })
    }

    /// The bytes waiting for end `e` to read (GetFileSize of a pipe).
    pub fn avail(&self, e: u32) -> u32 {
        self.ends.get(&e).map_or(0, |end| end.queue.iter().map(Msg::left).sum::<usize>() as u32)
    }

    /// WaitNamedPipe: Ok(None) when an instance listens, Ok(Some(default wait ms)) while none
    /// does.
    pub fn wait_named(&self, name: &str) -> Result<Option<u32>, u32> {
        let p = self.pipes.get(name).ok_or(OBJECT_NAME_NOT_FOUND)?;
        Ok(if p.listeners.is_empty() { Some(p.timeout) } else { None })
    }
}

impl Kern {
    /// CreateProcess: a new process's record; its id.
    pub fn new_process(&mut self, p: Proc) -> u32 {
        self.next_pid += 1;
        let pid = ROOT_PID + PID_STEP * self.next_pid;
        for (_, r) in &p.inherit {
            match *r {
                KRef::Pipe { end, .. } => self.add_ref(end, pid),
                KRef::Sync { id } => self.add_sync_ref(id, pid),
                KRef::Process { .. } => {}
            }
        }
        self.procs.insert(pid, p);
        self.changed();
        pid
    }
    /// The record of `pid` (the first process has none until it starts another).
    pub fn proc(&self, pid: u32) -> Option<&Proc> {
        self.procs.get(&pid)
    }
    /// Whether `pid` is a process (running or ended) this kernel knows.
    pub fn known(&self, pid: u32) -> bool {
        pid == ROOT_PID || self.procs.contains_key(&pid)
    }
    /// The exit code of `pid` once it ended.
    pub fn exit_code(&self, pid: u32) -> Option<u32> {
        self.procs.get(&pid).and_then(|p| p.exit)
    }
    /// ResumeThread of a process's first thread: the suspend count before.
    pub fn resume(&mut self, pid: u32) -> u32 {
        let Some(p) = self.procs.get_mut(&pid) else { return 0 };
        let n = p.suspend;
        p.suspend = n.saturating_sub(1);
        self.changed();
        n
    }
    /// TerminateProcess of another process: it ends with `code` (it notices and exits).
    pub fn terminate(&mut self, pid: u32, code: u32) -> bool {
        let p = self.procs.entry(pid).or_default();
        if p.exit.is_none() && p.kill.is_none() {
            p.kill = Some(code);
            self.changed();
        }
        true
    }
    /// Process `pid` ended with `code` (TerminateProcess's, if it was terminated): its handles
    /// close.
    pub fn exited(&mut self, pid: u32, code: u32) {
        let p = self.procs.entry(pid).or_default();
        if p.exit.is_some() {
            return;
        }
        p.exit = Some(p.kill.unwrap_or(code));
        for s in self.services.values_mut() { s.exited(pid); }
        self.services.retain(|_, s| !s.removable());
        self.com.exited(pid);
        // (a debugger's debuggees end with it unless it said otherwise)
        let code = self.procs[&pid].exit.unwrap_or(code);
        for p in self.dbg.exited(pid, pid + 0x10, code) {
            self.terminate(p, debug::STATUS_DEBUGGER_INACTIVE);
        }
        self.release_process(pid);
        // (its threads' mutexes are abandoned, its handles of named objects close)
        for o in self.sync.values_mut() {
            if let Sync::Mutex { owner, pid: op, count, abandoned } = &mut o.sync {
                if *owner != 0 && *op == pid {
                    *owner = 0;
                    *count = 0;
                    *abandoned = true;
                }
            }
        }
        // (its windows go; what was sent to them is answered 0)
        self.wins.retain(|w| w.pid != pid);
        let asked: Vec<u32> = self.vm.iter().filter(|r| r.to == pid).map(|r| r.id).collect();
        self.vm.retain(|r| r.to != pid);
        for id in asked {
            self.vm_replies.insert(id, VmReply { status: VM_GONE, ..Default::default() });
        }
        let gone: Vec<u32> = self.xmsgs.iter().filter(|x| x.to == pid && x.send).map(|x| x.id).collect();
        self.xmsgs.retain(|x| x.to != pid);
        for id in gone {
            self.replies.insert(id, 0);
        }
        let ids: Vec<u32> = self.sync.iter().filter(|(_, o)| o.refs.iter().any(|r| r.0 == pid)).map(|(&i, _)| i).collect();
        for id in ids {
            self.sync.get_mut(&id).unwrap().refs.retain(|r| r.0 != pid);
            self.drop_sync_if_unused(id);
        }
        self.changed();
    }
}

impl Kern {
    /// The top-level windows of process `pid` are now `list`.
    pub fn set_wins(&mut self, pid: u32, list: Vec<KWin>) {
        let old: Vec<&KWin> = self.wins.iter().filter(|w| w.pid == pid).collect();
        if old.len() == list.len() && old.iter().zip(&list).all(|(a, b)| *a == b) {
            return;
        }
        self.wins.retain(|w| w.pid != pid);
        self.wins.extend(list);
        self.changed();
    }
    /// FindWindow among the windows of the processes other than `me` (class in lower case).
    pub fn find_win(&self, me: u32, class: Option<&str>, title: Option<&str>) -> Option<&KWin> {
        self.wins.iter().find(|w| w.pid != me && class.is_none_or(|c| w.class == c) && title.is_none_or(|t| w.title == t))
    }
    pub fn win(&self, hwnd: u32) -> Option<&KWin> {
        self.wins.iter().find(|w| w.hwnd == hwnd)
    }
    /// Posts or sends `m` to process `m.to`: the message's id (what `reply` answers).
    pub fn put_msg(&mut self, mut m: XMsg) -> u32 {
        self.next_msg += 1;
        m.id = self.next_msg;
        if self.exit_code(m.to).is_some() {
            self.replies.insert(m.id, 0);
        } else {
            self.xmsgs.push(m);
        }
        self.changed();
        self.next_msg
    }
    /// Asks process `r.to` for something of its memory: the request's id (what `take_vm_reply`
    /// answers). A process that ended answers VM_GONE.
    pub fn put_vm(&mut self, mut r: VmReq) -> u32 {
        self.next_msg += 1;
        r.id = self.next_msg;
        if self.exit_code(r.to).is_some() || !self.procs.contains_key(&r.to) {
            self.vm_replies.insert(r.id, VmReply { status: VM_GONE, ..Default::default() });
        } else {
            self.vm.push(r);
        }
        self.changed();
        self.next_msg
    }
    /// The memory requests for process `pid`, in order.
    pub fn take_vm(&mut self, pid: u32) -> Vec<VmReq> {
        if !self.vm.iter().any(|r| r.to == pid) {
            return Vec::new();
        }
        let (mine, rest) = std::mem::take(&mut self.vm).into_iter().partition(|r| r.to == pid);
        self.vm = rest;
        self.changed();
        mine
    }
    pub fn vm_reply(&mut self, id: u32, r: VmReply) {
        self.vm_replies.insert(id, r);
        self.changed();
    }
    /// Request `id`'s answer once there is one (PENDING until then).
    pub fn take_vm_reply(&mut self, id: u32) -> Result<VmReply, u32> {
        let r = self.vm_replies.remove(&id).ok_or(PENDING)?;
        self.changed();
        Ok(r)
    }
    /// A thread started for another process (CreateRemoteThread) runs.
    pub fn thread_started(&mut self, tid: u32) {
        self.rthreads.insert(tid, None);
        self.changed();
    }
    /// A thread ended (one another process may wait for: else nothing).
    pub fn thread_ended(&mut self, tid: u32, code: u32) {
        if let Some(e @ None) = self.rthreads.get_mut(&tid) {
            *e = Some(code);
            self.changed();
        }
    }
    /// Thread `tid` of process `pid`: its exit code once it ended. The first thread ends with
    /// its process; any thread of a process that ended has.
    pub fn thread_exit(&self, pid: u32, tid: u32) -> Option<u32> {
        let proc_exit = self.exit_code(pid);
        match self.rthreads.get(&tid) {
            Some(Some(code)) => Some(*code),
            _ => proc_exit,
        }
    }
    /// The messages that came for process `pid`, in order.
    pub fn take_msgs(&mut self, pid: u32) -> Vec<XMsg> {
        if !self.xmsgs.iter().any(|x| x.to == pid) {
            return Vec::new();
        }
        let (mine, rest) = std::mem::take(&mut self.xmsgs).into_iter().partition(|x| x.to == pid);
        self.xmsgs = rest;
        self.changed();
        mine
    }
    /// A sent message's result.
    pub fn reply(&mut self, id: u32, r: u32) {
        self.replies.insert(id, r);
        self.changed();
    }
    /// The result of sent message `id` once there is one (PENDING until then).
    pub fn take_reply(&mut self, id: u32) -> Result<u32, u32> {
        let r = self.replies.remove(&id).ok_or(PENDING)?;
        self.changed();
        Ok(r)
    }
}

impl Kern {
    /// CreateEvent / CreateMutex / CreateSemaphore with a name: (id, whether it was there
    /// already); Err(kind of the object that has the name) for another kind.
    pub fn create_sync(&mut self, name: &str, s: Sync, pid: u32) -> Result<(u32, bool), u8> {
        if let Some(&id) = self.sync_names.get(name) {
            let o = self.sync.get_mut(&id).unwrap();
            if o.sync.kind() != s.kind() {
                return Err(o.sync.kind());
            }
            add_ref(&mut o.refs, pid);
            self.changed();
            return Ok((id, true));
        }
        self.next += 1;
        let id = self.next;
        self.sync.insert(id, KObj { sync: s, name: name.to_string(), refs: vec![(pid, 1)] });
        self.sync_names.insert(name.to_string(), id);
        self.changed();
        Ok((id, false))
    }
    /// The kind of the named object `name`, if there is one.
    /// A named sync object's state (NtQueryEvent / NtQueryMutant / NtQuerySemaphore)
    pub fn sync_state(&self, id: u32) -> Option<Sync> {
        self.sync.get(&id).map(|o| o.sync.clone())
    }
    pub fn sync_kind(&self, name: &str) -> Option<u8> {
        self.sync_names.get(name).map(|id| self.sync[id].sync.kind())
    }
    /// OpenEvent / OpenMutex / OpenSemaphore: Err(ERROR_FILE_NOT_FOUND, ERROR_INVALID_HANDLE)
    pub fn open_sync(&mut self, name: &str, kind: u8, pid: u32) -> Result<u32, u32> {
        let Some(&id) = self.sync_names.get(name) else { return Err(2) };
        let o = self.sync.get_mut(&id).unwrap();
        if o.sync.kind() != kind {
            return Err(6);
        }
        add_ref(&mut o.refs, pid);
        self.changed();
        Ok(id)
    }
    /// Another handle object of `pid` for named object `id` (inherited, duplicated)
    pub fn add_sync_ref(&mut self, id: u32, pid: u32) {
        if let Some(o) = self.sync.get_mut(&id) {
            add_ref(&mut o.refs, pid);
            self.changed();
        }
    }
    pub fn release_sync(&mut self, id: u32, pid: u32) {
        let Some(o) = self.sync.get_mut(&id) else { return };
        if let Some(i) = o.refs.iter().position(|r| r.0 == pid) {
            o.refs[i].1 -= 1;
            if o.refs[i].1 == 0 {
                o.refs.remove(i);
            }
        }
        self.drop_sync_if_unused(id);
        self.changed();
    }
    fn drop_sync_if_unused(&mut self, id: u32) {
        if self.sync.get(&id).is_some_and(|o| o.refs.is_empty()) {
            let o = self.sync.remove(&id).unwrap();
            self.sync_names.remove(&o.name);
        }
    }
    /// A wait's test of named object `id` for thread `tid` of `pid`: (signaled, abandoned);
    /// `take` takes it (an auto-reset event, a mutex, a semaphore's count).
    pub fn try_take(&mut self, id: u32, pid: u32, tid: u32, take: bool) -> Option<(bool, bool)> {
        let o = self.sync.get_mut(&id)?;
        let r = match &mut o.sync {
            Sync::Event { manual, set } => {
                let s = *set;
                if s && take && !*manual {
                    *set = false;
                }
                (s, false)
            }
            Sync::Mutex { owner, pid: op, count, abandoned } => {
                if *owner == 0 || *owner == tid {
                    let ab = *abandoned;
                    if take {
                        *owner = tid;
                        *op = pid;
                        *count += 1;
                        *abandoned = false;
                    }
                    (true, ab)
                } else {
                    (false, false)
                }
            }
            Sync::Semaphore { count, .. } => {
                let s = *count > 0;
                if s && take {
                    *count -= 1;
                }
                (s, false)
            }
        };
        if take && r.0 {
            self.changed();
        }
        Some(r)
    }
    /// SetEvent / ResetEvent; false if `id` is not an event.
    pub fn set_event(&mut self, id: u32, v: bool) -> bool {
        match self.sync.get_mut(&id).map(|o| &mut o.sync) {
            Some(Sync::Event { set, .. }) => {
                *set = v;
                self.changed();
                true
            }
            _ => false,
        }
    }
    /// ReleaseMutex by thread `tid`: Err(ERROR_NOT_OWNER, ERROR_INVALID_HANDLE)
    pub fn release_mutex(&mut self, id: u32, tid: u32) -> Result<(), u32> {
        match self.sync.get_mut(&id).map(|o| &mut o.sync) {
            Some(Sync::Mutex { owner, count, .. }) if *owner == tid && tid != 0 => {
                *count -= 1;
                if *count == 0 {
                    *owner = 0;
                }
                self.changed();
                Ok(())
            }
            Some(Sync::Mutex { .. }) => Err(288),
            _ => Err(6),
        }
    }
    /// ReleaseSemaphore: the count before, or Err(ERROR_TOO_MANY_POSTS, ERROR_INVALID_HANDLE)
    pub fn release_semaphore(&mut self, id: u32, n: i32) -> Result<i32, u32> {
        match self.sync.get_mut(&id).map(|o| &mut o.sync) {
            Some(Sync::Semaphore { count, max }) => {
                let prev = *count;
                if *count as i64 + n as i64 > *max as i64 {
                    return Err(298);
                }
                *count += n;
                self.changed();
                Ok(prev)
            }
            _ => Err(6),
        }
    }
    /// Thread `tid` ended: the mutexes it holds are abandoned.
    pub fn abandon(&mut self, tid: u32) {
        let mut any = false;
        for o in self.sync.values_mut() {
            if let Sync::Mutex { owner, count, abandoned, .. } = &mut o.sync {
                if *owner == tid {
                    *owner = 0;
                    *count = 0;
                    *abandoned = true;
                    any = true;
                }
            }
        }
        if any {
            self.changed();
        }
    }
}

fn add_ref(refs: &mut Vec<(u32, u32)>, pid: u32) {
    match refs.iter_mut().find(|r| r.0 == pid) {
        Some(r) => r.1 += 1,
        None => refs.push((pid, 1)),
    }
}

// ---------------------------------------------------------------------------------------------
// the one kernel, and waiting for it to change
//
// A process alone keeps the kernel to itself. Once it starts another (share), the host's
// processes keep it in the host's shared buffer (kern::host), each with a copy it reads again
// when another process changed it and writes back when it changed it.

#[derive(Default)]
struct Local {
    k: Kern,
    /// the shared buffer's generation this copy is (None: not read yet)
    loaded: Option<u32>,
}
static KERN: Mutex<Option<Local>> = Mutex::new(None);
static CHANGED: Condvar = Condvar::new();

fn local() -> std::sync::MutexGuard<'static, Option<Local>> {
    KERN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `f` on the kernel; returns its result and the generation after it (what
/// `wait_change` waits on: the shared buffer's once it is shared).
pub fn with<R>(f: impl FnOnce(&mut Kern) -> R) -> (R, u64) {
    let mut g = local();
    let l = g.get_or_insert_with(Local::default);
    let shared = host::shared();
    let mut now = 0;
    if shared {
        host::lock();
        now = host::gen();
        if l.loaded != Some(now) {
            if let Some(k) = host::load() {
                l.k = k;
            }
            l.loaded = Some(now);
        }
    }
    let before = l.k.gen;
    if shared {
        // (the processes that ended since: the host lists them)
        while let Some((pid, code)) = host::dead(l.k.dead_seen) {
            l.k.dead_seen += 1;
            l.k.exited(pid, code);
        }
    }
    let r = f(&mut l.k);
    let changed = l.k.gen != before;
    let gen = if shared {
        if changed {
            if host::store(&l.k) {
                now = host::bump();
                l.loaded = Some(now);
            } else {
                // (the others keep the state as it was, and so does this process: read again)
                l.loaded = None;
                if let Some(k) = host::load() {
                    l.k = k;
                }
            }
        }
        host::unlock();
        now as u64
    } else {
        l.k.gen
    };
    drop(g);
    if changed {
        CHANGED.notify_all();
        if shared {
            host::notify();
        }
    }
    (r, gen)
}

/// Shares the kernel with the processes this one starts: false if the host can't run another
/// process.
pub fn share() -> bool {
    // (the registry's changes reach its file before another process reads it)
    crate::misc::reg_flush();
    let mut g = local();
    if host::shared() {
        return true;
    }
    if !host::can_share() {
        return false;
    }
    let l = g.get_or_insert_with(Local::default);
    l.k.procs.entry(crate::pid()).or_default();
    l.k.changed();
    host::lock();
    if !host::store(&l.k) {
        host::unlock();
        return false;
    }
    l.loaded = Some(host::bump());
    host::start_sharing();
    host::unlock();
    drop(g);
    CHANGED.notify_all();
    true
}

/// Wakes everyone waiting for a change (a cancelled wait, say).
pub fn poke() {
    with(|k| k.changed());
}

/// Waits until the generation is no longer `seen` or `until` passes; false on the time limit.
pub fn wait_change(seen: u64, until: Option<Instant>) -> bool {
    if host::shared() {
        loop {
            if host::gen() != seen as u32 {
                return true;
            }
            let ms = match until {
                None => f64::INFINITY,
                Some(t) => {
                    let now = Instant::now();
                    if now >= t {
                        return false;
                    }
                    (t - now).as_secs_f64() * 1000.0
                }
            };
            host::wait(seen as u32, ms);
        }
    }
    let mut g = local();
    loop {
        if g.as_ref().map_or(0, |l| l.k.gen) != seen {
            return true;
        }
        // (sharing began meanwhile: wait the shared way)
        if host::shared() {
            drop(g);
            return wait_change(seen, until);
        }
        match until {
            None => g = CHANGED.wait(g).unwrap_or_else(|e| e.into_inner()),
            Some(t) => {
                let now = Instant::now();
                if now >= t {
                    return false;
                }
                g = CHANGED.wait_timeout(g, t - now).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }
}

/// Runs `f` until it is no longer PENDING: Err(IO_TIMEOUT) after `until`, Err(CANCELLED) once
/// `cancelled` says so.
pub fn block<T>(until: Option<Instant>, cancelled: &dyn Fn() -> bool, mut f: impl FnMut(&mut Kern) -> Result<T, u32>) -> Result<T, u32> {
    loop {
        let (r, gen) = with(&mut f);
        match r {
            Err(PENDING) => {
                if cancelled() {
                    return Err(CANCELLED);
                }
                if !wait_change(gen, until) {
                    return Err(IO_TIMEOUT);
                }
            }
            r => return r,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// as bytes

impl Enc for State {
    fn put(&self, w: &mut Vec<u8>) {
        (*self as u8).put(w)
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(match u8::get(r)? {
            1 => State::Disconnected,
            2 => State::Listening,
            3 => State::Connected,
            4 => State::Closing,
            _ => return None,
        })
    }
}
impl Enc for KRef {
    fn put(&self, w: &mut Vec<u8>) {
        match *self {
            KRef::Pipe { end, read, write, ovl } => {
                0u8.put(w);
                (end, (read, (write, ovl))).put(w)
            }
            KRef::Process { pid, access } => {
                1u8.put(w);
                (pid, access).put(w)
            }
            KRef::Sync { id } => {
                2u8.put(w);
                id.put(w)
            }
        }
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(match u8::get(r)? {
            0 => {
                let (end, (read, (write, ovl))) = Enc::get(r)?;
                KRef::Pipe { end, read, write, ovl }
            }
            1 => {
                let (pid, access) = Enc::get(r)?;
                KRef::Process { pid, access }
            }
            2 => KRef::Sync { id: Enc::get(r)? },
            _ => return None,
        })
    }
}
impl Enc for (u32, u32, u32) {
    fn put(&self, w: &mut Vec<u8>) {
        (self.0, (self.1, self.2)).put(w)
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        let (a, (b, c)) = Enc::get(r)?;
        Some((a, b, c))
    }
}
crate::enc_struct!(Msg { data, pos, id });
crate::enc_struct!(End { server, pipe, state, conn, queue, msg_type, outsize, insize, max, msg_read, nonblock, refs, sent, done });
crate::enc_struct!(Pipe { max, instances, sharing, msg_type, outsize, insize, timeout, listeners });
crate::enc_struct!(Proc { parent, image, cmdline, cwd, exit, kill, suspend, inherit, std, service, env });
impl Enc for Sync {
    fn put(&self, w: &mut Vec<u8>) {
        match *self {
            Sync::Event { manual, set } => (0u8, (manual, set)).put(w),
            Sync::Mutex { owner, pid, count, abandoned } => (1u8, ((owner, pid), (count, abandoned))).put(w),
            Sync::Semaphore { count, max } => (2u8, (count, max)).put(w),
        }
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(match u8::get(r)? {
            0 => {
                let (manual, set) = Enc::get(r)?;
                Sync::Event { manual, set }
            }
            1 => {
                let ((owner, pid), (count, abandoned)) = Enc::get(r)?;
                Sync::Mutex { owner, pid, count, abandoned }
            }
            2 => {
                let (count, max) = Enc::get(r)?;
                Sync::Semaphore { count, max }
            }
            _ => return None,
        })
    }
}
crate::enc_struct!(KObj { sync, name, refs });
crate::enc_struct!(KWin { hwnd, pid, tid, class, title });
crate::enc_struct!(XMsg { id, to, hwnd, msg, w, l, data, send });
crate::enc_struct!(VmReq { id, to, op, args, data });
crate::enc_struct!(VmReply { status, vals, data });
crate::enc_struct!(Kern { gen, next, ends, pipes, procs, next_pid, sync, sync_names, dead_seen, wins, xmsgs, replies, next_msg, services, com, vm, vm_replies, rthreads, dbg });

#[cfg(test)]
mod tests;
