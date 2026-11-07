//! koji's interpreter against Berkeley TestFloat 3e (http://www.jhauser.us/arithmetic/TestFloat.html):
//! testfloat_gen, built on SoftFloat 3e specialized for x86 SSE (8086-SSE: x86's default NaN,
//! NaN propagation and integer-overflow results), writes test cases (operands, correctly rounded
//! result, IEEE exception flags) for each operation and rounding mode, made to hit the corners:
//! ties, carries out of the significand, boundaries of the exponent range, subnormals,
//! infinities, NaNs, integer limits. Each case runs through the instruction (SSE scalar, packed,
//! VEX, FMA, conversions, ROUNDSS/SD, comparisons; x87 arithmetic at single and double
//! precision, loads/stores, FIST/FISTTP/FILD, FRNDINT, FPREM1, FUCOMI/FCOMI) with the rounding
//! mode in MXCSR or the x87 control word, and the result and MXCSR's exception flags must be
//! TestFloat's. The x87's own exception flags are not compared (koji keeps none); nor are
//! the x87's double rounding below the normal range and its two-NaN rule (tests/testfloat/ops.rs,
//! `skipped`).
//!
//! tests/testfloat/build.sh builds testfloat_gen into target/testfloat (pinned commits); without
//! it this test does nothing. tests/testfloat/expected.tsv: passing cases per instruction and
//! rounding mode; fewer fails, more prints a note (TF_UPDATE=1 rewrites). TF_FILTER=<substring
//! of the name>, TF_SHOW=<n> failures shown per run, TF_LEVEL=2 (TestFloat's level 2: much
//! longer), TF_GEN=<path to testfloat_gen>.
use iced_x86::code_asm::*;
use koji::{Cpu, Exit, Fault, Memory};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "../../../tests/testfloat/ops.rs"]
mod ops;
use ops::*;

const CODE: u32 = 0x1000;
const DATA: u32 = 0x10000;
const OUT: u32 = DATA + 0x100;

struct Ram(Vec<u8>);
impl Memory for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        match self.0.get_mut(a as usize) {
            Some(b) => {
                *b = v;
                Ok(())
            }
            None => Err(Fault { addr: a, write: true }),
        }
    }
}

/// The program: the instruction(s), then the flag read (STMXCSR [edi+8]); its end address
fn program(op: &Op, rc: u32) -> (Vec<u8>, u32) {
    let mut a = CodeAssembler::new(32).unwrap();
    body(op, rc, &mut a).unwrap();
    if op.x87.is_none() {
        a.stmxcsr(dword_ptr(edi + 8)).unwrap();
    }
    a.nop().unwrap();
    let code = a.assemble(CODE as u64).unwrap();
    let end = CODE + code.len() as u32 - 1;
    (code, end)
}

/// Runs one case; what differed
fn run(op: &Op, rc: u32, code: &[u8], end: u32, mem: &mut Ram, c: &Case) -> Option<String> {
    mem.0[CODE as usize..CODE as usize + code.len()].copy_from_slice(code);
    for (i, v) in c.args.iter().enumerate() {
        mem.0[DATA as usize + 8 * i..][..8].copy_from_slice(&v.to_le_bytes());
    }
    mem.0[OUT as usize..OUT as usize + 16].fill(0xcc);
    let mut cpu = Cpu { eip: CODE, ..Cpu::default() };
    cpu.regs[koji::EBX] = DATA;
    cpu.regs[koji::ESI] = DATA;
    cpu.regs[koji::EDI] = OUT;
    cpu.regs[koji::ESP] = 0x8000;
    cpu.mxcsr = 0x1f80 | rc << 13;
    if let Some(pc) = op.x87 {
        cpu.fpu.cw = x87_cw(pc, rc);
    }
    let mut n = 0;
    while cpu.eip != end {
        let e = koji::step(&mut cpu, mem);
        if e != Exit::Continue {
            return Some(format!("exit {e:?} at {:#x}", cpu.eip));
        }
        n += 1;
        if n > 10_000 {
            return Some("no end".into());
        }
    }
    let k = op.out.bytes();
    let mut got = [0u8; 8];
    got[..k].copy_from_slice(&mem.0[OUT as usize..OUT as usize + k]);
    let got = u64::from_le_bytes(got);
    let mut d = Vec::new();
    if got != c.want {
        d.push(format!("result {got:0w$x} want {:0w$x}", c.want, w = 2 * k));
    }
    if op.x87.is_none() {
        let mx = u32::from_le_bytes(mem.0[OUT as usize + 8..][..4].try_into().unwrap());
        let want = mxcsr_flags(c.flags);
        if mx & MX_CMP != want {
            d.push(format!("flags {:#x} want {want:#x}", mx & MX_CMP));
        }
    }
    (!d.is_empty()).then(|| d.join(", "))
}

