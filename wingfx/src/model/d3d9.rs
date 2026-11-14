//! Direct3D 9 device state: an executable specification of SetRenderState /
//! SetTextureStageState / SetSamplerState and their Get, and of state blocks (CreateStateBlock
//! of each D3DSTATEBLOCKTYPE, Begin/EndStateBlock recording, Capture, Apply).
//!
//! Sources (Microsoft Learn): "D3DRENDERSTATETYPE", "D3DTEXTURESTAGESTATETYPE",
//! "D3DSAMPLERSTATETYPE" (states and their defaults), "State Blocks Save and Restore State
//! (Direct3D 9)" and "Pixel / Vertex states" lists (what each state block type captures),
//! "IDirect3DDevice9::BeginStateBlock" (calls while recording are recorded, not applied).
//! MEASURED: what the documentation does not say, taken from Windows' answers (masu's
//! tests/winref/golden/d3d9state-32 and d3d9sb-32): which undocumented states can be read and
//! what they read, rs 40 being a pixel state, DMAPOFFSET belonging to D3DSBT_ALL only,
//! TEXCOORDINDEX / TEXTURETRANSFORMFLAGS in pixel blocks too, D3DTSS_CONSTANT in none.

use super::{Fail, Res};
use crate::case::{num, Op};
use crate::spec::hit;
use std::collections::BTreeMap;

pub const INVALIDCALL: u32 = 0x8876_086c;
const LEGACY: u32 = 0xbaad_cafe;

/// which state blocks other than D3DSBT_ALL capture a state
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Group {
    Pixel,
    Vertex,
    Both,
    /// D3DSBT_ALL only
    All,
    /// in no state block (MEASURED: D3DTSS_CONSTANT)
    Nowhere,
}
use Group::*;

