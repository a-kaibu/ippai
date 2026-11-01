//! The x86 -> wasm JIT on the host side: per thread, a block hash table read by the
//! dispatcher module (`vnx86::jit::dispatch`), and the helpers block modules import
//! (`vnhost_jit_step`, `vnhost_jit_eflags`, `vnhost_jit_status`). The modules themselves are
//! compiled and instantiated by vnjit.js; each thread has its own funcref table.
//!
//! VNVM_JIT=0 turns the JIT off (the interpreter runs everything). VNVM_JIT_DUMP=dir writes every
//! region module to dir/<first root>-<roots>.wasm. VNVM_JIT_LAYOUT=1 prints the block order, loops
//! and wasm bytes per mnemonic of new regions (of merged ones too with VNVM_JIT_STATS).
//!
//! Regions are kept across runs: with VNVM_JIT_URL set, the regions built since the last save are
//! sent there now and then (see [`maybe_save`]; vnvm-server merges them into what it has), and the
//! page (or pre.js under node) hands the next run that module and index (`jit::saved`). A
//! saved region goes in when its first root first misses, if the guest code is still the same:
//! no interpreter warm-up and no translation, and in Chromium the browser's code cache of the
//! module keeps V8's optimized code too.

use crate::cpu::Linear;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vnx86::jit::{self, wasm::Module};
use vnx86::{Cpu, Exit};

/// Misses at an address before its block is compiled (VNVM_JIT_THRESHOLD overrides).
const THRESHOLD: u32 = 50;

fn threshold() -> u32 {
    use std::sync::OnceLock;
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| std::env::var("VNVM_JIT_THRESHOLD").ok().and_then(|v| v.parse().ok()).unwrap_or(THRESHOLD))
}
/// Debugging a miscompile by bisection: VNVM_JIT_ONLY=lo-hi (hex) compiles only code in [lo, hi).
fn debug_allowed(a: u32) -> bool {
    use std::sync::OnceLock;
    static R: OnceLock<(u32, u32)> = OnceLock::new();
    let (lo, hi) = *R.get_or_init(|| {
        let v = std::env::var("VNVM_JIT_ONLY").unwrap_or_default();
        let mut it = v.split('-').map(|x| u32::from_str_radix(x.trim_start_matches("0x"), 16).ok());
        match (it.next().flatten(), it.next().flatten()) {
            (Some(lo), Some(hi)) => (lo, hi),
            _ => (0, u32::MAX),
        }
    });
    a >= lo && a < hi
}
/// Blocks the dispatcher runs before returning to the run loop.
const BUDGET: i32 = 1 << 14;
/// Most blocks compiled into one module.
const MAX_BLOCKS: usize = 1024;

#[cfg(target_os = "emscripten")]
extern "C" {
    fn vnvm_jit_init(ptr: *const u8, len: usize);
    fn vnvm_jit_load(ptr: *const u8, len: usize, n: u32) -> i32;
    fn vnvm_jit_run(cpu: *mut Cpu) -> i32;
    fn vnvm_jit_bundle_idx(ptr: *mut u8, cap: usize) -> i32;
    fn vnvm_jit_bundle_load() -> i32;
    fn vnvm_jit_save(url: *const std::ffi::c_char, ptr: *const u8, len: usize);
}

struct State {
    /// (eip, table index); eip 0 is a free entry
    hash: Vec<[u32; 2]>,
    used: usize,
    misses: HashMap<u32, u32>,
    /// addresses with a block, or that could not be translated
    known: HashSet<u32>,
    /// region (table index) -> the addresses it was built from
    roots: HashMap<u32, Vec<u32>>,
    /// jumps between regions, written by the regions: count, then (from, to) pairs
    ring: Box<[u32; 2 + 2 * jit::RING_SIZE as usize]>,
    /// (region, target) pairs already merged, and targets that did not fit
    merged: HashSet<(u32, u32)>,
    /// ring windows each hot jump topped so far
    hot: HashMap<(u32, u32), u32>,
    merges: u32,
    /// table index of the saved regions' first function on this thread, once instantiated
    saved: Option<i32>,
    /// entries of functions (jit::FN): regions never take their hash entry
    fns: HashSet<u32>,
    /// times each function was built
    fn_builds: HashMap<u32, u32>,
}

/// Samples of the same jump between regions (out of RING_SIZE) that make them merge.
const MERGE_SAMPLES: usize = 12;
/// Ring windows a jump must top that way before the merge: startup code has jumps that are
/// hot for a moment only, and each merge is a new region V8 compiles again.
const MERGE_WINDOWS: u32 = 8;
/// The same for functions (a function built again, or a callee becoming one).
const FN_SAMPLES: usize = 6;
const FN_WINDOWS: u32 = 2;
/// Builds of one function, at most.
const FN_BUILDS: u32 = 8;
/// Merges per thread, at most (each compiles a new region).
const MAX_MERGES: u32 = 512;

