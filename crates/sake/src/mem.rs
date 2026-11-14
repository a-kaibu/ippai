//! Guest memory: the low 2 GiB of the wasm linear memory, guest address = host address.
//!
//! The whole range is already backed (emscripten's INITIAL_MEMORY covers it and the host's
//! data starts at 0x80000000), so reserving and committing is bookkeeping only. Pages are
//! zero when first handed out; released ones are zeroed again before reuse.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// (not Windows' 0x10000: koji's JIT range-checks accesses with a 32 KB margin above its
/// GUEST_LO of 0x10000, and a block whose address is below that bails to the interpreter on
/// every access: Borland's memory manager put its table at 0x10000)
pub const LO: u32 = 0x3_0000;
pub const HI: u32 = 0x7ff0_0000;
/// Where sake's own data starts (crates/sake/build.sh GLOBAL_BASE, koji::jit::GUEST_HI)
pub const GUEST_TOP: u32 = 0x8000_0000;
const GRAN: u32 = 0x1_0000;
pub const PAGE: u32 = 0x1000;

// ---------------------------------------------------------------------------------------------
// 64-bit programs (PE32+): pointers, handles and SIZE_Ts are 8 bytes in their structures. Guest
// addresses are still below 4G (and below HI): the upper halves are written as zero and ignored
// when read.

static X64: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the program is a 64-bit one (set by the loader before anything runs).
#[inline(always)]
pub fn x64() -> bool {
    X64.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_x64() {
    X64.store(true, std::sync::atomic::Ordering::Relaxed);
    kura::d3d9::set_ptr_size(8);
}

/// Size of a pointer (4 or 8).
#[inline(always)]
pub fn ps() -> u32 {
    if x64() { 8 } else { 4 }
}

/// Reads a pointer-sized field (its low half on x64).
#[inline(always)]
pub fn rp(a: u32) -> u32 {
    r32(a)
}

/// Writes a pointer-sized field (zero-extended on x64; INVALID_HANDLE_VALUE and other small
/// negative values sign-extended).
#[inline(always)]
pub fn wp(a: u32, v: u32) {
    w32(a, v);
    if x64() {
        w32(a + 4, if v >= 0xffff_f000 { 0xffff_ffff } else { 0 });
    }
}

/// wp if `a` is not NULL.
#[inline]
pub fn putp(a: u32, v: u32) {
    if a != 0 {
        wp(a, v)
    }
}

/// The 64-bit value of guest address `a` as code sees it: an allocation that stands for one
/// above 4G (a fixed image based there, a VirtualAlloc asked there: `set_high`) lives at its
/// address's low half, and addresses in it get the upper half back.
#[inline]
pub fn widen(a: u32) -> u64 {
    if HIGH.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        return a as u64;
    }
    a as u64 | (find(a).map_or(0, |(_, r)| r.hi) as u64) << 32
}

/// How many allocations stand for addresses above 4G (widen's fast path: none).
static HIGH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Makes the allocation at `base` the one at `hi:base` (x64): addresses in it widen to that.
/// Memory is addressed modulo 4G (koji), so allocations above 4G can't share a low half; two
/// of them (or one and a plain low allocation) that would are refused by `reserve` already.
pub fn set_high(base: u32, hi: u32) -> bool {
    let mut sp = SPACE.lock().unwrap();
    let Some(r) = sp.get_mut(&base) else { return false };
    if r.hi == 0 && hi != 0 {
        HIGH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    } else if r.hi != 0 && hi == 0 {
        HIGH.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
    r.hi = hi;
    true
}

/// The upper half of the address the allocation holding `a` stands for (0: a plain one, or
/// none there).
pub fn high_of(a: u32) -> u32 {
    if HIGH.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        return 0;
    }
    find(a).map_or(0, |(_, r)| r.hi)
}

/// Writes guest address `v` as a pointer (x64: its 64-bit value, `widen`) if `a` is not NULL.
pub fn put_addr(a: u32, v: u32) {
    if a != 0 {
        if x64() { w64(a, widen(v)) } else { w32(a, v) }
    }
}

/// Writes `v` if `a` is not NULL.
#[inline]
pub fn put64(a: u32, v: u64) {
    if a != 0 {
        w64(a, v)
    }
}
#[inline(always)]
pub fn r64(a: u32) -> u64 {
    unsafe { (a as usize as *const u64).read_unaligned() }
}
#[inline(always)]
pub fn w64(a: u32, v: u64) {
    if guest(a, 8) {
        unsafe { (a as usize as *mut u64).write_unaligned(v) }
    }
}

/// Guest memory is below 2G (GUEST_TOP); sake's own heap and stacks are above it (emscripten's
/// GLOBAL_BASE). A write the guest asks for there (a bad pointer passed to an API: Wine's tests
/// pass some on purpose) would corrupt sake itself: it is dropped (Windows would fault).
#[inline(always)]
pub fn guest(a: u32, n: u32) -> bool {
    if a as u64 + n as u64 <= GUEST_TOP as u64 {
        return true;
    }
    refused(a, n);
    false
}
/// Whether `n` bytes at `a` are all guest memory (a check before reading a guest buffer whose
/// length the guest gave)
pub fn in_guest(a: u32, n: u32) -> bool {
    a as u64 + n as u64 <= GUEST_TOP as u64
}
#[cold]
#[inline(never)]
fn refused(a: u32, n: u32) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| eprintln!("sake: a write of {n} bytes to {a:#x} (not guest memory) was dropped"));
}

#[inline(always)]
pub fn r8(a: u32) -> u8 {
    unsafe { *(a as usize as *const u8) }
}
#[inline(always)]
pub fn r16(a: u32) -> u16 {
    unsafe { (a as usize as *const u16).read_unaligned() }
}
#[inline(always)]
pub fn r32(a: u32) -> u32 {
    unsafe { (a as usize as *const u32).read_unaligned() }
}
#[inline(always)]
pub fn w8(a: u32, v: u8) {
    if guest(a, 1) {
        unsafe { *(a as usize as *mut u8) = v }
    }
}
#[inline(always)]
pub fn w16(a: u32, v: u16) {
    if guest(a, 2) {
        unsafe { (a as usize as *mut u16).write_unaligned(v) }
    }
}
#[inline(always)]
pub fn w32(a: u32, v: u32) {
    if guest(a, 4) {
        unsafe { (a as usize as *mut u32).write_unaligned(v) }
    }
}
/// Writes `v` if `a` is not NULL.
#[inline]
pub fn put32(a: u32, v: u32) {
    if a != 0 {
        w32(a, v)
    }
}
pub fn slice<'a>(a: u32, n: u32) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(a as usize as *const u8, n as usize) }
}
/// Guest memory `a..a + n` for sake to write into, or None where that is not the guest's: in
/// the NULL pages (a failed allocation, a NULL pointer with a length) or reaching past
/// GUEST_TOP into sake's own memory (review 2026-11-14: the unchecked slice_mut wrote there).
pub fn guest_mut<'a>(a: u32, n: u32) -> Option<&'a mut [u8]> {
    if n == 0 {
        return Some(&mut []);
    }
    if !guest_range(a, n) {
        if a >= NULL_TOP {
            refused(a, n);
        }
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts_mut(a as usize as *mut u8, n as usize) })
}
/// Whether `a..a + n` is guest memory sake may write for the program (guest_mut's rule).
#[inline]
pub fn guest_range(a: u32, n: u32) -> bool {
    n == 0 || (a >= NULL_TOP && a as u64 + n as u64 <= GUEST_TOP as u64)
}
/// The guest's first 64 KiB: never handed out (NULL pointers fault on Windows too).
pub const NULL_TOP: u32 = 0x1_0000;
pub fn fill(a: u32, n: u32, v: u8) {
    if guest(a, n) {
        unsafe { std::ptr::write_bytes(a as usize as *mut u8, v, n as usize) }
    }
}
/// Zeroes [a, a+n) without writing to the pages in it that are zero already. The browser backs
/// a page of the wasm memory with RAM at its first write and never takes it back (wasm cannot
/// give memory back), while a read of a page never written costs nothing (the kernel's shared
/// zero page). Zeroing what the program reserved but never used (a 256 MB block of which it
/// committed a few pages, a HEAP_ZERO_MEMORY block it never fills) would make all of it RAM.
/// Short ranges are filled plainly.
pub fn zero(a: u32, n: u32) {
    if n < ZERO_SKIP_MIN {
        return fill(a, n, 0);
    }
    if !guest(a, n) {
        return;
    }
    let end = a as u64 + n as u64;
    let mut p = a as u64;
    while p < end {
        let next = ((p | (PAGE as u64 - 1)) + 1).min(end);
        // (inside guest memory: checked above)
        let s = unsafe { std::slice::from_raw_parts_mut(p as usize as *mut u8, (next - p) as usize) };
        if !is_zero(s) {
            s.fill(0);
        }
        p = next;
    }
}
/// (below this zero() fills: one or two pages)
const ZERO_SKIP_MIN: u32 = 2 * PAGE;

