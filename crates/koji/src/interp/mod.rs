//! The reference interpreter: decode one instruction with iced-x86 and
//! execute it. Slow but simple; the JIT is checked against it.

use crate::flags::{self, *};
use crate::{Cpu, Exit, Fault, Memory, ESP, EAX, ECX, EDX, EBP, ESI, EDI};
use iced_x86::{ConditionCode, Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

mod x87;
mod sse;
pub(crate) mod vex;

macro_rules! tryf {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(f) => return Exit::Fault(f),
        }
    };
}

/// Execute one instruction at `cpu.eip`.
pub fn step<M: Memory>(cpu: &mut Cpu, mem: &mut M) -> Exit {
    step_flow(cpu, mem).0
}

/// [`step`], also telling whether the instruction could transfer control (a branch, call,
/// return, interrupt...).
pub fn step_flow<M: Memory>(cpu: &mut Cpu, mem: &mut M) -> (Exit, bool) {
    step_cached(cpu, mem, None)
}

/// Instructions decoded before, by address (direct-mapped), with their bytes: the interpreter
/// runs code below the JIT's threshold dozens of times, and decoding was ~40% of its time.
/// An entry is used only if the bytes at its address are still the same.
pub struct DecodeCache(alloc::boxed::Box<[(u32, [u8; 15], u8, Instruction)]>);

impl DecodeCache {
    const N: usize = 2048;
    pub fn new() -> Self {
        DecodeCache(alloc::vec![(u32::MAX, [0; 15], 0, Instruction::default()); Self::N].into_boxed_slice())
    }
}

impl Default for DecodeCache {
    fn default() -> Self {
        Self::new()
    }
}

/// [`step_flow`] with a [`DecodeCache`].
pub fn step_cached<M: Memory>(cpu: &mut Cpu, mem: &mut M, cache: Option<&mut DecodeCache>) -> (Exit, bool) {
    // (16-bit mode: the linear address of CS:IP)
    let pc = if cpu.mode16 != 0 { cpu.pc16() } else { cpu.eip };
    if pc >= cpu.trap_lo && pc < cpu.trap_hi && cpu.rip_hi == 0 {
        return (Exit::Trap(pc), true);
    }
    cpu.materialize();
    let mut buf = [0u8; 15];
    let n = mem.fetch(pc, &mut buf);
    let slot = cache.map(|c| {
        let i = (pc ^ (pc >> 11)) as usize & (DecodeCache::N - 1);
        &mut c.0[i]
    });
    let ins = match slot {
        Some(e) if e.0 == pc && e.3.ip() >> 32 == cpu.rip_hi as u64 && e.3.ip() as u32 == cpu.eip && (e.2 as usize) <= n && e.1[..e.2 as usize] == buf[..e.2 as usize] => e.3,
        slot => {
            let bitness = if cpu.mode64 != 0 { 64 } else if cpu.mode16 != 0 { 16 } else { 32 };
            let rip = cpu.eip as u64 | (cpu.rip_hi as u64) << 32;
            let mut dec = Decoder::with_ip(bitness, &buf[..n], rip, DecoderOptions::NONE);
            let ins = dec.decode();
            if ins.is_invalid() {
                if n < 15 {
                    return (Exit::Fault(Fault { addr: pc.wrapping_add(n as u32), write: false }), true);
                }
                return (Exit::InvalidOpcode, true);
            }
            // (MOV to or from a control, debug or test register: privileged, and no register
            // of ours; found by fuzz/x86_interp)
            if (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && { let r = ins.op_register(i); r.is_cr() || r.is_dr() || r.is_tr() }) {
                return (Exit::InvalidOpcode, true);
            }
            if let Some(e) = slot {
                *e = (pc, buf, ins.len() as u8, ins);
            }
            ins
        }
    };
    let branch = ins.flow_control() != iced_x86::FlowControl::Next;
    // Only LOCK and XCHG can require atomic memory handling. Most instructions need
    // no operand scan on this hot path.
    let locked_mem = (ins.has_lock_prefix() || ins.mnemonic() == Mnemonic::Xchg)
        && (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory);
    if !locked_mem {
        let mut x = X { cpu, mem, ins: &ins, lock: None };
        return (x.exec(), branch);
    }
    // a locked read-modify-write: its store is a compare-and-swap against what it read, and
    // the instruction runs again if another thread changed the memory in between
    let (regs, hi, r8, eflags) = (cpu.regs, cpu.hi, cpu.r8, cpu.eflags);
    loop {
        let mut x = X { cpu: &mut *cpu, mem: &mut *mem, ins: &ins, lock: Some(Lock::default()) };
        let exit = x.exec();
        if !x.lock.is_some_and(|l| l.lost) {
            return (exit, branch);
        }
        cpu.regs = regs;
        cpu.hi = hi;
        cpu.r8 = r8;
        cpu.eflags = eflags;
        core::hint::spin_loop();
    }
}

/// State of a locked instruction: the value its memory operand read, and whether its store
/// found something else there.
#[derive(Default, Clone, Copy)]
pub(crate) struct Lock {
    seen: u64,
    lost: bool,
}

/// What a long-mode address above 4GB becomes (see [`X::addr`]): the last bytes of the 32-bit
/// space, never guest memory.
#[inline]
fn lin(a: u64, _m64: bool) -> u32 {
    // (modulo 4GB: see Cpu::rip_hi)
    a as u32
}

/// reg_info's index for segment registers: flat 32-bit user mode, they read as Wine's
/// selectors and writes are ignored
const SEG: usize = usize::MAX - 1;

fn selector(r: Register, m64: bool) -> u64 {
    match r {
        Register::CS => if m64 { 0x33 } else { 0x23 },
        Register::FS => 0x53,
        Register::GS => 0,
        _ => 0x2b,
    }
}

/// Index of a segment register in `Cpu::sreg` (ES CS SS DS FS GS); DS for none.
#[inline]
fn sidx(r: Register) -> usize {
    match r {
        Register::ES => 0,
        Register::CS => 1,
        Register::SS => 2,
        Register::FS => 4,
        Register::GS => 5,
        _ => 3,
    }
}

/// Register -> (index, bits, high byte)
#[inline]
fn reg_info(r: Register) -> (usize, u32, bool) {
    if r.is_gpr32() {
        (r.number(), 32, false)
    } else if r.is_gpr64() {
        (r.number(), 64, false)
    } else if r.is_gpr16() {
        (r.number(), 16, false)
    } else if r.is_gpr8() {
        // AL..BL, AH..BH, SPL..DIL, R8L..R15L
        let n = r.number();
        if n < 4 { (n, 8, false) } else if n < 8 { (n - 4, 8, true) } else { (n - 4, 8, false) }
    } else if r.is_segment_register() {
        (SEG, 16, false)
    } else {
        (usize::MAX, 0, false)
    }
}

pub(crate) struct X<'a, M: Memory> {
    pub(crate) cpu: &'a mut Cpu,
    pub(crate) mem: &'a mut M,
    pub(crate) ins: &'a Instruction,
    pub(crate) lock: Option<Lock>,
}

impl<'a, M: Memory> X<'a, M> {
    #[inline]
    pub(crate) fn m64(&self) -> bool {
        self.cpu.mode64 != 0
    }

    #[inline]
    pub(crate) fn m16(&self) -> bool {
        self.cpu.mode16 != 0
    }

    /// 16-bit mode: loads segment register `i` (ES CS SS DS FS GS) with `sel`; its base comes
    /// from the embedder's table at `Cpu::ldt` ([`crate::LDT_REAL`]: real-mode bases, sel * 16).
    pub(crate) fn load_seg(&mut self, i: usize, sel: u16) {
        let base = if self.cpu.ldt == crate::LDT_REAL { (sel as u32) << 4 } else if sel >> 3 == 0 { 0 } else { self.mem.read32(self.cpu.ldt.wrapping_add((sel as u32 >> 3) * 4)).unwrap_or(0) };
        self.cpu.sreg[i] = sel;
        self.cpu.sbase[i] = base;
    }

    /// Linear address of the stack slot at `sp`.
    #[inline]
    fn stk(&self, sp: u64) -> u32 {
        if self.m16() { self.cpu.sbase[2].wrapping_add(sp as u32 & 0xffff) } else { lin(sp, self.m64()) }
    }

    #[inline]
    fn get_reg(&self, r: Register) -> u64 {
        let (i, bits, high) = reg_info(r);
        if i == SEG {
            if self.m16() {
                return self.cpu.sreg[sidx(r)] as u64;
            }
            return selector(r, self.m64());
        }
        if bits == 64 || i >= 8 {
            let v = self.cpu.gpr(i);
            return v & flags::mask64(bits);
        }
        let v = self.cpu.regs[i];
        (match (bits, high) {
            (32, _) => v,
            (16, _) => v & 0xffff,
            (8, false) => v & 0xff,
            _ => (v >> 8) & 0xff,
        }) as u64
    }

