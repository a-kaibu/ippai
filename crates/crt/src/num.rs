//! String to number and back: strtol/strtoul/strtoll/strtoull (_strtoi64, _strtoui64), atoi,
//! strtod/strtof/atof (C11 7.22.1), and _itoa/_ltoa/_ultoa/_i64toa/_ui64toa (+ their _s forms).
//!
//! Integers: leading white space, an optional sign, then digits of `base` (0: 0x... hex, 0...
//! octal, else decimal; 16 also takes a 0x prefix, which only counts when a hex digit follows).
//! Out of range: the type's limit and ERANGE. No digits: 0 and the end pointer is the string
//! itself. A base other than 0 and 2..36: 0 and EINVAL.
//!
//! Floating point (the UCRT): the C99 subject sequences (decimal, 0x hexadecimal with p
//! exponent, INF/INFINITY, NAN/NAN(chars)), correctly rounded; overflow gives ±HUGE_VAL and
//! ERANGE, a non-zero subject that rounds to zero 0 and ERANGE; a subnormal result is no error
//! (unlike C11 7.12.1 and glibc: Windows, as Wine's ucrtbase tests record). The legacy msvcrt
//! reads decimal only.

use crate::{digit, is_space, Unit, EINVAL, ERANGE};

/// strto* of an integer: what was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Int {
    /// the magnitude (saturated at u64::MAX when `overflow`)
    pub mag: u64,
    pub neg: bool,
    /// units used (0: no conversion)
    pub end: usize,
    pub overflow: bool,
    /// the base was invalid
    pub bad_base: bool,
}

/// Reads an integer subject sequence of `base` from `s`.
pub fn parse_int<T: Unit>(s: &[T], base: u32) -> Int {
    parse(s, base, false)
}

/// `legacy`: msvcrt's reading, where "0x" (base 0 or 16) is taken as the prefix even when no
/// hex digit follows, and then there is no number ("-0x": nothing read).
fn parse<T: Unit>(s: &[T], base: u32, legacy: bool) -> Int {
    let mut r = Int { mag: 0, neg: false, end: 0, overflow: false, bad_base: false };
    if base == 1 || base > 36 {
        r.bad_base = true;
        return r;
    }
    let c = |i: usize| s.get(i).map_or(0, |u| u.code());
    let mut i = 0;
    while is_space(c(i)) {
        i += 1;
    }
    if c(i) == b'-' as u32 || c(i) == b'+' as u32 {
        r.neg = c(i) == b'-' as u32;
        i += 1;
    }
    let mut base = base;
    let x = |k: usize| c(k) == b'x' as u32 || c(k) == b'X' as u32;
    let hex_after = |k: usize| s.get(k).and_then(|&u| digit(u, 16)).is_some();
    if (base == 0 || base == 16) && c(i) == b'0' as u32 && x(i + 1) && (legacy || hex_after(i + 2)) {
        i += 2;
        base = 16;
    } else if base == 0 {
        base = if c(i) == b'0' as u32 { 8 } else { 10 };
    }
    let start = i;
    while let Some(d) = s.get(i).and_then(|&u| digit(u, base)) {
        match r.mag.checked_mul(base as u64).and_then(|v| v.checked_add(d as u64)) {
            Some(v) => r.mag = v,
            None => {
                r.overflow = true;
                r.mag = u64::MAX;
            }
        }
        i += 1;
    }
    if i > start {
        r.end = i;
    } else {
        r.neg = false;
    }
    r
}

/// The result of a conversion: value, units used, errno to set (0: none).
pub type Conv<V> = (V, usize, i32);

/// strtol (32-bit long) and the 64-bit strtoll / _strtoi64: `bits` 32 or 64.
pub fn strtol<T: Unit>(s: &[T], base: u32, bits: u32) -> Conv<i64> {
    signed(parse_int(s, base), bits)
}

/// msvcrt.dll's strtol: its "0x" reading (see `parse`), else strtol.
pub fn strtol_legacy<T: Unit>(s: &[T], base: u32, bits: u32) -> Conv<i64> {
    signed(parse(s, base, true), bits)
}

