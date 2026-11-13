//! mfc42.dll (MFC 4.2, ANSI, the shared-DLL build Visual C++ 6 programs link against), its
//! Unicode build mfc42u.dll and MFC 4.0's mfc40.dll (Visual C++ 4) for 32-bit programs: sake's
//! own implementation, no Microsoft or Wine code (the differences: `FLAVOR`). Programs import it by
//! ordinal only; specs.txt carries every export's ordinal, decorated name and argument bytes
//! (tests/mfcabi/mfc42.py makes them from the DLL's public symbols), so an export not done here
//! is logged by name and returns 0 instead of jumping to an unknown address (#159).
//!
//! What is here is what an MFC dialog program needs (sinsabell_bm.exe, a self-extractor of a
//! trial, #159): CString, the module/thread state, CWinApp / CWinThread with AfxWinMain and the
//! message pump, CWnd / CDialog with MFC's window procedure, message maps (the program's and
//! MFC's own, as guest structures) and modal loops, DDX, AfxBeginThread, CFile with
//! CFileException thrown as MFC throws it (a C++ `throw CFileException*`), CPaintDC.
//!
//! The objects are the program's: their layouts are MFC 4.2's (mfc42.dll 6.06.8063.0's
//! constructors, read in its disassembly), since the program's inline code and its own
//! classes (derived from CDialog...) read and write the fields. Virtual functions go through
//! the object's vtable: a program's class overrides some slots and points the others at the
//! MFC exports it imports. MFC's own vtables (objects MFC makes: a CWinThread, a temporary
//! CWnd for a handle) are arrays of the exports' trap addresses. Members are thiscall (`this`
//! in ECX, the callee pops the rest: specs.txt).

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::api::{self, implement, Ctx, Handler};
use crate::mem::{self, r16, r32, w16, w32, w8};

/// The MFC DLLs here: one implementation, registered under each DLL's decorated names (a name
/// says its argument types: `PBD` const char*, `PBG` const unsigned short*, so a handler means
/// the same in every DLL that exports its name).
pub const DLLS: &[&str] = &["mfc42", "mfc42u", "mfc40"];
/// The MFC the program imports, an index of DLLS: mfc42.dll; mfc42u.dll (MFC 4.2's Unicode
/// build: TCHAR is WCHAR, CString holds UTF-16, the windows MFC makes are Unicode ones);
/// mfc40.dll (MFC 4.0, Visual C++ 4: CCmdTarget without GetCommandMap, so the virtual
/// functions after it one slot lower, and CWinThread 8 bytes shorter: see `vcall`, `th`)
static FLAVOR: AtomicU8 = AtomicU8::new(0);
/// An import from `dll` (a key): the MFC the program runs on
pub fn use_dll(dll: &str) {
    if let Some(i) = DLLS.iter().position(|&d| d == dll) {
        FLAVOR.store(i as u8, Ordering::Relaxed);
    }
}
pub fn wide() -> bool {
    FLAVOR.load(Ordering::Relaxed) == 1
}
pub fn mfc40() -> bool {
    FLAVOR.load(Ordering::Relaxed) == 2
}
/// The DLL of the MFC in use (its exports: the traps MFC's own objects point at)
fn dll() -> &'static str {
    DLLS[FLAVOR.load(Ordering::Relaxed) as usize]
}
/// A CWinThread / CWinApp field at MFC 4.2's offset `o`: MFC 4.0's CWinThread lacks the 8
/// bytes 4.2 put after m_pfnThreadProc (+0x54), so its later fields and CWinApp's are 8 lower
/// (m_ptCursorLast +0x58, m_hInstance +0x64, m_pszAppName +0x74...)
fn th(o: u32) -> u32 {
    if mfc40() && o >= 0x58 { o - 8 } else { o }
}

// ---------------------------------------------------------------------------------------------
// TCHAR text: sake holds it as units (an ANSI build's bytes, a Unicode build's UTF-16 units)

/// Bytes of a TCHAR
fn unit() -> u32 {
    if wide() { 2 } else { 1 }
}
/// `n` TCHARs at `p`
fn rd(p: u32, n: u32) -> Vec<u16> {
    if wide() { (0..n).map(|i| r16(p + 2 * i)).collect() } else { mem::slice(p, n).iter().map(|&b| b as u16).collect() }
}
/// TCHARs written at `p`
fn wr(p: u32, t: &[u16]) {
    if wide() {
        for (i, &u) in t.iter().enumerate() {
            w16(p + 2 * i as u32, u);
        }
    } else {
        let b: Vec<u8> = t.iter().map(|&u| u as u8).collect();
        mem::write(p, &b);
    }
}
/// The TCHAR at index `i` of `p` set to `u` (a NUL: 0)
fn wr_at(p: u32, i: u32, u: u16) {
    if wide() { w16(p + 2 * i, u) } else { w8(p + i, u as u8) }
}
/// A NUL-terminated TCHAR string (NULL: empty)
fn sz(p: u32) -> Vec<u16> {
    match p {
        0 => Vec::new(),
        _ if wide() => mem::wstr(p),
        _ => mem::cstr(p).into_iter().map(|b| b as u16).collect(),
    }
}
/// Text from ANSI bytes (a Unicode build converts them: the ANSI code page)
fn from_a(b: &[u8]) -> Vec<u16> {
    if wide() { crate::text::mb2w(crate::text::acp(), b) } else { b.iter().map(|&b| b as u16).collect() }
}
/// Text from UTF-16 (an ANSI build converts it)
fn from_w(w: &[u16]) -> Vec<u16> {
    if wide() { w.to_vec() } else { crate::text::w2mb(crate::text::acp(), w).into_iter().map(|b| b as u16).collect() }
}
/// An `LPCSTR` argument (NULL: empty)
fn asz(p: u32) -> Vec<u16> {
    if p == 0 { Vec::new() } else { from_a(&mem::cstr(p)) }
}
/// An `LPCWSTR` argument (NULL: empty)
fn wsz(p: u32) -> Vec<u16> {
    if p == 0 { Vec::new() } else { from_w(&mem::wstr(p)) }
}
/// A `TCHAR` argument of a `D` (char) or `G` (unsigned short) parameter
fn ach(v: u32) -> Vec<u16> {
    from_a(&[v as u8])
}
fn wch(v: u32) -> Vec<u16> {
    from_w(&[v as u16])
}
/// Text for ASCII `s`
fn lit(s: &str) -> Vec<u16> {
    s.bytes().map(|b| b as u16).collect()
}
/// Text as a Rust string (numbers, messages)
fn string(t: &[u16]) -> String {
    if wide() { String::from_utf16_lossy(t) } else { crate::text::a2s(&t.iter().map(|&u| u as u8).collect::<Vec<_>>()) }
}

// ---------------------------------------------------------------------------------------------
// calling helpers

fn this(c: &Ctx) -> u32 {
    c.cpu.regs[1]
}
/// A thiscall function `f` on `obj` (ECX) with the stack arguments `args`.
fn tcall(c: &mut Ctx, f: u32, obj: u32, args: &[u32]) -> u32 {
    let ecx = c.cpu.regs[1];
    c.cpu.regs[1] = obj;
    let r = c.call(f, args);
    c.cpu.regs[1] = ecx;
    r
}
/// The virtual function in slot `slot` (MFC 4.2's numbering) of `obj`. (MFC 4.0's
/// CCmdTarget has no GetCommandMap, slot 13: its classes' later slots are one lower. The
/// slots above 13 called here are all CCmdTarget-derived classes'.)
fn vcall(c: &mut Ctx, obj: u32, slot: u32, args: &[u32]) -> u32 {
    let slot = if mfc40() && slot > 13 { slot - 1 } else { slot };
    let f = r32(r32(obj) + 4 * slot);
    tcall(c, f, obj, args)
}
/// An API of another DLL (its W form for a Unicode MFC: "SendMessageA" is SendMessageW there)
fn win(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let w;
    let name = match name.strip_suffix('A') {
        Some(stem) if wide() && api::exports(dll, &format!("{stem}W")) => {
            w = format!("{stem}W");
            &w
        }
        _ => name,
    };
    let f = api::export(dll, name);
    c.call(f, args)
}
fn user(c: &mut Ctx, name: &str, args: &[u32]) -> u32 {
    win(c, "user32", name, args)
}
fn k32(c: &mut Ctx, name: &str, args: &[u32]) -> u32 {
    win(c, "kernel32", name, args)
}

fn alloc(n: u32) -> u32 {
    mem::heap_alloc_small(n.max(1), true)
}
fn free(p: u32) {
    if p != 0 {
        mem::heap_free_small(p);
    }
}
/// A NUL-terminated copy of `s` (MFC's _tcsdup: the C runtime's heap)
fn strdup(s: &[u16]) -> u32 {
    let p = alloc((s.len() as u32 + 1) * unit());
    wr(p, s);
    wr_at(p, s.len() as u32, 0);
    p
}
/// Scratch guest memory for an out-parameter, freed when dropped.
struct Tmp(u32);
impl Tmp {
    fn new(n: u32) -> Tmp {
        Tmp(alloc(n))
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        free(self.0);
    }
}

fn lo(v: u32) -> u32 {
    v & 0xffff
}
fn hi(v: u32) -> u32 {
    v >> 16
}
fn slo(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}
fn shi(v: u32) -> u32 {
    (v >> 16) as u16 as i16 as i32 as u32
}

// ---------------------------------------------------------------------------------------------
// CString: m_pchData points past a CStringData header {nRefs, nDataLength, nAllocLength};
// the empty string is _afxPchNil (nRefs -1, shared, never freed). Programs read the length at
// [p - 8] inline.

pub mod cstring {
    use super::*;

    /// _afxInitData's characters: the empty string every CString starts as
    pub fn nil() -> u32 {
        static P: OnceLock<u32> = OnceLock::new();
        *P.get_or_init(|| {
            let d = mem::heap_alloc(16, true);
            w32(d, u32::MAX);
            d + 12
        })
    }
    pub fn refs(p: u32) -> i32 {
        r32(p - 12) as i32
    }
    pub fn len(p: u32) -> u32 {
        r32(p - 8)
    }
    pub fn cap(p: u32) -> u32 {
        r32(p - 4)
    }
    /// The text of the CString at `s` (the object).
    pub fn bytes(s: u32) -> Vec<u16> {
        let p = r32(s);
        rd(p, len(p))
    }
    /// AllocBuffer's sizes: 64, 128, 256 and 512-character blocks (MFC's fixed allocators),
    /// the exact length above
    pub fn alloc_len(n: u32) -> u32 {
        match n {
            0..=64 => 64,
            65..=128 => 128,
            129..=256 => 256,
            257..=512 => 512,
            _ => n,
        }
    }
    /// A new buffer of `n` characters (0: the empty string), its own (nRefs 1)
    fn alloc_data(n: u32) -> u32 {
        if n == 0 {
            return nil();
        }
        let a = alloc_len(n);
        let d = mem::heap_alloc_small(12 + (a + 1) * unit(), false);
        w32(d, 1);
        w32(d + 4, n);
        w32(d + 8, a);
        wr_at(d + 12, n, 0);
        d + 12
    }
    /// Releases a buffer: the last reference frees it (InterlockedDecrement, <= 0)
    fn release_data(p: u32) {
        if p == nil() || p == 0 {
            return;
        }
        let n = refs(p) - 1;
        w32(p - 12, n as u32);
        if n <= 0 {
            mem::heap_free_small(p - 12);
        }
    }
    pub fn init(s: u32) {
        w32(s, nil());
    }
    pub fn release(s: u32) {
        release_data(r32(s));
        w32(s, nil());
    }
    fn alloc_buffer(s: u32, n: u32) {
        w32(s, alloc_data(n));
    }
    /// AllocBeforeWrite: an own buffer that holds `n` characters
    fn alloc_before_write(s: u32, n: u32) {
        let p = r32(s);
        if refs(p) > 1 || n > cap(p) {
            release(s);
            alloc_buffer(s, n);
        }
    }
    /// The length of the buffer `p` set to `n` (and its NUL)
    pub fn set_len(p: u32, n: u32) {
        w32(p - 8, n);
        wr_at(p, n, 0);
    }
    /// AssignCopy
    pub fn assign(s: u32, src: &[u16]) {
        let n = src.len() as u32;
        alloc_before_write(s, n);
        let p = r32(s);
        if p == nil() {
            return;
        }
        wr(p, src);
        set_len(p, n);
    }
    /// CopyBeforeWrite: an own buffer (a shared one copied)
    pub fn copy_before_write(s: u32) {
        let p = r32(s);
        if refs(p) > 1 {
            let old = rd(p, len(p));
            release(s);
            alloc_buffer(s, old.len() as u32);
            let q = r32(s);
            if q != nil() {
                wr(q, &old);
            }
        }
    }
    /// operator=(const CString&): shares the source's buffer unless either is locked
    pub fn assign_string(s: u32, src: u32) {
        let (p, q) = (r32(s), r32(src));
        if p == q {
            return;
        }
        if (refs(p) < 0 && p != nil()) || refs(q) < 0 {
            let b = rd(q, len(q));
            assign(s, &b);
        } else {
            release(s);
            w32(s, q);
            w32(q - 12, (refs(q) + 1) as u32);
        }
    }
    /// ConcatCopy into a new string `s`
    pub fn concat_new(s: u32, a: &[u16], b: &[u16]) {
        let n = (a.len() + b.len()) as u32;
        alloc_buffer(s, n);
        let p = r32(s);
        if n != 0 {
            wr(p, a);
            wr(p + a.len() as u32 * unit(), b);
        }
    }
    /// ConcatInPlace
    pub fn append(s: u32, b: &[u16]) {
        if b.is_empty() {
            return;
        }
        let p = r32(s);
        let n = len(p);
        if refs(p) > 1 || n + b.len() as u32 > cap(p) {
            let a = rd(p, n);
            concat_new(s, &a, b);
            release_data(p);
        } else {
            wr(p + n * unit(), b);
            set_len(p, n + b.len() as u32);
        }
    }
    /// GetBufferSetLength / GetBuffer(n): an own buffer of at least `n` characters
    pub fn get_buffer(s: u32, n: u32) -> u32 {
        let p = r32(s);
        if refs(p) > 1 || n > cap(p) {
            let old = rd(p, len(p));
            release(s);
            alloc_buffer(s, n.max(old.len() as u32));
            let q = r32(s);
            if q != nil() {
                wr(q, &old);
                set_len(q, old.len() as u32);
            }
        }
        r32(s)
    }
    /// ReleaseBuffer(n): the length (-1: up to the NUL)
    pub fn release_buffer(s: u32, n: i32) {
        copy_before_write(s);
        let p = r32(s);
        if p == nil() {
            return;
        }
        let n = if n < 0 { sz(p).len() as u32 } else { n as u32 };
        set_len(p, n);
    }
    /// _istspace for TrimLeft / TrimRight (the C locale: tab..CR and space; iswspace's
    /// Unicode spaces for a Unicode build)
    pub fn is_space(u: u16) -> bool {
        u == 0x20 || (9..=13).contains(&u) || (wide() && matches!(u, 0x85 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000))
    }
    /// TrimLeft's and TrimRight's result for `b`, double-byte aware (Shift-JIS lead bytes keep
    /// their trail byte: _tcsinc)
    pub fn trim_left(b: &[u16]) -> &[u16] {
        let i = b.iter().position(|&x| !is_space(x)).unwrap_or(b.len());
        &b[i..]
    }
    pub fn trim_right(b: &[u16]) -> &[u16] {
        // (the last run of spaces that starts at a character boundary)
        let mut i = 0;
        let mut cut = None;
        while i < b.len() {
            if is_space(b[i]) {
                cut.get_or_insert(i);
                i += 1;
            } else {
                cut = None;
                i += if !wide() && crate::text::is_lead(b[i] as u8) && i + 1 < b.len() { 2 } else { 1 };
            }
        }
        &b[..cut.unwrap_or(b.len())]
    }
    pub fn set(s: u32, b: &[u16]) {
        if b.is_empty() {
            release(s);
        } else {
            assign(s, b);
        }
    }
}
use cstring as cs;

fn cs_ctor(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::init(s);
    s
}
fn cs_ctor_copy(c: &mut Ctx) -> u32 {
    let (s, src) = (this(c), c.a(0));
    let q = r32(src);
    if cs::refs(q) >= 0 {
        w32(s, q);
        w32(q - 12, (cs::refs(q) + 1) as u32);
    } else {
        cs::init(s);
        let b = cs::bytes(src);
        cs::assign(s, &b);
    }
    s
}
/// An `LPCSTR` (W false) or `LPCWSTR` (W true) argument as text
fn arg<const W: bool>(p: u32) -> Vec<u16> {
    if W { wsz(p) } else { asz(p) }
}
/// A `char` (W false) or `unsigned short` (W true) argument as text
fn chr<const W: bool>(v: u32) -> Vec<u16> {
    if W { wch(v) } else { ach(v) }
}
fn cs_ctor_sz<const W: bool>(c: &mut Ctx) -> u32 {
    let (s, p) = (this(c), c.a(0));
    cs::init(s);
    if p != 0 && p < 0x10000 {
        load_string(c, s, p);
    } else if p != 0 {
        cs::assign(s, &arg::<W>(p));
    }
    s
}
fn cs_ctor_ch<const W: bool>(c: &mut Ctx) -> u32 {
    let (s, ch, n) = (this(c), chr::<W>(c.a(0)), c.i(1).max(0) as usize);
    cs::init(s);
    cs::assign(s, &ch.repeat(n));
    s
}
fn cs_ctor_sz_n<const W: bool>(c: &mut Ctx) -> u32 {
    let (s, p, n) = (this(c), c.a(0), c.i(1).max(0) as u32);
    cs::init(s);
    let t = if W { from_w(&(0..n).map(|i| r16(p + 2 * i)).collect::<Vec<_>>()) } else { from_a(mem::slice(p, n)) };
    cs::assign(s, &t);
    s
}
fn cs_dtor(c: &mut Ctx) -> u32 {
    cs::release(this(c));
    0
}
fn cs_assign_string(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::assign_string(s, c.a(0));
    s
}
fn cs_assign_sz<const W: bool>(c: &mut Ctx) -> u32 {
    let (s, p) = (this(c), c.a(0));
    cs::assign(s, &arg::<W>(p));
    s
}
fn cs_assign_ch<const W: bool>(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::assign(s, &chr::<W>(c.a(0)));
    s
}
fn cs_append_string(c: &mut Ctx) -> u32 {
    let s = this(c);
    let b = cs::bytes(c.a(0));
    cs::append(s, &b);
    s
}
fn cs_append_sz<const W: bool>(c: &mut Ctx) -> u32 {
    let (s, p) = (this(c), c.a(0));
    cs::append(s, &arg::<W>(p));
    s
}
fn cs_append_ch<const W: bool>(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::append(s, &chr::<W>(c.a(0)));
    s
}
/// operator+(...) (stdcall, the result through the hidden first argument)
fn cs_plus_ss(c: &mut Ctx) -> u32 {
    let (r, a, b) = (c.a(0), cs::bytes(c.a(1)), cs::bytes(c.a(2)));
    cs::concat_new(r, &a, &b);
    r
}
fn cs_plus_sp<const W: bool>(c: &mut Ctx) -> u32 {
    let (r, a, b) = (c.a(0), cs::bytes(c.a(1)), arg::<W>(c.a(2)));
    cs::concat_new(r, &a, &b);
    r
}
fn cs_plus_ps<const W: bool>(c: &mut Ctx) -> u32 {
    let (r, a, b) = (c.a(0), arg::<W>(c.a(1)), cs::bytes(c.a(2)));
    cs::concat_new(r, &a, &b);
    r
}
fn cs_plus_sc<const W: bool>(c: &mut Ctx) -> u32 {
    let (r, a, b) = (c.a(0), cs::bytes(c.a(1)), chr::<W>(c.a(2)));
    cs::concat_new(r, &a, &b);
    r
}
fn cs_plus_cs<const W: bool>(c: &mut Ctx) -> u32 {
    let (r, a, b) = (c.a(0), chr::<W>(c.a(1)), cs::bytes(c.a(2)));
    cs::concat_new(r, &a, &b);
    r
}
fn cs_empty(c: &mut Ctx) -> u32 {
    let s = this(c);
    let p = r32(s);
    if cs::len(p) == 0 {
        return 0;
    }
    if cs::refs(p) >= 0 {
        cs::release(s);
    } else {
        cs::assign(s, &[]);
    }
    0
}
fn cs_trim_left(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::copy_before_write(s);
    let b = cs::bytes(s);
    let t = cs::trim_left(&b).to_vec();
    if t.len() != b.len() {
        let p = r32(s);
        wr(p, &t);
        cs::set_len(p, t.len() as u32);
    }
    0
}
fn cs_trim_right(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::copy_before_write(s);
    let b = cs::bytes(s);
    let n = cs::trim_right(&b).len() as u32;
    if n != b.len() as u32 {
        cs::set_len(r32(s), n);
    }
    0
}
fn cs_get_buffer(c: &mut Ctx) -> u32 {
    cs::get_buffer(this(c), c.i(0).max(0) as u32)
}
fn cs_get_buffer_set_length(c: &mut Ctx) -> u32 {
    let (s, n) = (this(c), c.i(0).max(0) as u32);
    let p = cs::get_buffer(s, n);
    if p != cs::nil() {
        cs::set_len(p, n);
    }
    p
}
fn cs_release_buffer(c: &mut Ctx) -> u32 {
    cs::release_buffer(this(c), c.i(0));
    0
}
/// Find(ch) / ReverseFind(ch) (a TCHAR: the `D` or `G` parameter of the DLL's own build)
fn cs_find_ch(c: &mut Ctx) -> u32 {
    let (b, ch) = (cs::bytes(this(c)), c.a(0) as u16 & if wide() { 0xffff } else { 0xff });
    b.iter().position(|&x| x == ch).map_or(u32::MAX, |i| i as u32)
}
fn cs_find_sz(c: &mut Ctx) -> u32 {
    let (b, n) = (cs::bytes(this(c)), sz(c.a(0)));
    if n.is_empty() {
        return 0;
    }
    b.windows(n.len()).position(|w| w == n.as_slice()).map_or(u32::MAX, |i| i as u32)
}
fn cs_reverse_find(c: &mut Ctx) -> u32 {
    let (b, ch) = (cs::bytes(this(c)), c.a(0) as u16 & if wide() { 0xffff } else { 0xff });
    b.iter().rposition(|&x| x == ch).map_or(u32::MAX, |i| i as u32)
}
/// _tcsupr / _tcslwr: ASCII letters (the C locale; a double-byte character's trail byte kept)
fn change_case(b: &mut [u16], upper: bool) {
    let mut i = 0;
    while i < b.len() {
        let u = b[i];
        if !wide() && crate::text::is_lead(u as u8) {
            i += 2;
            continue;
        }
        if upper && (0x61..=0x7a).contains(&u) {
            b[i] = u - 0x20;
        } else if !upper && (0x41..=0x5a).contains(&u) {
            b[i] = u + 0x20;
        }
        i += 1;
    }
}
fn cs_make_upper(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::copy_before_write(s);
    let mut b = cs::bytes(s);
    change_case(&mut b, true);
    wr(r32(s), &b);
    0
}
fn cs_make_lower(c: &mut Ctx) -> u32 {
    let s = this(c);
    cs::copy_before_write(s);
    let mut b = cs::bytes(s);
    change_case(&mut b, false);
    wr(r32(s), &b);
    0
}
/// Mid(first, count) / Left(count) / Right(count): a new CString through the hidden argument
fn sub(b: &[u16], first: i32, count: i32) -> Vec<u16> {
    let first = first.clamp(0, b.len() as i32) as usize;
    let count = count.clamp(0, (b.len() - first) as i32) as usize;
    b[first..first + count].to_vec()
}
fn cs_mid2(c: &mut Ctx) -> u32 {
    let (b, r) = (cs::bytes(this(c)), c.a(0));
    cs::init(r);
    cs::assign(r, &sub(&b, c.i(1), c.i(2)));
    r
}
fn cs_mid1(c: &mut Ctx) -> u32 {
    let (b, r) = (cs::bytes(this(c)), c.a(0));
    cs::init(r);
    cs::assign(r, &sub(&b, c.i(1), i32::MAX));
    r
}
fn cs_left(c: &mut Ctx) -> u32 {
    let (b, r) = (cs::bytes(this(c)), c.a(0));
    cs::init(r);
    cs::assign(r, &sub(&b, 0, c.i(1)));
    r
}
fn cs_right(c: &mut Ctx) -> u32 {
    let (b, r) = (cs::bytes(this(c)), c.a(0));
    let n = c.i(1).clamp(0, b.len() as i32);
    cs::init(r);
    cs::assign(r, &sub(&b, b.len() as i32 - n, n));
    r
}
/// Format(fmt, ...) (cdecl, `this` the first stack argument)
fn cs_format(c: &mut Ctx) -> u32 {
    let (s, f) = (c.a(0), c.a(1));
    let fmt = if f != 0 && f < 0x10000 {
        let t = Tmp::new(4);
        cs::init(t.0);
        load_string(c, t.0, f);
        let p = r32(t.0);
        let v = vsprintf(p, c.esp + 12);
        cs::release(t.0);
        v
    } else {
        vsprintf(f, c.esp + 12)
    };
    cs::set(s, &fmt.unwrap_or_default());
    0
}
/// _vstprintf's text
fn vsprintf(fmt: u32, va: u32) -> Option<Vec<u16>> {
    if wide() { crate::crt::vsprintf_w(fmt, va) } else { crate::crt::vsprintf_a(fmt, va).map(|b| b.into_iter().map(|b| b as u16).collect()) }
}
/// LoadString(id): the string resource of the program (or a DLL of its resources)
fn load_string(c: &mut Ctx, s: u32, id: u32) -> u32 {
    let buf = Tmp::new(1024 * unit());
    let inst = resource_handle(c);
    let n = user(c, "LoadStringA", &[inst, id, buf.0, 1024]);
    cs::set(s, &rd(buf.0, n));
    (n != 0) as u32
}
fn cs_load_string(c: &mut Ctx) -> u32 {
    let (s, id) = (this(c), c.a(0));
    load_string(c, s, id)
}

