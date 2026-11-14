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

use super::wasm::{Code, Func, Module, I32, I64, V128};

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
/// (cpu, call cache slot, target) -> hash table entry of a function's root, or 0 (kura's
/// vnhost_jit_callmiss)
const F_CALLMISS: u32 = 4;
/// Guest calls nested as wasm calls at most (deeper ones leave for the dispatcher)
const MAX_DEPTH: u32 = 128;

const MAX_INS: usize = 120;
const MAX_BLOCKS: usize = 256;
/// Instructions in a region at most (V8's optimizing compiler takes superlinear time on big
/// functions; jit/mod.rs's MAX_REGION_INS)
const MAX_REGION_INS: usize = 1000;
/// Blocks in a function at most (FN64; its entries' numbers must stay below 0x7ff)
const MAX_FN_BLOCKS: usize = 2000;
/// Entries of a jump table at most
const MAX_TABLE: u64 = 512;
/// A RET compares its target with this many of the region's return addresses at most
const MAX_RETURNS: usize = 8;
/// FN64: the biggest leaf called inline (blocks, instructions).
const LEAF_BLOCKS: usize = 6;
const LEAF_INS: usize = 40;

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
/// functions (FN64): how many guest calls deep this one is (0 from the dispatcher; the
/// dispatcher passes 0 in this parameter, as the region type's lazy-flag operand)
const L_DEPTH: u32 = 11;
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
    /// something the checks before emitting should have refused (a register with no local, more
    /// temps than there are): the code made is not run, the region is not made (review b01:
    /// these aborted the VM by assert/unwrap; the interpreter runs the code instead)
    bad: bool,
    /// iced's operand info, made once per translation (review b01: two Vecs per instruction)
    info: iced_x86::InstructionInfoFactory,
    /// blocks laid out as nested wasm blocks and loops (structure.rs): a jump is a `br` to
    /// its target's open label (`fwd`/`back`, by node), not a dispatch on L_BLK
    structured: bool,
    fwd: Vec<Option<u32>>,
    back: Vec<Option<u32>>,
    /// the block being emitted, and the node of each block's jump table (structured)
    cur: usize,
    table_node: BTreeMap<usize, usize>,
    /// loop headers' side dispatches: (entry inside the loop, next node, the last hop)
    side: BTreeMap<usize, Vec<(usize, usize, bool)>>,
    /// a function (FN64): CALL is a wasm call of the callee's function, RET returns
    fnm: bool,
    /// direct calls' targets that are `jmp [rip+slot]` (import thunks): (target, slot), the
    /// call going where the slot says
    thunks: &'a BTreeMap<u64, u64>,
    /// (debugging, MASU_JIT_LAYOUT) jumps the layout has no way for: (from, to)
    unlaid: Vec<(u64, u64)>,
}

