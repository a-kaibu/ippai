//! printf's formatting: C11 7.21.6.1 with the MS CRT's format specification syntax
//! (learn.microsoft.com "Format specification syntax: printf and wprintf functions"):
//!
//!   %[flags][width][.precision][size]type
//!
//! flags `- + space # 0`; width and precision digits or `*` (an int argument; a negative width
//! left-justifies, a negative precision is none); sizes `hh h l ll j z t L w I I32 I64`; types
//! `d i u o x X e E f F g G a A c C s S Z p n %`.
//!
//! MS specifics: `long` is 32 bits; `I`, `z`, `t` are pointer-sized; `%p` is the pointer in
//! uppercase hex, zero-padded to its full width; `%s`/`%c` mean the function's own character
//! width and `%S`/`%C` the other one (h forces narrow, l/w wide); `%Z` is an ANSI_STRING /
//! UNICODE_STRING; `%a` has 13 hex digits by default. Floating point: the UCRT prints the exact
//! value rounded half to even ("inf", "nan", "nan(ind)", "nan(snan)", two-digit exponents); the
//! legacy msvcrt rounds exact ties away from zero and prints 1.#INF / 1.#QNAN / 1.#SNAN / 1.#IND
//! with three-digit exponents.
//!
//! Errors (the UCRT's invalid parameter cases, the caller returns -1): an unknown conversion, a
//! `%` ending the format, `%n` when it is not enabled, output beyond [`MAX_OUT`].

use crate::dec::{self, Tie};
use crate::Unit;

/// The output limit (units): beyond it formatting fails (the CRT's limit is INT_MAX; an absurd
/// width must not exhaust the host's memory).
pub const MAX_OUT: usize = 1 << 26;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    /// 64-bit program: `I`, `z`, `t` and `%p` are 8 bytes
    pub ptr64: bool,
    /// msvcrt.dll / msvcr70-120 (see the module comment)
    pub legacy: bool,
    /// three-digit exponents (legacy, or the UCRT's _CRT_INTERNAL_PRINTF_LEGACY_THREE_DIGIT_EXPONENTS)
    pub three_digit_exp: bool,
    /// `%s`/`%c` without a size are wide in a wide function: the legacy CRT, and the UCRT with
    /// _CRT_INTERNAL_PRINTF_LEGACY_WIDE_SPECIFIERS (what its headers pass by default). Without it
    /// (the ISO mode) `%s` is narrow everywhere and `%ls` wide.
    pub legacy_wide: bool,
    /// `%n` stores the count (the UCRT after _set_printf_count_output(1)). Without it the
    /// UCRT fails and the legacy CRT skips the argument (msvcrt.dll on Windows writes nothing).
    pub allow_n: bool,
    /// the _printf_p family: `%n$` positions (crate::fmt::format_p; [`format`] ignores this)
    pub positional: bool,
}

impl Mode {
    /// msvcrt.dll and the msvcr*.dll runtimes.
    pub fn legacy(ptr64: bool) -> Mode {
        Mode { ptr64, legacy: true, three_digit_exp: true, legacy_wide: true, allow_n: false, positional: false }
    }
    /// ucrtbase with the options word of __stdio_common_v*printf.
    pub fn ucrt(ptr64: bool, options: u64) -> Mode {
        Mode {
            ptr64,
            legacy: options & 8 != 0,
            three_digit_exp: options & 0x10 != 0,
            legacy_wide: options & 4 != 0,
            allow_n: false,
            positional: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// an invalid format (the invalid parameter handler's case)
    Invalid,
    /// the output would exceed MAX_OUT
    TooLong,
}

/// Where the variable arguments and the strings they point to come from.
pub trait Args {
    /// The next integer argument, `bytes` (1, 2, 4 or 8) wide: its bits zero-extended.
    fn int(&mut self, bytes: u32) -> u64;
    /// The next floating-point argument (a double; long double is double in the MS CRT).
    fn float(&mut self) -> f64;
    /// The NUL-terminated ANSI string at `p`, at most `max` bytes of it.
    fn narrow(&mut self, p: u64, max: Option<usize>) -> Vec<u8>;
    /// The NUL-terminated UTF-16 string at `p`, at most `max` units of it.
    fn wide(&mut self, p: u64, max: Option<usize>) -> Vec<u16>;
    /// %Z: the (Length in bytes, Buffer) of the ANSI_STRING / UNICODE_STRING at `p`.
    fn counted(&mut self, p: u64) -> (usize, u64);
    /// ANSI (the process code page) to UTF-16, and back.
    fn a2w(&self, s: &[u8]) -> Vec<u16>;
    fn w2a(&self, s: &[u16]) -> Vec<u8>;
    /// %n: stores `v` as a `bytes`-wide integer at `p`.
    fn store(&mut self, p: u64, bytes: u32, v: u64);
}

#[derive(Clone, Copy, Default)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    prec: Option<usize>,
}

/// The size modifier: an integer's width in bytes and a string/character's kind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    None,
    Bytes(u32),
    /// h with c/s: narrow
    Short,
    /// l/w with c/s: wide
    Long,
    /// UCRT T with c/s: the output function's character width, even in ISO mode.
    Tchar,
}

