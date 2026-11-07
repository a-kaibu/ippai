//! Differential test of the 32-bit JIT (koji jit/mod.rs): random x86 code run by the
//! interpreter alone and by translated regions and functions (wasmtime, with the interpreter as
//! the `step` import), from the same state; registers, flags, x87/MMX/SSE state and memory must
//! match. The translations are entered through koji's own dispatcher module, as in the browser.
//! Each test picks a mode: regions (of 1, 2, 3 or any number of blocks: exits chain through the
//! hash table) or function mode (IPPAI_JIT_FN: calls as wasm calls through the call cache), and
//! the loop knobs (range check hoisting, stack slots in locals).
//!
//!   cargo run --release -- [count] [seed]
use iced_x86::code_asm::*;
use iced_x86::{BlockEncoder, BlockEncoderOptions, Code, Instruction, InstructionBlock, MemoryOperand, Register};
use koji::{Cpu, Exit, Fault};
use std::sync::atomic::Ordering::Relaxed;
use wasmtime::*;
mod native;

const CPU: u32 = 0x2000;
const HASH: u32 = 0x8000;
const HASH_N: u32 = 2048;
const RING: u32 = 0xc000;
const CODE: u32 = 0x10_0000;
const END: u32 = 0x18_0000;
const DATA: u32 = 0x20_0000;
const DATA_LEN: u32 = 0x1000;
const STACK: u32 = 0x30_0000;
/// guest memory the interpreter accepts: [0x10000, MEM)
const MEM: usize = 0x40_0000;
/// the call cache (Cpu::jit_calls) and the code page bits (Cpu::jit_code_pages), past MEM
const CALLS: u32 = 0x50_0000;
const PAGES: u32 = 0x58_0000;
const SCRATCH_END: usize = 0x60_0000;

struct Mem(*mut u8);
impl koji::Memory for Mem {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if !(0x10000..MEM as u32).contains(&a) {
            return Err(Fault { addr: a, write: false });
        }
        Ok(unsafe { *self.0.add(a as usize) })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if !(0x10000..MEM as u32).contains(&a) {
            return Err(Fault { addr: a, write: true });
        }
        unsafe { *self.0.add(a as usize) = v };
        Ok(())
    }
}

unsafe fn get_cpu(m: *mut u8) -> Cpu {
    std::ptr::read_unaligned(m.add(CPU as usize) as *const Cpu)
}
unsafe fn put_cpu(m: *mut u8, c: &Cpu) {
    std::ptr::write_unaligned(m.add(CPU as usize) as *mut Cpu, c.clone());
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
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize]
    }
    fn imm32(&mut self) -> i32 {
        match self.below(4) {
            0 => self.below(16) as i32,
            1 => -(self.below(16) as i32),
            2 => self.pick(&[0x7fff_ffff, -0x8000_0000, 0xff, 0x80, 0x7f, 0xffff, 0x8000]),
            _ => self.next() as i32,
        }
    }
}

// (EBX points at DATA, EBP at DATA + 0x100, ESP at the stack: only read, never written)
const R32: [AsmRegister32; 5] = [eax, ecx, edx, esi, edi];
const R16: [AsmRegister16; 5] = [ax, cx, dx, si, di];
const R8: [AsmRegister8; 6] = [al, cl, dl, ah, ch, dh];
const S32: [AsmRegister32; 8] = [eax, ecx, edx, esi, edi, ebx, ebp, esp];
const MM: [AsmRegisterMm; 8] = [mm0, mm1, mm2, mm3, mm4, mm5, mm6, mm7];
const XMM: [AsmRegisterXmm; 8] = [xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7];

/// A memory operand in DATA (or the stack), `n` bytes fitting.
fn mem(rng: &mut Rng, n: i32) -> AsmMemoryOperand {
    mem_a(rng, n, 1)
}

/// The same, aligned to `align` (legacy SSE's 16-byte operands: the CPU faults on others, koji
/// doesn't)
fn mem_a(rng: &mut Rng, n: i32, align: i32) -> AsmMemoryOperand {
    let off = rng.below((DATA_LEN as i32 - 0x100 - n) as u64) as i32 & !(align - 1);
    // (not ESP for aligned ones: a subroutine's ESP is 8 below)
    match if align > 1 { 1 + rng.below(5) } else { rng.below(6) } {
        0 => esp + (rng.below(0x7f0 - n as u64) as i32 & !(align - 1)),
        1 => ebp + (off - 0x100),
        // absolute (a global)
        2 => ptr(DATA as u64 + off as u64),
        // index only: ebp * 2
        3 => ebp * 2 + (off - DATA as i32 - 0x200),
        _ => ebx + off,
    }
}

