//! The inverse MDCT of the Vorbis I specification (what libvorbis's mdct_backward computes):
//! n/2 coefficients X to n samples
//!   y[i] = sum_k X[k] cos(2 pi / n (i + 1/2 + n/4) (k + 1/2)),
//! through a DCT-IV done with an n/8-point complex FFT.

#[derive(Clone, Copy, Default)]
struct C {
    re: f32,
    im: f32,
}

impl C {
    #[inline]
    fn mul(self, o: C) -> C {
        C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
}

fn cis(t: f64) -> C {
    C { re: t.cos() as f32, im: t.sin() as f32 }
}

pub struct Imdct {
    n: usize,
    /// e^(-i pi (k + 1/4) / m), e^(-i pi j / m) (m = n / 2)
    pre: Vec<C>,
    post: Vec<C>,
    /// the FFT's twiddles and bit reversal
    tw: Vec<C>,
    rev: Vec<u32>,
    buf: Vec<C>,
    u: Vec<f32>,
}

impl Imdct {
    pub fn new(n: usize) -> Imdct {
        assert!(n.is_power_of_two() && n >= 16);
        let m = n / 2;
        let p = m / 2;
        let pi = std::f64::consts::PI;
        let pre = (0..p).map(|k| cis(-pi * (k as f64 + 0.25) / m as f64)).collect();
        let post = (0..p).map(|j| cis(-pi * j as f64 / m as f64)).collect();
        let tw = (0..p / 2).map(|k| cis(-2.0 * pi * k as f64 / p as f64)).collect();
        let bits = p.trailing_zeros();
        let rev = (0..p as u32).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (32 - bits) }).collect();
        Imdct { n, pre, post, tw, rev, buf: vec![C::default(); p], u: vec![0.0; m] }
    }

    fn fft(&mut self) {
        let p = self.buf.len();
        for i in 0..p {
            let j = self.rev[i] as usize;
            if i < j {
                self.buf.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= p {
            let step = p / len;
            for s in (0..p).step_by(len) {
                for k in 0..len / 2 {
                    let w = self.tw[k * step];
                    let a = self.buf[s + k];
                    let b = self.buf[s + k + len / 2].mul(w);
                    self.buf[s + k] = C { re: a.re + b.re, im: a.im + b.im };
                    self.buf[s + k + len / 2] = C { re: a.re - b.re, im: a.im - b.im };
                }
            }
            len <<= 1;
        }
    }

    /// The DCT-IV of x (m values) into self.u.
    fn dct4(&mut self, x: &[f32]) {
        let m = x.len();
        let p = m / 2;
        for k in 0..p {
            self.buf[k] = C { re: x[2 * k], im: x[m - 1 - 2 * k] }.mul(self.pre[k]);
        }
        self.fft();
        for j in 0..p {
            let w = self.buf[j].mul(self.post[j]);
            self.u[2 * j] = w.re;
            self.u[m - 1 - 2 * j] = -w.im;
        }
    }

    /// In place: the first n/2 values of `v` (n long) are the coefficients; all n become samples.
    pub fn backward(&mut self, v: &mut [f32]) {
        let n = self.n;
        let m = n / 2;
        let h = m / 2;
        self.dct4(&v[..m]);
        let u = &self.u;
        for (i, y) in v.iter_mut().enumerate().take(n) {
            *y = if i < h {
                u[i + h]
            } else if i < 3 * h {
                -u[3 * h - 1 - i]
            } else {
                -u[i - 3 * h]
            };
        }
    }
}

/// The definition, for tests.
pub fn naive(x: &[f32]) -> Vec<f32> {
    let m = x.len();
    let n = 2 * m;
    let pi = std::f64::consts::PI;
    (0..n)
        .map(|i| {
            x.iter()
                .enumerate()
                .map(|(k, &v)| v as f64 * (2.0 * pi / n as f64 * (i as f64 + 0.5 + n as f64 / 4.0) * (k as f64 + 0.5)).cos())
                .sum::<f64>() as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn check(x: &[f32]) -> Result<(), TestCaseError> {
        let want = naive(x);
        let mut v = x.to_vec();
        v.resize(2 * x.len(), 0.0);
        Imdct::new(2 * x.len()).backward(&mut v);
        let scale = x.iter().map(|v| v.abs()).sum::<f32>().max(1.0);
        for (i, (a, b)) in v.iter().zip(&want).enumerate() {
            prop_assert!((a - b).abs() <= 2e-5 * scale, "sample {} of {}: {} vs {}", i, v.len(), a, b);
        }
        Ok(())
    }

    #[test]
    fn impulses() {
        for n in [16usize, 64, 256, 2048] {
            for k in [0, 1, n / 4, n / 2 - 1] {
                let mut x = vec![0f32; n / 2];
                x[k] = 1.0;
                check(&x).unwrap();
            }
        }
    }

    proptest! {
        #[test]
        fn random(bits in 3u32..9, seed in prop::collection::vec(-1000f32..1000.0, 256)) {
            let m = 1usize << bits;
            let x: Vec<f32> = (0..m).map(|i| seed[i % seed.len()] * (1.0 + i as f32 / 7.0).sin()).collect();
            check(&x)?;
        }
    }
}