/// Formats `fmt` (units up to its end; the caller strips the NUL) with the arguments `a`.
pub fn format<T: Unit>(fmt: &[T], m: Mode, a: &mut impl Args) -> Result<Vec<T>, Error> {
    let mut out: Vec<T> = Vec::with_capacity(fmt.len() + 16);
    let ch = |i: usize| fmt.get(i).map_or(0, |c| c.code());
    let mut i = 0;
    while i < fmt.len() {
        if ch(i) != b'%' as u32 {
            out.push(fmt[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= fmt.len() {
            if m.legacy {
                break;
            }
            return Err(Error::Invalid);
        }
        let mut s = Spec::default();
        loop {
            match ch(i) {
                0x2d => s.left = true,
                0x2b => s.plus = true,
                0x20 => s.space = true,
                0x23 => s.alt = true,
                0x30 => s.zero = true,
                _ => break,
            }
            i += 1;
        }
        if ch(i) == b'*' as u32 {
            let w = a.int(4) as u32 as i32;
            if w < 0 {
                s.left = true;
            }
            s.width = w.unsigned_abs() as usize;
            i += 1;
        } else {
            s.width = number(fmt, &mut i);
        }
        if ch(i) == b'.' as u32 {
            i += 1;
            if ch(i) == b'*' as u32 {
                let p = a.int(4) as u32 as i32;
                s.prec = (p >= 0).then_some(p as usize);
                i += 1;
            } else {
                s.prec = Some(number(fmt, &mut i));
            }
        }
        if s.width > MAX_OUT || s.prec.is_some_and(|p| p > MAX_OUT) {
            return Err(Error::TooLong);
        }
        let ptr = if m.ptr64 { 8 } else { 4 };
        let size = match ch(i) as u8 {
            // (the legacy CRT reads hh as h: %hhd of 255 is 255)
            b'h' if ch(i + 1) == b'h' as u32 => {
                i += 2;
                if m.legacy { Size::Short } else { Size::Bytes(1) }
            }
            b'h' => {
                i += 1;
                Size::Short
            }
            b'l' if ch(i + 1) == b'l' as u32 => {
                i += 2;
                Size::Bytes(8)
            }
            b'l' | b'w' => {
                i += 1;
                Size::Long
            }
            b'T' if !m.legacy => {
                i += 1;
                Size::Tchar
            }
            b'L' | b'j' => {
                i += 1;
                Size::Bytes(8)
            }
            b'z' | b't' => {
                i += 1;
                Size::Bytes(ptr)
            }
            b'I' if ch(i + 1) == b'6' as u32 && ch(i + 2) == b'4' as u32 => {
                i += 3;
                Size::Bytes(8)
            }
            b'I' if ch(i + 1) == b'3' as u32 && ch(i + 2) == b'2' as u32 => {
                i += 3;
                Size::Bytes(4)
            }
            b'I' => {
                i += 1;
                Size::Bytes(ptr)
            }
            _ => Size::None,
        };
        if i >= fmt.len() {
            if m.legacy {
                break;
            }
            return Err(Error::Invalid);
        }
        let ty = ch(i);
        i += 1;
        let ty = if ty < 0x80 { ty as u8 } else { 0 };
        match ty {
            b'd' | b'i' => {
                let bytes = int_bytes(size);
                let raw = a.int(bytes);
                let v = sign_extend(raw, bytes);
                let sign = if v < 0 {
                    "-"
                } else if s.plus {
                    "+"
                } else if s.space {
                    " "
                } else {
                    ""
                };
                let body = int_digits(v.unsigned_abs(), 10, false, s.prec);
                pad_number(&mut out, &s, sign.as_bytes(), &body, s.prec.is_none());
            }
            b'u' | b'o' | b'x' | b'X' => {
                let bytes = int_bytes(size);
                let v = a.int(bytes) & mask(bytes);
                let base = match ty {
                    b'u' => 10,
                    b'o' => 8,
                    _ => 16,
                };
                let mut body = int_digits(v, base, ty == b'X', s.prec);
                let mut prefix: &[u8] = b"";
                if s.alt {
                    if base == 8 && body.first() != Some(&b'0') {
                        body.insert(0, b'0');
                    } else if base == 16 && v != 0 {
                        prefix = if ty == b'X' { b"0X" } else { b"0x" };
                    }
                }
                pad_number(&mut out, &s, prefix, &body, s.prec.is_none());
            }
            b'p' => {
                let v = a.int(ptr) & mask(ptr);
                let body = int_digits(v, 16, true, Some(2 * ptr as usize));
                let s2 = Spec { prec: None, ..s };
                // (the legacy CRT's %#p: 0X00001234)
                let prefix: &[u8] = if s.alt && m.legacy { b"0X" } else { b"" };
                pad_number(&mut out, &s2, prefix, &body, true);
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
                let v = a.float();
                let (prefix, body, finite) = float(v, ty, &s, m);
                pad_number(&mut out, &s, &prefix, &body, finite);
            }
            b'c' | b'C' => {
                let wide = char_is_wide::<T>(ty == b'C', size, m);
                let v = a.int(4) as u32;
                let body: Vec<T> = if wide { from_wide(&[v as u16], a) } else { from_narrow(&[v as u8], a) };
                pad_text(&mut out, &s, &body);
            }
            b's' | b'S' => {
                let wide = char_is_wide::<T>(ty == b'S', size, m);
                let p = a.int(ptr) & mask(ptr);
                let body: Vec<T> = if p == 0 {
                    let mut v: Vec<T> = b"(null)".iter().map(|&b| T::from_ascii(b)).collect();
                    if let Some(pr) = s.prec {
                        v.truncate(pr);
                    }
                    v
                } else {
                    string(p, wide, s.prec, a)
                };
                pad_text(&mut out, &s, &body);
            }
            b'Z' => {
                let wide = char_is_wide::<T>(false, size, m);
                let p = a.int(ptr) & mask(ptr);
                let (len, buf) = if p == 0 { (0, 0) } else { a.counted(p) };
                let body: Vec<T> = if p == 0 || buf == 0 {
                    b"(null)".iter().map(|&b| T::from_ascii(b)).collect()
                } else if wide {
                    let w = a.wide(buf, Some(len / 2));
                    let w: Vec<u16> = w.into_iter().chain(std::iter::repeat(0)).take(len / 2).collect();
                    truncate(from_wide(&w, a), s.prec)
                } else {
                    let n = a.narrow(buf, Some(len));
                    let n: Vec<u8> = n.into_iter().chain(std::iter::repeat(0)).take(len).collect();
                    truncate(from_narrow(&n, a), s.prec)
                };
                pad_text(&mut out, &s, &body);
            }
            b'n' => {
                if !m.allow_n && !m.legacy {
                    return Err(Error::Invalid);
                }
                let bytes = int_bytes(size);
                let p = a.int(ptr) & mask(ptr);
                if p != 0 && m.allow_n {
                    a.store(p, bytes, out.len() as u64);
                }
            }
            b'%' => out.push(T::from_ascii(b'%')),
            _ => {
                if !m.legacy {
                    return Err(Error::Invalid);
                }
                // (the legacy CRT prints the character it did not understand)
                out.push(fmt[i - 1]);
            }
        }
        if out.len() > MAX_OUT {
            return Err(Error::TooLong);
        }
    }
    // (literal text after the last conversion counts too)
    if out.len() > MAX_OUT {
        return Err(Error::TooLong);
    }
    Ok(out)
}

/// The most positional arguments `_printf_p` takes (the CRT's _ARGMAX).
pub const MAX_POSITIONAL: usize = 100;

/// One argument as read from the caller's list.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Got {
    Int(u32, u64),
    Float(f64),
}

/// The kinds of argument reads a format makes, in order (its strings and %n are not read).
struct Reads(Vec<Got>);
impl Args for Reads {
    fn int(&mut self, bytes: u32) -> u64 {
        self.0.push(Got::Int(bytes, 0));
        0
    }
    fn float(&mut self) -> f64 {
        self.0.push(Got::Float(0.0));
        0.0
    }
    fn narrow(&mut self, _: u64, _: Option<usize>) -> Vec<u8> {
        Vec::new()
    }
    fn wide(&mut self, _: u64, _: Option<usize>) -> Vec<u16> {
        Vec::new()
    }
    fn counted(&mut self, _: u64) -> (usize, u64) {
        (0, 0)
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        s.iter().map(|&b| b as u16).collect()
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        s.iter().map(|&u| u as u8).collect()
    }
    fn store(&mut self, _: u64, _: u32, _: u64) {}
}

/// The arguments read once in position order, handed out in the order the format uses them;
/// strings and %n go to the caller's memory.
struct Replay<'a, A: Args> {
    inner: &'a mut A,
    vals: Vec<Got>,
    next: usize,
}
impl<A: Args> Args for Replay<'_, A> {
    fn int(&mut self, _: u32) -> u64 {
        self.next += 1;
        match self.vals.get(self.next - 1) {
            Some(Got::Int(_, v)) => *v,
            _ => 0,
        }
    }
    fn float(&mut self) -> f64 {
        self.next += 1;
        match self.vals.get(self.next - 1) {
            Some(Got::Float(v)) => *v,
            _ => 0.0,
        }
    }
    fn narrow(&mut self, p: u64, max: Option<usize>) -> Vec<u8> {
        self.inner.narrow(p, max)
    }
    fn wide(&mut self, p: u64, max: Option<usize>) -> Vec<u16> {
        self.inner.wide(p, max)
    }
    fn counted(&mut self, p: u64) -> (usize, u64) {
        self.inner.counted(p)
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        self.inner.a2w(s)
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        self.inner.w2a(s)
    }
    fn store(&mut self, p: u64, bytes: u32, v: u64) {
        self.inner.store(p, bytes, v)
    }
}