fn signed(r: Int, bits: u32) -> Conv<i64> {
    if r.bad_base {
        return (0, 0, EINVAL);
    }
    let max = (1u64 << (bits - 1)) - 1;
    let limit = if r.neg { max + 1 } else { max };
    if r.overflow || r.mag > limit {
        let v = if r.neg { -(max as i64) - 1 } else { max as i64 };
        return (v, r.end, ERANGE);
    }
    let v = if r.neg { (r.mag as i64).wrapping_neg() } else { r.mag as i64 };
    (v, r.end, 0)
}

/// strtoul and the 64-bit strtoull / _strtoui64: a negative subject is negated in the unsigned
/// type ("-1" is the maximum); beyond the type: the maximum and ERANGE.
pub fn strtoul<T: Unit>(s: &[T], base: u32, bits: u32) -> Conv<u64> {
    unsigned(parse_int(s, base), bits, false)
}

/// msvcrt.dll's strtoul: its "0x" reading, and the maximum of an overflow is negated too for a
/// '-' ("-0x2147483648" in base 16 is 1, Windows' answer).
pub fn strtoul_legacy<T: Unit>(s: &[T], base: u32, bits: u32) -> Conv<u64> {
    unsigned(parse(s, base, true), bits, true)
}

fn unsigned(r: Int, bits: u32, legacy: bool) -> Conv<u64> {
    if r.bad_base {
        return (0, 0, EINVAL);
    }
    let max = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    if r.overflow || r.mag > max {
        let v = if legacy && r.neg { max.wrapping_neg() & max } else { max };
        return (v, r.end, ERANGE);
    }
    let v = if r.neg { r.mag.wrapping_neg() & max } else { r.mag };
    (v, r.end, 0)
}

/// atoi/atol (32-bit), _atoi64/atoll (64-bit): strtol in base 10, the limit on overflow.
pub fn atoi<T: Unit>(s: &[T], bits: u32) -> Conv<i64> {
    strtol(s, 10, bits)
}

/// msvcrt.dll's atoi/atol: the decimal digits accumulated in 32 bits with wrap-around, no
/// range check, no errno ("4294967296" is 0, Windows' answer).
pub fn atoi_wrap<T: Unit>(s: &[T]) -> i32 {
    let c = |i: usize| s.get(i).map_or(0, |u| u.code());
    let mut i = 0;
    while is_space(c(i)) {
        i += 1;
    }
    let neg = c(i) == b'-' as u32;
    if neg || c(i) == b'+' as u32 {
        i += 1;
    }
    let mut v = 0u32;
    while (0x30..=0x39).contains(&c(i)) {
        v = v.wrapping_mul(10).wrapping_add(c(i) - 0x30);
        i += 1;
    }
    (if neg { v.wrapping_neg() } else { v }) as i32
}

/// The floating-point formats strtod and strtof convert to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    F32,
    F64,
}

impl Float {
    fn mant_bits(self) -> u32 {
        match self {
            Float::F32 => 23,
            Float::F64 => 52,
        }
    }
    fn exp_bias(self) -> i64 {
        match self {
            Float::F32 => 127,
            Float::F64 => 1023,
        }
    }
}

