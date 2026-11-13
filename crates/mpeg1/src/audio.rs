//! MPEG-1 audio layer II: frames from an elementary stream to 1152 stereo samples each (a
//! mono stream on both channels). A port of pl_mpeg's plm_audio (MIT, Dominic Szablewski; it
//! is based on kjmp2 by Martin J. Fiedler), fed bytes as they come.

use crate::bits::Bits;
use crate::tables::*;

pub const SAMPLES_PER_FRAME: usize = 1152;
const MODE_JOINT_STEREO: u32 = 1;
const MODE_MONO: u32 = 3;

/// A frame header's facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub rate: u32,
    /// bytes of the frame, header included
    pub size: usize,
    bitrate_index: usize,
    rate_index: usize,
    mode: u32,
    bound: usize,
    crc: bool,
}

/// Reads a frame header at the start of `d` (11 sync bits set, MPEG-1, layer II).
pub fn header(d: &[u8]) -> Option<Header> {
    if d.len() < 4 || d[0] != 0xff || d[1] & 0xe0 != 0xe0 {
        return None;
    }
    let mut b = Bits::new(d);
    b.skip(11);
    let version = b.read(2);
    let layer = b.read(2);
    let crc = b.read(1) == 0;
    if version != 3 || layer != 2 {
        return None;
    }
    let bitrate_index = (b.read(4) as i32 - 1) as usize;
    if bitrate_index > 13 {
        return None;
    }
    let rate_index = b.read(2) as usize;
    if rate_index == 3 {
        return None;
    }
    let padding = b.read(1) as usize;
    b.skip(1);
    let mode = b.read(2);
    let ext = b.read(2);
    let bound = if mode == MODE_JOINT_STEREO {
        ((ext + 1) << 2) as usize
    } else if mode == MODE_MONO {
        0
    } else {
        32
    };
    let rate = SAMPLE_RATE[rate_index];
    let size = (144000 * BIT_RATE[bitrate_index] / rate) as usize + padding;
    Some(Header { rate, size, bitrate_index, rate_index, mode, bound, crc })
}

pub struct Audio {
    es: Vec<u8>,
    at: usize,
    ended: bool,
    first: Option<Header>,
    allocation: [[u8; 32]; 2],
    sf_info: [[u8; 32]; 2],
    scale_factor: [[[i32; 3]; 32]; 2],
    sample: [[[i32; 3]; 32]; 2],
    d: Box<[f32; 1024]>,
    v: Box<[[f32; 1024]; 2]>,
    v_pos: usize,
    /// the last frame's samples, interleaved left / right
    pub out: Vec<f32>,
    pub frames: u64,
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

impl Audio {
    pub fn new() -> Self {
        let mut d = Box::new([0f32; 1024]);
        d[..512].copy_from_slice(&SYNTHESIS_WINDOW);
        d[512..].copy_from_slice(&SYNTHESIS_WINDOW);
        Audio {
            es: Vec::new(),
            at: 0,
            ended: false,
            first: None,
            allocation: [[0; 32]; 2],
            sf_info: [[0; 32]; 2],
            scale_factor: [[[0; 3]; 32]; 2],
            sample: [[[0; 3]; 32]; 2],
            d,
            v: Box::new([[0f32; 1024]; 2]),
            v_pos: 0,
            out: vec![0.0; SAMPLES_PER_FRAME * 2],
            frames: 0,
        }
    }

    /// The first frame's header (the stream's rate), once one was seen.
    pub fn header(&self) -> Option<Header> {
        self.first
    }

    pub fn push(&mut self, d: &[u8]) {
        if self.at > 1 << 16 && self.at * 2 > self.es.len() {
            self.es.drain(..self.at);
            self.at = 0;
        }
        self.es.extend_from_slice(d);
    }

    pub fn end(&mut self) {
        self.ended = true;
    }

    pub fn reset(&mut self) {
        self.es.clear();
        self.at = 0;
        self.ended = false;
    }