/// `n$` at `i` (digits then `$`): its position, `i` moved past it; None (and `i` kept) if there
/// is none.
fn position<T: Unit>(f: &[T], i: &mut usize) -> Option<usize> {
    let mut j = *i;
    let n = number(f, &mut j);
    if j > *i && f.get(j).map(|c| c.code()) == Some(b'$' as u32) {
        *i = j + 1;
        Some(n)
    } else {
        None
    }
}

/// The `_printf_p` family (MS "printf_p Positional Parameters"): `%n$` picks argument n for a
/// conversion, `*n$` for a width or precision. A format with none is plain printf. Otherwise
/// every conversion has its position (a mix is invalid), positions run from 1 to at most
/// [`MAX_POSITIONAL`] with none left out, and one used twice is read as the same type both times
/// (else invalid). The arguments are read in position order, then formatted in the format's.
pub fn format_p<T: Unit>(fmt: &[T], m: Mode, a: &mut impl Args) -> Result<Vec<T>, Error> {
    let ch = |i: usize| fmt.get(i).map_or(0, |c| c.code());
    // the format without its "n$" parts, and the position of each argument it reads, in order
    let mut plain: Vec<T> = Vec::with_capacity(fmt.len());
    let mut order: Vec<usize> = Vec::new();
    let (mut with, mut without) = (false, false);
    let mut i = 0;
    while i < fmt.len() {
        plain.push(fmt[i]);
        if ch(i) != b'%' as u32 {
            i += 1;
            continue;
        }
        i += 1;
        if ch(i) == b'%' as u32 {
            plain.push(fmt[i]);
            i += 1;
            continue;
        }
        let pos = position(fmt, &mut i);
        // flags
        while matches!(ch(i), 0x2d | 0x2b | 0x20 | 0x23 | 0x30) {
            plain.push(fmt[i]);
            i += 1;
        }
        // (a `*` takes a position exactly when its conversion has one)
        let star = |i: &mut usize, plain: &mut Vec<T>, order: &mut Vec<usize>| -> Result<(), Error> {
            if ch(*i) == b'*' as u32 {
                plain.push(fmt[*i]);
                *i += 1;
                match (position(fmt, i), pos) {
                    (Some(p), Some(_)) => order.push(p),
                    (None, None) => {}
                    _ => return Err(Error::Invalid),
                }
            } else {
                while (0x30..=0x39).contains(&ch(*i)) {
                    plain.push(fmt[*i]);
                    *i += 1;
                }
            }
            Ok(())
        };
        star(&mut i, &mut plain, &mut order)?;
        if ch(i) == b'.' as u32 {
            plain.push(fmt[i]);
            i += 1;
            star(&mut i, &mut plain, &mut order)?;
        }
        match pos {
            Some(p) => {
                with = true;
                order.push(p);
            }
            None => without = true,
        }
    }
    if !with {
        return format(fmt, m, a);
    }
    if without {
        return Err(Error::Invalid);
    }
    // what each read is (the conversions' sizes and types), then each position's kind
    let mut reads = Reads(Vec::new());
    format(&plain, m, &mut reads)?;
    if reads.0.len() != order.len() {
        return Err(Error::Invalid); // (%% inside a conversion, %n: positions that read nothing)
    }
    let max = order.iter().copied().max().unwrap_or(0);
    if order.contains(&0) || max > MAX_POSITIONAL {
        return Err(Error::Invalid);
    }
    let mut kind: Vec<Option<Got>> = vec![None; max];
    for (&p, &r) in order.iter().zip(&reads.0) {
        match kind[p - 1] {
            None => kind[p - 1] = Some(r),
            Some(k) if k == r => {}
            Some(_) => return Err(Error::Invalid),
        }
    }
    let mut vals = Vec::with_capacity(max);
    for k in &kind {
        vals.push(match k {
            None => return Err(Error::Invalid),
            Some(Got::Int(b, _)) => Got::Int(*b, a.int(*b)),
            Some(Got::Float(_)) => Got::Float(a.float()),
        });
    }
    let used: Vec<Got> = order.iter().map(|&p| vals[p - 1]).collect();
    format(&plain, m, &mut Replay { inner: a, vals: used, next: 0 })
}

