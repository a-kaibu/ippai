//! The DSP of the built-in XAPOs (xapo.rs): FXEQ (four peaking bands), FXMasteringLimiter, FXEcho,
//! FXReverb and AudioReverb (a Schroeder-Moorer reverberator: parallel damped combs into series
//! all-passes, after Freeverb's layout), on interleaved float frames of any channel count. Each
//! effect reads its parameter block (the SDK's structure, as SetParameters gave it, or empty
//! for the defaults) on every pass, so a change takes effect on the next one.

/// Little-endian f32 / u32 field `i` (4-byte units) of a parameter block, or `def`
fn f(p: &[u8], i: usize, def: f32) -> f32 {
    p.get(4 * i..4 * i + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).filter(|v| v.is_finite()).unwrap_or(def)
}
fn u(p: &[u8], i: usize, def: u32) -> u32 {
    p.get(4 * i..4 * i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(def)
}
fn db(v: f32) -> f32 {
    10f32.powf(v / 20.0)
}

/// XAudio2's limits (XAUDIO2_MAX_AUDIO_CHANNELS, XAUDIO2_MIN/MAX_SAMPLE_RATE): a format outside
/// them is passed through untouched (no channels: nothing to do; an absurd rate or channel
/// count: no delay lines of hundreds of MB on the audio thread)
pub const MAX_CHANNELS: usize = 64;
pub const RATES: std::ops::RangeInclusive<u32> = 1000..=200_000;
pub fn sane(ch: usize, rate: u32) -> bool {
    (1..=MAX_CHANNELS).contains(&ch) && RATES.contains(&rate)
}

/// One biquad section per channel (direct form I)
#[derive(Clone, Default)]
struct Biquad {
    c: [f64; 5],
    z: Vec<[f64; 4]>,
    /// what `c` was computed for (rate, f0, gain, bw as bits): kept while they stay
    key: Option<(u32, u32, u32, u32)>,
}

impl Biquad {
    /// RBJ peaking EQ: centre `f0` Hz, linear amplitude `gain` at the centre, `bw` octaves wide.
    /// In f64: at 20 Hz with a narrow band the poles are within 1e-4 of 1, where f32 coefficients
    /// and state lose the response
    fn peaking(&mut self, rate: f32, f0: f32, gain: f32, bw: f32) {
        let key = Some((rate.to_bits(), f0.to_bits(), gain.to_bits(), bw.to_bits()));
        if self.key == key {
            return;
        }
        self.key = key;
        let (rate, f0, gain, bw) = (rate as f64, f0 as f64, gain as f64, bw as f64);
        let a = gain.sqrt();
        let w0 = 2.0 * std::f64::consts::PI * f0 / rate;
        let (s, cs) = w0.sin_cos();
        let alpha = s * ((2f64.ln() / 2.0) * bw * w0 / s).sinh();
        let a0 = 1.0 + alpha / a;
        self.c = [(1.0 + alpha * a) / a0, (-2.0 * cs) / a0, (1.0 - alpha * a) / a0, (-2.0 * cs) / a0, (1.0 - alpha / a) / a0];
    }
    fn run(&mut self, buf: &mut [f32], ch: usize) {
        if self.z.len() != ch {
            self.z = vec![[0.0; 4]; ch];
        }
        let [b0, b1, b2, a1, a2] = self.c;
        for fr in buf.chunks_exact_mut(ch) {
            for (x, z) in fr.iter_mut().zip(self.z.iter_mut()) {
                let xi = *x as f64;
                let y = b0 * xi + b1 * z[0] + b2 * z[1] - a1 * z[2] - a2 * z[3];
                *z = [xi, z[0], y, z[2]];
                *x = y as f32;
            }
        }
    }
}

/// FXEQ_PARAMETERS { FrequencyCenter, Gain, Bandwidth } x 4
#[derive(Default)]
pub struct Eq {
    bands: [Biquad; 4],
}
pub const EQ_CENTRES: [f32; 4] = [100.0, 800.0, 2000.0, 10000.0];

impl Eq {
    pub fn run(&mut self, p: &[u8], buf: &mut [f32], ch: usize, rate: u32) {
        if !sane(ch, rate) {
            return;
        }
        let fs = rate as f32;
        for (i, b) in self.bands.iter_mut().enumerate() {
            let f0 = f(p, 3 * i, EQ_CENTRES[i]).clamp(20.0, 20000.0).min(fs * 0.49);
            let g = f(p, 3 * i + 1, 1.0).clamp(0.126, 7.94);
            let bw = f(p, 3 * i + 2, 1.0).clamp(0.1, 2.0);
            // (a band at unity gain is the identity: skipped)
            if (g - 1.0).abs() < 1e-6 {
                b.z.clear();
                continue;
            }
            b.peaking(fs, f0, g, bw);
            b.run(buf, ch);
        }
    }
}

/// FXMASTERINGLIMITER_PARAMETERS { Release (1-20), Loudness (1-1800) }: the input is scaled by
/// Loudness / 1000 and held under full scale by a gain that drops at once and recovers over
/// Release x 10 ms
pub struct Limiter {
    gain: f32,
}
impl Default for Limiter {
    fn default() -> Self {
        Limiter { gain: 1.0 }
    }
}
impl Limiter {
    pub fn run(&mut self, p: &[u8], buf: &mut [f32], ch: usize, rate: u32) {
        if !sane(ch, rate) {
            return;
        }
        let release = u(p, 0, 6).clamp(1, 20) as f32 * 0.01;
        let loud = u(p, 1, 1000).clamp(1, 1800) as f32 / 1000.0;
        let k = (-1.0 / (release * rate as f32)).exp();
        for fr in buf.chunks_exact_mut(ch) {
            let peak = fr.iter().fold(0f32, |m, x| m.max((x * loud).abs()));
            let want = if peak > 1.0 { 1.0 / peak } else { 1.0 };
            self.gain = if want < self.gain { want } else { want + (self.gain - want) * k };
            for x in fr.iter_mut() {
                *x = (*x * loud * self.gain).clamp(-1.0, 1.0);
            }
        }
    }
}

/// FXECHO_PARAMETERS { WetDryMix (0-1), Feedback (0-1), Delay (1-2000 ms) }; FXECHO_INITDATA
/// { MaxDelay } bounds the delay
#[derive(Default)]
pub struct Echo {
    line: Vec<f32>,
    at: usize,
    pub max_delay_ms: f32,
}
impl Echo {
    pub fn with_max(max_delay_ms: f32) -> Echo {
        Echo { max_delay_ms, ..Default::default() }
    }
    pub fn run(&mut self, p: &[u8], buf: &mut [f32], ch: usize, rate: u32) {
        if !sane(ch, rate) {
            return;
        }
        let max = if self.max_delay_ms > 0.0 { self.max_delay_ms.clamp(1.0, 2000.0) } else { 2000.0 };
        let wet = f(p, 0, 0.5).clamp(0.0, 1.0);
        let fb = f(p, 1, 0.5).clamp(0.0, 1.0);
        let delay = f(p, 2, 500.0).clamp(1.0, max);
        let cap = ((max / 1000.0 * rate as f32).ceil() as usize + 1) * ch;
        if self.line.len() != cap {
            self.line = vec![0.0; cap];
            self.at = 0;
        }
        let d = ((delay / 1000.0 * rate as f32).round() as usize).max(1) * ch;
        let n = self.line.len();
        for x in buf.iter_mut() {
            let r = (self.at + n - d) % n;
            let e = self.line[r];
            self.line[self.at] = *x + fb * e;
            *x = (1.0 - wet) * *x + wet * e;
            self.at = (self.at + 1) % n;
        }
    }
}

/// A delay line of one comb or all-pass: `len` samples of `buf` (made for the longest the
/// room size can ask for, so that a room size the game animates changes the length without
/// a new, empty line: no click, no allocation per change)
#[derive(Clone, Default)]
struct Line {
    buf: Vec<f32>,
    len: usize,
    at: usize,
    lp: f32,
}

/// Freeverb's tunings at 44.1 kHz (the right channels' combs are 23 samples longer)
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;

/// The reverberator: per channel, the combs (with damping) summed, then the all-passes
#[derive(Default)]
pub struct Reverb {
    combs: Vec<Vec<Line>>,
    alls: Vec<Vec<Line>>,
    /// (channels, rate) the lines were made for
    key: (usize, u32),
    /// the lengths' scale (bits) and decay the comb feedbacks `fbs` are of
    shape: (u32, u32),
    fbs: Vec<Vec<f32>>,
}

/// The largest comb scale a shape gives (fx: 0.5 + RoomSize 1; audio: 1)
const MAX_SCALE: f32 = 1.5;

/// What a reverb pass needs: the comb lengths' scale, the decay (feedback per comb from it),
/// damping, all-pass gain, and the dry / wet levels
pub struct ReverbShape {
    pub scale: f32,
    pub decay_s: f32,
    pub damp: f32,
    pub diffusion: f32,
    pub dry: f32,
    pub wet: f32,
}

impl ReverbShape {
    /// FXREVERB_PARAMETERS { Diffusion (0-1), RoomSize (0.0001-1) }: the reverberation added
    /// to the signal
    pub fn fx(p: &[u8]) -> ReverbShape {
        let diffusion = f(p, 0, 0.9).clamp(0.0, 1.0);
        let room = f(p, 1, 0.6).clamp(0.0001, 1.0);
        ReverbShape { scale: 0.5 + room, decay_s: 0.3 + 2.7 * room, damp: 0.25, diffusion, dry: 1.0, wet: 0.5 }
    }
    /// XAUDIO2FX_REVERB_PARAMETERS: WetDryMix (%), then (after the delays and the byte
    /// fields, from offset 20) RoomFilterFreq, RoomFilterMain, RoomFilterHF, ReflectionsGain,
    /// ReverbGain, DecayTime, Density, RoomSize (the floats lie at the same offsets in every
    /// version's layout)
    pub fn audio(p: &[u8]) -> ReverbShape {
        let mix = f(p, 0, 100.0).clamp(0.0, 100.0) / 100.0;
        let main = db(f(p, 6, 0.0).clamp(-100.0, 0.0));
        let hf = db(f(p, 7, 0.0).clamp(-100.0, 0.0));
        let gain = db(f(p, 9, 0.0).clamp(-100.0, 20.0));
        let decay = f(p, 10, 1.0).clamp(0.1, 1000.0);
        let density = f(p, 11, 100.0).clamp(0.0, 100.0) / 100.0;
        let room = f(p, 12, 100.0).clamp(1.0, 100.0);
        ReverbShape { scale: 0.4 + 0.6 * room / 100.0, decay_s: decay, damp: (1.0 - hf).clamp(0.0, 0.95), diffusion: density, dry: 1.0 - mix, wet: mix * gain * main }
    }
}

impl Reverb {
    pub fn run(&mut self, s: &ReverbShape, buf: &mut [f32], ch: usize, rate: u32) {
        if !sane(ch, rate) {
            return;
        }
        let scale = s.scale.clamp(0.01, MAX_SCALE);
        let len = |n: usize, c: usize, scale: f32| (((n + SPREAD * (c % 2)) as f32 * (scale * rate as f32 / 44100.0)) as usize).max(1);
        if self.key != (ch, rate) || self.combs.len() != ch {
            let line = |n: usize, c: usize| Line { buf: vec![0.0; len(n, c, MAX_SCALE)], len: 1, at: 0, lp: 0.0 };
            self.combs = (0..ch).map(|c| COMBS.iter().map(|&n| line(n, c)).collect()).collect();
            self.alls = (0..ch).map(|c| ALLPASSES.iter().map(|&n| line(n, c)).collect()).collect();
            self.key = (ch, rate);
            self.shape = (u32::MAX, 0);
        }
        if self.shape != (scale.to_bits(), s.decay_s.to_bits()) {
            self.shape = (scale.to_bits(), s.decay_s.to_bits());
            for (c, (cs, als)) in self.combs.iter_mut().zip(self.alls.iter_mut()).enumerate() {
                for (l, &n) in cs.iter_mut().zip(&COMBS).chain(als.iter_mut().zip(&ALLPASSES)) {
                    l.len = len(n, c, scale).min(l.buf.len());
                    l.at %= l.len;
                }
            }
            // a comb of n samples falls by 60 dB over the decay time
            self.fbs = self.combs.iter().map(|cs| cs.iter().map(|l| 10f32.powf(-3.0 * l.len as f32 / (s.decay_s * rate as f32)).min(0.98)).collect()).collect();
        }
        let ap = 0.3 + 0.4 * s.diffusion;
        for fr in buf.chunks_exact_mut(ch) {
            for (c, x) in fr.iter_mut().enumerate() {
                let input = *x * 0.015;
                let mut out = 0.0;
                for (l, &fb) in self.combs[c].iter_mut().zip(&self.fbs[c]) {
                    let y = l.buf[l.at];
                    l.lp = y * (1.0 - s.damp) + l.lp * s.damp;
                    l.buf[l.at] = input + l.lp * fb;
                    l.at += 1;
                    if l.at >= l.len {
                        l.at = 0;
                    }
                    out += y;
                }
                for l in self.alls[c].iter_mut() {
                    let b = l.buf[l.at];
                    let y = b - out;
                    l.buf[l.at] = out + b * ap;
                    l.at += 1;
                    if l.at >= l.len {
                        l.at = 0;
                    }
                    out = y;
                }
                *x = *x * s.dry + out * 3.0 * s.wet;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn params(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }
    fn sine(hz: f32, rate: u32, n: usize, ch: usize) -> Vec<f32> {
        (0..n * ch).map(|i| (2.0 * std::f32::consts::PI * hz * (i / ch) as f32 / rate as f32).sin()).collect()
    }
    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0f32, |m, x| m.max(x.abs()))
    }

    #[test]
    fn eq_unity_is_identity() {
        let mut e = Eq::default();
        let x = sine(440.0, 48000, 4800, 2);
        let mut y = x.clone();
        e.run(&[], &mut y, 2, 48000);
        assert_eq!(x, y);
    }

    /// A band's gain is the amplitude at its centre (after the filter settles)
    #[test]
    fn eq_band_gain_at_centre() {
        for (g, f0) in [(2.0f32, 1000.0f32), (0.5, 800.0), (4.0, 3000.0)] {
            let mut e = Eq::default();
            let p = params(&[f0, g, 1.0, 800.0, 1.0, 1.0, 2000.0, 1.0, 1.0, 10000.0, 1.0, 1.0]);
            let mut y = sine(f0, 48000, 48000, 1);
            e.run(&p, &mut y, 1, 48000);
            let got = peak(&y[24000..]);
            assert!((got - g).abs() < 0.03 * g, "{f0} Hz x{g}: {got}");
        }
    }

    #[test]
    fn echo_taps() {
        let mut e = Echo::default();
        // 10 ms at 1 kHz: 10 samples; half wet, feedback 0.5
        let p = params(&[0.5, 0.5, 10.0]);
        let mut y = vec![0.0; 40];
        y[0] = 1.0;
        e.run(&p, &mut y, 1, 1000);
        assert_eq!(y[0], 0.5);
        assert_eq!(y[10], 0.5);
        assert_eq!(y[20], 0.25);
        assert_eq!(y[30], 0.125);
        assert!(y.iter().enumerate().all(|(i, v)| i % 10 == 0 || *v == 0.0));
    }

    #[test]
    fn echo_max_delay() {
        let mut e = Echo { max_delay_ms: 5.0, ..Default::default() };
        let p = params(&[1.0, 0.0, 100.0]);
        let mut y = vec![0.0; 20];
        y[0] = 1.0;
        e.run(&p, &mut y, 1, 1000);
        // (the delay is held to the 5 ms maximum)
        assert_eq!(y[5], 1.0);
    }

    #[test]
    fn limiter_holds_full_scale() {
        let mut l = Limiter::default();
        let mut y: Vec<f32> = sine(100.0, 48000, 4800, 2).iter().map(|x| x * 4.0).collect();
        l.run(&[], &mut y, 2, 48000);
        assert!(peak(&y) <= 1.0);
        // quiet audio passes unchanged at the default loudness
        let x = sine(100.0, 48000, 480, 2).iter().map(|v| v * 0.25).collect::<Vec<_>>();
        let mut l = Limiter::default();
        let mut y = x.clone();
        l.run(&[], &mut y, 2, 48000);
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn reverb_tail_decays() {
        let mut r = Reverb::default();
        let s = ReverbShape::audio(&params(&[100.0, 0.0, 0.0, 0.0, 0.0, 5000.0, 0.0, 0.0, 0.0, 0.0, 0.5, 100.0, 100.0]));
        let mut y = vec![0.0; 48000 * 2];
        y[0] = 1.0;
        y[1] = 1.0;
        r.run(&s, &mut y, 2, 48000);
        let e = |a: usize, b: usize| y[a..b].iter().map(|v| v * v).sum::<f32>();
        // (the impulse itself is gone: fully wet)
        assert!(e(4000, 12000) > 0.0);
        assert!(e(4000, 12000) > 100.0 * e(80000, 88000), "{} {}", e(4000, 12000), e(80000, 88000));
        // and the channels differ (the stereo spread)
        assert!(y.chunks_exact(2).any(|f| (f[0] - f[1]).abs() > 1e-6));
    }

    #[test]
    fn reverb_dry_is_identity() {
        let mut r = Reverb::default();
        let s = ReverbShape::audio(&params(&[0.0]));
        let x = sine(300.0, 44100, 4410, 2);
        let mut y = x.clone();
        r.run(&s, &mut y, 2, 44100);
        assert_eq!(x, y);
    }

    /// The narrowest, lowest band (20 Hz, 0.1 octave, +18 dB): its centre gain holds, and an
    /// octave away it is near unity
    #[test]
    fn eq_low_narrow_band() {
        let p = params(&[20.0, 7.94, 0.1, 800.0, 1.0, 1.0, 2000.0, 1.0, 1.0, 10000.0, 1.0, 1.0]);
        for (hz, want) in [(20.0, 7.94f32), (40.0, 1.0)] {
            let mut y = sine(hz, 48000, 480000, 1);
            Eq::default().run(&p, &mut y, 1, 48000);
            let peak = y[432000..].iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!((peak / want - 1.0).abs() < 0.02, "{hz} Hz: {peak}");
        }
    }

    /// Three bands at +18 dB stacked (the case the old bound of 64 refused): about x500 at most
    #[test]
    fn eq_stacked_gains() {
        let p = params(&[6619.6245, 334.06134, 408.64935, 7706.5713, 7584.405, 6331.9116, 0.0, 0.0, 0.0, 4396.2837, 3334.7468]);
        let mut y = sine(6619.6245, 48000, 4800, 1);
        Eq::default().run(&p, &mut y, 1, 48000);
        let peak = y[2400..].iter().fold(0f32, |m, v| m.max(v.abs()));
        // band 0 alone gives 7.94 at its centre; the others add their skirts there
        assert!(peak.is_finite() && peak > 7.94 && peak < 7.94f32.powi(3), "{peak}");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Any parameters and any signal in full scale: the output stays finite and bounded
        /// (the limiter's within full scale)
        #[test]
        fn bounded(ps in proptest::collection::vec(-1e4f32..1e4, 0..14), xs in proptest::collection::vec(-1f32..1.0, 0..2000), ch in 1usize..3) {
            let p = params(&ps);
            let n = xs.len() / ch * ch;
            let x = &xs[..n];
            // the EQ is linear: |y| <= ||h||_1 max|x| (four +18 dB bands may stack to +72 dB, so no
            // fixed bound); its impulse response h decays (the filter is stable)
            // (20 Hz, 0.1 octave, +18 dB has a time constant of ~31000 samples: 10 s is ~15 of them)
            let mut h = vec![0f32; 480000];
            h[0] = 1.0;
            Eq::default().run(&p, &mut h, 1, 48000);
            prop_assert!(h.iter().all(|v| v.is_finite()));
            let tail = h[432000..].iter().fold(0f32, |m, v| m.max(v.abs()));
            prop_assert!(tail < 1e-4, "impulse response still {} after 9 s", tail);
            let l1: f64 = h.iter().map(|v| v.abs() as f64).sum();
            let xmax = x.iter().fold(0f32, |m, v| m.max(v.abs())) as f64;
            let mut y = x.to_vec();
            Eq::default().run(&p, &mut y, ch, 48000);
            prop_assert!(y.iter().all(|v| v.is_finite() && (v.abs() as f64) <= l1 * xmax * 1.001 + 1e-6));
            let mut y = x.to_vec();
            Echo::default().run(&p, &mut y, ch, 48000);
            prop_assert!(y.iter().all(|v| v.is_finite() && v.abs() < 1e3));
            let mut y = x.to_vec();
            Limiter::default().run(&p, &mut y, ch, 48000);
            prop_assert!(y.iter().all(|v| v.abs() <= 1.0));
            let mut y = x.to_vec();
            Reverb::default().run(&ReverbShape::audio(&p), &mut y, ch, 48000);
            prop_assert!(y.iter().all(|v| v.is_finite() && v.abs() < 1e3));
            let mut y = x.to_vec();
            Reverb::default().run(&ReverbShape::fx(&p), &mut y, ch, 48000);
            prop_assert!(y.iter().all(|v| v.is_finite() && v.abs() < 1e3));
        }

        /// Processing in pieces is processing at once (the state carries over)
        #[test]
        fn split_passes(xs in proptest::collection::vec(-1f32..1.0, 2..1200), cut in 0usize..600) {
            let n = xs.len() / 2 * 2;
            let cut = (cut * 2).min(n);
            let p = params(&[0.7, 0.4, 3.0]);
            let mut a = xs[..n].to_vec();
            Echo::default().run(&p, &mut a, 2, 1000);
            let mut e = Echo::default();
            let mut b = xs[..n].to_vec();
            let (l, r) = b.split_at_mut(cut);
            e.run(&p, l, 2, 1000);
            e.run(&p, r, 2, 1000);
            prop_assert_eq!(a, b);
        }

        /// Any channel count and rate a voice may claim (0 channels, 0 Hz, 4 GHz, 1000
        /// channels): no panic, no division by zero, nothing allocated past XAudio2's limits,
        /// and a format outside them passes through unchanged
        #[test]
        fn any_format_is_safe(ps in proptest::collection::vec(any::<f32>(), 0..14), n in 0usize..300, ch in prop_oneof![Just(0usize), 1usize..9, Just(64), Just(65), Just(1000)],
                              rate in prop_oneof![Just(0u32), Just(1), Just(999), 1000u32..200_001, Just(200_001), any::<u32>()], max_delay in any::<f32>()) {
            let p = params(&ps);
            let x: Vec<f32> = (0..n).map(|i| ((i * 37 % 200) as f32 / 100.0) - 1.0).collect();
            let ok = sane(ch, rate);
            let mut ys = vec![x.clone(); 5];
            Eq::default().run(&p, &mut ys[0], ch, rate);
            Limiter::default().run(&p, &mut ys[1], ch, rate);
            let mut e = Echo::with_max(max_delay);
            e.run(&p, &mut ys[2], ch, rate);
            prop_assert!(e.line.len() <= (2 * *RATES.end() as usize + 1) * MAX_CHANNELS);
            let mut r = Reverb::default();
            r.run(&ReverbShape::fx(&p), &mut ys[3], ch, rate);
            r.run(&ReverbShape::audio(&p), &mut ys[4], ch, rate);
            let samples: usize = r.combs.iter().chain(&r.alls).flatten().map(|l| l.buf.len()).sum();
            prop_assert!(samples <= MAX_CHANNELS * 12 * 1700 * 5);
            for y in &ys {
                if !ok {
                    prop_assert_eq!(y, &x);
                }
                prop_assert!(y.iter().all(|v| v.is_finite()));
            }
        }

        /// With a room size that stays, the reverb gives exactly what it gave before its lines
        /// were made for the largest room (the implementation before, kept here as the model);
        /// a room size that changes keeps the tail ringing (it used to start from silence,
        /// a click) and allocates nothing
        #[test]
        fn reverb_as_before_and_room_changes_keep_the_tail(xs in proptest::collection::vec(-1f32..1.0, 2..2000), room in 0.0001f32..1.0, room2 in 0.0001f32..1.0, rate in prop_oneof![Just(44100u32), Just(48000), Just(22050), 8000u32..96000]) {
            let n = xs.len() / 2 * 2;
            let shape = ReverbShape::fx(&params(&[0.9, room]));
            let mut a = xs[..n].to_vec();
            Reverb::default().run(&shape, &mut a, 2, rate);
            let mut b = xs[..n].to_vec();
            old::Reverb::default().run(&shape, &mut b, 2, rate);
            prop_assert_eq!(&a, &b);
            let mut r = Reverb::default();
            let mut imp = vec![0f32; 2 * 4000];
            imp[0] = 1.0;
            imp[1] = 1.0;
            r.run(&shape, &mut imp, 2, rate);
            let bufs: Vec<*const f32> = r.combs.iter().flatten().map(|l| l.buf.as_ptr()).collect();
            let mut z = vec![0f32; 2 * 2000];
            r.run(&ReverbShape::fx(&params(&[0.9, room2])), &mut z, 2, rate);
            prop_assert!(z.iter().any(|v| v.abs() > 1e-7), "the tail went silent at the room change");
            prop_assert_eq!(bufs, r.combs.iter().flatten().map(|l| l.buf.as_ptr()).collect::<Vec<_>>());
        }
    }

    /// The reverberator as it was (lines remade per room size, feedbacks per pass)
    mod old {
        use super::super::{ReverbShape, ALLPASSES, COMBS, SPREAD};
        #[derive(Clone, Default)]
        struct Line {
            buf: Vec<f32>,
            at: usize,
            lp: f32,
        }
        #[derive(Default)]
        pub struct Reverb {
            combs: Vec<Vec<Line>>,
            alls: Vec<Vec<Line>>,
            key: (usize, u32, u32),
        }
        impl Reverb {
            pub fn run(&mut self, s: &ReverbShape, buf: &mut [f32], ch: usize, rate: u32) {
                let sc = s.scale * rate as f32 / 44100.0;
                let key = (ch, rate, (s.scale * 1000.0) as u32);
                if self.key != key || self.combs.len() != ch {
                    let len = |n: usize, c: usize| (((n + SPREAD * (c % 2)) as f32 * sc) as usize).max(1);
                    self.combs = (0..ch).map(|c| COMBS.iter().map(|&n| Line { buf: vec![0.0; len(n, c)], at: 0, lp: 0.0 }).collect()).collect();
                    self.alls = (0..ch).map(|c| ALLPASSES.iter().map(|&n| Line { buf: vec![0.0; len(n, c)], at: 0, lp: 0.0 }).collect()).collect();
                    self.key = key;
                }
                let ap = 0.3 + 0.4 * s.diffusion;
                let fbs: Vec<Vec<f32>> = self.combs.iter().map(|cs| cs.iter().map(|l| 10f32.powf(-3.0 * l.buf.len() as f32 / (s.decay_s * rate as f32)).min(0.98)).collect()).collect();
                for fr in buf.chunks_exact_mut(ch) {
                    for (c, x) in fr.iter_mut().enumerate() {
                        let input = *x * 0.015;
                        let mut out = 0.0;
                        for (l, &fb) in self.combs[c].iter_mut().zip(&fbs[c]) {
                            let y = l.buf[l.at];
                            l.lp = y * (1.0 - s.damp) + l.lp * s.damp;
                            l.buf[l.at] = input + l.lp * fb;
                            l.at = (l.at + 1) % l.buf.len();
                            out += y;
                        }
                        for l in self.alls[c].iter_mut() {
                            let b = l.buf[l.at];
                            let y = b - out;
                            l.buf[l.at] = out + b * ap;
                            l.at = (l.at + 1) % l.buf.len();
                            out = y;
                        }
                        *x = *x * s.dry + out * 3.0 * s.wet;
                    }
                }
            }
        }
    }
}