/// documented render states: (D3DRENDERSTATETYPE, default, group)
pub const RENDER_STATES: &[(u32, &str, u32, Group)] = &[
    (7, "ZENABLE", 1, Pixel), // D3DZB_TRUE: the device has an automatic depth stencil
    (8, "FILLMODE", 3, Pixel),
    (9, "SHADEMODE", 2, Both),
    (14, "ZWRITEENABLE", 1, Pixel),
    (15, "ALPHATESTENABLE", 0, Pixel),
    (16, "LASTPIXEL", 1, Pixel),
    (19, "SRCBLEND", 2, Pixel),
    (20, "DESTBLEND", 1, Pixel),
    (22, "CULLMODE", 3, Vertex),
    (23, "ZFUNC", 4, Pixel),
    (24, "ALPHAREF", 0, Pixel),
    (25, "ALPHAFUNC", 8, Pixel),
    (26, "DITHERENABLE", 0, Pixel),
    (27, "ALPHABLENDENABLE", 0, Pixel),
    (28, "FOGENABLE", 0, Vertex),
    (29, "SPECULARENABLE", 0, Vertex),
    (34, "FOGCOLOR", 0, Vertex),
    (35, "FOGTABLEMODE", 0, Vertex),
    (36, "FOGSTART", 0, Both),
    (37, "FOGEND", 0x3f80_0000, Both),
    (38, "FOGDENSITY", 0x3f80_0000, Both),
    (40, "(40, MEASURED)", 0, Pixel),
    (48, "RANGEFOGENABLE", 0, Vertex),
    (52, "STENCILENABLE", 0, Pixel),
    (53, "STENCILFAIL", 1, Pixel),
    (54, "STENCILZFAIL", 1, Pixel),
    (55, "STENCILPASS", 1, Pixel),
    (56, "STENCILFUNC", 8, Pixel),
    (57, "STENCILREF", 0, Pixel),
    (58, "STENCILMASK", 0xffff_ffff, Pixel),
    (59, "STENCILWRITEMASK", 0xffff_ffff, Pixel),
    (60, "TEXTUREFACTOR", 0xffff_ffff, Pixel),
    (128, "WRAP0", 0, Pixel),
    (129, "WRAP1", 0, Pixel),
    (130, "WRAP2", 0, Pixel),
    (131, "WRAP3", 0, Pixel),
    (132, "WRAP4", 0, Pixel),
    (133, "WRAP5", 0, Pixel),
    (134, "WRAP6", 0, Pixel),
    (135, "WRAP7", 0, Pixel),
    (136, "CLIPPING", 1, Vertex),
    (137, "LIGHTING", 1, Vertex),
    (139, "AMBIENT", 0, Vertex),
    (140, "FOGVERTEXMODE", 0, Vertex),
    (141, "COLORVERTEX", 1, Vertex),
    (142, "LOCALVIEWER", 1, Vertex),
    (143, "NORMALIZENORMALS", 0, Vertex),
    (145, "DIFFUSEMATERIALSOURCE", 1, Vertex),
    (146, "SPECULARMATERIALSOURCE", 2, Vertex),
    (147, "AMBIENTMATERIALSOURCE", 0, Vertex),
    (148, "EMISSIVEMATERIALSOURCE", 0, Vertex),
    (151, "VERTEXBLEND", 0, Vertex),
    (152, "CLIPPLANEENABLE", 0, Vertex),
    (154, "POINTSIZE", 0x3f80_0000, Vertex),
    (155, "POINTSIZE_MIN", 0x3f80_0000, Vertex),
    (156, "POINTSPRITEENABLE", 0, Vertex),
    (157, "POINTSCALEENABLE", 0, Vertex),
    (158, "POINTSCALE_A", 0x3f80_0000, Vertex),
    (159, "POINTSCALE_B", 0, Vertex),
    (160, "POINTSCALE_C", 0, Vertex),
    (161, "MULTISAMPLEANTIALIAS", 1, Vertex),
    (162, "MULTISAMPLEMASK", 0xffff_ffff, Vertex),
    (163, "PATCHEDGESTYLE", 0, Vertex),
    (166, "POINTSIZE_MAX", 0x4600_0000, Vertex), // MEASURED: 8192.0 (the documentation says 64.0)
    (167, "INDEXEDVERTEXBLENDENABLE", 0, Vertex),
    (168, "COLORWRITEENABLE", 0xf, Pixel),
    (170, "TWEENFACTOR", 0, Vertex),
    (171, "BLENDOP", 1, Pixel),
    (172, "POSITIONDEGREE", 3, Vertex),
    (173, "NORMALDEGREE", 1, Vertex),
    (174, "SCISSORTESTENABLE", 0, Pixel),
    (175, "SLOPESCALEDEPTHBIAS", 0, Pixel),
    (176, "ANTIALIASEDLINEENABLE", 0, Pixel),
    (178, "MINTESSELLATIONLEVEL", 0x3f80_0000, Vertex),
    (179, "MAXTESSELLATIONLEVEL", 0x3f80_0000, Vertex),
    (180, "ADAPTIVETESS_X", 0, Vertex),
    (181, "ADAPTIVETESS_Y", 0, Vertex),
    (182, "ADAPTIVETESS_Z", 0x3f80_0000, Vertex),
    (183, "ADAPTIVETESS_W", 0, Vertex),
    (184, "ENABLEADAPTIVETESSELLATION", 0, Vertex),
    (185, "TWOSIDEDSTENCILMODE", 0, Pixel),
    (186, "CCW_STENCILFAIL", 1, Pixel),
    (187, "CCW_STENCILZFAIL", 1, Pixel),
    (188, "CCW_STENCILPASS", 1, Pixel),
    (189, "CCW_STENCILFUNC", 8, Pixel),
    (190, "COLORWRITEENABLE1", 0xf, Pixel),
    (191, "COLORWRITEENABLE2", 0xf, Pixel),
    (192, "COLORWRITEENABLE3", 0xf, Pixel),
    (193, "BLENDFACTOR", 0xffff_ffff, Pixel),
    (194, "SRGBWRITEENABLE", 0, Pixel),
    (195, "DEPTHBIAS", 0, Pixel),
    (198, "WRAP8", 0, Pixel),
    (199, "WRAP9", 0, Pixel),
    (200, "WRAP10", 0, Pixel),
    (201, "WRAP11", 0, Pixel),
    (202, "WRAP12", 0, Pixel),
    (203, "WRAP13", 0, Pixel),
    (204, "WRAP14", 0, Pixel),
    (205, "WRAP15", 0, Pixel),
    (206, "SEPARATEALPHABLENDENABLE", 0, Pixel),
    (207, "SRCBLENDALPHA", 2, Pixel),
    (208, "DESTBLENDALPHA", 1, Pixel),
    (209, "BLENDOPALPHA", 1, Pixel),
];