fn gen_path() -> PathBuf {
    std::env::var_os("TF_GEN").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/testfloat/testfloat_gen"))
}

fn expected_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/testfloat/expected.tsv")
}

const RC_NAME: [&str; 4] = ["near", "down", "up", "zero"];

#[test]
fn testfloat() {
    let gen = gen_path();
    if !gen.exists() {
        eprintln!("testfloat: no {} (tests/testfloat/build.sh)", gen.display());
        return;
    }
    let filter = std::env::var("TF_FILTER").unwrap_or_default();
    let show: usize = std::env::var("TF_SHOW").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let level: u32 = std::env::var("TF_LEVEL").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let expected: BTreeMap<String, (usize, usize)> = std::fs::read_to_string(expected_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((format!("{}\t{}", f.next()?, f.next()?), (f.next()?.parse().ok()?, f.next()?.parse().ok()?)))
        })
        .collect();
    let all = ops();
    let jobs: Vec<(&Op, u32)> = all.iter().filter(|o| o.name.contains(&filter)).flat_map(|o| (0..if o.rounds { 4 } else { 1 }).map(move |rc| (o, rc))).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(BTreeMap::new());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut mem = Ram(vec![0; 0x20000]);
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(op, rc)) = jobs.get(i) else { break };
                    let (code, end) = program(op, rc);
                    let (mut ok, mut n, mut shown) = (0, 0, 0);
                    let mut log = String::new();
                    generate(&gen, op, rc, level, |l| {
                        let Some(c) = parse_line(op, l) else { return };
                        if skipped(op, &c) {
                            return;
                        }
                        n += 1;
                        match run(op, rc, &code, end, &mut mem, &c) {
                            None => ok += 1,
                            Some(d) if shown < show => {
                                shown += 1;
                                log += &format!("{} {}: {l}: {d}\n", op.name, RC_NAME[rc as usize]);
                            }
                            Some(_) => {}
                        }
                    });
                    eprint!("{log}");
                    results.lock().unwrap().insert(format!("{}\t{}", op.name, RC_NAME[rc as usize]), (ok, n));
                }
            });
        }
    });
    let now = results.into_inner().unwrap();
    let (mut pass, mut total, mut worse) = (0, 0, Vec::new());
    for (k, &(ok, n)) in &now {
        pass += ok;
        total += n;
        match expected.get(k) {
            Some(&(want, _)) if ok < want => worse.push(format!("{k}: {ok}/{n} pass, expected.tsv says {want}")),
            Some(&(want, _)) if ok > want => eprintln!("testfloat: {k} improved {want} -> {ok}/{n} (TF_UPDATE=1 records it)"),
            Some(_) => {}
            None if ok < n => worse.push(format!("{k}: {ok}/{n} pass, not in expected.tsv")),
            None => {}
        }
    }
    eprintln!("testfloat: {pass}/{total} cases pass in {} runs", now.len());
    if std::env::var_os("TF_UPDATE").is_some() && level == 1 {
        let mut all = expected.clone();
        all.extend(now);
        let mut s = String::from("# instruction\trounding\tpassing\tcases  (crates/koji/tests/testfloat.rs; TF_UPDATE=1 rewrites)\n");
        for (k, (a, b)) in all {
            s += &format!("{k}\t{a}\t{b}\n");
        }
        std::fs::write(expected_path(), s).unwrap();
        return;
    }
    assert!(worse.is_empty(), "fewer passing cases than expected:\n{}", worse.join("\n"));
}
