//! Every instruction of the SIMD and bit-manipulation extensions koji implements, against the
//! host CPU: each iced `Code` of MMX, SSE..SSE4.2, AVX, AVX2, FMA3, BMI1, BMI2, POPCNT and
//! LZCNT (legacy and VEX encodings, register and memory forms, 128- and 256-bit) runs
//! `CASES` times from random state, once in the interpreter (long mode) and once natively.
//! General registers, the defined status flags, MXCSR (with its exception flags), all sixteen YMM registers (both
//! halves), the MMX registers and the memory the operands point into must agree.
//!
//! Unlike the sampled tests (diff_native*.rs), the list of codes is not hand-made: it is
//! every code iced knows for these features, so an instruction the interpreter lacks shows up
//! here as "not implemented" instead of not being tested. `IPPAI_ISA_FILTER=<prefix>` limits
//! the run to codes whose name or mnemonic starts with it; `IPPAI_ISA_CASES=<n>` sets the
//! cases per code.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use iced_x86::code_asm::*;
use iced_x86::{Code, CpuidFeature as F, EncodingKind, Instruction, Mnemonic as Mn, OpCodeOperandKind as K, OpKind, Register};
use koji::{Cpu, Exit, Fault, Memory};

const CASES: usize = 48;
const BUF: usize = 0x1000;
const G_CODE: u32 = 0x10_0000;
const G_DATA: u32 = 0x20_0000;
const G_MEM: usize = 0x40_0000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct State {
    gpr: [u64; 16],
    flags: u64,
    mxcsr: u64,
    ymm: [[u128; 2]; 16],
    mm: [u64; 8],
}

struct Mem(Vec<u8>);
impl Memory for Mem {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a as usize).copied().filter(|_| a >= 0x10000).ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if a < 0x10000 || a as usize >= self.0.len() {
            return Err(Fault { addr: a, write: true });
        }
        self.0[a as usize] = v;
        Ok(())
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
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The extensions under test; a code is tested when all its features are in `OK` and one is in
/// `TARGET`.
const TARGET: &[F] = &[F::MMX, F::SSE, F::SSE2, F::SSE3, F::SSSE3, F::SSE4_1, F::SSE4_2, F::POPCNT, F::LZCNT, F::BMI1, F::BMI2, F::AVX, F::AVX2, F::FMA];
const OK: &[F] = &[F::INTEL386, F::INTEL486, F::X64, F::CMOV, F::CX8];

/// Mnemonics left out: they change state outside what is compared (MXCSR loads can unmask
/// exceptions), are hints, or are not instructions of these extensions' data path.
fn skipped(m: Mn) -> bool {
    matches!(
        m,
        Mn::Ldmxcsr | Mn::Vldmxcsr | Mn::Prefetchnta | Mn::Prefetcht0 | Mn::Prefetcht1 | Mn::Prefetcht2 | Mn::Sfence | Mn::Lfence | Mn::Mfence | Mn::Clflush | Mn::Pause
            | Mn::Fxsave | Mn::Fxrstor | Mn::Fxsave64 | Mn::Fxrstor64 | Mn::Monitor | Mn::Mwait
    )
}

fn codes() -> Vec<Code> {
    let filter = std::env::var("IPPAI_ISA_FILTER").ok();
    Code::values()
        .filter(|&c| {
            let op = c.op_code();
            if !matches!(op.encoding(), EncodingKind::Legacy | EncodingKind::VEX) || !op.mode64() || op.op_count() == 0 && !matches!(c.mnemonic(), Mn::Vzeroupper | Mn::Vzeroall | Mn::Emms) {
                return false;
            }
            // from the Code itself: Instruction::with is only for codes without operands
            // (iced debug_asserts it, which failed every debug build and so cargo-mutants)
            let f = c.cpuid_features();
            if !f.iter().any(|x| TARGET.contains(x)) || !f.iter().all(|x| TARGET.contains(x) || OK.contains(x)) {
                return false;
            }
            if skipped(c.mnemonic()) || c.flow_control() != iced_x86::FlowControl::Next {
                return false;
            }
            (0..op.op_count()).all(|i| kind_ok(op.op_kind(i)))
                && filter.as_deref().is_none_or(|p| format!("{c:?}").starts_with(p) || format!("{:?}", c.mnemonic()).eq_ignore_ascii_case(p))
        })
        .collect()
}