// ---------------------------------------------------------------------------------------------
// module and thread state

/// AFX_MODULE_STATE (the program's: m_pCurrentWinApp +4, m_hCurrentInstanceHandle +8,
/// m_hCurrentResourceHandle +0xc, m_lpszCurrentAppName +0x10, m_bDLL +0x14, ...,
/// m_pfnAfxWndProc +0x103c, m_dwVersion +0x1040)
pub fn module_state() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| {
        let m = mem::heap_alloc(0x1100, true);
        w32(m + 0x103c, afx_wnd_proc());
        w32(m + 0x1040, 0x600);
        m
    })
}
fn app() -> u32 {
    r32(module_state() + 4)
}
fn resource_handle(c: &mut Ctx) -> u32 {
    let h = r32(module_state() + 0xc);
    if h != 0 {
        return h;
    }
    let _ = c;
    crate::k32::module_handle(0, false)
}

/// _AFX_THREAD_STATE's part sake keeps on the host side
#[derive(Clone, Copy, Default)]
struct Thread {
    /// m_pCurrentWinThread (AFX_MODULE_THREAD_STATE)
    current: u32,
    /// m_lastSentMsg: hwnd, message, wParam, lParam
    last: [u32; 4],
    /// m_pWndInit: the CWnd a window being created is attached to
    wnd_init: u32,
    /// m_hHookOldCbtFilter
    hook: u32,
    /// m_hLockoutNotifyWindow
    lockout: u32,
}
static THREADS: Mutex<Option<HashMap<u32, Thread>>> = Mutex::new(None);
fn ts() -> Thread {
    let tid = crate::cur().tid;
    THREADS.lock().unwrap().get_or_insert_with(HashMap::default).get(&tid).copied().unwrap_or_default()
}
fn ts_set(f: impl FnOnce(&mut Thread)) {
    let tid = crate::cur().tid;
    f(THREADS.lock().unwrap().get_or_insert_with(HashMap::default).entry(tid).or_default());
}

/// AfxGetThread: the thread's CWinThread, else the application
fn get_thread() -> u32 {
    match ts().current {
        0 => app(),
        t => t,
    }
}
/// AfxGetMainWnd: the thread's active or main window (CWinThread::GetMainWnd)
fn main_wnd() -> u32 {
    let t = get_thread();
    if t == 0 {
        return 0;
    }
    match r32(t + 0x24) {
        0 => r32(t + 0x20),
        a => a,
    }
}

fn AfxGetModuleState(_: &mut Ctx) -> u32 {
    module_state()
}
fn AfxGetThread(_: &mut Ctx) -> u32 {
    get_thread()
}
fn AfxFindResourceHandle(c: &mut Ctx) -> u32 {
    resource_handle(c)
}
fn AfxPostQuitMessage(c: &mut Ctx) -> u32 {
    let n = c.a(0);
    user(c, "PostQuitMessage", &[n]);
    0
}

// ---------------------------------------------------------------------------------------------
// run-time classes

/// (class, base, size) of the classes MFC 4.2 declares that programs meet most
const CLASSES: &[(&str, &str, u32)] = &[
    ("CObject", "", 4),
    ("CCmdTarget", "CObject", 0x20),
    ("CWinThread", "CCmdTarget", 0x6c),
    ("CWinApp", "CWinThread", 0xc4),
    ("CWnd", "CCmdTarget", 0x40),
    ("CDialog", "CWnd", 0x60),
    ("CCommonDialog", "CDialog", 0x60),
    ("CFileDialog", "CCommonDialog", 0),
    ("CPropertyPage", "CDialog", 0),
    ("CPropertySheet", "CWnd", 0),
    ("CFrameWnd", "CWnd", 0),
    ("CView", "CWnd", 0),
    ("CDocument", "CCmdTarget", 0),
    ("CButton", "CWnd", 0x40),
    ("CEdit", "CWnd", 0x40),
    ("CStatic", "CWnd", 0x40),
    ("CListBox", "CWnd", 0x40),
    ("CComboBox", "CWnd", 0x40),
    ("CScrollBar", "CWnd", 0x40),
    ("CProgressCtrl", "CWnd", 0x40),
    ("CSliderCtrl", "CWnd", 0x40),
    ("CSpinButtonCtrl", "CWnd", 0x40),
    ("CListCtrl", "CWnd", 0x40),
    ("CTreeCtrl", "CWnd", 0x40),
    ("CTabCtrl", "CWnd", 0x40),
    ("CAnimateCtrl", "CWnd", 0x40),
    ("CFile", "CObject", 0x10),
    ("CStdioFile", "CFile", 0x14),
    ("CMemFile", "CFile", 0),
    ("CException", "CObject", 8),
    ("CFileException", "CException", 0x14),
    ("CMemoryException", "CException", 8),
    ("CNotSupportedException", "CException", 8),
    ("CResourceException", "CException", 8),
    ("CUserException", "CException", 8),
    ("CArchiveException", "CException", 0x10),
    ("CDC", "CObject", 0x10),
    ("CPaintDC", "CDC", 0x54),
    ("CClientDC", "CDC", 0x14),
    ("CWindowDC", "CDC", 0x14),
    ("CGdiObject", "CObject", 8),
    ("CPen", "CGdiObject", 8),
    ("CBrush", "CGdiObject", 8),
    ("CFont", "CGdiObject", 8),
    ("CBitmap", "CGdiObject", 8),
    ("CPalette", "CGdiObject", 8),
    ("CRgn", "CGdiObject", 8),
    ("CMenu", "CObject", 8),
];

static RTC: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
static BASE_FNS: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

/// The CRuntimeClass of `name` {m_lpszClassName, m_nObjectSize, m_wSchema 0xffff,
/// m_pfnCreateObject, m_pfnGetBaseClass (the DLL build's), m_pNextClass}
pub fn runtime_class(name: &str) -> u32 {
    if let Some(&p) = RTC.lock().unwrap().get_or_insert_with(HashMap::default).get(name) {
        return p;
    }
    let (base, mut size) = CLASSES.iter().find(|x| x.0 == name).map_or(("CObject", 0), |x| (x.1, x.2));
    if name == "CWinThread" {
        size = th(size);
    }
    let p = mem::heap_alloc(24 + name.len() as u32 + 1, true);
    mem::write(p + 24, name.as_bytes());
    w32(p, p + 24);
    w32(p + 4, size);
    w32(p + 8, 0xffff);
    if name != "CObject" {
        let f = api::host_fn(&format!("mfc42 {name}::_GetBaseClass"), get_base_class, 0);
        BASE_FNS.lock().unwrap().get_or_insert_with(HashMap::default).insert(f, base.to_string());
        w32(p + 16, f);
    }
    RTC.lock().unwrap().get_or_insert_with(HashMap::default).insert(name.to_string(), p);
    p
}
fn get_base_class(c: &mut Ctx) -> u32 {
    let base = BASE_FNS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).cloned()).unwrap_or_default();
    runtime_class(&base)
}
/// GetRuntimeClass of any class (its name in the export's)
fn GetRuntimeClass(c: &mut Ctx) -> u32 {
    let n = api::name_of(c.trap).unwrap_or_default();
    let cls = n.split_once("?GetRuntimeClass@").and_then(|(_, r)| r.split_once("@@")).map_or("CObject", |x| x.0).to_string();
    runtime_class(&cls)
}
/// CObject::IsKindOf: the object's class or one of its bases is `rtc`
fn is_kind_of(c: &mut Ctx, obj: u32, rtc: u32) -> bool {
    let mut k = vcall(c, obj, 0, &[]);
    while k != 0 {
        if k == rtc {
            return true;
        }
        let f = r32(k + 16);
        k = if f == 0 { 0 } else { c.call(f, &[]) };
    }
    false
}
fn IsKindOf(c: &mut Ctx) -> u32 {
    let (o, k) = (this(c), c.a(0));
    is_kind_of(c, o, k) as u32
}

// ---------------------------------------------------------------------------------------------
// vtables

/// The vtable slots of MFC's classes: export names, "~" the scalar deleting destructor, "-N"
/// a function doing nothing that takes N bytes of arguments.
fn slots(class: &str) -> Vec<&'static str> {
    let obj = ["", "~", "-4", "?AssertValid@CObject@@UBEXXZ", "?Dump@CObject@@UBEXAAVCDumpContext@@@Z"];
    let target = [
        "?OnCmdMsg@CCmdTarget@@UAEHIHPAXPAUAFX_CMDHANDLERINFO@@@Z",
        "?OnFinalRelease@CCmdTarget@@UAEXXZ",
        "?IsInvokeAllowed@CCmdTarget@@UAEHJ@Z",
        "?GetDispatchIID@CCmdTarget@@UAEHPAU_GUID@@@Z",
        "?GetTypeInfoCount@CCmdTarget@@UAEIXZ",
        "?GetTypeLibCache@CCmdTarget@@UAEPAVCTypeLibCache@@XZ",
        "?GetTypeLib@CCmdTarget@@UAEJKPAPAUITypeLib@@@Z",
        "?GetMessageMap@CCmdTarget@@MBEPBUAFX_MSGMAP@@XZ",
        "?GetCommandMap@CCmdTarget@@MBEPBUAFX_OLECMDMAP@@XZ",
        "?GetDispatchMap@CCmdTarget@@MBEPBUAFX_DISPMAP@@XZ",
        "?GetConnectionMap@CCmdTarget@@MBEPBUAFX_CONNECTIONMAP@@XZ",
        "?GetInterfaceMap@CCmdTarget@@MBEPBUAFX_INTERFACEMAP@@XZ",
        "?GetEventSinkMap@CCmdTarget@@MBEPBUAFX_EVENTSINKMAP@@XZ",
        "?OnCreateAggregates@CCmdTarget@@UAEHXZ",
        "?GetInterfaceHook@CCmdTarget@@UAEPAUIUnknown@@PBX@Z",
        "?GetExtraConnectionPoints@CCmdTarget@@MAEHPAVCPtrArray@@@Z",
        "?GetConnectionHook@CCmdTarget@@MAEPAUIConnectionPoint@@ABU_GUID@@@Z",
    ];
    let thread = [
        "?InitInstance@CWinThread@@UAEHXZ",
        "?Run@CWinThread@@UAEHXZ",
        "?PreTranslateMessage@CWinThread@@UAEHPAUtagMSG@@@Z",
        "?PumpMessage@CWinThread@@UAEHXZ",
        "?OnIdle@CWinThread@@UAEHJ@Z",
        "?IsIdleMessage@CWinThread@@UAEHPAUtagMSG@@@Z",
        "?ExitInstance@CWinThread@@UAEHXZ",
        "?ProcessWndProcException@CWinThread@@UAEJPAVCException@@PBUtagMSG@@@Z",
        "?ProcessMessageFilter@CWinThread@@UAEHHPAUtagMSG@@@Z",
        "?GetMainWnd@CWinThread@@UAEPAVCWnd@@XZ",
        "?Delete@CWinThread@@UAEXXZ",
    ];
    let wnd = [
        "?PreSubclassWindow@CWnd@@UAEXXZ",
        "?Create@CWnd@@UAEHPBD0KABUtagRECT@@PAV1@IPAUCCreateContext@@@Z",
        "?DestroyWindow@CWnd@@UAEHXZ",
        "?PreCreateWindow@CWnd@@UAEHAAUtagCREATESTRUCTA@@@Z",
        "?CalcWindowRect@CWnd@@UAEXPAUtagRECT@@I@Z",
        "?OnToolHitTest@CWnd@@UBEHVCPoint@@PAUtagTOOLINFOA@@@Z",
        "?GetScrollBarCtrl@CWnd@@UBEPAVCScrollBar@@H@Z",
        "?WinHelpA@CWnd@@UAEXKI@Z",
        "?ContinueModal@CWnd@@UAEHXZ",
        "?EndModalLoop@CWnd@@UAEXH@Z",
        "?OnCommand@CWnd@@MAEHIJ@Z",
        "?OnNotify@CWnd@@MAEHIJPAJ@Z",
        "?GetSuperWndProcAddr@CWnd@@MAEPAP6GJPAUHWND__@@IIJ@ZXZ",
        "-4",
        "?BeginModalState@CWnd@@UAEXXZ",
        "?EndModalState@CWnd@@UAEXXZ",
        "?PreTranslateMessage@CWnd@@UAEHPAUtagMSG@@@Z",
        "?OnAmbientProperty@CWnd@@UAEHPAVCOleControlSite@@JPAUtagVARIANT@@@Z",
        "?WindowProc@CWnd@@MAEJIIJ@Z",
        "?OnWndMsg@CWnd@@MAEHIIJPAJ@Z",
        "?DefWindowProcA@CWnd@@MAEJIIJ@Z",
        "?PostNcDestroy@CWnd@@MAEXXZ",
        "?OnChildNotify@CWnd@@MAEHIIJPAJ@Z",
        "?CheckAutoCenter@CWnd@@UAEHXZ",
        "?IsFrameWnd@CWnd@@UBEHXZ",
        "?SetOccDialogInfo@CWnd@@MAEHPAU_AFX_OCC_DIALOG_INFO@@@Z",
    ];
    let dialog = [
        "?DoModal@CDialog@@UAEHXZ",
        "?OnInitDialog@CDialog@@UAEHXZ",
        "?OnSetFont@CDialog@@UAEXPAVCFont@@@Z",
        "?OnOK@CDialog@@MAEXXZ",
        "?OnCancel@CDialog@@MAEXXZ",
        "?PreInitDialog@CDialog@@MAEXXZ",
    ];
    let file = [
        "?GetPosition@CFile@@UBEKXZ",
        "?GetFileName@CFile@@UBE?AVCString@@XZ",
        "?GetFileTitle@CFile@@UBE?AVCString@@XZ",
        "?GetFilePath@CFile@@UBE?AVCString@@XZ",
        "?SetFilePath@CFile@@UAEXPBD@Z",
        "?Open@CFile@@UAEHPBDIPAVCFileException@@@Z",
        "?Duplicate@CFile@@UBEPAV1@XZ",
        "?Seek@CFile@@UAEJJI@Z",
        "?SetLength@CFile@@UAEXK@Z",
        "?GetLength@CFile@@UBEKXZ",
        "?Read@CFile@@UAEIPAXI@Z",
        "?Write@CFile@@UAEXPBXI@Z",
        "?LockRange@CFile@@UAEXKK@Z",
        "?UnlockRange@CFile@@UAEXKK@Z",
        "?Abort@CFile@@UAEXXZ",
        "?Flush@CFile@@UAEXXZ",
        "?Close@CFile@@UAEXXZ",
    ];
    let mut v: Vec<&'static str> = obj.to_vec();
    let fam = |c: &str| -> &'static str {
        match c {
            "CCmdTarget" | "CWinThread" | "CWinApp" => "target",
            "CDialog" => "dialog",
            "CFile" => "file",
            "CException" | "CFileException" => "exception",
            "CDC" | "CPaintDC" | "CClientDC" | "CWindowDC" | "CMenu" | "CGdiObject" | "CFont" | "CObject" => "object",
            _ => "wnd",
        }
    };
    match fam(class) {
        "object" => {}
        "file" => v.extend(file),
        "exception" => {
            v.push(if class == "CFileException" { "?GetErrorMessage@CFileException@@UAEHPADIPAI@Z" } else { "?GetErrorMessage@CException@@UAEHPADIPAI@Z" });
            v.push("?ReportError@CException@@UAEHII@Z");
        }
        f => {
            v.extend(target);
            if f == "target" {
                if class != "CCmdTarget" {
                    v.extend(thread);
                }
                if class == "CWinApp" {
                    v[12] = "?GetMessageMap@CWinApp@@MBEPBUAFX_MSGMAP@@XZ";
                    v[23] = "?Run@CWinApp@@UAEHXZ";
                    v[26] = "?OnIdle@CWinApp@@UAEHJ@Z";
                    v[28] = "?ExitInstance@CWinApp@@UAEHXZ";
                    v[29] = "?ProcessWndProcException@CWinApp@@UAEJPAVCException@@PBUtagMSG@@@Z";
                    v.extend([
                        "?OpenDocumentFile@CWinApp@@UAEPAVCDocument@@PBD@Z",
                        "?AddToRecentFileList@CWinApp@@UAEXPBD@Z",
                        "?InitApplication@CWinApp@@UAEHXZ",
                        "?SaveAllModified@CWinApp@@UAEHXZ",
                        "?DoMessageBox@CWinApp@@UAEHPBDII@Z",
                        "?DoWaitCursor@CWinApp@@UAEXH@Z",
                        "?OnDDECommand@CWinApp@@UAEHPAD@Z",
                        "?WinHelpA@CWinApp@@UAEXKI@Z",
                    ]);
                }
            } else {
                v[6] = "?OnFinalRelease@CWnd@@UAEXXZ";
                v[12] = "?GetMessageMap@CWnd@@MBEPBUAFX_MSGMAP@@XZ";
                v.extend(wnd);
                if f == "dialog" {
                    v[5] = "?OnCmdMsg@CDialog@@UAEHIHPAXPAUAFX_CMDHANDLERINFO@@@Z";
                    v[12] = "?GetMessageMap@CDialog@@MBEPBUAFX_MSGMAP@@XZ";
                    v[38] = "?PreTranslateMessage@CDialog@@UAEHPAUtagMSG@@@Z";
                    v[45] = "?CheckAutoCenter@CDialog@@UAEHXZ";
                    v[47] = "?SetOccDialogInfo@CDialog@@MAEHPAU_AFX_OCC_DIALOG_INFO@@@Z";
                    v.extend(dialog);
                }
            }
            // (MFC 4.0's CCmdTarget: no GetCommandMap)
            if mfc40() {
                v.remove(13);
            }
        }
    }
    v
}
static VTABLES: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
static DTORS: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);
/// MFC's vtable of `class` (made on first use)
/// The trap of MFC export `n` (an ANSI name) of the DLL in use: MFC42U's own name for a TCHAR one
fn export(n: &str) -> u32 {
    if wide() && !api::exports("mfc42u", n) {
        return api::export("mfc42u", &unicode_name(n));
    }
    // (one MFC 4.0 lacks or names otherwise: MFC 4.2's, the same handler)
    if mfc40() && !api::exports("mfc40", n) {
        return api::export("mfc42", n);
    }
    api::export(dll(), n)
}
pub fn vtable(class: &str) -> u32 {
    let key = format!("{} {class}", dll());
    if let Some(&v) = VTABLES.lock().unwrap().get_or_insert_with(HashMap::default).get(&key) {
        return v;
    }
    let s = slots(class);
    let v = mem::heap_alloc(4 * s.len() as u32, true);
    for (i, n) in s.iter().enumerate() {
        let f = match *n {
            "" => export(&format!("?GetRuntimeClass@{class}@@UBEPAUCRuntimeClass@@XZ")),
            "~" => {
                let f = api::host_fn(&format!("mfc42 {class}::`scalar deleting destructor'"), scalar_deleting_dtor, 4);
                DTORS.lock().unwrap().get_or_insert_with(HashMap::default).insert(f, class.to_string());
                f
            }
            n if n.starts_with('-') => api::host_fn(&format!("mfc42 nop{n}"), nop, n[1..].parse().unwrap_or(0)),
            n => export(n),
        };
        w32(v + 4 * i as u32, f);
    }
    VTABLES.lock().unwrap().get_or_insert_with(HashMap::default).insert(key, v);
    v
}
fn nop(_: &mut Ctx) -> u32 {
    0
}
fn ret1(_: &mut Ctx) -> u32 {
    1
}
fn ret_this(c: &mut Ctx) -> u32 {
    this(c)
}
fn ret_minus1(_: &mut Ctx) -> u32 {
    u32::MAX
}
fn scalar_deleting_dtor(c: &mut Ctx) -> u32 {
    let (o, flags) = (this(c), c.a(0));
    let class = DTORS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).cloned()).unwrap_or_default();
    destruct(c, &class, o);
    if flags & 1 != 0 {
        free(o);
    }
    o
}
/// The destructor of MFC's class `class` on `o`
fn destruct(c: &mut Ctx, class: &str, o: u32) {
    match class {
        "CWinApp" => app_dtor(o),
        "CWinThread" => thread_dtor(c, o),
        "CDialog" => wnd_dtor(c, o),
        "CFile" => file_dtor(c, o),
        "CFileException" => cs::release(o + 0x10),
        "CDC" | "CPaintDC" | "CClientDC" | "CWindowDC" => {}
        "CObject" | "CCmdTarget" | "CException" | "CMenu" | "CGdiObject" | "CFont" => {}
        _ => wnd_dtor(c, o),
    }
}

