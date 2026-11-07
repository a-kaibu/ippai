//! Intrinsic functions and texture methods (the ones written in HLSL are in prelude.rs).

use super::ast::*;
use super::lower::{Obj, Val, C, L};
use super::sm4::{self, Dst, Reg, RegF, Src};

type R<T> = Result<T, String>;

/// Intrinsics (and prelude functions) that take no out arguments.
pub const NO_OUT: &[&str] = &[
    "sqrt", "rsqrt", "frac", "floor", "ceil", "round", "trunc", "exp2", "log2", "ddx", "ddy", "ddx_coarse", "ddy_coarse", "ddx_fine",
    "ddy_fine", "rcp", "sin", "cos", "abs", "saturate", "min", "max", "clamp", "lerp", "mad", "dot", "cross", "length", "distance",
    "normalize", "step", "sign", "any", "all", "isnan", "isinf", "isfinite", "pow", "mul", "transpose", "clip", "asfloat", "asint",
    "asuint", "countbits", "reversebits", "firstbitlow", "firstbithigh", "f32tof16", "f16tof32", "fwidth", "atan", "atan2", "acos",
    "asin", "tan", "exp", "log", "log10", "sinh", "cosh", "tanh", "smoothstep", "fmod", "degrees", "radians", "ldexp", "reflect",
    "refract", "faceforward", "determinant", "tex1D", "tex2D", "tex3D", "texCUBE", "tex1Dlod", "tex2Dlod", "tex3Dlod", "texCUBElod",
    "tex2Dbias", "tex2Dproj", "tex2Dgrad", "texCUBEbias", "texCUBEgrad", "tex3Dbias", "tex1Dgrad", "tex3Dgrad", "tex1Dbias",
    "tex1Dproj", "tex3Dproj", "texCUBEproj",
];

fn imm(v: u32, n: usize) -> Vec<C> {
    vec![C::I(v); n]
}

fn temp(t: u32) -> Reg {
    Reg { f: RegF::Temp(t), rel: None }
}

