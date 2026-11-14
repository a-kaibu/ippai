//! The x86 -> wasm JIT on the host side: per thread, a block hash table read by the
//! dispatcher module (`koji::jit::dispatch`), and the helpers block modules import
//! (`vnhost_jit_step`, `vnhost_jit_eflags`, `vnhost_jit_status`). The modules themselves are
//! compiled and instantiated by vnjit.js; each thread has its own funcref table.
//!
//! MASU_JIT=0 turns the JIT off (the interpreter runs everything). MASU_JIT_DUMP=dir writes every
//! region module to dir/<first root>-<roots>.wasm. MASU_JIT_LAYOUT=1 prints the block order, loops
//! and wasm bytes per mnemonic of new regions (of merged ones too with MASU_JIT_STATS).
//! Functions (jit::FN) are translated on a compiler thread while the interpreter goes on;
//! only loading the module stays on the guest thread (MASU_JIT_ASYNC=0: all of it there).
//!
//! Regions are kept across runs: with MASU_JIT_URL set, the regions built since the last save are
//! sent there now and then (see [`maybe_save`]; masu serve merges them into what it has), and the
//! page (or pre.js under node) hands the next run that module and index (`jit::saved`). A
//! saved region goes in when its first root first misses, if the guest code is still the same:
//! no interpreter warm-up and no translation, and in Chromium the browser's code cache of the
//! module keeps V8's optimized code too.

use crate::cpu::Linear;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use koji::jit::{self, wasm::Module};
use koji::{Cpu, Exit};

mod park;

/// Misses at an address before its block is compiled (MASU_JIT_THRESHOLD overrides).
const THRESHOLD: u32 = 50;

fn threshold() -> u32 {
    use std::sync::OnceLock;
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| std::env::var("MASU_JIT_THRESHOLD").ok().and_then(|v| v.parse().ok()).unwrap_or(THRESHOLD))
}
/// Debugging a miscompile by bisection: MASU_JIT_ONLY=lo-hi (hex) compiles only code in [lo, hi),
/// MASU_JIT_ONLY=!lo-hi all but that.
fn debug_allowed(a: u32) -> bool {
    if interpreted_page(a) {
        return false;
    }
    debug_range(a)
}

/// Pages mapped or protected writable and executable at once, and the main executables' pages.
/// Code on a W|X page outside a main exe (an unpacker's, Mono's, a JS engine's) may be
/// rewritten without a protection change, which `code_changed` can't see: its translations
/// check their code on entry (see [`wx_page`]). (UPX-style packed exes keep the JIT; their
/// stubs unpack before the code runs.)
static WX_PAGES: [AtomicU32; 1 << 14] = [const { AtomicU32::new(0) }; 1 << 14];
static EXE_PAGES: [AtomicU32; 1 << 14] = [const { AtomicU32::new(0) }; 1 << 14];

/// The first and last page of `len` (not 0) bytes at `addr` (below 2 GB), the last one no
/// further than the last page below 2 GB (addr + len is summed wide: it wrapped a 32-bit usize
/// and the range lo..=hi then ran backwards).
fn page_span(addr: u32, len: u32) -> (usize, usize) {
    let end = (addr as u64 + len as u64 - 1) >> 12;
    ((addr >> 12) as usize, end.min((1 << 19) - 1) as usize)
}

#[cfg(kani)]
mod page_span_proofs {
    #[kani::proof]
    fn span_is_ordered_and_below_2g() {
        let addr: u32 = kani::any_where(|&a| a < 1 << 31);
        let len: u32 = kani::any_where(|&l| l > 0);
        let (lo, hi) = super::page_span(addr, len);
        assert!(lo <= hi && hi < 1 << 19);
    }
}

#[cfg(test)]
mod page_span_tests {
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn span_is_ordered_and_below_2g(addr in 0u32..1 << 31, len in 1u32..) {
            let (lo, hi) = super::page_span(addr, len);
            prop_assert!(lo <= hi && hi < 1 << 19);
            prop_assert_eq!(lo as u64, addr as u64 >> 12);
            prop_assert_eq!(hi as u64, ((addr as u64 + len as u64 - 1) >> 12).min((1 << 19) - 1));
        }
    }
    #[test]
    fn the_end_of_the_address_space() {
        assert_eq!(super::page_span(0x7fff_f000, u32::MAX), (0x7ffff, 0x7ffff));
        assert_eq!(super::page_span(0x1000, 0x2001), (1, 3));
    }
}

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

/// W|X pages outside the exe, where runtimes (Mono, .NET, script JITs, protectors) put code
/// and rewrite it from code elsewhere, unseen: translated with their code checked on entry
/// (koji's jit::set_fresh), a block whose code changed going to the interpreter and translated
/// again ([`stale_hit`]). MASU_JIT_WX=0: the interpreter runs them instead.
fn wx_page(a: u32) -> bool {
    page_set(&WX_PAGES, a) && !page_set(&EXE_PAGES, a)
}
/// Pages the interpreter keeps: rewritten by their own translated code, or W|X ones with
/// MASU_JIT_WX=0.
fn interpreted_page(a: u32) -> bool {
    page_set(&SMC_PAGES, a) || (wx_page(a) && !wx_checked())
}
/// Whether code on W|X pages outside the exe is translated (checked on entry): by both
/// translators (koji::jit's Gen::fresh_entry, jit::long's: a 64-bit process's Mono code too),
/// not with MASU_JIT_WX=0.
fn wx_checked() -> bool {
    !wx_interpreted()
}
fn wx_interpreted() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENV.get_or_init(|| std::env::var("MASU_JIT_WX").is_ok_and(|v| v == "0"))
}
/// Whether W|X pages outside the exe also have their translations' stores watched like the
/// exe's ([`translate_wx_pages`], MASU_JIT_WX=1: for programs that rewrite code there in
/// place from code there, ASProtect's decryptors).
fn wx_jit() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    WX_JIT.load(Ordering::Relaxed) || *ENV.get_or_init(|| std::env::var("MASU_JIT_WX").is_ok_and(|v| v != "0"))
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

/// Code checked on entry (see [`wx_page`]) found rewritten at `a`, a block's start: the
/// translations go (everyone's; builds of the code as it is now are made again or found by
/// their bytes). A page whose code keeps changing ([`STALE_LIMIT`] times) goes to the
/// interpreter, as one rewritten by its own code does.
pub(crate) fn stale_hit(a: u32) {
    // (this thread's translations from the page that changed are parked; one made from the
    // bytes there now goes back in: park.rs. Other threads find out on their own entries.)
    let (parked, new, back) = with_state(|st| {
        let (parked, new) = park_page(st, a);
        (parked, new, parked && reinstate(st, a))
    });
    if !parked {
        note_stale(a, "since translated");
        drop_states(a as usize >> 12, a as usize >> 12);
    } else if new {
        // (code not seen before: counted, as one that keeps changing goes to the interpreter)
        note_stale(a, "since translated");
    } else if stats() {
        eprintln!("vnjit: code at {a:#x} changed back to a version seen before (translation {})", if back { "put back" } else { "not made yet" });
    }
}

/// The bytes of `code` as they are now.
fn text_now(code: &[(u32, u32)]) -> Option<Vec<u8>> {
    code_bytes(&mut Linear, code)
}

/// Keeps a translation made from code on a W|X page checked on entry, for [`park_page`].
fn track_wx(st: &mut State, base: u32, entries: Vec<(u32, u32)>, roots: Vec<u32>, func: Option<u32>, code: &[(u32, u32)]) {
    if !wx_checked() || !code.iter().any(|&(a, n)| wx_page(a) || wx_page(a.saturating_add(n.max(1) - 1))) {
        return;
    }
    let Some(text) = text_now(code) else { return };
    st.park.track(park::Wx { base, entries, roots, func, code: code.to_vec(), text });
}

/// Takes this thread's translations from `a`'s page whose code changed out of its tables and
/// parks them: whether there were any, and whether the code there is new (park::Parked::new).
fn park_page(st: &mut State, a: u32) -> (bool, bool) {
    let park::Parked { out, new, .. } = st.park.park(a, &mut |c| text_now(c));
    for w in &out {
        for &(e, v) in &w.entries {
            if park::remove(&mut st.hash, e, v, jit::dispatch::slot) {
                st.used -= 1;
            }
            st.known.remove(&e);
            st.maps_rev += 1;
            for c in st.calls.iter_mut().filter(|c| c[1] == v) {
                *c = [0, 0];
            }
        }
        if let Some(f) = w.func {
            st.fns.remove(&f);
        }
        st.roots.remove(&w.base);
    }
    (!out.is_empty(), new)
}

/// Puts back the newest parked translation with a block at `a` made from the bytes there now;
/// whether there was one.
fn reinstate(st: &mut State, a: u32) -> bool {
    if !st.park.has_parked(a) {
        return false;
    }
    let Some(w) = st.park.reinstate(a, &mut |c| text_now(c)) else { return false };
    grow(st, w.entries.len());
    for &(e, v) in &w.entries {
        if insert(&mut st.hash, e, v) {
            st.used += 1;
        }
        st.known.insert(e);
    }
    if let Some(f) = w.func {
        st.fns.insert(f);
    }
    st.code_of.entry(w.base).or_insert_with(|| w.code.clone());
    st.roots.insert(w.base, w.roots);
    true
}

