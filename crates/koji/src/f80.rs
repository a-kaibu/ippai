//! The x87's 80-bit extended format in software, for the precise x87 mode (`fpu::X87_80`):
//! add, subtract, multiply, divide and square root correctly rounded to the precision control's
//! 24, 53 or 64 bits with the 15-bit exponent and the rounding control, conversions from and to
//! f32/f64/integers, compares. Rounding follows Berkeley SoftFloat's roundPackToExtF80 (x86:
//! tininess after rounding) and NaNs its 8086 rules (the larger significand of two; the invalid
//! result is the negative "indefinite"), so tests/testfloat's extF80 vectors check it. The status
//! flags are not kept (koji's x87 keeps none).

/// An 80-bit extended value: sign and 15-bit exponent, 64-bit significand with its explicit
/// integer bit (as in memory, little-endian: significand then sign/exponent).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct F80 {
    pub m: u64,
    pub se: u16,
}

/// The negative quiet NaN the x87 returns for an invalid operation ("indefinite").
pub const INDEFINITE: F80 = F80 { m: 0xc000_0000_0000_0000, se: 0xffff };
const BIAS: i32 = 0x3fff;

/// Rounding precision: the precision control field (0: 24 bits, 2: 53, 3 and the reserved 1: 64).
fn bits_of(pc: u32) -> u32 {
    match pc & 3 {
        0 => 32,
        2 => 64,
        _ => 80,
    }
}

impl F80 {
    pub fn from_bytes(b: &[u8; 10]) -> F80 {
        F80 { m: u64::from_le_bytes(b[..8].try_into().unwrap()), se: u16::from_le_bytes([b[8], b[9]]) }
    }
    pub fn to_bytes(self) -> [u8; 10] {
        let mut b = [0u8; 10];
        b[..8].copy_from_slice(&self.m.to_le_bytes());
        b[8..].copy_from_slice(&self.se.to_le_bytes());
        b
    }
    pub fn sign(self) -> bool {
        self.se & 0x8000 != 0
    }
    fn exp(self) -> i32 {
        (self.se & 0x7fff) as i32
    }
    pub fn is_nan(self) -> bool {
        self.exp() == 0x7fff && self.m << 1 != 0
    }
    fn is_snan(self) -> bool {
        self.is_nan() && self.m & 0x4000_0000_0000_0000 == 0
    }
    pub fn is_inf(self) -> bool {
        self.exp() == 0x7fff && self.m << 1 == 0 && self.m >> 63 != 0
    }
    pub fn is_zero(self) -> bool {
        self.exp() == 0 && self.m == 0
    }
    /// An encoding the 387 and later reject as an operand: an unnormal (no integer bit with a
    /// nonzero exponent), a pseudo-NaN or pseudo-infinity.
    fn unsupported(self) -> bool {
        self.exp() != 0 && self.m >> 63 == 0
    }
    fn pack(sign: bool, exp: i32, m: u64) -> F80 {
        F80 { m, se: (sign as u16) << 15 | exp as u16 }
    }
    pub fn neg(self) -> F80 {
        F80 { m: self.m, se: self.se ^ 0x8000 }
    }
    pub fn abs(self) -> F80 {
        F80 { m: self.m, se: self.se & 0x7fff }
    }
    /// (sign, biased exponent, significand with bit 63 set) of a finite nonzero value; a
    /// denormal's exponent goes below 1.
    fn unpack(self) -> (bool, i32, u64) {
        let e = self.exp();
        if e == 0 {
            let lz = self.m.leading_zeros() as i32;
            (self.sign(), 1 - lz, self.m << lz)
        } else {
            (self.sign(), e, self.m)
        }
    }

    pub fn from_f64(v: f64) -> F80 {
        F80::from_bytes(&crate::fpu::to_f80(v))
    }
    pub fn from_f32(v: f32) -> F80 {
        F80::from_f64(v as f64)
    }
    pub fn from_i64(i: i64) -> F80 {
        F80::from_bytes(&crate::fpu::int_to_f80(i))
    }