    /// Decodes the next whole frame into `out` (true), if one is in.
    pub fn decode(&mut self) -> bool {
        loop {
            // the next sync word; a header that disagrees with the first one is not a frame
            // (the sync word can occur in the data)
            while self.at + 1 < self.es.len() && !(self.es[self.at] == 0xff && self.es[self.at + 1] & 0xfe == 0xfc) {
                self.at += 1;
            }
            if self.at + 4 > self.es.len() {
                return false;
            }
            let Some(h) = header(&self.es[self.at..]) else {
                self.at += 1;
                continue;
            };
            if let Some(f) = self.first {
                if (f.bitrate_index, f.rate_index, f.mode) != (h.bitrate_index, h.rate_index, h.mode) {
                    self.at += 1;
                    continue;
                }
            }
            if self.at + h.size > self.es.len() {
                if !self.ended {
                    return false;
                }
                // (a cut last frame: what there is, the rest silence)
            }
            self.first.get_or_insert(h);
            let end = (self.at + h.size).min(self.es.len());
            // (a last frame cut inside its CRC has no data at all)
            let frame = self.es[(self.at + if h.crc { 6 } else { 4 }).min(end)..end].to_vec();
            self.at = end;
            self.frame(&h, &frame);
            self.frames += 1;
            return true;
        }
    }

    fn frame(&mut self, h: &Header, d: &[u8]) {
        let mut b = Bits::new(d);
        let tab1 = if h.mode == MODE_MONO { 0 } else { 1 };
        let tab2 = QUANT_LUT_STEP_1[tab1][h.bitrate_index] as usize;
        let tab3 = QUANT_LUT_STEP_2[tab2][h.rate_index] as usize;
        let sblimit = tab3 & 63;
        let tab3 = tab3 >> 6;
        let bound = h.bound.min(sblimit);
        let read_allocation = |b: &mut Bits, sb: usize| -> u8 {
            let tab4 = QUANT_LUT_STEP_3[tab3][sb];
            QUANT_LUT_STEP_4[(tab4 & 15) as usize][b.read((tab4 >> 4) as u32) as usize]
        };
        for sb in 0..bound {
            self.allocation[0][sb] = read_allocation(&mut b, sb);
            self.allocation[1][sb] = read_allocation(&mut b, sb);
        }
        for sb in bound..sblimit {
            let a = read_allocation(&mut b, sb);
            self.allocation[0][sb] = a;
            self.allocation[1][sb] = a;
        }
        let channels = if h.mode == MODE_MONO { 1 } else { 2 };
        for sb in 0..sblimit {
            for ch in 0..channels {
                if self.allocation[ch][sb] != 0 {
                    self.sf_info[ch][sb] = b.read(2) as u8;
                }
            }
            if h.mode == MODE_MONO {
                self.sf_info[1][sb] = self.sf_info[0][sb];
            }
        }
        for sb in 0..sblimit {
            for ch in 0..channels {
                if self.allocation[ch][sb] != 0 {
                    let sf = &mut self.scale_factor[ch][sb];
                    match self.sf_info[ch][sb] {
                        0 => {
                            sf[0] = b.read(6) as i32;
                            sf[1] = b.read(6) as i32;
                            sf[2] = b.read(6) as i32;
                        }
                        1 => {
                            sf[0] = b.read(6) as i32;
                            sf[1] = sf[0];
                            sf[2] = b.read(6) as i32;
                        }
                        2 => {
                            sf[0] = b.read(6) as i32;
                            sf[1] = sf[0];
                            sf[2] = sf[0];
                        }
                        _ => {
                            sf[0] = b.read(6) as i32;
                            sf[1] = b.read(6) as i32;
                            sf[2] = sf[1];
                        }
                    }
                }
            }
            if h.mode == MODE_MONO {
                self.scale_factor[1][sb] = self.scale_factor[0][sb];
            }
        }
        let mut out_pos = 0;
        for part in 0..3 {
            for _granule in 0..4 {
                for sb in 0..bound {
                    self.read_samples(&mut b, 0, sb, part);
                    self.read_samples(&mut b, 1, sb, part);
                }
                for sb in bound..sblimit {
                    self.read_samples(&mut b, 0, sb, part);
                    self.sample[1][sb] = self.sample[0][sb];
                }
                for sb in sblimit..32 {
                    self.sample[0][sb] = [0; 3];
                    self.sample[1][sb] = [0; 3];
                }
                for p in 0..3 {
                    self.v_pos = (self.v_pos.wrapping_sub(64)) & 1023;
                    for ch in 0..2 {
                        idct36(&self.sample[ch], p, &mut self.v[ch], self.v_pos);
                        let mut u = [0f32; 32];
                        let mut di = 512 - (self.v_pos >> 1);
                        let mut vi = (self.v_pos % 128) >> 1;
                        while vi < 1024 {
                            for k in u.iter_mut() {
                                *k += self.d[di] * self.v[ch][vi];
                                di += 1;
                                vi += 1;
                            }
                            vi += 128 - 32;
                            di += 64 - 32;
                        }
                        di -= 512 - 32;
                        vi = (128 - 32 + 1024) - vi;
                        while vi < 1024 {
                            for k in u.iter_mut() {
                                *k += self.d[di] * self.v[ch][vi];
                                di += 1;
                                vi += 1;
                            }
                            vi += 128 - 32;
                            di += 64 - 32;
                        }
                        for (j, &x) in u.iter().enumerate() {
                            // (pl_mpeg divides by 2^30 + 2^24: 1.6 % too quiet)
                            self.out[((out_pos + j) << 1) + ch] = x / -1073741824.0;
                        }
                    }
                    out_pos += 32;
                }
            }
        }
    }

