//! The reference interpreters as an oracle (shaderconf --oracle N [seed]): the cases' DXBC and
//! D3D9 bytecode, mutated; each mutant dxwgsl translates to valid WGSL and the interpreter runs
//! with a result every implementation must give (nothing out of range, no infinity or NaN on
//! the way) becomes a case whose expected pixels are the interpreter's. tests/shaders.mjs then
//! runs dxwgsl's WGSL of it on WebGPU: a difference is dxwgsl not doing what the instruction
//! does, for operand, modifier, swizzle and flow combinations no case was written for.

use super::{ref_d3d9, ref_dxbc, Case, Tol, Want};

/// tests/shaders.mjs's comparison.
#[cfg(test)]
pub fn close(got: f32, want: f32, tol: Tol) -> bool {
    if want.is_nan() {
        return got.is_nan();
    }
    if got.is_nan() {
        return false;
    }
    if got == want || (got == 0.0 && want.abs() < f32::MIN_POSITIVE) {
        return true;
    }
    if !want.is_finite() || !got.is_finite() {
        return false;
    }
    let ord = |x: f32| {
        let b = x.to_bits();
        if b & 0x8000_0000 != 0 { -((b & 0x7fff_ffff) as i64) } else { b as i64 }
    };
    match tol {
        Tol::Exact => false,
        Tol::Ulp(n) => (ord(got) - ord(want)).abs() <= n as i64,
        Tol::Abs(e) => (got - want).abs() <= e,
        Tol::Rel(r, a) => (got - want).abs() <= (r * want.abs()).max(a),
    }
}

