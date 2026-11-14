//! MS-MPEG4 v3, WMV 7 (WMV1) and WMV 8 (WMV2) video: FFmpeg's msmpeg4dec.c, msmpeg4.c,
//! wmv2dec.c, wmv2.c and the parts of h263dec.c / mpegvideo*.c they use, ported bit for bit
//! (with WMV2's IntraX8 pictures, crate::intrax8).

use crate::bits::Bits;
use crate::hpel;
use crate::idct;
use crate::intrax8::{IntraX8, Plane};
use crate::msmpeg4_tables as t;
use crate::vlc::Vlc;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    /// MP43 / DIV3
    V3,
    /// WMV1
    Wmv1,
    /// WMV2
    Wmv2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Params(&'static str),
    Data(&'static str),
    /// a kind of picture that is not ported (WMV3 sprites, Advanced profile)
    Unsupported(&'static str),
}

/// A decoded picture, YUV 4:2:0, planes of `stride` (`stride[1]` for chroma) bytes per row;
/// the visible part is `width` x `height`.
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub stride: [usize; 2],
    pub planes: [Vec<u8>; 3],
    pub key: bool,
}

impl Picture {
    /// BT.601 limited range to B G R A, top row first (what Windows' WMV decoder DMO gives as
    /// RGB).
    pub fn bgra(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut out = vec![0u8; w * h * 4];
        for (y, o) in out.chunks_exact_mut(w * 4).enumerate() {
            let (yr, ur, vr) = (&self.planes[0][y * self.stride[0]..][..w], &self.planes[1][(y / 2) * self.stride[1]..][..w.div_ceil(2)], &self.planes[2][(y / 2) * self.stride[1]..][..w.div_ceil(2)]);
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            let done = crate::simd::bgra_row(yr, ur, vr, o);
            #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
            let done = 0;
            bgra_row_c(&yr[done..], &ur[done / 2..], &vr[done / 2..], &mut o[done * 4..]);
        }
        out
    }
}

/// One row of Picture::bgra (`y` of an even column first)
pub(crate) fn bgra_row_c(y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) {
    for (x, (&l, o)) in y.iter().zip(out.chunks_exact_mut(4)).enumerate() {
        let c = (l as i32 - 16) * 298;
        let d = u[x / 2] as i32 - 128;
        let e = v[x / 2] as i32 - 128;
        o[0] = ((c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
        o[1] = ((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
        o[2] = ((c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
        o[3] = 255;
    }
}

// ---------------------------------------------------------------------------------------
// tables

const MAX_RUN: usize = 64;
const MAX_LEVEL: usize = 64;
const MV_ESC: i32 = 1099;
const DC_MAX: i32 = 119;

struct Rl {
    vlc: Vlc,
    n: usize,
    last: usize,
    run: &'static [i8],
    level: &'static [i8],
    max_level: [[i32; MAX_RUN + 1]; 2],
    max_run: [[i32; MAX_LEVEL + 1]; 2],
}

impl Rl {
    fn new(n: usize, last: usize, vlc: &[u16], run: &'static [i8], level: &'static [i8]) -> Rl {
        let codes: Vec<(u32, u8, i32)> = (0..=n).map(|i| (vlc[2 * i] as u32, vlc[2 * i + 1] as u8, i as i32)).collect();
        let mut max_level = [[0; MAX_RUN + 1]; 2];
        let mut max_run = [[0; MAX_LEVEL + 1]; 2];
        for l in 0..2 {
            let (s, e) = if l == 0 { (0, last) } else { (last, n) };
            for i in s..e {
                let (r, v) = (run[i] as usize, level[i] as usize);
                max_level[l][r] = max_level[l][r].max(v as i32);
                max_run[l][v] = max_run[l][v].max(r as i32);
            }
        }
        Rl { vlc: Vlc::new(9, &codes), n, last, run, level, max_level, max_run }
    }
    /// GET_RL_VLC on a table of (qmul, qadd): (level, run); level 0 = escape.
    #[inline]
    fn get(&self, r: &mut Bits, qmul: i32, qadd: i32) -> (i32, i32) {
        let s = self.vlc.read(r);
        if s < 0 {
            (MAX_LEVEL as i32, 66)
        } else if s as usize == self.n {
            (0, 66)
        } else {
            let s = s as usize;
            let run = self.run[s] as i32 + 1 + if s >= self.last { 192 } else { 0 };
            (self.level[s] as i32 * qmul + qadd, run)
        }
    }
}

struct Mv {
    vlc: Vlc,
    x: &'static [u8],
    y: &'static [u8],
}

struct Tables {
    rl: [Rl; 6],
    mv: [Mv; 2],
    dc: [[Vlc; 2]; 2],
    mb_i: Vlc,
    non_intra: [Vlc; 4],
    inter_intra: Vlc,
    scan: [[u8; 64]; 4],
    zigzag: [[u8; 64]; 3],
    abt: [[u8; 64]; 2],
}

fn pairs32(tab: &[u32], n: usize) -> Vec<(u32, u8, i32)> {
    (0..n).map(|i| (tab[2 * i], tab[2 * i + 1] as u8, i as i32)).collect()
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mv = |code: &[u16], bits: &[u8], x: &'static [u8], y: &'static [u8]| {
            let c: Vec<(u32, u8, i32)> = (0..=1099).map(|i| (code[i] as u32, bits[i], i as i32)).collect();
            Mv { vlc: Vlc::new(9, &c), x, y }
        };
        let dc = |k: usize| Vlc::new(9, &pairs32(&t::MSMP4_DC_TABLES[k * 240..k * 240 + 240], 120));
        let scan = |k: usize| <[u8; 64]>::try_from(&t::WMV1_SCANTABLE[k * 64..k * 64 + 64]).unwrap();
        let abt = |s: &[u8; 32]| {
            let mut a = [0u8; 64];
            a[..32].copy_from_slice(s);
            a
        };
        let mb_i: Vec<(u32, u8, i32)> = (0..64).map(|i| (t::MSMP4_MB_I_TABLE[2 * i] as u32, t::MSMP4_MB_I_TABLE[2 * i + 1] as u8, i as i32)).collect();
        let ii: Vec<(u32, u8, i32)> = (0..4).map(|i| (t::TABLE_INTER_INTRA[2 * i] as u32, t::TABLE_INTER_INTRA[2 * i + 1], i as i32)).collect();
        Tables {
            rl: [
                Rl::new(132, 85, &t::TABLE0_VLC, &t::TABLE0_RUN, &t::TABLE0_LEVEL),
                Rl::new(185, 119, &t::TABLE2_VLC, &t::TABLE2_RUN, &t::TABLE2_LEVEL),
                Rl::new(102, 67, &t::MPEG4_INTRA_VLC, &t::MPEG4_INTRA_RUN, &t::MPEG4_INTRA_LEVEL),
                Rl::new(148, 81, &t::TABLE1_VLC, &t::TABLE1_RUN, &t::TABLE1_LEVEL),
                Rl::new(168, 99, &t::TABLE4_VLC, &t::TABLE4_RUN, &t::TABLE4_LEVEL),
                Rl::new(102, 58, &t::INTER_VLC, &t::INTER_RUN, &t::INTER_LEVEL),
            ],
            mv: [mv(&t::TABLE0_MV_CODE, &t::TABLE0_MV_BITS, &t::TABLE0_MVX, &t::TABLE0_MVY), mv(&t::TABLE1_MV_CODE, &t::TABLE1_MV_BITS, &t::TABLE1_MVX, &t::TABLE1_MVY)],
            dc: [[dc(0), dc(1)], [dc(2), dc(3)]],
            mb_i: Vlc::new(9, &mb_i),
            non_intra: [
                Vlc::new(9, &pairs32(&t::TABLE_MB_NON_INTRA2, 128)),
                Vlc::new(9, &pairs32(&t::TABLE_MB_NON_INTRA3, 128)),
                Vlc::new(9, &pairs32(&t::TABLE_MB_NON_INTRA4, 128)),
                Vlc::new(9, &pairs32(&t::TABLE_MB_NON_INTRA, 128)),
            ],
            inter_intra: Vlc::new(3, &ii),
            scan: [scan(0), scan(1), scan(2), scan(3)],
            zigzag: [t::ZIGZAG_DIRECT, t::ALTERNATE_HORIZONTAL_SCAN, t::ALTERNATE_VERTICAL_SCAN],
            abt: [abt(&t::WMV2_SCANTABLEA), abt(&t::WMV2_SCANTABLEB)],
        }
    })
}

/// The DC VLC (table `k`, chroma or not) and the I-picture MB VLC, shared with VC-1.
pub(crate) fn dc_vlc(k: usize, chroma: bool) -> &'static Vlc {
    &tables().dc[k & 1][chroma as usize]
}
pub(crate) fn mb_i_vlc() -> &'static Vlc {
    &tables().mb_i
}

#[inline]
fn decode012(r: &mut Bits) -> u32 {
    if r.get1() == 0 { 0 } else { r.get1() + 1 }
}

#[inline]
fn mid_pred(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}

// ---------------------------------------------------------------------------------------
// the decoder

#[derive(Clone)]
struct Pic {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

pub struct Decoder {
    ver: Version,
    width: usize,
    height: usize,
    mb_w: usize,
    mb_h: usize,
    mbs: usize,
    b8: usize,
    ls: usize,
    uvls: usize,
    // kept from picture to picture (MpegEncContext's tables)
    dc_val: Vec<i16>,
    ac_val: Vec<[i16; 16]>,
    coded_block: Vec<u8>,
    mbintra: Vec<u8>,
    cur: Pic,
    x8: Option<IntraX8>,
    last: Option<Pic>,
    // the current picture's
    skip: Vec<bool>,
    motion: Vec<[i16; 2]>,
    key: bool,
    qscale: i32,
    y_dc_scale: i32,
    c_dc_scale: i32,
    rl_table_index: usize,
    rl_chroma_table_index: usize,
    dc_table_index: usize,
    mv_table_index: usize,
    per_mb_rl_table: bool,
    use_skip_mb_code: bool,
    inter_intra_pred: bool,
    no_rounding: bool,
    flipflop_rounding: bool,
    bit_rate: u32,
    slice_height: usize,
    esc3_level_length: u32,
    esc3_run_length: u32,
    // WMV2 (its extradata and picture header)
    mspel_bit: bool,
    loop_filter: bool,
    abt_flag: bool,
    j_type_bit: bool,
    top_left_mv_flag: bool,
    per_mb_rl_bit: bool,
    mspel: bool,
    per_mb_abt: bool,
    abt_type: u32,
    per_block_abt: bool,
    cbp_table_index: usize,
    hshift: usize,
    abt_type_table: [u32; 6],
    // the macroblock's
    mb_x: usize,
    mb_y: usize,
    first_slice_line: bool,
    resync_mb_x: usize,
    resync_mb_y: usize,
    mb_intra: bool,
    ac_pred: bool,
    h263_aic_dir: i32,
    mv: [i32; 2],
    last_index: [i32; 6],
    bi: [usize; 6],
    block: [[i16; 64]; 6],
    abt2: [[i16; 64]; 6],
}

impl Decoder {
    /// `extra`: the stream's extradata (after the BITMAPINFOHEADER; WMV2 needs its 4 bytes).
    pub fn new(ver: Version, width: usize, height: usize, extra: &[u8]) -> Result<Decoder, Error> {
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(Error::Params("picture size"));
        }
        let mb_w = width.div_ceil(16);
        let mb_h = height.div_ceil(16);
        let mbs = mb_w + 1;
        let b8 = mb_w * 2 + 1;
        let y_size = b8 * (2 * mb_h + 1);
        let c_size = mbs * (mb_h + 1);
        let yc = y_size + 2 * c_size;
        let (ls, uvls) = (mb_w * 16, mb_w * 8);
        let pic = Pic { y: vec![0; ls * mb_h * 16], u: vec![0; uvls * mb_h * 8], v: vec![0; uvls * mb_h * 8] };
        let mut d = Decoder {
            ver, width, height, mb_w, mb_h, mbs, b8, ls, uvls,
            dc_val: vec![1024; yc],
            ac_val: vec![[0; 16]; yc],
            coded_block: vec![0; y_size],
            mbintra: vec![1; mb_h * mbs],
            cur: pic, x8: None,
            last: None,
            skip: vec![false; mb_h * mbs],
            motion: vec![[0; 2]; b8 * (2 * mb_h + 2) + 8],
            key: false,
            qscale: 1, y_dc_scale: 8, c_dc_scale: 8,
            rl_table_index: 0, rl_chroma_table_index: 0, dc_table_index: 0, mv_table_index: 0,
            per_mb_rl_table: false, use_skip_mb_code: false, inter_intra_pred: false, no_rounding: false,
            flipflop_rounding: false, bit_rate: 0, slice_height: mb_h, esc3_level_length: 0, esc3_run_length: 0,
            mspel_bit: false, loop_filter: false, abt_flag: false, j_type_bit: false, top_left_mv_flag: false,
            per_mb_rl_bit: false, mspel: false, per_mb_abt: false, abt_type: 0, per_block_abt: false,
            cbp_table_index: 0, hshift: 0, abt_type_table: [0; 6],
            mb_x: 0, mb_y: 0, first_slice_line: true, resync_mb_x: 0, resync_mb_y: 0, mb_intra: false,
            ac_pred: false, h263_aic_dir: 0, mv: [0; 2], last_index: [-1; 6], bi: [0; 6],
            block: [[0; 64]; 6], abt2: [[0; 64]; 6],
        };
        if ver == Version::Wmv2 {
            // decode_ext_header
            if extra.len() < 4 {
                return Err(Error::Params("WMV2 extradata"));
            }
            let mut r = Bits::with_len(extra, 32);
            let _fps = r.get(5);
            d.bit_rate = r.get(11) * 1024;
            d.mspel_bit = r.get1() != 0;
            d.loop_filter = r.get1() != 0;
            d.abt_flag = r.get1() != 0;
            d.j_type_bit = r.get1() != 0;
            d.top_left_mv_flag = r.get1() != 0;
            d.per_mb_rl_bit = r.get1() != 0;
            let code = r.get(3) as usize;
            if code == 0 {
                return Err(Error::Params("WMV2 slices"));
            }
            d.slice_height = mb_h / code;
        }
        tables();
        Ok(d)
    }

    /// The visible picture size.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn set_qscale(&mut self, q: i32) {
        let q = q.clamp(1, 31);
        self.qscale = q;
        let (y, c): (&[u8; 32], &[u8; 32]) = match self.ver {
            Version::V3 => (&t::OLD_Y_DC_SCALE_TABLE, &t::WMV1_C_DC_SCALE_TABLE),
            _ => (&t::WMV1_Y_DC_SCALE_TABLE, &t::WMV1_C_DC_SCALE_TABLE),
        };
        self.y_dc_scale = y[q as usize] as i32;
        self.c_dc_scale = c[q as usize] as i32;
    }

    // flat indices of the dc_val / ac_val tables (dc_val[0] + block_index[n])
    #[inline]
    fn dci(&self, n: usize) -> usize {
        self.b8 + 1 + self.bi[n]
    }
    #[inline]
    fn wrap(&self, n: usize) -> usize {
        if n < 4 { self.b8 } else { self.mbs }
    }
    /// motion_val index of block_index[0]
    #[inline]
    fn mvi(&self) -> usize {
        self.b8 + 4 + self.bi[0]
    }

    fn init_block_index(&mut self) {
        let (b8, mbs, mb_h) = (self.b8, self.mbs, self.mb_h);
        let (x, y) = (self.mb_x, self.mb_y);
        self.bi = [
            b8 * (2 * y) + 2 * x,
            b8 * (2 * y) + 1 + 2 * x,
            b8 * (2 * y + 1) + 2 * x,
            b8 * (2 * y + 1) + 1 + 2 * x,
            mbs * (y + 1) + b8 * mb_h * 2 + x,
            mbs * (y + mb_h + 2) + b8 * mb_h * 2 + x,
        ];
    }
}

enum Header {
    Ok,
    /// a WMV2 picture that repeats the last one
    Skipped,
}

impl Decoder {
    /// ff_msmpeg4_decode_picture_header (v3 / WMV1)
    fn msmpeg4_header(&mut self, r: &mut Bits) -> Result<Header, Error> {
        if r.left() * 8 < (self.mb_w * self.mb_h) as isize {
            return Err(Error::Data("picture too small"));
        }
        let ty = r.get(2) + 1;
        if ty != 1 && ty != 2 {
            return Err(Error::Data("picture type"));
        }
        self.key = ty == 1;
        let q = r.get(5) as i32;
        if q == 0 {
            return Err(Error::Data("qscale"));
        }
        self.qscale = q;
        if self.key {
            let code = r.get(5) as usize;
            if code < 0x17 {
                return Err(Error::Data("slice code"));
            }
            self.slice_height = self.mb_h / (code - 0x16);
            match self.ver {
                Version::V3 => {
                    self.rl_chroma_table_index = decode012(r) as usize;
                    self.rl_table_index = decode012(r) as usize;
                    self.dc_table_index = r.get1() as usize;
                }
                _ => {
                    self.ext_header(r, (2 + 5 + 5 + 17 + 7) / 8);
                    self.per_mb_rl_table = self.bit_rate > 50 * 1024 && r.get1() != 0;
                    if !self.per_mb_rl_table {
                        self.rl_chroma_table_index = decode012(r) as usize;
                        self.rl_table_index = decode012(r) as usize;
                    }
                    self.dc_table_index = r.get1() as usize;
                    self.inter_intra_pred = false;
                }
            }
            self.no_rounding = true;
        } else {
            self.use_skip_mb_code = r.get1() != 0;
            match self.ver {
                Version::V3 => {
                    self.rl_table_index = decode012(r) as usize;
                    self.rl_chroma_table_index = self.rl_table_index;
                    self.dc_table_index = r.get1() as usize;
                    self.mv_table_index = r.get1() as usize;
                }
                _ => {
                    self.per_mb_rl_table = self.bit_rate > 50 * 1024 && r.get1() != 0;
                    if !self.per_mb_rl_table {
                        self.rl_table_index = decode012(r) as usize;
                        self.rl_chroma_table_index = self.rl_table_index;
                    }
                    self.dc_table_index = r.get1() as usize;
                    self.mv_table_index = r.get1() as usize;
                    self.inter_intra_pred = self.width * self.height < 320 * 240 && self.bit_rate <= 128 * 1024;
                }
            }
            self.no_rounding = if self.flipflop_rounding { !self.no_rounding } else { false };
        }
        self.esc3_level_length = 0;
        self.esc3_run_length = 0;
        Ok(Header::Ok)
    }

    /// ff_msmpeg4_decode_ext_header
    fn ext_header(&mut self, r: &mut Bits, buf_size: usize) {
        let left = (buf_size * 8) as isize - r.count() as isize;
        let length = 17;
        if left >= length && left < length + 8 {
            r.get(5);
            self.bit_rate = r.get(11) * 1024;
            self.flipflop_rounding = r.get1() != 0;
        } else if left < length + 8 {
            self.flipflop_rounding = false;
        }
    }

    /// ff_wmv2_decode_picture_header
    fn wmv2_header(&mut self, r: &mut Bits) -> Result<Header, Error> {
        self.key = r.get1() == 0;
        if self.key {
            r.get(7);
        }
        let q = r.get(5) as i32;
        if q == 0 {
            return Err(Error::Data("qscale"));
        }
        self.qscale = q;
        if !self.key && r.peek(1) != 0 {
            let mut g = r.clone();
            let skip_type = g.get(2);
            let mut run = if skip_type == 3 { self.mb_w } else { self.mb_h } as i32;
            while run > 0 {
                let block = run.min(25) as u32;
                if g.get(block) + 1 != 1 << block {
                    break;
                }
                run -= block as i32;
            }
            if run == 0 {
                return Ok(Header::Skipped);
            }
        }
        Ok(Header::Ok)
    }

    /// parse_mb_skip
    fn parse_mb_skip(&mut self, r: &mut Bits) -> Result<(), Error> {
        let (w, h, mbs) = (self.mb_w, self.mb_h, self.mbs);
        let skip_type = r.get(2);
        match skip_type {
            0 => self.skip.fill(false),
            1 => {
                if r.left() < (w * h) as isize {
                    return Err(Error::Data("skip bits"));
                }
                for y in 0..h {
                    for x in 0..w {
                        self.skip[y * mbs + x] = r.get1() != 0;
                    }
                }
            }
            2 => {
                for y in 0..h {
                    if r.left() < 1 {
                        return Err(Error::Data("skip bits"));
                    }
                    let all = r.get1() != 0;
                    for x in 0..w {
                        self.skip[y * mbs + x] = all || r.get1() != 0;
                    }
                }
            }
            _ => {
                for x in 0..w {
                    if r.left() < 1 {
                        return Err(Error::Data("skip bits"));
                    }
                    let all = r.get1() != 0;
                    for y in 0..h {
                        self.skip[y * mbs + x] = all || r.get1() != 0;
                    }
                }
            }
        }
        let coded = (0..h).flat_map(|y| (0..w).map(move |x| y * mbs + x)).filter(|&i| !self.skip[i]).count();
        if coded as isize > r.left() {
            return Err(Error::Data("skip bits"));
        }
        Ok(())
    }

    /// ff_wmv2_decode_secondary_picture_header
    fn wmv2_secondary(&mut self, r: &mut Bits) -> Result<bool, Error> {
        if self.key {
            self.skip.fill(false);
            let j_type = self.j_type_bit && r.get1() != 0;
            if j_type {
                // (ff_wmv2_decode_secondary_picture_header returns 1: the whole picture is IntraX8)
                self.inter_intra_pred = false;
                self.no_rounding = true;
                self.esc3_level_length = 0;
                self.esc3_run_length = 0;
                let (mb_w, mb_h) = (self.mb_w, self.mb_h);
                let x8 = self.x8.get_or_insert_with(|| IntraX8::new(mb_w, mb_h));
                let planes = [
                    Plane { px: &mut self.cur.y, stride: self.ls },
                    Plane { px: &mut self.cur.u, stride: self.uvls },
                    Plane { px: &mut self.cur.v, stride: self.uvls },
                ];
                let q = self.qscale;
                if x8.decode(r, planes, 2 * q, (q - 1) | 1, self.loop_filter) < mb_h * 2 {
                    return Err(Error::Data("IntraX8 picture"));
                }
                return Ok(true);
            }
            self.per_mb_rl_table = self.per_mb_rl_bit && r.get1() != 0;
            if !self.per_mb_rl_table {
                self.rl_chroma_table_index = decode012(r) as usize;
                self.rl_table_index = decode012(r) as usize;
            }
            self.dc_table_index = r.get1() as usize;
            if r.left() * 8 < (self.mb_w * self.mb_h) as isize {
                return Err(Error::Data("picture too small"));
            }
            self.inter_intra_pred = false;
            self.no_rounding = true;
        } else {
            self.parse_mb_skip(r)?;
            let cbp_index = decode012(r) as usize;
            const MAP: [[usize; 3]; 3] = [[0, 2, 1], [1, 0, 2], [2, 1, 0]];
            self.cbp_table_index = MAP[(self.qscale > 10) as usize + (self.qscale > 20) as usize][cbp_index];
            self.mspel = self.mspel_bit && r.get1() != 0;
            if self.abt_flag {
                self.per_mb_abt = r.get1() == 0;
                if !self.per_mb_abt {
                    self.abt_type = decode012(r);
                }
            }
            self.per_mb_rl_table = self.per_mb_rl_bit && r.get1() != 0;
            if !self.per_mb_rl_table {
                self.rl_table_index = decode012(r) as usize;
                self.rl_chroma_table_index = self.rl_table_index;
            }
            if r.left() < 2 {
                return Err(Error::Data("picture header"));
            }
            self.dc_table_index = r.get1() as usize;
            self.mv_table_index = r.get1() as usize;
            self.inter_intra_pred = false;
            self.no_rounding = !self.no_rounding;
        }
        self.esc3_level_length = 0;
        self.esc3_run_length = 0;
        Ok(false)
    }
}

impl Decoder {
    /// Decodes one picture. `Ok(None)`: a WMV2 picture that repeats the last one.
    pub fn decode(&mut self, buf: &[u8]) -> Result<Option<Picture>, Error> {
        let mut r = Bits::new(buf);
        let h = if self.ver == Version::Wmv2 { self.wmv2_header(&mut r)? } else { self.msmpeg4_header(&mut r)? };
        if let Header::Skipped = h {
            return Ok(None);
        }
        if !self.key && self.last.is_none() {
            // (a P picture first: FFmpeg's grey dummy reference)
            let mut g = self.cur.clone();
            g.y.fill(0x80);
            g.u.fill(0x80);
            g.v.fill(0x80);
            self.last = Some(g);
        }
        self.motion.fill([0; 2]);
        if self.ver != Version::Wmv2 {
            self.skip.fill(false);
        }
        let res = self.decode_picture(&mut r, buf.len());
        // (a damaged picture is still the reference for the next, as FFmpeg's concealed one)
        let out = self.picture();
        self.last = Some(self.cur.clone());
        res.map(|_| Some(out))
    }

    fn decode_picture(&mut self, r: &mut Bits, buf_size: usize) -> Result<(), Error> {
        if self.ver == Version::Wmv2 {
            if self.wmv2_secondary(r)? {
                return Ok(());
            }
        }
        self.mb_x = 0;
        self.mb_y = 0;
        let mut res = self.decode_slice(r);
        while self.mb_y < self.mb_h {
            if self.slice_height == 0 || self.mb_x != 0 || res.is_err() || self.mb_y % self.slice_height != 0 || r.left() < 0 {
                break;
            }
            if self.ver < Version::Wmv1 {
                self.clean_buffers();
            }
            if let Err(e) = self.decode_slice(r) {
                res = Err(e);
            }
        }
        if self.ver == Version::V3 && self.key {
            self.ext_header(r, buf_size);
        }
        res
    }

    /// ff_mpeg4_clean_buffers (the AC predictors around a slice's start)
    fn clean_buffers(&mut self) {
        let l_xy = (self.b8 + 1) as isize + (2 * self.mb_y as isize - 1) * self.b8 as isize + self.mb_x as isize * 2 - 1;
        for k in 0..self.b8 * 2 + 1 {
            self.ac_val[(l_xy + k as isize) as usize] = [0; 16];
        }
        let y_size = self.b8 * (2 * self.mb_h + 1);
        let c_size = self.mbs * (self.mb_h + 1);
        for p in 0..2 {
            let c_xy = (y_size + self.mbs + 1 + p * c_size) as isize + (self.mb_y as isize - 1) * self.mbs as isize + self.mb_x as isize - 1;
            for k in 0..self.mbs + 1 {
                self.ac_val[(c_xy + k as isize) as usize] = [0; 16];
            }
        }
    }

    fn decode_slice(&mut self, r: &mut Bits) -> Result<(), Error> {
        self.first_slice_line = true;
        self.resync_mb_x = self.mb_x;
        self.resync_mb_y = self.mb_y;
        self.set_qscale(self.qscale);
        while self.mb_y < self.mb_h {
            if self.resync_mb_y + self.slice_height == self.mb_y {
                return Ok(());
            }
            while self.mb_x < self.mb_w {
                self.init_block_index();
                if self.resync_mb_x == self.mb_x && self.resync_mb_y + 1 == self.mb_y {
                    self.first_slice_line = false;
                }
                let ret = if self.ver == Version::Wmv2 { self.wmv2_mb(r) } else { self.msmpeg4_mb(r) };
                self.update_motion_val();
                ret?;
                self.reconstruct();
                if self.loop_filter {
                    self.h263_loop_filter();
                }
                self.mb_x += 1;
            }
            self.mb_x = 0;
            self.mb_y += 1;
        }
        Ok(())
    }

    fn update_motion_val(&mut self) {
        let m = if self.mb_intra { [0, 0] } else { [self.mv[0] as i16, self.mv[1] as i16] };
        let xy = self.mvi();
        let w = self.b8;
        for k in [xy, xy + 1, xy + w, xy + w + 1] {
            self.motion[k] = m;
        }
    }

    fn skipped_mb(&mut self) {
        self.mb_intra = false;
        self.last_index = [-1; 6];
        self.mv = [0, 0];
        self.hshift = 0;
        self.skip[self.mb_y * self.mbs + self.mb_x] = true;
    }

    /// ff_msmpeg4_coded_block_pred
    fn coded_block_pred(&self, n: usize) -> (u8, usize) {
        let xy = self.b8 + 1 + self.bi[n];
        let w = self.b8;
        let (a, b, c) = (self.coded_block[xy - 1], self.coded_block[xy - 1 - w], self.coded_block[xy - w]);
        (if b == c { a } else { c }, xy)
    }

    fn intra_cbp(&mut self, code: i32) -> u32 {
        let mut cbp = 0;
        for i in 0..6 {
            let mut val = ((code >> (5 - i)) & 1) as u8;
            if i < 4 {
                let (pred, xy) = self.coded_block_pred(i);
                val ^= pred;
                self.coded_block[xy] = val;
            }
            cbp |= (val as u32) << (5 - i);
        }
        cbp
    }

    /// msmpeg4v34_decode_mb
    fn msmpeg4_mb(&mut self, r: &mut Bits) -> Result<(), Error> {
        let tb = tables();
        if r.left() <= 0 {
            return Err(Error::Data("end of data"));
        }
        let cbp;
        if !self.key {
            if self.use_skip_mb_code && r.get1() != 0 {
                self.skipped_mb();
                return Ok(());
            }
            let code = tb.non_intra[3].read(r);
            self.mb_intra = (!code & 0x40) != 0;
            cbp = (code & 0x3f) as u32;
        } else {
            self.mb_intra = true;
            let code = tb.mb_i.read(r);
            cbp = self.intra_cbp(code);
        }
        if !self.mb_intra {
            if self.per_mb_rl_table && cbp != 0 {
                self.rl_table_index = decode012(r) as usize;
                self.rl_chroma_table_index = self.rl_table_index;
            }
            let (mut mx, mut my) = self.h263_pred_motion();
            self.decode_motion(r, &mut mx, &mut my);
            self.mv = [mx, my];
        } else {
            self.ac_pred = r.get1() != 0;
            if self.inter_intra_pred {
                self.h263_aic_dir = tb.inter_intra.read(r);
            }
            if self.per_mb_rl_table && cbp != 0 {
                self.rl_table_index = decode012(r) as usize;
                self.rl_chroma_table_index = self.rl_table_index;
            }
        }
        self.block = [[0; 64]; 6];
        for i in 0..6 {
            self.decode_block(r, i, (cbp >> (5 - i)) & 1 != 0, None)?;
        }
        Ok(())
    }

    /// wmv2_decode_mb
    fn wmv2_mb(&mut self, r: &mut Bits) -> Result<(), Error> {
        let tb = tables();
        let cbp;
        if !self.key {
            if self.skip[self.mb_y * self.mbs + self.mb_x] {
                self.skipped_mb();
                return Ok(());
            }
            if r.left() <= 0 {
                return Err(Error::Data("end of data"));
            }
            let code = tb.non_intra[self.cbp_table_index].read(r);
            self.mb_intra = (!code & 0x40) != 0;
            cbp = (code & 0x3f) as u32;
        } else {
            self.mb_intra = true;
            if r.left() <= 0 {
                return Err(Error::Data("end of data"));
            }
            let code = tb.mb_i.read(r);
            cbp = self.intra_cbp(code);
        }
        if !self.mb_intra {
            let (mut mx, mut my) = self.wmv2_pred_motion(r);
            if cbp != 0 {
                self.block = [[0; 64]; 6];
                if self.per_mb_rl_table {
                    self.rl_table_index = decode012(r) as usize;
                    self.rl_chroma_table_index = self.rl_table_index;
                }
                if self.abt_flag && self.per_mb_abt {
                    self.per_block_abt = r.get1() != 0;
                    if !self.per_block_abt {
                        self.abt_type = decode012(r);
                    }
                } else {
                    self.per_block_abt = false;
                }
            }
            self.decode_motion(r, &mut mx, &mut my);
            self.hshift = if ((mx | my) & 1) != 0 && self.mspel { r.get1() as usize } else { 0 };
            self.mv = [mx, my];
            for i in 0..6 {
                self.wmv2_inter_block(r, i, (cbp >> (5 - i)) & 1 != 0)?;
            }
        } else {
            self.ac_pred = r.get1() != 0;
            if self.inter_intra_pred {
                self.h263_aic_dir = tb.inter_intra.read(r);
            }
            if self.per_mb_rl_table && cbp != 0 {
                self.rl_table_index = decode012(r) as usize;
                self.rl_chroma_table_index = self.rl_table_index;
            }
            self.block = [[0; 64]; 6];
            for i in 0..6 {
                self.decode_block(r, i, (cbp >> (5 - i)) & 1 != 0, None)?;
            }
        }
        Ok(())
    }

    fn wmv2_inter_block(&mut self, r: &mut Bits, n: usize, coded: bool) -> Result<(), Error> {
        if !coded {
            self.last_index[n] = -1;
            return Ok(());
        }
        if self.per_block_abt {
            self.abt_type = decode012(r);
        }
        self.abt_type_table[n] = self.abt_type;
        if self.abt_type != 0 {
            const SUB: [u32; 3] = [2, 3, 1];
            let scan = &tables().abt[self.abt_type as usize - 1];
            let sub = SUB[decode012(r) as usize];
            if sub & 1 != 0 {
                self.decode_block(r, n, true, Some(scan))?;
            }
            if sub & 2 != 0 {
                let mut b = std::mem::replace(&mut self.block[n], self.abt2[n]);
                let res = self.decode_block(r, n, true, Some(scan));
                std::mem::swap(&mut b, &mut self.block[n]);
                self.abt2[n] = b;
                res?;
            }
            self.last_index[n] = 63;
            Ok(())
        } else {
            self.decode_block(r, n, true, None)
        }
    }
}

impl Decoder {
    /// ff_h263_pred_motion for block 0
    fn h263_pred_motion(&self) -> (i32, i32) {
        let xy = self.mvi();
        let w = self.b8;
        let m = |k: usize| (self.motion[k][0] as i32, self.motion[k][1] as i32);
        let a = m(xy - 1);
        if self.first_slice_line {
            if self.mb_x == self.resync_mb_x {
                (0, 0)
            } else if self.mb_x + 1 == self.resync_mb_x {
                let c = m(xy + 2 - w);
                if self.mb_x == 0 { c } else { (mid_pred(a.0, 0, c.0), mid_pred(a.1, 0, c.1)) }
            } else {
                a
            }
        } else {
            let (b, c) = (m(xy - w), m(xy + 2 - w));
            (mid_pred(a.0, b.0, c.0), mid_pred(a.1, b.1, c.1))
        }
    }

    /// wmv2_pred_motion
    fn wmv2_pred_motion(&self, r: &mut Bits) -> (i32, i32) {
        let xy = self.mvi();
        let w = self.b8;
        let m = |k: usize| (self.motion[k][0] as i32, self.motion[k][1] as i32);
        let a = m(xy - 1);
        let diff = if self.mb_x != 0 && !self.first_slice_line && !self.mspel && self.top_left_mv_flag {
            let b = m(xy - w);
            (a.0 - b.0).abs().max((a.1 - b.1).abs())
        } else {
            0
        };
        let ty = if diff >= 8 { r.get1() } else { 2 };
        match ty {
            0 => a,
            1 => m(xy - w),
            _ if self.first_slice_line => a,
            _ => {
                let (b, c) = (m(xy - w), m(xy + 2 - w));
                (mid_pred(a.0, b.0, c.0), mid_pred(a.1, b.1, c.1))
            }
        }
    }

    /// ff_msmpeg4_decode_motion
    fn decode_motion(&self, r: &mut Bits, mx: &mut i32, my: &mut i32) {
        let mv = &tables().mv[self.mv_table_index];
        let code = mv.vlc.read(r);
        let (mut x, mut y);
        if code == MV_ESC {
            x = r.get(6) as i32;
            y = r.get(6) as i32;
        } else if code < 0 {
            // (FFmpeg reads table entry -1: garbage; a damaged stream either way)
            x = 32;
            y = 32;
        } else {
            x = mv.x[code as usize] as i32;
            y = mv.y[code as usize] as i32;
        }
        x += *mx - 32;
        y += *my - 32;
        for v in [&mut x, &mut y] {
            if *v <= -64 {
                *v += 64;
            } else if *v >= 64 {
                *v -= 64;
            }
        }
        *mx = x;
        *my = y;
    }

    /// ff_msmpeg4_pred_dc: (prediction, direction: 0 left 1 top)
    fn pred_dc(&self, n: usize) -> (i32, i32) {
        let scale = if n < 4 { self.y_dc_scale } else { self.c_dc_scale };
        let wrap = self.wrap(n);
        let i = self.dci(n);
        let dv = |k: usize| self.dc_val[k] as i32;
        let mut a = dv(i - 1);
        let mut b = dv(i - 1 - wrap);
        let mut c = dv(i - wrap);
        if self.first_slice_line && n & 2 == 0 && self.ver < Version::Wmv1 {
            b = 1024;
            c = 1024;
        }
        a = (a + (scale >> 1)) / scale;
        b = (b + (scale >> 1)) / scale;
        c = (c + (scale >> 1)) / scale;
        if self.ver > Version::V3 {
            if self.inter_intra_pred {
                return match n {
                    1 => (a, 0),
                    2 => (c, 1),
                    3 => if (a - b).abs() < (b - c).abs() { (c, 1) } else { (a, 0) },
                    _ => {
                        let a = if self.mb_x == 0 { (1024 + (scale >> 1)) / scale } else { self.pixel_dc(n, -8, 0, scale * 8) };
                        let c = if self.mb_y == 0 { (1024 + (scale >> 1)) / scale } else { self.pixel_dc(n, 0, -8, scale * 8) };
                        match self.h263_aic_dir {
                            0 => (a, 0),
                            1 => if n == 0 { (c, 1) } else { (a, 0) },
                            2 => if n == 0 { (a, 0) } else { (c, 1) },
                            _ => (c, 1),
                        }
                    }
                };
            }
            if (a - b).abs() < (b - c).abs() { (c, 1) } else { (a, 0) }
        } else if (a - b).abs() <= (b - c).abs() {
            (c, 1)
        } else {
            (a, 0)
        }
    }

    /// get_dc of the 8x8 pixels of the current picture beside block n (block 0 or chroma).
    fn pixel_dc(&self, n: usize, dx: isize, dy: isize, scale: i32) -> i32 {
        let (p, ls, x, y) = if n < 4 {
            (&self.cur.y, self.ls, 16 * self.mb_x, 16 * self.mb_y)
        } else {
            (if n == 4 { &self.cur.u } else { &self.cur.v }, self.uvls, 8 * self.mb_x, 8 * self.mb_y)
        };
        let (x, y) = ((x as isize + dx) as usize, (y as isize + dy) as usize);
        let mut sum = 0;
        for j in 0..8 {
            for i in 0..8 {
                sum += p[(y + j) * ls + x + i] as i32;
            }
        }
        (sum + (scale >> 1)) / scale
    }

    /// msmpeg4_decode_dc: (level, direction)
    fn decode_dc(&mut self, r: &mut Bits, n: usize) -> (i32, i32) {
        let vlc = &tables().dc[self.dc_table_index][(n >= 4) as usize];
        let mut level = vlc.read(r);
        if level == DC_MAX {
            level = r.get(8) as i32;
            if r.get1() != 0 {
                level = -level;
            }
        } else if level != 0 && r.get1() != 0 {
            level = -level;
        }
        let (pred, dir) = self.pred_dc(n);
        level += pred;
        let i = self.dci(n);
        self.dc_val[i] = (level * if n < 4 { self.y_dc_scale } else { self.c_dc_scale }) as i16;
        (level, dir)
    }

    /// ff_mpeg4_pred_ac (qscale is the same over a picture here)
    fn pred_ac(&mut self, n: usize, dir: i32) {
        let i = self.dci(n);
        let b = &mut self.block[n];
        if self.ac_pred {
            let p = if dir == 0 { self.ac_val[i - 1] } else { self.ac_val[i - if n < 4 { self.b8 } else { self.mbs }] };
            if dir == 0 {
                for k in 1..8 {
                    b[k << 3] = b[k << 3].wrapping_add(p[k]);
                }
            } else {
                for k in 1..8 {
                    b[k] = b[k].wrapping_add(p[k + 8]);
                }
            }
        }
        let a = &mut self.ac_val[i];
        for k in 1..8 {
            a[k] = b[k << 3];
            a[8 + k] = b[k];
        }
    }
}

impl Decoder {
    /// ff_msmpeg4_decode_block into self.block[n]
    fn decode_block(&mut self, r: &mut Bits, n: usize, coded: bool, given: Option<&'static [u8; 64]>) -> Result<(), Error> {
        let tb = tables();
        let (qmul, qadd, mut i, rl, run_diff, scan);
        let mut dc_dir = -1;
        if self.mb_intra {
            qmul = 1;
            qadd = 0;
            let (mut level, dir) = self.decode_dc(r, n);
            dc_dir = dir;
            if level < 0 && self.inter_intra_pred {
                level = 0;
            }
            if n < 4 {
                rl = &tb.rl[self.rl_table_index];
                if level > 256 * self.y_dc_scale && !self.inter_intra_pred {
                    return Err(Error::Data("dc overflow"));
                }
            } else {
                rl = &tb.rl[3 + self.rl_chroma_table_index];
                if level > 256 * self.c_dc_scale && !self.inter_intra_pred {
                    return Err(Error::Data("dc overflow"));
                }
            }
            self.block[n][0] = level as i16;
            run_diff = (self.ver >= Version::Wmv1) as i32;
            i = 0;
            let v3 = self.ver == Version::V3;
            scan = if self.ac_pred {
                if dir == 0 {
                    if v3 { &tb.zigzag[2] } else { &tb.scan[3] }
                } else if v3 {
                    &tb.zigzag[1]
                } else {
                    &tb.scan[2]
                }
            } else if v3 {
                &tb.zigzag[0]
            } else {
                &tb.scan[1]
            };
        } else {
            qmul = self.qscale << 1;
            qadd = (self.qscale - 1) | 1;
            i = -1;
            rl = &tb.rl[3 + self.rl_table_index];
            run_diff = 1;
            if !coded {
                self.last_index[n] = -1;
                return Ok(());
            }
            scan = given.unwrap_or(if self.ver == Version::V3 { &tb.zigzag[0] } else { &tb.scan[0] });
        }
        if coded {
            loop {
                let (mut level, mut run) = rl.get(r, qmul, qadd);
                if level == 0 {
                    let cache = r.peek32();
                    if cache & 0x8000_0000 == 0 {
                        if cache & 0x4000_0000 == 0 {
                            // third escape
                            r.skip(2);
                            let last;
                            if self.ver <= Version::V3 {
                                last = r.get1();
                                run = r.get(6) as i32;
                                level = r.get_sbits(8);
                            } else {
                                last = r.get1();
                                if self.esc3_level_length == 0 {
                                    let mut ll;
                                    if self.qscale < 8 {
                                        ll = r.get(3);
                                        if ll == 0 {
                                            ll = 8 + r.get1();
                                        }
                                    } else {
                                        ll = 2;
                                        while ll < 8 && r.peek(1) == 0 {
                                            ll += 1;
                                            r.skip(1);
                                        }
                                        if ll < 8 {
                                            r.skip(1);
                                        }
                                    }
                                    self.esc3_level_length = ll;
                                    self.esc3_run_length = r.get(2) + 3;
                                }
                                run = r.get(self.esc3_run_length) as i32;
                                let sign = r.get1();
                                level = r.get(self.esc3_level_length) as i32;
                                if sign != 0 {
                                    level = -level;
                                }
                            }
                            level = if level > 0 { level * qmul + qadd } else { level * qmul - qadd };
                            i += run + 1;
                            if last != 0 {
                                i += 192;
                            }
                        } else {
                            // second escape
                            r.skip(2);
                            (level, run) = rl.get(r, qmul, qadd);
                            i += run + rl.max_run[(run >> 7) as usize & 1][((level / qmul) as usize).min(MAX_LEVEL)] + run_diff;
                            if r.get1() != 0 {
                                level = -level;
                            }
                        }
                    } else {
                        // first escape
                        r.skip(1);
                        (level, run) = rl.get(r, qmul, qadd);
                        i += run;
                        level += rl.max_level[(run >> 7) as usize & 1][((run - 1) & 63) as usize] * qmul;
                        if r.get1() != 0 {
                            level = -level;
                        }
                    }
                } else {
                    i += run;
                    if r.get1() != 0 {
                        level = -level;
                    }
                }
                if i > 62 {
                    i -= 192;
                    if i & !63 != 0 {
                        if r.left() >= 0 {
                            // (FFmpeg's "ignoring overflow")
                            i = 63;
                            break;
                        }
                        return Err(Error::Data("ac-tex damaged"));
                    }
                    self.block[n][scan[i as usize] as usize] = level as i16;
                    break;
                }
                self.block[n][scan[i as usize] as usize] = level as i16;
            }
        }
        if self.mb_intra {
            self.pred_ac(n, dc_dir);
            if self.ac_pred {
                i = 63;
            }
        }
        if self.ver >= Version::Wmv1 && i > 0 {
            i = 63;
        }
        self.last_index[n] = i;
        Ok(())
    }
}

impl Decoder {
    /// ff_clean_intra_table_entries
    fn clean_intra(&mut self) {
        let w = self.b8;
        let xy = self.dci(0);
        for k in [xy, xy + 1, xy + w, xy + 1 + w] {
            self.dc_val[k] = 1024;
            self.ac_val[k] = [0; 16];
        }
        for n in [4, 5] {
            let k = self.dci(n);
            self.dc_val[k] = 1024;
            self.ac_val[k] = [0; 16];
        }
        self.mbintra[self.mb_y * self.mbs + self.mb_x] = 0;
    }

    /// ff_mpv_reconstruct_mb
    fn reconstruct(&mut self) {
        let mb_xy = self.mb_y * self.mbs + self.mb_x;
        let (ls, uvls) = (self.ls, self.uvls);
        let yo = self.mb_y * 16 * ls + self.mb_x * 16;
        let co = self.mb_y * 8 * uvls + self.mb_x * 8;
        let boff = [yo, yo + 8, yo + 8 * ls, yo + 8 * ls + 8];
        if !self.mb_intra {
            if self.mbintra[mb_xy] != 0 {
                self.clean_intra();
            }
            if self.ver == Version::Wmv2 && self.mspel {
                self.mspel_motion();
            } else {
                self.mpeg_motion();
            }
            for n in 0..6 {
                if self.last_index[n] < 0 {
                    continue;
                }
                let (plane, off, stride) = match n {
                    0..4 => (&mut self.cur.y, boff[n], ls),
                    4 => (&mut self.cur.u, co, uvls),
                    _ => (&mut self.cur.v, co, uvls),
                };
                let b = &mut self.block[n];
                if self.ver == Version::Wmv2 {
                    match self.abt_type_table[n] {
                        0 => idct::wmv2_add(plane, off, stride, b),
                        1 => {
                            idct::simple84_add(plane, off, stride, b);
                            idct::simple84_add(plane, off + 4 * stride, stride, &mut self.abt2[n]);
                            self.abt2[n] = [0; 64];
                        }
                        _ => {
                            idct::simple48_add(plane, off, stride, b);
                            idct::simple48_add(plane, off + 4, stride, &mut self.abt2[n]);
                            self.abt2[n] = [0; 64];
                        }
                    }
                } else {
                    idct::simple_add(&mut plane[off..], stride, b);
                }
            }
        } else {
            self.mbintra[mb_xy] = 1;
            let qmul = self.qscale << 1;
            let qadd = (self.qscale - 1) | 1;
            for n in 0..6 {
                let b = &mut self.block[n];
                // dct_unquantize_h263_intra
                b[0] = (b[0] as i32 * if n < 4 { self.y_dc_scale } else { self.c_dc_scale }) as i16;
                let nc = if self.ac_pred { 63 } else { raster_end(self.ver, self.last_index[n]) };
                for k in 1..=nc {
                    let l = b[k] as i32;
                    if l != 0 {
                        b[k] = if l < 0 { l * qmul - qadd } else { l * qmul + qadd } as i16;
                    }
                }
                let (plane, off, stride) = match n {
                    0..4 => (&mut self.cur.y, boff[n], ls),
                    4 => (&mut self.cur.u, co, uvls),
                    _ => (&mut self.cur.v, co, uvls),
                };
                if self.ver == Version::Wmv2 {
                    idct::wmv2_put(plane, off, stride, b);
                } else {
                    idct::simple_put(&mut plane[off..], stride, b);
                }
            }
        }
    }

    /// mpeg_motion_internal, 16x16, H.263 chroma
    fn mpeg_motion(&mut self) {
        let last = self.last.as_ref().expect("reference");
        let [mx, my] = self.mv;
        let rnd = !self.no_rounding;
        let dxy = (((my & 1) << 1) | (mx & 1)) as usize;
        let src_x = self.mb_x as i32 * 16 + (mx >> 1);
        let src_y = self.mb_y as i32 * 16 + (my >> 1);
        let uvdxy = dxy | (my & 2) as usize | ((mx & 2) >> 1) as usize;
        let (uvx, uvy) = (src_x >> 1, src_y >> 1);
        let (pw, ph) = ((self.mb_w * 16) as i32, (self.mb_h * 16) as i32);
        let mut win = [0u8; 17 * 17];
        let (s, ss) = hpel::source(&last.y, self.ls, pw, ph, src_x, src_y, 17, 17, &mut win);
        let yo = self.mb_y * 16 * self.ls + self.mb_x * 16;
        hpel::hpel(&mut self.cur.y, yo, self.ls, s, ss, 16, 16, dxy, rnd);
        let co = self.mb_y * 8 * self.uvls + self.mb_x * 8;
        for (src, dst) in [(&last.u, &mut self.cur.u), (&last.v, &mut self.cur.v)] {
            let mut w = [0u8; 81];
            let (s, ss) = hpel::source(src, self.uvls, pw / 2, ph / 2, uvx, uvy, 9, 9, &mut w);
            hpel::hpel(dst, co, self.uvls, s, ss, 8, 8, uvdxy, rnd);
        }
    }

    /// ff_mspel_motion
    fn mspel_motion(&mut self) {
        let last = self.last.as_ref().expect("reference");
        let [mx, my] = self.mv;
        let (w, h) = (self.width as i32, self.height as i32);
        let mut dxy = ((((my & 1) << 1) | (mx & 1)) * 2) as usize + self.hshift;
        let src_x = (self.mb_x as i32 * 16 + (mx >> 1)).clamp(-16, w);
        let src_y = (self.mb_y as i32 * 16 + (my >> 1)).clamp(-16, h);
        if src_x <= -16 || src_x >= w {
            dxy &= !3;
        }
        if src_y <= -16 || src_y >= h {
            dxy &= !4;
        }
        let (pw, ph) = ((self.mb_w * 16) as i32, (self.mb_h * 16) as i32);
        let mut win = [0u8; 19 * 19];
        let (s, ss) = hpel::source(&last.y, self.ls, pw, ph, src_x - 1, src_y - 1, 19, 19, &mut win);
        let yo = self.mb_y * 16 * self.ls + self.mb_x * 16;
        for (bx, by) in [(0, 0), (8, 0), (0, 8), (8, 8)] {
            hpel::mspel8(&mut self.cur.y, yo + by * self.ls + bx, self.ls, s, (1 + by) * ss + 1 + bx, ss, dxy);
        }
        let mut cdxy = 0;
        if mx & 3 != 0 {
            cdxy |= 1;
        }
        if my & 3 != 0 {
            cdxy |= 2;
        }
        let ux = (self.mb_x as i32 * 8 + (mx >> 2)).clamp(-8, w >> 1);
        if ux == w >> 1 {
            cdxy &= !1;
        }
        let uy = (self.mb_y as i32 * 8 + (my >> 2)).clamp(-8, h >> 1);
        if uy == h >> 1 {
            cdxy &= !2;
        }
        let rnd = !self.no_rounding;
        let co = self.mb_y * 8 * self.uvls + self.mb_x * 8;
        for (src, dst) in [(&last.u, &mut self.cur.u), (&last.v, &mut self.cur.v)] {
            let mut wn = [0u8; 81];
            let (s, ss) = hpel::source(src, self.uvls, pw / 2, ph / 2, ux, uy, 9, 9, &mut wn);
            hpel::hpel(dst, co, self.uvls, s, ss, 8, 8, cdxy, rnd);
        }
    }

    /// ff_h263_loop_filter (one qscale per picture)
    fn h263_loop_filter(&mut self) {
        let (ls, uvls) = (self.ls, self.uvls);
        let xy = self.mb_y * self.mbs + self.mb_x;
        let dy = self.mb_y * 16 * ls + self.mb_x * 16;
        let dc = self.mb_y * 8 * uvls + self.mb_x * 8;
        let q = |s: &Decoder, k: usize| if s.skip[k] { 0 } else { s.qscale };
        let qp_c = q(self, xy);
        let (y, u, v) = (&mut self.cur.y, &mut self.cur.u, &mut self.cur.v);
        if qp_c != 0 {
            v_filter(y, dy + 8 * ls, ls, qp_c);
            v_filter(y, dy + 8 * ls + 8, ls, qp_c);
        }
        if self.mb_y != 0 {
            let qp_tt = if self.skip[xy - self.mbs] { 0 } else { self.qscale };
            let qp_tc = if qp_c != 0 { qp_c } else { qp_tt };
            if qp_tc != 0 {
                v_filter(y, dy, ls, qp_tc);
                v_filter(y, dy + 8, ls, qp_tc);
                v_filter(u, dc, uvls, qp_tc);
                v_filter(v, dc, uvls, qp_tc);
            }
            if qp_tt != 0 {
                h_filter(y, dy - 8 * ls + 8, ls, qp_tt);
            }
            if self.mb_x != 0 {
                let qp_dt = if qp_tt != 0 || self.skip[xy - 1 - self.mbs] { qp_tt } else { self.qscale };
                if qp_dt != 0 {
                    h_filter(y, dy - 8 * ls, ls, qp_dt);
                    h_filter(u, dc - 8 * uvls, uvls, qp_dt);
                    h_filter(v, dc - 8 * uvls, uvls, qp_dt);
                }
            }
        }
        if qp_c != 0 {
            h_filter(y, dy + 8, ls, qp_c);
            if self.mb_y + 1 == self.mb_h {
                h_filter(y, dy + 8 * ls + 8, ls, qp_c);
            }
        }
        if self.mb_x != 0 {
            let qp_lc = if qp_c != 0 || self.skip[xy - 1] { qp_c } else { self.qscale };
            if qp_lc != 0 {
                h_filter(y, dy, ls, qp_lc);
                if self.mb_y + 1 == self.mb_h {
                    h_filter(y, dy + 8 * ls, ls, qp_lc);
                    h_filter(u, dc, uvls, qp_lc);
                    h_filter(v, dc, uvls, qp_lc);
                }
            }
        }
    }

    fn picture(&self) -> Picture {
        Picture { width: self.width, height: self.height, stride: [self.ls, self.uvls], planes: [self.cur.y.clone(), self.cur.u.clone(), self.cur.v.clone()], key: self.key }
    }
}

/// intra_scantable.raster_end[last]: the highest raster index among the first last + 1
fn raster_end(ver: Version, last: i32) -> usize {
    if last < 0 {
        return 0;
    }
    let tb = tables();
    let scan = if ver == Version::V3 { &tb.zigzag[0] } else { &tb.scan[1] };
    scan[..=last as usize].iter().copied().max().unwrap_or(0) as usize
}

fn lf(p0: i32, p1: i32, p2: i32, p3: i32, strength: i32) -> [u8; 4] {
    let d = (p0 - p3 + 4 * (p2 - p1)) / 8;
    let d1 = if d < -2 * strength {
        0
    } else if d < -strength {
        -2 * strength - d
    } else if d < strength {
        d
    } else if d < 2 * strength {
        2 * strength - d
    } else {
        0
    };
    let mut p1 = p1 + d1;
    let mut p2 = p2 - d1;
    if p1 & 256 != 0 {
        p1 = !(p1 >> 31);
    }
    if p2 & 256 != 0 {
        p2 = !(p2 >> 31);
    }
    let ad1 = d1.abs() >> 1;
    let d2 = ((p0 - p3) / 4).clamp(-ad1, ad1);
    [(p0 - d2) as u8, p1 as u8, p2 as u8, (p3 + d2) as u8]
}

/// h263_v_loop_filter: the edge above row `off`
fn v_filter(p: &mut [u8], off: usize, s: usize, q: i32) {
    let st = t::H263_LOOP_FILTER_STRENGTH[q as usize] as i32;
    for x in 0..8 {
        let o = off + x;
        let v = lf(p[o - 2 * s] as i32, p[o - s] as i32, p[o] as i32, p[o + s] as i32, st);
        (p[o - 2 * s], p[o - s], p[o], p[o + s]) = (v[0], v[1], v[2], v[3]);
    }
}

/// h263_h_loop_filter: the edge left of column `off`
fn h_filter(p: &mut [u8], off: usize, s: usize, q: i32) {
    let st = t::H263_LOOP_FILTER_STRENGTH[q as usize] as i32;
    for y in 0..8 {
        let o = off + y * s;
        let v = lf(p[o - 2] as i32, p[o - 1] as i32, p[o] as i32, p[o + 1] as i32, st);
        p[o - 2..o + 2].copy_from_slice(&v);
    }
}
