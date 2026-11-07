//! `native [count] [seed]`: koji's interpreter against this machine's CPU, whole random
//! programs (the generator of the JIT test: integer, SSE, and MMX or x87) run by both from the
//! same state. The CPU side is a freestanding 32-bit Linux process (native32.S, built with
//! gcc -m32) with the same memory layout. The x87 runs with double or single precision (any
//! rounding mode, MXCSR's too) on both sides: koji's registers are f64. The JIT test checks the JIT against the interpreter,
//! so through this the JIT is checked against the CPU too.

use super::*;
use std::io::Write as _;
use std::process::{Command, Stdio};

/// Builds the runner; None if this machine can't (no gcc -m32, no 32-bit execution).
fn build() -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("jit32test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("native32.S");
    let exe = dir.join("native32");
    std::fs::write(&src, include_str!("native32.S")).ok()?;
    let ok = Command::new("gcc").args(["-m32", "-nostdlib", "-static", "-o"]).arg(&exe).arg(&src).status().ok()?.success();
    ok.then_some(exe)
}

fn f64_to_f80(v: f64) -> [u8; 10] {
    let b = v.to_bits();
    let sign = (b >> 63) as u16;
    let exp = ((b >> 52) & 0x7ff) as i32;
    let man = b & ((1 << 52) - 1);
    let (e, m): (u16, u64) = if exp == 0 && man == 0 {
        (0, 0)
    } else if exp == 0x7ff {
        (0x7fff, 1 << 63 | man << 11)
    } else if exp == 0 {
        // a denormal: man * 2^-1074, normalized (the integer bit at 63)
        let lz = man.leading_zeros() as i32;
        ((63 - 1074 - lz + 16383) as u16, man << lz)
    } else {
        ((exp - 1023 + 16383) as u16, 1 << 63 | man << 11)
    };
    let mut out = [0u8; 10];
    out[..8].copy_from_slice(&m.to_le_bytes());
    out[8..].copy_from_slice(&(e | sign << 15).to_le_bytes());
    out
}

#[test]
fn f80_round_trip() {
    let mut r = Rng(7);
    for _ in 0..100_000 {
        let v = f64::from_bits(r.next());
        let w = f80_to_f64(&f64_to_f80(v)).unwrap();
        assert!(v.to_bits() == w.to_bits() || (v.is_nan() && w.is_nan()), "{v:e}");
    }
    for v in [0.0, -0.0, f64::MIN_POSITIVE, 5e-324, -5e-324, 1e-310, f64::MAX, f64::INFINITY, -1.5] {
        assert_eq!(f80_to_f64(&f64_to_f80(v)).unwrap().to_bits(), v.to_bits());
    }
}

/// None: not exactly a double (extra precision or out of range)
fn f80_to_f64(b: &[u8]) -> Option<f64> {
    let m = u64::from_le_bytes(b[..8].try_into().unwrap());
    let se = u16::from_le_bytes(b[8..10].try_into().unwrap());
    let sign = (se >> 15) as u64;
    let e = (se & 0x7fff) as i32;
    if e == 0 && m == 0 {
        return Some(f64::from_bits(sign << 63));
    }
    if e == 0x7fff {
        return Some(f64::from_bits(sign << 63 | 0x7ff << 52 | (m >> 11) & ((1 << 52) - 1)));
    }
    let x = e - 16383;
    if (-1022..=1023).contains(&x) && m >> 63 == 1 && m & 0x7ff == 0 {
        return Some(f64::from_bits(sign << 63 | ((x + 1023) as u64) << 52 | (m >> 11) & ((1 << 52) - 1)));
    }
    // a double's denormal range
    if (-1074..-1022).contains(&x) {
        let shift = (-1022 - x) as u32 + 11;
        if m & ((1u64 << shift) - 1) == 0 {
            return Some(f64::from_bits(sign << 63 | m >> shift));
        }
    }
    None
}

fn fx_image(c: &Cpu, mmx: bool) -> [u8; 512] {
    let mut fx = [0u8; 512];
    let f = &c.fpu;
    fx[0..2].copy_from_slice(&f.cw.to_le_bytes());
    fx[2..4].copy_from_slice(&((f.sw & !0x3800) | ((f.top as u16 & 7) << 11)).to_le_bytes());
    fx[24..28].copy_from_slice(&c.mxcsr.to_le_bytes());
    if mmx {
        // MMn is physical register n: all valid, TOP 0
        fx[4] = 0xff;
        for i in 0..8 {
            fx[32 + 16 * i..40 + 16 * i].copy_from_slice(&c.mm[i].to_le_bytes());
            fx[40 + 16 * i..42 + 16 * i].copy_from_slice(&0xffffu16.to_le_bytes());
        }
    } else {
        fx[4] = !f.empty;
        for i in 0..8 {
            let p = (f.top as usize + i) & 7;
            if f.empty & (1 << p) == 0 {
                fx[32 + 16 * i..42 + 16 * i].copy_from_slice(&f64_to_f80(f.regs[p]));
            }
        }
    }
    for i in 0..8 {
        fx[160 + 16 * i..176 + 16 * i].copy_from_slice(&c.xmm[i].to_le_bytes());
    }
    fx
}