impl L<'_> {
    fn float(&mut self, v: Val) -> R<Val> {
        let t = if v.ty.numeric() {
            v.ty.with_base(B::Float)
        } else {
            return Err("a number is needed".into());
        };
        self.x_cast(v, &t)
    }

    /// arguments converted to one common type
    fn same(&mut self, vs: Vec<Val>, float: bool) -> R<(Ty, Vec<Vec<C>>)> {
        let mut t = vs[0].ty.clone();
        for v in &vs[1..] {
            t = self.x_common(&t, &v.ty)?;
        }
        if float || t.base() == B::Bool {
            t = t.with_base(if float { B::Float } else { B::Int });
        }
        let mut out = Vec::new();
        for v in vs {
            out.push(self.x_cast(v, &t)?.c);
        }
        Ok((t, out))
    }

    fn unary_f(&mut self, op: u32, v: Val) -> R<Val> {
        let v = self.float(v)?;
        let c = self.x_alu(op, B::Float, &[&v.c]);
        Ok(Val::num(v.ty, c))
    }

    fn dot(&mut self, a: &[C], b: &[C], base: B) -> C {
        let n = a.len();
        if base == B::Float {
            if n == 1 {
                return self.x_alu(sm4::MUL, B::Float, &[a, b])[0];
            }
            let op = [sm4::DP2, sm4::DP3, sm4::DP4][n.clamp(2, 4) - 2];
            if a.iter().chain(b).all(|c| matches!(c, C::I(_))) {
                let s: f32 = a.iter().zip(b).map(|(x, y)| if let (C::I(x), C::I(y)) = (x, y) { f32::from_bits(*x) * f32::from_bits(*y) } else { 0.0 }).sum();
                return C::I(s.to_bits());
            }
            let t = self.x_tmp();
            let sa = self.x_opnd(a);
            let sb = self.x_opnd(b);
            self.x_emit(op, vec![Dst { r: temp(t), mask: 1 }], vec![sa, sb]);
            return C::R(temp(t), 0);
        }
        let p = self.x_alu2(sm4::IMUL, &[a, b], true);
        let mut s = p[0];
        for c in &p[1..] {
            s = self.x_alu(sm4::IADD, base, &[&[s], &[*c]])[0];
        }
        s
    }

    /// sum_i coef[i] * vecs[i]: one dp per output component when `cols` are in one register each,
    /// a mad chain otherwise
    fn lin(&mut self, coef: &[C], rows: &[Vec<C>], base: B) -> Vec<C> {
        let m = rows[0].len();
        let cols: Vec<Vec<C>> = (0..m).map(|j| rows.iter().map(|r| r[j]).collect()).collect();
        let one_reg = |v: &Vec<C>| match v[0] {
            C::R(r0, _) => v.iter().all(|c| matches!(c, C::R(r, _) if *r == r0)),
            _ => false,
        };
        if base == B::Float && cols.iter().all(one_reg) && coef.len() > 1 {
            return cols.iter().map(|c| self.dot(coef, c, base)).collect();
        }
        let (mul, mad) = if base == B::Float { (sm4::MUL, sm4::MAD) } else { (sm4::IMUL, sm4::IMAD) };
        let mut acc = if base == B::Float {
            self.x_alu(mul, base, &[&vec![coef[0]; m], &rows[0]])
        } else {
            self.x_alu2(mul, &[&vec![coef[0]; m], &rows[0]], true)
        };
        for (i, r) in rows.iter().enumerate().skip(1) {
            acc = self.x_alu(mad, base, &[&vec![coef[i]; m], r, &acc]);
        }
        acc
    }

    fn mul(&mut self, a: Val, b: Val) -> R<Val> {
        let base = if a.ty.base() == B::Float || b.ty.base() == B::Float { B::Float } else { B::Int };
        let a = self.x_cast(a.clone(), &a.ty.with_base(base))?;
        let b = self.x_cast(b.clone(), &b.ty.with_base(base))?;
        match (&a.ty, &b.ty) {
            (Ty::Scalar(_), _) | (_, Ty::Scalar(_)) => {
                let t = self.x_common(&a.ty, &b.ty)?;
                let x = self.x_cast(a, &t)?;
                let y = self.x_cast(b, &t)?;
                let c = if base == B::Float { self.x_alu(sm4::MUL, base, &[&x.c, &y.c]) } else { self.x_alu2(sm4::IMUL, &[&x.c, &y.c], true) };
                Ok(Val::num(t, c))
            }
            (Ty::Vec(_, n), Ty::Vec(_, m)) => {
                let k = (*n).min(*m) as usize;
                Ok(Val::num(Ty::Scalar(base), vec![self.dot(&a.c[..k], &b.c[..k], base)]))
            }
            (Ty::Vec(_, n), Ty::Mat(_, r, c)) => {
                let k = (*n).min(*r) as usize;
                let c = *c as usize;
                let rows: Vec<Vec<C>> = (0..k).map(|i| b.c[i * c..(i + 1) * c].to_vec()).collect();
                let v = self.lin(&a.c[..k], &rows, base);
                Ok(Val::num(Ty::vec(base, c), v))
            }
            (Ty::Mat(_, r, c), Ty::Vec(_, n)) => {
                let (r, c) = (*r as usize, *c as usize);
                let k = (*n as usize).min(c);
                // M v: columns of M weighted by v (or a dp per row)
                let cols: Vec<Vec<C>> = (0..k).map(|j| (0..r).map(|i| a.c[i * c + j]).collect()).collect();
                let v = self.lin(&b.c[..k], &cols, base);
                Ok(Val::num(Ty::vec(base, r), v))
            }
            (Ty::Mat(_, r1, c1), Ty::Mat(_, r2, c2)) => {
                let (r1, c1, r2, c2) = (*r1 as usize, *c1 as usize, *r2 as usize, *c2 as usize);
                let k = c1.min(r2);
                let rows: Vec<Vec<C>> = (0..k).map(|i| b.c[i * c2..(i + 1) * c2].to_vec()).collect();
                let mut out = Vec::new();
                for i in 0..r1 {
                    let v = self.lin(&a.c[i * c1..i * c1 + k], &rows, base);
                    out.extend(v);
                }
                Ok(Val::num(Ty::Mat(base, r1 as u8, c2 as u8), out))
            }
            _ => Err("mul of these types".into()),
        }
    }

    fn texture_of(&self, v: &Val) -> R<u32> {
        match v.obj {
            Obj::Tex(t) | Obj::Legacy(t, _) => Ok(t),
            _ => Err("a texture is needed".into()),
        }
    }
    fn sampler_of(&self, v: &Val) -> R<u32> {
        match v.obj {
            Obj::Samp(s) | Obj::Legacy(_, s) => Ok(s),
            _ => Err("a sampler is needed".into()),
        }
    }

    /// texel offsets of an immediate int vector
    fn offset_of(&mut self, e: Option<&Expr>) -> R<[i32; 3]> {
        let Some(e) = e else { return Ok([0; 3]) };
        let v = self.expr(e)?;
        let v = self.x_cast(v.clone(), &v.ty.with_base(B::Int))?;
        let mut o = [0; 3];
        for (k, c) in v.c.iter().take(3).enumerate() {
            match c {
                C::I(x) => o[k] = *x as i32,
                _ => return Err("texel offsets must be constants".into()),
            }
        }
        Ok(o)
    }

    /// sample* instruction; result: the texture's element
    #[allow(clippy::too_many_arguments)]
    fn sample(&mut self, op: u32, t: u32, s: u32, coord: Val, extra: Vec<Vec<C>>, offset: [i32; 3], swz_x: bool) -> R<Val> {
        let (dim, el) = self.x_tex_el(t);
        let n = dim.coords() + dim.array() as usize;
        let coord = self.float(coord)?;
        let mut cc = coord.c.clone();
        if cc.len() < n {
            return Err(format!("{n} texture coordinates needed"));
        }
        cc.truncate(n);
        let dst = self.x_tmp();
        let mut src = vec![self.x_opnd(&cc), Src::Tex(t, if swz_x { [0; 4] } else { [0, 1, 2, 3] }), Src::Samp(s)];
        for e in extra {
            let o = self.x_opnd(&e);
            src.push(o);
        }
        let comps = if swz_x { 1 } else { el.comps().max(1) };
        self.x_emit(op, vec![Dst { r: temp(dst), mask: if swz_x { 1 } else { 0xf } }], src).offset = offset;
        let ty = if swz_x { Ty::Scalar(B::Float) } else { el.clone() };
        Ok(Val::num(ty, (0..comps).map(|k| C::R(temp(dst), k as u8)).collect()))
    }

    pub(super) fn load(&mut self, t: u32, coord: Val, offset: [i32; 3], has_mip: bool) -> R<Val> {
        let (dim, el) = self.x_tex_el(t);
        let coord = self.x_cast(coord.clone(), &coord.ty.with_base(B::Int))?;
        let n = dim.coords() + dim.array() as usize;
        let c = &coord.c;
        let mip = if has_mip { c.get(n).copied().unwrap_or(C::I(0)) } else { C::I(0) };
        let z = C::I(0);
        let addr: Vec<C> = match dim {
            Dim::Buffer => vec![c[0], z, z, z],
            Dim::T1D => vec![c[0], mip, z, z],
            Dim::T2D => vec![c[0], c[1], z, mip],
            Dim::T1DArray => vec![c[0], c[1], z, mip],
            Dim::T2DArray | Dim::T3D => vec![c[0], c[1], c[2], mip],
            _ => return Err("Load from this texture type".into()),
        };
        let dst = self.x_tmp();
        let a = self.x_opnd(&addr);
        self.x_emit(sm4::LD, vec![Dst { r: temp(dst), mask: 0xf }], vec![a, Src::Tex(t, [0, 1, 2, 3])]).offset = offset;
        let comps = el.comps().max(1);
        Ok(Val::num(el, (0..comps).map(|k| C::R(temp(dst), k as u8)).collect()))
    }

    fn legacy(&mut self, name: &str, args: &[Expr]) -> R<Option<Val>> {
        let base = name.trim_end_matches(|c: char| c.is_ascii_lowercase()).to_string();
        let (dimname, rest) = match name.strip_prefix("tex") {
            Some(r) => {
                let d = ["1D", "2D", "3D", "CUBE"].iter().find(|d| r.starts_with(**d));
                match d {
                    Some(d) => (*d, &r[d.len()..]),
                    None => return Ok(None),
                }
            }
            None => return Ok(None),
        };
        let _ = base;
        let s = self.expr(&args[0])?;
        let t = self.texture_of(&s)?;
        let sm = self.sampler_of(&s)?;
        let n = match dimname {
            "1D" => 1,
            "2D" => 2,
            _ => 3,
        };
        let coord = self.expr(&args[1])?;
        let coord = self.float(coord)?;
        let pick = |v: &Val, k: usize| -> Vec<C> { v.c[..k.min(v.c.len())].to_vec() };
        let r = match rest {
            "" if args.len() == 4 => {
                let dx = self.expr(&args[2])?;
                let dy = self.expr(&args[3])?;
                let dx = self.float(dx)?;
                let dy = self.float(dy)?;
                self.sample(sm4::SAMPLE_D, t, sm, Val::num(coord.ty.clone(), pick(&coord, n)), vec![pick(&dx, n), pick(&dy, n)], [0; 3], false)?
            }
            "" => self.sample(sm4::SAMPLE, t, sm, Val::num(coord.ty.clone(), pick(&coord, n)), vec![], [0; 3], false)?,
            "grad" => {
                let dx = self.expr(&args[2])?;
                let dy = self.expr(&args[3])?;
                let dx = self.float(dx)?;
                let dy = self.float(dy)?;
                self.sample(sm4::SAMPLE_D, t, sm, Val::num(coord.ty.clone(), pick(&coord, n)), vec![pick(&dx, n), pick(&dy, n)], [0; 3], false)?
            }
            "lod" => {
                let w = coord.c[3];
                self.sample(sm4::SAMPLE_L, t, sm, Val::num(coord.ty.clone(), pick(&coord, n)), vec![vec![w]], [0; 3], false)?
            }
            "bias" => {
                let w = coord.c[3];
                self.sample(sm4::SAMPLE_B, t, sm, Val::num(coord.ty.clone(), pick(&coord, n)), vec![vec![w]], [0; 3], false)?
            }
            "proj" => {
                let w = coord.c[3];
                let c = pick(&coord, n);
                let d = self.x_alu(sm4::DIV, B::Float, &[&c, &vec![w; c.len()]]);
                self.sample(sm4::SAMPLE, t, sm, Val::num(Ty::vec(B::Float, n), d), vec![], [0; 3], false)?
            }
            _ => return Ok(None),
        };
        Ok(Some(r))
    }

    fn vals(&mut self, args: &[Expr]) -> R<Vec<Val>> {
        args.iter().map(|a| self.expr(a)).collect()
    }
}

