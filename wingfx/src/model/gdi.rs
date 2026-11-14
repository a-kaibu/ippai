//! GDI into DIB sections: an executable specification of PatBlt / BitBlt with the 256 ternary
//! raster operations, brushes, brush origin, clipping and pixel format conversion.
//!
//! Sources (Microsoft Learn): "Ternary Raster Operations" (the operation index is bits 16..23 of
//! the code; result bit = index bit number (P<<2 | S<<1 | D)), "BitBlt function" (format
//! conversion, monochrome rules), "PatBlt function", "Brush Origin", "IntersectClipRect".
//! Where the documentation says nothing, the rule chosen is marked `MEASURED` once Windows'
//! answers confirmed it, or the case is left `unmodeled` (only checked against Windows).

use super::{Fail, Res};
use crate::case::{color, num, Op};
use crate::rng::XorShift32;
use crate::spec::hit;

/// ternary raster operation on whole words: each result bit is bit (p<<2|s<<1|d) of `idx`
pub fn rop3(idx: u8, p: u32, s: u32, d: u32) -> u32 {
    let mut r = 0;
    for k in 0..8u32 {
        if idx >> k & 1 == 1 {
            let mp = if k & 4 != 0 { p } else { !p };
            let ms = if k & 2 != 0 { s } else { !s };
            let md = if k & 1 != 0 { d } else { !d };
            r |= mp & ms & md;
        }
    }
    r
}

/// whether the operation's result depends on the source (pattern, destination)
pub fn uses_s(idx: u8) -> bool {
    (idx >> 2 & 0x33) != (idx & 0x33)
}
pub fn uses_p(idx: u8) -> bool {
    (idx >> 4 & 0x0f) != (idx & 0x0f)
}
pub fn uses_d(idx: u8) -> bool {
    (idx >> 1 & 0x55) != (idx & 0x55)
}

/// a DIB section: rows top to bottom (whatever the memory order: the model needs none), each
/// `stride` bytes as in memory
#[derive(Clone, Debug, PartialEq)]
pub struct Surf {
    pub w: i32,
    pub h: i32,
    pub bpp: u32,
    pub f565: bool,
    /// memory order of the rows (what `fill` and the executor's memory see)
    pub topdown: bool,
    /// the color table (0xRRGGBB), for bpp <= 8
    pub pal: Vec<u32>,
    pub stride: usize,
    pub rows: Vec<Vec<u8>>,
}

pub fn default_palette(bpp: u32) -> Vec<u32> {
    const VGA: [u32; 16] = [0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0x808080, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff];
    match bpp {
        1 => vec![0, 0xffffff],
        4 => VGA.to_vec(),
        8 => (0..256u32).map(|i| ((i >> 5 & 7) * 255 / 7) << 16 | ((i >> 2 & 7) * 255 / 7) << 8 | (i & 3) * 255 / 3).collect(),
        _ => Vec::new(),
    }
}