fn number<T: Unit>(f: &[T], i: &mut usize) -> usize {
    let mut n = 0usize;
    while let Some(d) = f.get(*i).map(|c| c.code()).filter(|c| (0x30..=0x39).contains(c)) {
        n = n.saturating_mul(10).saturating_add((d - 0x30) as usize);
        *i += 1;
    }
    n
}

fn int_bytes(s: Size) -> u32 {
    match s {
        Size::Bytes(b) => b,
        Size::Short => 2,
        Size::None | Size::Long | Size::Tchar => 4,
    }
}

fn mask(bytes: u32) -> u64 {
    if bytes >= 8 { u64::MAX } else { (1u64 << (8 * bytes)) - 1 }
}

fn sign_extend(v: u64, bytes: u32) -> i64 {
    let sh = 64 - 8 * bytes.min(8);
    ((v << sh) as i64) >> sh
}

/// Whether a c/s conversion takes a wide argument: h narrow, l/w wide, else the function's own
/// width for the lowercase type (the other one for C/S) in the legacy mode; in the UCRT's ISO
/// mode lowercase is always narrow and uppercase the other width.
fn char_is_wide<T: Unit>(upper: bool, size: Size, m: Mode) -> bool {
    match size {
        Size::Short => false,
        Size::Long => true,
        Size::Tchar => T::WIDE,
        _ => {
            let own = T::WIDE && (m.legacy_wide || m.legacy);
            if upper { !own } else { own }
        }
    }
}

fn from_narrow<T: Unit>(s: &[u8], a: &impl Args) -> Vec<T> {
    if T::WIDE {
        a.a2w(s).into_iter().map(|u| T::from_code(u as u32)).collect()
    } else {
        s.iter().map(|&b| T::from_code(b as u32)).collect()
    }
}

fn from_wide<T: Unit>(s: &[u16], a: &impl Args) -> Vec<T> {
    if T::WIDE {
        s.iter().map(|&u| T::from_code(u as u32)).collect()
    } else {
        a.w2a(s).into_iter().map(|b| T::from_code(b as u32)).collect()
    }
}

fn truncate<T>(mut v: Vec<T>, prec: Option<usize>) -> Vec<T> {
    if let Some(p) = prec {
        v.truncate(p);
    }
    v
}

/// %s with a non-NULL pointer: the precision is the most units written (bytes in a narrow
/// function, UTF-16 units in a wide one); a converted string stops before a character that would
/// go past it.
fn string<T: Unit>(p: u64, wide: bool, prec: Option<usize>, a: &mut impl Args) -> Vec<T> {
    match (T::WIDE, wide) {
        (false, false) => from_narrow(&a.narrow(p, prec), a),
        (true, true) => from_wide(&a.wide(p, prec), a),
        (true, false) => {
            // each character is at most 2 bytes: twice the precision covers the ones kept
            let n = a.narrow(p, prec.map(|x| x.saturating_mul(2)));
            truncate(from_narrow(&n, a), prec)
        }
        (false, true) => {
            let w = a.wide(p, prec);
            let Some(limit) = prec else { return from_wide(&w, a) };
            let mut out = Vec::new();
            let mut k = 0;
            while k < w.len() {
                let n = if (0xd800..0xdc00).contains(&w[k]) && k + 1 < w.len() { 2 } else { 1 };
                let c: Vec<T> = from_wide(&w[k..k + n], a);
                if out.len() + c.len() > limit {
                    break;
                }
                out.extend(c);
                k += n;
            }
            out
        }
    }
}

/// Digits of `v` in `base`, at least `prec` of them (precision 0 and value 0: none).
fn int_digits(v: u64, base: u64, upper: bool, prec: Option<usize>) -> Vec<u8> {
    let set: &[u8; 16] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut d = Vec::new();
    let mut u = v;
    while u != 0 {
        d.push(set[(u % base) as usize]);
        u /= base;
    }
    let min = prec.unwrap_or(1);
    if d.len() < min {
        // (a precision of up to MAX_OUT zeros: at once, not a push each)
        d.resize(min, b'0');
    }
    d.reverse();
    d
}

/// Pads `prefix + body` (ASCII) to the width: spaces on the left (right with `-`), or zeros
/// after the prefix with `0` where zeros may pad (`zeros_ok`).
fn pad_number<T: Unit>(out: &mut Vec<T>, s: &Spec, prefix: &[u8], body: &[u8], zeros_ok: bool) {
    let len = prefix.len() + body.len();
    let pad = s.width.saturating_sub(len);
    let push = |out: &mut Vec<T>, b: &[u8]| out.extend(b.iter().map(|&c| T::from_ascii(c)));
    if s.left {
        push(out, prefix);
        push(out, body);
        out.extend(std::iter::repeat_n(T::from_ascii(b' '), pad));
    } else if s.zero && zeros_ok {
        push(out, prefix);
        out.extend(std::iter::repeat_n(T::from_ascii(b'0'), pad));
        push(out, body);
    } else {
        out.extend(std::iter::repeat_n(T::from_ascii(b' '), pad));
        push(out, prefix);
        push(out, body);
    }
}

/// Pads text (c, s, Z) with spaces (the MS CRT pads with zeros when `0` is given).
fn pad_text<T: Unit>(out: &mut Vec<T>, s: &Spec, body: &[T]) {
    let pad = s.width.saturating_sub(body.len());
    let fill = T::from_ascii(if s.zero && !s.left { b'0' } else { b' ' });
    if s.left {
        out.extend_from_slice(body);
        out.extend(std::iter::repeat_n(fill, pad));
    } else {
        out.extend(std::iter::repeat_n(fill, pad));
        out.extend_from_slice(body);
    }
}