    /// Writes a register with its own width (32-bit writes zero the upper half, as in long mode;
    /// in 32-bit mode nothing reads it).
    #[inline]
    fn set_reg(&mut self, r: Register, val: u64) {
        let (i, bits, high) = reg_info(r);
        if i == SEG {
            if self.m16() {
                self.load_seg(sidx(r), val as u16);
            }
            return;
        }
        let old = self.cpu.gpr(i);
        let v = match (bits, high) {
            (64, _) => val,
            (32, _) => val as u32 as u64,
            (16, _) => (old & !0xffff) | (val & 0xffff),
            (8, false) => (old & !0xff) | (val & 0xff),
            _ => (old & !0xff00) | ((val & 0xff) << 8),
        };
        self.cpu.set_gpr(i, v);
    }

    /// Stack pointer and its width.
    #[inline]
    fn sp(&self) -> u64 {
        if self.m64() { self.cpu.gpr(ESP) } else if self.m16() { self.cpu.regs[ESP] as u64 & 0xffff } else { self.cpu.regs[ESP] as u64 }
    }

    #[inline]
    fn set_sp(&mut self, v: u64) {
        if self.m64() {
            self.cpu.set_gpr(ESP, v)
        } else if self.m16() {
            self.cpu.regs[ESP] = (self.cpu.regs[ESP] & 0xffff_0000) | (v as u32 & 0xffff)
        } else {
            self.cpu.regs[ESP] = v as u32
        }
    }

    #[inline]
    fn stack_bits(&self) -> u32 {
        if self.m64() { 64 } else if self.m16() { 16 } else { 32 }
    }

    /// Width of what a CALL / RET / RETF / IRET / ENTER / LEAVE / PUSH sreg / POP sreg moves:
    /// its operand size (66 switches it: `o32 ret` in 16-bit code pops 4 bytes), not the
    /// stack's.
    fn transfer_bits(&self) -> u32 {
        use iced_x86::Code as C;
        match self.ins.code() {
            C::Retnw | C::Retnw_imm16 | C::Retfw | C::Retfw_imm16 | C::Iretw | C::Enterw_imm16_imm8 | C::Leavew | C::Call_rel16 | C::Call_rm16 | C::Call_ptr1616 | C::Call_m1616 => 16,
            C::Pushw_ES | C::Pushw_CS | C::Pushw_SS | C::Pushw_DS | C::Pushw_FS | C::Pushw_GS | C::Popw_ES | C::Popw_SS | C::Popw_DS | C::Popw_FS | C::Popw_GS => 16,
            C::Retnd | C::Retnd_imm16 | C::Retfd | C::Retfd_imm16 | C::Iretd | C::Enterd_imm16_imm8 | C::Leaved | C::Call_rel32_32 | C::Call_rm32 | C::Call_ptr1632 | C::Call_m1632 => 32,
            C::Pushd_ES | C::Pushd_CS | C::Pushd_SS | C::Pushd_DS | C::Pushd_FS | C::Pushd_GS | C::Popd_ES | C::Popd_SS | C::Popd_DS | C::Popd_FS | C::Popd_GS => 32,
            _ => self.stack_bits(),
        }
    }

    #[inline]
    fn sreg_operand(&self) -> bool {
        self.ins.op_kind(0) == OpKind::Register && reg_info(self.ins.op_register(0)).0 == SEG
    }

    /// Address size of the memory operand (what [`Self::offset`] wraps at).
    fn offset_bits(&self) -> u32 {
        let ins = self.ins;
        if ins.memory_base() != Register::None && ins.memory_base() != Register::RIP && ins.memory_base() != Register::EIP {
            reg_info(ins.memory_base()).1
        } else if ins.memory_index() != Register::None {
            reg_info(ins.memory_index()).1
        } else if self.m64() {
            64
        } else if self.m16() && ins.memory_displ_size() < 4 {
            16
        } else {
            32
        }
    }

    /// Effective address (offset only, no segment base): LEA.
    fn offset(&self) -> u64 {
        let ins = self.ins;
        let base = ins.memory_base();
        if base == Register::RIP || base == Register::EIP {
            return ins.memory_displacement64(); // iced resolves RIP-relative to the target
        }
        let index = ins.memory_index();
        let mut a = ins.memory_displacement64();
        let mut asz = if self.m64() { 64 } else if self.m16() && ins.memory_displ_size() < 4 { 16 } else { 32 };
        if base != Register::None {
            asz = reg_info(base).1;
            a = a.wrapping_add(self.get_reg(base));
        }
        if index != Register::None {
            asz = reg_info(index).1;
            a = a.wrapping_add(self.get_reg(index).wrapping_mul(ins.memory_index_scale() as u64));
        }
        a & flags::mask64(asz)
    }

    #[inline]
    fn seg_base(&self) -> u64 {
        if self.m16() {
            return self.cpu.sbase[sidx(self.ins.memory_segment())] as u64;
        }
        (match self.ins.memory_segment() {
            Register::FS => self.cpu.fs_base,
            Register::GS => self.cpu.gs_base,
            _ => 0,
        }) as u64
    }

    /// Linear address of the memory operand: modulo 4GB in long mode (see `Cpu::rip_hi`).
    #[inline]
    fn addr(&self) -> u32 {
        lin(self.offset().wrapping_add(self.seg_base()), self.m64())
    }

    fn op_bits(&self, i: u32) -> u32 {
        match self.ins.op_kind(i) {
            OpKind::Register => reg_info(self.ins.op_register(i)).1,
            OpKind::Memory => self.ins.memory_size().size() as u32 * 8,
            OpKind::Immediate8 => 8,
            OpKind::Immediate16 | OpKind::Immediate8to16 => 16,
            OpKind::Immediate32to64 | OpKind::Immediate8to64 | OpKind::Immediate64 => 64,
            _ => 32,
        }
    }

    fn read_mem(&mut self, addr: u32, bits: u32) -> Result<u64, Fault> {
        let v = match bits {
            8 => self.mem.read8(addr).map(|v| v as u64),
            16 => self.mem.read16(addr).map(|v| v as u64),
            64 => self.mem.read64(addr),
            _ => self.mem.read32(addr).map(|v| v as u64),
        }?;
        if let Some(l) = &mut self.lock {
            l.seen = v;
        }
        Ok(v)
    }

    fn write_mem(&mut self, addr: u32, bits: u32, v: u64) -> Result<(), Fault> {
        if let Some(l) = self.lock {
            let cur = self.mem.cmpxchg(addr, bits / 8, l.seen, v & flags::mask64(bits))?;
            if cur != l.seen {
                self.lock = Some(Lock { lost: true, ..l });
            }
            return Ok(());
        }
        match bits {
            8 => self.mem.write8(addr, v as u8),
            16 => self.mem.write16(addr, v as u16),
            64 => self.mem.write64(addr, v),
            _ => self.mem.write32(addr, v as u32),
        }
    }

    fn read_op(&mut self, i: u32) -> Result<u64, Fault> {
        match self.ins.op_kind(i) {
            OpKind::Register => Ok(self.get_reg(self.ins.op_register(i))),
            OpKind::Memory => {
                let bits = self.op_bits(i);
                let a = self.addr();
                self.read_mem(a, bits)
            }
            _ => Ok(self.ins.immediate(i) & flags::mask64(self.op_bits(i))),
        }
    }

    /// Write operand `i` (register or memory) with its own width.
    fn write_op(&mut self, i: u32, v: u64) -> Result<(), Fault> {
        match self.ins.op_kind(i) {
            OpKind::Register => {
                self.set_reg(self.ins.op_register(i), v);
                Ok(())
            }
            _ => {
                let bits = self.op_bits(i);
                let a = self.addr();
                self.write_mem(a, bits, v)
            }
        }
    }

    #[inline]
    fn set_status(&mut self, f: u32) {
        self.cpu.eflags = (self.cpu.eflags & !STATUS) | (f & STATUS);
    }

    /// Replace only the flags in `which`.
    #[inline]
    fn set_flags(&mut self, which: u32, f: u32) {
        self.cpu.eflags = (self.cpu.eflags & !which) | (f & which);
    }

    #[inline]
    fn cf(&self) -> u32 {
        self.cpu.eflags & CF
    }

    fn push(&mut self, v: u64, bits: u32) -> Result<(), Fault> {
        let sp = self.sp().wrapping_sub(bits as u64 / 8);
        self.write_mem(self.stk(sp), bits, v)?;
        self.set_sp(sp);
        Ok(())
    }

    fn pop(&mut self, bits: u32) -> Result<u64, Fault> {
        let sp = self.sp();
        let v = self.read_mem(self.stk(sp), bits)?;
        self.set_sp(sp.wrapping_add(bits as u64 / 8));
        Ok(v)
    }