/// MEASURED: render states outside the documented ones that Get answers (with 0xbaadcafe)
pub fn rs_legacy(i: u32) -> bool {
    matches!(i, 0 | 42 | 47 | 50 | 61..=63 | 96..=127 | 138 | 149 | 150 | 165 | 169 | 177 | 210..=255)
}

/// documented texture stage states: (D3DTEXTURESTAGESTATETYPE, default of stage `s`, group)
pub fn tss_doc(t: u32, s: u32) -> Option<(u32, Group)> {
    Some(match t {
        1 => (if s == 0 { 4 } else { 1 }, Pixel), // COLOROP: MODULATE on stage 0, DISABLE after
        2 => (2, Pixel),                          // COLORARG1 TEXTURE
        3 => (1, Pixel),                          // COLORARG2 CURRENT
        4 => (if s == 0 { 2 } else { 1 }, Pixel), // ALPHAOP: SELECTARG1 on stage 0
        5 => (2, Pixel),
        6 => (1, Pixel),
        7..=10 => (0, Pixel), // BUMPENVMAT00..11
        // TEXCOORDINDEX (the stage's own index) and TEXTURETRANSFORMFLAGS: the documentation
        // lists them as vertex states; MEASURED: pixel state blocks hold them too
        11 => (s, Both),
        22 | 23 => (0, Pixel), // BUMPENVLSCALE, BUMPENVLOFFSET
        24 => (0, Both),
        26..=28 => (1, Pixel), // COLORARG0, ALPHAARG0, RESULTARG: CURRENT
        32 => (0, Nowhere),   // CONSTANT: MEASURED, no state block holds it
        _ => return None,
    })
}

/// documented sampler states: (D3DSAMPLERSTATETYPE, default, group)
pub fn ss_doc(t: u32) -> Option<(u32, Group)> {
    const D: [u32; 14] = [0, 1, 1, 1, 0, 1, 1, 0, 0, 0, 1, 0, 0, 0];
    match t {
        13 => Some((0, All)), // DMAPOFFSET: MEASURED, in D3DSBT_ALL only
        1..=12 => Some((D[t as usize], Pixel)),
        _ => None,
    }
}

pub fn sampler_ok(s: u32) -> bool {
    s < 16 || (256..=260).contains(&s)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Rs(u32),
    Tss(u32, u32),
    Ss(u32, u32),
}

/// every documented state and its group
pub fn all_states() -> Vec<(Key, u32, Group)> {
    let mut v: Vec<(Key, u32, Group)> = RENDER_STATES.iter().map(|&(i, _, d, g)| (Key::Rs(i), d, g)).collect();
    for s in 0..8 {
        for t in 0..34 {
            if let Some((d, g)) = tss_doc(t, s) {
                v.push((Key::Tss(s, t), d, g));
            }
        }
    }
    for s in (0..16).chain(256..=260) {
        for t in 0..14 {
            if let Some((d, g)) = ss_doc(t) {
                v.push((Key::Ss(s, t), d, g));
            }
        }
    }
    v
}

/// how Get answers a state that is not documented: Some(value) readable, None INVALIDCALL
fn undocumented_read(k: Key) -> Option<u32> {
    match k {
        Key::Rs(i) if rs_legacy(i) => Some(LEGACY),
        Key::Rs(_) => None,
        Key::Tss(_, 33) => Some(0), // MEASURED
        Key::Tss(_, t) if t < 34 => Some(LEGACY),
        Key::Ss(_, 0) => Some(LEGACY),
        _ => None,
    }
}

#[derive(Default)]
pub struct D3d9 {
    dev: bool,
    state: BTreeMap<Key, u32>,
    recording: Option<BTreeMap<Key, u32>>,
    blocks: BTreeMap<i64, BTreeMap<Key, u32>>,
}

fn hit_ret(id: &'static str) -> bool {
    hit(id);
    true
}

fn n(op: &Op, i: usize) -> Result<i64, Fail> {
    op.tok.get(i).and_then(|t| num(t)).ok_or(Fail::Bad)
}

impl D3d9 {
    fn reset(&mut self) {
        self.state = all_states().into_iter().map(|(k, d, _)| (k, d)).collect();
        self.recording = None;
        self.blocks.clear();
    }