/// A region module any thread built, kept for the others: every thread (and every process,
/// for the DLLs they share) would otherwise translate the same code again.
struct Shared {
    bytes: Vec<u8>,
    starts: Vec<(u32, u32)>,
    code: Vec<(u32, u32)>,
    /// the guest code bytes of `code`, concatenated
    text: Vec<u8>,
}

/// Shared regions by their roots.
static SHARED: Mutex<Option<HashMap<Vec<u32>, Vec<Arc<Shared>>>>> = Mutex::new(None);

fn code_bytes(mem: &mut Linear, code: &[(u32, u32)]) -> Option<Vec<u8>> {
    let mut text = Vec::new();
    for &(a, n) in code {
        let at = text.len();
        text.resize(at + n as usize, 0);
        if vnx86::Memory::fetch(mem, a, &mut text[at..]) != n as usize {
            return None;
        }
    }
    Some(text)
}

/// A region for `roots` some thread built from the same code as ours.
fn shared(mem: &mut Linear, roots: &[u32]) -> Option<Arc<Shared>> {
    let list = SHARED.lock().unwrap().as_ref()?.get(roots)?.clone();
    list.into_iter().find(|r| code_bytes(mem, &r.code).is_some_and(|t| t == r.text))
}

fn share(mem: &mut Linear, roots: &[u32], bytes: Vec<u8>, region: &jit::Region) {
    let Some(text) = code_bytes(mem, &region.code) else { return };
    let r = Arc::new(Shared { bytes, starts: region.starts.clone(), code: region.code.clone(), text });
    SHARED.lock().unwrap().get_or_insert_with(HashMap::new).entry(roots.to_vec()).or_default().push(r);
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static EXIT: Cell<Option<Exit>> = const { Cell::new(None) };
}

pub fn enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| cfg!(target_os = "emscripten") && std::env::var("VNVM_JIT").map_or(true, |v| v != "0"))
}

pub enum Ran {
    /// budget used up or a signal pending
    Stopped,
    Exit(Exit),
    /// no block at cpu.eip, or a block wants the interpreter
    Missed,
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.get_or_insert_with(|| {
            let d = jit::dispatch::module();
            #[cfg(target_os = "emscripten")]
            unsafe {
                vnvm_jit_init(d.as_ptr(), d.len())
            };
            let _ = d;
            State {
                hash: vec![[0; 2]; 1 << 12],
                used: 0,
                misses: HashMap::new(),
                known: HashSet::new(),
                roots: HashMap::new(),
                ring: Box::new([0; 2 + 2 * jit::RING_SIZE as usize]),
                merged: HashSet::new(),
                hot: HashMap::new(),
                merges: 0,
                saved: None,
                fns: HashSet::new(),
                fn_builds: HashMap::new(),
            }
        });
        f(st)
    })
}

/// Switches read once: VNVM_JIT_COUNT=<secs> (transfer counters), VNVM_JIT_CALLS=1|2,
/// VNVM_JIT_NOCHECK=1 (measuring only: no range checks).
fn init_flags() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(secs) = std::env::var("VNVM_JIT_COUNT").ok().and_then(|v| v.parse::<u64>().ok()) {
            use std::sync::atomic::Ordering::Relaxed;
            if !jit::COUNT.swap(true, Relaxed) {
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(secs.max(1)));
                    let v: Vec<String> = jit::COUNTERS.iter().zip(jit::COUNTER_NAMES).map(|(c, n)| format!("{n}={}", c.swap(0, Relaxed))).collect();
                    eprintln!("vnjit count/{secs}s {}", v.join(" "));
                });
            }
        }
        // CALL/RET as wasm calls: 1 direct calls, 2 indirect ones too
        let calls = std::env::var("VNVM_JIT_CALLS").unwrap_or_default();
        jit::CALLS.store(calls == "1" || calls == "2", std::sync::atomic::Ordering::Relaxed);
        jit::INDIRECT_CALLS.store(calls == "2", std::sync::atomic::Ordering::Relaxed);
        jit::FN.store(std::env::var("VNVM_JIT_FN").is_ok_and(|v| v == "1"), std::sync::atomic::Ordering::Relaxed);
    });
}

pub fn run(cpu: &mut Cpu, pending: u32) -> Ran {
    init_flags();
    with_state(|st| {
        if st.ring[0] >= jit::RING_SIZE {
            merge_hot(st, cpu);
        }
        cpu.jit_hash = st.hash.as_ptr() as usize as u32;
        cpu.jit_mask = st.hash.len() as u32 - 1;
        cpu.jit_ring = st.ring.as_mut_ptr() as usize as u32;
    });
    cpu.jit_pending = pending;
    cpu.jit_budget = BUDGET;
    let depth = cpu.jit_depth;
    #[cfg(target_os = "emscripten")]
    let r = unsafe { vnvm_jit_run(cpu) };
    cpu.jit_depth = depth;
    #[cfg(not(target_os = "emscripten"))]
    let r = jit::RET_INTERP;
    match r {
        jit::RET_CONTINUE => Ran::Stopped,
        jit::RET_INTERP => Ran::Missed,
        _ => Ran::Exit(EXIT.with(|e| e.take()).unwrap_or(Exit::Continue)),
    }
}

