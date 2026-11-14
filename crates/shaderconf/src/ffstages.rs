//! Fixed-function texture stages (dxwgsl::d3d9ff::pixel) where no texture is bound: random
//! stage setups of the D3D9 ops whose results stay within 0..1, any args (DIFFUSE, CURRENT,
//! TEXTURE, TFACTOR, SPECULAR, TEMP, CONSTANT, COMPLEMENT, ALPHAREPLICATE), either RESULTARG,
//! against the "Texture Blending" / D3DTEXTUREOP formulas of the Direct3D 9 docs, written here
//! independently in f64. With no texture set, an op that reads D3DTA_TEXTURE (arg1 unless
//! SELECTARG2, arg2 unless SELECTARG1, arg0 of MULTIPLYADD and LERP) passes CURRENT through, as
//! Windows does (tests/winref/probes/d3d9notex.c; 正しい性奴○の使い方 leaves stage 1 at MODULATE
//! TEXTURE DIFFUSE with no texture, and a white stand-in made its screen white).
//!
//! The pixel shader becomes a function called with the diffuse and specular colors from
//! uniforms after its constants (TFACTOR c[0], stage constants c[1..9]): c[9], c[10].

use super::{Case, Tol, Want};
use dxwgsl::d3d9ff::{self as ff, PsKey};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn color(&mut self) -> [f64; 4] {
        // (on the 8-bit grid, as vertex colors and TFACTOR are)
        std::array::from_fn(|_| self.below(256) as f64 / 255.0)
    }
}

/// the ops whose results stay in 0..1 for inputs in 0..1 (not SUBTRACT, ADDSIGNED, DOTPRODUCT3,
/// the bump ops, PREMODULATE), nor the two that read the texture's alpha whatever the args
/// (BLENDTEXTUREALPHA, BLENDTEXTUREALPHAPM)
const COLOR_OPS: [u32; 16] = [2, 3, 4, 5, 6, 7, 11, 12, 14, 16, 18, 19, 20, 21, 25, 26];
const ALPHA_OPS: [u32; 12] = [2, 3, 4, 5, 6, 7, 11, 12, 14, 16, 25, 26];

type C = [f64; 4];

struct Regs {
    diffuse: C,
    specular: C,
    tfactor: C,
    constant: [C; 8],
}

fn arg(a: u32, stage: usize, r: &Regs, current: C, temp: C) -> C {
    let mut v = match a & 0xf {
        0 => r.diffuse,
        1 => current,
        // (none bound: only args the op does not read get here)
        2 => [f64::NAN; 4],
        3 => r.tfactor,
        4 => r.specular,
        5 => temp,
        6 => r.constant[stage],
        _ => current,
    };
    if a & 0x20 != 0 {
        v = [v[3]; 4]; // ALPHAREPLICATE
    }
    if a & 0x10 != 0 {
        v = v.map(|x| 1.0 - x); // COMPLEMENT
    }
    v
}

/// D3DTEXTUREOP o on one channel (k) of a1, a2, a0 (the formulas of the D3D9 docs)
fn op(o: u32, a1: C, a2: C, a0: C, k: usize, r: &Regs, current: C) -> f64 {
    let (x, y, z) = (a1[k], a2[k], a0[k]);
    match o {
        2 => x,
        3 => y,
        4 => x * y,
        5 => x * y * 2.0,
        6 => x * y * 4.0,
        7 => x + y,
        11 => x + y - x * y,
        12 => x * r.diffuse[3] + y * (1.0 - r.diffuse[3]),
        14 => x * r.tfactor[3] + y * (1.0 - r.tfactor[3]),
        16 => x * current[3] + y * (1.0 - current[3]),
        // (color only: rgb as the docs give them, alpha arg1's)
        18 => if k == 3 { a1[3] } else { x + a1[3] * y },
        19 => if k == 3 { a1[3] } else { x * y + a1[3] },
        20 => if k == 3 { a1[3] } else { (1.0 - a1[3]) * y + x },
        21 => if k == 3 { a1[3] } else { (1.0 - x) * y + a1[3] },
        25 => z + x * y,
        26 => z * x + (1.0 - z) * y,
        _ => unreachable!(),
    }
}

