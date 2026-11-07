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

#[derive(Default)]
pub struct Kern {
    /// changes with every change anyone may wait for
    pub gen: u64,
    next: u32,
    ends: BTreeMap<u32, End>,
    pipes: BTreeMap<String, Pipe>,
}

/// A pipe path's name (lower case, without the `\\.\pipe\` part); None if it is not a pipe path.
pub fn pipe_name(path: &str) -> Option<String> {
    let p = path.replace('/', "\\").to_ascii_lowercase();
    ["\\\\.\\pipe\\", "\\\\?\\pipe\\", "\\??\\pipe\\", "\\device\\namedpipe\\"]
        .iter()
        .find_map(|pre| p.strip_prefix(pre))
        .map(str::to_string)
}

/// PIPE_UNLIMITED_INSTANCES
pub const UNLIMITED: u32 = 255;

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
    fn changed(&mut self) {
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
            let pipe = Pipe {
                max: p.max,
                instances: 0,
                sharing,
                msg_type: p.msg_type,
                outsize: p.outsize,
                insize: p.insize,
                timeout,
                listeners: Vec::new(),
            };
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
        let p = NewPipe {
            access: 1,
            msg_type: false,
            msg_read: false,
            nonblock: false,
            max: 1,
            outsize: size,
            insize: size,
            timeout: 0,
            first: true,
        };
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
        self.changed();
        Ok((out, more))
    }

    /// WriteFile: all of `data` (writes don't wait for the reader here).
    pub fn write(&mut self, e: u32, data: &[u8]) -> Result<u32, u32> {
        let end = self.end(e)?;
        match end.state {
            State::Connected => {}
            State::Disconnected => return Err(PIPE_DISCONNECTED),
            State::Listening => return Err(PIPE_LISTENING),
            State::Closing => return Err(PIPE_CLOSING),
        }
        if !end.msg_type && data.is_empty() {
            return Ok(0);
        }
        let c = end.conn.expect("a connected end");
        self.ends.get_mut(&c).unwrap().queue.push_back(Msg { data: data.to_vec(), pos: 0 });
        self.changed();
        Ok(data.len() as u32)
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
        Ok(Peek {
            data,
            avail: avail as u32,
            msg_len: msg_len as u32,
        })
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
        self.ends.get_mut(&c).unwrap().queue.push_back(Msg { data: data.to_vec(), pos: 0 });
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
        Ok(Info {
            server: end.server,
            msg_type: end.msg_type,
            outsize: end.outsize,
            insize: end.insize,
            max: end.max,
            instances,
            msg_read: end.msg_read,
            nonblock: end.nonblock,
        })
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

// ---------------------------------------------------------------------------------------------
// the one kernel, and waiting for it to change

static KERN: Mutex<Option<Kern>> = Mutex::new(None);
static CHANGED: Condvar = Condvar::new();

/// Runs `f` on the kernel; returns its result and the generation after it.
pub fn with<R>(f: impl FnOnce(&mut Kern) -> R) -> (R, u64) {
    let mut g = KERN.lock().unwrap_or_else(|e| e.into_inner());
    let k = g.get_or_insert_with(Kern::default);
    let before = k.gen;
    let r = f(k);
    let gen = k.gen;
    drop(g);
    if gen != before {
        CHANGED.notify_all();
    }
    (r, gen)
}

/// Wakes everyone waiting for a change (a cancelled wait, say).
pub fn poke() {
    with(|k| k.changed());
}

/// Waits until the generation is no longer `seen` or `until` passes; false on the time limit.
pub fn wait_change(seen: u64, until: Option<Instant>) -> bool {
    let mut g = KERN.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if g.as_ref().map_or(0, |k| k.gen) != seen {
            return true;
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

#[cfg(test)]
mod tests;
