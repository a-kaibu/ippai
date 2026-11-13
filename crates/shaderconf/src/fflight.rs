//! Fixed-function lighting (dxwgsl::d3d9ff with LIGHTING on): the vertex shader kura makes for
//! a lit draw, run for random scenes (material, AMBIENT, WORLD and VIEW transforms, up to 8
//! point, spot and directional lights) against the lighting equation of the Direct3D 9 docs
//! ("Mathematics of Lighting", "Attenuation and Spotlight Factor", "Camera-relative lighting"),
//! written here independently: in f64, camera space through a Gauss-Jordan inverse.
//!
//! The vertex shader becomes a function a pixel shader calls with the vertex from uniforms
//! (after the shader's registers: position, normal, diffuse and specular colors), and the
//! pixel is its diffuse (oD0) or specular (oD1) output.

use super::{Case, Tol, Want};
use dxwgsl::d3d9::{USAGE_COLOR, USAGE_NORMAL, USAGE_POSITION};
use dxwgsl::d3d9ff::{self as ff, LightKey, VsKey};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// uniform in [a, b)
    fn f(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * ((self.next() >> 40) as f32 / (1u64 << 24) as f32)
    }
    fn v3(&mut self, a: f32, b: f32) -> [f32; 3] {
        [self.f(a, b), self.f(a, b), self.f(a, b)]
    }
    fn color(&mut self) -> [f32; 4] {
        [self.f(0.0, 1.0), self.f(0.0, 1.0), self.f(0.0, 1.0), self.f(0.0, 1.0)]
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// D3D row-major 4x4 from rotation (Euler angles), per-axis scale and translation: v * S * R * T
fn affine(r: &mut Rng, scale: (f32, f32), move_: f32) -> [f32; 16] {
    let (a, b, c) = (r.f(-3.1, 3.1), r.f(-3.1, 3.1), r.f(-3.1, 3.1));
    let s = r.v3(scale.0, scale.1);
    let t = r.v3(-move_, move_);
    let rx = [1.0, 0.0, 0.0, 0.0, a.cos(), a.sin(), 0.0, -a.sin(), a.cos()];
    let ry = [b.cos(), 0.0, -b.sin(), 0.0, 1.0, 0.0, b.sin(), 0.0, b.cos()];
    let rz = [c.cos(), c.sin(), 0.0, -c.sin(), c.cos(), 0.0, 0.0, 0.0, 1.0];
    let m3 = |x: &[f32; 9], y: &[f32; 9]| -> [f32; 9] { std::array::from_fn(|i| (0..3).map(|k| x[i / 3 * 3 + k] * y[k * 3 + i % 3]).sum()) };
    let rot = m3(&m3(&rx, &ry), &rz);
    let mut m = [0f32; 16];
    for i in 0..3 {
        for j in 0..3 {
            m[i * 4 + j] = s[i] * rot[i * 3 + j];
        }
        m[12 + i] = t[i];
    }
    m[15] = 1.0;
    m
}

#[derive(Clone)]
struct Scene {
    material: [f32; 17],
    ambient: u32,
    world: [f32; 16],
    view: [f32; 16],
    lights: Vec<ff::Light>,
    pos: [f32; 3],
    normal: [f32; 3],
    c0: [f32; 4],
    c1: [f32; 4],
}

fn scene(r: &mut Rng, count: usize) -> Scene {
    let mut material = [0f32; 17];
    for c in 0..4 {
        let v = r.color();
        material[c * 4..c * 4 + 4].copy_from_slice(&v);
    }
    // (scaled down, so sums of several lights stay below 1 often enough to be compared)
    for v in material.iter_mut().take(16) {
        *v *= 0.6;
    }
    material[16] = [0.0, 1.0, 4.0, r.f(1.0, 40.0)][r.below(4) as usize];
    let lights = (0..count)
        .map(|_| {
            let ty = 1 + r.below(3) as u32;
            let theta = r.f(0.2, 1.4);
            let k = |r: &mut Rng| {
                let c = r.color();
                [c[0] * 0.5, c[1] * 0.5, c[2] * 0.5, c[3]]
            };
            ff::Light {
                ty,
                diffuse: k(r),
                specular: k(r),
                ambient: k(r),
                position: r.v3(-4.0, 4.0),
                direction: r.v3(-1.0, 1.0),
                range: r.f(2.0, 12.0),
                falloff: [0.0, 1.0, r.f(0.3, 3.0)][r.below(3) as usize],
                attenuation: [r.f(0.2, 1.0), r.f(0.0, 0.3), r.f(0.0, 0.1)],
                theta,
                phi: theta + r.f(0.1, 1.2),
            }
        })
        .collect();
    Scene {
        material,
        ambient: (r.next() as u32) & 0xff7f7f7f,
        world: affine(r, (0.5, 2.0), 2.0),
        view: affine(r, (1.0, 1.0), 3.0),
        lights,
        pos: r.v3(-1.0, 1.0),
        normal: r.v3(-1.0, 1.0),
        c0: r.color(),
        c1: r.color(),
    }
}

/// General 4x4 inverse (Gauss-Jordan, partial pivoting), f64.
fn inverse(m: &[f64; 16]) -> Option<[f64; 16]> {
    let mut a = *m;
    let mut inv = [0f64; 16];
    for i in 0..4 {
        inv[i * 5] = 1.0;
    }
    for c in 0..4 {
        let p = (c..4).max_by(|&x, &y| a[x * 4 + c].abs().total_cmp(&a[y * 4 + c].abs()))?;
        if a[p * 4 + c].abs() < 1e-12 {
            return None;
        }
        for k in 0..4 {
            a.swap(c * 4 + k, p * 4 + k);
            inv.swap(c * 4 + k, p * 4 + k);
        }
        let d = a[c * 4 + c];
        for k in 0..4 {
            a[c * 4 + k] /= d;
            inv[c * 4 + k] /= d;
        }
        for r in 0..4 {
            if r != c {
                let f = a[r * 4 + c];
                for k in 0..4 {
                    a[r * 4 + k] -= f * a[c * 4 + k];
                    inv[r * 4 + k] -= f * inv[c * 4 + k];
                }
            }
        }
    }
    Some(inv)
}

type V3 = [f64; 3];
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l == 0.0 {
        a
    } else {
        a.map(|x| x / l)
    }
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
/// v * M with v = (x, y, z, w)
fn xf(v: [f64; 4], m: &[f64; 16]) -> [f64; 4] {
    std::array::from_fn(|c| (0..4).map(|k| v[k] * m[k * 4 + c]).sum())
}

/// The lit diffuse and specular colors (rgba, clamped), or None when the scene sits too close
/// to a discontinuity (range edge, N.L or N.H at 0, a spotlight's cone edges) to compare.
fn reference(s: &Scene, k: &LightKey, with_normal: bool) -> Option<[[f64; 4]; 2]> {
    const EPS: f64 = 2e-3;
    let d = |m: &[f32; 16]| m.map(|x| x as f64);
    let (w, v) = (d(&s.world), d(&s.view));
    let wv: [f64; 16] = std::array::from_fn(|i| (0..4).map(|k| w[i / 4 * 4 + k] * v[k * 4 + i % 4]).sum());
    let p = xf([s.pos[0] as f64, s.pos[1] as f64, s.pos[2] as f64, 1.0], &wv);
    let p = [p[0], p[1], p[2]];
    // normals by the inverse transpose of world * view: n * transpose(inverse(WV))
    let inv = inverse(&wv)?;
    let n0 = if with_normal { s.normal.map(|x| x as f64) } else { [0.0; 3] };
    let mut n: V3 = std::array::from_fn(|c| (0..3).map(|k| n0[k] * inv[c * 4 + k]).sum());
    if k.normalize {
        n = norm(n);
    }
    let m = |i: usize| -> [f64; 4] { std::array::from_fn(|c| s.material[i * 4 + c] as f64) };
    let vc = |src: u8, mat: usize| -> [f64; 4] {
        match src {
            1 => s.c0.map(|x| x as f64),
            2 => s.c1.map(|x| x as f64),
            _ => m(mat),
        }
    };
    let (cd, cs, ca, ce) = (vc(k.sources[0], 0), vc(k.sources[1], 2), vc(k.sources[2], 1), vc(k.sources[3], 3));
    let power = s.material[16] as f64;
    let ga = [16, 8, 0].map(|sh| ((s.ambient >> sh) & 0xff) as f64 / 255.0);
    let (mut amb, mut dif, mut spe) = ([0f64; 3], [0f64; 3], [0f64; 3]);
    for l in s.lights.iter().take(k.count as usize) {
        let lp = xf([l.position[0] as f64, l.position[1] as f64, l.position[2] as f64, 1.0], &v);
        let ld = xf([l.direction[0] as f64, l.direction[1] as f64, l.direction[2] as f64, 0.0], &v);
        let ldir = norm([ld[0], ld[1], ld[2]]);
        let (dir, atten) = if l.ty == 3 {
            (ldir.map(|x| -x), 1.0)
        } else {
            let to = sub([lp[0], lp[1], lp[2]], p);
            let dist = dot(to, to).sqrt();
            if (dist - l.range as f64).abs() < EPS {
                return None;
            }
            if dist > l.range as f64 {
                continue;
            }
            let a = l.attenuation.map(|x| x as f64);
            let mut att = 1.0 / (a[0] + a[1] * dist + a[2] * dist * dist);
            let dir = norm(to);
            if l.ty == 2 {
                let rho = dot(ldir.map(|x| -x), dir);
                let (ct, cp) = ((l.theta as f64 / 2.0).cos(), (l.phi as f64 / 2.0).cos());
                if (rho - ct).abs() < EPS || (rho - cp).abs() < EPS {
                    return None;
                }
                att *= if rho > ct {
                    1.0
                } else if rho <= cp {
                    0.0
                } else {
                    ((rho - cp) / (ct - cp)).powf(l.falloff as f64)
                };
            }
            (dir, att)
        };
        let nl = dot(n, dir);
        if with_normal && nl.abs() < EPS {
            return None;
        }
        for c in 0..3 {
            amb[c] += atten * l.ambient[c] as f64;
            dif[c] += atten * nl.max(0.0) * l.diffuse[c] as f64;
        }
        if k.specular {
            let view = if k.local_viewer { norm(p.map(|x| -x)) } else { [0.0, 0.0, -1.0] };
            let h = norm([dir[0] + view[0], dir[1] + view[1], dir[2] + view[2]]);
            let nh = dot(n, h);
            if with_normal && nh.abs() < EPS {
                return None;
            }
            if nl > 0.0 && nh > 0.0 {
                let f = if power == 0.0 { 1.0 } else { nh.powf(power) };
                for c in 0..3 {
                    spe[c] += atten * f * l.specular[c] as f64;
                }
            }
        }
    }
    let cl = |x: f64| x.clamp(0.0, 1.0);
    let diffuse: [f64; 4] = std::array::from_fn(|c| if c == 3 { cl(cd[3]) } else { cl(ce[c] + ca[c] * (ga[c] + amb[c]) + cd[c] * dif[c]) });
    let specular: [f64; 4] = std::array::from_fn(|c| if c == 3 { 0.0 } else { cl(cs[c] * spe[c]) });
    Some([diffuse, specular])
}

/// The vertex shader as a function, called by a pixel shader with the vertex from c[VS_CONSTS..]
fn wgsl(k: &VsKey, out: usize) -> String {
    let n = ff::VS_CONSTS;
    let v = ff::vertex(k)
        .replace("@vertex fn main(i: In) -> Out", "fn vmain(i: In) -> Out")
        .replace(&format!("c: array<vec4f, {n}>"), &format!("c: array<vec4f, {}>", n + 4));
    let inputs: String = k.inputs.iter().map(|&(_, _, loc, _)| format!("  i.i{loc} = u.c[{}];\n", n + loc)).collect();
    format!("{v}\n@fragment fn main() -> @location(0) vec4f {{\n  var i: In;\n{inputs}  let o = vmain(i);\n  return o.l{out};\n}}\n")
}

pub fn cases(out: &mut Vec<Case>, errors: &mut Vec<String>) {
    let mut r = Rng(0x11647);
    // keys: lights, specular, local viewer, normalize, vertex colors present, sources
    let mut keys = Vec::new();
    for count in [0u8, 1, 2, 3, 8] {
        for (specular, local_viewer, normalize) in [(false, false, false), (true, true, true), (true, false, true), (true, true, false)] {
            keys.push((count, specular, local_viewer, normalize, [true, true], [1u32, 2, 0, 0], true));
        }
    }
    // material sources: COLOR1 / COLOR2 everywhere, the material, a vertex without colors
    keys.push((3, true, true, true, [true, true], [2, 1, 1, 2], true));
    keys.push((3, true, true, true, [true, true], [0, 0, 0, 0], true));
    keys.push((3, true, true, true, [false, false], [1, 2, 1, 2], true));
    keys.push((2, true, false, true, [true, false], [1, 2, 2, 1], true));
    // (a vertex without a normal: only ambient and emissive light it)
    keys.push((3, true, true, true, [true, true], [1, 2, 0, 0], false));
    for (ki, &(count, specular, local_viewer, normalize, has, src, with_normal)) in keys.iter().enumerate() {
        let mut inputs = vec![(USAGE_POSITION, 0, 0, 0)];
        if with_normal {
            inputs.push((USAGE_NORMAL, 0, 1, 0));
        }
        if has[0] {
            inputs.push((USAGE_COLOR, 0, 2, 0));
        }
        if has[1] {
            inputs.push((USAGE_COLOR, 1, 3, 0));
        }
        let lk = LightKey { count, specular, local_viewer, normalize, sources: ff::sources(src, true, has) };
        let k = VsKey { inputs, rhw: false, tci: [(0, 0); 8], flat: false, light: Some(lk) };
        let size = dxwgsl::d3d9::uniform_size(ff::VS_CONSTS + 4) as usize;
        for o in 0..2 {
            let mut pixels = Vec::new();
            let mut tries = 0;
            while pixels.len() < 24 && tries < 400 {
                tries += 1;
                let s = scene(&mut r, count as usize);
                let Some(want) = reference(&s, &lk, with_normal) else { continue };
                let mut regs = vec![[0f32; 4]; (ff::VS_CONSTS + 4) as usize];
                ff::lighting_regs(&mut regs, &s.material, s.ambient, &s.world, &s.view, &s.lights);
                // (the shader's position transform: anything, the pixel is a color)
                let n = ff::VS_CONSTS as usize;
                regs[n] = [s.pos[0], s.pos[1], s.pos[2], 1.0];
                regs[n + 1] = [s.normal[0], s.normal[1], s.normal[2], 0.0];
                regs[n + 2] = s.c0;
                regs[n + 3] = s.c1;
                let mut d = vec![0u8; size];
                let at = (dxwgsl::d3d9::CONSTS_EXTRA as usize + 16) * 16;
                for (j, v) in regs.iter().flatten().enumerate() {
                    d[at + j * 4..at + j * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                let w: [Want; 4] = std::array::from_fn(|c| Some(vec![want[o][c] as f32]));
                pixels.push((d, w));
            }
            if pixels.len() < 8 {
                errors.push(format!("fflight/{ki}: only {} comparable scenes", pixels.len()));
                continue;
            }
            out.push(Case {
                name: format!("fflight/{}-l{count}{}{}{}-src{:?}", ["diffuse", "specular"][o], if specular { "s" } else { "" }, if local_viewer { "v" } else { "" }, if normalize { "n" } else { "" }, lk.sources),
                api: "d3d9ff",
                wgsl: wgsl(&k, o),
                src: format!("{k:?}"),
                code: Vec::new(),
                group: 0,
                binding: 0,
                pixels,
                tol: Tol::Abs(4e-3),
                rule: "D3D9 fixed-function lighting: Mathematics of Lighting (ambient, diffuse, specular, emissive; attenuation and spotlight factor; camera space)",
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's inverse is one.
    #[test]
    fn inverse_inverts() {
        let mut r = Rng(5);
        for _ in 0..200 {
            let m = affine(&mut r, (0.3, 3.0), 4.0).map(|x| x as f64);
            let i = inverse(&m).unwrap();
            for a in 0..4 {
                for b in 0..4 {
                    let x: f64 = (0..4).map(|k| m[a * 4 + k] * i[k * 4 + b]).sum();
                    assert!((x - if a == b { 1.0 } else { 0.0 }).abs() < 1e-9);
                }
            }
        }
    }

    /// Hand-worked scenes: an unlit vertex is emissive + ambient * material ambient (小悪魔
    /// ディーラーのキャンディ体験版's sprites: diffuse 0, emissive white, one directional light),
    /// a directional light straight on gives its diffuse color, a point light at distance 2 with
    /// attenuation (0, 0, 1) a quarter.
    #[test]
    fn worked_examples() {
        let ident: [f32; 16] = std::array::from_fn(|i| if i % 5 == 0 { 1.0 } else { 0.0 });
        let mut s = Scene { material: [0.0; 17], ambient: 0xff505050, world: ident, view: ident, lights: vec![], pos: [0.0; 3], normal: [0.0, 0.0, -1.0], c0: [0.0; 4], c1: [0.0; 4] };
        s.material[3] = 0.5; // diffuse alpha
        s.material[12..15].copy_from_slice(&[1.0, 1.0, 1.0]);
        let k = LightKey { count: 1, specular: false, local_viewer: true, normalize: false, sources: [0; 4] };
        let dl = ff::Light { ty: 3, diffuse: [1.0; 4], direction: [0.6667, -0.6667, 0.3333], ..Default::default() };
        s.lights = vec![dl];
        assert_eq!(reference(&s, &k, true).unwrap()[0], [1.0, 1.0, 1.0, 0.5]);
        // straight on: diffuse material 0.5 grey, light diffuse (1, 0.5, 0.25)
        s.material[12..15].copy_from_slice(&[0.0; 3]);
        s.material[0..3].copy_from_slice(&[0.5; 3]);
        s.ambient = 0;
        s.lights = vec![ff::Light { ty: 3, diffuse: [1.0, 0.5, 0.25, 1.0], direction: [0.0, 0.0, 1.0], ..Default::default() }];
        let d = reference(&s, &k, true).unwrap()[0];
        assert!((d[0] - 0.5).abs() < 1e-9 && (d[1] - 0.25).abs() < 1e-9 && (d[2] - 0.125).abs() < 1e-9, "{d:?}");
        // point light 2 in front, attenuation 1/d^2
        s.lights = vec![ff::Light { ty: 1, diffuse: [1.0; 4], position: [0.0, 0.0, -2.0], range: 10.0, attenuation: [0.0, 0.0, 1.0], ..Default::default() }];
        let d = reference(&s, &k, true).unwrap()[0];
        assert!((d[0] - 0.125).abs() < 1e-9, "{d:?}");
        // out of range: dark
        s.lights[0].range = 1.0;
        assert_eq!(reference(&s, &k, true).unwrap()[0][0], 0.0);
    }
}
