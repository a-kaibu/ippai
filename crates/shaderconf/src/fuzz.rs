//! Mutation fuzzing of the translators' front ends (shaderconf --fuzz N [seed]): the cases'
//! sources (HLSL, D3D9 assembly, GLSL) and their bytecode (DXBC, D3D9 tokens), mutated, go
//! through hlslc, dxwgsl and glwgsl. A panic, a hang (over 20 s) or WGSL that naga rejects
//! from an input a translator accepted is a failure; an error result is fine.

use crate::Case;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// (an input that makes a translator allocate over 1 GiB: reported with the input, then exit)
struct Guard;
static CURRENT: Mutex<String> = Mutex::new(String::new());
static GUARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
unsafe impl std::alloc::GlobalAlloc for Guard {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        if l.size() > 1 << 30 && GUARD.load(Ordering::Relaxed) {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "ALLOC of {} bytes on\n{}", l.size(), CURRENT.lock().map(|s| s.clone()).unwrap_or_default());
            std::process::exit(3);
        }
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, n: usize) -> *mut u8 {
        if n > 1 << 30 && GUARD.load(Ordering::Relaxed) {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "ALLOC of {n} bytes on\n{}", CURRENT.lock().map(|s| s.clone()).unwrap_or_default());
            std::process::exit(3);
        }
        unsafe { std::alloc::System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOC: Guard = Guard;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

const TOKENS: &[&str] = &[
    "(", ")", "[", "]", "{", "}", ";", ",", ".", ".x", ".xyzw", ".wzyx", "=", "+=", "*=", "/=", "%=", "<<", ">>", "&", "|", "^", "~", "!", "-", "+", "*", "/", "%", "?", ":",
    "==", "<", ">=", "&&", "||", "++", "--", "0", "1", "-1", "2147483647", "-2147483648", "4294967295", "65536", "1e38", "1e-45", "0.5", "1.#INF", "0x7fffffff",
    "float", "float4", "float4x4", "int", "int2", "uint", "bool", "half", "double", "min16float", "vec4", "mat4", "ivec3", "if", "else", "for", "while", "do", "break",
    "continue", "return", "discard", "switch", "case 0:", "default:", "struct S { float4 a; };", "static", "const", "uniform", "in", "out", "inout", "#define X", "#if 0",
    "#endif", "#include \"x\"", "\\\n", "/*", "*/", "//", "\n", " ", "x", "a", "r", "arr", "A", "B", "gl_FragColor", "gl_FragCoord", "main", "ps", "abs", "max", "lerp",
    "mul", "dot", "tex2D", "texture2D", "sampler2D", "Texture2D", "SamplerState", "register(c0)", "register(b0)", ": COLOR", ": SV_Target", "[unroll]", "[loop]",
    "mov", "add", "mad", "rcp", "texld", "if_lt", "endif", "loop", "endloop", "rep", "endrep", "def", "defi", "dcl_2d", "dcl_texcoord0", "r0", "c0", "i0", "b0", "aL", "v0",
    "oC0", "s0", "p0", "_sat", "_pp", "_x2",
];

const WORDS: &[u32] = &[0, 1, 2, 3, 4, 0xff, 0x100, 0xffff, 0x10000, 0x7fffffff, 0x80000000, 0xffffffff, 0xfffffffe, 0x3f800000, 0x7f800000, 0x7fc00000, 0xffff0000, 0x0000ffff, 0x80000001];

fn mutate_text(r: &mut Rng, s: &str, other: &str) -> String {
    let mut b: Vec<char> = s.chars().collect();
    for _ in 0..1 + r.below(4) {
        let at = r.below(b.len() + 1);
        match r.below(6) {
            0 if !b.is_empty() => {
                let n = 1 + r.below(16.min(b.len() - at.min(b.len())).max(1));
                let end = (at + n).min(b.len());
                b.drain(at.min(b.len())..end);
            }
            1 => {
                let t = TOKENS[r.below(TOKENS.len())];
                b.splice(at..at, t.chars());
            }
            2 if at < b.len() => {
                let t = TOKENS[r.below(TOKENS.len())];
                let end = (at + 1 + r.below(8)).min(b.len());
                b.splice(at..end, t.chars());
            }
            3 if !b.is_empty() => {
                // (a span duplicated)
                let from = r.below(b.len());
                let end = (from + 1 + r.below(40)).min(b.len());
                let span: Vec<char> = b[from..end].to_vec();
                b.splice(at..at, span);
            }
            4 => {
                // (a span of another source)
                let o: Vec<char> = other.chars().collect();
                if !o.is_empty() {
                    let from = r.below(o.len());
                    let end = (from + 1 + r.below(60)).min(o.len());
                    b.splice(at..at, o[from..end].iter().copied());
                }
            }
            _ if at < b.len() => {
                b[at] = (b' ' + r.below(95) as u8) as char;
            }
            _ => {}
        }
    }
    b.into_iter().collect()
}

fn mutate_words(r: &mut Rng, w: &[u32], keep: usize) -> Vec<u32> {
    let mut w = w.to_vec();
    for _ in 0..1 + r.below(4) {
        if w.len() <= keep {
            break;
        }
        let at = keep + r.below(w.len() - keep);
        match r.below(7) {
            0 => w[at] ^= 1 << r.below(32),
            1 => w[at] = WORDS[r.below(WORDS.len())],
            2 => w[at] = r.next() as u32,
            3 => w.truncate(at),
            4 => {
                w.remove(at);
            }
            5 => w.insert(at, WORDS[r.below(WORDS.len())]),
            _ => {
                // (a low field changed: register numbers, counts, opcodes)
                w[at] = (w[at] & !0xff) | r.below(256) as u32;
            }
        }
    }
    w
}

fn bytes(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn words(b: &[u8]) -> Vec<u32> {
    b.chunks(4).map(|c| {
        let mut a = [0u8; 4];
        a[..c.len()].copy_from_slice(c);
        u32::from_le_bytes(a)
    }).collect()
}

/// naga's verdict on translator output
fn valid(code: &str) -> Result<(), String> {
    let m = naga::front::wgsl::parse_str(code).map_err(|e| e.emit_to_string(code))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m).map(|_| ()).map_err(|e| format!("{e:?}"))
}

enum Input {
    Hlsl(String, &'static str),
    Asm(String),
    Dxbc(Vec<u8>),
    D3d9(Vec<u32>),
    Glsl(String),
}

impl Input {
    fn show(&self) -> String {
        match self {
            Input::Hlsl(s, t) => format!("HLSL ({t}):\n{s}"),
            Input::Asm(s) => format!("D3D9 assembly:\n{s}"),
            Input::Glsl(s) => format!("GLSL:\n{s}"),
            Input::Dxbc(b) => format!("DXBC: {}", words(b).iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" ")),
            Input::D3d9(w) => format!("D3D9 tokens: {}", w.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" ")),
        }
    }

    /// Ok(None): rejected (fine); Ok(Some(wgsl)): accepted
    fn run(&self) -> Result<Option<Vec<String>>, String> {
        let mut inc = |_: &str, _: bool| None;
        Ok(match self {
            Input::Hlsl(s, t) => match hlslc::compile(s, &[], &mut inc, "ps", t, 0) {
                Err(_) => None,
                Ok(code) if t.starts_with("ps_4") || t.starts_with("ps_5") => match dxwgsl::translate(&code, true, None) {
                    Ok(w) => Some(vec![w.code]),
                    Err(e) => return Err(format!("hlslc output dxwgsl rejects: {e}")),
                },
                Ok(code) => Some(d3d9_wgsl(&words(&code)).ok_or("hlslc output dxwgsl cannot parse")?),
            },
            Input::Asm(s) => match hlslc::d3d9asm::assemble(s) {
                Err(_) => None,
                Ok(w) => d3d9_wgsl(&w),
            },
            Input::Dxbc(b) => dxwgsl::translate(b, true, None).ok().map(|w| vec![w.code]),
            Input::D3d9(w) => d3d9_wgsl(w),
            Input::Glsl(s) => glwgsl::link(crate::cases::GL_VS, s, &Default::default()).ok().map(|p| vec![p.vs.wgsl, p.ps.wgsl]),
        })
    }
}