/// Whether every byte of `s` is 0 (stops at the first 256-byte block that is not; within a
/// block, no early exit, so that it is vectorized).
pub fn is_zero(s: &[u8]) -> bool {
    let mut c = s.chunks_exact(256);
    for b in &mut c {
        if b.iter().fold(0u8, |x, &y| x | y) != 0 {
            return false;
        }
    }
    c.remainder().iter().all(|&b| b == 0)
}

pub fn copy(dst: u32, src: u32, n: u32) {
    if guest(dst, n) {
        unsafe { std::ptr::copy(src as usize as *const u8, dst as usize as *mut u8, n as usize) }
    }
}
pub fn write(dst: u32, data: &[u8]) {
    if guest(dst, data.len() as u32) {
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), dst as usize as *mut u8, data.len()) }
    }
}

/// NUL-terminated byte string at `a` (without the NUL).
pub fn cstr(a: u32) -> Vec<u8> {
    if a == 0 {
        return Vec::new();
    }
    // (one scan of the guest memory up to GUEST_TOP: an unterminated string stops there
    // instead of wrapping past 4 GB, review b07)
    if a < GUEST_TOP {
        let s = slice(a, GUEST_TOP - a);
        return s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())].to_vec();
    }
    let mut n = 0;
    while r8(a + n) != 0 {
        n += 1;
    }
    slice(a, n).to_vec()
}
/// NUL-terminated UTF-16 string at `a`.
pub fn wstr(a: u32) -> Vec<u16> {
    if a == 0 {
        return Vec::new();
    }
    if a < GUEST_TOP {
        let s = slice(a, (GUEST_TOP - a) & !1);
        let units = s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]));
        let n = units.clone().position(|c| c == 0).unwrap_or(s.len() / 2);
        let mut v = Vec::with_capacity(n);
        v.extend(units.take(n));
        return v;
    }
    let mut v = Vec::new();
    let mut p = a;
    loop {
        let c = r16(p);
        if c == 0 {
            break;
        }
        v.push(c);
        p += 2;
    }
    v
}
/// Up to `n` bytes, stopping at a NUL.
pub fn cstrn(a: u32, n: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        let c = r8(a + i);
        if c == 0 {
            break;
        }
        v.push(c);
    }
    v
}
/// Up to `n` UTF-16 units, stopping at a NUL.
pub fn wstrn(a: u32, n: u32) -> Vec<u16> {
    let mut v = Vec::new();
    for i in 0..n {
        let c = r16(a + 2 * i);
        if c == 0 {
            break;
        }
        v.push(c);
    }
    v
}
/// Copies `s` + NUL to `dst` (at most `cap` bytes including the NUL); returns bytes without NUL.
pub fn put_cstr(dst: u32, cap: u32, s: &[u8]) -> u32 {
    if cap == 0 || dst == 0 {
        return 0;
    }
    let n = (s.len() as u32).min(cap - 1);
    write(dst, &s[..n as usize]);
    w8(dst + n, 0);
    n
}
pub fn put_wstr(dst: u32, cap: u32, s: &[u16]) -> u32 {
    if cap == 0 || dst == 0 {
        return 0;
    }
    let n = (s.len() as u32).min(cap - 1);
    for i in 0..n {
        w16(dst + 2 * i, s[i as usize]);
    }
    w16(dst + 2 * n, 0);
    n
}

// ---------------------------------------------------------------------------------------------
// the address space

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub size: u32,
    pub prot: u32,
    /// MEM_COMMIT (0x1000) or MEM_RESERVE (0x2000)
    pub state: u32,
    pub alloc_base: u32,
    pub alloc_prot: u32,
    /// MEM_IMAGE 0x1000000, MEM_MAPPED 0x40000, MEM_PRIVATE 0x20000
    pub kind: u32,
    /// x64: the upper half of the address it stands for (`set_high`), 0 for a plain one
    pub hi: u32,
}

/// Allocations by base. Each allocation is one region here (commit state is kept per
/// allocation, which is all games look at).
static SPACE: Mutex<BTreeMap<u32, Region>> = Mutex::new(BTreeMap::new());

/// Save states (snap.rs): the guest memory a state holds, its allocations.
pub fn snap_ranges(v: &mut Vec<(u32, u32)>) {
    let sp = SPACE.lock().unwrap_or_else(|e| e.into_inner());
    v.extend(sp.iter().map(|(&a, r)| (a, r.size)));
}

fn align_up(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}
/// `size` (at least 1) rounded up to whole pages, None past the guest address space (a size
/// near 4 GB wrapped to 0 and reserved an empty region the program took for ~4 GB, review b07)
fn page_size(size: u32) -> Option<u32> {
    let n = (size.max(1) as u64).div_ceil(PAGE as u64) * PAGE as u64;
    (n <= HI as u64).then_some(n as u32)
}

/// Reserves `size` bytes at `want` (0 = anywhere, lowest first; `top` = highest first).
pub fn reserve(want: u32, size: u32, prot: u32, kind: u32, top: bool) -> Option<u32> {
    let size = page_size(size)?;
    let mut sp = SPACE.lock().unwrap();
    let base = if want != 0 {
        let b = want & !(GRAN - 1);
        let end = b.checked_add(size)?;
        if b < LO || end > HI {
            return None;
        }
        let clash = sp.range(..end).next_back().is_some_and(|(&a, r)| a + r.size > b);
        if clash {
            return None;
        }
        b
    } else {
        let holes = holes(&sp, size);
        if top {
            let &(_, hi) = holes.iter().rev().find(|(lo, hi)| (hi - size) & !(GRAN - 1) >= *lo)?;
            (hi - size) & !(GRAN - 1)
        } else {
            holes.first()?.0
        }
    };
    insert(&mut sp, base, size, prot, kind);
    Some(base)
}

/// The free ranges [lo, hi) of the address space that hold `size` bytes.
fn holes(sp: &BTreeMap<u32, Region>, size: u32) -> Vec<(u32, u32)> {
    let mut holes = Vec::new();
    let mut at = LO;
    for (&a, r) in sp.iter() {
        if a >= at.saturating_add(size) {
            holes.push((at, a));
        }
        at = at.max(align_up(a + r.size, GRAN));
    }
    if HI >= at.saturating_add(size) {
        holes.push((at, HI));
    }
    holes
}

fn insert(sp: &mut BTreeMap<u32, Region>, base: u32, size: u32, prot: u32, kind: u32) {
    sp.insert(base, Region { size, prot, state: 0x2000, alloc_base: base, alloc_prot: prot, kind, hi: 0 });
    PAGES.lock().unwrap().insert(base, vec![0; (size / PAGE) as usize]);
}

/// Reserves `size` bytes at an `align`-aligned address (a power of two, at least GRAN), the
/// lowest one free; None when no free range holds all of them. (Never fewer: the program
/// takes the whole block as its own. A smaller block it once got at the top of a full address
/// space ran on over the TEBs, stacks and DLL images above, お仕置きLesson 体験版, Unity
/// 6000.4.)
pub fn reserve_aligned(size: u32, align: u32, prot: u32, kind: u32) -> Option<u32> {
    let size = page_size(size)?;
    let mut sp = SPACE.lock().unwrap();
    let base = aligned_fit(&holes(&sp, size), size, align)?;
    insert(&mut sp, base, size, prot, kind);
    Some(base)
}

/// The lowest `align`-aligned address with `size` bytes free in one of `holes` ([lo, hi))
fn aligned_fit(holes: &[(u32, u32)], size: u32, align: u32) -> Option<u32> {
    holes.iter().find_map(|&(lo, hi)| {
        let b = (lo as u64 + align as u64 - 1) & !(align as u64 - 1);
        (b + size as u64 <= hi as u64).then_some(b as u32)
    })
}

