//! scanf's conversions: C11 7.21.6.2 with the MS CRT's format specification
//! ("Format specification fields: scanf and wscanf functions"):
//!
//!   %[*][width][size]type
//!
//! types `d i u o x X p e E f F g G a A c C s S [ n %`, sizes `hh h l ll L j z t w I I32 I64`
//! (`l`/`L` with a float type: double; `h`/`l`/`w` with c, s, [ choose narrow or wide). The
//! secure forms (sscanf_s...) take a size argument (in characters) after the buffer of c, s
//! and [; a buffer too small is a failure that leaves an empty string.
//!
//! White space in the format skips any white space in the input; other characters must match;
//! conversions other than c, [ and n skip leading white space. Integer fields read like strtol
//! in their base (i: base 0) but an overflowing value wraps (the MS CRT accumulates modulo
//! 2^64); floating fields like strtod. The result is the number of fields assigned, or EOF (-1)
//! when the input ended before the first conversion.

use crate::num::{self, Float};
use crate::{digit, is_space, Unit};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    pub ptr64: bool,
    /// the _s forms: c, s and [ take a buffer size
    pub secure: bool,
    /// msvcrt and msvcr*: decimal floats only
    pub legacy: bool,
    /// `%s`/`%c` in a wide function read wide (the MS default; ISO mode: narrow)
    pub legacy_wide: bool,
}

/// Where the results go: the pointer arguments and the guest memory behind them.
pub trait Sink {
    /// The next pointer argument.
    fn ptr(&mut self) -> u64;
    /// The next buffer size argument (the _s forms; an unsigned int).
    fn size(&mut self) -> u64;
    fn int(&mut self, p: u64, bytes: u32, v: u64);
    /// A float (`bytes` 4) or double (8).
    fn float(&mut self, p: u64, bytes: u32, v: f64);
    /// Characters, exactly these units (a string's NUL included by the caller).
    fn narrow(&mut self, p: u64, s: &[u8]);
    fn wide(&mut self, p: u64, s: &[u16]);
    fn a2w(&self, s: &[u8]) -> Vec<u16>;
    fn w2a(&self, s: &[u16]) -> Vec<u8>;
}

enum Stop {
    /// the input ended
    Input,
    /// it did not match
    Matching,
}