impl super::structure::Nest for Gen<'_> {
    fn code(&mut self) -> &mut Code {
        &mut self.c
    }
    fn depth(&mut self) -> &mut u32 {
        &mut self.depth
    }
    fn fwd(&mut self) -> &mut Vec<Option<u32>> {
        &mut self.fwd
    }
    fn back(&mut self) -> &mut Vec<Option<u32>> {
        &mut self.back
    }
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
        if self.temps >= N_T {
            self.bad = true;
            return L_T + N_T - 1;
        }
        let t = L_T + self.temps;
        self.temps += 1;
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

    /// [`Self::reg`] of a register the checks let through: one with no local marks the
    /// translation [`Gen::bad`] (and stands for RAX meanwhile).
    fn reg_ok(&mut self, r: Register) -> (u32, u32, bool) {
        Self::reg(r).unwrap_or_else(|| {
            self.bad = true;
            (L_R, 64, false)
        })
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
        let (l, bits, high) = self.reg_ok(r);
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
        let (l, bits, high) = self.reg_ok(r);
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
                let (l, _, _) = self.reg_ok(r);
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
        if self.structured {
            let label = match (self.back[k], self.fwd[k]) {
                (Some(p), _) => (p, true),
                (None, Some(p)) => (p, false),
                // (not a way the layout has: out, and back in through the exit's lookup if
                // the target is one of the region's entries)
                (None, None) => {
                    if super::LAYOUT.load(Relaxed) {
                        self.unlaid.push((self.ins.ip(), target));
                    }
                    self.exit_to(target, 0);
                    return;
                }
            };
            if self.blocks[k].flags_in {
                self.flags_to_cpu();
            }
            if label.1 {
                // a loop's next round: count down the budget
                let b = cpu_off!(jit_budget);
                self.c.get(L_CPU).get(L_CPU).load32(b).i32c(1).sub().store32(b);
                self.c.get(L_CPU).load32(b).i32c(0).le_s().if_();
                self.depth += 1;
                self.exit_to(target, 0);
                self.depth -= 1;
                self.c.end();
            }
            self.c.br(self.depth - label.0);
            return;
        }
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
        c.get(L_DBG).i32c(ENTRY_SHIFT as i32).shr_u().u32c(0x7ff).and().set(L_BLK).br(6);
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
        for i in 2..N_PARAMS {
            // (a function's tail jump: as deep as this one, its RET goes back to our caller)
            if i == L_DEPTH {
                c.get(L_DEPTH);
            } else {
                c.i32c(0);
            }
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

    /// A function's CALL (FN64) of the target in i64 local `t`, its return address pushed: a
    /// wasm call of the callee's function (the call cache's, else the host's answer, a
    /// function's root only), the state in the Cpu around it. Back at `next` with status 0 it
    /// goes on there; returned anywhere else, or with another status, the state is in the Cpu
    /// and this function leaves the same way (longjmp, SEH, a callee that left for the
    /// dispatcher). No function there, too deep or the budget spent: out to the target.
    fn call_fn(&mut self, t: u32, next: u64) {
        let site = self.ins.ip() as u32;
        let (h, v) = (L_H, L_E);
        let f = self.flags;
        self.flags_to_cpu();
        self.flags = f;
        self.store_regs();
        let b = cpu_off!(jit_budget);
        self.c.block(); // out
        self.depth += 1;
        self.c.get(L_CPU).get(L_CPU).load32(b).i32c(1).sub().store32(b);
        self.c.get(L_CPU).load32(b).i32c(0).le_s();
        self.c.get(L_DEPTH).u32c(MAX_DEPTH).ge_u().or();
        self.c.get(t).i64c(32).x(I64_SHR_U).x(I32_WRAP).u32c((self.ins.ip() >> 32) as u32).ne().or();
        self.c.br_if(0);
        // the call cache: this site's slot holds (target, hash table entry) of the function
        // it called last
        let slot = (site.wrapping_mul(0x9e37_79b1) >> 20) & (super::CALL_CACHE - 1);
        self.c.get(L_CPU).load32(cpu_off!(jit_calls)).u32c(slot * 8).add().tee(h).load32(0).get(t).x(I32_WRAP).eq();
        self.c.if_i32().get(h).load32(4).else_().get(L_CPU).get(h).get(t).x(I32_WRAP).call(F_CALLMISS).end().tee(v).eqz().br_if(0);
        self.c.get(L_CPU).get(v);
        for i in 2..N_PARAMS {
            if i == L_DEPTH {
                self.c.get(L_DEPTH).i32c(1).add();
            } else {
                self.c.i32c(0);
            }
        }
        self.c.get(v).u32c((1 << ENTRY_SHIFT) - 1).and().call_indirect(2, 0).tee(h);
        self.c.get(L_CPU).load32(cpu_off!(eip)).u32c(next as u32).ne().or();
        self.c.get(L_CPU).load32(cpu_off!(rip_hi)).u32c((next >> 32) as u32).ne().or();
        self.c.if_().get(h).ret().end();
        self.load_regs();
        self.flags = Flags::Cpu;
        self.mx_checked = None;
        self.mx_seen = [[None; 2]; 16];
        self.goto(next);
        self.depth -= 1;
        self.c.end(); // out
        self.flags = Flags::Cpu;
        self.c.get(t);
        self.exit_dyn(0);
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
        let size = if self.smc || self.fresh.is_some() { stored_bytes(&mut self.info, &self.ins) } else { 0 };
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
                for u in self.info.info(&ins).used_registers() {
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
    /// ROL/ROR by an immediate or CL, any size (the count masked to 5 bits, 6 for 64-bit
    /// operands; 8/16-bit ones rotate by it modulo their size; a count of 0 changes no flag).
    fn rotate(&mut self) -> bool {
        let bits = self.op_bits(0);
        let ins = self.ins;
        let cmask: u32 = if bits == 64 { 0x3f } else { 0x1f };
        let by_cl = ins.op_kind(1) == OpKind::Register;
        if by_cl && ins.op_register(1) != Register::CL {
            return false;
        }
        if self.has_mem() && !self.addr(bits / 8) {
            return false;
        }
        let (v, r) = (self.temp(), self.temp());
        self.read_op(0, bits);
        self.c.set(v);
        let rol = ins.mnemonic() == Mnemonic::Rol;
        if !by_cl {
            let count = (ins.immediate8() as u32) & cmask;
            if count == 0 {
                self.write_op(0, bits, v);
                return true;
            }
            let eff = count % bits;
            if eff == 0 {
                self.c.get(v).set(r);
            } else if bits == 64 {
                self.c.get(v).i64c(eff as i64).x(if rol { I64_ROTL } else { I64_ROTR }).set(r);
            } else {
                let (k, j) = if rol { (eff, bits - eff) } else { (bits - eff, eff) };
                self.c.get(v).i64c(k as i64).x(I64_SHL).get(v).i64c(j as i64).x(I64_SHR_U).x(I64_OR).i64c(mask(bits)).x(I64_AND).set(r);
            }
            self.flags_to_eflags();
            self.rotate_flags(r, bits, rol);
            self.write_op(0, bits, r);
            return true;
        }
        // (by CL; the flags into EFLAGS on both ways, a count of 0 keeping them)
        self.flags_to_eflags();
        let n = self.temp();
        self.get_reg(Register::CL);
        self.c.i64c(cmask as i64).x(I64_AND).set(n);
        self.c.get(n).x(I64_EQZ).if_();
        self.depth += 1;
        // (nothing changes; a 32-bit register is still zero-extended)
        self.write_op(0, bits, v);
        self.c.else_();
        if bits == 64 {
            self.c.get(v).get(n).x(if rol { I64_ROTL } else { I64_ROTR }).set(r);
        } else {
            // e = count % bits; (v << k | v >> j) & mask with k + j = bits (e 0: v)
            self.c.get(n).i64c(bits as i64).x(I64_REM_U).set(n);
            if rol {
                self.c.get(v).get(n).x(I64_SHL).get(v).i64c(bits as i64).get(n).x(I64_SUB).x(I64_SHR_U);
            } else {
                self.c.get(v).get(n).x(I64_SHR_U).get(v).i64c(bits as i64).get(n).x(I64_SUB).x(I64_SHL);
            }
            self.c.x(I64_OR).i64c(mask(bits)).x(I64_AND).set(r);
        }
        self.rotate_flags(r, bits, rol);
        self.write_op(0, bits, r);
        self.depth -= 1;
        self.c.end();
        true
    }

    /// A rotate's CF and OF from its result in local `r` (into the Cpu's EFLAGS): ROL the low
    /// bit and top ^ CF, ROR the top and top ^ the bit below it.
    fn rotate_flags(&mut self, r: u32, bits: u32, rol: bool) {
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
            Rol | Ror if ins.op_count() == 2 && matches!(ins.op_kind(1), OpKind::Immediate8 | OpKind::Register) => self.rotate(),
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
            // (review b01: it went to the interpreter, in every x64 function epilogue. [RBP]
            // checked before RSP changes: a LEAVE that faults leaves RSP as it was, fuzz
            // regressions/x86_jit64/leave-faults)
            Leave if ins.code() == iced_x86::Code::Leaveq => {
                let (rsp, rbp) = (L_R + 4, L_R + 5);
                self.c.get(rbp).x(I32_WRAP).tee(L_A);
                let span = GUEST_HI - GUEST_LO;
                self.c.u32c(GUEST_LO).sub().u32c(span - 8).gt_u().if_();
                self.depth += 1;
                self.bail();
                self.depth -= 1;
                self.c.end();
                let t = self.temp();
                self.load(64);
                self.c.set(t);
                self.c.get(rbp).i64c(8).x(I64_ADD).set(rsp);
                self.c.get(t).set(rbp);
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
                // (a jump table's targets in the region: its dispatch node)
                // (its node may be a loop's header, as the node of the table every opcode of
                // an interpreter ends with: a back edge, counting down the budget)
                if let Some((p, back)) = self.table_node.get(&self.cur).and_then(|&t| self.back[t].map(|p| (p, true)).or(self.fwd[t].map(|p| (p, false)))) {
                    self.c.set(L_NT);
                    self.flags_to_cpu();
                    if back {
                        let b = cpu_off!(jit_budget);
                        self.c.get(L_CPU).get(L_CPU).load32(b).i32c(1).sub().store32(b);
                        self.c.get(L_CPU).load32(b).i32c(0).le_s().if_();
                        self.depth += 1;
                        self.c.get(L_NT);
                        self.exit_dyn(0);
                        self.depth -= 1;
                        self.c.end();
                    }
                    self.c.br(self.depth - p);
                    return true;
                }
                self.exit_dyn(0);
                true
            }
            // (a leaf's call, inline: as a region's)
            Call if ins.op0_kind() == OpKind::NearBranch64 && self.fnm && self.returns.contains(&ins.next_ip()) && self.index.contains_key(&ins.near_branch64()) => {
                let t = self.temp();
                self.c.i64c(ins.next_ip() as i64).set(t);
                self.push64(t);
                self.goto(ins.near_branch64());
                true
            }
            Call if ins.op0_kind() == OpKind::NearBranch64 && self.fnm => {
                let (t, r) = (self.temp(), self.temp());
                match self.thunks.get(&ins.near_branch64()) {
                    // (through an import thunk: where its slot says, as the thunk would)
                    Some(&slot) => {
                        self.c.u32c(slot as u32).set(L_A);
                        self.load(64);
                        self.c.set(t);
                    }
                    None => {
                        self.c.i64c(ins.near_branch64() as i64).set(t);
                    }
                }
                self.c.i64c(ins.next_ip() as i64).set(r);
                self.push64(r);
                self.call_fn(t, ins.next_ip());
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
                if self.fnm {
                    self.call_fn(t, ins.next_ip());
                    return true;
                }
                self.c.get(t);
                self.exit_dyn(0);
                true
            }
            Ret => {
                let extra = if ins.op_count() == 1 { ins.immediate16() as u32 } else { 0 };
                let t = self.temp();
                self.pop64(t, extra);
                // back to a call of the region, or an inline one of the function's: straight
                // there (else through the exit's lookup, or back to the calling function)
                for &r in self.returns {
                    self.c.get(t).i64c(r as i64).x(I64_EQ).if_();
                    self.depth += 1;
                    let f = self.flags;
                    self.goto(r);
                    self.flags = f;
                    self.depth -= 1;
                    self.c.end();
                }
                if self.fnm {
                    // called by a function: back to it (it goes on if this is its return
                    // address); from the dispatcher, on through the exit's lookup
                    self.c.get(L_DEPTH).if_();
                    self.depth += 1;
                    let f = self.flags;
                    self.flags_to_cpu();
                    self.flags = f;
                    self.store_regs();
                    self.c.get(L_CPU).get(t).x(I32_WRAP).store32(cpu_off!(eip));
                    self.c.get(L_CPU).get(t).i64c(32).x(I64_SHR_U).x(I32_WRAP).store32(cpu_off!(rip_hi));
                    self.c.i32c(0).ret();
                    self.depth -= 1;
                    self.c.end();
                    self.c.get(t);
                    self.exit_dyn(0);
                    return true;
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
            let (l, bits, _) = self.reg_ok(r);
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
fn stored_bytes(info: &mut iced_x86::InstructionInfoFactory, ins: &Instruction) -> u32 {
    use iced_x86::OpAccess;
    info.info(ins)
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
#[cfg(test)]
fn jump_table<M: Memory>(mem: &mut M, ins: &[Instruction], n: Option<(Register, u64)>) -> Vec<u64> {
    jump_table_in(mem, ins, n, &BTreeMap::new())
}

/// jump_table, with `outer`: what registers the block does not set hold (`lea r, [rip+X]`
/// elsewhere in the function, the only one for r: Lua 5.4's `jmp [r13+rax*8]` at the end of
/// every opcode, r13 the dispatch table set once at its entry). Only for finding blocks: the
/// jump goes wherever the register says.
fn jump_table_in<M: Memory>(mem: &mut M, ins: &[Instruction], n: Option<(Register, u64)>, outer: &BTreeMap<Register, Option<u64>>) -> Vec<u64> {
    use Mnemonic::*;
    // (no bound checked by the jump here: one the block itself sets, `movzx rI, byte [..]` or
    // `and rI, imm` (SQLite's `cmp byte [rdx+1], 9; ja ..` checks the byte it loads again),
    // its entries read until one goes far)
    let (n, def) = match n {
        Some(n) => (Some(n), None),
        None => match index_bound(ins) {
            Some((r, k, at)) => (Some((r, k)), Some(at)),
            None => (None, None),
        },
    };
    let Some((ireg, n)) = n else { return Vec::new() };
    let near = |t: u64| t.abs_diff(ins.last().unwrap().ip()) <= 0x10_0000;
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
            b => match get(vals, b) {
                Some(V::Const(c)) => c,
                // (not set in the block: the function's one value for it, if it has one)
                None if !ins.iter().any(|x| x.op_count() > 0 && x.op0_kind() == OpKind::Register && x.op0_register().full_register() == b.full_register()) => (*outer.get(&b.full_register())?)?,
                _ => return None,
            },
        };
        Some(base.wrapping_add(i.memory_displacement64()))
    };
    let mut targets = Vec::new();
    for (at_k, i) in ins.iter().enumerate() {
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
                                if def.is_some() && !near(b.wrapping_add(e)) {
                                    break;
                                }
                                targets.push(b.wrapping_add(e));
                            }
                        }
                    }
                    OpKind::Memory if i.memory_size().size() == 8 => {
                        if let Some(t) = table(&vals, i, 8) {
                            for k in 0..n {
                                let Some(e) = at(mem, t + 8 * k, 8) else { break };
                                if def.is_some() && !near(e) {
                                    break;
                                }
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
            if r == ireg && !matches!(v, Some(V::Entry(..))) && !vals.iter().any(|x| matches!(x.1, V::Entry(..))) && def.is_none_or(|d| at_k > d) {
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

/// The bound a block sets on a table index itself: the last `movzx rI, byte [..]` (256
/// entries) or `and rI, imm` (imm + 1) before its jump, and where it is.
fn index_bound(ins: &[Instruction]) -> Option<(Register, u64, usize)> {
    let [.., j] = ins else { return None };
    // (the register the jump's table is indexed by, and the instruction reading the table)
    let (p, idx) = match ins.iter().rposition(|i| i.op_count() > 1 && i.op1_kind() == OpKind::Memory && i.memory_index() != Register::None) {
        Some(p) => (p, ins[p].memory_index().full_register()),
        None if j.op0_kind() == OpKind::Memory && j.memory_index() != Register::None => (ins.len() - 1, j.memory_index().full_register()),
        None => return None,
    };
    for (k, i) in ins[..p].iter().enumerate().rev() {
        if i.op_count() == 0 || i.op0_kind() != OpKind::Register || i.op0_register().full_register() != idx {
            continue;
        }
        return match i.mnemonic() {
            Mnemonic::Movzx if i.memory_size().size() == 1 || (i.op1_kind() == OpKind::Register && i.op1_register().size() == 1) => Some((idx, 256, k)),
            Mnemonic::And if matches!(i.op1_kind(), OpKind::Immediate8to32 | OpKind::Immediate32 | OpKind::Immediate8to64 | OpKind::Immediate32to64) && (i.immediate(1) as i64) >= 0 => Some((idx, (i.immediate(1) + 1).min(MAX_TABLE), k)),
            _ => None,
        };
    }
    None
}

/// Whether the function at `f` is a small leaf (FN64 calls it inline): a few blocks of direct
/// jumps only, no call, indirect jump or anything else that leaves, ending in RETs.
fn leaf64<M: Memory>(mem: &mut M, f: u64, trap: (u32, u32)) -> bool {
    let (mut seen, mut todo, mut n, mut rets) = (Vec::new(), alloc::vec![f], 0, 0);
    while let Some(a) = todo.pop() {
        if seen.contains(&a) {
            continue;
        }
        if seen.len() >= LEAF_BLOCKS || (a as u32) >= trap.0 && (a as u32) < trap.1 {
            return false;
        }
        seen.push(a);
        let ins = decode(mem, a, trap);
        let Some(last) = ins.last() else { return false };
        n += ins.len();
        if n > LEAF_INS {
            return false;
        }
        let near = last.op0_kind() == OpKind::NearBranch64;
        match last.flow_control() {
            FlowControl::Return => rets += 1,
            FlowControl::Next => todo.push(last.next_ip()),
            FlowControl::UnconditionalBranch if near => todo.push(last.near_branch64()),
            FlowControl::ConditionalBranch if near => todo.extend([last.near_branch64(), last.next_ip()]),
            _ => return false,
        }
    }
    rets > 0
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
/// Regions nest their blocks as wasm blocks and loops (structure.rs, entries only where that
/// allows, loops entered in the middle through their headers) instead of dispatching every
/// jump between their blocks on a block number at the top of one loop. Off by default: on
/// flatbench x64 (2026-11-14) the jump tables, fib, strings and SQLite got 10-30% faster but
/// sort and the objects case 20-70% slower, as merges of hot regions stopped happening
/// (MASU_JIT64_STRUCT=1 in kura; tools/jit64test runs two tests in three so).
pub static STRUCTURED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Debugging and A/B: no entries inside loops (structure64's side dispatches).
pub static DEBUG_NO_SIDE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The region's control flow graph laid out by structure.rs (nodes: the blocks, a dispatch node
/// per jump table, and one before them all going to the entries) and its entries, the first
/// block first: the roots, the return addresses of calls and loop headers that are in no loop
/// but as its header (into the middle of a loop is not a structured way in).
fn structure64(blocks: &[Block], index: &BTreeMap<u64, usize>, returns: &[u64], (group_of, groups): (&BTreeMap<usize, usize>, &[Vec<u64>]), roots: &[u64], fnm: bool) -> Option<(super::structure::Layout, Vec<usize>, BTreeMap<usize, Vec<(usize, usize, bool)>>)> {
    let n = blocks.len();
    let find = |a: u64| index.get(&a).copied();
    let mut succ: Vec<Vec<usize>> = alloc::vec![Vec::new(); n + groups.len() + 1];
    let tnode: BTreeMap<usize, usize> = group_of.iter().map(|(&k, &g)| (k, n + g)).collect();
    for (k, b) in blocks.iter().enumerate() {
        let last = b.ins.last().unwrap();
        let near = last.op0_kind() == OpKind::NearBranch64;
        let mut s: Vec<usize> = Vec::new();
        match last.flow_control() {
            FlowControl::Next => s.extend(find(last.next_ip())),
            FlowControl::UnconditionalBranch if near => s.extend(find(last.near_branch64())),
            FlowControl::ConditionalBranch if near => {
                s.extend(find(last.near_branch64()));
                s.extend(find(last.next_ip()));
            }
            // (a function's calls come back: the return address follows; a leaf's inline)
            FlowControl::Call if fnm && near && returns.contains(&last.next_ip()) => s.extend(find(last.near_branch64())),
            FlowControl::Call | FlowControl::IndirectCall if fnm => s.extend(find(last.next_ip())),
            FlowControl::Call if near => s.extend(find(last.near_branch64())),
            FlowControl::Return => s.extend(returns.iter().filter_map(|&r| find(r))),
            FlowControl::IndirectBranch => s.extend(tnode.get(&k).copied()),
            _ => {}
        }
        s.sort_unstable();
        s.dedup();
        succ[k] = s;
    }
    for (i, t) in groups.iter().enumerate() {
        let mut s: Vec<usize> = t.iter().filter_map(|&a| find(a)).collect();
        s.sort_unstable();
        s.dedup();
        succ[n + i] = s;
    }
    let l0 = super::structure::from_succ(succ.clone(), 0, &[]);
    let rets: Vec<usize> = blocks
        .iter()
        .filter_map(|b| b.ins.last())
        .filter(|l| matches!(l.flow_control(), FlowControl::Call | FlowControl::IndirectCall))
        .filter_map(|l| find(l.next_ip()))
        .collect();
    let rooted: Vec<usize> = roots.iter().filter_map(|&r| find(r)).collect();
    // (a function's jump tables' targets too: an interpreter's opcodes, where it is left
    // when the budget runs out at its dispatch, or when the dispatcher comes back to one)
    let cases: Vec<usize> = if fnm { groups.iter().flatten().filter_map(|&a| find(a)).collect() } else { Vec::new() };
    let wanted = |k: usize| k != 0 && (rooted.contains(&k) || rets.contains(&k) || l0.header(k) || cases.contains(&k));
    let reached: Vec<bool> = {
        let mut r = alloc::vec![false; n + groups.len() + 1];
        for &v in l0.order_iter() {
            r[v] = true;
        }
        r
    };
    // (and the roots block 0 does not reach: merged regions' other roots)
    let mut direct = l0.entries(n, wanted);
    direct.extend((1..n).filter(|&k| !reached[k] && rooted.contains(&k)));
    let base_irr = l0.irreducible();
    if !direct.is_empty() && super::structure::from_succ(succ.clone(), 0, &direct).irreducible() > base_irr {
        // (one at a time: the ones that keep the graph as reducible as it was)
        let all = core::mem::take(&mut direct);
        for k in all {
            direct.push(k);
            if super::structure::from_succ(succ.clone(), 0, &direct).irreducible() > base_irr {
                direct.pop();
            }
        }
    }
    // entries inside loops: through the headers of the loops around them, outermost first
    // (each header's side dispatch goes on to the next, see emit_side64), the outermost one
    // an entry itself
    let outer = l0.entries(n, |k| l0.header(k));
    let bodies = l0.loop_bodies();
    let mut side: BTreeMap<usize, Vec<(usize, usize, bool)>> = BTreeMap::new();
    let mut inner = Vec::new();
    let mut heads = Vec::new();
    let mut succ2 = succ.clone();
    if !DEBUG_NO_SIDE.load(Relaxed) {
        for e in (1..n).filter(|&k| reached[k] && wanted(k) && !direct.contains(&k)) {
            let mut chain: Vec<(usize, usize)> = bodies.iter().filter(|(&h, b)| h != e && b[e]).map(|(&h, b)| (h, b.iter().filter(|&&x| x).count())).collect();
            chain.sort_by(|a, b| b.1.cmp(&a.1));
            let Some(&(h0, _)) = chain.first() else { continue };
            if !outer.contains(&h0) && h0 != 0 {
                continue;
            }
            let hs: Vec<usize> = chain.iter().map(|c| c.0).collect();
            for (i, &h) in hs.iter().enumerate() {
                let next = hs.get(i + 1).copied().unwrap_or(e);
                if !succ2[h].contains(&next) {
                    succ2[h].push(next);
                }
                side.entry(h).or_default().push((e, next, i + 1 == hs.len()));
            }
            inner.push(e);
            if h0 != 0 && !direct.contains(&h0) && !heads.contains(&h0) {
                heads.push(h0);
            }
        }
    }
    let mut extra: Vec<usize> = direct.iter().chain(heads.iter()).copied().collect();
    if !inner.is_empty() {
        // (the first node dispatches on L_BLK even with block 0 its only other way)
        if extra.is_empty() {
            extra.push(0);
        }
        let l = super::structure::from_succ(succ2.clone(), 0, &extra);
        if l.irreducible() <= l0.irreducible() {
            let mut entries: Vec<usize> = alloc::vec![0];
            for k in extra.into_iter().chain(inner.iter().copied()) {
                if !entries.contains(&k) {
                    entries.push(k);
                }
            }
            return Some((l, entries, side));
        }
    }
    // (without the entries inside loops)
    if direct.is_empty() {
        return Some((l0, alloc::vec![0], BTreeMap::new()));
    }
    let l = super::structure::from_succ(succ, 0, &direct);
    if l.irreducible() > l0.irreducible() {
        return Some((l0, alloc::vec![0], BTreeMap::new()));
    }
    Some((l, core::iter::once(0).chain(direct).collect(), BTreeMap::new()))
}

/// A loop header's side dispatch (see structure64): with L_BLK an entry inside the loop
/// (from the region's first node), on to the next header around it or to the entry itself
/// (L_BLK -1 again then); with L_BLK negative or another block, the header's own code.
fn emit_side64(g: &mut Gen<'_>, side: &[(usize, usize, bool)]) {
    let n = g.blocks.len();
    g.c.block(); // own
    g.depth += 1;
    let own = g.depth;
    g.c.get(L_BLK).i32c(0).lt_s().br_if(0);
    // (stubs: the last hops, and hops whose next header has no label here)
    let finals: Vec<(usize, usize)> = side.iter().filter(|s| s.2 || g.fwd[s.1].is_none()).map(|s| (s.0, s.1)).collect();
    for _ in &finals {
        g.c.block();
    }
    g.depth += finals.len() as u32;
    g.c.get(L_BLK).op(0x0e);
    super::wasm::uleb(&mut g.c.b, n as u64);
    for k in 0..n {
        let l = match side.iter().find(|s| s.0 == k) {
            Some(&(e, next, last)) => match (last, g.fwd[next]) {
                (false, Some(p)) => g.depth - p,
                _ => finals.iter().position(|f| f.0 == e).unwrap() as u32,
            },
            None => g.depth - own,
        };
        super::wasm::uleb(&mut g.c.b, l as u64);
    }
    super::wasm::uleb(&mut g.c.b, (g.depth - own) as u64);
    for &(e, next) in &finals {
        g.c.end();
        g.depth -= 1;
        g.c.i32c(-1).set(L_BLK);
        match g.fwd[e].filter(|_| next == e) {
            Some(p) => {
                g.c.br(g.depth - p);
            }
            // (no way there in the layout: the interpreter runs the entry's first instruction)
            None => g.exit_to(g.blocks[e].start, RET_INTERP),
        }
    }
    g.c.end(); // own
    g.depth -= 1;
    g.c.i32c(-1).set(L_BLK);
}

/// Block `k`'s code (its jumps go through Gen::goto).
fn emit_block64<M: Memory>(g: &mut Gen<'_>, mem: &mut M, k: usize, count: &mut usize, layout: &mut String) {
    let blocks = g.blocks;
    g.cur = k;
    g.flags = Flags::Cpu;
    g.mx_checked = None;
    g.mx_seen = [[None; 2]; 16];
    if let Some(side) = g.side.get(&k).cloned() {
        emit_side64(g, &side);
    }
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
        *count += 1;
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
        let l = list.last().unwrap();
        layout.push_str(&alloc::format!("{:#x} {} {:?} {:#x}\n", blocks[k].start, list.len(), l.mnemonic(), if l.op0_kind() == OpKind::NearBranch64 { l.near_branch64() } else { 0 }));
    }
}

/// A jump table's dispatch node (structured): the target in L_NT (flags in the Cpu), to its
/// block when it is one of the table's in the region (a perfect hash of the low half), else out.
fn emit_table64(g: &mut Gen<'_>, targets: &[u64], hi: u32) {
    g.flags = Flags::Cpu;
    g.mx_checked = None;
    g.mx_seen = [[None; 2]; 16];
    let mut keys: Vec<u64> = targets.iter().copied().filter(|a| g.index.contains_key(a) && (a >> 32) as u32 == hi).collect();
    keys.sort_unstable();
    keys.dedup();
    g.c.get(L_NT).i64c(32).x(I64_SHR_U).x(I32_WRAP).u32c(hi).ne().if_();
    g.depth += 1;
    g.c.get(L_NT);
    g.exit_dyn(0);
    g.depth -= 1;
    g.c.end();
    g.c.get(L_NT).x(I32_WRAP).set(L_A);
    if !keys.is_empty() {
        let low: Vec<u32> = keys.iter().map(|&a| a as u32).collect();
        let (mul, shift, slots) = super::structure::perfect_hash(&low);
        let m = keys.len();
        g.c.block(); // miss
        g.depth += 1;
        let miss = g.depth;
        for _ in 0..m {
            g.c.block();
        }
        g.depth += m as u32;
        g.c.get(L_A).u32c(mul).mul().i32c(shift as i32).shr_u().op(0x0e);
        super::wasm::uleb(&mut g.c.b, slots.len() as u64);
        for &i in &slots {
            super::wasm::uleb(&mut g.c.b, i as u64);
        }
        super::wasm::uleb(&mut g.c.b, m as u64);
        for &a in &keys {
            g.c.end();
            g.depth -= 1;
            g.c.get(L_A).u32c(a as u32).eq().if_();
            g.depth += 1;
            g.goto(a);
            g.flags = Flags::Cpu;
            g.depth -= 1;
            g.c.end();
            g.c.br(g.depth - miss);
        }
        g.c.end(); // miss
        g.depth -= 1;
    }
    g.c.get(L_NT);
    g.exit_dyn(0);
}

/// The first node of a region with more entries than its first block: `br_table` on L_BLK
/// (the hash table's block number) to the entry's label.
fn emit_entries64(g: &mut Gen<'_>, entries: &[usize]) {
    let n = g.blocks.len();
    let label = |g: &Gen<'_>, k: usize| g.depth - g.fwd[k].or(g.fwd[entries[0]]).expect("an entry's label");
    g.c.get(L_BLK).op(0x0e);
    super::wasm::uleb(&mut g.c.b, n as u64);
    let first = label(g, entries[0]);
    for k in 0..n {
        // (an entry inside loops: its outermost header's label, whose side dispatch goes on)
        let via = g.side.iter().filter(|(_, v)| v.iter().any(|s| s.0 == k)).map(|(&h, _)| h).find(|&h| g.fwd[h].is_some() && entries.contains(&h));
        let l = match via {
            Some(h) if !g.fwd[k].is_some() => label(g, h),
            _ if entries.contains(&k) => label(g, k),
            _ => first,
        };
        super::wasm::uleb(&mut g.c.b, l as u64);
    }
    super::wasm::uleb(&mut g.c.b, first as u64);
}

pub fn translate_region64<M: Memory>(module: &mut Module, mem: &mut M, roots: &[u64], trap: (u32, u32), accept: impl FnMut(u32, bool) -> bool) -> Option<Region> {
    // (each page read once: the instructions decoded, the bytes blocks check on entry and the
    // region's text agree although other threads may rewrite the code meanwhile)
    translate_seen64(module, &mut super::Snapshot::new(mem), roots, trap, accept, None)
}

/// x64 functions (`MASU_JIT64_FN`): off by default for now.
pub static FN64: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The function at `entry` (FN64) as one wasm function of the region type: its blocks are
/// the ones its own jumps reach (accept(a, false)), its calls' return addresses and jump
/// tables' targets (accept(a, true)), never a call's target. A CALL is a wasm call of the
/// callee's function (Gen::call_fn) and a RET returns to its caller (status 0, the state in
/// the Cpu, which goes on when that is its return address). Entered at any of `starts` (the
/// hash table's entries, tagged FN_TAG by the host: what calls go to).
/// `max_ins`: instructions it takes at most (a function built again takes more: big
/// interpreters' loops, whose cases are its blocks).
pub fn translate_function64<M: Memory>(module: &mut Module, mem: &mut M, entry: u64, trap: (u32, u32), max_ins: usize, accept: impl FnMut(u32, bool) -> bool) -> Option<Region> {
    translate_seen64(module, &mut super::Snapshot::new(mem), &[entry], trap, accept, Some(max_ins))
}

fn translate_seen64<M: Memory>(module: &mut Module, mem: &mut M, roots: &[u64], trap: (u32, u32), mut accept: impl FnMut(u32, bool) -> bool, fn_ins: Option<usize>) -> Option<Region> {
    let fnm = fn_ins.is_some();
    // (a host trap's address is no code: a function or region never starts there, a call
    // cache miss on an import's slot names one)
    if roots.iter().any(|&r| (r as u32) >= trap.0 && (r as u32) < trap.1) {
        return None;
    }
    let (max_blocks, max_ins) = match fn_ins {
        Some(n) => (MAX_FN_BLOCKS, n),
        None => (MAX_BLOCKS, MAX_REGION_INS),
    };
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
    // (block -> the targets of the jump table it ends with)
    let mut tables: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    let mut thunks: BTreeMap<u64, u64> = BTreeMap::new();
    let mut leas: BTreeMap<Register, Option<u64>> = BTreeMap::new();
    let mut thunk_code: Vec<(u32, u32)> = Vec::new();
    // (fnm: the return addresses of the calls of leaves, inline)
    let mut inline: Vec<u64> = Vec::new();
    let mut total = 0;
    while let Some((a, sure)) = todo.pop() {
        if index.contains_key(&a) || blocks.len() >= limit(&DEBUG_BLOCKS, max_blocks) || total >= max_ins {
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
        if fnm {
            // (registers set to an address in the function: the only value each takes in it,
            // or None)
            for i in &ins {
                let reads_only = matches!(i.mnemonic(), Mnemonic::Cmp | Mnemonic::Test | Mnemonic::Push | Mnemonic::Jmp | Mnemonic::Call | Mnemonic::Bt);
                if i.op_count() > 0 && i.op0_kind() == OpKind::Register && !reads_only {
                    let r = i.op0_register().full_register();
                    let v = (i.mnemonic() == Mnemonic::Lea && i.memory_base() == Register::RIP && i.memory_index() == Register::None && i.op0_register().is_gpr64()).then(|| i.memory_displacement64());
                    match v {
                        Some(v) => {
                            let e = leas.entry(r).or_insert(Some(v));
                            if *e != Some(v) {
                                *e = None;
                            }
                        }
                        None => {
                            leas.insert(r, None);
                        }
                    }
                }
            }
        }
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
            FlowControl::Call if fnm => {
                todo.push((last.next_ip(), true));
                // (a small leaf callee goes inline: its RET goes on at the return address when
                // that is where it returns to, see Gen's Ret)
                if last.op0_kind() == OpKind::NearBranch64 && leaf64(mem, last.near_branch64(), trap) {
                    inline.push(last.next_ip());
                    todo.insert(0, (last.near_branch64(), true));
                }
                // (an import thunk, `jmp [rip+slot]`: the call reads the slot itself)
                if last.op0_kind() == OpKind::NearBranch64 {
                    let f = last.near_branch64();
                    if let Some(j) = decode(mem, f, trap).first().filter(|j| j.mnemonic() == Mnemonic::Jmp && j.op0_kind() == OpKind::Memory && j.memory_base() == Register::RIP && j.memory_size().size() == 8) {
                        let slot = j.memory_displacement64();
                        if !super::fresh_at(f as u32) && (slot >> 32) == (f >> 32) && (GUEST_LO as u64..GUEST_HI as u64 - 8).contains(&(slot & 0xffff_ffff)) {
                            if thunks.insert(f, slot).is_none() {
                                thunk_code.push((f as u32, j.len() as u32));
                            }
                        }
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
                let t = jump_table_in(mem, &ins, n, &leas);
                for &t in t.iter().rev() {
                    todo.push((t, true));
                }
                if !t.is_empty() {
                    tables.insert(blocks.len(), t);
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

    super::module_head(module, true);

    let n = blocks.len() as u32;
    let mut returns: Vec<u64> = blocks
        .iter()
        .filter_map(|b| b.ins.last())
        .filter(|l| !fnm || inline.contains(&l.next_ip()))
        .filter(|l| l.flow_control() == FlowControl::Call && l.op0_kind() == OpKind::NearBranch64 && index.contains_key(&l.near_branch64()) && index.contains_key(&l.next_ip()))
        .map(|l| l.next_ip())
        .collect();
    returns.sort_unstable();
    returns.dedup();
    returns.truncate(MAX_RETURNS);
    // (one dispatch node per table: blocks jumping through the same one share it, as every
    // opcode's end in an interpreter whose dispatch is duplicated (Lua 5.4): with a node each,
    // the opcodes' loop had several ways in and left the layout)
    let mut groups: Vec<Vec<u64>> = Vec::new();
    let mut group_of: BTreeMap<usize, usize> = BTreeMap::new();
    for (&k, t) in &tables {
        let g = match groups.iter().position(|x| x == t) {
            Some(g) => g,
            None => {
                groups.push(t.clone());
                groups.len() - 1
            }
        };
        group_of.insert(k, g);
    }
    let lay = if !STRUCTURED.load(Relaxed) && !fnm { None } else { structure64(&blocks, &index, &returns, (&group_of, &groups), roots, fnm) };
    let nodes = blocks.len() + groups.len() + 1;
    let mut g = Gen { c: Code::default(), blocks: &blocks, index: &index, returns: &returns, flags: Flags::Cpu, depth: 0, ins: blocks[0].ins[0], temps: 0, mx_checked: None, mx_seen: [[None; 2]; 16], marks: Vec::new(), hi_swapped: Vec::new(), smc: false, fresh: None, watch_store: false, fell_back: false, bad: false, info: iced_x86::InstructionInfoFactory::new(), structured: lay.is_some(), fwd: alloc::vec![None; nodes], back: alloc::vec![None; nodes], cur: 0, table_node: BTreeMap::new(), side: BTreeMap::new(), fnm, thunks: &thunks, unlaid: Vec::new() };
    // the dispatcher's key is the low 32 bits: code at another 4GB alias is left to the interpreter
    g.c.get(L_CPU).load32(cpu_off!(rip_hi)).u32c((entry >> 32) as u32).ne().if_();
    g.c.i32c(RET_INTERP).ret();
    g.c.end();
    g.c.i32c(0).set(L_FC);
    g.load_regs();
    // (a function's entries are tagged FN_TAG, bit 31)
    g.c.get(L_ARG).i32c(ENTRY_SHIFT as i32).shr_u().u32c(0x7ff).and().set(L_BLK);
    let mut count = 0;
    let mut layout = String::new();
    let hi = (entry >> 32) as u32;
    let entries: Vec<usize>;
    if let Some((l, ents, side)) = &lay {
        entries = ents.clone();
        g.side = side.clone();
        // loop $top; block $exit; the blocks nested by structure.rs (the first node: a
        // br_table on L_BLK to the entries when there are more than one); end $exit
        g.c.loop_();
        g.c.block();
        let nb = blocks.len();
        for (&k, &i) in &group_of {
            g.table_node.insert(k, nb + i);
        }
        super::structure::emit(&mut g, l, |g, k| {
            if k < nb {
                emit_block64(g, mem, k, &mut count, &mut layout);
            } else if k < nb + groups.len() {
                // (a loop's header may be a table's node: its side dispatch first, as a block's)
                if let Some(side) = g.side.get(&k).cloned() {
                    g.flags = Flags::Cpu;
                    emit_side64(g, &side);
                }
                emit_table64(g, &groups[k - nb], hi);
            } else {
                emit_entries64(g, &entries);
            }
        });
    } else {
        entries = (0..blocks.len()).collect();
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
        for k in 0..n as usize {
            g.c.end();
            // inside: blocks k+1..n-1, the exit block, the loop
            g.depth = n - 1 - k as u32;
            emit_block64(&mut g, mem, k, &mut count, &mut layout);
        }
    }
    if g.bad {
        return None;
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
    for (f, t) in &g.unlaid {
        layout.push_str(&alloc::format!("out of the layout: {f:#x} -> {t:#x}\n"));
    }
    if super::LAYOUT.load(Relaxed) {
        layout.push_str(&alloc::format!("entries {:x?}\ntables {:x?}\n", entries.iter().map(|&k| blocks[k].start).collect::<Vec<_>>(), tables.iter().map(|(&k, t)| (blocks[k].start, t.len())).collect::<Vec<_>>()));
    }
    let code = splice_regs(core::mem::take(&mut g.c), &g.marks, DEBUG_ALL_REGS.load(Relaxed));
    module.funcs.push(Func { ty: 2, locals, code });
    let starts = entries.iter().map(|&k| (blocks[k].start as u32, k as u32)).collect();
    let code: Vec<(u32, u32)> = blocks.iter().map(|b| {
        let end = b.ins.last().unwrap().next_ip();
        (b.start as u32, (end - b.start) as u32)
    })
    // (the thunks' own bytes: the calls depend on them too, for page checks)
    .chain(thunk_code.iter().copied())
    .collect();
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

    /// A table whose index the block loads itself (SQLite's vdbeRecordCompareInt:
    /// `cmp byte [rdx+1], 9; ja` in the block before, then `movzx ecx, byte [rdx+1]; lea r9,
    /// [rip+T]; movsxd rcx, [r9+rcx*4]; add rcx, r9; jmp rcx`): its entries up to the first that
    /// goes far, and with `and ecx, 3` the first four only.
    #[test]
    fn jump_table_bounded_by_its_own_block() {
        struct Img(Vec<u8>);
        impl Memory for Img {
            fn read8(&mut self, a: u32) -> Result<u8, crate::Fault> {
                a.checked_sub(0x1000).and_then(|o| self.0.get(o as usize).copied()).ok_or(crate::Fault { addr: a, write: false })
            }
            fn write8(&mut self, a: u32, _: u8) -> Result<(), crate::Fault> {
                Err(crate::Fault { addr: a, write: true })
            }
        }
        let decode = |code: &[u8]| -> Vec<Instruction> { Decoder::with_ip(64, code, 0x1000, DecoderOptions::NONE).into_iter().collect() };
        // the table at 0x1100: offsets of 0x1040 0x1050 0x1060 0x1070 0x1080, then one 16 MB away
        let mut img = alloc::vec![0u8; 0x200];
        for (k, t) in [0x40i32, 0x50, 0x60, 0x70, 0x80, 0x100_0000].iter().enumerate() {
            let e = t - 0x100;
            img[0x100 + 4 * k..0x104 + 4 * k].copy_from_slice(&e.to_le_bytes());
        }
        #[rustfmt::skip]
        let code: &[u8] = &[
            0x0f, 0xb6, 0x4a, 0x01,                   // movzx ecx, byte [rdx+1]
            0x4c, 0x8d, 0x0d, 0xf5, 0x00, 0x00, 0x00, // lea r9, [rip+0xf5] (0x1100)
            0x49, 0x63, 0x0c, 0x89,                   // movsxd rcx, [r9+rcx*4]
            0x4c, 0x01, 0xc9,                         // add rcx, r9
            0xff, 0xe1,                               // jmp rcx
        ];
        img[..code.len()].copy_from_slice(code);
        let ins = decode(code);
        assert_eq!(index_bound(&ins), Some((Register::RCX, 256, 0)));
        assert_eq!(jump_table(&mut Img(img.clone()), &ins, None), alloc::vec![0x1040, 0x1050, 0x1060, 0x1070, 0x1080]);
        // and ecx, 3 after it: four
        #[rustfmt::skip]
        let code2: &[u8] = &[
            0x0f, 0xb6, 0x4a, 0x01, 0x83, 0xe1, 0x03, // movzx ecx, byte [rdx+1]; and ecx, 3
            0x4c, 0x8d, 0x0d, 0xf2, 0x00, 0x00, 0x00, // lea r9, [rip+0xf2] (0x1100)
            0x49, 0x63, 0x0c, 0x89, 0x4c, 0x01, 0xc9, 0xff, 0xe1,
        ];
        let mut img2 = img.clone();
        img2[..code2.len()].copy_from_slice(code2);
        let ins2 = decode(code2);
        assert_eq!(index_bound(&ins2), Some((Register::RCX, 4, 1)));
        assert_eq!(jump_table(&mut Img(img2), &ins2, None), alloc::vec![0x1040, 0x1050, 0x1060, 0x1070]);
        // the index changed after its bound: no table
        #[rustfmt::skip]
        let code3: &[u8] = &[
            0x0f, 0xb6, 0x4a, 0x01, 0xff, 0xc1,       // movzx ecx, byte [rdx+1]; inc ecx
            0x4c, 0x8d, 0x0d, 0xf3, 0x00, 0x00, 0x00, // lea r9, [rip+0xf3] (0x1100)
            0x49, 0x63, 0x0c, 0x89, 0x4c, 0x01, 0xc9, 0xff, 0xe1,
        ];
        let mut img3 = img;
        img3[..code3.len()].copy_from_slice(code3);
        let ins3 = decode(code3);
        assert!(jump_table(&mut Img(img3), &ins3, None).is_empty());
    }

    /// Lua 5.4's dispatch at the end of every opcode, `mov eax, ebx; and eax, 0x7f;
    /// jmp [r13+rax*8]`, r13 the table the function set once (`outer`): the table's entries
    /// up to the first that goes far; none when the block sets r13 itself to something else
    /// or the function's value for it is not known.
    #[test]
    fn jump_table_through_a_register_the_function_set() {
        struct Img(Vec<u8>);
        impl Memory for Img {
            fn read8(&mut self, a: u32) -> Result<u8, crate::Fault> {
                a.checked_sub(0x1000).and_then(|o| self.0.get(o as usize).copied()).ok_or(crate::Fault { addr: a, write: false })
            }
            fn write8(&mut self, a: u32, _: u8) -> Result<(), crate::Fault> {
                Err(crate::Fault { addr: a, write: true })
            }
        }
        let mut img = alloc::vec![0u8; 0x200];
        for (k, t) in [0x1040u64, 0x1050, 0x1060, 0x900_0000].iter().enumerate() {
            img[0x100 + 8 * k..0x108 + 8 * k].copy_from_slice(&t.to_le_bytes());
        }
        let decode = |code: &[u8]| -> Vec<Instruction> { Decoder::with_ip(64, code, 0x1000, DecoderOptions::NONE).into_iter().collect() };
        // mov eax, ebx; and eax, 0x7f; jmp [r13+rax*8]
        let ins = decode(&[0x89, 0xd8, 0x83, 0xe0, 0x7f, 0x41, 0xff, 0x64, 0xc5, 0x00]);
        assert_eq!(index_bound(&ins), Some((Register::RAX, 128, 1)));
        let mut outer = BTreeMap::new();
        outer.insert(Register::R13, Some(0x1100u64));
        assert_eq!(jump_table_in(&mut Img(img.clone()), &ins, None, &outer), alloc::vec![0x1040, 0x1050, 0x1060]);
        assert!(jump_table_in(&mut Img(img.clone()), &ins, None, &BTreeMap::new()).is_empty());
        outer.insert(Register::R13, None);
        assert!(jump_table_in(&mut Img(img.clone()), &ins, None, &outer).is_empty());
        // (r13 set in the block: its own value counts, unknown here)
        outer.insert(Register::R13, Some(0x1100u64));
        let ins2 = decode(&[0x4d, 0x89, 0xe5, 0x89, 0xd8, 0x83, 0xe0, 0x7f, 0x41, 0xff, 0x64, 0xc5, 0x00]); // mov r13, r12; ...
        assert!(jump_table_in(&mut Img(img), &ins2, None, &outer).is_empty());
    }

    /// A host trap's address is no code: a call through an import's slot misses the call
    /// cache at the trap, and a function built there would run the trap page's bytes (zeros:
    /// `add [rax], al`, a byte of the guest's memory changed: printf's first character, the
    /// FN64 host tests). Neither a function nor a region starts in the trap range, at any of
    /// its addresses; the same bytes outside it are translated.
    #[test]
    fn nothing_translated_at_a_host_trap() {
        struct Zeros;
        impl Memory for Zeros {
            fn read8(&mut self, _: u32) -> Result<u8, crate::Fault> {
                Ok(0)
            }
            fn write8(&mut self, a: u32, _: u8) -> Result<(), crate::Fault> {
                Err(crate::Fault { addr: a, write: true })
            }
        }
        let trap = (0xfe00_0000u32, 0xfe01_0000u32);
        // (the trap range is by the low 32 bits, in any 4GB alias)
        for full in [trap.0 as u64, trap.0 as u64 + 0x78, trap.1 as u64 - 1, 0x1_fe00_0078] {
            let a = full;
            assert!(translate_function64(&mut Module::default(), &mut Zeros, full, trap, 1000, |_, _| true).is_none(), "function at {a:#x}");
            assert!(translate_region64(&mut Module::default(), &mut Zeros, &[full], trap, |_, _| true).is_none(), "region at {a:#x}");
        }
        for a in [trap.0 - 0x10, trap.1] {
            assert!(translate_function64(&mut Module::default(), &mut Zeros, a as u64, trap, 1000, |_, _| true).is_some(), "function at {a:#x}");
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

    /// Debugging the layout of real code: LAYOUT_EXE (a PE32+), LAYOUT_BASE (where sake mapped
    /// it, hex), LAYOUT_ROOTS (hex, comma-separated): the region's blocks, its entries and its
    /// side dispatches. `cargo test -p koji --lib layout_of_exe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn layout_of_exe() {
        extern crate std;
        struct Img(u32, Vec<u8>);
        impl Memory for Img {
            fn read8(&mut self, a: u32) -> Result<u8, crate::Fault> {
                a.checked_sub(self.0).and_then(|o| self.1.get(o as usize).copied()).ok_or(crate::Fault { addr: a, write: false })
            }
            fn write8(&mut self, _: u32, _: u8) -> Result<(), crate::Fault> {
                Ok(())
            }
        }
        let image = std::fs::read(std::env::var("LAYOUT_EXE").unwrap()).unwrap();
        let base = u32::from_str_radix(&std::env::var("LAYOUT_BASE").unwrap(), 16).unwrap();
        let roots: Vec<u64> = std::env::var("LAYOUT_ROOTS").unwrap().split(',').map(|r| u64::from_str_radix(r, 16).unwrap()).collect();
        let pe = u32::from_le_bytes(image[0x3c..0x40].try_into().unwrap()) as usize;
        let nsec = u16::from_le_bytes(image[pe + 6..pe + 8].try_into().unwrap()) as usize;
        let opt = u16::from_le_bytes(image[pe + 20..pe + 22].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(image[pe + 24 + 56..pe + 24 + 60].try_into().unwrap());
        let mut mapped = alloc::vec![0u8; size as usize];
        for i in 0..nsec {
            let sh = pe + 24 + opt + i * 40;
            let va = u32::from_le_bytes(image[sh + 12..sh + 16].try_into().unwrap()) as usize;
            let raw = u32::from_le_bytes(image[sh + 16..sh + 20].try_into().unwrap()) as usize;
            let ptr = u32::from_le_bytes(image[sh + 20..sh + 24].try_into().unwrap()) as usize;
            let n = raw.min(mapped.len() - va).min(image.len() - ptr);
            mapped[va..va + n].copy_from_slice(&image[ptr..ptr + n]);
        }
        super::super::LAYOUT.store(true, Relaxed);
        let mut m = Module::default();
        let r = translate_region64(&mut m, &mut Img(base, mapped), &roots, (0xfe00_0000, 0xff00_0000), |_, _| true).unwrap();
        std::println!("{}", r.layout);
        std::println!("entries {:x?}", r.starts);
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