/// CCmdTarget's constructor's fields: m_dwRef 1, m_bResultExpected 1, m_pModuleState
fn cmd_target_init(o: u32, class: &str) {
    w32(o, vtable(class));
    w32(o + 4, 1);
    for off in [8, 0xc, 0x10, 0x18] {
        w32(o + off, 0);
    }
    w32(o + 0x14, 1);
    w32(o + 0x1c, module_state());
}
fn CCmdTarget_ctor(c: &mut Ctx) -> u32 {
    let o = this(c);
    cmd_target_init(o, "CCmdTarget");
    o
}

// ---------------------------------------------------------------------------------------------
// message maps

/// AFX_MSGMAP {pfnGetBaseMap, lpEntries} with AFX_MSGMAP_ENTRY {nMessage, nCode, nID, nLastID,
/// nSig, pfn} (24 bytes, nSig 0 ends them)
struct Maps {
    target: u32,
    wnd: u32,
    dialog: u32,
    app: u32,
    /// CCmdTarget::commandMap and its other (OLE) maps: empty
    empty: u32,
}
fn maps() -> &'static Maps {
    static M: OnceLock<Maps> = OnceLock::new();
    M.get_or_init(|| {
        let empty = mem::heap_alloc(32, true);
        let make = |base: Handler, name: &str, entries: &[[u32; 6]]| {
            let e = mem::heap_alloc(24 * (entries.len() as u32 + 1), true);
            for (i, x) in entries.iter().enumerate() {
                for (k, &v) in x.iter().enumerate() {
                    w32(e + 24 * i as u32 + 4 * k as u32, v);
                }
            }
            let m = mem::heap_alloc(8, true);
            if name != "CCmdTarget" {
                w32(m, api::host_fn(&format!("mfc42 {name}::_GetBaseMessageMap"), base, 0));
            }
            w32(m + 4, e);
            m
        };
        let target = make(nop, "CCmdTarget", &[]);
        let wnd = make(base_target, "CWnd", &[[0x82, 0, 0, 0, 12, api::host_fn("mfc42 CWnd::OnNcDestroy", OnNcDestroy, 0)]]);
        let vthunk = |slot: u32, f: Handler| api::host_fn(&format!("mfc42 vcall {slot}"), f, 0);
        let dialog = make(
            base_wnd,
            "CDialog",
            &[
                [0x111, 0, 1, 1, 12, vthunk(51, vcall51)],
                [0x111, 0, 2, 2, 12, vthunk(52, vcall52)],
                [0x110, 0, 0, 0, 10, api::host_fn("mfc42 CDialog::HandleInitDialog", HandleInitDialog, 8)],
                [0x30, 0, 0, 0, 10, api::host_fn("mfc42 CDialog::HandleSetFont", HandleSetFont, 8)],
            ],
        );
        let app = make(base_target, "CWinApp", &[]);
        Maps { target, wnd, dialog, app, empty }
    })
}
fn base_target(_: &mut Ctx) -> u32 {
    maps().target
}
fn base_wnd(_: &mut Ctx) -> u32 {
    maps().wnd
}
fn vcall51(c: &mut Ctx) -> u32 {
    let o = this(c);
    vcall(c, o, 51, &[])
}
fn vcall52(c: &mut Ctx) -> u32 {
    let o = this(c);
    vcall(c, o, 52, &[])
}
fn GetMessageMap_CCmdTarget(_: &mut Ctx) -> u32 {
    maps().target
}
fn GetMessageMap_CWnd(_: &mut Ctx) -> u32 {
    maps().wnd
}
fn GetMessageMap_CDialog(_: &mut Ctx) -> u32 {
    maps().dialog
}
fn GetMessageMap_CWinApp(_: &mut Ctx) -> u32 {
    maps().app
}
fn empty_map(_: &mut Ctx) -> u32 {
    maps().empty
}

/// AfxFindMessageEntry: the entry for (message, code, id) in `entries`
pub fn find_entry(entries: u32, msg: u32, code: u32, id: u32) -> Option<u32> {
    let mut e = entries;
    loop {
        let sig = r32(e + 16);
        if sig == 0 {
            return None;
        }
        if r32(e) == msg && r32(e + 4) == code && id >= r32(e + 8) && id <= r32(e + 12) {
            return Some(e);
        }
        e += 24;
    }
}
/// The registered-message entry (nMessage 0xC000, nSig points at the message's number)
fn find_registered(entries: u32, msg: u32) -> Option<u32> {
    let mut e = entries;
    loop {
        let sig = r32(e + 16);
        if sig == 0 {
            return None;
        }
        if r32(e) == 0xc000 && sig > 0x10000 && r32(sig) == msg {
            return Some(e);
        }
        e += 24;
    }
}
/// The message map chain of `obj` (GetMessageMap, then each pfnGetBaseMap)
fn map_chain(c: &mut Ctx, obj: u32) -> Vec<u32> {
    let mut v = Vec::new();
    let mut m = vcall(c, obj, 12, &[]);
    while m != 0 && v.len() < 64 {
        v.push(m);
        let f = r32(m);
        m = if f == 0 { 0 } else { c.call(f, &[]) };
    }
    v
}

/// How OnWndMsg calls a handler of signature nSig (AfxSig_*) with wParam / lParam
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum A {
    W,
    L,
    LoW,
    HiW,
    SLoW,
    SHiW,
    LoL,
    HiL,
    SLoL,
    SHiL,
    /// CWnd::FromHandle(wParam) / (lParam)
    WndW,
    WndL,
    /// CDC::FromHandle(wParam)
    DcW,
    /// CMenu::FromHandle(wParam) / (lParam)
    MenuW,
    MenuL,
    /// a CPoint by value from lParam (two dwords: x, y as signed words)
    PtL,
    /// m_hWnd == lParam
    SelfL,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum R {
    /// lResult = the handler's result
    Store,
    /// lResult stays 0
    Void,
    /// lResult = 1
    One,
    /// the handler's result; 0 is "not handled"
    NonZero,
}
/// The arguments and result of OnWndMsg's AfxSig cases (mfc42.dll 6.06's jump table;
/// AfxSig_hDWw and AfxSig_hDw, the WM_CTLCOLOR ones, are OnWndMsg's own)
pub fn sig_plan(sig: u32) -> Option<(&'static [A], R)> {
    use A::*;
    Some(match sig {
        1 => (&[DcW], R::Store),
        2 => (&[W], R::Store),
        3 | 8 => (&[WndW, SLoL, HiL], R::Store),
        6 => (&[LoW, WndL, HiW], R::Store),
        7 => (&[LoW, HiW], R::Store),
        9 | 42 => (&[L], R::Store),
        10 | 33 => (&[W, L], R::Store),
        11 => (&[LoW, HiW, MenuL], R::Store),
        12 => (&[], R::Void),
        13 => (&[W], R::Void),
        14 | 18 | 37 | 47 => (&[W, L], R::Void),
        15 => (&[SLoL, SHiL], R::Void),
        16 | 17 => (&[W, LoL, HiL], R::Void),
        19 => (&[SelfL, WndL, WndW], R::Void),
        20 => (&[DcW], R::Void),
        21 => (&[MenuW], R::Void),
        22 => (&[MenuW, LoL, HiL], R::Void),
        23 => (&[WndW], R::Void),
        24 => (&[WndW, LoL, HiL], R::Void),
        25 => (&[WndW, PtL], R::Void),
        26 => (&[WndW, L], R::Void),
        27 => (&[W, WndL], R::Void),
        28 => (&[LoW, WndL, HiW], R::Void),
        29 => (&[SLoW, SHiW, WndL], R::Void),
        30 => (&[SLoW, SHiW], R::Void),
        31 | 36 => (&[L], R::Void),
        32 | 43 => (&[W, L], R::One),
        34 => (&[PtL], R::Store),
        35 => (&[], R::Store),
        44 => (&[WndL], R::Void),
        45 => (&[WndW, L], R::Store),
        46 => (&[LoW, HiW, LoL, HiL], R::NonZero),
        48 => (&[LoW, HiW, L], R::Void),
        49 => (&[W, PtL], R::Void),
        _ => return None,
    })
}
/// The stack arguments of a plan (FromHandle objects made with `wnd`, `dc` and `menu`)
pub fn plan_args(plan: &[A], w: u32, l: u32, me: u32, mut wnd: impl FnMut(u32) -> u32, mut dc: impl FnMut(u32) -> u32, mut menu: impl FnMut(u32) -> u32) -> Vec<u32> {
    let mut v = Vec::new();
    for a in plan {
        match a {
            A::W => v.push(w),
            A::L => v.push(l),
            A::LoW => v.push(lo(w)),
            A::HiW => v.push(hi(w)),
            A::SLoW => v.push(slo(w)),
            A::SHiW => v.push(shi(w)),
            A::LoL => v.push(lo(l)),
            A::HiL => v.push(hi(l)),
            A::SLoL => v.push(slo(l)),
            A::SHiL => v.push(shi(l)),
            A::WndW => v.push(wnd(w)),
            A::WndL => v.push(wnd(l)),
            A::DcW => v.push(dc(w)),
            A::MenuW => v.push(menu(w)),
            A::MenuL => v.push(menu(l)),
            A::PtL => {
                v.push(slo(l));
                v.push(shi(l));
            }
            A::SelfL => v.push((me == l) as u32),
        }
    }
    v
}

/// CWnd::OnWndMsg(message, wParam, lParam, pResult)
fn OnWndMsg(c: &mut Ctx) -> u32 {
    let (o, msg, w, l, pres) = (this(c), c.a(0), c.a(1), c.a(2), c.a(3));
    let (handled, r) = on_wnd_msg(c, o, msg, w, l);
    if handled && pres != 0 {
        w32(pres, r);
    }
    handled as u32
}
fn on_wnd_msg(c: &mut Ctx, o: u32, msg: u32, w: u32, l: u32) -> (bool, u32) {
    if msg == 0x111 {
        return (vcall(c, o, 32, &[w, l]) != 0, 1);
    }
    if msg == 0x4e {
        let r = Tmp::new(4);
        let h = vcall(c, o, 33, &[w, l, r.0]) != 0;
        return (h, r32(r.0));
    }
    // WM_CTLCOLORxxx: the WM_CTLCOLOR (0x19) entry with AFX_CTLCOLOR {hWnd, hDC, nCtlType}
    let (key, ctl) = if (0x132..=0x138).contains(&msg) { (0x19, Some((l, w, msg - 0x132))) } else { (msg, None) };
    for m in map_chain(c, o) {
        let entries = r32(m + 4);
        let e = if key >= 0xc000 { find_registered(entries, key) } else { find_entry(entries, key, 0, 0) };
        let Some(e) = e else { continue };
        let (sig, pfn) = (r32(e + 16), r32(e + 20));
        if key >= 0xc000 {
            return (true, tcall(c, pfn, o, &[w, l]));
        }
        if let (4 | 5, Some((hwnd, hdc, ty))) = (sig, ctl) {
            let dc = dc_from_handle(hdc);
            let args = if sig == 4 { vec![dc, wnd_from_handle(hwnd), ty] } else { vec![dc, ty] };
            return (true, tcall(c, pfn, o, &args));
        }
        let Some((plan, ret)) = sig_plan(sig) else {
            return (true, 0);
        };
        let me = r32(o + 0x20);
        let args = plan_args(plan, w, l, me, wnd_from_handle, dc_from_handle, menu_from_handle);
        let r = tcall(c, pfn, o, &args);
        return match ret {
            R::Store => (true, r),
            R::Void => (true, 0),
            R::One => (true, 1),
            R::NonZero => (r != 0, r),
        };
    }
    (false, 0)
}

/// _AfxDispatchCmdMsg: calls a command handler of signature `sig`
fn dispatch_cmd(c: &mut Ctx, target: u32, id: u32, code: u32, pfn: u32, extra: u32, sig: u32, info: u32) -> u32 {
    if info != 0 {
        w32(info, target);
        w32(info + 4, pfn);
        return 1;
    }
    let notify = |extra: u32| (r32(extra + 4), r32(extra));
    match sig {
        2 => tcall(c, pfn, target, &[id]),
        12 => {
            tcall(c, pfn, target, &[]);
            1
        }
        13 => {
            tcall(c, pfn, target, &[id]);
            1
        }
        35 => tcall(c, pfn, target, &[]),
        38 | 39 => {
            let (nm, res) = notify(extra);
            let r = tcall(c, pfn, target, &[nm, res]);
            if sig == 38 { 1 } else { r }
        }
        40 | 41 => {
            let (nm, res) = notify(extra);
            let r = tcall(c, pfn, target, &[id, nm, res]);
            if sig == 40 { 1 } else { r }
        }
        44 | 45 => {
            if sig == 44 {
                tcall(c, pfn, target, &[extra]);
            } else {
                tcall(c, pfn, target, &[extra, id]);
            }
            let cont = r32(extra + 0x1c);
            w32(extra + 0x1c, 0);
            (cont == 0) as u32
        }
        46 => {
            tcall(c, pfn, target, &[extra]);
            1
        }
        47 => tcall(c, pfn, target, &[extra]),
        _ => {
            let _ = code;
            0
        }
    }
}
/// CCmdTarget::OnCmdMsg(nID, nCode, pExtra, pHandlerInfo)
fn cmd_target_on_cmd_msg(c: &mut Ctx, o: u32, id: u32, code: u32, extra: u32, info: u32) -> u32 {
    let (mut msg, mut code) = (0, code);
    if code != u32::MAX {
        msg = hi(code);
        code = lo(code);
    }
    if msg == 0 {
        msg = 0x111;
    }
    for m in map_chain(c, o) {
        if let Some(e) = find_entry(r32(m + 4), msg, code, id) {
            let (sig, pfn) = (r32(e + 16), r32(e + 20));
            return dispatch_cmd(c, o, id, code, pfn, extra, sig, info);
        }
    }
    0
}
fn OnCmdMsg_CCmdTarget(c: &mut Ctx) -> u32 {
    let o = this(c);
    let (id, code, extra, info) = (c.a(0), c.a(1), c.a(2), c.a(3));
    cmd_target_on_cmd_msg(c, o, id, code, extra, info)
}
/// CDialog::OnCmdMsg: its own maps, then the owner's, then the thread's
fn OnCmdMsg_CDialog(c: &mut Ctx) -> u32 {
    let o = this(c);
    let (id, code, extra, info) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if cmd_target_on_cmd_msg(c, o, id, code, extra, info) != 0 {
        return 1;
    }
    if (code != 0 && code != u32::MAX) || id == 0 || id >= 0x8000 && id >= 0xf000 {
        return 0;
    }
    let parent = user(c, "GetParent", &[r32(o + 0x20)]);
    if parent != 0 {
        let p = wnd_from_handle(parent);
        if vcall(c, p, 5, &[id, code, extra, info]) != 0 {
            return 1;
        }
    }
    let t = get_thread();
    if t != 0 && vcall(c, t, 5, &[id, code, extra, info]) != 0 {
        return 1;
    }
    0
}

// ---------------------------------------------------------------------------------------------
// windows: the handle map, AfxWndProc, the CWnd basics

static PERMANENT: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
static TEMPORARY: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
static TEMP_DC: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
static TEMP_MENU: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);

fn from_handle_permanent(h: u32) -> u32 {
    PERMANENT.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).copied().unwrap_or(0)
}
/// CWnd::FromHandle: the window's CWnd (a temporary one made for a window MFC doesn't have)
pub fn wnd_from_handle(h: u32) -> u32 {
    if h == 0 {
        return 0;
    }
    let p = from_handle_permanent(h);
    if p != 0 {
        return p;
    }
    *TEMPORARY.lock().unwrap().get_or_insert_with(HashMap::default).entry(h).or_insert_with(|| {
        let o = alloc(0x40);
        cmd_target_init(o, "CWnd");
        w32(o + 0x20, h);
        o
    })
}
fn dc_from_handle(h: u32) -> u32 {
    if h == 0 {
        return 0;
    }
    *TEMP_DC.lock().unwrap().get_or_insert_with(HashMap::default).entry(h).or_insert_with(|| {
        let o = alloc(0x10);
        w32(o, vtable("CDC"));
        w32(o + 4, h);
        w32(o + 8, h);
        o
    })
}
fn menu_from_handle(h: u32) -> u32 {
    if h == 0 {
        return 0;
    }
    *TEMP_MENU.lock().unwrap().get_or_insert_with(HashMap::default).entry(h).or_insert_with(|| {
        let o = alloc(8);
        w32(o, vtable("CMenu"));
        w32(o + 4, h);
        o
    })
}
fn attach(o: u32, h: u32) -> bool {
    if h == 0 {
        return false;
    }
    PERMANENT.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, o);
    TEMPORARY.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
    w32(o + 0x20, h);
    true
}
fn detach(o: u32) -> u32 {
    let h = r32(o + 0x20);
    if h != 0 {
        let mut g = PERMANENT.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::default);
        if m.get(&h) == Some(&o) {
            m.remove(&h);
        }
    }
    w32(o + 0x20, 0);
    h
}

/// AfxWndProc: the window procedure of every window MFC has a CWnd for
pub fn afx_wnd_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| api::host_fn("mfc42 AfxWndProc", AfxWndProc, 16))
}
fn AfxWndProc(c: &mut Ctx) -> u32 {
    let (h, m, w, l) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let o = from_handle_permanent(h);
    if o == 0 || r32(o + 0x20) != h {
        return user(c, "DefWindowProcA", &[h, m, w, l]);
    }
    call_wnd_proc(c, o, h, m, w, l)
}
/// AfxCallWndProc: WindowProc with the message as the thread's last sent one (Default())
fn call_wnd_proc(c: &mut Ctx, o: u32, h: u32, m: u32, w: u32, l: u32) -> u32 {
    let old = ts().last;
    ts_set(|t| t.last = [h, m, w, l]);
    let r = vcall(c, o, 40, &[m, w, l]);
    ts_set(|t| t.last = old);
    r
}
fn AfxCallWndProc(c: &mut Ctx) -> u32 {
    let (o, h, m, w, l) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    call_wnd_proc(c, o, h, m, w, l)
}
fn AfxGetAfxWndProc(_: &mut Ctx) -> u32 {
    afx_wnd_proc()
}
/// CWnd::WindowProc(message, wParam, lParam): OnWndMsg, else DefWindowProc
fn WindowProc(c: &mut Ctx) -> u32 {
    let (o, m, w, l) = (this(c), c.a(0), c.a(1), c.a(2));
    let r = Tmp::new(4);
    if vcall(c, o, 41, &[m, w, l, r.0]) == 0 {
        return vcall(c, o, 42, &[m, w, l]);
    }
    r32(r.0)
}
/// CWnd::DefWindowProc: the window's procedure before MFC's (m_pfnSuper, else
/// *GetSuperWndProcAddr()), else ::DefWindowProc
fn DefWindowProc(c: &mut Ctx) -> u32 {
    let (o, m, w, l) = (this(c), c.a(0), c.a(1), c.a(2));
    let h = r32(o + 0x20);
    let mut f = r32(o + 0x2c);
    if f == 0 {
        let p = vcall(c, o, 34, &[]);
        f = if p == 0 { 0 } else { r32(p) };
    }
    if f != 0 {
        user(c, "CallWindowProcA", &[f, h, m, w, l])
    } else {
        user(c, "DefWindowProcA", &[h, m, w, l])
    }
}
/// CWnd::Default(): DefWindowProc with the last message
fn Default(c: &mut Ctx) -> u32 {
    let o = this(c);
    let [_, m, w, l] = ts().last;
    vcall(c, o, 42, &[m, w, l])
}
fn GetSuperWndProcAddr(c: &mut Ctx) -> u32 {
    this(c) + 0x2c
}
/// CWnd::OnNcDestroy (CWnd's message map: WM_NCDESTROY)
fn OnNcDestroy(c: &mut Ctx) -> u32 {
    let o = this(c);
    let t = get_thread();
    if t != 0 {
        if r32(t + 0x20) == o {
            user(c, "PostQuitMessage", &[0]);
            w32(t + 0x20, 0);
        }
        if r32(t + 0x24) == o {
            w32(t + 0x24, 0);
        }
    }
    let h = r32(o + 0x20);
    let proc_ = user(c, "GetWindowLongA", &[h, (-4i32) as u32]);
    let [_, m, w, l] = ts().last;
    vcall(c, o, 42, &[m, w, l]);
    if user(c, "GetWindowLongA", &[h, (-4i32) as u32]) == proc_ {
        let p = vcall(c, o, 34, &[]);
        let sup = if p == 0 { 0 } else { r32(p) };
        if sup != 0 {
            user(c, "SetWindowLongA", &[h, (-4i32) as u32, sup]);
        }
    }
    detach(o);
    vcall(c, o, 43, &[]);
    0
}
/// CWnd::OnCommand(wParam, lParam): a control's notification reflected to it first, then
/// OnCmdMsg
fn OnCommand(c: &mut Ctx) -> u32 {
    let (o, w, l) = (this(c), c.a(0), c.a(1));
    let (id, mut code) = (lo(w), hi(w));
    if l == 0 {
        if id == 0 {
            return 0;
        }
        code = 0;
    } else {
        if ts().lockout == r32(o + 0x20) {
            return 1;
        }
        if reflect_last_msg(c, l, 0) {
            return 1;
        }
        if id == 0 {
            return 0;
        }
    }
    vcall(c, o, 5, &[id, code, 0, 0])
}
/// CWnd::OnNotify(wParam, lParam, pResult)
fn OnNotify(c: &mut Ctx) -> u32 {
    let (o, l, pres) = (this(c), c.a(1), c.a(2));
    let from = r32(l);
    let id = user(c, "GetDlgCtrlID", &[from]);
    let code = r32(l + 8);
    if ts().lockout == r32(o + 0x20) {
        return 1;
    }
    if reflect_last_msg(c, from, pres) {
        return 1;
    }
    let notify = Tmp::new(8);
    w32(notify.0, pres);
    w32(notify.0 + 4, l);
    vcall(c, o, 5, &[id, (0x4e << 16) | lo(code), notify.0, 0])
}
/// CWnd::ReflectLastMsg: the control's own CWnd handles the message first (OnChildNotify)
fn reflect_last_msg(c: &mut Ctx, child: u32, pres: u32) -> bool {
    let o = from_handle_permanent(child);
    if o == 0 {
        return false;
    }
    let [_, m, w, l] = ts().last;
    vcall(c, o, 44, &[m, w, l, pres]) != 0
}
/// CWnd::OnChildNotify -> ReflectChildNotify: the reflected message (WM_REFLECT_BASE +)
fn OnChildNotify(c: &mut Ctx) -> u32 {
    let (o, m, w, l, pres) = (this(c), c.a(0), c.a(1), c.a(2), c.a(3));
    const REFLECT: u32 = 0xbc00;
    if m == 0x111 {
        if cmd_target_on_cmd_msg(c, o, 0, ((REFLECT + 0x111) << 16) | hi(w), 0, 0) != 0 {
            if pres != 0 {
                w32(pres, 1);
            }
            return 1;
        }
        return 0;
    }
    if m == 0x4e {
        let notify = Tmp::new(8);
        w32(notify.0, pres);
        w32(notify.0 + 4, l);
        return cmd_target_on_cmd_msg(c, o, 0, ((REFLECT + 0x4e) << 16) | lo(r32(l + 8)), notify.0, 0);
    }
    if matches!(m, 0x114 | 0x115 | 0x2b | 0x2c | 0x2d | 0x39 | 0x2e | 0x2f | 0x210) || (0x132..=0x138).contains(&m) {
        let (h, r) = on_wnd_msg(c, o, REFLECT + m, w, l);
        if h && pres != 0 {
            w32(pres, r);
        }
        return h as u32;
    }
    0
}

