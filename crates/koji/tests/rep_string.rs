//! REP string instructions (review b01): in 16-bit code an address-size prefix (ECX/ESI/EDI)
//! still writes at ES:EDI, the bulk copy included; and a REP longer than koji::REP_STEP_MAX
//! runs over several steps, EIP at the instruction until it is done, with the same result as
//! a model that runs it element by element.

use koji::{step, Cpu, Exit, Fault, Memory, REP_STEP_MAX};
use proptest::prelude::*;

/// Flat memory; `bulk` offers host ranges (as kura's does) so REP MOVS/STOS may go in one copy.
struct Flat {
    m: Vec<u8>,
    bulk: bool,
}

impl Memory for Flat {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.m.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        match self.m.get_mut(a as usize) {
            Some(b) => {
                *b = v;
                Ok(())
            }
            None => Err(Fault { addr: a, write: true }),
        }
    }
    fn host_range(&mut self, a: u32, len: u32) -> Option<*mut u8> {
        let end = (a as usize).checked_add(len as usize)?;
        (self.bulk && end <= self.m.len()).then(|| unsafe { self.m.as_mut_ptr().add(a as usize) })
    }
}

const CODE: u32 = 0x100;

/// Steps until EIP leaves the instruction at CODE: (steps, last exit).
fn run(cpu: &mut Cpu, m: &mut Flat) -> (u64, Exit) {
    for n in 1..=1 << 20 {
        let e = step(cpu, m);
        if e != Exit::Continue || cpu.eip != CODE {
            return (n, e);
        }
    }
    panic!("the instruction never ended");
}

/// 16-bit real mode, ES base 0x20000: `a32 rep stosb` and `a32 rep movsb` (67 f3 aa / 67 f3 a4)
/// write at ES:EDI, never at linear EDI, whatever the count and the memory's host ranges.
#[test]
fn a32_rep_in_16_bit_code_writes_at_es() {
    for (bulk, op) in [(false, 0xaa), (true, 0xaa), (false, 0xa4), (true, 0xa4)] {
        let mut m = Flat { m: vec![0; 0x40000], bulk };
        m.m[CODE as usize..CODE as usize + 3].copy_from_slice(&[0x67, 0xf3, op]);
        for i in 0..64 {
            m.m[0x3000 + i] = 0x80 | i as u8; // DS:ESI, DS = 0x0300... (base 0x3000, ESI 0)
        }
        let mut cpu = Cpu { mode16: 1, ldt: koji::LDT_REAL, ..Cpu::default() };
        cpu.eip = CODE;
        cpu.eflags = 2;
        cpu.regs[0] = 0x5a; // AL
        cpu.regs[1] = 40; // ECX
        cpu.regs[6] = 0; // ESI
        cpu.regs[7] = 0x1000; // EDI
        for (k, seg) in [(0, 0x2000u16), (3, 0x0300)] {
            cpu.sreg[k] = seg;
            cpu.sbase[k] = (seg as u32) << 4;
        }
        let (_, e) = run(&mut cpu, &mut m);
        assert_eq!(e, Exit::Continue);
        let want: Vec<u8> = (0..40).map(|i| if op == 0xaa { 0x5a } else { 0x80 | i as u8 }).collect();
        assert_eq!(&m.m[0x21000..0x21028], &want[..], "bulk {bulk} op {op:#x}: at ES:EDI");
        assert!(m.m[0x1000..0x1100].iter().all(|&b| b == 0), "bulk {bulk} op {op:#x}: nothing at linear EDI");
        assert_eq!((cpu.regs[1], cpu.regs[7], cpu.eip), (0, 0x1028, CODE + 3));
    }
}

/// A `repne scasb` over more than REP_STEP_MAX bytes: each step leaves EIP at it with ECX and
/// EDI as far as it got, and the last one ends it.
#[test]
fn a_long_scan_takes_several_steps() {
    let n = 3 * REP_STEP_MAX as usize + 5;
    let mut m = Flat { m: vec![1; 0x1000 + n + 16], bulk: true };
    m.m[CODE as usize..CODE as usize + 2].copy_from_slice(&[0xf2, 0xae]);
    let mut cpu = Cpu { eip: CODE, eflags: 2, ..Cpu::default() };
    cpu.regs[1] = n as u32 + 100;
    cpu.regs[7] = 0x1000;
    m.m[0x1000 + n] = 0; // the byte it looks for (AL 0)
    for k in 1..=3u32 {
        assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
        assert_eq!(cpu.eip, CODE, "step {k}: still at it");
        assert_eq!(cpu.regs[7], 0x1000 + k * REP_STEP_MAX as u32);
        assert_eq!(cpu.regs[1], n as u32 + 100 - k * REP_STEP_MAX as u32);
    }
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    assert_eq!((cpu.eip, cpu.regs[7], cpu.regs[1]), (CODE + 2, 0x1000 + n as u32 + 1, 100 - 1));
    assert!(cpu.flags() & 0x40 != 0, "ZF: found");
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Movs,
    Stos,
    Lods,
    Cmps,
    Scas,
}