fn words(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// A case's pixel run through its interpreter: the target's four channels, and the taint.
pub fn interpret(c: &Case, data: &[u8]) -> Result<([f32; 4], Option<String>), String> {
    match c.api {
        "dxbc" => {
            let p = ref_dxbc::parse(&c.code)?;
            let cb: Vec<[u32; 4]> = words(data).chunks(4).map(|v| std::array::from_fn(|i| v.get(i).copied().unwrap_or(0))).collect();
            let (o, t) = ref_dxbc::run(&p, &[cb])?;
            Ok((o[0].map(f32::from_bits), t))
        }
        "d3d9" => {
            let p = ref_d3d9::parse(&words(&c.code))?;
            // (x[4], i[16], then c[])
            let at = (dxwgsl::d3d9::CONSTS_EXTRA as usize + 16) * 16;
            let consts: Vec<[f32; 4]> = words(data.get(at..).unwrap_or(&[])).chunks_exact(4).take(256).map(|v| std::array::from_fn(|i| f32::from_bits(v[i]))).collect();
            let (o, t) = ref_d3d9::run(&p, &consts)?;
            Ok((o[0], t))
        }
        a => Err(format!("no interpreter for {a}")),
    }
}


struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// A mutant: a few words of the program changed (operand tokens, swizzles, registers,
/// opcodes), copied from elsewhere in it, or flipped bits.
fn mutate(r: &mut Rng, w: &[u32], keep: usize) -> Vec<u32> {
    let mut w = w.to_vec();
    for _ in 0..1 + r.below(3) {
        if w.len() <= keep {
            break;
        }
        let i = keep + r.below(w.len() - keep);
        match r.below(4) {
            0 => w[i] ^= 1 << r.below(32),
            1 => w[i] ^= (r.below(4) as u32) << (4 + 2 * r.below(4)),
            2 => {
                let j = keep + r.below(w.len() - keep);
                w[i] = w[j];
            }
            _ => w[i] = (w[i] & !0x7ff) | ((w[i] & 0x7ff) ^ (1 << r.below(4))),
        }
    }
    w
}

fn to_bytes(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// dxwgsl's WGSL of a mutant, with the group, binding and uniform size it reads; None when
/// dxwgsl rejects it or it needs more than the one uniform buffer the runner binds.
fn translate(api: &str, code: &[u8]) -> Option<(String, u32, u32, usize)> {
    match api {
        "dxbc" => {
            let w = std::panic::catch_unwind(|| dxwgsl::translate(code, true, None)).ok()?.ok()?;
            match w.bindings.as_slice() {
                [(b, dxwgsl::Binding::Uniform(size))] => Some((w.code, 1, *b, *size as usize)),
                [] => Some((w.code, 1, 0, 0)),
                _ => None,
            }
        }
        _ => {
            let words = words(code);
            let s = std::panic::catch_unwind(|| dxwgsl::d3d9::Shader::parse(&words)).ok()??;
            if !s.pixel {
                return None;
            }
            let o = dxwgsl::d3d9::Options { link: 0, vs_inputs: Vec::new(), alpha_test: None, fog: false, addr: Vec::new(), targets: 1, flat: false };
            let wgsl = std::panic::catch_unwind(|| dxwgsl::d3d9::Gen::new(&s).pixel(&o)).ok()?;
            Some((wgsl, 0, 1, dxwgsl::d3d9::uniform_size(s.max_const) as usize))
        }
    }
}

/// The mutant's instructions as the interpreter reads them.
fn show(m: &Case) -> String {
    match m.api {
        "dxbc" => ref_dxbc::parse(&m.code).map(|p| p.show()).unwrap_or_else(|e| e),
        _ => ref_d3d9::parse(&words(&m.code)).map(|p| p.show()).unwrap_or_else(|e| e),
    }
}

pub fn cases(seeds: &[Case], n: usize, seed: u64) -> Vec<Case> {
    let seeds: Vec<&Case> = seeds.iter().filter(|c| c.api != "glsl" && !c.code.is_empty()).collect();
    let mut r = Rng(seed | 1);
    let mut out = Vec::new();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for k in 0..n {
        let c = seeds[r.below(seeds.len())];
        let keep = if c.api == "dxbc" { 8 + 2 * 4 } else { 1 };
        let w = mutate(&mut r, &words(&c.code), keep);
        let code = to_bytes(&w);
        if code == c.code {
            continue;
        }
        let shown = std::env::var("IPPAI_ORACLE_SHOW").ok() == Some(k.to_string());
        if shown {
            let hex = w.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" ");
            eprintln!("---- input\n{}: {hex}", if c.api == "dxbc" { "DXBC" } else { "D3D9 tokens" });
        }
        let Some((wgsl, group, binding, size)) = translate(c.api, &code) else { continue };
        // (WebGPU's least maxUniformBufferBindingSize)
        if size > 65536 {
            continue;
        }
        // (what the runner can draw: one float4 target, no textures)
        let target = wgsl.lines().any(|l| l.trim_start().starts_with("@location(0)") && (l.contains("vec4<f32>") || l.contains("vec4f")) && !l.contains("fn "));
        if !target || wgsl.contains(": texture_") || wgsl.contains(": sampler") || super::fuzz::valid(&wgsl).is_err() {
            continue;
        }
        let m = Case { name: format!("oracle/{}#{k}", c.name), api: c.api, wgsl, src: String::new(), code, group, binding, pixels: Vec::new(), tol: Tol::Rel(2e-3, 2e-3), rule: "a mutant of the case: what the reference interpreter gives" };
        let mut pixels = Vec::new();
        let mut ok = true;
        for (data, _) in &c.pixels {
            let mut d = data.clone();
            if d.len() < size {
                d.resize(size, 0);
            }
            match std::panic::catch_unwind(|| interpret(&m, &d)) {
                Ok(Ok((got, None))) => {
                    let want: [Want; 4] = got.map(|v| (v.is_finite() && v.abs() < 1e6).then(|| vec![v]));
                    pixels.push((d, want));
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if shown {
            let (a, b) = (words(&c.code), words(&m.code));
            let diff: Vec<String> = (0..a.len()).filter(|&i| a[i] != b[i]).map(|i| format!("word {i}: {:#010x} -> {:#010x}", a[i], b[i])).collect();
            eprintln!("{}\n---- mutant of {} ({})\n{}\n---- WGSL\n{}", show(&m), c.name, diff.join(", "), c.src, m.wgsl);
        }
        if ok && pixels.iter().any(|p| p.1.iter().any(|w| w.is_some())) {
            out.push(Case { pixels, ..m });
        }
    }
    std::panic::set_hook(hook);
    out
}