    #[inline]
    fn next(&self) -> u64 {
        if self.m64() { self.ins.next_ip() } else if self.m16() { self.ins.next_ip16() as u64 } else { self.ins.next_ip32() as u64 }
    }

    #[inline]
    fn branch(&self) -> u64 {
        if self.m64() {
            self.ins.near_branch_target()
        } else if self.m16() {
            self.ins.near_branch_target() & 0xffff
        } else {
            self.ins.near_branch32() as u64
        }
    }

    fn done(&mut self) -> Exit {
        self.cpu.eip = self.next() as u32;
        Exit::Continue
    }

    fn jump(&mut self, target: u64) -> Exit {
        if self.m16() {
            self.cpu.eip = target as u32 & 0xffff;
            return Exit::Continue;
        }
        self.cpu.eip = target as u32;
        self.cpu.rip_hi = (target >> 32) as u32;
        Exit::Continue
    }

    /// A far JMP/CALL (ptr16:16, or m16:16 / m16:32)?
    fn far(&self) -> bool {
        match self.ins.op_kind(0) {
            OpKind::FarBranch16 | OpKind::FarBranch32 => true,
            OpKind::Memory => matches!(self.ins.memory_size(), iced_x86::MemorySize::SegPtr16 | iced_x86::MemorySize::SegPtr32),
            _ => false,
        }
    }

    /// 16-bit mode: far JMP/CALL.
    fn far_transfer(&mut self) -> Exit {
        let ins = self.ins;
        let (sel, ip, w) = match ins.op_kind(0) {
            OpKind::FarBranch16 => (ins.far_branch_selector(), ins.far_branch16() as u64, 16),
            OpKind::FarBranch32 => (ins.far_branch_selector(), ins.far_branch32() as u64, 32),
            _ => {
                let w = if ins.memory_size() == iced_x86::MemorySize::SegPtr32 { 32 } else { 16 };
                let a = self.addr();
                let ip = tryf!(self.read_mem(a, w));
                let sel = tryf!(self.read_mem(a.wrapping_add(w / 8), 16)) as u16;
                (sel, ip, w)
            }
        };
        if ins.mnemonic() == Mnemonic::Call {
            let cs = self.cpu.sreg[1] as u64;
            let ret = self.next();
            tryf!(self.push(cs, w));
            tryf!(self.push(ret, w));
        }
        self.load_seg(1, sel);
        self.jump(ip)
    }

    fn cc(&self) -> bool {
        let cc = match self.ins.condition_code() {
            ConditionCode::o => 0,
            ConditionCode::no => 1,
            ConditionCode::b => 2,
            ConditionCode::ae => 3,
            ConditionCode::e => 4,
            ConditionCode::ne => 5,
            ConditionCode::be => 6,
            ConditionCode::a => 7,
            ConditionCode::s => 8,
            ConditionCode::ns => 9,
            ConditionCode::p => 10,
            ConditionCode::np => 11,
            ConditionCode::l => 12,
            ConditionCode::ge => 13,
            ConditionCode::le => 14,
            ConditionCode::g => 15,
            ConditionCode::None => return true,
        };
        flags::cond(self.cpu.eflags, cc)
    }

    fn exec(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        if ins.encoding() == iced_x86::EncodingKind::VEX {
            return self.vex();
        }
        match ins.mnemonic() {
            Nop | Pause | Wait | Endbr32 | Endbr64 | Prefetchnta | Prefetcht0 | Prefetcht1 | Prefetcht2 | Prefetchw
            | Lfence | Mfence | Sfence => self.done(),
            // CET shadow stack instructions are NOPs without CET (RDSSP leaves its register, so
            // code sees no shadow stack)
            Rdsspd | Rdsspq | Incsspd | Incsspq => self.done(),

            Mov | Movzx => {
                let v = tryf!(self.read_op(1));
                tryf!(self.write_op(0, v));
                self.done()
            }
            Movsx | Movsxd => {
                let bits = self.op_bits(1);
                let v = tryf!(self.read_op(1));
                let v = match bits {
                    8 => v as u8 as i8 as i64 as u64,
                    16 => v as u16 as i16 as i64 as u64,
                    _ => v as u32 as i32 as i64 as u64,
                };
                tryf!(self.write_op(0, v));
                self.done()
            }
            Lea => {
                let v = self.offset();
                tryf!(self.write_op(0, v));
                self.done()
            }
            Xchg => {
                let a = tryf!(self.read_op(0));
                let b = tryf!(self.read_op(1));
                tryf!(self.write_op(0, b));
                tryf!(self.write_op(1, a));
                self.done()
            }
            Bswap => {
                let v = tryf!(self.read_op(0));
                let r = if self.op_bits(0) == 64 { v.swap_bytes() } else { (v as u32).swap_bytes() as u64 };
                tryf!(self.write_op(0, r));
                self.done()
            }

            Add | Adc | Sub | Sbb | Cmp | And | Or | Xor | Test => {
                let bits = self.op_bits(0);
                let a = tryf!(self.read_op(0));
                let b = tryf!(self.read_op(1));
                let (r, f) = match ins.mnemonic() {
                    Add => flags::add64(a, b, 0, bits),
                    Adc => flags::add64(a, b, self.cf(), bits),
                    Sub | Cmp => flags::sub64(a, b, 0, bits),
                    Sbb => flags::sub64(a, b, self.cf(), bits),
                    And | Test => (a & b, flags::szp64(a & b, bits)),
                    Or => (a | b, flags::szp64(a | b, bits)),
                    _ => (a ^ b, flags::szp64(a ^ b, bits)),
                };
                if !matches!(ins.mnemonic(), Cmp | Test) {
                    tryf!(self.write_op(0, r));
                }
                self.set_status(f);
                self.done()
            }
            Inc | Dec => {
                let bits = self.op_bits(0);
                let a = tryf!(self.read_op(0));
                let (r, f) = if ins.mnemonic() == Inc { flags::add64(a, 1, 0, bits) } else { flags::sub64(a, 1, 0, bits) };
                tryf!(self.write_op(0, r));
                self.set_flags(STATUS & !CF, f);
                self.done()
            }
            Neg => {
                let bits = self.op_bits(0);
                let a = tryf!(self.read_op(0));
                let (r, f) = flags::sub64(0, a, 0, bits);
                tryf!(self.write_op(0, r));
                self.set_status(f);
                self.done()
            }
            Not => {
                let a = tryf!(self.read_op(0));
                tryf!(self.write_op(0, !a));
                self.done()
            }

            Shl | Sal | Shr | Sar | Rol | Ror | Rcl | Rcr => self.shift(),
            Shld | Shrd => self.shift_double(),

            Mul | Imul if ins.op_count() == 1 => self.mul1(),
            Imul => {
                let bits = self.op_bits(0);
                let (a, b) = if ins.op_count() == 3 {
                    (tryf!(self.read_op(1)), tryf!(self.read_op(2)))
                } else {
                    (tryf!(self.read_op(0)), tryf!(self.read_op(1)))
                };
                let sx = |v: u64| -> i128 {
                    match bits {
                        16 => v as u16 as i16 as i128,
                        32 => v as u32 as i32 as i128,
                        _ => v as i64 as i128,
                    }
                };
                let full = sx(a) * sx(b);
                let r = full as u64 & flags::mask64(bits);
                let ovf = sx(r) != full;
                tryf!(self.write_op(0, r));
                let f = flags::szp64(r, bits) | if ovf { CF | OF } else { 0 };
                self.set_status(f);
                self.done()
            }
            Div | Idiv => self.div(),

            Cbw => {
                let v = self.get_reg(Register::AL) as u8 as i8 as i16 as u16 as u64;
                self.set_reg(Register::AX, v);
                self.done()
            }
            Cwde => {
                let v = self.get_reg(Register::AX) as u16 as i16 as i32 as u32 as u64;
                self.set_reg(Register::EAX, v);
                self.done()
            }
            Cdqe => {
                let v = self.get_reg(Register::EAX) as u32 as i32 as i64 as u64;
                self.set_reg(Register::RAX, v);
                self.done()
            }
            Cwd => {
                let hi = if self.get_reg(Register::AX) & 0x8000 != 0 { 0xffff } else { 0 };
                self.set_reg(Register::DX, hi);
                self.done()
            }
            Cdq => {
                let v = ((self.get_reg(Register::EAX) as u32 as i32) >> 31) as u32 as u64;
                self.set_reg(Register::EDX, v);
                self.done()
            }
            Cqo => {
                let v = ((self.get_reg(Register::RAX) as i64) >> 63) as u64;
                self.set_reg(Register::RDX, v);
                self.done()
            }

            Push => {
                let bits = if self.sreg_operand() {
                    self.transfer_bits()
                } else if ins.op_kind(0) == OpKind::Register || ins.op_kind(0) == OpKind::Memory {
                    self.op_bits(0)
                } else {
                    ins.stack_pointer_increment().unsigned_abs() * 8
                };
                let v = tryf!(self.read_op(0));
                tryf!(self.push(v, bits));
                self.done()
            }
            Pop => {
                let bits = if self.sreg_operand() { self.transfer_bits() } else { self.op_bits(0) };
                let sp = self.sp();
                let v = tryf!(self.read_mem(self.stk(sp), bits));
                self.set_sp(sp.wrapping_add(bits as u64 / 8));
                // POP [ESP+x] uses ESP after the increment
                if let Err(f) = self.write_op(0, v) {
                    self.set_sp(sp);
                    return Exit::Fault(f);
                }
                self.done()
            }
            Pushad => {
                let sp = self.sp();
                let r = self.cpu.regs;
                for (k, &v) in [r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]].iter().enumerate() {
                    tryf!(self.mem.write32(self.stk(sp.wrapping_sub(4 * (k as u64 + 1))), v));
                }
                self.set_sp(sp.wrapping_sub(32));
                self.done()
            }
            Popad => {
                let sp = self.sp();
                let mut v = [0u32; 8];
                for (k, slot) in v.iter_mut().enumerate() {
                    *slot = tryf!(self.mem.read32(self.stk(sp.wrapping_add(4 * k as u64))));
                }
                // memory order: EDI ESI EBP (ESP) EBX EDX ECX EAX
                self.cpu.regs[EDI] = v[0];
                self.cpu.regs[ESI] = v[1];
                self.cpu.regs[EBP] = v[2];
                self.cpu.regs[3] = v[4];
                self.cpu.regs[EDX] = v[5];
                self.cpu.regs[ECX] = v[6];
                self.cpu.regs[EAX] = v[7];
                self.set_sp(sp.wrapping_add(32));
                self.done()
            }
            // PUSHA/POPA with an operand-size prefix (66 60 / 66 61): the low 16 bits only
            Pusha => {
                let sp = self.sp();
                let r = self.cpu.regs;
                for (k, &v) in [r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]].iter().enumerate() {
                    tryf!(self.mem.write16(self.stk(sp.wrapping_sub(2 * (k as u64 + 1))), v as u16));
                }
                self.set_sp(sp.wrapping_sub(16));
                self.done()
            }
            Popa => {
                let sp = self.sp();
                let mut v = [0u16; 8];
                for (k, slot) in v.iter_mut().enumerate() {
                    *slot = tryf!(self.mem.read16(self.stk(sp.wrapping_add(2 * k as u64))));
                }
                for (reg, k) in [(EDI, 0), (ESI, 1), (EBP, 2), (3, 4), (EDX, 5), (ECX, 6), (EAX, 7)] {
                    self.cpu.regs[reg] = (self.cpu.regs[reg] & 0xffff_0000) | v[k] as u32;
                }
                self.set_sp(sp.wrapping_add(16));
                self.done()
            }
            Pushfd | Pushf | Pushfq => {
                let bits = match ins.mnemonic() { Pushf => 16, Pushfq => 64, _ => 32 };
                let v = self.cpu.eflags & !(0x3 << 16); // VM and RF read as 0
                tryf!(self.push(v as u64, bits));
                self.done()
            }
            Popfd | Popf | Popfq => {
                let bits = match ins.mnemonic() { Popf => 16, Popfq => 64, _ => 32 };
                let v = tryf!(self.pop(bits)) as u32;
                let which = USER_WRITABLE & flags::mask(bits);
                self.cpu.eflags = (self.cpu.eflags & !which) | (v & which) | 2;
                self.done()
            }
            Lahf => {
                let v = (self.cpu.eflags & (SF | ZF | AF | PF | CF)) | 2;
                self.set_reg(Register::AH, v as u64);
                self.done()
            }
            Sahf => {
                let v = self.get_reg(Register::AH) as u32;
                self.set_flags(SF | ZF | AF | PF | CF, v);
                self.done()
            }
            Clc => { self.cpu.eflags &= !CF; self.done() }
            Stc => { self.cpu.eflags |= CF; self.done() }
            Cmc => { self.cpu.eflags ^= CF; self.done() }
            Cld => { self.cpu.eflags &= !DF; self.done() }
            Std => { self.cpu.eflags |= DF; self.done() }

