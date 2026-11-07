#![allow(non_snake_case)]
//! advapi32's event log (the classic API) kept in memory: Open/Close/RegisterEventSource,
//! ReportEvent, ReadEventLog, GetNumberOfEventLogRecords, GetOldestEventLogRecord,
//! GetEventLogInformation, ClearEventLog, BackupEventLog/OpenBackupEventLog (.evt files), and
//! the ETW session calls StartTrace/ControlTrace/StopTrace.
//!
//! Behaviour follows Wine's advapi32 eventlog tests (Windows' answers):
//! - a log is a key under HKLM\SYSTEM\CurrentControlSet\Services\Eventlog; a name opens that
//!   log, or the log that has it as a source subkey, or else Application;
//! - each handle has its own read position, between two records: a forwards read takes the
//!   records after it, a backwards one those before; a seek read starts at a record number and
//!   leaves the position past (forwards) or before (backwards) the last record it read; reads
//!   fill the buffer with whole records;
//! - record numbers keep counting after a clear (Vista SP1 and later);
//! - EVENTLOGRECORD: header, source, computer, padding to 8, SID, strings, data, padding to 4,
//!   the length again.

use crate::api::{implement, Ctx};
use crate::k32::{arg_str, last_error};
use crate::mem;
use crate::HashMap;
use std::sync::Mutex;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_CRC: u32 = 23;
const ERROR_BAD_LENGTH: u32 = 24;
const ERROR_HANDLE_EOF: u32 = 38;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_LEVEL: u32 = 124;
const ERROR_BAD_PATHNAME: u32 = 161;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_EVENTLOG_FILE_CORRUPT: u32 = 1500;
const RPC_S_SERVER_UNAVAILABLE: u32 = 1722;
const RPC_X_NULL_REF_POINTER: u32 = 1780;

pub const SEQUENTIAL: u32 = 1;
pub const SEEK: u32 = 2;
pub const FORWARDS: u32 = 4;
pub const BACKWARDS: u32 = 8;
const SIGNATURE: u32 = 0x654c_664c; // "LfLe"
const HEADER: usize = 56;
pub const COMPUTER: &str = "SAKE";
const EVENTLOG_KEY: &str = "hklm\\system\\currentcontrolset\\services\\eventlog";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rec {
    pub number: u32,
    pub time: u32,
    pub id: u32,
    pub ty: u16,
    pub category: u16,
    pub source: String,
    pub computer: String,
    pub sid: Vec<u8>,
    pub strings: Vec<String>,
    pub data: Vec<u8>,
}

fn align(n: usize, a: usize) -> usize {
    (n + a - 1) / a * a
}