/// Code at `a` on a W|X page found changed since translated (`how`): counted per page, which
/// the interpreter takes over at [`STALE_LIMIT`].
fn note_stale(a: u32, how: &str) {
    static COUNTS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
    let n = {
        let mut m = COUNTS.lock().unwrap();
        let c = m.get_or_insert_with(HashMap::new).entry(a >> 12).or_insert(0);
        *c += 1;
        *c
    };
    if stats() {
        eprintln!("vnjit: code at {a:#x} changed {how} ({n})");
    }
    if n == STALE_LIMIT {
        set_pages(&SMC_PAGES, a as usize, 1, true);
        if stats() {
            eprintln!("vnjit: page of {a:#x} keeps changing: interpreted from now on");
        }
    }
}
/// (Mono repoints each call site once, from its trampoline: a page of its methods changes
/// about once per call site in them; code rewriting itself all the time gets here fast)
const STALE_LIMIT: u32 = 256;

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
    // (code there checks itself on entry: the page stays translated)
    if wx_page(a) && wx_checked() {
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
        let v = std::env::var("MASU_JIT_ONLY").unwrap_or_default();
        let not = v.starts_with('!');
        let mut it = v.trim_start_matches('!').split('-').map(|x| u32::from_str_radix(x.trim_start_matches("0x"), 16).ok());
        match (it.next().flatten(), it.next().flatten()) {
            (Some(lo), Some(hi)) => (lo, hi, not),
            _ => (0, u32::MAX, false),
        }
    });
    (a >= lo && a < hi) != not
}
/// MASU_JIT_FN_ONLY=lo-hi[_lo-hi...] (hex): function mode only for entries in those ranges,
/// regions elsewhere.
fn fn_allowed(a: u32) -> bool {
    use std::sync::OnceLock;
    static R: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    let r = R.get_or_init(|| {
        let v = std::env::var("MASU_JIT_FN_ONLY").unwrap_or_default();
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
pub const BUDGET: i32 = 1 << 14;
/// Most blocks compiled into one module.
const MAX_BLOCKS: usize = 1024;

#[cfg(target_os = "emscripten")]
extern "C" {
    fn vnvm_jit_init(ptr: *const u8, len: usize);
    fn vnvm_jit_load(ptr: *const u8, len: usize, n: u32) -> i32;
    fn vnvm_jit_free(base: u32, n: u32);
    // JavaScript forwards this address to generated Wasm; it does not use Cpu's Rust layout.
    fn vnvm_jit_run(cpu: *mut std::ffi::c_void) -> i32;
    fn vnvm_jit_bundle_idx(ptr: *mut u8, cap: usize) -> i32;
    fn vnvm_jit_bundle_load() -> i32;
    fn vnvm_jit_save(url: *const std::ffi::c_char, ptr: *const u8, len: usize, id: u32);
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
    /// the same for each part that came after the start (see [`late_part`]; -1: it failed)
    #[cfg(target_os = "emscripten")]
    late: HashMap<u16, i32>,
    /// entries of functions (jit::FN): regions never take their hash entry
    fns: HashSet<u32>,
    /// times each function was built
    fn_builds: HashMap<u32, u32>,
    /// indirect call/jump sites of functions: the function, and the target it mostly takes
    site_fn: HashMap<u32, Vec<u32>>,
    hints: HashMap<u32, u32>,
    /// functions the compiler thread is translating, and where it says it is done
    pending: HashSet<u32>,
    /// the copy of `misses`, `known` and `hints` the jobs take with them, and the count of the
    /// changes to them that their sizes do not show (see [`JobMaps`])
    job_maps: JobMaps,
    maps_rev: u64,
    done_tx: std::sync::mpsc::Sender<Batch>,
    done_rx: std::sync::mpsc::Receiver<Batch>,
    /// this state's number (its jobs' [`Job::state`]): a thread's state is made again when code
    /// changes (check_generation), and the old one's results go nowhere
    id: u64,
    /// sites whose hint changed (a site going to several targets keeps its second hint)
    rehinted: HashSet<u32>,
    /// modules loaded one by one (not the saved bundle): (first table index, functions, bytes),
    /// for reclaim
    loads: Vec<(u32, u32, u32)>,
    /// loads since the last reclaim
    since_reclaim: u32,
    /// translations of code on W|X pages checked on entry, live and parked (park.rs)
    park: park::Parking,
    /// the guest code each entry's module (by its first table index) was made from: what goes
    /// when some of it changes (drop_pages)
    code_of: HashMap<u32, Vec<(u32, u32)>>,
    /// the starts of every block a loaded translation has, entries or not (a 64-bit region's
    /// blocks inside loops have no hash entry): what new regions may take in as translated
    translated: HashSet<u32>,
}

/// Loads between two looks for modules nothing refers to any more (reclaim); MASU_JIT_RECLAIM
/// sets it (1: after every load, the tests' stress), 0 never.
static RECLAIM_EVERY: AtomicU32 = AtomicU32::new(64);

/// Frees this thread's modules no entry refers to any more: a region merged into a bigger one,
/// a function built again, a region a function took the place of. Every jump into translated
/// code goes through the hash table or the call cache (koji never bakes a table index into
/// code), so a module none of whose table slots either holds is never entered again; its slots
/// are cleared (vnvm_jit_free) and V8 can collect it. Slots are not reused (the table only
/// grows: a stale index in the ring never names another module); a table grown past its bound
/// is replaced whole (TABLE_FULL). A module running further up
/// this thread's stack is not affected: clearing a slot does not stop code already in it, and
/// what it calls next is looked up again.
fn reclaim(st: &mut State) {
    st.since_reclaim = 0;
    let mask = (1u32 << jit::ENTRY_SHIFT) - 1;
    let mut live = HashSet::new();
    // (a function's entry names its adapter; the function itself is the next slot)
    let mut mark = |v: u32| {
        live.insert(v & mask);
        if v & jit::FN_TAG != 0 {
            live.insert((v & mask) + 1);
        }
    };
    st.hash.iter().filter(|e| e[0] != 0).for_each(|e| mark(e[1]));
    st.calls.iter().filter(|c| c[0] != 0).for_each(|c| mark(c[1]));
    // (and the parked ones: they may come back)
    st.park.bases().for_each(&mut mark);
    let mut freed = (0u64, 0u64);
    let mut gone = Vec::new();
    st.loads.retain(|&(base, n, bytes)| {
        if (base..base + n).any(|i| live.contains(&i)) {
            return true;
        }
        gone.push((base, n));
        freed.0 += 1;
        freed.1 += bytes as u64;
        false
    });
    for (base, n) in gone {
        #[cfg(target_os = "emscripten")]
        unsafe {
            vnvm_jit_free(base, n)
        };
        for i in base..base + n {
            st.roots.remove(&i);
        }
    }
    MODULES[3].fetch_add(freed.0, Ordering::Relaxed);
    MODULES[4].fetch_add(freed.1, Ordering::Relaxed);
}

/// Notes a module loaded at `base` (by itself: reclaim may free it once nothing refers to it).
fn note_load(st: &mut State, base: i32, n: u32, bytes: usize) {
    if base >= 0 {
        st.loads.push((base as u32, n, bytes as u32));
        st.since_reclaim += 1;
    }
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
/// The hops of `n` samples out of a ring of `len`, `hops` hops since the last one.
fn hop_share(hops: u32, n: usize, len: usize) -> u32 {
    (hops as u64 * n as u64 / len.max(1) as u64).min(u32::MAX as u64) as u32
}

/// Hops out of a long-mode region that make merge_hot64 merge it with where they go
/// (MASU_JIT_MERGE_HOPS).
fn merge_hops64() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("MASU_JIT_MERGE_HOPS").ok().and_then(|v| v.parse().ok()).unwrap_or(1 << 18))
}

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
    /// translated with the MXCSR flag checks (jit::Region::exact)
    exact: bool,
    /// made after the ones with smaller numbers (share_seq): the oldest go first
    seq: u64,
}

impl Shared {
    /// What it holds of sake's heap.
    fn size(&self) -> usize {
        self.bytes.len() + self.text.len() + (self.starts.len() + self.code.len()) * 8 + self.sites.len() * 4
    }
}

/// SHARED's bytes at most (module code, guest text and tables): past it the oldest go (each
/// thread keeps the modules it loaded; another thread then translates that code itself).
const SHARED_MAX: usize = 48 << 20;
/// Builds of one key kept (regions and functions of code that keeps changing: Mono, packers).
const SHARED_PER_KEY: usize = 4;

/// SHARED's size in bytes ([`Shared::size`]), kept as entries come and go.
static SHARED_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Adds `r` to SHARED under `key`: an older build of the same code (the same bytes) goes, so
/// do the oldest builds past SHARED_PER_KEY, and the oldest of all past SHARED_MAX.
fn share_push(key: Vec<u32>, r: Arc<Shared>) {
    let mut g = SHARED.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    let list = map.entry(key).or_default();
    let mut freed = 0;
    list.retain(|o| {
        let keep = !(o.exact == r.exact && o.build < r.build && o.text == r.text);
        if !keep {
            freed += o.size();
        }
        keep
    });
    let added = r.size();
    list.push(r);
    while list.len() > SHARED_PER_KEY {
        freed += list.remove(0).size();
    }
    let total = SHARED_BYTES.fetch_add(added, Ordering::Relaxed) + added - freed;
    SHARED_BYTES.fetch_sub(freed, Ordering::Relaxed);
    if total <= SHARED_MAX {
        return;
    }
    // the oldest (down to 3/4: not again at the next build)
    let mut all: Vec<(u64, usize)> = map.values().flatten().map(|s| (s.seq, s.size())).collect();
    all.sort_unstable();
    let mut over = total - SHARED_MAX * 3 / 4;
    let mut cut = 0;
    for &(seq, n) in &all {
        if over == 0 {
            break;
        }
        cut = seq + 1;
        over = over.saturating_sub(n);
    }
    let mut gone = (0, 0);
    map.values_mut().for_each(|l| {
        l.retain(|s| {
            let keep = s.seq >= cut;
            if !keep {
                gone.0 += 1;
                gone.1 += s.size();
            }
            keep
        })
    });
    map.retain(|_, l| !l.is_empty());
    SHARED_BYTES.fetch_sub(gone.1, Ordering::Relaxed);
    if stats() {
        eprintln!("vnjit: {} shared builds dropped ({} KiB; over {} MiB)", gone.0, gone.1 >> 10, SHARED_MAX >> 20);
    }
}

/// The order shared builds are made in ([`Shared::seq`]).
fn share_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Shared regions by their roots.
static SHARED: Mutex<Option<HashMap<Vec<u32>, Vec<Arc<Shared>>>>> = Mutex::new(None);

/// What the translations hold (MASU_JIT_COUNT prints it, jit_modules): modules loaded (each
/// thread's own), their bytes, the highest table index they took (all threads), modules freed
/// (reclaim) and their bytes.
static MODULES: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// [`MODULES`]: loaded, bytes, table, freed, freed bytes, µs in vnvm_jit_load (V8's
/// `new WebAssembly.Module` + `Instance` on the thread itself; the run log's perf lines).
pub(crate) fn jit_modules() -> [u64; 6] {
    MODULES.each_ref().map(|c| c.load(Ordering::Relaxed))
}

/// Compiles and instantiates `bytes` (`n` functions) on this thread: the table index of the
/// first, or -1. Synchronous: the program's threads never return to their event loop, so a
/// promise (WebAssembly.compile) would never resolve on them, and V8 compiles a function only
/// when first called (lazy compilation): this decodes and validates (docs/translator.md).
#[cfg(target_os = "emscripten")]
fn load_module(bytes: &[u8], n: u32) -> i32 {
    let t0 = Instant::now();
    let base = table_answer(unsafe { vnvm_jit_load(bytes.as_ptr(), bytes.len(), n) });
    MODULES[5].fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
    loaded(base, n, bytes.len())
}

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
    m.values().flatten().fold((0, 0), |(n, b), r| (n + 1, b + r.size()))
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

/// The bytes `region` was translated from: as its translator read them (code rewritten since
/// then won't match them), else as they are now.
fn region_text(mem: &mut Linear, region: &jit::Region) -> Option<Vec<u8>> {
    region.text.clone().or_else(|| code_bytes(mem, &region.code))
}

/// A region for `roots` some thread built from the same code as ours.
fn shared(mem: &mut Linear, roots: &[u32]) -> Option<Arc<Shared>> {
    let list = SHARED.lock().unwrap().as_ref()?.get(roots)?.clone();
    let exact = jit::mx_exact();
    list.into_iter().rev().find(|r| r.exact == exact && code_kept(&r.code) && code_bytes(mem, &r.code).is_some_and(|t| t == r.text))
}

fn share(mem: &mut Linear, roots: &[u32], bytes: Vec<u8>, region: &jit::Region) {
    share_build(mem, roots.to_vec(), bytes, region, 0);
}

fn share_build(mem: &mut Linear, key: Vec<u32>, bytes: Vec<u8>, region: &jit::Region, build: u32) {
    let Some(text) = region_text(mem, region) else { return };
    let r = Arc::new(Shared { bytes, starts: region.starts.clone(), code: region.code.clone(), text, build, sites: region.sites.clone(), exact: region.exact, seq: share_seq() });
    share_push(key, r);
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
    *ON.get_or_init(|| cfg!(target_os = "emscripten") && std::env::var("MASU_JIT").map_or(true, |v| v != "0"))
}

/// 16-bit code (Win16) through the JIT too (MASU_JIT16=0: the interpreter only).
pub fn enabled16() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("MASU_JIT16").map_or(true, |v| v != "0"))
}

/// 16-bit code runs translated with `cpu.eip` the linear CS:IP (see koji's `jit::SEG16`):
/// into that form before the dispatcher, back to IP after.
pub fn to_linear16(cpu: &mut Cpu) {
    cpu.eip = cpu.pc16();
}
pub fn from_linear16(cpu: &mut Cpu) {
    cpu.eip = cpu.eip.wrapping_sub(cpu.sbase[1]) & 0xffff;
}

pub fn enabled64() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("MASU_JIT64").map_or(true, |v| v != "0"))
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
    /// This thread's funcref table is full of slots reclaim cleared (host/vnjit.js answered
    /// TABLE_FULL): its translations start over with a new table at the next run loop entry.
    static FRESH_TABLE: Cell<bool> = const { Cell::new(false) };
}

/// vnvm_jit_load's / vnvm_jit_bundle_load's answer when the thread's table cannot take more
/// (host/vnjit.js: freed slots are not handed out again, so the table only grows; past its
/// bound, or when V8 refuses to grow it, the thread starts over with a fresh one).
const TABLE_FULL: i32 = -2;

/// A load's table index as the callers take it (-1: not loaded); TABLE_FULL also asks for the
/// fresh table.
#[cfg_attr(not(target_os = "emscripten"), allow(dead_code))]
fn table_answer(base: i32) -> i32 {
    if base == TABLE_FULL {
        FRESH_TABLE.with(|f| f.set(true));
        return -1;
    }
    base
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
    let (lo, hi) = page_span(addr, len);
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
    SHARED_BYTES.store(0, Ordering::Relaxed);
    changed(0, u32::MAX);
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
        drop_states(lo, hi);
        if stats() {
            eprintln!("vnjit code changed at {addr:#x}+{len:#x}: translations dropped");
        }
    }
}

/// The program read the MXCSR flags for the first time (koji::jit::MX_EXACT): every
/// translation made without the flag checks goes. Whether it did.
fn mx_check() -> bool {
    if !jit::mx_switched() {
        return false;
    }
    if stats() {
        eprintln!("vnjit: the program reads the MXCSR flags (at {:#x}): translations dropped, flags kept from now on", jit::MX_AT.load(Ordering::Relaxed));
    }
    drop_translations();
    true
}

/// The translations made from code on pages `lo..=hi` go, every thread's (each drops its own
/// at its next run: check_generation, drop_pages); shared builds stay, each found again only
/// by its code's bytes.
fn drop_states(lo: usize, hi: usize) {
    for p in lo..=hi.min((1 << 19) - 1) {
        CODE_PAGES[p >> 5].fetch_and(!(1 << (p & 31)), Ordering::Relaxed);
    }
    {
        let (a, e) = ((lo as u64) << 12, ((hi as u64) + 1) << 12);
        CODE_RANGES.lock().unwrap().retain(|&s, &mut end| (end as u64) <= a || (s as u64) >= e);
    }
    changed(lo as u32, hi as u32);
}

/// The code changes since a thread last looked: (generation, first page, last page), the
/// newest last; pages 0..=u32::MAX: everything. Kept for [`CHANGES_KEPT`] changes (a thread
/// that missed one drops all its translations).
static CHANGES: Mutex<std::collections::VecDeque<(u32, u32, u32)>> = Mutex::new(std::collections::VecDeque::new());
const CHANGES_KEPT: usize = 1024;

/// A new generation for a change of pages `lo..=hi`.
fn changed(lo: u32, hi: u32) {
    let mut c = CHANGES.lock().unwrap();
    let g = GENERATION.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    c.push_back((g, lo, hi));
    while c.len() > CHANGES_KEPT {
        c.pop_front();
    }
}

