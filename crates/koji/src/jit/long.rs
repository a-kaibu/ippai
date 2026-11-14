//! Long mode (x86-64) translation: a region of basic blocks (found from an entry by following
//! direct jumps) becomes one wasm function of the region type (`super::N_PARAMS` i32 in, the
//! status out) that the dispatcher and the hash table treat like a 32-bit region's.
//!
//! Unlike 32-bit regions it keeps nothing in the parameters: the sixteen 64-bit registers are
//! loaded from the Cpu into i64 locals at entry and written back at every exit, which returns
//! the status with the state in the Cpu (0: go on at `cpu.eip`/`cpu.rip_hi`, RET_INTERP: the
//! interpreter runs the instruction at `cpu.eip`, else what the `step` import returned).
//!
//! Guest addresses are the effective address modulo 4GB (see `Cpu::rip_hi`); RIP-relative
//! operands and pushed return addresses use the full 64-bit RIP. Accesses outside
//! [GUEST_LO, GUEST_HI) leave for the interpreter before the instruction changed anything.
//!
//! Status flags are lazy: the last flag-setting operation's kind, size and operands (i64
//! locals). Conditional jumps, SETcc and CMOVcc read them directly; at exits, before the
//! interpreter steps and before jumps to blocks that may read them, they go to the Cpu (a
//! 32-bit lazy record, or computed EFLAGS for 64-bit operations). Instructions the
//! translator does not handle run through the `step` import.

use super::wasm::{Code, Func, FuncType, Import, Module, F64, I32, I64, V128};

#[path = "long/avx.rs"]
mod avx;
#[path = "long/sse.rs"]
mod sse;
use super::{Region, ENTRY_SHIFT, GUEST_HI, GUEST_LO, N_PARAMS, RET_INTERP};
use crate::{Cpu, Memory};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::offset_of;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use iced_x86::{ConditionCode, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind, Register};

const F_STEP: u32 = 0;
const F_EFLAGS: u32 = 1;
const F_STATUS: u32 = 2;

const MAX_INS: usize = 120;
const MAX_BLOCKS: usize = 256;
/// Instructions in a region at most (V8's optimizing compiler takes superlinear time on big
/// functions; jit/mod.rs's MAX_REGION_INS)
const MAX_REGION_INS: usize = 1000;
/// Entries of a jump table at most
const MAX_TABLE: u64 = 512;
/// A RET compares its target with this many of the region's return addresses at most
const MAX_RETURNS: usize = 8;

/// Debugging a miscompile: instructions per block and blocks per region at most (0: the
/// defaults), and mnemonics (by their `Mnemonic` number) left to the interpreter.
pub static DEBUG_INS: AtomicUsize = AtomicUsize::new(0);
pub static DEBUG_BLOCKS: AtomicUsize = AtomicUsize::new(0);
pub static DEBUG_CHECK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Debugging: every region moves all sixteen registers between the Cpu and its locals at its
/// entry and exits (as before regions moved only the ones they name).
pub static DEBUG_ALL_REGS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub static DEBUG_OFF: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];

/// Leaves mnemonic `m` to the interpreter (see DEBUG_OFF).
pub fn debug_off(m: Mnemonic) {
    let i = m as usize;
    DEBUG_OFF[i / 64 % 32].fetch_or(1 << (i % 64), Relaxed);
}
/// [`debug_off`] by the mnemonic's name (any case); false if there is none.
pub fn debug_off_named(name: &str) -> bool {
    match Mnemonic::values().find(|m| alloc::format!("{m:?}").eq_ignore_ascii_case(name)) {
        Some(m) => {
            debug_off(m);
            true
        }
        None => false,
    }
}
fn limit(v: &AtomicUsize, default: usize) -> usize {
    match v.load(Relaxed) {
        0 => default,
        n => n.min(default),
    }
}

// locals (after the N_PARAMS i32 parameters: 0 cpu, 1 entry)
const L_CPU: u32 = 0;
const L_ARG: u32 = 1;
const L_R: u32 = N_PARAMS; // 16 i64: rax..r15
const L_FA: u32 = L_R + 16; // i64: the flag operation's operands and result
const L_FB: u32 = L_FA + 1;
const L_FR: u32 = L_FA + 2;
const L_NT: u32 = L_FA + 3; // i64: where an exit goes
const L_T: u32 = L_FA + 4; // N_T i64 temporaries
const N_T: u32 = 6;
const N_I64: u32 = 16 + 4 + N_T;
const L_FC: u32 = L_R + N_I64; // i32: carry kept by INC/DEC
const L_BLK: u32 = L_FC + 1; // i32
const L_A: u32 = L_FC + 2; // i32: the instruction's memory address
const L_ST: u32 = L_FC + 3; // i32: an exit's status
const L_DBG: u32 = L_FC + 4; // i32: debugging
const L_H: u32 = L_FC + 5; // i32: the exit's hash probe (see Gen::chain)
const L_E: u32 = L_FC + 6; // i32
const L_W: u32 = L_FC + 7; // i32: the address a store of the instruction writes (Gen::smc)
const N_I32: u32 = 8;
const L_V0: u32 = L_FC + N_I32; // v128: SSE operands (long/sse.rs)
const L_V1: u32 = L_V0 + 1;
const L_V2: u32 = L_V1 + 1; // v128: the MXCSR guard's scratch (long/sse.rs)

// EFLAGS bits
const CF: u32 = 1;
const PF: u32 = 4;
const AF: u32 = 0x10;
const ZF: u32 = 0x40;
const SF: u32 = 0x80;
const OF: u32 = 0x800;
const STATUS: u32 = CF | PF | AF | ZF | SF | OF;

// lazy kinds (crate::lazy's)
use crate::lazy::{ADC, ADD, DEC, INC, LOGIC, MUL, SAR, SBB, SHL, SHR, SUB};

fn off_lo(i: usize) -> u32 {
    if i < 8 { (offset_of!(Cpu, regs) + 4 * i) as u32 } else { (offset_of!(Cpu, r8) + 8 * (i - 8)) as u32 }
}
fn off_hi(i: usize) -> u32 {
    (offset_of!(Cpu, hi) + 4 * i) as u32
}
macro_rules! cpu_off {
    ($f:ident) => {
        offset_of!(Cpu, $f) as u32
    };
}

/// i64 opcodes the shared Code builder has no names for
trait I64Ops {
    fn x(&mut self, o: u8) -> &mut Self;
}
impl I64Ops for Code {
    fn x(&mut self, o: u8) -> &mut Self {
        self.op(o)
    }
}
const I64_EQZ: u8 = 0x50;
const I64_EQ: u8 = 0x51;
const I64_NE: u8 = 0x52;
const I64_LT_S: u8 = 0x53;
const I64_LT_U: u8 = 0x54;
const I64_LE_S: u8 = 0x57;
const I64_LE_U: u8 = 0x58;
const I64_POPCNT: u8 = 0x7b;
const I64_ADD: u8 = 0x7c;
const I64_SUB: u8 = 0x7d;
const I64_MUL: u8 = 0x7e;
const I64_DIV_S: u8 = 0x7f;
const I64_DIV_U: u8 = 0x80;
const I64_REM_S: u8 = 0x81;
const I64_REM_U: u8 = 0x82;
const I64_GE_U: u8 = 0x5a;
const I64_CLZ: u8 = 0x79;
const I64_CTZ: u8 = 0x7a;
const I64_ROTL: u8 = 0x89;
const I64_ROTR: u8 = 0x8a;
const I64_AND: u8 = 0x83;
const I64_OR: u8 = 0x84;
const I64_XOR: u8 = 0x85;
const I64_SHL: u8 = 0x86;
const I64_SHR_S: u8 = 0x87;
const I64_SHR_U: u8 = 0x88;
const I32_WRAP: u8 = 0xa7;
const I64_EXT_U: u8 = 0xad;

fn mask(bits: u32) -> i64 {
    if bits >= 64 { -1 } else { ((1u64 << bits) - 1) as i64 }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flags {
    /// in the Cpu (EFLAGS and its lazy record)
    Cpu,
    /// the last operation: kind (crate::lazy), operand bits
    Lazy(u32, u32),
}

struct Block {
    start: u64,
    ins: Vec<Instruction>,
    /// reads the status flags before writing all of them (or may: passes them on)
    flags_in: bool,
}

struct Gen<'a> {
    c: Code,
    blocks: &'a [Block],
    index: &'a BTreeMap<u64, usize>,
    /// return addresses of the region's calls into it (blocks of it)
    returns: &'a [u64],
    flags: Flags,
    /// blocks between the current code and the dispatch loop
    depth: u32,
    ins: Instruction,
    temps: u32,
    /// the depth at which this block's code has seen MXCSR.PE set (mxguard::mode): it stays
    /// set until an interpreter step (fallback) could clear it
    mx_checked: Option<u32>,
    /// per XMM register, the (up to two) ranges it was checked for in this block and not
    /// written since (mxguard::key): not checked again
    mx_seen: [[Option<super::mxguard::Key>; 2]; 16],
    /// load_regs / store_regs so far: (offset in c.b, index in c.locs, store)
    marks: Vec<(usize, usize, bool)>,
    /// YMM registers whose halves a VEX.256 translation has swapped (long/sse.rs): leaving for
    /// the interpreter swaps them back first
    hi_swapped: Vec<Register>,
    /// the block's code may be rewritten ([`super::WATCH`]): an instruction's store into a
    /// page with translated code leaves at the next instruction with Cpu::jit_smc set
    smc: bool,
    /// the block's code is checked on entry ([`super::FRESH`]: code on W|X pages outside the
    /// exe, Mono's): (its start, its length); a store into its own bytes leaves at the next
    /// instruction with Cpu::jit_smc set (Gen::fresh_store)
    fresh: Option<(u32, u32)>,
    /// the current instruction's store is checked so (its address kept in L_W by addr/push64)
    watch_store: bool,
    /// the current instruction went to the interpreter (whose stores the host checks)
    fell_back: bool,
}

/// The registers `code` reads or writes (bit i: register i's local, L_R + i), and the ones
/// it writes (local.set / local.tee).
fn regs_named(code: &Code) -> (u16, u16) {
    let (mut named, mut written) = (0u16, 0u16);
    for &p in &code.locs {
        let p = p as usize;
        let (mut l, mut shift, mut at) = (0u32, 0, p + 1);
        loop {
            let c = code.b[at];
            l |= ((c & 0x7f) as u32) << shift;
            at += 1;
            shift += 7;
            if c & 0x80 == 0 {
                break;
            }
        }
        if (L_R..L_R + 16).contains(&l) {
            let bit = 1u16 << (l - L_R);
            named |= bit;
            if code.b[p] != 0x20 {
                written |= bit;
            }
        }
    }
    (named, written)
}

/// Loads of the registers in `mask` from the Cpu into their locals.
fn emit_loads(c: &mut Code, mask: u16) {
    for i in (0..16).filter(|i| mask >> i & 1 != 0) {
        if i < 8 {
            c.get(L_CPU).load32(off_lo(i)).x(I64_EXT_U);
            c.get(L_CPU).load32(off_hi(i)).x(I64_EXT_U).i64c(32).x(I64_SHL).x(I64_OR);
        } else {
            c.get(L_CPU).load64(off_lo(i));
        }
        c.set(L_R + i as u32);
    }
}

/// Stores of the registers in `mask` from their locals to the Cpu.
fn emit_stores(c: &mut Code, mask: u16) {
    for i in (0..16).filter(|i| mask >> i & 1 != 0) {
        let r = L_R + i as u32;
        if i < 8 {
            c.get(L_CPU).get(r).x(I32_WRAP).store32(off_lo(i));
            c.get(L_CPU).get(r).i64c(32).x(I64_SHR_U).x(I32_WRAP).store32(off_hi(i));
        } else {
            c.get(L_CPU).get(r).store64(off_lo(i));
        }
    }
}

/// `code` with its load_regs / store_regs marks filled in: loads of every register the code
/// names, stores of the ones it writes. A region that touches four registers moves four,
/// not sixteen, at its entry and at each exit: the code between regions is what a hop runs,
/// and with sixteen it was most of it (and too much for the instruction cache once a program
/// hops among a thousand regions).
fn splice_regs(code: Code, marks: &[(usize, usize, bool)], all: bool) -> Code {
    let (named, written) = if all { (u16::MAX, u16::MAX) } else { regs_named(&code) };
    let mut out = Code { b: Vec::with_capacity(code.b.len() + marks.len() * 64), locs: Vec::with_capacity(code.locs.len()), cpu_at: false };
    let (mut from, mut lfrom) = (0, 0);
    for &(at, li, store) in marks {
        out.b.extend_from_slice(&code.b[from..at]);
        let delta = out.b.len() as i64 - at as i64;
        out.locs.extend(code.locs[lfrom..li].iter().map(|&p| (p as i64 + delta) as u32));
        if store {
            emit_stores(&mut out, written);
        } else {
            emit_loads(&mut out, named);
        }
        (from, lfrom) = (at, li);
    }
    out.b.extend_from_slice(&code.b[from..]);
    let delta = out.b.len() as i64 - code.b.len() as i64;
    out.locs.extend(code.locs[lfrom..].iter().map(|&p| (p as i64 + delta) as u32));
    out
}

impl<'a> Gen<'a> {
    fn temp(&mut self) -> u32 {
        let t = L_T + self.temps;
        self.temps += 1;
        assert!(self.temps <= N_T);
        t
    }

    // ---- registers

    /// Where the registers come from the Cpu: a mark that [`splice_regs`] fills with the
    /// loads of the registers the region's code names (the rest stay in the Cpu: nothing here
    /// reads or changes them).
    fn load_regs(&mut self) {
        self.marks.push((self.c.b.len(), self.c.locs.len(), false));
    }

