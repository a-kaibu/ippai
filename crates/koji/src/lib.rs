//! x86 user-mode CPU core (interpreter and x86->wasm JIT) for running Windows code with sake.
//!
//! The guest address space is flat 32-bit; segment registers other than FS/GS
//! (Windows' TEB) are ignored. The CPU never owns memory: everything goes
//! through [`Memory`], so the same core runs natively in tests (a Vec) and in
//! the browser (the wasm linear memory itself, guest address = wasm address).

#![no_std]
extern crate alloc;

pub(crate) mod interp;
pub mod flags;
pub mod lazy;
pub mod jit;
pub mod fpu;
pub mod round;

pub use iced_x86;
pub use fpu::Fpu;

/// The MXCSR exception flags of an SSE operation (for tests against the host CPU).
#[doc(hidden)]
pub use interp::sse::{flags32, flags64, narrow_flags, FOp};
pub use interp::{cpuid, cpuid_mode, CPUID_AVX, step, step_cached, step_flow, DecodeCache};

impl Cpu {
    /// EFLAGS with the lazy status flags worked out.
    #[inline]
    pub fn flags(&self) -> u32 {
        if self.lazy.op == 0 { self.eflags } else { (self.eflags & !flags::STATUS) | self.lazy.status() }
    }

    /// 64-bit general register `i` (0..16, encoding order).
    #[inline]
    pub fn gpr(&self, i: usize) -> u64 {
        if i < 8 { self.regs[i] as u64 | (self.hi[i] as u64) << 32 } else { self.r8[i - 8] }
    }

    #[inline]
    pub fn set_gpr(&mut self, i: usize, v: u64) {
        if i < 8 {
            self.regs[i] = v as u32;
            self.hi[i] = (v >> 32) as u32;
        } else {
            self.r8[i - 8] = v;
        }
    }

    /// XMM register `i` (0..16; 16 is the interpreter's scratch, `vex_tmp`).
    #[inline]
    pub fn xmm(&self, i: usize) -> u128 {
        if i < 8 { self.xmm[i] } else if i < 16 { self.xmm8[i - 8] } else { self.vex_tmp }
    }

    #[inline]
    pub fn set_xmm(&mut self, i: usize, v: u128) {
        if i < 8 { self.xmm[i] = v } else if i < 16 { self.xmm8[i - 8] = v } else { self.vex_tmp = v }
    }

    /// YMM register `i` (0..16, 16: the scratch): [low half (the XMM register), upper half].
    #[inline]
    pub fn ymm(&self, i: usize) -> [u128; 2] {
        [self.xmm(i), self.ymmh[i]]
    }

    #[inline]
    pub fn set_ymm(&mut self, i: usize, v: [u128; 2]) {
        self.set_xmm(i, v[0]);
        self.ymmh[i] = v[1];
    }

    /// Makes `eflags` exact again.
    #[inline]
    pub fn materialize(&mut self) {
        if self.lazy.op != 0 {
            self.eflags = self.flags();
            self.lazy.op = 0;
        }
    }
}

/// A memory access the guest may not make (unmapped / protected page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub addr: u32,
    pub write: bool,
}

