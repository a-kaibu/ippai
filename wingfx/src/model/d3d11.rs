//! Direct3D 11 state objects: an executable specification of CreateBlendState,
//! CreateDepthStencilState, CreateRasterizerState and CreateSamplerState (which descriptions
//! they accept), of GetDesc (the description the object keeps) and of the runtime giving back
//! the same object for the same description.
//!
//! Sources (Microsoft Learn): "D3D11_BLEND_DESC", "D3D11_RENDER_TARGET_BLEND_DESC",
//! "D3D11_BLEND" (_COLOR options not allowed for alpha), "D3D11_BLEND_OP",
//! "D3D11_DEPTH_STENCIL_DESC", "D3D11_DEPTH_STENCILOP_DESC", "D3D11_STENCIL_OP",
//! "D3D11_COMPARISON_FUNC", "D3D11_RASTERIZER_DESC", "D3D11_FILL_MODE", "D3D11_CULL_MODE",
//! "D3D11_SAMPLER_DESC" (MaxAnisotropy 1..16, MipLODBias -16..15.99, comparison func only for
//! comparison filters, border color only with D3D11_TEXTURE_ADDRESS_BORDER), "D3D11_FILTER",
//! "ID3D11Device::CreateBlendState" and the other three (ppState NULL validates and returns
//! S_FALSE; the same description gives the same object).
//! MEASURED (masu's tests/winref/golden/d3d11desc-32): what a disabled part of a description
//! becomes, BOOLs stored as 1, write masks over 0xf refused, MaxAnisotropy 0 accepted,
//! NaN DepthBiasClamp and MinLOD refused, the same object for descriptions equal after that.

use super::{Fail, Res};
use crate::case::{num, Op};
use crate::spec::hit;

pub const E_INVALIDARG: u32 = 0x8007_0057;
pub const S_FALSE: u32 = 1;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Blend,
    Depth,
    Raster,
    Sampler,
}
use Kind::*;