/// Runs on the CPU: the state after (as a Cpu: registers, flags, x87 or MMX, SSE), DATA, stack.
fn run_native(exe: &std::path::Path, code: &[u8], data: &[u8], c: &Cpu, mmx: bool) -> Result<(Cpu, Vec<u8>, Vec<u8>), String> {
    let mut input = Vec::new();
    for r in c.regs {
        input.extend_from_slice(&r.to_le_bytes());
    }
    input.extend_from_slice(&c.eflags.to_le_bytes());
    input.extend_from_slice(&(code.len() as u32).to_le_bytes());
    input.extend_from_slice(&fx_image(c, mmx));
    input.extend_from_slice(code);
    input.extend_from_slice(data);
    let mut child = Command::new("timeout").arg("10").arg(exe).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("{e}"))?;
    child.stdin.take().unwrap().write_all(&input).map_err(|e| format!("{e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("{e}"))?;
    if !out.status.success() || out.stdout.len() != 36 + 512 + 0x2000 {
        return Err(format!("native: {:?}", out.status));
    }
    let o = &out.stdout;
    let u32at = |i: usize| u32::from_le_bytes(o[i..i + 4].try_into().unwrap());
    let mut r = c.clone();
    for i in 0..8 {
        r.regs[i] = u32at(4 * i);
    }
    r.eflags = u32at(32);
    r.lazy = Default::default();
    r.eip = END;
    let fx = &o[36..36 + 512];
    let sw = u16::from_le_bytes(fx[2..4].try_into().unwrap());
    r.mxcsr = u32::from_le_bytes(fx[24..28].try_into().unwrap());
    for i in 0..8 {
        r.xmm[i] = u128::from_le_bytes(fx[160 + 16 * i..176 + 16 * i].try_into().unwrap());
    }
    if mmx {
        for i in 0..8 {
            r.mm[i] = u64::from_le_bytes(fx[32 + 16 * i..40 + 16 * i].try_into().unwrap());
        }
    } else {
        r.fpu.top = ((sw >> 11) & 7) as u32;
        r.fpu.sw = sw & !0x3800;
        r.fpu.empty = !fx[4];
        for i in 0..8 {
            let p = (r.fpu.top as usize + i) & 7;
            if r.fpu.empty & (1 << p) == 0 {
                r.fpu.regs[p] = f80_to_f64(&fx[32 + 16 * i..42 + 16 * i]).ok_or_else(|| format!("st{i} not a double: {:02x?}", &fx[32 + 16 * i..42 + 16 * i]))?;
            }
        }
    }
    Ok((r, o[36 + 512..36 + 512 + 0x1000].to_vec(), o[36 + 512 + 0x1000..].to_vec()))
}

/// What is compared: registers, flags, SSE, and MMX or the x87
fn compared(c: &Cpu, mmx: bool) -> Vec<(String, String)> {
    let mut c = c.clone();
    c.materialize();
    let mut v = Vec::new();
    for i in 0..8 {
        v.push((format!("r{i}"), format!("{:#x}", c.regs[i])));
        v.push((format!("xmm{i}"), format!("{:032x}", c.xmm[i])));
    }
    v.push(("flags".into(), format!("{:#x}", c.eflags & 0x8d5)));
    // (koji keeps no exception flags: MXCSR's are left out, FNSTSW's masked by the generator)
    v.push(("mxcsr".into(), format!("{:#x}", c.mxcsr & !0x3f)));
    if mmx {
        for i in 0..8 {
            v.push((format!("mm{i}"), format!("{:016x}", c.mm[i])));
        }
    } else {
        let f = &c.fpu;
        // (C1 left out: the CPU sets it when it rounded up, koji doesn't)
        v.push(("fpu".into(), format!("top {} empty {:#x} c {:#x}", f.top, f.empty, f.sw & 0x4500)));
        for p in 0..8 {
            if f.empty & 1 << p == 0 {
                v.push((format!("st r{p}"), format!("{:016x}", f.regs[p].to_bits())));
            }
        }
    }
    v
}

fn case(exe: &std::path::Path, mem: &mut [u8], list: &[Instruction], data: &[u8], cpu0: &Cpu, mmx: bool) -> (Vec<String>, Vec<u8>) {
    let Some(code) = encode(list, CODE as u64) else { return (Vec::new(), Vec::new()) };
    let base = mem.as_mut_ptr() as usize;
    mem[0x10000..MEM].fill(0);
    mem[CODE as usize..CODE as usize + code.len()].copy_from_slice(&code);
    mem[DATA as usize..DATA as usize + data.len()].copy_from_slice(data);
    unsafe { put_cpu(mem.as_mut_ptr(), cpu0) };
    let want = interp(base);
    let got = run_native(exe, &code, data, cpu0, mmx);
    let mut d = Vec::new();
    match (want, got) {
        (Err(_), Err(_)) => {}
        (Err(e), Ok(_)) => d.push(format!("interpreter: {e}, CPU ran to the end")),
        (Ok(()), Err(e)) => d.push(format!("CPU: {e}, interpreter ran to the end")),
        (Ok(()), Ok((n, ndata, nstack))) => {
            let k = unsafe { get_cpu(mem.as_mut_ptr()) };
            let (w, g): (std::collections::BTreeMap<_, _>, std::collections::BTreeMap<_, _>) = (compared(&k, mmx).into_iter().collect(), compared(&n, mmx).into_iter().collect());
            for key in w.keys().chain(g.keys().filter(|x| !w.contains_key(*x))) {
                if w.get(key) != g.get(key) {
                    d.push(format!("{key}: interpreter {:?} CPU {:?}", w.get(key), g.get(key)));
                }
            }
            let kd = &mem[DATA as usize..(DATA + DATA_LEN) as usize];
            if kd != ndata.as_slice() {
                let i = kd.iter().zip(&ndata).position(|(a, b)| a != b).unwrap();
                d.push(format!("data differs at +{i:#x}: interpreter {:02x} CPU {:02x}", kd[i], ndata[i]));
            }
            if mem[STACK as usize - 0x1000..STACK as usize] != nstack[..] {
                d.push("stack differs".into());
            }
        }
    }
    (d, code)
}

pub fn run(args: &[String]) {
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
    let seed: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    let Some(exe) = build() else {
        println!("native: can't build or run the 32-bit runner here (gcc -m32, IA-32 execution)");
        std::process::exit(2);
    };
    let mut mem = vec![0u8; MEM];
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 2);
    let mut fails = 0;
    NATIVE.store(true, Relaxed);
    for t in 0..count {
        let mmx = t % 2 == 0;
        NO_X87.store(mmx, Relaxed);
        NO_MMX.store(!mmx, Relaxed);
        let items = |rng: &mut Rng, n: usize| {
            let mut v: Vec<Vec<Instruction>> = Vec::new();
            while v.iter().map(|i| i.len()).sum::<usize>() < n {
                let i = item(rng);
                if !i.is_empty() {
                    v.push(i);
                }
            }
            v
        };
        let n = 5 + rng.below(30) as usize;
        let body = items(&mut rng, n);
        let mut subs = Vec::new();
        for _ in 0..rng.below(3) {
            let k = 1 + rng.below(8) as usize;
            subs.push(items(&mut rng, k));
        }
        let list = structure(&mut rng, body, subs);
        let data = random_data(&mut rng);
        let mut cpu0 = initial(&mut rng);
        // any rounding mode; x87 precision double or single (koji's registers are f64: not
        // extended)
        cpu0.fpu.cw = if rng.below(4) == 0 { 0x007f } else { 0x027f } | (rng.below(4) as u16) << 10;
        cpu0.mxcsr = 0x1f80 | (rng.below(4) as u32) << 13;
        if std::env::var("JONLY").is_ok_and(|o| o != t.to_string()) {
            continue;
        }
        let (diffs, _) = case(&exe, &mut mem, &list, &data, &cpu0, mmx);
        if diffs.is_empty() {
            continue;
        }
        let list = shrink(list, &diffs, |l| case(&exe, &mut mem, l, &data, &cpu0, mmx).0);
        let (diffs, code) = case(&exe, &mut mem, &list, &data, &cpu0, mmx);
        fails += 1;
        if fails <= 10 || std::env::var("JALL").is_ok() {
            println!("native test {t} ({}): {}", if mmx { "mmx" } else { "x87" }, diffs.join("; "));
            println!("    init: {:x?} flags {:#x} cw {:#x} mxcsr {:#x}", cpu0.regs, cpu0.eflags, cpu0.fpu.cw, cpu0.mxcsr);
            let mut dec = iced_x86::Decoder::with_ip(32, &code, CODE as u64, iced_x86::DecoderOptions::NONE);
            let mut f = iced_x86::FastFormatter::new();
            let mut s = String::new();
            for ins in &mut dec {
                s.clear();
                f.format(&ins, &mut s);
                // (DATA there, by the initial registers)
                let mut at = String::new();
                if ins.op_kinds().any(|k| k == iced_x86::OpKind::Memory) {
                    let reg = |r: iced_x86::Register| if r == iced_x86::Register::None { 0 } else { cpu0.regs[r.number() & 7] };
                    let a = reg(ins.memory_base()).wrapping_add(reg(ins.memory_index()).wrapping_mul(ins.memory_index_scale())).wrapping_add(ins.memory_displacement32());
                    if let Some(o) = a.checked_sub(DATA as u32).filter(|&o| o + 8 <= DATA_LEN) {
                        let q = u64::from_le_bytes(data[o as usize..o as usize + 8].try_into().unwrap());
                        at = format!("  ; [{:#x}] = {q:#018x} ({:e} / {:e})", o, f64::from_bits(q), f32::from_bits(q as u32));
                    }
                }
                println!("    {:#x} {s}{at}", ins.ip());
            }
        }
    }
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    println!("native: {count} tests, {fails} failed");
    if fails > 0 {
        std::process::exit(1);
    }
}
