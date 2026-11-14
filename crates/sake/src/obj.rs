//! Kernel objects behind handles, and waiting on them.
//!
//! One lock for everything waitable (events, mutexes, semaphores, threads, message queues), and
//! a condition variable per waiting thread. SetEvent, ReleaseMutex and ReleaseSemaphore wake only
//! the waiters of their object, as many as they let through (`signal_obj`, waits::wake); any
//! other change wakes every waiter (`signal`), which then rechecks.

use crate::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub enum Obj {
    Event { manual: bool, set: bool },
    /// a waitable timer: becomes set at `due`, then again every `period` if nonzero
    Timer { manual: bool, set: bool, due: Option<Instant>, period: Duration },
    Mutex { owner: u32, count: u32 },
    Semaphore { count: i32, max: i32 },
    Thread { tid: u32, done: bool, code: u32 },
    /// a process (this one: DuplicateHandle of GetCurrentProcess, OpenProcess of our id; or
    /// another, crate::process), and the access it was opened with
    Process { access: u32, pid: u32 },
    /// another process's thread (CreateProcess's hThread: its first)
    RemoteThread { pid: u32, tid: u32 },
    File(Arc<crate::files::File>),
    /// an end of a pipe (crate::kern)
    Pipe(crate::pipe::Handle),
    /// a named event, mutex or semaphore (crate::kern: other processes open it too)
    Shared(u32),
    Find(Arc<Mutex<crate::files::Find>>),
    /// a file mapping: its file (None: the pagefile), size, the one view of a pagefile section,
    /// and whether its views may write the file (PAGE_READWRITE / PAGE_EXECUTE_READWRITE)
    Mapping { file: Option<Arc<crate::files::File>>, size: u32, view: u32, write: bool },
    Key(crate::misc::Key),
    /// a debug object (NtCreateDebugObject): crate::kern's debug port
    DebugObject(u32),
    Other(&'static str),
}

/// Objects and the handles to them: every handle is its own number (DuplicateHandle and opening
/// a named object give a new one, closing one leaves the others valid); an object is kept under
/// the number of its first handle, with the count of its open handles.
#[derive(Default)]
pub struct Objs {
    map: HashMap<u32, (Obj, u32)>,
    handles: HashMap<u32, u32>,
    /// the handles child processes inherit (HANDLE_FLAG_INHERIT)
    inherit: crate::HashSet<u32>,
    /// the access handles opened with less than all have (OpenMutex & co.)
    access: HashMap<u32, u32>,
}

impl Objs {
    /// the object key of handle `h` (an object's key also finds it after its first handle closed)
    fn key(&self, h: &u32) -> Option<u32> {
        self.handles.get(h).copied().or_else(|| self.map.contains_key(h).then_some(*h))
    }
    pub fn get(&self, h: &u32) -> Option<&(Obj, u32)> {
        self.map.get(&self.key(h)?)
    }
    pub fn get_mut(&mut self, h: &u32) -> Option<&mut (Obj, u32)> {
        let k = self.key(h)?;
        self.map.get_mut(&k)
    }
    pub fn contains_key(&self, h: &u32) -> bool {
        self.handles.contains_key(h)
    }
    /// (object key, object) for every object
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &(Obj, u32))> {
        self.map.iter()
    }
    pub fn values(&self) -> impl Iterator<Item = &(Obj, u32)> {
        self.map.values()
    }
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut (Obj, u32)> {
        self.map.values_mut()
    }
}

impl Table {
    /// (handle, object) of every handle a child inherits
    pub fn inheritable(&self) -> impl Iterator<Item = (u32, &Obj)> {
        self.objs.inherit.iter().filter_map(|h| Some((*h, &self.objs.get(h)?.0)))
    }
}

/// HANDLE_FLAG_INHERIT of handle `h` (false: no such handle)
pub fn set_inherit(h: u32, on: bool) -> bool {
    with(|t| {
        if !t.objs.handles.contains_key(&h) {
            return false;
        }
        if on {
            t.objs.inherit.insert(h);
        } else {
            t.objs.inherit.remove(&h);
        }
        true
    })
}
pub fn inherits(h: u32) -> bool {
    with(|t| t.objs.inherit.contains(&h))
}