fn d3d9_wgsl(w: &[u32]) -> Option<Vec<String>> {
    let s = dxwgsl::d3d9::Shader::parse(w)?;
    let o = dxwgsl::d3d9::Options { link: 0, vs_inputs: Vec::new(), alpha_test: None, fog: false };
    let g = dxwgsl::d3d9::Gen::new(&s);
    Some(vec![if s.pixel { g.pixel(&o) } else { g.vertex(&o) }])
}

pub fn run(cases: &[Case], n: usize, seed: u64) -> bool {
    let mut seeds: Vec<Input> = Vec::new();
    let mut inc = |_: &str, _: bool| None;
    for c in cases {
        match c.api {
            "dxbc" => {
                seeds.push(Input::Hlsl(c.src.clone(), "ps_4_0"));
                if let Ok(b) = hlslc::compile(&c.src, &[], &mut inc, "ps", "ps_4_0", 0) {
                    seeds.push(Input::Dxbc(b));
                }
            }
            "d3d9" if c.src.trim_start().starts_with("ps_") => {
                seeds.push(Input::Asm(c.src.clone()));
                if let Ok(w) = hlslc::d3d9asm::assemble(&c.src) {
                    seeds.push(Input::D3d9(w));
                }
            }
            "d3d9" => {
                seeds.push(Input::Hlsl(c.src.clone(), "ps_3_0"));
                if let Ok(b) = hlslc::compile(&c.src, &[], &mut inc, "ps", "ps_3_0", 0) {
                    seeds.push(Input::D3d9(words(&b)));
                }
            }
            _ => seeds.push(Input::Glsl(c.src.clone())),
        }
    }
    // (a panic is caught and reported, not printed by the default hook)
    let msg = Arc::new(Mutex::new(String::new()));
    let m2 = msg.clone();
    panic::set_hook(Box::new(move |i| {
        *m2.lock().unwrap() = format!("{i}");
    }));
    // (a hang: the input it is on, then exit)
    let started = Arc::new(AtomicU64::new(0));
    let current = Arc::new(Mutex::new(String::new()));
    GUARD.store(true, Ordering::Relaxed);
    {
        let (st, cur) = (started.clone(), current.clone());
        let t0 = Instant::now();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let s = st.load(Ordering::Relaxed);
            if s != 0 && (t0.elapsed().as_millis() as u64).saturating_sub(s) > 20_000 {
                eprintln!("HANG (over 20 s) on\n{}", cur.lock().unwrap());
                std::process::exit(2);
            }
        });
        let mut r = Rng(seed | 1);
        let mut bad = 0usize;
        let (mut accepted, mut seen) = (0usize, std::collections::HashSet::new());
        for k in 0..n {
            let i = r.below(seeds.len());
            let o = r.below(seeds.len());
            let other = match &seeds[o] {
                Input::Hlsl(s, _) | Input::Asm(s) | Input::Glsl(s) => s.as_str(),
                _ => "",
            };
            let input = match &seeds[i] {
                Input::Hlsl(s, t) => Input::Hlsl(mutate_text(&mut r, s, other), t),
                Input::Asm(s) => Input::Asm(mutate_text(&mut r, s, other)),
                Input::Glsl(s) => Input::Glsl(mutate_text(&mut r, s, other)),
                // (the DXBC container header, the D3D9 version token kept most of the time)
                Input::Dxbc(b) => {
                    let keep = if r.below(8) == 0 { 0 } else { 8 };
                    Input::Dxbc(bytes(&mutate_words(&mut r, &words(b), keep)))
                }
                Input::D3d9(w) => {
                    let keep = if r.below(8) == 0 { 0 } else { 1 };
                    Input::D3d9(mutate_words(&mut r, w, keep))
                }
            };
            *current.lock().unwrap() = input.show();
            *CURRENT.lock().unwrap() = input.show();
            started.store(t0.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
            let res = panic::catch_unwind(AssertUnwindSafe(|| input.run()));
            started.store(0, Ordering::Relaxed);
            let fail = match res {
                Err(_) => Some(format!("panic: {}", msg.lock().unwrap())),
                Ok(Err(e)) => Some(e),
                Ok(Ok(None)) => None,
                Ok(Ok(Some(ws))) => {
                    accepted += 1;
                    ws.iter().find_map(|w| valid(w).err().map(|e| format!("invalid WGSL: {e}\n{w}")))
                }
            };
            if let Some(f) = fail {
                // (one report per distinct message)
                let key: String = f.lines().next().unwrap_or("").chars().filter(|c| !c.is_ascii_digit()).collect();
                if seen.insert(key) {
                    println!("FAIL #{k}: {f}\n---- input\n{}\n", input.show());
                }
                bad += 1;
            }
        }
        let _ = panic::take_hook();
        println!("fuzz: {n} inputs from {} seeds, {accepted} accepted, {bad} failed ({} distinct)", seeds.len(), seen.len());
        bad == 0
    }
}
