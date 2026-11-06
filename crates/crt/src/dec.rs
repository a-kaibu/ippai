//! Exact decimal expansion of doubles and correctly rounded digit strings (printf's %f %e %g).
//! A finite double is m * 2^e exactly; for e < 0 that is m * 5^-e / 10^-e, so its decimal
//! digits are those of the integer m * 5^-e. A small bignum does the rest.

/// How a tie (the dropped digits are exactly 5000...) rounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tie {
    /// to the even neighbour (IEEE 754; the UCRT since Windows 10 2004, glibc)
    Even,
    /// away from zero (the legacy msvcrt: "exactly representable values ending in 5 round up")
    Away,
}

/// The exact decimal digits of `v` (finite, sign ignored): v = 0.d1 d2 d3 ... * 10^point.
/// No leading or trailing zeros; zero is (empty, 0).
pub fn digits(v: f64) -> (Vec<u8>, i32) {
    let bits = v.to_bits() & !(1 << 63);
    let (exp, frac) = ((bits >> 52) as i32, bits & ((1 << 52) - 1));
    if exp == 0 && frac == 0 {
        return (Vec::new(), 0);
    }
    let (m, e) = if exp == 0 { (frac, -1074) } else { (frac | 1 << 52, exp - 1075) };
    let mut n = Big::from(m);
    let shift = if e >= 0 {
        n.shl(e as u32);
        0
    } else {
        for _ in 0..(-e) / 13 {
            n.mul(1_220_703_125); // 5^13
        }
        for _ in 0..(-e) % 13 {
            n.mul(5);
        }
        -e
    };
    let mut d = n.to_decimal();
    let point = d.len() as i32 - shift;
    while d.last() == Some(&0) {
        d.pop();
    }
    (d, point)
}

/// Rounds the digit string (as `digits` gives it) to `keep` digits counted from its first one
/// (`keep` may be <= 0: the value is then below one unit of the kept place). Returns `keep`
/// digits and the new point (one more when 9.99 carried into 10.0); a carry with nothing kept
/// gives the single digit 1. Digits are positional: digit i is worth 10^(point-1-i).
pub fn round(d: &[u8], point: i32, keep: i32, tie: Tie) -> (Vec<u8>, i32) {
    if keep < 0 {
        return (Vec::new(), point - keep);
    }
    let k = keep as usize;
    let mut out: Vec<u8> = d.iter().take(k).copied().collect();
    out.resize(k, 0);
    let up = match d.get(k) {
        None => false,
        Some(&f) if f > 5 => true,
        Some(&f) if f < 5 => false,
        Some(_) => {
            let rest = d[k + 1..].iter().any(|&x| x != 0);
            rest || match tie {
                Tie::Away => true,
                Tie::Even => k > 0 && out[k - 1] % 2 == 1,
            }
        }
    };
    if !up {
        return (out, point);
    }
    let mut i = k;
    loop {
        if i == 0 {
            // all nines (or nothing kept): 1 followed by zeros, one place up
            out.insert(0, 1);
            out.truncate(k.max(1));
            return (out, point + 1);
        }
        i -= 1;
        if out[i] == 9 {
            out[i] = 0;
        } else {
            out[i] += 1;
            return (out, point);
        }
    }
}

/// Unsigned bignum, 32-bit limbs, little-endian.
struct Big(Vec<u32>);

impl Big {
    fn from(v: u64) -> Big {
        Big(vec![v as u32, (v >> 32) as u32])
    }
    fn mul(&mut self, k: u32) {
        let mut carry = 0u64;
        for l in self.0.iter_mut() {
            let x = *l as u64 * k as u64 + carry;
            *l = x as u32;
            carry = x >> 32;
        }
        if carry != 0 {
            self.0.push(carry as u32);
        }
    }
    fn shl(&mut self, s: u32) {
        let (words, bits) = ((s / 32) as usize, s % 32);
        if bits != 0 {
            let mut carry = 0u32;
            for l in self.0.iter_mut() {
                let x = *l;
                *l = x << bits | carry;
                carry = x >> (32 - bits);
            }
            if carry != 0 {
                self.0.push(carry);
            }
        }
        self.0.splice(0..0, std::iter::repeat_n(0, words));
    }
    /// Divides by `k` in place; returns the remainder.
    fn div(&mut self, k: u32) -> u32 {
        let mut rem = 0u64;
        for l in self.0.iter_mut().rev() {
            let x = rem << 32 | *l as u64;
            *l = (x / k as u64) as u32;
            rem = x % k as u64;
        }
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        rem as u32
    }
    /// The decimal digits (values 0-9), most significant first, no leading zeros.
    fn to_decimal(mut self) -> Vec<u8> {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        let mut chunks = Vec::new();
        while !self.0.is_empty() {
            chunks.push(self.div(1_000_000_000));
        }
        let mut out = Vec::with_capacity(chunks.len() * 9);
        for (i, c) in chunks.iter().rev().enumerate() {
            let s = if i == 0 { c.to_string() } else { format!("{c:09}") };
            out.extend(s.bytes().map(|b| b - b'0'));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: f64) -> String {
        let (d, p) = digits(v);
        format!("0.{}e{p}", d.iter().map(|x| (b'0' + x) as char).collect::<String>())
    }

    #[test]
    fn exact_digits() {
        assert_eq!(text(1.0), "0.1e1");
        assert_eq!(text(0.5), "0.5e0");
        assert_eq!(text(0.1), "0.1000000000000000055511151231257827021181583404541015625e0");
        assert_eq!(text(1e23), "0.99999999999999991611392e23");
        assert_eq!(text(f64::from_bits(1)).len(), "0.".len() + 751 + "e-323".len());
        assert_eq!(text(0.0), "0.e0");
    }

    #[test]
    fn rounding() {
        let (d, p) = digits(0.5);
        assert_eq!(round(&d, p, 0, Tie::Even), (vec![], 0));
        assert_eq!(round(&d, p, 0, Tie::Away), (vec![1], 1));
        let (d, p) = digits(2.5);
        assert_eq!(round(&d, p, 1, Tie::Even), (vec![2], 1));
        assert_eq!(round(&d, p, 1, Tie::Away), (vec![3], 1));
        let (d, p) = digits(9.96);
        assert_eq!(round(&d, p, 2, Tie::Even), (vec![1, 0], 2));
    }
}