    /// To an IEEE format of `mbits` fraction bits and exponent bias `bias` (f64: 52/1023, f32:
    /// 23/127), rounded by `rc`: its bits. NaNs keep their sign and the upper payload, quieted.
    fn to_ieee(self, mbits: u32, bias: i32, rc: u32) -> u64 {
        let ebits = if mbits == 52 { 11 } else { 8 };
        let emax = (1i32 << ebits) - 1;
        let s = (self.sign() as u64) << (mbits + ebits);
        if self.exp() == 0x7fff || self.unsupported() {
            if self.is_inf() {
                return s | (emax as u64) << mbits;
            }
            let m = if self.unsupported() { INDEFINITE.m } else { self.m };
            let sign = if self.unsupported() { 1u64 << (mbits + ebits) } else { s };
            return sign | (emax as u64) << mbits | 1 << (mbits - 1) | (m << 1 >> (64 - mbits));
        }
        if self.is_zero() {
            return s;
        }
        let (sign, e, m) = self.unpack();
        let e = e - BIAS; // the value is m * 2^(e - 63)
        // in units of the result's last place: 2^(e - mbits), or the denormals' 2^(1 - bias - mbits)
        let ulp = (e - mbits as i32).max(1 - bias - mbits as i32);
        let shift = (ulp - (e - 63)) as u32; // (at least 63 - mbits)
        let q = round_shift(m, shift, rc, sign);
        if q == 0 {
            return s;
        }
        // q * 2^ulp: normal when q reaches 2^mbits (a carry included)
        // the exponent field for q in [2^mbits, 2^(mbits+1)) (2^(mbits+1): a carry, one more)
        let field = ulp + mbits as i32 + bias + (q >> (mbits + 1)) as i32;
        if q >> mbits != 0 && field >= emax {
            // overflow: infinity, or the largest finite value when rounding toward zero/away
            let inf = rc == 0 || (rc == 1 && sign) || (rc == 2 && !sign);
            return s | if inf { (emax as u64) << mbits } else { ((emax as u64) << mbits) - 1 };
        }
        let bits = if q >> mbits == 0 { q } else { (field as u64) << mbits | (q & ((1 << mbits) - 1)) };
        s | bits
    }
    pub fn to_f64(self, rc: u32) -> f64 {
        f64::from_bits(self.to_ieee(52, 1023, rc & 3))
    }
    pub fn to_f32(self, rc: u32) -> f32 {
        f32::from_bits(self.to_ieee(23, 127, rc & 3) as u32)
    }

    /// Rounded to an integer by `rc` (None: a NaN, infinity or unsupported encoding).
    pub fn to_int(self, rc: u32) -> Option<i128> {
        if self.exp() == 0x7fff || self.unsupported() {
            return None;
        }
        if self.is_zero() {
            return Some(0);
        }
        let (sign, e, m) = self.unpack();
        let e = e - BIAS;
        if e > 100 {
            return Some(if sign { i128::MIN } else { i128::MAX });
        }
        let q = if e >= 63 { (m as i128) << (e - 63) } else { round_shift(m, (63 - e) as u32, rc & 3, sign) as i128 };
        Some(if sign { -q } else { q })
    }

    /// FRNDINT: the integral value `rc` rounds to (NaNs quieted, infinities and zeros kept).
    pub fn round_int(self, rc: u32) -> F80 {
        if self.unsupported() {
            return INDEFINITE;
        }
        if self.is_nan() {
            return F80 { m: self.m | 0x4000_0000_0000_0000, se: self.se };
        }
        if self.exp() == 0x7fff || self.is_zero() {
            return self;
        }
        let (sign, e, m) = self.unpack();
        let e = e - BIAS;
        if e >= 63 {
            return self;
        }
        let q = round_shift(m, (63 - e) as u32, rc & 3, sign);
        if q == 0 {
            return F80::pack(sign, 0, 0);
        }
        let lz = q.leading_zeros();
        F80::pack(sign, BIAS + 63 - lz as i32, q << lz)
    }

    /// Compares two values: None when unordered (a NaN or unsupported encoding); zeros equal.
    pub fn compare(self, b: F80) -> Option<core::cmp::Ordering> {
        use core::cmp::Ordering::*;
        if self.is_nan() || b.is_nan() || self.unsupported() || b.unsupported() {
            return None;
        }
        if self.is_zero() && b.is_zero() {
            return Some(Equal);
        }
        let key = |x: F80| -> (bool, u16, u64) { (x.sign(), x.se & 0x7fff, x.m) };
        let (sa, ea, ma) = key(self);
        let (sb, eb, mb) = key(b);
        if sa != sb {
            return Some(if sa { Less } else { Greater });
        }
        // (denormals: exponent 0 with no integer bit, below every normal; pseudo-denormals
        // with it equal the smallest normal's scale: compare as unpacked)
        let mag = |e: u16, m: u64| -> (i32, u64) { if e == 0 { let x = F80 { m, se: 0 }; if m == 0 { (i32::MIN, 0) } else { let (_, e, m) = x.unpack(); (e, m) } } else { (e as i32, m) } };
        let o = mag(ea, ma).cmp(&mag(eb, mb));
        Some(if sa { o.reverse() } else { o })
    }