fn CWnd_ctor(c: &mut Ctx) -> u32 {
    let o = this(c);
    wnd_init(o, "CWnd");
    o
}
fn wnd_init(o: u32, class: &str) {
    cmd_target_init(o, class);
    mem::fill(o + 0x20, 0x20, 0);
}
/// ~CWnd: destroys the window still there
fn wnd_dtor(c: &mut Ctx, o: u32) {
    if r32(o + 0x20) != 0 {
        destroy_window(c, o);
    }
}
fn CWnd_dtor(c: &mut Ctx) -> u32 {
    let o = this(c);
    wnd_dtor(c, o);
    0
}
/// CWnd::DestroyWindow
fn destroy_window(c: &mut Ctx, o: u32) -> u32 {
    let h = r32(o + 0x20);
    if h == 0 {
        return 0;
    }
    let permanent = from_handle_permanent(h) != 0;
    let r = user(c, "DestroyWindow", &[h]);
    if !permanent {
        detach(o);
    }
    r
}
fn DestroyWindow(c: &mut Ctx) -> u32 {
    let o = this(c);
    destroy_window(c, o)
}
fn FromHandle(c: &mut Ctx) -> u32 {
    wnd_from_handle(c.a(0))
}
fn FromHandlePermanent(c: &mut Ctx) -> u32 {
    from_handle_permanent(c.a(0))
}
fn Attach(c: &mut Ctx) -> u32 {
    attach(this(c), c.a(0)) as u32
}
fn Detach(c: &mut Ctx) -> u32 {
    detach(this(c))
}
/// CWnd::SubclassWindow: attached, and MFC's procedure in front of the window's
fn subclass_window(c: &mut Ctx, o: u32, h: u32) -> bool {
    if !attach(o, h) {
        return false;
    }
    vcall(c, o, 22, &[]);
    let p = vcall(c, o, 34, &[]);
    let old = user(c, "SetWindowLongA", &[h, (-4i32) as u32, afx_wnd_proc()]);
    if p != 0 && r32(p) == 0 {
        w32(p, old);
    }
    true
}
fn SubclassWindow(c: &mut Ctx) -> u32 {
    let (o, h) = (this(c), c.a(0));
    subclass_window(c, o, h) as u32
}
fn SubclassDlgItem(c: &mut Ctx) -> u32 {
    let (o, id, parent) = (this(c), c.a(0), c.a(1));
    let h = user(c, "GetDlgItem", &[r32(parent + 0x20), id]);
    if h == 0 {
        return 0;
    }
    subclass_window(c, o, h) as u32
}
fn hwnd(c: &Ctx) -> u32 {
    r32(this(c) + 0x20)
}
macro_rules! forward {
    ($name:ident, $api:literal, $n:literal) => {
        fn $name(c: &mut Ctx) -> u32 {
            let mut a = vec![hwnd(c)];
            for i in 0..$n {
                a.push(c.a(i));
            }
            user(c, $api, &a)
        }
    };
}
forward!(SetWindowTextA, "SetWindowTextA", 1);
forward!(ShowWindow, "ShowWindow", 1);
forward!(EnableWindow, "EnableWindow", 1);
forward!(SetDlgItemTextA, "SetDlgItemTextA", 2);
forward!(GetDlgItemTextA, "GetDlgItemTextA", 3);
forward!(SetDlgItemInt, "SetDlgItemInt", 3);
forward!(GetDlgItemInt, "GetDlgItemInt", 3);
forward!(CheckDlgButton, "CheckDlgButton", 2);
forward!(IsDlgButtonChecked, "IsDlgButtonChecked", 1);
forward!(SendDlgItemMessageA, "SendDlgItemMessageA", 4);
forward!(GetWindowTextA, "GetWindowTextA", 2);
forward!(GetWindowTextLengthA, "GetWindowTextLengthA", 0);
forward!(MoveWindow, "MoveWindow", 5);
forward!(IsWindowEnabled, "IsWindowEnabled", 0);
fn GetDlgItem(c: &mut Ctx) -> u32 {
    let (h, id) = (hwnd(c), c.a(0));
    let ch = user(c, "GetDlgItem", &[h, id]);
    wnd_from_handle(ch)
}
fn SetFocus(c: &mut Ctx) -> u32 {
    let h = hwnd(c);
    let p = user(c, "SetFocus", &[h]);
    wnd_from_handle(p)
}
/// CWnd::MessageBox(text, caption (NULL: the application's name), type)
fn MessageBoxA(c: &mut Ctx) -> u32 {
    let (h, text, mut cap, ty) = (hwnd(c), c.a(0), c.a(1), c.a(2));
    if cap == 0 {
        cap = r32(module_state() + 0x10);
    }
    user(c, "MessageBoxA", &[h, text, cap, ty])
}
/// CWnd::GetSafeOwner_(hParent, &hWndTop): the owner of a modal window, the top-level window
/// disabled meanwhile
fn safe_owner(c: &mut Ctx, parent: u32, top: Option<u32>) -> u32 {
    let mut h = parent;
    if h == 0 {
        let m = main_wnd();
        if m != 0 {
            h = r32(m + 0x20);
        }
    }
    while h != 0 && user(c, "GetWindowLongA", &[h, (-16i32) as u32]) & 0x4000_0000 != 0 {
        h = user(c, "GetParent", &[h]);
    }
    let htop = h;
    if parent == 0 && h != 0 {
        h = user(c, "GetLastActivePopup", &[h]);
    }
    if let Some(top) = top {
        if htop != 0 && user(c, "IsWindowEnabled", &[htop]) != 0 && htop != h {
            w32(top, htop);
            user(c, "EnableWindow", &[htop, 0]);
        } else {
            w32(top, 0);
        }
    }
    h
}
fn GetSafeOwner(c: &mut Ctx) -> u32 {
    let (p, top) = (c.a(0), c.a(1));
    let hp = if p == 0 { 0 } else { r32(p + 0x20) };
    let h = safe_owner(c, hp, (top != 0).then_some(top));
    wnd_from_handle(h)
}
/// CenterWindow's position of a `dlg` rectangle centred on `center` inside `area`
pub fn center_pos(dlg: (i32, i32, i32, i32), center: (i32, i32, i32, i32), area: (i32, i32, i32, i32)) -> (i32, i32) {
    let (w, h) = (dlg.2 - dlg.0, dlg.3 - dlg.1);
    let mut x = (center.0 + center.2) / 2 - w / 2;
    let mut y = (center.1 + center.3) / 2 - h / 2;
    if x < area.0 {
        x = area.0;
    } else if x + w > area.2 {
        x = area.2 - w;
    }
    if y < area.1 {
        y = area.1;
    } else if y + h > area.3 {
        y = area.3 - h;
    }
    (x, y)
}
fn rect(p: u32) -> (i32, i32, i32, i32) {
    (r32(p) as i32, r32(p + 4) as i32, r32(p + 8) as i32, r32(p + 12) as i32)
}
/// CWnd::CenterWindow(pAlternateOwner)
fn CenterWindow(c: &mut Ctx) -> u32 {
    let (h, alt) = (hwnd(c), c.a(0));
    let style = user(c, "GetWindowLongA", &[h, (-16i32) as u32]);
    let mut center = if alt != 0 { r32(alt + 0x20) } else { 0 };
    if alt == 0 {
        center = if style & 0x4000_0000 != 0 { user(c, "GetParent", &[h]) } else { user(c, "GetWindow", &[h, 4]) };
        if center != 0 {
            let t = user(c, "SendMessageA", &[center, 0x36b, 0, 0]);
            if t != 0 {
                center = t;
            }
        }
    }
    let r = Tmp::new(48);
    let (rd, ra, rc) = (r.0, r.0 + 16, r.0 + 32);
    user(c, "GetWindowRect", &[h, rd]);
    if style & 0x4000_0000 == 0 {
        if center != 0 {
            let s = user(c, "GetWindowLongA", &[center, (-16i32) as u32]);
            if s & 0x1000_0000 == 0 || s & 0x2000_0000 != 0 {
                center = 0;
            }
        }
        user(c, "SystemParametersInfoA", &[0x30, 0, ra, 0]);
        if center == 0 {
            mem::copy(rc, ra, 16);
        } else {
            user(c, "GetWindowRect", &[center, rc]);
        }
    } else {
        let parent = user(c, "GetParent", &[h]);
        user(c, "GetClientRect", &[parent, ra]);
        user(c, "GetClientRect", &[center, rc]);
        user(c, "MapWindowPoints", &[center, parent, rc, 2]);
    }
    let (x, y) = center_pos(rect(rd), rect(rc), rect(ra));
    user(c, "SetWindowPos", &[h, 0, x as u32, y as u32, u32::MAX, u32::MAX, 0x15]);
    0
}

fn ContinueModal(c: &mut Ctx) -> u32 {
    r32(this(c) + 0x28) & 0x10
}
/// CWnd::EndModalLoop(nResult)
fn EndModalLoop(c: &mut Ctx) -> u32 {
    let (o, n) = (this(c), c.a(0));
    w32(o + 0x30, n);
    let f = r32(o + 0x28);
    if f & 0x10 != 0 {
        w32(o + 0x28, f & !0x10);
        user(c, "PostMessageA", &[r32(o + 0x20), 0, 0, 0]);
    }
    0
}
/// CWnd::RunModalLoop(dwFlags): the thread's PumpMessage until EndModalLoop
fn run_modal_loop(c: &mut Ctx, o: u32, flags: u32) -> u32 {
    let h = r32(o + 0x20);
    let mut idle = true;
    let mut count = 0u32;
    let mut show_idle = flags & 1 != 0 && user(c, "GetWindowLongA", &[h, (-16i32) as u32]) & 0x1000_0000 == 0;
    let parent = user(c, "GetParent", &[h]);
    w32(o + 0x28, r32(o + 0x28) | 0x18);
    let t = get_thread();
    let msg = t + 0x34;
    loop {
        while idle && user(c, "PeekMessageA", &[msg, 0, 0, 0, 0]) == 0 {
            if show_idle {
                user(c, "ShowWindow", &[h, 1]);
                user(c, "UpdateWindow", &[h]);
                show_idle = false;
            }
            if flags & 2 == 0 && parent != 0 && count == 0 {
                user(c, "SendMessageA", &[parent, 0x121, 0, h]);
            }
            let kick = flags & 4 == 0 && {
                let n = count;
                count += 1;
                user(c, "SendMessageA", &[h, 0x36a, 0, n]) != 0
            };
            if !kick {
                idle = false;
            }
        }
        loop {
            if vcall(c, o, 30, &[]) == 0 {
                w32(o + 0x28, r32(o + 0x28) & !0x18);
                return r32(o + 0x30);
            }
            if vcall(c, t, 25, &[]) == 0 {
                user(c, "PostQuitMessage", &[0]);
                return u32::MAX;
            }
            if show_idle {
                user(c, "ShowWindow", &[h, 1]);
                user(c, "UpdateWindow", &[h]);
                show_idle = false;
            }
            if vcall(c, o, 30, &[]) == 0 {
                w32(o + 0x28, r32(o + 0x28) & !0x18);
                return r32(o + 0x30);
            }
            if vcall(c, t, 27, &[msg]) != 0 {
                idle = true;
                count = 0;
            }
            if user(c, "PeekMessageA", &[msg, 0, 0, 0, 0]) == 0 {
                break;
            }
        }
    }
}
fn RunModalLoop(c: &mut Ctx) -> u32 {
    let (o, f) = (this(c), c.a(0));
    run_modal_loop(c, o, f)
}
/// CWnd::PreTranslateMessage: (tool tips) not handled
fn PreTranslateMessage_CWnd(_: &mut Ctx) -> u32 {
    0
}
/// CDialog::PreTranslateMessage -> PreTranslateInput: the dialog's keyboard and mouse
/// messages go to IsDialogMessage
fn PreTranslateMessage_CDialog(c: &mut Ctx) -> u32 {
    let (o, msg) = (this(c), c.a(0));
    let m = r32(msg + 4);
    if !((0x100..=0x108).contains(&m) || (0x200..=0x20d).contains(&m)) {
        return 0;
    }
    user(c, "IsDialogMessageA", &[r32(o + 0x20), msg])
}
/// CWnd::WalkPreTranslateTree(hWndStop, pMsg)
fn walk_pre_translate(c: &mut Ctx, stop: u32, msg: u32) -> bool {
    let mut h = r32(msg);
    while h != 0 {
        let o = from_handle_permanent(h);
        if o != 0 && vcall(c, o, 38, &[msg]) != 0 {
            return true;
        }
        if h == stop {
            break;
        }
        h = user(c, "GetParent", &[h]);
    }
    false
}
fn WalkPreTranslateTree(c: &mut Ctx) -> u32 {
    let (stop, msg) = (c.a(0), c.a(1));
    walk_pre_translate(c, stop, msg) as u32
}

// ---------------------------------------------------------------------------------------------
// window creation: the CBT hook that attaches the window being made to its CWnd

fn cbt_hook() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| api::host_fn("mfc42 _AfxCbtFilterHook", AfxCbtFilterHook, 12))
}
fn hook_window_create(c: &mut Ctx, o: u32) {
    if ts().hook == 0 {
        let tid = k32(c, "GetCurrentThreadId", &[]);
        let h = user(c, "SetWindowsHookExA", &[5, cbt_hook(), 0, tid]);
        ts_set(|t| t.hook = h);
    }
    ts_set(|t| t.wnd_init = o);
}
fn unhook_window_create(c: &mut Ctx) -> bool {
    let t = ts();
    if t.hook != 0 {
        user(c, "UnhookWindowsHookEx", &[t.hook]);
        ts_set(|t| t.hook = 0);
    }
    if t.wnd_init != 0 {
        ts_set(|t| t.wnd_init = 0);
        return false;
    }
    true
}
fn AfxHookWindowCreate(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    hook_window_create(c, o);
    0
}
fn AfxUnhookWindowCreate(c: &mut Ctx) -> u32 {
    unhook_window_create(c) as u32
}
fn AfxCbtFilterHook(c: &mut Ctx) -> u32 {
    let (code, w) = (c.a(0), c.a(1));
    if code != 3 {
        return 0;
    }
    let o = ts().wnd_init;
    if o == 0 {
        return 0;
    }
    attach(o, w);
    vcall(c, o, 22, &[]);
    let p = vcall(c, o, 34, &[]);
    let old = user(c, "SetWindowLongA", &[w, (-4i32) as u32, afx_wnd_proc()]);
    if old != afx_wnd_proc() && p != 0 {
        w32(p, old);
    }
    ts_set(|t| t.wnd_init = 0);
    0
}

// ---------------------------------------------------------------------------------------------
// CDialog

fn dialog_init(o: u32, tmpl: u32, parent: u32) {
    wnd_init(o, "CDialog");
    mem::fill(o + 0x40, 0x20, 0);
    w32(o + 0x44, tmpl);
    if tmpl < 0x10000 {
        w32(o + 0x40, tmpl);
    }
    w32(o + 0x54, parent);
}
fn CDialog_ctor(c: &mut Ctx) -> u32 {
    let o = this(c);
    dialog_init(o, 0, 0);
    o
}
fn CDialog_ctor_id(c: &mut Ctx) -> u32 {
    let (o, id, parent) = (this(c), c.a(0), c.a(1));
    dialog_init(o, id & 0xffff, parent);
    o
}
fn CDialog_ctor_name(c: &mut Ctx) -> u32 {
    let (o, name, parent) = (this(c), c.a(0), c.a(1));
    dialog_init(o, name, parent);
    o
}
fn CDialog_dtor(c: &mut Ctx) -> u32 {
    let o = this(c);
    wnd_dtor(c, o);
    0
}
/// The dialog template's resource: FindResource / LoadResource / LockResource
fn load_template(c: &mut Ctx, name: u32) -> (u32, u32) {
    let inst = resource_handle(c);
    let r = k32(c, "FindResourceA", &[inst, name, 5]);
    if r == 0 {
        return (inst, 0);
    }
    let g = k32(c, "LoadResource", &[inst, r]);
    (inst, k32(c, "LockResource", &[g]))
}
/// CWnd::CreateDlgIndirect: the dialog made with AfxDlgProc and attached to `o`
fn create_dlg_indirect(c: &mut Ctx, o: u32, tmpl: u32, parent: u32, inst: u32) -> bool {
    let inst = if inst == 0 { resource_handle(c) } else { inst };
    hook_window_create(c, o);
    let h = user(c, "CreateDialogIndirectParamA", &[inst, tmpl, parent, afx_dlg_proc(), 0]);
    if !unhook_window_create(c) {
        // (a window the hook never saw)
        if h != 0 && r32(o + 0x20) == 0 {
            subclass_window(c, o, h);
        }
    }
    h != 0
}
fn afx_dlg_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| api::host_fn("mfc42 AfxDlgProc", AfxDlgProc, 16))
}
/// AfxDlgProc: WM_INITDIALOG to the CDialog's OnInitDialog; nothing else
fn AfxDlgProc(c: &mut Ctx) -> u32 {
    let (h, m) = (c.a(0), c.a(1));
    if m != 0x110 {
        return 0;
    }
    let o = from_handle_permanent(h);
    if o != 0 && is_kind_of(c, o, runtime_class("CDialog")) {
        return vcall(c, o, 49, &[]);
    }
    1
}
/// CDialog::DoModal
fn DoModal(c: &mut Ctx) -> u32 {
    let o = this(c);
    let (name, htmpl) = (r32(o + 0x44), r32(o + 0x48));
    let mut tmpl = r32(o + 0x4c);
    let mut inst = resource_handle(c);
    if name != 0 {
        let (i, t) = load_template(c, name);
        inst = i;
        tmpl = t;
    } else if htmpl != 0 {
        tmpl = k32(c, "LockResource", &[htmpl]);
    }
    if tmpl == 0 {
        return u32::MAX;
    }
    // PreModal: the owner, its top-level window disabled
    let pw = r32(o + 0x54);
    let hp = safe_owner(c, if pw == 0 { 0 } else { r32(pw + 0x20) }, Some(o + 0x58));
    let enable_parent = hp != 0 && user(c, "IsWindowEnabled", &[hp]) != 0 && {
        user(c, "EnableWindow", &[hp, 0]);
        true
    };
    w32(o + 0x30, u32::MAX);
    w32(o + 0x28, r32(o + 0x28) | 0x10);
    if create_dlg_indirect(c, o, tmpl, hp, inst) {
        if r32(o + 0x28) & 0x10 != 0 {
            let h = r32(o + 0x20);
            let mut flags = 1;
            if user(c, "GetWindowLongA", &[h, (-16i32) as u32]) & 0x10 != 0 {
                flags |= 2;
            }
            run_modal_loop(c, o, flags);
        }
        let h = r32(o + 0x20);
        if h != 0 {
            user(c, "SetWindowPos", &[h, 0, 0, 0, 0, 0, 0x97]);
        }
    }
    if enable_parent {
        user(c, "EnableWindow", &[hp, 1]);
    }
    let h = r32(o + 0x20);
    if hp != 0 && h != 0 && user(c, "GetActiveWindow", &[]) == h {
        user(c, "SetActiveWindow", &[hp]);
    }
    vcall(c, o, 24, &[]);
    // PostModal
    unhook_window_create(c);
    detach(o);
    let top = r32(o + 0x58);
    if top != 0 && user(c, "IsWindow", &[top]) != 0 {
        user(c, "EnableWindow", &[top, 1]);
    }
    w32(o + 0x58, 0);
    r32(o + 0x30)
}
/// CDialog::Create(lpszTemplateName, pParentWnd)
fn Create_CDialog(c: &mut Ctx) -> u32 {
    let (o, name, parent) = (this(c), c.a(0), c.a(1));
    w32(o + 0x44, name);
    if name < 0x10000 && r32(o + 0x40) == 0 {
        w32(o + 0x40, name);
    }
    let (inst, tmpl) = load_template(c, name);
    if tmpl == 0 {
        return 0;
    }
    let hp = if parent == 0 { 0 } else { r32(parent + 0x20) };
    create_dlg_indirect(c, o, tmpl, hp, inst) as u32
}
/// CDialog::CreateIndirect(lpDialogTemplate, pParentWnd, lpDialogInit, hInst)
fn CreateIndirect(c: &mut Ctx) -> u32 {
    let (o, tmpl, parent, init) = (this(c), c.a(0), c.a(1), c.a(2));
    w32(o + 0x50, init);
    let hp = if parent == 0 { 0 } else { r32(parent + 0x20) };
    create_dlg_indirect(c, o, tmpl, hp, 0) as u32
}
/// MFC 4.0's CDialog::CreateIndirect(lpDialogTemplate, pParentWnd)
fn CreateIndirect_40(c: &mut Ctx) -> u32 {
    let (o, tmpl, parent) = (this(c), c.a(0), c.a(1));
    w32(o + 0x50, 0);
    let hp = if parent == 0 { 0 } else { r32(parent + 0x20) };
    create_dlg_indirect(c, o, tmpl, hp, 0) as u32
}
/// CDialog::InitModalIndirect(lpDialogTemplate, pParentWnd, lpDialogInit): DoModal's template
fn InitModalIndirect(c: &mut Ctx) -> u32 {
    let (o, t, parent, init) = (this(c), c.a(0), c.a(1), c.a(2));
    w32(o + 0x4c, t);
    w32(o + 0x54, parent);
    w32(o + 0x50, init);
    1
}
/// CDialog::HandleInitDialog (its message map: WM_INITDIALOG)
fn HandleInitDialog(c: &mut Ctx) -> u32 {
    let o = this(c);
    vcall(c, o, 53, &[]);
    let [_, m, w, l] = ts().last;
    vcall(c, o, 42, &[m, w, l])
}
/// CDialog::HandleSetFont: OnSetFont(CFont::FromHandle(wParam)), then Default()
fn HandleSetFont(c: &mut Ctx) -> u32 {
    let (o, w) = (this(c), c.a(0));
    let f = Tmp::new(8);
    w32(f.0, vtable("CFont"));
    w32(f.0 + 4, w);
    vcall(c, o, 50, &[f.0]);
    let [_, m, w, l] = ts().last;
    vcall(c, o, 42, &[m, w, l])
}
/// CWnd::ExecuteDlgInit: the RT_DLGINIT (240) resource's list and combo box strings
fn execute_dlg_init(c: &mut Ctx, o: u32, name: u32, data: u32) -> bool {
    let mut p = data;
    if p == 0 {
        if name == 0 {
            return true;
        }
        let inst = resource_handle(c);
        let r = k32(c, "FindResourceA", &[inst, name, 240]);
        if r == 0 {
            return true;
        }
        let g = k32(c, "LoadResource", &[inst, r]);
        p = k32(c, "LockResource", &[g]);
        if p == 0 {
            return true;
        }
    }
    let h = r32(o + 0x20);
    let mut ok = true;
    while ok && r16(p) != 0 {
        let (id, mut msg, len) = (r16(p) as u32, r16(p + 2) as u32, r32(p + 4));
        p += 8;
        msg = match msg {
            0x1234 => 0x401,
            0x401 => 0x180,
            0x403 => 0x143,
            m => m,
        };
        if (msg == 0x180 || msg == 0x143) && user(c, "SendDlgItemMessageA", &[h, id, msg, 0, p]) == u32::MAX {
            ok = false;
        }
        p += len;
    }
    ok
}
fn ExecuteDlgInit(c: &mut Ctx) -> u32 {
    let (o, name) = (this(c), c.a(0));
    execute_dlg_init(c, o, name, 0) as u32
}
/// CDialog::OnInitDialog: RT_DLGINIT, then UpdateData(FALSE)
fn OnInitDialog(c: &mut Ctx) -> u32 {
    let o = this(c);
    let (init, name) = (r32(o + 0x50), r32(o + 0x44));
    if !execute_dlg_init(c, o, name, init) || update_data(c, o, 0) == 0 {
        end_dialog(c, o, u32::MAX);
        return 0;
    }
    let help = user(c, "GetDlgItem", &[r32(o + 0x20), 0xe146]);
    if help != 0 {
        user(c, "ShowWindow", &[help, 0]);
    }
    1
}
/// CDialog::EndDialog(nResult)
fn end_dialog(c: &mut Ctx, o: u32, n: u32) {
    if r32(o + 0x28) & 0x18 != 0 {
        vcall(c, o, 31, &[n]);
    }
    user(c, "EndDialog", &[r32(o + 0x20), n]);
}
fn EndDialog(c: &mut Ctx) -> u32 {
    let (o, n) = (this(c), c.a(0));
    end_dialog(c, o, n);
    0
}
fn OnOK(c: &mut Ctx) -> u32 {
    let o = this(c);
    if update_data(c, o, 1) != 0 {
        end_dialog(c, o, 1);
    }
    0
}
fn OnCancel(c: &mut Ctx) -> u32 {
    let o = this(c);
    end_dialog(c, o, 2);
    0
}
fn SetOccDialogInfo_CDialog(c: &mut Ctx) -> u32 {
    w32(this(c) + 0x5c, c.a(0));
    1
}
fn CheckAutoCenter_CDialog(_: &mut Ctx) -> u32 {
    1
}

