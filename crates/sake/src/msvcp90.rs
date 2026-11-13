//! msvcp90.dll (Visual C++ 2008) for 32-bit programs: std::basic_string<char>'s out-of-line
//! members, which programs built against the VC++ 2008 runtime import (the DLL comes from the
//! redistributable's WinSxS, not with the game: 悪堕ラビリンス 体験版's lus004.exe, #136).
//!
//! The layout is VC9's (release, x86): the allocator (empty, 4 bytes with padding) at 0, the
//! union _Bx { char _Buf[16]; char *_Ptr; } at 4, _Mysize at 20, _Myres at 24 (28 bytes); the
//! characters are in _Buf while _Myres < 16. The game's inline code reads these fields, so the
//! sizes, capacities and growth (_Grow / _Copy / _Tidy / _Eos) are VC9's. Members are thiscall
//! (this in ECX, the callee pops the rest: specs.txt); memory comes from msvcr90's operator
//! new / delete (the guest heap).

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::mem::{self, r32, w32, w8};

const BX: u32 = 4;
const SIZE: u32 = 20;
const RES: u32 = 24;
/// _BUF_SIZE - 1: _Myres of an empty string
const ALLOC_MASK: u32 = 15;
pub const NPOS: u32 = u32::MAX;
/// basic_string<char>::max_size(): allocator<char>::max_size() - 1
const MAX_SIZE: u32 = u32::MAX - 1;

fn this(c: &Ctx) -> u32 {
    c.cpu.regs[1]
}

pub fn ptr(s: u32) -> u32 {
    if r32(s + RES) > ALLOC_MASK { r32(s + BX) } else { s + BX }
}
pub fn size(s: u32) -> u32 {
    r32(s + SIZE)
}
pub fn capacity(s: u32) -> u32 {
    r32(s + RES)
}
pub fn bytes(s: u32) -> Vec<u8> {
    mem::slice(ptr(s), size(s)).to_vec()
}

fn alloc(n: u32) -> u32 {
    mem::heap_alloc_small(n.max(1), false)
}
fn dealloc(p: u32) {
    mem::heap_free_small(p);
}

/// _Eos(n): the size, and the terminator after it
fn eos(s: u32, n: u32) {
    w32(s + SIZE, n);
    w8(ptr(s) + n, 0);
}

/// _Tidy(built, newsize): back to the inside buffer (keeping the first `newsize` characters)
fn tidy(s: u32, built: bool, newsize: u32) {
    if !built {
    } else if capacity(s) > ALLOC_MASK {
        let p = r32(s + BX);
        if newsize > 0 {
            let keep = mem::slice(p, newsize).to_vec();
            mem::write(s + BX, &keep);
        }
        dealloc(p);
    }
    w32(s + RES, ALLOC_MASK);
    eos(s, newsize);
}

/// _Copy(newsize, oldlen): a bigger buffer, the first `oldlen` characters kept
fn copy(s: u32, newsize: u32, oldlen: u32) {
    let res = capacity(s);
    let mut newres = newsize | ALLOC_MASK;
    if MAX_SIZE < newres {
        newres = newsize;
    } else if newres / 3 < res / 2 && res <= MAX_SIZE - res / 2 {
        newres = res + res / 2;
    }
    let p = alloc(newres + 1);
    if oldlen > 0 {
        let old = mem::slice(ptr(s), oldlen).to_vec();
        mem::write(p, &old);
    }
    tidy(s, true, 0);
    w32(s + BX, p);
    w32(s + RES, newres);
    eos(s, oldlen);
}

/// _Grow(newsize): room for `newsize` characters; whether there are any
fn grow(c: &mut Ctx, s: u32, newsize: u32) -> bool {
    if MAX_SIZE < newsize {
        crate::fatal(c.cpu, "msvcp90: basic_string too long (std::length_error is not thrown here)");
    }
    if capacity(s) < newsize {
        copy(s, newsize, size(s));
    } else if newsize == 0 {
        eos(s, 0);
    }
    newsize > 0
}

/// assign(const char *, count) of characters outside the string
fn assign_bytes(c: &mut Ctx, s: u32, b: &[u8]) {
    let n = b.len() as u32;
    if grow(c, s, n) {
        mem::write(ptr(s), b);
        eos(s, n);
    }
}

/// _Tidy() of a constructor: an empty string
fn init(s: u32) {
    w32(s + RES, ALLOC_MASK);
    eos(s, 0);
}