impl Kind {
    pub const ALL: [Kind; 4] = [Blend, Depth, Raster, Sampler];
    pub fn name(self) -> &'static str {
        ["blend", "depth", "raster", "sampler"][self as usize]
    }
    /// the description's size in 32-bit words
    pub fn words(self) -> usize {
        [66, 13, 10, 13][self as usize]
    }
    pub fn of(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// the defaults the documentation gives (CD3D11_*_DESC(D3D11_DEFAULT))
pub fn default_desc(k: Kind) -> Vec<u32> {
    match k {
        Blend => {
            let mut d = vec![0, 0];
            for _ in 0..8 {
                d.extend([0, 2, 1, 1, 2, 1, 1, 0xf]);
            }
            d
        }
        Depth => vec![1, 1, 2, 0, 0xffff, 1, 1, 1, 8, 1, 1, 1, 8],
        Raster => vec![3, 3, 0, 0, 0, 0, 1, 0, 0, 0],
        Sampler => vec![0x15, 3, 3, 3, 0, 1, 1, 0x3f80_0000, 0x3f80_0000, 0x3f80_0000, 0x3f80_0000, 0xff7f_ffff, 0x7f7f_ffff],
    }
}

fn f(v: u32) -> f32 {
    f32::from_bits(v)
}

fn boolean(v: &mut u32, id: &'static str) {
    if *v > 1 {
        hit(id);
    }
    *v = (*v != 0) as u32;
}

/// Some(the description GetDesc gives) when Create... accepts `d`, None when it refuses it
pub fn normalize(k: Kind, d: &[u32]) -> Option<Vec<u32>> {
    let mut d = d.to_vec();
    let ok = |c: bool, id: &'static str| {
        hit(id);
        c.then_some(())
    };
    match k {
        Blend => {
            boolean(&mut d[0], "D3D11.BLEND.BOOL");
            boolean(&mut d[1], "D3D11.BLEND.BOOL");
            let blend = |v: u32| matches!(v, 1..=11 | 14..=19);
            let alpha = |v: u32| blend(v) && !matches!(v, 3 | 4 | 9 | 10 | 16 | 17);
            let op = |v: u32| (1..=5).contains(&v);
            let used = if d[1] != 0 { 8 } else { 1 };
            for t in 0..8 {
                let rt = &mut d[2 + t * 8..10 + t * 8];
                if t >= used {
                    hit("D3D11.BLEND.SHARED");
                    continue;
                }
                if rt[7] > 0xff {
                    hit("D3D11.PAD");
                }
                rt[7] &= 0xff;
                ok(rt[7] <= 0xf, "D3D11.BLEND.MASK")?;
                boolean(&mut rt[0], "D3D11.BLEND.BOOL");
                if rt[0] != 0 {
                    ok(blend(rt[1]) && blend(rt[2]) && op(rt[3]), "D3D11.BLEND.ENUM")?;
                    ok(blend(rt[4]) && blend(rt[5]) && op(rt[6]), "D3D11.BLEND.ENUM")?;
                    ok(alpha(rt[4]) && alpha(rt[5]), "D3D11.BLEND.ALPHA")?;
                } else {
                    hit("D3D11.BLEND.DISABLED");
                    rt[1..7].copy_from_slice(&[2, 1, 1, 2, 1, 1]);
                }
            }
            if used == 1 {
                let rt0 = d[2..10].to_vec();
                for t in 1..8 {
                    d[2 + t * 8..10 + t * 8].copy_from_slice(&rt0);
                }
            }
        }
        Depth => {
            boolean(&mut d[0], "D3D11.DEPTH.BOOL");
            ok(d[1] <= 1, "D3D11.DEPTH.ENUM")?;
            if d[0] != 0 {
                ok((1..=8).contains(&d[2]), "D3D11.DEPTH.ENUM")?;
            } else {
                hit("D3D11.DEPTH.OFF");
                (d[1], d[2]) = (1, 2);
            }
            boolean(&mut d[3], "D3D11.DEPTH.BOOL");
            if d[4] > 0xffff {
                hit("D3D11.PAD");
            }
            d[4] &= 0xffff;
            if d[3] != 0 {
                ok(d[5..13].iter().all(|v| (1..=8).contains(v)), "D3D11.DEPTH.ENUM")?;
            } else {
                hit("D3D11.STENCIL.OFF");
                d[4] = 0xffff;
                for face in 0..2 {
                    d[5 + 4 * face..9 + 4 * face].copy_from_slice(&[1, 1, 1, 8]);
                }
            }
        }
        Raster => {
            ok((2..=3).contains(&d[0]) && (1..=3).contains(&d[1]), "D3D11.RASTER.ENUM")?;
            ok(!f(d[4]).is_nan(), "D3D11.RASTER.CLAMP")?;
            if !f(d[5]).is_finite() {
                hit(if f(d[5]).is_nan() { "D3D11.RASTER.SLOPENAN" } else { "D3D11.RASTER.SLOPE" });
            }
            for i in [2, 6, 7, 8, 9] {
                boolean(&mut d[i], "D3D11.RASTER.BOOL");
            }
        }
        Sampler => {
            let filter = d[0];
            ok(filter & !0x1ff == 0 && matches!(filter & 0x7f, 0 | 1 | 4 | 5 | 0x10 | 0x11 | 0x14 | 0x15 | 0x55), "D3D11.SAMPLER.FILTER")?;
            ok(d[1..4].iter().all(|v| (1..=5).contains(v)), "D3D11.SAMPLER.ADDRESS")?;
            ok(d[5] <= 16, "D3D11.SAMPLER.ANISO")?;
            if filter & 0x7f != 0x55 {
                d[5] = 0;
            }
            if filter & 0x180 == 0x80 {
                ok((1..=8).contains(&d[6]), "D3D11.SAMPLER.CMP")?;
            } else {
                hit("D3D11.SAMPLER.CMP");
                d[6] = 1;
            }
            let bias = f(d[4]);
            if bias.is_nan() {
                hit("D3D11.SAMPLER.BIASNAN");
            } else if bias.abs() >= 15.98 && bias.abs() <= 16.01 {
                hit("D3D11.SAMPLER.BIASEDGE");
            }
            ok((-16.0..=15.99).contains(&bias), "D3D11.SAMPLER.BIAS")?;
            ok(!f(d[11]).is_nan(), "D3D11.SAMPLER.LOD")?;
            if f(d[12]).is_nan() {
                hit("D3D11.SAMPLER.MAXLODNAN");
            }
            ok(!f(d[12]).is_nan(), "D3D11.SAMPLER.LOD")?;
            if d[1..4].contains(&4) {
                hit("D3D11.SAMPLER.BORDERKEEP");
            } else {
                hit("D3D11.SAMPLER.BORDER");
                d[7..11].fill(0);
            }
        }
    }
    Some(d)
}