/// Sets the entry of `eip`; true if it is new.
fn insert(hash: &mut [[u32; 2]], eip: u32, idx: u32) -> bool {
    let mask = hash.len() as u32 - 1;
    let mut h = jit::dispatch::slot(eip, mask);
    while hash[h as usize][0] != 0 && hash[h as usize][0] != eip {
        h = (h + 1) & mask;
    }
    let new = hash[h as usize][0] == 0;
    hash[h as usize] = [eip, idx];
    new
}

/// Compiles `module` and points the hash table at its region's blocks.
fn load(st: &mut State, bytes: &[u8], roots: Vec<u32>, starts: &[(u32, u32)]) -> bool {
    if let Some(dir) = std::env::var_os("VNVM_JIT_DUMP") {
        let name = format!("{:08x}-{}.wasm", roots[0], roots.len());
        let _ = std::fs::write(std::path::Path::new(&dir).join(name), bytes);
    }
    #[cfg(target_os = "emscripten")]
    let base = unsafe { vnvm_jit_load(bytes.as_ptr(), bytes.len(), 1) };
    #[cfg(not(target_os = "emscripten"))]
    let base = { let _ = bytes; -1 };
    if base < 0 {
        return false;
    }
    enter(st, base as u32, roots, starts);
    true
}

/// Points the hash table at the blocks of the region at table index `base`.
fn enter(st: &mut State, base: u32, roots: Vec<u32>, starts: &[(u32, u32)]) {
    for &(a, _) in starts {
        st.known.insert(a);
    }
    while (st.used + starts.len()) * 2 > st.hash.len() {
        let mut bigger = vec![[0u32; 2]; st.hash.len() * 2];
        for e in st.hash.iter().filter(|e| e[0] != 0) {
            insert(&mut bigger, e[0], e[1]);
        }
        st.hash = bigger;
    }
    for &(a, i) in starts {
        if st.fns.contains(&a) {
            continue;
        }
        if insert(&mut st.hash, a, base | i << jit::ENTRY_SHIFT) {
            st.used += 1;
        }
    }
    st.roots.insert(base, roots);
}

/// The saved regions (empty without any).
struct Saved {
    entries: Vec<jit::saved::Entry>,
    /// entries by their first root
    by_root: HashMap<u32, Vec<usize>>,
}

fn saved_regions() -> &'static Saved {
    static S: std::sync::OnceLock<Saved> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        #[cfg(target_os = "emscripten")]
        let entries = unsafe {
            let n = vnvm_jit_bundle_idx(std::ptr::null_mut(), 0);
            let mut b = vec![0u8; n.max(0) as usize];
            if n > 0 {
                vnvm_jit_bundle_idx(b.as_mut_ptr(), b.len());
            }
            jit::saved::decode_all(&b).unwrap_or_default()
        };
        #[cfg(not(target_os = "emscripten"))]
        let entries: Vec<jit::saved::Entry> = Vec::new();
        let mut by_root: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            by_root.entry(e.roots[0]).or_default().push(i);
        }
        Saved { entries, by_root }
    })
}

/// A saved region for `roots` (any built from `roots[0]` when `exact` is off: the one with most
/// roots) whose guest code is still there, and clear of the trap range `trap`.
fn find_saved(mem: &mut Linear, roots: &[u32], exact: bool, trap: (u32, u32)) -> Option<usize> {
    if profiling() {
        return None; // saved regions have no counters
    }
    let s = saved_regions();
    s.by_root
        .get(&roots[0])?
        .iter()
        .copied()
        .filter(|&i| !exact || s.entries[i].roots == roots)
        .filter(|&i| s.entries[i].code.iter().all(|&(a, n)| a.saturating_add(n) <= trap.0 || a >= trap.1))
        .filter(|&i| code_bytes(mem, &s.entries[i].code).is_some_and(|t| t == s.entries[i].text))
        .max_by_key(|&i| s.entries[i].roots.len())
}

/// Puts saved region `i` in (instantiating the saved module on this thread first).
fn load_saved(st: &mut State, i: usize) -> bool {
    #[cfg(target_os = "emscripten")]
    let base = *st.saved.get_or_insert_with(|| unsafe { vnvm_jit_bundle_load() });
    #[cfg(not(target_os = "emscripten"))]
    let base = -1;
    if base < 0 {
        return false;
    }
    let e = &saved_regions().entries[i];
    enter(st, base as u32 + i as u32, e.roots.clone(), &e.starts);
    true
}