pub fn call(l: &mut L, name: &str, args: &[Expr], line: u32) -> R<Val> {
    use sm4::*;
    let need = |n: usize| -> R<()> {
        if args.len() != n {
            Err(format!("line {line}: {name} takes {n} arguments"))
        } else {
            Ok(())
        }
    };
    if let Some(v) = l.legacy(name, args)? {
        return Ok(v);
    }
    Ok(match name {
        "GroupMemoryBarrier" | "GroupMemoryBarrierWithGroupSync" | "DeviceMemoryBarrier" | "DeviceMemoryBarrierWithGroupSync" | "AllMemoryBarrier"
        | "AllMemoryBarrierWithGroupSync" => {
            need(0)?;
            if !l.o.compute {
                return Err(format!("line {line}: {name} outside a compute shader"));
            }
            // sync flags: 1 threads in the group, 2 groupshared memory, 8 UAV memory
            let mem = match name.trim_end_matches("WithGroupSync") {
                "GroupMemoryBarrier" => 2,
                "DeviceMemoryBarrier" => 8,
                _ => 10,
            };
            let ctl = mem | name.ends_with("WithGroupSync") as u32;
            l.x_emit(SYNC, vec![], vec![]).ctl = ctl;
            Val::num(Ty::Void, vec![])
        }
        "sqrt" | "rsqrt" | "frac" | "floor" | "ceil" | "round" | "trunc" | "exp2" | "log2" | "ddx" | "ddy" | "ddx_coarse" | "ddy_coarse"
        | "ddx_fine" | "ddy_fine" => {
            need(1)?;
            let op = match name {
                "sqrt" => SQRT,
                "rsqrt" => RSQ,
                "frac" => FRC,
                "floor" => ROUND_NI,
                "ceil" => ROUND_PI,
                "round" => ROUND_NE,
                "trunc" => ROUND_Z,
                "exp2" => EXP,
                "log2" => LOG,
                "ddx" => DERIV_RTX,
                "ddy" => DERIV_RTY,
                "ddx_coarse" => 0x7a,
                "ddx_fine" => 0x7b,
                "ddy_coarse" => 0x7c,
                _ => 0x7d,
            };
            let v = l.expr(&args[0])?;
            l.unary_f(op, v)?
        }
        "rcp" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            let c = l.x_alu(DIV, B::Float, &[&imm(1f32.to_bits(), v.c.len()), &v.c]);
            Val::num(v.ty, c)
        }
        "sin" | "cos" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            if v.c.iter().all(|c| matches!(c, C::I(_))) {
                let c = v.c.iter().map(|c| if let C::I(x) = c { let f = f32::from_bits(*x); C::I(if name == "sin" { f.sin() } else { f.cos() }.to_bits()) } else { *c }).collect();
                return Ok(Val::num(v.ty, c));
            }
            let mut out = Vec::new();
            for ch in v.c.chunks(4) {
                let t = l.x_tmp();
                let s = l.x_opnd(ch);
                let d = Dst { r: temp(t), mask: (1u8 << ch.len()) - 1 };
                let null = Dst { r: Reg { f: RegF::Null, rel: None }, mask: 0 };
                let dst = if name == "sin" { vec![d, null] } else { vec![null, d] };
                l.x_emit(SINCOS, dst, vec![s]);
                out.extend((0..ch.len()).map(|k| C::R(temp(t), k as u8)));
            }
            Val::num(v.ty, out)
        }
        "sincos" => {
            need(3)?;
            let v = l.expr(&args[0])?;
            let s = call(l, "sin", &args[..1], line)?;
            let c = call(l, "cos", &args[..1], line)?;
            let _ = v;
            let ps = l.x_place(&args[1])?;
            let pc = l.x_place(&args[2])?;
            let s = l.x_cast(s, &ps.ty)?;
            let c = l.x_cast(c, &pc.ty)?;
            l.x_assign(&ps.c, &s.c);
            l.x_assign(&pc.c, &c.c);
            Val::num(Ty::Void, vec![])
        }
        "abs" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            if v.ty.base() == B::Float {
                let c = l.x_alu_m(MOV, B::Float, &[(&v.c, 2)]);
                Val::num(v.ty, c)
            } else {
                let v = l.x_cast(v.clone(), &v.ty.with_base(B::Int))?;
                let n = l.x_alu(INEG, B::Int, &[&v.c]);
                let c = l.x_alu(IMAX, B::Int, &[&v.c, &n]);
                Val::num(v.ty, c)
            }
        }
        "saturate" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            if v.c.iter().all(|c| matches!(c, C::I(_))) {
                let c = v.c.iter().map(|c| if let C::I(x) = c { C::I(f32::from_bits(*x).clamp(0.0, 1.0).to_bits()) } else { *c }).collect();
                return Ok(Val::num(v.ty, c));
            }
            let mut out = Vec::new();
            for ch in v.c.chunks(4) {
                let t = l.x_tmp();
                let d: Vec<C> = (0..ch.len()).map(|k| C::R(temp(t), k as u8)).collect();
                l.x_assign_sat(&d, ch);
                out.extend(d);
            }
            Val::num(v.ty, out)
        }
        "min" | "max" => {
            need(2)?;
            let vs = l.vals(args)?;
            let (t, c) = l.same(vs, false)?;
            let op = match (name, t.base()) {
                ("min", B::Float) => MIN,
                ("max", B::Float) => MAX,
                ("min", B::Uint) => UMIN,
                ("max", B::Uint) => UMAX,
                ("min", _) => IMIN,
                _ => IMAX,
            };
            let r = l.x_alu(op, t.base(), &[&c[0], &c[1]]);
            Val::num(t, r)
        }
        "clamp" => {
            need(3)?;
            let vs = l.vals(args)?;
            let (t, c) = l.same(vs, false)?;
            let (mx, mn) = match t.base() {
                B::Float => (MAX, MIN),
                B::Uint => (UMAX, UMIN),
                _ => (IMAX, IMIN),
            };
            let a = l.x_alu(mx, t.base(), &[&c[0], &c[1]]);
            let r = l.x_alu(mn, t.base(), &[&a, &c[2]]);
            Val::num(t, r)
        }
        "lerp" => {
            need(3)?;
            let vs = l.vals(args)?;
            let (t, c) = l.same(vs, true)?;
            // a + t (b - a)
            let d = l.x_alu_m(ADD, B::Float, &[(&c[1], 0), (&c[0], 1)]);
            let r = l.x_alu(MAD, B::Float, &[&c[2], &d, &c[0]]);
            Val::num(t, r)
        }
        "mad" => {
            need(3)?;
            let vs = l.vals(args)?;
            let (t, c) = l.same(vs, false)?;
            let r = if t.base() == B::Float {
                l.x_alu(MAD, B::Float, &[&c[0], &c[1], &c[2]])
            } else {
                l.x_alu(IMAD, t.base(), &[&c[0], &c[1], &c[2]])
            };
            Val::num(t, r)
        }
        "dot" => {
            need(2)?;
            let vs = l.vals(args)?;
            let (t, c) = l.same(vs, false)?;
            let b = t.base();
            let d = l.dot(&c[0], &c[1], b);
            Val::num(Ty::Scalar(b), vec![d])
        }
        "cross" => {
            need(2)?;
            let vs = l.vals(args)?;
            let (_, c) = l.same(vs, true)?;
            let (a, b) = (&c[0], &c[1]);
            if a.len() != 3 {
                return Err(format!("line {line}: cross of float3s only"));
            }
            let azxy = [a[2], a[0], a[1]];
            let byzx = [b[1], b[2], b[0]];
            let ayzx = [a[1], a[2], a[0]];
            let bzxy = [b[2], b[0], b[1]];
            let m = l.x_alu(MUL, B::Float, &[&azxy, &byzx]);
            let r = l.x_alu_m(MAD, B::Float, &[(&ayzx, 0), (&bzxy, 0), (&m, 1)]);
            Val::num(Ty::Vec(B::Float, 3), r)
        }
        "length" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            length(l, &v.c)?
        }
        "distance" => {
            need(2)?;
            let vs = l.vals(args)?;
            let (_, c) = l.same(vs, true)?;
            let d = l.x_alu_m(ADD, B::Float, &[(&c[0], 0), (&c[1], 1)]);
            length(l, &d)?
        }
        "normalize" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            let d = l.dot(&v.c, &v.c, B::Float);
            let r = l.x_alu(RSQ, B::Float, &[&[d]]);
            let n = v.c.len();
            let c = l.x_alu(MUL, B::Float, &[&v.c, &vec![r[0]; n]]);
            Val::num(v.ty, c)
        }
        "step" => {
            need(2)?;
            let vs = l.vals(args)?;
            let (t, c) = l.same(vs, true)?;
            let m = l.x_alu(GE, B::Bool, &[&c[1], &c[0]]);
            let n = m.len();
            let r = l.x_alu(AND, B::Float, &[&m, &imm(1f32.to_bits(), n)]);
            Val::num(t, r)
        }
        "sign" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let n = v.c.len();
            let (gt, lt) = if v.ty.base() == B::Float {
                (l.x_alu(LT, B::Bool, &[&imm(0, n), &v.c]), l.x_alu(LT, B::Bool, &[&v.c, &imm(0, n)]))
            } else {
                let v = l.x_cast(v.clone(), &v.ty.with_base(B::Int))?;
                (l.x_alu(ILT, B::Bool, &[&imm(0, n), &v.c]), l.x_alu(ILT, B::Bool, &[&v.c, &imm(0, n)]))
            };
            // (true is -1, or 1.0 in shader model 2/3)
            let r = if l.x_sm3() { l.x_alu_m(IADD, B::Int, &[(&gt, 0), (&lt, 1)]) } else { l.x_alu_m(IADD, B::Int, &[(&lt, 0), (&gt, 1)]) };
            Val::num(v.ty.with_base(B::Int), r)
        }
        "any" | "all" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let b = l.x_cast(v.clone(), &v.ty.with_base(B::Bool))?;
            let mut acc = b.c[0];
            for c in &b.c[1..] {
                acc = l.x_alu(if name == "any" { OR } else { AND }, B::Bool, &[&[acc], &[*c]])[0];
            }
            Val::num(Ty::Scalar(B::Bool), vec![acc])
        }
        "isnan" | "isinf" | "isfinite" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            let n = v.c.len();
            let inf = imm(f32::INFINITY.to_bits(), n);
            let r = match name {
                // (fxc assumes no NaNs unless asked for IEEE strictness: isnan is false)
                "isnan" if !l.o.ieee => imm(0, n),
                "isnan" => l.x_alu(NE, B::Bool, &[&v.c, &v.c]),
                "isinf" => l.x_alu_m(EQ, B::Float, &[(&v.c, 2), (&inf, 0)]),
                _ => l.x_alu_m(LT, B::Float, &[(&v.c, 2), (&inf, 0)]),
            };
            Val::num(v.ty.with_base(B::Bool), r)
        }
        "pow" => {
            need(2)?;
            let vs = l.vals(args)?;
            let (t, c) = l.same(vs, true)?;
            // small constant integer exponents: multiplications
            let small = c[1].iter().all(|x| *x == c[1][0]) && matches!(c[1][0], C::I(e) if [1.0, 2.0, 3.0, 4.0].contains(&f32::from_bits(e)));
            if small {
                let C::I(e) = c[1][0] else { unreachable!() };
                let mut r = c[0].clone();
                for _ in 1..f32::from_bits(e) as u32 {
                    r = l.x_alu(MUL, B::Float, &[&r, &c[0]]);
                }
                return Ok(Val::num(t, r));
            }
            let lg = l.x_alu(LOG, B::Float, &[&c[0]]);
            let m = l.x_alu(MUL, B::Float, &[&lg, &c[1]]);
            let r = l.x_alu(EXP, B::Float, &[&m]);
            Val::num(t, r)
        }
        "mul" => {
            need(2)?;
            let a = l.expr(&args[0])?;
            let b = l.expr(&args[1])?;
            l.mul(a, b)?
        }
        "transpose" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let Ty::Mat(b, r, c) = v.ty else {
                return Err(format!("line {line}: transpose of a non-matrix"));
            };
            let mut out = Vec::new();
            for j in 0..c as usize {
                for i in 0..r as usize {
                    out.push(v.c[i * c as usize + j]);
                }
            }
            Val::num(Ty::Mat(b, c, r), out)
        }
        "clip" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            let n = v.c.len();
            if l.x_sm3() && !v.c.iter().all(|c| matches!(c, C::I(_))) {
                // (texkill: any component below 0)
                let o = l.x_opnd(&v.c);
                let i = l.x_emit(DISCARD, vec![], vec![o]);
                i.ctl = n as u32;
                return Ok(Val::num(Ty::Void, vec![]));
            }
            let m = l.x_alu(LT, B::Bool, &[&v.c, &imm(0, n)]);
            for c in m {
                if c != C::I(0) {
                    l.x_cond(DISCARD, c, true);
                }
            }
            Val::num(Ty::Void, vec![])
        }
        "asfloat" | "asint" | "asuint" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let b = match name {
                "asfloat" => B::Float,
                "asint" => B::Int,
                _ => B::Uint,
            };
            Val::num(v.ty.with_base(b), v.c)
        }
        "countbits" | "reversebits" | "firstbitlow" | "firstbithigh" | "f32tof16" | "f16tof32" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let (op, from, to) = match name {
                "countbits" => (COUNTBITS, B::Uint, B::Uint),
                "reversebits" => (BFREV, B::Uint, B::Uint),
                "firstbitlow" => (FIRSTBIT_LO, B::Uint, B::Uint),
                "firstbithigh" if v.ty.base() == B::Int => (FIRSTBIT_SHI, B::Int, B::Int),
                "firstbithigh" => (FIRSTBIT_HI, B::Uint, B::Uint),
                "f32tof16" => (F32TOF16, B::Float, B::Uint),
                _ => (F16TOF32, B::Uint, B::Float),
            };
            let v = l.x_cast(v.clone(), &v.ty.with_base(from))?;
            let mut c = l.x_alu(op, to, &[&v.c]);
            if name == "firstbithigh" {
                // (firstbit_hi counts from the top bit, HLSL's firstbithigh from bit 0:
                // 31 - x, and no bit stays 0xffffffff)
                let n = c.len();
                let none = l.x_alu(sm4::IEQ, B::Bool, &[&c, &imm(u32::MAX, n)]);
                let neg = l.x_alu(INEG, B::Int, &[&c]);
                let lsb = l.x_alu(IADD, B::Int, &[&neg, &imm(31, n)]);
                c = l.x_alu(sm4::MOVC, to, &[&none, &c, &lsb]);
            }
            Val::num(v.ty.with_base(to), c)
        }
        "fwidth" => {
            need(1)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            let x = l.x_alu(DERIV_RTX, B::Float, &[&v.c]);
            let y = l.x_alu(DERIV_RTY, B::Float, &[&v.c]);
            let r = l.x_alu_m(ADD, B::Float, &[(&x, 2), (&y, 2)]);
            Val::num(v.ty, r)
        }
        "modf" => {
            need(2)?;
            let v = l.expr(&args[0])?;
            let v = l.float(v)?;
            let ip = l.x_alu(ROUND_Z, B::Float, &[&v.c]);
            let p = l.x_place(&args[1])?;
            let ipv = l.x_cast(Val::num(v.ty.clone(), ip.clone()), &p.ty)?;
            l.x_assign(&p.c, &ipv.c);
            let r = l.x_alu_m(ADD, B::Float, &[(&v.c, 0), (&ip, 1)]);
            Val::num(v.ty, r)
        }
        _ => return Err(format!("line {line}: unknown function '{name}'")),
    })
}

