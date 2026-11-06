//! Random programs (seeded): float4 expressions, locals, if/else and counted loops over integer
//! values, printed as HLSL (shader model 4 and 2/3) and GLSL, evaluated here exactly (every
//! intermediate an integer below 2^22, so f32 arithmetic on the GPU has no rounding to differ
//! by). They exercise what the per-instruction cases do not: register allocation, swizzles and
//! masks across temps, constant folding, flow control lowering.

use super::cases::{d3d9_words, dxbc_raw, glsl_raw};
use super::{Case, Tol, Want};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn int(&mut self, lo: i64, hi: i64) -> f64 {
        (lo + self.below((hi - lo + 1) as u64) as i64) as f64
    }
}

#[derive(Clone)]
enum E {
    In(usize),
    Var(usize),
    Const([f64; 4]),
    Swz(Box<E>, [u8; 4]),
    Bin(u8, Box<E>, Box<E>),
    Un(u8, Box<E>),
    /// a.x < b.x ? c : d
    Sel(Box<E>, Box<E>, Box<E>, Box<E>),
    Dot(Box<E>, Box<E>),
    /// arr[index(e)]
    Arr(Box<E>),
}

enum S {
    /// t.mask = e.mask
    Mask(usize, [bool; 4], E),
    /// arr[index(i)] = e
    ArrSet(E, E),
    /// for i < index(e) (a count from the inputs: 0..3)
    Dyn(E, Vec<S>),
    Set(usize, E),
    Add(usize, E),
    If(E, E, Vec<S>, Vec<S>),
    For(u32, Vec<S>),
}

const VARS: usize = 3;
const LIMIT: f64 = (1 << 22) as f64;

fn expr(r: &mut Rng, depth: u32) -> E {
    if depth == 0 || r.below(4) == 0 {
        return match r.below(3) {
            0 => E::In(r.below(3) as usize),
            1 => E::Var(r.below(VARS as u64) as usize),
            _ => E::Const([r.int(-8, 8), r.int(-8, 8), r.int(-8, 8), r.int(-8, 8)]),
        };
    }
    let sub = |r: &mut Rng| Box::new(expr(r, depth - 1));
    match r.below(10) {
        0 => {
            let e = sub(r);
            E::Swz(e, [r.below(4) as u8, r.below(4) as u8, r.below(4) as u8, r.below(4) as u8])
        }
        1..=4 => {
            let (a, b) = (sub(r), sub(r));
            E::Bin(r.below(5) as u8, a, b)
        }
        5 | 6 => {
            let e = sub(r);
            E::Un(r.below(4) as u8, e)
        }
        7 | 8 => {
            let (a, b, c, d) = (sub(r), sub(r), sub(r), sub(r));
            E::Sel(a, b, c, d)
        }
        _ if r.below(2) == 0 => {
            let (a, b) = (sub(r), sub(r));
            E::Dot(a, b)
        }
        _ => E::Arr(sub(r)),
    }
}

fn stmts(r: &mut Rng, depth: u32, n: u32, dynamic: bool) -> Vec<S> {
    (0..n)
        .map(|_| match if depth == 0 { r.below(4) } else { r.below(7) } {
            0 => S::Set(r.below(VARS as u64) as usize, expr(r, 3)),
            1 => S::Add(r.below(VARS as u64) as usize, expr(r, 2)),
            2 => S::Mask(r.below(VARS as u64) as usize, loop {
                let m = [r.below(2) == 0, r.below(2) == 0, r.below(2) == 0, r.below(2) == 0];
                if m.iter().any(|&b| b) {
                    break m;
                }
            }, expr(r, 2)),
            3 => S::ArrSet(expr(r, 1), expr(r, 2)),
            4 => S::If(expr(r, 2), expr(r, 2), stmts(r, depth - 1, 2, dynamic), stmts(r, depth - 1, 1, dynamic)),
            5 if dynamic => S::Dyn(expr(r, 1), stmts(r, depth - 1, 2, dynamic)),
            _ => S::For(1 + r.below(3) as u32, stmts(r, depth - 1, 2, dynamic)),
        })
        .collect()
}