/// Whether [a, a+n) lies inside one allocation (VirtualAlloc's MEM_COMMIT of a range that runs
/// past its reservation's end fails on Windows, ERROR_INVALID_ADDRESS)
pub fn in_one_allocation(a: u32, n: u32) -> bool {
    find(a).is_some_and(|(b, r)| a as u64 + n.max(1) as u64 <= b as u64 + r.size as u64)
}

/// The size a program reserves to find an aligned block inside (twice the block less a page,
/// Unity 6's allocators: 512 MB - 4 KB for a 256 MB block on a 256 MB boundary): the block
/// size, when `size` is that (blocks of 16 MB and up).
pub fn over_reserved_block(size: u32) -> Option<u32> {
    let s = size.checked_add(PAGE)?;
    (s.is_power_of_two() && s >= 32 << 20).then_some(s / 2)
}

/// Per-page commit state of each allocation: 0 reserved, COMMITTED | protection.
static PAGES: Mutex<BTreeMap<u32, Vec<u32>>> = Mutex::new(BTreeMap::new());
const COMMITTED: u32 = 0x8000_0000;

/// Tells the JIT which pages are writable and executable (Windows protection `prot`, 0: none):
/// it interprets W|X pages outside the main exe and checks stores on the main exe's.
fn jit_prot(a: u32, n: u32, prot: u32) {
    // PAGE_NOACCESS, PAGE_GUARD: any access faults; PAGE_READONLY, PAGE_EXECUTE(_READ): a
    // write (prot 0: decommitted or released, see decommit_range; reserved pages are not
    // checked)
    use kura::jit::{READ_FAULTS, WRITE_FAULTS};
    let none = prot & 0xff == 0x01 || prot & 0x100 != 0;
    let kind = if none {
        READ_FAULTS | WRITE_FAULTS
    } else if prot & 0xff != 0 && prot & 0xcc == 0 {
        WRITE_FAULTS
    } else {
        0
    };
    kura::jit::fault_pages(a, n, kind, none);
    let wx = prot & (0x40 | 0x80) != 0;
    let p = if prot == 0 { 0 } else if wx { libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC } else { libc::PROT_READ };
    kura::jit::pages_protected(a as usize, n as usize, p);
}

/// The pages [first, last) of an allocation of `size` bytes that [off, off + n) touches (`off`
/// inside it), cut at its end: in 64 bits, since `n` is the guest's (VirtualProtect,
/// NtProtectVirtualMemory, VirtualFree's size): `off + n + PAGE - 1` in u32 wrapped for n near
/// 0xFFFFFFFF, `last` came out before `first` and `p[first..last]` panicked, the VM gone, or
/// `(last - first) * PAGE` underflowed into the JIT's page tables (review A-13).
pub(crate) fn page_span(off: u32, n: u32, size: u32) -> (u32, u32) {
    let pages = size / PAGE;
    let first = (off / PAGE).min(pages);
    let last = (off as u64 + n as u64).div_ceil(PAGE as u64).min(pages as u64) as u32;
    (first, last.max(first))
}

/// Commits a whole allocation.
pub fn commit(base: u32, prot: u32) {
    let size = SPACE.lock().unwrap().get(&base).map(|r| r.size);
    if let Some(n) = size {
        commit_range(base, n, prot);
    }
}

/// Commits the pages of [a, a+n) (inside one allocation); false if they are not reserved.
pub fn commit_range(a: u32, n: u32, prot: u32) -> bool {
    let Some((b, r)) = find(a) else { return false };
    let (first, last) = page_span(a - b, n.max(1), r.size);
    if let Some(p) = PAGES.lock().unwrap().get_mut(&b) {
        for x in &mut p[first as usize..last as usize] {
            *x = COMMITTED | prot;
        }
    }
    if let Some(r) = SPACE.lock().unwrap().get_mut(&b) {
        r.state = 0x1000;
        r.prot = prot;
    }
    jit_prot(b + first * PAGE, (last - first) * PAGE, prot);
    true
}

/// Decommits (and zeroes) the pages of [a, a+n); n = 0: to the end of the allocation.
pub fn decommit_range(a: u32, n: u32) -> bool {
    let Some((b, r)) = find(a) else { return false };
    let (first, last) = if n == 0 { ((a - b) / PAGE, r.size / PAGE) } else { page_span(a - b, n, r.size) };
    let mut g = PAGES.lock().unwrap();
    if let Some(p) = g.get_mut(&b) {
        for i in first..last {
            if p[i as usize] != 0 {
                if !is_zero(slice(b + i * PAGE, PAGE)) {
                    fill(b + i * PAGE, PAGE, 0);
                }
                p[i as usize] = 0;
            }
        }
    }
    drop(g);
    jit_prot(b + first * PAGE, (last - first) * PAGE, 0);
    // an access to a decommitted page faults
    kura::jit::fault_pages(b + first * PAGE, (last - first) * PAGE, kura::jit::READ_FAULTS | kura::jit::WRITE_FAULTS, false);
    true
}

/// VirtualQuery: (allocation base, region, run start, run size, MEM_COMMIT/MEM_RESERVE, protection).
pub fn query(a: u32) -> Option<(u32, Region, u32, u32, u32, u32)> {
    let (b, r) = find(a)?;
    let g = PAGES.lock().unwrap();
    let p = g.get(&b)?;
    let i = ((a - b) / PAGE) as usize;
    let v = p[i];
    let mut j = i;
    while j < p.len() && p[j] == v {
        j += 1;
    }
    let state = if v & COMMITTED != 0 { 0x1000 } else { 0x2000 };
    Some((b, r, b + i as u32 * PAGE, (j - i) as u32 * PAGE, state, if v & COMMITTED != 0 { v & !COMMITTED } else { 0 }))
}

/// How many of the `n` bytes from `a` on can be read (`write` false) or written: committed pages
/// whose protection allows it (no PAGE_GUARD). `code_too`: executable pages count as writable,
/// as for WriteProcessMemory (it makes them writable for the copy; read-only data it refuses).
pub fn accessible(a: u32, n: u32, write: bool, code_too: bool) -> u32 {
    let mut at = a as u64;
    let end = a as u64 + n as u64;
    while at < end {
        if at < LO as u64 {
            break;
        }
        let Some((_, _, run, size, state, prot)) = query(at as u32) else { break };
        let ok = state == 0x1000
            && prot & 0x100 == 0
            && if write { prot & 0xcc != 0 || code_too && prot & 0x30 != 0 } else { prot & 0xfe != 0 };
        if !ok {
            break;
        }
        at = run as u64 + size as u64;
    }
    (at.min(end) - a as u64) as u32
}

/// Protection of the page at `a` (VirtualProtect), changing it to `prot` for [a, a+n).
pub fn protect(a: u32, n: u32, prot: u32) -> Option<u32> {
    let (b, r) = find(a)?;
    let mut g = PAGES.lock().unwrap();
    let p = g.get_mut(&b)?;
    let (first, last) = page_span(a - b, n.max(1), r.size);
    let old = p[first as usize] & !COMMITTED;
    for x in &mut p[first as usize..last as usize] {
        if *x & COMMITTED != 0 {
            *x = COMMITTED | prot;
        }
    }
    drop(g);
    jit_prot(b + first * PAGE, (last - first) * PAGE, prot);
    // writable code may be rewritten: the JIT drops what it translated there (QLIE patches its
    // code with VirtualProtect)
    if prot & 0xcc != 0 {
        kura::jit::code_changed(a as usize, (b + last * PAGE - a) as usize);
    }
    Some(if old == 0 { r.prot } else { old })
}

/// An access to `a` faulted: whether its page was a guard page (PAGE_GUARD), which it is no
/// longer (STATUS_GUARD_PAGE_VIOLATION, once; the next access goes through).
pub fn guard_hit(a: u32) -> bool {
    let Some((b, _)) = find(a) else { return false };
    let i = ((a - b) / PAGE) as usize;
    let mut g = PAGES.lock().unwrap();
    let Some(x) = g.get_mut(&b).and_then(|p| p.get_mut(i)) else { return false };
    if *x & COMMITTED == 0 || *x & 0x100 == 0 {
        return false;
    }
    *x &= !0x100;
    let prot = *x & !COMMITTED;
    drop(g);
    jit_prot(b + i as u32 * PAGE, PAGE, prot);
    true
}