/// ??0?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@QAE@XZ
fn ctor(c: &mut Ctx) -> u32 {
    let s = this(c);
    w32(s, 0);
    init(s);
    s
}
/// ??0...@QAE@PBD@Z (const char *)
fn ctor_cstr(c: &mut Ctx) -> u32 {
    let (s, p) = (this(c), c.a(0));
    w32(s, 0);
    init(s);
    let b = mem::cstr(p);
    assign_bytes(c, s, &b);
    s
}
/// ??0...@QAE@ABV01@@Z (const basic_string &)
fn ctor_copy(c: &mut Ctx) -> u32 {
    let (s, r) = (this(c), c.a(0));
    w32(s, 0);
    init(s);
    let b = bytes(r);
    assign_bytes(c, s, &b);
    s
}
/// ??1...@QAE@XZ
fn dtor(c: &mut Ctx) -> u32 {
    tidy(this(c), true, 0);
    0
}
/// ??4...@QAEAAV01@ABV01@@Z (const basic_string &): assign(right, 0, npos); itself: unchanged
fn assign_copy(c: &mut Ctx) -> u32 {
    let (s, r) = (this(c), c.a(0));
    if s != r {
        let b = bytes(r);
        assign_bytes(c, s, &b);
    }
    s
}
/// ?c_str@...@QBEPBDXZ
fn c_str(c: &mut Ctx) -> u32 {
    ptr(this(c))
}
/// ?size@...@QBEIXZ
fn size_fn(c: &mut Ctx) -> u32 {
    size(this(c))
}

/// find(const char *, off, count): the first position at or after `off` where `pat` starts
pub fn find_at(hay: &[u8], pat: &[u8], off: u32) -> u32 {
    let (n, k, off) = (hay.len(), pat.len(), off as usize);
    if k == 0 && off <= n {
        return off as u32;
    }
    if off < n && k <= n - off {
        if let Some(i) = hay[off..].windows(k).position(|w| w == pat) {
            return (off + i) as u32;
        }
    }
    NPOS
}
/// ?find@...@QBEIPBDI@Z (const char *, off)
fn find(c: &mut Ctx) -> u32 {
    let (s, p, off) = (this(c), c.a(0), c.a(1));
    find_at(&bytes(s), &mem::cstr(p), off)
}
/// ?substr@...@QBE?AV12@II@Z (hidden result, off, count): basic_string(*this, off, count)
fn substr(c: &mut Ctx) -> u32 {
    let (s, out, off, count) = (this(c), c.a(0), c.a(1), c.a(2));
    let n = size(s);
    if off > n {
        crate::fatal(c.cpu, "msvcp90: basic_string::substr position past the end (std::out_of_range is not thrown here)");
    }
    let num = count.min(n - off);
    let b = mem::slice(ptr(s) + off, num).to_vec();
    w32(out, 0);
    init(out);
    assign_bytes(c, out, &b);
    out
}

/// The data exports: ?npos@...@2IB
pub fn data(name: &str) -> Option<u32> {
    if name != "?npos@?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@2IB" {
        return None;
    }
    static P: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    Some(*P.get_or_init(|| {
        let p = mem::heap_alloc(4, true);
        w32(p, NPOS);
        p
    }))
}