/// native mode: no undefined flags read (they differ between CPUs)
static NATIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NO_MMX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NO_X87: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One random instruction or a few (into `a`)
fn random_ins(rng: &mut Rng, a: &mut CodeAssembler) -> Result<(), IcedError> {
    let g32 = |rng: &mut Rng| rng.pick(&R32);
    let gs32 = |rng: &mut Rng| rng.pick(&S32);
    let g16 = |rng: &mut Rng| rng.pick(&R16);
    let g8 = |rng: &mut Rng| rng.pick(&R8);
    let k = rng.below(64);
    match k {
        0 => a.mov(g32(rng), gs32(rng))?,
        1 => a.mov(g32(rng), rng.imm32())?,
        2 => a.mov(g32(rng), dword_ptr(mem(rng, 4)))?,
        3 => a.mov(dword_ptr(mem(rng, 4)), gs32(rng))?,
        4 => match rng.below(4) {
            0 => a.mov(g8(rng), g8(rng))?,
            1 => a.mov(byte_ptr(mem(rng, 1)), g8(rng))?,
            2 => a.mov(g16(rng), word_ptr(mem(rng, 2)))?,
            _ => a.mov(word_ptr(mem(rng, 2)), rng.imm32() as i16 as i32)?,
        },
        5 => match rng.below(5) {
            0 => a.movzx(g32(rng), byte_ptr(mem(rng, 1)))?,
            1 => a.movzx(g32(rng), g16(rng))?,
            2 => a.movsx(g32(rng), g8(rng))?,
            3 => a.movsx(g32(rng), word_ptr(mem(rng, 2)))?,
            _ => a.movzx(g16(rng), g8(rng))?,
        },
        6..=17 => {
            // ALU: add or adc sbb and sub xor cmp, then test
            let op = rng.below(9);
            macro_rules! alu {
                ($d:expr, $s:expr) => {
                    match op {
                        0 => a.add($d, $s)?,
                        1 => a.or($d, $s)?,
                        2 => a.adc($d, $s)?,
                        3 => a.sbb($d, $s)?,
                        4 => a.and($d, $s)?,
                        5 => a.sub($d, $s)?,
                        6 => a.xor($d, $s)?,
                        7 => a.cmp($d, $s)?,
                        _ => a.test($d, $s)?,
                    }
                };
            }
            macro_rules! alu_nt {
                ($d:expr, $s:expr) => {
                    match op {
                        0 => a.add($d, $s)?,
                        1 => a.or($d, $s)?,
                        2 => a.adc($d, $s)?,
                        3 => a.sbb($d, $s)?,
                        4 => a.and($d, $s)?,
                        5 => a.sub($d, $s)?,
                        6 => a.xor($d, $s)?,
                        _ => a.cmp($d, $s)?,
                    }
                };
            }
            match rng.below(9) {
                0 | 1 => alu!(g32(rng), gs32(rng)),
                2 => alu!(g32(rng), rng.imm32()),
                3 => alu_nt!(g32(rng), dword_ptr(mem(rng, 4))),
                4 if op != 8 => alu!(dword_ptr(mem(rng, 4)), rng.imm32()),
                4 => alu!(dword_ptr(mem(rng, 4)), g32(rng)),
                5 => alu!(dword_ptr(mem(rng, 4)), gs32(rng)),
                6 => alu!(g8(rng), g8(rng)),
                7 => alu!(g16(rng), rng.imm32() as i16 as i32),
                _ => alu!(byte_ptr(mem(rng, 1)), rng.imm32() as i8 as i32),
            }
        }
        18 | 19 => {
            let op = rng.below(4);
            macro_rules! un {
                ($d:expr) => {
                    match op {
                        0 => a.inc($d)?,
                        1 => a.dec($d)?,
                        2 => a.neg($d)?,
                        _ => a.not($d)?,
                    }
                };
            }
            match rng.below(4) {
                0 | 1 => un!(g32(rng)),
                2 => un!(dword_ptr(mem(rng, 4))),
                _ => un!(g8(rng)),
            }
        }
        20..=23 => {
            // shifts and rotates: by an immediate, 1, or CL
            let op = rng.below(7);
            let n = rng.pick(&[1u32, 2, 7, 8, 15, 16, 31, 0, 33]);
            macro_rules! sh {
                ($d:expr, $n:expr) => {
                    match op {
                        0 => a.shl($d, $n)?,
                        1 => a.shr($d, $n)?,
                        2 => a.sar($d, $n)?,
                        3 => a.rol($d, $n)?,
                        4 => a.ror($d, $n)?,
                        5 => a.rcl($d, $n)?,
                        _ => a.rcr($d, $n)?,
                    }
                };
            }
            match rng.below(5) {
                0 => sh!(g32(rng), n),
                1 => sh!(dword_ptr(mem(rng, 4)), n),
                2 => {
                    a.mov(ecx, rng.below(40) as u32)?;
                    sh!(rng.pick(&[eax, edx, esi, edi]), cl)
                }
                3 => sh!(g8(rng), n & 7),
                _ => sh!(g16(rng), n & 15),
            }
            // (OF is undefined after more than 1 bit, and CPUs differ: native mode defines it)
            if NATIVE.load(Relaxed) {
                a.adc(eax, 0)?
            }
        }
        24 => {
            match rng.below(4) {
                0 => a.shld(g32(rng), g32(rng), rng.below(32) as u32)?,
                1 => a.shrd(g32(rng), g32(rng), rng.below(32) as u32)?,
                2 => {
                    a.mov(ecx, rng.below(40) as u32)?;
                    a.shld(rng.pick(&[eax, edx, esi]), rng.pick(&[edx, esi, edi]), cl)?
                }
                _ => a.shrd(dword_ptr(mem(rng, 4)), g32(rng), rng.below(32) as u32)?,
            }
            if NATIVE.load(Relaxed) {
                a.adc(eax, 0)?
            }
        }
        25 | 26 => match rng.below(6) {
            // (SF ZF AF PF are undefined after IMUL, and CPUs differ: native mode defines them)
            k @ 0..=2 => {
                match k {
                    0 => a.imul_2(g32(rng), gs32(rng))?,
                    1 => a.imul_3(g32(rng), gs32(rng), rng.imm32())?,
                    _ => a.imul_2(g32(rng), dword_ptr(mem(rng, 4)))?,
                }
                if NATIVE.load(Relaxed) {
                    a.adc(eax, 0)?
                }
            }
            3 => {
                a.imul(g32(rng))?;
                a.adc(edx, 0)?
            }
            4 => {
                a.mul(rng.pick(&[ecx, esi, edi, ebx]))?;
                a.adc(edx, 0)?
            }
            _ => {
                a.imul_2(g16(rng), g16(rng))?;
                if NATIVE.load(Relaxed) {
                    a.adc(eax, 0)?
                }
            }
        },
        27 => {
            // division: a divisor that isn't 0 (overflow still possible: both stop then)
            let d = rng.pick(&[ecx, esi, edi]);
            a.or(d, 1 + rng.below(1000) as i32)?;
            if rng.below(2) == 0 {
                a.xor(edx, edx)?;
                a.div(d)?
            } else {
                a.cdq()?;
                a.idiv(d)?
            }
            // (the flags are undefined after a division)
            a.test(eax, eax)?
        }
        28 | 29 => {
            let d = g32(rng);
            match rng.below(4) {
                0 => a.lea(d, gs32(rng) + gs32(rng) * rng.pick(&[1, 2, 4, 8]) + rng.imm32())?,
                1 => a.lea(d, gs32(rng) * rng.pick(&[2, 4, 8]))?,
                2 => a.lea(d, gs32(rng) + rng.imm32())?,
                _ => a.lea(d, ptr(rng.next() as u32 as u64))?,
            }
        }
        30 => {
            let cc = rng.below(16);
            let (d, s) = (g32(rng), gs32(rng));
            let m = dword_ptr(mem(rng, 4));
            match cc {
                0 => a.cmovo(d, s)?,
                1 => a.cmovno(d, m)?,
                2 => a.cmovb(d, s)?,
                3 => a.cmovae(d, s)?,
                4 => a.cmove(d, s)?,
                5 => a.cmovne(d, m)?,
                6 => a.cmovbe(d, s)?,
                7 => a.cmova(d, s)?,
                8 => a.cmovs(d, s)?,
                9 => a.cmovns(d, s)?,
                10 => a.cmovp(d, s)?,
                11 => a.cmovnp(d, s)?,
                12 => a.cmovl(d, s)?,
                13 => a.cmovge(d, m)?,
                14 => a.cmovle(d, s)?,
                _ => a.cmovg(d, s)?,
            }
        }
        31 => {
            let d = g8(rng);
            match rng.below(8) {
                0 => a.setb(d)?,
                1 => a.sete(byte_ptr(mem(rng, 1)))?,
                2 => a.setbe(d)?,
                3 => a.sets(d)?,
                4 => a.setp(d)?,
                5 => a.setl(d)?,
                6 => a.setle(d)?,
                _ => a.seto(d)?,
            }
        }
        32 => match rng.below(7) {
            0 => a.cdq()?,
            1 => a.cwde()?,
            2 => a.cbw()?,
            3 => a.bswap(g32(rng))?,
            4 => a.lahf()?,
            5 => {
                a.mov(ah, rng.below(256) as u32)?;
                a.sahf()?
            }
            _ => a.cmc()?,
        },
        33 => match rng.below(5) {
            0 => a.xchg(g32(rng), g32(rng))?,
            1 => a.xchg(dword_ptr(mem(rng, 4)), g32(rng))?,
            2 => a.xadd(dword_ptr(mem(rng, 4)), g32(rng))?,
            3 => a.cmpxchg(dword_ptr(mem(rng, 4)), g32(rng))?,
            _ => a.cmpxchg(g32(rng), g32(rng))?,
        },
        34 => {
            // locked read-modify-writes
            let m = dword_ptr(mem(rng, 4));
            let s = g32(rng);
            match rng.below(5) {
                0 => a.lock().add(m, s)?,
                1 => a.lock().xadd(m, s)?,
                2 => a.lock().cmpxchg(m, s)?,
                3 => a.lock().inc(m)?,
                _ => a.lock().or(m, rng.imm32())?,
            }
        }
        35 => {
            let op = rng.below(4);
            let n = rng.below(32) as u32;
            macro_rules! bt {
                ($d:expr, $s:expr) => {
                    match op {
                        0 => a.bt($d, $s)?,
                        1 => a.bts($d, $s)?,
                        2 => a.btr($d, $s)?,
                        _ => a.btc($d, $s)?,
                    }
                };
            }
            match rng.below(3) {
                0 => bt!(g32(rng), n),
                1 => bt!(dword_ptr(mem(rng, 4)), n),
                _ => bt!(g32(rng), g32(rng)),
            }
            // (only CF defined: taken in, the rest defined)
            a.adc(eax, 0)?
        }
        36 => {
            match rng.below(4) {
                0 => a.bsf(g32(rng), gs32(rng))?,
                1 => a.bsr(g32(rng), gs32(rng))?,
                2 => a.popcnt(g32(rng), gs32(rng))?,
                _ => a.tzcnt(g32(rng), gs32(rng))?,
            }
            // (most flags undefined: ZF kept for a jcc)
            if rng.below(2) == 0 {
                a.test(eax, eax)?
            } else {
                a.setz(dh)?;
                a.test(eax, eax)?;
                a.test(dh, dh)?
            }
        }
        37 => {
            // push/pop pairs
            match rng.below(4) {
                0 => {
                    a.push(gs32(rng))?;
                    a.pop(g32(rng))?
                }
                1 => {
                    a.push(rng.imm32())?;
                    a.pop(dword_ptr(mem(rng, 4)))?
                }
                2 => {
                    a.pushfd()?;
                    a.pop(g32(rng))?
                }
                _ => {
                    a.push(dword_ptr(mem(rng, 4)))?;
                    a.pop(g32(rng))?
                }
            }
        }
        38 => {
            // string ops over DATA (ESI/EDI set first, a small ECX)
            a.lea(esi, ebx + rng.below(0x400) as i32)?;
            a.lea(edi, ebx + 0x800 + rng.below(0x400) as i32)?;
            a.mov(ecx, rng.below(20) as u32)?;
            match rng.below(6) {
                0 => a.rep().movsd()?,
                1 => a.rep().stosd()?,
                2 => a.rep().movsb()?,
                3 => a.rep().stosb()?,
                4 => a.repe().cmpsb()?,
                _ => a.lodsd()?,
            }
        }
        // (native mode: MMX or x87 in a test, not both: they share registers on the CPU, not in
        // koji)
        39..=47 if !NO_MMX.load(Relaxed) => random_mmx(rng, a)?,
        48..=57 => random_sse(rng, a)?,
        58.. if !NO_X87.load(Relaxed) => random_x87(rng, a)?,
        _ => a.xor(g32(rng), gs32(rng))?,
    }
    Ok(())
}