/// strtod (F64) / strtof (F32): the value (an f32's exactly as f64), units used, errno.
pub fn strtod<T: Unit>(s: &[T], kind: Float, legacy: bool) -> Conv<f64> {
    let c = |i: usize| s.get(i).map_or(0, |u| u.code());
    let lower = |i: usize| {
        let x = c(i);
        if (0x41..=0x5a).contains(&x) { x + 32 } else { x }
    };
    let word = |at: usize, w: &[u8]| w.iter().enumerate().all(|(k, &b)| lower(at + k) == b as u32);
    let mut i = 0;
    while is_space(c(i)) {
        i += 1;
    }
    let neg = c(i) == b'-' as u32;
    if neg || c(i) == b'+' as u32 {
        i += 1;
    }
    let signed = |v: f64| if neg { -v } else { v };
    if !legacy {
        if word(i, b"inf") {
            let n = if word(i, b"infinity") { 8 } else { 3 };
            return (signed(f64::INFINITY), i + n, 0);
        }
        if word(i, b"nan") {
            let mut end = i + 3;
            let mut bits = 0x7ff8_0000_0000_0000u64;
            if c(end) == b'(' as u32 {
                let mut k = end + 1;
                while matches!(c(k) as u8, b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'_') && c(k) < 0x80 {
                    k += 1;
                }
                if c(k) == b')' as u32 {
                    if word(end + 1, b"snan)") {
                        bits = 0x7ff4_0000_0000_0000;
                    } else if word(end + 1, b"ind)") {
                        bits = 0xfff8_0000_0000_0000;
                    }
                    end = k + 1;
                }
            }
            let v = f64::from_bits(bits);
            let v = if neg { f64::from_bits(v.to_bits() ^ 1 << 63) } else { v };
            let v = if kind == Float::F32 { v as f32 as f64 } else { v };
            return (v, end, 0);
        }
        if c(i) == b'0' as u32 && (c(i + 1) | 0x20) == b'x' as u32 {
            if let Some((v, end, e)) = hex_float(s, i + 2, kind) {
                return (signed(v), end, e);
            }
            // "0x" alone: the subject is the 0
            return (signed(0.0), i + 1, 0);
        }
    }
    // decimal: digits [. digits] [e [sign] digits]
    let mut digits: Vec<u8> = Vec::new();
    let mut point: i64 = 0; // decimal exponent adjustment
    let mut any = false;
    let mut k = i;
    while (0x30..=0x39).contains(&c(k)) {
        any = true;
        if !(digits.is_empty() && c(k) == 0x30) {
            digits.push(c(k) as u8);
        }
        k += 1;
    }
    let int_digits = digits.len() as i64;
    if c(k) == b'.' as u32 {
        k += 1;
        while (0x30..=0x39).contains(&c(k)) {
            any = true;
            if digits.is_empty() && c(k) == 0x30 {
                point -= 1;
            } else {
                digits.push(c(k) as u8);
            }
            k += 1;
        }
    }
    if !any {
        return (0.0, 0, 0);
    }
    let frac_digits = digits.len() as i64 - int_digits;
    let mut end = k;
    let mut exp: i64 = 0;
    if (c(k) | 0x20) == b'e' as u32 {
        let mut j = k + 1;
        let eneg = c(j) == b'-' as u32;
        if eneg || c(j) == b'+' as u32 {
            j += 1;
        }
        if (0x30..=0x39).contains(&c(j)) {
            while (0x30..=0x39).contains(&c(j)) {
                exp = (exp * 10 + (c(j) - 0x30) as i64).min(1 << 40);
                j += 1;
            }
            if eneg {
                exp = -exp;
            }
            end = j;
        }
    }
    if digits.is_empty() {
        return (signed(0.0), end, 0);
    }
    // value = D * 10^E with D the digits as an integer
    let n = digits.len() as i64;
    let e = (exp + point - frac_digits).clamp(-n - 1000, 1000);
    let text = format!("{}e{}", std::str::from_utf8(&digits).unwrap_or("0"), e);
    let v = match kind {
        Float::F64 => text.parse::<f64>().unwrap_or(0.0),
        Float::F32 => text.parse::<f32>().unwrap_or(0.0) as f64,
    };
    (signed(v), end, range_errno(v))
}

/// ERANGE for an overflow (infinity from finite digits) or an underflow to zero (of a subject
/// that is not zero: the callers return early for those).
fn range_errno(v: f64) -> i32 {
    if v.is_infinite() || v == 0.0 { ERANGE } else { 0 }
}