pub fn init_dll() {
    const S: &str = "?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@";
    for (name, f) in [
        (format!("??0{S}QAE@XZ"), ctor as crate::api::Handler),
        (format!("??0{S}QAE@PBD@Z"), ctor_cstr),
        (format!("??0{S}QAE@ABV01@@Z"), ctor_copy),
        (format!("??1{S}QAE@XZ"), dtor),
        (format!("??4{S}QAEAAV01@ABV01@@Z"), assign_copy),
        (format!("?c_str@{S}QBEPBDXZ"), c_str),
        (format!("?size@{S}QBEIXZ"), size_fn),
        (format!("?find@{S}QBEIPBDI@Z"), find),
        (format!("?substr@{S}QBE?AV12@II@Z"), substr),
    ] {
        implement("msvcp90", &name, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_guest::enter;
    use proptest::prelude::*;

    /// a thiscall member with `this` in ECX and the stack arguments
    fn tcall(f: crate::api::Handler, this: u32, args: &[u32]) -> u32 {
        let sp = mem::heap_alloc(4 + 4 * args.len() as u32, true);
        for (i, &a) in args.iter().enumerate() {
            w32(sp + 4 + 4 * i as u32, a);
        }
        let mut cpu = koji::Cpu::default();
        cpu.regs[1] = this;
        let r = f(&mut Ctx { cpu: &mut cpu, esp: sp, edx: None, jumped: false, st0: None, trap: 0 });
        mem::heap_free(sp);
        r
    }
    fn cstring(b: &[u8]) -> u32 {
        let p = mem::heap_alloc(b.len() as u32 + 1, true);
        mem::write(p, b);
        p
    }
    fn new_str(b: &[u8]) -> u32 {
        let s = mem::heap_alloc(28, true);
        let p = cstring(b);
        assert_eq!(tcall(ctor_cstr, s, &[p]), s);
        mem::heap_free(p);
        s
    }
    /// The string's invariants: VC9's capacity rule, the terminator, the characters.
    fn check(s: u32, want: &[u8]) {
        assert_eq!(bytes(s), want);
        assert_eq!(mem::r8(ptr(s) + size(s)), 0);
        assert!(capacity(s) >= size(s) && capacity(s) >= ALLOC_MASK);
        assert_eq!(ptr(s) == s + BX, capacity(s) == ALLOC_MASK);
    }

    #[test]
    fn vc9_layout_and_growth() {
        let _g = enter();
        let s = mem::heap_alloc(28, true);
        tcall(ctor, s, &[]);
        assert_eq!((size(s), capacity(s), mem::r8(s + BX)), (0, 15, 0));
        let short = new_str(b"0123456789abcde");
        assert_eq!((size(short), capacity(short), ptr(short)), (15, 15, short + BX));
        // (16 characters: a heap buffer of 16 | 15 = 31)
        let long = new_str(b"0123456789abcdef");
        assert_eq!((size(long), capacity(long)), (16, 31));
        assert_ne!(ptr(long), long + BX);
        // assigning longer to a 31: max(n | 15, 31 + 15)
        let big = new_str(&[b'x'; 40]);
        assert_eq!(capacity(big), 47);
        tcall(assign_copy, long, &[big]);
        check(long, &[b'x'; 40]);
        assert_eq!(capacity(long), 47);
        // shorter: the buffer stays
        tcall(assign_copy, long, &[short]);
        check(long, b"0123456789abcde");
        assert_eq!(capacity(long), 47);
        // itself
        tcall(assign_copy, long, &[long]);
        check(long, b"0123456789abcde");
        for x in [s, short, long, big] {
            tcall(dtor, x, &[]);
            assert_eq!(capacity(x), 15);
        }
        let n = data("?npos@?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@2IB").unwrap();
        assert_eq!(r32(n), u32::MAX);
    }

    #[test]
    fn find_and_substr() {
        let _g = enter();
        let s = new_str(b"data/script/main.txt");
        let pat = cstring(b"/");
        assert_eq!(tcall(find, s, &[pat, 0]), 4);
        assert_eq!(tcall(find, s, &[pat, 5]), 11);
        assert_eq!(tcall(find, s, &[pat, 12]), NPOS);
        let empty = cstring(b"");
        assert_eq!(tcall(find, s, &[empty, 20]), 20);
        assert_eq!(tcall(find, s, &[empty, 21]), NPOS);
        let out = mem::heap_alloc(28, true);
        assert_eq!(tcall(substr, s, &[out, 5, NPOS]), out);
        check(out, b"script/main.txt");
        let out2 = mem::heap_alloc(28, true);
        tcall(substr, s, &[out2, 20, 3]);
        check(out2, b"");
    }

    proptest! {
        /// Against Rust's own strings: construction, copies, assignment, find, substr; the VC9
        /// capacity rule holds after every step and destruction frees back to the buffer.
        #[test]
        fn model(a in proptest::collection::vec(1u8..=255, 0..70), b in proptest::collection::vec(1u8..=255, 0..70),
                 pat in proptest::collection::vec(1u8..=255, 0..4), off in 0u32..80, count in prop_oneof![Just(NPOS), 0u32..80]) {
            let _g = enter();
            let sa = new_str(&a);
            check(sa, &a);
            let sb = mem::heap_alloc(28, true);
            tcall(ctor_copy, sb, &[sa]);
            check(sb, &a);
            let sc = new_str(&b);
            let before = capacity(sb);
            tcall(assign_copy, sb, &[sc]);
            check(sb, &b);
            // (a capacity never shrinks on assignment)
            prop_assert!(capacity(sb) >= before);
            let p = cstring(&pat);
            let want = if pat.is_empty() {
                if off as usize <= a.len() { off } else { NPOS }
            } else {
                (off as usize..a.len()).find(|&i| a[i..].starts_with(&pat)).map_or(NPOS, |i| i as u32)
            };
            prop_assert_eq!(tcall(find, sa, &[p, off]), want);
            if off as usize <= a.len() {
                let out = mem::heap_alloc(28, true);
                tcall(substr, sa, &[out, off, count]);
                let end = (off as usize).saturating_add(count as usize).min(a.len());
                check(out, &a[off as usize..end]);
                tcall(dtor, out, &[]);
                mem::heap_free(out);
            }
            for s in [sa, sb, sc] {
                tcall(dtor, s, &[]);
                prop_assert_eq!(capacity(s), 15);
                prop_assert_eq!(size(s), 0);
                mem::heap_free(s);
            }
            mem::heap_free(p);
        }
    }
}