/// A handle of number `h` (an inherited one keeps its number in the child).
pub fn add_at(h: u32, o: Obj, inherit: bool) {
    with(|t| {
        t.objs.map.insert(h, (o, 1));
        t.objs.handles.insert(h, h);
        if inherit {
            t.objs.inherit.insert(h);
        }
        t.next = t.next.max(h + 4);
    })
}

pub struct Table {
    pub objs: Objs,
    next: u32,
    /// named objects: name -> handle of the first one
    pub names: HashMap<String, u32>,
    pub gen: u64,
    /// the waiting threads, by the object keys they wait on, and each one's condition variable
    waits: waits::wake::WaitList<u32>,
    cvs: HashMap<u64, Arc<Condvar>>,
}
impl Table {
    fn new() -> Self {
        Table { objs: Objs::default(), next: 0x100, names: HashMap::default(), gen: 0, waits: Default::default(), cvs: HashMap::default() }
    }
    fn notify(&self, ids: &[u64]) {
        for id in ids {
            if let Some(cv) = self.cvs.get(id) {
                cv.notify_one();
            }
        }
    }
    /// Whether object `k` lets some waiter through now (without taking it); false for the kinds
    /// only `signal` wakes for.
    fn signaled(&self, k: u32) -> bool {
        match self.objs.map.get(&k) {
            Some((Obj::Event { set, .. } | Obj::Timer { set, .. }, _)) => *set,
            Some((Obj::Semaphore { count, .. }, _)) => *count > 0,
            Some((Obj::Mutex { owner, .. }, _)) => *owner == 0 || *owner == ABANDONED,
            Some((Obj::Thread { done, .. }, _)) => *done,
            _ => false,
        }
    }
    /// A waiter goes: the wakeup it may have taken goes on to another waiter of each of its
    /// objects still signaled (waits::wake).
    fn leave(&mut self, id: u64) {
        self.cvs.remove(&id);
        let Some((_, keys)) = self.waits.remove(id) else { return };
        let mut out = Vec::new();
        for k in keys {
            if self.signaled(k) {
                self.waits.wake(&k, 1, &mut out);
            }
        }
        self.notify(&out);
    }
}

pub static TABLE: Mutex<Option<Table>> = Mutex::new(None);

/// The guest threads' API counters (waits::handoff: a waiter woken by another thread's
/// SetEvent lets that thread reach its next API call first, as on Windows; api.rs counts)
pub static HANDOFF: std::sync::LazyLock<waits::handoff::Handoff> = std::sync::LazyLock::new(|| waits::handoff::Handoff::new(1024));
/// The last SetEvent of each event (object key) by a guest thread
static SIGNALS: Mutex<Option<HashMap<u32, waits::handoff::Mark>>> = Mutex::new(None);

/// Thread `cur()` signals handle `h` (called under the table's lock)
pub fn note_signal(t: &Table, h: u32) {
    if !crate::on_guest_thread() {
        return;
    }
    if let Some(k) = t.objs.key(&h) {
        let m = HANDOFF.mark(crate::cur().tid);
        SIGNALS.lock().unwrap().get_or_insert_with(HashMap::default).insert(k, m);
    }
}
/// A woken waiter lets its signaler reach its next API call (SAKE_TRACE_HANDOFF: the waits
/// longer than 1 ms, and the ones that ran out)
fn defer_to(m: waits::handoff::Mark, tid: u32) {
    let t = Instant::now();
    let passed = HANDOFF.wait_passed(m, tid);
    if crate::env_on!("SAKE_TRACE_HANDOFF") && (!passed || t.elapsed() >= Duration::from_millis(1)) {
        eprintln!("sake: thread {tid:04x} woke by {:04x}'s signal waited {:?}{}", m.tid, t.elapsed(), if passed { "" } else { " (ran out)" });
    }
}
/// The signal that left handle `h` signaled (by a guest thread)
fn signal_of(t: &Table, h: u32) -> Option<waits::handoff::Mark> {
    let k = t.objs.key(&h)?;
    SIGNALS.lock().unwrap().as_ref()?.get(&k).copied()
}

pub fn lock() -> MutexGuard<'static, Option<Table>> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn with<R>(f: impl FnOnce(&mut Table) -> R) -> R {
    let mut g = lock();
    f(g.get_or_insert_with(|| Table::new()))
}