    pub fn add(self, b: F80, pc: u32, rc: u32) -> F80 {
        add_sub(self, b, false, pc, rc)
    }
    pub fn sub(self, b: F80, pc: u32, rc: u32) -> F80 {
        add_sub(self, b, true, pc, rc)
    }

    pub fn mul(self, b: F80, pc: u32, rc: u32) -> F80 {
        if let Some(n) = nan_operands(self, b) {
            return n;
        }
        let sign = self.sign() ^ b.sign();
        if self.is_inf() || b.is_inf() {
            return if self.is_zero() || b.is_zero() { INDEFINITE } else { F80::pack(sign, 0x7fff, 1 << 63) };
        }
        if self.is_zero() || b.is_zero() {
            return F80::pack(sign, 0, 0);
        }
        let (_, ea, ma) = self.unpack();
        let (_, eb, mb) = b.unpack();
        let mut p = ma as u128 * mb as u128;
        let mut e = ea + eb - 0x3ffe;
        if p >> 127 == 0 {
            p <<= 1;
            e -= 1;
        }
        round_pack(sign, e, (p >> 64) as u64, p as u64, bits_of(pc), rc & 3)
    }

    pub fn div(self, b: F80, pc: u32, rc: u32) -> F80 {
        if let Some(n) = nan_operands(self, b) {
            return n;
        }
        let sign = self.sign() ^ b.sign();
        if self.is_inf() {
            return if b.is_inf() { INDEFINITE } else { F80::pack(sign, 0x7fff, 1 << 63) };
        }
        if b.is_inf() {
            return F80::pack(sign, 0, 0);
        }
        if b.is_zero() {
            return if self.is_zero() { INDEFINITE } else { F80::pack(sign, 0x7fff, 1 << 63) };
        }
        if self.is_zero() {
            return F80::pack(sign, 0, 0);
        }
        let (_, ea, ma) = self.unpack();
        let (_, eb, mb) = b.unpack();
        let (num, e) = if ma >= mb { ((ma as u128) << 63, ea - eb + BIAS) } else { ((ma as u128) << 64, ea - eb + BIAS - 1) };
        let (q, r) = (num / mb as u128, num % mb as u128);
        let n2 = r << 64;
        let (q2, r2) = (n2 / mb as u128, n2 % mb as u128);
        round_pack(sign, e, q as u64, q2 as u64 | (r2 != 0) as u64, bits_of(pc), rc & 3)
    }

    pub fn sqrt(self, pc: u32, rc: u32) -> F80 {
        if self.unsupported() {
            return INDEFINITE;
        }
        if self.is_nan() {
            return F80 { m: self.m | 0x4000_0000_0000_0000, se: self.se };
        }
        if self.is_zero() {
            return self;
        }
        if self.sign() {
            return INDEFINITE;
        }
        if self.is_inf() {
            return self;
        }
        let (_, e, m) = self.unpack();
        let k = e - BIAS;
        let (n, ez) = if k & 1 == 0 { ((m as u128) << 63, k / 2 + BIAS) } else { ((m as u128) << 64, (k - 1).div_euclid(2) + BIAS) };
        let s = isqrt(n);
        let r = n - s * s;
        let extra = if r == 0 { 0 } else if r > s { 0xc000_0000_0000_0000 } else { 0x4000_0000_0000_0000 };
        round_pack(false, ez, s as u64, extra, bits_of(pc), rc & 3)
    }
}

/// floor(sqrt(n))
fn isqrt(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    let mut x = libm::sqrt(n as f64) as u128;
    // (Newton steps from the estimate, then exact)
    for _ in 0..4 {
        if x == 0 {
            break;
        }
        x = (x + n / x) / 2;
    }
    while x.checked_mul(x).is_none_or(|p| p > n) {
        x -= 1;
    }
    while (x + 1).checked_mul(x + 1).is_some_and(|p| p <= n) {
        x += 1;
    }
    x
}

