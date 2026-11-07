//! The x86 -> wasm JIT on the host side: per thread, a block hash table read by the
//! dispatcher module (`koji::jit::dispatch`), and the helpers block modules import
//! (`vnhost_jit_step`, `vnhost_jit_eflags`, `vnhost_jit_status`). The modules themselves are
//! compiled and instantiated by vnjit.js; each thread has its own funcref table.
//!
//! IPPAI_JIT=0 turns the JIT off (the interpreter runs everything). IPPAI_JIT_DUMP=dir writes every
//! region module to dir/<first root>-<roots>.wasm. IPPAI_JIT_LAYOUT=1 prints the block order, loops
//! and wasm bytes per mnemonic of new regions (of merged ones too with IPPAI_JIT_STATS).
//! Functions (jit::FN) are translated on a compiler thread while the interpreter goes on;
//! only loading the module stays on the guest thread (IPPAI_JIT_ASYNC=0: all of it there).
//!
//! Regions are kept across runs: with IPPAI_JIT_URL set, the regions built since the last save are
//! sent there now and then (see [`maybe_save`]; ippai serve merges them into what it has), and the
//! page (or pre.js under node) hands the next run that module and index (`jit::saved`). A
//! saved region goes in when its first root first misses, if the guest code is still the same:
//! no interpreter warm-up and no translation, and in Chromium the browser's code cache of the
//! module keeps V8's optimized code too.

use crate::cpu::Linear;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use koji::jit::{self, wasm::Module};
use koji::{Cpu, Exit};

/// Misses at an address before its block is compiled (IPPAI_JIT_THRESHOLD overrides).
const THRESHOLD: u32 = 50;

fn threshold() -> u32 {
    use std::sync::OnceLock;
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| std::env::var("IPPAI_JIT_THRESHOLD").ok().and_then(|v| v.parse().ok()).unwrap_or(THRESHOLD))
}
/// Debugging a miscompile by bisection: IPPAI_JIT_ONLY=lo-hi (hex) compiles only code in [lo, hi),
/// IPPAI_JIT_ONLY=!lo-hi all but that.
fn debug_allowed(a: u32) -> bool {
    if wx_page(a) || page_set(&SMC_PAGES, a) {
        return false;
    }
    debug_range(a)
}

/// Pages mapped or protected writable and executable at once, and the main executables' pages.
/// Code on a W|X page outside a main exe (an unpacker's or a JS engine's) may be rewritten
/// without a protection change, which `code_changed` can't see: the interpreter runs it
/// (UPX-style packed exes keep the JIT; their stubs unpack before the code runs).
static WX_PAGES: [AtomicU32; 1 << 14] = [const { AtomicU32::new(0) }; 1 << 14];
static EXE_PAGES: [AtomicU32; 1 << 14] = [const { AtomicU32::new(0) }; 1 << 14];

fn set_pages(map: &[AtomicU32], addr: usize, len: usize, on: bool) {
    if len == 0 || addr >= 1 << 31 {
        return;
    }
    let (lo, hi) = (addr >> 12, (addr.saturating_add(len - 1) >> 12).min((1 << 19) - 1));
    for p in lo..=hi {
        if on {
            map[p >> 5].fetch_or(1 << (p & 31), Ordering::Relaxed);
        } else {
            map[p >> 5].fetch_and(!(1 << (p & 31)), Ordering::Relaxed);
        }
    }
}

fn page_set(map: &[AtomicU32], a: u32) -> bool {
    let p = (a >> 12) as usize;
    p < 1 << 19 && map[p >> 5].load(Ordering::Relaxed) & (1 << (p & 31)) != 0
}

fn wx_page(a: u32) -> bool {
    page_set(&WX_PAGES, a) && !page_set(&EXE_PAGES, a) && !wx_jit()
}
/// Whether code on W|X pages outside the exe is translated too, its stores watched like the
/// exe's (IPPAI_JIT_WX=1, or [`translate_wx_pages`]): stores into it from code elsewhere go
/// unseen, so only for programs known to keep their code there (ASProtect's decryptors).
fn wx_jit() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    WX_JIT.load(Ordering::Relaxed) || *ENV.get_or_init(|| std::env::var_os("IPPAI_JIT_WX").is_some())
}
static WX_JIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn translate_wx_pages() {
    WX_JIT.store(true, Ordering::Relaxed);
}

/// A main executable's W|X pages: packers (ASProtect) decrypt code there in place, also code
/// that already ran, so translations of code there test their stores (jit::set_watch), and
/// the interpreter tests its own (`interp_write`).
fn exe_wx_page(a: u32) -> bool {
    page_set(&WX_PAGES, a) && (page_set(&EXE_PAGES, a) || wx_jit())
}

/// Pages whose translated code was written over: the interpreter runs them from then on.
static SMC_PAGES: [AtomicU32; 1 << 14] = [const { AtomicU32::new(0) }; 1 << 14];

/// A store to `a`, a page with translated code: the translations go (everyone's), and so
/// does the JIT for that page.
pub(crate) fn smc_hit(a: u32) {
    // (data beside the code on its page: nothing to drop)
    if !in_code(a) {
        return;
    }
    set_pages(&SMC_PAGES, a as usize, 1, true);
    if stats() {
        eprintln!("vnjit: code at {a:#x} rewritten");
    }
    code_changed(a as usize, 1);
}

/// Whether code built from `code` may run (none of it on a page the interpreter keeps since
/// a store rewrote translated code there): shared and saved builds are checked by their
/// bytes, which data stores beside the code leave alone.
fn code_kept(code: &[(u32, u32)]) -> bool {
    code.iter().all(|&(a, n)| !page_set(&SMC_PAGES, a) && !page_set(&SMC_PAGES, a.saturating_add(n.max(1) - 1)))
}

/// The interpreter's store to `a` (cpu.rs Linear): whether it hit translated code (then
/// handled by [`smc_hit`]).
#[inline(always)]
pub(crate) fn interp_write(a: u32) -> bool {
    let p = (a >> 12) as usize;
    if p >= 1 << 19 || CODE_PAGES[p >> 5].load(Ordering::Relaxed) & (1 << (p & 31)) == 0 {
        return false;
    }
    smc_hit(a);
    true
}

/// The guest's pages `addr..addr + len` got protection `prot` (mmap, mprotect; unmapped: 0).
pub fn pages_protected(addr: usize, len: usize, prot: i32) {
    let wx = prot & libc::PROT_WRITE != 0 && prot & libc::PROT_EXEC != 0;
    set_pages(&WX_PAGES, addr, len, wx);
    if prot == 0 {
        set_pages(&EXE_PAGES, addr, len, false);
    }
}

/// The loader mapped a main executable (not a DLL) there.
pub fn exe_image(addr: u32, size: u32) {
    set_pages(&EXE_PAGES, addr as usize, size as usize, true);
}

fn debug_range(a: u32) -> bool {
    use std::sync::OnceLock;
    static R: OnceLock<(u32, u32, bool)> = OnceLock::new();
    let (lo, hi, not) = *R.get_or_init(|| {
        // ("!lo-hi": everything but [lo, hi))
        let v = std::env::var("IPPAI_JIT_ONLY").unwrap_or_default();
        let not = v.starts_with('!');
        let mut it = v.trim_start_matches('!').split('-').map(|x| u32::from_str_radix(x.trim_start_matches("0x"), 16).ok());
        match (it.next().flatten(), it.next().flatten()) {
            (Some(lo), Some(hi)) => (lo, hi, not),
            _ => (0, u32::MAX, false),
        }
    });
    (a >= lo && a < hi) != not
}
/// IPPAI_JIT_FN_ONLY=lo-hi[_lo-hi...] (hex): function mode only for entries in those ranges,
/// regions elsewhere.
fn fn_allowed(a: u32) -> bool {
    use std::sync::OnceLock;
    static R: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    let r = R.get_or_init(|| {
        let v = std::env::var("IPPAI_JIT_FN_ONLY").unwrap_or_default();
        v.split('_')
            .filter_map(|r| {
                let mut it = r.split('-').map(|x| u32::from_str_radix(x.trim_start_matches("0x"), 16).ok());
                Some((it.next().flatten()?, it.next().flatten()?))
            })
            .collect::<Vec<_>>()
    });
    if !r.is_empty() {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| eprintln!("vnjit: function mode only in {r:x?}"));
    }
    r.is_empty() || r.iter().any(|&(lo, hi)| a >= lo && a < hi)
}
/// Blocks the dispatcher runs before returning to the run loop.
const BUDGET: i32 = 1 << 14;
/// Most blocks compiled into one module.
const MAX_BLOCKS: usize = 1024;