/// A guard page at `a` was hit: if it is inside the 32-bit thread stack of `teb`, the page
/// below becomes the guard page (unless it is the stack's last one) and StackLimit the hit
/// page's base.
pub fn stack_grow(teb: u32, a: u32) -> bool {
    let (dealloc, base) = (r32(teb + 0xe0c), r32(teb + 4));
    if dealloc == 0 || !(dealloc..base).contains(&a) {
        return false;
    }
    let page = a & !(PAGE - 1);
    if page >= dealloc + PAGE {
        commit_range(page - PAGE, PAGE, 0x04 | 0x100);
    }
    if page < r32(teb + 8) {
        w32(teb + 8, page);
    }
    true
}

/// Whether `a` is in the lowest page of the thread stack of `teb` (between DeallocationStack
/// and a page above it), which new_teb leaves inaccessible.
pub fn stack_floor(teb: u32, a: u32) -> bool {
    let dealloc = if x64() { r64(teb + 0x1478) } else { r32(teb + 0xe0c) as u64 };
    dealloc != 0 && (dealloc..dealloc + PAGE as u64).contains(&(a as u64))
}

/// The allocation holding `a`.
pub fn find(a: u32) -> Option<(u32, Region)> {
    let sp = SPACE.lock().unwrap();
    let (&b, r) = sp.range(..=a).next_back()?;
    (a < b + r.size).then_some((b, *r))
}

pub fn release(base: u32) -> bool {
    // zeroed before it leaves the map: once out, another thread may reserve it (a new thread's
    // stack was zeroed under it)
    let mut sp = SPACE.lock().unwrap();
    let Some(r) = sp.get(&base).copied() else { return false };
    kura::jit::code_changed(base as usize, r.size as usize);
    jit_prot(base, r.size, 0);
    zero(base, r.size);
    if r.hi != 0 {
        HIGH.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
    sp.remove(&base);
    PAGES.lock().unwrap().remove(&base);
    true
}

/// The allocations, for logs (SAKE_TRACE_MEM): base, size, kind; adjacent small ones merged.
pub fn dump() -> String {
    let sp = SPACE.lock().unwrap();
    let mut out = String::new();
    let pages = PAGES.lock().unwrap();
    for (&b, r) in sp.iter() {
        // (pages committed, and the end of the last one, in KB from the start)
        let (n, top) = pages.get(&b).map_or((0, 0), |p| {
            (p.iter().filter(|&&x| x & COMMITTED != 0).count(), p.iter().rposition(|&x| x & COMMITTED != 0).map_or(0, |i| i + 1))
        });
        out += &format!("  {b:08x}-{:08x} {:6} KB kind {:x} state {:x}, committed {} KB up to +{} KB\n", b + r.size, r.size >> 10, r.kind, r.state, n * 4, top * 4);
    }
    out
}

/// The free range at `a` (for VirtualQuery): (start, size).
pub fn free_at(a: u32) -> (u32, u32) {
    let sp = SPACE.lock().unwrap();
    let lo = sp.range(..=a).next_back().map_or(0, |(&b, r)| b + r.size);
    let hi = sp.range(a..).next().map_or(GUEST_TOP, |(&b, _)| b);
    (lo & !(PAGE - 1), hi - (lo & !(PAGE - 1)))
}

/// Committed read-write memory anywhere.
pub fn alloc(size: u32) -> Option<u32> {
    let b = reserve(0, size, 4, 0x20000, false)?;
    commit(b, 4);
    Some(b)
}

// ---------------------------------------------------------------------------------------------
// the guest heap (HeapAlloc, GlobalAlloc, LocalAlloc, malloc): dlmalloc over guest regions,
// with an 8-byte header holding the size the caller asked for

struct Sys;

unsafe impl dlmalloc::Allocator for Sys {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        let Some(size) = u32::try_from(size).ok().filter(|&n| n <= HI - GRAN).map(|n| align_up(n, GRAN)) else {
            return (std::ptr::null_mut(), 0, 0);
        };
        match alloc(size) {
            Some(b) => (b as usize as *mut u8, size as usize, 0),
            None => (std::ptr::null_mut(), 0, 0),
        }
    }
    fn remap(&self, _ptr: *mut u8, _old: usize, _new: usize, _can_move: bool) -> *mut u8 {
        std::ptr::null_mut()
    }
    fn free_part(&self, _ptr: *mut u8, _old: usize, _new: usize) -> bool {
        false
    }
    fn free(&self, ptr: *mut u8, _size: usize) -> bool {
        release(ptr as usize as u32)
    }
    fn can_release_part(&self, _flags: u32) -> bool {
        false
    }
    fn allocates_zeros(&self) -> bool {
        true
    }
    fn page_size(&self) -> usize {
        PAGE as usize
    }
}

static HEAP: Mutex<dlmalloc::Dlmalloc<Sys>> = Mutex::new(dlmalloc::Dlmalloc::new_with_allocator(Sys));

/// (16: x64 code keeps SSE values in heap blocks and expects them 16-byte aligned; the size the
/// caller asked for is at p-8, the magic at p-4)
const HDR: u32 = 16;
const MAGIC: u32 = 0x6e69_7776;
/// a small block whose size class (16-byte steps up to SLAB_MAX) bounds its capacity: freed
/// ones go to fast.rs's per-class lists in guest memory (marked SLAB_FREE while there)
pub const SLAB: u32 = 0x6e69_7777;
pub const SLAB_FREE: u32 = 0x6e69_7778;
pub const SLAB_MAX: u32 = 512;

/// The free lists of small blocks: a spin lock word, then the head of class k (k*16 bytes)
/// at +4k for k in 1..=32. The x86 stubs in fast.rs pop and push them.
pub fn slab_table() -> u32 {
    static T: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *T.get_or_init(|| heap_alloc(4 * (1 + SLAB_MAX / 16), true))
}

fn slab_class(size: u32) -> u32 {
    ((size + 15) / 16).max(1)
}

/// A small block's class for good (at p-12, tagged): its size word changes with in-place
/// reallocations, and a 512-byte block shrunk to 8 went back to the 16-byte list when freed,
/// for ever (the big classes kept being refilled from the heap). crt.c and fast.rs read it too.
pub const SLAB_CAP: u32 = 0x5ca9_0000;

/// The class of small block `p`: its tagged word, or (a block from before the word, a state
/// saved earlier) its size's.
fn slab_cap(p: u32) -> u32 {
    let k = r32(p - 12).wrapping_sub(SLAB_CAP);
    if (1..=SLAB_MAX / 16).contains(&k) { k } else { slab_class(r32(p - 8)) }
}

/// (tests: blocks made for the class lists)
#[cfg(test)]
pub(crate) static SLAB_MADE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn slab_lock(t: u32) -> &'static std::sync::atomic::AtomicU32 {
    unsafe { std::sync::atomic::AtomicU32::from_ptr(t as usize as *mut u32) }
}

/// heap_alloc for HeapAlloc/malloc: small sizes come from the class lists (refilled here when
/// empty, a few blocks at a time), so the x86 fast path can serve the next ones.
pub fn heap_alloc_small(size: u32, zero: bool) -> u32 {
    heap_alloc_in(0, size, zero)
}

/// Every small block ever made (they never go back to dlmalloc): what heap_destroy walks
static SLABS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// The big blocks of each private heap (HeapCreate), by heap
static BIG: Mutex<Option<std::collections::HashMap<u32, std::collections::HashSet<u32>>>> = Mutex::new(None);

/// heap_alloc_small for HeapAlloc of private heap `heap` (0: no heap of its own, as malloc's
/// and the process heap's): the block carries its heap at p-16 (the x86 stubs and crt.c write
/// it too), so heap_destroy can free what a heap still holds.
pub fn heap_alloc_in(heap: u32, size: u32, zero: bool) -> u32 {
    if size > SLAB_MAX {
        let p = heap_alloc(size, zero);
        if p != 0 && heap != 0 {
            w32(p - 16, heap);
            BIG.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).entry(heap).or_default().insert(p);
        }
        return p;
    }
    use std::sync::atomic::Ordering;
    let k = slab_class(size);
    let t = slab_table();
    let lock = slab_lock(t);
    while lock.swap(1, Ordering::Acquire) != 0 {
        std::hint::spin_loop();
    }
    let head = r32(t + 4 * k);
    let mut made = Vec::new();
    let p = if head != 0 {
        w32(t + 4 * k, r32(head));
        // (its heap goes with it while the lists are held: heap_destroy walks under the lock)
        w32(head - 16, heap);
        head
    } else {
        // a new block for the caller and a few for the list
        let n = (8192 / (k * 16)).clamp(2, 256);
        let mut first = 0;
        for i in 0..n {
            let b = heap_alloc(k * 16, false);
            if b == 0 {
                break;
            }
            w32(b - 12, SLAB_CAP | k);
            w32(b - 16, heap);
            made.push(b);
            #[cfg(test)]
            SLAB_MADE.fetch_add(1, Ordering::Relaxed);
            if i == 0 {
                first = b;
            } else {
                w32(b - 4, SLAB_FREE);
                w32(b, r32(t + 4 * k));
                w32(t + 4 * k, b);
            }
        }
        first
    };
    lock.store(0, Ordering::Release);
    // (after the lists' lock: heap_destroy takes this one first)
    if !made.is_empty() {
        SLABS.lock().unwrap_or_else(|e| e.into_inner()).extend(made);
    }
    if p == 0 {
        return 0;
    }
    w32(p - 8, size);
    w32(p - 4, SLAB);
    if zero {
        fill(p, size, 0);
    }
    p
}