            // BCD adjustments (32-bit mode only; the flags the SDM leaves undefined come out 0)
            Daa | Das => {
                let ax = self.cpu.regs[EAX];
                let (al, cf, af) = (ax & 0xff, self.cf() != 0, self.cpu.eflags & AF != 0);
                let mut r = al;
                let (mut ncf, mut naf) = (false, false);
                if al & 0xf > 9 || af {
                    naf = true;
                    if ins.mnemonic() == Daa {
                        ncf = cf || al + 6 > 0xff;
                        r = (r + 6) & 0xff;
                    } else {
                        ncf = cf || al < 6;
                        r = r.wrapping_sub(6) & 0xff;
                    }
                }
                if al > 0x99 || cf {
                    r = if ins.mnemonic() == Daa { r + 0x60 } else { r.wrapping_sub(0x60) } & 0xff;
                    ncf = true;
                } else if ins.mnemonic() == Daa {
                    ncf = false;
                }
                self.cpu.regs[EAX] = (ax & !0xff) | r;
                self.set_status(szp(r, 8) | if ncf { CF } else { 0 } | if naf { AF } else { 0 });
                self.done()
            }
            Aaa | Aas => {
                let ax = self.cpu.regs[EAX];
                let adjust = ax & 0xf > 9 || self.cpu.eflags & AF != 0;
                let mut v = ax & 0xffff;
                if adjust {
                    v = if ins.mnemonic() == Aaa {
                        v.wrapping_add(0x106)
                    } else {
                        v.wrapping_sub(6).wrapping_sub(0x100)
                    } & 0xffff;
                }
                v &= 0xff0f;
                self.cpu.regs[EAX] = (ax & !0xffff) | v;
                self.set_status(szp(v & 0xff, 8) | if adjust { CF | AF } else { 0 });
                self.done()
            }
            Aam => {
                let base = ins.immediate8() as u32;
                if base == 0 { return Exit::DivideError; }
                let ax = self.cpu.regs[EAX];
                let al = ax & 0xff;
                let (q, r) = (al / base, al % base);
                self.cpu.regs[EAX] = (ax & !0xffff) | q << 8 | r;
                self.set_status(szp(r, 8));
                self.done()
            }
            Aad => {
                let base = ins.immediate8() as u32;
                let ax = self.cpu.regs[EAX];
                let r = ((ax & 0xff) + ((ax >> 8) & 0xff) * base) & 0xff;
                self.cpu.regs[EAX] = (ax & !0xffff) | r;
                self.set_status(szp(r, 8));
                self.done()
            }

            Enter => {
                let size = ins.immediate16() as u64;
                let level = ins.immediate8_2nd() as u64 & 31;
                let (w, sb) = (self.transfer_bits(), self.transfer_bits() as u64 / 8);
                let m64 = self.m64();
                let rbp = if m64 { Register::RBP } else if w == 16 { Register::BP } else { Register::EBP };
                let bp = self.get_reg(rbp);
                let mut sp = self.sp().wrapping_sub(sb);
                tryf!(self.write_mem(self.stk(sp), w, bp));
                let frame = sp;
                for l in 1..level {
                    let v = tryf!(self.read_mem(self.stk(bp.wrapping_sub(sb * l)), w));
                    sp = sp.wrapping_sub(sb);
                    tryf!(self.write_mem(self.stk(sp), w, v));
                }
                if level > 0 {
                    sp = sp.wrapping_sub(sb);
                    tryf!(self.write_mem(self.stk(sp), w, frame));
                }
                self.set_reg(rbp, frame);
                self.set_sp(sp.wrapping_sub(size));
                self.done()
            }
            Leave => {
                let m64 = self.m64();
                let w = self.transfer_bits();
                let rbp = if m64 { Register::RBP } else if w == 16 { Register::BP } else { Register::EBP };
                // (the stack's width picks SP or ESP, the operand size BP or EBP)
                let sp_from = if m64 { Register::RBP } else if self.m16() { Register::BP } else { Register::EBP };
                let bp = self.get_reg(sp_from);
                let v = tryf!(self.read_mem(self.stk(bp), w));
                self.set_sp(bp.wrapping_add(w as u64 / 8));
                self.set_reg(rbp, v);
                self.done()
            }