/// e f g a and their capitals: (sign and 0x, body, whether zeros may pad it).
fn float(v: f64, ty: u8, s: &Spec, m: Mode) -> (Vec<u8>, Vec<u8>, bool) {
    let upper = ty.is_ascii_uppercase();
    let neg = v.is_sign_negative();
    let sign: &[u8] = if neg {
        b"-"
    } else if s.plus {
        b"+"
    } else if s.space {
        b" "
    } else {
        b""
    };
    let sign = sign.to_vec();
    let lower = ty.to_ascii_lowercase();
    if !v.is_finite() {
        let bits = v.to_bits();
        let quiet = bits & (1 << 51) != 0;
        let ind = bits == 0xfff8_0000_0000_0000;
        if m.legacy {
            let word: &[u8] = if v.is_infinite() {
                b"#INF"
            } else if ind {
                b"#IND"
            } else if quiet {
                b"#QNAN"
            } else {
                b"#SNAN"
            };
            return (sign, legacy_special(word, lower, s), true);
        }
        let word: &str = if v.is_infinite() {
            "inf"
        } else if ind {
            "nan(ind)"
        } else if quiet {
            "nan"
        } else {
            "nan(snan)"
        };
        let body = if upper { word.to_ascii_uppercase() } else { word.to_string() };
        return (sign, body.into_bytes(), false);
    }
    let tie = if m.legacy { Tie::Away } else { Tie::Even };
    let exp_digits = if m.three_digit_exp || m.legacy { 3 } else { 2 };
    // (the legacy CRT works from 17 significant digits: 1e308 prints 1797...57 then zeros)
    let (d, pt) = if m.legacy { digits17(v) } else { dec::digits(v) };
    let n = Num { d: &d, pt, tie };
    let body = match lower {
        b'f' => fixed(n, s.prec.unwrap_or(6), s.alt),
        b'e' => sci(n, s.prec.unwrap_or(6), s.alt, upper, exp_digits),
        // (the legacy CRT's %#g of zero has all its precision after the point)
        b'g' if m.legacy && s.alt && d.is_empty() => fixed(n, s.prec.unwrap_or(6).max(1), true),
        b'g' => general(n, s.prec.unwrap_or(6), s.alt, upper, exp_digits),
        _ => {
            let mut prefix = sign;
            prefix.extend_from_slice(if upper { b"0X" } else { b"0x" });
            return (prefix, hex(v, s.prec, s.alt, upper), true);
        }
    };
    (sign, body, true)
}

/// The legacy CRT's 1.#INF style: "1" and the word stand in for the digits, rounded like digits
/// (any character from '5' up rounds the last kept one up: %.2f of infinity is 1.#J, %.3f of
/// -1.#IND is -1.#IO); %e adds e+000 and %g drops the trailing zeros.
fn legacy_special(word: &[u8], ty: u8, s: &Spec) -> Vec<u8> {
    let prec = s.prec.unwrap_or(6);
    let keep = match ty {
        b'g' => prec.max(1),
        _ => prec + 1,
    };
    let all: Vec<u8> = b"1".iter().chain(word).copied().collect();
    let at = |i: usize| all.get(i).copied().unwrap_or(b'0');
    let mut r: Vec<u8> = (0..keep).map(at).collect();
    if at(keep) >= b'5' {
        // (the words are short: a carry never reaches the leading 1)
        if let Some(k) = (0..keep).rev().find(|&k| r[k] != b'9') {
            r[k] += 1;
            r[k + 1..].iter_mut().for_each(|c| *c = b'0');
        }
    }
    let mut b = vec![r[0]];
    if keep > 1 || s.alt {
        b.push(b'.');
    }
    b.extend_from_slice(&r[1..]);
    if ty == b'g' && !s.alt {
        while b.last() == Some(&b'0') {
            b.pop();
        }
        if b.last() == Some(&b'.') {
            b.pop();
        }
    }
    if ty == b'e' {
        b.extend_from_slice(b"e+000");
    }
    b
}

/// The legacy CRT's digits: |v| correctly rounded to 17 significant digits; what follows is
/// zeros.
fn digits17(v: f64) -> (Vec<u8>, i32) {
    let (d, p) = dec::digits(v);
    if d.len() <= 17 {
        return (d, p);
    }
    let (mut r, p) = dec::round(&d, p, 17, Tie::Even);
    while r.last() == Some(&0) {
        r.pop();
    }
    (r, p)
}

/// The decimal digits of a value (as dec::digits gives them) and how ties round.
#[derive(Clone, Copy)]
struct Num<'a> {
    d: &'a [u8],
    pt: i32,
    tie: Tie,
}

fn ascii(d: u8) -> u8 {
    b'0' + d
}

/// %f: `prec` digits after the point.
fn fixed(n: Num, prec: usize, alt: bool) -> Vec<u8> {
    let keep = n.pt.saturating_add(prec.min(i32::MAX as usize / 2) as i32);
    let (r, p) = dec::round(n.d, n.pt, keep, n.tie);
    let at = |i: i32| if i >= 0 && (i as usize) < r.len() { r[i as usize] } else { 0 };
    let mut out = Vec::with_capacity(p.max(1) as usize + prec + 2);
    if p <= 0 {
        out.push(b'0');
    } else {
        for i in 0..p {
            out.push(ascii(at(i)));
        }
    }
    if prec > 0 || alt {
        out.push(b'.');
    }
    for k in 0..prec as i32 {
        out.push(ascii(at(p + k)));
    }
    out
}

/// %e: one digit, the point, `prec` digits, then e±dd (at least `exp_digits` digits).
fn sci(n: Num, prec: usize, alt: bool, upper: bool, exp_digits: usize) -> Vec<u8> {
    let (r, x) = if n.d.is_empty() {
        (vec![0; prec + 1], 0)
    } else {
        let (r, p) = dec::round(n.d, n.pt, prec as i32 + 1, n.tie);
        (r, p - 1)
    };
    let mut out = vec![ascii(r[0])];
    if prec > 0 || alt {
        out.push(b'.');
    }
    out.extend(r[1..].iter().map(|&x| ascii(x)));
    exponent(&mut out, if upper { b'E' } else { b'e' }, x, exp_digits);
    out
}

