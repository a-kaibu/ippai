//! Windows Media Audio 1 and 2 (wFormatTag 0x160 / 0x161): FFmpeg 7.1's wmadec.c and wma.c
//! ported (LGPL-2.1-or-later, like them). A packet of `block_align` bytes is a superframe of
//! frames of `frame_len` samples (bit reservoir: a frame may start in the packet before);
//! frames are blocks of MDCT coefficients (run/level coded) times exponents (VLC or LSP),
//! noise-filled in the high bands.

use crate::bits::Bits;
use crate::mdct::Imdct;
use crate::vlc::Vlc;
use crate::wma_tables as t;

const BLOCK_MIN_BITS: u32 = 7;
const BLOCK_MAX_BITS: u32 = 11;
const BLOCK_MAX_SIZE: usize = 1 << BLOCK_MAX_BITS;
const BLOCK_NB_SIZES: usize = (BLOCK_MAX_BITS - BLOCK_MIN_BITS + 1) as usize;
const HIGH_BAND_MAX_SIZE: usize = 16;
const NB_LSP_COEFS: usize = 10;
const MAX_CODED_SUPERFRAME_SIZE: usize = 32768;
const MAX_CHANNELS: usize = 2;
const NOISE_TAB_SIZE: usize = 8192;
const LSP_POW_BITS: u32 = 7;
const VLCBITS: u32 = 9;
const EXPVLCBITS: u32 = 8;
const HGAINVLCBITS: u32 = 9;
/// bytes of zeros after a buffer (FFmpeg's AV_INPUT_BUFFER_PADDING_SIZE)
const PADDING: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// the stream's parameters are not a WMA stream's
    Params(&'static str),
    /// a packet does not decode
    Data(&'static str),
}

struct CoefTable {
    vlc: Vlc,
    run: Vec<u16>,
    level: Vec<f32>,
}

fn coef_table(k: usize) -> CoefTable {
    let (codes, bits, levels): (&[u32], &[u8], &[u16]) = match k {
        0 => (&t::COEF0_HUFFCODES, &t::COEF0_HUFFBITS, &t::LEVELS0),
        1 => (&t::COEF1_HUFFCODES, &t::COEF1_HUFFBITS, &t::LEVELS1),
        2 => (&t::COEF2_HUFFCODES, &t::COEF2_HUFFBITS, &t::LEVELS2),
        3 => (&t::COEF3_HUFFCODES, &t::COEF3_HUFFBITS, &t::LEVELS3),
        4 => (&t::COEF4_HUFFCODES, &t::COEF4_HUFFBITS, &t::LEVELS4),
        _ => (&t::COEF5_HUFFCODES, &t::COEF5_HUFFBITS, &t::LEVELS5),
    };
    let n = bits.len();
    let vlc = Vlc::from_codes(VLCBITS, bits, codes);
    let mut run = vec![0u16; n];
    let mut level = vec![0f32; n];
    let (mut i, mut lv, mut k) = (2usize, 1f32, 0usize);
    while i < n {
        let l = levels[k] as usize;
        k += 1;
        for j in 0..l {
            if i >= n {
                break;
            }
            run[i] = j as u16;
            level[i] = lv;
            i += 1;
        }
        lv += 1.0;
    }
    CoefTable { vlc, run, level }
}

fn frame_len_bits(rate: u32, version: u32) -> u32 {
    if rate <= 16000 {
        9
    } else if rate <= 22050 || (rate <= 32000 && version == 1) {
        10
    } else {
        11
    }
}

fn total_gain_to_bits(g: i32) -> u32 {
    match g {
        _ if g < 15 => 13,
        _ if g < 32 => 12,
        _ if g < 40 => 11,
        _ if g < 45 => 10,
        _ => 9,
    }
}

fn log2(v: u32) -> u32 {
    31 - (v | 1).leading_zeros()
}

/// pow(10, i / 16) for i in -60..95
fn pow_tab() -> &'static [f32; 156] {
    static P: std::sync::OnceLock<[f32; 156]> = std::sync::OnceLock::new();
    P.get_or_init(|| std::array::from_fn(|i| 10f64.powf((i as f64 - 60.0) / 16.0) as f32))
}

fn sine_window(n: usize) -> Vec<f32> {
    (0..n).map(|i| ((i as f64 + 0.5) * (std::f64::consts::PI / (2.0 * n as f64))).sin() as f32).collect()
}