/// Scans `input` (without its NUL) by `fmt`. Returns the number of fields assigned or -1.
pub fn scan<T: Unit>(input: &[T], fmt: &[T], m: Mode, k: &mut impl Sink) -> i32 {
    let mut pos = 0usize;
    let mut assigned = 0i32;
    let mut converted = false;
    let f = |i: usize| fmt.get(i).map_or(0, |c| c.code());
    let inp = |p: usize| input.get(p).map(|c| c.code());
    let mut i = 0;
    let result = 'run: {
        while i < fmt.len() {
            let c = f(i);
            if is_space(c) {
                while i < fmt.len() && is_space(f(i)) {
                    i += 1;
                }
                while inp(pos).is_some_and(is_space) {
                    pos += 1;
                }
                continue;
            }
            if c != b'%' as u32 {
                match inp(pos) {
                    None => break 'run Stop::Input,
                    Some(x) if x != c => break 'run Stop::Matching,
                    _ => {}
                }
                pos += 1;
                i += 1;
                continue;
            }
            i += 1;
            let suppress = f(i) == b'*' as u32;
            if suppress {
                i += 1;
            }
            let mut width = 0usize;
            let mut has_width = false;
            while (0x30..=0x39).contains(&f(i)) {
                width = width.saturating_mul(10).saturating_add((f(i) - 0x30) as usize);
                has_width = true;
                i += 1;
            }
            let ptr = if m.ptr64 { 8 } else { 4 };
            // size: (integer bytes, float double, narrow/wide override)
            let (mut ibytes, mut dbl, mut chars): (u32, bool, Option<bool>) = (4, false, None);
            match f(i) as u8 {
                b'h' if f(i + 1) == b'h' as u32 => {
                    ibytes = 1;
                    chars = Some(false);
                    i += 2;
                }
                b'h' => {
                    ibytes = 2;
                    chars = Some(false);
                    i += 1;
                }
                b'l' if f(i + 1) == b'l' as u32 => {
                    ibytes = 8;
                    dbl = true;
                    i += 2;
                }
                b'l' => {
                    dbl = true;
                    chars = Some(true);
                    i += 1;
                }
                b'w' => {
                    chars = Some(true);
                    i += 1;
                }
                b'L' | b'j' => {
                    ibytes = 8;
                    dbl = true;
                    i += 1;
                }
                b'z' | b't' => {
                    ibytes = ptr;
                    i += 1;
                }
                b'I' if f(i + 1) == b'6' as u32 && f(i + 2) == b'4' as u32 => {
                    ibytes = 8;
                    i += 3;
                }
                b'I' if f(i + 1) == b'3' as u32 && f(i + 2) == b'2' as u32 => {
                    ibytes = 4;
                    i += 3;
                }
                b'I' => {
                    ibytes = ptr;
                    i += 1;
                }
                _ => {}
            }
            let ty = f(i);
            if i >= fmt.len() {
                break 'run Stop::Matching;
            }
            i += 1;
            let ty = if ty < 0x80 { ty as u8 } else { 0 };
            if !matches!(ty, b'c' | b'C' | b'[' | b'n') {
                while inp(pos).is_some_and(is_space) {
                    pos += 1;
                }
            }
            let limit = |pos: usize| if has_width && width > 0 { pos.saturating_add(width).min(input.len()) } else { input.len() };
            match ty {
                b'%' => {
                    match inp(pos) {
                        None => break 'run Stop::Input,
                        Some(0x25) => pos += 1,
                        _ => break 'run Stop::Matching,
                    }
                    continue;
                }
                b'n' => {
                    if !suppress {
                        let p = k.ptr();
                        k.int(p, ibytes, pos as u64);
                    }
                    continue;
                }
                b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'p' => {
                    if inp(pos).is_none() {
                        break 'run Stop::Input;
                    }
                    let base = match ty {
                        b'd' | b'u' => 10,
                        b'i' => 0,
                        b'o' => 8,
                        _ => 16,
                    };
                    let Some((v, used)) = int_field(&input[pos..limit(pos)], base) else { break 'run Stop::Matching };
                    pos += used;
                    if !suppress {
                        let p = k.ptr();
                        k.int(p, if ty == b'p' { ptr } else { ibytes }, v);
                        assigned += 1;
                    }
                }
                b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
                    if inp(pos).is_none() {
                        break 'run Stop::Input;
                    }
                    let kind = if dbl { Float::F64 } else { Float::F32 };
                    // the field is the longest prefix of a floating constant (no going back:
                    // "1e" or "0x" stays read), its value what strtod makes of it
                    let Some(n) = float_prefix(&input[pos..limit(pos)], m.legacy) else { break 'run Stop::Matching };
                    let field = &input[pos..pos + n];
                    let (v, used, _) = num::strtod(field, kind, m.legacy);
                    if used == 0 {
                        break 'run Stop::Matching;
                    }
                    pos += field.len();
                    if !suppress {
                        let p = k.ptr();
                        k.float(p, if dbl { 8 } else { 4 }, v);
                        assigned += 1;
                    }
                }
                b'c' | b'C' | b's' | b'S' | b'[' => {
                    let mut set: Option<(bool, Vec<u32>, Vec<(u32, u32)>)> = None;
                    if ty == b'[' {
                        // the scan set: [^...] negates; a ']' first is a member; a-z a range
                        let neg = f(i) == b'^' as u32;
                        if neg {
                            i += 1;
                        }
                        let mut singles = Vec::new();
                        let mut ranges = Vec::new();
                        let start = i;
                        while i < fmt.len() && (f(i) != b']' as u32 || i == start) {
                            let a = f(i);
                            if f(i + 1) == b'-' as u32 && i + 2 < fmt.len() && f(i + 2) != b']' as u32 {
                                let b = f(i + 2);
                                ranges.push((a.min(b), a.max(b)));
                                i += 3;
                            } else {
                                singles.push(a);
                                i += 1;
                            }
                        }
                        if i >= fmt.len() {
                            break 'run Stop::Matching;
                        }
                        i += 1; // ']'
                        set = Some((neg, singles, ranges));
                    }
                    if inp(pos).is_none() {
                        break 'run Stop::Input;
                    }
                    let end = match ty {
                        b'c' | b'C' => {
                            // (what is left when the input ends first: the MS CRT and glibc
                            // store and count those characters)
                            let n = if has_width && width > 0 { width } else { 1 };
                            pos.saturating_add(n).min(input.len())
                        }
                        b'[' => {
                            let (neg, singles, ranges) = set.as_ref().unwrap();
                            let lim = limit(pos);
                            let mut e = pos;
                            while e < lim {
                                let x = input[e].code();
                                let member = singles.contains(&x) || ranges.iter().any(|&(a, b)| (a..=b).contains(&x));
                                if member == *neg {
                                    break;
                                }
                                e += 1;
                            }
                            if e == pos {
                                break 'run Stop::Matching;
                            }
                            e
                        }
                        _ => {
                            let lim = limit(pos);
                            let mut e = pos;
                            while e < lim && !is_space(input[e].code()) {
                                e += 1;
                            }
                            e
                        }
                    };
                    let field = &input[pos..end];
                    pos = end;
                    if suppress {
                        converted = true;
                        continue;
                    }
                    let upper = ty == b'C' || ty == b'S';
                    let wide = match chars {
                        Some(w) => w,
                        None => {
                            let own = T::WIDE && (m.legacy_wide || m.legacy);
                            if upper { !own } else { own }
                        }
                    };
                    let p = k.ptr();
                    let cap = if m.secure { Some(k.size()) } else { None };
                    let string = !matches!(ty, b'c' | b'C');
                    let units: Vec<u32> = if wide == T::WIDE {
                        field.iter().map(|c| c.code()).collect()
                    } else if wide {
                        let b: Vec<u8> = field.iter().map(|c| c.code() as u8).collect();
                        k.a2w(&b).into_iter().map(|u| u as u32).collect()
                    } else {
                        let w: Vec<u16> = field.iter().map(|c| c.code() as u16).collect();
                        k.w2a(&w).into_iter().map(|u| u as u32).collect()
                    };
                    let need = units.len() + string as usize;
                    if let Some(cap) = cap {
                        if (cap as usize) < need {
                            if cap > 0 {
                                if wide {
                                    k.wide(p, &[0]);
                                } else {
                                    k.narrow(p, &[0]);
                                }
                            }
                            break 'run Stop::Matching;
                        }
                    }
                    if wide {
                        let mut v: Vec<u16> = units.iter().map(|&u| u as u16).collect();
                        if string {
                            v.push(0);
                        }
                        k.wide(p, &v);
                    } else {
                        let mut v: Vec<u8> = units.iter().map(|&u| u as u8).collect();
                        if string {
                            v.push(0);
                        }
                        k.narrow(p, &v);
                    }
                    assigned += 1;
                }
                _ => break 'run Stop::Matching,
            }
            converted = true;
        }
        return assigned;
    };
    match result {
        Stop::Input if !converted => -1,
        _ => assigned,
    }
}