#[cfg(target_os = "emscripten")]
extern "C" {
    fn vnvm_jit_init(ptr: *const u8, len: usize);
    fn vnvm_jit_load(ptr: *const u8, len: usize, n: u32) -> i32;
    // JavaScript forwards this address to generated Wasm; it does not use Cpu's Rust layout.
    fn vnvm_jit_run(cpu: *mut std::ffi::c_void) -> i32;
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
    /// the call cache (Cpu::jit_calls)
    calls: Box<[[u32; 2]; jit::CALL_CACHE as usize]>,
    /// (region, target) pairs already merged, and targets that did not fit
    merged: HashSet<(u32, u32)>,
    /// ring windows each hot jump topped so far
    hot: HashMap<(u32, u32), u32>,
    merges: u32,
    /// table index of the saved regions' first function on this thread, once instantiated
    #[cfg(target_os = "emscripten")]
    saved: Option<i32>,
    /// entries of functions (jit::FN): regions never take their hash entry
    fns: HashSet<u32>,
    /// times each function was built
    fn_builds: HashMap<u32, u32>,
    /// indirect call/jump sites of functions: the function, and the target it mostly takes
    site_fn: HashMap<u32, Vec<u32>>,
    hints: HashMap<u32, u32>,
    /// functions the compiler thread is translating, and where it says it is done
    pending: HashSet<u32>,
    done_tx: std::sync::mpsc::Sender<Batch>,
    done_rx: std::sync::mpsc::Receiver<Batch>,
    /// sites whose hint changed (a site going to several targets keeps its second hint)
    rehinted: HashSet<u32>,
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
    /// a function's (jit::FN): its build (1 up; 0 for a region) and indirect call sites
    build: u32,
    sites: Vec<u32>,
}

/// Shared regions by their roots.
static SHARED: Mutex<Option<HashMap<Vec<u32>, Vec<Arc<Shared>>>>> = Mutex::new(None);

/// What the translations hold (IPPAI_JIT_COUNT prints it; never freed so far): modules loaded
/// (each thread's own), their bytes, the highest table index they took (all threads).
static MODULES: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

#[cfg(target_os = "emscripten")]
fn loaded(base: i32, n: u32, bytes: usize) -> i32 {
    if base >= 0 {
        MODULES[0].fetch_add(1, Ordering::Relaxed);
        MODULES[1].fetch_add(bytes as u64, Ordering::Relaxed);
        MODULES[2].fetch_max(base as u64 + n as u64, Ordering::Relaxed);
    }
    base
}

/// SHARED: (modules, bytes of module code, guest text and tables).
fn shared_size() -> (usize, usize) {
    let g = SHARED.lock().unwrap();
    let Some(m) = g.as_ref() else { return (0, 0) };
    m.values().flatten().fold((0, 0), |(n, b), r| (n + 1, b + r.bytes.len() + r.text.len() + (r.starts.len() + r.code.len()) * 8 + r.sites.len() * 4))
}

fn code_bytes(mem: &mut Linear, code: &[(u32, u32)]) -> Option<Vec<u8>> {
    let mut text = Vec::new();
    for &(a, n) in code {
        let at = text.len();
        text.resize(at + n as usize, 0);
        if koji::Memory::fetch(mem, a, &mut text[at..]) != n as usize {
            return None;
        }
    }
    Some(text)
}

/// A region for `roots` some thread built from the same code as ours.
fn shared(mem: &mut Linear, roots: &[u32]) -> Option<Arc<Shared>> {
    let list = SHARED.lock().unwrap().as_ref()?.get(roots)?.clone();
    list.into_iter().rev().find(|r| code_kept(&r.code) && code_bytes(mem, &r.code).is_some_and(|t| t == r.text))
}

fn share(mem: &mut Linear, roots: &[u32], bytes: Vec<u8>, region: &jit::Region) {
    share_build(mem, roots.to_vec(), bytes, region, 0);
}

fn share_build(mem: &mut Linear, key: Vec<u32>, bytes: Vec<u8>, region: &jit::Region, build: u32) {
    let Some(text) = code_bytes(mem, &region.code) else { return };
    let r = Arc::new(Shared { bytes, starts: region.starts.clone(), code: region.code.clone(), text, build, sites: region.sites.clone() });
    SHARED.lock().unwrap().get_or_insert_with(HashMap::new).entry(key).or_default().push(r);
}

/// The key of the function at `eip` among the shared modules (regions use their roots).
fn fn_key(eip: u32) -> [u32; 2] {
    [eip, jit::FN_TAG]
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static EXIT: Cell<Option<Exit>> = const { Cell::new(None) };
}

pub fn enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| cfg!(target_os = "emscripten") && std::env::var("IPPAI_JIT").map_or(true, |v| v != "0"))
}

pub fn enabled64() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("IPPAI_JIT64").map_or(true, |v| v != "0"))
}

pub enum Ran {
    /// budget used up or a signal pending
    Stopped,
    Exit(Exit),
    /// no block at cpu.eip, or a block wants the interpreter
    Missed,
}

/// Pages (of 4 KB) that translated code was made from, any thread's.
static CODE_PAGES: [AtomicU32; 1 << 14] = [const { AtomicU32::new(0) }; 1 << 14];
/// Changed when code that was translated may have changed: every thread then drops its
/// translations (at its next entry into the run loop).
static GENERATION: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static SEEN: Cell<u32> = const { Cell::new(0) };
}

/// The translated code's byte ranges (start -> end), for telling a store into code from one
/// into data on the same page ([`smc_hit`]); dropped with the translations.
static CODE_RANGES: Mutex<BTreeMap<u32, u32>> = Mutex::new(BTreeMap::new());

fn in_code(a: u32) -> bool {
    // (blocks are short: those starting up to 4 KB below)
    CODE_RANGES.lock().unwrap().range(a.saturating_sub(0x1000)..=a).any(|(_, &e)| a < e)
}

fn mark_code(code: &[(u32, u32)]) {
    {
        let mut r = CODE_RANGES.lock().unwrap();
        for &(a, n) in code {
            let e = r.entry(a).or_insert(0);
            *e = (*e).max(a.saturating_add(n.max(1)));
        }
    }
    for &(a, n) in code {
        for p in (a >> 12)..=(a.saturating_add(n.max(1) - 1) >> 12) {
            if let Some(w) = CODE_PAGES.get(p as usize >> 5) {
                w.fetch_or(1 << (p & 31), Ordering::Relaxed);
            }
        }
    }
}

/// A byte per guest page of [0, 2 GB): [`READ_FAULTS`] | [`WRITE_FAULTS`] where an access
/// faults; made with the first such page (koji's PAGE_MAP points at it once a page that
/// turns the checks on is marked).
static FAULT_MAP: std::sync::OnceLock<&'static [std::sync::atomic::AtomicU8]> = std::sync::OnceLock::new();

/// A read of the page faults (and a write: PAGE_NOACCESS, PAGE_GUARD, decommitted).
pub const READ_FAULTS: u8 = 1;
/// A write faults (PAGE_READONLY, PAGE_EXECUTE_READ).
pub const WRITE_FAULTS: u8 = 2;

/// Marks the guest's pages `addr..addr + len` as faulting on the accesses `kind` says (0:
/// none). The interpreter checks every access against the marks; translated code only from
/// the first mark with `checks` on (PAGE_NOACCESS, PAGE_GUARD: rare, and their faults are
/// part of how programs work), which turns the page checks on for good: translations made
/// without them are dropped, everyone's, and saved ones are no longer used. Read-only and
/// decommitted pages don't turn them on (every program has read-only sections, and checked
/// accesses cost the JIT), so translated code writes them only while the checks are off.
pub fn fault_pages(addr: u32, len: u32, kind: u8, checks: bool) {
    if len == 0 || addr >= 1 << 31 || (kind == 0 && FAULT_MAP.get().is_none()) {
        return;
    }
    let map = *FAULT_MAP.get_or_init(|| {
        let m: &'static [std::sync::atomic::AtomicU8] = Box::leak((0..1 << 19).map(|_| std::sync::atomic::AtomicU8::new(0)).collect());
        m
    });
    let (lo, hi) = (addr as usize >> 12, ((addr as usize + len as usize - 1) >> 12).min((1 << 19) - 1));
    for b in &map[lo..=hi] {
        b.store(kind, Ordering::Relaxed);
    }
    if kind != 0 && checks && jit::PAGE_MAP.load(Ordering::Relaxed) == 0 {
        jit::PAGE_MAP.store(map.as_ptr() as usize, Ordering::SeqCst);
        drop_translations();
        if stats() {
            eprintln!("vnjit pages {addr:#x}+{len:#x} fault: accesses check pages from now on");
        }
    }
}

/// Whether the access [addr, + len) touches a page marked by [`fault_pages`] for it.
#[inline(always)]
pub(crate) fn faults(addr: u32, len: u32, write: bool) -> bool {
    let Some(map) = FAULT_MAP.get() else { return false };
    let (lo, hi) = (addr as usize >> 12, (addr as usize + len.max(1) as usize - 1) >> 12);
    let m = if write { READ_FAULTS | WRITE_FAULTS } else { READ_FAULTS };
    map.get(lo).is_some_and(|b| b.load(Ordering::Relaxed) & m != 0) || map.get(hi).is_some_and(|b| b.load(Ordering::Relaxed) & m != 0)
}