            Jmp | Call if self.m16() && self.far() => self.far_transfer(),
            Retf if self.m16() => {
                let w = self.transfer_bits();
                let ip = tryf!(self.pop(w));
                let cs = tryf!(self.pop(w));
                let extra = if ins.op_count() == 1 { ins.immediate16() as u64 } else { 0 };
                let sp = self.sp();
                self.set_sp(sp.wrapping_add(extra));
                self.load_seg(1, cs as u16);
                self.jump(ip)
            }
            Iret | Iretd if self.m16() => {
                let w = self.transfer_bits();
                let ip = tryf!(self.pop(w));
                let cs = tryf!(self.pop(w));
                let f = tryf!(self.pop(w)) as u32;
                let which = USER_WRITABLE & if w == 16 { 0xffff } else { !0 };
                self.cpu.eflags = (self.cpu.eflags & !which) | (f & which) | 2;
                self.load_seg(1, cs as u16);
                self.jump(ip)
            }
            Lds | Les | Lss | Lfs | Lgs => {
                let bits = self.op_bits(0);
                let a = self.addr();
                let off = tryf!(self.read_mem(a, bits));
                let sel = tryf!(self.read_mem(a.wrapping_add(bits / 8), 16));
                if self.m16() {
                    let i = match ins.mnemonic() { Les => 0, Lss => 2, Lds => 3, Lfs => 4, _ => 5 };
                    self.load_seg(i, sel as u16);
                }
                self.set_reg(ins.op_register(0), off);
                self.done()
            }
            Jmp => match ins.op_kind(0) {
                OpKind::NearBranch32 | OpKind::NearBranch16 | OpKind::NearBranch64 => self.jump(self.branch()),
                _ => {
                    let t = tryf!(self.read_op(0));
                    self.jump(t)
                }
            },
            Call => {
                let t = match ins.op_kind(0) {
                    OpKind::NearBranch32 | OpKind::NearBranch16 | OpKind::NearBranch64 => self.branch(),
                    _ => tryf!(self.read_op(0)),
                };
                let ret = self.next();
                tryf!(self.push(ret, self.transfer_bits()));
                self.jump(t)
            }
            Ret => {
                let sp = self.sp();
                let w = self.transfer_bits();
                let t = tryf!(self.read_mem(self.stk(sp), w));
                let extra = if ins.op_count() == 1 { ins.immediate16() as u64 } else { 0 };
                self.set_sp(sp.wrapping_add(w as u64 / 8 + extra));
                self.jump(t)
            }
            Ja | Jae | Jb | Jbe | Je | Jg | Jge | Jl | Jle | Jne | Jno | Jnp | Jns | Jo | Jp | Js => {
                if self.cc() { self.jump(self.branch()) } else { self.done() }
            }
            Jecxz | Jcxz | Jrcxz => {
                let c = match ins.mnemonic() {
                    Jcxz => self.get_reg(Register::CX),
                    Jecxz => self.get_reg(Register::ECX),
                    _ => self.get_reg(Register::RCX),
                };
                if c == 0 { self.jump(self.branch()) } else { self.done() }
            }
            Loop | Loope | Loopne => {
                use iced_x86::Code as C;
                let r = match ins.code() {
                    C::Loop_rel8_16_CX | C::Loop_rel8_32_CX | C::Loope_rel8_16_CX | C::Loope_rel8_32_CX
                    | C::Loopne_rel8_16_CX | C::Loopne_rel8_32_CX => Register::CX,
                    C::Loop_rel8_16_RCX | C::Loop_rel8_64_RCX | C::Loope_rel8_16_RCX | C::Loope_rel8_64_RCX
                    | C::Loopne_rel8_16_RCX | C::Loopne_rel8_64_RCX => Register::RCX,
                    _ => Register::ECX,
                };
                let c = self.get_reg(r).wrapping_sub(1) & flags::mask64(reg_info(r).1);
                self.set_reg(r, c);
                let zf = self.cpu.eflags & ZF != 0;
                let take = c != 0
                    && match ins.mnemonic() {
                        Loope => zf,
                        Loopne => !zf,
                        _ => true,
                    };
                if take { self.jump(self.branch()) } else { self.done() }
            }
            Seta | Setae | Setb | Setbe | Sete | Setg | Setge | Setl | Setle | Setne | Setno | Setnp | Setns
            | Seto | Setp | Sets => {
                let v = self.cc() as u64;
                tryf!(self.write_op(0, v));
                self.done()
            }
            Cmova | Cmovae | Cmovb | Cmovbe | Cmove | Cmovg | Cmovge | Cmovl | Cmovle | Cmovne | Cmovno
            | Cmovnp | Cmovns | Cmovo | Cmovp | Cmovs => {
                // the source is read (and may fault) even if the condition is false; a 32-bit
                // destination is zero-extended either way in long mode
                let v = tryf!(self.read_op(1));
                if self.cc() {
                    tryf!(self.write_op(0, v));
                } else if self.op_bits(0) == 32 && self.m64() {
                    let d = self.get_reg(ins.op_register(0));
                    self.set_reg(ins.op_register(0), d);
                }
                self.done()
            }

            Bt | Bts | Btr | Btc => self.bit_test(),
            Bsf | Bsr => {
                let v = tryf!(self.read_op(1));
                if v == 0 {
                    self.cpu.eflags |= ZF;
                } else {
                    let r = if ins.mnemonic() == Bsf { v.trailing_zeros() } else { 63 - v.leading_zeros() };
                    tryf!(self.write_op(0, r as u64));
                    self.cpu.eflags &= !ZF;
                }
                self.done()
            }
            Tzcnt | Lzcnt | Popcnt => {
                let bits = self.op_bits(0);
                let v = tryf!(self.read_op(1)) & flags::mask64(bits);
                let r = match ins.mnemonic() {
                    Popcnt => v.count_ones(),
                    Tzcnt => if v == 0 { bits } else { v.trailing_zeros() },
                    _ => v.leading_zeros() - (64 - bits),
                };
                tryf!(self.write_op(0, r as u64));
                let f = match ins.mnemonic() {
                    Popcnt => if v == 0 { ZF } else { 0 },
                    _ => (if r == 0 { ZF } else { 0 }) | (if v == 0 || (ins.mnemonic() == Lzcnt && r == bits) { CF } else { 0 }),
                };
                self.set_status(f);
                self.done()
            }

            // SSE4.2 CRC32 (Castagnoli, bit-reflected): the source's bytes from the lowest; a 32-bit
            // result, zero-extended into a 64-bit destination; no flags
            Crc32 => {
                let n = self.op_bits(1) / 8;
                let v = tryf!(self.read_op(1));
                let crc = (0..n).fold(self.get_reg(ins.op0_register()) as u32, |c, i| crc32c_byte(c, (v >> (8 * i)) as u8));
                tryf!(self.write_op(0, crc as u64));
                self.done()
            }

            Cmpxchg => {
                let bits = self.op_bits(0);
                let d = tryf!(self.read_op(0));
                let acc_reg = match bits { 8 => Register::AL, 16 => Register::AX, 32 => Register::EAX, _ => Register::RAX };
                let acc = self.get_reg(acc_reg);
                let (_, f) = flags::sub64(acc, d, 0, bits);
                if acc == d {
                    let s = tryf!(self.read_op(1));
                    tryf!(self.write_op(0, s));
                } else {
                    // a memory destination is written back unchanged (a register one is not
                    // written at all: no zero-extension in long mode)
                    if ins.op_kind(0) == OpKind::Memory {
                        tryf!(self.write_op(0, d));
                    }
                    self.set_reg(acc_reg, d);
                }
                self.set_status(f);
                self.done()
            }
            Cmpxchg8b => {
                let a = self.addr();
                let cmp = (self.cpu.regs[EDX] as u64) << 32 | self.cpu.regs[EAX] as u64;
                let n = (self.cpu.regs[ECX] as u64) << 32 | self.cpu.regs[3] as u64;
                // always atomic: a plain store of the unchanged value could undo another
                // thread's write
                let d = tryf!(self.mem.cmpxchg(a, 8, cmp, n));
                if d == cmp {
                    self.cpu.eflags |= ZF;
                } else {
                    self.set_reg(Register::EAX, d as u32 as u64);
                    self.set_reg(Register::EDX, d >> 32);
                    self.cpu.eflags &= !ZF;
                }
                self.done()
            }
            Cmpxchg16b => {
                let a = self.addr();
                let cmp = self.cpu.gpr(EAX) as u128 | (self.cpu.gpr(EDX) as u128) << 64;
                let n = self.cpu.gpr(3) as u128 | (self.cpu.gpr(ECX) as u128) << 64;
                let d = tryf!(self.mem.cmpxchg16(a, cmp, n));
                if d == cmp {
                    self.cpu.eflags |= ZF;
                } else {
                    self.cpu.set_gpr(EAX, d as u64);
                    self.cpu.set_gpr(EDX, (d >> 64) as u64);
                    self.cpu.eflags &= !ZF;
                }
                self.done()
            }
            Xadd => {
                let bits = self.op_bits(0);
                let d = tryf!(self.read_op(0));
                let s = tryf!(self.read_op(1));
                let (r, f) = flags::add64(d, s, 0, bits);
                // DEST is written last (XADD r, r with the same register)
                if ins.op_kind(0) == OpKind::Memory {
                    tryf!(self.write_op(0, r));
                    tryf!(self.write_op(1, d));
                } else {
                    tryf!(self.write_op(1, d));
                    tryf!(self.write_op(0, r));
                }
                self.set_status(f);
                self.done()
            }