pub const CURRENT_PROCESS: u32 = 0xffff_ffff;
pub const CURRENT_THREAD: u32 = 0xffff_fffe;

pub fn add(o: Obj) -> u32 {
    with(|t| {
        let h = t.next;
        t.next += 4;
        t.objs.map.insert(h, (o, 1));
        t.objs.handles.insert(h, h);
        h
    })
}

/// Another handle to the same object (DuplicateHandle, opening a named object).
pub fn dup(h: u32) -> u32 {
    with(|t| {
        let Some(k) = t.objs.key(&h) else { return 0 };
        t.objs.map.get_mut(&k).unwrap().1 += 1;
        let n = t.next;
        t.next += 4;
        t.objs.handles.insert(n, k);
        n
    })
}

/// Closes one handle; the object goes with its last one.
pub fn close(h: u32) -> bool {
    let (r, gone, port) = with(|t| {
        let Some(k) = t.objs.handles.remove(&h) else { return (false, None, None) };
        t.objs.inherit.remove(&h);
        t.objs.access.remove(&h);
        // (a completion port is closed with its last handle, though waits still hold it:
        // they end, signaled, as on Windows)
        let port = (matches!(t.objs.map.get(&k), Some((Obj::Other("iocp"), _))) && !t.objs.handles.values().any(|&v| v == k)).then_some(k);
        (true, unref(t, k), port)
    });
    if let Some(k) = port {
        crate::iocp::closed(k);
    }
    gone_object(h, gone);
    r
}

/// One reference to object `k` less: the object when that was its last
fn unref(t: &mut Table, k: u32) -> Option<Obj> {
    let rc = &mut t.objs.map.get_mut(&k)?.1;
    *rc -= 1;
    if *rc == 0 {
        t.names.retain(|_, v| *v != k);
        if let Some(m) = SIGNALS.lock().unwrap().as_mut() {
            m.remove(&k);
        }
        return t.objs.map.remove(&k).map(|o| o.0);
    }
    None
}

/// An object that went (its last handle closed, or the last wait on it ended)
fn gone_object(h: u32, gone: Option<Obj>) {
    // (outside the table's lock: the kernel's comes after it, never before)
    match gone {
        Some(Obj::Pipe(p)) => crate::pipe::closed(p),
        Some(Obj::Other("iocp")) => crate::iocp::gone(h),
        Some(Obj::Other("afd")) => {
            crate::afd::closed(h);
            crate::iocp::forget(h);
        }
        Some(Obj::Shared(id)) => {
            crate::kern::with(|k| k.release_sync(id, crate::pid()));
        }
        Some(Obj::DebugObject(port)) => crate::dbg::port_closed(port),
        _ => {}
    }
}

pub fn file(h: u32) -> Option<Arc<crate::files::File>> {
    with(|t| match t.objs.get(&h) {
        Some((Obj::File(f), _)) => Some(f.clone()),
        Some((Obj::Mapping { file: Some(f), .. }, _)) => Some(f.clone()),
        _ => None,
    })
}

/// Wakes every waiter (after changing anything they may wait for).
pub fn signal() {
    with(|t| {
        t.gen += 1;
        let mut out = Vec::new();
        t.waits.wake_all(&mut out);
        t.notify(&out);
    });
}

/// Handle `h`'s object became signaled for `n` waiters (an auto-reset event or a released mutex
/// 1, a semaphore the count released, a manual-reset event all): wakes only its waiters.
pub fn signal_obj(h: u32, n: usize) {
    with(|t| {
        t.gen += 1;
        let Some(k) = t.objs.key(&norm(h)) else { return };
        let mut out = Vec::new();
        t.waits.wake(&k, n, &mut out);
        t.notify(&out);
    });
}

thread_local! {
    /// this thread's condition variable for its waits (Table::cvs while it waits)
    static CV: Arc<Condvar> = Arc::new(Condvar::new());
}

pub fn new_thread_obj(tid: u32) -> u32 {
    add(Obj::Thread { tid, done: false, code: 0x103 })
}