/// Every thread drops its translations (at its next entry into the run loop).
fn drop_translations() {
    for w in &CODE_PAGES {
        w.store(0, Ordering::Relaxed);
    }
    CODE_RANGES.lock().unwrap().clear();
    *SHARED.lock().unwrap() = None;
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// The guest's pages `addr..addr + len` may get new code (made writable, unmapped, mapped over,
/// or the instruction cache flushed): translations of code there are dropped, everyone's.
pub fn code_changed(addr: usize, len: usize) {
    if len == 0 || addr >= 1 << 31 {
        return;
    }
    let (lo, hi) = (addr >> 12, (addr.saturating_add(len - 1) >> 12).min((1 << 19) - 1));
    let mut hit = false;
    for p in lo..=hi {
        if CODE_PAGES[p >> 5].load(Ordering::Relaxed) & (1 << (p & 31)) != 0 {
            hit = true;
            break;
        }
    }
    if hit {
        for w in &CODE_PAGES {
            w.store(0, Ordering::Relaxed);
        }
        CODE_RANGES.lock().unwrap().clear();
        GENERATION.fetch_add(1, Ordering::SeqCst);
        if stats() {
            eprintln!("vnjit code changed at {addr:#x}+{len:#x}: translations dropped");
        }
    }
}

/// Drops this thread's translations if code changed since it made them.
fn check_generation() {
    let g = GENERATION.load(Ordering::Relaxed);
    if SEEN.with(|s| s.replace(g)) != g {
        STATE.with(|s| *s.borrow_mut() = None);
    }
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
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            State {
                pending: HashSet::new(),
                done_tx,
                done_rx,
                hash: vec![[0; 2]; 1 << 12],
                used: 0,
                misses: HashMap::new(),
                known: HashSet::new(),
                roots: HashMap::new(),
                ring: Box::new([0; 2 + 2 * jit::RING_SIZE as usize]),
                calls: Box::new([[0; 2]; jit::CALL_CACHE as usize]),
                merged: HashSet::new(),
                hot: HashMap::new(),
                merges: 0,
                #[cfg(target_os = "emscripten")]
                saved: None,
                fns: HashSet::new(),
                fn_builds: HashMap::new(),
                site_fn: HashMap::new(),
                hints: HashMap::new(),
                rehinted: HashSet::new(),
            }
        });
        f(st)
    })
}

/// Compiles and their time (µs) for IPPAI_JIT_COUNT: regions, functions.
static COMPILES: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];

/// Compiles since the start: regions, functions, rebuilt functions (the crash log).
static COMPILED: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

pub(crate) fn compiled_totals() -> [u32; 3] {
    COMPILED.each_ref().map(|c| c.load(Ordering::Relaxed))
}

fn count_compile(k: usize, t0: Instant) {
    COMPILED[k].fetch_add(1, Ordering::Relaxed);
    COMPILES[2 * k].fetch_add(1, Ordering::Relaxed);
    COMPILES[2 * k + 1].fetch_add(t0.elapsed().as_micros() as u32, Ordering::Relaxed);
}

/// Switches read once: IPPAI_JIT_COUNT=<secs> (transfer counters),
/// IPPAI_JIT_NOCHECK=1 (measuring only: no range checks).
fn init_flags() {
    // (once: this runs on every entry into translated code, and std::env::var takes a lock
    // and walks the environment)
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var("IPPAI_JIT_FN_ENTRIES").is_ok_and(|v| v == "0") {
            jit::FN_ENTRIES.store(false, Ordering::Relaxed);
        }
        if let Ok(v) = std::env::var("IPPAI_JIT_PROBE") {
            for (p, a) in jit::PROBES.iter().zip(v.split('_').filter_map(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok())) {
                p.store(a, Ordering::Relaxed);
            }
        }
        if std::env::var("IPPAI_JIT_HOIST").is_ok_and(|v| v == "0") {
            jit::HOIST.store(false, Ordering::Relaxed);
        }
        if let Some(secs) = std::env::var("IPPAI_JIT_COUNT").ok().and_then(|v| v.parse::<u64>().ok()) {
            use std::sync::atomic::Ordering::Relaxed;
            if !jit::COUNT.swap(true, Relaxed) {
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(secs.max(1)));
                    let v: Vec<String> = jit::COUNTERS.iter().zip(jit::COUNTER_NAMES).map(|(c, n)| format!("{n}={}", c.swap(0, Relaxed))).collect();
                    let c: Vec<u32> = COMPILES.iter().map(|c| c.swap(0, Relaxed)).collect();
                    let (sn, sb) = shared_size();
                    let m: Vec<u64> = MODULES.iter().map(|c| c.load(Relaxed)).collect();
                    eprintln!(
                        "vnjit count/{secs}s {} | regions {} {} ms, functions {} {} ms (rebuilt {}, {} ms) | shared {sn} ({} KiB), loaded {} modules ({} KiB), table {}",
                        v.join(" "), c[0], c[1] / 1000, c[2], c[3] / 1000, c[4], c[5] / 1000, sb >> 10, m[0], m[1] >> 10, m[2]
                    );
                });
            }
        }
        // CALL/RET as wasm calls: 1 direct calls, 2 indirect ones too
        // debugging the long-mode translator: IPPAI_JIT64_INS / _BLOCKS (limits), IPPAI_JIT64_OFF
        // (mnemonics, as iced names them, left to the interpreter)
        let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        jit::long::DEBUG_INS.store(num("IPPAI_JIT64_INS"), std::sync::atomic::Ordering::Relaxed);
        jit::long::DEBUG_BLOCKS.store(num("IPPAI_JIT64_BLOCKS"), std::sync::atomic::Ordering::Relaxed);
        jit::long::DEBUG_CHECK.store(num("IPPAI_JIT64_CHECK") != 0, std::sync::atomic::Ordering::Relaxed);
        for name in std::env::var("IPPAI_JIT64_OFF").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
            if !jit::long::debug_off_named(name) {
                eprintln!("IPPAI_JIT64_OFF: no mnemonic {name}");
            }
        }
        jit::set_watch(exe_wx_page);
        jit::set_deny(|a| wx_page(a) || page_set(&SMC_PAGES, a));
        // function mode by default (IPPAI_JIT_FN=0: regions)
        // (a constant stored: the wasm build stored false for this flag when the value was
        // the expression's, and FN mode was off by default)
        if !std::env::var("IPPAI_JIT_FN").is_ok_and(|v| v == "0") {
            jit::FN.store(true, std::sync::atomic::Ordering::Relaxed);
        }

    });
}

pub fn run(cpu: &mut Cpu) -> Ran {
    init_flags();
    check_generation();
    // (saves come after builds; this covers a run whose builds were all done early)
    thread_local! { static RUNS: Cell<u32> = const { Cell::new(0) }; }
    if RUNS.with(|r| { r.set(r.get().wrapping_add(1)); r.get() % 65536 == 0 }) {
        maybe_save();
    }
    with_state(|st| {
        if !st.pending.is_empty() {
            take_built(st, &mut Linear);
        }
        if st.ring[0] >= jit::RING_SIZE {
            merge_hot(st, cpu);
        }
        cpu.jit_hash = st.hash.as_ptr() as usize as u32;
        cpu.jit_mask = st.hash.len() as u32 - 1;
        cpu.jit_ring = st.ring.as_mut_ptr() as usize as u32;
        cpu.jit_calls = st.calls.as_mut_ptr() as usize as u32;
    });
    cpu.jit_budget = BUDGET;
    cpu.jit_code_pages = CODE_PAGES.as_ptr() as usize as u32;
    let depth = cpu.jit_depth;
    #[cfg(target_os = "emscripten")]
    let expect = {
        // IPPAI_JIT64_VERIFY=1: single-instruction regions, =2: whole regions; one region per call
        // now and then, checked against the interpreter
        thread_local! { static VN: Cell<u32> = const { Cell::new(0) }; }
        let mode = if cpu.mode64 != 0 { verify64() } else { 0 };
        let verify = mode != 0 && debug_allowed(cpu.eip) && VN.with(|v| { v.set(v.get().wrapping_add(1)); v.get() % match mode { 1 => 4, 2 => 32, _ => 1 } == 0 });
        verify.then(|| {
            cpu.jit_budget = 2;
            let mut c = cpu.clone();
            let mut rec = Recorder(Vec::new(), HashMap::new());
            let mut states = vec![(c.clone(), 0)];
            for _ in 0..if mode == 1 { 1 } else { 400 } {
                if koji::step(&mut c, &mut rec) != Exit::Continue {
                    break;
                }
                states.push((c.clone(), rec.0.len()));
            }
            (cpu.clone(), states, rec.0)
        })
    };
    #[cfg(target_os = "emscripten")]
    let r = unsafe { vnvm_jit_run((cpu as *mut Cpu).cast()) };
    #[cfg(target_os = "emscripten")]
    if let Some((before, states, writes)) = expect {
        if r == jit::RET_CONTINUE || r == jit::RET_INTERP {
            check64(&before, &states, cpu, &writes);
        }
    }
    cpu.jit_depth = depth;
    if cpu.jit_smc != 0 {
        smc_hit(std::mem::take(&mut cpu.jit_smc));
    }
    #[cfg(not(target_os = "emscripten"))]
    let r = jit::RET_INTERP;
    match r {
        jit::RET_CONTINUE => Ran::Stopped,
        jit::RET_INTERP => Ran::Missed,
        _ => Ran::Exit(EXIT.with(|e| e.take()).unwrap_or(Exit::Continue)),
    }
}

