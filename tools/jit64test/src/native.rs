//! `native [count] [seed] [filter]`: koji's interpreter against this machine's CPU, one
//! random instruction at a time (the long-mode instructions the JIT leaves to the
//! interpreter: SSE/SSE2, bit operations, shifts by CL, rotates, multiplies, ...). Memory
//! operands are [rbx + disp] into a buffer at a different address on each side (rbx is
//! never written).

use iced_x86::code_asm::*;
use iced_x86::{Code, CpuidFeature, Instruction, MemoryOperand, Mnemonic, OpCodeOperandKind as K, Register};
use koji::{Cpu, Exit, Fault};

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct State {
    gpr: [u64; 16],
    flags: u64,
    xmm: [u128; 16],
}

const BUF: usize = 0x1000;
const G_CODE: u32 = 0x10_0000;
const G_DATA: u32 = 0x20_0000;
const G_MEM: usize = 0x40_0000;

struct Mem(Vec<u8>);
impl koji::Memory for Mem {
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

fn ok_feature(f: CpuidFeature) -> bool {
    use CpuidFeature as F;
    matches!(f, F::INTEL8086 | F::INTEL186 | F::INTEL286 | F::INTEL386 | F::INTEL486 | F::X64 | F::CMOV | F::SSE | F::SSE2 | F::CX8 | F::CMPXCHG16B | F::SSSE3 | F::SSE4_1 | F::AVX | F::AVX2 | F::FMA)
}

/// Codes to test: legacy-encoded, 64-bit mode, plain flow, operands we can make.
fn codes() -> Vec<Code> {
    use Mnemonic as M;
    Code::values()
        .filter(|&c| {
            let op = c.op_code();
            let ins = Instruction::with(c);
            matches!(op.encoding(), iced_x86::EncodingKind::Legacy | iced_x86::EncodingKind::VEX)
                && op.mode64()
                && !op.is_instruction() == false
                && ins.cpuid_features().iter().all(|&f| ok_feature(f))
                && ins.flow_control() == iced_x86::FlowControl::Next
                && !ins.is_privileged()
                && !format!("{c:?}").contains("fence")
                && !matches!(
                    c.mnemonic(),
                    M::Cpuid | M::Rdtsc | M::Rdtscp | M::Xlatb | M::Lfence | M::Mfence | M::Sfence
                        | M::Prefetchnta | M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 | M::Prefetchw | M::Clflush
                        | M::Ldmxcsr | M::Stmxcsr | M::Fxsave | M::Fxrstor | M::Fxsave64 | M::Fxrstor64 | M::Maskmovdqu
                        | M::Pause | M::Nop  | M::Movnti | M::Movntdq
                        | M::Movntpd | M::Movntps | M::Bound | M::Lahf | M::Sahf | M::Wait | M::Lar | M::Lsl
                        | M::Verr | M::Verw | M::Rdpid | M::Ud0 | M::Ud1 | M::Ud2 | M::Int | M::Int3 | M::Into | M::Hlt
                        | M::Syscall | M::Sysret | M::Iret | M::Leave | M::Enter | M::Popf | M::Popfq | M::Popfd | M::Cwd
                        | M::Smsw | M::Str | M::Sldt | M::Sgdt | M::Sidt | M::Cbw | M::Vzeroupper | M::Vzeroall | M::Vldmxcsr | M::Vstmxcsr | M::Vmaskmovdqu
                )
                && (0..op.op_count()).all(|i| maker(op.op_kind(i)).is_some())
                && (0..op.op_count()).all(|i| !matches!(op.op_kind(i), K::seg_reg | K::mm_reg | K::mm_or_mem | K::mm_rm | K::st0 | K::sti_opcode | K::cr_reg | K::dr_reg | K::tr_reg | K::bnd_reg | K::bnd_or_mem_mpx | K::k_reg))
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Mk {
    Gpr(u32),       // bits
    GprRm(u32),     // reg or memory
    Mem,
    Xmm,
    XmmRm,
    Imm(u32),       // bits
    Fixed(Register),
    One,
}

fn maker(k: K) -> Option<Mk> {
    Some(match k {
        K::r8_reg | K::r8_opcode => Mk::Gpr(8),
        K::r16_reg | K::r16_opcode | K::r16_reg_mem | K::r16_rm => Mk::Gpr(16),
        K::r32_reg | K::r32_opcode | K::r32_rm | K::r32_reg_mem => Mk::Gpr(32),
        K::r64_reg | K::r64_opcode | K::r64_rm | K::r64_reg_mem => Mk::Gpr(64),
        K::r8_or_mem => Mk::GprRm(8),
        K::r16_or_mem => Mk::GprRm(16),
        K::r32_or_mem => Mk::GprRm(32),
        K::r64_or_mem => Mk::GprRm(64),
        K::mem => Mk::Mem,
        K::xmm_reg | K::xmm_rm | K::xmm_vvvv => Mk::Xmm,
        K::xmm_or_mem => Mk::XmmRm,
        K::imm8 | K::imm8sex16 | K::imm8sex32 | K::imm8sex64 => Mk::Imm(8),
        K::imm16 => Mk::Imm(16),
        K::imm32 | K::imm32sex64 => Mk::Imm(32),
        K::imm64 => Mk::Imm(64),
        K::imm8_const_1 => Mk::One,
        K::al => Mk::Fixed(Register::AL),
        K::cl => Mk::Fixed(Register::CL),
        K::ax => Mk::Fixed(Register::AX),
        K::dx => Mk::Fixed(Register::DX),
        K::eax => Mk::Fixed(Register::EAX),
        K::rax => Mk::Fixed(Register::RAX),
        _ => return None,
    })
}

const GPRS: [Register; 14] = [Register::RAX, Register::RCX, Register::RDX, Register::RBP, Register::RSI, Register::RDI, Register::R8, Register::R9, Register::R10, Register::R11, Register::R12, Register::R13, Register::R14, Register::R15];

fn gpr(rng: &mut Rng, bits: u32, rex_ok: bool) -> Register {
    let r = GPRS[rng.below(GPRS.len() as u64) as usize];
    let n = r.number();
    match bits {
        64 => r,
        32 => Register::EAX + n as u32,
        16 => Register::AX + n as u32,
        _ => {
            if n < 4 {
                Register::AL + n as u32
            } else if n < 8 {
                if rex_ok { Register::SPL + (n - 4) as u32 } else { Register::AL }
            } else {
                Register::R8L + (n - 8) as u32
            }
        }
    }
}

fn make(rng: &mut Rng, c: Code) -> Option<Instruction> {
    let op = c.op_code();
    let mut ins = Instruction::default();
    ins.set_code(c);
    ins.set_code_size(iced_x86::CodeSize::Code64);
    let mut have_mem = false;
    for i in 0..op.op_count() {
        let mk = maker(op.op_kind(i))?;
        let mem = |rng: &mut Rng| MemoryOperand::new(Register::RBX, Register::None, 1, (rng.below(BUF as u64 / 16 - 2) * 16) as i64, 1, false, Register::None);
        match mk {
            Mk::Gpr(b) => {
                ins.set_op_kind(i, iced_x86::OpKind::Register);
                ins.set_op_register(i, gpr(rng, b, true));
            }
            Mk::GprRm(b) => {
                if rng.below(3) == 0 && !have_mem {
                    have_mem = true;
                    ins.set_op_kind(i, iced_x86::OpKind::Memory);
                    let m = mem(rng);
                    ins.set_memory_base(m.base);
                    ins.set_memory_displacement64(m.displacement as u64);
                    ins.set_memory_displ_size(1);
                } else {
                    ins.set_op_kind(i, iced_x86::OpKind::Register);
                    ins.set_op_register(i, gpr(rng, b, true));
                }
            }
            Mk::Mem => {
                have_mem = true;
                ins.set_op_kind(i, iced_x86::OpKind::Memory);
                let m = mem(rng);
                ins.set_memory_base(m.base);
                ins.set_memory_displacement64(m.displacement as u64);
                ins.set_memory_displ_size(1);
            }
            Mk::Xmm => {
                ins.set_op_kind(i, iced_x86::OpKind::Register);
                ins.set_op_register(i, Register::XMM0 + rng.below(16) as u32);
            }
            Mk::XmmRm => {
                if rng.below(3) == 0 && !have_mem {
                    have_mem = true;
                    ins.set_op_kind(i, iced_x86::OpKind::Memory);
                    let m = mem(rng);
                    ins.set_memory_base(m.base);
                    ins.set_memory_displacement64(m.displacement as u64);
                    ins.set_memory_displ_size(1);
                } else {
                    ins.set_op_kind(i, iced_x86::OpKind::Register);
                    ins.set_op_register(i, Register::XMM0 + rng.below(16) as u32);
                }
            }
            Mk::Imm(b) => {
                let k = match op.op_kind(i) {
                    K::imm8 => iced_x86::OpKind::Immediate8,
                    K::imm8sex16 => iced_x86::OpKind::Immediate8to16,
                    K::imm8sex32 => iced_x86::OpKind::Immediate8to32,
                    K::imm8sex64 => iced_x86::OpKind::Immediate8to64,
                    K::imm16 => iced_x86::OpKind::Immediate16,
                    K::imm32 => iced_x86::OpKind::Immediate32,
                    K::imm32sex64 => iced_x86::OpKind::Immediate32to64,
                    _ => iced_x86::OpKind::Immediate64,
                };
                ins.set_op_kind(i, k);
                let v = rng.next();
                match b {
                    8 => ins.set_immediate8(if rng.below(3) == 0 { (v % 70) as u8 } else { v as u8 }),
                    16 => ins.set_immediate16(v as u16),
                    32 => ins.set_immediate32(v as u32),
                    _ => ins.set_immediate64(v),
                }
            }
            Mk::Fixed(r) => {
                ins.set_op_kind(i, iced_x86::OpKind::Register);
                ins.set_op_register(i, r);
            }
            Mk::One => {
                ins.set_op_kind(i, iced_x86::OpKind::Immediate8);
                ins.set_immediate8(1);
            }
        }
    }
    Some(ins)
}

const FLAG_MAP: [(u32, u64); 6] = [(1, 0x800), (2, 0x80), (4, 0x40), (8, 0x10), (0x10, 1), (0x20, 4)];

/// The native run of `bytes` from state `s`.
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
    let xmms = [xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9, xmm10, xmm11, xmm12, xmm13, xmm14, xmm15];
    let regs = [rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8, r9, r10, r11, r12, r13, r14, r15];
    for (i, x) in xmms.iter().enumerate() {
        a.movdqu(*x, xmmword_ptr(rdi + 144 + 16 * i as i32)).unwrap();
    }
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
    for (i, x) in xmms.iter().enumerate() {
        a.movdqu(xmmword_ptr(rdi + 144 + 16 * i as i32), *x).unwrap();
    }
    a.cld().unwrap();
    a.mov(rsp, qword_ptr(save_rsp)).unwrap();
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

pub fn run(args: &[String]) {
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20000);
    let seed: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    let filter = args.get(4).cloned();
    let mut list = codes();
    if let Some(f) = &filter {
        list.retain(|c| format!("{:?}", c.mnemonic()).eq_ignore_ascii_case(f) || format!("{c:?}").starts_with(f.as_str()));
    }
    eprintln!("{} codes", list.len());
    let page = unsafe { libc::mmap(std::ptr::null_mut(), 0x10000, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) } as *mut u8;
    let mut nbuf = vec![0u8; BUF + 64];
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut fails: std::collections::HashMap<Code, (usize, String)> = Default::default();
    let mut unimpl: std::collections::HashMap<Code, usize> = Default::default();
    let mut mem = Mem(vec![0u8; G_MEM]);
    for _ in 0..count {
        let strings = filter.as_deref().map_or(rng.below(8) == 0, |f| f == "string");
        let (c, ins, bytes) = if strings {
            // movs/cmps/stos/lods/scas with an optional rep/repne and operand size
            let mut b = Vec::new();
            match rng.below(4) {
                0 => b.push(0xf3),
                1 => b.push(0xf2),
                _ => {}
            }
            let op = [0xa4u8, 0xa6, 0xaa, 0xac, 0xae][rng.below(5) as usize];
            match rng.below(4) {
                0 => b.push(op),
                1 => b.push(op + 1),
                2 => b.extend([0x66, op + 1]),
                _ => b.extend([0x48, op + 1]),
            }
            if b[0] == 0x66 || b[0] == 0x48 || b[0] == op || b[0] == op + 1 {
            } else if b[1] == 0x66 {
                b.swap(0, 1);
            }
            let ins = iced_x86::Decoder::with_ip(64, &b, G_CODE as u64, 0).decode();
            if ins.is_invalid() {
                continue;
            }
            (ins.code(), ins, b)
        } else {
            let c = list[rng.below(list.len() as u64) as usize];
            let Some(ins) = make(&mut rng, c) else { continue };
            if std::env::var_os("NV").is_some() {
                eprintln!("enc {c:?}");
            }
            let mut enc = iced_x86::Encoder::new(64);
            if enc.encode(&ins, G_CODE as u64).is_err() {
                continue;
            }
            (c, ins, enc.take_buffer())
        };
        // state
        let mut s = State::default();
        for i in 0..16 {
            s.gpr[i] = match rng.below(4) {
                0 => rng.below(70),
                1 => rng.next() as u32 as u64,
                2 => (rng.next() as i32 as i64) as u64,
                _ => rng.next(),
            };
            s.xmm[i] = match rng.below(3) {
                // doubles/floats in useful ranges too
                0 => (((rng.below(2000) as f64 - 1000.0) / 7.0).to_bits() as u128) | ((((rng.below(2000) as f64) / 3.0).to_bits() as u128) << 64),
                1 => {
                    let f = |r: &mut Rng| ((r.below(2000) as f32 - 1000.0) / 3.0).to_bits() as u128;
                    f(&mut rng) | f(&mut rng) << 32 | f(&mut rng) << 64 | f(&mut rng) << 96
                }
                _ => rng.next() as u128 | (rng.next() as u128) << 64,
            };
        }
        if matches!(c.mnemonic(), iced_x86::Mnemonic::Bt | iced_x86::Mnemonic::Bts | iced_x86::Mnemonic::Btr | iced_x86::Mnemonic::Btc)
            && ins.op0_kind() == iced_x86::OpKind::Memory
            && ins.op1_kind() == iced_x86::OpKind::Register
        {
            // keep the bit offset inside the buffer
            let room = (BUF as u64 - ins.memory_displacement64() - 8) * 8;
            s.gpr[ins.op1_register().full_register().number()] = rng.below(room);
        }
        s.flags = 0x202 | (rng.next() & 0x8d5);
        // registers that hold buffer offsets: the base, the stack, string pointers
        let mut ptrs = vec![3usize, 4];
        s.gpr[3] = 0;
        s.gpr[4] = 0x800;
        if ins.is_string_instruction() {
            ptrs.extend([6, 7]);
            s.gpr[6] = 0x400 + rng.below(0x800);
            s.gpr[7] = 0x400 + rng.below(0x800);
            s.gpr[1] = rng.below(40);
            if rng.below(2) == 0 {
                s.flags |= 0x400;
            }
            if rng.below(4) == 0 {
                // equal runs so repe cmps keeps going
                s.gpr[7] = s.gpr[6] ^ 0x200;
            }
        }
        let data: Vec<u8> = (0..BUF).map(|_| rng.next() as u8).collect();
        // align the native buffer to 16
        let off = (16 - (nbuf.as_ptr() as usize & 15)) & 15;
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
            cpu.set_xmm(i, s.xmm[i]);
        }
        for &p in &ptrs {
            cpu.set_gpr(p, s.gpr[p] + G_DATA as u64);
        }
        cpu.eflags = s.flags as u32;
        mem.0[G_CODE as usize..G_CODE as usize + bytes.len()].copy_from_slice(&bytes);
        mem.0[G_DATA as usize..G_DATA as usize + BUF].copy_from_slice(&data);
        let e = koji::step(&mut cpu, &mut mem);
        if e != Exit::Continue {
            *unimpl.entry(c).or_default() += 1;
            continue;
        }
        if std::env::var_os("NV").is_some() {
            let mut f = iced_x86::FastFormatter::new();
            let mut t = String::new();
            f.format(&ins, &mut t);
            eprintln!("{c:?} {t}");
        }
        if std::env::var_os("NONAT").is_some() {
            continue;
        }
        unsafe { native(&bytes, &mut ns, page) };
        cpu.materialize();
        // compare
        let undef: u64 = FLAG_MAP.iter().filter(|(b, _)| ins.rflags_undefined() & b != 0).map(|(_, f)| *f).sum();
        let mut d = Vec::new();
        for i in 0..16 {
            if ptrs.contains(&i) {
                let (a, b) = (cpu.gpr(i).wrapping_sub(G_DATA as u64), ns.gpr[i].wrapping_sub(nptr as u64));
                if a != b {
                    d.push(format!("r{i} interp +{a:#x} cpu +{b:#x}"));
                }
                continue;
            }
            if cpu.gpr(i) != ns.gpr[i] {
                d.push(format!("r{i} interp {:#x} cpu {:#x}", cpu.gpr(i), ns.gpr[i]));
            }
        }
        let fm = 0xcd5 & !undef;
        if cpu.eflags as u64 & fm != ns.flags & fm {
            d.push(format!("flags interp {:#x} cpu {:#x}", cpu.eflags as u64 & fm, ns.flags & fm));
        }
        for i in 0..16 {
            if cpu.xmm(i) != ns.xmm[i] {
                d.push(format!("xmm{i} interp {:#034x} cpu {:#034x}", cpu.xmm(i), ns.xmm[i]));
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
            let e = fails.entry(c).or_insert((0, String::new()));
            e.0 += 1;
            if e.1.is_empty() {
                let ins_regs: Vec<String> = (0..16).map(|i| format!("{:x}", s.gpr[i])).collect();
                e.1 = format!("{txt}: {} | in flags {:#x} regs {}", d.join("; "), s.flags, ins_regs.join(" "));
            }
        }
    }
    let mut v: Vec<_> = fails.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1 .0));
    for (c, (n, s)) in &v {
        println!("{c:?} x{n}: {s}");
    }
    let mut u: Vec<_> = unimpl.into_iter().collect();
    u.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!("interpreter exits on: {}", u.iter().map(|(c, n)| format!("{c:?} x{n}")).collect::<Vec<_>>().join(", "));
    println!("{} codes failing", v.len());
}