type V = [f64; 4];

struct Ev<'a> {
    ins: &'a [V; 3],
    vars: [V; VARS],
    arr: [V; 4],
    ok: bool,
}

/// An array index or loop count from a value: its x clamped to 0..3
fn index(v: V) -> usize {
    v[0].clamp(0.0, 3.0) as usize
}

impl Ev<'_> {
    fn chk(&mut self, v: V) -> V {
        if v.iter().any(|x| x.abs() > LIMIT) {
            self.ok = false;
        }
        v
    }
    fn e(&mut self, e: &E) -> V {
        let v = match e {
            E::In(i) => self.ins[*i],
            E::Var(i) => self.vars[*i],
            E::Const(c) => *c,
            E::Swz(a, s) => {
                let a = self.e(a);
                s.map(|k| a[k as usize])
            }
            E::Bin(op, a, b) => {
                let (a, b) = (self.e(a), self.e(b));
                std::array::from_fn(|i| match op {
                    0 => a[i] + b[i],
                    1 => a[i] - b[i],
                    2 => a[i] * b[i],
                    3 => a[i].min(b[i]),
                    _ => a[i].max(b[i]),
                })
            }
            E::Un(op, a) => {
                let a = self.e(a);
                a.map(|x| match op {
                    0 => -x,
                    1 => x.abs(),
                    2 => (x * 0.5).floor(),
                    _ => x.clamp(-64.0, 64.0),
                })
            }
            E::Sel(a, b, c, d) => {
                let (a, b, c, d) = (self.e(a), self.e(b), self.e(c), self.e(d));
                if a[0] < b[0] {
                    c
                } else {
                    d
                }
            }
            E::Dot(a, b) => {
                let (a, b) = (self.e(a), self.e(b));
                let d = (0..4).map(|i| a[i] * b[i]).sum();
                [d; 4]
            }
            E::Arr(i) => {
                let i = self.e(i);
                self.arr[index(i)]
            }
        };
        self.chk(v)
    }
    fn s(&mut self, s: &[S]) {
        for st in s {
            match st {
                S::Mask(v, m, e) => {
                    let x = self.e(e);
                    for i in 0..4 {
                        if m[i] {
                            self.vars[*v][i] = x[i];
                        }
                    }
                }
                S::ArrSet(i, e) => {
                    let (i, x) = (self.e(i), self.e(e));
                    self.arr[index(i)] = x;
                }
                S::Dyn(n, b) => {
                    // (the bound is evaluated before every iteration, as the shader does)
                    let mut i = 0;
                    while i < index(self.e(n)) {
                        self.s(b);
                        i += 1;
                    }
                }
                S::Set(v, e) => self.vars[*v] = self.e(e),
                S::Add(v, e) => {
                    let x = self.e(e);
                    let n = std::array::from_fn(|i| self.vars[*v][i] + x[i]);
                    self.vars[*v] = self.chk(n);
                }
                S::If(a, b, t, f) => {
                    let (a, b) = (self.e(a), self.e(b));
                    if a[0] < b[0] {
                        self.s(t)
                    } else {
                        self.s(f)
                    }
                }
                S::For(n, b) => {
                    for _ in 0..*n {
                        self.s(b)
                    }
                }
            }
        }
    }
}