fn exponent(out: &mut Vec<u8>, e: u8, x: i32, digits: usize) {
    out.push(e);
    out.push(if x < 0 { b'-' } else { b'+' });
    let s = x.unsigned_abs().to_string();
    out.extend(std::iter::repeat_n(b'0', digits.saturating_sub(s.len())));
    out.extend(s.bytes());
}

/// %g: precision P (0 is 1) significant digits; %e style when the exponent X < -4 or X >= P,
/// else %f with P-1-X decimals; trailing zeros (and a bare point) go unless `#`.
fn general(n: Num, prec: usize, alt: bool, upper: bool, exp_digits: usize) -> Vec<u8> {
    let p = prec.max(1);
    let x = if n.d.is_empty() { 0 } else { dec::round(n.d, n.pt, p as i32, n.tie).1 - 1 };
    let mut out = if (x as i64) < p as i64 && x >= -4 {
        fixed(n, (p as i64 - 1 - x as i64) as usize, alt)
    } else {
        sci(n, p - 1, alt, upper, exp_digits)
    };
    if !alt {
        let end = out.iter().position(|&c| c == b'e' || c == b'E').unwrap_or(out.len());
        if out[..end].contains(&b'.') {
            let mut cut = end;
            while out[cut - 1] == b'0' {
                cut -= 1;
            }
            if out[cut - 1] == b'.' {
                cut -= 1;
            }
            out.drain(cut..end);
        }
    }
    out
}