/// Guest memory. Little-endian, unaligned accesses allowed.
pub trait Memory {
    fn read8(&mut self, addr: u32) -> Result<u8, Fault>;
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), Fault>;

    #[inline]
    fn read16(&mut self, addr: u32) -> Result<u16, Fault> {
        Ok(self.read8(addr)? as u16 | (self.read8(addr.wrapping_add(1))? as u16) << 8)
    }
    #[inline]
    fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
        Ok(self.read16(addr)? as u32 | (self.read16(addr.wrapping_add(2))? as u32) << 16)
    }
    #[inline]
    fn read64(&mut self, addr: u32) -> Result<u64, Fault> {
        Ok(self.read32(addr)? as u64 | (self.read32(addr.wrapping_add(4))? as u64) << 32)
    }
    #[inline]
    fn write16(&mut self, addr: u32, v: u16) -> Result<(), Fault> {
        self.write8(addr, v as u8)?;
        self.write8(addr.wrapping_add(1), (v >> 8) as u8)
    }
    #[inline]
    fn write32(&mut self, addr: u32, v: u32) -> Result<(), Fault> {
        self.write16(addr, v as u16)?;
        self.write16(addr.wrapping_add(2), (v >> 16) as u16)
    }
    #[inline]
    fn write64(&mut self, addr: u32, v: u64) -> Result<(), Fault> {
        self.write32(addr, v as u32)?;
        self.write32(addr.wrapping_add(4), (v >> 32) as u32)
    }
    /// Compare-and-swap of `bytes` (1, 2, 4 or 8) bytes at `addr`, atomic with respect to other
    /// threads' accesses: stores `new` if the memory holds `old`; returns what it held.
    fn cmpxchg(&mut self, addr: u32, bytes: u32, old: u64, new: u64) -> Result<u64, Fault> {
        let cur = match bytes {
            1 => self.read8(addr)? as u64,
            2 => self.read16(addr)? as u64,
            4 => self.read32(addr)? as u64,
            _ => self.read64(addr)?,
        };
        if cur == old {
            match bytes {
                1 => self.write8(addr, new as u8)?,
                2 => self.write16(addr, new as u16)?,
                4 => self.write32(addr, new as u32)?,
                _ => self.write64(addr, new)?,
            }
        }
        Ok(cur)
    }
    /// CMPXCHG16B: compare-and-swap of the 16 bytes at `addr`, atomic with respect to other
    /// threads' 16-byte swaps; returns what the memory held.
    fn cmpxchg16(&mut self, addr: u32, old: u128, new: u128) -> Result<u128, Fault> {
        let cur = self.read64(addr)? as u128 | (self.read64(addr.wrapping_add(8))? as u128) << 64;
        if cur == old {
            self.write64(addr, new as u64)?;
            self.write64(addr.wrapping_add(8), (new >> 64) as u64)?;
        }
        Ok(cur)
    }
    /// Host pointer to [addr, addr + len) if all of it is plain accessible memory (bulk
    /// string operations); None to go byte by byte.
    fn host_range(&mut self, _addr: u32, _len: u32) -> Option<*mut u8> {
        None
    }

    /// Copy up to `buf.len()` instruction bytes at `addr`; returns how many
    /// were readable (decoding fails cleanly on a short fetch).
    fn fetch(&mut self, addr: u32, buf: &mut [u8]) -> usize {
        for (i, b) in buf.iter_mut().enumerate() {
            match self.read8(addr.wrapping_add(i as u32)) {
                Ok(v) => *b = v,
                Err(_) => return i,
            }
        }
        buf.len()
    }
}

/// General registers in encoding order.
pub const EAX: usize = 0;
pub const ECX: usize = 1;
pub const EDX: usize = 2;
pub const EBX: usize = 3;
pub const ESP: usize = 4;
pub const EBP: usize = 5;
pub const ESI: usize = 6;
pub const EDI: usize = 7;