/// HLSL and GLSL differ in the vector type, constructors of a broadcast, and inputs' names.
struct Lang {
    vec: &'static str,
    ins: [&'static str; 3],
    glsl: bool,
}

fn num(x: f64) -> String {
    format!("{x:.1}")
}

fn pe(l: &Lang, e: &E) -> String {
    match e {
        E::In(i) => l.ins[*i].to_string(),
        E::Var(i) => format!("t{i}"),
        E::Const(c) => format!("{}({}, {}, {}, {})", l.vec, num(c[0]), num(c[1]), num(c[2]), num(c[3])),
        E::Swz(a, s) => format!("({}).{}", pe(l, a), s.iter().map(|&k| b"xyzw"[k as usize] as char).collect::<String>()),
        E::Bin(op, a, b) => match op {
            0 => format!("({} + {})", pe(l, a), pe(l, b)),
            1 => format!("({} - {})", pe(l, a), pe(l, b)),
            2 => format!("({} * {})", pe(l, a), pe(l, b)),
            3 => format!("min({}, {})", pe(l, a), pe(l, b)),
            _ => format!("max({}, {})", pe(l, a), pe(l, b)),
        },
        E::Un(op, a) => match op {
            0 => format!("(-{})", pe(l, a)),
            1 => format!("abs({})", pe(l, a)),
            2 => format!("floor({} * 0.5)", pe(l, a)),
            _ => format!("clamp({}, -64.0, 64.0)", pe(l, a)),
        },
        E::Sel(a, b, c, d) => format!("((({}).x < ({}).x) ? {} : {})", pe(l, a), pe(l, b), pe(l, c), pe(l, d)),
        E::Arr(i) => format!("arr[{}]", idx(l, i)),
        E::Dot(a, b) => {
            if l.glsl {
                format!("vec4(dot({}, {}))", pe(l, a), pe(l, b))
            } else {
                format!("dot({}, {}).xxxx", pe(l, a), pe(l, b))
            }
        }
    }
}

fn idx(l: &Lang, e: &E) -> String {
    if l.glsl {
        format!("int(clamp(({}).x, 0.0, 3.0))", pe(l, e))
    } else {
        format!("(int)clamp(({}).x, 0.0, 3.0)", pe(l, e))
    }
}

fn ps(l: &Lang, s: &[S], ind: usize, out: &mut String) {
    let pad = "  ".repeat(ind);
    for st in s {
        match st {
            S::Mask(v, m, e) => {
                let k: String = (0..4).filter(|&i| m[i]).map(|i| b"xyzw"[i] as char).collect();
                out.push_str(&format!("{pad}t{v}.{k} = ({}).{k};\n", pe(l, e)));
            }
            S::ArrSet(i, e) => out.push_str(&format!("{pad}arr[{}] = {};\n", idx(l, i), pe(l, e))),
            S::Dyn(n, b) => {
                out.push_str(&format!("{pad}for (int i{ind} = 0; i{ind} < {}; i{ind}++) {{\n", idx(l, n)));
                ps(l, b, ind + 1, out);
                out.push_str(&format!("{pad}}}\n"));
            }
            S::Set(v, e) => out.push_str(&format!("{pad}t{v} = {};\n", pe(l, e))),
            S::Add(v, e) => out.push_str(&format!("{pad}t{v} += {};\n", pe(l, e))),
            S::If(a, b, t, f) => {
                out.push_str(&format!("{pad}if (({}).x < ({}).x) {{\n", pe(l, a), pe(l, b)));
                ps(l, t, ind + 1, out);
                out.push_str(&format!("{pad}}} else {{\n"));
                ps(l, f, ind + 1, out);
                out.push_str(&format!("{pad}}}\n"));
            }
            S::For(n, b) => {
                out.push_str(&format!("{pad}for (int i{ind} = 0; i{ind} < {n}; i{ind}++) {{\n"));
                ps(l, b, ind + 1, out);
                out.push_str(&format!("{pad}}}\n"));
            }
        }
    }
}

fn body(l: &Lang, prog: &[S]) -> String {
    let mut s = String::new();
    for v in 0..VARS {
        s.push_str(&format!("  {} t{v} = {}(0.0, 0.0, 0.0, 0.0);\n", l.vec, l.vec));
    }
    s.push_str(&format!("  {} arr[4];\n", l.vec));
    for i in 0..4 {
        s.push_str(&format!("  arr[{i}] = {}({}.0, 1.0, 2.0, 3.0);\n", l.vec, i));
    }
    ps(l, prog, 1, &mut s);
    s
}

/// `n` programs of `count` statements nested `depth` deep, named `prefix`NNN.
pub fn cases(out: &mut Vec<Case>, errors: &mut Vec<String>, n: u32, seed: u64, depth: u32, count: u32, prefix: &str) {
    let mut r = Rng(seed | 1);
    let mut made = 0;
    while made < n {
        // (every other program has loops counted by the inputs: shader model 3 has none of those,
        // they go to DXBC and GLSL only)
        let dynamic = made % 2 == 1;
        let prog = stmts(&mut r, depth, count, dynamic);
        let result = r.below(VARS as u64) as usize;
        // six pixels of inputs; programs whose values leave the exact range are drawn again
        let mut pixels = Vec::new();
        let mut ok = true;
        for _ in 0..6 {
            let ins: [V; 3] = std::array::from_fn(|_| [r.int(-20, 20), r.int(-20, 20), r.int(-20, 20), r.int(-20, 20)]);
            let mut ev = Ev { ins: &ins, vars: [[0.0; 4]; VARS], arr: std::array::from_fn(|i| [i as f64, 1.0, 2.0, 3.0]), ok: true };
            ev.s(&prog);
            ok &= ev.ok;
            pixels.push((ins, ev.vars[result]));
        }
        if !ok {
            continue;
        }
        made += 1;
        let want = |v: &V| -> [Want; 4] { v.map(|x| Some(vec![x as f32])) };
        let f32s = |ins: &[V; 3]| -> Vec<[f32; 4]> { ins.iter().map(|v| v.map(|x| x as f32)).collect() };
        let name = format!("{prefix}{made:03}");
        let hl = Lang { vec: "float4", ins: ["A", "B", "Cc"], glsl: false };
        let b = body(&hl, &prog);
        let src4 = format!("cbuffer C : register(b0) {{ float4 A; float4 B; float4 Cc; }};\nfloat4 ps() : SV_Target {{\n{b}  return t{result};\n}}\n");
        dxbc_raw(out, errors, &name, "random program (exact integer values)", &src4, "ps_4_0", Tol::Exact,
            pixels.iter().map(|(ins, v)| (f32s(ins).concat().iter().map(|x| x.to_bits()).collect(), want(v))).collect());
        if !dynamic {
        let src3 = format!("float4 A : register(c0); float4 B : register(c1); float4 Cc : register(c2);\nfloat4 ps() : COLOR {{\n{b}  return t{result};\n}}\n");
        let mut inc = |_: &str, _: bool| None;
        match hlslc::compile(&src3, &[], &mut inc, "ps", "ps_3_0", 0) {
            Ok(code) => {
                let words: Vec<u32> = code.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
                d3d9_words(out, errors, &format!("hlsl_{name}"), "random program through hlslc's shader model 3 (exact integer values)", &words, &src3, Tol::Exact,
                    pixels.iter().map(|(ins, v)| { let f = f32s(ins); ([f[0], f[1], f[2]], want(v)) }).collect());
            }
            Err(e) => errors.push(format!("d3d9/hlsl_{name}: {e}\n{src3}")),
        }
        }
        let gl = Lang { vec: "vec4", ins: ["a", "b", "c"], glsl: true };
        let gb = body(&gl, &prog);
        glsl_raw(out, errors, &name, "random program (exact integer values)", &format!("void main() {{\n{gb}  gl_FragColor = t{result};\n}}"), Tol::Exact,
            pixels.iter().map(|(ins, v)| {
                let f = f32s(ins);
                (vec![("a", f[0].iter().map(|x| x.to_bits()).collect()), ("b", f[1].iter().map(|x| x.to_bits()).collect()), ("c", f[2].iter().map(|x| x.to_bits()).collect())], want(v))
            }).collect());
    }
}