/// `m >> shift` rounded by `rc` (0 nearest even, 1 down, 2 up, 3 toward zero) for a value of
/// sign `sign`.
fn round_shift(m: u64, shift: u32, rc: u32, sign: bool) -> u64 {
    if shift == 0 {
        return m;
    }
    let x = m as u128;
    // (shifted out entirely: below half of the last place when shift > 64)
    let (q, rest, half) = if shift > 64 { (0, x, None) } else { (x >> shift, x & ((1u128 << shift) - 1), Some(1u128 << (shift - 1))) };
    if rest == 0 {
        return q as u64;
    }
    let up = match rc {
        0 => half.is_some_and(|h| rest > h || (rest == h && q & 1 != 0)),
        1 => sign,
        2 => !sign,
        _ => false,
    };
    q as u64 + up as u64
}

/// The x87's NaN rules for two operands (or an unsupported encoding: invalid): None when both
/// are numbers.
fn nan_operands(a: F80, b: F80) -> Option<F80> {
    if a.unsupported() || b.unsupported() {
        return Some(INDEFINITE);
    }
    if !a.is_nan() && !b.is_nan() {
        return None;
    }
    let quiet = |x: F80| F80 { m: x.m | 0xc000_0000_0000_0000, se: x.se };
    let (sa, sb) = (a.is_snan(), b.is_snan());
    if sa || sb {
        if sa && !sb {
            return Some(quiet(if b.is_nan() { b } else { a }));
        }
        if sb && !sa {
            return Some(quiet(if a.is_nan() { a } else { b }));
        }
    }
    // the larger magnitude (exponent, then significand); equal: the positive one
    let (ma, mb) = (a.se & 0x7fff, b.se & 0x7fff);
    let pick_b = if ma != mb { ma < mb } else if a.m != b.m { a.m < b.m } else { a.se >= b.se };
    Some(quiet(if pick_b { b } else { a }))
}

fn add_sub(a: F80, b: F80, subtract: bool, pc: u32, rc: u32) -> F80 {
    if let Some(n) = nan_operands(a, b) {
        return n;
    }
    let b = if subtract { b.neg() } else { b };
    let rc = rc & 3;
    if a.is_inf() || b.is_inf() {
        if a.is_inf() && b.is_inf() && a.sign() != b.sign() {
            return INDEFINITE;
        }
        return if a.is_inf() { a } else { b };
    }
    if a.is_zero() && b.is_zero() {
        let sign = if a.sign() == b.sign() { a.sign() } else { rc == 1 };
        return F80::pack(sign, 0, 0);
    }
    if a.is_zero() || b.is_zero() {
        // the other one, rounded to the precision
        let x = if a.is_zero() { b } else { a };
        let (s, e, m) = x.unpack();
        return round_pack(s, e, m, 0, bits_of(pc), rc);
    }
    let (sa, ea, ma) = a.unpack();
    let (sb, eb, mb) = b.unpack();
    // |x| >= |y|
    let ((sx, ex, mx), (sy, ey, my)) = if (ea, ma) >= (eb, mb) { ((sa, ea, ma), (sb, eb, mb)) } else { ((sb, eb, mb), (sa, ea, ma)) };
    let d = (ex - ey) as u32;
    let x = (mx as u128) << 63;
    let y0 = (my as u128) << 63;
    let y = if d >= 127 { 1 } else { (y0 >> d) | ((y0 & ((1u128 << d) - 1)) != 0) as u128 };
    let z = if sx == sy { x + y } else { x - y };
    if z == 0 {
        return F80::pack(rc == 1, 0, 0);
    }
    let lz = z.leading_zeros();
    let z = z << lz;
    round_pack(sx, ex + 1 - lz as i32, (z >> 64) as u64, z as u64, bits_of(pc), rc)
}