/// heap_free for HeapFree/free: small blocks back to their class list.
pub fn heap_free_small(p: u32) -> bool {
    if p < LO + HDR || r32(p - 4) != SLAB {
        return heap_free(p);
    }
    use std::sync::atomic::Ordering;
    let k = slab_cap(p);
    let t = slab_table();
    let lock = slab_lock(t);
    while lock.swap(1, Ordering::Acquire) != 0 {
        std::hint::spin_loop();
    }
    w32(p - 4, SLAB_FREE);
    w32(p, r32(t + 4 * k));
    w32(t + 4 * k, p);
    lock.store(0, Ordering::Release);
    true
}

/// Block `p` (heap_alloc_in's) as one of private heap `heap` (0: none), as a block that moved
/// in HeapReAlloc
pub fn heap_retag(p: u32, heap: u32) {
    use std::sync::atomic::Ordering;
    if !heap_owns(p) {
        return;
    }
    if r32(p - 4) == SLAB {
        let lock = slab_lock(slab_table());
        while lock.swap(1, Ordering::Acquire) != 0 {
            std::hint::spin_loop();
        }
        w32(p - 16, heap);
        lock.store(0, Ordering::Release);
        return;
    }
    let mut m = BIG.lock().unwrap_or_else(|e| e.into_inner());
    let m = m.get_or_insert_with(Default::default);
    let old = r32(p - 16);
    if old != 0 {
        m.get_mut(&old).map(|s| s.remove(&p));
    }
    w32(p - 16, heap);
    if heap != 0 {
        m.entry(heap).or_default().insert(p);
    }
}

/// HeapDestroy: frees every block heap `heap` still holds (its small ones back to their class
/// lists, its big ones to the heap); the number freed. Programs that make a heap, fill it and
/// destroy it (script VMs, per-scene arenas, CRTs of unloaded DLLs) kept every block.
pub fn heap_destroy(heap: u32) -> usize {
    use std::sync::atomic::Ordering;
    if heap == 0 {
        return 0;
    }
    let mut n = 0;
    let t = slab_table();
    let lock = slab_lock(t);
    let slabs = SLABS.lock().unwrap_or_else(|e| e.into_inner());
    while lock.swap(1, Ordering::Acquire) != 0 {
        std::hint::spin_loop();
    }
    for &b in slabs.iter() {
        if r32(b - 4) == SLAB && r32(b - 16) == heap {
            let k = slab_cap(b);
            w32(b - 4, SLAB_FREE);
            w32(b, r32(t + 4 * k));
            w32(t + 4 * k, b);
            n += 1;
        }
    }
    lock.store(0, Ordering::Release);
    drop(slabs);
    let big = BIG.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(&heap)).unwrap_or_default();
    for p in big {
        if r32(p - 4) == MAGIC && r32(p - 16) == heap && heap_free(p) {
            n += 1;
        }
    }
    n
}

pub fn heap_alloc(size: u32, zero: bool) -> u32 {
    let Some(total) = size.checked_add(HDR) else { return 0 };
    let p = unsafe { HEAP.lock().unwrap().malloc(total as usize, 16) } as usize as u32;
    if p == 0 {
        return 0;
    }
    w32(p + HDR - 16, 0);
    w32(p + HDR - 8, size);
    w32(p + HDR - 4, MAGIC);
    if zero {
        crate::mem::zero(p + HDR, size);
    }
    p + HDR
}

pub fn heap_size(p: u32) -> u32 {
    if p == 0 || !heap_owns(p) {
        return u32::MAX;
    }
    r32(p - 8)
}

/// Whether `p` came from heap_alloc (games free pointers of other heaps through ours rarely,
/// but GlobalFree of a handle and the like do happen).
pub fn heap_owns(p: u32) -> bool {
    p >= LO + HDR && matches!(r32(p - 4), MAGIC | SLAB)
}

pub fn heap_free(p: u32) -> bool {
    if !heap_owns(p) {
        return false;
    }
    let size = r32(p - 8);
    if r32(p - 4) == MAGIC && r32(p - 16) != 0 {
        let h = r32(p - 16);
        if let Some(m) = BIG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            m.get_mut(&h).map(|s| s.remove(&p));
        }
    }
    w32(p - 4, 0);
    // (c_free: the header's size may be below the chunk's after an in-place shrink)
    let _ = size;
    unsafe { HEAP.lock().unwrap().c_free((p - HDR) as usize as *mut u8) };
    true
}

/// Whether block `p` can take `size` bytes in place: a small block within its class, or any
/// block shrinking
pub fn heap_fits(p: u32, size: u32) -> bool {
    if p == 0 || !heap_owns(p) {
        return false;
    }
    let old = heap_size(p);
    if r32(p - 4) == SLAB { size <= SLAB_MAX && slab_class(size) <= slab_cap(p) } else { size <= old }
}

pub fn heap_realloc(p: u32, size: u32, zero: bool) -> u32 {
    if p == 0 {
        return heap_alloc_small(size, zero);
    }
    // (a pointer the heap never gave, or gave and took back: its "size" is whatever lies
    // before it, and a copy of that many bytes could reach anywhere)
    if !heap_owns(p) {
        return 0;
    }
    let old = heap_size(p);
    if heap_fits(p, size) {
        if zero && size > old {
            crate::mem::zero(p + old, size - old);
        }
        w32(p - 8, size);
        return p;
    }
    let n = heap_alloc_small(size, false);
    if n == 0 {
        return 0;
    }
    copy(n, p, old.min(size));
    if zero && size > old {
        crate::mem::zero(n + old, size - old);
    }
    heap_free_small(p);
    n
}

/// A locked read-modify-write of the dword at `a` (the Interlocked* functions): returns the old
/// value and stores `f(old)`. x86 allows the dword unaligned (mono's JIT keeps counters in
/// packed structures) but wasm atomics fault on it: an unaligned one inside an aligned qword
/// swaps that qword, one that straddles two takes a lock (atomic against the others that do).
pub fn atomic_rmw32(a: u32, f: impl Fn(u32) -> u32) -> u32 {
    rmw32_at(a as usize, f)
}
// (fetch_update is try_update on newer toolchains, which the one pinned here lacks)
#[allow(deprecated)]
pub(crate) fn rmw32_at(p: usize, f: impl Fn(u32) -> u32) -> u32 {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst};
    unsafe {
        if p % 4 == 0 {
            let a = &*(p as *const AtomicU32);
            return a.fetch_update(SeqCst, SeqCst, |v| Some(f(v))).unwrap_or_else(|v| v);
        }
        let word = p & !7;
        if p - word <= 4 {
            let w = &*(word as *const AtomicU64);
            let sh = (p - word) as u32 * 8;
            let m = 0xffff_ffffu64 << sh;
            let mut old = 0;
            let _ = w.fetch_update(SeqCst, SeqCst, |cur| {
                old = ((cur & m) >> sh) as u32;
                Some((cur & !m) | ((f(old) as u64) << sh))
            });
            return old;
        }
        static SPLIT: Mutex<()> = Mutex::new(());
        let _g = SPLIT.lock().unwrap_or_else(|e| e.into_inner());
        let old = (p as *const u32).read_unaligned();
        (p as *mut u32).write_unaligned(f(old));
        old
    }
}

#[cfg(test)]
mod tests {
    use super::{over_reserved_block, rmw32_at, GUEST_TOP};