impl Surf {
    pub fn new(w: i32, h: i32, bpp: u32, f565: bool, topdown: bool, pal: Vec<u32>) -> Surf {
        let stride = ((w as usize * bpp as usize + 31) / 32) * 4;
        Surf { w, h, bpp, f565, topdown, pal, stride, rows: vec![vec![0; stride]; h as usize] }
    }
    pub fn mask(&self) -> u32 {
        if self.bpp == 32 { !0 } else { (1u32 << self.bpp) - 1 }
    }
    pub fn get(&self, x: i32, y: i32) -> u32 {
        let r = &self.rows[y as usize];
        let x = x as usize;
        match self.bpp {
            1 => (r[x / 8] >> (7 - x % 8) & 1) as u32,
            4 => (r[x / 2] >> if x % 2 == 0 { 4 } else { 0 } & 15) as u32,
            8 => r[x] as u32,
            16 => u16::from_le_bytes([r[2 * x], r[2 * x + 1]]) as u32,
            24 => u32::from_le_bytes([r[3 * x], r[3 * x + 1], r[3 * x + 2], 0]),
            _ => u32::from_le_bytes([r[4 * x], r[4 * x + 1], r[4 * x + 2], r[4 * x + 3]]),
        }
    }
    pub fn set(&mut self, x: i32, y: i32, v: u32) {
        let bpp = self.bpp;
        let r = &mut self.rows[y as usize];
        let x = x as usize;
        match bpp {
            1 => {
                let b = 7 - x % 8;
                r[x / 8] = r[x / 8] & !(1 << b) | ((v & 1) as u8) << b;
            }
            4 => {
                let sh = if x % 2 == 0 { 4 } else { 0 };
                r[x / 2] = r[x / 2] & !(15 << sh) | ((v & 15) as u8) << sh;
            }
            8 => r[x] = v as u8,
            16 => r[2 * x..2 * x + 2].copy_from_slice(&(v as u16).to_le_bytes()),
            24 => r[3 * x..3 * x + 3].copy_from_slice(&v.to_le_bytes()[..3]),
            _ => r[4 * x..4 * x + 4].copy_from_slice(&v.to_le_bytes()),
        }
    }
    /// a pixel value as a color (0xRRGGBB)
    pub fn to_rgb(&self, v: u32) -> u32 {
        if self.bpp == 16 {
            hit("GDI.FMT.EXPAND");
        }
        let x5 = |c: u32| c << 3 | c >> 2;
        match self.bpp {
            1 | 4 | 8 => self.pal[v as usize],
            16 if self.f565 => x5(v >> 11 & 31) << 16 | ((v >> 5 & 63) << 2 | (v >> 5 & 63) >> 4) << 8 | x5(v & 31),
            16 => x5(v >> 10 & 31) << 16 | x5(v >> 5 & 31) << 8 | x5(v & 31),
            _ => v & 0xffffff,
        }
    }
    /// a color as this format's pixel value: truncation for 16 bpp, the nearest color table
    /// entry for palettes (squared RGB distance; a tie goes to the later entry, MEASURED for
    /// GetDIBits in tests/winref of masu)
    pub fn from_rgb(&self, c: u32) -> u32 {
        let (r, g, b) = (c >> 16 & 255, c >> 8 & 255, c & 255);
        hit(if self.bpp <= 8 { "GDI.FMT.EXACT" } else { "GDI.FMT.NARROW" });
        match self.bpp {
            1 | 4 | 8 => {
                // a color the table holds: its entry (the last one of that color); any other
                // color: open (GDI.FMT.NEAREST, checked once the operation is done)
                match self.pal.iter().rposition(|&e| e & 0xff_ffff == c & 0xff_ffff) {
                    Some(i) => i as u32,
                    None => {
                        INEXACT.with(|i| i.set(true));
                        0
                    }
                }
            }
            16 if self.f565 => (r >> 3) << 11 | (g >> 2) << 5 | b >> 3,
            16 => (r >> 3) << 10 | (g >> 3) << 5 | b >> 3,
            _ => c,
        }
    }
    /// whether raw values carry over unchanged between two surfaces
    pub fn same_format(&self, o: &Surf) -> bool {
        self.bpp == o.bpp && (self.bpp > 8 || self.pal == o.pal) && (self.bpp != 16 || self.f565 == o.f565)
    }
}

thread_local! {
    /// a color was written into a color table format that holds no entry of that color
    static INEXACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Debug, PartialEq)]
pub enum Brush {
    Solid(u32),
    Hatch(i64, u32),
    Pattern(Surf),
    Null,
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub s: Surf,
    pub text: u32,
    pub bk: u32,
    pub bkmode: i64,
    pub org: (i32, i32),
    /// the clip rectangle (x0, y0, x1, y1), None = no clip region
    pub clip: Option<(i32, i32, i32, i32)>,
    pub brush: Brush,
    /// SetStretchBltMode (a memory DC starts with BLACKONWHITE)
    pub mode: i64,
}

