//! DirectDraw 7 blits between system-memory offscreen surfaces: an executable specification of
//! IDirectDrawSurface7::Blt (copy, color fill, source and destination color keys and their
//! overrides, mirroring) and BltFast, with Lock showing the bytes.
//!
//! Sources (Microsoft Learn): "IDirectDrawSurface7::Blt", "IDirectDrawSurface7::BltFast",
//! "DDBLTFX", "DDCOLORKEY", "Color Keying" (a source key's pixels are not copied; a destination
//! key's pixels are the only ones written). MEASURED (masu's tests/winref/golden/ddraw-32): a
//! rectangle past the surface or empty is DDERR_INVALIDRECT, without a clipper.

use super::{Fail, Res};
use crate::case::{num, Op};
use crate::spec::hit;

pub const DDERR_INVALIDRECT: u32 = 0x8876_0096;

#[derive(Clone)]
pub struct Surf {
    pub w: i64,
    pub h: i64,
    pub bpp: u32,
    pub px: Vec<u32>,
    pub src_key: Option<u32>,
    pub dest_key: Option<u32>,
}

impl Surf {
    fn get(&self, x: i64, y: i64) -> u32 {
        self.px[(y * self.w + x) as usize]
    }
    fn set(&mut self, x: i64, y: i64, v: u32) {
        self.px[(y * self.w + x) as usize] = v;
    }
    fn mask(&self) -> u32 {
        if self.bpp == 32 { !0 } else { (1 << self.bpp) - 1 }
    }
}

#[derive(Default)]
pub struct Ddraw {
    dev: bool,
    s: [Option<Surf>; 8],
}

type Rect = (i64, i64, i64, i64);

fn n(op: &Op, i: usize) -> Res<i64> {
    op.tok.get(i).and_then(|t| num(t)).ok_or(Fail::Bad)
}

/// a rect at token i ("all" or 4 numbers) and the next token
fn rect(op: &Op, i: usize) -> Res<(Option<Rect>, usize)> {
    if op.tok.get(i).is_some_and(|t| t == "all") {
        return Ok((None, i + 1));
    }
    Ok((Some((n(op, i)?, n(op, i + 1)?, n(op, i + 2)?, n(op, i + 3)?)), i + 4))
}