    /// Where the registers go back to the Cpu: a mark for the stores of the ones the code
    /// writes (see [`splice_regs`]).
    fn store_regs(&mut self) {
        self.marks.push((self.c.b.len(), self.c.locs.len(), true));
    }

    /// (register local, bits, high byte)
    fn reg(r: Register) -> Option<(u32, u32, bool)> {
        use Register::*;
        let (i, bits, high) = match r {
            AL | CL | DL | BL => ((r as u32 - AL as u32), 8, false),
            AH | CH | DH | BH => ((r as u32 - AH as u32), 8, true),
            SPL | BPL | SIL | DIL => ((r as u32 - SPL as u32) + 4, 8, false),
            R8L | R9L | R10L | R11L | R12L | R13L | R14L | R15L => ((r as u32 - R8L as u32) + 8, 8, false),
            AX | CX | DX | BX | SP | BP | SI | DI => ((r as u32 - AX as u32), 16, false),
            R8W | R9W | R10W | R11W | R12W | R13W | R14W | R15W => ((r as u32 - R8W as u32) + 8, 16, false),
            EAX | ECX | EDX | EBX | ESP | EBP | ESI | EDI => ((r as u32 - EAX as u32), 32, false),
            R8D | R9D | R10D | R11D | R12D | R13D | R14D | R15D => ((r as u32 - R8D as u32) + 8, 32, false),
            RAX | RCX | RDX | RBX | RSP | RBP | RSI | RDI => ((r as u32 - RAX as u32), 64, false),
            R8 | R9 | R10 | R11 | R12 | R13 | R14 | R15 => ((r as u32 - R8 as u32) + 8, 64, false),
            _ => return Option::None,
        };
        Some((L_R + i, bits, high))
    }

    /// Pushes the register's value (i64, zero-extended).
    fn get_reg(&mut self, r: Register) {
        let (l, bits, high) = Self::reg(r).unwrap();
        self.c.get(l);
        if high {
            self.c.i64c(8).x(I64_SHR_U);
        }
        if bits == 32 {
            self.c.x(I32_WRAP).x(I64_EXT_U);
        } else if bits < 64 {
            self.c.i64c(mask(bits)).x(I64_AND);
        }
    }

    /// Sets the register from local `v` (i64; bits above the register's ignored). 32-bit
    /// writes clear the upper half.
    fn set_reg(&mut self, r: Register, v: u32) {
        let (l, bits, high) = Self::reg(r).unwrap();
        match (bits, high) {
            (64, _) => {
                self.c.get(v).set(l);
            }
            (32, _) => {
                self.c.get(v).x(I32_WRAP).x(I64_EXT_U).set(l);
            }
            (8, true) => {
                self.c.get(l).i64c(!0xff00).x(I64_AND);
                self.c.get(v).i64c(0xff).x(I64_AND).i64c(8).x(I64_SHL).x(I64_OR).set(l);
            }
            _ => {
                self.c.get(l).i64c(!mask(bits)).x(I64_AND);
                self.c.get(v).i64c(mask(bits)).x(I64_AND).x(I64_OR).set(l);
            }
        }
    }

    // ---- memory

    /// The memory operand's address (modulo 4GB) into L_A, leaving for the interpreter if
    /// `size` bytes there are outside the guest range. None: not an operand we can address.
    fn addr(&mut self, size: u32) -> bool {
        let ins = self.ins;
        let seg = match ins.memory_segment() {
            Register::FS => Some(cpu_off!(fs_base)),
            Register::GS => Some(cpu_off!(gs_base)),
            Register::ES | Register::CS | Register::SS | Register::DS | Register::None => None,
            _ => return false,
        };
        let base = ins.memory_base();
        let index = ins.memory_index();
        let usable = |r: Register| r == Register::None || Self::reg(r).is_some_and(|(_, bits, high)| !high && bits >= 32);
        if !(base == Register::RIP || base == Register::EIP || usable(base)) || !usable(index) {
            return false;
        }
        if base == Register::RIP || base == Register::EIP {
            // (iced: the absolute address)
            let t = ins.memory_displacement64() as u32;
            self.c.u32c(t);
        } else {
            let mut any = false;
            for (r, scale) in [(base, 1u32), (index, ins.memory_index_scale())] {
                if r == Register::None {
                    continue;
                }
                let (l, _, _) = Self::reg(r).unwrap();
                // (only the low 32 bits matter modulo 4GB)
                self.c.get(l).x(I32_WRAP);
                if scale > 1 {
                    self.c.i32c(scale.trailing_zeros() as i32).shl();
                }
                if any {
                    self.c.add();
                }
                any = true;
            }
            let d = ins.memory_displacement64() as u32;
            if !any {
                self.c.u32c(d);
            } else if d != 0 {
                self.c.u32c(d).add();
            }
        }
        if let Some(o) = seg {
            self.c.get(L_CPU).load32(o).add();
        }
        self.c.tee(L_A);
        if self.watch_store {
            self.c.get(L_A).set(L_W);
        }
        let span = GUEST_HI - GUEST_LO;
        self.c.u32c(GUEST_LO).sub().u32c(span - size).gt_u().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
        // the pages that fault (super::PAGE_MAP)
        if super::page_check() {
            let ins = self.ins;
            super::pages_marked(&mut self.c, L_A, size, &ins);
            self.c.if_();
            self.depth += 1;
            self.bail();
            self.depth -= 1;
            self.c.end();
        }
        true
    }

    fn load(&mut self, bits: u32) {
        self.c.get(L_A);
        match bits {
            8 => self.c.x(0x31).x(0).x(0),  // i64.load8_u
            16 => self.c.x(0x33).x(0).x(0), // i64.load16_u
            32 => self.c.x(0x35).x(0).x(0), // i64.load32_u
            _ => self.c.load64(0),
        };
    }

    /// Stores local `v` to L_A.
    fn store(&mut self, bits: u32, v: u32) {
        self.c.get(L_A).get(v);
        match bits {
            8 => self.c.x(0x3c).x(0).x(0),  // i64.store8
            16 => self.c.x(0x3d).x(0).x(0), // i64.store16
            32 => self.c.x(0x3e).x(0).x(0), // i64.store32
            _ => self.c.store64(0),
        };
    }

    fn op_bits(&self, i: u32) -> u32 {
        match self.ins.op_kind(i) {
            OpKind::Register => Self::reg(self.ins.op_register(i)).map_or(0, |r| r.1),
            OpKind::Memory => self.ins.memory_size().size() as u32 * 8,
            _ => 0,
        }
    }

    /// Pushes operand `i` (i64, zero-extended to 64 from `bits`). Memory: at L_A (addr() done).
    fn read_op(&mut self, i: u32, bits: u32) {
        match self.ins.op_kind(i) {
            OpKind::Register => self.get_reg(self.ins.op_register(i)),
            OpKind::Memory => self.load(bits),
            _ => {
                let v = self.ins.immediate(i) as i64;
                self.c.i64c(v & mask(bits));
            }
        }
    }

    fn write_op(&mut self, i: u32, bits: u32, v: u32) {
        match self.ins.op_kind(i) {
            OpKind::Register => self.set_reg(self.ins.op_register(i), v),
            _ => self.store(bits, v),
        }
    }

    fn simple(&self, i: u32) -> bool {
        match self.ins.op_kind(i) {
            OpKind::Register => Self::reg(self.ins.op_register(i)).is_some(),
            OpKind::Memory => true,
            OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate32 | OpKind::Immediate64 | OpKind::Immediate8to16 | OpKind::Immediate8to32 | OpKind::Immediate8to64 | OpKind::Immediate32to64 => true,
            OpKind::NearBranch64 => true,
            _ => false,
        }
    }

    fn has_mem(&self) -> bool {
        (0..self.ins.op_count()).any(|i| self.ins.op_kind(i) == OpKind::Memory)
    }

    // ---- flags

    /// Sign-extends the i64 on the stack from `bits`.
    fn sext(&mut self, bits: u32) {
        match bits {
            8 => self.c.x(0xc2),  // i64.extend8_s
            16 => self.c.x(0xc3), // i64.extend16_s
            32 => self.c.x(0xc4), // i64.extend32_s
            _ => &mut self.c,
        };
    }

    /// Pushes `a op b` (i64 locals; `op` an i64 binary opcode) cut to `bits`, zero-extended.
    /// 32 bits: the i32 form (no masks for V8 to keep).
    fn bin(&mut self, a: u32, b: u32, op: u8, bits: u32) {
        if bits == 32 {
            // (each i64 opcode here is its i32 one + 0x12)
            self.c.get(a).x(I32_WRAP).get(b).x(I32_WRAP).x(op - 0x12).x(I64_EXT_U);
        } else {
            self.c.get(a).get(b).x(op);
            if bits < 64 {
                self.c.i64c(mask(bits)).x(I64_AND);
            }
        }
    }

    /// Pushes bit `bits - 1` of the i64 on the stack, as i32.
    fn sign_bit(&mut self, bits: u32) {
        self.c.i64c(bits as i64 - 1).x(I64_SHR_U).i64c(1).x(I64_AND).x(I32_WRAP);
    }

    fn zf(&mut self) {
        self.c.get(L_FR).x(I64_EQZ);
    }
    fn sf(&mut self, bits: u32) {
        self.c.get(L_FR);
        if bits == 32 {
            self.c.x(I32_WRAP).i32c(0).lt_s();
        } else {
            self.sign_bit(bits);
        }
    }
    fn pf(&mut self) {
        self.c.get(L_FR).i64c(0xff).x(I64_AND).x(I64_POPCNT).i64c(1).x(I64_AND).x(I64_EQZ);
    }
    /// OF of ADD (`add`) or SUB
    fn of_addsub(&mut self, add: bool, bits: u32) {
        self.c.get(L_FA).get(L_FB).x(I64_XOR);
        if add {
            self.c.i64c(-1).x(I64_XOR);
        }
        self.c.get(L_FA).get(L_FR).x(I64_XOR).x(I64_AND);
        self.sign_bit(bits);
    }
    /// CF of ADC (`adc`) or SBB, the carry in in L_FC (i32 0/1): a + b + c wrapped below a (or
    /// to a with a carry), a < b + c.
    fn cf_carry(&mut self, adc: bool) {
        if adc {
            self.c.get(L_FR).get(L_FA).x(I64_LT_U);
            self.c.get(L_FR).get(L_FA).x(I64_EQ).get(L_FC).and().or();
        } else {
            self.c.get(L_FA).get(L_FB).x(I64_LT_U);
            self.c.get(L_FA).get(L_FB).x(I64_EQ).get(L_FC).and().or();
        }
    }

    /// The status flags (i32 on the stack) into EFLAGS, all of them (no lazy record).
    fn status_exact(&mut self) {
        let t = L_ST; // (free outside exits)
        self.c.set(t);
        self.c.get(L_CPU);
        self.c.get(L_CPU).load32(cpu_off!(eflags)).u32c(!STATUS).and().get(t).or().store32(cpu_off!(eflags));
        self.c.get(L_CPU).i32c(0).store32(offset_of!(Cpu, lazy) as u32);
        self.flags = Flags::Cpu;
    }

    /// CF of a MUL: whether the product did not fit (i32 0/1)
    fn mul_overflow(&mut self, bits: u32) {
        if bits < 64 {
            self.c.get(L_FA);
            self.sext(bits);
            self.c.get(L_FB);
            self.sext(bits);
            self.c.x(I64_MUL);
            self.c.get(L_FR);
            self.sext(bits);
            self.c.x(I64_NE);
        } else {
            // r = a*b wrapped: it fit unless r/b != a (b == -1: only a == MIN overflows)
            self.c.get(L_FB).i64c(-1).x(I64_EQ).if_i32();
            self.c.get(L_FA).i64c(i64::MIN).x(I64_EQ);
            self.c.else_();
            self.c.get(L_FB).x(I64_EQZ).if_i32().i32c(0).else_();
            self.c.get(L_FR).get(L_FB).x(I64_DIV_S).get(L_FA).x(I64_NE);
            self.c.end();
            self.c.end();
        }
    }

    /// Pushes condition `cc` (i32 0/1).
    fn cond(&mut self, cc: ConditionCode) {
        use ConditionCode as C;
        let (base, neg) = match cc {
            C::o => (C::o, false),
            C::no => (C::o, true),
            C::b => (C::b, false),
            C::ae => (C::b, true),
            C::e => (C::e, false),
            C::ne => (C::e, true),
            C::be => (C::be, false),
            C::a => (C::be, true),
            C::s => (C::s, false),
            C::ns => (C::s, true),
            C::p => (C::p, false),
            C::np => (C::p, true),
            C::l => (C::l, false),
            C::ge => (C::l, true),
            C::le => (C::le, false),
            C::g => (C::le, true),
            C::None => {
                self.c.i32c(1);
                return;
            }
        };
        if !self.cond_lazy(base) {
            self.flags_to_cpu();
            self.cond_from_eflags(base);
        } else if DEBUG_CHECK.load(Relaxed) {
            // (debugging: the lazy condition against the materialized flags; a mismatch goes to
            // the status import as op 0xdead)
            let f = self.flags;
            self.c.set(L_DBG);
            self.flags_to_cpu();
            self.flags = f;
            self.cond_from_eflags(base);
            self.c.get(L_DBG).ne().if_();
            self.c.u32c(0xdead).u32c(self.ins.ip() as u32).u32c(base as u32).get(L_DBG).get(L_CPU).load32(cpu_off!(eflags)).call(F_STATUS).drop_();
            self.c.end();
            self.c.get(L_DBG);
        }
        if neg {
            self.c.eqz();
        }
    }