/// Sends the regions built since the last save to VNVM_JIT_URL (as one module and its index)
/// once the run is past its start, at most every SAVE_EVERY and only when there are enough to
/// be worth it: each save changes the module, and Chromium then forgets its compiled code.
fn maybe_save() {
    const SAVE_AFTER: Duration = Duration::from_secs(20);
    const SAVE_EVERY: Duration = Duration::from_secs(30);
    const SAVE_MIN: usize = 32;
    static URL: std::sync::OnceLock<Option<std::ffi::CString>> = std::sync::OnceLock::new();
    static SAVE: Mutex<(Option<Instant>, Option<Instant>, Option<HashSet<Vec<u32>>>, bool)> = Mutex::new((None, None, None, false));
    let Some(url) = URL.get_or_init(|| std::env::var("VNVM_JIT_URL").ok().and_then(|u| std::ffi::CString::new(u).ok())) else {
        return;
    };
    if profiling() {
        return; // the counters' addresses are only this run's
    }
    let Ok(mut guard) = SAVE.try_lock() else { return };
    let g = &mut *guard;
    let now = Instant::now();
    let start = *g.0.get_or_insert(now);
    if now - start < SAVE_AFTER || g.1.is_some_and(|t| now - t < SAVE_EVERY) {
        return;
    }
    let done = g.2.get_or_insert_with(HashSet::new);
    let new: Vec<(Vec<u32>, Arc<Shared>)> = {
        let sh = SHARED.lock().unwrap();
        let Some(sh) = sh.as_ref() else { return };
        sh.iter().filter(|(r, _)| !done.contains(*r)).map(|(r, l)| (r.clone(), l[0].clone())).collect()
    };
    if new.len() < SAVE_MIN {
        return;
    }
    g.1 = Some(now);
    let entries: Vec<jit::saved::Entry> = new
        .iter()
        .map(|(r, s)| jit::saved::Entry { roots: r.clone(), starts: s.starts.clone(), code: s.code.clone(), text: s.text.clone() })
        .collect();
    let mods: Vec<&[u8]> = new.iter().map(|(_, s)| &s.bytes[..]).collect();
    let names: Vec<String> = entries.iter().map(|e| e.name()).collect();
    let Some(wasm) = jit::wasm::merge(&mods, &names) else { return };
    for (r, _) in new {
        done.insert(r);
    }
    let idx = jit::saved::encode(&entries);
    let mut body = (wasm.len() as u32).to_le_bytes().to_vec();
    body.extend_from_slice(&wasm);
    body.extend_from_slice(&idx);
    // the first time, the store chunks the run read so far too (for the page to fetch ahead)
    let chunks = if g.3 { None } else { Some(crate::vfs::fetched()) };
    g.3 = true;
    #[cfg(target_os = "emscripten")]
    unsafe {
        vnvm_jit_save(url.as_ptr(), body.as_ptr(), body.len());
        if let Some(c) = &chunks {
            let u = std::ffi::CString::new(format!("{}.chunks", url.to_str().unwrap_or(""))).unwrap();
            vnvm_jit_save(u.as_ptr(), c.as_ptr(), c.len());
        }
    };
    let _ = (url, body, chunks);
}

/// Whether the block at `a` ran: the interpreter stops at every branch, so each block that
/// ran before it was compiled missed at least once. Regions take only those (a third of the
/// code they reached statically ran, and V8 compiles all of it); the rest leave the region.
fn ran(misses: &HashMap<u32, u32>, known: &HashSet<u32>, a: u32) -> bool {
    misses.contains_key(&a) || known.contains(&a)
}