#[cfg(target_os = "emscripten")]
fn verify64() -> u32 {
    use std::sync::OnceLock;
    static ON: OnceLock<u32> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("IPPAI_JIT64_VERIFY").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        if on == 1 {
            jit::long::DEBUG_INS.store(1, Ordering::Relaxed);
            jit::long::DEBUG_BLOCKS.store(1, Ordering::Relaxed);
        }
        on
    })
}

/// Guest memory for a dry run of one instruction: reads see its own writes, which are kept
/// (address, byte) instead of done.
#[cfg(target_os = "emscripten")]
struct Recorder(Vec<(u32, u8)>, HashMap<u32, u8>);

#[cfg(target_os = "emscripten")]
impl koji::Memory for Recorder {
    fn read8(&mut self, addr: u32) -> Result<u8, koji::Fault> {
        match self.1.get(&addr) {
            Some(&v) => Ok(v),
            None => koji::Memory::read8(&mut Linear, addr),
        }
    }
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), koji::Fault> {
        koji::Memory::read8(&mut Linear, addr)?;
        self.0.push((addr, v));
        self.1.insert(addr, v);
        Ok(())
    }
}

/// How `got` differs from the interpreter's `want` (its writes: `writes`).
#[cfg(target_os = "emscripten")]
fn diff64(want: &Cpu, got: &Cpu, writes: &[(u32, u8)]) -> Vec<String> {
    let (mut w, mut g) = (want.clone(), got.clone());
    w.materialize();
    g.materialize();
    let mut d = Vec::new();
    for i in 0..16 {
        if w.gpr(i) != g.gpr(i) {
            d.push(format!("r{i} want {:#x} got {:#x}", w.gpr(i), g.gpr(i)));
        }
    }
    if w.eflags & 0x8d5 != g.eflags & 0x8d5 {
        d.push(format!("flags want {:#x} got {:#x}", w.eflags & 0x8d5, g.eflags & 0x8d5));
    }
    let mut last = HashMap::new();
    for &(a, v) in writes {
        last.insert(a, v);
    }
    for (&a, &v) in &last {
        let now = koji::Memory::read8(&mut Linear, a).unwrap_or(0);
        if now != v {
            d.push(format!("mem {a:#x} want {v:#x} got {now:#x}"));
            break;
        }
    }
    d
}

/// Reports a region run (from `before` to `got`) that no interpreter state of the same RIP
/// matches (`states`: after each instruction, with how many of `writes` were made).
#[cfg(target_os = "emscripten")]
fn check64(before: &Cpu, states: &[(Cpu, usize)], got: &Cpu, writes: &[(u32, u8)]) {
    static N: AtomicU32 = AtomicU32::new(0);
    if N.fetch_add(1, Ordering::Relaxed) % 100_000 == 0 {
        eprintln!("vnjit64 verify: {} checked", N.load(Ordering::Relaxed));
    }
    let mut first = None;
    let mut arrivals = 0;
    for (c, n) in states {
        if (c.eip, c.rip_hi) != (got.eip, got.rip_hi) {
            continue;
        }
        arrivals += 1;
        let d = diff64(c, got, &writes[..*n]);
        if d.is_empty() {
            return;
        }
        first.get_or_insert(d);
    }
    let d = first.unwrap_or_else(|| vec![format!("rip {:x}{:08x} not reached in {} steps", got.rip_hi, got.eip, states.len())]);
    if arrivals == 0 && states.len() >= 400 {
        return; // (too far)
    }
    static SEEN: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
    if !SEEN.lock().unwrap().get_or_insert_with(HashSet::new).insert(before.eip) {
        return;
    }
    let mut bf = before.clone();
    bf.materialize();
    let regs: Vec<String> = (0..16).map(|i| format!("{:x}", bf.gpr(i))).collect();
    eprintln!("vnjit64 verify {:x}{:08x} -> {:x}{:08x} ({} steps, {arrivals} arrivals): {} | before flags {:#x} regs {}", before.rip_hi, before.eip, got.rip_hi, got.eip, states.len(), d.join("; "), bf.eflags, regs.join(" "));
}

/// Sets the entry of `eip`; true if it is new.
/// Whether `eip` has a block in the hash table.
fn compiled(hash: &[[u32; 2]], eip: u32) -> bool {
    let mask = hash.len() as u32 - 1;
    let mut h = jit::dispatch::slot(eip, mask);
    while hash[h as usize][0] != 0 {
        if hash[h as usize][0] == eip {
            return true;
        }
        h = (h + 1) & mask;
    }
    false
}

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
    if let Some(dir) = std::env::var_os("IPPAI_JIT_DUMP") {
        let name = format!("{:08x}-{}.wasm", roots[0], roots.len());
        let _ = std::fs::write(std::path::Path::new(&dir).join(name), bytes);
    }
    #[cfg(target_os = "emscripten")]
    let base = loaded(unsafe { vnvm_jit_load(bytes.as_ptr(), bytes.len(), 1) }, 1, bytes.len());
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
    /// each entry's table index past the bundle's first
    offsets: Vec<u32>,
    /// regions by their first root
    by_root: HashMap<u32, Vec<usize>>,
    /// functions by their entry (all builds)
    by_fn: HashMap<u32, Vec<usize>>,
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
        let (mut by_root, mut by_fn): (HashMap<u32, Vec<usize>>, HashMap<u32, Vec<usize>>) = (HashMap::new(), HashMap::new());
        for (i, e) in entries.iter().enumerate() {
            if e.build > 0 { &mut by_fn } else { &mut by_root }.entry(e.roots[0]).or_default().push(i);
        }
        Saved { offsets: jit::saved::offsets(&entries), entries, by_root, by_fn }
    })
}

/// A saved region for `roots` (any built from `roots[0]` when `exact` is off: the one with most
/// roots) whose guest code is still there, and clear of the trap range `trap`.
fn find_saved(mem: &mut Linear, roots: &[u32], exact: bool, trap: (u32, u32)) -> Option<usize> {
    if profiling() || jit::page_check() {
        return None; // saved regions have no counters, nor page checks
    }
    let s = saved_regions();
    s.by_root
        .get(&roots[0])?
        .iter()
        .copied()
        .filter(|&i| !exact || s.entries[i].roots == roots)
        .filter(|&i| s.entries[i].code.iter().all(|&(a, n)| a.saturating_add(n) <= trap.0 || a >= trap.1))
        .filter(|&i| code_kept(&s.entries[i].code) && code_bytes(mem, &s.entries[i].code).is_some_and(|t| t == s.entries[i].text))
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
    let s = saved_regions();
    let e = &s.entries[i];
    mark_code(&e.code);
    enter(st, base as u32 + s.offsets[i], e.roots.clone(), &e.starts);
    true
}

/// A saved function at `eip` (its newest build) whose guest code is still there.
fn find_saved_fn(mem: &mut Linear, eip: u32) -> Option<usize> {
    if profiling() || jit::page_check() {
        return None;
    }
    let s = saved_regions();
    s.by_fn
        .get(&eip)?
        .iter()
        .copied()
        .filter(|&i| code_kept(&s.entries[i].code) && code_bytes(mem, &s.entries[i].code).is_some_and(|t| t == s.entries[i].text))
        .max_by_key(|&i| s.entries[i].build)
}

/// Puts saved function `i` in.
fn load_saved_fn(st: &mut State, i: usize) -> bool {
    #[cfg(target_os = "emscripten")]
    let base = *st.saved.get_or_insert_with(|| unsafe { vnvm_jit_bundle_load() });
    #[cfg(not(target_os = "emscripten"))]
    let base = -1;
    if base < 0 {
        return false;
    }
    let s = saved_regions();
    let e = &s.entries[i];
    st.fn_builds.insert(e.roots[0], e.build);
    enter_fn(st, base as u32 + s.offsets[i], e.roots[0], &e.starts, &e.sites, &e.code);
    true
}

