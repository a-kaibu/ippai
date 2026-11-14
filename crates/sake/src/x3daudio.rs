//! X3DAudio (x3daudio1_7, and the copy in xaudio2_8 / xaudio2_9): X3DAudioInitialize keeps the
//! speaker layout and the speed of sound in the handle; X3DAudioCalculate turns a listener and an
//! emitter into an output matrix (constant-power panning between the two speakers around the
//! emitter's direction, the volume curve on the distance), a Doppler factor, low-pass and reverb
//! levels from their curves, and the emitter angle and distance.
//!
//! The structures are packed (x3daudio.h `#pragma pack(1)`): pointers are 4 bytes (x86) or 8
//! (x64) with no padding. Directions follow Direct3D's left-handed coordinates; azimuths are
//! clockwise from the front, in [0, 2π) (X3DAUDIO_2PI is the LFE channel).

#![allow(non_snake_case)]

use std::f32::consts::PI;

use crate::api::{implement, Ctx};
use crate::mem::{self, ps, r32, rp, w32};

pub type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn scale(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn len(a: V3) -> f32 {
    dot(a, a).sqrt()
}

pub const TWO_PI: f32 = 2.0 * PI;

/// Speaker bits (SPEAKER_*) and their azimuths, in channel order.
const SPEAKERS: [(u32, f32); 11] = [
    (0x1, 7.0 * PI / 4.0), // FRONT_LEFT
    (0x2, PI / 4.0),       // FRONT_RIGHT
    (0x4, 0.0),            // FRONT_CENTER
    (0x8, TWO_PI),         // LOW_FREQUENCY
    (0x10, 5.0 * PI / 4.0), // BACK_LEFT
    (0x20, 3.0 * PI / 4.0), // BACK_RIGHT
    (0x40, 15.0 * PI / 8.0), // FRONT_LEFT_OF_CENTER
    (0x80, PI / 8.0),      // FRONT_RIGHT_OF_CENTER
    (0x100, PI),           // BACK_CENTER
    (0x200, 3.0 * PI / 2.0), // SIDE_LEFT
    (0x400, PI / 2.0),     // SIDE_RIGHT
];

/// The output channels of a speaker mask: (azimuth, is LFE), in channel order.
pub fn layout(mask: u32) -> Vec<(f32, bool)> {
    let mut v: Vec<(f32, bool)> = SPEAKERS.iter().filter(|s| mask & s.0 != 0).map(|s| (s.1, s.0 == 0x8)).collect();
    // (bits past SPEAKER_SIDE_RIGHT: the top speakers, taken as centre)
    for b in 11..18 {
        if mask & (1 << b) != 0 {
            v.push((0.0, false));
        }
    }
    v
}

pub const MATRIX: u32 = 0x1;
pub const DELAY: u32 = 0x2;
pub const LPF_DIRECT: u32 = 0x4;
pub const LPF_REVERB: u32 = 0x8;
pub const REVERB: u32 = 0x10;
pub const DOPPLER: u32 = 0x20;
pub const EMITTER_ANGLE: u32 = 0x40;
pub const ZEROCENTER: u32 = 0x10000;
pub const REDIRECT_TO_LFE: u32 = 0x20000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cone {
    pub inner_angle: f32,
    pub outer_angle: f32,
    pub inner_volume: f32,
    pub outer_volume: f32,
    pub inner_lpf: f32,
    pub outer_lpf: f32,
    pub inner_reverb: f32,
    pub outer_reverb: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Listener {
    pub front: V3,
    pub top: V3,
    pub pos: V3,
    pub vel: V3,
    pub cone: Option<Cone>,
}

#[derive(Clone, Debug, Default)]
pub struct Emitter {
    pub cone: Option<Cone>,
    pub front: V3,
    pub top: V3,
    pub pos: V3,
    pub vel: V3,
    pub inner_radius: f32,
    pub inner_radius_angle: f32,
    pub channels: u32,
    pub channel_radius: f32,
    pub azimuths: Vec<f32>,
    pub volume_curve: Option<Vec<(f32, f32)>>,
    pub lfe_curve: Option<Vec<(f32, f32)>>,
    pub lpf_direct_curve: Option<Vec<(f32, f32)>>,
    pub lpf_reverb_curve: Option<Vec<(f32, f32)>>,
    pub reverb_curve: Option<Vec<(f32, f32)>>,
    pub curve_scaler: f32,
    pub doppler_scaler: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dsp {
    /// [dst][src], dst * src_channels + src (what SetOutputMatrix takes)
    pub matrix: Vec<f32>,
    pub delays: Vec<f32>,
    pub lpf_direct: f32,
    pub lpf_reverb: f32,
    pub reverb: f32,
    pub doppler: f32,
    pub angle: f32,
    pub distance: f32,
    pub emitter_velocity: f32,
    pub listener_velocity: f32,
}

/// A piecewise-linear curve of normalized distance (points by increasing distance); past
/// the last point, its value.
pub fn curve(points: &[(f32, f32)], d: f32) -> f32 {
    let Some(&first) = points.first() else { return 1.0 };
    if d <= first.0 {
        return first.1;
    }
    for w in points.windows(2) {
        let ((d0, v0), (d1, v1)) = (w[0], w[1]);
        if d <= d1 {
            return if d1 > d0 { v0 + (v1 - v0) * (d - d0) / (d1 - d0) } else { v1 };
        }
    }
    points[points.len() - 1].1
}

/// The cone's (volume, lpf, reverb) at `angle` (radians off its axis): inner values inside half
/// the inner angle, outer past half the outer angle, linear in between.
fn cone_at(c: &Cone, angle: f32) -> (f32, f32, f32) {
    let (hi, ho) = (c.inner_angle / 2.0, c.outer_angle / 2.0);
    let t = if angle <= hi {
        0.0
    } else if angle >= ho || ho <= hi {
        1.0
    } else {
        (angle - hi) / (ho - hi)
    };
    let l = |a: f32, b: f32| a + (b - a) * t;
    (l(c.inner_volume, c.outer_volume), l(c.inner_lpf, c.outer_lpf), l(c.inner_reverb, c.outer_reverb))
}

fn angle_between(a: V3, b: V3) -> f32 {
    let n = len(a) * len(b);
    if n <= f32::MIN_POSITIVE {
        return 0.0;
    }
    (dot(a, b) / n).clamp(-1.0, 1.0).acos()
}

/// Azimuth of `v` seen by a listener facing `front` with `top` up, in [0, 2π)
fn azimuth(v: V3, front: V3, top: V3) -> f32 {
    let right = cross(top, front);
    let a = dot(v, right).atan2(dot(v, front));
    if a < 0.0 {
        a + TWO_PI
    } else {
        a
    }
}

/// Constant-power gains of a source at azimuth `az` over the speakers `spk` (azimuths; one gain
/// each): the two speakers around it share it by cos/sin of its place between them.
pub fn pan(az: f32, spk: &[f32]) -> Vec<f32> {
    let mut g = vec![0.0; spk.len()];
    match spk.len() {
        0 => return g,
        1 => {
            g[0] = 1.0;
            return g;
        }
        _ => {}
    }
    let mut order: Vec<usize> = (0..spk.len()).collect();
    order.sort_by(|&a, &b| spk[a].total_cmp(&spk[b]));
    // speakers only in front (stereo, 3.0...: a gap of more than half the circle): a source
    // behind is heard where its mirror in front is, and one outside them on the outer speaker
    let signed = |a: f32| if a > PI { a - TWO_PI } else { a };
    let gap = (0..order.len()).map(|k| {
        let (a0, a1) = (spk[order[k]], spk[order[(k + 1) % order.len()]]);
        if a1 > a0 { a1 - a0 } else { a1 + TWO_PI - a0 }
    });
    if gap.fold(0.0f32, f32::max) > PI {
        let mut s = signed(az);
        if s > PI / 2.0 {
            s = PI - s;
        } else if s < -PI / 2.0 {
            s = -PI - s;
        }
        let mut front: Vec<usize> = (0..spk.len()).collect();
        front.sort_by(|&a, &b| signed(spk[a]).total_cmp(&signed(spk[b])));
        let (lo, hi) = (front[0], front[front.len() - 1]);
        if s <= signed(spk[lo]) {
            g[lo] = 1.0;
            return g;
        }
        if s >= signed(spk[hi]) {
            g[hi] = 1.0;
            return g;
        }
        for w in front.windows(2) {
            let (a0, a1) = (signed(spk[w[0]]), signed(spk[w[1]]));
            if s >= a0 && s <= a1 {
                let t = if a1 > a0 { (s - a0) / (a1 - a0) } else { 0.0 };
                g[w[0]] = (t * PI / 2.0).cos();
                g[w[1]] += (t * PI / 2.0).sin();
                return g;
            }
        }
    }
    for k in 0..order.len() {
        let (i, j) = (order[k], order[(k + 1) % order.len()]);
        let (a0, mut a1) = (spk[i], spk[j]);
        if a1 <= a0 {
            a1 += TWO_PI;
        }
        let mut x = az;
        if x < a0 {
            x += TWO_PI;
        }
        if x >= a0 && x <= a1 {
            let t = if a1 > a0 { (x - a0) / (a1 - a0) } else { 0.0 };
            g[i] = (t * PI / 2.0).cos();
            g[j] += (t * PI / 2.0).sin();
            return g;
        }
    }
    g[order[0]] = 1.0;
    g
}

/// XAUDIO2_MAX_AUDIO_CHANNELS: the most channels an emitter or an output has.
pub const MAX_CHANNELS: u32 = 64;

/// The emitter's ChannelCount and the DSP settings' DstChannelCount as X3DAudioCalculate uses
/// them: 1 to 64 each (review 2026-11-14: a garbage count, such as an uninitialised 0xCCCCCCCC,
/// sized the matrix and the write-back loops: an allocation abort or zeros written across memory).
pub fn counts(channels: u32, dst: u32) -> (usize, usize) {
    (channels.clamp(1, MAX_CHANNELS) as usize, dst.clamp(1, MAX_CHANNELS) as usize)
}

/// X3DAudioCalculate
pub fn calculate(mask: u32, speed: f32, l: &Listener, e: &Emitter, flags: u32, dst: usize) -> Dsp {
    let (src, dst) = counts(e.channels, dst.min(MAX_CHANNELS as usize) as u32);
    let scaler = if e.curve_scaler > 0.0 { e.curve_scaler } else { 1.0 };
    let to_e = sub(e.pos, l.pos);
    let dist = len(to_e);
    let nd = dist / scaler;
    let default_volume = |d: f32| 1.0 / d.max(1.0);
    let vol_at = |d: f32| e.volume_curve.as_deref().map_or_else(|| default_volume(d), |c| curve(c, d));
    let lfe_at = |d: f32| e.lfe_curve.as_deref().map_or_else(|| vol_at(d), |c| curve(c, d));
    let mut out = Dsp { distance: dist, doppler: 1.0, lpf_direct: 1.0, lpf_reverb: 1.0, reverb: 0.0, ..Default::default() };

    // the cones: the emitter's (seen from the listener, single-channel emitters only) and the listener's
    let (mut cv, mut cl, mut cr) = (1.0, 1.0, 1.0);
    let e_to_l = sub(l.pos, e.pos);
    if let (Some(c), 1) = (e.cone, src) {
        let (v, f, r) = cone_at(&c, angle_between(e.front, e_to_l));
        cv *= v;
        cl *= f;
        cr *= r;
    }
    if let Some(c) = l.cone {
        let (v, f, r) = cone_at(&c, angle_between(l.front, to_e));
        cv *= v;
        cl *= f;
        cr *= r;
    }

    if flags & MATRIX != 0 {
        let outs = layout(mask);
        let dst = dst.min(outs.len()).max(1);
        let outs = &outs[..dst.min(outs.len())];
        let lfe = outs.iter().position(|o| o.1);
        let zero_center = flags & ZEROCENTER != 0;
        let speakers: Vec<usize> = (0..outs.len()).filter(|&i| !outs[i].1 && !(zero_center && outs[i].0 == 0.0 && outs.len() > 2)).collect();
        let azs: Vec<f32> = speakers.iter().map(|&i| outs[i].0).collect();
        let mut m = vec![0.0f32; dst * src];
        let eright = cross(e.top, e.front);
        for ch in 0..src {
            let caz = e.azimuths.get(ch).copied().unwrap_or(0.0);
            if src > 1 && caz >= TWO_PI {
                // the LFE channel: the LFE speaker only
                if let Some(k) = lfe {
                    m[k * src + ch] = lfe_at(nd);
                }
                continue;
            }
            // the channel's position: around the emitter at its radius
            let p = if src > 1 {
                add(e.pos, add(scale(e.front, caz.cos() * e.channel_radius), scale(eright, caz.sin() * e.channel_radius)))
            } else {
                e.pos
            };
            let v = sub(p, l.pos);
            let d = len(v);
            let g = vol_at(d / scaler) * cv;
            let mut gains = if d > f32::MIN_POSITIVE { pan(azimuth(v, l.front, l.top), &azs) } else { vec![1.0 / (azs.len().max(1) as f32).sqrt(); azs.len()] };
            // inside the inner radius the sound spreads to every speaker
            if e.inner_radius > 0.0 && d < e.inner_radius && !azs.is_empty() {
                let t = 1.0 - d / e.inner_radius;
                let even = 1.0 / (azs.len() as f32).sqrt();
                for x in gains.iter_mut() {
                    *x = ((1.0 - t) * *x * *x + t * even * even).sqrt();
                }
            }
            for (k, &i) in speakers.iter().enumerate() {
                m[i * src + ch] = gains[k] * g;
            }
            if let (Some(k), true) = (lfe, flags & REDIRECT_TO_LFE != 0) {
                m[k * src + ch] += lfe_at(nd) / src as f32;
            }
        }
        out.matrix = m;
    }
    if flags & DELAY != 0 {
        out.delays = vec![0.0; dst];
    }
    if flags & LPF_DIRECT != 0 {
        out.lpf_direct = e.lpf_direct_curve.as_deref().map_or_else(|| curve(&[(0.0, 1.0), (1.0, 0.75)], nd), |c| curve(c, nd)) * cl;
    }
    if flags & LPF_REVERB != 0 {
        out.lpf_reverb = e.lpf_reverb_curve.as_deref().map_or_else(|| curve(&[(0.0, 0.75), (1.0, 0.75)], nd), |c| curve(c, nd)) * cl;
    }
    if flags & REVERB != 0 {
        out.reverb = e.reverb_curve.as_deref().map_or_else(|| curve(&[(0.0, 1.0), (1.0, 0.0)], nd), |c| curve(c, nd)) * cr;
    }
    if flags & DOPPLER != 0 {
        // velocities along the emitter -> listener direction
        let u = if dist > f32::MIN_POSITIVE { scale(e_to_l, 1.0 / dist) } else { [0.0; 3] };
        let (ev, lv) = (dot(e.vel, u), dot(l.vel, u));
        out.emitter_velocity = ev;
        out.listener_velocity = lv;
        let ds = e.doppler_scaler.max(0.0);
        let c = speed.max(f32::MIN_POSITIVE);
        // (clamped so neither moves at the speed of sound or beyond)
        let es = (ev * ds).min(c * 0.5);
        let ls = (lv * ds).min(c * 0.5);
        out.doppler = ((c - ls) / (c - es)).clamp(0.0, 4.0);
    }
    if flags & EMITTER_ANGLE != 0 {
        out.angle = angle_between(e.front, e_to_l);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// the guest side

/// X3DAudioInitialize(SpeakerChannelMask, SpeedOfSound, X3DAUDIO_HANDLE): the handle (20 bytes)
/// keeps the mask, the speed and a mark.
fn X3DAudioInitialize(c: &mut Ctx) -> u32 {
    let mask = c.a(0);
    let speed = f32::from_bits(if mem::x64() { c.xmm(1) as u32 } else { c.a(1) });
    let h = c.a(2);
    if h == 0 || !(speed >= f32::MIN_POSITIVE) || !speed.is_finite() {
        return 0x8878_0001; // X3DAUDIO_E_INVALID_CALL
    }
    w32(h, mask);
    w32(h + 4, speed.to_bits());
    w32(h + 8, layout(mask).len() as u32);
    w32(h + 12, 0x3344_5833); // "3XD3"
    w32(h + 16, 0);
    0
}

struct Rd {
    p: u32,
}
impl Rd {
    fn f(&mut self) -> f32 {
        let v = f32::from_bits(r32(self.p));
        self.p += 4;
        v
    }
    fn u(&mut self) -> u32 {
        let v = r32(self.p);
        self.p += 4;
        v
    }
    fn v3(&mut self) -> V3 {
        [self.f(), self.f(), self.f()]
    }
    fn ptr(&mut self) -> u32 {
        let v = rp(self.p);
        self.p += ps();
        v
    }
}

fn read_cone(p: u32) -> Option<Cone> {
    if p == 0 {
        return None;
    }
    let mut r = Rd { p };
    Some(Cone {
        inner_angle: r.f(),
        outer_angle: r.f(),
        inner_volume: r.f(),
        outer_volume: r.f(),
        inner_lpf: r.f(),
        outer_lpf: r.f(),
        inner_reverb: r.f(),
        outer_reverb: r.f(),
    })
}

/// X3DAUDIO_DISTANCE_CURVE { pPoints, PointCount } of { Distance, DSPSetting }
fn read_curve(p: u32) -> Option<Vec<(f32, f32)>> {
    if p == 0 {
        return None;
    }
    let pts = rp(p);
    let n = r32(p + ps()).min(1024);
    if pts == 0 || n == 0 {
        return None;
    }
    Some((0..n).map(|i| (f32::from_bits(r32(pts + 8 * i)), f32::from_bits(r32(pts + 8 * i + 4)))).collect())
}

/// X3DAudioCalculate(handle, pListener, pEmitter, Flags, pDSPSettings)
fn X3DAudioCalculate(c: &mut Ctx) -> u32 {
    let (h, lp, ep, flags, dp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if h == 0 || lp == 0 || ep == 0 || dp == 0 {
        return 0;
    }
    let (mask, speed) = (r32(h), f32::from_bits(r32(h + 4)));
    let mut r = Rd { p: lp };
    let l = Listener { front: r.v3(), top: r.v3(), pos: r.v3(), vel: r.v3(), cone: read_cone(r.ptr()) };
    let mut r = Rd { p: ep };
    let cone = read_cone(r.ptr());
    let (front, top, pos, vel) = (r.v3(), r.v3(), r.v3(), r.v3());
    let (inner_radius, inner_radius_angle) = (r.f(), r.f());
    let channels = r.u();
    let channel_radius = r.f();
    let paz = r.ptr();
    let curves: Vec<u32> = (0..5).map(|_| r.ptr()).collect();
    let (curve_scaler, doppler_scaler) = (r.f(), r.f());
    let azimuths = if paz != 0 { (0..channels.min(MAX_CHANNELS)).map(|i| f32::from_bits(r32(paz + 4 * i))).collect() } else { Vec::new() };
    let e = Emitter {
        cone,
        front,
        top,
        pos,
        vel,
        inner_radius,
        inner_radius_angle,
        channels,
        channel_radius,
        azimuths,
        volume_curve: read_curve(curves[0]),
        lfe_curve: read_curve(curves[1]),
        lpf_direct_curve: read_curve(curves[2]),
        lpf_reverb_curve: read_curve(curves[3]),
        reverb_curve: read_curve(curves[4]),
        curve_scaler,
        doppler_scaler,
    };
    // X3DAUDIO_DSP_SETTINGS { pMatrixCoefficients, pDelayTimes, SrcChannelCount, DstChannelCount, ... }
    let (pm, pd) = (rp(dp), rp(dp + ps()));
    let f = dp + 2 * ps();
    let (src, dst) = counts(channels, r32(f + 4));
    let d = calculate(mask, speed, &l, &e, flags, dst);
    if flags & MATRIX != 0 && pm != 0 {
        for i in 0..dst * src {
            w32(pm + 4 * i as u32, d.matrix.get(i).copied().unwrap_or(0.0).to_bits());
        }
    }
    if flags & DELAY != 0 && pd != 0 {
        for i in 0..dst {
            w32(pd + 4 * i as u32, d.delays.get(i).copied().unwrap_or(0.0).to_bits());
        }
    }
    let put = |o: u32, v: f32| w32(f + o, v.to_bits());
    if flags & LPF_DIRECT != 0 {
        put(8, d.lpf_direct);
    }
    if flags & LPF_REVERB != 0 {
        put(12, d.lpf_reverb);
    }
    if flags & REVERB != 0 {
        put(16, d.reverb);
    }
    if flags & DOPPLER != 0 {
        put(20, d.doppler);
        put(32, d.emitter_velocity);
        put(36, d.listener_velocity);
    }
    if flags & EMITTER_ANGLE != 0 {
        put(24, d.angle);
    }
    put(28, d.distance);
    0
}

pub fn init() {
    for dll in ["x3daudio1_7", "xaudio2_8"] {
        implement(dll, "X3DAudioInitialize", X3DAudioInitialize);
        implement(dll, "X3DAudioCalculate", X3DAudioCalculate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener() -> Listener {
        Listener { front: [0.0, 0.0, 1.0], top: [0.0, 1.0, 0.0], ..Default::default() }
    }
    fn emitter(pos: V3) -> Emitter {
        Emitter { front: [0.0, 0.0, 1.0], top: [0.0, 1.0, 0.0], pos, channels: 1, curve_scaler: 1.0, doppler_scaler: 1.0, ..Default::default() }
    }
    const STEREO: u32 = 0x3;
    const ALL: u32 = MATRIX | LPF_DIRECT | LPF_REVERB | REVERB | DOPPLER | EMITTER_ANGLE;

    /// a tiny xorshift, for the property tests
    struct Rng(u64);
    impl Rng {
        fn f(&mut self, lo: f32, hi: f32) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            lo + (hi - lo) * ((self.0 >> 40) as f32 / (1u64 << 24) as f32)
        }
    }

    /// review 2026-11-14: garbage channel counts (uninitialised 0xCCCCCCCC structures) sized the
    /// matrix, the delays and the write-back loops
    #[test]
    fn garbage_counts_stay_bounded() {
        let mut e = emitter([1.0, 0.0, 2.0]);
        for (ch, dst) in [(0xcccc_cccc, 0xcccc_cccc), (0, 0), (u32::MAX, 2), (2, u32::MAX), (65, 65)] {
            e.channels = ch;
            let (src, d) = counts(ch, dst);
            assert!((1..=64).contains(&src) && (1..=64).contains(&d));
            let out = calculate(0x3f, 343.5, &listener(), &e, MATRIX | DELAY, dst as usize);
            assert!(out.matrix.len() <= 64 * 64 && out.delays.len() <= 64, "{ch:#x} {dst:#x}");
        }
        // sane counts are kept as they are
        assert_eq!(counts(2, 6), (2, 6));
        assert_eq!(counts(64, 64), (64, 64));
    }

    proptest::proptest! {
        #[test]
        fn counts_clamp(ch in proptest::prelude::any::<u32>(), dst in proptest::prelude::any::<u32>(), mask in proptest::prelude::any::<u32>(), flags in 0u32..0x80) {
            let (s, d) = counts(ch, dst);
            proptest::prop_assert_eq!(s, ch.clamp(1, 64) as usize);
            proptest::prop_assert_eq!(d, dst.clamp(1, 64) as usize);
            let mut e = emitter([0.5, 0.25, 1.0]);
            e.channels = ch;
            let out = calculate(mask, 343.5, &listener(), &e, flags, dst as usize);
            proptest::prop_assert!(out.matrix.len() <= s * d && out.delays.len() <= d);
        }
    }

    #[test]
    fn right_is_right() {
        let d = calculate(STEREO, 343.5, &listener(), &emitter([5.0, 0.0, 0.0]), MATRIX, 2);
        assert!((d.matrix[1] - 0.2).abs() < 1e-6 && d.matrix[0] == 0.0, "{:?}", d.matrix);
        let d = calculate(STEREO, 343.5, &listener(), &emitter([-5.0, 0.0, 0.0]), MATRIX, 2);
        assert!((d.matrix[0] - 0.2).abs() < 1e-6 && d.matrix[1] == 0.0, "{:?}", d.matrix);
    }

    #[test]
    fn ahead_is_centered_and_at_distance_one_full() {
        let d = calculate(STEREO, 343.5, &listener(), &emitter([0.0, 0.0, 1.0]), MATRIX, 2);
        assert!((d.matrix[0] - d.matrix[1]).abs() < 1e-6);
        assert!((d.matrix[0] * d.matrix[0] + d.matrix[1] * d.matrix[1] - 1.0).abs() < 1e-5);
        assert_eq!(d.distance, 1.0);
    }

    #[test]
    fn default_curve_is_inverse_distance() {
        for k in [2.0f32, 4.0, 10.0] {
            let d = calculate(STEREO, 343.5, &listener(), &emitter([0.0, 0.0, k]), MATRIX, 2);
            let p = (d.matrix[0].powi(2) + d.matrix[1].powi(2)).sqrt();
            assert!((p - 1.0 / k).abs() < 1e-5, "{k}: {p}");
        }
    }

    #[test]
    fn curve_points() {
        let c = [(0.0, 1.0), (0.5, 0.5), (1.0, 0.0)];
        assert_eq!(curve(&c, -1.0), 1.0);
        assert_eq!(curve(&c, 0.25), 0.75);
        assert_eq!(curve(&c, 0.75), 0.25);
        assert_eq!(curve(&c, 3.0), 0.0);
    }

    #[test]
    fn stationary_doppler_is_one() {
        let d = calculate(STEREO, 343.5, &listener(), &emitter([3.0, 1.0, 2.0]), DOPPLER, 2);
        assert_eq!(d.doppler, 1.0);
    }

    #[test]
    fn approaching_raises_receding_lowers_pitch() {
        let mut e = emitter([0.0, 0.0, 10.0]);
        e.vel = [0.0, 0.0, -30.0]; // towards the listener
        let up = calculate(STEREO, 343.5, &listener(), &e, DOPPLER, 2).doppler;
        e.vel = [0.0, 0.0, 30.0];
        let down = calculate(STEREO, 343.5, &listener(), &e, DOPPLER, 2).doppler;
        assert!(up > 1.0 && down < 1.0, "{up} {down}");
        assert!((up - 343.5 / (343.5 - 30.0)).abs() < 1e-4);
    }

    #[test]
    fn cone_attenuates_behind() {
        let mut e = emitter([0.0, 0.0, 2.0]);
        e.front = [0.0, 0.0, 1.0]; // facing away from the listener
        e.cone = Some(Cone { inner_angle: PI / 2.0, outer_angle: PI, inner_volume: 1.0, outer_volume: 0.25, inner_lpf: 1.0, outer_lpf: 0.5, inner_reverb: 1.0, outer_reverb: 1.0 });
        let d = calculate(STEREO, 343.5, &listener(), &e, MATRIX | LPF_DIRECT, 2);
        let p = (d.matrix[0].powi(2) + d.matrix[1].powi(2)).sqrt();
        assert!((p - 0.25 * 0.5).abs() < 1e-5, "{p}");
        assert!((d.lpf_direct - 0.5 * curve(&[(0.0, 1.0), (1.0, 0.75)], 2.0)).abs() < 1e-6);
    }

    #[test]
    fn multichannel_lfe_goes_to_lfe() {
        let mut e = emitter([0.0, 0.0, 1.0]);
        e.channels = 2;
        e.azimuths = vec![0.0, TWO_PI];
        e.channel_radius = 0.0;
        let d = calculate(0x3f, 343.5, &listener(), &e, MATRIX, 6); // 5.1
        // dst 3 = LFE gets src 1 only
        assert!(d.matrix[3 * 2 + 1] > 0.99);
        assert_eq!(d.matrix[3 * 2], 0.0);
        for dst in [0, 1, 2, 4, 5] {
            assert_eq!(d.matrix[dst * 2 + 1], 0.0);
        }
    }

    /// properties over random scenes: gains are finite and non-negative, the panned power equals
    /// the volume curve's gain squared, and mirroring left/right swaps the stereo gains
    #[test]
    fn random_scenes() {
        let mut r = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..20000 {
            let pos = [r.f(-50.0, 50.0), r.f(-50.0, 50.0), r.f(-50.0, 50.0)];
            if len(pos) < 1e-3 {
                continue;
            }
            let e = emitter(pos);
            let d = calculate(STEREO, 343.5, &listener(), &e, ALL, 2);
            assert!(d.matrix.iter().all(|x| x.is_finite() && *x >= 0.0), "{pos:?} {:?}", d.matrix);
            let p = d.matrix[0].powi(2) + d.matrix[1].powi(2);
            let want = (1.0 / len(pos).max(1.0)).powi(2);
            assert!((p - want).abs() < 1e-4 * want.max(1e-3), "{pos:?}: {p} vs {want}");
            let m = calculate(STEREO, 343.5, &listener(), &emitter([-pos[0], pos[1], pos[2]]), MATRIX, 2);
            assert!((m.matrix[0] - d.matrix[1]).abs() < 1e-4 && (m.matrix[1] - d.matrix[0]).abs() < 1e-4, "{pos:?}");
            assert!(d.doppler == 1.0 && d.angle.is_finite() && (0.0..=PI).contains(&d.angle));
            // 5.1: power over the five full-range speakers is the same
            let s = calculate(0x3f, 343.5, &listener(), &e, MATRIX, 6);
            let p5: f32 = [0, 1, 2, 4, 5].iter().map(|&i| s.matrix[i].powi(2)).sum();
            assert!((p5 - want).abs() < 1e-4 * want.max(1e-3), "{pos:?}: 5.1 {p5}");
            assert_eq!(s.matrix[3], 0.0);
        }
    }

    #[test]
    fn stereo_folds_the_back() {
        let spk = [7.0 * PI / 4.0, PI / 4.0];
        assert_eq!(pan(PI / 2.0, &spk), vec![0.0, 1.0]);
        assert_eq!(pan(3.0 * PI / 2.0, &spk), vec![1.0, 0.0]);
        let a = pan(PI / 8.0, &spk);
        let b = pan(PI - PI / 8.0, &spk);
        assert!((a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6);
        for i in 0..3600 {
            let g = pan(i as f32 * TWO_PI / 3600.0, &spk);
            assert!((g[0] * g[0] + g[1] * g[1] - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn pan_covers_the_circle() {
        let spk = [7.0 * PI / 4.0, PI / 4.0, 0.0, 5.0 * PI / 4.0, 3.0 * PI / 4.0];
        for i in 0..3600 {
            let az = i as f32 * TWO_PI / 3600.0;
            let g = pan(az, &spk);
            let p: f32 = g.iter().map(|x| x * x).sum();
            assert!((p - 1.0).abs() < 1e-5, "{az}: {g:?}");
            assert!(g.iter().filter(|x| **x > 1e-6).count() <= 2);
        }
        // a source on a speaker is on it alone
        for (k, &a) in spk.iter().enumerate() {
            let g = pan(a, &spk);
            assert!((g[k] - 1.0).abs() < 1e-5, "{k}: {g:?}");
        }
    }
}