/// The jumps between regions the ring saw most: a frequent one gets a new region holding both
/// sides (the source region's roots plus the target), so it becomes a jump inside a function.
fn merge_hot(st: &mut State, cpu: &Cpu) {
    const N: usize = jit::RING_SIZE as usize;
    let mut jumps = [0u64; N];
    for (i, j) in jumps.iter_mut().enumerate() {
        *j = (st.ring[1 + 2 * i] as u64) << 32 | st.ring[2 + 2 * i] as u64;
    }
    if stats() {
        thread_local! { static HOPS: std::cell::Cell<(u64, u64, Option<std::time::Instant>)> = const { std::cell::Cell::new((0, 0, None)) }; }
        HOPS.with(|h| {
            let (mut n, mut last, t) = h.get();
            let t = t.unwrap_or_else(std::time::Instant::now);
            n += st.ring[0] as u64;
            thread_local! { static PAIRS: RefCell<HashMap<(u32, u32), u64>> = RefCell::new(HashMap::new()); }
            PAIRS.with(|p| {
                let mut p = p.borrow_mut();
                for j in &jumps {
                    let from = st.roots.get(&((j >> 32) as u32)).map_or(0, |r| r[0]);
                    *p.entry((from, *j as u32)).or_default() += 1;
                }
                if n - last > 50_000_000 {
                    let mut v: Vec<_> = p.iter().collect();
                    v.sort_by(|a, b| b.1.cmp(a.1));
                    let top: Vec<String> = v.iter().take(16).map(|((f, t), c)| format!("{f:x}->{t:x}:{c}")).collect();
                    eprintln!("vnjit hop pairs {}", top.join(" "));
                    p.clear();
                }
            });
            if n - last > 50_000_000 { eprintln!("vnjit hops {n} in {:?}", t.elapsed()); last = n; }
            h.set((n, last, Some(t)));
        });
    }
    st.ring[0] = 0;
    // pairs by frequency (runs of equal ones once sorted)
    jumps.sort_unstable();
    let mut runs: Vec<(usize, u64)> = Vec::new();
    for i in 0..N {
        match runs.last_mut() {
            Some(r) if r.1 == jumps[i] => r.0 += 1,
            _ => runs.push((1, jumps[i])),
        }
    }
    runs.sort_by(|a, b| b.0.cmp(&a.0));
    // functions (jit::FN): a call or tail jump that found no function there, or a function
    // left often (built again with the blocks that ran since)
    for &(c, p) in &runs {
        let (from, to) = ((p >> 32) as u32, p as u32);
        if c < FN_SAMPLES {
            break;
        }
        let fnish = (from == jit::FN_TAG && !st.fns.contains(&to)) || (from != jit::FN_TAG && st.fns.contains(&from) && !st.fns.contains(&to));
        if !fnish {
            continue;
        }
        let f = if from == jit::FN_TAG { to } else { from };
        let w = st.hot.entry((from, to)).or_insert(0);
        *w += 1;
        let n = st.fn_builds.get(&f).copied().unwrap_or(0);
        if *w >= FN_WINDOWS && n < FN_BUILDS {
            st.hot.remove(&(from, to));
            compile_fn(st, f, (cpu.trap_lo, cpu.trap_hi), &mut Linear);
            return;
        }
    }
    // regions: the most frequent pair between them
    let Some(&(c, best)) = runs.iter().find(|r| {
        let (from, to) = ((r.1 >> 32) as u32, r.1 as u32);
        from != jit::FN_TAG && !st.fns.contains(&from) && !st.fns.contains(&to)
    }) else {
        return;
    };
    let (from, to) = ((best >> 32) as u32, best as u32);
    if c < MERGE_SAMPLES || st.merges >= MAX_MERGES || st.fns.contains(&to) {
        return;
    }
    let w = st.hot.entry((from, to)).or_insert(0);
    *w += 1;
    if *w < MERGE_WINDOWS || !st.merged.insert((from, to)) || !st.merged.insert((u32::MAX, to)) {
        return;
    }
    let Some(roots) = st.roots.get(&from) else { return };
    let mut roots = roots.clone();
    roots.push(to);
    if let Some(r) = shared(&mut Linear, &roots) {
        st.merges += 1;
        load(st, &r.bytes, roots, &r.starts);
        return;
    }
    if let Some(i) = find_saved(&mut Linear, &roots, true, (cpu.trap_lo, cpu.trap_hi)) {
        st.merges += 1;
        load_saved(st, i);
        return;
    }
    let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
    let mut m = Module { name: Some(format!("x86_{:08x}_{}", roots[0], roots.len())), ..Module::default() };
    if let Some(n) = std::env::var("VNVM_JIT_SLOTS").ok().and_then(|v| v.parse::<u32>().ok()) {
        jit::SLOTS_MAX.store(n.min(6), std::sync::atomic::Ordering::Relaxed);
    }
    if std::env::var_os("VNVM_JIT_LAYOUT").is_some() {
        jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let (misses, known) = (&st.misses, &st.known);
    profiling();
    let Some(mut region) = jit::translate_region(&mut m, &mut Linear, &roots, MAX_BLOCKS, |a| {
        debug_allowed(a) && !(a >= lo && a < hi) && (roots.contains(&a) || ran(misses, known, a))
    }) else {
        return;
    };
    if !region.starts.iter().any(|s| s.0 == to) {
        return; // too big already
    }
    st.merges += 1;
    if stats() {
        eprintln!("vnjit merge {from:#x} -> {to:#x}: {} blocks, roots {:x?}\n{}", region.starts.len(), roots, region.layout);
    }
    let bytes = m.finish();
    if load(st, &bytes, roots.clone(), &region.starts) {
        keep_profile(&mut region);
        share(&mut Linear, &roots, bytes, &region);
        maybe_save();
    }
}

/// Counts a miss at cpu.eip and compiles its block once it is warm; true if it did.
pub(crate) fn maybe_compile(cpu: &mut Cpu, mem: &mut Linear) -> bool {
    let eip = cpu.eip;
    if !(jit::GUEST_LO..jit::GUEST_HI).contains(&eip) || (eip >= cpu.trap_lo && eip < cpu.trap_hi) || !debug_allowed(eip) {
        return false;
    }
    with_state(|st| {
        if st.known.contains(&eip) {
            return false;
        }
        // another thread's region from the same code goes in at once
        if st.misses.get(&eip).is_none() {
            if let Some(r) = shared(mem, &[eip]) {
                return load(st, &r.bytes, vec![eip], &r.starts);
            }
            if let Some(i) = find_saved(mem, &[eip], false, (cpu.trap_lo, cpu.trap_hi)) {
                if load_saved(st, i) {
                    return true;
                }
            }
        }
        let n = st.misses.entry(eip).or_insert(0);
        *n += 1;
        if *n < threshold() {
            return false;
        }
        st.misses.remove(&eip);
        if jit::FN.load(std::sync::atomic::Ordering::Relaxed) && (called(cpu, mem) || std::env::var_os("VNVM_JIT_FN_CALLED").is_none()) {
            return compile_fn(st, eip, (cpu.trap_lo, cpu.trap_hi), mem);
        }
        // the block and what it statically reaches, as one function
        let mut m = Module { name: Some(format!("x86_{eip:08x}")), ..Module::default() };
        if std::env::var_os("VNVM_JIT_LAYOUT").is_some() {
            jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
        let (misses, known) = (&st.misses, &st.known);
        profiling();
        let t0 = std::time::Instant::now();
        let Some(mut region) = jit::translate_region(&mut m, mem, &[eip], MAX_BLOCKS, |a| {
            debug_allowed(a) && !(a >= lo && a < hi) && !known.contains(&a) && (a == eip || misses.contains_key(&a))
        }) else {
            st.known.insert(eip);
            return false;
        };
        if std::env::var_os("VNVM_JIT_LAYOUT").is_some() {
            eprintln!("vnjit fresh {eip:#x}\n{}", region.layout);
        }
        let bytes = m.finish();
        let t1 = std::time::Instant::now();
        let ok = load(st, &bytes, vec![eip], &region.starts);
        if std::env::var_os("VNVM_JIT_TIME").is_some() {
            eprintln!("vnjit time {eip:#x}: {} ins, translate {:?}, load {:?}", region.instructions, t1 - t0, t1.elapsed());
        }
        if ok {
            keep_profile(&mut region);
            share(mem, &[eip], bytes, &region);
            maybe_save();
        } else {
            st.known.insert(eip);
        }
        ok
    })
}

/// Whether cpu.eip looks like a function's entry: the return address on the stack follows a
/// CALL of it (or an indirect CALL). Only a guess: a wrong one costs speed, not correctness.
fn called(cpu: &Cpu, mem: &mut Linear) -> bool {
    let mut b = [0u8; 4];
    if vnx86::Memory::fetch(mem, cpu.regs[4], &mut b) < 4 {
        return false;
    }
    let ra = u32::from_le_bytes(b);
    let mut c = [0u8; 7];
    if ra < 7 || vnx86::Memory::fetch(mem, ra - 7, &mut c) < 7 {
        return false;
    }
    if c[2] == 0xe8 && ra.wrapping_add(u32::from_le_bytes([c[3], c[4], c[5], c[6]])) == cpu.eip {
        return true;
    }
    [2usize, 3, 4, 6, 7].iter().any(|&k| c[7 - k] == 0xff && (c[8 - k] >> 3) & 7 == 2)
}

/// Compiles the function at cpu.eip (jit::FN): its adapter and itself, the hash entry tagged.
fn compile_fn(st: &mut State, eip: u32, (lo, hi): (u32, u32), mem: &mut Linear) -> bool {
    *st.fn_builds.entry(eip).or_insert(0) += 1;
    if std::env::var_os("VNVM_JIT_LAYOUT").is_some() {
        jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let mut m = Module { name: Some(format!("x86_{eip:08x}_fn")), ..Module::default() };
    let (misses, known) = (&st.misses, &st.known);
    let Some(region) = jit::translate_function(&mut m, mem, eip, MAX_BLOCKS, (lo, hi), |a| debug_allowed(a) && !(a >= lo && a < hi) && (a == eip || ran(misses, known, a))) else {
        st.known.insert(eip);
        return false;
    };
    if std::env::var_os("VNVM_JIT_LAYOUT").is_some() {
        eprintln!("vnjit fn {eip:#x}\n{}", region.layout);
    }
    if stats() {
        eprintln!("vnjit fn {eip:#x} build {}: {} instructions", st.fn_builds[&eip], region.instructions);
    }
    let bytes = m.finish();
    if let Some(dir) = std::env::var_os("VNVM_JIT_DUMP") {
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{eip:08x}-fn.wasm")), &bytes);
    }
    #[cfg(target_os = "emscripten")]
    let base = unsafe { vnvm_jit_load(bytes.as_ptr(), bytes.len(), 2) };
    #[cfg(not(target_os = "emscripten"))]
    let base = -1;
    st.known.insert(eip);
    if base < 0 {
        return false;
    }
    // every block of it enters it (a region at one of them hands over to the function)
    for &(a, _) in &region.starts {
        st.fns.insert(a);
        st.known.insert(a);
    }
    enter(st, base as u32, vec![eip], &[]);
    while (st.used + region.starts.len()) * 2 > st.hash.len() {
        let mut bigger = vec![[0u32; 2]; st.hash.len() * 2];
        for e in st.hash.iter().filter(|e| e[0] != 0) {
            insert(&mut bigger, e[0], e[1]);
        }
        st.hash = bigger;
    }
    for &(a, k) in &region.starts {
        if insert(&mut st.hash, a, base as u32 | k << jit::ENTRY_SHIFT | jit::FN_TAG) {
            st.used += 1;
        }
    }
    true
}

thread_local! {
    static DECODED: RefCell<vnx86::DecodeCache> = RefCell::new(vnx86::DecodeCache::new());
}

/// One instruction in the interpreter (with this thread's cache of decoded instructions).
pub(crate) fn step(cpu: &mut Cpu, mem: &mut Linear) -> (Exit, bool) {
    DECODED.with(|d| match d.try_borrow_mut() {
        Ok(mut d) => vnx86::step_cached(cpu, mem, Some(&mut d)),
        // (an instruction that calls back into the guest: not while the cache is in use)
        Err(_) => vnx86::step_flow(cpu, mem),
    })
}

/// One instruction run by the interpreter for a block: 0 if it completed, else 2 with the exit
/// kept for [`run`].
#[no_mangle]
pub unsafe extern "C" fn vnhost_jit_step(cpu: *mut Cpu) -> i32 {
    if stats() {
        count_step(&*cpu);
    }
    match step(&mut *cpu, &mut Linear).0 {
        Exit::Continue => 0,
        e => {
            EXIT.with(|x| x.set(Some(e)));
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_jit_eflags(cpu: *mut Cpu) -> u32 {
    let cpu = &mut *cpu;
    cpu.materialize();
    cpu.eflags
}

/// A region leaving with its x87 cache (vnx86::jit::x87_out).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn vnhost_jit_x87(cpu: *mut Cpu, top: u32, info: u32, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64) {
    jit::x87_out(&mut (*cpu).fpu, top, info, [a, b, c, d, e, f, g, h]);
}

#[no_mangle]
pub extern "C" fn vnhost_jit_status(op: u32, a: u32, b: u32, r: u32, c: u32) -> u32 {
    vnx86::lazy::Lazy { op, a, b, r, c }.status()
}

/// VNVM_JIT_PROFILE: every region's blocks (address, entry counter, instruction keys).
static PROF: Mutex<Vec<(u32, u32, Vec<String>)>> = Mutex::new(Vec::new());

fn profiling() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VNVM_JIT_PROFILE").is_some();
        jit::PROFILE.store(on, std::sync::atomic::Ordering::Relaxed);
        // VNVM_JIT_PROFILE=<seconds> (> 1): print the counts that long after the first region
        // (under node, where no page asks for them)
        if let Some(secs) = std::env::var("VNVM_JIT_PROFILE").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&s| s > 1) {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(secs));
                vnhost_jit_profile();
            });
        }
        on
    })
}