    /// Review A-13: guest sizes near 0xFFFFFFFF in VirtualProtect, VirtualFree(MEM_DECOMMIT) and
    /// commits: the pages run to the allocation's end (the u32 sum wrapped, `p[first..last]`
    /// panicked with last < first, `(last - first) * PAGE` underflowed into the JIT tables).
    #[test]
    fn huge_sizes_stop_at_the_allocation() {
        use super::{commit_range, decommit_range, protect, query, release, reserve, PAGE};
        let _g = crate::test_guest::enter();
        let b = reserve(0, 16 * PAGE, 4, 0x2_0000, false).unwrap();
        assert!(commit_range(b, 16 * PAGE, 4));
        for n in [u32::MAX, u32::MAX - 0xfff, u32::MAX - 0x2fff, 0xffff_f001, 0x8000_0000] {
            assert_eq!(protect(b + 3 * PAGE, n, 2), Some(4), "{n:#x}");
            let q = query(b + 15 * PAGE).unwrap();
            assert_eq!(q.5, 2, "{n:#x}: the last page read-only");
            assert_eq!(query(b + 2 * PAGE).unwrap().5, 4, "{n:#x}: pages before untouched");
            assert!(commit_range(b + 3 * PAGE, n, 4), "{n:#x}");
            assert_eq!(query(b + 15 * PAGE).unwrap().5, 4);
        }
        assert!(decommit_range(b + 5 * PAGE, u32::MAX - 0x3fff));
        assert_eq!(query(b + 15 * PAGE).unwrap().4, 0x2000, "decommitted to the end");
        assert_eq!(query(b + 4 * PAGE).unwrap().4, 0x1000, "the page before stays");
        assert!(release(b));
    }

    proptest::proptest! {
        /// page_span for any offset inside, any size and any allocation: first <= last <= the
        /// allocation's pages; the pages are the ones [off, off + n) touches when it fits.
        #[test]
        fn page_span_any(pages in 1u32..0x10_0000, off_page in 0u32..0x10_0000, within in 0u32..0x1000, n: u32) {
            use super::{page_span, PAGE};
            let size = pages * PAGE;
            let off = (off_page % pages) * PAGE + within;
            let (first, last) = page_span(off, n, size);
            proptest::prop_assert!(first <= last && last <= pages);
            proptest::prop_assert_eq!(first, off / PAGE);
            if off as u64 + n as u64 <= size as u64 {
                proptest::prop_assert_eq!(last as u64, (off as u64 + n as u64).div_ceil(PAGE as u64));
            } else {
                proptest::prop_assert_eq!(last, pages);
            }
        }
    }

    /// review 2026-11-14: sake wrote program-given (pointer, length) pairs through an unchecked
    /// slice: into the NULL pages after a failed allocation, or past 2 GB into its own heap
    #[test]
    fn guest_mut_refuses_what_is_not_the_guests() {
        use super::{guest_mut, guest_range, NULL_TOP};
        assert!(guest_mut(0, 0).is_some_and(|s| s.is_empty()));
        assert!(guest_mut(0, 16).is_none());
        assert!(guest_mut(NULL_TOP - 1, 1).is_none());
        assert!(guest_mut(GUEST_TOP, 1).is_none());
        assert!(guest_mut(GUEST_TOP - 4, 5).is_none());
        assert!(guest_mut(0x40_0000, u32::MAX).is_none());
        assert!(guest_mut(0xffff_fff0, 0x20).is_none());
        assert!(guest_range(NULL_TOP, 1) && guest_range(GUEST_TOP - 4, 4) && guest_range(0x40_0000, GUEST_TOP - 0x40_0000));
    }

    proptest::proptest! {
        /// guest_range: empty, or wholly in [64 KiB, 2 GB), no wrapping
        #[test]
        fn guest_range_is_the_guests_half(a in proptest::prelude::any::<u32>(), n in proptest::prelude::any::<u32>()) {
            use super::{guest_range, NULL_TOP};
            let want = n == 0 || (a >= NULL_TOP && (a as u64 + n as u64) <= GUEST_TOP as u64);
            proptest::prop_assert_eq!(guest_range(a, n), want);
            if guest_range(a, n) && n != 0 {
                proptest::prop_assert!(a.checked_add(n).is_some_and(|e| e <= GUEST_TOP));
            }
        }
    }

    /// Blocks standing for addresses above 4G (#62), as VirtualAlloc and the loader make them:
    /// reserved at the low half, `set_high`; every address inside widens to the high one,
    /// those outside don't; a request whose low half meets a block already there is refused
    /// (koji addresses memory modulo 4G). The bookkeeping only (no guest memory: native test),
    /// in 0x5000_0000..0x7000_0000, where no other test reserves.
    mod high {
        use super::super::{find, high_of, reserve, set_high, widen, HIGH, SPACE};
        use proptest::prelude::*;
        use std::sync::atomic::Ordering::Relaxed;