    /// Pushes condition `base` from the Cpu's flags (the eflags import).
    fn cond_from_eflags(&mut self, base: ConditionCode) {
        use ConditionCode as C;
        self.c.get(L_CPU).call(F_EFLAGS);
        let t = L_ST; // (free outside exits)
        self.c.set(t);
        let bit = |c: &mut Code, b: u32| {
            c.get(t).u32c(b).and().i32c(0).ne();
        };
        match base {
            C::o => bit(&mut self.c, OF),
            C::b => bit(&mut self.c, CF),
            C::e => bit(&mut self.c, ZF),
            C::be => {
                self.c.get(t).u32c(CF | ZF).and().i32c(0).ne();
            }
            C::s => bit(&mut self.c, SF),
            C::p => bit(&mut self.c, PF),
            C::l => {
                // SF != OF: bit 7 vs bit 11
                self.c.get(t).i32c(4).shr_u().get(t).xor().u32c(SF).and().i32c(0).ne();
            }
            _ => {
                self.c.get(t).i32c(4).shr_u().get(t).xor().u32c(SF).and().i32c(0).ne();
                bit(&mut self.c, ZF);
                self.c.or();
            }
        }
    }

    /// The condition from the lazy operation, if this kind gives it directly.
    fn cond_lazy(&mut self, cc: ConditionCode) -> bool {
        use ConditionCode as C;
        let Flags::Lazy(kind, bits) = self.flags else { return false };
        match (kind, cc) {
            (_, C::e) if kind != MUL => self.zf(),
            (_, C::s) if kind != MUL => self.sf(bits),
            (_, C::p) if kind != MUL => self.pf(),
            (SUB, C::b) => {
                self.c.get(L_FA).get(L_FB).x(I64_LT_U);
            }
            (SUB, C::be) => {
                self.c.get(L_FA).get(L_FB).x(I64_LE_U);
            }
            (SUB, C::l) if bits == 32 => {
                self.c.get(L_FA).x(I32_WRAP).get(L_FB).x(I32_WRAP).lt_s();
            }
            (SUB, C::l) => {
                self.c.get(L_FA);
                self.sext(bits);
                self.c.get(L_FB);
                self.sext(bits);
                self.c.x(I64_LT_S);
            }
            (SUB, C::le) if bits == 32 => {
                self.c.get(L_FA).x(I32_WRAP).get(L_FB).x(I32_WRAP).le_s();
            }
            (SUB, C::le) => {
                self.c.get(L_FA);
                self.sext(bits);
                self.c.get(L_FB);
                self.sext(bits);
                self.c.x(I64_LE_S);
            }
            (SUB, C::o) => self.of_addsub(false, bits),
            (ADD, C::b) => {
                self.c.get(L_FR).get(L_FA).x(I64_LT_U);
            }
            (ADD, C::be) => {
                self.c.get(L_FR).get(L_FA).x(I64_LT_U);
                self.zf();
                self.c.or();
            }
            (ADD, C::o) | (ADC, C::o) => self.of_addsub(true, bits),
            (SBB, C::o) => self.of_addsub(false, bits),
            (ADC, C::b) | (SBB, C::b) => self.cf_carry(kind == ADC),
            (ADD, C::l) => {
                self.sf(bits);
                self.of_addsub(true, bits);
                self.c.xor();
            }
            (ADD, C::le) => {
                self.sf(bits);
                self.of_addsub(true, bits);
                self.c.xor();
                self.zf();
                self.c.or();
            }
            (LOGIC, C::b) | (LOGIC, C::o) => {
                self.c.i32c(0);
            }
            (LOGIC, C::be) => self.zf(),
            (LOGIC, C::l) => self.sf(bits),
            (LOGIC, C::le) => {
                self.sf(bits);
                self.zf();
                self.c.or();
            }
            (MUL, C::b) | (MUL, C::o) => self.mul_overflow(bits),
            _ => return false,
        }
        true
    }

    /// The status flags (i32) of the lazy operation of `bits`-wide operands, computed.
    fn status(&mut self, kind: u32, bits: u32) {
        // ZF SF PF
        self.zf();
        self.c.i32c(6).shl();
        self.sf(bits);
        self.c.i32c(7).shl().or();
        self.pf();
        self.c.i32c(2).shl().or();
        let af = |g: &mut Self| {
            g.c.get(L_FA).get(L_FB).x(I64_XOR).get(L_FR).x(I64_XOR).i64c(4).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().i32c(4).shl().or();
        };
        match kind {
            ADD | SUB => {
                if kind == ADD {
                    self.c.get(L_FR).get(L_FA).x(I64_LT_U);
                } else {
                    self.c.get(L_FA).get(L_FB).x(I64_LT_U);
                }
                self.c.or();
                self.of_addsub(kind == ADD, bits);
                self.c.i32c(11).shl().or();
                af(self);
            }
            ADC | SBB => {
                self.cf_carry(kind == ADC);
                self.c.or();
                self.of_addsub(kind == ADC, bits);
                self.c.i32c(11).shl().or();
                af(self);
            }
            INC | DEC => {
                self.c.get(L_FC).i32c(1).and().or();
                let min = if bits == 64 { i64::MIN } else { 1 << (bits - 1) };
                self.c.get(if kind == INC { L_FR } else { L_FA }).i64c(min).x(I64_EQ).i32c(11).shl().or();
                // AF: bit 4 of a ^ 1 ^ r
                self.c.get(L_FA).get(L_FR).x(I64_XOR).i64c(4).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().i32c(4).shl().or();
            }
            SHL => {
                // CF: the last bit out (b = count 1..bits-1); OF: MSB(r) ^ CF
                self.c.get(L_FA).i64c(bits as i64).get(L_FB).x(I64_SUB).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().tee(L_ST).or();
                self.c.get(L_FR);
                self.sign_bit(bits);
                self.c.get(L_ST).xor().i32c(11).shl().or();
            }
            SHR => {
                self.c.get(L_FA).get(L_FB).i64c(1).x(I64_SUB).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().or();
                self.c.get(L_FA);
                self.sign_bit(bits);
                self.c.i32c(11).shl().or();
            }
            SAR => {
                self.c.get(L_FA).get(L_FB).i64c(1).x(I64_SUB).x(I64_SHR_S).x(I32_WRAP).i32c(1).and().or();
            }
            MUL => {
                self.mul_overflow(bits);
                self.c.u32c(CF | OF).mul().or();
            }
            _ => {} // LOGIC: CF OF AF 0
        }
    }

    /// Puts the lazy flags into the Cpu (state Cpu afterwards).
    fn flags_to_cpu(&mut self) {
        let Flags::Lazy(kind, bits) = self.flags else { return };
        use crate::lazy::Lazy;
        let lazy = offset_of!(Cpu, lazy) as u32;
        if bits == 64 && !matches!(kind, ADD | SUB | ADC | SBB | LOGIC | INC | DEC | MUL) {
            // (64-bit shifts: the flags themselves)
            self.c.get(L_CPU);
            self.c.get(L_CPU).load32(cpu_off!(eflags)).u32c(!STATUS).and();
            self.status(kind, 64);
            self.c.or().store32(cpu_off!(eflags));
            self.c.get(L_CPU).i32c(0).store32(lazy);
        } else {
            let size = match bits {
                8 => 0,
                16 => 1,
                32 => 2,
                _ => 3,
            };
            self.c.get(L_CPU).u32c(kind * 4 + size).store32(lazy);
            for (o, l) in [(offset_of!(Lazy, a), L_FA), (offset_of!(Lazy, b), L_FB), (offset_of!(Lazy, r), L_FR)] {
                self.c.get(L_CPU).get(l).store64(lazy + o as u32);
            }
            self.c.get(L_CPU);
            if kind == MUL {
                self.mul_overflow(bits);
            } else {
                self.c.get(L_FC);
            }
            self.c.store32(lazy + offset_of!(Lazy, c) as u32);
        }
        self.flags = Flags::Cpu;
    }

    /// The status flags into EFLAGS itself (the lazy record cleared), for an instruction that
    /// changes some of them and keeps the rest. Flags already in the Cpu may still be a lazy
    /// record (from the interpreter or an exit): the host works it out first.
    fn flags_to_eflags(&mut self) {
        let lazy = offset_of!(Cpu, lazy) as u32;
        match self.flags {
            Flags::Lazy(kind, bits) => {
                self.c.get(L_CPU);
                self.c.get(L_CPU).load32(cpu_off!(eflags)).u32c(!STATUS).and();
                self.status(kind, bits);
                self.c.or().store32(cpu_off!(eflags));
                self.c.get(L_CPU).i32c(0).store32(lazy);
                self.flags = Flags::Cpu;
            }
            Flags::Cpu => {
                // (a lazy record left by an exit or the interpreter: EFLAGS worked out)
                self.c.get(L_CPU).load32(lazy).if_();
                self.c.get(L_CPU).call(F_EFLAGS).drop_();
                self.c.end();
            }
        }
    }

    /// Records a flag-setting operation whose operands/result are in L_FA L_FB L_FR.
    fn set_flags(&mut self, kind: u32, bits: u32) {
        self.flags = Flags::Lazy(kind, bits);
    }

    // ---- exits

    /// Leaves with the target (i64) on the stack and status `st`, the locals' state going to
    /// the Cpu.
    fn exit_dyn(&mut self, st: i32) {
        self.c.set(L_NT);
        // (the code after this, if any, is another path: it keeps the lazy flags)
        let f = self.flags;
        self.flags_to_cpu();
        self.flags = f;
        self.c.i32c(st).set(L_ST);
        // the exit block (inside the dispatch loop)
        self.c.br(self.depth);
    }

    fn exit_to(&mut self, target: u64, st: i32) {
        self.c.i64c(target as i64);
        self.exit_dyn(st);
    }

    /// Leaves so the interpreter runs the current instruction.
    fn bail(&mut self) {
        if !self.hi_swapped.is_empty() {
            let r = self.hi_swapped.clone();
            self.swap_hi(&r);
        }
        let ip = self.ins.ip();
        self.exit_to(ip, RET_INTERP);
    }

    /// Goes to `target`: a block of the region when it is one, else out.
    fn goto(&mut self, target: u64) {
        let Some(&k) = self.index.get(&target) else {
            self.exit_to(target, 0);
            return;
        };
        if self.blocks[k].flags_in {
            self.flags_to_cpu();
        }
        if target <= self.ins.ip() {
            // backward: count down the budget
            let b = cpu_off!(jit_budget);
            self.c.get(L_CPU).get(L_CPU).load32(b).i32c(1).sub().store32(b);
            self.c.get(L_CPU).load32(b).i32c(0).le_s().if_();
            self.depth += 1;
            self.exit_to(target, 0);
            self.depth -= 1;
            self.c.end();
        }
        self.c.u32c(k as u32).set(L_BLK).br(self.depth + 1);
    }

    /// The exit block's way on when the target (L_NT) has a region and the budget lasts: a
    /// block of this region goes on in the dispatch loop (the registers still in locals),
    /// another region's is a tail call of it with the state in the Cpu (no dispatcher round
    /// trip). Code here is inside the dispatch loop only; `hi` is the region's 4GB alias.
    fn chain(&mut self, hi: u32) {
        let c = &mut self.c;
        let (hash, maskf, b) = (cpu_off!(jit_hash), cpu_off!(jit_mask), cpu_off!(jit_budget));
        let mask = (1u32 << ENTRY_SHIFT) - 1;
        c.get(L_ST).eqz().if_(); // loop 1
        c.get(L_CPU).get(L_CPU).load32(b).i32c(1).sub().store32(b);
        c.get(L_CPU).load32(b).i32c(0).gt_s();
        // (address 0 is the key of an empty slot: never a region)
        c.get(L_NT).x(I32_WRAP).i32c(0).ne().and().if_(); // loop 2
        // (L_H holds the mask first: dispatch::slot)
        c.get(L_CPU).load32(maskf).set(L_H).get(L_NT).x(I32_WRAP);
        super::dispatch::probe_slot(c, L_H);
        c.set(L_H);
        c.block(); // miss: loop 3
        c.loop_(); // probe: miss 1, loop 4
        c.get(L_CPU).load32(hash).get(L_H).i32c(3).shl().add().tee(L_E).load32(0).set(L_DBG);
        c.get(L_DBG).get(L_NT).x(I32_WRAP).eq().if_(); // probe 1, miss 2, loop 5
        c.get(L_E).load32(4).set(L_DBG);
        c.get(L_DBG).u32c(mask).and().get(L_ARG).u32c(mask).and().eq();
        c.get(L_NT).i64c(32).x(I64_SHR_U).x(I32_WRAP).u32c(hi).eq().and().if_(); // loop 6
        c.get(L_DBG).i32c(ENTRY_SHIFT as i32).shr_u().set(L_BLK).br(6);
        c.end();
        // (into the ring the host reads to merge regions that jump to each other a lot:
        // ring[0] counts, then RING_SIZE (from, to) pairs)
        let ring = cpu_off!(jit_ring);
        c.get(L_CPU).load32(ring).tee(L_H).get(L_H).load32(0).tee(L_E).i32c(1).add().store32(0);
        c.get(L_H).get(L_E).i32c(super::RING_SIZE as i32 - 1).and().i32c(3).shl().add().tee(L_H);
        c.get(L_ARG).u32c(mask).and().store32(4);
        c.get(L_H).get(L_NT).x(I32_WRAP).store32(8);
        self.store_regs();
        let c = &mut self.c;
        c.get(L_CPU).get(L_NT).x(I32_WRAP).store32(cpu_off!(eip));
        c.get(L_CPU).get(L_NT).i64c(32).x(I64_SHR_U).x(I32_WRAP).store32(cpu_off!(rip_hi));
        c.get(L_CPU).get(L_DBG);
        for _ in 2..N_PARAMS {
            c.i32c(0);
        }
        c.get(L_DBG).u32c(mask).and();
        c.op(0x13).op(2).op(0); // return_call_indirect (the region type), table 0
        c.end();
        c.get(L_DBG).eqz().br_if(1);
        c.get(L_H).i32c(1).add().get(L_CPU).load32(maskf).and().set(L_H).br(0);
        c.end(); // probe
        c.end(); // miss
        c.end();
        c.end();
    }