fn keep_profile(region: &mut jit::Region) {
    if profiling() {
        PROF.lock().unwrap().append(&mut region.profile);
    }
}

/// Prints the executed instruction counts so far (blocks entered times their instructions:
/// a block left early still counts whole; the interpreter's own steps are not in it).
#[no_mangle]
pub extern "C" fn vnhost_jit_profile() {
    let prof = PROF.lock().unwrap();
    let mut by_key: HashMap<&str, u64> = HashMap::new();
    let mut by_cat: HashMap<&str, u64> = HashMap::new();
    let mut blocks: Vec<(u64, u32, usize)> = Vec::new();
    let mut total = 0u64;
    for (a, at, keys) in prof.iter() {
        let n = unsafe { (*at as usize as *const u32).read_volatile() } as u64;
        if n == 0 {
            continue;
        }
        blocks.push((n * keys.len() as u64, *a, keys.len()));
        for k in keys {
            *by_key.entry(k).or_default() += n;
            *by_cat.entry(category(k)).or_default() += n;
            total += n;
        }
    }
    let mut keys: Vec<_> = by_key.into_iter().collect();
    keys.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    let mut mn: HashMap<&str, u64> = HashMap::new();
    for (k, n) in &keys {
        let m = k.trim_start_matches("lock ").trim_start_matches("rep ").trim_start_matches("repne ");
        *mn.entry(m.split(' ').next().unwrap_or(m)).or_default() += n;
    }
    let mut mn: Vec<_> = mn.into_iter().collect();
    mn.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    let mut cats: Vec<_> = by_cat.into_iter().collect();
    cats.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    blocks.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let mut o = format!("PROF total {total} blocks {}\n", prof.len());
    for (c, n) in &cats {
        o += &format!("PROF cat {c} {n}\n");
    }
    for (k, n) in mn.iter().take(80) {
        o += &format!("PROF mn {k} {n}\n");
    }
    for (k, n) in keys.iter().take(200) {
        o += &format!("PROF key {k} {n}\n");
    }
    // VNVM_PROF_BLOCKS=<n>: more of the hottest blocks (default 40)
    let nblocks = std::env::var("VNVM_PROF_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    for (n, a, len) in blocks.iter().take(nblocks) {
        o += &format!("PROF block {a:08x} {len} {n}\n");
    }
    // per 64KB of guest address space (to attribute to modules offline)
    let mut areas: HashMap<u32, u64> = HashMap::new();
    for (n, a, _) in &blocks {
        *areas.entry(a >> 16).or_default() += n;
    }
    let mut areas: Vec<_> = areas.into_iter().collect();
    areas.sort_unstable();
    for (a, n) in areas {
        o += &format!("PROF area {:08x} {n}\n", a << 16);
    }
    o += "PROF end\n";
    eprint!("{o}");
}

