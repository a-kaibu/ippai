//! Performance counter providers and consumers (advapi32 Perf*, perflib V2).
//!
//! A provider (PerfStartProvider(Ex)) is a guest block (Windows' handle points at the
//! provider's data, not at a kernel object). It registers counter sets
//! (PerfSetCounterSetInfo); an instance (PerfCreateInstance) is PERF_COUNTERSET_INSTANCE
//! followed by one 8-byte slot per counter and the instance's name; the value setters write
//! the slot: a pointer for PERF_ATTRIB_BY_REFERENCE counters, else a DWORD or a ULONGLONG by
//! the counter type's size bits. Consumers (PerfOpenQueryHandle) see no counter set of
//! this process: added counters get ERROR_WMI_GUID_NOT_FOUND, a query returns only the header.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, r32, w32, wstr};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_WMI_GUID_NOT_FOUND: u32 = 4200;

const ATTRIB_BY_REFERENCE: u64 = 1;
const SIZE_MASK: u32 = 0x300;
const SIZE_DWORD: u32 = 0;
const SIZE_LARGE: u32 = 0x100;

/// sizeof(PERF_COUNTERSET_INFO), PERF_COUNTER_INFO, PERF_COUNTERSET_INSTANCE
const SET_INFO: u32 = 40;
const COUNTER_INFO: u32 = 32;
const INSTANCE: u32 = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Counter {
    pub id: u32,
    pub ty: u32,
    pub attrib: u64,
}

#[derive(Default)]
struct Provider {
    /// counter set GUID -> its counters
    sets: HashMap<[u8; 16], Vec<Counter>>,
    /// instance block -> counter set
    instances: HashMap<u32, [u8; 16]>,
}

static PROVIDERS: Mutex<Option<HashMap<u32, Provider>>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut HashMap<u32, Provider>) -> R) -> R {
    f(PROVIDERS.lock().unwrap().get_or_insert_with(HashMap::new))
}

/// How a value setter may write counter `c`: Ok(slot is a pointer / DWORD / ULONGLONG)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slot {
    Ref,
    Dword,
    Large,
}
pub fn slot(c: &Counter) -> Option<Slot> {
    if c.attrib & ATTRIB_BY_REFERENCE != 0 {
        return Some(Slot::Ref);
    }
    match c.ty & SIZE_MASK {
        SIZE_DWORD => Some(Slot::Dword),
        SIZE_LARGE => Some(Slot::Large),
        _ => None,
    }
}

/// The size of an instance block: header, the slots, the name (with its NUL), to 8 bytes
pub fn instance_size(counters: u32, name_bytes: u32) -> u32 {
    (INSTANCE + 8 * counters + name_bytes + 7) & !7
}

fn guid(p: u32) -> [u8; 16] {
    mem::slice(p, 16).try_into().unwrap()
}

fn start(c: &mut Ctx, g: u32, out: u32) -> u32 {
    if g == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    // (a block of the provider's own: its GUID)
    let h = mem::heap_alloc(32, true);
    mem::write(h, mem::slice(g, 16));
    with(|m| m.insert(h, Provider::default()));
    mem::putp(out, h);
    let _ = c;
    0
}
/// PerfStartProvider(provider GUID, callback, &handle)
fn PerfStartProvider(c: &mut Ctx) -> u32 {
    let (g, out) = (c.a(0), c.a(2));
    start(c, g, out)
}
/// PerfStartProviderEx(provider GUID, PERF_PROVIDER_CONTEXT, &handle)
fn PerfStartProviderEx(c: &mut Ctx) -> u32 {
    let (g, ctx, out) = (c.a(0), c.a(1), c.a(2));
    let need = if mem::x64() { 40 } else { 24 };
    if ctx == 0 || r32(ctx) < need {
        return ERROR_INVALID_PARAMETER;
    }
    start(c, g, out)
}
fn PerfStopProvider(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let Some(p) = with(|m| m.remove(&h)) else { return ERROR_INVALID_PARAMETER };
    for i in p.instances.keys() {
        mem::heap_free(*i);
    }
    mem::heap_free(h);
    0
}