/// The hexadecimal subject after "0x": hex digits [. hex digits] [p [sign] digits], rounded
/// half to even into `kind`. None when no hex digit follows.
fn hex_float<T: Unit>(s: &[T], at: usize, kind: Float) -> Option<(f64, usize, i32)> {
    let hd = |i: usize| s.get(i).and_then(|&u| if u.code() < 0x80 { digit(u, 16) } else { None });
    let c = |i: usize| s.get(i).map_or(0, |u| u.code());
    // the first 30 significant hex digits exactly (120 bits), the rest as a sticky bit
    let mut m: u128 = 0;
    let mut nd = 0u32;
    let mut sticky = false;
    let mut exp: i64 = 0; // binary exponent of m's last bit
    let mut any = false;
    let mut k = at;
    while let Some(d) = hd(k) {
        any = true;
        if m == 0 && d == 0 {
        } else if nd < 30 {
            m = m << 4 | d as u128;
            nd += 1;
        } else {
            sticky |= d != 0;
            exp += 4;
        }
        k += 1;
    }
    if c(k) == b'.' as u32 {
        let mut j = k + 1;
        while let Some(d) = hd(j) {
            any = true;
            if m == 0 && d == 0 {
                exp -= 4;
            } else if nd < 30 {
                m = m << 4 | d as u128;
                nd += 1;
                exp -= 4;
            } else {
                sticky |= d != 0;
            }
            j += 1;
        }
        if any {
            k = j;
        }
    }
    if !any {
        return None;
    }
    let mut end = k;
    if (c(k) | 0x20) == b'p' as u32 {
        let mut j = k + 1;
        let eneg = c(j) == b'-' as u32;
        if eneg || c(j) == b'+' as u32 {
            j += 1;
        }
        if (0x30..=0x39).contains(&c(j)) {
            let mut e: i64 = 0;
            while (0x30..=0x39).contains(&c(j)) {
                e = (e * 10 + (c(j) - 0x30) as i64).min(1 << 40);
                j += 1;
            }
            exp += if eneg { -e } else { e };
            end = j;
        }
    }
    if m == 0 {
        return Some((0.0, end, 0));
    }
    let v = round_binary(m, sticky, exp, kind);
    Some((v, end, range_errno(v)))
}

/// m * 2^exp (m != 0, `sticky`: non-zero bits below m) rounded half to even into `kind`, as
/// f64.
fn round_binary(m: u128, sticky: bool, exp: i64, kind: Float) -> f64 {
    let p = kind.mant_bits() as i64 + 1; // significand bits
    let top = 127 - m.leading_zeros() as i64; // index of m's highest bit
    let e = top + exp; // the value's binary exponent (2^e <= v < 2^(e+1))
    let emin = 1 - kind.exp_bias();
    let emax = kind.exp_bias();
    // the last kept bit's exponent: normal p bits, subnormal fixed at emin - (p - 1)
    let lsb = (e - (p - 1)).max(emin - (p - 1));
    let shift = lsb - exp; // bits of m to drop
    let (mut q, round_up) = if shift <= 0 {
        ((m << (-shift).min(127) as u32), false)
    } else if shift >= 128 {
        (0, false) // (far below half the smallest subnormal: m < 2^120)
    } else {
        let q = m >> shift;
        let rest = m & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        (q, rest > half || (rest == half && (sticky || q & 1 == 1)))
    };
    if round_up {
        q += 1;
    }
    // q * 2^lsb; q may have reached 2^p (carry into the next binade)
    let mut lsb = lsb;
    if q >> p != 0 {
        q >>= 1;
        lsb += 1;
    }
    let e_final = if q >> (p - 1) != 0 { lsb + p - 1 } else { emin - 1 };
    if e_final > emax {
        return f64::INFINITY;
    }
    let mant = (q as u64) & ((1u64 << (p - 1)) - 1);
    let biased = (e_final + kind.exp_bias()) as u64;
    let v = match kind {
        Float::F64 => f64::from_bits(biased << 52 | mant),
        Float::F32 => f32::from_bits(((biased as u32) << 23) | mant as u32) as f64,
    };
    v
}

/// The digits of `v` in `radix` (2..=36, lowercase letters), with a '-' for a negative value
/// in base 10 (`signed` types only); other bases show the two's complement bits of `bits`.
pub fn to_radix(v: i64, bits: u32, signed: bool, radix: u32) -> Vec<u8> {
    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let neg = signed && radix == 10 && v < 0;
    let mut u = if neg { v.unsigned_abs() } else { v as u64 & mask };
    let mut d = Vec::new();
    loop {
        d.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(u % radix as u64) as usize]);
        u /= radix as u64;
        if u == 0 {
            break;
        }
    }
    if neg {
        d.push(b'-');
    }
    d.reverse();
    d
}