fn put(out: &mut Vec<u8>, s: &str, wide: bool) {
    if wide {
        out.extend(s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    } else {
        out.extend(crate::text::s2a(s));
        out.push(0);
    }
}

impl Rec {
    /// The EVENTLOGRECORD as ReadEventLogA (narrow) or W reads it
    pub fn encode(&self, wide: bool) -> Vec<u8> {
        let mut v = vec![0u8; HEADER];
        put(&mut v, &self.source, wide);
        put(&mut v, &self.computer, wide);
        v.resize(align(v.len(), 8), 0);
        let sid_at = v.len();
        v.extend(&self.sid);
        let strings_at = v.len();
        for s in &self.strings {
            put(&mut v, s, wide);
        }
        let data_at = v.len();
        v.extend(&self.data);
        v.resize(align(v.len(), 4) + 4, 0);
        let len = v.len() as u32;
        let n = v.len();
        v[n - 4..].copy_from_slice(&len.to_le_bytes());
        let fields: [u32; 5] = [len, SIGNATURE, self.number, self.time, self.time];
        for (i, f) in fields.iter().enumerate() {
            v[4 * i..4 * i + 4].copy_from_slice(&f.to_le_bytes());
        }
        v[20..24].copy_from_slice(&self.id.to_le_bytes());
        for (i, h) in [self.ty, self.strings.len() as u16, self.category, 0]
            .iter()
            .enumerate()
        {
            v[24 + 2 * i..26 + 2 * i].copy_from_slice(&h.to_le_bytes());
        }
        let rest = [
            0u32,
            strings_at as u32,
            self.sid.len() as u32,
            sid_at as u32,
            self.data.len() as u32,
            data_at as u32,
        ];
        for (i, f) in rest.iter().enumerate() {
            v[32 + 4 * i..36 + 4 * i].copy_from_slice(&f.to_le_bytes());
        }
        v
    }

    /// A record as encode(wide) wrote it (None: not one)
    pub fn decode(b: &[u8]) -> Option<Rec> {
        let d = |o: usize| -> Option<u32> {
            Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
        };
        let h = |o: usize| -> Option<u16> {
            Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
        };
        let len = d(0)? as usize;
        if len < HEADER + 4 || len > b.len() || d(4)? != SIGNATURE || d(len - 4)? as usize != len {
            return None;
        }
        let b = &b[..len];
        let wstr = |at: &mut usize| -> Option<String> {
            let mut s = vec![];
            loop {
                let c = u16::from_le_bytes(b.get(*at..*at + 2)?.try_into().ok()?);
                *at += 2;
                if c == 0 {
                    return Some(String::from_utf16_lossy(&s));
                }
                s.push(c);
            }
        };
        let mut at = HEADER;
        let source = wstr(&mut at)?;
        let computer = wstr(&mut at)?;
        let (sl, so) = (d(40)? as usize, d(44)? as usize);
        let (dl, dof) = (d(48)? as usize, d(52)? as usize);
        let mut sa = d(36)? as usize;
        let mut strings = vec![];
        for _ in 0..h(26)? {
            strings.push(wstr(&mut sa)?);
        }
        Some(Rec {
            number: d(8)?,
            time: d(12)?,
            id: d(20)?,
            ty: h(24)?,
            category: h(28)?,
            source,
            computer,
            sid: b.get(so..so + sl)?.to_vec(),
            strings,
            data: b.get(dof..dof + dl)?.to_vec(),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Log {
    pub recs: Vec<Rec>,
    /// the number the next record gets
    pub next: u32,
}

impl Log {
    pub fn report(&mut self, mut r: Rec) {
        r.number = self.next.max(1);
        self.next = r.number + 1;
        self.recs.push(r);
    }
    pub fn oldest(&self) -> u32 {
        self.recs.first().map_or(0, |r| r.number)
    }
    pub fn clear(&mut self) {
        self.next = self.next.max(1);
        self.recs.clear();
    }

    /// The .evt file BackupEventLog writes: header, the records (wide), the end-of-file record
    pub fn evt(&self) -> Vec<u8> {
        let mut recs = vec![];
        for r in &self.recs {
            recs.extend(r.encode(true));
        }
        let end = 0x30 + recs.len() as u32;
        let (cur, old) = (self.next.max(1), self.oldest().max(1));
        let mut v = vec![];
        for x in [
            0x30, SIGNATURE, 1, 1, 0x30, end, cur, old, 0x8_0000, 0, 0, 0x30,
        ] {
            v.extend(u32::to_le_bytes(x));
        }
        v.extend(recs);
        for x in [
            0x28,
            0x1111_1111,
            0x2222_2222,
            0x3333_3333,
            0x4444_4444,
            0x30,
            end,
            cur,
            old,
            0x28,
        ] {
            v.extend(u32::to_le_bytes(x));
        }
        v
    }

    pub fn from_evt(b: &[u8]) -> Option<Log> {
        let d = |o: usize| -> Option<u32> {
            Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
        };
        if d(0)? != 0x30 || d(4)? != SIGNATURE || d(44)? != 0x30 {
            return None;
        }
        let mut log = Log {
            recs: vec![],
            next: d(24)?,
        };
        let mut at = 0x30;
        loop {
            if d(at)? == 0x28 && d(at + 4)? == 0x1111_1111 && d(at + 36)? == 0x28 {
                return Some(log);
            }
            let r = Rec::decode(&b[at..])?;
            at += d(at)? as usize;
            log.recs.push(r);
        }
    }
}

/// The read position of a handle: before records[pos] (None: not read yet)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cursor(pub Option<usize>);

/// What one ReadEventLog call returns: Ok(bytes) or Err((error, needed))
pub fn read(
    log: &Log,
    cur: &mut Cursor,
    flags: u32,
    offset: u32,
    size: usize,
    wide: bool,
) -> Result<Vec<u8>, (u32, usize)> {
    let n = log.recs.len();
    let fwd = flags & FORWARDS != 0;
    let mut i = if flags & SEEK != 0 {
        match log.recs.iter().position(|r| r.number == offset) {
            Some(i) => i as isize,
            None => return Err((ERROR_INVALID_PARAMETER, 0)),
        }
    } else {
        let p = cur.0.unwrap_or(if fwd { 0 } else { n }).min(n) as isize;
        if fwd {
            p
        } else {
            p - 1
        }
    };
    let mut out = vec![];
    while i >= 0 && (i as usize) < n {
        let b = log.recs[i as usize].encode(wide);
        if out.len() + b.len() > size {
            if out.is_empty() {
                return Err((ERROR_INSUFFICIENT_BUFFER, b.len()));
            }
            break;
        }
        out.extend(b);
        cur.0 = Some(if fwd { i as usize + 1 } else { i as usize });
        i += if fwd { 1 } else { -1 };
    }
    if out.is_empty() {
        return Err((ERROR_HANDLE_EOF, 0));
    }
    Ok(out)
}

/// The flags ReadEventLog accepts: one of SEQUENTIAL/SEEK and one of FORWARDS/BACKWARDS
pub fn flags_ok(f: u32) -> bool {
    f & !15 == 0
        && (f & SEQUENTIAL != 0) != (f & SEEK != 0)
        && (f & FORWARDS != 0) != (f & BACKWARDS != 0)
}

pub fn type_ok(t: u32) -> bool {
    matches!(t, 0 | 1 | 2 | 4 | 8 | 0x10)
}

/// The Application log after boot: a few records of the event system service
fn application_log() -> Log {
    let mut l = Log::default();
    let t = now();
    for _ in 0..4 {
        l.report(Rec {
            time: t,
            id: 4625,
            ty: 4,
            source: "EventSystem".into(),
            computer: COMPUTER.into(),
            ..Default::default()
        });
    }
    l
}

/// The System log as it is after boot: the event log service's own records, the first one
/// "event log started" (6005) with its 24 bytes of data
fn system_log() -> Log {
    let mut l = Log::default();
    let t = now();
    for i in 0..16u32 {
        let id = [0x8000_1775, 0x8000_1779, 0x8000_177d, 0x8000_177d][i as usize % 4];
        l.report(Rec {
            time: t,
            id,
            ty: 4,
            source: "EventLog".into(),
            computer: COMPUTER.into(),
            data: vec![0; 24],
            ..Default::default()
        });
    }
    l
}

fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}

enum Handle {
    /// a live log, the source records are written as
    Live {
        log: String,
        source: String,
        cur: Cursor,
    },
    Backup {
        log: Log,
        cur: Cursor,
    },
}

#[derive(Default)]
struct State {
    logs: HashMap<String, Log>,
    handles: HashMap<u32, Handle>,
    next: u32,
    traces: HashMap<String, u64>,
}

static ST: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = ST.lock().unwrap();
    let s = g.get_or_insert_with(|| {
        let mut s = State {
            next: 0x6e10_0000,
            ..Default::default()
        };
        s.logs.insert("system".into(), system_log());
        s.logs.insert("application".into(), application_log());
        s
    });
    f(s)
}

fn fail(e: u32) -> u32 {
    last_error(e);
    0
}

fn add(h: Handle) -> u32 {
    with(|s| {
        s.next += 8;
        let n = s.next;
        s.handles.insert(n, h);
        n
    })
}

/// The log a name opens: a log key, the log with that source, or Application
pub fn log_of(name: &str, is_key: impl Fn(&str) -> bool, logs: &[String]) -> String {
    let n = name.to_lowercase();
    if !n.is_empty() && !n.contains('\\') && is_key(&format!("{EVENTLOG_KEY}\\{n}")) {
        return n;
    }
    logs.iter()
        .find(|l| {
            !n.is_empty()
                && !n.contains('\\')
                && is_key(&format!("{EVENTLOG_KEY}\\{}\\{n}", l.to_lowercase()))
        })
        .map_or("application".into(), |l| l.to_lowercase())
}

/// Server names: NULL, "" or this machine are local; any other is unreachable
fn server_ok(p: u32, wide: bool) -> bool {
    if p == 0 {
        return true;
    }
    let s = arg_str(p, wide);
    let s = s.trim_start_matches('\\');
    s.is_empty()
        || s.eq_ignore_ascii_case(COMPUTER)
        || s == "."
        || s.eq_ignore_ascii_case("localhost")
}

/// OpenEventLog(server, source) and RegisterEventSource(server, source)
fn open(c: &mut Ctx, wide: bool) -> u32 {
    let (srv, src) = (c.a(0), c.a(1));
    if src == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    if !server_ok(srv, wide) {
        return fail(RPC_S_SERVER_UNAVAILABLE);
    }
    let source = arg_str(src, wide);
    let logs = crate::misc::reg_subkeys(EVENTLOG_KEY);
    let log = log_of(&source, crate::misc::reg_key_exists, &logs);
    with(|s| {
        s.logs.entry(log.clone()).or_default();
    });
    last_error(0);
    add(Handle::Live {
        log,
        source,
        cur: Cursor(None),
    })
}

/// CloseEventLog / DeregisterEventSource
fn close(c: &mut Ctx) -> u32 {
    if with(|s| s.handles.remove(&c.a(0))).is_some() {
        1
    } else {
        fail(ERROR_INVALID_HANDLE)
    }
}

/// The records a handle reads (a copy)
fn log_for(h: u32) -> Option<Log> {
    with(|s| match s.handles.get(&h)? {
        Handle::Live { log, .. } => s.logs.get(log).cloned(),
        Handle::Backup { log, .. } => Some(log.clone()),
    })
}

fn count(c: &mut Ctx, oldest: bool) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let Some(l) = log_for(h) else {
        return fail(ERROR_INVALID_HANDLE);
    };
    mem::w32(
        out,
        if oldest {
            l.oldest()
        } else {
            l.recs.len() as u32
        },
    );
    1
}

/// GetEventLogInformation(h, level, buffer, size, &needed): EVENTLOG_FULL_INFORMATION only
fn info(c: &mut Ctx) -> u32 {
    let (h, level, buf, size, need) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if level != 0 {
        return fail(ERROR_INVALID_LEVEL);
    }
    if log_for(h).is_none() {
        return fail(ERROR_INVALID_HANDLE);
    }
    if buf == 0 || need == 0 {
        return fail(RPC_X_NULL_REF_POINTER);
    }
    mem::w32(need, 4);
    if size < 4 {
        return fail(ERROR_INSUFFICIENT_BUFFER);
    }
    mem::w32(buf, 0);
    1
}

/// ReportEvent(h, type, category, id, sid, nstrings, datasize, strings, data)
fn report(c: &mut Ctx, wide: bool) -> u32 {
    let (h, ty, cat, id, sid, ns, dsize, strs, data) = (
        c.a(0),
        c.a(1) & 0xffff,
        c.a(2) & 0xffff,
        c.a(3),
        c.a(4),
        c.a(5) & 0xffff,
        c.a(6),
        c.a(7),
        c.a(8),
    );
    let Some((log, source)) = with(|s| match s.handles.get(&h) {
        Some(Handle::Live { log, source, .. }) => Some((log.clone(), source.clone())),
        _ => None,
    }) else {
        return fail(ERROR_INVALID_HANDLE);
    };
    if !type_ok(ty) {
        return fail(ERROR_CRC);
    }
    if ns != 0 && strs == 0 || dsize != 0 && data == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let strings = (0..ns)
        .map(|i| arg_str(mem::rp(strs + i * mem::ps()), wide))
        .collect();
    let sid = if sid == 0 {
        vec![]
    } else {
        mem::slice(sid, 8 + 4 * mem::r8(sid + 1) as u32).to_vec()
    };
    let data = if dsize == 0 {
        vec![]
    } else {
        mem::slice(data, dsize).to_vec()
    };
    let r = Rec {
        time: now(),
        id,
        ty: ty as u16,
        category: cat as u16,
        source,
        computer: COMPUTER.into(),
        sid,
        strings,
        data,
        number: 0,
    };
    with(|s| s.logs.entry(log).or_default().report(r));
    last_error(0);
    1
}

/// ReadEventLog(h, flags, offset, buffer, size, &read, &needed)
fn read_log(c: &mut Ctx, wide: bool) -> u32 {
    let (h, flags, offset, buf, size, pread, pneed) =
        (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    if buf == 0 || pread == 0 || pneed == 0 || !flags_ok(flags) {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let r = with(|s| {
        let State { logs, handles, .. } = s;
        Some(match handles.get_mut(&h)? {
            Handle::Live { log, cur, .. } => read(
                logs.get(log.as_str())?,
                cur,
                flags,
                offset,
                size as usize,
                wide,
            ),
            Handle::Backup { log, cur } => read(log, cur, flags, offset, size as usize, wide),
        })
    });
    match r {
        None => fail(ERROR_INVALID_HANDLE),
        Some(Ok(b)) => {
            mem::write(buf, &b);
            mem::w32(pread, b.len() as u32);
            mem::w32(pneed, 0);
            1
        }
        Some(Err((e, need))) => {
            mem::w32(pread, 0);
            mem::w32(pneed, need as u32);
            fail(e)
        }
    }
}

/// Writes a backup; Err(error)
fn backup_to(l: &Log, name: &str) -> Result<(), u32> {
    if crate::files::resolve(name).is_some() {
        return Err(ERROR_ALREADY_EXISTS);
    }
    let p = crate::files::resolve_new(name);
    kura::fs::write(p, l.evt()).map_err(|_| ERROR_BAD_PATHNAME)
}

/// BackupEventLog(h, file)
fn backup(c: &mut Ctx, wide: bool) -> u32 {
    let (h, f) = (c.a(0), c.a(1));
    if f == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let Some(l) = log_for(h) else {
        return fail(ERROR_INVALID_HANDLE);
    };
    match backup_to(&l, &arg_str(f, wide)) {
        Ok(()) => 1,
        Err(e) => fail(e),
    }
}

/// ClearEventLog(h, backup file or NULL): live logs only
fn clear(c: &mut Ctx, wide: bool) -> u32 {
    let (h, f) = (c.a(0), c.a(1));
    let Some(log) = with(|s| match s.handles.get(&h) {
        Some(Handle::Live { log, .. }) => Some(log.clone()),
        _ => None,
    }) else {
        return fail(ERROR_INVALID_HANDLE);
    };
    if f != 0 {
        let l = with(|s| s.logs.get(&log).cloned().unwrap_or_default());
        if let Err(e) = backup_to(&l, &arg_str(f, wide)) {
            return fail(e);
        }
    }
    with(|s| s.logs.entry(log).or_default().clear());
    1
}

/// OpenBackupEventLog(server, file)
fn open_backup(c: &mut Ctx, wide: bool) -> u32 {
    let (srv, f) = (c.a(0), c.a(1));
    if f == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    if !server_ok(srv, wide) {
        return fail(RPC_S_SERVER_UNAVAILABLE);
    }
    let name = arg_str(f, wide);
    let Some(p) = crate::files::resolve(&name) else {
        return fail(ERROR_FILE_NOT_FOUND);
    };
    let Some(log) = kura::fs::read(p).ok().and_then(|b| Log::from_evt(&b)) else {
        return fail(ERROR_EVENTLOG_FILE_CORRUPT);
    };
    add(Handle::Backup {
        log,
        cur: Cursor(None),
    })
}

// ---------------------------------------------------------------------------------------------
// ETW sessions: EVENT_TRACE_PROPERTIES is a WNODE_HEADER (48 bytes: BufferSize at 0, Guid at 24)
// then BufferSize .. LoggerThreadId (a HANDLE at 104), LogFileNameOffset, LoggerNameOffset;
// 120 bytes on x86 and x64.

const PROPS: u32 = 120;
const SYSTEM_TRACE_GUID: [u8; 16] = [
    0xad, 0x4a, 0x81, 0x9e, 0x04, 0x32, 0xd2, 0x11, 0x9a, 0x82, 0x00, 0x60, 0x08, 0xa8, 0x69, 0x39,
];

/// The error StartTrace answers for these properties (0: it starts)
pub fn trace_check(
    buf: u32,
    name_len: u32,
    file_off: u32,
    name_off: u32,
    mode: u32,
    guid: &[u8],
    kernel: bool,
) -> u32 {
    if buf < PROPS {
        return ERROR_BAD_LENGTH;
    }
    let off_bad = |o: u32| o != 0 && (o < PROPS || o >= buf);
    if off_bad(file_off) || off_bad(name_off) {
        return ERROR_INVALID_PARAMETER;
    }
    if name_off != 0 && name_len + 1 > buf - name_off {
        return ERROR_BAD_LENGTH;
    }
    if mode & 3 == 3 {
        return ERROR_INVALID_PARAMETER;
    }
    if guid == SYSTEM_TRACE_GUID && !kernel {
        return ERROR_INVALID_PARAMETER;
    }
    if file_off == 0 && mode & 0x100 == 0 {
        return ERROR_BAD_PATHNAME;
    }
    0
}

fn offs() -> (u32, u32) {
    if mem::x64() {
        (112, 116)
    } else {
        (108, 112)
    }
}

/// StartTrace(&handle, name, properties) -> error
fn start_trace(c: &mut Ctx, wide: bool) -> u32 {
    let (ph, pn, p) = (c.a(0), c.a(1), c.a(2));
    if ph == 0 || pn == 0 || p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let name = arg_str(pn, wide);
    let len = if wide {
        name.encode_utf16().count() as u32 * 2
    } else {
        crate::text::s2a(&name).len() as u32
    };
    let (fo, no) = offs();
    let e = trace_check(
        mem::r32(p),
        len,
        mem::r32(p + fo),
        mem::r32(p + no),
        mem::r32(p + 64),
        mem::slice(p + 24, 16),
        name == "NT Kernel Logger",
    );
    if e != 0 {
        return e;
    }
    with(|s| {
        if s.traces.contains_key(&name.to_lowercase()) {
            return ERROR_ALREADY_EXISTS;
        }
        let h = 0x1000 + s.traces.len() as u64 * 16 + 16;
        s.traces.insert(name.to_lowercase(), h);
        mem::w64(ph, h);
        0
    })
}

/// ControlTrace(handle, name, properties, control) / StopTrace(handle, name, properties)
fn control_trace(h: u64, pn: u32, control: u32, wide: bool) -> u32 {
    let name = if pn != 0 {
        Some(arg_str(pn, wide).to_lowercase())
    } else {
        None
    };
    with(|s| {
        let key = name.or_else(|| s.traces.iter().find(|x| *x.1 == h).map(|x| x.0.clone()));
        match key.filter(|k| s.traces.contains_key(k)) {
            None => 4201, // ERROR_WMI_INSTANCE_NOT_FOUND
            Some(k) => {
                if control == 1 {
                    s.traces.remove(&k);
                }
                0
            }
        }
    })
}

fn trace_args(c: &mut Ctx) -> (u64, u32) {
    if mem::x64() {
        (c.a64(0), c.a(1))
    } else {
        (c.a(0) as u64 | (c.a(1) as u64) << 32, c.a(2))
    }
}

pub fn init() {
    let d = "advapi32";
    implement(d, "OpenEventLogA", |c| open(c, false));
    implement(d, "OpenEventLogW", |c| open(c, true));
    implement(d, "RegisterEventSourceA", |c| open(c, false));
    implement(d, "RegisterEventSourceW", |c| open(c, true));
    implement(d, "CloseEventLog", close);
    implement(d, "DeregisterEventSource", close);
    implement(d, "GetNumberOfEventLogRecords", |c| count(c, false));
    implement(d, "GetOldestEventLogRecord", |c| count(c, true));
    implement(d, "GetEventLogInformation", info);
    implement(d, "ReportEventA", |c| report(c, false));
    implement(d, "ReportEventW", |c| report(c, true));
    implement(d, "ReadEventLogA", |c| read_log(c, false));
    implement(d, "ReadEventLogW", |c| read_log(c, true));
    implement(d, "BackupEventLogA", |c| backup(c, false));
    implement(d, "BackupEventLogW", |c| backup(c, true));
    implement(d, "ClearEventLogA", |c| clear(c, false));
    implement(d, "ClearEventLogW", |c| clear(c, true));
    implement(d, "OpenBackupEventLogA", |c| open_backup(c, false));
    implement(d, "OpenBackupEventLogW", |c| open_backup(c, true));
    implement(d, "NotifyChangeEventLog", |c| {
        if log_for(c.a(0)).is_some() {
            1
        } else {
            fail(ERROR_INVALID_HANDLE)
        }
    });
    implement(d, "StartTraceA", |c| start_trace(c, false));
    implement(d, "StartTraceW", |c| start_trace(c, true));
    // ControlTrace(TRACEHANDLE, name, props, control); StopTrace(TRACEHANDLE, name, props)
    implement(d, "ControlTraceA", |c| {
        let (h, n) = trace_args(c);
        let ctl = if mem::x64() { c.a(3) } else { c.a(4) };
        control_trace(h, n, ctl, false)
    });
    implement(d, "ControlTraceW", |c| {
        let (h, n) = trace_args(c);
        let ctl = if mem::x64() { c.a(3) } else { c.a(4) };
        control_trace(h, n, ctl, true)
    });
    implement(d, "StopTraceA", |c| {
        let (h, n) = trace_args(c);
        control_trace(h, n, 1, false)
    });
    implement(d, "StopTraceW", |c| {
        let (h, n) = trace_args(c);
        control_trace(h, n, 1, true)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rec(src: &str, strings: &[&str], sid: usize, data: usize) -> Rec {
        Rec {
            time: 7,
            id: 5,
            ty: 2,
            category: 1,
            source: src.into(),
            computer: COMPUTER.into(),
            sid: vec![1; sid],
            strings: strings.iter().map(|s| s.to_string()).collect(),
            data: vec![9; data],
            number: 0,
        }
    }

    #[test]
    fn record_layout() {
        // the service's start record: offsets to the SID, strings and data are all the same
        let r = Rec {
            data: vec![0; 24],
            ..rec("EventLog", &[], 0, 0)
        };
        let w = r.encode(true);
        let d = |o: usize| u32::from_le_bytes(w[o..o + 4].try_into().unwrap());
        let at = (56 + 18 + 10 + 7) & !7;
        assert_eq!((d(36), d(44), d(52), d(48)), (at, at, at, 24));
        assert_eq!(d(0) as usize, w.len());
        assert_eq!(d(w.len() - 4) as usize, w.len());
        assert_eq!(d(4), SIGNATURE);
        let a = r.encode(false);
        let da = u32::from_le_bytes(a[52..56].try_into().unwrap());
        assert_eq!(da, (56 + 9 + 5 + 7) & !7);
    }

    #[test]
    fn windows_reads() {
        let mut l = Log::default();
        for _ in 0..12 {
            l.report(rec("EventLog", &[], 0, 24));
        }
        let num = |b: &[u8]| u32::from_le_bytes(b[8..12].try_into().unwrap());
        let one = 200;
        let mut c = Cursor::default();
        // sequential: the offset is ignored; changing direction reads the last record again
        assert_eq!(
            num(&read(&l, &mut c, SEQUENTIAL | FORWARDS, 100, one, true).unwrap()),
            1
        );
        assert_eq!(
            num(&read(&l, &mut c, SEQUENTIAL | FORWARDS, 200, one, true).unwrap()),
            2
        );
        assert_eq!(
            num(&read(&l, &mut c, SEQUENTIAL | BACKWARDS, 300, one, true).unwrap()),
            2
        );
        assert_eq!(
            num(&read(&l, &mut c, SEQUENTIAL | BACKWARDS, 400, one, true).unwrap()),
            1
        );
        assert_eq!(
            read(&l, &mut c, SEQUENTIAL | BACKWARDS, 0, one, true),
            Err((ERROR_HANDLE_EOF, 0))
        );
        assert_eq!(
            read(&l, &mut c, SEEK | BACKWARDS, 0, one, true),
            Err((ERROR_INVALID_PARAMETER, 0))
        );
        // a fresh backwards read starts at the newest
        let mut c = Cursor::default();
        assert_eq!(
            num(&read(&l, &mut c, SEQUENTIAL | BACKWARDS, 100, one, true).unwrap()),
            12
        );
        assert_eq!(
            num(&read(&l, &mut c, SEQUENTIAL | BACKWARDS, 100, one, true).unwrap()),
            11
        );
        // seek forwards then sequential: the record after the last one read
        let mut c = Cursor::default();
        assert_eq!(
            read(&l, &mut c, SEEK | FORWARDS, 13, one, true),
            Err((ERROR_INVALID_PARAMETER, 0))
        );
        for (f, o, want) in [
            (SEEK | FORWARDS, 2, 2),
            (SEEK | FORWARDS, 4, 4),
            (SEEK | FORWARDS, 3, 3),
            (SEQUENTIAL | FORWARDS, 100, 4),
            (SEEK | BACKWARDS, 10, 10),
        ] {
            assert_eq!(num(&read(&l, &mut c, f, o, one, true).unwrap()), want);
        }
        let mut c = Cursor::default();
        for (f, o, want) in [
            (SEEK | BACKWARDS, 5, 5),
            (SEEK | BACKWARDS, 3, 3),
            (SEEK | BACKWARDS, 4, 4),
            (SEQUENTIAL | BACKWARDS, 100, 3),
            (SEEK | FORWARDS, 10, 10),
        ] {
            assert_eq!(num(&read(&l, &mut c, f, o, one, true).unwrap()), want);
        }
        // a small buffer: the size of the record, and the position does not move
        let mut c = Cursor::default();
        let size = l.recs[0].encode(false).len();
        assert_eq!(
            read(&l, &mut c, SEQUENTIAL | FORWARDS, 0, 56, false),
            Err((ERROR_INSUFFICIENT_BUFFER, size))
        );
        assert_eq!(c, Cursor(None));
        // a big one: whole records
        assert_eq!(
            read(&l, &mut c, SEQUENTIAL | FORWARDS, 0, 3 * size + 1, false)
                .unwrap()
                .len(),
            3 * size
        );
        assert_eq!(c, Cursor(Some(3)));
    }

    #[test]
    fn numbers_continue_after_clear() {
        let mut l = Log::default();
        l.report(rec("Wine", &[], 0, 0));
        l.clear();
        assert_eq!((l.recs.len(), l.oldest()), (0, 0));
        l.report(rec("Wine", &[], 0, 0));
        assert_eq!(l.oldest(), 2);
    }

    #[test]
    fn flags() {
        for f in 0..32 {
            let want = matches!(f, 5 | 9 | 6 | 10);
            assert_eq!(flags_ok(f), want, "{f}");
        }
    }

    #[test]
    fn logs_and_sources() {
        let keys = [
            "application",
            "system",
            "system\\eventlog",
            "wine",
            "wine\\winesrc",
        ]
        .map(|k| format!("{EVENTLOG_KEY}\\{k}"));
        let is_key = |k: &str| keys.iter().any(|x| x == k);
        let logs = ["Application".to_string(), "System".into(), "Wine".into()];
        assert_eq!(log_of("SYSTEM", is_key, &logs), "system");
        assert_eq!(log_of("EventLog", is_key, &logs), "system");
        assert_eq!(log_of("WineSrc", is_key, &logs), "wine");
        assert_eq!(log_of("deadbeef", is_key, &logs), "application");
        assert_eq!(log_of("wine\\winesrc", is_key, &logs), "application");
    }

    #[test]
    fn trace_properties() {
        let g = [0u8; 16];
        let full = PROPS + 5 + 9;
        assert_eq!(
            trace_check(0, 4, PROPS + 5, PROPS, 0, &g, false),
            ERROR_BAD_LENGTH
        );
        assert_eq!(
            trace_check(full, 21, PROPS + 5, PROPS, 0, &g, false),
            ERROR_BAD_LENGTH
        );
        assert_eq!(
            trace_check(full, 4, 1, PROPS, 0, &g, false),
            ERROR_INVALID_PARAMETER
        );
        assert_eq!(
            trace_check(full, 4, PROPS + 5, 1, 0, &g, false),
            ERROR_INVALID_PARAMETER
        );
        assert_eq!(
            trace_check(full, 4, PROPS + 5, PROPS, 3, &g, false),
            ERROR_INVALID_PARAMETER
        );
        assert_eq!(
            trace_check(full, 4, PROPS + 5, PROPS, 0, &SYSTEM_TRACE_GUID, false),
            ERROR_INVALID_PARAMETER
        );
        assert_eq!(
            trace_check(full, 4, 0, PROPS, 0, &g, false),
            ERROR_BAD_PATHNAME
        );
        assert_eq!(trace_check(full, 4, 0, PROPS, 0x100, &g, false), 0);
        assert_eq!(trace_check(full, 4, PROPS + 5, PROPS, 0, &g, false), 0);
    }

    proptest! {
        #[test]
        fn records_round_trip(src in "[a-zA-Z0-9 ]{0,12}", strings in proptest::collection::vec("[ -~]{0,20}", 0..4), sid in 0usize..30, data in 0usize..50) {
            let s: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
            let r = Rec { number: 3, ..rec(&src, &s, sid, data) };
            let w = r.encode(true);
            prop_assert_eq!(w.len() % 4, 0);
            prop_assert_eq!(Rec::decode(&w), Some(r.clone()));
            // narrow records have the same fields at the same header places
            let a = r.encode(false);
            prop_assert_eq!(&a[4..32], &w[4..32]);
            prop_assert_eq!(&a[40..44], &w[40..44]);
        }

        #[test]
        fn backups_round_trip(n in 0usize..6, data in 0usize..40, cleared in 0u32..5) {
            let mut l = Log::default();
            for _ in 0..cleared { l.report(rec("x", &[], 0, 0)); }
            l.clear();
            for i in 0..n { l.report(rec("Src", &["a", "bc"][..i % 3], i, data)); }
            let b = l.evt();
            prop_assert_eq!(Log::from_evt(&b), Some(l));
            prop_assert_eq!(Log::from_evt(&b[..b.len() - 1]), None);
        }

        /// Reading forwards then backwards, one record a time, gives the log both ways
        #[test]
        fn sequential_both_ways(n in 1usize..10) {
            let mut l = Log::default();
            for _ in 0..n { l.report(rec("s", &[], 0, 0)); }
            let mut c = Cursor::default();
            let one = l.recs[0].encode(true).len();
            let num = |b: &[u8]| u32::from_le_bytes(b[8..12].try_into().unwrap());
            for i in 1..=n { prop_assert_eq!(num(&read(&l, &mut c, SEQUENTIAL | FORWARDS, 0, one, true).unwrap()), i as u32); }
            prop_assert_eq!(read(&l, &mut c, SEQUENTIAL | FORWARDS, 0, one, true), Err((ERROR_HANDLE_EOF, 0)));
            for i in (1..=n).rev() { prop_assert_eq!(num(&read(&l, &mut c, SEQUENTIAL | BACKWARDS, 0, one, true).unwrap()), i as u32); }
        }
    }
}
