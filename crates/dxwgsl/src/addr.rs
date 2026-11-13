//! Direct3D 9's BORDER and MIRRORONCE texture addressing, which WebGPU's samplers do not have:
//! the sampler clamps (to the edge) and the shader does the rest.
//!
//! - MIRRORONCE: mirroring at 0 maps texel i to -1 - i, that is u to -u, so a clamping sampler
//!   at |u| reads what mirror-once reads.
//! - BORDER, point filtering: the texel floor(u * W) is the texture's when 0 <= u < 1, the
//!   border color otherwise.
//! - BORDER, linear filtering: in texel space x = u * W - 0.5 the two taps are floor(x) and
//!   floor(x) + 1 with weights 1 - f and f. A clamping sampler reads the edge texel for a tap
//!   past the edge, so the result is mix(border, clamped, w) with w the weight of the taps
//!   inside: saturate(x + 1) * saturate(W - x) per axis (1 for x in [0, W - 1], f below it,
//!   1 - f above). Exact for bilinear on level 0 (both axes multiply: the four taps' weights
//!   are products).

/// How one sampler addresses: `flags` bit 0 / 1 BORDER in u / v, bit 2 / 3 MIRRORONCE in u / v,
/// bit 4 linear (magnification) filtering; `border` the border color, RGBA (f32 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Addr {
    pub sampler: u32,
    pub flags: u32,
    pub border: [u32; 4],
}

impl Addr {
    /// Direct3D 9's: the border a D3DCOLOR (ARGB)
    pub fn argb(sampler: u32, flags: u32, c: u32) -> Addr {
        let f = |sh: u32| (((c >> sh) & 0xff) as f32 / 255.0).to_bits();
        Addr { sampler, flags, border: [f(16), f(8), f(0), f(24)] }
    }
}

/// The flags of an address mode in u and v (D3DTEXTUREADDRESS and D3D11_TEXTURE_ADDRESS_MODE
/// alike: 1 wrap, 2 mirror, 3 clamp, 4 border, 5 mirror once) and whether magnification filters
/// linearly; 0 when the sampler does it all.
pub fn flags(u: u32, v: u32, linear: bool) -> u32 {
    let f = (u == 4) as u32 | ((v == 4) as u32) << 1 | ((u == 5) as u32) << 2 | ((v == 5) as u32) << 3;
    if f == 0 { 0 } else { f | (linear as u32) << 4 }
}

/// An address mode as the sampler does it: BORDER and MIRRORONCE clamp.
pub fn sampler_mode(m: u32) -> u32 {
    if m == 4 || m == 5 { 3 } else { m }
}

/// am{sampler}(uv): the coordinate to sample at.
pub fn coord_fn(x: &Addr) -> String {
    let m = |bit: u32, c: &str| if x.flags & bit != 0 { format!("abs(uv.{c})") } else { format!("uv.{c}") };
    format!("fn am{}(uv: vec2f) -> vec2f {{ return vec2f({}, {}); }}\n", x.sampler, m(4, "x"), m(8, "y"))
}

/// `name`(color, uv): a sample of `tex` with the border.
pub fn border_fn(x: &Addr, tex: &str, name: &str) -> String {
    if x.flags & 3 == 0 {
        return format!("fn {name}(c: vec4f, uv: vec2f) -> vec4f {{ return c; }}\n");
    }
    let one = |bit: u32, e: String| if x.flags & bit != 0 { e } else { "1.0".into() };
    let k = if x.flags & 16 != 0 {
        format!(
            "let d = vec2f(textureDimensions({tex}));\n  let t = uv * d - 0.5;\n  let w = saturate(t + 1.0) * saturate(d - t);\n  let k = {} * {};",
            one(1, "w.x".into()),
            one(2, "w.y".into())
        )
    } else {
        let i = |c: &str| format!("f32(uv.{c} >= 0.0 && uv.{c} < 1.0)");
        format!("let k = {} * {};", one(1, i("x")), one(2, i("y")))
    };
    let b = x.border.map(|v| {
        let f = f32::from_bits(v);
        format!("{:?}", if f.is_finite() { f } else { 0.0 })
    });
    format!("fn {name}(c: vec4f, uv: vec2f) -> vec4f {{\n  {k}\n  return mix(vec4f({}), c, k);\n}}\n", b.join(", "))
}