            Movsb | Movsw | Movsd | Movsq | Stosb | Stosw | Stosd | Stosq | Lodsb | Lodsw | Lodsd | Lodsq | Cmpsb
            | Cmpsw | Cmpsd | Cmpsq | Scasb | Scasw | Scasd | Scasq
                if ins.op_count() == 0
                    || matches!(ins.op_kind(0), OpKind::MemoryESEDI | OpKind::MemorySegESI | OpKind::MemoryESRDI
                        | OpKind::MemorySegRSI | OpKind::MemoryESDI | OpKind::MemorySegSI)
                    || (ins.op_kind(0) == OpKind::Register && !ins.op_register(0).is_xmm()) =>
            {
                self.string_op()
            }
            Xlatb => {
                let m64 = self.m64() && ins.memory_base() == Register::RBX;
                let b = if m64 { self.cpu.gpr(3) } else { self.cpu.regs[3] as u64 };
                let a = b.wrapping_add(self.get_reg(Register::AL)) & if m64 { u64::MAX } else if self.m16() { 0xffff } else { 0xffff_ffff };
                let v = tryf!(self.mem.read8(lin(a.wrapping_add(self.seg_base()), self.m64())));
                self.set_reg(Register::AL, v as u64);
                self.done()
            }

            Cpuid => {
                let (a, b, c, d) = cpuid_mode(self.cpu.regs[EAX], self.cpu.regs[ECX], self.m64());
                self.set_reg(Register::EAX, a as u64);
                self.set_reg(Register::EBX, b as u64);
                self.set_reg(Register::ECX, c as u64);
                self.set_reg(Register::EDX, d as u64);
                self.done()
            }
            Rdtsc => {
                self.set_reg(Register::EAX, self.cpu.tsc as u32 as u64);
                self.set_reg(Register::EDX, self.cpu.tsc >> 32);
                self.done()
            }
            Int3 => {
                self.cpu.eip = self.next() as u32;
                Exit::Interrupt(3)
            }
            Int => {
                self.cpu.eip = self.next() as u32;
                Exit::Interrupt(ins.immediate8())
            }
            // INTO: INT 4 when OF is set (a trap: EIP past it)
            Into => {
                if self.cpu.flags() & OF == 0 {
                    return self.done();
                }
                self.cpu.eip = self.next() as u32;
                Exit::Interrupt(4)
            }
            // BOUND: #BR (vector 5, a fault: EIP stays) when the index is outside
            // [lower, upper], signed, both read from the memory operand
            Bound => {
                let bits = self.op_bits(0);
                let a = self.addr();
                let lo = tryf!(self.read_mem(a, bits));
                let hi = tryf!(self.read_mem(a.wrapping_add(bits / 8), bits));
                let v = self.get_reg(ins.op_register(0));
                let sx = |x: u64| if bits == 16 { x as u16 as i16 as i64 } else { x as u32 as i32 as i64 };
                if sx(v) < sx(lo) || sx(v) > sx(hi) {
                    return Exit::Interrupt(5);
                }
                self.done()
            }
            // SALC (undocumented, D6): AL = CF ? FF : 0
            Salc => {
                let v = if self.cf() != 0 { 0xff } else { 0 };
                self.set_reg(Register::AL, v);
                self.done()
            }
            Syscall => {
                // long mode SYSCALL: RCX = return address, R11 = RFLAGS; the embedder sees it as
                // a trap at the instruction after it
                self.cpu.set_gpr(ECX, self.next());
                self.cpu.set_gpr(11, self.cpu.flags() as u64);
                self.cpu.eip = self.next() as u32;
                Exit::Syscall
            }
            Hlt => Exit::Halt,
            _ => {
                use iced_x86::CpuidFeature as F;
                let feats = ins.cpuid_features();
                if feats.iter().any(|f| matches!(f, F::FPU | F::FPU287 | F::FPU387 | F::FPU287XL_ONLY | F::FPU387SL_ONLY)) {
                    self.x87()
                } else {
                    self.sse()
                }
            }
        }
    }

    fn shift(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let bits = self.op_bits(0);
        let m = flags::mask64(bits);
        let a = tryf!(self.read_op(0)) & m;
        let cmask = if bits == 64 { 0x3f } else { 0x1f };
        let count = if ins.op_count() > 1 { (tryf!(self.read_op(1)) & cmask) as u32 } else { 1 };
        let sb = flags::sign64(bits);
        let eff = match ins.mnemonic() {
            Rcl | Rcr => count % (bits + 1),
            Rol | Ror => count % bits,
            _ => count,
        };
        if count == 0 {
            // a 32-bit register destination is still zero-extended in long mode
            if bits == 32 && ins.op_kind(0) == OpKind::Register {
                tryf!(self.write_op(0, a));
            }
            return self.done();
        }
        let cf_in = self.cf();
        let (r, f, which) = match ins.mnemonic() {
            Shl | Sal => {
                let wide = (a as u128) << count;
                let r = wide as u64 & m;
                let cf = if count <= bits { ((wide >> bits) & 1) as u32 } else { 0 };
                let of = ((r & sb != 0) as u32) ^ cf;
                (r, flags::szp64(r, bits) | cf * CF | of * OF, STATUS)
            }
            Shr => {
                let r = if count >= 64 { 0 } else { a >> count };
                let cf = if count <= bits { ((a >> (count - 1)) & 1) as u32 } else { 0 };
                let of = (a & sb != 0) as u32;
                (r, flags::szp64(r, bits) | cf * CF | of * OF, STATUS)
            }
            Sar => {
                let sx = ((a << (64 - bits)) as i64) >> (64 - bits);
                let c = count.min(63);
                let r = (sx >> c) as u64 & m;
                let cf = ((sx >> (c - 1).min(63)) & 1) as u32;
                let cf = if count >= bits { (sx < 0) as u32 } else { cf };
                (r, flags::szp64(r, bits) | cf * CF, STATUS)
            }
            Rol => {
                let r = if eff == 0 { a } else { ((a << eff) | (a >> (bits - eff))) & m };
                let cf = (r & 1) as u32;
                let of = ((r & sb != 0) as u32) ^ cf;
                (r, cf * CF | of * OF, CF | OF)
            }
            Ror => {
                let r = if eff == 0 { a } else { ((a >> eff) | (a << (bits - eff))) & m };
                let cf = (r & sb != 0) as u32;
                let of = cf ^ ((r & (sb >> 1) != 0) as u32);
                (r, cf * CF | of * OF, CF | OF)
            }
            Rcl => {
                let mut v = a as u128 | (cf_in as u128) << bits;
                let w = bits + 1;
                if eff != 0 {
                    v = ((v << eff) | (v >> (w - eff))) & ((1u128 << w) - 1);
                }
                let r = v as u64 & m;
                let cf = ((v >> bits) & 1) as u32;
                let of = ((r & sb != 0) as u32) ^ cf;
                (r, cf * CF | of * OF, CF | OF)
            }
            _ => {
                // RCR
                let mut v = a as u128 | (cf_in as u128) << bits;
                let w = bits + 1;
                if eff != 0 {
                    v = ((v >> eff) | (v << (w - eff))) & ((1u128 << w) - 1);
                }
                let r = v as u64 & m;
                let cf = ((v >> bits) & 1) as u32;
                let of = ((r & sb != 0) as u32) ^ ((r & (sb >> 1) != 0) as u32);
                (r, cf * CF | of * OF, CF | OF)
            }
        };
        tryf!(self.write_op(0, r));
        self.set_flags(which, f);
        self.done()
    }

    fn shift_double(&mut self) -> Exit {
        let ins = self.ins;
        let bits = self.op_bits(0);
        let m = flags::mask64(bits);
        let d = tryf!(self.read_op(0)) & m;
        let s = tryf!(self.read_op(1)) & m;
        let count = (tryf!(self.read_op(2)) & if bits == 64 { 0x3f } else { 0x1f }) as u32;
        if count == 0 {
            if bits == 32 && ins.op_kind(0) == OpKind::Register {
                tryf!(self.write_op(0, d));
            }
            return self.done();
        }
        let sb = flags::sign64(bits);
        // (16-bit counts above 16 are undefined; this matches what the hardware gives for 32/64)
        let (r, cf) = if ins.mnemonic() == Mnemonic::Shld {
            let wide = ((d as u128) << bits | s as u128) << count;
            let r = (wide >> bits) as u64 & m;
            let cf = if bits == 64 { ((d >> (64 - count)) & 1) as u32 } else { ((wide >> (2 * bits)) & 1) as u32 };
            (r, cf)
        } else {
            let wide = ((s as u128) << bits | d as u128) >> (count - 1);
            let cf = (wide & 1) as u32;
            ((wide >> 1) as u64 & m, cf)
        };
        let of = ((r ^ d) & sb != 0) as u32;
        tryf!(self.write_op(0, r));
        self.set_status(flags::szp64(r, bits) | cf * CF | of * OF);
        self.done()
    }

    fn mul1(&mut self) -> Exit {
        let ins = self.ins;
        let bits = self.op_bits(0);
        let s = tryf!(self.read_op(0));
        let signed = ins.mnemonic() == Mnemonic::Imul;
        let ovf;
        let lo;
        match bits {
            8 => {
                let a = self.get_reg(Register::AL) as u32;
                let s = s as u32;
                let p = if signed { (a as u8 as i8 as i32 * s as u8 as i8 as i32) as u32 } else { a * (s & 0xff) };
                self.set_reg(Register::AX, (p & 0xffff) as u64);
                lo = (p & 0xff) as u64;
                ovf = if signed { p as u16 as i16 != p as u8 as i8 as i16 } else { p & 0xff00 != 0 };
            }
            16 => {
                let a = self.get_reg(Register::AX) as u32;
                let s = s as u32;
                let p = if signed { (a as u16 as i16 as i32 * s as u16 as i16 as i32) as u32 } else { a * (s & 0xffff) };
                self.set_reg(Register::AX, (p & 0xffff) as u64);
                self.set_reg(Register::DX, (p >> 16) as u64);
                lo = (p & 0xffff) as u64;
                ovf = if signed { p as i32 != p as u16 as i16 as i32 } else { p >> 16 != 0 };
            }
            32 => {
                let a = self.get_reg(Register::EAX);
                let p = if signed { (a as u32 as i32 as i64 * s as u32 as i32 as i64) as u64 } else { a * (s & 0xffff_ffff) };
                self.set_reg(Register::EAX, p as u32 as u64);
                self.set_reg(Register::EDX, p >> 32);
                lo = p as u32 as u64;
                ovf = if signed { p as i64 != p as u32 as i32 as i64 } else { p >> 32 != 0 };
            }
            _ => {
                let a = self.get_reg(Register::RAX);
                let p = if signed { (a as i64 as i128 * s as i64 as i128) as u128 } else { a as u128 * s as u128 };
                self.set_reg(Register::RAX, p as u64);
                self.set_reg(Register::RDX, (p >> 64) as u64);
                lo = p as u64;
                ovf = if signed { p as i128 != p as u64 as i64 as i128 } else { p >> 64 != 0 };
            }
        }
        let f = flags::szp64(lo, bits) | if ovf { CF | OF } else { 0 };
        self.set_status(f);
        self.done()
    }

    fn div(&mut self) -> Exit {
        let ins = self.ins;
        let bits = self.op_bits(0);
        let s = tryf!(self.read_op(0));
        let signed = ins.mnemonic() == Mnemonic::Idiv;
        match bits {
            8 => {
                let a = self.get_reg(Register::AX) as u32;
                let s = s as u32;
                let (q, rem) = if signed {
                    let d = s as u8 as i8 as i32;
                    if d == 0 { return Exit::DivideError; }
                    let n = a as u16 as i16 as i32;
                    let q = n / d;
                    if q != q as i8 as i32 { return Exit::DivideError; }
                    (q as u32, (n % d) as u32)
                } else {
                    let d = s & 0xff;
                    if d == 0 { return Exit::DivideError; }
                    let q = a / d;
                    if q > 0xff { return Exit::DivideError; }
                    (q, a % d)
                };
                self.set_reg(Register::AX, ((q & 0xff) | (rem & 0xff) << 8) as u64);
            }
            16 => {
                let a = (self.get_reg(Register::DX) as u32) << 16 | self.get_reg(Register::AX) as u32;
                let s = s as u32;
                let (q, rem) = if signed {
                    let d = s as u16 as i16 as i32;
                    if d == 0 { return Exit::DivideError; }
                    let n = a as i32;
                    let (q, rm) = (n.wrapping_div(d), n.wrapping_rem(d));
                    if q != q as i16 as i32 || (n == i32::MIN && d == -1) { return Exit::DivideError; }
                    (q as u32, rm as u32)
                } else {
                    let d = s & 0xffff;
                    if d == 0 { return Exit::DivideError; }
                    let q = a / d;
                    if q > 0xffff { return Exit::DivideError; }
                    (q, a % d)
                };
                self.set_reg(Register::AX, (q & 0xffff) as u64);
                self.set_reg(Register::DX, (rem & 0xffff) as u64);
            }
            32 => {
                let a = self.get_reg(Register::EDX) << 32 | self.get_reg(Register::EAX);
                let (q, rem) = if signed {
                    let d = s as u32 as i32 as i64;
                    if d == 0 { return Exit::DivideError; }
                    let n = a as i64;
                    if n == i64::MIN && d == -1 { return Exit::DivideError; }
                    let q = n / d;
                    if q != q as i32 as i64 { return Exit::DivideError; }
                    (q as u32, (n % d) as u32)
                } else {
                    let d = s & 0xffff_ffff;
                    if d == 0 { return Exit::DivideError; }
                    let q = a / d;
                    if q > 0xffff_ffff { return Exit::DivideError; }
                    (q as u32, (a % d) as u32)
                };
                self.set_reg(Register::EAX, q as u64);
                self.set_reg(Register::EDX, rem as u64);
            }
            _ => {
                let a = (self.get_reg(Register::RDX) as u128) << 64 | self.get_reg(Register::RAX) as u128;
                let (q, rem) = if signed {
                    let d = s as i64 as i128;
                    if d == 0 { return Exit::DivideError; }
                    let n = a as i128;
                    if n == i128::MIN && d == -1 { return Exit::DivideError; }
                    let q = n / d;
                    if q != q as i64 as i128 { return Exit::DivideError; }
                    (q as u64, (n % d) as u64)
                } else {
                    let d = s as u128;
                    if d == 0 { return Exit::DivideError; }
                    let q = a / d;
                    if q > u64::MAX as u128 { return Exit::DivideError; }
                    (q as u64, (a % d) as u64)
                };
                self.set_reg(Register::RAX, q);
                self.set_reg(Register::RDX, rem);
            }
        }
        // flags are undefined after DIV; leave them alone
        self.done()
    }

    fn bit_test(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let bits = self.op_bits(0);
        let off = tryf!(self.read_op(1));
        let reg_offset = ins.op_kind(1) == OpKind::Register;
        let (val, addr) = if ins.op_kind(0) == OpKind::Memory && reg_offset {
            // bit string: the offset is signed and may address outside the operand
            let o = match bits { 16 => off as u16 as i16 as i64, 32 => off as u32 as i32 as i64, _ => off as i64 };
            let sh = match bits { 16 => 4, 32 => 5, _ => 6 };
            // (the offset wraps at the address size: 16-bit addressing stays in the segment)
            let mut eff = self.offset().wrapping_add(((o >> sh) * (bits as i64 / 8)) as u64);
            if self.offset_bits() == 16 {
                eff &= 0xffff;
            }
            let a = lin(eff.wrapping_add(self.seg_base()), self.m64());
            (tryf!(self.read_mem(a, bits)), Some(a))
        } else {
            (tryf!(self.read_op(0)), None)
        };
        let bit = (off & (bits as u64 - 1)) as u32;
        let cf = ((val >> bit) & 1) as u32;
        let nv = match ins.mnemonic() {
            Bts => val | 1 << bit,
            Btr => val & !(1 << bit),
            Btc => val ^ 1 << bit,
            _ => val,
        };
        if ins.mnemonic() != Bt {
            match addr {
                Some(a) => tryf!(self.write_mem(a, bits, nv)),
                None => tryf!(self.write_op(0, nv)),
            }
        }
        self.set_flags(CF, cf);
        self.done()
    }

    /// The registers a string instruction uses: (count, source, destination) by address size.
    fn string_regs(&self) -> (Register, Register, Register) {
        let k = (0..self.ins.op_count()).map(|i| self.ins.op_kind(i));
        let mut a64 = self.m64();
        let mut a16 = false;
        for k in k {
            match k {
                OpKind::MemorySegESI | OpKind::MemoryESEDI => a64 = false,
                OpKind::MemorySegSI | OpKind::MemoryESDI => { a64 = false; a16 = true }
                _ => {}
            }
        }
        if a64 {
            (Register::RCX, Register::RSI, Register::RDI)
        } else if a16 {
            (Register::CX, Register::SI, Register::DI)
        } else {
            (Register::ECX, Register::ESI, Register::EDI)
        }
    }

    /// REP MOVS / REP STOS over plain memory in one go, when that gives the same result as
    /// element by element (no overlap that a forward or backward copy would see).
    fn bulk(&mut self, mn: Mnemonic, bits: u32, step: u64, src_seg: u64) -> bool {
        use Mnemonic::*;
        let (rc, rs, rd) = self.string_regs();
        if rc == Register::CX {
            return false;
        }
        let n = self.get_reg(rc);
        let size = bits as u64 / 8;
        if n < 16 || n > (1 << 28) {
            return false;
        }
        let len = n * size;
        let back = (step as i64) < 0;
        let m64 = self.m64();
        // lowest address of each range
        let lo = |p: u64| if back { p.wrapping_sub(len - size) } else { p };
        let di = lo(self.get_reg(rd));
        if m64 && (di >> 32 != 0 || (di + len) >> 32 != 0) {
            return false;
        }
        let (di, len32) = (di as u32, len as u32);
        let Some(d) = self.mem.host_range(di, len32) else { return false };
        let adv = (step as i64).wrapping_mul(n as i64) as u64;
        match mn {
            Movsb | Movsw | Movsd | Movsq => {
                let si = lo(self.get_reg(rs).wrapping_add(src_seg));
                if m64 && (si >> 32 != 0 || (si + len) >> 32 != 0) {
                    return false;
                }
                let si = si as u32;
                let Some(s) = self.mem.host_range(si, len32) else { return false };
                // forward copies equal memmove unless the destination starts inside the
                // source above it; backward ones unless it starts below
                let overlap = (di as u64) < si as u64 + len && (si as u64) < di as u64 + len;
                if overlap && di != si && ((!back && di > si) || (back && di < si)) {
                    return false;
                }
                unsafe { core::ptr::copy(s, d, len as usize) };
                let v = self.get_reg(rs).wrapping_add(adv);
                self.set_reg(rs, v);
            }
            Stosb | Stosw | Stosd | Stosq => {
                let v = self.get_reg(Register::RAX);
                unsafe {
                    match size {
                        1 => core::ptr::write_bytes(d, v as u8, len as usize),
                        2 => (0..n as usize).for_each(|i| (d as *mut u16).add(i).write_unaligned(v as u16)),
                        4 => (0..n as usize).for_each(|i| (d as *mut u32).add(i).write_unaligned(v as u32)),
                        _ => (0..n as usize).for_each(|i| (d as *mut u64).add(i).write_unaligned(v)),
                    }
                }
            }
            _ => return false,
        }
        let v = self.get_reg(rd).wrapping_add(adv);
        self.set_reg(rd, v);
        self.set_reg(rc, 0);
        true
    }

    fn string_op(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let mn = ins.mnemonic();
        let bits = match mn {
            Movsb | Stosb | Lodsb | Cmpsb | Scasb => 8,
            Movsw | Stosw | Lodsw | Cmpsw | Scasw => 16,
            Movsq | Stosq | Lodsq | Cmpsq | Scasq => 64,
            _ => 32,
        };
        let step: u64 = if self.cpu.eflags & DF != 0 { (bits as u64 / 8).wrapping_neg() } else { bits as u64 / 8 };
        let rep = ins.has_rep_prefix() || ins.has_repe_prefix();
        let repne = ins.has_repne_prefix();
        let src_seg = self.seg_base(); // DS:ESI may take a segment override (FS)
        let compare = matches!(mn, Cmpsb | Cmpsw | Cmpsd | Cmpsq | Scasb | Scasw | Scasd | Scasq);
        if rep && !compare && self.bulk(mn, bits, step, src_seg) {
            return self.done();
        }
        let (rc, rs, rd) = self.string_regs();
        let am = flags::mask64(reg_info(rc).1);
        let m64 = self.m64();
        let acc = match bits { 8 => Register::AL, 16 => Register::AX, 32 => Register::EAX, _ => Register::RAX };
        loop {
            if (rep || repne) && self.get_reg(rc) == 0 {
                break;
            }
            let si = self.get_reg(rs);
            let di = self.get_reg(rd);
            let sa = lin(si.wrapping_add(src_seg) & if m64 { u64::MAX } else { 0xffff_ffff }, m64);
            let da = if self.m16() { self.cpu.sbase[0].wrapping_add(di as u32) } else { lin(di, m64) };
            match mn {
                Movsb | Movsw | Movsd | Movsq => {
                    let v = tryf!(self.read_mem(sa, bits));
                    tryf!(self.write_mem(da, bits, v));
                    self.set_reg(rs, si.wrapping_add(step) & am);
                    self.set_reg(rd, di.wrapping_add(step) & am);
                }
                Stosb | Stosw | Stosd | Stosq => {
                    let v = self.get_reg(acc);
                    tryf!(self.write_mem(da, bits, v));
                    self.set_reg(rd, di.wrapping_add(step) & am);
                }
                Lodsb | Lodsw | Lodsd | Lodsq => {
                    let v = tryf!(self.read_mem(sa, bits));
                    self.set_reg(acc, v);
                    self.set_reg(rs, si.wrapping_add(step) & am);
                }
                Cmpsb | Cmpsw | Cmpsd | Cmpsq => {
                    let a = tryf!(self.read_mem(sa, bits));
                    let b = tryf!(self.read_mem(da, bits));
                    let (_, f) = flags::sub64(a, b, 0, bits);
                    self.set_status(f);
                    self.set_reg(rs, si.wrapping_add(step) & am);
                    self.set_reg(rd, di.wrapping_add(step) & am);
                }
                _ => {
                    let b = tryf!(self.read_mem(da, bits));
                    let (_, f) = flags::sub64(self.get_reg(acc), b, 0, bits);
                    self.set_status(f);
                    self.set_reg(rd, di.wrapping_add(step) & am);
                }
            }
            if !(rep || repne) {
                break;
            }
            let c = self.get_reg(rc).wrapping_sub(1) & am;
            self.set_reg(rc, c);
            if compare {
                let zf = self.cpu.eflags & ZF != 0;
                if (rep && !zf) || (repne && zf) {
                    break;
                }
            }
        }
        self.done()
    }
}