/// PerfSetCounterSetInfo(provider, PERF_COUNTERSET_INFO + PERF_COUNTER_INFO[], size)
fn PerfSetCounterSetInfo(c: &mut Ctx) -> u32 {
    let (h, t, size) = (c.a(0), c.a(1), c.a(2));
    if t == 0 || size < SET_INFO {
        return ERROR_INVALID_PARAMETER;
    }
    let n = r32(t + 32);
    if n == 0 || size < SET_INFO + COUNTER_INFO * n {
        return ERROR_INVALID_PARAMETER;
    }
    let counters: Vec<Counter> = (0..n)
        .map(|i| {
            let e = t + SET_INFO + COUNTER_INFO * i;
            Counter { id: r32(e), ty: r32(e + 4), attrib: mem::r64(e + 8) }
        })
        .collect();
    let g = guid(t);
    with(|m| {
        let Some(p) = m.get_mut(&h) else { return ERROR_INVALID_PARAMETER };
        if p.sets.contains_key(&g) {
            return ERROR_ALREADY_EXISTS;
        }
        p.sets.insert(g, counters);
        0
    })
}

/// PerfCreateInstance(provider, counter set GUID, name, id): the instance block, or NULL with
/// the error (a success leaves the last error alone)
fn PerfCreateInstance(c: &mut Ctx) -> u32 {
    let (h, g, name, id) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if g == 0 || name == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let g = guid(g);
    let Some(n) = with(|m| m.get(&h).and_then(|p| p.sets.get(&g)).map(|s| s.len() as u32)) else {
        last_error(ERROR_NOT_FOUND);
        return 0;
    };
    let w = wstr(name);
    let name_bytes = 2 * w.len() as u32 + 2;
    let size = instance_size(n, name_bytes);
    let b = mem::heap_alloc(size, true);
    mem::write(b, &g);
    w32(b + 16, size);
    w32(b + 20, id);
    w32(b + 24, INSTANCE + 8 * n);
    w32(b + 28, name_bytes);
    mem::put_wstr(b + INSTANCE + 8 * n, w.len() as u32 + 1, &w);
    with(|m| m.get_mut(&h).map(|p| p.instances.insert(b, g)));
    b
}
fn PerfDeleteInstance(c: &mut Ctx) -> u32 {
    let (h, i) = (c.a(0), c.a(1));
    if with(|m| m.get_mut(&h).and_then(|p| p.instances.remove(&i))).is_none() {
        return ERROR_INVALID_PARAMETER;
    }
    mem::heap_free(i);
    0
}

