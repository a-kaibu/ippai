//! The JIT against Berkeley TestFloat (tests/testfloat/build.sh, the cases of
//! crates/koji/tests/testfloat.rs): each instruction of tests/testfloat/ops.rs in a loop over a
//! batch of TestFloat cases, translated (random region/function modes) and run by the
//! interpreter; run_case compares the two, and the JIT's results and MXCSR flags must be
//! TestFloat's. A sample of each instruction's cases per rounding mode (TF_N, default 2000;
//! 0 = all), TF_FILTER=<substring of the name>, TF_GEN=<testfloat_gen>.
//!
//!   cargo run --release -- testfloat
use super::*;
#[path = "../../../tests/testfloat/ops.rs"]
mod ops;
use ops::*;

/// cases per program: operands at DATA + 24k, results at DATA + OUTS + 16k
const BATCH: usize = 40;
const OUTS: i32 = 0x400;
const MODE: i32 = 0xfe0;

fn program(op: &Op, rc: u32) -> Result<Vec<Instruction>, IcedError> {
    let mut a = CodeAssembler::new(32)?;
    let mut top = a.create_label();
    a.mov(esi, ebx)?;
    a.lea(edi, ptr(ebx + OUTS))?;
    a.mov(dword_ptr(ebx + COUNTER as i32), BATCH as u32)?;
    if op.x87.is_some() {
        a.fldcw(word_ptr(ebx + MODE))?;
    }
    a.set_label(&mut top)?;
    if op.x87.is_none() {
        // (each case from clear flags)
        a.ldmxcsr(dword_ptr(ebx + MODE))?;
    }
    body(op, rc, &mut a)?;
    if op.x87.is_none() {
        a.stmxcsr(dword_ptr(edi + 8))?;
    }
    a.add(esi, 24)?;
    a.add(edi, 16)?;
    a.dec(dword_ptr(ebx + COUNTER as i32))?;
    a.jne(top)?;
    a.jmp(END as u64)?;
    Ok(a.take_instructions())
}

pub fn run(engine: &Engine, dispatch: &Module, memory: &SharedMemory, base: usize) -> usize {
    let gen = std::env::var_os("TF_GEN").map(std::path::PathBuf::from).unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/testfloat/testfloat_gen").into());
    if !gen.exists() {
        println!("testfloat: no {} (tests/testfloat/build.sh)", gen.display());
        return 1;
    }
    let want_n: usize = std::env::var("TF_N").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let filter = std::env::var("TF_FILTER").unwrap_or_default();
    let mut rng = Rng(0x7e57_f10a7 | 1);
    let (mut fails, mut cases, mut runs) = (0, 0, 0);
    for op in ops().iter().filter(|o| o.name.contains(&filter)) {
        for rc in 0..if op.rounds { 4 } else { 1 } {
            let list = program(op, rc).unwrap();
            let mut all = Vec::new();
            generate(&gen, op, rc, 1, |l| {
                if let Some(c) = parse_line(op, l) {
                    if !skipped(op, &c) {
                        all.push(c);
                    }
                }
            });
            // (a sample spread over the whole list: TestFloat's cases come in groups)
            let stride = if want_n == 0 { 1 } else { (all.len() / want_n).max(1) };
            let sample: Vec<&Case> = all.iter().step_by(stride).collect();
            let mut shown = 0;
            for batch in sample.chunks(BATCH) {
                let mut d = random_data(&mut rng);
                for k in 0..BATCH {
                    // (a short last batch repeats its first case)
                    let c = batch.get(k).unwrap_or(&batch[0]);
                    for (i, v) in c.args.iter().enumerate() {
                        d[24 * k + 8 * i..][..8].copy_from_slice(&v.to_le_bytes());
                    }
                }
                let mode_word = match op.x87 {
                    Some(pc) => x87_cw(pc, rc) as u32,
                    None => 0x1f80 | rc << 13,
                };
                d[MODE as usize..MODE as usize + 4].copy_from_slice(&mode_word.to_le_bytes());
                let mut cpu0 = initial(&mut rng);
                cpu0.mxcsr = 0x1f80 | rc << 13;
                cpu0.fpu = koji::Fpu::default();
                if let Some(pc) = op.x87 {
                    cpu0.fpu.cw = x87_cw(pc, rc);
                }
                let mode = Mode { fns: rng.below(2) == 0, blocks: rng.pick(&[1, 2, koji::jit::MAX_BLOCKS]), hoist: rng.below(2) == 0, slots: 0, pre: rng.below(4) == 0, exact: true, fresh: false, stale: 0 };
                runs += 1;
                let (mut diffs, _) = run_case(engine, dispatch, memory, base, &list, &d, &cpu0, mode);
                // the JIT's results (left in memory by run_case) against TestFloat's
                let m = unsafe { std::slice::from_raw_parts((base + DATA as usize) as *const u8, DATA_LEN as usize) };
                for (k, c) in batch.iter().enumerate() {
                    cases += 1;
                    let o = OUTS as usize + 16 * k;
                    let n = op.out.bytes();
                    let mut got = [0u8; 8];
                    got[..n].copy_from_slice(&m[o..o + n]);
                    let got = u64::from_le_bytes(got);
                    if got != c.want {
                        diffs.push(format!("case {k} {:x?}: result {got:x} want {:x}", &c.args[..op.args.len()], c.want));
                    }
                    if op.x87.is_none() {
                        let mx = u32::from_le_bytes(m[o + 8..o + 12].try_into().unwrap()) & MX_CMP;
                        if mx != mxcsr_flags(c.flags) {
                            diffs.push(format!("case {k} {:x?}: flags {mx:#x} want {:#x}", &c.args[..op.args.len()], mxcsr_flags(c.flags)));
                        }
                    }
                }
                if !diffs.is_empty() {
                    fails += 1;
                    if shown < 3 {
                        shown += 1;
                        println!("testfloat {} rc {rc} {mode:?}: {}", op.name, diffs.iter().take(4).cloned().collect::<Vec<_>>().join("; "));
                    }
                }
            }
        }
    }
    println!("testfloat: {runs} programs, {cases} cases, {fails} programs failed");
    fails
}