    pub fn op(&mut self, op: &Op, out: &mut Vec<String>) -> Res<bool> {
        let t = op.tok.as_slice();
        match t[0].as_str() {
            "device" => {
                self.reset();
                self.dev = true;
                out.push("device ok".into());
            }
            "rs" | "tss" | "ss" if !self.dev => out.push("nodev".into()),
            "rs" | "tss" | "ss" => {
                let set = match t.get(1).map(String::as_str) {
                    Some("set") => true,
                    Some("get") => false,
                    _ => return Err(Fail::Bad),
                };
                let two = t[0] != "rs";
                let a = n(op, 2)?;
                let b = if two { n(op, 3)? } else { 0 };
                let v = if set { n(op, if two { 4 } else { 3 })? as u32 } else { 0 };
                let (a, b) = (a as u32, b as u32);
                let key = match t[0].as_str() {
                    "rs" if a < 256 => Key::Rs(a),
                    "tss" if a < 8 && b < 34 => Key::Tss(a, b),
                    "ss" if sampler_ok(a) && b < 14 => Key::Ss(a, b),
                    _ => return Err(Fail::Unmodeled("D3D9.INDEX.RANGE")),
                };
                let what = if two { format!("{} {} {}", t[0], a, t[3]) } else { format!("rs {a}") };
                let documented = self.state.contains_key(&key);
                if set {
                    hit("D3D9.SETGET");
                    if !documented {
                        return Err(Fail::Unmodeled("D3D9.SET.UNDOC"));
                    }
                    match &mut self.recording {
                        Some(r) => {
                            r.insert(key, v);
                        }
                        None => {
                            self.state.insert(key, v);
                        }
                    }
                } else {
                    hit(match (key, documented) {
                        (Key::Rs(166), _) => "D3D9.RS.POINTSIZEMAX",
                        (Key::Rs(_), true) => "D3D9.RS.DEFAULT",
                        (Key::Rs(_), false) => "D3D9.RS.UNDOC",
                        (Key::Tss(..), true) => "D3D9.TSS.DEFAULT",
                        (Key::Tss(..), false) => "D3D9.TSS.UNDOC",
                        (Key::Ss(..), _) => "D3D9.SS.DEFAULT",
                    });
                    match self.state.get(&key).copied().or_else(|| undocumented_read(key)) {
                        Some(v) => out.push(format!("{what} = {v:08x}")),
                        None => out.push(format!("{what} hr {INVALIDCALL:08x}")),
                    }
                }
            }
            "sb" if !self.dev => out.push("nodev".into()),
            "sb" => {
                let verb = t.get(1).ok_or(Fail::Bad)?.as_str();
                if verb == "begin" {
                    if self.recording.is_some() {
                        return Err(Fail::Unmodeled("D3D9.SB.MISUSE"));
                    }
                    hit("D3D9.SB.RECORD");
                    self.recording = Some(BTreeMap::new());
                    return Ok(true);
                }
                let i = n(op, 2)?;
                if !(0..8).contains(&i) {
                    return Err(Fail::Bad);
                }
                if self.recording.is_some() && verb != "end" {
                    return Err(Fail::Unmodeled("D3D9.SB.MISUSE"));
                }
                match verb {
                    "end" => {
                        let r = self.recording.take().ok_or(Fail::Unmodeled("D3D9.SB.MISUSE"))?;
                        self.blocks.insert(i, r);
                    }
                    "create" => {
                        let ty = n(op, 3)?;
                        let want: fn(Group) -> bool = match ty {
                            1 => |g| g != Nowhere,
                            2 => |g| matches!(g, Pixel | Both),
                            3 => |g| matches!(g, Vertex | Both),
                            _ => return Err(Fail::Unmodeled("D3D9.SB.BADTYPE")),
                        };
                        hit("D3D9.SB.TYPES");
                        hit("D3D9.SB.GROUPS");
                        let b = all_states().into_iter().filter(|s| want(s.2)).map(|(k, ..)| (k, self.state[&k])).collect();
                        self.blocks.insert(i, b);
                    }
                    "capture" | "apply" | "release" => {
                        let Some(b) = self.blocks.get_mut(&i) else {
                            out.push(format!("sb {i} none"));
                            return Ok(true);
                        };
                        match verb {
                            "capture" if hit_ret("D3D9.SB.CAPTURE") => b.iter_mut().for_each(|(k, v)| *v = self.state[k]),
                            "apply" if hit_ret("D3D9.SB.APPLY") => b.iter().for_each(|(k, v)| {
                                self.state.insert(*k, *v);
                            }),
                            _ => {
                                self.blocks.remove(&i);
                            }
                        }
                    }
                    _ => return Err(Fail::Bad),
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