/// Sends the regions built since the last save to IPPAI_JIT_URL (as one module and its index)
/// once the run is past its start, at most every SAVE_EVERY and only when there are enough to
/// be worth it: each save changes the module, and Chromium then forgets its compiled code.
fn maybe_save() {
    const SAVE_AFTER: Duration = Duration::from_secs(20);
    const SAVE_EVERY: Duration = Duration::from_secs(30);
    const SAVE_MIN: usize = 32;
    static URL: std::sync::OnceLock<Option<std::ffi::CString>> = std::sync::OnceLock::new();
    static SAVE: Mutex<(Option<Instant>, Option<Instant>, Option<HashMap<Vec<u32>, u32>>, bool)> = Mutex::new((None, None, None, false));
    let Some(url) = URL.get_or_init(|| std::env::var("IPPAI_JIT_URL").ok().and_then(|u| std::ffi::CString::new(u).ok())) else {
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
    // (a function's newest build; a newer one than saved goes again)
    let done = g.2.get_or_insert_with(HashMap::new);
    let new: Vec<(Vec<u32>, Arc<Shared>)> = {
        let sh = SHARED.lock().unwrap();
        let Some(sh) = sh.as_ref() else { return };
        sh.iter()
            .filter_map(|(r, l)| {
                let s = l.iter().max_by_key(|s| s.build)?;
                (done.get(r).is_none_or(|&b| s.build > b)).then(|| (r.clone(), s.clone()))
            })
            .collect()
    };
    if new.len() < SAVE_MIN {
        return;
    }
    g.1 = Some(now);
    for (r, s) in &new {
        done.insert(r.clone(), s.build);
    }
    // the first time, the store chunks the run read so far too (for the page to fetch ahead)
    let chunks = if g.3 { None } else { Some(crate::vfs::fetched()) };
    g.3 = true;
    drop(guard);
    // merged and sent on a thread of its own: the merge of a few hundred functions took the
    // game's thread tens of ms
    static SAVER: std::sync::OnceLock<Option<std::sync::mpsc::Sender<Save>>> = std::sync::OnceLock::new();
    let tx = SAVER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Save>();
        std::thread::Builder::new().name("vnjit-save".into()).spawn(move || rx.into_iter().for_each(save)).ok()?;
        Some(tx)
    });
    let job = (url.as_c_str(), new, chunks);
    match tx {
        Some(tx) => {
            let _ = tx.send(job);
        }
        None => save(job),
    }
}