/// The length of the longest prefix of `s` that is (or begins) a floating constant:
/// [sign] digits [. digits] [e [sign] digits], 0x hex [. hex] [p [sign] digits], INF[INITY],
/// NAN[(chars)] (decimal only for the legacy CRT). None: it is no number (a mantissa without
/// digits, "0x" alone, a word short of INF/NAN); an exponent cut short is fine ("1e" reads 1).
fn float_prefix<T: Unit>(s: &[T], legacy: bool) -> Option<usize> {
    let c = |i: usize| s.get(i).map_or(0, |u| u.code());
    let lower = |i: usize| c(i) | if (0x41..=0x5a).contains(&c(i)) { 0x20 } else { 0 };
    let dec = |i: usize| (0x30..=0x39).contains(&c(i));
    let hex = |i: usize| s.get(i).is_some_and(|&u| u.code() < 0x80 && digit(u, 16).is_some());
    let mut i = 0;
    if c(0) == b'-' as u32 || c(0) == b'+' as u32 {
        i = 1;
    }
    let word = |mut i: usize, w: &[u8]| {
        for &b in w {
            if lower(i) != b as u32 {
                break;
            }
            i += 1;
        }
        i
    };
    if !legacy && lower(i) == b'i' as u32 {
        let k = word(i, b"infinity");
        return (k >= i + 3).then_some(k);
    }
    if !legacy && lower(i) == b'n' as u32 {
        let k = word(i, b"nan");
        if k < i + 3 {
            return None;
        }
        if c(k) == b'(' as u32 {
            let mut j = k + 1;
            while (c(j) < 0x80 && (c(j) as u8).is_ascii_alphanumeric()) || c(j) == b'_' as u32 {
                j += 1;
            }
            return Some(if c(j) == b')' as u32 { j + 1 } else { j });
        }
        return Some(k);
    }
    let is_hex = !legacy && c(i) == b'0' as u32 && lower(i + 1) == b'x' as u32;
    let (digit_at, exp_char): (&dyn Fn(usize) -> bool, u32) = if is_hex {
        i += 2;
        (&hex, b'p' as u32)
    } else {
        (&dec, b'e' as u32)
    };
    // ("0x" that ends the field (its width, the input) reads as the 0, as in glibc; followed by
    // something else it is no number)
    let mut any = is_hex && i >= s.len();
    while digit_at(i) {
        i += 1;
        any = true;
    }
    if c(i) == b'.' as u32 {
        i += 1;
        while digit_at(i) {
            i += 1;
            any = true;
        }
    }
    if any && lower(i) == exp_char {
        i += 1;
        if c(i) == b'-' as u32 || c(i) == b'+' as u32 {
            i += 1;
        }
        while dec(i) {
            i += 1;
        }
    }
    any.then_some(i.min(s.len()))
}