/// equal as GetDesc gives them, and whether only as floats (0.0 and -0.0)
fn same_bits(k: Kind, a: &[u32], b: &[u32]) -> (bool, bool) {
    if a == b {
        return (true, false);
    }
    let floats: &[usize] = match k {
        Raster => &[4, 5],
        Sampler => &[4, 7, 8, 9, 10, 11, 12],
        _ => &[],
    };
    let close = a.iter().zip(b).enumerate().all(|(i, (x, y))| x == y || (floats.contains(&i) && f(*x) == f(*y)));
    (false, close)
}

#[derive(Default)]
pub struct D3d11 {
    dev: bool,
    /// every state line of the case: the object's kind and description (None: no object)
    objs: Vec<Option<(Kind, Vec<u32>)>>,
}

impl D3d11 {
    pub fn op(&mut self, op: &Op, out: &mut Vec<String>) -> Res<bool> {
        let t = op.tok.as_slice();
        if t[0] == "device11" {
            hit("D3D11.DEVICE");
            self.dev = true;
            self.objs.clear();
            out.push("device11 ok".into());
            return Ok(true);
        }
        let Some(k) = Kind::of(&t[0]) else { return Ok(false) };
        if !self.dev {
            out.push("nodev".into());
            return Ok(true);
        }
        let check = t.get(1).is_some_and(|s| s == "check");
        let nodesc = t.get(1).is_some_and(|s| s == "nodesc");
        let at = 1 + check as usize + nodesc as usize;
        if self.objs.len() >= 64 {
            return Err(Fail::Bad);
        }
        if nodesc {
            if t.len() != 2 {
                return Err(Fail::Bad);
            }
            hit("D3D11.NODESC");
            self.objs.push(None);
            out.push(format!("{} hr {E_INVALIDARG:08x}", k.name()));
            return Ok(true);
        }
        if t.len() - at > k.words() {
            return Err(Fail::Bad);
        }
        let mut d: Vec<u32> = t[at..].iter().map(|s| num(s).map(|v| v as u32).ok_or(Fail::Bad)).collect::<Res<_>>()?;
        d.resize(k.words(), 0);
        let n = normalize(k, &d);
        if check {
            hit("D3D11.CHECK");
            self.objs.push(None);
            let hr = if n.is_some() { S_FALSE } else { E_INVALIDARG };
            out.push(format!("{} hr {hr:08x}", k.name()));
            return Ok(true);
        }
        let Some(n) = n else {
            self.objs.push(None);
            out.push(format!("{} hr {E_INVALIDARG:08x}", k.name()));
            return Ok(true);
        };
        let mut same = None;
        for (i, o) in self.objs.iter().enumerate() {
            if let Some((ok, od)) = o {
                if *ok != k {
                    continue;
                }
                match same_bits(k, od, &n) {
                    (true, _) => {
                        same = Some(i);
                        break;
                    }
                    (false, true) => return Err(Fail::Unmodeled("D3D11.DEDUP.FLOAT")),
                    _ => {}
                }
            }
        }
        hit("D3D11.DEDUP");
        let words: Vec<String> = n.iter().map(|w| format!("{w:x}")).collect();
        let same = same.map_or("-".to_string(), |i| i.to_string());
        out.push(format!("{} = {} same {same}", k.name(), words.join(" ")));
        self.objs.push(Some((k, n)));
        Ok(true)
    }
}
