//! Guest memory: the low 2 GiB of the wasm linear memory, guest address = host address.
//!
//! The whole range is already backed (emscripten's INITIAL_MEMORY covers it and the host's
//! data starts at 0x80000000), so reserving and committing is bookkeeping only. Pages are
//! zero when first handed out; released ones are zeroed again before reuse.

use std::collections::BTreeMap;
use std::sync::Mutex;

pub const LO: u32 = 0x1_0000;
pub const HI: u32 = 0x7ff0_0000;
const GRAN: u32 = 0x1_0000;
pub const PAGE: u32 = 0x1000;

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
    unsafe { *(a as usize as *mut u8) = v }
}
#[inline(always)]
pub fn w16(a: u32, v: u16) {
    unsafe { (a as usize as *mut u16).write_unaligned(v) }
}
#[inline(always)]
pub fn w32(a: u32, v: u32) {
    unsafe { (a as usize as *mut u32).write_unaligned(v) }
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
    unsafe { std::ptr::write_bytes(a as usize as *mut u8, v, n as usize) }
}
pub fn copy(dst: u32, src: u32, n: u32) {
    unsafe { std::ptr::copy(src as usize as *const u8, dst as usize as *mut u8, n as usize) }
}
pub fn write(dst: u32, data: &[u8]) {
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), dst as usize as *mut u8, data.len()) }
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
    Some(if old == 0 { r.prot } else { old })
}

/// The allocation holding `a`.
pub fn find(a: u32) -> Option<(u32, Region)> {
    let sp = SPACE.lock().unwrap();
    let (&b, r) = sp.range(..=a).next_back()?;
    (a < b + r.size).then_some((b, *r))
}

pub fn release(base: u32) -> bool {
    let r = SPACE.lock().unwrap().remove(&base);
    PAGES.lock().unwrap().remove(&base);
    match r {
        Some(r) => {
            fill(base, r.size, 0);
            true
        }
        None => false,
    }
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

const HDR: u32 = 8;

pub fn heap_alloc(size: u32, zero: bool) -> u32 {
    let p = unsafe { HEAP.lock().unwrap().malloc((size + HDR) as usize, 8) } as usize as u32;
    if p == 0 {
        return 0;
    }
    w32(p, size);
    w32(p + 4, 0x6e69_7776);
    if zero {
        fill(p + HDR, size, 0);
    }
    p + HDR
}

pub fn heap_size(p: u32) -> u32 {
    if p == 0 {
        return u32::MAX;
    }
    r32(p - HDR)
}

/// Whether `p` came from heap_alloc (games free pointers of other heaps through ours rarely,
/// but GlobalFree of a handle and the like do happen).
pub fn heap_owns(p: u32) -> bool {
    p >= LO + HDR && r32(p - 4) == 0x6e69_7776
}

pub fn heap_free(p: u32) -> bool {
    if !heap_owns(p) {
        return false;
    }
    let size = r32(p - HDR);
    w32(p - 4, 0);
    unsafe { HEAP.lock().unwrap().free((p - HDR) as usize as *mut u8, (size + HDR) as usize, 8) };
    true
}

pub fn heap_realloc(p: u32, size: u32, zero: bool) -> u32 {
    if p == 0 {
        return heap_alloc(size, zero);
    }
    let old = heap_size(p);
    let n = heap_alloc(size, false);
    if n == 0 {
        return 0;
    }
    copy(n, p, old.min(size));
    if zero && size > old {
        fill(n + old, size - old, 0);
    }
    heap_free(p);
    n
}