/// the rect inside the surface and not empty (MEASURED: else DDERR_INVALIDRECT)
fn check(r: Option<Rect>, s: &Surf) -> Option<Rect> {
    let (x0, y0, x1, y1) = r.unwrap_or((0, 0, s.w, s.h));
    hit("DDRAW.RECT");
    (x0 >= 0 && y0 >= 0 && x1 <= s.w && y1 <= s.h && x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
}

struct Keys {
    src: Option<u32>,
    dest: Option<u32>,
    lr: bool,
    ud: bool,
}

impl Ddraw {
    fn slot(&self, op: &Op, i: usize) -> Res<usize> {
        let s = n(op, i)?;
        if !(0..8).contains(&s) || self.s[s as usize].is_none() {
            return Err(Fail::Bad);
        }
        Ok(s as usize)
    }

    /// a copy of src's rect sr to d's rect dr (equal sizes), as the keys say
    fn copy(&mut self, d: usize, dr: Rect, src: &Surf, sr: Rect, k: &Keys) -> Res<()> {
        let (w, h) = (dr.2 - dr.0, dr.3 - dr.1);
        if (w, h) != (sr.2 - sr.0, sr.3 - sr.1) {
            return Err(Fail::Unmodeled("DDRAW.STRETCH"));
        }
        let dst = self.s[d].as_mut().unwrap();
        if dst.bpp != src.bpp {
            return Err(Fail::Unmodeled("DDRAW.FORMATS"));
        }
        hit("DDRAW.COPY");
        if src.bpp == 32 && (k.src.is_some_and(|v| v >> 24 != 0) || k.dest.is_some_and(|v| v >> 24 != 0)) {
            hit("DDRAW.KEY.BITS");
        }
        if k.lr || k.ud {
            hit("DDRAW.MIRROR");
        }
        for y in 0..h {
            for x in 0..w {
                let sx = if k.lr { sr.2 - 1 - x } else { sr.0 + x };
                let sy = if k.ud { sr.3 - 1 - y } else { sr.1 + y };
                let v = src.get(sx, sy);
                if k.src == Some(v) {
                    continue;
                }
                if let Some(dk) = k.dest {
                    if dst.get(dr.0 + x, dr.1 + y) != dk {
                        continue;
                    }
                }
                dst.set(dr.0 + x, dr.1 + y, v);
            }
        }
        Ok(())
    }

    pub fn op(&mut self, op: &Op, out: &mut Vec<String>) -> Res<bool> {
        let t = op.tok.as_slice();
        match t[0].as_str() {
            "dd" => {
                *self = Ddraw { dev: true, ..Default::default() };
                hit("DDRAW.DEVICE");
                out.push("dd ok".into());
            }
            "ddsurf" | "ddfill" | "dddump" | "ddkey" | "ddblt" | "ddcolor" | "ddfast" if !self.dev => out.push("nodev".into()),
            "ddsurf" => {
                let (i, w, h) = (n(op, 1)?, n(op, 2)?, n(op, 3)?);
                if !(0..8).contains(&i) || !(1..=256).contains(&w) || !(1..=256).contains(&h) {
                    return Err(Fail::Bad);
                }
                let bpp = match t.get(4).map(String::as_str) {
                    Some("8") => 8,
                    Some("555" | "565") => 16,
                    Some("32") => 32,
                    _ => return Err(Fail::Bad),
                };
                // (the pixels a new surface starts with are not documented: cases fill first)
                self.s[i as usize] = Some(Surf { w, h, bpp, px: vec![0; (w * h) as usize], src_key: None, dest_key: None });
            }
            "ddfill" => {
                let s = self.slot(op, 1)?;
                let mut r = crate::rng::XorShift32::new(n(op, 2)? as u32);
                let surf = self.s[s].as_mut().unwrap();
                let bytes = surf.bpp / 8;
                for p in surf.px.iter_mut() {
                    let mut v = 0;
                    for b in 0..bytes {
                        v |= (r.next() & 0xff) << (8 * b);
                    }
                    *p = v;
                }
            }
            "dddump" => {
                let s = self.slot(op, 1)?;
                let surf = self.s[s].as_ref().unwrap();
                out.push(format!("dd {s} {} {} {}", surf.w, surf.h, surf.bpp));
                for row in surf.px.chunks(surf.w as usize) {
                    out.push(row.iter().map(|v| v.to_le_bytes()[..surf.bpp as usize / 8].iter().map(|b| format!("{b:02x}")).collect::<String>()).collect());
                }
            }
            "ddkey" => {
                let s = self.slot(op, 1)?;
                let v = if t.get(3).is_some_and(|v| v == "none") { None } else { Some(n(op, 3)? as u32) };
                let surf = self.s[s].as_mut().unwrap();
                let v = v.map(|v| v & surf.mask());
                match t.get(2).map(String::as_str) {
                    Some("src") => surf.src_key = v,
                    Some("dest") => surf.dest_key = v,
                    _ => return Err(Fail::Bad),
                }
                hit("DDRAW.KEY.SET");
            }
            "ddcolor" => {
                let d = self.slot(op, 1)?;
                let (r, i) = rect(op, 2)?;
                let v = n(op, i)? as u32;
                if t.len() != i + 1 {
                    return Err(Fail::Bad);
                }
                let surf = self.s[d].as_mut().unwrap();
                let Some(r) = check(r, surf) else {
                    out.push(format!("ret {DDERR_INVALIDRECT:08x}"));
                    return Ok(true);
                };
                if v & !surf.mask() != 0 {
                    return Err(Fail::Unmodeled("DDRAW.FILL.HIGHBITS"));
                }
                hit("DDRAW.FILL");
                for y in r.1..r.3 {
                    for x in r.0..r.2 {
                        surf.set(x, y, v);
                    }
                }
                out.push("ret 00000000".into());
            }
            "ddblt" => {
                let d = self.slot(op, 1)?;
                let (dr, i) = rect(op, 2)?;
                let s = if t.get(i).is_some_and(|v| v == "-") { None } else { Some(self.slot(op, i)?) };
                let (sr, mut i) = rect(op, i + 1)?;
                let mut k = Keys { src: None, dest: None, lr: false, ud: false };
                let src = s.map(|s| self.s[s].clone().unwrap());
                let dst = self.s[d].as_ref().unwrap();
                while i < t.len() {
                    match t[i].as_str() {
                        "keysrc" => k.src = Some(src.as_ref().and_then(|s| s.src_key).ok_or(Fail::Unmodeled("DDRAW.KEY.UNSET"))?),
                        "keydest" => k.dest = Some(dst.dest_key.ok_or(Fail::Unmodeled("DDRAW.KEY.UNSET"))?),
                        "mirrorlr" => k.lr = true,
                        "mirrorud" => k.ud = true,
                        "keysrcov" | "keydestov" => {
                            i += 1;
                            let v = n(op, i)? as u32;
                            hit("DDRAW.KEY.OVERRIDE");
                            if t[i - 1] == "keysrcov" {
                                k.src = Some(v & src.as_ref().map_or(!0, |s| s.mask()));
                            } else {
                                k.dest = Some(v & dst.mask());
                            }
                        }
                        _ => return Err(Fail::Bad),
                    }
                    i += 1;
                }
                if k.src.is_some() || k.dest.is_some() {
                    hit("DDRAW.KEY");
                }
                let Some(src) = src else { return Err(Fail::Unmodeled("DDRAW.NOSOURCE")) };
                let (Some(dr), Some(sr)) = (check(dr, dst), check(sr, &src)) else {
                    out.push(format!("ret {DDERR_INVALIDRECT:08x}"));
                    return Ok(true);
                };
                if s == Some(d) {
                    hit("DDRAW.OVERLAP");
                }
                self.copy(d, dr, &src, sr, &k)?;
                out.push("ret 00000000".into());
            }
            "ddfast" => {
                let d = self.slot(op, 1)?;
                let (x, y) = (n(op, 2)?, n(op, 3)?);
                let s = self.slot(op, 4)?;
                let (sr, mut i) = rect(op, 5)?;
                let src = self.s[s].clone().unwrap();
                let dst = self.s[d].as_ref().unwrap();
                let mut k = Keys { src: None, dest: None, lr: false, ud: false };
                while i < t.len() {
                    match t[i].as_str() {
                        "srckey" => k.src = Some(src.src_key.ok_or(Fail::Unmodeled("DDRAW.KEY.UNSET"))?),
                        "destkey" => k.dest = Some(dst.dest_key.ok_or(Fail::Unmodeled("DDRAW.KEY.UNSET"))?),
                        _ => return Err(Fail::Bad),
                    }
                    i += 1;
                }
                if k.src.is_some() && k.dest.is_some() {
                    return Err(Fail::Unmodeled("DDRAW.FAST.BOTHKEYS"));
                }
                if k.src.is_some() || k.dest.is_some() {
                    hit("DDRAW.KEY");
                }
                hit("DDRAW.FAST");
                let Some(sr) = check(sr, &src) else {
                    out.push(format!("ret {DDERR_INVALIDRECT:08x}"));
                    return Ok(true);
                };
                let dr = (x, y, x + sr.2 - sr.0, y + sr.3 - sr.1);
                let Some(dr) = check(Some(dr), dst) else {
                    out.push(format!("ret {DDERR_INVALIDRECT:08x}"));
                    return Ok(true);
                };
                if s == d {
                    hit("DDRAW.OVERLAP");
                }
                self.copy(d, dr, &src, sr, &k)?;
                out.push("ret 00000000".into());
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
