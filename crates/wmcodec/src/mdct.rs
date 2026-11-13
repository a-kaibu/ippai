//! The inverse MDCT FFmpeg's WMA decoder uses (the old ff_imdct_calc: pre-rotation, an
//! n/4-point complex inverse FFT, post-rotation), with its scale.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C {
    pub re: f32,
    pub im: f32,
}

/// An in-place radix-2 complex FFT of exp(+2 pi i jk / n) (FFmpeg's inverse FFT, unscaled).
pub struct Fft {
    n: usize,
    rev: Vec<u32>,
    tw: Vec<C>,
}

impl Fft {
    pub fn new(bits: u32) -> Fft {
        let n = 1usize << bits;
        let rev = (0..n as u32).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (32 - bits) }).collect();
        let tw = (0..n / 2).map(|k| {
            let a = 2.0 * std::f64::consts::PI * k as f64 / n as f64;
            C { re: a.cos() as f32, im: a.sin() as f32 }
        }).collect();
        Fft { n, rev, tw }
    }
    pub fn run(&self, z: &mut [C]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i] as usize;
            if i < j {
                z.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for s in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let w = self.tw[k * step];
                    let a = z[s + k];
                    let b = z[s + k + len / 2];
                    let t = C { re: b.re * w.re - b.im * w.im, im: b.re * w.im + b.im * w.re };
                    z[s + k] = C { re: a.re + t.re, im: a.im + t.im };
                    z[s + k + len / 2] = C { re: a.re - t.re, im: a.im - t.im };
                }
            }
            len <<= 1;
        }
    }
}

/// The inverse MDCT of n/2 coefficients into n samples (`bits`: log2 n).
pub struct Imdct {
    n: usize,
    tcos: Vec<f32>,
    tsin: Vec<f32>,
    fft: Fft,
    z: Vec<C>,
}

impl Imdct {
    pub fn new(bits: u32, scale: f64) -> Imdct {
        let n = 1usize << bits;
        let n4 = n / 4;
        let theta = 1.0 / 8.0 + if scale < 0.0 { n4 as f64 } else { 0.0 };
        let sc = scale.abs().sqrt();
        let mut tcos = Vec::with_capacity(n4);
        let mut tsin = Vec::with_capacity(n4);
        for i in 0..n4 {
            let alpha = 2.0 * std::f64::consts::PI * (i as f64 + theta) / n as f64;
            tcos.push((-alpha.cos() * sc) as f32);
            tsin.push((-alpha.sin() * sc) as f32);
        }
        Imdct { n, tcos, tsin, fft: Fft::new(bits - 2), z: vec![C::default(); n4] }
    }

    /// n/2 samples: the middle half of the full output (ff_imdct_half).
    fn half(&mut self, out: &mut [f32], input: &[f32]) {
        let (n2, n4, n8) = (self.n / 2, self.n / 4, self.n / 8);
        for k in 0..n4 {
            let (a, b) = (input[n2 - 1 - 2 * k], input[2 * k]);
            let (c, s) = (self.tcos[k], self.tsin[k]);
            self.z[k] = C { re: a * c - b * s, im: a * s + b * c };
        }
        self.fft.run(&mut self.z);
        let z = &mut self.z;
        for k in 0..n8 {
            let (p, q) = (n8 - k - 1, n8 + k);
            let (r0, i1) = {
                let (a, b, c, s) = (z[p].im, z[p].re, self.tsin[p], self.tcos[p]);
                (a * c - b * s, a * s + b * c)
            };
            let (r1, i0) = {
                let (a, b, c, s) = (z[q].im, z[q].re, self.tsin[q], self.tcos[q]);
                (a * c - b * s, a * s + b * c)
            };
            z[p] = C { re: r0, im: i0 };
            z[q] = C { re: r1, im: i1 };
        }
        for k in 0..n4 {
            out[2 * k] = z[k].re;
            out[2 * k + 1] = z[k].im;
        }
    }

    /// The n samples of n/2 coefficients.
    pub fn calc(&mut self, out: &mut [f32], input: &[f32]) {
        let (n, n2, n4) = (self.n, self.n / 2, self.n / 4);
        self.half(&mut out[n4..n4 + n2], input);
        for k in 0..n4 {
            out[k] = -out[n2 - k - 1];
            out[n - k - 1] = out[n2 + k];
        }
    }
}

/// The definition, for tests: y[j] = sum_k X[k] cos(2 pi / n (j + 1/2 + n/4)(k + 1/2)) * scale'.
#[cfg(test)]
pub fn imdct_naive(input: &[f32], scale: f64) -> Vec<f32> {
    let n2 = input.len();
    let n = 2 * n2;
    (0..n)
        .map(|j| {
            let mut s = 0.0f64;
            for (k, &x) in input.iter().enumerate() {
                s += x as f64 * (2.0 * std::f64::consts::PI / n as f64 * (j as f64 + 0.5 + n as f64 / 4.0) * (k as f64 + 0.5)).cos();
            }
            (s * scale) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The fast transform is the definition (up to the sign and scale FFmpeg's has).
        #[test]
        fn fast_is_the_definition(bits in 4u32..=12, seed in any::<u64>()) {
            let n = 1usize << bits;
            let mut x = seed;
            let input: Vec<f32> = (0..n / 2).map(|_| { x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); ((x >> 40) as f32 / 16777216.0) - 0.5 }).collect();
            let mut m = Imdct::new(bits, 1.0);
            let mut out = vec![0.0; n];
            m.calc(&mut out, &input);
            let want = imdct_naive(&input, -1.0);
            let err = out.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            prop_assert!(err < 2e-3 * (n as f32).sqrt(), "err {err}");
        }
    }
}