/// _itoa_s and friends: the digits into `buf` (its size in units) with the NUL, or the error:
/// EINVAL for no buffer / size 0 / a bad radix, ERANGE when it does not fit (buf[0] = 0 then,
/// as on any error with a buffer).
pub fn xtoa_s<T: Unit>(v: i64, bits: u32, signed: bool, buf: Option<&mut [T]>, radix: u32) -> i32 {
    let Some(buf) = buf else { return EINVAL };
    if buf.is_empty() {
        return EINVAL;
    }
    buf[0] = T::default();
    let neg = signed && radix == 10 && v < 0;
    if buf.len() <= if neg { 2 } else { 1 } {
        return ERANGE;
    }
    if !(2..=36).contains(&radix) {
        return EINVAL;
    }
    let d = to_radix(v, bits, signed, radix);
    if d.len() + 1 > buf.len() {
        return ERANGE;
    }
    for (k, &b) in d.iter().enumerate() {
        buf[k] = T::from_ascii(b);
    }
    buf[d.len()] = T::default();
    0
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// The digits' value in `radix` (no '-').
    fn value(d: &[u8], radix: u64) -> u64 {
        d.iter().fold(0u64, |acc, &c| acc * radix + if c <= b'9' { c - b'0' } else { c - b'a' + 10 } as u64)
    }

    /// _itoa/_ltoa in base 10: a '-' for a negative value, then its magnitude with no leading
    /// zero. Every 16-bit value and the 32-bit extremes (all 32-bit values are beyond what the
    /// solver does in minutes: the decimal division chain; the unit tests and PBT have them).
    #[kani::proof]
    #[kani::unwind(13)]
    fn itoa10_digits() {
        let small: i16 = kani::any();
        let v: i32 = if kani::any() { small as i32 } else if kani::any() { i32::MIN } else { i32::MAX };
        let s = to_radix(v as i64, 32, true, 10);
        let neg = s[0] == b'-';
        let d = &s[neg as usize..];
        assert!(neg == (v < 0) && !d.is_empty() && d.len() <= 10);
        assert!(d.len() == 1 || d[0] != b'0');
        assert!(d.iter().all(|c| c.is_ascii_digit()));
        assert!(value(d, 10) == (v as i64).unsigned_abs());
    }

    /// Other bases print the 32-bit two's complement (16 and 8; base 2's 32 digits are
    /// beyond what the solver does in minutes, the unit tests have it).
    #[kani::proof]
    #[kani::unwind(13)]
    fn itoa_bits_digits() {
        let v: i32 = kani::any();
        let r: u32 = if kani::any() { 16 } else { 8 };
        let s = to_radix(v as i64, 32, true, r);
        assert!(s.len() == 1 || s[0] != b'0');
        assert!(value(&s, r as u64) == v as u32 as u64);
    }

    /// _itoa_s: EINVAL / ERANGE / the digits and a NUL, never past the buffer.
    #[kani::proof]
    #[kani::unwind(13)]
    fn itoa_s_contract() {
        let v: i32 = kani::any();
        let mut b: [u8; 6] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= 6);
        let orig = b;
        let r = xtoa_s(v as i64, 32, true, Some(&mut b[..n]), 10);
        let want = to_radix(v as i64, 32, true, 10);
        if n == 0 {
            assert!(r == EINVAL);
        } else if want.len() + 1 > n {
            assert!(r == ERANGE && b[0] == 0);
        } else {
            assert!(r == 0 && b[..want.len()] == want[..] && b[want.len()] == 0);
        }
        assert!(b[n..] == orig[n..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(strtol(b"  -123x", 10, 32), (-123, 6, 0));
        assert_eq!(strtol(b"0x1f", 0, 32), (31, 4, 0));
        assert_eq!(strtol(b"0x", 16, 32), (0, 1, 0));
        assert_eq!(strtol(b"0xg", 0, 32), (0, 1, 0));
        assert_eq!(strtol(b"017", 0, 32), (15, 3, 0));
        assert_eq!(strtol(b"2147483648", 10, 32), (i32::MAX as i64, 10, ERANGE));
        assert_eq!(strtol(b"-2147483648", 10, 32), (i32::MIN as i64, 11, 0));
        assert_eq!(strtol(b"  +", 10, 32), (0, 0, 0));
        assert_eq!(strtol(b"1", 1, 32), (0, 0, EINVAL));
        assert_eq!(strtoul(b"-1", 10, 32), (u32::MAX as u64, 2, 0));
        assert_eq!(strtoul(b"4294967296", 10, 32), (u32::MAX as u64, 10, ERANGE));
        assert_eq!(strtoul(b"99999999999999999999", 10, 64), (u64::MAX, 20, ERANGE));
        // msvcrt.dll (Windows' answers, tests/winref crt_conv)
        assert_eq!(strtol_legacy(b"-0x", 16, 32), (0, 0, 0));
        assert_eq!(strtol_legacy(b"-0x", 0, 32), (0, 0, 0));
        assert_eq!(strtol_legacy(b"-0x", 36, 32), (-33, 3, 0));
        assert_eq!(strtoul_legacy(b"-2147483648", 16, 32), (1, 11, ERANGE));
        assert_eq!(strtol_legacy(b"0x1A", 0, 32), (26, 4, 0));
        assert_eq!(atoi_wrap(b"4294967296"), 0);
        assert_eq!(atoi_wrap(b"99999999999"), 1215752191);
        assert_eq!(atoi_wrap(b"-2147483649"), 2147483647);
        let w: Vec<u16> = "１２3".encode_utf16().collect();
        assert_eq!(strtol(&w, 10, 32), (123, 3, 0));
    }

    #[test]
    fn floats() {
        assert_eq!(strtod(b"1.5e3x", Float::F64, false), (1500.0, 5, 0));
        assert_eq!(strtod(b"1e", Float::F64, false), (1.0, 1, 0));
        assert_eq!(strtod(b".", Float::F64, false), (0.0, 0, 0));
        assert_eq!(strtod(b"0x1.8p1", Float::F64, false), (3.0, 7, 0));
        assert_eq!(strtod(b"0x", Float::F64, false), (0.0, 1, 0));
        assert_eq!(strtod(b"-INFINITY", Float::F64, false), (f64::NEG_INFINITY, 9, 0));
        assert_eq!(strtod(b"inf", Float::F64, true), (0.0, 0, 0));
        assert!(strtod(b"nan(123)", Float::F64, false).0.is_nan());
        assert_eq!(strtod(b"nan(123", Float::F64, false).1, 3);
        assert_eq!(strtod(b"1e999", Float::F64, false), (f64::INFINITY, 5, ERANGE));
        assert_eq!(strtod(b"1e-999", Float::F64, false), (0.0, 6, ERANGE));
        // (Windows: a subnormal result is no range error, zero is; Wine's ucrtbase string tests)
        assert_eq!(strtod(b"0x1p-1074", Float::F64, false), (f64::from_bits(1), 9, 0));
        assert_eq!(strtod(b"0x1p-1026", Float::F64, false), (1.3906711615670009e-309, 9, 0));
        assert_eq!(strtod(b"4.9406564584124654e-324", Float::F64, false), (f64::from_bits(1), 23, 0));
        assert_eq!(strtod(b"2.48e-324", Float::F64, false), (f64::from_bits(1), 9, 0));
        assert_eq!(strtod(b"2.47e-324", Float::F64, false), (0.0, 9, ERANGE));
        assert_eq!(strtod(b"1.4e-45", Float::F32, false), (f32::from_bits(1) as f64, 7, 0));
        assert_eq!(strtod(b"-1.e-60", Float::F32, false), (-0.0, 7, ERANGE));
        assert_eq!(strtod(b"0x1.fffffffffffff8p0", Float::F64, false).0, 2.0);
        assert_eq!(strtod(b"0x1.fffffffffffff7p0", Float::F64, false).0, f64::from_bits(0x3fff_ffff_ffff_ffff));
        assert_eq!(strtod(b"3.4028236e38", Float::F32, false), (f64::INFINITY, 12, ERANGE));
    }

    #[test]
    fn itoa() {
        assert_eq!(to_radix(-255, 32, true, 16), b"ffffff01");
        assert_eq!(to_radix(-255, 32, true, 10), b"-255");
        let mut b = [1u8; 4];
        assert_eq!(xtoa_s(-12, 32, true, Some(&mut b), 10), 0);
        assert_eq!(&b, b"-12\0");
        assert_eq!(xtoa_s(-123, 32, true, Some(&mut b), 10), ERANGE);
        assert_eq!(b[0], 0);
        assert_eq!(xtoa_s(1, 32, true, Some(&mut b), 37), EINVAL);
    }
}