#[derive(Default)]
pub struct Gdi {
    pub slots: Vec<Option<Slot>>,
}

fn n(op: &Op, i: usize) -> Result<i64, Fail> {
    op.tok.get(i).and_then(|t| num(t)).ok_or(Fail::Bad)
}
fn c(op: &Op, i: usize) -> Result<u32, Fail> {
    op.tok.get(i).and_then(|t| color(t)).ok_or(Fail::Bad)
}

impl Gdi {
    pub fn new() -> Gdi {
        Gdi { slots: vec![None; 8] }
    }
    fn slot(&mut self, op: &Op, i: usize) -> Result<&mut Slot, Fail> {
        let k = n(op, i)?;
        self.slots.get_mut(k as usize).and_then(|s| s.as_mut()).ok_or(Fail::Bad)
    }

    /// runs one op; Ok(true) when it was a GDI op
    pub fn op(&mut self, op: &Op, out: &mut Vec<String>) -> Res<bool> {
        INEXACT.with(|i| i.set(false));
        let r = self.op_inner(op, out)?;
        if INEXACT.with(|i| i.get()) {
            // MEASURED: not the nearest entry by RGB distance (Windows' answers differ there)
            return Err(Fail::Unmodeled("GDI.FMT.NEAREST"));
        }
        Ok(r)
    }