/// One byte of CRC-32C (polynomial 0x1EDC6F41, reflected 0x82F63B78), no pre/post inversion
pub(crate) fn crc32c_byte(c: u32, b: u8) -> u32 {
    (0..8).fold(c ^ b as u32, |c, _| if c & 1 != 0 { c >> 1 ^ 0x82f6_3b78 } else { c >> 1 })
}

/// A Pentium 4 class CPU with SSE2 (what VN-era code checks for).
/// CPUID as seen by guest code: a Pentium 4 class CPU with SSE2.
pub fn cpuid(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    cpuid_mode(leaf, sub, false)
}

/// Long mode reports what x64 programs of today require on top (Godot 4 refuses to start
/// without SSE4.2): SSE3, SSSE3, CMPXCHG16B, SSE4.1, SSE4.2, POPCNT, all of which the
/// interpreter runs. 32-bit code keeps the Pentium 4 view, so VN-era code stays on its SSE2 paths.
pub fn cpuid_mode(leaf: u32, _sub: u32, long: bool) -> (u32, u32, u32, u32) {
    const LONG_ECX: u32 = (1 << 0) | (1 << 9) | (1 << 13) | (1 << 19) | (1 << 20) | (1 << 23);
    match leaf {
        1 if long => (0x0000_06f1, 0, LONG_ECX, (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26)),
        0 => (1, u32::from_le_bytes(*b"Genu"), u32::from_le_bytes(*b"ntel"), u32::from_le_bytes(*b"ineI")),
        // family 6 model 15; FPU TSC CX8 CMOV MMX FXSR SSE SSE2
        1 => (0x0000_06f1, 0, 0, (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26)),
        // the extended leaves exist on every x86 since the Pentium 4: programs loop from
        // 0x80000000 up to the highest one with a signed compare (AdvHD: 0 there meant 2^31
        // rounds, appending each result to a vector until memory ran out)
        0x8000_0000 => (0x8000_0004, 0, 0, 0),
        0x8000_0002..=0x8000_0004 => {
            const BRAND: &[u8; 48] = b"vnvm virtual x86 processor\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
            let o = (leaf - 0x8000_0002) as usize * 16;
            let w = |i: usize| u32::from_le_bytes(BRAND[o + i * 4..o + i * 4 + 4].try_into().unwrap());
            (w(0), w(1), w(2), w(3))
        }
        _ => (0, 0, 0, 0),
    }
}

#[cfg(test)]
mod cpuid_tests {
    use super::*;

    #[test]
    fn long_mode_adds_sse42_and_32_bit_stays_p4() {
        let (_, _, c32, d32) = cpuid_mode(1, 0, false);
        let (_, _, c64, d64) = cpuid_mode(1, 0, true);
        assert_eq!((c32, d32), (0, d64));
        for bit in [0, 9, 13, 19, 20, 23] {
            assert_ne!(c64 & 1 << bit, 0, "bit {bit}");
        }
        assert_eq!(cpuid(1, 0), cpuid_mode(1, 0, false));
    }

    #[test]
    fn crc32c_check_value() {
        // CRC-32C of "123456789" with the usual inversions is 0xE3069283
        let c = b"123456789".iter().fold(!0u32, |c, &b| crc32c_byte(c, b));
        assert_eq!(!c, 0xe306_9283);
    }
}
