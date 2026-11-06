//! Vorbis I decoding written to give what libvorbis gives, so that sake can take over a game's
//! own (x86) libvorbis at its packet decoding step (mapping0_inverse) and hand back the same
//! spectrum-to-time samples. Also a whole-stream decoder (windows, overlap-add) and an Ogg page
//! reader, for checking it against other decoders.

pub mod bits;
pub mod codebook;
pub mod decode;
pub mod floor1_table;
pub mod imdct;
pub mod ogg;
pub mod setup;

pub use bits::Bits;
pub use setup::{Ident, Setup};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// a malformed header
    Header,
    /// a codebook whose lengths are not a tree
    Codebook,
    /// a stream this decoder does not handle (floor 0)
    Unsupported,
}

/// The Vorbis window's slope rising over `len` samples.
fn slope(len: usize) -> Vec<f32> {
    let pi = std::f64::consts::PI;
    (0..len)
        .map(|i| {
            let s = ((i as f64 + 0.5) / len as f64 * pi / 2.0).sin();
            (pi / 2.0 * s * s).sin() as f32
        })
        .collect()
}

/// The window of a stream's blocks (libvorbis's _vorbis_apply_window).
pub struct Window {
    blocksize: [usize; 2],
    slopes: [Vec<f32>; 2],
}

impl Window {
    pub fn new(blocksize: [usize; 2]) -> Window {
        Window { blocksize, slopes: [slope(blocksize[0] / 2), slope(blocksize[1] / 2)] }
    }

    /// Windows a block of size `w` whose neighbours have sizes `lw` and `nw` (each 0 short,
    /// 1 long; a short block's neighbours count as short).
    pub fn apply(&self, d: &mut [f32], lw: usize, w: usize, nw: usize) {
        let (lw, nw) = if w == 1 { (lw.min(1), nw.min(1)) } else { (0, 0) };
        let n = self.blocksize[w];
        let (ln, rn) = (self.blocksize[lw], self.blocksize[nw]);
        let ls = n / 4 - ln / 4;
        let rs = n / 2 + n / 4 - rn / 4;
        let (lsl, rsl) = (&self.slopes[lw], &self.slopes[nw]);
        for (i, s) in d[..n].iter_mut().enumerate() {
            *s *= if i < ls {
                0.0
            } else if i < ls + ln / 2 {
                lsl[i - ls]
            } else if i < rs {
                1.0
            } else if i < rs + rn / 2 {
                rsl[rn / 2 - 1 - (i - rs)]
            } else {
                0.0
            };
        }
    }
}

/// A whole stream: packets in, interleaved float samples out (libvorbis's synthesis, blockin
/// and pcmout as the specification describes them).
pub struct Decoder {
    pub setup: Setup,
    state: decode::State,
    window: Window,
    /// the previous block, windowed, and its size (0: none yet)
    prev: Vec<Vec<f32>>,
    prev_n: usize,
    block: Vec<Vec<f32>>,
}

impl Decoder {
    /// From the identification and setup headers (the comment header is not needed).
    pub fn new(ident: &[u8], setup: &[u8]) -> Result<Decoder, Error> {
        let setup = Setup::read(Ident::read(ident)?, setup)?;
        let state = decode::State::new(&setup);
        let [s, l] = setup.ident.blocksize;
        let ch = setup.ident.channels;
        Ok(Decoder { window: Window::new([s, l]), state, prev: vec![vec![0.0; l]; ch], prev_n: 0, block: vec![vec![0.0; l]; ch], setup })
    }

    /// Decodes an audio packet; appends its finished samples (interleaved) to `out`. A packet
    /// that is not audio is skipped (as libvorbis).
    pub fn packet(&mut self, packet: &[u8], out: &mut Vec<f32>) {
        let mut b = Bits::new(packet);
        let Some((mode, pw, nw)) = decode::packet_header(&self.setup, &mut b) else { return };
        let w = self.setup.modes[mode].blockflag as usize;
        let [bs0, bs1] = self.setup.ident.blocksize;
        let n = [bs0, bs1][w];
        {
            let mut pcm: Vec<&mut [f32]> = self.block.iter_mut().map(|v| &mut v[..n]).collect();
            decode::mapping_inverse(&self.setup, &mut self.state, &mut b, mode, &mut pcm);
        }
        for v in self.block.iter_mut() {
            self.window.apply(v, pw as usize, w, nw as usize);
        }
        // overlap-add: from the previous block's centre to this one's
        let pn = self.prev_n;
        if pn > 0 {
            let len = pn / 4 + n / 4;
            let ch = self.setup.ident.channels;
            let base = out.len();
            out.resize(base + len * ch, 0.0);
            for c in 0..ch {
                for j in 0..len {
                    let p = pn / 2 + j;
                    let mut s = if p < pn { self.prev[c][p] } else { 0.0 };
                    let q = j as isize - (pn / 4) as isize + (n / 4) as isize;
                    if q >= 0 && (q as usize) < n {
                        s += self.block[c][q as usize];
                    }
                    out[base + j * ch + c] = s;
                }
            }
        }
        std::mem::swap(&mut self.prev, &mut self.block);
        self.prev_n = n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where two blocks overlap, their windows' squares add up to 1 (Princen-Bradley), for
    /// every combination of block sizes; outside the slopes a window is 0 or 1.
    #[test]
    fn window_power_complementary() {
        let bs = [256, 2048];
        let win = Window::new(bs);
        for lw in 0..2 {
            for w in 0..2 {
                for nw in 0..2 {
                    let n = bs[w];
                    let mut d = vec![1f32; n];
                    win.apply(&mut d, lw, w, nw);
                    // the right half of this block against a next block of size nw
                    let nn = bs[if w == 1 { nw } else { 0 }];
                    let mut next = vec![1f32; nn];
                    win.apply(&mut next, w, if w == 1 { nw } else { 0 }, 0);
                    // the overlap (r samples' slope): this block's from 3n/4 - r/4, the
                    // next's from nn/4 - r/4
                    let r = bs[w.min(if w == 1 { nw } else { 0 })];
                    let (a0, b0) = (3 * n / 4 - r / 4, nn / 4 - r / 4);
                    for i in 0..r / 2 {
                        let s = d[a0 + i].powi(2) + next[b0 + i].powi(2);
                        assert!((s - 1.0).abs() < 1e-5, "lw {lw} w {w} nw {nw} i {i}: {s}");
                    }
                    assert!(d.iter().all(|v| (0.0..=1.0).contains(v)));
                }
            }
        }
    }
}