/// The slot of counter `id` in instance `i`, if the counter is there and may be set as `want`
fn counter_slot(h: u32, i: u32, id: u32, want: Slot) -> Result<u32, u32> {
    with(|m| {
        let p = m.get(&h).ok_or(ERROR_INVALID_PARAMETER)?;
        let set = p.instances.get(&i).ok_or(ERROR_INVALID_PARAMETER)?;
        let cs = &p.sets[set];
        let k = cs.iter().position(|c| c.id == id).ok_or(ERROR_NOT_FOUND)?;
        if slot(&cs[k]) != Some(want) {
            return Err(ERROR_INVALID_PARAMETER);
        }
        Ok(i + INSTANCE + 8 * k as u32)
    })
}
/// PerfSetCounterRefValue(provider, instance, id, address)
fn PerfSetCounterRefValue(c: &mut Ctx) -> u32 {
    match counter_slot(c.a(0), c.a(1), c.a(2), Slot::Ref) {
        Ok(s) => {
            mem::putp(s, c.a(3));
            0
        }
        Err(e) => e,
    }
}
/// PerfSetULongCounterValue(provider, instance, id, value)
fn PerfSetULongCounterValue(c: &mut Ctx) -> u32 {
    match counter_slot(c.a(0), c.a(1), c.a(2), Slot::Dword) {
        Ok(s) => {
            w32(s, c.a(3));
            0
        }
        Err(e) => e,
    }
}
/// PerfSetULongLongCounterValue(provider, instance, id, value)
fn PerfSetULongLongCounterValue(c: &mut Ctx) -> u32 {
    let v = if mem::x64() { c.a64(3) } else { c.a(3) as u64 | (c.a(4) as u64) << 32 };
    match counter_slot(c.a(0), c.a(1), c.a(2), Slot::Large) {
        Ok(s) => {
            mem::w64(s, v);
            0
        }
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------------------------
// Consumers

/// Query handles: blocks of their own
static QUERIES: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn PerfOpenQueryHandle(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let h = mem::heap_alloc(16, true);
    QUERIES.lock().unwrap().push(h);
    mem::putp(out, h);
    0
}
fn PerfCloseQueryHandle(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let mut q = QUERIES.lock().unwrap();
    let Some(i) = q.iter().position(|&x| x == h) else { return 6 };
    q.remove(i);
    mem::heap_free(h);
    0
}
/// PerfAddCounters(query, PERF_COUNTER_IDENTIFIER list, size): each {GUID, Status, Size, ...}
fn PerfAddCounters(c: &mut Ctx) -> u32 {
    let (h, ids, size) = (c.a(0), c.a(1), c.a(2));
    if !QUERIES.lock().unwrap().contains(&h) {
        return 6;
    }
    if ids == 0 || size < 40 {
        return ERROR_INVALID_PARAMETER;
    }
    let mut at = 0;
    while at + 40 <= size {
        let e = ids + at;
        let n = r32(e + 20);
        if n < 40 || at + n > size {
            return ERROR_INVALID_PARAMETER;
        }
        w32(e + 16, ERROR_WMI_GUID_NOT_FOUND);
        at += n;
    }
    0
}
/// PerfQueryCounterData(query, PERF_DATA_HEADER buffer, size, &needed): the header alone
fn PerfQueryCounterData(c: &mut Ctx) -> u32 {
    let (h, buf, size, need) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if need == 0 || !QUERIES.lock().unwrap().contains(&h) {
        return ERROR_INVALID_PARAMETER;
    }
    w32(need, 48);
    if buf == 0 || size < 48 {
        return ERROR_NOT_ENOUGH_MEMORY;
    }
    let ts = crate::k32::qpc();
    let ft = crate::files::filetime(std::time::SystemTime::now());
    mem::fill(buf, 48, 0);
    w32(buf, 48);
    mem::w64(buf + 8, ts);
    mem::w64(buf + 16, ft);
    mem::w64(buf + 24, 10_000_000);
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    crate::k32::write_systemtime(buf + 32, secs, ((ft / 10_000) % 1000) as u32);
    0
}

pub fn init() {
    reg!("advapi32";
        PerfStartProvider, PerfStartProviderEx, PerfStopProvider, PerfSetCounterSetInfo, PerfCreateInstance,
        PerfDeleteInstance, PerfSetCounterRefValue, PerfSetULongCounterValue, PerfSetULongLongCounterValue,
        PerfOpenQueryHandle, PerfCloseQueryHandle, PerfAddCounters, PerfQueryCounterData,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn slots_by_type() {
        let c = |ty, attrib| Counter { id: 1, ty, attrib };
        // PERF_COUNTER_COUNTER is a DWORD rate; BY_REFERENCE wins over the size
        assert_eq!(slot(&c(0x10410400, 0)), Some(Slot::Dword));
        assert_eq!(slot(&c(0x10410500, 0)), Some(Slot::Large));
        assert_eq!(slot(&c(0x10410400, 1)), Some(Slot::Ref));
        assert_eq!(slot(&c(0x10410600, 0)), None);
        assert_eq!(instance_size(2, 6), 56);
    }

    proptest! {
        /// An instance block holds its header, every slot and the name, 8-byte aligned
        #[test]
        fn instance_sizes(n in 0u32..64, chars in 0u32..256) {
            let s = instance_size(n, 2 * chars + 2);
            prop_assert_eq!(s % 8, 0);
            prop_assert!(s >= INSTANCE + 8 * n + 2 * chars + 2 && s < INSTANCE + 8 * n + 2 * chars + 2 + 8);
        }
    }
}