pub struct Decoder {
    pub channels: usize,
    pub rate: u32,
    block_align: usize,
    version: u32,
    use_bit_reservoir: bool,
    use_variable_block_len: bool,
    use_exp_vlc: bool,
    use_noise_coding: bool,
    byte_offset_bits: u32,
    exp_vlc: Option<Vlc>,
    exponent_bands: [[u16; 25]; BLOCK_NB_SIZES],
    high_band_start: [i32; BLOCK_NB_SIZES],
    coefs_start: i32,
    coefs_end: [i32; BLOCK_NB_SIZES],
    exponent_high_sizes: [usize; BLOCK_NB_SIZES],
    exponent_high_bands: [[i32; HIGH_BAND_MAX_SIZE]; BLOCK_NB_SIZES],
    hgain_vlc: Option<Vlc>,
    high_band_coded: [[bool; HIGH_BAND_MAX_SIZE]; MAX_CHANNELS],
    high_band_values: [[i32; HIGH_BAND_MAX_SIZE]; MAX_CHANNELS],
    coef: [CoefTable; 2],
    pub frame_len: usize,
    frame_len_bits: u32,
    nb_block_sizes: usize,
    reset_block_lengths: bool,
    block_len_bits: u32,
    next_block_len_bits: u32,
    prev_block_len_bits: u32,
    block_len: usize,
    block_pos: usize,
    ms_stereo: bool,
    channel_coded: [bool; MAX_CHANNELS],
    exponents_bsize: [usize; MAX_CHANNELS],
    exponents: Vec<[f32; BLOCK_MAX_SIZE]>,
    max_exponent: [f32; MAX_CHANNELS],
    coefs1: Vec<[f32; BLOCK_MAX_SIZE]>,
    coefs: Vec<[f32; BLOCK_MAX_SIZE]>,
    output: Vec<f32>,
    mdct: Vec<Imdct>,
    windows: Vec<Vec<f32>>,
    frame_out: Vec<Vec<f32>>,
    last_superframe: Vec<u8>,
    last_bitoffset: usize,
    last_superframe_len: usize,
    exponents_initialized: [bool; MAX_CHANNELS],
    noise_table: Vec<f32>,
    noise_index: usize,
    noise_mult: f32,
    lsp_cos_table: Vec<f32>,
    lsp_pow_e_table: [f32; 256],
    lsp_pow_m_table1: [f32; 1 << LSP_POW_BITS],
    lsp_pow_m_table2: [f32; 1 << LSP_POW_BITS],
    /// samples of the output still to drop (the decoder's delay, 2 frames)
    pub skip: usize,
}

