//! MPEG-1 video (ISO/IEC 11172-2): pictures from an elementary stream, in display order, as
//! YCbCr 4:2:0 planes. A port of pl_mpeg's plm_video (MIT, Dominic Szablewski), with its
//! integer IDCT, so its output is pl_mpeg's to the bit; fed bytes as they come (`push`), a
//! picture decoded once the start code after it is in (or the stream ended).

use crate::bits::Bits;
use crate::tables::*;

const PICTURE: u8 = 0x00;
const SEQUENCE: u8 = 0xb3;
const EXTENSION: u8 = 0xb5;
const USER_DATA: u8 = 0xb2;
const GOP: u8 = 0xb8;
const SEQUENCE_END: u8 = 0xb7;

const TYPE_I: u32 = 1;
const TYPE_P: u32 = 2;
const TYPE_B: u32 = 3;

/// A decoded picture: planes with whole macroblocks (`y_stride` x 16-aligned height; chroma
/// half of each), of which `width` x `height` is the picture.
#[derive(Clone, Default)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub y_stride: usize,
    pub c_stride: usize,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
}

/// The sequence header's facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub width: usize,
    pub height: usize,
    /// pictures a second (0: unknown)
    pub rate: f64,
}

#[derive(Default, Clone, Copy)]
struct Motion {
    full_px: bool,
    is_set: bool,
    r_size: u32,
    h: i32,
    v: i32,
}

pub struct Video {
    es: Vec<u8>,
    /// where the next start code is looked for in `es`
    at: usize,
    ended: bool,
    header: Option<Header>,
    mb_width: i32,
    mb_height: i32,
    mb_size: i32,
    luma_width: usize,
    chroma_width: usize,
    intra_quant: [u8; 64],
    non_intra_quant: [u8; 64],
    /// the three picture buffers; current / forward / backward are indices into it
    frames: Vec<Frame>,
    cur: usize,
    fwd: usize,
    bwd: usize,
    picture_type: u32,
    motion_fwd: Motion,
    motion_bwd: Motion,
    quantizer_scale: i32,
    slice_begin: bool,
    mb_address: i32,
    mb_row: i32,
    mb_col: i32,
    mb_type: u32,
    mb_intra: bool,
    dc_predictor: [i32; 3],
    block: [i32; 64],
    has_reference: bool,
    /// pictures handed out
    pub pictures: u64,
}

impl Default for Video {
    fn default() -> Self {
        Self::new()
    }
}

impl Video {
    pub fn new() -> Self {
        Video {
            es: Vec::new(),
            at: 0,
            ended: false,
            header: None,
            mb_width: 0,
            mb_height: 0,
            mb_size: 0,
            luma_width: 0,
            chroma_width: 0,
            intra_quant: INTRA_QUANT,
            non_intra_quant: [16; 64],
            frames: Vec::new(),
            cur: 0,
            fwd: 1,
            bwd: 2,
            picture_type: 0,
            motion_fwd: Motion::default(),
            motion_bwd: Motion::default(),
            quantizer_scale: 0,
            slice_begin: false,
            mb_address: 0,
            mb_row: 0,
            mb_col: 0,
            mb_type: 0,
            mb_intra: false,
            dc_predictor: [128; 3],
            block: [0; 64],
            has_reference: false,
            pictures: 0,
        }
    }

    pub fn header(&self) -> Option<Header> {
        self.header
    }

    /// More of the stream.
    pub fn push(&mut self, d: &[u8]) {
        // (what was decoded already goes once it is a good part of the buffer)
        if self.at > 1 << 20 && self.at * 2 > self.es.len() {
            self.es.drain(..self.at);
            self.at = 0;
        }
        self.es.extend_from_slice(d);
    }

    /// No more of the stream: the last picture decodes without a start code after it.
    pub fn end(&mut self) {
        self.ended = true;
    }

    /// Back to the start of a new stream (a seek): the next picture must be an intra one.
    pub fn reset(&mut self) {
        self.es.clear();
        self.at = 0;
        self.ended = false;
        self.has_reference = false;
    }