fn category(k: &str) -> &'static str {
    let m = k.trim_start_matches("lock ").trim_start_matches("rep ").trim_start_matches("repne ");
    let mn = m.split(' ').next().unwrap_or(m);
    if k.contains("xmm") || mn.ends_with("ss") && mn != "cmpss" || mn.starts_with("cvt") || mn == "ldmxcsr" || mn == "stmxcsr" {
        "sse"
    } else if k.contains("mm") || mn == "emms" {
        "mmx"
    } else if mn.starts_with('f') && mn != "fxsave" || mn == "wait" {
        "x87"
    } else if k.contains("str") {
        "string"
    } else if k.starts_with("lock") || mn == "xchg" || mn.starts_with("cmpxchg") {
        "atomic"
    } else if matches!(mn, "call" | "ret" | "jmp" | "push" | "pop" | "leave" | "enter") || mn.starts_with('j') {
        "control/stack"
    } else {
        "int"
    }
}

fn stats() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VNVM_JIT_STATS").is_some())
}

/// VNVM_JIT_STATS: which instructions blocks hand to the interpreter, printed every 2^22.
fn count_step(cpu: &Cpu) {
    thread_local! {
        static COUNTS: RefCell<(u64, HashMap<String, u64>)> = RefCell::new((0, HashMap::new()));
    }
    let mut b = [0u8; 16];
    let n = vnx86::Memory::fetch(&mut Linear, cpu.eip, &mut b);
    let mut d = vnx86::iced_x86::Decoder::with_ip(32, &b[..n], cpu.eip as u64, vnx86::iced_x86::DecoderOptions::NONE);
    let ins = d.decode();
    let pre = if ins.has_lock_prefix() {
        "lock "
    } else if ins.has_rep_prefix() || ins.has_repe_prefix() || ins.has_repne_prefix() {
        "rep "
    } else {
        ""
    };
    let key = format!("{pre}{:?}", ins.mnemonic());
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        *c.1.entry(key).or_insert(0) += 1;
        c.0 += 1;
        if c.0 % (1 << 22) == 0 {
            let mut v: Vec<_> = c.1.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let top: Vec<String> = v.iter().take(20).map(|(k, n)| format!("{k}:{n}")).collect();
            eprintln!("vnjit steps {}: {}", c.0, top.join(" "));
        }
    });
}