/// The pages changed in generations `seen + 1 ..= now` (None: all of it, or not known).
fn changes_since(seen: u32, now: u32) -> Option<Vec<(u32, u32)>> {
    let c = CHANGES.lock().unwrap();
    // (every generation in between must still be listed)
    if !c.front().is_some_and(|f| f.0.wrapping_sub(seen) as i32 <= 1) {
        return None;
    }
    let mut out = Vec::new();
    for &(g, lo, hi) in c.iter() {
        let d = g.wrapping_sub(seen);
        if d == 0 || d > now.wrapping_sub(seen) {
            continue;
        }
        if lo == 0 && hi == u32::MAX {
            return None;
        }
        out.push((lo, hi));
    }
    Some(out)
}

/// Drops this thread's translations if code changed since it made them: the ones made from
/// the changed pages (all of them when that is not known, or while the compiler thread is
/// translating for this thread).
fn check_generation() {
    mx_check();
    let g = GENERATION.load(Ordering::Relaxed);
    let seen = SEEN.with(|s| s.replace(g));
    let fresh = FRESH_TABLE.with(|f| f.take());
    if seen == g && !fresh {
        return;
    }
    let kept = !fresh && changes_since(seen, g).is_some_and(|pages| STATE.with(|s| s.borrow_mut().as_mut().is_none_or(|st| drop_pages(st, &pages))));
    if !kept {
        STATE.with(|s| *s.borrow_mut() = None);
    }
}

/// Removes the translations made from code on the pages `pages` (first, last) from this
/// thread's state: their hash and call cache entries (the modules go at the next reclaim),
/// and what was known of their addresses (so they are translated again). False when it
/// cannot (a translation in flight may be of the old code): the whole state goes then.
fn drop_pages(st: &mut State, pages: &[(u32, u32)]) -> bool {
    let hit = |a: u32, n: u32| {
        let (p0, p1) = (a >> 12, a.saturating_add(n.max(1) - 1) >> 12);
        pages.iter().any(|&(lo, hi)| p0 <= hi && p1 >= lo)
    };
    st.known.retain(|&a| !hit(a, 1));
    let bad: HashSet<u32> = st.code_of.iter().filter(|(_, c)| c.iter().any(|&(a, n)| hit(a, n))).map(|(&b, _)| b).collect();
    if bad.is_empty() {
        return true;
    }
    if !st.pending.is_empty() {
        return false;
    }
    let mask = (1u32 << jit::ENTRY_SHIFT) - 1;
    let (gone, kept): (Vec<[u32; 2]>, Vec<[u32; 2]>) = st.hash.iter().filter(|e| e[0] != 0).partition(|e| bad.contains(&(e[1] & mask)));
    st.hash.iter_mut().for_each(|e| *e = [0, 0]);
    for e in &kept {
        insert(&mut st.hash, e[0], e[1]);
    }
    st.used = kept.len();
    for c in st.calls.iter_mut().filter(|c| c[0] != 0 && bad.contains(&(c[1] & mask))) {
        *c = [0, 0];
    }
    for e in &gone {
        st.known.remove(&e[0]);
        st.fns.remove(&e[0]);
        st.fn_builds.remove(&e[0]);
    }
    let fns = &st.fns;
    st.site_fn.retain(|_, l| {
        l.retain(|f| fns.contains(f));
        !l.is_empty()
    });
    for b in &bad {
        if let Some(c) = st.code_of.get(b) {
            for &(a, _) in c {
                st.translated.remove(&a);
            }
        }
        st.code_of.remove(b);
        st.roots.remove(b);
    }
    if stats() {
        eprintln!("vnjit: {} translations made from changed code dropped ({} entries), {} kept", bad.len(), gone.len(), st.code_of.len());
    }
    true
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
            static STATES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            State {
                pending: HashSet::new(),
                job_maps: JobMaps::default(),
                maps_rev: 0,
                done_tx,
                done_rx,
                id: STATES.fetch_add(1, Ordering::Relaxed),
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
                #[cfg(target_os = "emscripten")]
                late: HashMap::new(),
                fns: HashSet::new(),
                fn_builds: HashMap::new(),
                site_fn: HashMap::new(),
                hints: HashMap::new(),
                rehinted: HashSet::new(),
                loads: Vec::new(),
                since_reclaim: 0,
                park: park::Parking::default(),
                code_of: HashMap::new(),
                translated: HashSet::new(),
            }
        });
        f(st)
    })
}

/// Compiles and their time (µs) for MASU_JIT_COUNT: regions, functions.
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

