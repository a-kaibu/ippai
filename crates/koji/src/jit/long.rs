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

use super::wasm::{Code, Func, FuncType, Import, Module, F64, I32, I64};
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
const MAX_BLOCKS: usize = 64;

/// Debugging a miscompile: instructions per block and blocks per region at most (0: the
/// defaults), and mnemonics (by their `Mnemonic` number) left to the interpreter.
pub static DEBUG_INS: AtomicUsize = AtomicUsize::new(0);
pub static DEBUG_BLOCKS: AtomicUsize = AtomicUsize::new(0);
pub static DEBUG_CHECK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
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
const N_I32: u32 = 5;

// EFLAGS bits
const CF: u32 = 1;
const PF: u32 = 4;
const AF: u32 = 0x10;
const ZF: u32 = 0x40;
const SF: u32 = 0x80;
const OF: u32 = 0x800;
const STATUS: u32 = CF | PF | AF | ZF | SF | OF;

// lazy kinds (crate::lazy's)
use crate::lazy::{ADD, DEC, INC, LOGIC, MUL, SAR, SHL, SHR, SUB};

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
    flags: Flags,
    /// blocks between the current code and the dispatch loop
    depth: u32,
    ins: Instruction,
    temps: u32,
}

impl<'a> Gen<'a> {
    fn temp(&mut self) -> u32 {
        let t = L_T + self.temps;
        self.temps += 1;
        assert!(self.temps <= N_T);
        t
    }

    // ---- registers

    fn load_regs(&mut self) {
        for i in 0..16 {
            if i < 8 {
                self.c.get(L_CPU).load32(off_lo(i)).x(I64_EXT_U);
                self.c.get(L_CPU).load32(off_hi(i)).x(I64_EXT_U).i64c(32).x(I64_SHL).x(I64_OR);
            } else {
                self.c.get(L_CPU).load64(off_lo(i));
            }
            self.c.set(L_R + i as u32);
        }
    }