/// What [`save`] sends: the URL, the new regions, the chunks read (the first time).
type Save = (&'static std::ffi::CStr, Vec<(Vec<u32>, Arc<Shared>)>, Option<Vec<u8>>);

fn save((url, new, chunks): Save) {
    let entries: Vec<jit::saved::Entry> = new
        .iter()
        .map(|(r, s)| jit::saved::Entry {
            roots: if s.build > 0 { r[..1].to_vec() } else { r.clone() },
            starts: s.starts.clone(),
            code: s.code.clone(),
            text: s.text.clone(),
            build: s.build,
            sites: s.sites.clone(),
        })
        .collect();
    let mods: Vec<&[u8]> = new.iter().map(|(_, s)| &s.bytes[..]).collect();
    let names = jit::saved::func_names(&entries);
    let Some(wasm) = jit::wasm::merge(&mods, &names) else {
        eprintln!("vnjit: save: {} modules don't merge", mods.len());
        return;
    };
    let idx = jit::saved::encode(&entries);
    if stats() || crate::env_on!("IPPAI_JIT_SAVE_LOG") {
        eprintln!("vnjit: saving {} regions ({} bytes)", entries.len(), wasm.len());
    }
    let mut body = (wasm.len() as u32).to_le_bytes().to_vec();
    body.extend_from_slice(&wasm);
    body.extend_from_slice(&idx);
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
    if cpu.mode64 != 0 {
        return merge_hot64(st, cpu, &jumps);
    }
    // (only the runs that could matter: both users below need FN_SAMPLES or more; this runs
    // every RING_SIZE hops, so no allocation for the usual none)
    let mut runs: Vec<(usize, u64)> = Vec::new();
    let mut i = 0;
    while i < N {
        let mut k = i + 1;
        while k < N && jumps[k] == jumps[i] {
            k += 1;
        }
        if k - i >= FN_SAMPLES.min(MERGE_SAMPLES) {
            runs.push((k - i, jumps[i]));
        }
        i = k;
    }
    runs.sort_by(|a, b| b.0.cmp(&a.0));
    if stats() {
        thread_local! { static N: Cell<u32> = const { Cell::new(0) }; }
        if N.with(|n| { n.set(n.get() + 1); n.get() % 4000 == 0 }) {
            let top: Vec<String> = runs.iter().take(6).map(|(c, p)| format!("{:x}->{:x}:{c}", p >> 32, *p as u32)).collect();
            eprintln!("vnjit ring {}", top.join(" "));
        }
    }
    // functions (jit::FN): a call or tail jump that found no function there, or a function
    // left often (built again with the blocks that ran since)
    for &(c, p) in &runs {
        let (from, to) = ((p >> 32) as u32, p as u32);
        if c < FN_SAMPLES {
            break;
        }
        // (FN_TAG | site, target): an indirect call or jump; a new hint builds its function again
        if from & jit::FN_TAG != 0 && from != jit::FN_TAG {
            let site = from & !jit::FN_TAG;
            let Some(fs) = st.site_fn.get(&site).cloned() else {
                if stats() {
                    eprintln!("vnjit site {site:#x}: no function");
                }
                continue;
            };
            if st.hints.get(&site) == Some(&to) || st.rehinted.contains(&site) {
                continue;
            }
            let w = st.hot.entry((from, to)).or_insert(0);
            *w += 1;
            if *w >= FN_WINDOWS {
                st.hot.remove(&(from, to));
                if st.hints.insert(site, to).is_some() {
                    st.rehinted.insert(site);
                }
                if stats() {
                    eprintln!("vnjit hint {site:#x} -> {to:#x} (functions {fs:x?})");
                }
                // every function with the site
                for f in fs {
                    if st.fn_builds.get(&f).copied().unwrap_or(0) < FN_BUILDS {
                        compile_fn(st, f, (cpu.trap_lo, cpu.trap_hi), &mut Linear, false);
                    }
                }
                return;
            }
            continue;
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
            compile_fn(st, f, (cpu.trap_lo, cpu.trap_hi), &mut Linear, false);
            return;
        }
    }
    // regions: the most frequent pair between them
    let Some(&(c, best)) = runs.iter().find(|r| {
        let (from, to) = ((r.1 >> 32) as u32, r.1 as u32);
        from & jit::FN_TAG == 0 && !st.fns.contains(&from) && !st.fns.contains(&to)
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
        mark_code(&r.code);
        load(st, &r.bytes, roots, &r.starts);
        return;
    }
    if let Some(i) = find_saved(&mut Linear, &roots, true, (cpu.trap_lo, cpu.trap_hi)) {
        st.merges += 1;
        load_saved(st, i);
        return;
    }
    let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
    if cpu.mode64 != 0 {
        return merge_long(st, cpu, roots, from, to);
    }
    let mut m = Module { name: Some(format!("x86_{:08x}_{}", roots[0], roots.len())), ..Module::default() };
    if let Some(n) = std::env::var("IPPAI_JIT_SLOTS").ok().and_then(|v| v.parse::<u32>().ok()) {
        jit::SLOTS_MAX.store(n.min(6), std::sync::atomic::Ordering::Relaxed);
    }
    if crate::env_on!("IPPAI_JIT_LAYOUT") {
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
    mark_code(&region.code);
    if load(st, &bytes, roots.clone(), &region.starts) {
        keep_profile(&mut region);
        share(&mut Linear, &roots, bytes, &region);
        maybe_save();
    }
}

/// merge_hot for long mode (jit::long): the region that left for others most (out of the
/// ring's sorted samples `jumps`) is built again with the targets it went to more than once
/// (a switch's cases, a callee, a virtual call's methods), once it topped MERGE_WINDOWS rings.
fn merge_hot64(st: &mut State, cpu: &Cpu, jumps: &[u64]) {
    let mut best = (0, 0u32);
    let mut i = 0;
    while i < jumps.len() {
        let from = (jumps[i] >> 32) as u32;
        let k = i + jumps[i..].iter().take_while(|j| (**j >> 32) as u32 == from).count();
        if k - i > best.0 {
            best = (k - i, from);
        }
        i = k;
    }
    let (n, from) = best;
    if n < MERGE_SAMPLES || st.merges >= MAX_MERGES {
        return;
    }
    let w = st.hot.entry((from, u32::MAX)).or_insert(0);
    *w += 1;
    if *w < MERGE_WINDOWS {
        return;
    }
    st.hot.remove(&(from, u32::MAX));
    let Some(roots) = st.roots.get(&from) else { return };
    let mut roots = roots.clone();
    let mut new = Vec::new();
    let mut i = 0;
    while i < jumps.len() {
        let k = i + jumps[i..].iter().take_while(|j| **j == jumps[i]).count();
        let to = jumps[i] as u32;
        if (jumps[i] >> 32) as u32 == from && k - i >= 2 && !roots.contains(&to) && st.merged.insert((from, to)) {
            new.push(to);
        }
        i = k;
    }
    if new.is_empty() || roots.len() + new.len() > 32 {
        return;
    }
    let to = new[0];
    roots.extend(new);
    if let Some(r) = shared(&mut Linear, &roots) {
        st.merges += 1;
        mark_code(&r.code);
        load(st, &r.bytes, roots, &r.starts);
        return;
    }
    merge_long(st, cpu, roots, from, to);
}

/// merge_hot's region for long mode (jit::long): the roots' blocks and the ones that ran.
fn merge_long(st: &mut State, cpu: &Cpu, roots: Vec<u32>, from: u32, to: u32) {
    let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
    let full: Vec<u64> = roots.iter().map(|&r| r as u64 | (cpu.rip_hi as u64) << 32).collect();
    let mut m = Module { name: Some(format!("x64_{:x}{:08x}_{}", cpu.rip_hi, roots[0], roots.len())), ..Module::default() };
    let (misses, known, hash) = (&st.misses, &st.known, &st.hash);
    let Some(region) = jit::long::translate_region64(&mut m, &mut Linear, &full, (lo, hi), |a, sure| {
        debug_allowed(a) && !(a >= lo && a < hi) && (misses.contains_key(&a) || compiled(hash, a) || sure && !known.contains(&a))
    }) else {
        return;
    };
    if !region.starts.iter().any(|s| s.0 == to) {
        return; // too big already
    }
    st.merges += 1;
    if stats() || crate::env_on!("IPPAI_JIT_LAYOUT") {
        eprintln!("vnjit64 merge {from:#x} -> {to:#x}: {} blocks, roots {:x?}\n{}", region.starts.len(), roots, region.layout);
    }
    let bytes = m.finish();
    mark_code(&region.code);
    if load(st, &bytes, roots.clone(), &region.starts) {
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
        if !st.pending.is_empty() {
            take_built(st, mem);
        }
        if st.known.contains(&eip) {
            return false;
        }
        // another thread's region from the same code goes in at once
        if st.misses.get(&eip).is_none() {
            if jit::FN.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(r) = shared(mem, &fn_key(eip)) {
                    st.fn_builds.insert(eip, r.build);
                    return install_fn(st, eip, &r.bytes, &r.starts, &r.sites, &r.code);
                }
                if cpu.mode64 == 0 {
                    if let Some(i) = find_saved_fn(mem, eip) {
                        if load_saved_fn(st, i) {
                            return true;
                        }
                    }
                }
            }
            if let Some(r) = shared(mem, &[eip]) {
                mark_code(&r.code);
                return load(st, &r.bytes, vec![eip], &r.starts);
            }
            // (a 64-bit region checks the upper half of RIP itself on entry)
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
        let fnm = jit::FN.load(std::sync::atomic::Ordering::Relaxed) && cpu.mode64 == 0 && fn_allowed(eip);
        let part = fnm && !called(cpu, mem);
        // a block inside a function the compiler thread has yet to finish: most likely in it
        // (the interpreter runs its blocks meanwhile, each reaching the threshold), so not
        // built on its own unless it is still missing after
        if part && !st.pending.is_empty() {
            return false;
        }
        st.misses.remove(&eip);
        if cpu.mode64 != 0 {
            return compile_long(st, cpu, mem);
        }
        if fnm {
            // (part: not a function's entry, as where a function's blocks that had not run yet
            // go on: without the blocks compiled already, which it enters through the dispatcher)
            return compile_fn(st, eip, (cpu.trap_lo, cpu.trap_hi), mem, part);
        }
        // the block and what it statically reaches, as one function
        let mut m = Module { name: Some(format!("x86_{eip:08x}")), ..Module::default() };
        if crate::env_on!("IPPAI_JIT_LAYOUT") {
            jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
        let (misses, known) = (&st.misses, &st.known);
        profiling();
        let t0 = std::time::Instant::now();
        let Some(mut region) = jit::translate_region(&mut m, mem, &[eip], MAX_BLOCKS, |a| {
            debug_allowed(a) && !(a >= lo && a < hi) && !known.contains(&a) && (a == eip || misses.contains_key(&a))
        }) else {
            if stats() {
                eprintln!("vnjit refused {eip:#x}");
            }
            st.known.insert(eip);
            return false;
        };
        if crate::env_on!("IPPAI_JIT_LAYOUT") {
            eprintln!("vnjit fresh {eip:#x}\n{}", region.layout);
        }
        let bytes = m.finish();
        let t1 = std::time::Instant::now();
        mark_code(&region.code);
        let ok = load(st, &bytes, vec![eip], &region.starts);
        count_compile(0, t0);
        if crate::env_on!("IPPAI_JIT_TIME") {
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

/// Compiles the long-mode region at cpu.eip (jit::long; no FN mode or merges yet).
fn compile_long(st: &mut State, cpu: &Cpu, mem: &mut Linear) -> bool {
    let eip = cpu.eip;
    let mut m = Module { name: Some(format!("x64_{:x}{eip:08x}", cpu.rip_hi)), ..Module::default() };
    let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
    let (misses, known, hash) = (&st.misses, &st.known, &st.hash);
    let entry = eip as u64 | (cpu.rip_hi as u64) << 32;
    if crate::env_on!("IPPAI_JIT_LAYOUT") {
        jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let t0 = std::time::Instant::now();
    let Some(region) = jit::long::translate_region64(&mut m, mem, &[entry], (lo, hi), |a, sure| {
        // (a block reached by a call or a jump table may be in another region already: it is
        // taken in too, the jump staying inside; `known` has those and the refused ones)
        debug_allowed(a) && !(a >= lo && a < hi) && (!known.contains(&a) || sure && compiled(hash, a)) && (a == eip || sure || misses.contains_key(&a))
    }) else {
        st.known.insert(eip);
        return false;
    };
    if crate::env_on!("IPPAI_JIT_LAYOUT") {
        eprintln!("vnjit64 region {entry:#x}\n{}", region.layout);
    }
    let bytes = m.finish();
    let t1 = std::time::Instant::now();
    let ok = load(st, &bytes, vec![eip], &region.starts);
    if crate::env_on!("IPPAI_JIT_TIME") {
        eprintln!("vnjit time {eip:#x}: {} ins, translate {:?}, load {:?}", region.instructions, t1 - t0, t1.elapsed());
    }
    if ok {
        mark_code(&region.code);
        share(mem, &[eip], bytes, &region);
        maybe_save();
    } else {
        st.known.insert(eip);
    }
    ok
}

/// Whether cpu.eip looks like the target of a CALL whose return address is on the stack.
fn called(cpu: &Cpu, mem: &mut Linear) -> bool {
    let mut b = [0u8; 4];
    if koji::Memory::fetch(mem, cpu.regs[4], &mut b) < 4 {
        return false;
    }
    let ra = u32::from_le_bytes(b);
    let mut c = [0u8; 7];
    if ra < 7 || koji::Memory::fetch(mem, ra - 7, &mut c) < 7 {
        return false;
    }
    if c[2] == 0xe8 && ra.wrapping_add(u32::from_le_bytes([c[3], c[4], c[5], c[6]])) == cpu.eip {
        return true;
    }
    [2usize, 3, 4, 6, 7].iter().any(|&k| c[7 - k] == 0xff && (c[8 - k] >> 3) & 7 == 2)
}

/// Compiles the function at cpu.eip (jit::FN): its adapter and itself, the hash entry tagged.
static FN_INS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn compile_fn(st: &mut State, eip: u32, (lo, hi): (u32, u32), mem: &mut Linear, part: bool) -> bool {
    if st.pending.contains(&eip) {
        return false;
    }
    // another thread's build (as new as the one this would be, or newer)
    if let Some(r) = shared(mem, &fn_key(eip)) {
        if r.build > st.fn_builds.get(&eip).copied().unwrap_or(0) {
            st.fn_builds.insert(eip, r.build);
            return install_fn(st, eip, &r.bytes, &r.starts, &r.sites, &r.code);
        }
    }
    *st.fn_builds.entry(eip).or_insert(0) += 1;
    let build = st.fn_builds[&eip];
    if let Some(jobs) = compiler() {
        // translated on the compiler thread; the interpreter goes on meanwhile (a build takes
        // a few ms here, several times that on a phone: a dropped frame each, in the story's
        // scene changes)
        let tc = Instant::now();
        let job = Job { eip, lo, hi, part, build, misses: st.misses.clone(), known: st.known.clone(), hints: st.hints.clone(), tid: std::thread::current().id(), done: st.done_tx.clone() };
        if crate::env_on!("IPPAI_JIT_TIME") {
            eprintln!("vnjit job {eip:#x}: misses {} known {} hints {}, copied in {:?}", st.misses.len(), st.known.len(), st.hints.len(), tc.elapsed());
        }
        if jobs.send(job).is_ok() {
            st.pending.insert(eip);
            return false;
        }
    }
    let t0 = Instant::now();
    let built = build_fn(mem, eip, (lo, hi), part, build, &st.misses, &st.known, &st.hints);
    finish_fn(st, eip, build, built, None, t0)
}

/// A function translated (on the compiler thread or not): (module, region, its code bytes).
type Built = Option<(Vec<u8>, jit::Region, Vec<u8>)>;

/// A function for the compiler thread to translate, with this thread's view of what ran.
struct Job {
    eip: u32,
    lo: u32,
    hi: u32,
    part: bool,
    build: u32,
    misses: HashMap<u32, u32>,
    known: HashSet<u32>,
    hints: HashMap<u32, u32>,
    /// the guest thread asking (its jobs that wait together come back as one batch)
    tid: std::thread::ThreadId,
    done: std::sync::mpsc::Sender<Batch>,
}

/// Functions the compiler thread finished for one guest thread: (eip, build, built) each, and
/// when there are several, one module of all the built ones (in order, two functions each:
/// adapter, function) so the guest thread loads them at once. Loading a module costs the guest
/// thread far more per module than per function, and a scene change asks for hundreds.
struct Batch {
    items: Vec<(u32, u32, Built)>,
    merged: Option<Vec<u8>>,
    /// built with page checks (koji's PAGE_MAP)
    checked: bool,
}

/// Functions one batch takes, at most (one load: a longer pause of its own).
const BATCH_MAX: usize = 16;

/// The compiler thread's queue (IPPAI_JIT_ASYNC=0: none, functions are translated where they
/// are needed).
fn compiler() -> Option<&'static std::sync::mpsc::Sender<Job>> {
    static Q: std::sync::OnceLock<Option<std::sync::mpsc::Sender<Job>>> = std::sync::OnceLock::new();
    Q.get_or_init(|| {
        if std::env::var("IPPAI_JIT_ASYNC").is_ok_and(|v| v == "0") {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("vnjit".into())
            .spawn(move || {
                // the jobs waiting when one is taken go with it, a batch per guest thread
                let mut waiting: std::collections::VecDeque<Job> = std::collections::VecDeque::new();
                loop {
                    let Some(j) = waiting.pop_front().or_else(|| rx.recv().ok()) else { break };
                    waiting.extend(rx.try_iter());
                    let mut jobs = vec![j];
                    let mut i = 0;
                    while i < waiting.len() && jobs.len() < BATCH_MAX {
                        if waiting[i].tid == jobs[0].tid {
                            jobs.extend(waiting.remove(i));
                        } else {
                            i += 1;
                        }
                    }
                    let done = jobs[0].done.clone();
                    let checked = jit::page_check();
                    let items: Vec<(u32, u32, Built)> = jobs
                        .into_iter()
                        .map(|j| (j.eip, j.build, build_fn(&mut Linear, j.eip, (j.lo, j.hi), j.part, j.build, &j.misses, &j.known, &j.hints)))
                        .collect();
                    let mods: Vec<&[u8]> = items.iter().filter_map(|(_, _, b)| b.as_ref().map(|b| &b.0[..])).collect();
                    let merged = if mods.len() > 1 { jit::wasm::merge(&mods, &[]) } else { None };
                    let _ = done.send(Batch { items, merged, checked });
                }
            })
            .ok()?;
        Some(tx)
    })
    .as_ref()
}

/// Installs the functions the compiler thread finished for this thread.
fn take_built(st: &mut State, mem: &mut Linear) {
    while let Ok(Batch { items, merged, checked }) = st.done_rx.try_recv() {
        // (built before the page checks came on: dropped, translated again when hot)
        if jit::page_check() && !checked {
            for (eip, _, _) in &items {
                st.pending.remove(eip);
            }
            continue;
        }
        // (the code changed since: dropped, as if it had not been translated; it is again
        // when hot)
        let kept: Vec<bool> = items
            .iter()
            .map(|(_, _, b)| b.as_ref().is_none_or(|(_, r, text)| code_kept(&r.code) && code_bytes(mem, &r.code).is_some_and(|t| t == *text)))
            .collect();
        // all of them at once when none was dropped
        let t0 = Instant::now();
        let mut base = match merged {
            Some(m) if kept.iter().all(|&k| k) => {
                let n = 2 * items.iter().filter(|i| i.2.is_some()).count();
                #[cfg(target_os = "emscripten")]
                let b = loaded(unsafe { vnvm_jit_load(m.as_ptr(), m.len(), n as u32) }, n as u32, m.len());
                #[cfg(not(target_os = "emscripten"))]
                let b = { let _ = (m, n); -1 };
                if crate::env_on!("IPPAI_JIT_TIME") {
                    eprintln!("vnjit time batch of {}: load {:?}", n / 2, t0.elapsed());
                }
                (b >= 0).then_some(b as u32)
            }
            _ => None,
        };
        for ((eip, build, built), k) in items.into_iter().zip(kept) {
            st.pending.remove(&eip);
            let at = built.is_some().then_some(base).flatten();
            if built.is_some() {
                base = base.map(|b| b + 2);
            }
            if k {
                finish_fn(st, eip, build, built, at, t0);
            }
        }
    }
}

fn build_fn(mem: &mut Linear, eip: u32, (lo, hi): (u32, u32), part: bool, build: u32, misses: &HashMap<u32, u32>, known: &HashSet<u32>, hints: &HashMap<u32, u32>) -> Built {
    let t0 = Instant::now();
    if crate::env_on!("IPPAI_JIT_LAYOUT") {
        jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    profiling();
    let mut m = Module { name: Some(format!("x86_{eip:08x}_fn")), ..Module::default() };
    // a function built again (it left its blocks often) may take more: twice per build
    // (big interpreters' loops, a C library's long switch), from IPPAI_JIT_FN_INS
    let base = *FN_INS.get_or_init(|| std::env::var("IPPAI_JIT_FN_INS").ok().and_then(|v| v.parse().ok()).unwrap_or(1000));
    let region = jit::translate_function(&mut m, mem, eip, jit::MAX_BLOCKS, base << (build.min(4) - 1), (lo, hi), &|a| hints.get(&a).copied(), &|a| debug_allowed(a) && ran(misses, known, a), |a| debug_allowed(a) && !(a >= lo && a < hi) && (a == eip || if part { !known.contains(&a) && misses.contains_key(&a) } else { ran(misses, known, a) }))?;
    if crate::env_on!("IPPAI_JIT_LAYOUT") {
        eprintln!("vnjit fn {eip:#x}\n{}", region.layout);
    }
    if stats() {
        eprintln!("vnjit fn {eip:#x} build {build}: {} instructions", region.instructions);
    }
    let bytes = m.finish();
    if let Some(dir) = std::env::var_os("IPPAI_JIT_DUMP") {
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{eip:08x}-fn.wasm")), &bytes);
    }
    if crate::env_on!("IPPAI_JIT_TIME") {
        eprintln!("vnjit time {eip:#x}: {} ins, translate {:?} build {build} part {part}", region.instructions, t0.elapsed());
    }
    let text = code_bytes(mem, &region.code)?;
    Some((bytes, region, text))
}

/// Loads a translated function (or notes that it could not be); `at`: already loaded (with
/// others) at that table index.
fn finish_fn(st: &mut State, eip: u32, build: u32, built: Built, at: Option<u32>, t0: Instant) -> bool {
    let Some((bytes, mut region, text)) = built else {
        st.known.insert(eip);
        return false;
    };
    let t1 = Instant::now();
    let ok = match at {
        Some(base) => {
            enter_fn(st, base, eip, &region.starts, &region.sites, &region.code);
            true
        }
        None => install_fn(st, eip, &bytes, &region.starts, &region.sites, &region.code),
    };
    if at.is_none() && crate::env_on!("IPPAI_JIT_TIME") {
        eprintln!("vnjit time {eip:#x}: {} ins, load {:?}", region.instructions, t1.elapsed());
    }
    count_compile(1, t0);
    if build > 1 {
        count_compile(2, t0);
    }
    if ok {
        keep_profile(&mut region);
        let r = Arc::new(Shared { bytes, starts: region.starts, code: region.code, text, build, sites: region.sites });
        SHARED.lock().unwrap().get_or_insert_with(HashMap::new).entry(fn_key(eip).to_vec()).or_default().push(r);
        maybe_save();
    }
    ok
}

/// Compiles a function's module (adapter, function) and points the hash table at its blocks.
fn install_fn(st: &mut State, eip: u32, bytes: &[u8], starts: &[(u32, u32)], sites: &[u32], code: &[(u32, u32)]) -> bool {
    #[cfg(target_os = "emscripten")]
    let base = loaded(unsafe { vnvm_jit_load(bytes.as_ptr(), bytes.len(), 2) }, 2, bytes.len());
    #[cfg(not(target_os = "emscripten"))]
    let base = { let _ = bytes; -1 };
    st.known.insert(eip);
    if base < 0 {
        return false;
    }
    enter_fn(st, base as u32, eip, starts, sites, code);
    true
}

/// Points the hash table at the function (adapter at table index `base`, itself next).
fn enter_fn(st: &mut State, base: u32, eip: u32, starts: &[(u32, u32)], sites: &[u32], code: &[(u32, u32)]) {
    st.known.insert(eip);
    mark_code(code);
    for &a in sites {
        let l = st.site_fn.entry(a).or_default();
        if !l.contains(&eip) {
            l.push(eip);
        }
    }
    st.fns.insert(eip);
    for &(a, _) in starts {
        st.known.insert(a);
    }
    enter(st, base as u32, vec![eip], &[]);
    while (st.used + starts.len()) * 2 > st.hash.len() {
        let mut bigger = vec![[0u32; 2]; st.hash.len() * 2];
        for e in st.hash.iter().filter(|e| e[0] != 0) {
            insert(&mut bigger, e[0], e[1]);
        }
        st.hash = bigger;
    }
    for (i, &(a, _)) in starts.iter().enumerate() {
        // (which entry: 0 the root, the first start)
        let v = base as u32 | (i as u32) << jit::ENTRY_SHIFT | jit::FN_TAG;
        if insert(&mut st.hash, a, v) {
            st.used += 1;
        }
        // call sites that cached an earlier build go to this one
        if i == 0 {
            for c in st.calls.iter_mut().filter(|c| c[0] == a) {
                c[1] = v;
            }
        }
    }
}

thread_local! {
    static DECODED: RefCell<koji::DecodeCache> = RefCell::new(koji::DecodeCache::new());
}

/// One instruction in the interpreter (with this thread's cache of decoded instructions).
pub(crate) fn step(cpu: &mut Cpu, mem: &mut Linear) -> (Exit, bool) {
    DECODED.with(|d| match d.try_borrow_mut() {
        Ok(mut d) => koji::step_cached(cpu, mem, Some(&mut d)),
        // (an instruction that calls back into the guest: not while the cache is in use)
        Err(_) => koji::step_flow(cpu, mem),
    })
}

/// A call site's miss in the call cache (function mode): the hash table's entry for `target`
/// if it is a function's (also put in the cache slot at `slot`), else 0 with (FN_TAG, target)
/// in the ring (a callee called often becomes a function, see merge_hot).
#[no_mangle]
pub unsafe extern "C" fn vnhost_jit_callmiss(cpu: u32, slot: u32, target: u32) -> u32 {
    let cpu = &*(cpu as usize as *const Cpu);
    let hash = std::slice::from_raw_parts(cpu.jit_hash as usize as *const [u32; 2], cpu.jit_mask as usize + 1);
    let mut h = jit::dispatch::slot(target, cpu.jit_mask);
    while hash[h as usize][0] != 0 {
        if hash[h as usize][0] == target {
            let v = hash[h as usize][1];
            // (a call goes to a function's root only)
            if v & jit::FN_TAG == 0 || v >> jit::ENTRY_SHIFT & 0x7ff != 0 {
                break;
            }
            *(slot as usize as *mut [u32; 2]) = [target, v];
            return v;
        }
        h = (h + 1) & cpu.jit_mask;
    }
    let ring = cpu.jit_ring as usize as *mut u32;
    let n = *ring;
    *ring = n.wrapping_add(1);
    let i = (n & (jit::RING_SIZE - 1)) as usize;
    *ring.add(1 + 2 * i) = jit::FN_TAG;
    *ring.add(2 + 2 * i) = target;
    0
}

/// One instruction run by the interpreter for a block: 0 if it completed, else 2 with the exit
/// kept for [`run`].
#[no_mangle]
pub unsafe extern "C" fn vnhost_jit_step(cpu: u32) -> i32 {
    // (u32: an i32 in the region modules' imports, the 64-bit build too)
    let cpu = cpu as usize as *mut Cpu;
    if stats() {
        count_step(&*cpu);
    }
    if jit::PROBES[0].load(Ordering::Relaxed) != 0 && jit::PROBES.iter().any(|p| p.load(Ordering::Relaxed) == (*cpu).eip) {
        let c = &*cpu;
        let f = |x: u128| (0..4).map(|i| f32::from_bits((x >> (32 * i)) as u32)).collect::<Vec<_>>();
        eprintln!("vnjit probe {:#x}: regs {:x?} xmm0 {:?} xmm1 {:?} xmm2 {:?} xmm3 {:?}", c.eip, c.regs, f(c.xmm[0]), f(c.xmm[1]), f(c.xmm[2]), f(c.xmm[3]));
    }
    let g = GENERATION.load(Ordering::Relaxed);
    match step(&mut *cpu, &mut Linear).0 {
        // (it rewrote translated code: the block leaves)
        Exit::Continue if GENERATION.load(Ordering::Relaxed) != g => 2,
        // (POPF set TF, or LDMXCSR / FXRSTOR unmasked an SSE exception: from here on the
        // interpreter runs the code, see cpu_run)
        Exit::Continue if (*cpu).eflags & koji::flags::TF != 0 || (*cpu).mxcsr & 0x1f80 != 0x1f80 => 2,
        Exit::Continue => 0,
        e => {
            EXIT.with(|x| x.set(Some(e)));
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_jit_eflags(cpu: u32) -> u32 {
    let cpu = &mut *(cpu as usize as *mut Cpu);
    cpu.materialize();
    cpu.eflags
}

/// A region leaving with its x87 cache (koji::jit::x87_out).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn vnhost_jit_x87(cpu: u32, top: u32, info: u32, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64) {
    let cpu = cpu as usize as *mut Cpu;
    jit::x87_out(&mut (*cpu).fpu, top, info, [a, b, c, d, e, f, g, h]);
}

#[no_mangle]
pub extern "C" fn vnhost_jit_status(op: u32, a: u32, b: u32, r: u32, c: u32) -> u32 {
    if op == 0xdead {
        // (jit::long's IPPAI_JIT64_CHECK: a lazy condition that disagrees with the flags)
        eprintln!("vnjit64 check: condition {b} at {a:#x}: lazy {r}, eflags {c:#x}");
        return 0;
    }
    koji::lazy::Lazy { op, a: a as u64, b: b as u64, r: r as u64, c }.status()
}

/// IPPAI_JIT_PROFILE: every region's blocks (address, entry counter, instruction keys).
static PROF: Mutex<Vec<(u32, u32, Vec<String>)>> = Mutex::new(Vec::new());

fn profiling() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = crate::env_on!("IPPAI_JIT_PROFILE");
        jit::PROFILE.store(on, std::sync::atomic::Ordering::Relaxed);
        // IPPAI_JIT_PROFILE=<seconds> (> 1): print the counts every that many seconds after the
        // first region (under node, where no page asks for them; counts add up, so the difference
        // of two prints is that window)
        if let Some(secs) = std::env::var("IPPAI_JIT_PROFILE").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&s| s > 1) {
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(secs));
                vnhost_jit_profile();
            });
        }
        on
    // (set on every call: a store made only once inside get_or_init was not seen by translate
    // on other threads, and every region came back without counters)
    }) && { jit::PROFILE.store(true, std::sync::atomic::Ordering::Relaxed); true }
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
    // IPPAI_PROF_BLOCKS=<n>: more of the hottest blocks (default 40)
    let nblocks = std::env::var("IPPAI_PROF_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
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
    *ON.get_or_init(|| std::env::var_os("IPPAI_JIT_STATS").is_some())
}

/// IPPAI_JIT_STATS: which instructions blocks hand to the interpreter, printed every 2^22.
fn count_step(cpu: &Cpu) {
    thread_local! {
        static COUNTS: RefCell<(u64, HashMap<String, u64>)> = RefCell::new((0, HashMap::new()));
    }
    let mut b = [0u8; 16];
    let n = koji::Memory::fetch(&mut Linear, cpu.eip, &mut b);
    let bits = if cpu.mode64 != 0 { 64 } else { 32 };
    let mut d = koji::iced_x86::Decoder::with_ip(bits, &b[..n], cpu.eip as u64, koji::iced_x86::DecoderOptions::NONE);
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

/// IPPAI_JIT_STATS: where the JIT hands back to the run loop, printed every 2^20.
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

/// IPPAI_JIT_STATS: entries into the run loop per thread (a guest spinning on system calls
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
            eprintln!("vnjit thread {:?}: {} entries in {:?}: {}", std::thread::current().id(), c.0, t0.elapsed(), top.join(" "));
        }
    });
}

