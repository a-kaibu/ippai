//! xorshift32: the generator of `fill` (SPEC.md) and of the case generators.

#[derive(Clone, Debug)]
pub struct XorShift32(pub u32);

impl XorShift32 {
    pub fn new(seed: u32) -> Self {
        // (0 is a fixed point of xorshift: the executor uses the same substitute)
        XorShift32(if seed == 0 { 0x9e37_79b9 } else { seed })
    }
    pub fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// uniform enough in lo..=hi (modulo bias is irrelevant for test generation)
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() as i64).rem_euclid(hi - lo + 1)
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.range(0, xs.len() as i64 - 1) as usize]
    }
    pub fn chance(&mut self, n: u32) -> bool {
        self.next() % n == 0
    }
}