/// VNVM_JIT_STATS: where the JIT hands back to the run loop, printed every 2^20.
pub(crate) fn count_miss(eip: u32) {
    if !stats() {
        return;
    }
    thread_local! {
        static MISSES: RefCell<(u64, HashMap<u32, u64>)> = RefCell::new((0, HashMap::new()));
    }
    MISSES.with(|c| {
        let mut c = c.borrow_mut();
        *c.1.entry(eip).or_insert(0) += 1;
        c.0 += 1;
        if c.0 % (1 << 20) == 0 {
            let mut v: Vec<_> = c.1.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let top: Vec<String> = v.iter().take(12).map(|(k, n)| format!("{k:#x}:{n}")).collect();
            eprintln!("vnjit misses {}: {}", c.0, top.join(" "));
        }
    });
}

/// VNVM_JIT_STATS: entries into the run loop per thread (a guest spinning on system calls
/// shows up here), printed every 2^16 with the addresses seen most.
pub(crate) fn count_entry(eip: u32) {
    if !stats() {
        return;
    }
    thread_local! {
        static ENTRIES: RefCell<(u64, HashMap<u32, u64>, Option<std::time::Instant>)> = RefCell::new((0, HashMap::new(), None));
    }
    ENTRIES.with(|c| {
        let mut c = c.borrow_mut();
        *c.1.entry(eip).or_insert(0) += 1;
        c.0 += 1;
        let t0 = *c.2.get_or_insert_with(std::time::Instant::now);
        if c.0 % (1 << 16) == 0 {
            let mut v: Vec<_> = c.1.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let top: Vec<String> = v.iter().take(6).map(|(k, n)| format!("{k:#x}:{n}")).collect();
            eprintln!("vnjit pid {} thread {:?}: {} entries in {:?}: {}", crate::proc::current(), std::thread::current().id(), c.0, t0.elapsed(), top.join(" "));
        }
    });
}