/// Architectural state of one guest thread. `repr(C)`: the JIT addresses the fields directly.
#[derive(Clone, Debug)]
#[repr(C)]
pub struct Cpu {
    pub regs: [u32; 8],
    pub eip: u32,
    /// EFLAGS (bit 1 is kept set). Its status flags are stale while `lazy.op` is set: use
    /// [`Cpu::flags`] / [`Cpu::materialize`].
    pub eflags: u32,
    /// Status flags of the last JIT-executed operation, computed only when needed.
    pub lazy: lazy::Lazy,
    /// For the JIT's dispatcher (crates/kura): block hash table, its mask, and how many more
    /// blocks to run before returning.
    pub jit_hash: u32,
    pub jit_mask: u32,
    pub jit_budget: i32,
    /// JIT: ring of recent jumps between regions (see jit::emit_lookup)
    pub jit_ring: u32,
    pub fs_base: u32,
    pub gs_base: u32,
    /// [start, end): jumping here stops execution with [`Exit::Trap`]
    /// (sake's API trap addresses).
    pub trap_lo: u32,
    pub trap_hi: u32,
    /// Value returned by RDTSC (the embedder advances it).
    pub tsc: u64,
    pub fpu: Fpu,
    /// MMX registers (kept apart from the x87 stack: code does not mix them).
    pub mm: [u64; 8],
    pub xmm: [u128; 8],
    pub mxcsr: u32,
    /// x86-64 (long mode) state. In 32-bit mode none of it is used; RIP, RSP and the FS/GS
    /// bases always fit in 32 bits (all guest memory is below 4GB), so they stay in `eip`,
    /// `regs` and `fs_base`/`gs_base`. `hi` holds the upper halves of RAX..RDI.
    pub mode64: u32,
    pub hi: [u32; 8],
    pub r8: [u64; 8],
    pub xmm8: [u128; 8],
    /// JIT: guest calls currently nested as wasm calls (see jit::Gen::call_out)
    pub jit_depth: u32,
    /// Long mode: the upper half of RIP. Memory is addressed modulo 4GB (an image whose base is
    /// above 4GB and can't be relocated lives at the base's low half, see Wine's virtual.c),
    /// but RIP keeps its full value for what code computes from it (RIP-relative addresses,
    /// return addresses, unwinding).
    pub rip_hi: u32,
    /// JIT: the thread's call cache (function mode: a direct-mapped table of (target, hash
    /// table entry) pairs that call sites index by their own address, see jit::Gen::call_fn)
    pub jit_calls: u32,
    /// JIT: the host's bitmap of pages with translated code (a bit per 4 KB page), which
    /// translations of code that may rewrite code test their stores against
    /// (jit::set_watch), and the address such a store hit (0: none; the host drops the
    /// translations, see kura's jit::smc_hit)
    pub jit_code_pages: u32,
    pub jit_smc: u32,
    /// JIT: the start of a block whose code is no longer what it was translated from (code the
    /// host has its translations check on entry, jit::set_fresh); the host drops them (0: none)
    pub jit_stale: u32,
    /// The interpreter's scratch vector register (XMM16 to it): a VEX instruction's
    /// non-destructive source runs its legacy form on a copy here (interp/vex.rs)
    pub vex_tmp: u128,
    /// 16-bit protected mode (Win16): code, stack and addresses are 16-bit, and memory
    /// operands add the base of their segment. `eip` is IP (an offset in CS).
    pub mode16: u32,
    /// Selectors and bases of ES CS SS DS FS GS (16-bit mode only)
    pub sreg: [u16; 6],
    pub sbase: [u32; 6],
    /// 16-bit mode: a table of segment bases (u32 per selector index, sel >> 3) that loading
    /// a segment register reads
    pub ldt: u32,
    /// The upper halves (bits 128..255) of YMM0..15, and of the scratch register (16). Legacy
    /// SSE instructions keep them; VEX.128 instructions clear their destination's.
    pub ymmh: [u128; 17],
}

/// `Cpu::ldt` value for real-mode segment loading in 16-bit mode (base = selector * 16): what
/// the hardware-recorded 80386 real-mode suite (crates/koji/tests/singlestep.rs) runs on.
pub const LDT_REAL: u32 = u32::MAX;

impl Cpu {
    /// 16-bit mode: the linear address of CS:IP.
    #[inline]
    pub fn pc16(&self) -> u32 {
        self.sbase[1].wrapping_add(self.eip & 0xffff)
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu {
            regs: [0; 8], eip: 0, eflags: 0x202, lazy: lazy::Lazy::default(), jit_hash: 0, jit_mask: 0,
            jit_budget: 0, jit_ring: 0, fs_base: 0, gs_base: 0, trap_lo: 0, trap_hi: 0, tsc: 0,
            fpu: Fpu::default(), mm: [0; 8], xmm: [0; 8], mxcsr: 0x1f80,
            mode64: 0, hi: [0; 8], r8: [0; 8], xmm8: [0; 8], jit_depth: 0, rip_hi: 0, jit_calls: 0, jit_code_pages: 0, jit_smc: 0, jit_stale: 0,
            vex_tmp: 0, mode16: 0, sreg: [0; 6], sbase: [0; 6], ldt: 0, ymmh: [0; 17],
        }
    }
}