/// The model: one element at a time, as the manuals describe it (32-bit flat, no faults).
fn model(op: Op, size: u32, rep: u8, back: bool, regs: &mut [u32; 8], m: &mut [u8]) -> bool {
    let step = if back { size.wrapping_neg() } else { size };
    let rd = |m: &[u8], a: u32| (0..size).fold(0u64, |v, i| v | (m[(a + i) as usize] as u64) << (8 * i));
    let mut zf = false;
    while rep == 0 || regs[1] != 0 {
        let (si, di) = (regs[6], regs[7]);
        match op {
            Op::Movs => {
                let v = rd(m, si);
                for i in 0..size {
                    m[(di + i) as usize] = (v >> (8 * i)) as u8;
                }
            }
            Op::Stos => {
                for i in 0..size {
                    m[(di + i) as usize] = (regs[0] as u64 >> (8 * i)) as u8;
                }
            }
            Op::Lods => {
                let mask = if size == 4 { u32::MAX } else { (1 << (8 * size)) - 1 };
                regs[0] = regs[0] & !mask | rd(m, si) as u32;
            }
            Op::Cmps => zf = rd(m, si) == rd(m, di),
            Op::Scas => zf = rd(m, di) == regs[0] as u64 & if size == 4 { u32::MAX as u64 } else { (1 << (8 * size)) - 1 },
        }
        if matches!(op, Op::Movs | Op::Lods | Op::Cmps) {
            regs[6] = si.wrapping_add(step);
        }
        if !matches!(op, Op::Lods) {
            regs[7] = di.wrapping_add(step);
        }
        if rep == 0 {
            break;
        }
        regs[1] -= 1;
        if matches!(op, Op::Cmps | Op::Scas) && ((rep == 0xf3 && !zf) || (rep == 0xf2 && zf)) {
            break;
        }
    }
    zf
}

const MEM: usize = 0x80000;
const SRC: u32 = 0x10000;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    /// Any string instruction, REP or not, forward or back, any count up to past three times
    /// REP_STEP_MAX, overlapping or not, with host ranges or without: stepped until EIP leaves
    /// it, memory and registers are the model's.
    #[test]
    fn rep_strings_match_the_model(
        op in prop_oneof![Just(Op::Movs), Just(Op::Stos), Just(Op::Lods), Just(Op::Cmps), Just(Op::Scas)],
        size in prop_oneof![Just(1u32), Just(2), Just(4)],
        rep in prop_oneof![Just(0u8), Just(0xf3), Just(0xf2)],
        back: bool, bulk: bool,
        count in prop_oneof![0u32..40, (REP_STEP_MAX as u32 - 3)..(REP_STEP_MAX as u32 + 3), 0u32..(3 * REP_STEP_MAX as u32 + 7)],
        gap in -64i32..64,
        fill in 0u8..4, al: u32, seed: u64,
    ) {
        // (source and destination ranges inside memory, either way; `gap` overlaps them)
        let span = count.max(1) * size;
        prop_assume!(span <= 0x30000);
        let src = if back { SRC + span } else { SRC };
        let dst = (src as i64 + span as i64 + gap as i64 * size as i64 - if gap >= 0 { 0 } else { span as i64 }) as u32;
        let dst = dst.clamp(0x1000 + span, MEM as u32 - span - 8);
        let mut mem = vec![0u8; MEM];
        let mut x = seed | 1;
        for b in &mut mem[0x1000..] {
            x ^= x << 13; x ^= x >> 7; x ^= x << 17;
            *b = (x % (fill as u64 + 1)) as u8; // (few values: compares and scans end now and then)
        }
        let opc = match (op, size) {
            (Op::Movs, 1) => 0xa4, (Op::Movs, _) => 0xa5, (Op::Stos, 1) => 0xaa, (Op::Stos, _) => 0xab,
            (Op::Lods, 1) => 0xac, (Op::Lods, _) => 0xad, (Op::Cmps, 1) => 0xa6, (Op::Cmps, _) => 0xa7,
            (Op::Scas, 1) => 0xae, (Op::Scas, _) => 0xaf,
        };
        let mut code = vec![];
        if size == 2 { code.push(0x66); }
        if rep != 0 { code.push(rep); }
        code.push(opc);
        mem[CODE as usize..CODE as usize + code.len()].copy_from_slice(&code);
        let mut regs = [0u32; 8];
        regs[0] = al % 3;
        regs[1] = count;
        regs[6] = if back { src + span - size } else { src };
        regs[7] = if back { dst + span - size } else { dst };
        let mut want_mem = mem.clone();
        let mut want = regs;
        let zf = model(op, size, rep, back, &mut want, &mut want_mem);

        let mut m = Flat { m: mem, bulk };
        let mut cpu = Cpu { eip: CODE, eflags: 2 | if back { 0x400 } else { 0 }, regs, ..Cpu::default() };
        let (steps, e) = run(&mut cpu, &mut m);
        prop_assert_eq!(e, Exit::Continue);
        prop_assert_eq!(cpu.eip, CODE + code.len() as u32);
        prop_assert_eq!(cpu.regs, want);
        prop_assert!(m.m == want_mem, "memory differs");
        if matches!(op, Op::Cmps | Op::Scas) && (rep == 0 || count > 0) {
            prop_assert_eq!(cpu.flags() & 0x40 != 0, zf);
        }
        // (and no step ran more than REP_STEP_MAX of them)
        if rep != 0 {
            prop_assert!(steps <= (count as u64).div_ceil(REP_STEP_MAX).max(1));
        }
    }
}