impl Decoder {
    /// A decoder for a stream of WAVEFORMATEX `tag` (0x160 / 0x161), with the bytes after the
    /// WAVEFORMATEX (`extra`).
    pub fn new(tag: u16, channels: u16, rate: u32, bit_rate: u32, block_align: u16, extra: &[u8]) -> Result<Decoder, Error> {
        let version = match tag {
            0x160 => 1,
            0x161 => 2,
            _ => return Err(Error::Params("not WMA 1/2")),
        };
        if block_align == 0 {
            return Err(Error::Params("block_align is not set"));
        }
        let channels = channels as usize;
        if rate == 0 || rate > 50000 || channels == 0 || channels > 2 || bit_rate == 0 {
            return Err(Error::Params("rate, channels or bit rate"));
        }
        let rd16 = |o: usize| u16::from_le_bytes([extra[o], extra[o + 1]]) as u32;
        let flags2 = if version == 1 && extra.len() >= 4 {
            rd16(2)
        } else if version == 2 && extra.len() >= 6 {
            rd16(4)
        } else {
            0
        };
        let use_exp_vlc = flags2 & 1 != 0;
        let use_bit_reservoir = flags2 & 2 != 0;
        let mut use_variable_block_len = flags2 & 4 != 0;
        if version == 2 && extra.len() >= 8 && rd16(4) == 0xd && use_variable_block_len {
            use_variable_block_len = false;
        }
        let flb = frame_len_bits(rate, version);
        let frame_len = 1usize << flb;
        let nb_block_sizes = if use_variable_block_len {
            let mut nb = ((flags2 >> 3) & 3) as usize + 1;
            if bit_rate / channels as u32 >= 32000 {
                nb += 2;
            }
            nb.min((flb - BLOCK_MIN_BITS) as usize) + 1
        } else {
            1
        };
        let mut use_noise_coding = true;
        let mut high_freq = rate as f32 * 0.5;
        let mut rate1 = rate;
        if version == 2 {
            rate1 = [44100, 22050, 16000, 11025, 8000].into_iter().find(|&r| rate >= r).unwrap_or(rate);
        }
        let bps = bit_rate as f32 / (channels as f32 * rate as f32);
        let byte_offset_bits = log2((bps * frame_len as f32 / 8.0 + 0.5) as i32 as u32) + 2;
        if byte_offset_bits + 3 > 25 {
            return Err(Error::Params("byte_offset_bits too large"));
        }
        let bps1 = if channels == 2 { bps * 1.6 } else { bps };
        match rate1 {
            44100 => {
                if bps1 >= 0.61 {
                    use_noise_coding = false
                } else {
                    high_freq *= 0.4
                }
            }
            22050 => {
                if bps1 >= 1.16 {
                    use_noise_coding = false
                } else if bps1 >= 0.72 {
                    high_freq *= 0.7
                } else {
                    high_freq *= 0.6
                }
            }
            16000 => high_freq *= if bps > 0.5 { 0.5 } else { 0.3 },
            11025 => high_freq *= 0.7,
            8000 => {
                if bps <= 0.625 {
                    high_freq *= 0.5
                } else if bps > 0.75 {
                    use_noise_coding = false
                } else {
                    high_freq *= 0.65
                }
            }
            _ => {
                high_freq *= if bps >= 0.8 {
                    0.75
                } else if bps >= 0.6 {
                    0.6
                } else {
                    0.5
                }
            }
        }
        let mut exponent_sizes = [0usize; BLOCK_NB_SIZES];
        let mut exponent_bands = [[0u16; 25]; BLOCK_NB_SIZES];
        let mut high_band_start = [0i32; BLOCK_NB_SIZES];
        let mut coefs_end = [0i32; BLOCK_NB_SIZES];
        let mut exponent_high_sizes = [0usize; BLOCK_NB_SIZES];
        let mut exponent_high_bands = [[0i32; HIGH_BAND_MAX_SIZE]; BLOCK_NB_SIZES];
        let coefs_start = if version == 1 { 3 } else { 0 };
        let r = rate as i32;
        for k in 0..nb_block_sizes {
            let block_len = (frame_len >> k) as i32;
            if version == 1 {
                let mut lpos = 0;
                let mut i = 0;
                while i < 25 {
                    let a = t::WMA_CRITICAL_FREQS[i] as i32;
                    let pos = ((block_len * 2 * a) + (r >> 1)) / r;
                    let pos = pos.min(block_len);
                    exponent_bands[0][i] = (pos - lpos) as u16;
                    if pos >= block_len {
                        i += 1;
                        break;
                    }
                    lpos = pos;
                    i += 1;
                }
                exponent_sizes[0] = i;
            } else {
                let a = flb as i32 - BLOCK_MIN_BITS as i32 - k as i32;
                let table: Option<&[u8]> = if a < 3 && a >= 0 {
                    let a = a as usize;
                    if rate >= 44100 {
                        Some(&t::EXPONENT_BAND_44100[a * 25..a * 25 + 25])
                    } else if rate >= 32000 {
                        Some(&t::EXPONENT_BAND_32000[a * 25..a * 25 + 25])
                    } else if rate >= 22050 {
                        Some(&t::EXPONENT_BAND_22050[a * 25..a * 25 + 25])
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(tb) = table {
                    let n = tb[0] as usize;
                    for i in 0..n {
                        exponent_bands[k][i] = tb[1 + i] as u16;
                    }
                    exponent_sizes[k] = n;
                } else {
                    let (mut j, mut lpos) = (0usize, 0i32);
                    for i in 0..25 {
                        let a = t::WMA_CRITICAL_FREQS[i] as i32;
                        let mut pos = ((block_len * 2 * a) + (r << 1)) / (4 * r);
                        pos <<= 2;
                        pos = pos.min(block_len);
                        if pos > lpos {
                            exponent_bands[k][j] = (pos - lpos) as u16;
                            j += 1;
                        }
                        if pos >= block_len {
                            break;
                        }
                        lpos = pos;
                    }
                    exponent_sizes[k] = j;
                }
            }
            coefs_end[k] = ((frame_len - (frame_len * 9) / 100) >> k) as i32;
            high_band_start[k] = ((block_len as f32 * 2.0 * high_freq) / rate as f32 + 0.5) as i32;
            let n = exponent_sizes[k];
            let (mut j, mut pos) = (0usize, 0i32);
            for i in 0..n {
                let mut start = pos;
                pos += exponent_bands[k][i] as i32;
                let mut end = pos;
                start = start.max(high_band_start[k]);
                end = end.min(coefs_end[k]);
                if end > start && j < HIGH_BAND_MAX_SIZE {
                    exponent_high_bands[k][j] = end - start;
                    j += 1;
                }
            }
            exponent_high_sizes[k] = j;
        }
        let windows = (0..nb_block_sizes).map(|i| sine_window(1 << (flb as usize - i))).collect();
        let mut noise_table = vec![0f32; NOISE_TAB_SIZE];
        let mut noise_mult = 0.0;
        if use_noise_coding {
            noise_mult = if use_exp_vlc { 0.02 } else { 0.04 };
            let mut seed: u32 = 1;
            let norm = ((1.0 / (1u64 << 31) as f64) * 3f64.sqrt() * noise_mult as f64) as f32;
            for v in noise_table.iter_mut() {
                seed = seed.wrapping_mul(314159).wrapping_add(1);
                *v = (seed as i32) as f32 * norm;
            }
        }
        let mut coef_vlc_table = 2;
        if rate >= 32000 {
            if bps1 < 0.72 {
                coef_vlc_table = 0;
            } else if bps1 < 1.16 {
                coef_vlc_table = 1;
            }
        }
        let coef = [coef_table(coef_vlc_table * 2), coef_table(coef_vlc_table * 2 + 1)];
        let mdct = (0..nb_block_sizes).map(|i| Imdct::new(flb - i as u32 + 1, 1.0 / 32768.0)).collect();
        let hgain_vlc = use_noise_coding.then(|| {
            let lens: Vec<u8> = (0..37).map(|i| t::FF_WMA_HGAIN_HUFFTAB[2 * i + 1]).collect();
            let syms: Vec<i32> = (0..37).map(|i| t::FF_WMA_HGAIN_HUFFTAB[2 * i] as i32).collect();
            Vlc::from_lengths(HGAINVLCBITS, &lens, Some(&syms), -18)
        });
        let exp_vlc = use_exp_vlc.then(|| Vlc::from_codes(EXPVLCBITS, &t::AAC_SCALEFACTOR_BITS, &t::AAC_SCALEFACTOR_CODE));
        let mut d = Decoder {
            channels,
            rate,
            block_align: block_align as usize,
            version,
            use_bit_reservoir,
            use_variable_block_len,
            use_exp_vlc,
            use_noise_coding,
            byte_offset_bits,
            exp_vlc,
            exponent_bands,
            high_band_start,
            coefs_start,
            coefs_end,
            exponent_high_sizes,
            exponent_high_bands,
            hgain_vlc,
            high_band_coded: [[false; HIGH_BAND_MAX_SIZE]; MAX_CHANNELS],
            high_band_values: [[0; HIGH_BAND_MAX_SIZE]; MAX_CHANNELS],
            coef,
            frame_len,
            frame_len_bits: flb,
            nb_block_sizes,
            reset_block_lengths: true,
            block_len_bits: flb,
            next_block_len_bits: flb,
            prev_block_len_bits: flb,
            block_len: 0,
            block_pos: 0,
            ms_stereo: false,
            channel_coded: [false; MAX_CHANNELS],
            exponents_bsize: [0; MAX_CHANNELS],
            exponents: vec![[0.0; BLOCK_MAX_SIZE]; MAX_CHANNELS],
            max_exponent: [1.0; MAX_CHANNELS],
            coefs1: vec![[0.0; BLOCK_MAX_SIZE]; MAX_CHANNELS],
            coefs: vec![[0.0; BLOCK_MAX_SIZE]; MAX_CHANNELS],
            output: vec![0.0; BLOCK_MAX_SIZE * 2],
            mdct,
            windows,
            frame_out: vec![vec![0.0; BLOCK_MAX_SIZE * 2]; MAX_CHANNELS],
            last_superframe: vec![0; MAX_CODED_SUPERFRAME_SIZE + PADDING],
            last_bitoffset: 0,
            last_superframe_len: 0,
            exponents_initialized: [false; MAX_CHANNELS],
            noise_table,
            noise_index: 0,
            noise_mult,
            lsp_cos_table: Vec::new(),
            lsp_pow_e_table: [0.0; 256],
            lsp_pow_m_table1: [0.0; 1 << LSP_POW_BITS],
            lsp_pow_m_table2: [0.0; 1 << LSP_POW_BITS],
            skip: frame_len * 2,
        };
        if !use_exp_vlc {
            d.lsp_to_curve_init();
        }
        Ok(d)
    }

    fn lsp_to_curve_init(&mut self) {
        let wdel = std::f64::consts::PI / self.frame_len as f64;
        self.lsp_cos_table = (0..self.frame_len).map(|i| (2.0 * (wdel * i as f64).cos()) as f32).collect();
        for i in 0..256 {
            let e = i as f64 - 126.0;
            self.lsp_pow_e_table[i] = (e * -0.25).exp2() as f32;
        }
        let mut b = 1.0f32;
        for i in (0..(1usize << LSP_POW_BITS)).rev() {
            let m = (1 << LSP_POW_BITS) + i;
            let a = m as f32 * (0.5 / (1 << LSP_POW_BITS) as f32);
            let a = (1.0 / (a as f64).sqrt().sqrt()) as f32;
            self.lsp_pow_m_table1[i] = 2.0 * a - b;
            self.lsp_pow_m_table2[i] = b - a;
            b = a;
        }
    }

    fn pow_m1_4(&self, x: f32) -> f32 {
        let u = x.to_bits();
        let e = (u >> 23) as usize;
        let m = ((u >> (23 - LSP_POW_BITS)) & ((1 << LSP_POW_BITS) - 1)) as usize;
        let tf = f32::from_bits(((u << LSP_POW_BITS) & ((1 << 23) - 1)) | (127 << 23));
        let a = self.lsp_pow_m_table1[m];
        let b = self.lsp_pow_m_table2[m];
        self.lsp_pow_e_table[e & 255] * (a + b * tf)
    }

    fn decode_exp_lsp(&mut self, r: &mut Bits, ch: usize) {
        let mut lsp = [0f32; NB_LSP_COEFS];
        for (i, l) in lsp.iter_mut().enumerate() {
            let v = if i == 0 || i >= 8 { r.get(3) } else { r.get(4) } as usize;
            *l = t::FF_WMA_LSP_CODEBOOK[i * 16 + v];
        }
        let mut val_max = 0f32;
        for i in 0..self.block_len {
            let (mut p, mut q) = (0.5f32, 0.5f32);
            let w = self.lsp_cos_table[i];
            let mut j = 1;
            while j < NB_LSP_COEFS {
                q *= w - lsp[j - 1];
                p *= w - lsp[j];
                j += 2;
            }
            p *= p * (2.0 - w);
            q *= q * (2.0 + w);
            let v = self.pow_m1_4(p + q);
            if v > val_max {
                val_max = v;
            }
            self.exponents[ch][i] = v;
        }
        self.max_exponent[ch] = val_max;
    }

    fn decode_exp_vlc(&mut self, r: &mut Bits, ch: usize) -> Result<(), Error> {
        let ptab = pow_tab();
        let bands = self.exponent_bands[(self.frame_len_bits - self.block_len_bits) as usize];
        let mut bi = 0usize;
        let mut q = 0usize;
        let q_end = self.block_len;
        let mut max_scale = 0f32;
        let mut last_exp: i32;
        // (FFmpeg's Duff's device writes n values, or 4 when n is 0)
        let fill = |e: &mut [f32; BLOCK_MAX_SIZE], q: &mut usize, n: u16, v: f32| {
            for _ in 0..if n == 0 { 4 } else { n } {
                if *q < BLOCK_MAX_SIZE {
                    e[*q] = v;
                }
                *q += 1;
            }
        };
        if self.version == 1 {
            last_exp = r.get(5) as i32 + 10;
            let v = ptab[(last_exp + 60) as usize];
            max_scale = v;
            let n = bands[bi];
            bi += 1;
            fill(&mut self.exponents[ch], &mut q, n, v);
        } else {
            last_exp = 36;
        }
        let vlc = self.exp_vlc.as_ref().ok_or(Error::Data("no exponent VLC"))?;
        while q < q_end {
            let code = vlc.read(r);
            if code < 0 {
                return Err(Error::Data("exponent code"));
            }
            last_exp += code - 60;
            if (last_exp + 60) as u32 >= 156 {
                return Err(Error::Data("exponent out of range"));
            }
            let v = ptab[(last_exp + 60) as usize];
            if v > max_scale {
                max_scale = v;
            }
            let n = *bands.get(bi).ok_or(Error::Data("exponent bands"))?;
            bi += 1;
            if n == 0 {
                return Err(Error::Data("empty band"));
            }
            fill(&mut self.exponents[ch], &mut q, n, v);
        }
        self.max_exponent[ch] = max_scale;
        Ok(())
    }

    fn window(&mut self, ch: usize, index: usize) {
        let out = &mut self.frame_out[ch][index..];
        let inp = &self.output;
        let bl = self.block_len;
        if self.block_len_bits <= self.prev_block_len_bits {
            let w = &self.windows[(self.frame_len_bits - self.block_len_bits) as usize];
            for i in 0..bl {
                out[i] += inp[i] * w[i];
            }
        } else {
            let pbl = 1usize << self.prev_block_len_bits;
            let n = (bl - pbl) / 2;
            let w = &self.windows[(self.frame_len_bits - self.prev_block_len_bits) as usize];
            for i in 0..pbl {
                out[n + i] += inp[n + i] * w[i];
            }
            out[n + pbl..n + pbl + n].copy_from_slice(&inp[n + pbl..n + pbl + n]);
        }
        let out = &mut out[bl..];
        let inp = &inp[bl..];
        if self.block_len_bits <= self.next_block_len_bits {
            let w = &self.windows[(self.frame_len_bits - self.block_len_bits) as usize];
            for i in 0..bl {
                out[i] = inp[i] * w[bl - 1 - i];
            }
        } else {
            let nbl = 1usize << self.next_block_len_bits;
            let n = (bl - nbl) / 2;
            let w = &self.windows[(self.frame_len_bits - self.next_block_len_bits) as usize];
            out[..n].copy_from_slice(&inp[..n]);
            for i in 0..nbl {
                out[n + i] = inp[n + i] * w[nbl - 1 - i];
            }
            for v in &mut out[n + nbl..n + nbl + n] {
                *v = 0.0;
            }
        }
    }

    fn run_level(&mut self, r: &mut Bits, ch: usize, tindex: usize, num_coefs: i32, coef_nb_bits: u32) -> Result<(), Error> {
        let tb = &self.coef[tindex];
        let mask = self.block_len - 1;
        let ptr = &mut self.coefs1[ch];
        let mut offset: i32 = 0;
        while offset < num_coefs {
            let code = tb.vlc.read(r);
            if code > 1 {
                offset += tb.run[code as usize] as i32;
                let neg = r.get1() == 0;
                let l = tb.level[code as usize];
                ptr[offset as usize & mask] = if neg { -l } else { l };
            } else if code == 1 {
                break;
            } else {
                // escape (an invalid code, -1, is taken as one too, as in FFmpeg)
                let level = r.get(coef_nb_bits) as i32;
                offset += r.get(self.frame_len_bits) as i32;
                let sign = r.get1() as i32 - 1;
                ptr[offset as usize & mask] = ((level ^ sign) - sign) as f32;
            }
            offset += 1;
        }
        if offset > num_coefs {
            return Err(Error::Data("spectral RLE overflow"));
        }
        Ok(())
    }

    /// Ok(true): the frame's last block.
    fn decode_block(&mut self, r: &mut Bits) -> Result<bool, Error> {
        let channels = self.channels;
        if self.use_variable_block_len {
            let n = log2(self.nb_block_sizes as u32 - 1) + 1;
            if self.reset_block_lengths {
                self.reset_block_lengths = false;
                let v = r.get(n) as usize;
                if v >= self.nb_block_sizes {
                    return Err(Error::Data("prev_block_len_bits"));
                }
                self.prev_block_len_bits = self.frame_len_bits - v as u32;
                let v = r.get(n) as usize;
                if v >= self.nb_block_sizes {
                    return Err(Error::Data("block_len_bits"));
                }
                self.block_len_bits = self.frame_len_bits - v as u32;
            } else {
                self.prev_block_len_bits = self.block_len_bits;
                self.block_len_bits = self.next_block_len_bits;
            }
            let v = r.get(n) as usize;
            if v >= self.nb_block_sizes {
                return Err(Error::Data("next_block_len_bits"));
            }
            self.next_block_len_bits = self.frame_len_bits - v as u32;
        } else {
            self.next_block_len_bits = self.frame_len_bits;
            self.prev_block_len_bits = self.frame_len_bits;
            self.block_len_bits = self.frame_len_bits;
        }
        if (self.frame_len_bits - self.block_len_bits) as usize >= self.nb_block_sizes {
            return Err(Error::Data("block_len_bits not valid"));
        }
        self.block_len = 1 << self.block_len_bits;
        if self.block_pos + self.block_len > self.frame_len {
            return Err(Error::Data("frame_len overflow"));
        }
        if channels == 2 {
            self.ms_stereo = r.get1() != 0;
        }
        let mut any = false;
        for ch in 0..channels {
            self.channel_coded[ch] = r.get1() != 0;
            any |= self.channel_coded[ch];
        }
        let bsize = (self.frame_len_bits - self.block_len_bits) as usize;
        if any {
            self.coded_block(r, bsize)?;
        }
        // (next:) the MDCT of each channel into the frame
        for ch in 0..channels {
            let n4 = self.block_len / 2;
            if self.channel_coded[ch] {
                // (disjoint fields: no copy of the 8 KiB coefficient array per block)
                self.mdct[bsize].calc(&mut self.output[..2 * self.block_len], &self.coefs[ch][..self.block_len]);
            } else if !(self.ms_stereo && ch == 1) {
                self.output.iter_mut().for_each(|v| *v = 0.0);
            }
            let index = self.frame_len / 2 + self.block_pos - n4;
            self.window(ch, index);
        }
        self.block_pos += self.block_len;
        Ok(self.block_pos >= self.frame_len)
    }

    fn coded_block(&mut self, r: &mut Bits, bsize: usize) -> Result<(), Error> {
        let channels = self.channels;
        let mut total_gain = 1i32;
        loop {
            if r.left() < 7 {
                return Err(Error::Data("total_gain overread"));
            }
            let a = r.get(7) as i32;
            total_gain += a;
            if a != 127 {
                break;
            }
        }
        let coef_nb_bits = total_gain_to_bits(total_gain);
        let n = self.coefs_end[bsize] - self.coefs_start;
        let mut nb_coefs = [n; MAX_CHANNELS];
        if self.use_noise_coding {
            for ch in 0..channels {
                if self.channel_coded[ch] {
                    for i in 0..self.exponent_high_sizes[bsize] {
                        let a = r.get1() != 0;
                        self.high_band_coded[ch][i] = a;
                        if a {
                            nb_coefs[ch] -= self.exponent_high_bands[bsize][i];
                        }
                    }
                }
            }
            for ch in 0..channels {
                if self.channel_coded[ch] {
                    let mut val: Option<i32> = None;
                    for i in 0..self.exponent_high_sizes[bsize] {
                        if self.high_band_coded[ch][i] {
                            let v = match val {
                                None => r.get(7) as i32 - 19,
                                Some(v) => v + self.hgain_vlc.as_ref().map_or(0, |h| h.read(r)),
                            };
                            val = Some(v);
                            self.high_band_values[ch][i] = v;
                        }
                    }
                }
            }
        }
        if self.block_len_bits == self.frame_len_bits || r.get1() != 0 {
            for ch in 0..channels {
                if self.channel_coded[ch] {
                    if self.use_exp_vlc {
                        self.decode_exp_vlc(r, ch)?;
                    } else {
                        self.decode_exp_lsp(r, ch);
                    }
                    self.exponents_bsize[ch] = bsize;
                    self.exponents_initialized[ch] = true;
                }
            }
        }
        for ch in 0..channels {
            if self.channel_coded[ch] && !self.exponents_initialized[ch] {
                return Err(Error::Data("exponents not initialized"));
            }
        }
        for ch in 0..channels {
            if self.channel_coded[ch] {
                let tindex = (ch == 1 && self.ms_stereo) as usize;
                let bl = self.block_len;
                self.coefs1[ch][..bl].iter_mut().for_each(|v| *v = 0.0);
                self.run_level(r, ch, tindex, nb_coefs[ch], coef_nb_bits)?;
            }
            if self.version == 1 && channels >= 2 {
                r.align();
            }
        }
        let n4 = self.block_len / 2;
        let mut mdct_norm = 1.0 / n4 as f32;
        if self.version == 1 {
            mdct_norm *= (n4 as f32).sqrt();
        }
        for ch in 0..channels {
            if !self.channel_coded[ch] {
                continue;
            }
            let esize = self.exponents_bsize[ch];
            let e_at = |i: usize| (i << bsize) >> esize;
            let mut mult = 10f64.powf(total_gain as f64 * 0.05) as f32 / self.max_exponent[ch];
            mult *= mdct_norm;
            let mut ci = 0usize; // into coefs
            let mut c1 = 0usize; // into coefs1
            let exps = &self.exponents[ch];
            let coefs1 = &self.coefs1[ch];
            let out = &mut self.coefs[ch];
            if self.use_noise_coding {
                let mult1 = mult;
                for i in 0..self.coefs_start as usize {
                    out[ci] = self.noise_table[self.noise_index] * exps[e_at(i)] * mult1;
                    ci += 1;
                    self.noise_index = (self.noise_index + 1) & (NOISE_TAB_SIZE - 1);
                }
                let n1 = self.exponent_high_sizes[bsize];
                let mut exp_power = [0f32; HIGH_BAND_MAX_SIZE];
                let mut eb = e_at(self.high_band_start[bsize] as usize);
                let mut last_high_band = 0usize;
                for j in 0..n1 {
                    let n = self.exponent_high_bands[bsize][j] as usize;
                    if self.high_band_coded[ch][j] {
                        let mut e2 = 0f32;
                        for i in 0..n {
                            let v = exps.get(eb + e_at(i)).copied().unwrap_or(0.0);
                            e2 += v * v;
                        }
                        exp_power[j] = e2 / n as f32;
                        last_high_band = j;
                    }
                    eb += (n << bsize) >> esize;
                }
                let mut eb = e_at(self.coefs_start as usize);
                for j in -1i32..n1 as i32 {
                    let n = if j < 0 { (self.high_band_start[bsize] - self.coefs_start) as usize } else { self.exponent_high_bands[bsize][j as usize] as usize };
                    if j >= 0 && self.high_band_coded[ch][j as usize] {
                        let j = j as usize;
                        let mut m1 = (exp_power[j] / exp_power[last_high_band]).sqrt();
                        m1 *= 10f64.powf(self.high_band_values[ch][j] as f64 * 0.05) as f32;
                        m1 /= self.max_exponent[ch] * self.noise_mult;
                        m1 *= mdct_norm;
                        for i in 0..n {
                            let noise = self.noise_table[self.noise_index];
                            self.noise_index = (self.noise_index + 1) & (NOISE_TAB_SIZE - 1);
                            if ci < BLOCK_MAX_SIZE {
                                out[ci] = noise * exps.get(eb + e_at(i)).copied().unwrap_or(0.0) * m1;
                            }
                            ci += 1;
                        }
                    } else {
                        for i in 0..n {
                            let noise = self.noise_table[self.noise_index];
                            self.noise_index = (self.noise_index + 1) & (NOISE_TAB_SIZE - 1);
                            if ci < BLOCK_MAX_SIZE {
                                out[ci] = (coefs1.get(c1).copied().unwrap_or(0.0) + noise) * exps.get(eb + e_at(i)).copied().unwrap_or(0.0) * mult;
                            }
                            ci += 1;
                            c1 += 1;
                        }
                    }
                    eb += (n << bsize) >> esize;
                }
                let n = self.block_len as i32 - self.coefs_end[bsize];
                // (exponents[-(1 << bsize) >> esize] relative to where the bands ended)
                let idx = (eb as i64 + ((-(1i64 << bsize)) >> esize)).max(0) as usize;
                let mult1 = mult * exps.get(idx).copied().unwrap_or(0.0);
                for _ in 0..n.max(0) {
                    if ci < BLOCK_MAX_SIZE {
                        out[ci] = self.noise_table[self.noise_index] * mult1;
                    }
                    ci += 1;
                    self.noise_index = (self.noise_index + 1) & (NOISE_TAB_SIZE - 1);
                }
            } else {
                for _ in 0..self.coefs_start {
                    out[ci] = 0.0;
                    ci += 1;
                }
                for i in 0..nb_coefs[ch].max(0) as usize {
                    if ci < BLOCK_MAX_SIZE {
                        out[ci] = coefs1[i] * exps[e_at(i)] * mult;
                    }
                    ci += 1;
                }
                let n = self.block_len as i32 - self.coefs_end[bsize];
                for _ in 0..n.max(0) {
                    if ci < BLOCK_MAX_SIZE {
                        out[ci] = 0.0;
                    }
                    ci += 1;
                }
            }
        }
        if self.ms_stereo && self.channel_coded[1] {
            if !self.channel_coded[0] {
                self.coefs[0][..self.block_len].iter_mut().for_each(|v| *v = 0.0);
                self.channel_coded[0] = true;
            }
            for i in 0..self.block_len {
                let (a, b) = (self.coefs[0][i], self.coefs[1][i]);
                self.coefs[0][i] = a + b;
                self.coefs[1][i] = a - b;
            }
        }
        Ok(())
    }

    /// Decodes a frame of `frame_len` samples per channel onto `out` (planar).
    fn decode_frame(&mut self, r: &mut Bits, out: &mut [Vec<f32>]) -> Result<(), Error> {
        self.block_pos = 0;
        loop {
            if self.decode_block(r)? {
                break;
            }
        }
        for ch in 0..self.channels {
            out[ch].extend_from_slice(&self.frame_out[ch][..self.frame_len]);
            let fl = self.frame_len;
            self.frame_out[ch].copy_within(fl..2 * fl, 0);
            // (the second half is the overlap the next frame adds to: what the blocks wrote)
        }
        Ok(())
    }

    /// Decodes one packet (a superframe, `block_align` bytes): planar samples, before the
    /// decoder's delay is taken off (`skip`).
    pub fn decode_raw(&mut self, buf: &[u8]) -> Result<Vec<Vec<f32>>, Error> {
        let mut out = vec![Vec::new(); self.channels];
        if buf.len() < self.block_align {
            return Err(Error::Data("packet smaller than block_align"));
        }
        let buf_size = self.block_align;
        let mut padded = buf[..buf_size].to_vec();
        padded.resize(buf_size + PADDING, 0);
        let buf = &padded[..];
        let mut r = Bits::with_len(buf, buf_size * 8);
        let nb_frames;
        if self.use_bit_reservoir {
            r.skip(4);
            let nf = r.get(4) as i32 - (self.last_superframe_len == 0) as i32;
            if nf <= 0 {
                let is_error = nf < 0 || r.left() <= 8;
                if is_error {
                    return Err(Error::Data("nb_frames"));
                }
                if self.last_superframe_len + buf_size - 1 > MAX_CODED_SUPERFRAME_SIZE {
                    self.last_superframe_len = 0;
                    return Err(Error::Data("superframe too large"));
                }
                let mut q = self.last_superframe_len;
                for _ in 0..buf_size - 1 {
                    self.last_superframe[q] = r.get(8) as u8;
                    q += 1;
                }
                self.last_superframe[q..q + PADDING].iter_mut().for_each(|b| *b = 0);
                // (FFmpeg adds bits, not bytes, here: kept as it is)
                self.last_superframe_len += 8 * buf_size - 8;
                return Ok(out);
            }
            nb_frames = nf;
        } else {
            nb_frames = 1;
        }
        let fail = |s: &mut Decoder, e: Error| {
            s.last_superframe_len = 0;
            Err(e)
        };
        if self.use_bit_reservoir {
            let bit_offset = r.get(self.byte_offset_bits + 3) as usize;
            if bit_offset as isize > r.left() {
                return fail(self, Error::Data("bit offset past the packet"));
            }
            let mut nb_frames = nb_frames;
            if self.last_superframe_len > 0 {
                if self.last_superframe_len + bit_offset.div_ceil(8) > MAX_CODED_SUPERFRAME_SIZE {
                    return fail(self, Error::Data("superframe too large"));
                }
                let mut q = self.last_superframe_len;
                let mut len = bit_offset;
                while len > 7 {
                    self.last_superframe[q] = r.get(8) as u8;
                    q += 1;
                    len -= 8;
                }
                if len > 0 {
                    self.last_superframe[q] = (r.get(len as u32) << (8 - len)) as u8;
                    q += 1;
                }
                self.last_superframe[q..q + PADDING].iter_mut().for_each(|b| *b = 0);
                let last = std::mem::take(&mut self.last_superframe);
                let mut lr = Bits::with_len(&last, self.last_superframe_len * 8 + bit_offset);
                if self.last_bitoffset > 0 {
                    lr.skip(self.last_bitoffset);
                }
                let res = self.decode_frame(&mut lr, &mut out);
                self.last_superframe = last;
                if let Err(e) = res {
                    return fail(self, e);
                }
                nb_frames -= 1;
            }
            let pos = bit_offset + 4 + 4 + self.byte_offset_bits as usize + 3;
            if pos >= MAX_CODED_SUPERFRAME_SIZE * 8 || pos > buf_size * 8 {
                return Err(Error::Data("frame position"));
            }
            let mut fr = Bits::with_len(&buf[pos >> 3..], (buf_size - (pos >> 3)) * 8);
            fr.skip(pos & 7);
            self.reset_block_lengths = true;
            for _ in 0..nb_frames {
                if let Err(e) = self.decode_frame(&mut fr, &mut out) {
                    return fail(self, e);
                }
            }
            let p = fr.count() + ((bit_offset + 4 + 4 + self.byte_offset_bits as usize + 3) & !7);
            self.last_bitoffset = p & 7;
            let p = p >> 3;
            if p > buf_size {
                return fail(self, Error::Data("frame ran past the packet"));
            }
            let len = buf_size - p;
            if len > MAX_CODED_SUPERFRAME_SIZE {
                return fail(self, Error::Data("len"));
            }
            self.last_superframe_len = len;
            self.last_superframe[..len].copy_from_slice(&buf[p..p + len]);
        } else if let Err(e) = self.decode_frame(&mut r, &mut out) {
            return fail(self, e);
        }
        Ok(out)
    }

    /// Decodes one packet into interleaved 16-bit samples, the decoder's delay taken off the
    /// start of the stream.
    pub fn decode(&mut self, buf: &[u8]) -> Result<Vec<i16>, Error> {
        let p = self.decode_raw(buf)?;
        Ok(self.interleave(p))
    }

    fn interleave(&mut self, p: Vec<Vec<f32>>) -> Vec<i16> {
        let n = p.first().map_or(0, |c| c.len());
        let drop = self.skip.min(n);
        self.skip -= drop;
        let mut out = Vec::with_capacity((n - drop) * self.channels);
        for i in drop..n {
            for c in &p {
                out.push((c[i] * 32768.0).round().clamp(-32768.0, 32767.0) as i16);
            }
        }
        out
    }

    /// The last frame's overlap (the end of the stream).
    pub fn drain(&mut self) -> Vec<i16> {
        let p: Vec<Vec<f32>> = (0..self.channels).map(|ch| self.frame_out[ch][..self.frame_len].to_vec()).collect();
        self.last_superframe_len = 0;
        self.interleave(p)
    }

    /// A seek: the bit reservoir and the overlap are dropped.
    pub fn flush(&mut self) {
        self.last_bitoffset = 0;
        self.last_superframe_len = 0;
        self.skip = self.frame_len * 2;
        for f in &mut self.frame_out {
            f.iter_mut().for_each(|v| *v = 0.0);
        }
    }
}