/// Whether op o reads the texture through its args (what Windows turns into SELECTARG1 CURRENT)
fn reads_texture(o: u32, a1: u32, a2: u32, a0: u32) -> bool {
    let t = |a: u32| a & 0xf == 2;
    (t(a1) && o != 3) || (t(a2) && o != 2) || (t(a0) && (o == 25 || o == 26))
}

/// The pixel: each stage in turn (none with a texture), saturated, specular added at the end.
fn reference(stages: &[[u32; 10]], specular: bool, r: &Regs) -> C {
    let mut current = r.diffuse;
    let mut temp = [0.0; 4];
    for (st, s) in stages.iter().enumerate() {
        let (cop, c1, c2, c0) = if reads_texture(s[0], s[1], s[2], s[3]) { (2, 1, 1, 1) } else { (s[0], s[1], s[2], s[3]) };
        let (aop, x1, x2, x0) = if reads_texture(s[4], s[5], s[6], s[7]) { (2, 1, 1, 1) } else { (s[4], s[5], s[6], s[7]) };
        let (ca1, ca2, ca0) = (arg(c1, st, r, current, temp), arg(c2, st, r, current, temp), arg(c0, st, r, current, temp));
        let (aa1, aa2, aa0) = (arg(x1, st, r, current, temp), arg(x2, st, r, current, temp), arg(x0, st, r, current, temp));
        let mut v = [0.0; 4];
        for k in 0..3 {
            v[k] = op(cop, ca1, ca2, ca0, k, r, current).clamp(0.0, 1.0);
        }
        v[3] = op(aop, aa1, aa2, aa0, 3, r, current).clamp(0.0, 1.0);
        if s[8] & 0xf == 5 {
            temp = v;
        } else {
            current = v;
        }
    }
    if specular {
        for k in 0..3 {
            current[k] += r.specular[k];
        }
    }
    current.map(|x| x.clamp(0.0, 1.0))
}

fn wgsl(k: &PsKey) -> String {
    let n = ff::PS_CONSTS;
    let p = ff::pixel(k)
        .replace("@fragment fn main(i: In) -> @location(0) vec4f {", "fn pmain(i: In) -> vec4f {")
        .replace(&format!("c: array<vec4f, {n}>"), &format!("c: array<vec4f, {}>", n + 2));
    format!("{p}\n@fragment fn main() -> @location(0) vec4f {{\n  var i: In;\n  i.l0 = u.c[{n}];\n  i.l1 = u.c[{}];\n  return pmain(i);\n}}\n", n + 1)
}

fn stage(r: &mut Rng) -> [u32; 10] {
    let a = |r: &mut Rng| {
        let base = r.below(7) as u32;
        base | if r.below(4) == 0 { 0x10 } else { 0 } | if r.below(4) == 0 { 0x20 } else { 0 }
    };
    let cop = COLOR_OPS[r.below(COLOR_OPS.len() as u64) as usize];
    let aop = ALPHA_OPS[r.below(ALPHA_OPS.len() as u64) as usize];
    let res = if r.below(4) == 0 { 5 } else { 1 };
    [cop, a(r), a(r), a(r), aop, a(r), a(r), a(r), res, 0]
}