    /// Runs the current instruction through the `step` import; with `ends`, leaves after it at
    /// the Cpu's EIP.
    fn fallback(&mut self, ends: bool) {
        self.fell_back = true;
        self.flags_to_cpu();
        self.store_regs();
        let ip = self.ins.ip();
        self.c.get(L_CPU).u32c(ip as u32).store32(cpu_off!(eip));
        self.c.get(L_CPU).u32c((ip >> 32) as u32).store32(cpu_off!(rip_hi));
        self.c.get(L_CPU).call(F_STEP).tee(L_ST);
        if ends {
            Self::ret_status(&mut self.c, L_ST);
            return;
        }
        self.c.if_();
        Self::ret_status(&mut self.c, L_ST);
        self.c.end();
        self.load_regs();
        self.flags = Flags::Cpu;
        self.mx_checked = None;
        self.mx_seen = [[None; 2]; 16];
    }

    /// `return status` with the state already in the Cpu.
    fn ret_status(c: &mut Code, st: u32) {
        c.get(st).ret();
    }

    // ---- instructions

    fn push64(&mut self, v: u32) -> bool {
        let rsp = L_R + 4;
        self.c.get(rsp).x(I32_WRAP).i32c(8).sub().tee(L_A);
        if self.watch_store {
            self.c.get(L_A).set(L_W);
        }
        let span = GUEST_HI - GUEST_LO;
        self.c.u32c(GUEST_LO).sub().u32c(span - 8).gt_u().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
        self.store(64, v);
        self.c.get(rsp).i64c(8).x(I64_SUB).set(rsp);
        true
    }

    /// Pops into local `v`.
    fn pop64(&mut self, v: u32, extra: u32) {
        let rsp = L_R + 4;
        self.c.get(rsp).x(I32_WRAP).tee(L_A);
        let span = GUEST_HI - GUEST_LO;
        self.c.u32c(GUEST_LO).sub().u32c(span - 8).gt_u().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
        self.load(64);
        self.c.set(v);
        self.c.get(rsp).i64c(8 + extra as i64).x(I64_ADD).set(rsp);
    }

    /// One instruction; false if it ended the block.
    fn emit(&mut self) -> bool {
        // (a store in a block whose code may be rewritten: see Gen::smc, Gen::fresh)
        let size = if self.smc || self.fresh.is_some() { stored_bytes(&self.ins) } else { 0 };
        self.watch_store = size != 0;
        self.fell_back = false;
        let open = self.emit_one();
        if self.watch_store && open && !self.fell_back {
            if self.smc {
                self.smc_check(size);
            }
            if self.fresh.is_some() {
                self.fresh_store(size);
            }
        }
        self.watch_store = false;
        open
    }

    /// After the current instruction's store of `size` bytes at L_W: when a page it wrote has
    /// translated code (Cpu::jit_code_pages, a bit per 4 KB page), leaves for the host at the
    /// next instruction with Cpu::jit_smc = the address (kura's smc_hit drops the translations
    /// and keeps the page to the interpreter, as for koji::jit's 32-bit Gen::smc_check).
    fn smc_check(&mut self, size: u32) {
        for o in [0, size - 1] {
            if o > 0 {
                // (the last byte's page, when the store crosses into the next one)
                self.c.get(L_W).u32c(o).add().set(L_E);
            } else {
                self.c.get(L_W).set(L_E);
            }
            self.c.get(L_CPU).load32(cpu_off!(jit_code_pages));
            self.c.get(L_E).i32c(17).shr_u().i32c(2).shl().add().load32(0);
            self.c.get(L_E).i32c(12).shr_u().shr_u().i32c(1).and();
            if o > 0 {
                self.c.or();
            }
        }
        self.c.if_();
        self.depth += 1;
        self.c.get(L_CPU).get(L_W).store32(cpu_off!(jit_smc));
        if !self.hi_swapped.is_empty() {
            let r = self.hi_swapped.clone();
            self.swap_hi(&r);
        }
        let next = self.ins.next_ip();
        self.exit_to(next, RET_INTERP);
        self.depth -= 1;
        self.c.end();
    }

    /// The start of a block whose code is checked on entry ([`super::FRESH`]): unless its
    /// bytes are still `bytes` (those it was translated from), leaves for the interpreter at its
    /// start with Cpu::jit_stale = its address (kura's stale_hit drops the translations; a page
    /// that keeps changing goes to the interpreter), as koji::jit's 32-bit Gen::fresh_entry.
    fn fresh_entry(&mut self, a: u32, bytes: &[u8]) {
        for (i, (o, w)) in fresh_parts(bytes.len() as u32).into_iter().enumerate() {
            let mut v = [0u8; 8];
            v[..w as usize].copy_from_slice(&bytes[o as usize..(o + w) as usize]);
            let v = u64::from_le_bytes(v);
            self.c.u32c(a.wrapping_add(o));
            match w {
                8 => {
                    self.c.load64(0).i64c(v as i64).op(0x52); // i64.ne
                }
                4 => {
                    self.c.load32(0).u32c(v as u32).ne();
                }
                2 => {
                    self.c.load16u(0).u32c(v as u32).ne();
                }
                _ => {
                    self.c.load8u(0).u32c(v as u32).ne();
                }
            }
            if i > 0 {
                self.c.or();
            }
        }
        self.c.if_();
        self.depth += 1;
        self.c.get(L_CPU).u32c(a).store32(cpu_off!(jit_stale));
        let at = self.ins.ip();
        self.exit_to(at, RET_INTERP);
        self.depth -= 1;
        self.c.end();
    }

    /// After the current instruction's store of `size` bytes at L_W in a block checked on entry:
    /// if it wrote into the block's own bytes, leaves at the next instruction with
    /// Cpu::jit_smc = the block's start (the host keeps the page to the interpreter), as the
    /// 32-bit Gen::fresh_store.
    fn fresh_store(&mut self, size: u32) {
        let Some((a, n)) = self.fresh else { return };
        // a + n > w and w + size > a: (w + size - 1 - a) < n + size - 1
        self.c.get(L_W).u32c((size - 1).wrapping_sub(a)).add().u32c(n + size - 1).lt_u();
        self.c.if_();
        self.depth += 1;
        self.c.get(L_CPU).u32c(a).store32(cpu_off!(jit_smc));
        if !self.hi_swapped.is_empty() {
            let r = self.hi_swapped.clone();
            self.swap_hi(&r);
        }
        let next = self.ins.next_ip();
        self.exit_to(next, RET_INTERP);
        self.depth -= 1;
        self.c.end();
    }

    fn emit_one(&mut self) -> bool {
        let ins = self.ins;
        self.temps = 0;
        let xchg_mem = ins.mnemonic() == Mnemonic::Xchg && self.has_mem();
        let rep = (ins.has_rep_prefix() || ins.has_repne_prefix()) && !super::rep_ignored(&ins);
        if (ins.has_lock_prefix() || xchg_mem) && !rep && self.locked() {
            return true;
        }
        if ins.has_lock_prefix() || rep || xchg_mem {
            let ends = ins.flow_control() != FlowControl::Next;
            self.fallback(ends);
            return !ends;
        }
        let n = ins.op_count();
        let mi = ins.mnemonic() as usize;
        let off = DEBUG_OFF[mi / 64 % 32].load(Relaxed) & (1 << (mi % 64)) != 0;
        let vector = (0..n).any(|i| ins.op_kind(i) == OpKind::Register && (ins.op_register(i).is_vector_register() || ins.op_register(i).is_mm()))
            || ins.encoding() != iced_x86::EncodingKind::Legacy
            || ins.mnemonic() == Mnemonic::Emms;
        if !off && vector && self.sse() {
            if self.mx_seen.iter().any(|s| s[0].is_some()) {
                // (what the instruction writes: checked no more)
                use iced_x86::OpAccess;
                for u in iced_x86::InstructionInfoFactory::new().info(&ins).used_registers() {
                    // (a YMM register's lower half is the XMM register)
                    if (u.register().is_xmm() || u.register().is_ymm()) && !matches!(u.access(), OpAccess::Read | OpAccess::CondRead | OpAccess::None) {
                        self.mx_seen[u.register().number()] = [None; 2];
                    }
                }
            }
            return true;
        }
        let all_simple = !off && (0..n).all(|i| self.simple(i));
        let handled = all_simple && self.emit_ins();
        if handled {
            return ins.flow_control() == FlowControl::Next;
        }
        let ends = ins.flow_control() != FlowControl::Next;
        self.fallback(ends);
        !ends
    }

    /// SHL/SHR/SAR r/m32 or r/m64 by CL. A count of 0 (CL masked) changes no flag, so the
    /// flags are set (into the Cpu) only when it isn't.
    /// A locked read-modify-write (or xchg with memory) of an aligned dword or qword: one wasm
    /// atomic (else nothing emitted; unaligned: the interpreter).
    fn locked(&mut self) -> bool {
        use Mnemonic::*;
        let ins = self.ins;
        let m = ins.mnemonic();
        let n = ins.op_count();
        let Some(mi) = (0..n).find(|&i| ins.op_kind(i) == OpKind::Memory) else {
            return false;
        };
        let bits = self.op_bits(mi);
        let shape = match m {
            Inc | Dec => n == 1,
            Xadd | Cmpxchg => n == 2 && mi == 0 && ins.op1_kind() == OpKind::Register,
            Xchg => n == 2 && ins.op_kind(1 - mi) == OpKind::Register,
            Add | Sub | And | Or | Xor => n == 2 && mi == 0,
            _ => false,
        };
        let other_ok = n == 1 || matches!(self.op_bits(1 - mi), 0) || self.op_bits(1 - mi) == bits;
        if !shape || (bits != 32 && bits != 64) || !(0..n).all(|i| self.simple(i)) || !other_ok || !self.addr(bits / 8) {
            return false;
        }
        self.c.get(L_A).i32c(bits as i32 / 8 - 1).and().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
        // (i64.atomic.rmw.* or i64.atomic.rmw32.*_u: the group's first opcode + 1 or + 6)
        let (sub, align) = if bits == 64 { (1, 3) } else { (6, 2) };
        let (b, old) = (self.temp(), self.temp());
        if matches!(m, Inc | Dec) {
            // CF stays
            self.cond(ConditionCode::b);
            self.c.set(L_FC);
            self.c.i64c(1);
        } else {
            self.read_op(1 - mi, bits);
        }
        self.c.set(b);
        if m == Cmpxchg {
            let acc = if bits == 64 { Register::RAX } else { Register::EAX };
            self.c.get(L_A);
            self.get_reg(acc);
            self.c.set(L_FA).get(L_FA).get(b);
            self.c.atomic(0x48 + sub, align).set(old);
            // ZF from acc - old; the accumulator gets the old value when they differ
            self.c.get(old).set(L_FB);
            self.c.get(L_FA).get(L_FB).x(I64_SUB);
            if bits < 64 {
                self.c.i64c(mask(bits)).x(I64_AND);
            }
            self.c.set(L_FR);
            self.set_flags(SUB, bits);
            self.c.get(L_FA).get(L_FB).x(I64_NE).if_();
            self.depth += 1;
            self.set_reg(acc, old);
            self.depth -= 1;
            self.c.end();
            return true;
        }
        let group = match m {
            Add | Inc | Xadd => 0x1e,
            Sub | Dec => 0x25,
            And => 0x2c,
            Or => 0x33,
            Xor => 0x3a,
            _ => 0x41,
        };
        self.c.get(L_A).get(b).atomic(group + sub, align).set(old);
        if m == Xchg || m == Xadd {
            self.set_reg(ins.op_register(1 - mi), old);
        }
        if m == Xchg {
            return true;
        }
        self.c.get(old).set(L_FA).get(b).set(L_FB);
        self.c.get(L_FA).get(L_FB);
        let kind = match m {
            Add | Xadd => { self.c.x(I64_ADD); ADD }
            Inc => { self.c.x(I64_ADD); INC }
            Sub => { self.c.x(I64_SUB); SUB }
            Dec => { self.c.x(I64_SUB); DEC }
            And => { self.c.x(I64_AND); LOGIC }
            Or => { self.c.x(I64_OR); LOGIC }
            _ => { self.c.x(I64_XOR); LOGIC }
        };
        if bits < 64 {
            self.c.i64c(mask(bits)).x(I64_AND);
        }
        self.c.set(L_FR);
        self.set_flags(kind, bits);
        true
    }