fn random_mmx(rng: &mut Rng, a: &mut CodeAssembler) -> Result<(), IcedError> {
    let (d, s) = (rng.pick(&MM), rng.pick(&MM));
    let m = qword_ptr(mem(rng, 8));
    let src_mem = rng.below(3) == 0;
    macro_rules! two {
        ($f:ident) => {
            if src_mem {
                a.$f(d, m)?
            } else {
                a.$f(d, s)?
            }
        };
    }
    match rng.below(40) {
        0 => two!(movq),
        1 => a.movq(m, d)?,
        2 => a.movd(d, rng.pick(&S32))?,
        3 => a.movd(rng.pick(&R32), d)?,
        4 => a.movd(dword_ptr(mem(rng, 4)), d)?,
        5 => a.movntq(m, d)?,
        6 => a.emms()?,
        7 => two!(paddb),
        8 => two!(paddw),
        9 => two!(paddd),
        10 => two!(paddusb),
        11 => two!(paddsw),
        12 => two!(psubb),
        13 => two!(psubusw),
        14 => two!(psubsb),
        15 => two!(pmullw),
        16 => two!(pmulhw),
        17 => two!(pmaddwd),
        18 => two!(pand),
        19 => two!(pandn),
        20 => two!(por),
        21 => two!(pxor),
        22 => two!(pcmpeqb),
        23 => two!(pcmpgtw),
        24 => two!(pcmpgtd),
        25 => two!(punpcklbw),
        26 => two!(punpckhwd),
        27 => two!(punpckldq),
        28 => two!(packuswb),
        29 => two!(packsswb),
        30 => two!(packssdw),
        31 => a.psllw(d, rng.below(20) as u32)?,
        32 => a.psrld(d, rng.below(40) as u32)?,
        33 => a.psraw(d, rng.below(20) as u32)?,
        34 => a.psrlq(d, rng.below(70) as u32)?,
        35 => two!(psllq),
        36 => two!(pavgb),
        37 => two!(pmaxsw),
        38 => a.pshufw(d, s, rng.below(256) as u32)?,
        _ => two!(pmulhuw),
    }
    Ok(())
}