// ---------------------------------------------------------------------------------------------
// DDX

/// CWnd::UpdateData(bSaveAndValidate): DoDataExchange with a CDataExchange
fn update_data(c: &mut Ctx, o: u32, save: u32) -> u32 {
    let dx = Tmp::new(16);
    w32(dx.0, save);
    w32(dx.0 + 4, o);
    let old = ts().lockout;
    let h = r32(o + 0x20);
    ts_set(|t| t.lockout = h);
    vcall(c, o, 35, &[dx.0]);
    ts_set(|t| t.lockout = old);
    1
}
fn UpdateData(c: &mut Ctx) -> u32 {
    let (o, s) = (this(c), c.a(0));
    update_data(c, o, s)
}
/// CDataExchange::PrepareCtrl(nIDC)
fn prepare_ctrl(c: &mut Ctx, dx: u32, id: u32, edit: bool) -> u32 {
    let dlg = r32(dx + 4);
    let h = user(c, "GetDlgItem", &[r32(dlg + 0x20), id]);
    w32(dx + 8, h);
    w32(dx + 12, edit as u32);
    h
}
fn PrepareCtrl(c: &mut Ctx) -> u32 {
    let (dx, id) = (this(c), c.a(0));
    prepare_ctrl(c, dx, id, false)
}
fn PrepareEditCtrl(c: &mut Ctx) -> u32 {
    let (dx, id) = (this(c), c.a(0));
    prepare_ctrl(c, dx, id, true)
}
/// The window text
fn window_text(c: &mut Ctx, h: u32) -> Vec<u16> {
    let n = user(c, "GetWindowTextLengthA", &[h]);
    let b = Tmp::new((n + 1) * unit());
    let got = user(c, "GetWindowTextA", &[h, b.0, n + 1]);
    rd(b.0, got.min(n))
}
/// AfxSetWindowText: set only when it differs (no flicker)
fn set_window_text(c: &mut Ctx, h: u32, text: &[u16]) {
    if window_text(c, h) != text {
        let t = strdup(text);
        user(c, "SetWindowTextA", &[h, t]);
        free(t);
    }
}
/// DDX_Text(pDX, nIDC, CString&)
fn DDX_Text_CString(c: &mut Ctx) -> u32 {
    let (dx, id, s) = (c.a(0), c.a(1), c.a(2));
    let h = prepare_ctrl(c, dx, id, true);
    if r32(dx) != 0 {
        let t = window_text(c, h);
        cs::set(s, &t);
    } else {
        let t = cs::bytes(s);
        set_window_text(c, h, &t);
    }
    0
}
/// DDX_Text(pDX, nIDC, int&) (and UINT, long, DWORD)
fn DDX_Text_int(c: &mut Ctx) -> u32 {
    let (dx, id, v) = (c.a(0), c.a(1), c.a(2));
    let h = prepare_ctrl(c, dx, id, true);
    if r32(dx) != 0 {
        let t = window_text(c, h);
        let s = string(&t);
        let s = s.trim();
        w32(v, s.parse::<i64>().unwrap_or(0) as u32);
    } else {
        set_window_text(c, h, &lit(&(r32(v) as i32).to_string()));
    }
    0
}
/// DDX_Check(pDX, nIDC, int&)
fn DDX_Check(c: &mut Ctx) -> u32 {
    let (dx, id, v) = (c.a(0), c.a(1), c.a(2));
    let h = prepare_ctrl(c, dx, id, false);
    if r32(dx) != 0 {
        let s = user(c, "SendMessageA", &[h, 0xf0, 0, 0]);
        w32(v, s);
    } else {
        user(c, "SendMessageA", &[h, 0xf1, r32(v), 0]);
    }
    0
}
/// DDX_Control(pDX, nIDC, CWnd&): the control subclassed into the CWnd once
fn DDX_Control(c: &mut Ctx) -> u32 {
    let (dx, id, o) = (c.a(0), c.a(1), c.a(2));
    if r32(o + 0x20) == 0 {
        let h = prepare_ctrl(c, dx, id, false);
        subclass_window(c, o, h);
    }
    0
}

// ---------------------------------------------------------------------------------------------
// CWinThread / CWinApp

fn thread_init(o: u32, class: &str) {
    cmd_target_init(o, class);
    mem::fill(o + 0x20, th(0x6c) - 0x20, 0);
    w32(o + 0x28, 1);
}
fn CWinThread_ctor(c: &mut Ctx) -> u32 {
    let o = this(c);
    thread_init(o, "CWinThread");
    o
}
fn CWinThread_ctor_proc(c: &mut Ctx) -> u32 {
    let (o, f, p) = (this(c), c.a(0), c.a(1));
    thread_init(o, "CWinThread");
    w32(o + 0x54, f);
    w32(o + 0x50, p);
    o
}
fn thread_dtor(c: &mut Ctx, o: u32) {
    let h = r32(o + 0x2c);
    if h != 0 && h != 0xffff_fffe {
        k32(c, "CloseHandle", &[h]);
    }
    if ts().current == o {
        ts_set(|t| t.current = 0);
    }
}
fn CWinThread_dtor(c: &mut Ctx) -> u32 {
    let o = this(c);
    thread_dtor(c, o);
    0
}
/// CWinApp::CWinApp(lpszAppName)
fn CWinApp_ctor(c: &mut Ctx) -> u32 {
    let (o, name) = (this(c), c.a(0));
    thread_init(o, "CWinApp");
    mem::fill(o + th(0x6c), 0xc4 - th(0x6c), 0);
    if name != 0 {
        w32(o + th(0x7c), strdup(&sz(name)));
    }
    w32(o + th(0xbc), 0x200);
    w32(module_state() + 4, o);
    ts_set(|t| t.current = o);
    let (th, tid) = (k32(c, "GetCurrentThread", &[]), k32(c, "GetCurrentThreadId", &[]));
    w32(o + 0x2c, th);
    w32(o + 0x30, tid);
    o
}
fn app_dtor(o: u32) {
    for off in [0x7c, 0x8c, 0x90, 0x94].map(th) {
        free(r32(o + off));
        w32(o + off, 0);
    }
    let m = module_state();
    if r32(m + 4) == o {
        w32(m + 4, 0);
        w32(m + 0x10, 0);
    }
    w32(o + 0x2c, 0);
    if ts().current == o {
        ts_set(|t| t.current = 0);
    }
}
fn CWinApp_dtor(c: &mut Ctx) -> u32 {
    app_dtor(this(c));
    0
}
/// CWinApp::SetCurrentHandles: the program's name, its help and profile file names
fn set_current_handles(c: &mut Ctx, o: u32) {
    let buf = Tmp::new(520);
    let n = k32(c, "GetModuleFileNameA", &[r32(o + th(0x6c)), buf.0, 260]);
    let path = rd(buf.0, n);
    let stem_end = path.iter().rposition(|&b| b == b'.' as u16).unwrap_or(path.len());
    let file_start = file_start(&path);
    let exe = path[file_start..stem_end.max(file_start)].to_vec();
    if r32(o + th(0x8c)) == 0 {
        w32(o + th(0x8c), strdup(&exe));
    }
    if r32(o + th(0x7c)) == 0 {
        let t = Tmp::new(256 * unit());
        let inst = resource_handle(c);
        let n = user(c, "LoadStringA", &[inst, 0xe000, t.0, 256]);
        let name = if n != 0 { rd(t.0, n) } else { exe.clone() };
        w32(o + th(0x7c), strdup(&name));
    }
    w32(module_state() + 0x10, r32(o + th(0x7c)));
    if r32(o + th(0x90)) == 0 {
        let mut h = path[..stem_end].to_vec();
        h.extend(lit(".HLP"));
        w32(o + th(0x90), strdup(&h));
    }
    if r32(o + th(0x94)) == 0 {
        let mut p = exe;
        p.extend(lit(".INI"));
        w32(o + th(0x94), strdup(&p));
    }
}
/// AfxWinInit(hInstance, hPrevInstance, lpCmdLine, nCmdShow)
fn win_init(c: &mut Ctx, inst: u32, prev: u32, cmd: u32, show: u32) -> u32 {
    let e = k32(c, "SetErrorMode", &[0]);
    k32(c, "SetErrorMode", &[e | 0x8001]);
    let m = module_state();
    w32(m + 8, inst);
    w32(m + 0xc, inst);
    let a = app();
    if a != 0 {
        w32(a + th(0x6c), inst);
        w32(a + th(0x70), prev);
        w32(a + th(0x74), cmd);
        w32(a + th(0x78), show);
        set_current_handles(c, a);
    }
    1
}
fn AfxWinInit(c: &mut Ctx) -> u32 {
    let (i, p, cmd, s) = (c.a(0), c.a(1), c.a(2), c.a(3));
    win_init(c, i, p, cmd, s)
}
/// AfxWinMain: AfxWinInit, InitApplication, InitInstance, then Run (or ExitInstance)
fn AfxWinMain(c: &mut Ctx) -> u32 {
    let (i, p, cmd, s) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let t = get_thread();
    let a = app();
    let mut r = u32::MAX;
    if win_init(c, i, p, cmd, s) != 0 && (a == 0 || vcall(c, a, 35, &[]) != 0) && t != 0 {
        if vcall(c, t, 22, &[]) == 0 {
            let m = r32(t + 0x20);
            if m != 0 {
                vcall(c, m, 24, &[]);
            }
            r = vcall(c, t, 28, &[]);
        } else {
            r = vcall(c, t, 23, &[]);
        }
    }
    r
}
/// CWinThread::PumpMessage: one message, PreTranslateMessage or dispatched
fn PumpMessage(c: &mut Ctx) -> u32 {
    let t = this(c);
    let msg = t + 0x34;
    if user(c, "GetMessageA", &[msg, 0, 0, 0]) == 0 {
        return 0;
    }
    if r32(msg + 4) != 0x36a && vcall(c, t, 24, &[msg]) == 0 {
        user(c, "TranslateMessage", &[msg]);
        user(c, "DispatchMessageA", &[msg]);
    }
    1
}
/// CWinThread::PreTranslateMessage: the windows from the target up to the main window
fn PreTranslateMessage_CWinThread(c: &mut Ctx) -> u32 {
    let msg = c.a(0);
    let m = main_wnd();
    let stop = if m == 0 { 0 } else { r32(m + 0x20) };
    if walk_pre_translate(c, stop, msg) {
        return 1;
    }
    if m != 0 {
        let h = r32(msg);
        let mut top = h;
        while top != 0 && user(c, "GetWindowLongA", &[top, (-16i32) as u32]) & 0x4000_0000 != 0 {
            top = user(c, "GetParent", &[top]);
        }
        if top != stop && h != 0 {
            return vcall(c, m, 38, &[msg]);
        }
    }
    0
}
/// CWinThread::IsIdleMessage: not a repeated mouse move, WM_PAINT or WM_SYSTIMER
pub fn is_idle_message(msg: u32, pt: (u32, u32), last_pt: &mut (u32, u32), last_msg: &mut u32) -> bool {
    if msg == 0x200 || msg == 0xa0 {
        if *last_pt == pt && msg == *last_msg {
            return false;
        }
        *last_pt = pt;
        *last_msg = msg;
        return true;
    }
    msg != 0xf && msg != 0x118
}
fn IsIdleMessage(c: &mut Ctx) -> u32 {
    let (t, msg) = (this(c), c.a(0));
    let mut last = (r32(t + th(0x60)), r32(t + th(0x64)));
    let mut lm = r32(t + th(0x68));
    let r = is_idle_message(r32(msg + 4), (r32(msg + 20), r32(msg + 24)), &mut last, &mut lm);
    w32(t + th(0x60), last.0);
    w32(t + th(0x64), last.1);
    w32(t + th(0x68), lm);
    r as u32
}
fn OnIdle(c: &mut Ctx) -> u32 {
    (c.i(0) < 1) as u32
}
fn ExitInstance(c: &mut Ctx) -> u32 {
    r32(this(c) + 0x34 + 8)
}
/// CWinThread::Run: the message loop with idle processing, then ExitInstance
fn Run(c: &mut Ctx) -> u32 {
    let t = this(c);
    let msg = t + 0x34;
    let mut idle = true;
    let mut count = 0u32;
    loop {
        while idle && user(c, "PeekMessageA", &[msg, 0, 0, 0, 0]) == 0 {
            let n = count;
            count += 1;
            if vcall(c, t, 26, &[n]) == 0 {
                idle = false;
            }
        }
        loop {
            if vcall(c, t, 25, &[]) == 0 {
                return vcall(c, t, 28, &[]);
            }
            if vcall(c, t, 27, &[msg]) != 0 {
                idle = true;
                count = 0;
            }
            if user(c, "PeekMessageA", &[msg, 0, 0, 0, 0]) == 0 {
                break;
            }
        }
    }
}
fn GetMainWnd(c: &mut Ctx) -> u32 {
    let t = this(c);
    match r32(t + 0x24) {
        0 => r32(t + 0x20),
        a => a,
    }
}
/// CWinThread::Delete: deleted when auto-delete
fn Delete_CWinThread(c: &mut Ctx) -> u32 {
    let t = this(c);
    if r32(t + 0x28) != 0 {
        vcall(c, t, 1, &[1]);
    }
    0
}
fn thread_entry_fn() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| api::host_fn("mfc42 _AfxThreadEntry", AfxThreadEntry, 4))
}
/// _AfxThreadEntry: the new thread's CWinThread, its procedure (or InitInstance / Run), then
/// AfxEndThread
fn AfxThreadEntry(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    ts_set(|x| x.current = t);
    let a = app();
    if a != 0 && r32(t + 0x20) == 0 {
        let m = r32(a + 0x20);
        if m != 0 && r32(m + 0x20) != 0 {
            w32(t + 0x20, m);
        }
    }
    let f = r32(t + 0x54);
    let r = if f != 0 {
        c.call(f, &[r32(t + 0x50)])
    } else if vcall(c, t, 22, &[]) == 0 {
        vcall(c, t, 28, &[])
    } else {
        vcall(c, t, 23, &[])
    };
    end_thread(c, t);
    r
}
fn end_thread(c: &mut Ctx, t: u32) {
    ts_set(|x| x.current = 0);
    if t != 0 && r32(t + 0x28) != 0 {
        vcall(c, t, 32, &[]);
    }
}
/// AfxBeginThread(pfnThreadProc, pParam, nPriority, nStackSize, dwCreateFlags, lpSecurityAttrs)
fn AfxBeginThread(c: &mut Ctx) -> u32 {
    let (f, p, prio, stack, flags, sa) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let t = alloc(th(0x6c));
    thread_init(t, "CWinThread");
    w32(t + 0x54, f);
    w32(t + 0x50, p);
    begin_thread(c, t, prio, stack, flags, sa)
}
/// AfxBeginThread(pThreadClass, nPriority, ...): a CWinThread-derived class's object
fn AfxBeginThread_class(c: &mut Ctx) -> u32 {
    let (cls, prio, stack, flags, sa) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let create = r32(cls + 12);
    if create == 0 {
        return 0;
    }
    let t = c.call(create, &[]);
    if t == 0 {
        return 0;
    }
    begin_thread(c, t, prio, stack, flags, sa)
}
fn begin_thread(c: &mut Ctx, t: u32, prio: u32, stack: u32, flags: u32, sa: u32) -> u32 {
    let id = Tmp::new(4);
    let h = k32(c, "CreateThread", &[sa, stack, thread_entry_fn(), t, flags | 4, id.0]);
    if h == 0 {
        vcall(c, t, 32, &[]);
        return 0;
    }
    w32(t + 0x2c, h);
    w32(t + 0x30, r32(id.0));
    k32(c, "SetThreadPriority", &[h, prio]);
    if flags & 4 == 0 {
        k32(c, "ResumeThread", &[h]);
    }
    t
}
fn AfxEndThread(c: &mut Ctx) -> u32 {
    let n = c.a(0);
    let t = ts().current;
    end_thread(c, t);
    k32(c, "ExitThread", &[n]);
    0
}
fn CreateThread_(c: &mut Ctx) -> u32 {
    let (t, flags, stack, sa) = (this(c), c.a(0), c.a(1), c.a(2));
    (begin_thread(c, t, 0, stack, flags, sa) != 0) as u32
}
fn Run_CWinApp(c: &mut Ctx) -> u32 {
    Run(c)
}
/// CWinApp::DoMessageBox(lpszPrompt, nType, nIDPrompt): MessageBox owned by the safe owner,
/// the application's name as the caption, an exclamation mark when no icon was asked for
pub fn message_box_type(ty: u32) -> u32 {
    if ty & 0xf0 == 0 && matches!(ty & 0xf, 0 | 1 | 3 | 4) {
        ty | 0x30
    } else {
        ty
    }
}
fn do_message_box(c: &mut Ctx, text: u32, ty: u32) -> u32 {
    let top = Tmp::new(4);
    let h = safe_owner(c, 0, Some(top.0));
    let cap = r32(module_state() + 0x10);
    let r = user(c, "MessageBoxA", &[h, text, cap, message_box_type(ty)]);
    let t = r32(top.0);
    if t != 0 {
        user(c, "EnableWindow", &[t, 1]);
    }
    r
}
fn DoMessageBox(c: &mut Ctx) -> u32 {
    let (text, ty) = (c.a(0), c.a(1));
    do_message_box(c, text, ty)
}
/// AfxMessageBox(lpszText, nType, nIDHelp): the application's DoMessageBox
fn AfxMessageBox(c: &mut Ctx) -> u32 {
    let (text, ty, help) = (c.a(0), c.a(1), c.a(2));
    let a = app();
    if a != 0 {
        vcall(c, a, 37, &[text, ty, help])
    } else {
        do_message_box(c, text, ty)
    }
}
/// AfxMessageBox(nIDPrompt, nType, nIDHelp)
fn AfxMessageBox_id(c: &mut Ctx) -> u32 {
    let (id, ty, help) = (c.a(0), c.a(1), c.a(2));
    let s = Tmp::new(4);
    cs::init(s.0);
    load_string(c, s.0, id);
    let text = r32(s.0);
    let a = app();
    let r = if a != 0 { vcall(c, a, 37, &[text, ty, if help == u32::MAX { id } else { help }]) } else { do_message_box(c, text, ty) };
    cs::release(s.0);
    r
}
/// CWinApp::Enable3dControls: CTL3D32.DLL isn't there (as on Windows since XP)
fn Enable3dControls(_: &mut Ctx) -> u32 {
    0
}
fn GetProfileIntA(c: &mut Ctx) -> u32 {
    let (o, sec, key, def) = (this(c), c.a(0), c.a(1), c.a(2));
    let ini = r32(o + th(0x94));
    k32(c, "GetPrivateProfileIntA", &[sec, key, def, ini])
}
fn WriteProfileInt(c: &mut Ctx) -> u32 {
    let (o, sec, key, v) = (this(c), c.a(0), c.a(1), c.a(2));
    let ini = r32(o + th(0x94));
    let t = strdup(&lit(&(v as i32).to_string()));
    let r = k32(c, "WritePrivateProfileStringA", &[sec, key, t, ini]);
    free(t);
    r
}
/// CWinApp::GetProfileString(section, entry, default): a CString through the hidden argument
fn GetProfileStringA(c: &mut Ctx) -> u32 {
    let (o, r, sec, key, def) = (this(c), c.a(0), c.a(1), c.a(2), c.a(3));
    let ini = r32(o + th(0x94));
    let b = Tmp::new(4096 * unit());
    let d = if def == 0 { strdup(&[]) } else { 0 };
    let n = k32(c, "GetPrivateProfileStringA", &[sec, key, if def == 0 { d } else { def }, b.0, 4096, ini]);
    free(d);
    cs::init(r);
    cs::assign(r, &rd(b.0, n));
    r
}
fn WriteProfileStringA(c: &mut Ctx) -> u32 {
    let (o, sec, key, v) = (this(c), c.a(0), c.a(1), c.a(2));
    let ini = r32(o + th(0x94));
    k32(c, "WritePrivateProfileStringA", &[sec, key, v, ini])
}

// ---------------------------------------------------------------------------------------------
// CDC

fn dc_init(o: u32, class: &str) {
    w32(o, vtable(class));
    w32(o + 4, 0);
    w32(o + 8, 0);
    w32(o + 12, 0);
}
fn CDC_ctor(c: &mut Ctx) -> u32 {
    let o = this(c);
    dc_init(o, "CDC");
    o
}
fn CDC_dtor(c: &mut Ctx) -> u32 {
    let o = this(c);
    let h = r32(o + 4);
    if h != 0 {
        win(c, "gdi32", "DeleteDC", &[h]);
        w32(o + 4, 0);
        w32(o + 8, 0);
    }
    0
}
fn CDC_Attach(c: &mut Ctx) -> u32 {
    let (o, h) = (this(c), c.a(0));
    if h == 0 {
        return 0;
    }
    w32(o + 4, h);
    w32(o + 8, h);
    1
}
fn CDC_Detach(c: &mut Ctx) -> u32 {
    let o = this(c);
    let h = r32(o + 4);
    w32(o + 4, 0);
    w32(o + 8, 0);
    h
}
fn CDC_FromHandle(c: &mut Ctx) -> u32 {
    dc_from_handle(c.a(0))
}
/// CPaintDC(CWnd*): BeginPaint into m_ps
fn CPaintDC_ctor(c: &mut Ctx) -> u32 {
    let (o, w) = (this(c), c.a(0));
    dc_init(o, "CPaintDC");
    let h = r32(w + 0x20);
    w32(o + 0x10, h);
    let dc = user(c, "BeginPaint", &[h, o + 0x14]);
    w32(o + 4, dc);
    w32(o + 8, dc);
    o
}
fn CPaintDC_dtor(c: &mut Ctx) -> u32 {
    let o = this(c);
    user(c, "EndPaint", &[r32(o + 0x10), o + 0x14]);
    w32(o + 4, 0);
    w32(o + 8, 0);
    0
}
fn CClientDC_ctor(c: &mut Ctx) -> u32 {
    let (o, w) = (this(c), c.a(0));
    dc_init(o, "CClientDC");
    let h = if w == 0 { 0 } else { r32(w + 0x20) };
    w32(o + 0x10, h);
    let dc = user(c, "GetDC", &[h]);
    w32(o + 4, dc);
    w32(o + 8, dc);
    o
}
fn CWindowDC_ctor(c: &mut Ctx) -> u32 {
    let (o, w) = (this(c), c.a(0));
    dc_init(o, "CWindowDC");
    let h = if w == 0 { 0 } else { r32(w + 0x20) };
    w32(o + 0x10, h);
    let dc = user(c, "GetWindowDC", &[h]);
    w32(o + 4, dc);
    w32(o + 8, dc);
    o
}
fn CClientDC_dtor(c: &mut Ctx) -> u32 {
    let o = this(c);
    user(c, "ReleaseDC", &[r32(o + 0x10), r32(o + 4)]);
    w32(o + 4, 0);
    w32(o + 8, 0);
    0
}