    fn shift_cl(&mut self, bits: u32) -> bool {
        use Mnemonic::*;
        let m = self.ins.mnemonic();
        if self.has_mem() && !self.addr(bits / 8) {
            return false;
        }
        self.flags_to_cpu();
        self.read_op(0, bits);
        self.c.set(L_FA);
        self.get_reg(Register::CL);
        self.c.i64c(bits as i64 - 1).x(I64_AND).set(L_FB);
        self.c.get(L_FA);
        let kind = match m {
            Shl | Sal => {
                self.c.get(L_FB).x(I64_SHL);
                SHL
            }
            Shr => {
                self.c.get(L_FB).x(I64_SHR_U);
                SHR
            }
            _ => {
                self.sext(bits);
                self.c.get(L_FB).x(I64_SHR_S);
                SAR
            }
        };
        if bits < 64 {
            self.c.i64c(mask(bits)).x(I64_AND);
        }
        self.c.set(L_FR);
        self.c.get(L_FB).x(I64_EQZ).eqz().if_();
        self.set_flags(kind, bits);
        self.flags_to_cpu();
        self.c.end();
        // (count 0: the same value back; a 32-bit register still zero-extended)
        self.write_op(0, bits, L_FR);
        true
    }

    /// BT/BTS/BTR/BTC on a 32/64-bit register, or on memory by an immediate (a register
    /// offset into memory addresses a bit string: the interpreter's). Only CF changes.
    fn bit_test(&mut self) -> bool {
        use Mnemonic::*;
        let ins = self.ins;
        let bits = self.op_bits(0);
        let imm = ins.op_kind(1) == OpKind::Immediate8;
        if bits < 32 || (self.has_mem() && (!imm || !self.addr(bits / 8))) {
            return false;
        }
        self.flags_to_eflags();
        let (v, b) = (self.temp(), self.temp());
        self.read_op(0, bits);
        self.c.set(v);
        self.read_op(1, if imm { 8 } else { bits });
        self.c.i64c(bits as i64 - 1).x(I64_AND).set(b);
        let ef = cpu_off!(eflags);
        self.c.get(L_CPU);
        self.c.get(L_CPU).load32(ef).u32c(!CF).and();
        self.c.get(v).get(b).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().or().store32(ef);
        let m = ins.mnemonic();
        if m != Bt {
            self.c.get(v).i64c(1).get(b).x(I64_SHL);
            match m {
                Bts => self.c.x(I64_OR),
                Btr => self.c.i64c(-1).x(I64_XOR).x(I64_AND),
                _ => self.c.x(I64_XOR),
            };
            self.c.set(v);
            self.write_op(0, bits, v);
        }
        true
    }

    /// BSF/BSR r32/r64, r/m: the index of the lowest/highest set bit; a zero source sets ZF
    /// and leaves the destination as it was. The other flags are kept (as the interpreter
    /// does: they are undefined).
    fn bit_scan(&mut self) -> bool {
        let bits = self.op_bits(0);
        if bits < 32 || (self.has_mem() && !self.addr(bits / 8)) {
            return false;
        }
        self.flags_to_eflags();
        let t = self.temp();
        self.read_op(1, bits);
        self.c.set(t);
        let ef = cpu_off!(eflags);
        self.c.get(L_CPU);
        self.c.get(L_CPU).load32(ef).u32c(!ZF).and();
        self.c.get(t).x(I64_EQZ).i32c(6).shl().or().store32(ef);
        self.c.get(t).x(I64_EQZ).eqz().if_();
        if self.ins.mnemonic() == Mnemonic::Bsf {
            self.c.get(t).x(0x7a); // i64.ctz
        } else {
            self.c.i64c(63).get(t).x(0x79).x(I64_SUB); // 63 - i64.clz
        }
        let r = self.temp();
        self.c.set(r);
        self.write_op(0, bits, r);
        self.c.end();
        true
    }