/// SoftFloat's roundPackToExtF80: `sig` with bit 63 set (unless the value is tiny), `extra`
/// the bits below it, `exp` biased (at most 0: tiny), `prec` 32/64/80.
fn round_pack(sign: bool, mut exp: i32, mut sig: u64, mut extra: u64, prec: u32, rc: u32) -> F80 {
    let near = rc == 0;
    let max = |s: bool| rc == if s { 1 } else { 2 }; // (rounding away from zero, directed)
    if prec != 80 {
        let (mut inc, mut mask): (u64, u64) = if prec == 64 { (0x400, 0x7ff) } else { (0x80_0000_0000, 0xff_ffff_ffff) };
        sig |= (extra != 0) as u64;
        if !near {
            inc = if max(sign) { mask } else { 0 };
        }
        let mut bits = sig & mask;
        if (exp - 1) as u32 >= 0x7ffd {
            if exp <= 0 {
                sig = jam64(sig, (1 - exp) as u32);
                bits = sig & mask;
                sig = sig.wrapping_add(inc);
                exp = (sig >> 63) as i32;
                let one = mask + 1;
                if near && bits << 1 == one {
                    mask |= one;
                }
                sig &= !mask;
                return F80::pack(sign, exp, sig);
            }
            if exp > 0x7ffe || (exp == 0x7ffe && sig.wrapping_add(inc) < sig) {
                return overflow(sign, rc, !mask);
            }
        }
        let s2 = sig.wrapping_add(inc);
        sig = s2;
        if sig < inc {
            exp += 1;
            sig = 1 << 63;
        }
        let one = mask + 1;
        if near && bits << 1 == one {
            mask |= one;
        }
        sig &= !mask;
        return F80::pack(sign, exp, sig);
    }
    let mut inc = extra >= 1 << 63;
    if !near {
        inc = max(sign) && extra != 0;
    }
    if (exp - 1) as u32 >= 0x7ffd {
        if exp <= 0 {
            let n = (1 - exp) as u32;
            (sig, extra) = jam64_extra(sig, extra, n);
            exp = 0;
            inc = extra >= 1 << 63;
            if !near {
                inc = max(sign) && extra != 0;
            }
            if inc {
                sig = sig.wrapping_add(1);
                if near && extra & 0x7fff_ffff_ffff_ffff == 0 {
                    sig &= !1;
                }
                exp = (sig >> 63) as i32;
            }
            return F80::pack(sign, exp, sig);
        }
        if exp > 0x7ffe || (exp == 0x7ffe && sig == u64::MAX && inc) {
            return overflow(sign, rc, u64::MAX);
        }
    }
    if inc {
        sig = sig.wrapping_add(1);
        if sig == 0 {
            exp += 1;
            sig = 1 << 63;
        } else if near && extra & 0x7fff_ffff_ffff_ffff == 0 {
            sig &= !1;
        }
    }
    F80::pack(sign, exp, sig)
}

/// Overflow: infinity when rounding to nearest or away from zero, else the largest finite value
/// of the precision (`largest`: its significand).
fn overflow(sign: bool, rc: u32, largest: u64) -> F80 {
    if rc == 0 || rc == if sign { 1 } else { 2 } {
        F80::pack(sign, 0x7fff, 1 << 63)
    } else {
        F80::pack(sign, 0x7ffe, largest)
    }
}

/// `a >> n` with the bits shifted out ORed into bit 0.
fn jam64(a: u64, n: u32) -> u64 {
    if n == 0 {
        a
    } else if n < 64 {
        a >> n | ((a << (64 - n)) != 0) as u64
    } else {
        (a != 0) as u64
    }
}