fn random_sse(rng: &mut Rng, a: &mut CodeAssembler) -> Result<(), IcedError> {
    let (d, s) = (rng.pick(&XMM), rng.pick(&XMM));
    let m = xmmword_ptr(mem_a(rng, 16, 16));
    let src_mem = rng.below(4) == 0;
    macro_rules! two {
        ($f:ident) => {
            if src_mem {
                a.$f(d, m)?
            } else {
                a.$f(d, s)?
            }
        };
    }
    match rng.below(44) {
        0 => two!(movaps),
        1 => a.movups(m, d)?,
        2 => a.movd(d, rng.pick(&S32))?,
        3 => a.movd(rng.pick(&R32), d)?,
        4 => a.movq(d, qword_ptr(mem(rng, 8)))?,
        5 => a.movq(qword_ptr(mem(rng, 8)), d)?,
        6 => a.movss(d, dword_ptr(mem(rng, 4)))?,
        7 => a.movsd_2(qword_ptr(mem(rng, 8)), d)?,
        8 => two!(addps),
        9 => two!(mulps),
        10 => two!(subss),
        11 => two!(minps),
        12 => two!(maxss),
        13 => two!(addsd),
        14 => two!(mulsd),
        15 => two!(divsd),
        16 => two!(sqrtps),
        17 => two!(andps),
        18 => two!(xorps),
        19 => two!(paddd),
        20 => two!(paddw),
        21 => two!(psubusb),
        22 => two!(pmullw),
        23 => two!(pmaddwd),
        24 => two!(pcmpeqd),
        25 => two!(pcmpgtb),
        26 => two!(punpcklbw),
        27 => two!(punpckhdq),
        28 => two!(packuswb),
        29 => two!(packssdw),
        30 => a.pshufd(d, s, rng.below(256) as u32)?,
        31 => a.shufps(d, s, rng.below(256) as u32)?,
        32 => a.pslld(d, rng.below(40) as u32)?,
        33 => a.psrldq(d, rng.below(20) as u32)?,
        34 => a.psraw(d, rng.below(20) as u32)?,
        35 => two!(cvtdq2ps),
        36 => two!(cvttps2dq),
        37 => a.cvtsi2sd(d, rng.pick(&S32))?,
        38 => a.cvttsd2si(rng.pick(&R32), s)?,
        39 => two!(pshufb),
        40 => two!(unpcklps),
        41 => a.pmovmskb(rng.pick(&R32), s)?,
        42 => two!(pavgw),
        _ => two!(pminub),
    }
    Ok(())
}

/// x87 sequences that leave the stack as they found it (doubles and floats in DATA)
fn random_x87(rng: &mut Rng, a: &mut CodeAssembler) -> Result<(), IcedError> {
    let q = |rng: &mut Rng| qword_ptr(mem(rng, 8));
    match rng.below(10) {
        0 => {
            a.fld(q(rng))?;
            a.fstp(q(rng))?
        }
        1 => {
            a.fld(q(rng))?;
            a.fld(q(rng))?;
            match rng.below(4) {
                0 => a.faddp(st1, st0)?,
                1 => a.fmulp(st1, st0)?,
                2 => a.fsubp(st1, st0)?,
                _ => a.fdivrp(st1, st0)?,
            }
            a.fstp(q(rng))?
        }
        2 => {
            a.fild(dword_ptr(mem(rng, 4)))?;
            a.fmul(dword_ptr(mem(rng, 4)))?;
            a.fistp(dword_ptr(mem(rng, 4)))?
        }
        3 => {
            a.fld1()?;
            a.fld(q(rng))?;
            a.fxch(st0, st1)?;
            a.fsubrp(st1, st0)?;
            a.fstp(dword_ptr(mem(rng, 4)))?
        }
        4 => {
            a.fld(q(rng))?;
            a.fabs()?;
            a.fsqrt()?;
            a.fstp(q(rng))?
        }
        5 => {
            a.fld(q(rng))?;
            a.fld(q(rng))?;
            a.fcomip(st0, st1)?;
            a.fstp(st0)?
        }
        6 => {
            a.fldz()?;
            a.fadd(q(rng))?;
            a.fchs()?;
            a.fst(q(rng))?;
            a.fstp(st0)?
        }
        7 => {
            a.fld(q(rng))?;
            a.fistp(qword_ptr(mem(rng, 8)))?
        }
        8 => {
            a.fld(dword_ptr(mem(rng, 4)))?;
            a.fld(st0)?;
            a.fmulp(st1, st0)?;
            a.fstp(q(rng))?
        }
        _ => {
            a.fld(q(rng))?;
            a.fld(q(rng))?;
            a.fucomp(st0, st1)?;
            a.fnstsw(ax)?;
            // (C0..C3 and TOP: koji keeps no exception flags)
            a.and(ax, 0x7f00)?;
            a.fstp(st0)?
        }
    }
    Ok(())
}

fn encode(list: &[Instruction], rip: u64) -> Option<Vec<u8>> {
    // (a shrunk list may have lost a branch's target)
    for i in list {
        if i.op0_kind() == iced_x86::OpKind::NearBranch32 && i.near_branch32() as u64 >= LBASE && !list.iter().any(|j| j.ip() == i.near_branch32() as u64) {
            return None;
        }
    }
    let block = InstructionBlock::new(list, rip);
    BlockEncoder::encode(32, block, BlockEncoderOptions::NONE).ok().map(|r| r.code_buffer)
}

const LBASE: u64 = 0x7000_0000;
const COUNTER: i64 = 0xff8;