/// Whether `h` is signaled for thread `tid`; takes it (auto-reset event, mutex, semaphore).
fn try_take(t: &mut Table, h: u32, tid: u32, take: bool) -> Option<bool> {
    let (o, _) = t.objs.get_mut(&h)?;
    Some(match o {
        Obj::Event { manual, set } => {
            let s = *set;
            if s && take && !*manual {
                *set = false;
            }
            s
        }
        Obj::Timer { manual, set, due, period } => {
            if let Some(d) = *due {
                if Instant::now() >= d {
                    *set = true;
                    *due = (!period.is_zero()).then(|| d + *period);
                }
            }
            let s = *set;
            if s && take && !*manual {
                *set = false;
            }
            s
        }
        Obj::Mutex { owner, count } => {
            if *owner == 0 || *owner == tid || *owner == ABANDONED {
                if take {
                    *owner = tid;
                    *count += 1;
                }
                true
            } else {
                false
            }
        }
        Obj::Semaphore { count, .. } => {
            if *count > 0 {
                if take {
                    *count -= 1;
                }
                true
            } else {
                false
            }
        }
        Obj::Thread { done, .. } => *done,
        Obj::Shared(id) => crate::kern::with(|k| k.try_take(*id, crate::pid(), tid, take)).0.is_some_and(|r| r.0),
        Obj::Process { pid, .. } => crate::process::ended(*pid),
        Obj::RemoteThread { pid, tid } => crate::kern::with(|k| k.thread_exit(*pid, *tid)).0.is_some(),
        // a completion port: while it holds packets (waiting on it takes none)
        Obj::Other("iocp") => crate::iocp::has_packets(h),
        // files and the like count as signaled
        _ => true,
    })
}

pub const WAIT_TIMEOUT: u32 = 0x102;
/// a mutex whose owner thread ended holding it (until the next wait takes it)
pub const ABANDONED: u32 = u32::MAX;

fn abandoned(t: &Table, h: u32) -> bool {
    match t.objs.get(&h) {
        Some((Obj::Mutex { owner: ABANDONED, .. }, _)) => true,
        Some((Obj::Shared(id), _)) => crate::kern::with(|k| k.try_take(*id, 0, 0, false)).0.is_some_and(|r| r.1),
        _ => false,
    }
}
pub const WAIT_FAILED: u32 = 0xffff_ffff;

/// WaitForMultipleObjects; `extra` is an extra wake condition (the message queue), checked
/// under the lock, which makes the result `handles.len()`.
/// Whether every handle can be waited on (taking none of them)
pub fn waitable(handles: &[u32]) -> bool {
    let tid = crate::cur().tid;
    let me = crate::cur().obj;
    let mut g = lock();
    let t = g.get_or_insert_with(|| Table::new());
    handles.iter().all(|&h| try_take(t, if h == CURRENT_THREAD { me } else { h }, tid, false).is_some())
}
/// Handle `h` has only the rights `mask` (waiting needs SYNCHRONIZE)
pub fn set_access(h: u32, mask: u32) {
    with(|t| t.objs.access.insert(h, mask));
}
/// Whether handle `h` grants any of `bits` (a handle opened without a mask grants all)
pub fn allows_any(h: u32, bits: u32) -> bool {
    with(|t| t.objs.access.get(&norm(h)).is_none_or(|a| a & bits != 0))
}
/// Whether handle `h` grants all of `want` (a handle opened without a mask grants all)
pub fn allows(h: u32, want: u32) -> bool {
    with(|t| t.objs.access.get(&norm(h)).is_none_or(|a| a & want == want))
}