/// Why [`step`] stopped. EIP points at the instruction that caused it
/// (for `Trap`, at the trap address itself).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The instruction completed.
    Continue,
    /// Control reached the trap range.
    Trap(u32),
    /// Page fault.
    Fault(Fault),
    /// #DE (DIV / IDIV by zero or overflow).
    DivideError,
    /// #UD, or an instruction this core does not implement.
    InvalidOpcode,
    /// INT n (INT3 = 3).
    Interrupt(u8),
    /// SYSCALL (long mode); EIP is past it, RCX and R11 are set as the instruction sets them.
    Syscall,
    /// HLT (privileged in user mode; the embedder decides).
    Halt,
    /// The single-step trap (#DB): an instruction that began with TF set completed (32/64-bit
    /// modes); EIP is past it.
    SingleStep,
    /// #XM: an SSE floating-point exception whose MXCSR mask bit is clear. The MXCSR flags of
    /// the instruction are set, its destination is not written and EIP points at it.
    SimdException,
}

/// FXSAVE image (512 bytes) of the x87/SSE state at `a`.
pub fn fxsave<M: Memory + ?Sized>(cpu: &Cpu, mem: &mut M, a: u32) -> Result<(), Fault> {
    let f = &cpu.fpu;
    let tw = !f.empty; // abridged: 1 = valid
    mem.write16(a, f.cw)?;
    mem.write16(a + 2, f.status())?;
    mem.write8(a + 4, tw)?;
    mem.write8(a + 5, 0)?;
    for o in (6..24).step_by(2) {
        mem.write16(a + o, 0)?;
    }
    mem.write32(a + 24, cpu.mxcsr)?;
    mem.write32(a + 28, 0xffff)?;
    for i in 0..8u32 {
        let b = fpu::to_f80(cpu.fpu.st(i));
        for (k, x) in b.iter().enumerate() {
            mem.write8(a + 32 + 16 * i + k as u32, *x)?;
        }
        for k in 10..16 {
            mem.write8(a + 32 + 16 * i + k, 0)?;
        }
    }
    for i in 0..if cpu.mode64 != 0 { 16 } else { 8 } {
        let x = cpu.xmm(i as usize);
        mem.write64(a + 160 + 16 * i, x as u64)?;
        mem.write64(a + 168 + 16 * i, (x >> 64) as u64)?;
    }
    Ok(())
}

/// Loads the x87/SSE state from an FXSAVE image at `a`.
pub fn fxrstor<M: Memory + ?Sized>(cpu: &mut Cpu, mem: &mut M, a: u32) -> Result<(), Fault> {
    let cw = mem.read16(a)?;
    let sw = mem.read16(a + 2)?;
    let tw = mem.read8(a + 4)?;
    let mxcsr = mem.read32(a + 24)?;
    let mut regs = [0.0; 8];
    for (i, r) in regs.iter_mut().enumerate() {
        let mut b = [0u8; 10];
        for (k, x) in b.iter_mut().enumerate() {
            *x = mem.read8(a + 32 + 16 * i as u32 + k as u32)?;
        }
        *r = fpu::from_f80(&b);
    }
    let mut xmm = [0u128; 16];
    for (i, x) in xmm.iter_mut().enumerate().take(if cpu.mode64 != 0 { 16 } else { 8 }) {
        let lo = mem.read64(a + 160 + 16 * i as u32)?;
        let hi = mem.read64(a + 168 + 16 * i as u32)?;
        *x = lo as u128 | (hi as u128) << 64;
    }
    let f = &mut cpu.fpu;
    f.cw = cw;
    f.set_status(sw);
    f.empty = !tw;
    f.exact = 0;
    for (i, r) in regs.iter().enumerate() {
        let p = ((f.top + i as u32) & 7) as usize;
        f.regs[p] = *r;
    }
    cpu.mxcsr = mxcsr;
    crate::jit::mx_modes(mxcsr, cpu.eip);
    for (i, x) in xmm.iter().enumerate().take(if cpu.mode64 != 0 { 16 } else { 8 }) {
        cpu.set_xmm(i, *x);
    }
    Ok(())
}