/// Labels every instruction (its IP: an id for the encoder), adds branches: forward jcc, a
/// counted loop, calls to subroutines after the final jmp. Items stay in one piece: branches
/// and calls go only between them.
fn structure(rng: &mut Rng, body: Vec<Vec<Instruction>>, subs: Vec<Vec<Vec<Instruction>>>) -> Vec<Instruction> {
    let mut id = 0u64;
    let mut label = |mut i: Instruction| {
        id += 1;
        i.set_ip(LBASE + id * 16);
        i
    };
    let mut list: Vec<Instruction> = Vec::new();
    let mut cut = std::collections::HashSet::new();
    for it in body {
        for (k, i) in it.into_iter().enumerate() {
            let i = label(i);
            if k == 0 {
                cut.insert(i.ip());
            }
            list.push(i);
        }
    }
    // positions between items: (i, j), i < j < len
    let pair = |rng: &mut Rng, list: &[Instruction], cut: &std::collections::HashSet<u64>| {
        let at: Vec<usize> = (0..list.len()).filter(|&k| cut.contains(&list[k].ip())).collect();
        if at.len() < 2 {
            return None;
        }
        let i = rng.below(at.len() as u64 - 1) as usize;
        let j = i + 1 + rng.below((at.len() - i - 1) as u64) as usize;
        Some((at[i], at[j]))
    };
    let cnt = MemoryOperand::new(Register::EBX, Register::None, 1, COUNTER, 1, false, Register::None);
    let mut lp = None;
    if rng.below(2) == 0 {
        if let Some((i, j)) = pair(rng, &list, &cut) {
            // loop over [i, j)
            let head = list[i].ip();
            let dec = label(Instruction::with1(Code::Dec_rm32, cnt).unwrap());
            let jne = label(Instruction::with_branch(Code::Jne_rel32_32, head).unwrap());
            let init = label(Instruction::with2(Code::Mov_rm32_imm32, cnt, 1 + rng.below(4) as u32).unwrap());
            // (the dec and jne one piece; after the loop: the jne's next)
            cut.insert(dec.ip());
            cut.insert(init.ip());
            lp = Some((init.ip(), jne.ip()));
            list.insert(j, jne);
            list.insert(j, dec);
            list.insert(i, init);
        }
    }
    for _ in 0..rng.below(4) {
        // a forward jcc
        let Some((i, j)) = pair(rng, &list, &cut) else { break };
        let cc = rng.below(16) as u16;
        let code: Code = unsafe { std::mem::transmute::<u16, Code>(Code::Jo_rel32_32 as u16 + cc * 3) };
        let target = list[j].ip();
        // not into the loop from outside it (it would skip the counter's init)
        if let Some((a, b)) = lp {
            let pa = list.iter().position(|x| x.ip() == a).unwrap();
            let pb = list.iter().position(|x| x.ip() == b).unwrap();
            if (i <= pa || i > pb) && j > pa && j <= pb {
                continue;
            }
        }
        let jcc = label(Instruction::with_branch(code, target).unwrap());
        cut.insert(jcc.ip());
        list.insert(i, jcc);
    }
    let mut tail = Vec::new();
    for sub in subs {
        let sub: Vec<Instruction> = sub.into_iter().flatten().map(&mut label).collect();
        let Some(first) = sub.first().map(|i| i.ip()) else { continue };
        let call = label(Instruction::with_branch(Code::Call_rel32_32, first).unwrap());
        let push = label(Instruction::with1(Code::Push_r32, Register::EAX).unwrap());
        let at: Vec<usize> = (0..list.len()).filter(|&k| cut.contains(&list[k].ip())).chain([list.len()]).collect();
        let at = at[rng.below(at.len() as u64) as usize];
        cut.insert(push.ip());
        list.insert(at, call);
        list.insert(at, push);
        tail.extend(sub);
        // (stdcall: pops the pushed argument)
        tail.push(label(Instruction::with1(Code::Retnd_imm16, 4u32).unwrap()));
    }
    let mut jmp = Instruction::with_branch(Code::Jmp_rel32_32, END as u64).unwrap();
    jmp.set_ip(1); // (kept by the shrinker)
    list.push(jmp);
    list.extend(tail);
    list
}

/// Instructions for one random item
fn item(rng: &mut Rng) -> Vec<Instruction> {
    let mut a = CodeAssembler::new(32).unwrap();
    match random_ins(rng, &mut a) {
        Ok(()) => a.take_instructions(),
        Err(_) => Vec::new(),
    }
}

/// What kinds of difference (the names before ':')
fn kinds(d: &[String]) -> std::collections::BTreeSet<String> {
    d.iter().map(|s| s.split(':').next().unwrap_or("").to_string()).collect()
}

/// Drops instructions while the same kinds of difference remain (not another failure, such as
/// an x87 stack underflow the removal makes). Kept: the end, returns, the loop counter, string
/// ops' pointers, what defines the flags after a division, bit scan or test, what masks FNSTSW.
fn shrink(mut list: Vec<Instruction>, diffs: &[String], mut run: impl FnMut(&[Instruction]) -> Vec<String>) -> Vec<Instruction> {
    use iced_x86::Mnemonic as M;
    let want = kinds(diffs);
    let mut i = 0;
    while i < list.len() {
        let prev = if i > 0 { list[i - 1].mnemonic() } else { M::Nop };
        let keep = list[i].ip() == 1
            || list[i].code() == Code::Retnd_imm16
            || (list[i].op0_kind() == iced_x86::OpKind::Memory && list[i].memory_base() == Register::EBX && list[i].memory_displacement64() == COUNTER as u64)
            || (list[i].mnemonic() == M::Lea && matches!(list[i].op0_register(), Register::ESI | Register::EDI) && list[i].memory_base() == Register::EBX)
            || (matches!(list[i].mnemonic(), M::Test | M::Setne | M::Sete) && matches!(prev, M::Div | M::Idiv | M::Bsf | M::Bsr | M::Tzcnt | M::Popcnt | M::Sete | M::Test))
            || (list[i].mnemonic() == M::Adc && matches!(prev, M::Bt | M::Bts | M::Btr | M::Btc | M::Mul | M::Imul | M::Shl | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr | M::Shld | M::Shrd))
            || (list[i].mnemonic() == M::And && prev == M::Fnstsw);
        if keep {
            i += 1;
            continue;
        }
        let mut l2 = list.clone();
        l2.remove(i);
        let d = run(&l2);
        if !d.is_empty() && kinds(&d) == want {
            list = l2;
        } else {
            i += 1;
        }
    }
    list
}

/// Programs every run checks
fn fixed() -> Result<Vec<(&'static str, Vec<Instruction>)>, IcedError> {
    let mut v = Vec::new();
    // x87 pushes in a loop (the stack grows across the back edge), popped after it
    let mut a = CodeAssembler::new(32)?;
    let mut top = a.create_label();
    a.mov(dword_ptr(ebx + COUNTER as i32), 4)?;
    a.set_label(&mut top)?;
    a.fld(qword_ptr(ebp + 0x40))?;
    a.fstp(qword_ptr(ebp + 0x48))?;
    a.fldz()?;
    a.fadd(qword_ptr(ebp + 0x50))?;
    a.dec(dword_ptr(ebx + COUNTER as i32))?;
    a.jne(top)?;
    for k in 0..4 {
        a.fstp(qword_ptr(ebx + 0x200 + 8 * k))?;
    }
    a.jmp(END as u64)?;
    v.push(("x87 loop push", a.take_instructions()));
    // and pops in a loop
    let mut a = CodeAssembler::new(32)?;
    let mut top = a.create_label();
    for k in 0..4 {
        a.fld(qword_ptr(ebx + 0x100 + 8 * k))?;
    }
    a.mov(dword_ptr(ebx + COUNTER as i32), 4)?;
    a.set_label(&mut top)?;
    a.fmul(qword_ptr(ebp + 0x40))?;
    a.fstp(qword_ptr(ebx + 0x300))?;
    a.dec(dword_ptr(ebx + COUNTER as i32))?;
    a.jne(top)?;
    a.jmp(END as u64)?;
    v.push(("x87 loop pop", a.take_instructions()));
    Ok(v)
}