        fn forget(base: u32) {
            let r = SPACE.lock().unwrap().remove(&base).unwrap();
            if r.hi != 0 {
                HIGH.fetch_sub(1, Relaxed);
            }
            super::super::PAGES.lock().unwrap().remove(&base);
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(300))]
            #[test]
            fn widen_restores_the_high_half(reqs in proptest::collection::vec((1u32..0x7ffe, 0x5000u32..0x6f00, 1u32..0x40), 1..12), probes in proptest::collection::vec(0x5000_0000u32..0x7000_0000, 40)) {
                static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
                let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
                let before = HIGH.load(Relaxed);
                let mut got: Vec<(u32, u32, u32)> = Vec::new();
                for &(hi, lo64k, pages) in &reqs {
                    let (lo, size) = (lo64k << 16, pages * 0x1000);
                    let meets = got.iter().any(|&(b, n, _)| lo < b + n && b < lo + size);
                    let r = reserve(lo, size, 4, 0x2_0000, false);
                    prop_assert_eq!(r.is_none(), meets, "{:#x}+{:#x}", lo, size);
                    if let Some(b) = r {
                        prop_assert_eq!(b, lo);
                        prop_assert!(set_high(b, hi));
                        got.push((b, size, hi));
                    }
                }
                prop_assert_eq!(HIGH.load(Relaxed), before + got.len() as u32);
                for a in probes.iter().copied().chain(got.iter().flat_map(|&(b, n, _)| [b, b + n - 1, b + n])) {
                    let want = got.iter().find(|&&(b, n, _)| a.wrapping_sub(b) < n).map_or(0, |g| g.2);
                    prop_assert_eq!(widen(a), a as u64 | (want as u64) << 32, "{:#x}", a);
                    prop_assert_eq!(high_of(a), want);
                    prop_assert_eq!(find(a).is_some(), got.iter().any(|&(b, n, _)| a.wrapping_sub(b) < n));
                }
                // made plain again, then gone: no high block left
                if let Some(&(b, _, _)) = got.first() {
                    prop_assert!(set_high(b, 0));
                    prop_assert_eq!(widen(b), b as u64);
                }
                for &(b, _, _) in &got {
                    forget(b);
                }
                prop_assert_eq!(HIGH.load(Relaxed), before);
            }
        }
    }

    /// sake's data starts where the JIT's direct guest range ends (build.sh's GLOBAL_BASE too)
    #[test]
    fn guest_top_is_the_jits() {
        assert_eq!(GUEST_TOP, koji::jit::GUEST_HI);
        let build = include_str!("../build.sh");
        assert!(build.contains(&format!("-sGLOBAL_BASE={}", GUEST_TOP)), "build.sh GLOBAL_BASE");
    }

    /// An aligned block is all there or not given: the lowest aligned address whose whole
    /// block lies in one hole (a smaller block at the top of the space once ran Unity 6 over
    /// the TEBs above it)
    mod aligned {
        use super::super::aligned_fit;
        use proptest::prelude::*;

        #[test]
        fn top_of_a_full_space() {
            // (お仕置きLesson: 30 regions of 64 MB taken, 28 MB left under the TEBs)
            let holes = [(0x7c00_0000, 0x7dc1_0000)];
            assert_eq!(aligned_fit(&holes, 0x400_0000, 0x400_0000), None);
            assert_eq!(aligned_fit(&holes, 0x100_0000, 0x100_0000), Some(0x7c00_0000));
            assert_eq!(aligned_fit(&[(0x3_0000, 0x900_0000)], 0x400_0000, 0x400_0000), Some(0x400_0000));
        }

        proptest! {
            #[test]
            fn whole_lowest_aligned(mut cuts in prop::collection::vec(0x3u32..0x7ff0, 0..24), size_pages in 1u32..0x8000, shift in 16u32..28) {
                // holes from sorted cut points (in 64 KB units), every other gap a hole
                cuts.sort();
                cuts.dedup();
                let holes: Vec<(u32, u32)> = cuts.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0] << 16, c[1] << 16)).collect();
                let (size, align) = (size_pages << 12, 1u32 << shift);
                let got = aligned_fit(&holes, size, align);
                let fits = |b: u64| holes.iter().any(|&(lo, hi)| b >= lo as u64 && b + size as u64 <= hi as u64);
                match got {
                    Some(b) => {
                        prop_assert_eq!(b % align, 0);
                        prop_assert!(fits(b as u64));
                        // nothing lower
                        let mut x = 0u64;
                        while x < b as u64 {
                            prop_assert!(!fits(x));
                            x += align as u64;
                        }
                    }
                    None => {
                        let mut x = 0u64;
                        while x < 0x8000_0000 {
                            prop_assert!(!fits(x));
                            x += align as u64;
                        }
                    }
                }
            }
        }
    }

    /// mem::zero: the range ends up zero and nothing outside it changes, whatever was there
    /// (pages that were zero, pages with one byte set anywhere, ranges cut at any byte); and it
    /// leaves pages that were zero unwritten, so that they never become RAM (the RSS tests).
    mod zeroing {
        use super::super::{is_zero, release, reserve, commit, decommit_range, heap_alloc, heap_free, slice, guest_mut, w8, zero, PAGE};
        use proptest::prelude::*;

        #[test]
        fn is_zero_finds_any_byte() {
            for len in [0usize, 1, 255, 256, 257, 4095, 4096, 8191] {
                let mut v = vec![0u8; len];
                assert!(is_zero(&v));
                for i in 0..len {
                    v[i] = 1;
                    assert!(!is_zero(&v), "len {len} byte {i}");
                    v[i] = 0;
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]
            #[test]
            fn zero_is_fill(start in 0u32..3 * PAGE, len in 0u32..6 * PAGE, dirty in prop::collection::vec((0u32..9 * PAGE, 1u8..=255), 0..40)) {
                let _g = crate::test_guest::enter();
                let b = reserve(0, 10 * PAGE, 4, 0x20000, false).unwrap();
                commit(b, 4);
                for &(at, v) in &dirty {
                    w8(b + at, v);
                }
                let before = slice(b, 10 * PAGE).to_vec();
                zero(b + start, len);
                let after = slice(b, 10 * PAGE);
                for i in 0..10 * PAGE {
                    let inside = i >= start && i < start + len;
                    prop_assert_eq!(after[i as usize], if inside { 0 } else { before[i as usize] }, "byte {:#x}", i);
                }
                release(b);
            }
        }

        /// This process's resident memory (smaps Rss of the guest range: the kernel's zero page,
        /// which reads of never-written pages map, is not counted), KiB
        fn guest_rss_kib() -> u64 {
            let (lo, hi) = (super::super::LO as u64, super::super::HI as u64);
            let mut rss = 0;
            let mut inside = false;
            for l in std::fs::read_to_string("/proc/self/smaps").unwrap().lines() {
                if let Some((range, _)) = l.split_once(' ') {
                    if let Some((a, b)) = range.split_once('-') {
                        if let (Ok(a), Ok(b)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) {
                            inside = a < hi && b > lo;
                            continue;
                        }
                    }
                }
                if inside {
                    if let Some(v) = l.strip_prefix("Rss:") {
                        rss += v.trim().trim_end_matches(" kB").trim().parse::<u64>().unwrap();
                    }
                }
            }
            rss
        }

        /// Releasing a big reservation the program used one page of, decommitting pages it
        /// committed and never wrote, a zeroed heap block it never writes: none of it becomes
        /// RAM (each used to write zeros over all of it: 64 MB of RAM for a page used).
        #[test]
        fn unused_pages_stay_out_of_ram() {
            let _g = crate::test_guest::enter();
            const BIG: u32 = 64 << 20;
            let slack = |d: u64| assert!(d < 1024, "{d} KiB became resident");
            // reserve, commit all, write one page, release
            let before = guest_rss_kib();
            let b = reserve(0, BIG, 4, 0x20000, false).unwrap();
            commit(b, 4);
            guest_mut(b + 0x123_4000, 16).unwrap().fill(7);
            release(b);
            slack(guest_rss_kib().saturating_sub(before));
            // the page written is zero again for whoever gets the block next
            let b2 = reserve(b, BIG, 4, 0x20000, false).unwrap();
            assert!(is_zero(slice(b2 + 0x123_4000, PAGE)));
            // commit and decommit without writing
            commit(b2, 4);
            decommit_range(b2, 0);
            release(b2);
            slack(guest_rss_kib().saturating_sub(before));
            // HEAP_ZERO_MEMORY / calloc of fresh memory
            let h = heap_alloc(BIG / 2, true);
            assert_ne!(h, 0);
            assert!(is_zero(slice(h, PAGE)) && is_zero(slice(h + BIG / 2 - PAGE, PAGE)));
            slack(guest_rss_kib().saturating_sub(before));
            heap_free(h);
            slack(guest_rss_kib().saturating_sub(before));
        }
    }

    /// A commit is inside one allocation or refused: not cut at its end
    #[test]
    fn commit_stays_in_one_allocation() {
        use super::{in_one_allocation, reserve, PAGES, SPACE};
        // (bookkeeping only, then forgotten: other tests own 0x5000_0000..0x7000_0000)
        let b = reserve(0, 0x2_0000, 4, 0x20000, false).unwrap();
        assert!(in_one_allocation(b, 0x2_0000));
        assert!(in_one_allocation(b + 0x1_f000, 0x1000));
        assert!(in_one_allocation(b + 0x1_fffc, 4));
        assert!(!in_one_allocation(b + 0x1_f000, 0x1001));
        assert!(!in_one_allocation(b + 0x1_fffc, 5));
        assert!(!in_one_allocation(b + 0x2_0000, 1));
        assert!(!in_one_allocation(b - 1, 2));
        SPACE.lock().unwrap().remove(&b);
        PAGES.lock().unwrap().remove(&b);
    }

    #[test]
    fn over_reserve_idiom() {
        assert_eq!(over_reserved_block(0x1fff_f000), Some(0x1000_0000));
        assert_eq!(over_reserved_block(0x01ff_f000), Some(0x0100_0000));
        // a plain power of two, small blocks and other sizes: not the idiom
        assert_eq!(over_reserved_block(0x2000_0000), None);
        assert_eq!(over_reserved_block(0x00ff_f000), None);
        assert_eq!(over_reserved_block(0x1fff_e000), None);
        assert_eq!(over_reserved_block(0xffff_f000), None); // (4 GB)
        assert_eq!(over_reserved_block(u32::MAX), None);
    }

    /// every offset in a 16-byte buffer: the dword changes, its neighbours don't
    #[test]
    fn rmw32_any_alignment() {
        #[repr(align(8))]
        struct B([u8; 16]);
        for off in 0..=12usize {
            let mut b = B([0xaa; 16]);
            b.0[off..off + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
            let p = b.0.as_mut_ptr() as usize + off;
            assert_eq!(rmw32_at(p, |v| v.wrapping_add(0x0101_0101)), 0x1122_3344);
            assert_eq!(u32::from_le_bytes(b.0[off..off + 4].try_into().unwrap()), 0x1223_3445, "offset {off}");
            assert!(b.0[..off].iter().chain(&b.0[off + 4..]).all(|&x| x == 0xaa), "offset {off}");
        }
    }

    /// increments from several threads at an offset inside a qword and one across two: none lost
    #[test]
    fn rmw32_unaligned_threads() {
        #[repr(align(8))]
        struct B([u8; 24]);
        let mut b = Box::new(B([0; 24]));
        let base = b.0.as_mut_ptr() as usize;
        for off in [1usize, 2, 6] {
            let p = base + off;
            let hs: Vec<_> = (0..4).map(|_| std::thread::spawn(move || (0..20000).for_each(|_| { rmw32_at(p, |v| v + 1); }))).collect();
            hs.into_iter().for_each(|h| h.join().unwrap());
            assert_eq!(unsafe { (p as *const u32).read_unaligned() }, 80000, "offset {off}");
            unsafe { (p as *mut u32).write_unaligned(0) };
        }
    }
}

/// Review b07: HeapDestroy frees what a private heap still holds, and only that.
#[cfg(test)]
mod heap_destroy_tests {
    use super::*;
    use proptest::prelude::*;

    fn live(p: u32) -> bool {
        matches!(r32(p - 4), MAGIC | SLAB)
    }

    #[test]
    fn destroy_frees_small_and_big_blocks_of_its_heap_only() {
        let _g = crate::test_guest::enter();
        let (a, b) = (0x7a_0000, 0x7b_0000);
        let mine: Vec<u32> = (0..300).map(|i| heap_alloc_in(a, [8, 100, 512, 513, 70_000][i % 5], false)).collect();
        let other: Vec<u32> = (0..50).map(|i| heap_alloc_in(b, [16, 4000][i % 2], false)).collect();
        let plain: Vec<u32> = (0..50).map(|i| heap_alloc_small([24, 9000][i % 2], false)).collect();
        for (i, &p) in other.iter().chain(&plain).enumerate() {
            w32(p, i as u32 ^ 0x5a5a_0000);
        }
        assert_eq!(heap_destroy(a), mine.len());
        assert!(mine.iter().all(|&p| !live(p)), "every block of the heap went");
        for (i, &p) in other.iter().chain(&plain).enumerate() {
            assert!(live(p), "{p:#x} of another heap");
            assert_eq!(r32(p), i as u32 ^ 0x5a5a_0000, "{p:#x} kept its bytes");
        }
        // (again: nothing left)
        assert_eq!(heap_destroy(a), 0);
        assert_eq!(heap_destroy(b), other.len());
        for p in plain {
            assert!(heap_free_small(p));
        }
    }

    #[test]
    fn a_block_that_moves_stays_its_heaps() {
        let _g = crate::test_guest::enter();
        let h = 0x7c_0000;
        let p = heap_alloc_in(h, 40, false);
        let q = heap_realloc(p, 5000, false);
        heap_retag(q, h);
        let r = heap_realloc(q, 100, false);
        assert_eq!(r, q, "shrinks in place");
        let s = heap_alloc_in(h, 3000, false);
        let t = heap_realloc(s, 200, false);
        heap_retag(t, h);
        assert_eq!(heap_destroy(h), 2);
        assert!(!live(q) && !live(t));
    }

    #[test]
    fn bad_pointers_have_no_size_and_do_not_move() {
        let _g = crate::test_guest::enter();
        let p = heap_alloc(64, true);
        assert!(heap_free(p));
        assert_eq!(heap_size(p), u32::MAX, "a freed block");
        assert_eq!(heap_realloc(p, 10, false), 0);
        assert!(!heap_fits(p, 10));
        let q = heap_alloc(64, true);
        assert_eq!(heap_size(q + 16), u32::MAX, "inside a block");
        assert_eq!(heap_realloc(q + 16, 1000, false), 0);
        assert_eq!(heap_size(4), u32::MAX);
        assert!(heap_free(q));
    }

    #[derive(Debug, Clone)]
    enum Op {
        Alloc(u8, u32),
        Free(usize),
        Realloc(usize, u32),
        Destroy(u8),
    }

    fn op() -> impl Strategy<Value = Op> {
        let size = prop_oneof![4u32..=512, 513u32..20_000];
        prop_oneof![
            4 => (0u8..4, size.clone()).prop_map(|(h, n)| Op::Alloc(h, n)),
            2 => any::<usize>().prop_map(Op::Free),
            2 => (any::<usize>(), size).prop_map(|(i, n)| Op::Realloc(i, n)),
            1 => (1u8..4).prop_map(Op::Destroy),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
        /// Model: each live block has a heap (0: malloc's) and a stamp in its first word; a
        /// destroy frees exactly the live blocks of that heap, every other block keeps its
        /// stamp, and none is handed out twice.
        #[test]
        fn heaps_against_a_model(ops in proptest::collection::vec(op(), 1..120)) {
            let _g = crate::test_guest::enter();
            let heap = |h: u8| if h == 0 { 0 } else { 0x90_0000 + h as u32 * 0x1_0000 };
            // (what a failed case before this one left)
            for h in 1..4 {
                heap_destroy(heap(h));
            }
            let mut blocks: Vec<(u32, u8, u32)> = vec![];
            let mut stamp = 1u32;
            for o in ops {
                match o {
                    Op::Alloc(h, n) => {
                        let p = heap_alloc_in(heap(h), n, false);
                        prop_assert!(p != 0);
                        prop_assert!(blocks.iter().all(|b| b.0 != p), "handed out twice");
                        w32(p, stamp);
                        blocks.push((p, h, stamp));
                        stamp += 1;
                    }
                    Op::Free(i) if !blocks.is_empty() => {
                        let (p, ..) = blocks.swap_remove(i % blocks.len());
                        prop_assert!(heap_free_small(p));
                    }
                    Op::Realloc(i, n) if !blocks.is_empty() => {
                        let i = i % blocks.len();
                        let (p, h, s) = blocks[i];
                        let q = heap_realloc(p, n, false);
                        prop_assert!(q != 0);
                        heap_retag(q, heap(h));
                        prop_assert_eq!(r32(q), s);
                        blocks[i].0 = q;
                    }
                    Op::Destroy(h) => {
                        let n = blocks.iter().filter(|b| b.1 == h).count();
                        prop_assert_eq!(heap_destroy(heap(h)), n);
                        for b in blocks.iter().filter(|b| b.1 == h) {
                            prop_assert!(!live(b.0));
                        }
                        blocks.retain(|b| b.1 != h);
                    }
                    _ => {}
                }
                for b in &blocks {
                    prop_assert!(live(b.0));
                    prop_assert_eq!(r32(b.0), b.2);
                }
            }
            for h in 1..4 {
                let n = blocks.iter().filter(|b| b.1 == h).count();
                prop_assert_eq!(heap_destroy(heap(h)), n);
            }
            for b in blocks.iter().filter(|b| b.1 == 0) {
                prop_assert!(heap_free_small(b.0));
            }
        }
    }
}

#[cfg(test)]
mod review_b07_tests {
    use super::*;
    use proptest::prelude::*;

    /// The byte-at-a-time readers the bounded scans replaced
    fn cstr_ref(a: u32) -> Vec<u8> {
        let mut v = Vec::new();
        let mut p = a;
        while r8(p) != 0 {
            v.push(r8(p));
            p += 1;
        }
        v
    }
    fn wstr_ref(a: u32) -> Vec<u16> {
        let mut v = Vec::new();
        let mut p = a;
        while r16(p) != 0 {
            v.push(r16(p));
            p += 2;
        }
        v
    }

    #[test]
    fn sizes_past_the_address_space_are_refused() {
        let _g = crate::test_guest::enter();
        for n in [0xffff_f001u32, 0xffff_ffff, HI + 1, GUEST_TOP] {
            assert_eq!(reserve(0, n, 4, 0x20000, false), None, "{n:#x}");
            assert_eq!(reserve(0, n, 4, 0x20000, true), None, "{n:#x}");
            assert_eq!(reserve_aligned(n, GRAN, 4, 0x20000), None, "{n:#x}");
            assert_eq!(alloc(n), None, "{n:#x}");
        }
        assert_eq!(heap_alloc(0xffff_fff0, false), 0);
    }

    proptest! {
        /// Pages: at least the size, less than a page more, None only past HI
        #[test]
        fn page_sizes_round_up_without_wrapping(size: u32) {
            match page_size(size) {
                Some(n) => {
                    prop_assert!(n % PAGE == 0 && n >= size.max(1) && n - size.max(1) < PAGE && n <= HI);
                }
                None => prop_assert!(size as u64 > HI as u64 - PAGE as u64),
            }
        }

        /// The bounded scans read what the byte/unit loops read, at any alignment
        #[test]
        fn strings_read_as_the_plain_loops_do(bytes in proptest::collection::vec(any::<u8>(), 0..300), at in 0u32..8, nul in 0usize..320) {
            let _g = crate::test_guest::enter();
            let b = heap_alloc(400, true);
            prop_assert!(b != 0);
            fill(b, 400, 0);
            let mut v = bytes.clone();
            if nul < v.len() {
                v[nul] = 0;
            }
            for (i, &c) in v.iter().enumerate() {
                w8(b + at + i as u32, c);
            }
            prop_assert_eq!(cstr(b + at), cstr_ref(b + at));
            prop_assert_eq!(wstr(b + at), wstr_ref(b + at));
            heap_free(b);
        }
    }
}