    fn op_inner(&mut self, op: &Op, out: &mut Vec<String>) -> Res<bool> {
        let t0 = op.tok[0].as_str();
        match t0 {
            "dib" => {
                let k = n(op, 1)?;
                if !(0..8).contains(&k) {
                    return Err(Fail::Bad);
                }
                let (w, h, bpp) = (n(op, 2)?, n(op, 3)?, n(op, 4)?);
                let mut i = 5;
                let mut topdown = true;
                if op.tok.get(i).is_some_and(|t| t == "td" || t == "bu") {
                    topdown = op.tok[i] == "td";
                    i += 1;
                }
                let f565 = op.tok.get(i).is_some_and(|t| t == "565");
                if f565 {
                    i += 1;
                }
                if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) || !(1..=256).contains(&w) || !(1..=256).contains(&h) || (f565 && bpp != 16) {
                    return Err(Fail::Unmodeled("GDI.DIB.PARAMS"));
                }
                let mut pal = default_palette(bpp as u32);
                for (j, t) in op.tok[i..].iter().enumerate().take(pal.len()) {
                    pal[j] = color(t).ok_or(Fail::Bad)?;
                }
                // (a memory DC's defaults: black text, white background, opaque, the white stock brush)
                self.slots[k as usize] = Some(Slot { s: Surf::new(w as i32, h as i32, bpp as u32, f565, topdown, pal), text: 0, bk: 0xffffff, bkmode: 2, org: (0, 0), clip: None, brush: Brush::Solid(0xffffff), mode: 1 });
            }
            "fill" => {
                let seed = n(op, 2)? as u32;
                let s = &mut self.slot(op, 1)?.s;
                let mut r = XorShift32::new(seed);
                // (memory order: a bottom-up DIB's first memory row is the picture's last)
                for m in 0..s.h as usize {
                    let y = if s.topdown { m } else { s.h as usize - 1 - m };
                    for b in s.rows[y].iter_mut() {
                        *b = r.next() as u8;
                    }
                }
            }
            "poke" => {
                let (x, y) = (n(op, 2)? as i32, n(op, 3)? as i32);
                let hex = op.tok.get(4).ok_or(Fail::Bad)?.clone();
                let s = &mut self.slot(op, 1)?.s;
                if s.bpp < 8 || x < 0 || y < 0 || x >= s.w || y >= s.h {
                    return Err(Fail::Bad);
                }
                let bytes = (s.bpp / 8) as usize;
                for i in 0..bytes.min(hex.len() / 2) {
                    s.rows[y as usize][x as usize * bytes + i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| Fail::Bad)?;
                }
            }
            "dump" => {
                let k = n(op, 1)?;
                let s = &self.slot(op, 1)?.s;
                out.push(format!("px {} {} {} {}", k, s.w, s.h, s.bpp));
                let bytes = (s.w as usize * s.bpp as usize + 7) / 8;
                for r in &s.rows {
                    out.push(r[..bytes].iter().map(|b| format!("{b:02x}")).collect());
                }
            }
            "brush" => {
                let kind = op.tok.get(2).ok_or(Fail::Bad)?.clone();
                let b = match kind.as_str() {
                    "solid" => Brush::Solid(c(op, 3)?),
                    "hatch" => Brush::Hatch(n(op, 3)?, c(op, 4)?),
                    "dib" => {
                        let mut p = self.slot(op, 3)?.s.clone();
                        // (the executor packs the DIB as BI_RGB: a 5-6-5 picture is read as 5-5-5)
                        p.f565 = false;
                        Brush::Pattern(p)
                    }
                    "null" => Brush::Null,
                    _ => return Err(Fail::Bad),
                };
                self.slot(op, 1)?.brush = b;
            }
            "text" => self.slot(op, 1)?.text = c(op, 2)?,
            "bk" => self.slot(op, 1)?.bk = c(op, 2)?,
            "bkmode" => self.slot(op, 1)?.bkmode = n(op, 2)?,
            "org" => {
                let (x, y) = (n(op, 2)? as i32, n(op, 3)? as i32);
                self.slot(op, 1)?.org = (x, y);
            }
            "clip" => {
                let r = (n(op, 2)? as i32, n(op, 3)? as i32, n(op, 4)? as i32, n(op, 5)? as i32);
                hit("GDI.CLIP.INTERSECT");
                if r.0 > r.2 || r.1 > r.3 {
                    return Err(Fail::Unmodeled("GDI.CLIP.UNORDERED"));
                }
                let sl = self.slot(op, 1)?;
                let c = match sl.clip {
                    None => r,
                    // (what it returns on a DC with a clip region already: not measured yet)
                    Some(_) => return Err(Fail::Unmodeled("GDI.CLIP.AGAIN")),
                };
                sl.clip = Some(c);
                // MEASURED: COMPLEXREGION (3) on a memory DC without a clip region, whatever
                // the rectangle (the documentation lists NULLREGION / SIMPLEREGION / COMPLEXREGION)
                hit("GDI.CLIP.RESULT");
                out.push("clip 3".into());
            }
            "noclip" => self.slot(op, 1)?.clip = None,
            "mode" => self.slot(op, 1)?.mode = n(op, 2)?,
            "stretchblt" => {
                let v: Vec<i32> = (2..12).filter(|&i| i != 6).map(|i| n(op, i).map(|v| v as i32)).collect::<Result<_, _>>()?;
                let (k, ks) = (n(op, 1)? as usize, n(op, 6)? as usize);
                self.slot(op, 1)?;
                self.slot(op, 6)?;
                let (x, y, w, h, sx, sy, sw, sh, rop) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8] as u32);
                if self.slots[k].as_ref().unwrap().mode == 4 {
                    // MEASURED: HALFTONE differs from BitBlt even unscaled, between formats
                    return Err(Fail::Unmodeled("GDI.STRETCH.HALFTONE"));
                }
                if w == sw && h == sh {
                    // (an unscaled StretchBlt is BitBlt)
                    hit("GDI.STRETCH.SAME");
                    let r = self.blt(k, x, y, w, h, Some((ks, sx, sy)), rop, false)?;
                    out.push(format!("ret {}", r as u8));
                } else {
                    let r = self.stretch(k, x, y, w, h, ks, sx, sy, sw, sh, rop)?;
                    out.push(format!("ret {}", r as u8));
                }
            }
            "getpixel" | "setpixel" => {
                let (x, y) = (n(op, 2)? as i32, n(op, 3)? as i32);
                let c = if t0 == "setpixel" { Some(c(op, 4)?) } else { None };
                let sl = self.slot(op, 1)?;
                let inside = x >= 0 && y >= 0 && x < sl.s.w && y < sl.s.h && sl.clip.is_none_or(|r| x >= r.0 && x < r.2 && y >= r.1 && y < r.3);
                hit(if c.is_some() { "GDI.PIXEL.SET" } else { "GDI.PIXEL.GET" });
                if !inside {
                    out.push("pixel invalid".into());
                } else if let Some(c) = c {
                    if sl.s.bpp == 1 {
                        return Err(Fail::Unmodeled("GDI.PIXEL.SETMONO"));
                    }
                    let v = sl.s.from_rgb(c);
                    sl.s.set(x, y, v);
                    out.push(format!("pixel {:06x}", sl.s.to_rgb(v)));
                } else {
                    out.push(format!("pixel {:06x}", sl.s.to_rgb(sl.s.get(x, y))));
                }
            }
            "getdibits" | "setdibits" => {
                let bpp = n(op, 2)? as u32;
                let f565 = op.tok.get(3).is_some_and(|t| t == "565");
                let seed = if t0 == "setdibits" { n(op, if f565 { 4 } else { 3 })? as u32 } else { 0 };
                let sl = self.slot(op, 1)?;
                let (w, h) = (sl.s.w, sl.s.h);
                // MEASURED: GetDIBits to 16 bpp gives the bitmap's own 16 bpp format, else 5-5-5,
                // whatever masks BI_BITFIELDS asks for (pixel golden)
                let f565 = if t0 == "getdibits" && bpp == 16 && f565 {
                    hit("GDI.DIBITS.GET16");
                    sl.s.bpp == 16 && sl.s.f565
                } else {
                    f565
                };
                let mut o = Surf::new(w, h, bpp, f565, true, default_palette(bpp));
                if bpp <= 8 && t0 == "getdibits" {
                    return Err(Fail::Unmodeled("GDI.DIBITS.GETPAL"));
                }
                if (bpp == 1) != (sl.s.bpp == 1) {
                    return Err(Fail::Unmodeled("GDI.BLT.MONO"));
                }
                hit(if t0 == "getdibits" { "GDI.DIBITS.GET" } else { "GDI.DIBITS.SET" });
                if t0 == "getdibits" {
                    for y in 0..h {
                        for x in 0..w {
                            let v = sl.s.get(x, y);
                            o.set(x, y, if o.same_format(&sl.s) { v } else { o.from_rgb(sl.s.to_rgb(v)) });
                        }
                    }
                    out.push(format!("dibits {h}"));
                    let bytes = (w as usize * bpp as usize + 7) / 8;
                    for r in &o.rows {
                        out.push(r[..bytes].iter().map(|b| format!("{b:02x}")).collect());
                    }
                } else {
                    let mut r = XorShift32::new(seed);
                    for row in o.rows.iter_mut() {
                        for b in row.iter_mut() {
                            *b = r.next() as u8;
                        }
                    }
                    for y in 0..h {
                        for x in 0..w {
                            let v = o.get(x, y);
                            let d = if sl.s.same_format(&o) { v } else { sl.s.from_rgb(o.to_rgb(v)) };
                            sl.s.set(x, y, d);
                        }
                    }
                    out.push(format!("setdibits {h}"));
                }
            }
            "patblt" => {
                let (x, y, w, h, rop) = (n(op, 2)?, n(op, 3)?, n(op, 4)?, n(op, 5)?, n(op, 6)?);
                let k = n(op, 1)? as usize;
                self.slot(op, 1)?;
                let r = self.blt(k, x as i32, y as i32, w as i32, h as i32, None, rop as u32, true)?;
                out.push(format!("ret {}", r as u8));
            }
            "bitblt" => {
                let (x, y, w, h) = (n(op, 2)?, n(op, 3)?, n(op, 4)?, n(op, 5)?);
                let (sx, sy, rop) = (n(op, 7)?, n(op, 8)?, n(op, 9)?);
                let k = n(op, 1)? as usize;
                let ks = n(op, 6)? as usize;
                self.slot(op, 1)?;
                self.slot(op, 6)?;
                let r = self.blt(k, x as i32, y as i32, w as i32, h as i32, Some((ks, sx as i32, sy as i32)), rop as u32, false)?;
                out.push(format!("ret {}", r as u8));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// StretchBlt, scaled: COLORONCOLOR (and any mode when no axis shrinks) samples the source
    /// pixel under each destination pixel's center, floor((2i + 1) * sw / 2w) (Bresenham-like
    /// DDA; MEASURED once Windows' answers come)
    #[allow(clippy::too_many_arguments)]
    pub fn stretch(&mut self, k: usize, x: i32, y: i32, w: i32, h: i32, ks: usize, sx: i32, sy: i32, sw: i32, sh: i32, rop: u32) -> Res<bool> {
        if w <= 0 || h <= 0 || sw <= 0 || sh <= 0 {
            return Err(Fail::Unmodeled("GDI.STRETCH.MIRROR"));
        }
        if k == 7 || ks == 7 {
            return Err(Fail::Unmodeled("GDI.DIB.PARAMS"));
        }
        let mode = self.slots[k].as_ref().unwrap().mode;
        if mode == 4 {
            // MEASURED: HALFTONE filters when enlarging too (stretch golden)
            return Err(Fail::Unmodeled("GDI.STRETCH.HALFTONE"));
        }
        if (sw > w || sh > h) && mode != 3 {
            return Err(Fail::Unmodeled("GDI.STRETCH.COMBINE"));
        }
        let src = self.slots[ks].as_ref().unwrap();
        if sx < 0 || sy < 0 || sx + sw > src.s.w || sy + sh > src.s.h {
            return Err(Fail::Unmodeled("GDI.STRETCH.SRCOUT"));
        }
        hit("GDI.STRETCH.SAMPLE");
        // the sampled source as a picture of the destination's size, then an unscaled blit
        let mut tmp = Surf::new(w, h, src.s.bpp, src.s.f565, true, src.s.pal.clone());
        for j in 0..h {
            for i in 0..w {
                let (qx, qy) = (sx + ((2 * i + 1) * sw) / (2 * w), sy + ((2 * j + 1) * sh) / (2 * h));
                tmp.set(i, j, src.s.get(qx, qy));
            }
        }
        let mut t = src.clone();
        t.s = tmp;
        let saved = self.slots[7].replace(t);
        let r = self.blt(k, x, y, w, h, Some((7, 0, 0)), rop, false);
        self.slots[7] = saved;
        r
    }

    /// PatBlt (src None) and BitBlt into slot `k`
    #[allow(clippy::too_many_arguments)]
    pub fn blt(&mut self, k: usize, x: i32, y: i32, w: i32, h: i32, src: Option<(usize, i32, i32)>, rop: u32, pat: bool) -> Res<bool> {
        let idx = (rop >> 16) as u8;
        if w < 0 || h < 0 {
            return Err(Fail::Unmodeled("GDI.BLT.NEGATIVE"));
        }
        if rop & 0xff00_0000 != 0 {
            return Err(Fail::Unmodeled("GDI.ROP3.FLAGS"));
        }
        if pat && uses_s(idx) {
            // PatBlt documents only operations without a source
            return Err(Fail::Unmodeled("GDI.PATBLT.SRCOP"));
        }
        hit("GDI.ROP3.TRUTH");
        if rop & 0xffff != 0 {
            hit("GDI.ROP3.LOWWORD");
        }
        hit(if pat { "GDI.PATBLT.ROPS" } else if uses_s(idx) { "GDI.BLT.CONVERT" } else { "GDI.ROP3.NOSRC" });
        if src.is_some_and(|(ks, ..)| ks == k) && uses_s(idx) {
            hit("GDI.BLT.OVERLAP");
        }
        let d = self.slots[k].as_ref().unwrap();
        let sp = if uses_s(idx) { src.map(|(ks, sx, sy)| (self.slots[ks].as_ref().unwrap().clone(), sx, sy)) } else { None };
        if uses_p(idx) {
            match &d.brush {
                Brush::Null => return Err(Fail::Unmodeled("GDI.BRUSH.NULL")),
                Brush::Hatch(..) => return Err(Fail::Unmodeled("GDI.BRUSH.HATCH")),
                Brush::Pattern(p) if p.bpp == 1 => return Err(Fail::Unmodeled("GDI.BRUSH.MONOPATTERN")),
                Brush::Solid(_) => hit(if d.s.bpp <= 8 { "GDI.BRUSH.SOLIDPAL" } else { "GDI.BRUSH.SOLID" }),
                Brush::Pattern(_) => {
                    hit("GDI.BRUSH.DIBPATTERN");
                    hit("GDI.BRUSH.ORIGIN");
                }
            }
        }
        if let Some((s, ..)) = &sp {
            if (s.s.bpp == 1) != (d.s.bpp == 1) {
                return Err(Fail::Unmodeled("GDI.BLT.MONO"));
            }
        }
        let d = self.slots[k].as_mut().unwrap();
        hit("GDI.FMT.LAYOUT");
        hit("GDI.BLT.CLIP");
        let (mut x0, mut y0, mut x1, mut y1) = (x.max(0), y.max(0), (x + w).min(d.s.w), (y + h).min(d.s.h));
        if let Some(c) = d.clip {
            x0 = x0.max(c.0);
            y0 = y0.max(c.1);
            x1 = x1.min(c.2);
            y1 = y1.min(c.3);
        }
        let mask = d.s.mask();
        let solid = match &d.brush {
            Brush::Solid(c) => d.s.from_rgb(*c),
            _ => 0,
        };
        for py in y0..y1 {
            for px in x0..x1 {
                let sv = match &sp {
                    None => 0,
                    Some((s, sx, sy)) => {
                        let (qx, qy) = (sx + px - x, sy + py - y);
                        if qx < 0 || qy < 0 || qx >= s.s.w || qy >= s.s.h {
                            // source pixels outside the source bitmap draw nothing
                            hit("GDI.BLT.SRCOUT");
                            continue;
                        }
                        let v = s.s.get(qx, qy);
                        if s.s.same_format(&d.s) {
                            hit("GDI.FMT.RAW");
                            v
                        } else {
                            d.s.from_rgb(s.s.to_rgb(v))
                        }
                    }
                };
                let pv = match &d.brush {
                    Brush::Pattern(p) => {
                        let v = p.get((px - d.org.0).rem_euclid(p.w), (py - d.org.1).rem_euclid(p.h));
                        if p.same_format(&d.s) { v } else { d.s.from_rgb(p.to_rgb(v)) }
                    }
                    _ => solid,
                };
                let dv = d.s.get(px, py);
                d.s.set(px, py, rop3(idx, pv, sv, dv) & mask);
            }
        }
        Ok(true)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;
    /// rop3 is the documented truth table on every bit, for every operation and operands
    #[kani::proof]
    fn rop3_truth_table() {
        let (idx, p, s, d): (u8, u32, u32, u32) = (kani::any(), kani::any(), kani::any(), kani::any());
        let bit: u32 = kani::any();
        kani::assume(bit < 32);
        let k = (p >> bit & 1) << 2 | (s >> bit & 1) << 1 | (d >> bit & 1);
        assert_eq!(rop3(idx, p, s, d) >> bit & 1, (idx as u32) >> k & 1);
    }
}