/// What a test runs the JIT with
#[derive(Clone, Copy, Debug)]
struct Mode {
    /// function mode; else regions of at most `blocks` blocks
    fns: bool,
    blocks: usize,
    hoist: bool,
    slots: u32,
    /// every instruction translated first (in reverse: nearly every exit chains)
    pre: bool,
}

struct Jit {
    store: Store<()>,
    linker: Linker<()>,
    engine: Engine,
    table: Table,
    run: TypedFunc<i32, i32>,
    mode: Mode,
    known: std::collections::HashSet<u32>,
}

fn hash_insert(base: usize, a: u32, v: u32) {
    let mut h = koji::jit::dispatch::slot(a, HASH_N - 1);
    loop {
        let e = (base + (HASH + 8 * h) as usize) as *mut u32;
        let key = unsafe { e.read() };
        if key == 0 || key == a {
            unsafe {
                e.write(a);
                e.add(1).write(v);
            }
            return;
        }
        h = (h + 1) & (HASH_N - 1);
    }
}

impl Jit {
    fn new(engine: &Engine, dispatch: &Module, memory: &SharedMemory, base: usize, mode: Mode) -> Jit {
        let mut store = Store::new(engine, ());
        let mut linker = Linker::new(engine);
        linker.define(&store, "env", "memory", memory.clone()).unwrap();
        let table = Table::new(&mut store, TableType::new(RefType::FUNCREF, 0, None), Ref::Func(None)).unwrap();
        linker.define(&store, "env", "table", table).unwrap();
        linker
            .func_wrap("env", "step", move |cpu: i32| -> i32 {
                let m = base as *mut u8;
                assert_eq!(cpu as u32, CPU);
                let mut c = unsafe { get_cpu(m) };
                let r = koji::step(&mut c, &mut Mem(m));
                unsafe { put_cpu(m, &c) };
                match r {
                    Exit::Continue => 0,
                    _ => 2,
                }
            })
            .unwrap();
        linker
            .func_wrap("env", "eflags", move |_cpu: i32| -> i32 {
                let m = base as *mut u8;
                let mut c = unsafe { get_cpu(m) };
                c.materialize();
                unsafe { put_cpu(m, &c) };
                c.eflags as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "round", |k: u32, a: f64, b: f64, m: u32| -> f64 {
                let r = koji::round::jit(k, a, b, m);
                if std::env::var("JTRACE").is_ok() {
                    println!("round {k} {:#x} {:#x} {m:#x} -> {:#x}", a.to_bits(), b.to_bits(), r.to_bits());
                }
                r
            })
            .unwrap();
        linker
            .func_wrap("env", "status", |op: i32, a: i32, b: i32, r: i32, c: i32| -> i32 {
                koji::lazy::Lazy { op: op as u32, a: a as u32 as u64, b: b as u32 as u64, r: r as u32 as u64, c: c as u32 }.status() as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "x87", move |_cpu: i32, top: i32, info: i32, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64| {
                let m = base as *mut u8;
                let mut cpu = unsafe { get_cpu(m) };
                koji::jit::x87_out(&mut cpu.fpu, top as u32, info as u32, [a, b, c, d, e, f, g, h]);
                unsafe { put_cpu(m, &cpu) };
            })
            .unwrap();
        // (kura's vnhost_jit_callmiss)
        linker
            .func_wrap("env", "callmiss", move |_cpu: i32, slot: i32, target: i32| -> i32 {
                let target = target as u32;
                let mut h = koji::jit::dispatch::slot(target, HASH_N - 1);
                loop {
                    let e = (base + (HASH + 8 * h) as usize) as *const u32;
                    let key = unsafe { e.read() };
                    if key == 0 {
                        break;
                    }
                    if key == target {
                        let v = unsafe { e.add(1).read() };
                        if v & koji::jit::FN_TAG == 0 || v >> koji::jit::ENTRY_SHIFT & 0x7ff != 0 {
                            break;
                        }
                        unsafe { ((base + slot as u32 as usize) as *mut [u32; 2]).write_unaligned([target, v]) };
                        return v as i32;
                    }
                    h = (h + 1) & (HASH_N - 1);
                }
                let ring = (base + RING as usize) as *mut u32;
                unsafe {
                    let n = *ring;
                    *ring = n.wrapping_add(1);
                    let i = (n & (koji::jit::RING_SIZE - 1)) as usize;
                    *ring.add(1 + 2 * i) = koji::jit::FN_TAG;
                    *ring.add(2 + 2 * i) = target;
                }
                0
            })
            .unwrap();
        let inst = linker.instantiate(&mut store, dispatch).unwrap();
        let run = inst.get_typed_func::<i32, i32>(&mut store, "f0").unwrap();
        unsafe { std::ptr::write_bytes((base + HASH as usize) as *mut u8, 0, (HASH_N * 8) as usize + 0x1000) };
        unsafe { std::ptr::write_bytes((base + CALLS as usize) as *mut u8, 0, SCRATCH_END - CALLS as usize) };
        Jit { store, linker, engine: engine.clone(), table, run, mode, known: Default::default() }
    }

    /// Translates at `eip` (a function in function mode, mostly), into the hash table.
    fn translate(&mut self, base: usize, eip: u32) -> Result<(), String> {
        self.known.insert(eip);
        let mut m = koji::jit::wasm::Module::default();
        let accept = |a: u32| a != END;
        let fns = self.mode.fns && eip % 3 != 1;
        let region = if fns {
            koji::jit::FN.store(true, Relaxed);
            koji::jit::translate_function(&mut m, &mut Mem(base as *mut u8), eip, koji::jit::MAX_BLOCKS, 1000, (END, END + 16), &|_| None, &|_| true, accept)
        } else {
            koji::jit::FN.store(self.mode.fns, Relaxed);
            koji::jit::translate_region(&mut m, &mut Mem(base as *mut u8), &[eip], self.mode.blocks, accept)
        }
        .ok_or("refused")?;
        let bytes = m.finish();
        if std::env::var("JDUMP").is_ok() {
            std::fs::write(format!("/tmp/jit32-{eip:x}.wasm"), &bytes).ok();
        }
        let module = Module::new(&self.engine, &bytes).map_err(|e| {
            std::fs::write("/tmp/jit32-bad.wasm", &bytes).ok();
            format!("invalid module (in /tmp/jit32-bad.wasm): {e:?}")
        })?;
        let inst = self.linker.instantiate(&mut self.store, &module).map_err(|e| format!("{e}"))?;
        let n = if fns { 2 } else { 1 };
        let idx = self.table.size(&self.store) as u32;
        for i in 0..n {
            let f = inst.get_func(&mut self.store, &format!("f{i}")).unwrap();
            self.table.grow(&mut self.store, 1, Ref::Func(Some(f))).unwrap();
        }
        for (i, &(a, k)) in region.starts.iter().enumerate() {
            self.known.insert(a);
            let v = if fns { idx | (i as u32) << koji::jit::ENTRY_SHIFT | koji::jit::FN_TAG } else { idx | k << koji::jit::ENTRY_SHIFT };
            hash_insert(base, a, v);
        }
        Ok(())
    }

    /// Runs from cpu.eip until END: the dispatcher, translations of what it misses, the
    /// interpreter for what they leave to it.
    fn run(&mut self, base: usize) -> Result<(), String> {
        let m = base as *mut u8;
        for _ in 0..200_000 {
            let mut c = unsafe { get_cpu(m) };
            if c.eip == END {
                return Ok(());
            }
            c.jit_hash = HASH;
            c.jit_mask = HASH_N - 1;
            c.jit_ring = RING;
            c.jit_calls = CALLS;
            c.jit_code_pages = PAGES;
            c.jit_budget = 1 << 16;
            unsafe { put_cpu(m, &c) };
            let r = self.run.call(&mut self.store, CPU as i32).map_err(|e| format!("trap: {e:?}"))?;
            let c = unsafe { get_cpu(m) };
            if std::env::var("JTRACE").is_ok() {
                eprintln!("run -> {r} eip {:#x} top {} empty {:#x}", c.eip, c.fpu.top, c.fpu.empty);
            }
            match r {
                0 => {}
                1 if c.eip == END => return Ok(()),
                1 if !self.known.contains(&c.eip) && self.translate(base, c.eip).is_ok() => {}
                1 => {
                    let mut c = c;
                    let r = koji::step(&mut c, &mut Mem(m));
                    unsafe { put_cpu(m, &c) };
                    if r != Exit::Continue {
                        return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
                    }
                }
                koji::jit::RET_FN => {}
                s => return Err(format!("status {s} at {:#x}", c.eip)),
            }
        }
        Err("no end".into())
    }
}

fn interp(base: usize) -> Result<(), String> {
    let m = base as *mut u8;
    let mut c = unsafe { get_cpu(m) };
    for _ in 0..1_000_000 {
        if c.eip == END {
            unsafe { put_cpu(m, &c) };
            return Ok(());
        }
        let r = koji::step(&mut c, &mut Mem(m));
        if r != Exit::Continue {
            unsafe { put_cpu(m, &c) };
            return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
        }
    }
    Err("no end".into())
}

fn initial(rng: &mut Rng) -> Cpu {
    let mut c = Cpu { eip: CODE, ..Cpu::default() };
    for i in 0..8 {
        c.regs[i] = match rng.below(4) {
            0 => rng.below(256) as u32,
            1 => rng.below(64) as u32,
            2 => rng.next() as u32 | 0x8000_0000,
            _ => rng.next() as u32,
        };
    }
    c.regs[3] = DATA; // ebx
    c.regs[4] = STACK - 0x800; // esp
    c.regs[5] = DATA + 0x100; // ebp
    c.eflags = 0x202 | (rng.next() as u32 & 0x8d5);
    for i in 0..8 {
        c.xmm[i] = match rng.below(4) {
            0 => rng.next() as u128 | (rng.next() as u128) << 64,
            1 => (0..4).fold(0, |v, k| v | ((((rng.next() as i32) >> rng.below(31)) as f32 / 8.0).to_bits() as u128) << (32 * k)),
            2 => (0..2).fold(0, |v, k| v | ((((rng.next() as i64) >> rng.below(63)) as f64 / 8.0).to_bits() as u128) << (64 * k)),
            _ => (0..8).fold(0, |v, k| v | ((rng.below(512) as u16).wrapping_sub(256) as u128) << (16 * k)),
        };
        c.mm[i] = rng.next();
    }
    c.mxcsr = 0x1f80 | if rng.below(4) == 0 { (rng.below(4) as u32) << 13 } else { 0 };
    // (the x87's: double, single or extended precision (f64 in koji), any rounding mode)
    if rng.below(3) == 0 {
        c.fpu.cw = [0x027f, 0x007f, 0x037f][rng.below(3) as usize] | (rng.below(4) as u16) << 10;
    }
    c
}

/// DATA: random bytes, and doubles/floats/small integers the x87 and SSE code reads sensibly
fn random_data(rng: &mut Rng) -> Vec<u8> {
    let mut d: Vec<u8> = (0..DATA_LEN).map(|_| rng.next() as u8).collect();
    for k in 0..DATA_LEN as usize / 8 {
        let v: u64 = match rng.below(4) {
            0 => ((rng.next() as i32 >> rng.below(31)) as f64 / 4.0).to_bits(),
            1 => ((rng.next() as i16) as f32 / 2.0).to_bits() as u64 | ((rng.below(100) as f32).to_bits() as u64) << 32,
            2 => rng.below(1000) as u64 | (rng.below(1000) as u64) << 32,
            _ => continue,
        };
        d[k * 8..k * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    d
}

/// The state compared: (registers, flags, FPU, MMX, SSE) as text
fn state(c: &Cpu) -> Vec<(String, String)> {
    let mut c = c.clone();
    c.materialize();
    let mut v = Vec::new();
    for i in 0..8 {
        v.push((format!("r{i}"), format!("{:#x}", c.regs[i])));
    }
    v.push(("eip".into(), format!("{:#x}", c.eip)));
    v.push(("flags".into(), format!("{:#x}", c.eflags & 0x8d5)));
    for i in 0..8 {
        v.push((format!("mm{i}"), format!("{:016x}", c.mm[i])));
        v.push((format!("xmm{i}"), format!("{:032x}", c.xmm[i])));
    }
    let f = &c.fpu;
    v.push(("fpu".into(), format!("top {} empty {:#x} sw {:#x}", f.top, f.empty, f.sw & 0x4700)));
    for i in 0..8 {
        if f.empty & 1 << i == 0 {
            v.push((format!("st r{i}"), format!("{:016x}", f.regs[i].to_bits())));
        }
    }
    v
}

fn run_case(engine: &Engine, dispatch: &Module, memory: &SharedMemory, base: usize, list: &[Instruction], data: &[u8], cpu0: &Cpu, mode: Mode) -> (Vec<String>, Vec<u8>) {
    let m = base as *mut u8;
    let Some(code) = encode(list, CODE as u64) else { return (Vec::new(), Vec::new()) };
    let setup = || unsafe {
        std::ptr::write_bytes(m.add(0x10000), 0, MEM - 0x10000);
        std::ptr::copy_nonoverlapping(code.as_ptr(), m.add(CODE as usize), code.len());
        std::ptr::copy_nonoverlapping(data.as_ptr(), m.add(DATA as usize), data.len());
        put_cpu(m, cpu0);
    };
    let snap = || unsafe { (get_cpu(m), std::slice::from_raw_parts(m.add(DATA as usize), DATA_LEN as usize).to_vec(), std::slice::from_raw_parts(m.add(STACK as usize - 0x1000), 0x1000).to_vec()) };
    setup();
    let want = interp(base);
    let (want_cpu, want_data, want_stack) = snap();
    let mut diffs = Vec::new();
    if want.as_ref().is_err_and(|e| e == "no end") {
        return (diffs, code);
    }
    setup();
    koji::jit::HOIST.store(mode.hoist, Relaxed);
    koji::jit::SLOTS_MAX.store(mode.slots, Relaxed);
    koji::jit::FN.store(mode.fns, Relaxed);
    let mut jit = Jit::new(engine, dispatch, memory, base, mode);
    if mode.pre {
        let mut dec = iced_x86::Decoder::with_ip(32, &code, CODE as u64, iced_x86::DecoderOptions::NONE);
        let ips: Vec<u32> = (&mut dec).into_iter().map(|i| i.ip() as u32).collect();
        for &ip in ips.iter().rev() {
            let _ = jit.translate(base, ip);
        }
    }
    let got = jit.run(base);
    let (got_cpu, got_data, got_stack) = snap();
    if want.is_err() && got.is_err() {
        return (diffs, code); // (both stopped: where may differ)
    }
    if want.is_ok() != got.is_ok() {
        diffs.push(format!("runs: interp {want:?} jit {got:?}"));
    }
    let (w, g): (std::collections::BTreeMap<_, _>, std::collections::BTreeMap<_, _>) = (state(&want_cpu).into_iter().collect(), state(&got_cpu).into_iter().collect());
    for k in w.keys().chain(g.keys().filter(|k| !w.contains_key(*k))) {
        if w.get(k) != g.get(k) {
            diffs.push(format!("{k}: want {:?} got {:?}", w.get(k), g.get(k)));
        }
    }
    if want_data != got_data {
        let i = want_data.iter().zip(&got_data).position(|(a, b)| a != b).unwrap();
        diffs.push(format!("data differs at +{i:#x}"));
    }
    if want_stack != got_stack {
        diffs.push("stack differs".into());
    }
    (diffs, code)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("native") {
        return native::run(&args);
    }
    let count: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500);
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut config = Config::new();
    config.wasm_threads(true);
    let engine = Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, MemoryType::shared(koji::jit::wasm::MEMORY_MIN_PAGES, koji::jit::wasm::MEMORY_MIN_PAGES)).unwrap();
    assert!(memory.data().len() >= SCRATCH_END);
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let dispatch = Module::new(&engine, koji::jit::dispatch::module()).unwrap();
    let mut fails = 0;
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    // fixed programs first, in every mode
    for (name, list) in fixed().unwrap() {
        for fns in [false, true] {
            for blocks in [1, 2, koji::jit::MAX_BLOCKS] {
                let mode = Mode { fns, blocks, hoist: true, slots: 0, pre: false };
                let data = random_data(&mut rng);
                let cpu0 = initial(&mut rng);
                let (diffs, _) = run_case(&engine, &dispatch, &memory, base, &list, &data, &cpu0, mode);
                if !diffs.is_empty() {
                    fails += 1;
                    println!("fixed {name} {mode:?}: {}", diffs.join("; "));
                }
            }
        }
    }
    for t in 0..count {
        let mode = Mode {
            fns: rng.below(2) == 0,
            blocks: rng.pick(&[1, 2, 3, koji::jit::MAX_BLOCKS]),
            hoist: rng.below(2) == 0,
            slots: rng.pick(&[0, 0, 6]),
            pre: rng.below(4) == 0,
        };
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
        let n = 5 + rng.below(40) as usize;
        let body = items(&mut rng, n);
        let mut subs = Vec::new();
        for _ in 0..rng.below(3) {
            let k = 1 + rng.below(8) as usize;
            subs.push(items(&mut rng, k));
        }
        let mut list = structure(&mut rng, body, subs);
        let data = random_data(&mut rng);
        let cpu0 = initial(&mut rng);
        // (JONLY=<test>: that one alone)
        if std::env::var("JONLY").is_ok_and(|o| o != t.to_string()) {
            continue;
        }
        let (diffs, _) = run_case(&engine, &dispatch, &memory, base, &list, &data, &cpu0, mode);
        if !diffs.is_empty() {
            list = shrink(list, &diffs, |l| run_case(&engine, &dispatch, &memory, base, l, &data, &cpu0, mode).0);
        }
        let (diffs, code) = run_case(&engine, &dispatch, &memory, base, &list, &data, &cpu0, mode);
        if !diffs.is_empty() {
            fails += 1;
            if fails <= 10 || std::env::var("JALL").is_ok() {
                println!("test {t} {mode:?}: {}", diffs.join("; "));
                println!("    init: {:x?} flags {:#x} cw {:#x} mxcsr {:#x}", cpu0.regs, cpu0.eflags, cpu0.fpu.cw, cpu0.mxcsr);
                let mut dec = iced_x86::Decoder::with_ip(32, &code, CODE as u64, iced_x86::DecoderOptions::NONE);
                let mut f = iced_x86::FastFormatter::new();
                let mut s = String::new();
                for ins in &mut dec {
                    s.clear();
                    f.format(&ins, &mut s);
                    println!("    {:#x} {s}", ins.ip());
                }
            }
        }
    }
    println!("{count} tests, {fails} failed");
    if fails > 0 {
        std::process::exit(1);
    }
}
