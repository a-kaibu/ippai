//! Kernel objects behind handles, and waiting on them.
//!
//! One lock and one condition variable for everything waitable (events, mutexes, semaphores,
//! threads, message queues): any change wakes all waiters, which then recheck. Simple, and
//! games have few threads.

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
    /// this process (DuplicateHandle of GetCurrentProcess, OpenProcess of our id): the access
    /// it was opened with
    Process { access: u32 },
    File(Arc<crate::files::File>),
    /// an end of a pipe (crate::kern)
    Pipe(crate::pipe::Handle),
    Find(Arc<Mutex<crate::files::Find>>),
    Mapping { file: Option<Arc<crate::files::File>>, size: u32, view: u32 },
    Key(crate::misc::Key),
    Other(&'static str),
}

/// Objects and the handles to them: every handle is its own number (DuplicateHandle and opening
/// a named object give a new one, closing one leaves the others valid); an object is kept under
/// the number of its first handle, with the count of its open handles.
#[derive(Default)]
pub struct Objs {
    map: HashMap<u32, (Obj, u32)>,
    handles: HashMap<u32, u32>,
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

pub struct Table {
    pub objs: Objs,
    next: u32,
    /// named objects: name -> handle of the first one
    pub names: HashMap<String, u32>,
    pub gen: u64,
}

pub static TABLE: Mutex<Option<Table>> = Mutex::new(None);
pub static WAKE: Condvar = Condvar::new();

pub fn lock() -> MutexGuard<'static, Option<Table>> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn with<R>(f: impl FnOnce(&mut Table) -> R) -> R {
    let mut g = lock();
    f(g.get_or_insert_with(|| Table { objs: Objs::default(), next: 0x100, names: HashMap::default(), gen: 0 }))
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
    let (r, gone) = with(|t| {
        let Some(k) = t.objs.handles.remove(&h) else { return (false, None) };
        let rc = &mut t.objs.map.get_mut(&k).unwrap().1;
        *rc -= 1;
        let mut gone = None;
        if *rc == 0 {
            gone = t.objs.map.remove(&k).map(|o| o.0);
            t.names.retain(|_, v| *v != k);
        }
        (true, gone)
    });
    // (outside the table's lock: the kernel's comes after it, never before)
    if let Some(Obj::Pipe(p)) = gone {
        crate::pipe::closed(p);
    }
    r
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
    with(|t| t.gen += 1);
    WAKE.notify_all();
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
        Obj::Process { .. } => false,
        // files and the like count as signaled
        _ => true,
    })
}

pub const WAIT_TIMEOUT: u32 = 0x102;
/// a mutex whose owner thread ended holding it (until the next wait takes it)
pub const ABANDONED: u32 = u32::MAX;

fn abandoned(t: &Table, h: u32) -> bool {
    matches!(t.objs.get(&h), Some((Obj::Mutex { owner: ABANDONED, .. }, _)))
}
pub const WAIT_FAILED: u32 = 0xffff_ffff;

/// WaitForMultipleObjects; `extra` is an extra wake condition (the message queue), checked
/// under the lock, which makes the result `handles.len()`.
pub fn wait(handles: &[u32], all: bool, ms: u32, extra: Option<&dyn Fn() -> bool>) -> u32 {
    let tid = crate::cur().tid;
    let me = crate::cur().obj;
    let handles: Vec<u32> = handles.iter().map(|&h| if h == CURRENT_THREAD { me } else { h }).collect();
    let deadline = (ms != 0xffff_ffff).then(|| Instant::now() + Duration::from_millis(ms as u64));
    let mut g = lock();
    loop {
        let t = g.get_or_insert_with(|| Table { objs: Objs::default(), next: 0x100, names: HashMap::default(), gen: 0 });
        if let Some(x) = extra {
            if x() {
                return handles.len() as u32;
            }
        }
        if all {
            let ok: Option<Vec<bool>> = handles.iter().map(|&h| try_take(t, h, tid, false)).collect();
            match ok {
                None => {
                    crate::k32::last_error(6);
                    return WAIT_FAILED;
                }
                Some(v) if v.iter().all(|&b| b) => {
                    let ab = handles.iter().position(|&h| abandoned(t, h));
                    for &h in &handles {
                        try_take(t, h, tid, true);
                    }
                    return ab.map_or(0, |_| 0x80);
                }
                _ => {}
            }
        } else {
            for (i, &h) in handles.iter().enumerate() {
                let ab = abandoned(t, h);
                match try_take(t, h, tid, true) {
                    None => {
                        crate::k32::last_error(6);
                        return WAIT_FAILED;
                    }
                    Some(true) => return i as u32 + if ab { 0x80 } else { 0 },
                    Some(false) => {}
                }
            }
        }
        let gen = t.gen;
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
        match deadline {
            None => {
                while g.as_ref().unwrap().gen == gen {
                    g = WAKE.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            }
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    if timer == Some(d) {
                        continue;
                    }
                    return WAIT_TIMEOUT;
                }
                g = WAKE.wait_timeout(g, d - now).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }
}