/// Direct3D 9's functions of the samplers in `a` (texture tex{s}): am{s}, ab{s}.
pub fn helpers(a: &[Addr]) -> String {
    a.iter().map(|x| coord_fn(x) + &border_fn(x, &format!("tex{}", x.sampler), &format!("ab{}", x.sampler))).collect()
}

/// `sample` (a sample of sampler `s` at am{s}(uv)) with the border, or as it is when `s` needs
/// nothing.
pub fn wrap(a: &[Addr], s: u32, sample: &str, uv: &str) -> String {
    if a.iter().any(|x| x.sampler == s) { format!("ab{s}({sample}, {uv})") } else { sample.to_string() }
}

/// The coordinate sampler `s` samples at.
pub fn coord(a: &[Addr], s: u32, uv: &str) -> String {
    if a.iter().any(|x| x.sampler == s) { format!("am{s}({uv})") } else { uv.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A W x H texture of one channel, sampled as Direct3D defines it: each tap's texel index
    /// per axis is wrapped by the address mode (3 clamp, 4 border: the border value, 5 mirror
    /// once: -1 - i below 0, then clamped).
    fn d3d(t: &[f32], w: i32, h: i32, uv: (f32, f32), modes: (u32, u32), linear: bool, border: f32) -> f32 {
        let axis = |i: i32, n: i32, m: u32| -> Option<i32> {
            match m {
                4 if i < 0 || i >= n => None,
                5 => Some((if i < 0 { -1 - i } else { i }).clamp(0, n - 1)),
                _ => Some(i.clamp(0, n - 1)),
            }
        };
        let tap = |x: i32, y: i32| match (axis(x, w, modes.0), axis(y, h, modes.1)) {
            (Some(x), Some(y)) => t[(y * w + x) as usize],
            _ => border,
        };
        if !linear {
            return tap((uv.0 * w as f32).floor() as i32, (uv.1 * h as f32).floor() as i32);
        }
        let (x, y) = (uv.0 * w as f32 - 0.5, uv.1 * h as f32 - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        (1.0 - fy) * ((1.0 - fx) * tap(x0, y0) + fx * tap(x0 + 1, y0)) + fy * ((1.0 - fx) * tap(x0, y0 + 1) + fx * tap(x0 + 1, y0 + 1))
    }

    /// What the generated WGSL computes: a clamping sample at am(uv), then ab's mix (the same
    /// expressions, in Rust).
    fn ours(t: &[f32], w: i32, h: i32, uv: (f32, f32), modes: (u32, u32), linear: bool, border: f32) -> f32 {
        let f = flags(modes.0, modes.1, linear);
        let am = (if f & 4 != 0 { uv.0.abs() } else { uv.0 }, if f & 8 != 0 { uv.1.abs() } else { uv.1 });
        let c = d3d(t, w, h, am, (sampler_mode(modes.0), sampler_mode(modes.1)), linear, 0.0);
        let sat = |x: f32| x.clamp(0.0, 1.0);
        let k = |on: bool, u: f32, n: i32| {
            if !on {
                1.0
            } else if linear {
                let x = u * n as f32 - 0.5;
                sat(x + 1.0) * sat(n as f32 - x)
            } else {
                (u >= 0.0 && u < 1.0) as u32 as f32
            }
        };
        let k = k(f & 1 != 0, uv.0, w) * k(f & 2 != 0, uv.1, h);
        border * (1.0 - k) + c * k
    }

    #[test]
    fn border_and_mirror_once_are_what_d3d_samples() {
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut n = 0;
        for _ in 0..4000 {
            let (w, h) = (1 + (rnd() % 9) as i32, 1 + (rnd() % 9) as i32);
            let t: Vec<f32> = (0..w * h).map(|_| (rnd() % 256) as f32 / 255.0).collect();
            let modes = (1 + rnd() % 5, 1 + rnd() % 5);
            let modes = (if modes.0 < 3 { 3 } else { modes.0 }, if modes.1 < 3 { 3 } else { modes.1 });
            let linear = rnd() & 1 != 0;
            let border = (rnd() % 256) as f32 / 255.0;
            for _ in 0..16 {
                // (on a grid away from texel edges, where float rounding picks either texel)
                let g = |r: u32, n: i32| (((r % (n as u32 * 40)) as f32 - n as f32 * 13.0) + 0.37) / (n as f32 * 8.0);
                let uv = (g(rnd(), w), g(rnd(), h));
                let (a, b) = (d3d(&t, w, h, uv, modes, linear, border), ours(&t, w, h, uv, modes, linear, border));
                assert!((a - b).abs() < 1e-5, "{w}x{h} {modes:?} linear {linear} at {uv:?}: d3d {a}, ours {b}");
                n += 1;
            }
        }
        assert_eq!(n, 64000);
    }

    #[test]
    fn flags_of_modes() {
        assert_eq!(flags(1, 2, true), 0);
        assert_eq!(flags(3, 3, false), 0);
        assert_eq!(flags(4, 1, false), 1);
        assert_eq!(flags(1, 4, true), 2 | 16);
        assert_eq!(flags(5, 4, false), 4 | 2);
        assert_eq!(flags(1, 5, true), 8 | 16);
        let a = Addr::argb(2, 1, 0x80ff4000);
        assert_eq!(a.border.map(f32::from_bits), [1.0, 64.0 / 255.0, 0.0, 128.0 / 255.0]);
        for m in 0..8 {
            assert_eq!(sampler_mode(m), if m == 4 || m == 5 { 3 } else { m });
        }
    }

    fn valid(w: &str) {
        let m = naga::front::wgsl::parse_str(w).unwrap_or_else(|e| panic!("{}\n{w}", e.emit_to_string(w)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&m)
            .unwrap_or_else(|e| panic!("{e:?}\n{w}"));
    }

    #[test]
    fn shaders_with_border_are_valid_wgsl() {
        // ps_2_0: dcl t0; dcl_2d s0; texld r0, t0, s0; mov oC0, r0
        let code = [
            0xffff0200u32, 0x0200001f, 0x80000000, 0xb00f0000, 0x0200001f, 0x90000000, 0xa00f0800, 0x03000042, 0x800f0000,
            0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff,
        ];
        let s = crate::d3d9::Shader::parse(&code).unwrap();
        for f in [1, 2, 3, 4, 8, 12, 17, 18, 19, 21, 30, 31] {
            let a = vec![Addr::argb(0, f, 0xff40c0e0)];
            let opts = crate::d3d9::Options { link: 0x3ff, vs_inputs: Vec::new(), alpha_test: None, fog: false, addr: a.clone(), targets: 1 };
            let w = crate::d3d9::Gen::new(&s).pixel(&opts);
            assert!(w.contains("am0(") && (f & 3 == 0 || w.contains("ab0(")), "{w}");
            valid(&w);
            // modulate texture by diffuse on stage 0, stage 1 the texture of stage 1
            let k = crate::d3d9ff::PsKey {
                stages: vec![[4, 2, 0, 1, 2, 2, 0, 1, 1, 0], [2, 2, 1, 1, 2, 2, 1, 1, 1, 1]],
                specular: false,
                alpha_test: None,
                tex: [2, 2, 0, 0, 0, 0, 0, 0],
                flat: false,
                addr: vec![a[0], Addr::argb(1, f, 0x80102030)],
                vs_writes: None,
            };
            let w = crate::d3d9ff::pixel(&k);
            assert!(w.contains("am1(") && (f & 3 == 0 || w.contains("ab1(")), "{w}");
            valid(&w);
        }
        // nothing for samplers the shader does not sample
        let opts = crate::d3d9::Options { link: 0x3ff, vs_inputs: Vec::new(), alpha_test: None, fog: false, addr: vec![Addr::argb(3, 1, 0)], targets: 1 };
        assert!(!crate::d3d9::Gen::new(&s).pixel(&opts).contains("ab3"));
    }
}