fn kind_ok(k: K) -> bool {
    matches!(
        k,
        K::r8_reg | K::r16_reg | K::r16_rm | K::r32_reg | K::r32_rm | K::r32_vvvv | K::r64_reg | K::r64_rm | K::r64_vvvv
            | K::r8_or_mem | K::r16_or_mem | K::r32_or_mem | K::r64_or_mem | K::r32_reg_mem | K::r64_reg_mem | K::r16_reg_mem
            | K::mem | K::mm_reg | K::mm_rm | K::mm_or_mem | K::xmm_reg | K::xmm_rm | K::xmm_vvvv | K::xmm_is4 | K::xmm_or_mem
            | K::ymm_reg | K::ymm_rm | K::ymm_vvvv | K::ymm_is4 | K::ymm_or_mem
            | K::mem_vsib32x | K::mem_vsib64x | K::mem_vsib32y | K::mem_vsib64y
            | K::imm8 | K::imm8sex16 | K::imm8sex32 | K::imm8sex64 | K::imm16 | K::imm32 | K::imm32sex64
    )
}

/// General registers for random operands: not RBX (the memory base) or RSP.
const GPRS: [u32; 14] = [0, 1, 2, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

fn gpr(rng: &mut Rng, bits: u32) -> Register {
    let n = GPRS[rng.below(GPRS.len() as u64) as usize];
    match bits {
        64 => Register::RAX + n,
        32 => Register::EAX + n,
        16 => Register::AX + n,
        _ => {
            if n < 4 {
                Register::AL + n
            } else if n < 8 {
                Register::SPL + (n - 4)
            } else {
                Register::R8L + (n - 8)
            }
        }
    }
}

fn set_mem(ins: &mut Instruction, i: u32, rng: &mut Rng) {
    ins.set_op_kind(i, OpKind::Memory);
    ins.set_memory_base(Register::RBX);
    // 32-aligned (aligned 256-bit loads), room for a 32-byte operand
    ins.set_memory_displacement64(rng.below(BUF as u64 / 32 - 4) * 32);
    ins.set_memory_displ_size(1);
}

/// A random instance of `c` (the VSIB index register is returned: its lanes are set to small
/// indices).
fn make(rng: &mut Rng, c: Code) -> Option<(Instruction, Option<Register>)> {
    let op = c.op_code();
    let mut ins = Instruction::default();
    ins.set_code(c);
    ins.set_code_size(iced_x86::CodeSize::Code64);
    let mem_form = rng.below(3) == 0;
    let mut vsib = None;
    for i in 0..op.op_count() {
        let k = op.op_kind(i);
        let reg = |ins: &mut Instruction, r: Register| {
            ins.set_op_kind(i, OpKind::Register);
            ins.set_op_register(i, r);
        };
        match k {
            K::r8_reg => reg(&mut ins, gpr(rng, 8)),
            K::r16_reg | K::r16_rm | K::r16_reg_mem => reg(&mut ins, gpr(rng, 16)),
            K::r32_reg | K::r32_rm | K::r32_vvvv | K::r32_reg_mem => reg(&mut ins, gpr(rng, 32)),
            K::r64_reg | K::r64_rm | K::r64_vvvv | K::r64_reg_mem => reg(&mut ins, gpr(rng, 64)),
            K::r8_or_mem | K::r16_or_mem | K::r32_or_mem | K::r64_or_mem => {
                if mem_form {
                    set_mem(&mut ins, i, rng);
                } else {
                    let b = match k {
                        K::r8_or_mem => 8,
                        K::r16_or_mem => 16,
                        K::r32_or_mem => 32,
                        _ => 64,
                    };
                    reg(&mut ins, gpr(rng, b));
                }
            }
            K::mem => set_mem(&mut ins, i, rng),
            K::mem_vsib32x | K::mem_vsib64x | K::mem_vsib32y | K::mem_vsib64y => {
                set_mem(&mut ins, i, rng);
                let y = matches!(k, K::mem_vsib32y | K::mem_vsib64y);
                let x = (if y { Register::YMM0 } else { Register::XMM0 }) + rng.below(16) as u32;
                ins.set_memory_index(x);
                ins.set_memory_index_scale([1, 2, 4, 8][rng.below(4) as usize]);
                // keep [rbx + disp + index * scale] inside the buffer
                ins.set_memory_displacement64(rng.below(8) * 32);
                vsib = Some(x);
            }
            K::mm_reg | K::mm_rm => reg(&mut ins, Register::MM0 + rng.below(8) as u32),
            K::mm_or_mem => {
                if mem_form {
                    set_mem(&mut ins, i, rng)
                } else {
                    reg(&mut ins, Register::MM0 + rng.below(8) as u32)
                }
            }
            K::xmm_reg | K::xmm_rm | K::xmm_vvvv | K::xmm_is4 => reg(&mut ins, Register::XMM0 + rng.below(16) as u32),
            K::xmm_or_mem => {
                if mem_form {
                    set_mem(&mut ins, i, rng)
                } else {
                    reg(&mut ins, Register::XMM0 + rng.below(16) as u32)
                }
            }
            K::ymm_reg | K::ymm_rm | K::ymm_vvvv | K::ymm_is4 => reg(&mut ins, Register::YMM0 + rng.below(16) as u32),
            K::ymm_or_mem => {
                if mem_form {
                    set_mem(&mut ins, i, rng)
                } else {
                    reg(&mut ins, Register::YMM0 + rng.below(16) as u32)
                }
            }
            K::imm8 | K::imm8sex16 | K::imm8sex32 | K::imm8sex64 => {
                ins.set_op_kind(
                    i,
                    match k {
                        K::imm8 => OpKind::Immediate8,
                        K::imm8sex16 => OpKind::Immediate8to16,
                        K::imm8sex32 => OpKind::Immediate8to32,
                        _ => OpKind::Immediate8to64,
                    },
                );
                let v = rng.next();
                ins.set_immediate8(if rng.below(3) == 0 { (v % 70) as u8 } else { v as u8 });
            }
            K::imm16 => {
                ins.set_op_kind(i, OpKind::Immediate16);
                ins.set_immediate16(rng.next() as u16);
            }
            K::imm32 | K::imm32sex64 => {
                ins.set_op_kind(i, if k == K::imm32 { OpKind::Immediate32 } else { OpKind::Immediate32to64 });
                ins.set_immediate32(rng.next() as u32);
            }
            _ => return None,
        }
    }
    // gathers: the mask, index and destination must be different registers
    if let Some(x) = vsib {
        let regs: Vec<u32> = (0..op.op_count()).filter(|&i| ins.op_kind(i) == OpKind::Register).map(|i| ins.op_register(i).number() as u32).collect();
        let idx = x.number() as u32;
        if regs.contains(&idx) || (regs.len() == 2 && regs[0] == regs[1]) {
            return None;
        }
    }
    Some((ins, vsib))
}

/// iced's RFLAGS bits -> EFLAGS bits
const FLAG_MAP: [(u32, u64); 6] = [(1, 0x800), (2, 0x80), (4, 0x40), (8, 0x10), (0x10, 1), (0x20, 4)];

/// The native run of `bytes` from state `s` (the state block's address in RDI).
unsafe fn native(bytes: &[u8], s: &mut State, page: *mut u8) {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut save_rsp = a.create_label();
    let mut state = a.create_label();
    let mut tmp = a.create_label();
    for r in [rbx, rbp, r12, r13, r14, r15] {
        a.push(r).unwrap();
    }
    a.mov(qword_ptr(save_rsp), rsp).unwrap();
    a.mov(qword_ptr(state), rdi).unwrap();
    let ymms = [ymm0, ymm1, ymm2, ymm3, ymm4, ymm5, ymm6, ymm7, ymm8, ymm9, ymm10, ymm11, ymm12, ymm13, ymm14, ymm15];
    let mms = [mm0, mm1, mm2, mm3, mm4, mm5, mm6, mm7];
    let regs = [rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8, r9, r10, r11, r12, r13, r14, r15];
    const MX: i32 = 136;
    const Y: i32 = 144;
    const MM: i32 = Y + 16 * 32;
    for (i, y) in ymms.iter().enumerate() {
        a.vmovdqu(*y, ymmword_ptr(rdi + Y + 32 * i as i32)).unwrap();
    }
    for (i, m) in mms.iter().enumerate() {
        a.movq(*m, qword_ptr(rdi + MM + 8 * i as i32)).unwrap();
    }
    a.ldmxcsr(dword_ptr(rdi + MX)).unwrap();
    a.push(qword_ptr(rdi + 128)).unwrap();
    a.popfq().unwrap();
    for (i, r) in regs.iter().enumerate() {
        if i != 4 && i != 7 {
            a.mov(*r, qword_ptr(rdi + 8 * i as i32)).unwrap();
        }
    }
    a.mov(rsp, qword_ptr(rdi + 32)).unwrap();
    a.mov(rdi, qword_ptr(rdi + 56)).unwrap();
    a.db(bytes).unwrap();
    a.mov(qword_ptr(tmp), rdi).unwrap();
    a.mov(rdi, qword_ptr(state)).unwrap();
    for (i, r) in regs.iter().enumerate() {
        if i != 4 && i != 7 {
            a.mov(qword_ptr(rdi + 8 * i as i32), *r).unwrap();
        }
    }
    a.mov(qword_ptr(rdi + 32), rsp).unwrap();
    a.mov(rsp, qword_ptr(save_rsp)).unwrap();
    a.mov(rax, qword_ptr(tmp)).unwrap();
    a.mov(qword_ptr(rdi + 56), rax).unwrap();
    a.pushfq().unwrap();
    a.pop(rax).unwrap();
    a.mov(qword_ptr(rdi + 128), rax).unwrap();
    a.stmxcsr(dword_ptr(rdi + MX)).unwrap();
    for (i, y) in ymms.iter().enumerate() {
        a.vmovdqu(ymmword_ptr(rdi + Y + 32 * i as i32), *y).unwrap();
    }
    for (i, m) in mms.iter().enumerate() {
        a.movq(qword_ptr(rdi + MM + 8 * i as i32), *m).unwrap();
    }
    a.emms().unwrap();
    a.vzeroupper().unwrap();
    a.cld().unwrap();
    for r in [r15, r14, r13, r12, rbp, rbx] {
        a.pop(r).unwrap();
    }
    a.ret().unwrap();
    a.set_label(&mut save_rsp).unwrap();
    a.dq(&[0]).unwrap();
    a.set_label(&mut state).unwrap();
    a.dq(&[0]).unwrap();
    a.set_label(&mut tmp).unwrap();
    a.dq(&[0]).unwrap();
    let code = a.assemble(page as u64).unwrap();
    std::ptr::copy_nonoverlapping(code.as_ptr(), page, code.len());
    let f: extern "sysv64" fn(*mut State) = std::mem::transmute(page);
    f(s);
}

/// Special values: zeros, infinities, quiet and signaling NaNs of both signs, denormals,
/// the largest and smallest normals, one.
const F32_SPECIAL: [u32; 12] = [0, 0x8000_0000, 0x7f80_0000, 0xff80_0000, 0x7fc0_0000, 0xffc1_2345, 0x7fa0_0001, 0xff80_0101, 0x0000_0001, 0x807f_ffff, 0x7f7f_ffff, 0x3f80_0000];
const F64_SPECIAL: [u64; 12] = [
    0, 1 << 63, 0x7ff0 << 48, 0xfff0 << 48, 0x7ff8 << 48, 0xfff8_1234_5678_9abc, 0x7ff4_0000_0000_0001, 0xfff0_0000_0000_0101, 1, 0x800f_ffff_ffff_ffff,
    0x7fef_ffff_ffff_ffff, 0x3ff0 << 48,
];

fn rand_lane(rng: &mut Rng) -> u128 {
    match rng.below(6) {
        4 => (0..4).fold(0u128, |v, i| v | (F32_SPECIAL[rng.below(12) as usize] as u128) << (32 * i)),
        5 => (0..2).fold(0u128, |v, i| v | (F64_SPECIAL[rng.below(12) as usize] as u128) << (64 * i)),
        // doubles and floats in useful ranges
        0 => (((rng.below(2000) as f64 - 1000.0) / 7.0).to_bits() as u128) | ((((rng.below(2000) as f64) / 3.0).to_bits() as u128) << 64),
        1 => {
            let f = |r: &mut Rng| ((r.below(2000) as f32 - 1000.0) / 3.0).to_bits() as u128;
            f(rng) | f(rng) << 32 | f(rng) << 64 | f(rng) << 96
        }
        // small integers (lane indices, shift counts)
        2 => (0..16).fold(0u128, |v, i| v | ((rng.below(40) as u128) << (8 * i))),
        _ => rng.next() as u128 | (rng.next() as u128) << 64,
    }
}

#[test]
fn every_simd_and_bit_instruction_matches_the_host_cpu() {
    if !(std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") && std::is_x86_feature_detected!("bmi2") && std::is_x86_feature_detected!("lzcnt")) {
        eprintln!("host CPU without AVX2/FMA/BMI2/LZCNT: skipped");
        return;
    }
    let cases: usize = std::env::var("IPPAI_ISA_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(CASES);
    let list = codes();
    eprintln!("{} codes x {cases}", list.len());
    let page = unsafe { libc::mmap(std::ptr::null_mut(), 0x10000, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) } as *mut u8;
    let mut nbuf = vec![0u8; BUF + 64];
    let mut fails: std::collections::BTreeMap<String, (usize, String)> = Default::default();
    let mut unimpl: std::collections::BTreeMap<String, usize> = Default::default();
    let mut mem = Mem(vec![0u8; G_MEM]);
    let mut ran = 0;
    for (ci, &c) in list.iter().enumerate() {
        let mut rng = Rng((ci as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut done = 0;
        for _ in 0..cases * 4 {
            if done == cases {
                break;
            }
            let Some((ins, vsib)) = make(&mut rng, c) else { continue };
            let mut enc = iced_x86::Encoder::new(64);
            if enc.encode(&ins, G_CODE as u64).is_err() {
                continue;
            }
            let bytes = enc.take_buffer();
            done += 1;
            let mut s = State::default();
            for i in 0..16 {
                s.gpr[i] = match rng.below(4) {
                    0 => rng.below(70),
                    1 => rng.next() as u32 as u64,
                    2 => (rng.next() as i32 as i64) as u64,
                    _ => rng.next(),
                };
                s.ymm[i] = [rand_lane(&mut rng), rand_lane(&mut rng)];
            }
            for m in s.mm.iter_mut() {
                *m = rand_lane(&mut rng) as u64;
            }
            if let Some(x) = vsib {
                // small indices (dwords or qwords), so every lane stays inside the buffer
                let n = x.number();
                let q = matches!(c.op_code().op_kind((0..c.op_code().op_count()).find(|&i| ins.op_kind(i) == OpKind::Memory).unwrap()), K::mem_vsib64x | K::mem_vsib64y);
                let mut v = [0u128; 2];
                for l in 0..(if q { 4 } else { 8 }) {
                    let idx = rng.below(48) as u128;
                    if q {
                        v[l / 2] |= idx << (64 * (l % 2));
                    } else {
                        v[l / 4] |= idx << (32 * (l % 4));
                    }
                }
                s.ymm[n] = v;
            }
            s.flags = 0x202 | (rng.next() & 0x8d5);
            // (any rounding mode: the flags and results of every RC against the CPU)
            s.mxcsr = 0x1f80 | rng.below(4) << 13;
            let mut ptrs = vec![3usize, 4];
            s.gpr[3] = 0;
            s.gpr[4] = 0x800;
            if matches!(c.mnemonic(), Mn::Maskmovq | Mn::Maskmovdqu | Mn::Vmaskmovdqu) {
                ptrs.push(7);
                s.gpr[7] = 0x400 + rng.below(0x400);
            }
            let data: Vec<u8> = (0..BUF).map(|_| rng.next() as u8).collect();
            let off = (32 - (nbuf.as_ptr() as usize & 31)) & 31;
            let nptr = unsafe { nbuf.as_mut_ptr().add(off) };
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), nptr, BUF) };
            let mut ns = s;
            for &p in &ptrs {
                ns.gpr[p] = s.gpr[p] + nptr as u64;
            }
            // interpreter
            let mut cpu = Cpu { mode64: 1, eip: G_CODE, ..Cpu::default() };
            for i in 0..16 {
                cpu.set_gpr(i, s.gpr[i]);
                cpu.set_ymm(i, s.ymm[i]);
            }
            cpu.mm = s.mm;
            for &p in &ptrs {
                cpu.set_gpr(p, s.gpr[p] + G_DATA as u64);
            }
            cpu.eflags = s.flags as u32;
            cpu.mxcsr = s.mxcsr as u32;
            mem.0[G_CODE as usize..G_CODE as usize + bytes.len()].copy_from_slice(&bytes);
            mem.0[G_DATA as usize..G_DATA as usize + BUF].copy_from_slice(&data);
            let e = koji::step(&mut cpu, &mut mem);
            if e != Exit::Continue {
                *unimpl.entry(format!("{c:?}")).or_default() += 1;
                continue;
            }
            // (the host's own MXCSR comes back: the interpreter's arithmetic runs on it)
            #[allow(deprecated)]
            unsafe {
                let host = std::arch::x86_64::_mm_getcsr();
                native(&bytes, &mut ns, page);
                std::arch::x86_64::_mm_setcsr(host);
            }
            cpu.materialize();
            ran += 1;
            let undef: u64 = FLAG_MAP.iter().filter(|(b, _)| ins.rflags_undefined() & b != 0).map(|(_, f)| *f).sum();
            // RCP/RSQRT: approximations, each vendor's own (within 1.5 * 2^-12 of the exact value)
            let approx = matches!(c.mnemonic(), Mn::Rcpps | Mn::Rcpss | Mn::Rsqrtps | Mn::Rsqrtss | Mn::Vrcpps | Mn::Vrcpss | Mn::Vrsqrtps | Mn::Vrsqrtss);
            let near = |a: u128, b: u128| {
                (0..4).all(|k| {
                    let (x, y) = (f32::from_bits((a >> (32 * k)) as u32), f32::from_bits((b >> (32 * k)) as u32));
                    x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()) || ((x - y).abs() as f64) <= (y.abs() as f64) * 3.0 / 4096.0
                })
            };
            // DPPS/DPPD: which NaN a lane gets when operands are NaN differs between the lanes
            // of one result on Intel (the hardware's own order of the sums); only "a NaN" is
            // checked there
            let nan_bits = match c.mnemonic() {
                Mn::Dpps | Mn::Vdpps => 32,
                Mn::Dppd | Mn::Vdppd => 64,
                _ => 0,
            };
            let nan_same = |a: u128, b: u128| {
                nan_bits != 0
                    && (0..128 / nan_bits).all(|k| {
                        let m = if nan_bits == 64 { u64::MAX as u128 } else { 0xffff_ffff };
                        let (x, y) = ((a >> (nan_bits * k)) & m, (b >> (nan_bits * k)) & m);
                        let nan = |v: u128| if nan_bits == 64 { f64::from_bits(v as u64).is_nan() } else { f32::from_bits(v as u32).is_nan() };
                        x == y || (nan(x) && nan(y))
                    })
            };
            let mut d = Vec::new();
            for i in 0..16 {
                if ptrs.contains(&i) {
                    let (a, b) = (cpu.gpr(i).wrapping_sub(G_DATA as u64), ns.gpr[i].wrapping_sub(nptr as u64));
                    if a != b {
                        d.push(format!("r{i} interp +{a:#x} cpu +{b:#x}"));
                    }
                } else if cpu.gpr(i) != ns.gpr[i] {
                    d.push(format!("r{i} interp {:#x} cpu {:#x}", cpu.gpr(i), ns.gpr[i]));
                }
            }
            let fm = 0x8d5 & !undef;
            if cpu.eflags as u64 & fm != ns.flags & fm {
                d.push(format!("flags interp {:#x} cpu {:#x}", cpu.eflags as u64 & fm, ns.flags & fm));
            }
            if cpu.mxcsr as u64 != ns.mxcsr {
                d.push(format!("mxcsr interp {:#x} cpu {:#x}", cpu.mxcsr, ns.mxcsr));
            }
            for i in 0..16 {
                let y = cpu.ymm(i);
                for (h, (&got, &want)) in y.iter().zip(&ns.ymm[i]).enumerate() {
                    if got != want && !(approx && near(got, want)) && !nan_same(got, want) {
                        d.push(format!("ymm{i}.{} interp {got:#034x} cpu {want:#034x}", if h == 0 { "lo" } else { "hi" }));
                    }
                }
            }
            for i in 0..8 {
                if cpu.mm[i] != ns.mm[i] {
                    d.push(format!("mm{i} interp {:#x} cpu {:#x}", cpu.mm[i], ns.mm[i]));
                }
            }
            let nb = unsafe { std::slice::from_raw_parts(nptr, BUF) };
            if &mem.0[G_DATA as usize..G_DATA as usize + BUF] != nb {
                let i = nb.iter().zip(&mem.0[G_DATA as usize..]).position(|(a, b)| a != b).unwrap();
                d.push(format!("mem +{i:#x}"));
            }
            if !d.is_empty() {
                let mut f = iced_x86::FastFormatter::new();
                let mut txt = String::new();
                f.format(&ins, &mut txt);
                let e = fails.entry(format!("{c:?}")).or_insert((0, String::new()));
                e.0 += 1;
                if e.1.is_empty() {
                    let y: Vec<String> = (0..16).map(|i| format!("{:032x}{:032x}", s.ymm[i][1], s.ymm[i][0])).collect();
                    let g: Vec<String> = (0..16).map(|i| format!("{:x}", s.gpr[i])).collect();
                    e.1 = format!("{txt}: {}\n    in flags {:#x} mxcsr {:#x} gpr {}\n    in ymm {}", d.join("; "), s.flags, s.mxcsr, g.join(" "), y.join(" "));
                }
            }
        }
    }
    let mut report = String::new();
    for (c, (n, s)) in &fails {
        report.push_str(&format!("{c} x{n}: {s}\n"));
    }
    if !unimpl.is_empty() {
        report.push_str(&format!("not implemented ({}): {}\n", unimpl.len(), unimpl.keys().cloned().collect::<Vec<_>>().join(" ")));
    }
    eprintln!("{ran} cases run, {} codes failing, {} not implemented", fails.len(), unimpl.len());
    assert!(fails.is_empty() && unimpl.is_empty(), "\n{report}");
}