/// (a:extra) >> n: the new significand and the bits below it (the lowest jammed).
fn jam64_extra(a: u64, extra: u64, n: u32) -> (u64, u64) {
    if n == 0 {
        (a, extra)
    } else if n < 64 {
        (a >> n, a << (64 - n) | (extra != 0) as u64)
    } else if n == 64 {
        (0, a | (extra != 0) as u64)
    } else {
        (0, ((a != 0) || extra != 0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64) -> F80 {
        F80::from_f64(v)
    }

    /// Exact cases against f64 arithmetic: values whose results f64 holds exactly
    #[test]
    fn small_exact_results() {
        for (a, b) in [(1.5, 2.25), (-3.0, 0.125), (1e10, -7.0), (0.5, 0.5), (3.0, -3.0)] {
            assert_eq!(f(a).add(f(b), 3, 0), f(a + b), "{a} + {b}");
            assert_eq!(f(a).sub(f(b), 3, 0), f(a - b), "{a} - {b}");
            assert_eq!(f(a).mul(f(b), 3, 0), f(a * b), "{a} * {b}");
        }
        assert_eq!(f(1.0).div(f(4.0), 3, 0), f(0.25));
        assert_eq!(f(2.25).sqrt(3, 0), f(1.5));
        assert_eq!(f(3.0).sub(f(3.0), 3, 1), F80::pack(true, 0, 0)); // (round down: -0)
    }

    /// 1/3 at the three precisions, as Windows' x87 gives it (fp_regress.rs has the f64 side):
    /// 64 bits 0xAAAA...AAAB, 53 bits the f64 value, 24 bits the f32 one
    #[test]
    fn one_third() {
        let t = f(1.0).div(f(3.0), 3, 0);
        assert_eq!((t.se, t.m), (0x3ffd, 0xaaaa_aaaa_aaaa_aaab));
        assert_eq!(f(1.0).div(f(3.0), 2, 0), f(1.0 / 3.0));
        assert_eq!(f(1.0).div(f(3.0), 0, 0), f((1.0f32 / 3.0) as f64));
        // the 64-bit one stored to f64: rounds again, to the same nearest
        assert_eq!(t.to_f64(0), 1.0 / 3.0);
    }

    /// to_f64 / to_f32 against Rust's conversions of exactly representable or doubly
    /// representable values, subnormals and overflow per rounding control
    #[test]
    fn conversions() {
        for v in [0.0, -0.0, 1.0, -2.5, 1e-310, -4.9e-324, f64::MAX, f64::MIN_POSITIVE, 123456.789, f64::INFINITY] {
            assert_eq!(f(v).to_f64(0).to_bits(), v.to_bits(), "{v}");
            assert_eq!(f(v).to_f32(0).to_bits(), (v as f32).to_bits(), "{v}");
        }
        assert!(f(f64::NAN).to_f64(0).is_nan());
        // 2^1024: infinity to nearest, MAX toward zero
        let big = F80::pack(false, BIAS + 1024, 1 << 63);
        assert_eq!(big.to_f64(0), f64::INFINITY);
        assert_eq!(big.to_f64(3), f64::MAX);
        assert_eq!(big.neg().to_f64(2), -f64::MAX);
        assert_eq!(big.neg().to_f64(1), f64::NEG_INFINITY);
        // 1 + 2^-60: up gives the next f64, nearest and down 1
        let x = F80::pack(false, BIAS, 1 << 63 | 8);
        assert_eq!(x.to_f64(0), 1.0);
        assert_eq!(x.to_f64(2), 1.0 + f64::EPSILON);
        assert_eq!(x.to_f64(1), 1.0);
        assert_eq!(F80::from_i64(i64::MIN).to_int(0), Some(i64::MIN as i128));
        assert_eq!(f(2.5).to_int(0), Some(2));
        assert_eq!(f(-2.5).to_int(1), Some(-3));
        assert_eq!(f(2.5).round_int(2), f(3.0));
    }

    #[test]
    fn specials() {
        let inf = f(f64::INFINITY);
        assert_eq!(inf.sub(inf, 3, 0), INDEFINITE);
        assert_eq!(f(0.0).mul(inf, 3, 0), INDEFINITE);
        assert_eq!(f(0.0).div(f(0.0), 3, 0), INDEFINITE);
        assert_eq!(f(-1.0).sqrt(3, 0), INDEFINITE);
        assert_eq!(f(-0.0).sqrt(3, 0), f(-0.0));
        assert_eq!(f(1.0).div(f(0.0), 3, 0), inf);
        // a QNaN operand comes back (quiet); two: the larger significand
        let q1 = F80 { m: 0xc000_0000_0000_0001, se: 0x7fff };
        let q2 = F80 { m: 0xc000_0000_0000_0002, se: 0xffff };
        assert_eq!(f(1.0).add(q1, 3, 0), q1);
        assert_eq!(q1.mul(q2, 3, 0), q2);
        let s = F80 { m: 0x8000_0000_0000_0005, se: 0x7fff };
        assert_eq!(s.add(f(1.0), 3, 0), F80 { m: 0xc000_0000_0000_0005, se: 0x7fff });
        // an unnormal is invalid
        assert_eq!(F80 { m: 1, se: 0x3fff }.add(f(1.0), 3, 0), INDEFINITE);
        assert_eq!(f(1.0).compare(f(-0.0)), Some(core::cmp::Ordering::Greater));
        assert_eq!(f(0.0).compare(f(-0.0)), Some(core::cmp::Ordering::Equal));
        assert_eq!(q1.compare(f(0.0)), None);
    }
}