/// Switches read once: MASU_JIT_COUNT=<secs> (transfer counters),
/// MASU_JIT_NOCHECK=1 (measuring only: no range checks).
fn init_flags() {
    // (once: this runs on every entry into translated code, and std::env::var takes a lock
    // and walks the environment)
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // (the MXCSR flag checks from the start, not from the program's first read of them)
        if crate::env_on!("MASU_MX_EXACT") {
            jit::MX_EXACT.store(true, Ordering::SeqCst);
        }
        if std::env::var("MASU_JIT_FN_ENTRIES").is_ok_and(|v| v == "0") {
            jit::FN_ENTRIES.store(false, Ordering::Relaxed);
        }
        if let Ok(v) = std::env::var("MASU_JIT_PROBE") {
            for (p, a) in jit::PROBES.iter().zip(v.split('_').filter_map(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok())) {
                p.store(a, Ordering::Relaxed);
            }
        }
        if let Some(n) = std::env::var("MASU_JIT_RECLAIM").ok().and_then(|v| v.parse().ok()) {
            RECLAIM_EVERY.store(n, Ordering::Relaxed);
        }
        if std::env::var("MASU_JIT_HOIST").is_ok_and(|v| v == "0") {
            jit::HOIST.store(false, Ordering::Relaxed);
        }
        if let Some(secs) = std::env::var("MASU_JIT_COUNT").ok().and_then(|v| v.parse::<u64>().ok()) {
            use std::sync::atomic::Ordering::Relaxed;
            if !jit::COUNT.swap(true, Relaxed) {
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(secs.max(1)));
                    let v: Vec<String> = jit::COUNTERS.iter().zip(jit::COUNTER_NAMES).map(|(c, n)| format!("{n}={}", c.swap(0, Relaxed))).collect();
                    let c: Vec<u32> = COMPILES.iter().map(|c| c.swap(0, Relaxed)).collect();
                    let (sn, sb) = shared_size();
                    let m: Vec<u64> = MODULES.iter().map(|c| c.load(Relaxed)).collect();
                    eprintln!(
                        "vnjit count/{secs}s {} | regions {} {} ms, functions {} {} ms (rebuilt {}, {} ms) | shared {sn} ({} KiB), loaded {} modules ({} KiB), table {}, freed {} ({} KiB), loading {} ms",
                        v.join(" "), c[0], c[1] / 1000, c[2], c[3] / 1000, c[4], c[5] / 1000, sb >> 10, m[0], m[1] >> 10, m[2], m[3], m[4] >> 10, m[5] / 1000
                    );
                });
            }
        }
        // measuring: every load/store the JIT emits counts itself, the top ones printed every
        // <secs> (Cpu fields by offset, the rest together; koji::jit::wasm::MEMCOUNT)
        if let Some(secs) = std::env::var("MASU_JIT_MEMCOUNT").ok().and_then(|v| v.parse::<u64>().ok()) {
            use jit::wasm::{MEM_COUNTS, MEM_CPU_SLOTS, MEM_SITES, MEM_SITE_SLOTS};
            use std::sync::atomic::Ordering::Relaxed;
            if !jit::wasm::MEMCOUNT.swap(true, Relaxed) {
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(secs.max(1)));
                    let c: Vec<u64> = MEM_COUNTS.iter().map(|c| c.swap(0, Relaxed) as u64).collect();
                    let mut lines = Vec::new();
                    for (k, name) in MEM_SITES.iter().enumerate() {
                        let c = &c[k * MEM_SITE_SLOTS..(k + 1) * MEM_SITE_SLOTS];
                        let cpu: u64 = c[..2 * MEM_CPU_SLOTS].iter().sum();
                        let (ol, os) = (c[2 * MEM_CPU_SLOTS], c[2 * MEM_CPU_SLOTS + 1]);
                        if cpu + ol + os == 0 {
                            continue;
                        }
                        let mut top: Vec<(usize, u64)> = c[..2 * MEM_CPU_SLOTS].iter().copied().enumerate().filter(|x| x.1 > 0).collect();
                        top.sort_by(|a, b| b.1.cmp(&a.1));
                        let t: Vec<String> = top.iter().take(24).map(|&(i, n)| format!("{}{}={}k", if i >= MEM_CPU_SLOTS { "st+" } else { "ld+" }, i % MEM_CPU_SLOTS, n / 1000)).collect();
                        lines.push(format!("  {name}: cpu {}k other ld {}k st {}k | {}", cpu / 1000, ol / 1000, os / 1000, t.join(" ")));
                    }
                    eprintln!("vnjit mem/{secs}s\n{}", lines.join("\n"));
                });
            }
        }
        // CALL/RET as wasm calls: 1 direct calls, 2 indirect ones too
        // debugging the long-mode translator: MASU_JIT64_INS / _BLOCKS (limits), MASU_JIT64_OFF
        // (mnemonics, as iced names them, left to the interpreter)
        let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        jit::long::DEBUG_INS.store(num("MASU_JIT64_INS"), std::sync::atomic::Ordering::Relaxed);
        jit::long::DEBUG_BLOCKS.store(num("MASU_JIT64_BLOCKS"), std::sync::atomic::Ordering::Relaxed);
        jit::long::DEBUG_CHECK.store(num("MASU_JIT64_CHECK") != 0, std::sync::atomic::Ordering::Relaxed);
        // (MASU_JIT64_STRUCT=1: a region's blocks nested (jit::long::STRUCTURED), for A/B)
        jit::long::STRUCTURED.store(num("MASU_JIT64_STRUCT") != 0, std::sync::atomic::Ordering::Relaxed);
        jit::long::DEBUG_NO_SIDE.store(num("MASU_JIT64_NO_SIDE") != 0, std::sync::atomic::Ordering::Relaxed);
        for name in std::env::var("MASU_JIT64_OFF").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
            if !jit::long::debug_off_named(name) {
                eprintln!("MASU_JIT64_OFF: no mnemonic {name}");
            }
        }
        jit::set_watch(exe_wx_page);
        jit::set_fresh(wx_page);
        jit::set_deny(interpreted_page);
        // function mode by default (MASU_JIT_FN=0: regions)
        // (a constant stored: the wasm build stored false for this flag when the value was
        // the expression's, and FN mode was off by default)
        if !std::env::var("MASU_JIT_FN").is_ok_and(|v| v == "0") {
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
        let every = RECLAIM_EVERY.load(Ordering::Relaxed);
        if every != 0 && st.since_reclaim >= every {
            reclaim(st);
        }
        // (16-bit regions stay one per root: each translated with its own code segment's base)
        if st.ring[0] >= jit::RING_SIZE {
            if cpu.mode16 != 0 {
                st.ring[0] = 0;
            } else {
                merge_hot(st, cpu);
            }
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
        // MASU_JIT64_VERIFY=1: single-instruction regions, =2: whole regions; one region per call
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
    if cpu.jit_stale != 0 {
        stale_hit(std::mem::take(&mut cpu.jit_stale));
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
        let on = std::env::var("MASU_JIT64_VERIFY").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
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
fn load(st: &mut State, bytes: &[u8], roots: Vec<u32>, starts: &[(u32, u32)], code: &[(u32, u32)]) -> bool {
    if let Some(dir) = jit_dump() {
        let name = format!("{:08x}-{}.wasm", roots[0], roots.len());
        let _ = std::fs::write(std::path::Path::new(&dir).join(name), bytes);
    }
    #[cfg(target_os = "emscripten")]
    let base = load_module(bytes, 1);
    #[cfg(not(target_os = "emscripten"))]
    let base = { let _ = bytes; -1 };
    if base < 0 {
        return false;
    }
    note_load(st, base, 1, bytes.len());
    let base = base as u32;
    let entries: Vec<(u32, u32)> = starts.iter().filter(|s| !st.fns.contains(&s.0)).map(|&(a, i)| (a, base | i << jit::ENTRY_SHIFT)).collect();
    enter(st, base, roots.clone(), starts, code);
    track_wx(st, base, entries, roots, None, code);
    true
}

/// Makes the hash table big enough for `n` more entries (at most half full).
fn grow(st: &mut State, n: usize) {
    while (st.used + n) * 2 > st.hash.len() {
        let mut bigger = vec![[0u32; 2]; st.hash.len() * 2];
        for e in st.hash.iter().filter(|e| e[0] != 0) {
            insert(&mut bigger, e[0], e[1]);
        }
        st.hash = bigger;
    }
}

/// Points the hash table at the blocks of the region at table index `base`.
fn enter(st: &mut State, base: u32, roots: Vec<u32>, starts: &[(u32, u32)], code: &[(u32, u32)]) {
    st.translated.extend(code.iter().map(|c| c.0));
    st.code_of.insert(base, code.to_vec());
    for &(a, _) in starts {
        st.known.insert(a);
    }
    grow(st, starts.len());
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

/// The saved regions (empty without any): the bundle the page started the module with (part
/// 0), then the parts it hands over once the program runs ([`vnhost_jit_late`]: the page waits
/// only for the first part, the regions runs saved first, #123).
#[derive(Default)]
struct Saved {
    entries: Vec<jit::saved::Entry>,
    /// each entry's part, and its table index past the part's first function
    part: Vec<u16>,
    offsets: Vec<u32>,
    /// regions by their first root
    by_root: HashMap<u32, Vec<usize>>,
    /// functions by their entry (all builds)
    by_fn: HashMap<u32, Vec<usize>>,
    /// late parts in: (module bytes, its function count), part k at k - 1
    late: Vec<(&'static [u8], u32)>,
}

impl Saved {
    /// Adds part `part`'s entries (its module's functions in their order).
    fn add(&mut self, part: u16, entries: Vec<jit::saved::Entry>) {
        let offsets = jit::saved::offsets(&entries);
        for (e, o) in entries.into_iter().zip(offsets) {
            let i = self.entries.len();
            if e.build > 0 { &mut self.by_fn } else { &mut self.by_root }.entry(e.roots[0]).or_default().push(i);
            self.entries.push(e);
            self.part.push(part);
            self.offsets.push(o);
        }
    }
}

/// Parts the page handed over after the start: (module address, length, index address,
/// length). The page writes a slot, then the count (one writer: the page's main thread, which
/// must not block); threads take them in at their next look for a saved region (saved_regions).
const LATE_MAX: usize = 64;
struct Inbox {
    slots: [[AtomicUsize; 4]; LATE_MAX],
    count: AtomicUsize,
}

impl Inbox {
    const fn new() -> Self {
        Inbox { slots: [const { [const { AtomicUsize::new(0) }; 4] }; LATE_MAX], count: AtomicUsize::new(0) }
    }
    /// (one writer)
    fn push(&self, v: [usize; 4]) -> bool {
        let k = self.count.load(Ordering::Relaxed);
        if k >= LATE_MAX {
            return false;
        }
        for (a, v) in self.slots[k].iter().zip(v) {
            a.store(v, Ordering::Relaxed);
        }
        self.count.store(k + 1, Ordering::Release);
        true
    }
    fn len(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
    /// slot `k` < [`len`] (that call's Acquire saw its stores)
    fn get(&self, k: usize) -> [usize; 4] {
        self.slots[k].each_ref().map(|a| a.load(Ordering::Relaxed))
    }
}

static LATE: Inbox = Inbox::new();

/// The page: a saved part that came after the module started, its module and index in memory
/// from malloc, kura's from now on (never freed). 0 taken, -1 refused (no room).
#[no_mangle]
pub extern "C" fn vnhost_jit_late(wasm: *const u8, wasm_len: u32, idx: *const u8, idx_len: u32) -> i32 {
    if wasm.is_null() || idx.is_null() || !LATE.push([wasm as usize, wasm_len as usize, idx as usize, idx_len as usize]) {
        return -1;
    }
    0
}

static SAVED: std::sync::LazyLock<std::sync::RwLock<Saved>> = std::sync::LazyLock::new(|| {
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
    let mut s = Saved::default();
    s.add(0, entries);
    std::sync::RwLock::new(s)
});

/// The saved regions, with the parts the page has handed over since the last look.
fn saved_regions() -> std::sync::RwLockReadGuard<'static, Saved> {
    let n = LATE.len();
    {
        let s = SAVED.read().unwrap();
        if s.late.len() >= n {
            return s;
        }
    }
    let mut s = SAVED.write().unwrap();
    take_late(&mut s, &LATE, n);
    drop(s);
    SAVED.read().unwrap()
}

/// Puts `inbox`'s parts below `n` into `s` (those not in yet), as parts 1 up. A part whose
/// index does not decode counts with no regions (its module is then never loaded).
fn take_late(s: &mut Saved, inbox: &Inbox, n: usize) {
    while s.late.len() < n {
        let k = s.late.len();
        let [w, wl, i, il] = inbox.get(k);
        // SAFETY: the page gave both to kura for good (vnhost_jit_late), written before the count
        let (wasm, idx): (&'static [u8], &[u8]) = unsafe { (std::slice::from_raw_parts(w as *const u8, wl), std::slice::from_raw_parts(i as *const u8, il)) };
        let entries = jit::saved::decode_all(idx).unwrap_or_default();
        let funcs = entries.iter().map(|e| e.width()).sum();
        s.late.push((wasm, funcs));
        s.add(k as u16 + 1, entries);
    }
}

/// A saved region for `roots` (any built from `roots[0]` when `exact` is off: the one with most
/// roots) whose guest code is still there, and clear of the trap range `trap`.
fn find_saved(mem: &mut Linear, roots: &[u32], exact: bool, trap: (u32, u32)) -> Option<usize> {
    if profiling() || jit::page_check() || jit::mx_exact() {
        return None; // saved regions have no counters, nor page checks, nor MXCSR flag checks
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

/// The table index on this thread of the first function of part `part` (instantiated on this
/// thread first: the bundle the page compiled, or a late part's module compiled here), or -1.
#[cfg(target_os = "emscripten")]
fn part_base(st: &mut State, s: &Saved, part: u16) -> i32 {
    if part == 0 {
        return *st.saved.get_or_insert_with(|| table_answer(unsafe { vnvm_jit_bundle_load() }));
    }
    *st.late.entry(part).or_insert_with(|| {
        let (wasm, n) = s.late[part as usize - 1];
        // (not in st.loads: kept for good, as the bundle)
        let t0 = Instant::now();
        let base = table_answer(unsafe { vnvm_jit_load(wasm.as_ptr(), wasm.len(), n) });
        MODULES[5].fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
        // (the first thread to load each part says so: the run log shows the late parts used)
        static SAID: Mutex<Vec<u16>> = Mutex::new(Vec::new());
        let mut said = SAID.lock().unwrap();
        if base < 0 || !said.contains(&part) {
            said.push(part);
            eprintln!("vnjit: saved part {part} loaded: {n} functions, {} KiB, {} ms{}", wasm.len() >> 10, t0.elapsed().as_millis(), if base < 0 { " (failed)" } else { "" });
        }
        base
    })
}

/// Puts saved region `i` in (instantiating its part on this thread first).
fn load_saved(st: &mut State, i: usize) -> bool {
    let s = saved_regions();
    #[cfg(target_os = "emscripten")]
    let base = part_base(st, &s, s.part[i]);
    #[cfg(not(target_os = "emscripten"))]
    let base = -1;
    if base < 0 {
        return false;
    }
    let e = &s.entries[i];
    mark_code(&e.code);
    enter(st, base as u32 + s.offsets[i], e.roots.clone(), &e.starts, &e.code);
    true
}

/// A saved function at `eip` (its newest build) whose guest code is still there.
fn find_saved_fn(mem: &mut Linear, eip: u32) -> Option<usize> {
    if profiling() || jit::page_check() || jit::mx_exact() {
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
    let s = saved_regions();
    #[cfg(target_os = "emscripten")]
    let base = part_base(st, &s, s.part[i]);
    #[cfg(not(target_os = "emscripten"))]
    let base = -1;
    if base < 0 {
        return false;
    }
    let e = &s.entries[i];
    st.fn_builds.insert(e.roots[0], e.build);
    enter_fn(st, base as u32 + s.offsets[i], e.roots[0], &e.starts, &e.sites, &e.code);
    true
}

/// Sends the regions built since the last save to MASU_JIT_URL (as one module and its index)
/// once the run is past its start, at most every SAVE_EVERY and only when there are enough to
/// be worth it: each save changes the module, and Chromium then forgets its compiled code.
/// What the page said of a save (`vnhost_jit_saved`; host/vnjit.js): taken (2xx), refused for
/// good (a 4xx but 408/429, 507: sent again it would be again), or failed (no answer, 5xx,
/// 408/429, or none in SAVE_TIMEOUT): sent again later.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ack {
    Failed = 0,
    Taken = 1,
    Refused = 2,
}

const SAVE_EVERY: Duration = Duration::from_secs(30);
/// A save with no answer by then counts as failed (a slow phone's upload of a big one included).
const SAVE_TIMEOUT: Duration = Duration::from_secs(300);

/// What has been sent to be saved and what came of it (maybe_save): a region counts as saved
/// only once the server took it; one whose save failed goes again, the wait doubling (30 s up
/// to 8 min) while saves keep failing.
#[derive(Default)]
struct SaveBook {
    /// a region's newest build the server took (or refused for good)
    done: HashMap<Vec<u32>, u32>,
    /// sent, no answer yet: id -> (when, the regions' builds, the chunks list's length when it is one)
    inflight: HashMap<u32, (Instant, Vec<(Vec<u32>, u32)>, Option<usize>)>,
    next_id: u32,
    /// failures in a row
    fails: u32,
    /// the length of the longest chunks list the server took (none yet: None)
    chunks_taken: Option<usize>,
}

/// The chunks list goes again once the run has read this many more chunks than the server has
/// (a game's whole play, not only its first seconds: 金恋 waited ~1.2 s for each file it read
/// first long after its first save, and the next run's page fetches ahead only what is listed).
const CHUNKS_STEP: usize = 32;

impl SaveBook {
    /// Whether `build` of region `r` is to be sent: newer than the one taken, and not on its way.
    fn wanted(&self, r: &[u32], build: u32) -> bool {
        self.done.get(r).is_none_or(|&b| build > b) && !self.inflight.values().any(|(_, rs, _)| rs.iter().any(|(x, b)| x == r && *b >= build))
    }

    fn send(&mut self, now: Instant, regions: Vec<(Vec<u32>, u32)>, chunks: Option<usize>) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        self.inflight.insert(self.next_id, (now, regions, chunks));
        self.next_id
    }

    /// Whether the chunks list (`len` chunks now) is to be sent: none on its way, and never
    /// taken yet or CHUNKS_STEP longer than the one taken.
    fn chunks_due(&self, len: usize) -> bool {
        !self.inflight.values().any(|(.., c)| c.is_some()) && self.chunks_taken.is_none_or(|t| len >= t + CHUNKS_STEP)
    }

    fn answer(&mut self, id: u32, a: Ack) {
        let Some((_, regions, chunks)) = self.inflight.remove(&id) else { return };
        match a {
            Ack::Taken | Ack::Refused => {
                for (r, b) in regions {
                    let d = self.done.entry(r).or_insert(b);
                    *d = (*d).max(b);
                }
                // (refused: not again until it is longer either)
                if let Some(n) = chunks {
                    self.chunks_taken = Some(self.chunks_taken.unwrap_or(0).max(n));
                }
                if a == Ack::Taken {
                    self.fails = 0;
                }
            }
            Ack::Failed => {
                self.fails += 1;
            }
        }
    }

    /// Saves past SAVE_TIMEOUT with no answer: failed.
    fn expire(&mut self, now: Instant) {
        let old: Vec<u32> = self.inflight.iter().filter(|(_, (t, ..))| now - *t >= SAVE_TIMEOUT).map(|(&id, _)| id).collect();
        for id in old {
            self.answer(id, Ack::Failed);
        }
    }

    /// The wait before the next save.
    fn wait(&self) -> Duration {
        SAVE_EVERY * (1 << self.fails.min(4))
    }
}

/// The page's answers to saves, slot `id % 32`: `1 << 63 | id << 8 | ack` (written on the page's
/// main thread, read by maybe_save on the program's: atomics only, nothing that waits).
static SAVE_ACKS: [std::sync::atomic::AtomicU64; 32] = [const { std::sync::atomic::AtomicU64::new(0) }; 32];

/// The id of a send no save waits for (save_chunks' trace).
const NO_ACK: u32 = u32::MAX;

fn ack(id: u32, a: Ack) {
    SAVE_ACKS[id as usize % 32].store(1 << 63 | (id as u64) << 8 | a as u64, Ordering::Release);
}

/// host/vnjit.js, when the server answered save `id` (or the request failed): `ack` an [`Ack`].
#[no_mangle]
pub extern "C" fn vnhost_jit_saved(id: u32, a: u32) {
    // (a trace's: no save waits for it, and its slot is another save's)
    if id == NO_ACK {
        return;
    }
    ack(id, match a {
        1 => Ack::Taken,
        2 => Ack::Refused,
        _ => Ack::Failed,
    });
}

/// The answers that came for the saves on their way.
fn take_acks(book: &mut SaveBook) {
    let ids: Vec<u32> = book.inflight.keys().copied().collect();
    for id in ids {
        let v = SAVE_ACKS[id as usize % 32].load(Ordering::Acquire);
        if v >> 63 == 1 && (v >> 8) as u32 == id {
            book.answer(id, match v & 0xff {
                1 => Ack::Taken,
                2 => Ack::Refused,
                _ => Ack::Failed,
            });
        }
    }
}

/// Whether `every` has passed since `last` (and if so, `last` is now).
fn due(last: &mut Option<Instant>, now: Instant, every: Duration) -> bool {
    if last.is_some_and(|t| now.saturating_duration_since(t) < every) {
        return false;
    }
    *last = Some(now);
    true
}

#[cfg(test)]
mod due_tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        /// Over any increasing times: true at the first, then exactly at the first time at
        /// least `every` after the last true.
        #[test]
        fn once_per_interval(steps in proptest::collection::vec(0u64..3000, 1..100), every in 1u64..2000) {
            let t0 = Instant::now();
            let (mut last, mut at, mut prev): (Option<Instant>, u64, Option<u64>) = (None, 0, None);
            for d in steps {
                at += d;
                let got = due(&mut last, t0 + Duration::from_millis(at), Duration::from_millis(every));
                let want = prev.is_none_or(|p| at - p >= every);
                prop_assert_eq!(got, want);
                if want {
                    prev = Some(at);
                }
            }
        }
    }
}

/// MASU_JIT_DUMP, read once (it was read from the environment at every module load).
fn jit_dump() -> Option<&'static std::ffi::OsString> {
    static DIR: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| std::env::var_os("MASU_JIT_DUMP")).as_ref()
}

fn maybe_save() {
    const SAVE_AFTER: Duration = Duration::from_secs(20);
    const SAVE_MIN: usize = 32;
    static URL: std::sync::OnceLock<Option<std::ffi::CString>> = std::sync::OnceLock::new();
    static SAVE: Mutex<(Option<Instant>, Option<Instant>, Option<SaveBook>)> = Mutex::new((None, None, None));
    // (looked at once a second at most: called after every compile, each look walked all of
    // SHARED and copied the chunk list, on the game's thread, for the whole long tail of a run)
    static LOOKED: Mutex<Option<Instant>> = Mutex::new(None);
    let Some(url) = URL.get_or_init(|| std::env::var("MASU_JIT_URL").ok().and_then(|u| std::ffi::CString::new(u).ok())) else {
        return;
    };
    if profiling() || jit::mx_exact() {
        return; // the counters' addresses are only this run's (and saved regions are loaded
                // only without the MXCSR flag checks, find_saved)
    }
    let now = Instant::now();
    if !due(&mut LOOKED.lock().unwrap_or_else(|e| e.into_inner()), now, Duration::from_secs(1)) {
        return;
    }
    let Ok(mut guard) = SAVE.try_lock() else { return };
    let g = &mut *guard;
    let start = *g.0.get_or_insert(now);
    let book = g.2.get_or_insert_with(SaveBook::default);
    take_acks(book);
    book.expire(now);
    if now - start < SAVE_AFTER || g.1.is_some_and(|t| now - t < book.wait()) {
        return;
    }
    // (a function's newest build; a newer one than saved goes again)
    let new: Vec<(Vec<u32>, Arc<Shared>)> = {
        let sh = SHARED.lock().unwrap();
        let Some(sh) = sh.as_ref() else { return };
        sh.iter()
            .filter_map(|(r, l)| {
                let s = l.iter().max_by_key(|s| s.build)?;
                book.wanted(r, s.build).then(|| (r.clone(), s.clone()))
            })
            .collect()
    };
    // the store chunks the run read so far, in order (the next run's page fetches them ahead):
    // the first time, then again whenever CHUNKS_STEP more were read (whether or not regions go)
    let list = crate::vfs::fetched();
    let chunks = book.chunks_due(list.len() / 16).then(|| (book.send(now, Vec::new(), Some(list.len() / 16)), list));
    // (fewer when they are what a failed save left: the run may have little more to add)
    if new.len() < SAVE_MIN && (book.fails == 0 || new.is_empty()) {
        drop(guard);
        if let Some((cid, c)) = chunks {
            save_chunks(url, cid, &c);
        }
        return;
    }
    g.1 = Some(now);
    let id = book.send(now, new.iter().map(|(r, s)| (r.clone(), s.build)).collect(), None);
    drop(guard);
    // merged and sent on a thread of its own: the merge of a few hundred functions took the
    // game's thread tens of ms
    // (one per save-state epoch: a restored copy has none of the old thread)
    static SAVER: Mutex<(u32, Option<Option<std::sync::mpsc::Sender<Save>>>)> = Mutex::new((0, None));
    let tx = {
        let mut q = SAVER.lock().unwrap_or_else(|e| e.into_inner());
        if q.0 != crate::snap::epoch() {
            *q = (crate::snap::epoch(), None);
        }
        q.1.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Save>();
            std::thread::Builder::new()
                .name("vnjit-save".into())
                .spawn(move || {
                    rx.into_iter().for_each(|j| {
                        save(j);
                        crate::snap::drain_done();
                    })
                })
                .ok()?;
            Some(tx)
        })
        .clone()
    };
    let job = (url.as_c_str(), id, new, chunks);
    match tx {
        Some(tx) => {
            crate::snap::drain_add();
            if tx.send(job).is_err() {
                crate::snap::drain_done();
            }
        }
        None => save(job),
    }
}

/// What [`save`] sends: the URL, the save's id and new regions, the chunks read (the first
/// time, with their id).
type Save = (&'static std::ffi::CStr, u32, Vec<(Vec<u32>, Arc<Shared>)>, Option<(u32, Vec<u8>)>);

fn save((url, id, new, chunks): Save) {
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
        // (they would not the next time either)
        ack(id, Ack::Refused);
        if let Some((c, _)) = chunks {
            ack(c, Ack::Failed);
        }
        return;
    };
    let idx = jit::saved::encode(&entries);
    if stats() || crate::env_on!("MASU_JIT_SAVE_LOG") {
        eprintln!("vnjit: saving {} regions ({} bytes)", entries.len(), wasm.len());
    }
    let mut body = (wasm.len() as u32).to_le_bytes().to_vec();
    body.extend_from_slice(&wasm);
    body.extend_from_slice(&idx);
    #[cfg(target_os = "emscripten")]
    unsafe {
        vnvm_jit_save(url.as_ptr(), body.as_ptr(), body.len(), id);
    };
    if let Some((cid, c)) = &chunks {
        save_chunks(url, *cid, c);
    }
    let _ = (url, body);
}

/// Where the chunks list goes: MASU_CHUNKS_URL (the page's, per game: games that start the same
/// program, all SiglusEngine.exe at Z:\ippai\game, read different chunks), else beside the
/// regions (`<url>.chunks`).
fn chunks_url(url: &std::ffi::CStr) -> std::ffi::CString {
    let u = std::env::var("MASU_CHUNKS_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| format!("{}.chunks", url.to_str().unwrap_or("")));
    std::ffi::CString::new(u).unwrap_or_default()
}

/// Sends the chunks list `c` (save `id`; the page answers it as any save), and with it the run's
/// read trace so far to MASU_TRACE_URL (the page's, one per run: vfs/trace.rs), answered to no
/// save (NO_ACK: a lost one goes again with the next list, longer).
fn save_chunks(url: &std::ffi::CStr, id: u32, c: &[u8]) {
    let u = chunks_url(url);
    #[cfg(target_os = "emscripten")]
    unsafe {
        vnvm_jit_save(u.as_ptr(), c.as_ptr(), c.len(), id);
    }
    let _ = (u, id, c);
    if let Some(t) = std::env::var("MASU_TRACE_URL").ok().filter(|u| !u.is_empty()).and_then(|u| std::ffi::CString::new(u).ok()) {
        let text = crate::vfs::trace::text();
        #[cfg(target_os = "emscripten")]
        unsafe {
            vnvm_jit_save(t.as_ptr(), text.as_ptr(), text.len(), NO_ACK);
        }
        let _ = (t, text);
    }
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
    let hops = std::mem::take(&mut st.ring[0]);
    // pairs by frequency (runs of equal ones once sorted)
    jumps.sort_unstable();
    if cpu.mode64 != 0 {
        return merge_hot64(st, cpu, &jumps, hops);
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
                st.maps_rev += 1;
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
        load(st, &r.bytes, roots, &r.starts, &r.code);
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
    if let Some(n) = std::env::var("MASU_JIT_SLOTS").ok().and_then(|v| v.parse::<u32>().ok()) {
        jit::SLOTS_MAX.store(n.min(6), std::sync::atomic::Ordering::Relaxed);
    }
    if crate::env_on!("MASU_JIT_LAYOUT") {
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
    if load(st, &bytes, roots.clone(), &region.starts, &region.code) {
        keep_profile(&mut region);
        share(&mut Linear, &roots, bytes, &region);
        maybe_save();
    }
}

/// merge_hot for long mode (jit::long): the region that left for others most (out of the
/// ring's sorted samples `jumps`) is built again with the targets it went to more than once
/// (a switch's cases, a callee, a virtual call's methods), once it left merge_hops64 times.
fn merge_hot64(st: &mut State, cpu: &Cpu, jumps: &[u64], hops: u32) {
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
    // (the hops out of `from` since the last ring, estimated from its share of the ring: a
    // merge compiles the whole region again, bigger, and startup code has regions that are
    // the top source of hops for a few thousand hops only. Counting rings instead, a Godot
    // game merged 1300 regions on its way to the title, which took 70 s instead of 40 s)
    let w = st.hot.entry((from, u32::MAX)).or_insert(0);
    *w = w.saturating_add(hop_share(hops, n, jumps.len()));
    if *w < merge_hops64() {
        return;
    }
    st.hot.remove(&(from, u32::MAX));
    let Some(roots) = st.roots.get(&from) else {
        if stats() {
            eprintln!("vnjit64 merge from {from:#x}: no region there");
        }
        return;
    };
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
        if stats() {
            eprintln!("vnjit64 merge from {from:#x} (roots {roots:x?}): {} new targets", new.len());
        }
        return;
    }
    let to = new[0];
    roots.extend(new);
    if let Some(r) = shared(&mut Linear, &roots) {
        st.merges += 1;
        mark_code(&r.code);
        load(st, &r.bytes, roots, &r.starts, &r.code);
        return;
    }
    merge_long(st, cpu, roots, from, to);
}

/// merge_hot's region for long mode (jit::long): the roots' blocks and the ones that ran.
fn merge_long(st: &mut State, cpu: &Cpu, roots: Vec<u32>, from: u32, to: u32) {
    let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
    let full: Vec<u64> = roots.iter().map(|&r| r as u64 | (cpu.rip_hi as u64) << 32).collect();
    let mut m = Module { name: Some(format!("x64_{:x}{:08x}_{}", cpu.rip_hi, roots[0], roots.len())), ..Module::default() };
    let (misses, known, hash, translated) = (&st.misses, &st.known, &st.hash, &st.translated);
    let Some(region) = jit::long::translate_region64(&mut m, &mut Linear, &full, (lo, hi), |a, sure| {
        debug_allowed(a) && !(a >= lo && a < hi) && (misses.contains_key(&a) || compiled(hash, a) || translated.contains(&a) || sure && !known.contains(&a))
    }) else {
        return;
    };
    if !region.starts.iter().any(|s| s.0 == to) {
        if stats() {
            eprintln!("vnjit64 merge {from:#x} -> {to:#x} refused: not an entry of the region ({} entries)", region.starts.len());
        }
        return; // too big already
    }
    st.merges += 1;
    if stats() || crate::env_on!("MASU_JIT_LAYOUT") {
        eprintln!("vnjit64 merge {from:#x} -> {to:#x}: {} blocks, roots {:x?}\n{}", region.starts.len(), roots, region.layout);
    }
    let bytes = m.finish();
    mark_code(&region.code);
    if load(st, &bytes, roots.clone(), &region.starts, &region.code) {
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
        // (code rewritten back to a version this thread translated before)
        if reinstate(st, eip) {
            return true;
        }
        // another thread's region from the same code goes in at once
        // (16-bit regions are neither shared nor saved: their code is decoded with a segment base)
        if st.misses.get(&eip).is_none() && cpu.mode16 == 0 {
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
                return load(st, &r.bytes, vec![eip], &r.starts, &r.code);
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
        st.maps_rev += 1;
        if cpu.mode64 != 0 {
            return compile_long(st, cpu, mem);
        }
        if cpu.mode16 != 0 {
            return compile16(st, cpu, mem);
        }
        if fnm {
            // (part: not a function's entry, as where a function's blocks that had not run yet
            // go on: without the blocks compiled already, which it enters through the dispatcher)
            return compile_fn(st, eip, (cpu.trap_lo, cpu.trap_hi), mem, part);
        }
        // the block and what it statically reaches, as one function
        let mut m = Module { name: Some(format!("x86_{eip:08x}")), ..Module::default() };
        if crate::env_on!("MASU_JIT_LAYOUT") {
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
        if crate::env_on!("MASU_JIT_LAYOUT") {
            eprintln!("vnjit fresh {eip:#x}\n{}", region.layout);
        }
        let bytes = m.finish();
        let t1 = std::time::Instant::now();
        mark_code(&region.code);
        let ok = load(st, &bytes, vec![eip], &region.starts, &region.code);
        count_compile(0, t0);
        if crate::env_on!("MASU_JIT_TIME") {
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

/// Compiles the 16-bit region at cpu.eip (linear; koji's jit::SEG16): one root, not shared or saved.
fn compile16(st: &mut State, cpu: &Cpu, mem: &mut Linear) -> bool {
    let eip = cpu.eip;
    let mut m = Module { name: Some(format!("x16_{eip:08x}")), ..Module::default() };
    let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
    let (misses, known) = (&st.misses, &st.known);
    if crate::env_on!("MASU_JIT_LAYOUT") {
        jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let t0 = std::time::Instant::now();
    let Some(region) = jit::translate_region16(&mut m, mem, eip, cpu.sbase[1], MAX_BLOCKS, |a| {
        debug_allowed(a) && !(a >= lo && a < hi) && !known.contains(&a) && (a == eip || misses.contains_key(&a))
    }) else {
        st.known.insert(eip);
        return false;
    };
    if crate::env_on!("MASU_JIT_LAYOUT") {
        eprintln!("vnjit16 region {eip:#x}\n{}", region.layout);
    }
    let bytes = m.finish();
    let ok = load(st, &bytes, vec![eip], &region.starts, &region.code);
    count_compile(0, t0);
    if ok {
        mark_code(&region.code);
    } else {
        st.known.insert(eip);
    }
    ok
}

/// Compiles the long-mode region at cpu.eip (jit::long; no FN mode or merges yet).
fn compile_long(st: &mut State, cpu: &Cpu, mem: &mut Linear) -> bool {
    let eip = cpu.eip;
    let mut m = Module { name: Some(format!("x64_{:x}{eip:08x}", cpu.rip_hi)), ..Module::default() };
    let (lo, hi) = (cpu.trap_lo, cpu.trap_hi);
    let (misses, known, hash) = (&st.misses, &st.known, &st.hash);
    let entry = eip as u64 | (cpu.rip_hi as u64) << 32;
    if crate::env_on!("MASU_JIT_LAYOUT") {
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
    if crate::env_on!("MASU_JIT_LAYOUT") {
        eprintln!("vnjit64 region {entry:#x}\n{}", region.layout);
    }
    let bytes = m.finish();
    let t1 = std::time::Instant::now();
    let ok = load(st, &bytes, vec![eip], &region.starts, &region.code);
    if crate::env_on!("MASU_JIT_TIME") {
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
        let maps = st.job_maps.get(&st.misses, &st.known, &st.hints, st.maps_rev);
        let job = Job { eip, lo, hi, part, build, maps, state: st.id, done: st.done_tx.clone() };
        if crate::env_on!("MASU_JIT_TIME") {
            eprintln!("vnjit job {eip:#x}: misses {} known {} hints {}, copied in {:?}", st.misses.len(), st.known.len(), st.hints.len(), tc.elapsed());
        }
        crate::snap::drain_add();
        if jobs.send(job).is_ok() {
            st.pending.insert(eip);
            return false;
        }
        crate::snap::drain_done();
    }
    let t0 = Instant::now();
    let built = build_fn(mem, eip, (lo, hi), part, build, &st.misses, &st.known, &st.hints);
    finish_fn(st, eip, build, built, None, t0)
}

/// A function translated (on the compiler thread or not): (module, region, its code bytes).
type Built = Option<(Vec<u8>, jit::Region, Vec<u8>)>;

/// A function for the compiler thread to translate, with this thread's view of what ran.
/// What a job reads of its thread's state: the addresses in `misses` and `known`, and `hints`.
type Maps = (HashMap<u32, u32>, HashSet<u32>, HashMap<u32, u32>);

/// The copy of a thread's `misses`, `known` and `hints` its jobs share. A scene change asks for
/// hundreds of functions, and each job took its own copy of all three (tens of thousands of
/// entries each, on the guest thread, all alive in the queue at once). The translation reads
/// only which addresses are in them and what the hints say, so one copy serves until an
/// address is added (the sizes say so) or one is taken out or a hint replaced (`rev` counts
/// those); a miss counted again changes nothing a job reads.
#[derive(Default)]
struct JobMaps {
    made: Option<(std::sync::Arc<Maps>, [usize; 3], u64)>,
}

impl JobMaps {
    fn get(&mut self, misses: &HashMap<u32, u32>, known: &HashSet<u32>, hints: &HashMap<u32, u32>, rev: u64) -> std::sync::Arc<Maps> {
        let at = [misses.len(), known.len(), hints.len()];
        match &self.made {
            Some((m, a, r)) if *a == at && *r == rev => m.clone(),
            _ => {
                let m = std::sync::Arc::new((misses.clone(), known.clone(), hints.clone()));
                self.made = Some((m.clone(), at, rev));
                m
            }
        }
    }
}

#[cfg(test)]
mod job_maps_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Any changes made as jit.rs makes them (an address added; one taken out or a hint
        /// replaced with `rev` counted; a miss counted again): the copy a job gets names the
        /// addresses and hints of that moment, and it is the same copy while nothing a job
        /// reads changed.
        #[test]
        fn a_job_reads_the_state_of_its_moment(ops in proptest::collection::vec((0u8..8, 0u32..12, 0u32..4), 1..200)) {
            let (mut misses, mut known, mut hints) = (HashMap::new(), HashSet::new(), HashMap::new());
            let (mut rev, mut cache) = (0u64, JobMaps::default());
            let mut last: Option<std::sync::Arc<Maps>> = None;
            let mut changed = true;
            for (op, a, v) in ops {
                match op {
                    0 => {
                        changed |= !misses.contains_key(&a);
                        *misses.entry(a).or_insert(0) += 1;
                    }
                    1 => changed |= known.insert(a),
                    2 => {
                        // (counted whether or not it changed anything: copied again then, which is allowed)
                        changed = true;
                        rev += 1;
                        hints.insert(a, v);
                    }
                    3 => {
                        changed = true;
                        misses.remove(&a);
                        rev += 1;
                    }
                    4 => {
                        changed = true;
                        known.remove(&a);
                        rev += 1;
                    }
                    _ => {
                        let m = cache.get(&misses, &known, &hints, rev);
                        let keys = |h: &HashMap<u32, u32>| h.keys().copied().collect::<HashSet<u32>>();
                        prop_assert_eq!(keys(&m.0), keys(&misses));
                        prop_assert_eq!(&m.1, &known);
                        prop_assert_eq!(&m.2, &hints);
                        if let (Some(l), false) = (&last, changed) {
                            prop_assert!(std::sync::Arc::ptr_eq(l, &m), "copied again though nothing changed");
                        }
                        last = Some(m);
                        changed = false;
                    }
                }
            }
        }
    }
}

struct Job {
    eip: u32,
    lo: u32,
    hi: u32,
    part: bool,
    build: u32,
    maps: std::sync::Arc<Maps>,
    /// the [`State`] asking (its jobs that wait together come back as one batch, to its `done`).
    /// Not the thread: a thread whose state was made again (check_generation) has jobs of both
    /// in the queue, and a batch of both went to the old state's channel, which nobody reads:
    /// the new state's job stayed pending for good, and while one is pending no part of a
    /// function is built (maybe_compile), so the game ran in the interpreter, 100 to 250 times
    /// slower (竜翼のメロディア 体験版 stood still twice for 40 s, #214).
    state: u64,
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
    /// built with the MXCSR flag checks (koji::jit::MX_EXACT)
    exact: bool,
}

/// Functions one batch takes, at most (one load: a longer pause of its own).
const BATCH_MAX: usize = 16;

/// `first` and the jobs in `waiting` with its key, in order, up to `max` in all; the rest stay
/// in `waiting`, in order. All of a batch goes back to the first one's state (its `done`).
fn take_batch<T, K: PartialEq>(first: T, waiting: &mut std::collections::VecDeque<T>, key: impl Fn(&T) -> K, max: usize) -> Vec<T> {
    let k = key(&first);
    let mut jobs = vec![first];
    let mut i = 0;
    while i < waiting.len() && jobs.len() < max {
        if key(&waiting[i]) == k {
            jobs.extend(waiting.remove(i));
        } else {
            i += 1;
        }
    }
    jobs
}

/// The compiler thread's queue (MASU_JIT_ASYNC=0: none, functions are translated where they
/// are needed).
fn compiler() -> Option<std::sync::mpsc::Sender<Job>> {
    // (one per save-state epoch: a restored copy has none of the old thread)
    static Q: Mutex<(u32, Option<Option<std::sync::mpsc::Sender<Job>>>)> = Mutex::new((0, None));
    let mut q = Q.lock().unwrap_or_else(|e| e.into_inner());
    if q.0 != crate::snap::epoch() {
        *q = (crate::snap::epoch(), None);
    }
    q.1.get_or_insert_with(|| {
        if std::env::var("MASU_JIT_ASYNC").is_ok_and(|v| v == "0") {
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
                    let jobs = take_batch(j, &mut waiting, |j| j.state, BATCH_MAX);
                    let done = jobs[0].done.clone();
                    let checked = jit::page_check();
                    let exact = jit::mx_exact();
                    let items: Vec<(u32, u32, Built)> = jobs
                        .into_iter()
                        .map(|j| (j.eip, j.build, build_fn(&mut Linear, j.eip, (j.lo, j.hi), j.part, j.build, &j.maps.0, &j.maps.1, &j.maps.2)))
                        .collect();
                    let mods: Vec<&[u8]> = items.iter().filter_map(|(_, _, b)| b.as_ref().map(|b| &b.0[..])).collect();
                    let merged = if mods.len() > 1 { jit::wasm::merge(&mods, &[]) } else { None };
                    let n = items.len();
                    let _ = done.send(Batch { items, merged, checked, exact });
                    drop(done);
                    (0..n).for_each(|_| crate::snap::drain_done());
                }
            })
            .ok()?;
        Some(tx)
    })
    .clone()
}

/// Installs the functions the compiler thread finished for this thread.
fn take_built(st: &mut State, mem: &mut Linear) {
    while let Ok(Batch { items, merged, checked, exact }) = st.done_rx.try_recv() {
        // (built before the page checks or the MXCSR flag checks came on: dropped, translated
        // again when hot)
        if jit::page_check() && !checked || jit::mx_exact() && !exact {
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
        for ((eip, _, b), k) in items.iter().zip(&kept) {
            if !k && b.is_some() && wx_page(*eip) {
                note_stale(*eip, "while translated");
            }
        }
        // all of them at once when none was dropped
        let t0 = Instant::now();
        let mut base = match merged {
            Some(m) if kept.iter().all(|&k| k) => {
                let n = 2 * items.iter().filter(|i| i.2.is_some()).count();
                #[cfg(target_os = "emscripten")]
                let b = load_module(&m, n as u32);
                #[cfg(not(target_os = "emscripten"))]
                let b = { let _ = (&m, n); -1 };
                note_load(st, b, n as u32, m.len());
                if crate::env_on!("MASU_JIT_TIME") {
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
    if crate::env_on!("MASU_JIT_LAYOUT") {
        jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    profiling();
    let mut m = Module { name: Some(format!("x86_{eip:08x}_fn")), ..Module::default() };
    // a function built again (it left its blocks often) may take more: twice per build
    // (big interpreters' loops, a C library's long switch), from MASU_JIT_FN_INS
    let base = *FN_INS.get_or_init(|| std::env::var("MASU_JIT_FN_INS").ok().and_then(|v| v.parse().ok()).unwrap_or(1000));
    let region = jit::translate_function(&mut m, mem, eip, jit::MAX_BLOCKS, base << (build.min(4) - 1), (lo, hi), &|a| hints.get(&a).copied(), &|a| debug_allowed(a) && ran(misses, known, a), |a| debug_allowed(a) && !(a >= lo && a < hi) && (a == eip || if part { !known.contains(&a) && misses.contains_key(&a) } else { ran(misses, known, a) }))?;
    if crate::env_on!("MASU_JIT_LAYOUT") {
        eprintln!("vnjit fn {eip:#x}\n{}", region.layout);
    }
    if stats() {
        eprintln!("vnjit fn {eip:#x} build {build}: {} instructions", region.instructions);
    }
    let bytes = m.finish();
    if let Some(dir) = jit_dump() {
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{eip:08x}-fn.wasm")), &bytes);
    }
    if crate::env_on!("MASU_JIT_TIME") {
        eprintln!("vnjit time {eip:#x}: {} ins, translate {:?} build {build} part {part}", region.instructions, t0.elapsed());
    }
    let text = region_text(mem, &region)?;
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
    if at.is_none() && crate::env_on!("MASU_JIT_TIME") {
        eprintln!("vnjit time {eip:#x}: {} ins, load {:?}", region.instructions, t1.elapsed());
    }
    count_compile(1, t0);
    if build > 1 {
        count_compile(2, t0);
    }
    if ok {
        keep_profile(&mut region);
        let r = Arc::new(Shared { bytes, starts: region.starts, code: region.code, text, build, sites: region.sites, exact: region.exact, seq: share_seq() });
        share_push(fn_key(eip).to_vec(), r);
        maybe_save();
    }
    ok
}

/// Compiles a function's module (adapter, function) and points the hash table at its blocks.
fn install_fn(st: &mut State, eip: u32, bytes: &[u8], starts: &[(u32, u32)], sites: &[u32], code: &[(u32, u32)]) -> bool {
    #[cfg(target_os = "emscripten")]
    let base = load_module(bytes, 2);
    #[cfg(not(target_os = "emscripten"))]
    let base = { let _ = bytes; -1 };
    st.known.insert(eip);
    if base < 0 {
        return false;
    }
    note_load(st, base, 2, bytes.len());
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
    enter(st, base as u32, vec![eip], &[], code);
    grow(st, starts.len());
    let mut entries = Vec::new();
    for (i, &(a, _)) in starts.iter().enumerate() {
        // (which entry: 0 the root, the first start)
        let v = base as u32 | (i as u32) << jit::ENTRY_SHIFT | jit::FN_TAG;
        if insert(&mut st.hash, a, v) {
            st.used += 1;
        }
        entries.push((a, v));
        // call sites that cached an earlier build go to this one
        if i == 0 {
            for c in st.calls.iter_mut().filter(|c| c[0] == a) {
                c[1] = v;
            }
        }
    }
    track_wx(st, base, entries, vec![eip], Some(eip), code);
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
        // (and the stack's top words: return addresses, arguments)
        let stack: Vec<u32> = (0..12).map(|i| koji::Memory::read32(&mut Linear, c.regs[4].wrapping_add(4 * i)).unwrap_or(0)).collect();
        // ([esp] as a C string: a call's buffer)
        let s: String = (0..40).map(|i| koji::Memory::read8(&mut Linear, stack[0].wrapping_add(i)).unwrap_or(0)).take_while(|&b| b != 0).map(|b| b as char).collect();
        eprintln!("vnjit probe {:#x}: regs {:x?} xmm0 {:?} xmm1 {:?} xmm2 {:?} xmm3 {:?} stack {:x?} str {s:?}", c.eip, c.regs, f(c.xmm[0]), f(c.xmm[1]), f(c.xmm[2]), f(c.xmm[3]), stack);
    }
    let g = GENERATION.load(Ordering::Relaxed);
    let at = ((*cpu).eip, (*cpu).rip_hi);
    let m16 = (*cpu).mode16 != 0;
    if m16 {
        from_linear16(&mut *cpu);
    }
    let r = step(&mut *cpu, &mut Linear).0;
    if m16 {
        to_linear16(&mut *cpu);
    }
    match r {
        // (it rewrote translated code: the block leaves)
        Exit::Continue if GENERATION.load(Ordering::Relaxed) != g => 2,
        // (POPF set TF, or LDMXCSR / FXRSTOR unmasked an SSE exception or set a rounding mode
        // other than to nearest: from here on the interpreter runs the code, see cpu_run)
        Exit::Continue if (*cpu).eflags & koji::flags::TF != 0 || !jit::sse_ok((*cpu).mxcsr) => 2,
        // (it read the MXCSR flags first: the region, made without their checks, leaves)
        Exit::Continue if mx_check() => 2,
        // (a REP stopped part way, koji::REP_STEP_MAX: the region leaves, the instruction goes
        // on at the next run; a block would else go past it)
        Exit::Continue if ((*cpu).eip, (*cpu).rip_hi) == at => 2,
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
        // (jit::long's MASU_JIT64_CHECK: a lazy condition that disagrees with the flags)
        eprintln!("vnjit64 check: condition {b} at {a:#x}: lazy {r}, eflags {c:#x}");
        return 0;
    }
    koji::lazy::Lazy { op, a: a as u64, b: b as u64, r: r as u64, c }.status()
}

/// MASU_JIT_PROFILE: every region's blocks (address, entry counter, instruction keys).
static PROF: Mutex<Vec<(u32, u32, Vec<String>)>> = Mutex::new(Vec::new());

fn profiling() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = crate::env_on!("MASU_JIT_PROFILE");
        jit::PROFILE.store(on, std::sync::atomic::Ordering::Relaxed);
        // MASU_JIT_PROFILE=<seconds> (> 1): print the counts every that many seconds after the
        // first region (under node, where no page asks for them; counts add up, so the difference
        // of two prints is that window)
        if let Some(secs) = std::env::var("MASU_JIT_PROFILE").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&s| s > 1) {
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
    // MASU_PROF_BLOCKS=<n>: more of the hottest blocks (default 40)
    let nblocks = std::env::var("MASU_PROF_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
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
    *ON.get_or_init(|| std::env::var_os("MASU_JIT_STATS").is_some())
}

/// MASU_JIT_STATS: which instructions blocks hand to the interpreter, printed every 2^22.
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

/// MASU_JIT_STATS: where the JIT hands back to the run loop, printed every 2^20.
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

/// MASU_JIT_STATS: entries into the run loop per thread (a guest spinning on system calls
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


#[cfg(test)]
mod batch_tests {
    use super::take_batch;
    use proptest::prelude::*;
    use std::collections::VecDeque;

    /// The queue as #214 had it: a thread's state made again while two of the old state's
    /// jobs waited; the new state's job must not go back with them.
    #[test]
    fn a_new_state_of_the_same_thread_is_its_own_batch() {
        // (state, eip)
        let mut waiting: VecDeque<(u64, u32)> = [(0, 0x4727fa), (1, 0x45c8b9)].into();
        let b = take_batch((0, 0x4727e9), &mut waiting, |j| j.0, 16);
        assert_eq!(b, vec![(0, 0x4727e9), (0, 0x4727fa)]);
        assert_eq!(waiting, VecDeque::from([(1, 0x45c8b9)]));
        let first = waiting.pop_front().unwrap();
        assert_eq!(take_batch(first, &mut waiting, |j| j.0, 16), vec![(1, 0x45c8b9)]);
    }

    proptest! {
        /// Draining a queue batch by batch, as the compiler thread does: every job comes back
        /// exactly once, each batch to one key only, at most `max`, and each key's jobs in the
        /// order they were sent.
        #[test]
        fn batches_keep_every_job_with_its_own_key(keys in prop::collection::vec(0u8..4, 0..64), max in 1usize..20) {
            let mut waiting: VecDeque<(u8, usize)> = keys.iter().copied().zip(0..).collect();
            let mut seen = Vec::new();
            while let Some(first) = waiting.pop_front() {
                let b = take_batch(first, &mut waiting, |j| j.0, max);
                prop_assert!(!b.is_empty() && b.len() <= max);
                prop_assert!(b.iter().all(|j| j.0 == b[0].0));
                prop_assert!(b.windows(2).all(|w| w[0].1 < w[1].1));
                // (what stays behind keeps its order too)
                prop_assert!(waiting.iter().zip(waiting.iter().skip(1)).all(|(a, c)| a.1 < c.1));
                seen.extend(b);
            }
            let mut ids: Vec<usize> = seen.iter().map(|j| j.1).collect();
            ids.sort();
            prop_assert_eq!(ids, (0..keys.len()).collect::<Vec<_>>());
            for k in 0..4u8 {
                let order: Vec<usize> = seen.iter().filter(|j| j.0 == k).map(|j| j.1).collect();
                prop_assert!(order.windows(2).all(|w| w[0] < w[1]));
            }
        }
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    /// host/vnjit.js's TABLE_FULL: the load failed, and the thread's state (with its table) is
    /// made again at its next run loop entry, once; any other answer leaves the state alone.
    #[test]
    fn a_full_table_starts_the_thread_over_once() {
        check_generation();
        with_state(|_| ());
        assert_eq!(table_answer(12), 12);
        assert_eq!(table_answer(-1), -1);
        assert!(!FRESH_TABLE.with(|f| f.get()));
        assert_eq!(table_answer(TABLE_FULL), -1);
        assert!(FRESH_TABLE.with(|f| f.get()));
        check_generation();
        assert!(STATE.with(|s| s.borrow().is_none()));
        assert!(!FRESH_TABLE.with(|f| f.get()));
    }
}

#[cfg(test)]
mod save_tests {
    use super::*;

    fn regions(n: u32, build: u32) -> Vec<(Vec<u32>, u32)> {
        (0..n).map(|i| (vec![0x1000 + i], build)).collect()
    }

    #[test]
    fn saved_only_once_taken() {
        let t0 = Instant::now();
        let mut b = SaveBook::default();
        let id = b.send(t0, regions(3, 0), None);
        // on its way: not sent again, not saved either
        assert!(!b.wanted(&[0x1000], 0));
        assert!(b.wanted(&[0x1000], 1), "a newer build goes");
        b.answer(id, Ack::Taken);
        assert!(!b.wanted(&[0x1000], 0) && b.wanted(&[0x1000], 1));
        assert!(b.inflight.is_empty());
        assert_eq!(b.wait(), SAVE_EVERY);
    }

    #[test]
    fn a_failed_save_goes_again_later_and_later() {
        let t0 = Instant::now();
        let mut b = SaveBook::default();
        let mut waits = Vec::new();
        for _ in 0..6 {
            let id = b.send(t0, regions(2, 0), None);
            b.answer(id, Ack::Failed);
            assert!(b.wanted(&[0x1000], 0) && b.wanted(&[0x1001], 0), "sent again");
            waits.push(b.wait().as_secs());
        }
        assert_eq!(waits, [60, 120, 240, 480, 480, 480]);
        let id = b.send(t0, regions(2, 0), None);
        b.answer(id, Ack::Taken);
        assert_eq!(b.wait(), SAVE_EVERY, "back to every 30 s once one is taken");
        assert!(!b.wanted(&[0x1000], 0));
    }

    #[test]
    fn refused_for_good_is_not_sent_again() {
        let mut b = SaveBook::default();
        let id = b.send(Instant::now(), regions(1, 2), None);
        b.answer(id, Ack::Refused);
        assert!(!b.wanted(&[0x1000], 2) && !b.wanted(&[0x1000], 1));
        assert!(b.wanted(&[0x1000], 3));
    }

    #[test]
    fn no_answer_counts_as_failed() {
        let t0 = Instant::now();
        let mut b = SaveBook::default();
        b.send(t0, regions(1, 0), None);
        assert!(b.chunks_due(0), "the first time, however short");
        let c = b.send(t0, Vec::new(), Some(5));
        assert!(!b.chunks_due(500), "not while one is on its way");
        b.expire(t0 + SAVE_TIMEOUT - Duration::from_secs(1));
        assert_eq!(b.inflight.len(), 2);
        b.expire(t0 + SAVE_TIMEOUT);
        assert!(b.inflight.is_empty() && b.wanted(&[0x1000], 0));
        assert!(b.chunks_due(5), "the chunks list goes again too");
        // a late answer for one already given up: nothing
        b.answer(c, Ack::Taken);
        assert!(b.chunks_due(5) && b.chunks_taken.is_none());
    }

    /// The chunks list goes again as the run reads more (a whole play's reads, not its first
    /// 20 s), CHUNKS_STEP at a time; one taken is the floor, a failed one changes nothing.
    #[test]
    fn chunks_list_goes_again_as_it_grows() {
        let t0 = Instant::now();
        let mut b = SaveBook::default();
        let c = b.send(t0, Vec::new(), Some(10));
        b.answer(c, Ack::Taken);
        assert_eq!(b.chunks_taken, Some(10));
        assert!(!b.chunks_due(10) && !b.chunks_due(10 + CHUNKS_STEP - 1));
        assert!(b.chunks_due(10 + CHUNKS_STEP));
        let c = b.send(t0, Vec::new(), Some(10 + CHUNKS_STEP));
        b.answer(c, Ack::Failed);
        assert_eq!(b.chunks_taken, Some(10), "failed: the old one stays the floor");
        assert!(b.chunks_due(10 + CHUNKS_STEP), "and it goes again");
        let c = b.send(t0, Vec::new(), Some(10 + CHUNKS_STEP));
        b.answer(c, Ack::Refused);
        assert!(!b.chunks_due(10 + CHUNKS_STEP), "refused: not again until it is longer");
        // a region save's answer does not touch the list's
        let r = b.send(t0, regions(1, 0), None);
        b.answer(r, Ack::Taken);
        assert_eq!(b.chunks_taken, Some(10 + CHUNKS_STEP));
        // random answers: the floor only grows, and is the longest taken
        let mut seed = 3u32;
        let mut longest = 10 + CHUNKS_STEP;
        for i in 0..2000 {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            let n = (seed >> 8) as usize % 400;
            if !b.chunks_due(n) {
                continue;
            }
            let c = b.send(t0, Vec::new(), Some(n));
            assert!(!b.chunks_due(n + 1000), "one at a time ({i})");
            let a = [Ack::Taken, Ack::Refused, Ack::Failed][(seed >> 20) as usize % 3];
            b.answer(c, a);
            if a != Ack::Failed {
                longest = longest.max(n);
            }
            assert_eq!(b.chunks_taken, Some(longest));
        }
    }

    /// Games that start the same program (every SiglusEngine.exe at Z:\ippai\game) keep lists
    /// of their own: the page's MASU_CHUNKS_URL, else beside the regions.
    #[test]
    fn chunks_url_is_the_pages() {
        let url = std::ffi::CString::new("http://h/jit-save/sake-x-1").unwrap();
        // (one test changes the variable: no other reads it)
        std::env::remove_var("MASU_CHUNKS_URL");
        assert_eq!(chunks_url(&url).to_str().unwrap(), "http://h/jit-save/sake-x-1.chunks");
        std::env::set_var("MASU_CHUNKS_URL", "http://h/jit-save/sake-x-1.kinkoi.chunks");
        assert_eq!(chunks_url(&url).to_str().unwrap(), "http://h/jit-save/sake-x-1.kinkoi.chunks");
        std::env::remove_var("MASU_CHUNKS_URL");
    }

    /// A trace's answer (NO_ACK) takes no save's slot.
    #[test]
    fn a_trace_answer_leaves_the_saves_alone() {
        let mut b = SaveBook::default();
        // (id 31 mod 32: NO_ACK's slot; far from the other tests' ids)
        b.next_id = 2014;
        let a = b.send(Instant::now(), regions(1, 0), None);
        assert_eq!(a % 32, NO_ACK % 32);
        vnhost_jit_saved(a, 1);
        vnhost_jit_saved(NO_ACK, 0);
        take_acks(&mut b);
        assert!(!b.wanted(&[0x1000], 0), "the save's own answer stands");
    }

    #[test]
    fn answers_come_through_the_slots() {
        let mut b = SaveBook::default();
        // (ids far from the other tests': slots are per process)
        b.next_id = 1000;
        let a = b.send(Instant::now(), regions(1, 0), None);
        let c = b.send(Instant::now(), vec![(vec![0x2000], 0)], None);
        vnhost_jit_saved(a, 1);
        // a stale answer in c's slot (an id 32 before): not c's
        ack(c.wrapping_sub(32), Ack::Taken);
        take_acks(&mut b);
        assert!(!b.wanted(&[0x1000], 0));
        assert!(b.inflight.contains_key(&c));
        vnhost_jit_saved(c, 0);
        take_acks(&mut b);
        assert!(b.inflight.is_empty() && b.wanted(&[0x2000], 0) && b.fails == 1);
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test]
    fn hop_share_estimates_a_sources_hops() {
        assert_eq!(hop_share(16384, 64, 64), 16384);
        assert_eq!(hop_share(16384, 16, 64), 4096);
        assert_eq!(hop_share(100, 12, 64), 18);
        assert_eq!(hop_share(u32::MAX, 64, 64), u32::MAX);
        assert_eq!(hop_share(5, 1, 0), 5); // (no ring: no division by zero)
    }

    /// The policy merge_hot64 follows: startup code that tops many short rings (each read after
    /// 64 hops, at the next miss or API call) stays as it is; a loop that hops for a few
    /// hundred thousand times gets merged.
    #[test]
    fn short_lived_sources_are_not_merged() {
        let limit = merge_hops64();
        // a Godot startup: 1300 rings of 64 hops, the source in 12 of each
        let startup: u64 = (0..1300).map(|_| hop_share(64, 12, 64) as u64).sum();
        assert!(startup < limit as u64);
        // a sort's comparison calls: rings every 16384 hops (the budget), the source in half
        let mut w = 0u32;
        let rings = (0..100).take_while(|_| {
            w = w.saturating_add(hop_share(16384, 32, 64));
            w < limit
        });
        assert!(rings.count() < 40, "merged within a few hundred thousand hops");
    }
}

#[cfg(test)]
mod late_tests {
    use super::*;
    use jit::saved::Entry;
    use proptest::prelude::*;

    fn entry(root: u32, build: u32) -> Entry {
        Entry { roots: vec![root], starts: vec![(root, 0)], code: vec![(root, 4)], text: vec![0x90; 4], build, sites: vec![] }
    }

    /// A part as the page hands it over: its bytes kept for good (as kura keeps the page's).
    fn hand(inbox: &Inbox, entries: &[Entry]) -> bool {
        let idx: &'static [u8] = Vec::leak(jit::saved::encode(entries));
        let wasm: &'static [u8] = Vec::leak(vec![0u8, 0x61, 0x73, 0x6d]);
        inbox.push([wasm.as_ptr() as usize, wasm.len(), idx.as_ptr() as usize, idx.len()])
    }

    proptest! {
        /// However the saved regions come in parts (the first at the start, the others taken
        /// in at any later looks), each region's part and its index in that part's module are
        /// those of the part's own list, and the lookups find every region and function.
        #[test]
        fn late_parts_keep_each_part_s_offsets(
            parts in prop::collection::vec(prop::collection::vec((0u32..64, 0u32..3), 0..12), 1..6),
            looks in prop::collection::vec(0usize..6, 0..6),
        ) {
            let parts: Vec<Vec<Entry>> = parts.iter().map(|p| p.iter().map(|&(r, b)| entry(0x40_0000 + r * 16, b)).collect()).collect();
            let inbox = Inbox::new();
            let mut s = Saved::default();
            s.add(0, parts[0].clone());
            for p in &parts[1..] {
                prop_assert!(hand(&inbox, p));
            }
            // (looks in any order of counts: never backwards, the inbox only grows)
            let mut seen = 0;
            for l in looks.iter().map(|&l| l.min(inbox.len())) {
                seen = seen.max(l);
                take_late(&mut s, &inbox, seen);
            }
            take_late(&mut s, &inbox, inbox.len());
            prop_assert_eq!(s.late.len(), parts.len() - 1);
            let mut i = 0;
            for (k, p) in parts.iter().enumerate() {
                let offs = jit::saved::offsets(p);
                for (e, o) in p.iter().zip(offs) {
                    prop_assert!(s.entries[i] == *e);
                    prop_assert_eq!(s.part[i], k as u16);
                    prop_assert_eq!(s.offsets[i], o);
                    let by = if e.build > 0 { &s.by_fn } else { &s.by_root };
                    prop_assert!(by[&e.roots[0]].contains(&i));
                    i += 1;
                }
                if k > 0 {
                    prop_assert_eq!(s.late[k - 1].1, p.iter().map(|e| e.width()).sum::<u32>());
                }
            }
            prop_assert_eq!(i, s.entries.len());
        }
    }

    /// The page's main thread hands parts over while threads look (each taking in what it
    /// sees under the write lock): every part comes in once, in order, none lost or torn.
    #[test]
    fn parts_handed_over_while_threads_look() {
        for _ in 0..50 {
            let inbox: &'static Inbox = Box::leak(Box::new(Inbox::new()));
            let saved = std::sync::RwLock::new(Saved::default());
            std::thread::scope(|sc| {
                sc.spawn(|| {
                    for k in 0..LATE_MAX as u32 {
                        assert!(hand(inbox, &[entry(0x1000 * (k + 1), 0), entry(0x1000 * (k + 1) + 8, 1)]));
                        std::thread::yield_now();
                    }
                    assert!(!hand(inbox, &[entry(1, 0)]), "past LATE_MAX: refused");
                });
                for _ in 0..3 {
                    sc.spawn(|| {
                        while saved.read().unwrap().late.len() < LATE_MAX {
                            let n = inbox.len();
                            if saved.read().unwrap().late.len() < n {
                                take_late(&mut saved.write().unwrap(), inbox, n);
                            }
                        }
                    });
                }
            });
            let s = saved.into_inner().unwrap();
            assert_eq!(s.late.len(), LATE_MAX);
            assert_eq!(s.entries.len(), 2 * LATE_MAX);
            for (i, e) in s.entries.iter().enumerate() {
                let k = i / 2;
                assert_eq!(s.part[i] as usize, k + 1);
                assert_eq!(e.roots[0], 0x1000 * (k as u32 + 1) + if i % 2 == 1 { 8 } else { 0 });
                assert_eq!(s.offsets[i], if i % 2 == 1 { 1 } else { 0 });
            }
        }
    }
}

#[cfg(test)]
mod invalidate_tests {
    use super::*;
    use proptest::prelude::*;

    /// A region at table index `base`: entries at `starts`, made from `code`.
    fn put(st: &mut State, base: u32, starts: &[u32], code: &[(u32, u32)]) {
        let s: Vec<(u32, u32)> = starts.iter().enumerate().map(|(i, &a)| (a, i as u32)).collect();
        enter(st, base, vec![starts[0]], &s, code);
    }

    fn slot_of(st: &State, a: u32) -> Option<u32> {
        let mask = st.hash.len() as u32 - 1;
        let mut h = jit::dispatch::slot(a, mask);
        while st.hash[h as usize][0] != 0 {
            if st.hash[h as usize][0] == a {
                return Some(st.hash[h as usize][1] & ((1 << jit::ENTRY_SHIFT) - 1));
            }
            h = (h + 1) & mask;
        }
        None
    }

    #[test]
    fn a_page_written_drops_only_what_was_made_from_it() {
        with_state(|st| {
            // region 1 on pages 0x401 and 0x402, region 2 on 0x500, region 3 on 0x401 too
            put(st, 1, &[0x401100, 0x402010], &[(0x401100, 0x20), (0x401ff0, 0x30)]);
            put(st, 2, &[0x500000, 0x500040], &[(0x500000, 0x60)]);
            put(st, 3, &[0x401800], &[(0x401800, 8)]);
            st.calls[0] = [0x401100, 1];
            st.calls[1] = [0x500000, 2];
            assert!(drop_pages(st, &[(0x402, 0x402)]));
            // (region 1 runs into page 0x402: all of it goes, region 3 beside it stays)
            assert_eq!(slot_of(st, 0x401100), None);
            assert_eq!(slot_of(st, 0x402010), None);
            assert_eq!(slot_of(st, 0x401800), Some(3));
            assert_eq!(slot_of(st, 0x500040), Some(2));
            assert_eq!(st.calls[0], [0, 0]);
            assert_eq!(st.calls[1], [0x500000, 2]);
            assert!(!st.code_of.contains_key(&1) && !st.translated.contains(&0x401100) && st.translated.contains(&0x401800));
            assert!(!st.known.contains(&0x401100), "translated again when it runs");
            // nothing made from page 0x600: nothing goes
            assert!(drop_pages(st, &[(0x600, 0x6ff)]));
            assert_eq!(st.used, 3);
            // a translation in flight may be of the old code: the whole state goes instead
            st.pending.insert(0x500000);
            assert!(!drop_pages(st, &[(0x500, 0x500)]));
        });
        STATE.with(|s| *s.borrow_mut() = None);
    }

    #[test]
    fn changes_since_a_generation() {
        let g0 = GENERATION.load(Ordering::SeqCst);
        changed(0x10, 0x11);
        changed(0x20, 0x20);
        let g = GENERATION.load(Ordering::SeqCst);
        let c = changes_since(g0, g).unwrap();
        assert!(c.contains(&(0x10, 0x11)) && c.contains(&(0x20, 0x20)));
        assert_eq!(changes_since(g, g).unwrap(), vec![]);
        // one generation forgotten: not known
        for _ in 0..CHANGES_KEPT + 1 {
            changed(0x30, 0x30);
        }
        assert!(changes_since(g, GENERATION.load(Ordering::SeqCst)).is_none());
        // everything changed: not known either
        let g1 = GENERATION.load(Ordering::SeqCst);
        changed(0, u32::MAX);
        assert!(changes_since(g1, GENERATION.load(Ordering::SeqCst)).is_none());
    }

    proptest! {
        /// Any regions, any written pages: what stays was made from none of them, and every
        /// entry of a region made from none of them stays.
        #[test]
        fn drop_pages_property(regions in proptest::collection::vec(proptest::collection::vec((0u32..64, 1u32..0x3000), 1..4), 1..24), pages in proptest::collection::vec((0u32..64, 0u32..3), 1..4)) {
            let pages: Vec<(u32, u32)> = pages.iter().map(|&(p, n)| (0x400 + p, 0x400 + p + n)).collect();
            with_state(|st| {
                let mut all = Vec::new();
                for (i, r) in regions.iter().enumerate() {
                    let base = 1 + i as u32;
                    // (distinct entry addresses per region)
                    let code: Vec<(u32, u32)> = r.iter().map(|&(p, n)| (0x40_0000 + p * 0x1000 + i as u32 * 16, n)).collect();
                    let starts: Vec<u32> = code.iter().map(|c| c.0).collect();
                    put(st, base, &starts, &code);
                    all.push((base, starts, code));
                }
                prop_assert!(drop_pages(st, &pages));
                let hit = |a: u32, n: u32| pages.iter().any(|&(lo, hi)| a >> 12 <= hi && (a + n - 1) >> 12 >= lo);
                for (base, starts, code) in &all {
                    let bad = code.iter().any(|&(a, n)| hit(a, n));
                    for &a in starts {
                        match slot_of(st, a) {
                            Some(s) => prop_assert!(!all.iter().find(|r| r.0 == s).unwrap().2.iter().any(|&(a, n)| hit(a, n)), "{a:#x} still goes to changed code"),
                            None => prop_assert!(bad || all.iter().any(|r| r.0 > *base && r.1.contains(&a)), "{a:#x} of an untouched region dropped"),
                        }
                    }
                }
                Ok(())
            })?;
            STATE.with(|s| *s.borrow_mut() = None);
        }
    }

    #[test]
    fn shared_builds_are_bounded() {
        let mk = |text: u8, build: u32, n: usize| Arc::new(Shared { bytes: vec![0; n], starts: vec![], code: vec![], text: vec![text], build, sites: vec![], exact: false, seq: share_seq() });
        let key = vec![0xdead_0000u32];
        // a newer build of the same code takes the older one's place
        share_push(key.clone(), mk(1, 1, 10));
        share_push(key.clone(), mk(1, 2, 10));
        let l = SHARED.lock().unwrap().as_ref().unwrap()[&key].clone();
        assert_eq!(l.iter().map(|s| s.build).collect::<Vec<_>>(), vec![2]);
        // other code (rewritten) is kept beside it, up to SHARED_PER_KEY builds
        for t in 2..10 {
            share_push(key.clone(), mk(t, 1, 10));
        }
        assert_eq!(SHARED.lock().unwrap().as_ref().unwrap()[&key].len(), SHARED_PER_KEY);
        // the oldest go past SHARED_MAX
        for i in 0..20u32 {
            share_push(vec![0xbeef_0000 + i], mk(0, 1, 4 << 20));
        }
        assert!(SHARED_BYTES.load(Ordering::Relaxed) <= SHARED_MAX);
        let total: usize = SHARED.lock().unwrap().as_ref().unwrap().values().flatten().map(|s| s.size()).sum();
        assert_eq!(total, SHARED_BYTES.load(Ordering::Relaxed));
        assert!(SHARED.lock().unwrap().as_ref().unwrap().contains_key(&vec![0xbeef_0000 + 19]), "the newest stays");
        assert!(!SHARED.lock().unwrap().as_ref().unwrap().contains_key(&vec![0xbeef_0000]), "the oldest went");
    }
}