/// A handle as the object table knows it: the low two bits are the program's (Windows ignores
/// them), pseudo-handles stay
pub fn norm(h: u32) -> u32 {
    if is_pseudo(h) { h } else { h & !3 }
}
/// GetCurrentProcess, GetCurrentThread, the token pseudo-handles and the rest of the top 16
pub fn is_pseudo(h: u32) -> bool {
    h >= 0xffff_fff0
}
/// What waiting on GetCurrentProcess() waits for: something never signaled (this process
/// has not ended while it waits)
fn self_process() -> u32 {
    static H: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *H.get_or_init(|| add(Obj::Event { manual: true, set: false }))
}
pub fn wait(handles: &[u32], all: bool, ms: u32, extra: Option<&dyn Fn() -> bool>) -> u32 {
    let me = crate::cur().obj;
    let tid = crate::cur().tid;
    let n = |h: u32| match h {
        CURRENT_THREAD => me,
        CURRENT_PROCESS => self_process(),
        h => norm(h),
    };
    // (on the stack for the usual few handles: a poll allocated three Vecs, review C-5)
    let mut small = [0u32; 8];
    let many: Vec<u32>;
    let handles: &[u32] = if handles.len() <= small.len() {
        for (d, &h) in small.iter_mut().zip(handles) {
            *d = n(h);
        }
        &small[..handles.len()]
    } else {
        many = handles.iter().map(|&h| n(h)).collect();
        &many
    };
    // One lock for what most waits are: a poll (0 ms), or an object signaled already. The
    // access check and the first try, as wait_held's first round would (it took the table's lock
    // four times). (Not while a save state or TerminateThread waits for this thread: those
    // stop it in wait_held.)
    if !kura::snap::pausing() && !crate::k32::terminating() {
        let first = with(|t| {
            // (a handle without SYNCHRONIZE cannot be waited on)
            if handles.iter().any(|h| t.objs.access.get(h).is_some_and(|a| a & 0x0010_0000 == 0)) {
                return Some(Err(5));
            }
            if let Some(x) = extra {
                if x() {
                    return Some(Ok(handles.len() as u32));
                }
            }
            match attempt(t, handles, all, tid, false) {
                Attempt::Done(r, _) => Some(Ok(r)),
                Attempt::Failed => Some(Err(6)),
                Attempt::Wait if ms == 0 => Some(Ok(WAIT_TIMEOUT)),
                Attempt::Wait => None,
            }
        });
        match first {
            Some(Ok(r)) => return r,
            Some(Err(e)) => {
                crate::k32::last_error(e);
                return WAIT_FAILED;
            }
            None => {}
        }
    } else if with(|t| handles.iter().any(|h| t.objs.access.get(h).is_some_and(|a| a & 0x0010_0000 == 0))) {
        crate::k32::last_error(5);
        return WAIT_FAILED;
    }
    // the wait holds its objects: one whose last handle closes meanwhile stays (a mutex then
    // abandoned is still WAIT_ABANDONED for this waiter)
    let keys: Vec<Option<u32>> = with(|t| {
        handles.iter().map(|h| {
            let k = t.objs.key(h)?;
            t.objs.map.get_mut(&k)?.1 += 1;
            Some(k)
        }).collect()
    });
    let held: Vec<u32> = handles.iter().zip(&keys).map(|(&h, k)| k.unwrap_or(h)).collect();
    let r = wait_held(&held, all, ms, extra);
    let gone: Vec<(u32, Obj)> = with(|t| keys.iter().flatten().filter_map(|&k| unref(t, k).map(|o| (k, o))).collect());
    for (k, o) in gone {
        gone_object(k, Some(o));
    }
    r
}

/// One try at a wait on `handles` (`all` of them, else the first signaled one).
enum Attempt {
    /// the result (WAIT_OBJECT_0 + i, WAIT_ABANDONED + i), and the signalers to defer to
    /// (only when `blocked`: a waiter that was woken defers to its signaler)
    Done(u32, Vec<waits::handoff::Mark>),
    /// a handle is not a waitable object (ERROR_INVALID_HANDLE)
    Failed,
    /// nothing to take yet
    Wait,
}

fn attempt(t: &mut Table, handles: &[u32], all: bool, tid: u32, blocked: bool) -> Attempt {
    if all {
        let mut ready = true;
        for &h in handles {
            match try_take(t, h, tid, false) {
                None => return Attempt::Failed,
                Some(b) => ready &= b,
            }
        }
        if !ready {
            return Attempt::Wait;
        }
        let ab = handles.iter().position(|&h| abandoned(t, h));
        for &h in handles {
            try_take(t, h, tid, true);
        }
        let marks: Vec<_> = if blocked { handles.iter().filter_map(|&h| signal_of(t, h)).collect() } else { Vec::new() };
        return Attempt::Done(ab.map_or(0, |_| 0x80), marks);
    }
    for (i, &h) in handles.iter().enumerate() {
        let ab = abandoned(t, h);
        match try_take(t, h, tid, true) {
            None => return Attempt::Failed,
            Some(true) => {
                // (woken by another thread's SetEvent: it runs on to its next call first)
                let m = if blocked { signal_of(t, h) } else { None };
                return Attempt::Done(i as u32 + if ab { 0x80 } else { 0 }, m.into_iter().collect());
            }
            Some(false) => {}
        }
    }
    Attempt::Wait
}