    /// The start code at or after `from` in the buffer: (its position, its value).
    fn find(&self, from: usize) -> Option<(usize, u8)> {
        let d = &self.es;
        let mut i = from;
        while i + 3 < d.len() {
            if d[i + 2] > 1 {
                i += 3;
            } else if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
                return Some((i, d[i + 3]));
            } else {
                i += 1;
            }
        }
        None
    }

    /// The next picture in display order, if one is complete.
    pub fn decode(&mut self) -> Option<&Frame> {
        loop {
            let Some((p, code)) = self.find(self.at) else {
                // the last reference picture is still to show at the end
                if self.ended && self.has_reference && self.header.is_some() {
                    self.has_reference = false;
                    self.at = self.es.len();
                    self.pictures += 1;
                    return Some(&self.frames[self.bwd]);
                }
                return None;
            };
            match code {
                SEQUENCE => {
                    // (64 bits + two matrices at most)
                    if self.es.len() < p + 4 + 140 && !self.ended {
                        return None;
                    }
                    let end = (p + 4 + 140).min(self.es.len());
                    let d = self.es[p + 4..end].to_vec();
                    self.sequence_header(&d);
                    self.at = p + 4;
                }
                PICTURE if self.header.is_some() => {
                    // the whole picture: up to the next picture / sequence / GOP / end code
                    let mut q = p + 4;
                    let next = loop {
                        match self.find(q) {
                            Some((n, c)) if matches!(c, PICTURE | SEQUENCE | GOP | SEQUENCE_END) => break Some(n),
                            Some((n, _)) => q = n + 3,
                            None => break None,
                        }
                    };
                    let end = match next {
                        Some(n) => n,
                        None if self.ended => self.es.len(),
                        None => return None,
                    };
                    // (with a start code and room after it, as pl_mpeg reads the stream on)
                    let mut pic = self.es[p + 4..end].to_vec();
                    pic.extend_from_slice(&[0, 0, 1, SEQUENCE_END, 0, 0, 0, 0]);
                    self.at = end;
                    self.picture(&pic);
                    let shown = if self.picture_type == TYPE_B {
                        Some(self.cur)
                    } else if matches!(self.picture_type, TYPE_I | TYPE_P) {
                        if self.has_reference {
                            Some(self.fwd)
                        } else {
                            self.has_reference = true;
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(f) = shown {
                        self.pictures += 1;
                        return Some(&self.frames[f]);
                    }
                }
                _ => self.at = p + 4,
            }
        }
    }

    fn sequence_header(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        let width = b.read(12) as usize;
        let height = b.read(12) as usize;
        if width == 0 || height == 0 {
            return;
        }
        b.skip(4); // pixel aspect ratio
        let rate = PICTURE_RATE[b.read(4) as usize];
        b.skip(18 + 1 + 10 + 1);
        let mut intra = INTRA_QUANT;
        if b.read(1) != 0 {
            for &z in ZIG_ZAG.iter() {
                intra[z as usize] = b.read(8) as u8;
            }
        }
        let mut non_intra = [16u8; 64];
        if b.read(1) != 0 {
            for &z in ZIG_ZAG.iter() {
                non_intra[z as usize] = b.read(8) as u8;
            }
        }
        self.intra_quant = intra;
        self.non_intra_quant = non_intra;
        // (a repeated header of the same size keeps the pictures)
        if self.header.is_some_and(|h| h.width == width && h.height == height) {
            self.header = Some(Header { width, height, rate });
            return;
        }
        self.header = Some(Header { width, height, rate });
        self.mb_width = width.div_ceil(16) as i32;
        self.mb_height = height.div_ceil(16) as i32;
        self.mb_size = self.mb_width * self.mb_height;
        self.luma_width = self.mb_width as usize * 16;
        self.chroma_width = self.mb_width as usize * 8;
        let luma = self.luma_width * self.mb_height as usize * 16;
        let chroma = self.chroma_width * self.mb_height as usize * 8;
        // (a row and a pixel more, as pl_mpeg's frames sit one after another in memory: a
        // half-pixel motion vector at the bottom edge reads past its plane)
        let frame = Frame {
            width,
            height,
            y_stride: self.luma_width,
            c_stride: self.chroma_width,
            y: vec![0; luma + self.luma_width + 1],
            cb: vec![0; chroma + self.chroma_width + 1],
            cr: vec![0; chroma + self.chroma_width + 1],
        };
        self.frames = vec![frame.clone(), frame.clone(), frame];
        self.cur = 0;
        self.fwd = 1;
        self.bwd = 2;
        self.has_reference = false;
    }

    fn picture(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        b.skip(10); // temporal reference
        self.picture_type = b.read(3);
        b.skip(16); // vbv delay
        if self.picture_type == 0 || self.picture_type > TYPE_B {
            return;
        }
        if matches!(self.picture_type, TYPE_P | TYPE_B) {
            self.motion_fwd.full_px = b.read(1) != 0;
            let f_code = b.read(3);
            if f_code == 0 {
                self.picture_type = 0;
                return;
            }
            self.motion_fwd.r_size = f_code - 1;
        }
        if self.picture_type == TYPE_B {
            self.motion_bwd.full_px = b.read(1) != 0;
            let f_code = b.read(3);
            if f_code == 0 {
                self.picture_type = 0;
                return;
            }
            self.motion_bwd.r_size = f_code - 1;
        }
        let temp = self.fwd;
        if matches!(self.picture_type, TYPE_I | TYPE_P) {
            self.fwd = self.bwd;
        }
        let mut code = loop {
            match b.next_start_code() {
                Some(EXTENSION) | Some(USER_DATA) => continue,
                c => break c,
            }
        };
        while let Some(c @ 0x01..=0xaf) = code {
            self.slice(&mut b, c as i32);
            if self.mb_address >= self.mb_size - 1 {
                break;
            }
            code = b.next_start_code();
        }
        if matches!(self.picture_type, TYPE_I | TYPE_P) {
            self.bwd = self.cur;
            self.cur = temp;
        }
    }

    fn slice(&mut self, b: &mut Bits, slice: i32) {
        self.slice_begin = true;
        self.mb_address = (slice - 1) * self.mb_width - 1;
        self.motion_bwd.h = 0;
        self.motion_fwd.h = 0;
        self.motion_bwd.v = 0;
        self.motion_fwd.v = 0;
        self.dc_predictor = [128; 3];
        self.quantizer_scale = b.read(5) as i32;
        while b.read(1) != 0 {
            b.skip(8);
        }
        loop {
            self.macroblock(b);
            if !(self.mb_address < self.mb_size - 1 && b.peek_non_zero(23)) {
                break;
            }
        }
    }

    fn macroblock(&mut self, b: &mut Bits) {
        let mut increment = 0i32;
        let mut t = b.vlc(MB_ADDRESS_INCREMENT) as i32;
        while t == 34 {
            t = b.vlc(MB_ADDRESS_INCREMENT) as i32;
        }
        while t == 35 {
            increment += 33;
            t = b.vlc(MB_ADDRESS_INCREMENT) as i32;
        }
        increment += t;
        if self.slice_begin {
            self.slice_begin = false;
            self.mb_address += increment;
        } else {
            if self.mb_address + increment >= self.mb_size {
                return;
            }
            if increment > 1 {
                self.dc_predictor = [128; 3];
                if self.picture_type == TYPE_P {
                    self.motion_fwd.h = 0;
                    self.motion_fwd.v = 0;
                }
            }
            while increment > 1 {
                self.mb_address += 1;
                self.mb_row = self.mb_address / self.mb_width;
                self.mb_col = self.mb_address % self.mb_width;
                self.predict_macroblock();
                increment -= 1;
            }
            self.mb_address += 1;
        }
        if self.mb_address < 0 {
            return;
        }
        self.mb_row = self.mb_address / self.mb_width;
        self.mb_col = self.mb_address % self.mb_width;
        if self.mb_col >= self.mb_width || self.mb_row >= self.mb_height {
            return;
        }
        let table = match self.picture_type {
            TYPE_I => MB_TYPE_I,
            TYPE_P => MB_TYPE_P,
            _ => MB_TYPE_B,
        };
        self.mb_type = b.vlc(table) as u32;
        self.mb_intra = self.mb_type & 1 != 0;
        self.motion_fwd.is_set = self.mb_type & 8 != 0;
        self.motion_bwd.is_set = self.mb_type & 4 != 0;
        if self.mb_type & 0x10 != 0 {
            self.quantizer_scale = b.read(5) as i32;
        }
        if self.mb_intra {
            self.motion_bwd.h = 0;
            self.motion_fwd.h = 0;
            self.motion_bwd.v = 0;
            self.motion_fwd.v = 0;
        } else {
            self.dc_predictor = [128; 3];
            self.motion_vectors(b);
            self.predict_macroblock();
        }
        let cbp = if self.mb_type & 2 != 0 {
            b.vlc(CODED_BLOCK_PATTERN) as u32
        } else if self.mb_intra {
            0x3f
        } else {
            0
        };
        for block in 0..6 {
            if cbp & (0x20 >> block) != 0 {
                self.decode_block(b, block);
            }
        }
    }

    fn motion_vectors(&mut self, b: &mut Bits) {
        if self.motion_fwd.is_set {
            let r = self.motion_fwd.r_size;
            self.motion_fwd.h = motion_vector(b, r, self.motion_fwd.h);
            self.motion_fwd.v = motion_vector(b, r, self.motion_fwd.v);
        } else if self.picture_type == TYPE_P {
            self.motion_fwd.h = 0;
            self.motion_fwd.v = 0;
        }
        if self.motion_bwd.is_set {
            let r = self.motion_bwd.r_size;
            self.motion_bwd.h = motion_vector(b, r, self.motion_bwd.h);
            self.motion_bwd.v = motion_vector(b, r, self.motion_bwd.v);
        }
    }

    fn predict_macroblock(&mut self) {
        let (mut fh, mut fv) = (self.motion_fwd.h, self.motion_fwd.v);
        if self.motion_fwd.full_px {
            fh <<= 1;
            fv <<= 1;
        }
        if self.picture_type == TYPE_B {
            let (mut bh, mut bv) = (self.motion_bwd.h, self.motion_bwd.v);
            if self.motion_bwd.full_px {
                bh <<= 1;
                bv <<= 1;
            }
            if self.motion_fwd.is_set {
                self.copy_macroblock(self.fwd, fh, fv, false);
                if self.motion_bwd.is_set {
                    self.copy_macroblock(self.bwd, bh, bv, true);
                }
            } else {
                self.copy_macroblock(self.bwd, bh, bv, false);
            }
        } else {
            self.copy_macroblock(self.fwd, fh, fv, false);
        }
    }

    fn copy_macroblock(&mut self, s: usize, mh: i32, mv: i32, interp: bool) {
        if s == self.cur || self.frames.len() < 3 {
            return;
        }
        let (src, dst) = two(&mut self.frames, s, self.cur);
        let (row, col, mbw, mbh) = (self.mb_row, self.mb_col, self.mb_width, self.mb_height);
        process(&src.y, &mut dst.y, row, col, mbw, mbh, mh, mv, 16, interp);
        process(&src.cr, &mut dst.cr, row, col, mbw, mbh, mh / 2, mv / 2, 8, interp);
        process(&src.cb, &mut dst.cb, row, col, mbw, mbh, mh / 2, mv / 2, 8, interp);
    }

    fn decode_block(&mut self, b: &mut Bits, block: usize) {
        let mut n = 0usize;
        let quant;
        if self.mb_intra {
            let plane = if block > 3 { block - 3 } else { 0 };
            let predictor = self.dc_predictor[plane];
            let size = b.vlc(if plane == 0 { DCT_SIZE_Y } else { DCT_SIZE_C }) as u32;
            self.block[0] = if size > 0 {
                let diff = b.read(size) as i32;
                if diff & (1 << (size - 1)) != 0 {
                    predictor.wrapping_add(diff)
                } else {
                    predictor.wrapping_add((-(1i32 << size)) | (diff + 1))
                }
            } else {
                predictor
            };
            self.dc_predictor[plane] = self.block[0];
            self.block[0] = self.block[0].wrapping_shl(3 + 5);
            quant = self.intra_quant;
            n = 1;
        } else {
            quant = self.non_intra_quant;
        }
        loop {
            let coeff = b.vlc(DCT_COEFF);
            if coeff == 0x0001 && n > 0 && b.read(1) == 0 {
                break; // end of block
            }
            let (run, mut level);
            if coeff == 0xffff {
                run = b.read(6) as usize;
                level = b.read(8) as i32;
                if level == 0 {
                    level = b.read(8) as i32;
                } else if level == 128 {
                    level = b.read(8) as i32 - 256;
                } else if level > 128 {
                    level -= 256;
                }
            } else {
                run = (coeff >> 8) as usize;
                level = (coeff & 0xff) as i32;
                if b.read(1) != 0 {
                    level = -level;
                }
            }
            n += run;
            if n >= 64 {
                return;
            }
            let z = ZIG_ZAG[n] as usize;
            n += 1;
            level <<= 1;
            if !self.mb_intra {
                level += if level < 0 { -1 } else { 1 };
            }
            level = (level * self.quantizer_scale * quant[z] as i32) >> 4;
            if level & 1 == 0 {
                level -= if level > 0 { 1 } else { -1 };
            }
            level = level.clamp(-2048, 2047);
            self.block[z] = level * PREMULTIPLIER[z] as i32;
        }
        let (row, col) = (self.mb_row as usize, self.mb_col as usize);
        let lw = self.luma_width;
        let cw = self.chroma_width;
        let cur = self.cur;
        let f = &mut self.frames[cur];
        let (d, dw, mut di) = if block < 4 {
            let mut di = (row * lw + col) << 4;
            if block & 1 != 0 {
                di += 8;
            }
            if block & 2 != 0 {
                di += lw << 3;
            }
            (&mut f.y, lw, di)
        } else {
            (if block == 4 { &mut f.cb } else { &mut f.cr }, cw, ((row * lw) << 2) + (col << 3))
        };
        if di + 7 * dw + 8 > d.len() {
            self.block = [0; 64];
            return;
        }
        let s = &mut self.block;
        if n == 1 {
            let v = (s[0].wrapping_add(128)) >> 8;
            for _ in 0..8 {
                for x in 0..8 {
                    d[di + x] = if self.mb_intra { clamp(v) } else { clamp(d[di + x] as i32 + v) };
                }
                di += dw;
            }
            s[0] = 0;
        } else {
            idct_put(s, d, di, dw, self.mb_intra);
        }
    }
}

fn two(f: &mut [Frame], s: usize, d: usize) -> (&Frame, &mut Frame) {
    if s < d {
        let (a, b) = f.split_at_mut(d);
        (&a[s], &mut b[0])
    } else {
        let (a, b) = f.split_at_mut(s);
        (&b[0], &mut a[d])
    }
}

fn clamp(n: i32) -> u8 {
    n.clamp(0, 255) as u8
}

fn motion_vector(b: &mut Bits, r_size: u32, motion: i32) -> i32 {
    let fscale = 1i32 << r_size;
    let m_code = b.vlc(MOTION) as i16 as i32;
    let d = if m_code != 0 && fscale != 1 {
        let r = b.read(r_size) as i32;
        let d = ((m_code.abs() - 1) << r_size) + r + 1;
        if m_code < 0 {
            -d
        } else {
            d
        }
    } else {
        m_code
    };
    let mut m = motion + d;
    if m > (fscale << 4) - 1 {
        m -= fscale << 5;
    } else if m < -(fscale << 4) {
        m += fscale << 5;
    }
    m
}

#[allow(clippy::too_many_arguments)]
fn process(s: &[u8], d: &mut [u8], row: i32, col: i32, mb_width: i32, mb_height: i32, mh: i32, mv: i32, bs: i32, interp: bool) {
    let dw = mb_width * bs;
    let hp = mh >> 1;
    let vp = mv >> 1;
    let odd_h = mh & 1 == 1;
    let odd_v = mv & 1 == 1;
    let si = ((row * bs) + vp) as i64 * dw as i64 + (col * bs) as i64 + hp as i64;
    let di = ((row * dw + col) * bs) as i64;
    let max = dw as i64 * (mb_height * bs - bs + 1) as i64 - bs as i64;
    // (pl_mpeg compares unsigned: a negative address is past the maximum)
    if si < 0 || di < 0 || si > max || di > max {
        return;
    }
    let (dw, bs) = (dw as usize, bs as usize);
    let (si, di) = (si as usize, di as usize);
    if si + (bs - 1 + odd_v as usize) * dw + bs + odd_h as usize > s.len() || di + (bs - 1) * dw + bs > d.len() {
        return;
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    return unsafe { crate::simd::process_rows(s, d, si, di, dw, bs, odd_h, odd_v, interp) };
    #[allow(unreachable_code)]
    process_rows(s, d, si, di, dw, bs, odd_h, odd_v, interp)
}

/// process's loop (the scalar code crate::simd's is held to).
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn process_rows(s: &[u8], d: &mut [u8], mut si: usize, mut di: usize, dw: usize, bs: usize, odd_h: bool, odd_v: bool, interp: bool) {
    for _ in 0..bs {
        for x in 0..bs {
            let p = match (odd_h, odd_v) {
                (false, false) => s[si + x] as u32,
                (false, true) => (s[si + x] as u32 + s[si + x + dw] as u32 + 1) >> 1,
                (true, false) => (s[si + x] as u32 + s[si + x + 1] as u32 + 1) >> 1,
                (true, true) => (s[si + x] as u32 + s[si + x + 1] as u32 + s[si + x + dw] as u32 + s[si + x + dw + 1] as u32 + 2) >> 2,
            };
            d[di + x] = if interp { ((d[di + x] as u32 + p + 1) >> 1) as u8 } else { p as u8 };
        }
        si += dw;
        di += dw;
    }
}

/// pl_mpeg's integer IDCT (in place; rows of 8), with wrapping arithmetic so corrupt data
/// cannot overflow.
pub fn idct(b: &mut [i32; 64]) {
    #[inline]
    fn m(a: i32, k: i32) -> i32 {
        a.wrapping_mul(k).wrapping_add(128) >> 8
    }
    for i in 0..8 {
        let b1 = b[4 * 8 + i];
        let b3 = b[2 * 8 + i].wrapping_add(b[6 * 8 + i]);
        let b4 = b[5 * 8 + i].wrapping_sub(b[3 * 8 + i]);
        let tmp1 = b[8 + i].wrapping_add(b[7 * 8 + i]);
        let tmp2 = b[3 * 8 + i].wrapping_add(b[5 * 8 + i]);
        let b6 = b[8 + i].wrapping_sub(b[7 * 8 + i]);
        let b7 = tmp1.wrapping_add(tmp2);
        let m0 = b[i];
        let x4 = (b6.wrapping_mul(473).wrapping_sub(b4.wrapping_mul(196)).wrapping_add(128) >> 8).wrapping_sub(b7);
        let x0 = x4.wrapping_sub(m(tmp1.wrapping_sub(tmp2), 362));
        let x1 = m0.wrapping_sub(b1);
        let x2 = m(b[2 * 8 + i].wrapping_sub(b[6 * 8 + i]), 362).wrapping_sub(b3);
        let x3 = m0.wrapping_add(b1);
        let y3 = x1.wrapping_add(x2);
        let y4 = x3.wrapping_add(b3);
        let y5 = x1.wrapping_sub(x2);
        let y6 = x3.wrapping_sub(b3);
        let y7 = x0.wrapping_neg().wrapping_sub(b4.wrapping_mul(473).wrapping_add(b6.wrapping_mul(196)).wrapping_add(128) >> 8);
        b[i] = b7.wrapping_add(y4);
        b[8 + i] = x4.wrapping_add(y3);
        b[2 * 8 + i] = y5.wrapping_sub(x0);
        b[3 * 8 + i] = y6.wrapping_sub(y7);
        b[4 * 8 + i] = y6.wrapping_add(y7);
        b[5 * 8 + i] = x0.wrapping_add(y5);
        b[6 * 8 + i] = y3.wrapping_sub(x4);
        b[7 * 8 + i] = y4.wrapping_sub(b7);
    }
    for i in (0..64).step_by(8) {
        let b1 = b[4 + i];
        let b3 = b[2 + i].wrapping_add(b[6 + i]);
        let b4 = b[5 + i].wrapping_sub(b[3 + i]);
        let tmp1 = b[1 + i].wrapping_add(b[7 + i]);
        let tmp2 = b[3 + i].wrapping_add(b[5 + i]);
        let b6 = b[1 + i].wrapping_sub(b[7 + i]);
        let b7 = tmp1.wrapping_add(tmp2);
        let m0 = b[i];
        let x4 = (b6.wrapping_mul(473).wrapping_sub(b4.wrapping_mul(196)).wrapping_add(128) >> 8).wrapping_sub(b7);
        let x0 = x4.wrapping_sub(m(tmp1.wrapping_sub(tmp2), 362));
        let x1 = m0.wrapping_sub(b1);
        let x2 = m(b[2 + i].wrapping_sub(b[6 + i]), 362).wrapping_sub(b3);
        let x3 = m0.wrapping_add(b1);
        let y3 = x1.wrapping_add(x2);
        let y4 = x3.wrapping_add(b3);
        let y5 = x1.wrapping_sub(x2);
        let y6 = x3.wrapping_sub(b3);
        let y7 = x0.wrapping_neg().wrapping_sub(b4.wrapping_mul(473).wrapping_add(b6.wrapping_mul(196)).wrapping_add(128) >> 8);
        let r = |v: i32| v.wrapping_add(128) >> 8;
        b[i] = r(b7.wrapping_add(y4));
        b[1 + i] = r(x4.wrapping_add(y3));
        b[2 + i] = r(y5.wrapping_sub(x0));
        b[3 + i] = r(y6.wrapping_sub(y7));
        b[4 + i] = r(y6.wrapping_add(y7));
        b[5 + i] = r(x0.wrapping_add(y5));
        b[6 + i] = r(y3.wrapping_sub(x4));
        b[7 + i] = r(y4.wrapping_sub(b7));
    }
}

/// [`idct`] of `s`, then added to (intra: written to) `d` at `di`, rows `dw` apart, clamped
/// to bytes; `s` is left zero. `d` holds the 8 rows.
#[doc(hidden)]
pub fn idct_put(s: &mut [i32; 64], d: &mut [u8], di: usize, dw: usize, intra: bool) {
    assert!(di + 7 * dw + 8 <= d.len());
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    return unsafe { crate::simd::idct_put(s, d, di, dw, intra) };
    #[allow(unreachable_code)]
    idct_put_scalar(s, d, di, dw, intra)
}

/// idct_put's scalar code (crate::simd's is held to it).
#[doc(hidden)]
pub fn idct_put_scalar(s: &mut [i32; 64], d: &mut [u8], mut di: usize, dw: usize, intra: bool) {
    idct(s);
    for y in 0..8 {
        for x in 0..8 {
            let v = s[y * 8 + x];
            d[di + x] = if intra { clamp(v) } else { clamp(d[di + x] as i32 + v) };
        }
        di += dw;
    }
    *s = [0; 64];
}

/// A picture as 32-bit BGRX pixels (DirectShow's RGB32), BT.601 as pl_mpeg converts:
/// `stride` bytes a row, rows from the bottom when `bottom_up` (a DIB with a positive height).
pub fn to_bgrx(f: &Frame, out: &mut [u8], stride: usize, bottom_up: bool) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        let (w, h) = (f.width & !1, f.height & !1);
        assert!(f.y.len() >= h * f.y_stride && f.cb.len() >= h / 2 * f.c_stride && f.cr.len() >= h / 2 * f.c_stride && f.c_stride >= w / 2 && f.y_stride >= w);
        for row in 0..h / 2 {
            let mut col = 0;
            while col + 8 <= w / 2 && unsafe { crate::simd::bgrx16(f, out, stride, bottom_up, h, row, col) } {
                col += 8;
            }
            bgrx_pairs(f, out, stride, bottom_up, row, col);
        }
        return;
    }
    #[allow(unreachable_code)]
    to_bgrx_scalar(f, out, stride, bottom_up)
}

