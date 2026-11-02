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
    Process,
    File(Arc<crate::files::File>),
    Find(Arc<Mutex<crate::files::Find>>),
    Mapping { file: Option<Arc<crate::files::File>>, size: u32, view: u32 },
    Key(crate::misc::Key),
    Other(&'static str),
}

pub struct Table {
    pub objs: HashMap<u32, (Obj, u32)>,
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
    f(g.get_or_insert_with(|| Table { objs: HashMap::default(), next: 0x100, names: HashMap::default(), gen: 0 }))
}

pub const CURRENT_PROCESS: u32 = 0xffff_ffff;
pub const CURRENT_THREAD: u32 = 0xffff_fffe;

pub fn add(o: Obj) -> u32 {
    with(|t| {
        let h = t.next;
        t.next += 4;
        t.objs.insert(h, (o, 1));
        h
    })
}

/// Another handle to the same object (DuplicateHandle): objects are refcounted by handle.
pub fn dup(h: u32) -> u32 {
    with(|t| {
        let Some((_, rc)) = t.objs.get_mut(&h) else { return 0 };
        *rc += 1;
        h
    })
}

pub fn close(h: u32) -> bool {
    with(|t| match t.objs.get_mut(&h) {
        Some((_, rc)) if *rc > 1 => {
            *rc -= 1;
            true
        }
        Some(_) => {
            t.objs.remove(&h);
            t.names.retain(|_, v| *v != h);
            true
        }
        None => false,
    })
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
            if *owner == 0 || *owner == tid {
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
        Obj::Process => false,
        // files and the like count as signaled
        _ => true,
    })
}

pub const WAIT_TIMEOUT: u32 = 0x102;
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
        let t = g.get_or_insert_with(|| Table { objs: HashMap::default(), next: 0x100, names: HashMap::default(), gen: 0 });
        if let Some(x) = extra {
            if x() {
                return handles.len() as u32;
            }
        }
        if all {
            let ok: Option<Vec<bool>> = handles.iter().map(|&h| try_take(t, h, tid, false)).collect();
            match ok {
                None => return WAIT_FAILED,
                Some(v) if v.iter().all(|&b| b) => {
                    for &h in &handles {
                        try_take(t, h, tid, true);
                    }
                    return 0;
                }
                _ => {}
            }
        } else {
            for (i, &h) in handles.iter().enumerate() {
                match try_take(t, h, tid, true) {
                    None => return WAIT_FAILED,
                    Some(true) => return i as u32,
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