/// An integer field (already cut to its width): (value modulo 2^64, units used). "0x" counts
/// as a zero followed by its prefix (the MS CRT has seen the 0 as a digit).
fn int_field<T: Unit>(s: &[T], base: u32) -> Option<(u64, usize)> {
    let c = |i: usize| s.get(i).map_or(0, |u| u.code());
    let mut i = 0;
    let neg = c(0) == b'-' as u32;
    if neg || c(0) == b'+' as u32 {
        i += 1;
    }
    let mut base = base;
    let mut seen = false;
    if (base == 0 || base == 16) && c(i) == b'0' as u32 && (c(i + 1) | 0x20) == b'x' as u32 && i + 1 < s.len() {
        i += 2;
        base = 16;
        seen = true;
    } else if base == 0 {
        base = if c(i) == b'0' as u32 { 8 } else { 10 };
    }
    let mut v: u64 = 0;
    while let Some(d) = s.get(i).and_then(|&u| digit(u, base)) {
        v = v.wrapping_mul(base as u64).wrapping_add(d as u64);
        seen = true;
        i += 1;
    }
    seen.then(|| (if neg { v.wrapping_neg() } else { v }, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        ptrs: u64,
        sizes: Vec<u64>,
        out: Vec<String>,
    }
    impl Sink for Rec {
        fn ptr(&mut self) -> u64 {
            self.ptrs += 1;
            self.ptrs
        }
        fn size(&mut self) -> u64 {
            if self.sizes.is_empty() { 0 } else { self.sizes.remove(0) }
        }
        fn int(&mut self, p: u64, bytes: u32, v: u64) {
            self.out.push(format!("{p}:i{bytes}={v:x}"));
        }
        fn float(&mut self, p: u64, bytes: u32, v: f64) {
            self.out.push(format!("{p}:f{bytes}={v}"));
        }
        fn narrow(&mut self, p: u64, s: &[u8]) {
            self.out.push(format!("{p}:s={:?}", String::from_utf8_lossy(s)));
        }
        fn wide(&mut self, p: u64, s: &[u16]) {
            self.out.push(format!("{p}:w={:?}", String::from_utf16_lossy(s)));
        }
        fn a2w(&self, s: &[u8]) -> Vec<u16> {
            s.iter().map(|&b| b as u16).collect()
        }
        fn w2a(&self, s: &[u16]) -> Vec<u8> {
            s.iter().map(|&u| u as u8).collect()
        }
    }

    fn run(input: &str, fmt: &str, m: Mode) -> (i32, Vec<String>) {
        let mut r = Rec::default();
        let n = scan(input.as_bytes(), fmt.as_bytes(), m, &mut r);
        (n, r.out)
    }

    #[test]
    fn basics() {
        let m = Mode::default();
        assert_eq!(run("12 abc 3.5", "%d %s %lf", m), (3, vec!["1:i4=c".into(), "2:s=\"abc\\0\"".into(), "3:f8=3.5".into()]));
        assert_eq!(run("", "%d", m).0, -1);
        assert_eq!(run("   ", "%d", m).0, -1);
        assert_eq!(run("x", "%d", m).0, 0);
        assert_eq!(run("1", "%d %d", m).0, 1);
        assert_eq!(run("1", "%*d%d", m).0, 0);
        // (fuzz: a width near usize::MAX)
        assert_eq!(run("12", "%99999999999999999999999d", m).0, 1);
        assert_eq!(run("1.5", "%99999999999999999999999lf", m).0, 1);
        assert_eq!(run("abc]de", "%[]a-c]%n", m).1, vec!["1:s=\"abc]\\0\"".to_string(), "2:i4=4".into()]);
        assert_eq!(run("12345", "%3d%d", m).1, vec!["1:i4=7b".to_string(), "2:i4=2d".into()]);
        assert_eq!(run("0x1f", "%i", m).1, vec!["1:i4=1f".to_string()]);
        assert_eq!(run("-1", "%u", m).1, vec!["1:i4=ffffffffffffffff".to_string()]);
        assert_eq!(run("ab", "%c%c", m).1.len(), 2);
        assert_eq!(run("100%", "%d%%", m).0, 1);
    }

    #[test]
    fn secure_sizes() {
        let m = Mode { secure: true, ..Default::default() };
        let mut r = Rec { sizes: vec![3], ..Default::default() };
        assert_eq!(scan(b"abc", b"%s", m, &mut r), 0);
        assert_eq!(r.out, vec!["1:s=\"\\0\"".to_string()]);
        let mut r = Rec { sizes: vec![4], ..Default::default() };
        assert_eq!(scan(b"abc", b"%s", m, &mut r), 1);
    }
}