// ---------------------------------------------------------------------------------------------
// exceptions: MFC throws a pointer to a CException-derived object (`throw pException`)

/// The ThrowInfo of `CFileException*` (catchable as CFileException*, CException*, CObject*)
fn throw_info(class: &str) -> u32 {
    static T: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
    if let Some(&p) = T.lock().unwrap().get_or_insert_with(HashMap::default).get(class) {
        return p;
    }
    let mut chain = vec![class.to_string()];
    let mut k = class.to_string();
    while let Some(b) = CLASSES.iter().find(|x| x.0 == k).map(|x| x.1).filter(|b| !b.is_empty()) {
        chain.push(b.to_string());
        k = b.to_string();
    }
    let n = chain.len() as u32;
    let arr = mem::heap_alloc(4 + 4 * n, true);
    w32(arr, n);
    for (i, cls) in chain.iter().enumerate() {
        let name = format!(".PAV{cls}@@");
        let td = mem::heap_alloc(8 + name.len() as u32 + 1, true);
        mem::write(td + 8, name.as_bytes());
        let ct = mem::heap_alloc(28, true);
        w32(ct, 1);
        w32(ct + 4, td);
        w32(ct + 12, u32::MAX);
        w32(ct + 20, 4);
        w32(arr + 4 + 4 * i as u32, ct);
    }
    let ti = mem::heap_alloc(16, true);
    w32(ti + 12, arr);
    T.lock().unwrap().get_or_insert_with(HashMap::default).insert(class.to_string(), ti);
    ti
}
/// Throws `obj` (a pointer of class `class`) from the handler `c` (its stdcall/thiscall
/// arguments `bytes` popped)
fn throw(c: &mut Ctx, obj: u32, class: &str, bytes: u32) -> u32 {
    let slot = alloc(4);
    w32(slot, obj);
    let ret = r32(c.esp);
    c.cpu.eip = ret;
    c.cpu.regs[koji::ESP] = c.esp + 4 + bytes;
    let ti = throw_info(class);
    crate::seh::raise(c.cpu, 0xe06d_7363, 1, ret, &[0x1993_0520, slot, ti]);
    c.jumped = true;
    0
}
/// CFileException::OsErrorToException: the cause of a Win32 error
pub fn os_error_cause(e: u32) -> u32 {
    match e {
        0 => 0,
        2 | 0x12 => 2,
        3 | 0xf | 0x7b | 0xa1 | 0xce => 3,
        4 => 4,
        5 | 0x10 | 0x13 | 0x52 | 0x53 => 5,
        6 => 6,
        0x20 => 11,
        0x21 => 12,
        0x26 => 14,
        0x27 | 0x70 => 13,
        0x19 | 0x84 | 0x83 => 9,
        0x1d | 0x1e | 0x1f | 0x22 | 0x23 | 0x24 => 10,
        _ => 1,
    }
}
fn new_file_exception(cause: u32, os: u32, name: &[u16]) -> u32 {
    let e = alloc(0x14);
    w32(e, vtable("CFileException"));
    w32(e + 4, 1);
    w32(e + 8, cause);
    w32(e + 12, os);
    cs::init(e + 0x10);
    cs::assign(e + 0x10, name);
    e
}
fn throw_file(c: &mut Ctx, cause: u32, os: u32, name: &[u16], bytes: u32) -> u32 {
    let e = new_file_exception(cause, os, name);
    throw(c, e, "CFileException", bytes)
}
fn AfxThrowFileException(c: &mut Ctx) -> u32 {
    let (cause, os, n) = (c.a(0), c.a(1), c.a(2));
    let name = sz(n);
    throw_file(c, cause, os, &name, 12)
}
fn ThrowOsError(c: &mut Ctx) -> u32 {
    let (os, n) = (c.a(0), c.a(1));
    let name = sz(n);
    if os == 0 {
        return 0;
    }
    throw_file(c, os_error_cause(os), os, &name, 8)
}
fn simple_exception(c: &mut Ctx, class: &str) -> u32 {
    let e = alloc(8);
    w32(e, vtable(class));
    w32(e + 4, 1);
    throw(c, e, class, 0)
}
fn AfxThrowMemoryException(c: &mut Ctx) -> u32 {
    simple_exception(c, "CMemoryException")
}
fn AfxThrowNotSupportedException(c: &mut Ctx) -> u32 {
    simple_exception(c, "CNotSupportedException")
}
fn AfxThrowResourceException(c: &mut Ctx) -> u32 {
    simple_exception(c, "CResourceException")
}
fn AfxThrowUserException(c: &mut Ctx) -> u32 {
    simple_exception(c, "CUserException")
}
fn CException_ctor(c: &mut Ctx) -> u32 {
    let o = this(c);
    w32(o, vtable("CException"));
    w32(o + 4, 1);
    o
}
fn CException_ctor_b(c: &mut Ctx) -> u32 {
    let (o, b) = (this(c), c.a(0));
    w32(o, vtable("CException"));
    w32(o + 4, b);
    o
}
/// CException::Delete: deleted when auto-delete
fn Delete_CException(c: &mut Ctx) -> u32 {
    let o = this(c);
    if r32(o + 4) as i32 > 0 {
        vcall(c, o, 1, &[1]);
    }
    0
}
fn GetErrorMessage_CException(c: &mut Ctx) -> u32 {
    let (buf, max, help) = (c.a(0), c.a(1), c.a(2));
    if help != 0 {
        w32(help, 0);
    }
    if max != 0 && buf != 0 {
        wr_at(buf, 0, 0);
    }
    0
}
/// MFC's message of a CFileException cause (AFX_IDP_FILE_*, "%1" the file)
pub fn file_error_text(cause: u32) -> &'static str {
    match cause {
        0 => "Unexpected file format.",
        2 => "%1 was not found.",
        3 => "%1 contains an invalid path.",
        4 => "%1 could not be opened because there are too many open files.",
        5 => "Access to %1 was denied.",
        6 => "An incorrect file handle was associated with %1.",
        7 => "%1 could not be removed because it is the current directory.",
        8 => "%1 could not be created because the directory is full.",
        9 => "Seek failed on %1",
        10 => "A hardware I/O error was reported while accessing %1.",
        11 => "A sharing violation occurred while accessing %1.",
        12 => "A locking violation occurred while accessing %1.",
        13 => "Disk full while accessing %1.",
        14 => "An attempt was made to access %1 past its end.",
        _ => "An unknown error occurred while accessing %1.",
    }
}
/// CFileException::GetErrorMessage(lpszError, nMaxError, pnHelpContext)
fn GetErrorMessage_CFileException(c: &mut Ctx) -> u32 {
    let (o, buf, max, help) = (this(c), c.a(0), c.a(1), c.a(2));
    let cause = r32(o + 8);
    if help != 0 {
        w32(help, cause + 0xf0e0);
    }
    let mut name = cs::bytes(o + 0x10);
    if name.is_empty() {
        name = lit("an unnamed file");
    }
    let (head, tail) = file_error_text(cause).split_once("%1").unwrap_or((file_error_text(cause), ""));
    let mut s = lit(head);
    s.extend(name);
    s.extend(lit(tail));
    if buf != 0 && max != 0 {
        let n = s.len().min(max as usize - 1);
        wr(buf, &s[..n]);
        wr_at(buf, n as u32, 0);
    }
    1
}
/// CException::ReportError(nType, nMessageID)
fn ReportError(c: &mut Ctx) -> u32 {
    let (o, ty, id) = (this(c), c.a(0), c.a(1));
    let b = Tmp::new(512 * unit());
    let help = Tmp::new(4);
    if vcall(c, o, 5, &[b.0, 512, help.0]) != 0 {
        let a = app();
        return if a != 0 { vcall(c, a, 37, &[b.0, ty, r32(help.0)]) } else { do_message_box(c, b.0, ty) };
    }
    let s = strdup(&lit(if id == 0 { "No error message is available." } else { "" }));
    let r = if id != 0 {
        let t = Tmp::new(4);
        cs::init(t.0);
        load_string(c, t.0, id);
        let r = do_message_box(c, r32(t.0), ty);
        cs::release(t.0);
        r
    } else {
        do_message_box(c, s, ty)
    };
    free(s);
    r
}

// ---------------------------------------------------------------------------------------------
// CFile: m_hFile +4 (hFileNull -1), m_bCloseOnDelete +8, m_strFileName +0xc

fn file_init(o: u32) {
    w32(o, vtable("CFile"));
    w32(o + 4, u32::MAX);
    w32(o + 8, 0);
    cs::init(o + 0xc);
}
fn CFile_ctor(c: &mut Ctx) -> u32 {
    let o = this(c);
    file_init(o);
    o
}
fn CFile_ctor_h(c: &mut Ctx) -> u32 {
    let (o, h) = (this(c), c.a(0));
    file_init(o);
    w32(o + 4, h);
    o
}
fn CFile_ctor_open(c: &mut Ctx) -> u32 {
    let (o, name, flags) = (this(c), c.a(0), c.a(1));
    file_init(o);
    let e = Tmp::new(0x14);
    if open(c, o, name, flags, e.0) == 0 {
        let cause = r32(e.0 + 8);
        let os = r32(e.0 + 12);
        let n = sz(name);
        return throw_file(c, cause, os, &n, 8);
    }
    o
}
fn file_dtor(c: &mut Ctx, o: u32) {
    if r32(o + 4) != u32::MAX && r32(o + 8) != 0 {
        let h = r32(o + 4);
        k32(c, "CloseHandle", &[h]);
        w32(o + 4, u32::MAX);
    }
    cs::release(o + 0xc);
}
fn CFile_dtor(c: &mut Ctx) -> u32 {
    let o = this(c);
    file_dtor(c, o);
    0
}
/// CreateFile's (access, share, disposition) for CFile's open flags
pub fn open_args(flags: u32) -> (u32, u32, u32) {
    let access = match flags & 3 {
        0 => 0x8000_0000,
        1 => 0x4000_0000,
        _ => 0xc000_0000,
    };
    let share = match flags & 0x70 {
        0x20 => 1,
        0x30 => 2,
        0x40 => 3,
        _ => 0,
    };
    let disp = if flags & 0x1000 != 0 {
        if flags & 0x2000 != 0 { 4 } else { 2 }
    } else {
        3
    };
    (access, share, disp)
}
fn open(c: &mut Ctx, o: u32, name: u32, flags: u32, err: u32) -> u32 {
    w32(o + 8, 0);
    w32(o + 4, u32::MAX);
    let full = Tmp::new(520);
    let n = k32(c, "GetFullPathNameA", &[name, 260, full.0, 0]);
    let path = if n != 0 && n < 260 { rd(full.0, n) } else { sz(name) };
    cs::set(o + 0xc, &path);
    let (access, share, disp) = open_args(flags);
    let sa = Tmp::new(12);
    w32(sa.0, 12);
    w32(sa.0 + 8, (flags & 0x80 == 0) as u32);
    let h = k32(c, "CreateFileA", &[name, access, share, sa.0, disp, 0x80, 0]);
    if h == u32::MAX {
        if err != 0 {
            let e = k32(c, "GetLastError", &[]);
            w32(err + 12, e);
            w32(err + 8, os_error_cause(e));
            // (the caller's CFileException: its m_strFileName constructed)
            cs::set(err + 0x10, &sz(name));
        }
        return 0;
    }
    w32(o + 4, h);
    w32(o + 8, 1);
    1
}
/// CFile::Open(lpszFileName, nOpenFlags, pError)
fn Open(c: &mut Ctx) -> u32 {
    let (o, name, flags, err) = (this(c), c.a(0), c.a(1), c.a(2));
    open(c, o, name, flags, err)
}
fn file_name(o: u32) -> Vec<u16> {
    cs::bytes(o + 0xc)
}
fn Read(c: &mut Ctx) -> u32 {
    let (o, buf, n) = (this(c), c.a(0), c.a(1));
    if n == 0 {
        return 0;
    }
    let got = Tmp::new(4);
    let h = r32(o + 4);
    if k32(c, "ReadFile", &[h, buf, n, got.0, 0]) == 0 {
        let e = k32(c, "GetLastError", &[]);
        let name = file_name(o);
        return throw_file(c, os_error_cause(e), e, &name, 8);
    }
    r32(got.0)
}
fn Write(c: &mut Ctx) -> u32 {
    let (o, buf, n) = (this(c), c.a(0), c.a(1));
    if n == 0 {
        return 0;
    }
    let put = Tmp::new(4);
    let h = r32(o + 4);
    if k32(c, "WriteFile", &[h, buf, n, put.0, 0]) == 0 {
        let e = k32(c, "GetLastError", &[]);
        let name = file_name(o);
        return throw_file(c, os_error_cause(e), e, &name, 8);
    }
    if r32(put.0) != n {
        let name = file_name(o);
        return throw_file(c, 13, 0, &name, 8);
    }
    0
}
fn Close(c: &mut Ctx) -> u32 {
    let o = this(c);
    let h = r32(o + 4);
    let mut ok = true;
    if h != u32::MAX {
        ok = k32(c, "CloseHandle", &[h]) != 0;
    }
    w32(o + 4, u32::MAX);
    w32(o + 8, 0);
    let name = file_name(o);
    cs::release(o + 0xc);
    if !ok {
        let e = k32(c, "GetLastError", &[]);
        return throw_file(c, os_error_cause(e), e, &name, 0);
    }
    0
}
fn Abort(c: &mut Ctx) -> u32 {
    let o = this(c);
    let h = r32(o + 4);
    if h != u32::MAX {
        k32(c, "CloseHandle", &[h]);
        w32(o + 4, u32::MAX);
    }
    cs::release(o + 0xc);
    0
}
fn Seek(c: &mut Ctx) -> u32 {
    let (o, off, from) = (this(c), c.a(0), c.a(1));
    let h = r32(o + 4);
    let r = k32(c, "SetFilePointer", &[h, off, 0, from]);
    if r == u32::MAX {
        let e = k32(c, "GetLastError", &[]);
        if e != 0 {
            let name = file_name(o);
            return throw_file(c, os_error_cause(e), e, &name, 8);
        }
    }
    r
}
fn GetPosition(c: &mut Ctx) -> u32 {
    let h = r32(this(c) + 4);
    k32(c, "SetFilePointer", &[h, 0, 0, 1])
}
fn GetLength(c: &mut Ctx) -> u32 {
    let h = r32(this(c) + 4);
    k32(c, "GetFileSize", &[h, 0])
}
fn SetLength(c: &mut Ctx) -> u32 {
    let (o, n) = (this(c), c.a(0));
    let h = r32(o + 4);
    k32(c, "SetFilePointer", &[h, n, 0, 0]);
    k32(c, "SetEndOfFile", &[h]);
    0
}
fn Flush(c: &mut Ctx) -> u32 {
    let h = r32(this(c) + 4);
    if h != u32::MAX {
        k32(c, "FlushFileBuffers", &[h]);
    }
    0
}
/// GetFileName / GetFilePath (CString through the hidden argument)
fn GetFilePath(c: &mut Ctx) -> u32 {
    let (o, r) = (this(c), c.a(0));
    cs::init(r);
    cs::assign(r, &file_name(o));
    r
}
fn GetFileName(c: &mut Ctx) -> u32 {
    let (o, r) = (this(c), c.a(0));
    let p = file_name(o);
    let s = file_start(&p);
    cs::init(r);
    cs::assign(r, &p[s..]);
    r
}
/// Where the file name of a path starts (after its last \\ or /)
fn file_start(p: &[u16]) -> usize {
    p.iter().rposition(|&b| b == b'\\' as u16 || b == b'/' as u16).map_or(0, |i| i + 1)
}
fn SetFilePath(c: &mut Ctx) -> u32 {
    let (o, p) = (this(c), c.a(0));
    cs::set(o + 0xc, &sz(p));
    0
}

// ---------------------------------------------------------------------------------------------
// memory

fn operator_new(c: &mut Ctx) -> u32 {
    let n = c.a(0);
    let p = mem::heap_alloc_small(n.max(1), false);
    if p == 0 {
        return AfxThrowMemoryException(c);
    }
    p
}
fn operator_delete(c: &mut Ctx) -> u32 {
    free(c.a(0));
    0
}

// ---------------------------------------------------------------------------------------------

/// The guest address of a data export.
pub fn data(name: &str) -> Option<u32> {
    let m = maps();
    Some(match name {
        "?messageMap@CCmdTarget@@1UAFX_MSGMAP@@B" => m.target,
        "?messageMap@CWnd@@1UAFX_MSGMAP@@B" => m.wnd,
        "?messageMap@CDialog@@1UAFX_MSGMAP@@B" => m.dialog,
        "?messageMap@CWinApp@@1UAFX_MSGMAP@@B" => m.app,
        n if n.starts_with("?messageMap@") => m.target,
        n if n.starts_with("?class") && n.ends_with("@@2UCRuntimeClass@@B") => {
            let cls = n.trim_start_matches("?class").split('@').next()?;
            runtime_class(cls)
        }
        "?afxPchNil@@3PBDB" | "?afxPchNil@@3PBGB" => {
            static P: OnceLock<u32> = OnceLock::new();
            *P.get_or_init(|| {
                let p = mem::heap_alloc(4, true);
                w32(p, cs::nil());
                p
            })
        }
        _ => return None,
    })
}