fn length(l: &mut L, c: &[C]) -> R<Val> {
    if c.len() == 1 {
        let r = l.x_alu_m(sm4::MOV, B::Float, &[(c, 2)]);
        return Ok(Val::num(Ty::Scalar(B::Float), r));
    }
    let d = l.dot(c, c, B::Float);
    let r = l.x_alu(sm4::SQRT, B::Float, &[&[d]]);
    Ok(Val::num(Ty::Scalar(B::Float), r))
}

pub fn method(l: &mut L, o: Val, name: &str, args: &[Expr]) -> R<Val> {
    use sm4::*;
    let t = l.texture_of(&o)?;
    let (dim, _) = l.x_tex_el(t);
    let n = dim.coords();
    let samp = |l: &mut L, k: usize| -> R<u32> {
        let v = l.expr(args.get(k).ok_or("missing sampler argument")?)?;
        l.sampler_of(&v)
    };
    let arg = |l: &mut L, k: usize| -> R<Val> { l.expr(args.get(k).ok_or(format!("missing argument {k} of {name}"))?) };
    let fl = |l: &mut L, v: Val, k: usize| -> R<Vec<C>> {
        let v = l.float(v)?;
        Ok(v.c[..k.min(v.c.len())].to_vec())
    };
    Ok(match name {
        "Sample" => {
            let s = samp(l, 0)?;
            let c = arg(l, 1)?;
            let off = l.offset_of(args.get(2))?;
            l.sample(SAMPLE, t, s, c, vec![], off, false)?
        }
        "SampleLevel" | "SampleBias" => {
            let s = samp(l, 0)?;
            let c = arg(l, 1)?;
            let lod = arg(l, 2)?;
            let lod = fl(l, lod, 1)?;
            let off = l.offset_of(args.get(3))?;
            l.sample(if name == "SampleLevel" { SAMPLE_L } else { SAMPLE_B }, t, s, c, vec![lod], off, false)?
        }
        "SampleGrad" => {
            let s = samp(l, 0)?;
            let c = arg(l, 1)?;
            let dx = arg(l, 2)?;
            let dx = fl(l, dx, n)?;
            let dy = arg(l, 3)?;
            let dy = fl(l, dy, n)?;
            let off = l.offset_of(args.get(4))?;
            l.sample(SAMPLE_D, t, s, c, vec![dx, dy], off, false)?
        }
        "SampleCmp" | "SampleCmpLevelZero" => {
            let s = samp(l, 0)?;
            l.x_samp_cmp(s);
            let c = arg(l, 1)?;
            let r = arg(l, 2)?;
            let r = fl(l, r, 1)?;
            let off = l.offset_of(args.get(3))?;
            l.sample(if name == "SampleCmp" { SAMPLE_C } else { SAMPLE_C_LZ }, t, s, c, vec![r], off, true)?
        }
        "Gather" | "GatherRed" | "GatherGreen" | "GatherBlue" | "GatherAlpha" => {
            let s = samp(l, 0)?;
            let c = arg(l, 1)?;
            let off = l.offset_of(args.get(2))?;
            let mut v = l.sample(GATHER4, t, s, c, vec![], off, false)?;
            let ch = match name {
                "GatherGreen" => 1,
                "GatherBlue" => 2,
                "GatherAlpha" => 3,
                _ => 0,
            };
            if ch != 0 {
                // (the channel: the sampler operand's component select)
                l.x_last().src[2] = Src::SampC(s, ch);
            }
            v.ty = Ty::Vec(B::Float, 4);
            let C::R(r, _) = v.c[0] else { unreachable!() };
            v.c = (0..4).map(|k| C::R(r, k)).collect();
            v
        }
        "Load" => {
            let c = arg(l, 0)?;
            let off = l.offset_of(args.get(1))?;
            l.load(t, c, off, dim != Dim::Buffer)?
        }
        "GetDimensions" => {
            // ([mip,] width[, height][, elements/depth], [levels])
            let dst = l.x_tmp();
            let has_mip = dim != Dim::Buffer && args.len() > n + dim.array() as usize;
            let mip = if has_mip {
                let m = arg(l, 0)?;
                l.x_cast(m, &Ty::Scalar(B::Uint))?.c
            } else {
                vec![C::I(0)]
            };
            let ms = l.x_opnd(&mip);
            let mut ins_ctl = 2;
            let outs = &args[has_mip as usize..];
            // (float outputs: resinfo float)
            let first = l.x_place(&outs[0])?;
            if first.ty.base() == B::Float {
                ins_ctl = 0;
            }
            l.x_emit(RESINFO, vec![Dst { r: temp(dst), mask: 0xf }], vec![ms, Src::Tex(t, [0, 1, 2, 3])]).ctl = ins_ctl;
            let comps: Vec<u8> = match outs.len() {
                1 => vec![0],
                2 => vec![0, 1],
                3 if dim.array() || dim == Dim::T3D || dim == Dim::T1DArray => vec![0, 1, 2],
                3 => vec![0, 1, 3],
                _ => vec![0, 1, 2, 3],
            };
            let rb = if ins_ctl == 2 { B::Uint } else { B::Float };
            for (k, e) in outs.iter().enumerate() {
                let p = l.x_place(e)?;
                let c = *comps.get(k).unwrap_or(&3);
                let v = Val::num(Ty::Scalar(rb), vec![C::R(temp(dst), c)]);
                let v = l.x_cast(v, &p.ty)?;
                l.x_assign(&p.c, &v.c);
            }
            let _ = l.x_major();
            Val::num(Ty::Void, vec![])
        }
        _ => return Err(format!("unknown texture method {name}")),
    })
}