    fn store_regs(&mut self) {
        for i in 0..16 {
            let r = L_R + i as u32;
            if i < 8 {
                self.c.get(L_CPU).get(r).x(I32_WRAP).store32(off_lo(i));
                self.c.get(L_CPU).get(r).i64c(32).x(I64_SHR_U).x(I32_WRAP).store32(off_hi(i));
            } else {
                self.c.get(L_CPU).get(r).store64(off_lo(i));
            }
        }
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
        if bits < 64 {
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
                self.c.get(v).i64c(0xffff_ffff).x(I64_AND).set(l);
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
        let span = GUEST_HI - GUEST_LO;
        self.c.u32c(GUEST_LO).sub().u32c(span - size).gt_u().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
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
        if bits < 64 {
            let s = 64 - bits as i64;
            self.c.i64c(s).x(I64_SHL).i64c(s).x(I64_SHR_S);
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
        self.sign_bit(bits);
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
            (SUB, C::l) => {
                self.c.get(L_FA);
                self.sext(bits);
                self.c.get(L_FB);
                self.sext(bits);
                self.c.x(I64_LT_S);
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
            (ADD, C::o) => self.of_addsub(true, bits),
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

    /// The status flags (i32) of the lazy operation, computed (64-bit operations).
    fn status64(&mut self, kind: u32) {
        let bits = 64;
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
            INC | DEC => {
                self.c.get(L_FC).i32c(1).and().or();
                self.c.get(if kind == INC { L_FR } else { L_FA }).i64c(i64::MIN).x(I64_EQ).i32c(11).shl().or();
                // AF: bit 4 of a ^ 1 ^ r
                self.c.get(L_FA).get(L_FR).x(I64_XOR).i64c(4).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().i32c(4).shl().or();
            }
            SHL => {
                // CF: the last bit out (b = count 1..63); OF: MSB(r) ^ CF
                self.c.get(L_FA).i64c(64).get(L_FB).x(I64_SUB).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().tee(L_ST).or();
                self.c.get(L_FR);
                self.sign_bit(64);
                self.c.get(L_ST).xor().i32c(11).shl().or();
            }
            SHR => {
                self.c.get(L_FA).get(L_FB).i64c(1).x(I64_SUB).x(I64_SHR_U).x(I32_WRAP).i32c(1).and().or();
                self.c.get(L_FA);
                self.sign_bit(64);
                self.c.i32c(11).shl().or();
            }
            SAR => {
                self.c.get(L_FA).get(L_FB).i64c(1).x(I64_SUB).x(I64_SHR_S).x(I32_WRAP).i32c(1).and().or();
            }
            MUL => {
                self.mul_overflow(64);
                self.c.u32c(CF | OF).mul().or();
            }
            _ => {} // LOGIC: CF OF AF 0
        }
    }

    /// Puts the lazy flags into the Cpu (state Cpu afterwards).
    fn flags_to_cpu(&mut self) {
        let Flags::Lazy(kind, bits) = self.flags else { return };
        let lazy = offset_of!(Cpu, lazy) as u32;
        if bits == 64 {
            self.c.get(L_CPU);
            self.c.get(L_CPU).load32(cpu_off!(eflags)).u32c(!STATUS).and();
            self.status64(kind);
            self.c.or().store32(cpu_off!(eflags));
            self.c.get(L_CPU).i32c(0).store32(lazy);
        } else {
            let size = match bits {
                8 => 0,
                16 => 1,
                _ => 2,
            };
            self.c.get(L_CPU).u32c(kind * 4 + size).store32(lazy);
            for (k, l) in [(1, L_FA), (2, L_FB), (3, L_FR)] {
                self.c.get(L_CPU).get(l).x(I32_WRAP).store32(lazy + 4 * k);
            }
            self.c.get(L_CPU);
            if kind == MUL {
                self.mul_overflow(bits);
            } else {
                self.c.get(L_FC);
            }
            self.c.store32(lazy + 16);
        }
        self.flags = Flags::Cpu;
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
        // the dispatch loop, then the exit block
        self.c.br(self.depth + 1);
    }

    fn exit_to(&mut self, target: u64, st: i32) {
        self.c.i64c(target as i64);
        self.exit_dyn(st);
    }

    /// Leaves so the interpreter runs the current instruction.
    fn bail(&mut self) {
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
        self.c.u32c(k as u32).set(L_BLK).br(self.depth);
    }

    /// Runs the current instruction through the `step` import; with `ends`, leaves after it at
    /// the Cpu's EIP.
    fn fallback(&mut self, ends: bool) {
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
    }

    /// `return status` with the state already in the Cpu.
    fn ret_status(c: &mut Code, st: u32) {
        c.get(st).ret();
    }

    // ---- instructions

    fn push64(&mut self, v: u32) -> bool {
        let rsp = L_R + 4;
        self.c.get(rsp).x(I32_WRAP).i32c(8).sub().tee(L_A);
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
        let ins = self.ins;
        self.temps = 0;
        if ins.has_lock_prefix() || ins.has_rep_prefix() || ins.has_repne_prefix() {
            let ends = ins.flow_control() != FlowControl::Next;
            self.fallback(ends);
            return !ends;
        }
        let n = ins.op_count();
        let mi = ins.mnemonic() as usize;
        let off = DEBUG_OFF[mi / 64 % 32].load(Relaxed) & (1 << (mi % 64)) != 0;
        let all_simple = !off && (0..n).all(|i| self.simple(i));
        let handled = all_simple && self.emit_ins();
        if handled {
            return ins.flow_control() == FlowControl::Next;
        }
        let ends = ins.flow_control() != FlowControl::Next;
        self.fallback(ends);
        !ends
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
                if !self.lea_value() {
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
                self.c.get(L_FA).get(L_FB);
                let kind = match m {
                    Add => {
                        self.c.x(I64_ADD);
                        ADD
                    }
                    Sub | Cmp => {
                        self.c.x(I64_SUB);
                        SUB
                    }
                    And | Test => {
                        self.c.x(I64_AND);
                        LOGIC
                    }
                    Or => {
                        self.c.x(I64_OR);
                        LOGIC
                    }
                    _ => {
                        self.c.x(I64_XOR);
                        LOGIC
                    }
                };
                if bits < 64 {
                    self.c.i64c(mask(bits)).x(I64_AND);
                }
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
                self.c.get(L_FA);
                let kind = match m {
                    Shl | Sal => {
                        self.c.i64c(count as i64).x(I64_SHL);
                        SHL
                    }
                    Shr => {
                        self.c.i64c(count as i64).x(I64_SHR_U);
                        SHR
                    }
                    _ => {
                        self.sext(bits);
                        self.c.i64c(count as i64).x(I64_SHR_S);
                        SAR
                    }
                };
                if bits < 64 {
                    self.c.i64c(mask(bits)).x(I64_AND);
                }
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
                self.c.get(L_FA).get(L_FB).x(I64_MUL);
                if bits < 64 {
                    self.c.i64c(mask(bits)).x(I64_AND);
                }
                self.c.set(L_FR);
                self.set_flags(MUL, bits);
                self.write_op(0, bits, L_FR);
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
                // (a jump to a block that reads the flags puts them in the Cpu: on both paths)
                let (t, f) = (ins.near_branch64(), ins.next_ip());
                let live = |a: u64| self.index.get(&a).is_none_or(|&k| self.blocks[k].flags_in);
                if live(t) || live(f) {
                    // the condition from the lazy record, then the record to the Cpu
                    self.cond(ins.condition_code());
                    self.c.set(L_A);
                    self.flags_to_cpu();
                    self.c.get(L_A);
                } else {
                    self.cond(ins.condition_code());
                }
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
                self.exit_to(ins.near_branch64(), 0);
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
                self.c.get(t);
                self.exit_dyn(0);
                true
            }
            _ => false,
        }
    }

    /// LEA's address (i64, without the modulo) on the stack.
    fn lea_value(&mut self) -> bool {
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
        let mut any = false;
        for (r, scale) in [(base, 1u32), (ins.memory_index(), ins.memory_index_scale())] {
            if r == Register::None {
                continue;
            }
            let (l, bits, _) = Self::reg(r).unwrap();
            self.c.get(l);
            if bits == 32 {
                self.c.i64c(0xffff_ffff).x(I64_AND);
            }
            if scale > 1 {
                self.c.i64c(scale.trailing_zeros() as i64).x(I64_SHL);
            }
            if any {
                self.c.x(I64_ADD);
            }
            any = true;
        }
        let d = ins.memory_displacement64() as i64;
        if !any {
            self.c.i64c(d);
        } else if d != 0 {
            self.c.i64c(d).x(I64_ADD);
        }
        true
    }
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

/// Translates the region at `entry` (its full RIP) into `module` as one function. `accept`:
/// whether another block (by its low 32 bits) may join.
pub fn translate_region64<M: Memory>(module: &mut Module, mem: &mut M, entry: u64, trap: (u32, u32), mut accept: impl FnMut(u32) -> bool) -> Option<Region> {
    // blocks: the entry and what its direct jumps reach
    let mut blocks: Vec<Block> = Vec::new();
    let mut index = BTreeMap::new();
    let mut todo = alloc::vec![entry];
    while let Some(a) = todo.pop() {
        if index.contains_key(&a) || blocks.len() >= limit(&DEBUG_BLOCKS, MAX_BLOCKS) {
            continue;
        }
        if a != entry && !accept(a as u32) {
            continue;
        }
        let ins = decode(mem, a, trap);
        if ins.is_empty() {
            if a == entry {
                return None;
            }
            continue;
        }
        let last = *ins.last().unwrap();
        match last.flow_control() {
            FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch if last.op0_kind() == OpKind::NearBranch64 => {
                todo.push(last.near_branch64());
                if last.flow_control() == FlowControl::ConditionalBranch {
                    todo.push(last.next_ip());
                }
            }
            FlowControl::Next => todo.push(last.next_ip()),
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
    let mut g = Gen { c: Code::default(), blocks: &blocks, index: &index, flags: Flags::Cpu, depth: 0, ins: blocks[0].ins[0], temps: 0 };
    // the dispatcher's key is the low 32 bits: code at another 4GB alias is left to the interpreter
    g.c.get(L_CPU).load32(cpu_off!(rip_hi)).u32c((entry >> 32) as u32).ne().if_();
    g.c.i32c(RET_INTERP).ret();
    g.c.end();
    g.c.i32c(0).set(L_FC);
    g.load_regs();
    g.c.get(L_ARG).i32c(ENTRY_SHIFT as i32).shr_u().set(L_BLK);
    // block $exit; loop $top; block b_{n-1} .. block b_0; br_table; end b_0: code; ...
    g.c.block();
    g.c.loop_();
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
        // inside: blocks k+1..n-1, the loop, the exit block
        g.depth = n - 1 - k as u32;
        g.flags = Flags::Cpu;
        let list = &blocks[k].ins;
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
    g.c.end(); // loop (unreachable at its end: every block leaves by a branch)
    g.c.op(0x00); // unreachable
    g.c.end(); // exit: L_NT, L_ST
    g.store_regs();
    g.c.get(L_CPU).get(L_NT).x(I32_WRAP).store32(cpu_off!(eip));
    g.c.get(L_CPU).get(L_NT).i64c(32).x(I64_SHR_U).x(I32_WRAP).store32(cpu_off!(rip_hi));
    Gen::ret_status(&mut g.c, L_ST);
    let mut locals = alloc::vec![I64; N_I64 as usize];
    locals.extend_from_slice(&[I32; N_I32 as usize]);
    let code = core::mem::take(&mut g.c);
    module.funcs.push(Func { ty: 2, locals, code });
    let starts = blocks.iter().enumerate().map(|(k, b)| (b.start as u32, k as u32)).collect();
    let code = blocks.iter().map(|b| {
        let end = b.ins.last().unwrap().next_ip();
        (b.start as u32, (end - b.start) as u32)
    }).collect();
    Some(Region { starts, instructions: count, code, layout, profile: Vec::new(), sites: Vec::new() })
}