/// (name, handler) of the exports done here
fn table() -> Vec<(&'static str, Handler)> {
    vec![
        // CString
        ("??0CString@@QAE@XZ", cs_ctor),
        ("??0CString@@QAE@ABV0@@Z", cs_ctor_copy),
        ("??0CString@@QAE@PBD@Z", cs_ctor_sz::<false>),
        ("??0CString@@QAE@PBG@Z", cs_ctor_sz::<true>),
        ("??0CString@@QAE@PBDH@Z", cs_ctor_sz_n::<false>),
        ("??0CString@@QAE@PBGH@Z", cs_ctor_sz_n::<true>),
        ("??0CString@@QAE@DH@Z", cs_ctor_ch::<false>),
        ("??0CString@@QAE@GH@Z", cs_ctor_ch::<true>),
        ("??1CString@@QAE@XZ", cs_dtor),
        ("??4CString@@QAEABV0@ABV0@@Z", cs_assign_string),
        ("??4CString@@QAEABV0@PBD@Z", cs_assign_sz::<false>),
        ("??4CString@@QAEABV0@PBG@Z", cs_assign_sz::<true>),
        ("??4CString@@QAEABV0@D@Z", cs_assign_ch::<false>),
        ("??4CString@@QAEABV0@G@Z", cs_assign_ch::<true>),
        ("??YCString@@QAEABV0@ABV0@@Z", cs_append_string),
        ("??YCString@@QAEABV0@PBD@Z", cs_append_sz::<false>),
        ("??YCString@@QAEABV0@PBG@Z", cs_append_sz::<true>),
        ("??YCString@@QAEABV0@D@Z", cs_append_ch::<false>),
        ("??YCString@@QAEABV0@G@Z", cs_append_ch::<true>),
        ("??H@YG?AVCString@@ABV0@0@Z", cs_plus_ss),
        ("??H@YG?AVCString@@ABV0@PBD@Z", cs_plus_sp::<false>),
        ("??H@YG?AVCString@@ABV0@PBG@Z", cs_plus_sp::<true>),
        ("??H@YG?AVCString@@PBDABV0@@Z", cs_plus_ps::<false>),
        ("??H@YG?AVCString@@PBGABV0@@Z", cs_plus_ps::<true>),
        ("??H@YG?AVCString@@ABV0@D@Z", cs_plus_sc::<false>),
        ("??H@YG?AVCString@@ABV0@G@Z", cs_plus_sc::<true>),
        ("??H@YG?AVCString@@DABV0@@Z", cs_plus_cs::<false>),
        ("??H@YG?AVCString@@GABV0@@Z", cs_plus_cs::<true>),
        ("?Empty@CString@@QAEXXZ", cs_empty),
        ("?TrimLeft@CString@@QAEXXZ", cs_trim_left),
        ("?TrimRight@CString@@QAEXXZ", cs_trim_right),
        ("?GetBuffer@CString@@QAEPADH@Z", cs_get_buffer),
        ("?GetBufferSetLength@CString@@QAEPADH@Z", cs_get_buffer_set_length),
        ("?ReleaseBuffer@CString@@QAEXH@Z", cs_release_buffer),
        ("?Find@CString@@QBEHD@Z", cs_find_ch),
        ("?Find@CString@@QBEHG@Z", cs_find_ch),
        ("?Find@CString@@QBEHPBD@Z", cs_find_sz),
        ("?ReverseFind@CString@@QBEHD@Z", cs_reverse_find),
        ("?ReverseFind@CString@@QBEHG@Z", cs_reverse_find),
        ("?MakeUpper@CString@@QAEXXZ", cs_make_upper),
        ("?MakeLower@CString@@QAEXXZ", cs_make_lower),
        ("?Mid@CString@@QBE?AV1@HH@Z", cs_mid2),
        ("?Mid@CString@@QBE?AV1@H@Z", cs_mid1),
        ("?Left@CString@@QBE?AV1@H@Z", cs_left),
        ("?Right@CString@@QBE?AV1@H@Z", cs_right),
        ("?Format@CString@@QAAXPBDZZ", cs_format),
        ("?Format@CString@@QAAXIZZ", cs_format),
        ("?LoadStringA@CString@@QAEHI@Z", cs_load_string),
        // state
        ("?AfxGetModuleState@@YGPAVAFX_MODULE_STATE@@XZ", AfxGetModuleState),
        ("?AfxGetThread@@YGPAVCWinThread@@XZ", AfxGetThread),
        ("?AfxFindResourceHandle@@YGPAUHINSTANCE__@@PBD0@Z", AfxFindResourceHandle),
        ("?AfxPostQuitMessage@@YGXH@Z", AfxPostQuitMessage),
        ("?AfxWinInit@@YGHPAUHINSTANCE__@@0PADH@Z", AfxWinInit),
        ("?AfxWinMain@@YGHPAUHINSTANCE__@@0PADH@Z", AfxWinMain),
        ("?AfxMessageBox@@YGHPBDII@Z", AfxMessageBox),
        ("?AfxMessageBox@@YGHIII@Z", AfxMessageBox_id),
        ("?AfxBeginThread@@YGPAVCWinThread@@P6AIPAX@Z0HIKPAU_SECURITY_ATTRIBUTES@@@Z", AfxBeginThread),
        ("?AfxBeginThread@@YGPAVCWinThread@@PAUCRuntimeClass@@HIKPAU_SECURITY_ATTRIBUTES@@@Z", AfxBeginThread_class),
        ("?AfxEndThread@@YGXIH@Z", AfxEndThread),
        ("?AfxEndThread@@YGXI@Z", AfxEndThread),
        ("?AfxHookWindowCreate@@YGXPAVCWnd@@@Z", AfxHookWindowCreate),
        ("?AfxUnhookWindowCreate@@YGHXZ", AfxUnhookWindowCreate),
        ("?AfxGetAfxWndProc@@YGP6GJPAUHWND__@@IIJ@ZXZ", AfxGetAfxWndProc),
        ("?AfxWndProc@@YGJPAUHWND__@@IIJ@Z", AfxWndProc),
        ("?AfxCallWndProc@@YGJPAVCWnd@@PAUHWND__@@IIJ@Z", AfxCallWndProc),
        ("??2@YAPAXI@Z", operator_new),
        ("??3@YAXPAX@Z", operator_delete),
        ("?AfxThrowFileException@@YGXHJPBD@Z", AfxThrowFileException),
        ("?ThrowOsError@CFileException@@SGXJPBD@Z", ThrowOsError),
        ("?AfxThrowMemoryException@@YGXXZ", AfxThrowMemoryException),
        ("?AfxThrowNotSupportedException@@YGXXZ", AfxThrowNotSupportedException),
        ("?AfxThrowResourceException@@YGXXZ", AfxThrowResourceException),
        ("?AfxThrowUserException@@YGXXZ", AfxThrowUserException),
        // CObject / CCmdTarget
        ("??1CObject@@UAE@XZ", nop),
        ("?IsKindOf@CObject@@QBEHPBUCRuntimeClass@@@Z", IsKindOf),
        ("?AssertValid@CObject@@UBEXXZ", nop),
        ("?Dump@CObject@@UBEXAAVCDumpContext@@@Z", nop),
        ("??0CCmdTarget@@QAE@XZ", CCmdTarget_ctor),
        ("??1CCmdTarget@@UAE@XZ", nop),
        ("?OnCmdMsg@CCmdTarget@@UAEHIHPAXPAUAFX_CMDHANDLERINFO@@@Z", OnCmdMsg_CCmdTarget),
        ("?OnFinalRelease@CCmdTarget@@UAEXXZ", nop),
        ("?IsInvokeAllowed@CCmdTarget@@UAEHJ@Z", ret1),
        ("?GetDispatchIID@CCmdTarget@@UAEHPAU_GUID@@@Z", nop),
        ("?GetTypeInfoCount@CCmdTarget@@UAEIXZ", nop),
        ("?GetTypeLibCache@CCmdTarget@@UAEPAVCTypeLibCache@@XZ", nop),
        ("?GetTypeLib@CCmdTarget@@UAEJKPAPAUITypeLib@@@Z", get_type_lib),
        ("?GetMessageMap@CCmdTarget@@MBEPBUAFX_MSGMAP@@XZ", GetMessageMap_CCmdTarget),
        ("?GetCommandMap@CCmdTarget@@MBEPBUAFX_OLECMDMAP@@XZ", empty_map),
        ("?GetDispatchMap@CCmdTarget@@MBEPBUAFX_DISPMAP@@XZ", empty_map),
        ("?GetConnectionMap@CCmdTarget@@MBEPBUAFX_CONNECTIONMAP@@XZ", empty_map),
        ("?GetInterfaceMap@CCmdTarget@@MBEPBUAFX_INTERFACEMAP@@XZ", empty_map),
        ("?GetEventSinkMap@CCmdTarget@@MBEPBUAFX_EVENTSINKMAP@@XZ", empty_map),
        ("?OnCreateAggregates@CCmdTarget@@UAEHXZ", ret1),
        ("?GetInterfaceHook@CCmdTarget@@UAEPAUIUnknown@@PBX@Z", nop),
        ("?GetExtraConnectionPoints@CCmdTarget@@MAEHPAVCPtrArray@@@Z", nop),
        ("?GetConnectionHook@CCmdTarget@@MAEPAUIConnectionPoint@@ABU_GUID@@@Z", nop),
        // CWinThread / CWinApp
        ("??0CWinThread@@QAE@XZ", CWinThread_ctor),
        ("??0CWinThread@@QAE@P6AIPAX@Z0@Z", CWinThread_ctor_proc),
        ("??1CWinThread@@UAE@XZ", CWinThread_dtor),
        ("?InitInstance@CWinThread@@UAEHXZ", nop),
        ("?Run@CWinThread@@UAEHXZ", Run),
        ("?PreTranslateMessage@CWinThread@@UAEHPAUtagMSG@@@Z", PreTranslateMessage_CWinThread),
        ("?PumpMessage@CWinThread@@UAEHXZ", PumpMessage),
        ("?OnIdle@CWinThread@@UAEHJ@Z", OnIdle),
        ("?IsIdleMessage@CWinThread@@UAEHPAUtagMSG@@@Z", IsIdleMessage),
        ("?ExitInstance@CWinThread@@UAEHXZ", ExitInstance),
        ("?ProcessWndProcException@CWinThread@@UAEJPAVCException@@PBUtagMSG@@@Z", nop),
        ("?ProcessMessageFilter@CWinThread@@UAEHHPAUtagMSG@@@Z", nop),
        ("?GetMainWnd@CWinThread@@UAEPAVCWnd@@XZ", GetMainWnd),
        ("?Delete@CWinThread@@UAEXXZ", Delete_CWinThread),
        ("?CreateThread@CWinThread@@QAEHKIPAU_SECURITY_ATTRIBUTES@@@Z", CreateThread_),
        ("??0CWinApp@@QAE@PBD@Z", CWinApp_ctor),
        ("??1CWinApp@@UAE@XZ", CWinApp_dtor),
        ("?InitInstance@CWinApp@@UAEHXZ", ret1),
        ("?Run@CWinApp@@UAEHXZ", Run_CWinApp),
        ("?OnIdle@CWinApp@@UAEHJ@Z", OnIdle),
        ("?ExitInstance@CWinApp@@UAEHXZ", ExitInstance),
        ("?ProcessWndProcException@CWinApp@@UAEJPAVCException@@PBUtagMSG@@@Z", nop),
        ("?GetMessageMap@CWinApp@@MBEPBUAFX_MSGMAP@@XZ", GetMessageMap_CWinApp),
        ("?OpenDocumentFile@CWinApp@@UAEPAVCDocument@@PBD@Z", nop),
        ("?AddToRecentFileList@CWinApp@@UAEXPBD@Z", nop),
        ("?InitApplication@CWinApp@@UAEHXZ", ret1),
        ("?SaveAllModified@CWinApp@@UAEHXZ", ret1),
        ("?DoMessageBox@CWinApp@@UAEHPBDII@Z", DoMessageBox),
        ("?DoWaitCursor@CWinApp@@UAEXH@Z", nop),
        ("?OnDDECommand@CWinApp@@UAEHPAD@Z", nop),
        ("?WinHelpA@CWinApp@@UAEXKI@Z", nop),
        ("?OnHelp@CWinApp@@IAEXXZ", nop),
        ("?Enable3dControls@CWinApp@@IAEHXZ", Enable3dControls),
        ("?SetCurrentHandles@CWinApp@@QAEXXZ", set_current_handles_),
        ("?GetProfileIntA@CWinApp@@QAEIPBD0H@Z", GetProfileIntA),
        ("?WriteProfileInt@CWinApp@@QAEHPBD0H@Z", WriteProfileInt),
        ("?GetProfileStringA@CWinApp@@QAE?AVCString@@PBD00@Z", GetProfileStringA),
        ("?WriteProfileStringA@CWinApp@@QAEHPBD00@Z", WriteProfileStringA),
        // CWnd
        ("??0CWnd@@QAE@XZ", CWnd_ctor),
        ("??1CWnd@@UAE@XZ", CWnd_dtor),
        ("?GetMessageMap@CWnd@@MBEPBUAFX_MSGMAP@@XZ", GetMessageMap_CWnd),
        ("?OnFinalRelease@CWnd@@UAEXXZ", nop),
        ("?PreSubclassWindow@CWnd@@UAEXXZ", nop),
        ("?DestroyWindow@CWnd@@UAEHXZ", DestroyWindow),
        ("?PreCreateWindow@CWnd@@UAEHAAUtagCREATESTRUCTA@@@Z", ret1),
        ("?CalcWindowRect@CWnd@@UAEXPAUtagRECT@@I@Z", CalcWindowRect),
        ("?OnToolHitTest@CWnd@@UBEHVCPoint@@PAUtagTOOLINFOA@@@Z", ret_minus1),
        ("?GetScrollBarCtrl@CWnd@@UBEPAVCScrollBar@@H@Z", nop),
        ("?WinHelpA@CWnd@@UAEXKI@Z", nop),
        ("?ContinueModal@CWnd@@UAEHXZ", ContinueModal),
        ("?EndModalLoop@CWnd@@UAEXH@Z", EndModalLoop),
        ("?OnCommand@CWnd@@MAEHIJ@Z", OnCommand),
        ("?OnNotify@CWnd@@MAEHIJPAJ@Z", OnNotify),
        ("?GetSuperWndProcAddr@CWnd@@MAEPAP6GJPAUHWND__@@IIJ@ZXZ", GetSuperWndProcAddr),
        ("?DoDataExchange@CWnd@@MAEXPAVCDataExchange@@@Z", nop),
        ("?BeginModalState@CWnd@@UAEXXZ", nop),
        ("?EndModalState@CWnd@@UAEXXZ", nop),
        ("?PreTranslateMessage@CWnd@@UAEHPAUtagMSG@@@Z", PreTranslateMessage_CWnd),
        ("?OnAmbientProperty@CWnd@@UAEHPAVCOleControlSite@@JPAUtagVARIANT@@@Z", nop),
        ("?WindowProc@CWnd@@MAEJIIJ@Z", WindowProc),
        ("?OnWndMsg@CWnd@@MAEHIIJPAJ@Z", OnWndMsg),
        ("?DefWindowProcA@CWnd@@MAEJIIJ@Z", DefWindowProc),
        ("?PostNcDestroy@CWnd@@MAEXXZ", nop),
        ("?OnChildNotify@CWnd@@MAEHIIJPAJ@Z", OnChildNotify),
        ("?CheckAutoCenter@CWnd@@UAEHXZ", ret1),
        ("?IsFrameWnd@CWnd@@UBEHXZ", nop),
        ("?SetOccDialogInfo@CWnd@@MAEHPAU_AFX_OCC_DIALOG_INFO@@@Z", nop),
        ("?Default@CWnd@@IAEJXZ", Default),
        ("?FromHandle@CWnd@@SGPAV1@PAUHWND__@@@Z", FromHandle),
        ("?FromHandlePermanent@CWnd@@SGPAV1@PAUHWND__@@@Z", FromHandlePermanent),
        ("?Attach@CWnd@@QAEHPAUHWND__@@@Z", Attach),
        ("?Detach@CWnd@@QAEPAUHWND__@@XZ", Detach),
        ("?SubclassWindow@CWnd@@QAEHPAUHWND__@@@Z", SubclassWindow),
        ("?SubclassDlgItem@CWnd@@QAEHIPAV1@@Z", SubclassDlgItem),
        ("?SetWindowTextA@CWnd@@QAEXPBD@Z", SetWindowTextA),
        ("?GetWindowTextA@CWnd@@QBEHPADH@Z", GetWindowTextA),
        ("?GetWindowTextLengthA@CWnd@@QBEHXZ", GetWindowTextLengthA),
        ("?ShowWindow@CWnd@@QAEHH@Z", ShowWindow),
        ("?EnableWindow@CWnd@@QAEHH@Z", EnableWindow),
        ("?SetDlgItemTextA@CWnd@@QAEXHPBD@Z", SetDlgItemTextA),
        ("?GetDlgItemTextA@CWnd@@QBEHHPADH@Z", GetDlgItemTextA),
        ("?SetDlgItemInt@CWnd@@QAEXHIH@Z", SetDlgItemInt),
        ("?GetDlgItemInt@CWnd@@QBEIHPAHH@Z", GetDlgItemInt),
        ("?CheckDlgButton@CWnd@@QAEXHI@Z", CheckDlgButton),
        ("?IsDlgButtonChecked@CWnd@@QBEIH@Z", IsDlgButtonChecked),
        ("?SendDlgItemMessageA@CWnd@@QAEJHIIJ@Z", SendDlgItemMessageA),
        ("?MoveWindow@CWnd@@QAEXHHHHH@Z", MoveWindow),
        ("?GetDlgItem@CWnd@@QBEPAV1@H@Z", GetDlgItem),
        ("?SetFocus@CWnd@@QAEPAV1@XZ", SetFocus),
        ("?MessageBoxA@CWnd@@QAEHPBD0I@Z", MessageBoxA),
        ("?CenterWindow@CWnd@@QAEXPAV1@@Z", CenterWindow),
        ("?GetSafeOwner@CWnd@@SGPAV1@PAV1@PAPAUHWND__@@@Z", GetSafeOwner),
        ("?RunModalLoop@CWnd@@QAEHK@Z", RunModalLoop),
        ("?WalkPreTranslateTree@CWnd@@SGHPAUHWND__@@PAUtagMSG@@@Z", WalkPreTranslateTree),
        ("?UpdateData@CWnd@@QAEHH@Z", UpdateData),
        ("?ExecuteDlgInit@CWnd@@QAEHPBD@Z", ExecuteDlgInit),
        ("?IsWindowEnabled@CWnd@@QBEHXZ", IsWindowEnabled),
        // CDialog
        ("??0CDialog@@QAE@XZ", CDialog_ctor),
        ("??0CDialog@@QAE@IPAVCWnd@@@Z", CDialog_ctor_id),
        ("??0CDialog@@QAE@PBDPAVCWnd@@@Z", CDialog_ctor_name),
        ("??1CDialog@@UAE@XZ", CDialog_dtor),
        ("?GetMessageMap@CDialog@@MBEPBUAFX_MSGMAP@@XZ", GetMessageMap_CDialog),
        ("?OnCmdMsg@CDialog@@UAEHIHPAXPAUAFX_CMDHANDLERINFO@@@Z", OnCmdMsg_CDialog),
        ("?PreTranslateMessage@CDialog@@UAEHPAUtagMSG@@@Z", PreTranslateMessage_CDialog),
        ("?CheckAutoCenter@CDialog@@UAEHXZ", CheckAutoCenter_CDialog),
        ("?SetOccDialogInfo@CDialog@@MAEHPAU_AFX_OCC_DIALOG_INFO@@@Z", SetOccDialogInfo_CDialog),
        ("?DoModal@CDialog@@UAEHXZ", DoModal),
        ("?OnInitDialog@CDialog@@UAEHXZ", OnInitDialog),
        ("?OnSetFont@CDialog@@UAEXPAVCFont@@@Z", nop),
        ("?OnOK@CDialog@@MAEXXZ", OnOK),
        ("?OnCancel@CDialog@@MAEXXZ", OnCancel),
        ("?PreInitDialog@CDialog@@MAEXXZ", nop),
        ("?Create@CDialog@@QAEHPBDPAVCWnd@@@Z", Create_CDialog),
        ("?CreateIndirect@CDialog@@QAEHPBUDLGTEMPLATE@@PAVCWnd@@PAX@Z", CreateIndirect),
        ("?CreateIndirect@CDialog@@QAEHPBUDLGTEMPLATE@@PAVCWnd@@@Z", CreateIndirect_40),
        ("?EndDialog@CDialog@@QAEXH@Z", EndDialog),
        ("?InitModalIndirect@CDialog@@QAEHPBUDLGTEMPLATE@@PAVCWnd@@PAX@Z", InitModalIndirect),
        // DDX
        ("?DDX_Text@@YGXPAVCDataExchange@@HAAVCString@@@Z", DDX_Text_CString),
        ("?DDX_Text@@YGXPAVCDataExchange@@HAAH@Z", DDX_Text_int),
        ("?DDX_Text@@YGXPAVCDataExchange@@HAAI@Z", DDX_Text_int),
        ("?DDX_Text@@YGXPAVCDataExchange@@HAAJ@Z", DDX_Text_int),
        ("?DDX_Text@@YGXPAVCDataExchange@@HAAK@Z", DDX_Text_int),
        ("?DDX_Check@@YGXPAVCDataExchange@@HAAH@Z", DDX_Check),
        ("?DDX_Control@@YGXPAVCDataExchange@@HAAVCWnd@@@Z", DDX_Control),
        ("?PrepareCtrl@CDataExchange@@QAEPAUHWND__@@H@Z", PrepareCtrl),
        ("?PrepareEditCtrl@CDataExchange@@QAEPAUHWND__@@H@Z", PrepareEditCtrl),
        // controls (their destructors are CWnd's; the window is destroyed with the CWnd)
        ("??1CProgressCtrl@@UAE@XZ", CWnd_dtor),
        ("??1CButton@@UAE@XZ", CWnd_dtor),
        ("??1CEdit@@UAE@XZ", CWnd_dtor),
        ("??1CStatic@@UAE@XZ", CWnd_dtor),
        ("??1CListBox@@UAE@XZ", CWnd_dtor),
        ("??1CComboBox@@UAE@XZ", CWnd_dtor),
        ("??1CSliderCtrl@@UAE@XZ", CWnd_dtor),
        // CDC
        ("??0CDC@@QAE@XZ", CDC_ctor),
        ("??1CDC@@UAE@XZ", CDC_dtor),
        ("?Attach@CDC@@QAEHPAUHDC__@@@Z", CDC_Attach),
        ("?Detach@CDC@@QAEPAUHDC__@@XZ", CDC_Detach),
        ("?FromHandle@CDC@@SGPAV1@PAUHDC__@@@Z", CDC_FromHandle),
        ("??0CPaintDC@@QAE@PAVCWnd@@@Z", CPaintDC_ctor),
        ("??1CPaintDC@@UAE@XZ", CPaintDC_dtor),
        ("??0CClientDC@@QAE@PAVCWnd@@@Z", CClientDC_ctor),
        ("??1CClientDC@@UAE@XZ", CClientDC_dtor),
        ("??0CWindowDC@@QAE@PAVCWnd@@@Z", CWindowDC_ctor),
        ("??1CWindowDC@@UAE@XZ", CClientDC_dtor),
        // CException / CFileException
        ("??0CException@@QAE@XZ", CException_ctor),
        ("??0CException@@QAE@H@Z", CException_ctor_b),
        ("??1CException@@UAE@XZ", nop),
        ("?Delete@CException@@QAEXXZ", Delete_CException),
        ("?GetErrorMessage@CException@@UAEHPADIPAI@Z", GetErrorMessage_CException),
        ("?GetErrorMessage@CFileException@@UAEHPADIPAI@Z", GetErrorMessage_CFileException),
        ("?ReportError@CException@@UAEHII@Z", ReportError),
        ("?OsErrorToException@CFileException@@SGHJ@Z", os_error_to_exception),
        // CFile
        ("??0CFile@@QAE@XZ", CFile_ctor),
        ("??0CFile@@QAE@H@Z", CFile_ctor_h),
        ("??0CFile@@QAE@PBDI@Z", CFile_ctor_open),
        ("??1CFile@@UAE@XZ", CFile_dtor),
        ("?Open@CFile@@UAEHPBDIPAVCFileException@@@Z", Open),
        ("?Read@CFile@@UAEIPAXI@Z", Read),
        ("?Write@CFile@@UAEXPBXI@Z", Write),
        ("?Close@CFile@@UAEXXZ", Close),
        ("?Abort@CFile@@UAEXXZ", Abort),
        ("?Seek@CFile@@UAEJJI@Z", Seek),
        ("?GetPosition@CFile@@UBEKXZ", GetPosition),
        ("?GetLength@CFile@@UBEKXZ", GetLength),
        ("?SetLength@CFile@@UAEXK@Z", SetLength),
        ("?Flush@CFile@@UAEXXZ", Flush),
        ("?GetFilePath@CFile@@UBE?AVCString@@XZ", GetFilePath),
        ("?GetFileName@CFile@@UBE?AVCString@@XZ", GetFileName),
        ("?SetFilePath@CFile@@UAEXPBD@Z", SetFilePath),
        ("?LockRange@CFile@@UAEXKK@Z", nop),
        ("?UnlockRange@CFile@@UAEXKK@Z", nop),
    ]
}
fn get_type_lib(_: &mut Ctx) -> u32 {
    0x8002_9c4a
}
fn set_current_handles_(c: &mut Ctx) -> u32 {
    let o = this(c);
    set_current_handles(c, o);
    0
}
fn os_error_to_exception(c: &mut Ctx) -> u32 {
    os_error_cause(c.a(0))
}
/// CWnd::CalcWindowRect(lpClientRect, nAdjustType): AdjustWindowRectEx with the window's styles
fn CalcWindowRect(c: &mut Ctx) -> u32 {
    let (h, r) = (hwnd(c), c.a(0));
    let ex = user(c, "GetWindowLongA", &[h, (-20i32) as u32]);
    let st = user(c, "GetWindowLongA", &[h, (-16i32) as u32]);
    user(c, "AdjustWindowRectEx", &[r, st, 0, ex]);
    0
}