fn wait_held(handles: &[u32], all: bool, ms: u32, extra: Option<&dyn Fn() -> bool>) -> u32 {
    let tid = crate::cur().tid;
    let deadline = (ms != 0xffff_ffff).then(|| Instant::now() + Duration::from_millis(ms as u64));
    let cv = CV.with(Arc::clone);
    let mut g = lock();
    // (whether it waited: only a waiter that was woken defers to its signaler)
    let mut blocked = false;
    // (this wait's entry in the table's list while it has one)
    let mut id: Option<u64> = None;
    let (r, marks) = loop {
        // (a save state: the wait stands still here, holding nothing; see snap.rs)
        if kura::snap::pausing() {
            drop(g);
            crate::snap::point();
            g = lock();
        }
        // (TerminateThread ended this thread: its entry goes first, with what woke it)
        if crate::k32::terminating() {
            if let Some(id) = id.take() {
                g.as_mut().unwrap().leave(id);
            }
            drop(g);
            crate::k32::check_terminate();
            g = lock();
        }
        let t = g.get_or_insert_with(Table::new);
        if let Some(x) = extra {
            if x() {
                break (handles.len() as u32, Vec::new());
            }
        }
        match attempt(t, handles, all, tid, blocked) {
            Attempt::Failed => {
                crate::k32::last_error(6);
                break (WAIT_FAILED, Vec::new());
            }
            Attempt::Done(r, marks) => break (r, marks),
            Attempt::Wait => {}
        }
        // (timers turn set by themselves: wake when the first one is due)
        let timer = handles
            .iter()
            .filter_map(|h| match t.objs.get(h) {
                Some((Obj::Timer { due, .. }, _)) => *due,
                _ => None,
            })
            .min();
        let deadline = match (deadline, timer) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Some(d) = deadline {
            if Instant::now() >= d {
                if timer == Some(d) {
                    continue;
                }
                break (WAIT_TIMEOUT, Vec::new());
            }
        }
        // sleeps until one of its objects (or anything, `signal`) changes
        let me = match id {
            Some(me) => {
                t.waits.rearm(me);
                me
            }
            None => {
                let me = t.waits.add(handles, all);
                t.cvs.insert(me, cv.clone());
                id = Some(me);
                me
            }
        };
        blocked = true;
        match deadline {
            None => {
                while !g.as_ref().unwrap().waits.poked(me) {
                    g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            }
            Some(d) => {
                let now = Instant::now();
                if now < d {
                    g = cv.wait_timeout(g, d - now).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        }
    };
    if let Some(id) = id {
        g.as_mut().unwrap().leave(id);
    }
    drop(g);
    for m in marks {
        defer_to(m, tid);
    }
    r
}

/// `wait`'s one-lock first try (review C-5) against the way it waited before: the access check,
/// then wait_held's rounds. Same result, same last error, same objects after, for any mix of
/// events, mutexes, semaphores, bad handles and handles without SYNCHRONIZE.
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod wait_tests {
    use super::*;
    use crate::test_guest::{enter, guest_thread};
    use proptest::prelude::*;

    #[derive(Clone, Copy, Debug)]
    enum Spec {
        Event { manual: bool, set: bool },
        /// 0 none, 1 this thread (count 1), 2 another thread, 3 abandoned
        Mutex(u8),
        Sem(i32),
        Bad,
        NoSync,
    }

    fn spec() -> impl Strategy<Value = Spec> {
        prop_oneof![
            4 => (any::<bool>(), any::<bool>()).prop_map(|(manual, set)| Spec::Event { manual, set }),
            3 => (0u8..4).prop_map(Spec::Mutex),
            2 => (0i32..3).prop_map(Spec::Sem),
            1 => Just(Spec::Bad),
            1 => Just(Spec::NoSync),
        ]
    }

    fn make(specs: &[Spec]) -> Vec<u32> {
        let me = crate::cur().tid;
        specs
            .iter()
            .map(|s| match *s {
                Spec::Event { manual, set } => add(Obj::Event { manual, set }),
                Spec::Mutex(k) => add(Obj::Mutex { owner: [0, me, me + 0x1000, ABANDONED][k as usize], count: (k == 1) as u32 }),
                Spec::Sem(count) => add(Obj::Semaphore { count, max: 4 }),
                Spec::Bad => 0x7ff0_0000,
                Spec::NoSync => {
                    let h = add(Obj::Event { manual: true, set: true });
                    set_access(h, 0x0002);
                    h
                }
            })
            .collect()
    }

    fn state(hs: &[u32]) -> Vec<String> {
        with(|t| {
            hs.iter()
                .map(|h| match t.objs.get(h) {
                    Some((Obj::Event { manual, set }, _)) => format!("event {manual} {set}"),
                    Some((Obj::Mutex { owner, count }, _)) => format!("mutex {owner:#x} {count}"),
                    Some((Obj::Semaphore { count, .. }, _)) => format!("sem {count}"),
                    Some(_) => "other".into(),
                    None => "none".into(),
                })
                .collect()
        })
    }

    /// The wait as it was: access check, then wait_held
    fn reference(hs: &[u32], all: bool, ms: u32) -> u32 {
        if with(|t| hs.iter().any(|h| t.objs.access.get(h).is_some_and(|a| a & 0x0010_0000 == 0))) {
            crate::k32::last_error(5);
            return WAIT_FAILED;
        }
        wait_held(hs, all, ms, None)
    }

    fn run(specs: &[Spec], all: bool, new: bool) -> (u32, u32, Vec<String>) {
        let hs = make(specs);
        crate::k32::last_error(0);
        let r = if new { wait(&hs, all, 0, None) } else { reference(&hs, all, 0) };
        let e = crate::k32::get_last_error();
        let st = state(&hs);
        for &h in &hs {
            close(h);
        }
        (r, if r == WAIT_FAILED { e } else { 0 }, st)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]
        #[test]
        fn poll_same_as_before(specs in proptest::collection::vec(spec(), 1..12), all in any::<bool>()) {
            let _g = enter();
            guest_thread();
            let new = run(&specs, all, true);
            let old = run(&specs, all, false);
            // (a mutex this thread owns counts up once more: the same either way)
            prop_assert_eq!(new, old, "{:?} all={}", specs, all);
        }
    }

    #[test]
    fn poll_results() {
        let _g = enter();
        guest_thread();
        let e = add(Obj::Event { manual: false, set: false });
        assert_eq!(wait(&[e], false, 0, None), WAIT_TIMEOUT);
        with(|t| if let Some((Obj::Event { set, .. }, _)) = t.objs.get_mut(&e) { *set = true });
        assert_eq!(wait(&[e], false, 0, None), 0);
        // (auto-reset: taken)
        assert_eq!(wait(&[e], false, 0, None), WAIT_TIMEOUT);
        let m = add(Obj::Mutex { owner: ABANDONED, count: 0 });
        assert_eq!(wait(&[e, m], false, 0, None), 0x81);
        // an extra condition answers before the objects (the message queue)
        assert_eq!(wait(&[e], false, 0, Some(&|| true)), 1);
        // more handles than the stack's room
        let many: Vec<u32> = (0..20).map(|_| add(Obj::Event { manual: true, set: true })).collect();
        assert_eq!(wait(&many, true, 0, None), 0);
        let mut more = many.clone();
        more.push(e);
        assert_eq!(wait(&more, true, 0, None), WAIT_TIMEOUT);
        assert_eq!(wait(&more, false, 0, None), 0);
        assert_eq!(wait(&[0x7ff0_0004], false, 0, None), WAIT_FAILED);
        assert_eq!(crate::k32::get_last_error(), 6);
        for h in many.into_iter().chain([e, m]) {
            close(h);
        }
    }

    /// A wait that does not find its object signaled still blocks until another thread sets it
    /// (and holds the object meanwhile: closing its handle does not lose the wake).
    #[test]
    fn blocking_wait_wakes() {
        let _g = enter();
        guest_thread();
        let e = add(Obj::Event { manual: false, set: false });
        let d = dup(e);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            with(|t| if let Some((Obj::Event { set, .. }, _)) = t.objs.get_mut(&d) { *set = true });
            signal();
        });
        let start = Instant::now();
        assert_eq!(wait(&[e], false, 5000, None), 0);
        assert!(start.elapsed() >= Duration::from_millis(40));
        t.join().unwrap();
        // timed out, not signaled
        assert_eq!(wait(&[e], false, 30, None), WAIT_TIMEOUT);
        close(e);
        close(d);
    }
}