    /// MUL/IMUL r/m32 or r/m64: EDX:EAX or RDX:RAX = EAX or RAX times the operand. SF ZF PF
    /// from the low half, CF = OF = the high half is not just the low half's extension, AF 0
    /// (the interpreter's).
    fn mul1(&mut self) -> bool {
        let bits = self.op_bits(0);
        let signed = self.ins.mnemonic() == Mnemonic::Imul;
        if bits < 32 || (self.has_mem() && !self.addr(bits / 8)) {
            return false;
        }
        let (a, b, lo, hi, t, u) = (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.read_op(0, bits);
        self.c.set(b);
        self.c.get(L_R);
        if bits == 32 {
            self.c.i64c(0xffff_ffff).x(I64_AND);
            if signed {
                self.sext(32);
                self.c.set(a);
                self.c.get(b);
                self.sext(32);
                self.c.set(b);
                self.c.get(a);
            }
            self.c.get(b).x(I64_MUL).set(t);
            self.c.get(t).i64c(0xffff_ffff).x(I64_AND).set(lo);
            self.c.get(t).i64c(32).x(I64_SHR_U).i64c(0xffff_ffff).x(I64_AND).set(hi);
            // (overflow: the product is not its low half extended)
            self.c.get(t);
            if signed {
                self.c.get(lo);
                self.sext(32);
            } else {
                self.c.get(lo);
            }
            self.c.x(I64_NE).set(L_A);
        } else {
            self.c.set(a);
            // the high half from 32-bit pieces: a1:a0 * b1:b0
            let m32 = 0xffff_ffffi64;
            let half = |c: &mut Code, v: u32, high: bool| {
                c.get(v);
                if high {
                    c.i64c(32).x(I64_SHR_U);
                } else {
                    c.i64c(m32).x(I64_AND);
                }
            };
            // t = a0*b0 >> 32; u = a1*b0; mid = t + (u & m32) + a0*b1 (wraps? no: < 2^64 + ...)
            half(&mut self.c, a, false);
            half(&mut self.c, b, false);
            self.c.x(I64_MUL).i64c(32).x(I64_SHR_U).set(t);
            half(&mut self.c, a, true);
            half(&mut self.c, b, false);
            self.c.x(I64_MUL).set(u);
            // t = t + lo32(u) + lo32(a0*b1); hi = a1*b1 + (u >> 32) + (a0*b1 >> 32) + (t >> 32)
            self.c.get(t).get(u).i64c(m32).x(I64_AND).x(I64_ADD).set(t);
            half(&mut self.c, a, false);
            half(&mut self.c, b, true);
            self.c.x(I64_MUL).set(lo); // (lo: a0*b1 for now)
            self.c.get(t).get(lo).i64c(m32).x(I64_AND).x(I64_ADD).set(t);
            half(&mut self.c, a, true);
            half(&mut self.c, b, true);
            self.c.x(I64_MUL);
            self.c.get(u).i64c(32).x(I64_SHR_U).x(I64_ADD);
            self.c.get(lo).i64c(32).x(I64_SHR_U).x(I64_ADD);
            self.c.get(t).i64c(32).x(I64_SHR_U).x(I64_ADD).set(hi);
            if signed {
                // signed high half: minus b if a < 0, minus a if b < 0
                self.c.get(hi);
                self.c.get(b).i64c(0).get(a).i64c(0).x(I64_LT_S).select().x(I64_SUB);
                self.c.get(a).i64c(0).get(b).i64c(0).x(I64_LT_S).select().x(I64_SUB).set(hi);
            }
            self.c.get(a).get(b).x(I64_MUL).set(lo);
            self.c.get(hi);
            if signed {
                self.c.get(lo).i64c(63).x(I64_SHR_S);
            } else {
                self.c.i64c(0);
            }
            self.c.x(I64_NE).set(L_A);
        }
        let (rl, rh) = if bits == 32 { (Register::EAX, Register::EDX) } else { (Register::RAX, Register::RDX) };
        self.set_reg(rl, lo);
        self.set_reg(rh, hi);
        self.c.get(lo).set(L_FR);
        self.zf();
        self.c.i32c(6).shl();
        self.sf(bits);
        self.c.i32c(7).shl().or();
        self.pf();
        self.c.i32c(2).shl().or();
        self.c.get(L_A).u32c(CF | OF).mul().or();
        self.status_exact();
        true
    }

    /// DIV/IDIV r/m32 or r/m64 when the quotient fits (else, and for a zero divisor, the
    /// interpreter raises the fault). 64-bit: only a dividend RDX:RAX that is RAX extended.
    /// The flags stay (undefined).
    fn div1(&mut self) -> bool {
        let bits = self.op_bits(0);
        let signed = self.ins.mnemonic() == Mnemonic::Idiv;
        if bits < 32 || (self.has_mem() && !self.addr(bits / 8)) {
            return false;
        }
        let (n, d, q, r) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.read_op(0, bits);
        if signed {
            self.sext(bits);
        }
        self.c.set(d);
        // the cases to leave to the interpreter: L_A
        self.c.get(d).x(I64_EQZ);
        let m32 = 0xffff_ffffi64;
        if bits == 32 {
            self.c.get(L_R + 2).i64c(m32).x(I64_AND).i64c(32).x(I64_SHL).get(L_R).i64c(m32).x(I64_AND).x(I64_OR).set(n);
            if signed {
                self.c.get(n).i64c(i64::MIN).x(I64_EQ).get(d).i64c(-1).x(I64_EQ).and().or();
            } else {
                // (the quotient fits unless EDX >= the divisor)
                self.c.get(L_R + 2).i64c(m32).x(I64_AND).get(d).x(I64_GE_U).or();
            }
        } else {
            self.c.get(L_R).set(n);
            if signed {
                self.c.get(L_R + 2).get(n).i64c(63).x(I64_SHR_S).x(I64_NE).or();
                self.c.get(n).i64c(i64::MIN).x(I64_EQ).get(d).i64c(-1).x(I64_EQ).and().or();
            } else {
                self.c.get(L_R + 2).i64c(0).x(I64_NE).or();
            }
        }
        self.c.if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
        self.c.get(n).get(d).x(if signed { I64_DIV_S } else { I64_DIV_U }).set(q);
        if bits == 32 && signed {
            // (a quotient that doesn't fit 32 bits)
            self.c.get(q);
            self.sext(32);
            self.c.get(q).x(I64_NE).if_();
            self.depth += 1;
            self.bail();
            self.depth -= 1;
            self.c.end();
        }
        self.c.get(n).get(d).x(if signed { I64_REM_S } else { I64_REM_U }).set(r);
        let (rq, rr) = if bits == 32 { (Register::EAX, Register::EDX) } else { (Register::RAX, Register::RDX) };
        self.set_reg(rq, q);
        self.set_reg(rr, r);
        true
    }

    /// TZCNT/LZCNT/POPCNT r, r/m (16 bits: the interpreter): ZF = a zero result, CF = a zero
    /// source (LZCNT: a result of the width), the rest 0 (POPCNT: ZF = zero source, CF 0).
    fn count_bits(&mut self) -> bool {
        use Mnemonic::*;
        let bits = self.op_bits(0);
        if bits < 32 || (self.has_mem() && !self.addr(bits / 8)) {
            return false;
        }
        let m = self.ins.mnemonic();
        let (v, r) = (self.temp(), self.temp());
        self.read_op(1, bits);
        self.c.set(v);
        match m {
            Popcnt => {
                self.c.get(v).x(I64_POPCNT).set(r);
            }
            Tzcnt => {
                // (i64.ctz of 0 is 64: a 32-bit zero gives 32)
                self.c.get(v);
                if bits == 32 {
                    self.c.i64c(1 << 32).x(I64_OR);
                }
                self.c.x(I64_CTZ).set(r);
            }
            _ => {
                self.c.get(v).x(I64_CLZ);
                if bits == 32 {
                    self.c.i64c(32).x(I64_SUB);
                }
                self.c.set(r);
            }
        }
        self.write_op(0, bits, r);
        if m == Popcnt {
            self.c.get(v).x(I64_EQZ).i32c(6).shl();
        } else {
            self.c.get(r).x(I64_EQZ).i32c(6).shl();
            self.c.get(v).x(I64_EQZ);
            if m == Lzcnt {
                self.c.get(r).i64c(bits as i64).x(I64_EQ).or();
            }
            self.c.or();
        }
        self.status_exact();
        true
    }

    /// ROL/ROR r/m32 or r/m64 by an immediate: CF and OF only (the others stay); a count of 0
    /// (masked) changes nothing (a 32-bit register still zero-extended).
    fn rotate(&mut self) -> bool {
        let bits = self.op_bits(0);
        if bits < 32 {
            return false;
        }
        let ins = self.ins;
        let count = (ins.immediate8() as u32) & (bits - 1);
        if self.has_mem() && !self.addr(bits / 8) {
            return false;
        }
        let (v, r) = (self.temp(), self.temp());
        self.read_op(0, bits);
        self.c.set(v);
        if count == 0 {
            self.write_op(0, bits, v);
            return true;
        }
        let rol = ins.mnemonic() == Mnemonic::Rol;
        if bits == 64 {
            self.c.get(v).i64c(count as i64).x(if rol { I64_ROTL } else { I64_ROTR }).set(r);
        } else {
            let (k, j) = if rol { (count, 32 - count) } else { (32 - count, count) };
            self.c.get(v).i64c(k as i64).x(I64_SHL).get(v).i64c(j as i64).x(I64_SHR_U).x(I64_OR).i64c(0xffff_ffff).x(I64_AND).set(r);
        }
        self.flags_to_eflags();
        // CF: ROL the low bit, ROR the top; OF: ROL top ^ CF, ROR top ^ the bit below it
        let ef = cpu_off!(eflags);
        self.c.get(L_CPU);
        self.c.get(L_CPU).load32(ef).u32c(!(CF | OF)).and();
        if rol {
            self.c.get(r).i64c(1).x(I64_AND).x(I32_WRAP);
            self.c.get(r).i64c(bits as i64 - 1).x(I64_SHR_U).x(I32_WRAP).get(r).x(I32_WRAP).xor().i32c(1).and().i32c(11).shl().or().or();
        } else {
            self.c.get(r).i64c(bits as i64 - 1).x(I64_SHR_U).x(I32_WRAP).i32c(1).and();
            self.c.get(r).i64c(bits as i64 - 1).x(I64_SHR_U).get(r).i64c(bits as i64 - 2).x(I64_SHR_U).x(I64_XOR).x(I32_WRAP).i32c(1).and().i32c(11).shl().or().or();
        }
        self.c.store32(ef);
        self.write_op(0, bits, r);
        true
    }

    /// The instruction if it is one this handles (else nothing emitted).
    fn emit_ins(&mut self) -> bool {
        use Mnemonic::*;
        let ins = self.ins;
        let m = ins.mnemonic();
        let n = ins.op_count();
        match m {
            // (MFENCE: store-load ordering, which wasm on ARM64 doesn't give by itself)
            Mfence => {
                self.c.op(0xfe).op(0x03).op(0x00); // atomic.fence
                true
            }
            Bsf | Bsr if n == 2 => self.bit_scan(),
            Bt | Bts | Btr | Btc => self.bit_test(),
            Nop | Endbr64 | Endbr32 | Pause | Lfence | Sfence | Prefetchnta | Prefetcht0 | Prefetcht1 | Prefetcht2 | Prefetchw => true,
            Mov => {
                let bits = self.op_bits(0).max(self.op_bits(1));
                if bits == 0 || (self.has_mem() && !self.addr(bits / 8)) {
                    return false;
                }
                let t = self.temp();
                self.read_op(1, bits);
                self.c.set(t);
                self.write_op(0, bits, t);
                true
            }
            Movzx | Movsx | Movsxd => {
                let (db, sb) = (self.op_bits(0), self.op_bits(1));
                if self.has_mem() && !self.addr(sb / 8) {
                    return false;
                }
                let t = self.temp();
                self.read_op(1, sb);
                if m != Movzx {
                    self.sext(sb);
                }
                self.c.set(t);
                self.write_op(0, db, t);
                true
            }
            Lea => {
                let db = self.op_bits(0);
                let t = self.temp();
                if !self.lea_value(db) {
                    return false;
                }
                self.c.set(t);
                self.write_op(0, db, t);
                true
            }
            Add | Sub | And | Or | Xor | Cmp | Test => {
                let bits = self.op_bits(0);
                if bits == 0 || (self.has_mem() && !self.addr(bits / 8)) {
                    return false;
                }
                // (xor r, r: a constant 0)
                self.read_op(0, bits);
                self.c.set(L_FA);
                self.read_op(1, bits);
                self.c.set(L_FB);
                let (op, kind) = match m {
                    Add => (I64_ADD, ADD),
                    Sub | Cmp => (I64_SUB, SUB),
                    And | Test => (I64_AND, LOGIC),
                    Or => (I64_OR, LOGIC),
                    _ => (I64_XOR, LOGIC),
                };
                self.bin(L_FA, L_FB, op, bits);
                self.c.set(L_FR);
                self.set_flags(kind, bits);
                if m != Cmp && m != Test {
                    self.write_op(0, bits, L_FR);
                }
                true
            }
            Inc | Dec | Neg | Not => {
                let bits = self.op_bits(0);
                if bits == 0 || (self.has_mem() && !self.addr(bits / 8)) {
                    return false;
                }
                match m {
                    Not => {
                        let t = self.temp();
                        self.read_op(0, bits);
                        self.c.i64c(-1).x(I64_XOR).set(t);
                        self.write_op(0, bits, t);
                    }
                    Neg => {
                        self.c.i64c(0).set(L_FA);
                        self.read_op(0, bits);
                        self.c.set(L_FB);
                        self.c.i64c(0).get(L_FB).x(I64_SUB);
                        if bits < 64 {
                            self.c.i64c(mask(bits)).x(I64_AND);
                        }
                        self.c.set(L_FR);
                        self.set_flags(SUB, bits);
                        self.write_op(0, bits, L_FR);
                    }
                    _ => {
                        // CF stays: keep it
                        self.cond(ConditionCode::b);
                        self.c.set(L_FC);
                        self.read_op(0, bits);
                        self.c.set(L_FA);
                        self.c.i64c(1).set(L_FB);
                        self.c.get(L_FA).i64c(1).x(if m == Inc { I64_ADD } else { I64_SUB });
                        if bits < 64 {
                            self.c.i64c(mask(bits)).x(I64_AND);
                        }
                        self.c.set(L_FR);
                        self.set_flags(if m == Inc { INC } else { DEC }, bits);
                        self.write_op(0, bits, L_FR);
                    }
                }
                true
            }
            Shl | Shr | Sar | Sal => {
                let bits = self.op_bits(0);
                let count = match ins.op_kind(1) {
                    OpKind::Immediate8 => ins.immediate8() as u32,
                    _ if ins.op_register(1) == Register::CL && bits >= 32 => return self.shift_cl(bits),
                    _ => return false,
                } & if bits == 64 { 63 } else { 31 };
                if bits == 0 || (bits < 32 && count >= bits) {
                    return false;
                }
                if count == 0 {
                    // (no change, flags included; the access still faults; a 32-bit register
                    // is still written: zero-extended)
                    if self.has_mem() {
                        return self.addr(bits / 8);
                    }
                    if bits == 32 {
                        let t = self.temp();
                        self.read_op(0, 32);
                        self.c.set(t);
                        self.write_op(0, 32, t);
                    }
                    return true;
                }
                if self.has_mem() && !self.addr(bits / 8) {
                    return false;
                }
                self.read_op(0, bits);
                self.c.set(L_FA);
                self.c.i64c(count as i64).set(L_FB);
                let kind = match m {
                    Shl | Sal => {
                        self.bin(L_FA, L_FB, I64_SHL, bits);
                        SHL
                    }
                    Shr => {
                        self.bin(L_FA, L_FB, I64_SHR_U, bits);
                        SHR
                    }
                    _ if bits == 32 => {
                        self.bin(L_FA, L_FB, I64_SHR_S, 32);
                        SAR
                    }
                    _ => {
                        self.c.get(L_FA);
                        self.sext(bits);
                        self.c.i64c(count as i64).x(I64_SHR_S);
                        if bits < 64 {
                            self.c.i64c(mask(bits)).x(I64_AND);
                        }
                        SAR
                    }
                };
                self.c.set(L_FR);
                self.set_flags(kind, bits);
                self.write_op(0, bits, L_FR);
                true
            }
            Imul if n >= 2 => {
                let bits = self.op_bits(0);
                if bits < 16 || (self.has_mem() && !self.addr(bits / 8)) {
                    return false;
                }
                let (a, b) = if n == 3 { (1, 2) } else { (0, 1) };
                self.read_op(a, bits);
                self.c.set(L_FA);
                self.read_op(b, bits);
                self.c.set(L_FB);
                self.bin(L_FA, L_FB, I64_MUL, bits);
                self.c.set(L_FR);
                self.set_flags(MUL, bits);
                self.write_op(0, bits, L_FR);
                true
            }
            Adc | Sbb => {
                let bits = self.op_bits(0);
                if bits == 0 || (self.has_mem() && !self.addr(bits / 8)) {
                    return false;
                }
                // the carry in (CF of what came before)
                self.cond(ConditionCode::b);
                self.c.set(L_FC);
                self.read_op(0, bits);
                self.c.set(L_FA);
                self.read_op(1, bits);
                self.c.set(L_FB);
                let op = if m == Adc { I64_ADD } else { I64_SUB };
                self.c.get(L_FA).get(L_FB).x(op).get(L_FC).x(I64_EXT_U).x(op);
                if bits < 64 {
                    self.c.i64c(mask(bits)).x(I64_AND);
                }
                self.c.set(L_FR);
                self.set_flags(if m == Adc { ADC } else { SBB }, bits);
                self.write_op(0, bits, L_FR);
                true
            }
            Mul | Imul if n == 1 => self.mul1(),
            Div | Idiv => self.div1(),
            Tzcnt | Lzcnt | Popcnt => self.count_bits(),
            Rol | Ror if ins.op_kind(1) == OpKind::Immediate8 => self.rotate(),
            Bswap if self.op_bits(0) >= 32 => {
                let bits = self.op_bits(0);
                let (v, r) = (self.temp(), self.temp());
                self.read_op(0, bits);
                self.c.set(v);
                // (bytes k and bits/8-1-k swap)
                self.c.i64c(0).set(r);
                for k in 0..bits / 8 {
                    let to = bits - 8 - 8 * k;
                    self.c.get(r).get(v).i64c(8 * k as i64).x(I64_SHR_U).i64c(0xff).x(I64_AND).i64c(to as i64).x(I64_SHL).x(I64_OR).set(r);
                }
                self.write_op(0, bits, r);
                true
            }
            Cmovo | Cmovno | Cmovb | Cmovae | Cmove | Cmovne | Cmovbe | Cmova | Cmovs | Cmovns | Cmovp | Cmovnp | Cmovl | Cmovge | Cmovle | Cmovg => {
                let bits = self.op_bits(0);
                if self.has_mem() && !self.addr(bits / 8) {
                    return false;
                }
                let t = self.temp();
                // (a 32-bit CMOV clears the upper half either way)
                self.read_op(1, bits);
                self.get_reg(ins.op_register(0));
                self.cond(ins.condition_code());
                self.c.select().set(t);
                self.write_op(0, bits, t);
                true
            }
            Seto | Setno | Setb | Setae | Sete | Setne | Setbe | Seta | Sets | Setns | Setp | Setnp | Setl | Setge | Setle | Setg => {
                if self.has_mem() && !self.addr(1) {
                    return false;
                }
                let t = self.temp();
                self.cond(ins.condition_code());
                self.c.x(I64_EXT_U).set(t);
                self.write_op(0, 8, t);
                true
            }
            Cdqe | Cwde | Cbw => {
                let (from, to) = match m {
                    Cdqe => (32, 64),
                    Cwde => (16, 32),
                    _ => (8, 16),
                };
                let t = self.temp();
                self.c.get(L_R);
                self.sext(from);
                self.c.set(t);
                let r = match to {
                    64 => Register::RAX,
                    32 => Register::EAX,
                    _ => Register::AX,
                };
                self.set_reg(r, t);
                true
            }
            Cdq | Cqo | Cwd => {
                let bits = match m {
                    Cqo => 64,
                    Cdq => 32,
                    _ => 16,
                };
                let t = self.temp();
                self.c.get(L_R);
                self.sext(bits);
                self.c.i64c(63).x(I64_SHR_S).set(t);
                let r = match bits {
                    64 => Register::RDX,
                    32 => Register::EDX,
                    _ => Register::DX,
                };
                self.set_reg(r, t);
                true
            }
            Xchg if ins.op0_kind() == OpKind::Register && ins.op1_kind() == OpKind::Register => {
                let bits = self.op_bits(0);
                let (t, u) = (self.temp(), self.temp());
                self.read_op(0, bits);
                self.c.set(t);
                self.read_op(1, bits);
                self.c.set(u);
                self.write_op(0, bits, u);
                self.write_op(1, bits, t);
                true
            }
            Push if self.op_bits(0) == 64 || matches!(ins.op0_kind(), OpKind::Immediate8to64 | OpKind::Immediate32to64) => {
                if self.has_mem() && !self.addr(8) {
                    return false;
                }
                let t = self.temp();
                self.read_op(0, 64);
                self.c.set(t);
                self.push64(t)
            }
            Pop if ins.op0_kind() == OpKind::Register && self.op_bits(0) == 64 => {
                let t = self.temp();
                self.pop64(t, 0);
                self.set_reg(ins.op_register(0), t);
                true
            }
            Jmp if ins.op0_kind() == OpKind::NearBranch64 => {
                self.goto(ins.near_branch64());
                true
            }
            Jo | Jno | Jb | Jae | Je | Jne | Jbe | Ja | Js | Jns | Jp | Jnp | Jl | Jge | Jle | Jg if ins.op0_kind() == OpKind::NearBranch64 => {
                // (each path puts the flags in the Cpu itself if its target reads them)
                let (t, f) = (ins.near_branch64(), ins.next_ip());
                self.cond(ins.condition_code());
                self.c.if_();
                self.depth += 1;
                let fl = self.flags;
                self.goto(t);
                self.flags = fl;
                self.depth -= 1;
                self.c.end();
                self.goto(f);
                true
            }
            Jmp if matches!(ins.op0_kind(), OpKind::Register | OpKind::Memory) && self.op_bits(0) == 64 => {
                if self.has_mem() && !self.addr(8) {
                    return false;
                }
                self.read_op(0, 64);
                self.exit_dyn(0);
                true
            }
            Call if ins.op0_kind() == OpKind::NearBranch64 => {
                let t = self.temp();
                self.c.i64c(ins.next_ip() as i64).set(t);
                self.push64(t);
                self.goto(ins.near_branch64());
                true
            }
            Call if matches!(ins.op0_kind(), OpKind::Register | OpKind::Memory) && self.op_bits(0) == 64 => {
                if self.has_mem() && !self.addr(8) {
                    return false;
                }
                let (t, r) = (self.temp(), self.temp());
                self.read_op(0, 64);
                self.c.set(t);
                self.c.i64c(ins.next_ip() as i64).set(r);
                self.push64(r);
                self.c.get(t);
                self.exit_dyn(0);
                true
            }
            Ret => {
                let extra = if ins.op_count() == 1 { ins.immediate16() as u32 } else { 0 };
                let t = self.temp();
                self.pop64(t, extra);
                // back to a call of the region: straight there (else through the exit's lookup)
                for &r in self.returns {
                    self.c.get(t).i64c(r as i64).x(I64_EQ).if_();
                    self.depth += 1;
                    let f = self.flags;
                    self.goto(r);
                    self.flags = f;
                    self.depth -= 1;
                    self.c.end();
                }
                self.c.get(t);
                self.exit_dyn(0);
                true
            }
            _ => false,
        }
    }

    /// LEA's address (i64, without the modulo) on the stack.
    /// Pushes LEA's address (i64), cut to `db` bits when the destination is 32 bits (worked
    /// out in i32 then: only the low halves count).
    fn lea_value(&mut self, db: u32) -> bool {
        let ins = self.ins;
        let base = ins.memory_base();
        let usable = |r: Register| r == Register::None || Self::reg(r).is_some_and(|(_, bits, high)| !high && bits >= 32);
        if !(base == Register::RIP || base == Register::EIP || usable(base)) || !usable(ins.memory_index()) {
            return false;
        }
        if base == Register::RIP || base == Register::EIP {
            self.c.i64c(ins.memory_displacement64() as i64);
            return true;
        }
        let w32 = db == 32;
        let mut any = false;
        for (r, scale) in [(base, 1u32), (ins.memory_index(), ins.memory_index_scale())] {
            if r == Register::None {
                continue;
            }
            let (l, bits, _) = Self::reg(r).unwrap();
            self.c.get(l);
            if w32 {
                self.c.x(I32_WRAP);
                if scale > 1 {
                    self.c.i32c(scale.trailing_zeros() as i32).shl();
                }
                if any {
                    self.c.add();
                }
            } else {
                if bits == 32 {
                    self.c.i64c(0xffff_ffff).x(I64_AND);
                }
                if scale > 1 {
                    self.c.i64c(scale.trailing_zeros() as i64).x(I64_SHL);
                }
                if any {
                    self.c.x(I64_ADD);
                }
            }
            any = true;
        }
        let d = ins.memory_displacement64() as i64;
        if w32 {
            if !any {
                self.c.i32c(d as i32);
            } else if d as i32 != 0 {
                self.c.i32c(d as i32).add();
            }
            self.c.x(I64_EXT_U);
        } else if !any {
            self.c.i64c(d);
        } else if d != 0 {
            self.c.i64c(d).x(I64_ADD);
        }
        true
    }
}

/// How a block of `n` bytes is compared on entry (Gen::fresh_entry): (offset, width) pieces of
/// 8 bytes (the last one ending at the end, overlapping), or one of 4/2/1 for short code. They
/// cover every byte and stay inside the block.
fn fresh_parts(n: u32) -> Vec<(u32, u32)> {
    let w = match n {
        8.. => 8,
        4..=7 => 4,
        2..=3 => 2,
        _ => 1,
    };
    let mut parts = Vec::new();
    let mut o = 0;
    while o + w < n {
        parts.push((o, w));
        o += w;
    }
    parts.push((n - w, w));
    parts
}

/// The bytes `ins` writes to memory (0: none), as one store (the widest of them).
fn stored_bytes(ins: &Instruction) -> u32 {
    use iced_x86::OpAccess;
    iced_x86::InstructionInfoFactory::new()
        .info(ins)
        .used_memory()
        .iter()
        .filter(|u| matches!(u.access(), OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite))
        .map(|u| u.memory_size().size() as u32)
        .max()
        .unwrap_or(0)
}

fn reads_flags(ins: &Instruction) -> (u32, u32) {
    // (iced's RflagsBits: OF 1, SF 2, ZF 4, AF 8, CF 0x10, PF 0x20)
    let all = 0x3f;
    let mut read = ins.rflags_read() & all;
    // (undefined flags are not counted as written: the interpreter keeps some of them, e.g. BSR)
    let written = (ins.rflags_written() | ins.rflags_cleared() | ins.rflags_set()) & all;
    // a shift or rotate by a count that may be 0 leaves the flags as they were: it reads them
    use Mnemonic::*;
    if matches!(ins.mnemonic(), Shl | Shr | Sar | Sal | Rol | Ror | Rcl | Rcr | Shld | Shrd) {
        let c = ins.op_count() - 1;
        let zero = match ins.op_kind(c) {
            // (31: conservative for 64-bit operands)
            OpKind::Immediate8 => ins.immediate8() & 31 == 0,
            _ => true,
        };
        if zero {
            read |= written;
        }
    }
    (read, written)
}

/// The bound a block ending in an unsigned conditional jump after `cmp r, imm` puts on `r` in
/// the block it goes to when r is in range: (block, r as 64-bit, values 0..n).
fn bound(ins: &[Instruction]) -> Option<(u64, Register, u64)> {
    let [.., c, j] = ins else { return None };
    if c.mnemonic() != Mnemonic::Cmp || c.op0_kind() != OpKind::Register || !matches!(c.op1_kind(), OpKind::Immediate8to32 | OpKind::Immediate32 | OpKind::Immediate8to64 | OpKind::Immediate32to64 | OpKind::Immediate8) {
        return None;
    }
    let r = c.op0_register();
    if !(r.is_gpr32() || r.is_gpr64()) {
        return None;
    }
    let v = c.immediate(1);
    let (to, n) = match j.mnemonic() {
        // ja default: the fall-through has r <= v; jae: r < v; jbe / jb: the target
        Mnemonic::Ja => (j.next_ip(), v.checked_add(1)?),
        Mnemonic::Jae => (j.next_ip(), v),
        Mnemonic::Jbe => (j.near_branch64(), v.checked_add(1)?),
        Mnemonic::Jb => (j.near_branch64(), v),
        _ => return None,
    };
    Some((to, r.full_register(), n.min(MAX_TABLE)))
}

/// The targets of a block ending in a jump through a table indexed by a register `n` bounds
/// (MinGW: `lea rB,[T]; movsxd rX,[rB+rI*4]; add rX,rB; jmp rX`; MSVC: `lea rB,[ImageBase];
/// mov eX,[rB+rI*4+T]; add rX,rB; jmp rX`; or `jmp [rB+rI*8]` of absolute addresses). Only for
/// finding blocks: the jump itself goes wherever the register says (Gen::chain).
fn jump_table<M: Memory>(mem: &mut M, ins: &[Instruction], n: Option<(Register, u64)>) -> Vec<u64> {
    use Mnemonic::*;
    let Some((ireg, n)) = n else { return Vec::new() };
    #[derive(Clone, Copy)]
    enum V {
        Const(u64),
        /// a 4-byte entry of the table at .0 (sign-extended: .1)
        Entry(u64, bool),
        /// an entry plus a base
        Target(u64, bool, u64),
    }
    let mut vals: Vec<(Register, V)> = Vec::new();
    let get = |vals: &Vec<(Register, V)>, r: Register| vals.iter().rev().find(|v| v.0 == r.full_register()).map(|v| v.1);
    let last = ins.last().unwrap();
    // the table's address of a memory operand [base + ireg*scale + disp]
    let table = |vals: &Vec<(Register, V)>, i: &Instruction, scale: u32| -> Option<u64> {
        if i.memory_index().full_register() != ireg || i.memory_index_scale() != scale || i.memory_segment() != Register::DS && i.memory_segment() != Register::None {
            return None;
        }
        let base = match i.memory_base() {
            Register::None => 0,
            b => match get(vals, b)? {
                V::Const(c) => c,
                _ => return None,
            },
        };
        Some(base.wrapping_add(i.memory_displacement64()))
    };
    let mut targets = Vec::new();
    for i in ins {
        let dst = if i.op_count() > 0 && i.op0_kind() == OpKind::Register { Some(i.op0_register()) } else { None };
        let v = match i.mnemonic() {
            Lea if i.memory_base() == Register::RIP && i.memory_index() == Register::None => Some(V::Const(i.memory_displacement64())),
            Movsxd if i.op1_kind() == OpKind::Memory => table(&vals, i, 4).map(|t| V::Entry(t, true)),
            Mov if i.op1_kind() == OpKind::Memory && dst.is_some_and(|r| r.is_gpr32()) => table(&vals, i, 4).map(|t| V::Entry(t, false)),
            Add if i.op1_kind() == OpKind::Register && dst.is_some() => match (get(&vals, dst.unwrap()), get(&vals, i.op1_register())) {
                (Some(V::Entry(t, s)), Some(V::Const(b))) | (Some(V::Const(b)), Some(V::Entry(t, s))) => Some(V::Target(t, s, b)),
                _ => None,
            },
            Jmp if i.ip() == last.ip() => {
                let at = |mem: &mut M, a: u64, k: u32| -> Option<u64> {
                    let mut b = [0u8; 8];
                    (mem.fetch(a as u32, &mut b[..k as usize]) == k as usize).then(|| u64::from_le_bytes(b))
                };
                match i.op0_kind() {
                    OpKind::Register => {
                        if let Some(V::Target(t, signed, b)) = get(&vals, i.op0_register()) {
                            for k in 0..n {
                                let Some(e) = at(mem, t + 4 * k, 4) else { break };
                                let e = if signed { e as u32 as i32 as i64 as u64 } else { e };
                                targets.push(b.wrapping_add(e));
                            }
                        }
                    }
                    OpKind::Memory if i.memory_size().size() == 8 => {
                        if let Some(t) = table(&vals, i, 8) {
                            for k in 0..n {
                                let Some(e) = at(mem, t + 8 * k, 8) else { break };
                                targets.push(e);
                            }
                        }
                    }
                    _ => {}
                }
                None
            }
            _ => None,
        };
        // (whatever else writes a register makes it unknown; the index must stay as checked
        // until the table is read)
        let reads_only = matches!(i.mnemonic(), Cmp | Test | Push | Jmp | Bt);
        if let Some(r) = dst.filter(|_| !reads_only).map(|r| r.full_register()) {
            if r == ireg && !matches!(v, Some(V::Entry(..))) && !vals.iter().any(|x| matches!(x.1, V::Entry(..))) {
                return Vec::new();
            }
            vals.retain(|x| x.0 != r);
            if let Some(v) = v {
                vals.push((r, v));
            }
        }
    }
    targets.sort_unstable();
    targets.dedup();
    targets
}

fn decode<M: Memory>(mem: &mut M, ip: u64, trap: (u32, u32)) -> Vec<Instruction> {
    let mut out = Vec::new();
    let mut at = ip;
    while out.len() < limit(&DEBUG_INS, MAX_INS) {
        let lo = at as u32;
        if !out.is_empty() && lo >= trap.0 && lo < trap.1 {
            break;
        }
        let mut buf = [0u8; 15];
        let n = mem.fetch(lo, &mut buf);
        let mut dec = Decoder::with_ip(64, &buf[..n], at, DecoderOptions::NONE);
        let ins = dec.decode();
        if ins.is_invalid() {
            break;
        }
        at = ins.next_ip();
        let fc = ins.flow_control();
        out.push(ins);
        if fc != FlowControl::Next {
            break;
        }
    }
    out
}

/// Translates the region at `roots` (full RIPs, one 4GB alias: the first's) into `module` as one
/// function, entered at any of its blocks. `accept`: whether another block (by its low 32 bits)
/// may join.
pub fn translate_region64<M: Memory>(module: &mut Module, mem: &mut M, roots: &[u64], trap: (u32, u32), accept: impl FnMut(u32, bool) -> bool) -> Option<Region> {
    // (each page read once: the instructions decoded, the bytes blocks check on entry and the
    // region's text agree although other threads may rewrite the code meanwhile)
    translate_seen64(module, &mut super::Snapshot::new(mem), roots, trap, accept)
}

fn translate_seen64<M: Memory>(module: &mut Module, mem: &mut M, roots: &[u64], trap: (u32, u32), mut accept: impl FnMut(u32, bool) -> bool) -> Option<Region> {
    let exact = super::mx_exact();
    let entry = roots[0];
    // blocks: the entry and what its direct jumps reach (accept(a, false)), and what may run
    // without having run yet (accept(a, true)): direct calls' targets and return addresses,
    // the targets of a jump table. Exits to a block of the region go on in it (Gen::chain), so
    // a call and its RET, or a switch, stay inside.
    let mut blocks: Vec<Block> = Vec::new();
    let mut index = BTreeMap::new();
    let mut todo: Vec<(u64, bool)> = roots.iter().rev().map(|&r| (r, false)).collect();
    // (an unsigned bound of a register checked by the jump into a block: (block, register, n))
    let mut bounds: Vec<(u64, Register, u64)> = Vec::new();
    let mut total = 0;
    while let Some((a, sure)) = todo.pop() {
        if index.contains_key(&a) || blocks.len() >= limit(&DEBUG_BLOCKS, MAX_BLOCKS) || total >= MAX_REGION_INS {
            continue;
        }
        if !roots.contains(&a) && !accept(a as u32, sure) {
            continue;
        }
        let ins = decode(mem, a, trap);
        if ins.is_empty() {
            if a == entry {
                return None;
            }
            continue;
        }
        total += ins.len();
        let last = *ins.last().unwrap();
        match last.flow_control() {
            FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch if last.op0_kind() == OpKind::NearBranch64 => {
                todo.push((last.near_branch64(), false));
                if last.flow_control() == FlowControl::ConditionalBranch {
                    todo.push((last.next_ip(), false));
                    if let Some(b) = bound(&ins) {
                        bounds.push(b);
                    }
                }
            }
            FlowControl::Call if last.op0_kind() == OpKind::NearBranch64 => {
                // (the callee after the caller's own code)
                todo.insert(0, (last.near_branch64(), true));
                todo.push((last.next_ip(), true));
            }
            FlowControl::Call | FlowControl::IndirectCall => todo.push((last.next_ip(), true)),
            FlowControl::IndirectBranch => {
                let n = bounds.iter().find(|b| b.0 == a).map(|b| (b.1, b.2));
                for t in jump_table(mem, &ins, n).into_iter().rev() {
                    todo.push((t, true));
                }
            }
            FlowControl::Next => todo.push((last.next_ip(), false)),
            _ => {}
        }
        index.insert(a, blocks.len());
        blocks.push(Block { start: a, ins, flags_in: true });
    }
    // flags a block reads before writing them all
    for b in &mut blocks {
        let mut written = 0;
        let mut live = None;
        for i in &b.ins {
            let (r, w) = reads_flags(i);
            if r & !written != 0 || matches!(i.flow_control(), FlowControl::Call | FlowControl::IndirectCall | FlowControl::Return | FlowControl::IndirectBranch | FlowControl::Interrupt | FlowControl::Exception) {
                live = Some(r & !written != 0 || written != 0x3f);
                break;
            }
            written |= w;
            if written == 0x3f {
                live = Some(false);
                break;
            }
        }
        b.flags_in = live.unwrap_or(true);
    }

    if module.types.is_empty() {
        module.types.push(FuncType { params: alloc::vec![I32], results: alloc::vec![I32] });
        module.types.push(FuncType { params: alloc::vec![I32; 5], results: alloc::vec![I32] });
        module.types.push(FuncType { params: alloc::vec![I32; N_PARAMS as usize], results: alloc::vec![I32] });
        module.imports.push(Import::Memory);
        module.imports.push(Import::Func("step", 0));
        module.imports.push(Import::Func("eflags", 0));
        module.imports.push(Import::Func("status", 1));
        let mut x87 = alloc::vec![I32; 3];
        x87.extend_from_slice(&[F64; 8]);
        module.types.push(FuncType { params: x87, results: alloc::vec![] });
        module.imports.push(Import::Func("x87", 3));
        module.imports.push(Import::Table("table"));
    }

    let n = blocks.len() as u32;
    let mut returns: Vec<u64> = blocks
        .iter()
        .filter_map(|b| b.ins.last())
        .filter(|l| l.flow_control() == FlowControl::Call && l.op0_kind() == OpKind::NearBranch64 && index.contains_key(&l.near_branch64()) && index.contains_key(&l.next_ip()))
        .map(|l| l.next_ip())
        .collect();
    returns.sort_unstable();
    returns.dedup();
    returns.truncate(MAX_RETURNS);
    let mut g = Gen { c: Code::default(), blocks: &blocks, index: &index, returns: &returns, flags: Flags::Cpu, depth: 0, ins: blocks[0].ins[0], temps: 0, mx_checked: None, mx_seen: [[None; 2]; 16], marks: Vec::new(), hi_swapped: Vec::new(), smc: false, fresh: None, watch_store: false, fell_back: false };
    // the dispatcher's key is the low 32 bits: code at another 4GB alias is left to the interpreter
    g.c.get(L_CPU).load32(cpu_off!(rip_hi)).u32c((entry >> 32) as u32).ne().if_();
    g.c.i32c(RET_INTERP).ret();
    g.c.end();
    g.c.i32c(0).set(L_FC);
    g.load_regs();
    g.c.get(L_ARG).i32c(ENTRY_SHIFT as i32).shr_u().set(L_BLK);
    // loop $top; block $exit; block b_{n-1} .. block b_0; br_table; end b_0: code; ...
    g.c.loop_();
    g.c.block();
    for _ in 0..n {
        g.c.block();
    }
    g.c.get(L_BLK);
    {
        // br_table: 0x0e vec(labels) default
        g.c.op(0x0e);
        let mut tmp = Vec::new();
        super::wasm::uleb(&mut tmp, n as u64);
        for k in 0..n {
            super::wasm::uleb(&mut tmp, k as u64);
        }
        super::wasm::uleb(&mut tmp, 0);
        for b in tmp {
            g.c.op(b);
        }
    }
    let mut count = 0;
    let mut layout = String::new();
    for k in 0..n as usize {
        g.c.end();
        // inside: blocks k+1..n-1, the exit block, the loop
        g.depth = n - 1 - k as u32;
        g.flags = Flags::Cpu;
        g.mx_checked = None;
        g.mx_seen = [[None; 2]; 16];
        g.smc = super::WATCH.get().is_some_and(|w| w(blocks[k].start as u32));
        let list = &blocks[k].ins;
        g.fresh = None;
        let start = blocks[k].start as u32;
        if super::fresh_at(start) {
            let end = list.last().unwrap().next_ip();
            let mut b = alloc::vec![0u8; end.wrapping_sub(blocks[k].start) as usize];
            if !b.is_empty() && mem.fetch(start, &mut b) == b.len() {
                g.ins = list[0];
                g.fresh_entry(start, &b);
                g.fresh = Some((start, b.len() as u32));
            }
        }
        let mut open = true;
        for ins in list {
            g.ins = *ins;
            count += 1;
            if !g.emit() {
                open = false;
                break;
            }
        }
        if open {
            // fell through (block size cap, a trap address, an undecodable instruction)
            let next = list.last().unwrap().next_ip();
            g.goto(next);
        }
        if super::LAYOUT.load(core::sync::atomic::Ordering::Relaxed) {
            layout.push_str(&alloc::format!("{:#x} {}\n", blocks[k].start, list.len()));
        }
    }
    g.c.end(); // exit: L_NT, L_ST (every block leaves by a branch)
    g.chain((entry >> 32) as u32);
    g.store_regs();
    g.c.get(L_CPU).get(L_NT).x(I32_WRAP).store32(cpu_off!(eip));
    g.c.get(L_CPU).get(L_NT).i64c(32).x(I64_SHR_U).x(I32_WRAP).store32(cpu_off!(rip_hi));
    Gen::ret_status(&mut g.c, L_ST);
    g.c.end(); // loop
    g.c.op(0x00); // unreachable
    let mut locals = alloc::vec![I64; N_I64 as usize];
    locals.extend_from_slice(&[I32; N_I32 as usize]);
    locals.extend_from_slice(&[V128; 2]);
    locals.push(V128);
    let code = splice_regs(core::mem::take(&mut g.c), &g.marks, DEBUG_ALL_REGS.load(Relaxed));
    module.funcs.push(Func { ty: 2, locals, code });
    let starts = blocks.iter().enumerate().map(|(k, b)| (b.start as u32, k as u32)).collect();
    let code: Vec<(u32, u32)> = blocks.iter().map(|b| {
        let end = b.ins.last().unwrap().next_ip();
        (b.start as u32, (end - b.start) as u32)
    }).collect();
    // (the bytes translated, as this translation read them)
    let text = super::code_text(mem, &code);
    Some(Region { starts, instructions: count, code, text, layout, profile: Vec::new(), sites: Vec::new(), exact })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoMem;
    impl Memory for NoMem {
        fn read8(&mut self, a: u32) -> Result<u8, crate::Fault> {
            Err(crate::Fault { addr: a, write: false })
        }
        fn write8(&mut self, a: u32, _: u8) -> Result<(), crate::Fault> {
            Err(crate::Fault { addr: a, write: true })
        }
    }

    /// A bounded block ending in an indirect jump with `add [mem], reg` in it (no register
    /// destination) has no table and is no panic; so are random instruction runs before a jump.
    #[test]
    fn jump_table_with_a_memory_destination() {
        let run = |bytes: &[u8]| {
            let mut dec = Decoder::with_ip(64, bytes, 0x1000, DecoderOptions::NONE);
            let ins: Vec<Instruction> = core::iter::from_fn(|| dec.can_decode().then(|| dec.decode())).filter(|i| !i.is_invalid()).collect();
            if ins.is_empty() {
                return None;
            }
            Some(jump_table(&mut NoMem, &ins, Some((Register::RCX, 4))))
        };
        // add [rax], rcx; jmp rax
        assert_eq!(run(&[0x48, 0x01, 0x08, 0xff, 0xe0]), Some(Vec::new()));
        let mut x = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..20000 {
            let mut b = [0u8; 14];
            for v in &mut b[..12] {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                // (mostly the opcodes the table reader looks at, with random operands)
                *v = if x >> 60 < 4 { [0x01, 0x03, 0x48, 0x8b, 0x8d, 0x63, 0x4c, 0x04][(x >> 8) as usize % 8] } else { x as u8 };
            }
            b[12..].copy_from_slice(&[0xff, 0xe0]);
            run(&b);
        }
    }

    /// (local index, opcode) of every local.get/set/tee `code.locs` lists, checking that each
    /// points at one
    fn local_ops(code: &Code) -> Vec<(u32, u8)> {
        code.locs
            .iter()
            .map(|&p| {
                let p = p as usize;
                let op = code.b[p];
                assert!((0x20..=0x22).contains(&op), "locs entry at {p} is opcode {op:#x}");
                let (mut l, mut shift, mut at) = (0u32, 0, p + 1);
                loop {
                    let c = code.b[at];
                    l |= ((c & 0x7f) as u32) << shift;
                    at += 1;
                    shift += 7;
                    if c & 0x80 == 0 {
                        break;
                    }
                }
                (l, op)
            })
            .collect()
    }

    #[test]
    fn regions_move_only_the_registers_they_name() {
        // rax read, rcx written, r9 read and written; a load mark at the top, stores at two exits
        let mut c = Code::default();
        let mut marks = Vec::new();
        marks.push((c.b.len(), c.locs.len(), false));
        c.get(L_R).set(L_R + 1);
        marks.push((c.b.len(), c.locs.len(), true));
        c.get(L_R + 9).i64c(1).x(I64_ADD).set(L_R + 9).i32c(7).set(L_ST);
        marks.push((c.b.len(), c.locs.len(), true));
        let out = splice_regs(c, &marks, false);
        let ops = local_ops(&out);
        let loaded: Vec<u32> = ops.iter().filter(|o| o.1 == 0x21 && (L_R..L_R + 16).contains(&o.0)).map(|o| o.0 - L_R).collect();
        // loads (local.set after the Cpu load) of rax, rcx, r9 first
        assert_eq!(&loaded[..3], &[0, 1, 9]);
        // the stores read rcx and r9 (written), never rax (only read) or anything else
        let mut named = 0u16;
        for &(l, op) in &ops {
            if op == 0x20 && (L_R..L_R + 16).contains(&l) {
                named |= 1 << (l - L_R);
            }
        }
        assert_eq!(named, 1 | 1 << 1 | 1 << 9);
        // (in the result every named register is written: its load)
        assert_eq!(regs_named(&out).0, 1 | 1 << 1 | 1 << 9);
        // all sixteen with DEBUG_ALL_REGS's flag
        let mut c = Code::default();
        let marks = [(0, 0, false), (0, 0, true)];
        c.get(L_R);
        let all = splice_regs(c, &marks, true);
        assert_eq!(regs_named(&all), (u16::MAX, u16::MAX));
    }

    /// Property: for random code and marks, splicing keeps every other byte in order, keeps
    /// `locs` pointing at the local ops (so compact_locals still works on the result), and
    /// puts at each mark exactly the loads of the named registers or the stores of the
    /// written ones.
    #[test]
    fn splice_regs_property() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut rnd = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as u32
        };
        for _ in 0..500 {
            let mut c = Code::default();
            let mut marks = Vec::new();
            let mut plain = Code::default(); // the same code without the marks
            let mut at_marks = Vec::new(); // (offset in plain, store)
            let n = 1 + rnd(40);
            for _ in 0..n {
                match rnd(6) {
                    0 => {
                        let store = rnd(2) == 1;
                        marks.push((c.b.len(), c.locs.len(), store));
                        at_marks.push((plain.b.len(), store));
                    }
                    1 => {
                        let l = L_R + rnd(16);
                        c.get(l);
                        plain.get(l);
                    }
                    2 => {
                        let l = L_R + rnd(16);
                        c.set(l);
                        plain.set(l);
                    }
                    3 => {
                        let l = L_R + rnd(16);
                        c.tee(l);
                        plain.tee(l);
                    }
                    4 => {
                        // other locals (and big indices: multi-byte LEBs)
                        let l = [L_CPU, L_ST, L_NT, L_V2, 300][rnd(5) as usize];
                        c.get(l);
                        plain.get(l);
                    }
                    _ => {
                        let v = rnd(1 << 20) as i32 - (1 << 19);
                        c.i32c(v);
                        plain.i32c(v);
                    }
                }
            }
            let (named, written) = regs_named(&plain);
            assert_eq!(written & !named, 0);
            let out = splice_regs(c, &marks, false);
            // the reference: the plain code with each mark's code put in by hand
            let mut want = Code::default();
            let mut from = 0;
            for &(at, store) in &at_marks {
                want.b.extend_from_slice(&plain.b[from..at]);
                if store {
                    emit_stores(&mut want, written);
                } else {
                    emit_loads(&mut want, named);
                }
                from = at;
            }
            want.b.extend_from_slice(&plain.b[from..]);
            assert_eq!(out.b, want.b);
            // locs: one per local op of the plain code plus those of the marks' code
            let ops = local_ops(&out);
            // (rax..rdi: two 32-bit halves, two Cpu gets each way; r8..r15 one)
            let ops_of = |mask: u16, low: usize, high: usize| (0..16).filter(|i| mask >> i & 1 != 0).map(|i| if i < 8 { low } else { high }).sum::<usize>();
            let per_load = ops_of(named, 3, 2); // (cpu get [x2], local set)
            let per_store = ops_of(written, 4, 2); // (cpu get, local get [x2])
            let extra: usize = at_marks.iter().map(|m| if m.1 { per_store } else { per_load }).sum();
            assert_eq!(ops.len(), plain.locs.len() + extra);
            assert_eq!(regs_named(&out).0 & !named, 0);
        }
    }
}