/// %a (after its 0x): the leading hex digit (1, or 0 for zero and subnormals), the point and the fraction's
/// 13 hex digits (or `prec`, rounded half to even), then p±exponent in decimal.
fn hex(v: f64, prec: Option<usize>, alt: bool, upper: bool) -> Vec<u8> {
    let bits = v.to_bits() & !(1 << 63);
    let (e, frac) = ((bits >> 52) as i64, bits & ((1 << 52) - 1));
    let (mut lead, exp) = if e == 0 { (0u64, if frac == 0 { 0 } else { -1022 }) } else { (1u64, e - 1023) };
    let mut f = frac;
    let n = prec.unwrap_or(13);
    let mut digits: Vec<u8> = Vec::new();
    if n < 13 {
        let drop = 4 * (13 - n as u32);
        let kept = f >> drop;
        let rest = f & ((1u64 << drop) - 1);
        let half = 1u64 << (drop - 1);
        let mut k = kept;
        if rest > half || (rest == half && kept & 1 == 1) || (n == 0 && rest == half && lead & 1 == 1) {
            k += 1;
        }
        if n == 0 {
            // (no fraction digits: the carry goes to the leading digit)
            if k != 0 {
                lead += 1;
            }
            f = 0;
        } else if k >> (4 * n) != 0 {
            lead += 1;
            f = 0;
        } else {
            f = k << drop;
        }
    }
    for i in 0..n.min(13) {
        digits.push(((f >> (48 - 4 * i)) & 15) as u8);
    }
    let set: &[u8; 16] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut out = vec![set[lead as usize]];
    if n > 0 || alt {
        out.push(b'.');
    }
    out.extend(digits.iter().map(|&d| set[d as usize]));
    out.extend(std::iter::repeat_n(b'0', n.saturating_sub(13)));
    out.push(if upper { b'P' } else { b'p' });
    out.push(if exp < 0 { b'-' } else { b'+' });
    out.extend(exp.unsigned_abs().to_string().bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Arguments from a list; strings live in `strs` (pointer = index + 1).
    pub struct List {
        pub ints: Vec<u64>,
        pub floats: Vec<f64>,
        pub strs: Vec<Vec<u16>>,
        pub stored: Vec<(u64, u32, u64)>,
    }
    impl Args for List {
        fn int(&mut self, bytes: u32) -> u64 {
            let v = if self.ints.is_empty() { 0 } else { self.ints.remove(0) };
            v & mask(bytes)
        }
        fn float(&mut self) -> f64 {
            if self.floats.is_empty() { 0.0 } else { self.floats.remove(0) }
        }
        fn narrow(&mut self, p: u64, max: Option<usize>) -> Vec<u8> {
            let s: Vec<u8> = self.strs[p as usize - 1].iter().map(|&u| u as u8).collect();
            s.into_iter().take(max.unwrap_or(usize::MAX)).collect()
        }
        fn wide(&mut self, p: u64, max: Option<usize>) -> Vec<u16> {
            self.strs[p as usize - 1].iter().copied().take(max.unwrap_or(usize::MAX)).collect()
        }
        fn counted(&mut self, p: u64) -> (usize, u64) {
            (self.strs[p as usize - 1].len(), p)
        }
        fn a2w(&self, s: &[u8]) -> Vec<u16> {
            s.iter().map(|&b| b as u16).collect()
        }
        fn w2a(&self, s: &[u16]) -> Vec<u8> {
            s.iter().map(|&u| if u < 0x100 { u as u8 } else { b'?' }).collect()
        }
        fn store(&mut self, p: u64, bytes: u32, v: u64) {
            self.stored.push((p, bytes, v));
        }
    }

    fn f(fmt: &str, ints: &[u64], floats: &[f64], m: Mode) -> String {
        let mut a = List { ints: ints.to_vec(), floats: floats.to_vec(), strs: vec![], stored: vec![] };
        let o = format(fmt.as_bytes(), m, &mut a).unwrap();
        String::from_utf8(o).unwrap()
    }
    fn u(fmt: &str, ints: &[u64], floats: &[f64]) -> String {
        f(fmt, ints, floats, Mode::ucrt(false, 0))
    }

    /// A va_list: integers and doubles in the caller's order (strings: index + 1 into `strs`).
    struct Seq {
        vals: Vec<Got>,
        strs: Vec<Vec<u8>>,
        stored: Vec<(u64, u32, u64)>,
    }
    impl Args for Seq {
        fn int(&mut self, bytes: u32) -> u64 {
            match (!self.vals.is_empty()).then(|| self.vals.remove(0)) {
                Some(Got::Int(_, v)) => v & mask(bytes),
                _ => 0xbad,
            }
        }
        fn float(&mut self) -> f64 {
            match (!self.vals.is_empty()).then(|| self.vals.remove(0)) {
                Some(Got::Float(v)) => v,
                _ => -0.5,
            }
        }
        fn narrow(&mut self, p: u64, max: Option<usize>) -> Vec<u8> {
            let s = (p as usize).checked_sub(1).and_then(|i| self.strs.get(i)).cloned().unwrap_or_default();
            s.into_iter().take(max.unwrap_or(usize::MAX)).collect()
        }
        fn wide(&mut self, p: u64, max: Option<usize>) -> Vec<u16> {
            self.narrow(p, max).into_iter().map(|b| b as u16).collect()
        }
        fn counted(&mut self, p: u64) -> (usize, u64) {
            (self.narrow(p, None).len(), p)
        }
        fn a2w(&self, s: &[u8]) -> Vec<u16> {
            s.iter().map(|&b| b as u16).collect()
        }
        fn w2a(&self, s: &[u16]) -> Vec<u8> {
            s.iter().map(|&u| u as u8).collect()
        }
        fn store(&mut self, p: u64, bytes: u32, v: u64) {
            self.stored.push((p, bytes, v));
        }
    }
    fn seq(vals: &[Got]) -> Seq {
        Seq { vals: vals.to_vec(), strs: vec![b"November".to_vec(), b"10".to_vec()], stored: vec![] }
    }
    fn p(fmt: &str, vals: &[Got]) -> Result<String, Error> {
        format_p(fmt.as_bytes(), Mode::ucrt(false, 0), &mut seq(vals)).map(|o| String::from_utf8(o).unwrap())
    }
    use Got::{Float as F, Int as I};

    // MS's "printf_p Positional Parameters" and its rules
    #[test]
    fn positional_arguments() {
        // (the documentation's example: strings 1 and 2, in either order)
        assert_eq!(p("%1$s %2$s", &[I(4, 1), I(4, 2)]).unwrap(), "November 10");
        assert_eq!(p("%2$s %1$s", &[I(4, 1), I(4, 2)]).unwrap(), "10 November");
        assert_eq!(p("%2$d %1$d %2$d", &[I(4, 7), I(4, 9)]).unwrap(), "9 7 9");
        // an int and a double in either order of the list; 64-bit and 32-bit
        assert_eq!(p("%2$.2f/%1$d", &[I(4, 3), F(2.5)]).unwrap(), "2.50/3");
        assert_eq!(p("%1$.2f/%2$I64x", &[F(0.25), I(8, 1 << 40)]).unwrap(), "0.25/10000000000");
        // width and precision from positions
        assert_eq!(p("[%2$*1$d]", &[I(4, 5), I(4, 42)]).unwrap(), "[   42]");
        assert_eq!(p("[%3$-*1$.*2$f]", &[I(4, 8), I(4, 1), F(3.14159)]).unwrap(), "[3.1     ]");
        assert_eq!(p("%1$c%2$%", &[I(4, b'x' as u64)]), Err(Error::Invalid)); // (%% has no position)
        assert_eq!(p("100%% %1$d", &[I(4, 5)]).unwrap(), "100% 5");
        // %n through a position
        let mut a = seq(&[I(4, 77)]);
        assert_eq!(format_p(b"ab%1$n", Mode { allow_n: true, ..Mode::ucrt(false, 0) }, &mut a).unwrap(), b"ab");
        assert_eq!(a.stored, [(77, 4, 2)]);
        // no position anywhere: printf as it is
        assert_eq!(p("%d %s", &[I(4, 1), I(4, 2)]).unwrap(), "1 10");
        // invalid: a mix, a gap, position 0, one past _ARGMAX, one position read as two types,
        // a * without its position (or with one in a conversion without)
        for bad in ["%1$d %d", "%d %1$d", "%1$d %3$d", "%0$d", "%101$d", "%1$d %1$f", "%1$d %1$I64d", "%2$*d", "%*1$d", "%1$.*d"] {
            assert_eq!(p(bad, &[I(4, 1), I(4, 2), I(4, 3)]), Err(Error::Invalid), "{bad}");
        }
        let ok100: String = (1..=100).rev().map(|i| format!("%{i}$d,")).collect();
        let vals: Vec<Got> = (1..=100).map(|i| I(4, i)).collect();
        let want: String = (1..=100).rev().map(|i| format!("{i},")).collect();
        assert_eq!(p(&ok100, &vals).unwrap(), want);
        // a bad conversion is still one
        assert_eq!(p("%1$y", &[I(4, 1)]), Err(Error::Invalid));
    }

    proptest::proptest! {
        /// A format of conversions given in some order with positions is the plain format of
        /// the same conversions in that order, with the list's arguments in that order: any
        /// permutation, any repeats, ints of 4 and 8 bytes, doubles and widths from arguments.
        #[test]
        fn positions_are_a_reordering(
            kinds in proptest::collection::vec((0u8..4, proptest::prelude::any::<u64>(), -1e9f64..1e9), 1..8),
            picks in proptest::collection::vec(proptest::prelude::any::<proptest::sample::Index>(), 0..12),
            seed in proptest::prelude::any::<u64>(),
        ) {
            let n = kinds.len();
            // every position at least once, then repeats, in a shuffled order
            let mut uses: Vec<usize> = (0..n).collect();
            uses.extend(picks.iter().map(|ix| ix.index(n)));
            let mut r = seed | 1;
            for k in (1..uses.len()).rev() {
                r ^= r << 13; r ^= r >> 7; r ^= r << 17;
                uses.swap(k, (r % (k as u64 + 1)) as usize);
            }
            let conv = |k: u8, pos: Option<usize>| {
                let at = pos.map_or(String::new(), |p| format!("{}$", p + 1));
                match k { 0 => format!("%{at}d"), 1 => format!("%{at}I64x"), 2 => format!("%{at}.3f"), _ => format!("%{at}u") }
            };
            let val = |i: usize| match kinds[i].0 { 0 | 3 => I(4, kinds[i].1 & 0xffff_ffff), 1 => I(8, kinds[i].1), _ => F(kinds[i].2) };
            let pf: String = uses.iter().map(|&i| conv(kinds[i].0, Some(i)) + "|").collect();
            let plain: String = uses.iter().map(|&i| conv(kinds[i].0, None) + "|").collect();
            let list: Vec<Got> = (0..n).map(val).collect();
            let in_order: Vec<Got> = uses.iter().map(|&i| val(i)).collect();
            let got = format_p(pf.as_bytes(), Mode::ucrt(false, 0), &mut seq(&list)).unwrap();
            let want = format(plain.as_bytes(), Mode::ucrt(false, 0), &mut seq(&in_order)).unwrap();
            proptest::prop_assert_eq!(got, want);
            // (and without positions, format_p is format)
            let again = format_p(plain.as_bytes(), Mode::ucrt(false, 0), &mut seq(&in_order)).unwrap();
            let want = format(plain.as_bytes(), Mode::ucrt(false, 0), &mut seq(&in_order)).unwrap();
            proptest::prop_assert_eq!(again, want);
        }

        /// Any format at all: format_p never panics, and one without `$` is format's result.
        #[test]
        fn any_format_p(f in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..40)) {
            let vals: Vec<Got> = (0..8).map(|i| I(4, i)).collect();
            let got = format_p(&f, Mode::ucrt(false, 0), &mut seq(&vals));
            if !f.contains(&b'$') {
                proptest::prop_assert_eq!(got, format(&f, Mode::ucrt(false, 0), &mut seq(&vals)));
            }
        }
    }

    #[test]
    fn tchar_modifier_follows_output_width_including_uppercase() {
        for options in [0, 4, 0x24] {
            let mode = Mode::ucrt(false, options);
            let mut args = List { ints: vec![1, 2, 0x3042, 0x3044], strs: vec![vec![0x3046], vec![0x3048]], floats: vec![], stored: vec![] };
            let fmt: Vec<u16> = "%Ts%TS%Tc%TC".encode_utf16().collect();
            assert_eq!(format(&fmt, mode, &mut args).unwrap(), [0x3046, 0x3048, 0x3042, 0x3044]);
            let mut args = List { ints: vec![1, 2, 65, 66], strs: vec![vec![67], vec![68]], floats: vec![], stored: vec![] };
            assert_eq!(format(b"%Ts%TS%Tc%TC", mode, &mut args).unwrap(), b"CDAB");
        }
    }

    #[test]
    fn ms_specifics() {
        assert_eq!(f("%p", &[0x12ff7c], &[], Mode::ucrt(false, 0)), "0012FF7C");
        assert_eq!(f("%p", &[0x12ff7c], &[], Mode::ucrt(true, 0)), "000000000012FF7C");
        assert_eq!(u("%I64d %I32d %Id", &[u64::MAX, 7, 5], &[]), "-1 7 5");
        assert_eq!(f("%zu", &[1 << 40], &[], Mode::ucrt(true, 0)), "1099511627776");
        assert_eq!(u("%a", &[], &[1.0]), "0x1.0000000000000p+0");
        assert_eq!(u("%.1a", &[], &[1.0]), "0x1.0p+0");
        assert_eq!(u("%e", &[], &[1.0]), "1.000000e+00");
        assert_eq!(f("%e", &[], &[1.0], Mode::legacy(false)), "1.000000e+000");
        assert_eq!(f("%f", &[], &[f64::INFINITY], Mode::legacy(false)), "1.#INF00");
        assert_eq!(f("%e", &[], &[f64::NAN], Mode::legacy(false)), "1.#QNAN0e+000");
        assert_eq!(f("%g", &[], &[-f64::INFINITY], Mode::legacy(false)), "-1.#INF");
        assert_eq!(u("%f %F", &[], &[f64::INFINITY, f64::NAN]), "inf NAN");
        assert_eq!(u("%f", &[], &[f64::from_bits(0xfff8_0000_0000_0000)]), "-nan(ind)");
        assert_eq!(u("%f", &[], &[f64::from_bits(0x7ff4_0000_0000_0000)]), "nan(snan)");
        // ties: half even (UCRT) vs away (legacy)
        assert_eq!(u("%.0f %.0f %.1f", &[], &[0.5, 2.5, 0.25]), "0 2 0.2");
        assert_eq!(f("%.0f %.0f %.1f", &[], &[0.5, 2.5, 0.25], Mode::legacy(false)), "1 3 0.3");
    }

    #[test]
    fn errors() {
        let mut a = List { ints: vec![], floats: vec![], strs: vec![], stored: vec![] };
        assert_eq!(format(b"%y", Mode::ucrt(false, 0), &mut a), Err(Error::Invalid));
        assert_eq!(format(b"abc%", Mode::ucrt(false, 0), &mut a), Err(Error::Invalid));
        assert_eq!(format(b"%n", Mode::ucrt(false, 0), &mut a), Err(Error::Invalid));
        assert_eq!(format(b"%y", Mode::legacy(false), &mut a).unwrap(), b"y");
        assert_eq!(format(b"%999999999d", Mode::ucrt(false, 0), &mut a), Err(Error::TooLong));
        // (fuzz: a conversion of exactly MAX_OUT, then literal text)
        let f = format!("%{}dxy", MAX_OUT);
        let mut a = List { ints: vec![1], floats: vec![], strs: vec![], stored: vec![] };
        assert_eq!(format(f.as_bytes(), Mode::ucrt(false, 0), &mut a), Err(Error::TooLong));
    }

    #[test]
    fn strings() {
        let mut a = List { ints: vec![1, 2, 0], floats: vec![], strs: vec!["abc".encode_utf16().collect(), "xyz".encode_utf16().collect()], stored: vec![] };
        let o = format("%.2s|%ls|%s".as_bytes(), Mode::ucrt(false, 0), &mut a).unwrap();
        assert_eq!(o, b"ab|xyz|(null)");
        let mut a = List { ints: vec![1, 2], floats: vec![], strs: vec!["abc".encode_utf16().collect(), "xyz".encode_utf16().collect()], stored: vec![] };
        // legacy wide specifiers: %s in a wide function is wide, %S narrow
        let fw: Vec<u16> = "%s %S".encode_utf16().collect();
        let o = format(&fw, Mode::ucrt(false, 4), &mut a).unwrap();
        assert_eq!(String::from_utf16(&o).unwrap(), "abc xyz");
    }
}