/// to_bgrx's scalar code (crate::simd's is held to it).
#[doc(hidden)]
pub fn to_bgrx_scalar(f: &Frame, out: &mut [u8], stride: usize, bottom_up: bool) {
    for row in 0..(f.height & !1) / 2 {
        bgrx_pairs(f, out, stride, bottom_up, row, 0);
    }
}

/// to_bgrx of rows 2 row, 2 row + 1 from chroma column `from` on.
fn bgrx_pairs(f: &Frame, out: &mut [u8], stride: usize, bottom_up: bool, row: usize, from: usize) {
    let (w, h) = (f.width & !1, f.height & !1);
    for col in from..w / 2 {
        let ci = row * f.c_stride + col;
        let cr = f.cr[ci] as i32 - 128;
        let cb = f.cb[ci] as i32 - 128;
        let r = (cr * 104597) >> 16;
        let g = (cb * 25674 + cr * 53278) >> 16;
        let b = (cb * 132201) >> 16;
        for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let (x, y) = (col * 2 + dx, row * 2 + dy);
            let yv = ((f.y[y * f.y_stride + x] as i32 - 16) * 76309) >> 16;
            let oy = if bottom_up { h - 1 - y } else { y };
            let o = oy * stride + x * 4;
            if o + 4 <= out.len() {
                out[o] = clamp(yv + b);
                out[o + 1] = clamp(yv - g);
                out[o + 2] = clamp(yv + r);
                out[o + 3] = 0xff;
            }
        }
    }
}