    fn read_samples(&mut self, b: &mut Bits, ch: usize, sb: usize, part: usize) {
        let q = self.allocation[ch][sb];
        let sample = &mut self.sample[ch][sb];
        if q == 0 {
            *sample = [0; 3];
            return;
        }
        let (levels, group, bits) = QUANT_TAB[q as usize - 1];
        let mut sf = self.scale_factor[ch][sb][part];
        if sf == 63 {
            sf = 0;
        } else {
            let shift = sf / 3;
            sf = (SCALEFACTOR_BASE[(sf % 3) as usize] + ((1 << shift) >> 1)) >> shift;
        }
        let mut adj = levels as i32;
        if group {
            let mut val = b.read(bits as u32) as i32;
            sample[0] = val % adj;
            val /= adj;
            sample[1] = val % adj;
            sample[2] = val / adj;
        } else {
            sample[0] = b.read(bits as u32) as i32;
            sample[1] = b.read(bits as u32) as i32;
            sample[2] = b.read(bits as u32) as i32;
        }
        // (pl_mpeg takes 65536 / (levels + 1): coarse quantizers came out up to a quarter too
        // quiet; ISO 11172-3 2.4.3.3.4 steps by 2 / levels)
        let scale = (65536 + adj / 2) / adj;
        adj = ((adj + 1) >> 1) - 1;
        for s in sample.iter_mut() {
            let val = (adj - *s).wrapping_mul(scale);
            *s = (val.wrapping_mul(sf >> 12).wrapping_add((val.wrapping_mul(sf & 4095).wrapping_add(2048)) >> 12)) >> 12;
        }
    }
}

/// The 32-point DCT of one third of a subband sample set into V at `dp` (pl_mpeg's
/// plm_audio_idct36, its constants and order of operations kept).
fn idct36(s: &[[i32; 3]; 32], ss: usize, d: &mut [f32; 1024], dp: usize) {
    let f = |i: usize| s[i][ss] as f32;
    let t01 = f(0) + f(31);
    let t02 = (f(0) - f(31)) * 0.500602998235;
    let mut t03 = f(1) + f(30);
    let mut t04 = (f(1) - f(30)) * 0.505470959898;
    let mut t05 = f(2) + f(29);
    let mut t06 = (f(2) - f(29)) * 0.515447309923;
    let mut t07 = f(3) + f(28);
    let mut t08 = (f(3) - f(28)) * 0.53104259109;
    let mut t09 = f(4) + f(27);
    let mut t10 = (f(4) - f(27)) * 0.553103896034;
    let mut t11 = f(5) + f(26);
    let mut t12 = (f(5) - f(26)) * 0.582934968206;
    let mut t13 = f(6) + f(25);
    let mut t14 = (f(6) - f(25)) * 0.622504123036;
    let mut t15 = f(7) + f(24);
    let mut t16 = (f(7) - f(24)) * 0.674808341455;
    let mut t17 = f(8) + f(23);
    let mut t18 = (f(8) - f(23)) * 0.744536271002;
    let mut t19 = f(9) + f(22);
    let mut t20 = (f(9) - f(22)) * 0.839349645416;
    let mut t21 = f(10) + f(21);
    let mut t22 = (f(10) - f(21)) * 0.972568237862;
    let mut t23 = f(11) + f(20);
    let mut t24 = (f(11) - f(20)) * 1.16943993343;
    let mut t25 = f(12) + f(19);
    let mut t26 = (f(12) - f(19)) * 1.48416461631;
    let mut t27 = f(13) + f(18);
    let mut t28 = (f(13) - f(18)) * 2.05778100995;
    let mut t29 = f(14) + f(17);
    let mut t30 = (f(14) - f(17)) * 3.40760841847;
    let mut t31 = f(15) + f(16);
    let mut t32 = (f(15) - f(16)) * 10.1900081235;
    let mut t01 = t01;
    let mut t02 = t02;
    let mut t33;

    t33 = t01 + t31; t31 = (t01 - t31) * 0.502419286188;
    t01 = t03 + t29; t29 = (t03 - t29) * 0.52249861494;
    t03 = t05 + t27; t27 = (t05 - t27) * 0.566944034816;
    t05 = t07 + t25; t25 = (t07 - t25) * 0.64682178336;
    t07 = t09 + t23; t23 = (t09 - t23) * 0.788154623451;
    t09 = t11 + t21; t21 = (t11 - t21) * 1.06067768599;
    t11 = t13 + t19; t19 = (t13 - t19) * 1.72244709824;
    t13 = t15 + t17; t17 = (t15 - t17) * 5.10114861869;
    t15 = t33 + t13; t13 = (t33 - t13) * 0.509795579104;
    t33 = t01 + t11; t01 = (t01 - t11) * 0.601344886935;
    t11 = t03 + t09; t09 = (t03 - t09) * 0.899976223136;
    t03 = t05 + t07; t07 = (t05 - t07) * 2.56291544774;
    t05 = t15 + t03; t15 = (t15 - t03) * 0.541196100146;
    t03 = t33 + t11; t11 = (t33 - t11) * 1.30656296488;
    t33 = t05 + t03; t05 = (t05 - t03) * 0.707106781187;
    t03 = t15 + t11; t15 = (t15 - t11) * 0.707106781187;
    t03 += t15;
    t11 = t13 + t07; t13 = (t13 - t07) * 0.541196100146;
    t07 = t01 + t09; t09 = (t01 - t09) * 1.30656296488;
    t01 = t11 + t07; t07 = (t11 - t07) * 0.707106781187;
    t11 = t13 + t09; t13 = (t13 - t09) * 0.707106781187;
    t11 += t13; t01 += t11;
    t11 += t07; t07 += t13;
    t09 = t31 + t17; t31 = (t31 - t17) * 0.509795579104;
    t17 = t29 + t19; t29 = (t29 - t19) * 0.601344886935;
    t19 = t27 + t21; t21 = (t27 - t21) * 0.899976223136;
    t27 = t25 + t23; t23 = (t25 - t23) * 2.56291544774;
    t25 = t09 + t27; t09 = (t09 - t27) * 0.541196100146;
    t27 = t17 + t19; t19 = (t17 - t19) * 1.30656296488;
    t17 = t25 + t27; t27 = (t25 - t27) * 0.707106781187;
    t25 = t09 + t19; t19 = (t09 - t19) * 0.707106781187;
    t25 += t19;
    t09 = t31 + t23; t31 = (t31 - t23) * 0.541196100146;
    t23 = t29 + t21; t21 = (t29 - t21) * 1.30656296488;
    t29 = t09 + t23; t23 = (t09 - t23) * 0.707106781187;
    t09 = t31 + t21; t31 = (t31 - t21) * 0.707106781187;
    t09 += t31; t29 += t09; t09 += t23; t23 += t31;
    t17 += t29; t29 += t25; t25 += t09; t09 += t27;
    t27 += t23; t23 += t19; t19 += t31;
    t21 = t02 + t32; t02 = (t02 - t32) * 0.502419286188;
    t32 = t04 + t30; t04 = (t04 - t30) * 0.52249861494;
    t30 = t06 + t28; t28 = (t06 - t28) * 0.566944034816;
    t06 = t08 + t26; t08 = (t08 - t26) * 0.64682178336;
    t26 = t10 + t24; t10 = (t10 - t24) * 0.788154623451;
    t24 = t12 + t22; t22 = (t12 - t22) * 1.06067768599;
    t12 = t14 + t20; t20 = (t14 - t20) * 1.72244709824;
    t14 = t16 + t18; t16 = (t16 - t18) * 5.10114861869;
    t18 = t21 + t14; t14 = (t21 - t14) * 0.509795579104;
    t21 = t32 + t12; t32 = (t32 - t12) * 0.601344886935;
    t12 = t30 + t24; t24 = (t30 - t24) * 0.899976223136;
    t30 = t06 + t26; t26 = (t06 - t26) * 2.56291544774;
    t06 = t18 + t30; t18 = (t18 - t30) * 0.541196100146;
    t30 = t21 + t12; t12 = (t21 - t12) * 1.30656296488;
    t21 = t06 + t30; t30 = (t06 - t30) * 0.707106781187;
    t06 = t18 + t12; t12 = (t18 - t12) * 0.707106781187;
    t06 += t12;
    t18 = t14 + t26; t26 = (t14 - t26) * 0.541196100146;
    t14 = t32 + t24; t24 = (t32 - t24) * 1.30656296488;
    t32 = t18 + t14; t14 = (t18 - t14) * 0.707106781187;
    t18 = t26 + t24; t24 = (t26 - t24) * 0.707106781187;
    t18 += t24; t32 += t18;
    t18 += t14; t26 = t14 + t24;
    t14 = t02 + t16; t02 = (t02 - t16) * 0.509795579104;
    t16 = t04 + t20; t04 = (t04 - t20) * 0.601344886935;
    t20 = t28 + t22; t22 = (t28 - t22) * 0.899976223136;
    t28 = t08 + t10; t10 = (t08 - t10) * 2.56291544774;
    t08 = t14 + t28; t14 = (t14 - t28) * 0.541196100146;
    t28 = t16 + t20; t20 = (t16 - t20) * 1.30656296488;
    t16 = t08 + t28; t28 = (t08 - t28) * 0.707106781187;
    t08 = t14 + t20; t20 = (t14 - t20) * 0.707106781187;
    t08 += t20;
    t14 = t02 + t10; t02 = (t02 - t10) * 0.541196100146;
    t10 = t04 + t22; t22 = (t04 - t22) * 1.30656296488;
    t04 = t14 + t10; t10 = (t14 - t10) * 0.707106781187;
    t14 = t02 + t22; t02 = (t02 - t22) * 0.707106781187;
    t14 += t02; t04 += t14; t14 += t10; t10 += t02;
    t16 += t04; t04 += t08; t08 += t14; t14 += t28;
    t28 += t10; t10 += t20; t20 += t02; t21 += t16;
    t16 += t32; t32 += t04; t04 += t06; t06 += t08;
    t08 += t18; t18 += t14; t14 += t30; t30 += t28;
    t28 += t26; t26 += t10; t10 += t12; t12 += t20;
    t20 += t24; t24 += t02;

    d[dp + 48] = -t33;
    d[dp + 49] = -t21; d[dp + 47] = -t21;
    d[dp + 50] = -t17; d[dp + 46] = -t17;
    d[dp + 51] = -t16; d[dp + 45] = -t16;
    d[dp + 52] = -t01; d[dp + 44] = -t01;
    d[dp + 53] = -t32; d[dp + 43] = -t32;
    d[dp + 54] = -t29; d[dp + 42] = -t29;
    d[dp + 55] = -t04; d[dp + 41] = -t04;
    d[dp + 56] = -t03; d[dp + 40] = -t03;
    d[dp + 57] = -t06; d[dp + 39] = -t06;
    d[dp + 58] = -t25; d[dp + 38] = -t25;
    d[dp + 59] = -t08; d[dp + 37] = -t08;
    d[dp + 60] = -t11; d[dp + 36] = -t11;
    d[dp + 61] = -t18; d[dp + 35] = -t18;
    d[dp + 62] = -t09; d[dp + 34] = -t09;
    d[dp + 63] = -t14; d[dp + 33] = -t14;
    d[dp + 32] = -t05;
    d[dp] = t05; d[dp + 31] = -t30;
    d[dp + 1] = t30; d[dp + 30] = -t27;
    d[dp + 2] = t27; d[dp + 29] = -t28;
    d[dp + 3] = t28; d[dp + 28] = -t07;
    d[dp + 4] = t07; d[dp + 27] = -t26;
    d[dp + 5] = t26; d[dp + 26] = -t23;
    d[dp + 6] = t23; d[dp + 25] = -t10;
    d[dp + 7] = t10; d[dp + 24] = -t15;
    d[dp + 8] = t15; d[dp + 23] = -t12;
    d[dp + 9] = t12; d[dp + 22] = -t19;
    d[dp + 10] = t19; d[dp + 21] = -t20;
    d[dp + 11] = t20; d[dp + 20] = -t13;
    d[dp + 12] = t13; d[dp + 19] = -t24;
    d[dp + 13] = t24; d[dp + 18] = -t31;
    d[dp + 14] = t31; d[dp + 17] = -t02;
    d[dp + 15] = t02; d[dp + 16] = 0.0;
}