/// MFC42U's name of an ANSI export whose strings are TCHARs (LPCTSTR `PBD` -> `PBG`, LPTSTR
/// `PAD` -> `PAG`, an A function -> its W twin: GetWindowTextA -> GetWindowTextW)
pub fn unicode_name(n: &str) -> String {
    let w = n.replace("PBD", "PBG").replace("PAD", "PAG").replace("tagTOOLINFOA@", "tagTOOLINFOW@").replace("tagCREATESTRUCTA@", "tagCREATESTRUCTW@");
    match w.split_once('@') {
        Some((f, rest)) if f.ends_with('A') && !f.starts_with("??") => format!("{}W@{rest}", &f[..f.len() - 1]),
        _ => w,
    }
}
/// (DLL, name, handler) of every export done here: the table's names in each DLL that exports
/// them, and MFC42U's Unicode names of the TCHAR ones
pub fn registrations() -> Vec<(&'static str, String, Handler)> {
    let t = table();
    let names: std::collections::HashSet<&str> = t.iter().map(|x| x.0).collect();
    let mut out = Vec::new();
    for &(n, f) in &t {
        for &d in DLLS {
            if api::exports(d, n) {
                out.push((d, n.to_string(), f));
            }
        }
        let w = unicode_name(n);
        if w != n && !names.contains(w.as_str()) && api::exports("mfc42u", &w) {
            out.push(("mfc42u", w, f));
        }
    }
    for &d in DLLS {
        // every class's GetRuntimeClass
        for n in api::spec_names(d) {
            if n.starts_with("?GetRuntimeClass@") {
                out.push((d, n, GetRuntimeClass as Handler));
            }
        }
    }
    out
}
pub fn init() {
    for (d, n, f) in registrations() {
        implement(d, &n, f);
    }
    let _ = ret_this;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_guest::enter;
    use proptest::prelude::*;

    fn tcall_(f: Handler, this: u32, args: &[u32]) -> u32 {
        let sp = mem::heap_alloc(8 + 4 * args.len() as u32, true);
        for (i, &a) in args.iter().enumerate() {
            w32(sp + 4 + 4 * i as u32, a);
        }
        let mut cpu = koji::Cpu::default();
        cpu.regs[1] = this;
        let r = f(&mut Ctx { cpu: &mut cpu, esp: sp, edx: None, jumped: false, st0: None, trap: 0, this: None });
        mem::heap_free(sp);
        r
    }
    /// Units of ASCII / ANSI bytes
    fn u(s: &[u8]) -> Vec<u16> {
        s.iter().map(|&b| b as u16).collect()
    }
    /// A native string (TCHARs) in guest memory
    fn sz_(s: &[u16]) -> u32 {
        strdup(s)
    }
    /// Runs `f` with MFC42U's (wide) or MFC42's TCHARs, back to MFC42's after
    fn with_wide<R>(w: bool, f: impl FnOnce() -> R) -> R {
        with_dll(if w { "mfc42u" } else { "mfc42" }, f)
    }
    /// Runs `f` on the MFC of `dll`, back to MFC42's after
    fn with_dll<R>(dll: &str, f: impl FnOnce() -> R) -> R {
        use_dll(dll);
        let r = f();
        use_dll("mfc42");
        r
    }
    /// The native constructor and operators of the DLL in use (PBD in MFC42, PBG in MFC42U)
    fn ctor_sz() -> Handler {
        if wide() { cs_ctor_sz::<true> } else { cs_ctor_sz::<false> }
    }
    fn new_cs(s: &[u16]) -> u32 {
        let o = alloc(4);
        let p = sz_(s);
        tcall_(ctor_sz(), o, &[p]);
        free(p);
        o
    }
    /// The header of a CString as MFC 4.2 keeps it
    fn check(s: u32) {
        let p = r32(s);
        if p == cs::nil() {
            assert_eq!(cs::refs(p), -1);
            assert_eq!(cs::len(p), 0);
            assert_eq!(r16(p), 0);
            return;
        }
        assert!(cs::refs(p) >= 1);
        assert!(cs::len(p) <= cs::cap(p));
        // (a size class, or the exact length above 512, at least the length)
        assert!(matches!(cs::cap(p), 64 | 128 | 256 | 512) || cs::cap(p) > 512);
        assert_eq!(rd(p + cs::len(p) * unit(), 1), [0]);
    }

    #[test]
    fn empty_string_is_shared_nil() {
        let _g = enter();
        let a = alloc(4);
        tcall_(cs_ctor, a, &[]);
        let b = new_cs(&[]);
        assert_eq!(r32(a), cs::nil());
        assert_eq!(r32(b), cs::nil());
        // (programs read the length at [p - 8] inline)
        assert_eq!(r32(r32(a) - 8), 0);
        tcall_(cs_dtor, a, &[]);
        tcall_(cs_dtor, b, &[]);
        assert_eq!(cs::refs(cs::nil()), -1);
    }

    #[test]
    fn copy_shares_and_writes_copy() {
        let _g = enter();
        let a = new_cs(&u(b"abc"));
        let b = alloc(4);
        tcall_(cs_ctor_copy, b, &[a]);
        assert_eq!(r32(a), r32(b));
        assert_eq!(cs::refs(r32(a)), 2);
        // a write unshares
        tcall_(cs_append_ch::<false>, b, &[b'd' as u32]);
        assert_ne!(r32(a), r32(b));
        assert_eq!(cs::bytes(a), u(b"abc"));
        assert_eq!(cs::bytes(b), u(b"abcd"));
        assert_eq!(cs::refs(r32(a)), 1);
        check(a);
        check(b);
        tcall_(cs_dtor, a, &[]);
        tcall_(cs_dtor, b, &[]);
    }

    #[test]
    fn alloc_sizes_are_mfcs() {
        assert_eq!(cs::alloc_len(1), 64);
        assert_eq!(cs::alloc_len(64), 64);
        assert_eq!(cs::alloc_len(65), 128);
        assert_eq!(cs::alloc_len(256), 256);
        assert_eq!(cs::alloc_len(300), 512);
        assert_eq!(cs::alloc_len(513), 513);
    }

    #[test]
    fn trim_left_and_plus() {
        let _g = enter();
        let a = new_cs(&u(b" \t\r\nC:\\game "));
        tcall_(cs_trim_left, a, &[]);
        assert_eq!(cs::bytes(a), u(b"C:\\game "));
        check(a);
        let r = alloc(4);
        let p = sz_(&u(b"\\x"));
        tcall_(cs_plus_sp::<false>, 0, &[r, a, p]);
        assert_eq!(cs::bytes(r), u(b"C:\\game \\x"));
        check(r);
        let r2 = alloc(4);
        tcall_(cs_plus_ps::<false>, 0, &[r2, p, a]);
        assert_eq!(cs::bytes(r2), u(b"\\xC:\\game "));
        for s in [a, r, r2] {
            tcall_(cs_dtor, s, &[]);
        }
        free(p);
    }

    #[test]
    fn trim_right_keeps_double_byte_characters() {
        let _g = enter();
        // (a Shift-JIS trail byte 0x20 isn't a space: "ー" is 81 5B, a "0x81 0x20" pair keeps)
        assert_eq!(cs::trim_right(&u(b"ab  ")), u(b"ab"));
        assert_eq!(cs::trim_right(&u(b"\x81\x20")), u(b"\x81\x20"));
        assert_eq!(cs::trim_right(&u(b"   ")), u(b""));
        assert_eq!(cs::trim_left(&u(b"  x ")), u(b"x "));
    }

    /// MFC42U: CString holds UTF-16 (the length in characters, 2-byte NUL), its ANSI forms
    /// convert, iswspace trims the ideographic space, a CJK unit 0x5C2F isn't a '\' or a '/'
    #[test]
    fn unicode_cstring() {
        let _g = enter();
        with_wide(true, || {
            let text: Vec<u16> = "\u{3000} 日本語 ".encode_utf16().collect();
            let a = new_cs(&text);
            assert_eq!(cs::len(r32(a)), text.len() as u32);
            assert_eq!(r16(r32(a) + 2 * text.len() as u32), 0);
            tcall_(cs_trim_left, a, &[]);
            tcall_(cs_trim_right, a, &[]);
            assert_eq!(String::from_utf16_lossy(&cs::bytes(a)), "日本語");
            check(a);
            // operator=(LPCSTR) converts from the ANSI code page
            let p = mem::heap_alloc(8, true);
            mem::write(p, b"ab");
            tcall_(cs_assign_sz::<false>, a, &[p]);
            assert_eq!(cs::bytes(a), u(b"ab"));
            tcall_(cs_append_ch::<true>, a, &[0x3042]);
            assert_eq!(String::from_utf16_lossy(&cs::bytes(a)), "abあ");
            assert_eq!(file_start(&[0x5c2f, b'x' as u16]), 0);
            assert_eq!(file_start(&u(b"C:\\x\\y.exe")), 5);
            tcall_(cs_dtor, a, &[]);
        });
    }

    #[test]
    fn unicode_names_of_tchar_exports() {
        assert_eq!(unicode_name("?SetWindowTextA@CWnd@@QAEXPBD@Z"), "?SetWindowTextW@CWnd@@QAEXPBG@Z");
        assert_eq!(unicode_name("?GetBuffer@CString@@QAEPADH@Z"), "?GetBuffer@CString@@QAEPAGH@Z");
        assert_eq!(unicode_name("?PreCreateWindow@CWnd@@UAEHAAUtagCREATESTRUCTA@@@Z"), "?PreCreateWindow@CWnd@@UAEHAAUtagCREATESTRUCTW@@@Z");
        assert_eq!(unicode_name("??0CWnd@@QAE@XZ"), "??0CWnd@@QAE@XZ");
    }

    #[derive(Debug, Clone)]
    enum Op {
        AssignSz(Vec<u16>),
        AssignOther,
        AppendSz(Vec<u16>),
        AppendCh(u16),
        TrimLeft,
        TrimRight,
        Empty,
        CopyFromOther,
        Upper,
    }
    /// Text units: ASCII and spaces (and, for MFC42U, kana and the ideographic space)
    fn op(wide: bool) -> impl Strategy<Value = Op> {
        let unit = if wide { prop_oneof![Just(0x20u16), Just(9), Just(0x3000), 0x21u16..0x7f, 0x3041u16..0x3097].boxed() } else { prop_oneof![Just(0x20u16), Just(9), 0x21u16..0x7f].boxed() };
        let text = proptest::collection::vec(unit.clone(), 0..140);
        prop_oneof![
            text.clone().prop_map(Op::AssignSz),
            Just(Op::AssignOther),
            text.prop_map(Op::AppendSz),
            unit.prop_map(Op::AppendCh),
            Just(Op::TrimLeft),
            Just(Op::TrimRight),
            Just(Op::Empty),
            Just(Op::CopyFromOther),
            Just(Op::Upper),
        ]
    }
    fn ops() -> impl Strategy<Value = (bool, Vec<(Op, bool)>)> {
        any::<bool>().prop_flat_map(|w| (Just(w), proptest::collection::vec((op(w), any::<bool>()), 1..40)))
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]
        /// Two CStrings under random operations stay what a model (the units) says, with
        /// headers MFC's: reference counts, the size classes, the terminator; shared buffers
        /// counted. MFC42's (bytes) and MFC42U's (UTF-16) alike.
        #[test]
        fn cstring_matches_model((wide, ops) in ops()) {
            let _g = enter();
            with_wide(wide, || -> Result<(), TestCaseError> {
                let (assign_sz, append_sz, append_ch): (Handler, Handler, Handler) = if wide { (cs_assign_sz::<true>, cs_append_sz::<true>, cs_append_ch::<true>) } else { (cs_assign_sz::<false>, cs_append_sz::<false>, cs_append_ch::<false>) };
                let s = [new_cs(&[]), new_cs(&u(b"seed"))];
                let mut m: [Vec<u16>; 2] = [Vec::new(), u(b"seed")];
                for (o, which) in ops {
                    let (i, j) = if which { (0, 1) } else { (1, 0) };
                    match o {
                        Op::AssignSz(b) => { let p = sz_(&b); tcall_(assign_sz, s[i], &[p]); free(p); m[i] = b; }
                        Op::AssignOther => { tcall_(cs_assign_string, s[i], &[s[j]]); m[i] = m[j].clone(); }
                        Op::AppendSz(b) => { let p = sz_(&b); tcall_(append_sz, s[i], &[p]); free(p); m[i].extend_from_slice(&b); }
                        Op::AppendCh(ch) => { tcall_(append_ch, s[i], &[ch as u32]); m[i].push(ch); }
                        Op::TrimLeft => { tcall_(cs_trim_left, s[i], &[]); m[i] = cs::trim_left(&m[i]).to_vec(); }
                        Op::TrimRight => { tcall_(cs_trim_right, s[i], &[]); m[i] = cs::trim_right(&m[i]).to_vec(); }
                        Op::Empty => { tcall_(cs_empty, s[i], &[]); m[i].clear(); }
                        Op::CopyFromOther => { tcall_(cs_dtor, s[i], &[]); tcall_(cs_ctor_copy, s[i], &[s[j]]); m[i] = m[j].clone(); }
                        Op::Upper => { tcall_(cs_make_upper, s[i], &[]); for x in m[i].iter_mut() { if (0x61..=0x7a).contains(x) { *x -= 0x20; } } }
                    }
                    for k in 0..2 {
                        prop_assert_eq!(&cs::bytes(s[k]), &m[k]);
                        check(s[k]);
                    }
                    let (p, q) = (r32(s[0]), r32(s[1]));
                    if p == q && p != cs::nil() {
                        prop_assert_eq!(cs::refs(p), 2);
                    } else {
                        for p in [p, q] {
                            if p != cs::nil() { prop_assert_eq!(cs::refs(p), 1); }
                        }
                    }
                }
                for k in s { tcall_(cs_dtor, k, &[]); }
                Ok(())
            })?;
        }

        /// Mid / Left / Right clamp as MFC 4.2's do (never past the string)
        #[test]
        fn substrings_clamp(b in proptest::collection::vec(0x21u16..0x7f, 0..80), first in -5i32..90, count in -5i32..90) {
            let r = sub(&b, first, count);
            let f = first.clamp(0, b.len() as i32) as usize;
            prop_assert!(r.len() <= b.len() - f);
            prop_assert_eq!(&b[f..f + r.len()], r.as_slice());
        }

        /// CenterWindow keeps the window inside the work area when it fits there, centred on
        /// the centre window otherwise
        #[test]
        fn centre_inside_area(w in 1i32..900, h in 1i32..700, cx in -500i32..2500, cy in -500i32..1500, cw in 0i32..1500, ch in 0i32..1000) {
            let area = (0, 0, 1920, 1040);
            let (x, y) = center_pos((0, 0, w, h), (cx, cy, cx + cw, cy + ch), area);
            prop_assert!(x >= 0 && x + w <= 1920);
            prop_assert!(y >= 0 && y + h <= 1040);
            let (ux, uy) = ((2 * cx + cw) / 2 - w / 2, (2 * cy + ch) / 2 - h / 2);
            if ux >= 0 && ux + w <= 1920 { prop_assert_eq!(x, ux); }
            if uy >= 0 && uy + h <= 1040 { prop_assert_eq!(y, uy); }
        }
    }

    #[test]
    fn message_entries_found_by_range() {
        let _g = enter();
        let e = mem::heap_alloc(24 * 3, true);
        let ent = [[0x111u32, 0, 0x3e9, 0x3e9, 12, 0x1111], [0x111, 0x300, 10, 20, 13, 0x2222]];
        for (i, x) in ent.iter().enumerate() {
            for (k, &v) in x.iter().enumerate() {
                w32(e + 24 * i as u32 + 4 * k as u32, v);
            }
        }
        assert_eq!(find_entry(e, 0x111, 0, 0x3e9), Some(e));
        assert_eq!(find_entry(e, 0x111, 0, 0x3ea), None);
        assert_eq!(find_entry(e, 0x111, 0x300, 15), Some(e + 24));
        assert_eq!(find_entry(e, 0x111, 0x300, 21), None);
        assert_eq!(find_entry(e, 0x110, 0, 0), None);
    }

    #[test]
    fn signatures_pass_mfcs_arguments() {
        // (MFC 4.2's OnWndMsg: OnSize(UINT, int cx, int cy), OnLButtonDown(UINT, CPoint),
        // OnPaint(), OnQueryDragIcon() returning the cursor, OnHScroll(UINT, UINT, CScrollBar*))
        let none = |_| 0;
        let w = 0x0002_0001;
        let l = 0xffff_fffe;
        assert_eq!(sig_plan(16), Some((&[A::W, A::LoL, A::HiL][..], R::Void)));
        assert_eq!(plan_args(sig_plan(16).unwrap().0, w, 0x0300_0400, 0, none, none, none), vec![w, 0x400, 0x300]);
        assert_eq!(plan_args(sig_plan(49).unwrap().0, w, l, 0, none, none, none), vec![w, (-2i32) as u32, u32::MAX]);
        assert_eq!(sig_plan(12), Some((&[][..], R::Void)));
        assert_eq!(sig_plan(35), Some((&[][..], R::Store)));
        assert_eq!(plan_args(sig_plan(29).unwrap().0, 0xfffe_0005, 0x77, 0, |h| h + 1, none, none), vec![5, (-2i32) as u32, 0x78]);
        assert_eq!(plan_args(sig_plan(19).unwrap().0, 1, 0x50, 0x50, |h| h, none, none), vec![1, 0x50, 1]);
        assert_eq!(sig_plan(0), None);
        assert_eq!(sig_plan(50), None);
    }

    #[test]
    fn open_flags_map_to_createfile() {
        // modeRead, modeWrite | modeCreate, modeReadWrite | modeCreate | modeNoTruncate | shareDenyWrite
        assert_eq!(open_args(0), (0x8000_0000, 0, 3));
        assert_eq!(open_args(0x1001), (0x4000_0000, 0, 2));
        assert_eq!(open_args(0x3022), (0xc000_0000, 1, 4));
        assert_eq!(open_args(0x40), (0x8000_0000, 3, 3));
    }

    #[test]
    fn file_errors_map_to_causes() {
        assert_eq!(os_error_cause(2), 2);
        assert_eq!(os_error_cause(3), 3);
        assert_eq!(os_error_cause(5), 5);
        assert_eq!(os_error_cause(0x20), 11);
        assert_eq!(os_error_cause(0x70), 13);
        assert_eq!(os_error_cause(0x26), 14);
        assert_eq!(os_error_cause(0x1234), 1);
        assert!(file_error_text(2).contains("%1"));
    }

    #[test]
    fn message_box_gets_mfcs_default_icon() {
        assert_eq!(message_box_type(0), 0x30);
        assert_eq!(message_box_type(4), 0x34);
        assert_eq!(message_box_type(0x10), 0x10);
        assert_eq!(message_box_type(2), 2);
    }

    #[test]
    fn idle_messages() {
        let (mut pt, mut m) = ((0, 0), 0);
        assert!(is_idle_message(0x200, (5, 5), &mut pt, &mut m));
        assert!(!is_idle_message(0x200, (5, 5), &mut pt, &mut m));
        assert!(is_idle_message(0x200, (6, 5), &mut pt, &mut m));
        assert!(!is_idle_message(0xf, (0, 0), &mut pt, &mut m));
        assert!(!is_idle_message(0x118, (0, 0), &mut pt, &mut m));
        assert!(is_idle_message(0x100, (0, 0), &mut pt, &mut m));
    }

    /// Every export done here is in specs.txt (so its arguments are popped as MFC's are), and
    /// every vtable slot names an export (or a stub of sake's)
    #[test]
    fn implemented_exports_are_in_the_spec() {
        let names: std::collections::HashSet<String> = api::spec_names("mfc42").into_iter().collect();
        let unames: std::collections::HashSet<String> = api::spec_names("mfc42u").into_iter().collect();
        let names40: std::collections::HashSet<String> = api::spec_names("mfc40").into_iter().collect();
        let missing: Vec<&str> = table().iter().map(|x| x.0).filter(|n| !names.contains(*n) && !unames.contains(*n) && !names40.contains(*n)).collect();
        assert!(missing.is_empty(), "not mfc42 / mfc42u / mfc40 exports: {missing:?}");
        // MFC42U has (under its own names) everything MFC42's table does
        let regs = registrations();
        let u: std::collections::HashSet<&str> = regs.iter().filter(|r| r.0 == "mfc42u").map(|r| r.1.as_str()).collect();
        let a: Vec<&str> = regs.iter().filter(|r| r.0 == "mfc42").map(|r| r.1.as_str()).collect();
        // (CString's TCHAR `D` parameters are `G` there)
        let twin = |n: &str| n.replace("@DH@", "@GH@").replace("@D@Z", "@G@Z").replace("@@DABV0", "@@GABV0").replace("HD@Z", "HG@Z");
        let lost: Vec<&&str> = a.iter().filter(|n| !u.contains(**n) && !u.contains(unicode_name(n).as_str()) && !u.contains(twin(n).as_str())).collect();
        assert!(lost.is_empty(), "not in mfc42u: {lost:?}");
        for class in ["CObject", "CCmdTarget", "CWinThread", "CWinApp", "CWnd", "CDialog", "CProgressCtrl", "CFile", "CException", "CFileException", "CPaintDC"] {
            let s = slots(class);
            let want = match class {
                "CObject" | "CPaintDC" => 5,
                "CCmdTarget" => 22,
                "CWinThread" => 33,
                "CWinApp" => 41,
                "CWnd" | "CProgressCtrl" => 48,
                "CDialog" => 54,
                "CFile" => 22,
                _ => 7,
            };
            assert_eq!(s.len(), want, "{class}");
            let bad: Vec<&&str> = s.iter().filter(|n| !(n.is_empty() || n.starts_with('~') || n.starts_with('-') || names.contains(**n))).collect();
            assert!(bad.is_empty(), "{class}: {bad:?}");
            let ubad: Vec<&&str> = s.iter().filter(|n| !(n.is_empty() || n.starts_with('~') || n.starts_with('-') || unames.contains(**n) || unames.contains(&unicode_name(n)))).collect();
            assert!(ubad.is_empty(), "mfc42u {class}: {ubad:?}");
            if !["CObject", "CPaintDC", "CFile", "CException", "CFileException"].contains(&class) {
                assert!(s[12].starts_with("?GetMessageMap@"), "{class}");
            }
        }
        // (the slots the program's vtables hold: sinsabell_bm.exe's CDialog-derived class)
        let d = slots("CDialog");
        assert_eq!(d[40], "?WindowProc@CWnd@@MAEJIIJ@Z");
        assert_eq!(d[48], "?DoModal@CDialog@@UAEHXZ");
        assert_eq!(d[51], "?OnOK@CDialog@@MAEXXZ");
        assert_eq!(slots("CWinApp")[37], "?DoMessageBox@CWinApp@@UAEHPBDII@Z");
        assert_eq!(slots("CWinApp")[25], "?PumpMessage@CWinThread@@UAEHXZ");
    }

    /// MFC 4.0 (mfc40.dll 4.1.6140's vtables, read from its constructors): no GetCommandMap,
    /// so every CCmdTarget class's later slots are one lower; CWinThread's fields after +0x54
    /// and CWinApp's 8 lower; the exports this table names there are its own or MFC 4.2's
    #[test]
    fn mfc40_layouts() {
        let _g = enter();
        with_dll("mfc40", || {
            let names: std::collections::HashSet<String> = api::spec_names("mfc40").into_iter().chain(api::spec_names("mfc42")).collect();
            for (class, want) in [("CCmdTarget", 21), ("CWinThread", 32), ("CWinApp", 40), ("CWnd", 47), ("CDialog", 53), ("CFile", 22), ("CException", 7)] {
                let s = slots(class);
                assert_eq!(s.len(), want, "{class}");
                let bad: Vec<&&str> = s.iter().filter(|n| !(n.is_empty() || n.starts_with('~') || n.starts_with('-') || names.contains(**n))).collect();
                assert!(bad.is_empty(), "{class}: {bad:?}");
            }
            let (app, dlg, wnd) = (slots("CWinApp"), slots("CDialog"), slots("CWnd"));
            assert_eq!(app[21], "?InitInstance@CWinThread@@UAEHXZ");
            assert_eq!(app[34], "?InitApplication@CWinApp@@UAEHXZ");
            assert_eq!(app[36], "?DoMessageBox@CWinApp@@UAEHPBDII@Z");
            assert_eq!(wnd[39], "?WindowProc@CWnd@@MAEJIIJ@Z");
            assert_eq!(wnd[41], "?DefWindowProcA@CWnd@@MAEJIIJ@Z");
            assert_eq!(dlg[47], "?DoModal@CDialog@@UAEHXZ");
            assert_eq!(dlg[50], "?OnOK@CDialog@@MAEXXZ");
            // (what `vcall`'s MFC 4.2 numbers come to)
            let o = alloc(0x60);
            w32(o, vtable("CDialog"));
            assert_eq!(api::name_of(r32(r32(o) + 4 * 47)).unwrap(), "mfc40!?DoModal@CDialog@@UAEHXZ");
            free(o);
            assert_eq!((th(0x54), th(0x60), th(0x6c), th(0x7c), th(0xbc)), (0x54, 0x58, 0x64, 0x74, 0xb4));
        });
        assert_eq!(th(0x7c), 0x7c);
    }
}