pub fn cases(out: &mut Vec<Case>, errors: &mut Vec<String>) {
    let mut r = Rng(0x2191);
    let n = ff::PS_CONSTS as usize;
    let size = dxwgsl::d3d9::uniform_size(ff::PS_CONSTS + 2) as usize;
    let at = (dxwgsl::d3d9::CONSTS_EXTRA as usize + 16) * 16;
    // the game's: stage 0 SELECTARG1 DIFFUSE (its texture is not bound here), stage 1 MODULATE
    // TEXTURE DIFFUSE with none; then random setups of 1 to 4 stages
    let mut setups = vec![(vec![[2, 0, 1, 1, 2, 0, 1, 1, 1, 0], [4, 2, 0, 1, 4, 2, 0, 1, 1, 0]], false)];
    for _ in 0..60 {
        let count = 1 + r.below(4) as usize;
        setups.push(((0..count).map(|_| stage(&mut r)).collect(), r.below(2) == 0));
    }
    for (si, (stages, specular)) in setups.iter().enumerate() {
        let k = PsKey { stages: stages.clone(), specular: *specular, ..Default::default() };
        let mut pixels = Vec::new();
        for _ in 0..16 {
            let g = Regs { diffuse: r.color(), specular: r.color(), tfactor: r.color(), constant: std::array::from_fn(|_| r.color()) };
            let want = reference(stages, *specular, &g);
            let mut regs = vec![[0f32; 4]; n + 2];
            regs[0] = g.tfactor.map(|x| x as f32);
            for s in 0..8 {
                regs[1 + s] = g.constant[s].map(|x| x as f32);
            }
            regs[n] = g.diffuse.map(|x| x as f32);
            regs[n + 1] = g.specular.map(|x| x as f32);
            let mut d = vec![0u8; size];
            for (j, v) in regs.iter().flatten().enumerate() {
                d[at + j * 4..at + j * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            let w: [Want; 4] = std::array::from_fn(|c| Some(vec![want[c] as f32]));
            pixels.push((d, w));
        }
        if pixels.is_empty() {
            errors.push(format!("ffstages/{si}: no pixels"));
            continue;
        }
        out.push(Case {
            name: format!("ffstages/{si}-{}{}", stages.len(), if *specular { "s" } else { "" }),
            api: "d3d9ff",
            wgsl: wgsl(&k),
            src: format!("{k:?}"),
            code: Vec::new(),
            group: 0,
            binding: 1,
            pixels,
            tol: Tol::Abs(1e-4),
            rule: "D3D9 texture blending (D3DTEXTUREOP formulas, args, RESULTARG, saturation per stage); a stage that reads D3DTA_TEXTURE with none bound passes CURRENT (SELECTARG1 CURRENT)",
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs() -> Regs {
        Regs { diffuse: [1.0, 1.0, 1.0, 0.75], specular: [0.0; 4], tfactor: [0.5, 0.25, 0.0, 1.0], constant: [[0.0; 4]; 8] }
    }

    /// the game's two stages: stage 1 (MODULATE TEXTURE DIFFUSE, nothing bound) keeps stage 0's
    #[test]
    fn unbound_stage_passes_current() {
        let s = [[2, 3, 1, 1, 2, 3, 1, 1, 1, 0], [4, 2, 0, 1, 4, 2, 0, 1, 1, 0]];
        assert_eq!(reference(&s, false, &regs()), [0.5, 0.25, 0.0, 1.0]);
    }

    /// args the op does not read may name the texture: SELECTARG2 of (TEXTURE, TFACTOR)
    #[test]
    fn unread_texture_arg_is_no_texture_read() {
        assert!(!reads_texture(3, 2, 3, 1));
        assert!(!reads_texture(2, 3, 2, 1));
        assert!(!reads_texture(7, 3, 0, 2));
        assert!(reads_texture(26, 3, 0, 2) && reads_texture(25, 3, 0, 2) && reads_texture(4, 0x12, 0, 1));
        let s = [[3, 2, 3, 1, 3, 2, 3, 1, 1, 0]];
        assert_eq!(reference(&s, false, &regs()), [0.5, 0.25, 0.0, 1.0]);
    }

    /// the translator's rule is this one, for every op and args
    #[test]
    fn translator_rule_matches() {
        for o in 2..=26 {
            for a1 in [0, 2, 0x12, 3] {
                for a2 in [0, 2, 0x22, 4] {
                    for a0 in [1, 2] {
                        assert_eq!(ff::no_texture(o, a1, a2, a0), reads_texture(o, a1, a2, a0), "op {o} args {a1:#x} {a2:#x} {a0:#x}");
                    }
                }
            }
        }
    }

    #[test]
    fn cases_are_valid_wgsl() {
        let (mut out, mut errors) = (Vec::new(), Vec::new());
        cases(&mut out, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(out.len() > 50);
        for c in &out {
            let m = naga::front::wgsl::parse_str(&c.wgsl).unwrap_or_else(|e| panic!("{}: {}\n{}", c.name, e.emit_to_string(&c.wgsl), c.wgsl));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m).unwrap_or_else(|e| panic!("{}: {e:?}\n{}", c.name, c.wgsl));
        }
    }
}
