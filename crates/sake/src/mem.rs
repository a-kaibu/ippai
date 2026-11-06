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

/// The 64-bit value of guest address `a` as code sees it: the low alias of a module mapped
/// above 4G (see pe::ALIAS) gets its upper half back.
#[inline]
pub fn widen(a: u32) -> u64 {
    let (lo, hi, n) = crate::pe::alias();
    if n != 0 && a.wrapping_sub(lo) < n { a as u64 | (hi as u64) << 32 } else { a as u64 }
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

/// Guest memory is below 2G (HI); sake's own heap and stacks are above it (emscripten's
/// GLOBAL_BASE). A write the guest asks for there (a bad pointer passed to an API: Wine's tests
/// pass some on purpose) would corrupt sake itself: it is dropped (Windows would fault).
#[inline(always)]
fn guest(a: u32, n: u32) -> bool {
    if a as u64 + n as u64 <= 0x8000_0000 {
        return true;
    }
    refused(a, n);
    false
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
pub fn slice_mut<'a>(a: u32, n: u32) -> &'a mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(a as usize as *mut u8, n as usize) }
}
pub fn fill(a: u32, n: u32, v: u8) {
    if guest(a, n) {
        unsafe { std::ptr::write_bytes(a as usize as *mut u8, v, n as usize) }
    }
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
}

/// Allocations by base. Each allocation is one region here (commit state is kept per
/// allocation, which is all games look at).
static SPACE: Mutex<BTreeMap<u32, Region>> = Mutex::new(BTreeMap::new());

fn align_up(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}

/// Reserves `size` bytes at `want` (0 = anywhere, lowest first; `top` = highest first).
pub fn reserve(want: u32, size: u32, prot: u32, kind: u32, top: bool) -> Option<u32> {
    let size = align_up(size.max(1), PAGE);
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
        let mut holes = Vec::new();
        let mut at = LO;
        for (&a, r) in sp.iter() {
            if a >= at + size {
                holes.push((at, a));
            }
            at = at.max(align_up(a + r.size, GRAN));
        }
        if HI >= at + size {
            holes.push((at, HI));
        }
        if top {
            let &(_, hi) = holes.iter().rev().find(|(lo, hi)| (hi - size) & !(GRAN - 1) >= *lo)?;
            (hi - size) & !(GRAN - 1)
        } else {
            holes.first()?.0
        }
    };
    sp.insert(base, Region { size, prot, state: 0x2000, alloc_base: base, alloc_prot: prot, kind });
    PAGES.lock().unwrap().insert(base, vec![0; (size / PAGE) as usize]);
    Some(base)
}

/// Per-page commit state of each allocation: 0 reserved, COMMITTED | protection.
static PAGES: Mutex<BTreeMap<u32, Vec<u32>>> = Mutex::new(BTreeMap::new());
const COMMITTED: u32 = 0x8000_0000;

/// Tells the JIT which pages are writable and executable (Windows protection `prot`, 0: none):
/// it interprets W|X pages outside the main exe and checks stores on the main exe's.
fn jit_prot(a: u32, n: u32, prot: u32) {
    // PAGE_NOACCESS, PAGE_GUARD: any access faults (prot 0: decommitted or released; reserved
    // pages are not checked)
    kura::jit::fault_pages(a, n, prot & 0xff == 0x01 || prot & 0x100 != 0);
    let wx = prot & (0x40 | 0x80) != 0;
    let p = if prot == 0 { 0 } else if wx { libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC } else { libc::PROT_READ };
    kura::jit::pages_protected(a as usize, n as usize, p);
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
    let first = (a - b) / PAGE;
    let last = ((a - b + n.max(1) + PAGE - 1) / PAGE).min(r.size / PAGE);
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
    let first = (a - b) / PAGE;
    let last = if n == 0 { r.size / PAGE } else { ((a - b + n + PAGE - 1) / PAGE).min(r.size / PAGE) };
    let mut g = PAGES.lock().unwrap();
    if let Some(p) = g.get_mut(&b) {
        for i in first..last {
            if p[i as usize] != 0 {
                fill(b + i * PAGE, PAGE, 0);
                p[i as usize] = 0;
            }
        }
    }
    drop(g);
    jit_prot(b + first * PAGE, (last - first) * PAGE, 0);
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

/// Protection of the page at `a` (VirtualProtect), changing it to `prot` for [a, a+n).
pub fn protect(a: u32, n: u32, prot: u32) -> Option<u32> {
    let (b, r) = find(a)?;
    let mut g = PAGES.lock().unwrap();
    let p = g.get_mut(&b)?;
    let first = (a - b) / PAGE;
    let last = ((a - b + n.max(1) + PAGE - 1) / PAGE).min(r.size / PAGE);
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
        kura::jit::code_changed(a as usize, n as usize);
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
    fill(base, r.size, 0);
    sp.remove(&base);
    PAGES.lock().unwrap().remove(&base);
    true
}

/// The free range at `a` (for VirtualQuery): (start, size).
pub fn free_at(a: u32) -> (u32, u32) {
    let sp = SPACE.lock().unwrap();
    let lo = sp.range(..=a).next_back().map_or(0, |(&b, r)| b + r.size);
    let hi = sp.range(a..).next().map_or(0x8000_0000, |(&b, _)| b);
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
        let size = align_up(size as u32, GRAN);
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

fn slab_lock(t: u32) -> &'static std::sync::atomic::AtomicU32 {
    unsafe { std::sync::atomic::AtomicU32::from_ptr(slice_mut(t, 4).as_mut_ptr() as *mut u32) }
}

/// heap_alloc for HeapAlloc/malloc: small sizes come from the class lists (refilled here when
/// empty, a few blocks at a time), so the x86 fast path can serve the next ones.
pub fn heap_alloc_small(size: u32, zero: bool) -> u32 {
    if size > SLAB_MAX {
        return heap_alloc(size, zero);
    }
    use std::sync::atomic::Ordering;
    let k = slab_class(size);
    let t = slab_table();
    let lock = slab_lock(t);
    while lock.swap(1, Ordering::Acquire) != 0 {
        std::hint::spin_loop();
    }
    let head = r32(t + 4 * k);
    let p = if head != 0 {
        w32(t + 4 * k, r32(head));
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
    let k = slab_class(r32(p - 8));
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

pub fn heap_alloc(size: u32, zero: bool) -> u32 {
    let p = unsafe { HEAP.lock().unwrap().malloc((size + HDR) as usize, 16) } as usize as u32;
    if p == 0 {
        return 0;
    }
    w32(p + HDR - 8, size);
    w32(p + HDR - 4, MAGIC);
    if zero {
        fill(p + HDR, size, 0);
    }
    p + HDR
}

pub fn heap_size(p: u32) -> u32 {
    if p == 0 {
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
    w32(p - 4, 0);
    // (c_free: the header's size may be below the chunk's after an in-place shrink)
    let _ = size;
    unsafe { HEAP.lock().unwrap().c_free((p - HDR) as usize as *mut u8) };
    true
}

pub fn heap_realloc(p: u32, size: u32, zero: bool) -> u32 {
    if p == 0 {
        return heap_alloc_small(size, zero);
    }
    let old = heap_size(p);
    // in place: a small block within its class, or any block shrinking
    let fits = if r32(p - 4) == SLAB { size <= SLAB_MAX && slab_class(size) <= slab_class(old) } else { size <= old };
    if fits {
        if zero && size > old {
            fill(p + old, size - old, 0);
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
        fill(n + old, size - old, 0);
    }
    heap_free_small(p);
    n
}
