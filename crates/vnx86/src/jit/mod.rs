//! x86 -> wasm translation of basic blocks.
//!
//! A block runs from an address up to and including the first instruction that can transfer
//! control. It becomes one wasm function `(cpu: i32) -> i32` that keeps the eight general
//! registers in locals, records the status flags lazily ([`crate::lazy`]) and returns:
//!
//! - 0: done, continue at `cpu.eip`
//! - 1: run the interpreter at `cpu.eip` (an access outside the guest range, or anything
//!   the embedder wants checked the slow way)
//! - otherwise what the `step` import returned for an instruction it ran on the block's behalf
//!
//! Instructions the translator does not handle are run by the `step` import (the
//! interpreter, one instruction) in the middle of the block. A branch back to the block's own
//! start becomes a wasm loop; backward jumps count down `cpu.jit_budget` (signals are noticed
//! when it runs out).
//!
//! Imports of a block module: `env.memory` (the shared linear memory), `env.step(cpu) -> i32`,
//! `env.eflags(cpu) -> i32` (materializes and returns EFLAGS) and
//! `env.status(op, a, b, r, c) -> i32` (status flags of a lazy operation).

pub mod wasm;
pub mod dispatch;
mod structure;
pub mod saved;
mod packed;
pub mod long;

use crate::lazy;
use crate::{Cpu, Fpu, Memory};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::offset_of;
use iced_x86::{ConditionCode, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind, Register, RflagsBits};
use wasm::{Code, Func, FuncType, Import, Module, F64, I32, I64, V128};

/// Guest addresses the JIT accesses directly: [GUEST_LO, GUEST_HI). Anything else goes
/// through the interpreter (which knows the real rules).
pub const GUEST_LO: u32 = 0x10000;
pub const GUEST_HI: u32 = 0x8000_0000;

/// Longest block, in instructions.
const MAX_INS: usize = 200;

// block return codes
pub const RET_CONTINUE: i32 = 0;
pub const RET_INTERP: i32 = 1;
/// A RET left the region with the state in the function's results (status, next EIP,
/// registers, lazy flags, budget: the parameters' layout) instead of in the Cpu: a CALL that
/// made a wasm call continues right after it when EIP is its return address.
pub const RET_VALUES: i32 = 0x5245_5456;
/// Guest CALLs to other regions become wasm calls (returning through RET_VALUES) instead of
/// tail calls through the hash table (VNVM_JIT_CALLS=1; off by default, see
/// docs/translator.md).
pub static CALLS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Debugging (VNVM_JIT_COUNT): counters of control transfers the generated code bumps
/// (wasm32 only: their addresses are i32 constants). See [`COUNTER_NAMES`].
pub static COUNT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub static COUNTERS: [core::sync::atomic::AtomicU32; 15] = [const { core::sync::atomic::AtomicU32::new(0) }; 15];
pub const COUNTER_NAMES: [&str; 15] = ["call", "call-slow", "call-back|rest", "call-elsewhere", "call-exit", "ret-values", "lookup", "lookup-other", "lookup-own", "lookup-miss", "entry", "ret-ra1", "top", "side", "block"];
fn bump(c: &mut Code, i: usize) {
    if COUNT.load(core::sync::atomic::Ordering::Relaxed) && cfg!(target_pointer_width = "32") {
        let at = COUNTERS[i].as_ptr() as usize as i32;
        c.i32c(at).i32c(at).load32(0).i32c(1).add().store32(0);
    }
}
/// Functions entered in the middle too (VNVM_JIT_FN_ENTRIES=0: at their root only).
pub static FN_ENTRIES: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);
/// Debugging (VNVM_JIT_PROBE): instructions run by the interpreter (which prints the state
/// before them), 0 = none.
pub static PROBES: [core::sync::atomic::AtomicU32; 8] = [const { core::sync::atomic::AtomicU32::new(0) }; 8];
/// Indirect CALLs as wasm calls too (VNVM_JIT_CALLS=2)
pub static INDIRECT_CALLS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Function-level translation (VNVM_JIT_FN=1): a guest function (an address CALLs go to)
/// becomes one wasm function `(cpu) -> status` with the guest state in the Cpu at its entry
/// and exits. Its blocks are the ones its own jumps reach (never a call's target); a CALL is a
/// wasm call of the callee's function when it has one (found in the hash table, entries
/// tagged FN_TAG), and RET returns RET_FN with EIP in the Cpu. The caller goes on after the
/// call when EIP is its return address, else passes the status up (longjmp, SEH, too deep, a
/// callee that left for the dispatcher): the dispatcher continues at EIP. Status flags are
/// dead across CALL and RET (C calling conventions). Regions stop following calls too.
pub static FN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
fn fn_on() -> bool {
    FN.load(core::sync::atomic::Ordering::Relaxed)
}
/// A jump's target that is in L_NEXT (`Gen::push_target`).
const TARGET_NEXT: u32 = u32::MAX;
/// The type of functions: (cpu, call depth) -> status.
const T_FN: u32 = 4;
/// A function's RET (EIP in the Cpu).
pub const RET_FN: i32 = 0x5245_5446;
/// Hash table entries of functions: the table index of the function's adapter for the
/// region calling convention (the function itself is at the next index) | FN_TAG.
pub const FN_TAG: u32 = 1 << 31;
/// Most guest calls nested as wasm calls (deeper ones are tail calls: the wasm stack is small).
const MAX_DEPTH: u32 = 200;

// function indices in a block module
const F_STEP: u32 = 0;
const F_EFLAGS: u32 = 1;
const F_STATUS: u32 = 2;
const F_X87: u32 = 3; // (cpu, TOP, info, 8 f64): see x87_out
const F_CALLMISS: u32 = 4; // (cpu, slot, target) -> entry: see call_fn
const T_CALLMISS: u32 = 5;
/// Slots of a thread's call cache (Cpu::jit_calls).
pub const CALL_CACHE: u32 = 1 << 12;

// parameters of a region function: what a jump between regions hands over
const L_CPU: u32 = 0;
const L_ARG: u32 = 1; // entry: table index | block << ENTRY_SHIFT (functions: the call depth)
const L_REG: u32 = 2; // 8 registers
const L_OP: u32 = 10; // lazy flags (L_OP -1: in the Cpu)
const L_FA: u32 = 11;
const L_FB: u32 = 12;
const L_FR: u32 = 13;
const L_FC: u32 = 14;
const L_BUD: u32 = 15; // cpu.jit_budget while inside
pub(crate) const N_PARAMS: u32 = 16;
// locals
const L_NEXT: u32 = 16;
const L_ST: u32 = 17;
const L_BLK: u32 = 18;
// the latest call's return address in the region, and its block
const L_RA1: u32 = 19;
const L_RK1: u32 = 20;
const L_T: u32 = 21; // 6 temporaries
const N_T: u32 = 10;
const L_W: u32 = L_T + N_T; // i64 temporary
const L_F0: u32 = L_W + 1; // two f64 temporaries
const L_F1: u32 = L_W + 2;
const L_V0: u32 = L_W + 3; // two v128 temporaries
const L_V1: u32 = L_W + 4;
const N_LOCALS_I32: u32 = L_W - N_PARAMS;
const L_FTOP: u32 = L_V1 + 1; // x87 TOP when the block's cache started (i32)
const L_FREG: u32 = L_V1 + 2; // 8 f64: the cached x87 registers, by slot (see FCache)
const L_MMX: u32 = L_FREG + 8; // 8 v128: the cached MMX registers (see Gen::mmx)
const L_XMM: u32 = L_MMX + 8; // 8 v128: the cached XMM registers (see Gen::xmm)
const L_AB: u32 = L_XMM + 8; // i32: the base register of the instruction's memory operand (see Gen::folded)
const L_SLOT: u32 = L_AB + 1; // N_SLOTS i32: stack slots kept in locals in a loop (see Gen::slots)
/// functions: ESP as the caller passed it (the third parameter once the locals are compacted)
const L_ESPIN: u32 = L_SLOT + N_SLOTS;
const N_SLOTS: u32 = 6;
/// How many slots a loop keeps (at most N_SLOTS; VNVM_JIT_SLOTS, default 0: slower on x64,
/// where the extra live locals spill)
pub static SLOTS_MAX: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Pairs in the ring of jumps between regions (`Cpu::jit_ring`).
pub const RING_SIZE: u32 = 64;

/// A hash table entry's value: the function's table index | the block's index << ENTRY_SHIFT.
pub const ENTRY_SHIFT: u32 = 20;
/// A function's blocks it can be entered at are below this (hash entries: the entry's index in
/// `starts` << ENTRY_SHIFT, 0 the root).
const MAX_ENTRY: usize = 0x7fe;
/// Most blocks in one region.
pub const MAX_BLOCKS: usize = 1 << 11;
/// Most instructions in one region: V8's optimizing compiler takes superlinear time on big
/// functions (and runs the code in its baseline tier until it is done).
const MAX_REGION_INS: usize = 1000;
/// Where code may be rewritten without a protection change while translated (the host's
/// choice: a main executable's W|X pages, which packers decrypt in place): translations of
/// code there test each store against the pages with translated code (Cpu::jit_code_pages)
/// and leave at the next instruction with Cpu::jit_smc set when one hits.
static WATCH: spin_once::Once = spin_once::Once::new();

mod spin_once {
    use core::sync::atomic::{AtomicUsize, Ordering};
    pub struct Once(AtomicUsize);
    impl Once {
        pub const fn new() -> Self {
            Once(AtomicUsize::new(0))
        }
        pub fn set(&self, f: fn(u32) -> bool) {
            self.0.store(f as usize, Ordering::Relaxed);
        }
        pub fn get(&self) -> Option<fn(u32) -> bool> {
            let v = self.0.load(Ordering::Relaxed);
            // SAFETY: only ever set from a `fn(u32) -> bool`
            (v != 0).then(|| unsafe { core::mem::transmute::<usize, fn(u32) -> bool>(v) })
        }
    }
}

pub fn set_watch(f: fn(u32) -> bool) {
    WATCH.set(f);
}

/// Code never translated, inlined callees' too (the host's: pages the interpreter keeps, see
/// vnhost's jit::debug_allowed).
static DENY: spin_once::Once = spin_once::Once::new();

pub fn set_deny(f: fn(u32) -> bool) {
    DENY.set(f);
}

/// The same for functions (jit::FN; VNVM_JIT_FN_INS)
pub static FN_MAX_INS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(1000);
/// Inlined callees: at most this many instructions each, and in all in a region.
const MAX_INLINE: usize = 128;
const MAX_INLINE_TOTAL: usize = 400;

/// Field offsets in [`Cpu`].
struct Off;
impl Off {
    fn reg(i: usize) -> u32 {
        (offset_of!(Cpu, regs) + 4 * i) as u32
    }
    fn eip() -> u32 {
        offset_of!(Cpu, eip) as u32
    }
    fn eflags() -> u32 {
        offset_of!(Cpu, eflags) as u32
    }
    fn lazy() -> u32 {
        offset_of!(Cpu, lazy) as u32
    }
    fn fs() -> u32 {
        offset_of!(Cpu, fs_base) as u32
    }
    fn gs() -> u32 {
        offset_of!(Cpu, gs_base) as u32
    }
    fn budget() -> u32 {
        offset_of!(Cpu, jit_budget) as u32
    }
    fn calls() -> u32 {
        offset_of!(Cpu, jit_depth) as u32
    }
    fn code_pages() -> u32 {
        offset_of!(Cpu, jit_code_pages) as u32
    }
    fn smc() -> u32 {
        offset_of!(Cpu, jit_smc) as u32
    }
    fn call_cache() -> u32 {
        offset_of!(Cpu, jit_calls) as u32
    }
    fn mxcsr() -> u32 {
        offset_of!(Cpu, mxcsr) as u32
    }
    fn fpu(field: usize) -> u32 {
        (offset_of!(Cpu, fpu) + field) as u32
    }
    fn fregs() -> u32 {
        Off::fpu(offset_of!(Fpu, regs))
    }
    fn fints() -> u32 {
        Off::fpu(offset_of!(Fpu, ints))
    }
    fn fexact() -> u32 {
        Off::fpu(offset_of!(Fpu, exact))
    }
    fn fempty() -> u32 {
        Off::fpu(offset_of!(Fpu, empty))
    }
    fn fcw() -> u32 {
        Off::fpu(offset_of!(Fpu, cw))
    }
    fn ftop() -> u32 {
        Off::fpu(offset_of!(Fpu, top))
    }
    fn xmm(r: Register) -> u32 {
        (offset_of!(Cpu, xmm) + 16 * (r as usize - Register::XMM0 as usize)) as u32
    }
    fn mm(r: Register) -> u32 {
        (offset_of!(Cpu, mm) + 8 * (r as usize - Register::MM0 as usize)) as u32
    }
}

/// A translated region: one function holding blocks found from an entry by following direct
/// branches. Blocks jump to each other inside the function; returns and indirect jumps look
/// the target up in the embedder's hash table and stay inside if it is one of the region's own.
pub struct Region {
    /// (block start address, block i): entered with `(table index | i << ENTRY_SHIFT)`
    pub starts: Vec<(u32, u32)>,
    pub instructions: usize,
    /// the guest code it was translated from: (address, length)
    pub code: Vec<(u32, u32)>,
    /// with [`LAYOUT`] on: the blocks in order (address, inline context) and the loops, then
    /// `SZ <mnemonic> <count> <bytes>` lines and the function's size
    pub layout: String,
    /// with [`PROFILE`] on: per block (address, address of its u32 entry counter, the
    /// instructions as "mnemonic operands" keys)
    pub profile: Vec<(u32, u32, Vec<String>)>,
    /// functions: the indirect calls and jumps (addresses) whose targets the ring samples
    pub sites: Vec<u32>,
}

/// Debugging: count block entries (a u32 per block, leaked) and fill [`Region::profile`].
/// wasm32 only (the counter address is an i32 constant).
pub static PROFILE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// "mnemonic operands" for profiles: operands as r8/r16/r32/mm/xmm/st/seg, m<bits>, i.
pub fn profile_key(ins: &Instruction) -> String {
    let mut k = alloc::format!("{:?}", ins.mnemonic()).to_lowercase();
    for i in 0..ins.op_count() {
        k.push(if i == 0 { ' ' } else { ',' });
        match ins.op_kind(i) {
            OpKind::Register => {
                let r = ins.op_register(i);
                k.push_str(if r.is_gpr8() {
                    "r8"
                } else if r.is_gpr16() {
                    "r16"
                } else if r.is_gpr32() {
                    "r32"
                } else if r.is_mm() {
                    "mm"
                } else if r.is_xmm() {
                    "xmm"
                } else if r.is_st() {
                    "st"
                } else if r.is_segment_register() {
                    "seg"
                } else {
                    "reg"
                });
            }
            OpKind::Memory => {
                k.push('m');
                k.push_str(&alloc::format!("{}", ins.memory_size().size() * 8));
            }
            OpKind::NearBranch32 | OpKind::NearBranch16 => k.push_str("rel"),
            OpKind::MemorySegESI | OpKind::MemorySegSI | OpKind::MemoryESDI | OpKind::MemoryESEDI | OpKind::MemorySegEDI => k.push_str("str"),
            _ => k.push('i'),
        }
    }
    if ins.has_rep_prefix() || ins.has_repe_prefix() {
        k.insert_str(0, "rep ");
    } else if ins.has_repne_prefix() {
        k.insert_str(0, "repne ");
    }
    if ins.has_lock_prefix() {
        k.insert_str(0, "lock ");
    }
    k
}

/// Debugging: fill [`Region::layout`].
pub static LAYOUT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Where the status flags are at a point of the block.
#[derive(Clone, Copy, PartialEq)]
enum Flags {
    /// not known statically: the lazy operation in L_OP/L_FA.. if L_OP != -1, else in the Cpu
    /// (`eflags`, or its own lazy state)
    Entry,
    /// lazy operation `op` (kind * 4 + size) with operands in L_FA/L_FB/L_FR/L_FC
    Known(u32),
}

struct Gen<'a> {
    c: Code,
    /// a function (FN), not a region
    fnm: bool,
    /// laid out by `structure` (jumps go to open labels or leave)
    structured: bool,
    /// dispatch nodes of jump tables: (context, table) -> node (after the blocks)
    jnode: BTreeMap<usize, usize>,
    /// The guest registers the code names (the others stay in the Cpu: never loaded or
    /// stored) and those it writes (the only ones that can differ from the Cpu's after a load);
    /// all for regions
    regs: u8,
    wregs: u8,
    /// registers written since they were last loaded from the Cpu, in the block so far: what
    /// a call or RET stores
    dirty: u8,
    /// functions: per block, those that can be dirty at its start (see [`dirty_in`]; all of
    /// `wregs` if absent)
    dirty_in: Vec<u8>,
    /// per register, the largest value it can have here if known (since an `and r32, imm` or
    /// a movzx in the block): an access to [table + r * scale] then needs no check
    small: [Option<u32>; 8],
    /// functions: per block, the registers checked to be in the guest range on every way in
    /// (see [`safe_in`])
    safe_in: Vec<u8>,
    /// loop headers (functions) whose registers in the mask are checked on the way into the
    /// loop instead of in it (see [`safe_in`])
    hoist: BTreeMap<usize, (u8, u32)>,
    /// functions: per block, the blocks with an edge to it, and the lazy operation each block
    /// emitted so far ends with (see [`Gen::predict`])
    preds: Vec<Vec<u32>>,
    flags_out: Vec<Option<u32>>,
    /// the block's stores are tested for rewriting translated code (see [`WATCH`])
    smc: bool,
    /// with [`LAYOUT`]: whether the instruction went to the interpreter, and per mnemonic
    /// ("*" marking those) the count and wasm bytes
    fell_back: bool,
    sizes: BTreeMap<String, (u32, u32)>,
    /// blocks done by [`Gen::hle`] instead of their guest code
    hle: BTreeMap<u32, Hle>,
    /// functions: import thunks (`jmp [slot]`) calls go to: thunk -> slot (the call goes where
    /// the slot points, one wasm call instead of two)
    thunks: BTreeMap<u32, u32>,
    /// with [`PROFILE`]: each block's entry counter address
    prof: Vec<u32>,
    /// open wasm blocks inside the exit block
    depth: u32,
    flags: Flags,
    /// (context, address) -> block; context 0 is the region's own code, others inlined calls
    targets: &'a BTreeMap<(u32, u32), u32>,
    block_index: u32,
    ctx: u32,
    ctxs: &'a [u32],
    /// per context: the return address of its call
    ret_to: &'a [u32],
    /// call blocks with an inlined callee: (its entry block, the target an indirect call checks)
    inlined: &'a BTreeMap<usize, (usize, Option<u32>)>,
    /// status flags live at the end of each block
    live_out: Vec<u32>,
    loops: &'a BTreeMap<usize, usize>,
    /// per block: the (absolute) depth of an open wasm label just before it, for forward
    /// jumps; of the open loop label it heads, for backward ones; whether it heads a loop
    fwd: Vec<Option<u32>>,
    back: Vec<Option<u32>>,
    head: Vec<bool>,
    /// return addresses of the region's own calls (checked first by RET)
    returns: &'a [u32],
    ins: &'a Instruction,
    /// status flags read after this instruction (iced RflagsBits)
    live_after: u32,
    temps: u32,
    /// Registers whose value was checked to be at least SAFE_MARGIN inside the guest range,
    /// with how far they moved since (push/pop): accesses near them need no check.
    safe: [Option<i32>; 8],
    /// The same for base + index << scale sums (base, index, scale shift; neither ESP): checked
    /// once, accesses near the sum need no check until one of the two registers changes
    safe2: Vec<(u8, u8, u8)>,
    fc: FCache,
    /// MMX registers whose value is in L_MMX + i (low half; the high half is anything), and those changed: kept
    /// until the block leaves or calls the interpreter, like the x87 ones
    mmx: (u8, u8),
    /// XMM registers whose value is in L_XMM + i, and those changed; for the 128-bit moves and
    /// logic ops only (any other instruction naming an XMM register writes them back and
    /// drops them first)
    xmm: (u8, u8),
    /// The instruction's memory operand when it is a checked register (copied to L_AB) plus a
    /// positive displacement: (the address local, the displacement), which operand accesses
    /// then put in the load's or store's offset (the add can't wrap: the register is well
    /// inside the guest range)
    folded: Option<(u32, u32)>,
    /// A single-block loop whose MMX registers stay in locals across iterations: (block, the
    /// registers it names, loaded before the loop), and those of the block being emitted
    mm_pre: Option<(usize, u8)>,
    mm_loop: u8,
    /// The same for XMM registers (when the loop names them only in 128-bit moves and logic ops)
    xm_pre: Option<(usize, u8)>,
    xm_loop: u8,
    /// Stack slots kept in locals through the loop being emitted: 32-bit [base + disp] (base a
    /// register the loop doesn't write), slot i in L_SLOT + i, loaded before the loop. Stores
    /// write memory too, so it is always current; any other store in the loop that touches
    /// them reloads them (see `slot_loops`).
    slots: Option<(Register, Vec<i32>)>,
    /// The x87 cache across a loop with x87 code (its blocks): the cache each block starts with
    /// (`fc_in`, none: empty); jumps between them change the cache in place to the target's,
    /// or keep it when it is the same. `fc_out` gets the cache each jump came with (the first
    /// per target) while the loop is emitted to find them (see emit_level).
    fc_range: Option<(usize, usize)>,
    fc_in: BTreeMap<usize, FCache>,
    fc_out: BTreeMap<usize, FCache>,
    /// The same across a whole function (structured): (its root, its number of blocks); the
    /// root and the dispatch nodes start with an empty cache
    fc_fn: Option<(usize, usize)>,
    notes: String,
    /// SETcc r8 followed by MOVZX r32, r8: emitted as one write of r32
    fuse_zx: bool,
}

/// The x87 registers a block has touched, kept in locals until it leaves. Slot r is the
/// physical register (L_FTOP + r) & 7, so ST(i) is slot (delta + i) & 7. Bit r of each mask:
/// `have` its value is in L_FREG + r, `dirty` it was changed; `valid`/`empty` its empty and exact
/// bits are to be set as after a write (not empty, not exact) or a pop (empty). The Cpu is
/// brought up to date whenever the block leaves or calls the interpreter.
#[derive(Clone, Copy, Default, PartialEq)]
struct FCache {
    on: bool,
    delta: i32,
    have: u8,
    dirty: u8,
    valid: u8,
    empty: u8,
}

/// See [`Gen::safe`].
const SAFE_MARGIN: i32 = 0x8000;
/// Measuring only (VNVM_JIT_NOCHECK=1): no range checks on memory accesses (unsafe).
pub static NOCHECK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Pushes what a jump to another region hands over (registers, lazy flags, budget).
fn push_state(c: &mut Code) {
    for l in L_REG..N_PARAMS {
        c.get(l);
    }
}
fn nocheck() -> bool {
    NOCHECK.load(core::sync::atomic::Ordering::Relaxed)
}

const STATUS_BITS: u32 =
    RflagsBits::OF | RflagsBits::SF | RflagsBits::ZF | RflagsBits::AF | RflagsBits::CF | RflagsBits::PF;

fn reg_index(r: Register) -> Option<(u32, u32, bool)> {
    if r.is_gpr32() {
        Some((r.number() as u32, 32, false))
    } else if r.is_gpr16() {
        Some((r.number() as u32, 16, false))
    } else if r.is_gpr8() {
        let n = r.number() as u32;
        Some(if n < 4 { (n, 8, false) } else { (n - 4, 8, true) })
    } else {
        None
    }
}

/// `& m`, unless m keeps everything.
fn and_mask(c: &mut Code, m: u32) {
    if m != u32::MAX {
        c.u32c(m).and();
    }
}

fn mask(bits: u32) -> u32 {
    if bits >= 32 { u32::MAX } else { (1 << bits) - 1 }
}

/// An operand the generator can handle.
fn simple_operand(ins: &Instruction, i: u32) -> bool {
    match ins.op_kind(i) {
        OpKind::Register => reg_index(ins.op_register(i)).is_some(),
        OpKind::Memory => {
            let ok_reg = |r: Register| r == Register::None || r.is_gpr32();
            ok_reg(ins.memory_base())
                && ok_reg(ins.memory_index())
                && matches!(ins.memory_segment(), Register::DS | Register::SS | Register::ES | Register::CS | Register::FS | Register::GS)
                && matches!(ins.memory_size().size(), 1 | 2 | 4)
        }
        OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate32 | OpKind::Immediate8to16 | OpKind::Immediate8to32 => true,
        _ => false,
    }
}

/// A memory operand with 32-bit registers and an ordinary segment.
fn mem_ok(ins: &Instruction) -> bool {
    let ok_reg = |r: Register| r == Register::None || r.is_gpr32();
    ok_reg(ins.memory_base())
        && ok_reg(ins.memory_index())
        && matches!(ins.memory_segment(), Register::DS | Register::SS | Register::ES | Register::CS | Register::FS | Register::GS)
}

/// x87 memory operand sizes the JIT handles (f32/f64).
fn fmem_ok(ins: &Instruction) -> bool {
    mem_ok(ins) && matches!(ins.memory_size(), iced_x86::MemorySize::Float32 | iced_x86::MemorySize::Float64)
}

/// Operand `i` is an XMM register or a 128-bit memory operand the JIT can address.
/// The 128-bit bitwise logic ops (on an XMM register and an XMM register or m128).
fn is_logic(ins: &Instruction) -> bool {
    use Mnemonic::*;
    matches!(ins.mnemonic(), Pxor | Xorps | Xorpd | Por | Orps | Orpd | Pand | Andps | Andpd | Pandn | Andnps | Andnpd)
        && ins.op_kind(0) == OpKind::Register
        && ins.op_register(0).is_xmm()
        && xmm_or_m128(ins, 1)
}

/// The instructions that use the XMM cache (see `Gen::xmm`): 128-bit moves and logic ops.
fn xmm_cached(ins: &Instruction) -> bool {
    use Mnemonic::*;
    is_logic(ins)
        || matches!(ins.mnemonic(), Movdqu | Movdqa | Movups | Movaps | Movupd | Movapd | Lddqu | Movntps | Movntpd | Movntdq)
            && xmm_or_m128(ins, 0)
            && xmm_or_m128(ins, 1)
}

fn xmm_or_m128(ins: &Instruction, i: u32) -> bool {
    match ins.op_kind(i) {
        OpKind::Register => ins.op_register(i).is_xmm(),
        OpKind::Memory => mem_ok(ins) && ins.memory_size().size() == 16,
        _ => false,
    }
}

/// x87 instructions the block's register cache (FCache) implements.
fn fc_handles(ins: &Instruction) -> bool {
    use Mnemonic::*;
    let reg = |i| ins.op_kind(i) == OpKind::Register;
    match ins.mnemonic() {
        Fld | Fst | Fstp => if reg(0) { true } else { fmem_ok(ins) },
        Fldz | Fld1 => true,
        Fild => matches!(ins.memory_size(), iced_x86::MemorySize::Int16 | iced_x86::MemorySize::Int32 | iced_x86::MemorySize::Int64) && mem_ok(ins),
        Fxch => ins.op_count() == 2,
        Fist | Fistp => matches!(ins.memory_size(), iced_x86::MemorySize::Int16 | iced_x86::MemorySize::Int32 | iced_x86::MemorySize::Int64) && mem_ok(ins),
        Fadd | Faddp | Fsub | Fsubp | Fsubr | Fsubrp | Fmul | Fmulp | Fdiv | Fdivp | Fdivr | Fdivrp => {
            (ins.op_count() == 1 && ins.op_kind(0) == OpKind::Memory && fmem_ok(ins)) || (ins.op_count() == 2 && reg(0))
        }
        _ => false,
    }
}

/// A locked (or XCHG) read-modify-write of a dword in memory, from a register or an immediate.
fn locked32(ins: &Instruction) -> bool {
    use Mnemonic::*;
    let mem = (0..ins.op_count()).filter(|&i| ins.op_kind(i) == OpKind::Memory).count() == 1;
    let src_ok = (0..ins.op_count()).all(|i| match ins.op_kind(i) {
        OpKind::Memory => mem_ok(ins) && ins.memory_size().size() == 4,
        OpKind::Register => ins.op_register(i).is_gpr32(),
        OpKind::Immediate8to32 | OpKind::Immediate32 => true,
        _ => false,
    });
    let ok = match ins.mnemonic() {
        Inc | Dec => ins.op_count() == 1,
        Xadd | Cmpxchg => ins.op_kind(0) == OpKind::Memory && ins.op_kind(1) == OpKind::Register,
        Xchg => ins.op_count() == 2,
        _ => ins.op_count() == 2 && ins.op_kind(0) == OpKind::Memory,
    };
    (ins.has_lock_prefix() || ins.mnemonic() == Xchg) && mem && src_ok && ok
}

/// MOVS/STOS/LODS/... with 32-bit addressing (not the SSE MOVSD)
fn is_string(ins: &Instruction) -> bool {
    (0..ins.op_count()).any(|i| matches!(ins.op_kind(i), OpKind::MemoryESEDI | OpKind::MemorySegESI))
}

/// x87 instructions that leave the register cache alone (the control word is not cached).
fn fpu_neutral(ins: &Instruction) -> bool {
    use Mnemonic::*;
    matches!(ins.mnemonic(), Wait | Fldcw | Fnstcw | Fstcw) && (ins.mnemonic() == Wait || mem_ok(ins))
}

/// Reads or writes x87/MMX state (the cache must be in the Cpu, and is stale after).
fn touches_fpu(ins: &Instruction) -> bool {
    use iced_x86::CpuidFeature::*;
    ins.cpuid_features().iter().any(|f| matches!(f, FPU | FPU287 | FPU387 | FXSR | MMX))
        || (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_mm())
}

fn st_index(r: Register) -> u32 {
    r as u32 - Register::ST0 as u32
}

fn op_bits(ins: &Instruction, i: u32) -> u32 {
    match ins.op_kind(i) {
        OpKind::Register => reg_index(ins.op_register(i)).map_or(32, |r| r.1),
        OpKind::Memory => ins.memory_size().size() as u32 * 8,
        OpKind::Immediate8 => 8,
        OpKind::Immediate16 | OpKind::Immediate8to16 => 16,
        _ => 32,
    }
}

fn cc_index(cc: ConditionCode) -> u32 {
    match cc {
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
        _ => 15,
    }
}

/// iced RflagsBits a condition reads
fn cc_reads(cc: u32) -> u32 {
    match cc >> 1 {
        0 => RflagsBits::OF,
        1 => RflagsBits::CF,
        2 => RflagsBits::ZF,
        3 => RflagsBits::CF | RflagsBits::ZF,
        4 => RflagsBits::SF,
        5 => RflagsBits::PF,
        6 => RflagsBits::SF | RflagsBits::OF,
        _ => RflagsBits::ZF | RflagsBits::SF | RflagsBits::OF,
    }
}

impl<'a> Gen<'a> {
    fn temp(&mut self) -> u32 {
        let t = L_T + self.temps;
        self.temps += 1;
        assert!(self.temps <= N_T);
        t
    }

    fn eip(&self) -> u32 {
        self.ins.ip32()
    }

    /// Leaves the block: continue at the address on top of the stack with status `st`.
    fn exit_with(&mut self, st: i32) {
        self.mmx_flush();
        self.xmm_flush();
        self.x87_flush_out();
        self.c.set(L_NEXT).i32c(st).set(L_ST).br(self.depth);
    }

    /// The C library function `h` at the block's address (its arguments on the stack; the
    /// block's RET returns). The guest code runs instead (in the interpreter) when a range is
    /// outside the guest's.
    fn hle(&mut self, h: Hle) {
        let (sp, d, s, n) = (L_REG + 4, self.temp(), self.temp(), self.temp());
        self.check(sp, 16);
        self.c.get(sp).load32(4).set(d).get(sp).load32(8).set(s).get(sp).load32(12).set(n);
        let span = GUEST_HI - GUEST_LO;
        self.c.get(n).u32c(span).gt_u();
        self.c.get(d).u32c(GUEST_LO).sub().u32c(span).get(n).sub().gt_u().or();
        if h == Hle::Move {
            self.c.get(s).u32c(GUEST_LO).sub().u32c(span).get(n).sub().gt_u().or();
        }
        self.c.if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
        self.c.get(d).get(s).get(n);
        match h {
            // wasm's memory.copy is a memmove
            Hle::Move => self.c.memory_copy(),
            Hle::Set => self.c.memory_fill(),
        };
        self.c.get(d).set(L_REG);
        self.temps -= 3;
    }

    /// After a store to `e` (see [`WATCH`]): when its page has translated code, leaves for the
    /// host at the next instruction with Cpu::jit_smc = e.
    fn smc_check(&mut self, e: u32) {
        self.c.get(L_CPU).load32(Off::code_pages());
        self.c.get(e).i32c(17).shr_u().i32c(2).shl().add().load32(0);
        self.c.get(e).i32c(12).shr_u().shr_u().i32c(1).and();
        self.c.if_();
        self.depth += 1;
        self.c.get(L_CPU).get(e).store32(Off::smc());
        let next = self.ins.next_ip32();
        self.c.u32c(next);
        self.exit_with(RET_INTERP);
        self.depth -= 1;
        self.c.end();
    }

    /// Leaves the block so the interpreter runs the current instruction.
    fn bail(&mut self) {
        let eip = self.eip();
        self.c.u32c(eip);
        self.exit_with(RET_INTERP);
    }

    // ---- x87 (the f64 model of crate::fpu, cached per block: FCache)

    /// Starts the cache at the first x87 instruction of the block.
    fn fc_begin(&mut self) {
        if !self.fc.on {
            self.c.get(L_CPU).load32(Off::ftop()).set(L_FTOP);
            self.fc = FCache { on: true, ..FCache::default() };
        }
    }

    /// Slot of ST(i).
    fn fc_slot(&mut self, i: u32) -> u32 {
        self.fc_begin();
        (self.fc.delta + i as i32).rem_euclid(8) as u32
    }

    /// New temporary with the physical register of slot `r`.
    fn fc_phys(&mut self, r: u32) -> u32 {
        let p = self.temp();
        self.c.get(L_FTOP);
        if r != 0 {
            self.c.i32c(r as i32).add();
        }
        self.c.i32c(7).and().set(p);
        p
    }

    /// Pushes the address of slot `r`'s register (the Fpu field offset goes on the access).
    fn fc_phys_addr(&mut self, r: u32) {
        self.c.get(L_FTOP);
        if r != 0 {
            self.c.i32c(r as i32).add();
        }
        self.c.i32c(7).and().i32c(3).shl().get(L_CPU).add();
    }

    /// The local holding ST(i), loading it from the Cpu the first time.
    fn fc_get(&mut self, i: u32) -> u32 {
        let r = self.fc_slot(i);
        if self.fc.have & (1 << r) == 0 {
            self.fc_phys_addr(r);
            self.c.f64load(Off::fregs()).set(L_FREG + r);
            self.fc.have |= 1 << r;
        }
        L_FREG + r
    }

    /// Slot `r` was written (its local holds the value).
    fn fc_wrote(&mut self, r: u32) {
        let b = 1 << r;
        self.fc.have |= b;
        self.fc.dirty |= b;
        self.fc.valid |= b;
        self.fc.empty &= !b;
    }

    /// Slot `r`'s tags are set in the Cpu right away from here (not when leaving).
    fn fc_runtime_tags(&mut self, r: u32) {
        self.fc.valid &= !(1 << r);
        self.fc.empty &= !(1 << r);
    }

    fn fc_pop(&mut self) {
        let b = 1 << self.fc_slot(0);
        self.fc.have &= !b;
        self.fc.dirty &= !b;
        self.fc.valid &= !b;
        self.fc.empty |= b;
        self.fc.delta += 1;
    }

    /// Pushes `mask` (by slot) rotated to physical registers, in its low 8 bits (the rest is
    /// anything: it only goes into u8 stores): mask:mask << TOP >> 8.
    fn fc_rot(&mut self, mask: u8) {
        self.c.i32c(mask as i32 * 0x101).get(L_FTOP).shl().i32c(8).shr_u();
    }

    /// Brings the Cpu up to date (code only: the caches stay as they are).
    fn fc_flush(&mut self) {
        self.mmx_flush();
        self.xmm_flush();
        self.x87_flush();
    }

    fn xmm_flush(&mut self) {
        for i in 0..8 {
            if self.xmm.1 & (1 << i) != 0 {
                self.c.get(L_CPU).get(L_XMM + i).v128store(Off::xmm(Register::XMM0 + i));
            }
        }
    }

    /// The local holding XMM register `r`, loaded from the Cpu the first time in the block.
    fn xmm_get(&mut self, r: Register) -> u32 {
        let i = r as u32 - Register::XMM0 as u32;
        if self.xmm.0 & (1 << i) == 0 {
            self.c.get(L_CPU).v128load(Off::xmm(r)).set(L_XMM + i);
            self.xmm.0 |= 1 << i;
        }
        L_XMM + i
    }

    /// Sets XMM register `r` from the v128 on the stack.
    fn xmm_set(&mut self, r: Register) {
        let i = r as u32 - Register::XMM0 as u32;
        self.c.set(L_XMM + i);
        self.xmm.0 |= 1 << i;
        self.xmm.1 |= 1 << i;
    }

    fn mmx_flush(&mut self) {
        for i in 0..8 {
            if self.mmx.1 & (1 << i) != 0 {
                self.c.get(L_CPU).get(L_MMX + i).low64().store64(Off::mm(Register::MM0 + i));
            }
        }
    }

    /// [`Self::x87_flush`] when leaving: by a call to [`x87_out`] unless it is a store or two.
    /// (Inline, V8 computed the tag masks of all the exits in a loop ahead of it, in the hot
    /// path, and kept them in spilled registers; the code is smaller too.)
    fn x87_flush_out(&mut self) {
        let fc = self.fc;
        if !fc.on || fc.dirty.count_ones() + if fc.valid | fc.empty != 0 { 4 } else { 0 } <= 2 {
            return self.x87_flush();
        }
        self.c.get(L_CPU).get(L_FTOP);
        let info = fc.dirty as u32 | (fc.valid as u32) << 8 | (fc.empty as u32) << 16 | (fc.delta.rem_euclid(8) as u32) << 24;
        self.c.u32c(info);
        for r in 0..8 {
            self.c.get(L_FREG + r);
        }
        self.c.call(F_X87);
    }

    fn x87_flush(&mut self) {
        let fc = self.fc;
        if !fc.on {
            return;
        }
        for r in 0..8 {
            if fc.dirty & (1 << r) != 0 {
                self.fc_phys_addr(r);
                self.c.get(L_FREG + r).f64store(Off::fregs());
            }
        }
        if fc.valid | fc.empty != 0 {
            // empty = empty & ~valid | popped; exact &= ~(valid | popped) (u8 stores drop the
            // bits rotated past 7)
            self.c.get(L_CPU).get(L_CPU).load8u(Off::fempty());
            self.fc_rot(fc.valid);
            self.c.i32c(-1).xor().and();
            self.fc_rot(fc.empty);
            self.c.or().store8(Off::fempty());
            self.c.get(L_CPU).get(L_CPU).load8u(Off::fexact());
            self.fc_rot(fc.valid | fc.empty);
            self.c.i32c(-1).xor().and().store8(Off::fexact());
        }
        if fc.delta.rem_euclid(8) != 0 {
            self.c.get(L_CPU).get(L_FTOP).i32c(fc.delta).add().i32c(7).and().store32(Off::ftop());
        }
    }

    /// Pushes the address of register `p` (the Fpu field offset goes on the load/store).
    fn freg_at(&mut self, p: u32, shift: i32) {
        self.c.get(p).i32c(shift).shl().get(L_CPU).add();
    }

    fn fbit(&mut self, field: u32, p: u32, set: bool) {
        self.c.get(L_CPU).get(L_CPU).load8u(field).i32c(1).get(p).shl();
        if set {
            self.c.or();
        } else {
            self.c.i32c(-1).xor().and();
        }
        self.c.store8(field);
    }

    /// Pushes the f32/f64 memory operand at checked address `a` as f64.
    fn fload_mem(&mut self, a: u32) {
        let o = self.mem_at(a);
        if self.ins.memory_size() == iced_x86::MemorySize::Float32 {
            self.c.f32load(o).op(0xbb /* f64.promote_f32 */);
        } else {
            self.c.f64load(o);
        }
    }

    /// Swaps bits `a` and `b` of a byte field.
    fn fswap_bits(&mut self, field: u32, a: u32, b: u32) {
        let (x, y) = (self.temp(), self.temp());
        self.c.get(L_CPU).load8u(field).set(x);
        // y = ((x >> a) ^ (x >> b)) & 1; x ^= y << a | y << b
        self.c.get(x).get(a).shr_u().get(x).get(b).shr_u().xor().i32c(1).and().set(y);
        self.c.get(L_CPU).get(x).get(y).get(a).shl().get(y).get(b).shl().or().xor().store8(field);
        self.temps -= 2;
    }

    // ---- registers

    fn get_reg(&mut self, r: Register) {
        let (i, bits, high) = reg_index(r).unwrap();
        self.c.get(L_REG + i);
        match (bits, high) {
            (32, _) => {}
            (16, _) => {
                self.c.u32c(0xffff).and();
            }
            (8, false) => {
                self.c.u32c(0xff).and();
            }
            _ => {
                self.c.i32c(8).shr_u().u32c(0xff).and();
            }
        }
    }

    /// Sets register `r` from the value in local `v`.
    fn set_reg_from(&mut self, r: Register, v: u32) {
        let (i, bits, high) = reg_index(r).unwrap();
        let l = L_REG + i;
        match (bits, high) {
            (32, _) => {
                self.c.get(v).set(l);
            }
            (16, _) => {
                self.c.get(l).u32c(0xffff_0000).and().get(v).u32c(0xffff).and().or().set(l);
            }
            (8, false) => {
                self.c.get(l).u32c(0xffff_ff00).and().get(v).u32c(0xff).and().or().set(l);
            }
            _ => {
                self.c.get(l).u32c(0xffff_00ff).and().get(v).u32c(0xff).and().i32c(8).shl().or().set(l);
            }
        }
    }

    // ---- memory

    /// Computes the linear address of the memory operand into a new temporary and checks it
    /// (leaving the block if outside the guest range).
    fn addr(&mut self, size: u32) -> u32 {
        let ins = self.ins;
        let t = self.temp();
        if let Some(t) = self.addr_pair(t, size) {
            return t;
        }
        self.c.u32c(ins.memory_displacement32());
        if ins.memory_base() != Register::None {
            self.c.get(L_REG + ins.memory_base().number() as u32).add();
        }
        if ins.memory_index() != Register::None {
            self.c.get(L_REG + ins.memory_index().number() as u32);
            match ins.memory_index_scale() {
                1 => {}
                s => {
                    self.c.i32c(s.trailing_zeros() as i32).shl();
                }
            }
            self.c.add();
        }
        match ins.memory_segment() {
            Register::FS => {
                self.c.get(L_CPU).load32(Off::fs()).add();
            }
            Register::GS => {
                self.c.get(L_CPU).load32(Off::gs()).add();
            }
            _ => {}
        }
        self.c.set(t);
        let base = ins.memory_base();
        let seg = ins.memory_segment();
        // [table + r * scale] with r known small: in the guest range whatever r is
        if base == Register::None && ins.memory_index().is_gpr32() && seg != Register::FS && seg != Register::GS {
            if let Some(m) = self.small[ins.memory_index().number()] {
                let lo = ins.memory_displacement32() as u64;
                let hi = lo + m as u64 * ins.memory_index_scale() as u64 + size as u64;
                if lo >= GUEST_LO as u64 && hi <= GUEST_HI as u64 {
                    return t;
                }
            }
        }
        if base.is_gpr32() && ins.memory_index() == Register::None && seg != Register::FS && seg != Register::GS {
            let disp = ins.memory_displacement32() as i32;
            if self.near_safe(base.number(), disp, size) {
                if disp > 0 {
                    self.c.get(L_REG + base.number() as u32).set(L_AB);
                    self.folded = Some((t, disp as u32));
                }
                return t;
            }
        }
        if nocheck() {
            return t;
        }
        // checked as t - GUEST_LO, which the access then uses with GUEST_LO as its offset
        // (V8 folds the subtraction into the address arithmetic: one instruction less)
        self.c.get(t).u32c(GUEST_LO).sub().tee(L_AB).u32c(GUEST_HI - GUEST_LO - size).gt_u().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
        self.folded = Some((t, GUEST_LO));
        t
    }

    /// [base + index << scale + disp] with a small displacement: the sum is checked to be
    /// SAFE_MARGIN inside the guest range (once per block until base or index changes; V8
    /// can't merge the checks itself, every one has its own exit), then the access needs none.
    /// The sum is in L_AB, a positive displacement folded into the access (see `folded`).
    fn addr_pair(&mut self, t: u32, size: u32) -> Option<u32> {
        let ins = self.ins;
        let (base, index) = (ins.memory_base(), ins.memory_index());
        let disp = ins.memory_displacement32() as i32;
        if !base.is_gpr32() || !index.is_gpr32() || base == Register::ESP || index == Register::ESP
            || matches!(ins.memory_segment(), Register::FS | Register::GS)
            || disp.unsigned_abs() >= SAFE_MARGIN as u32 / 2
        {
            return None;
        }
        let key = (base.number() as u8, index.number() as u8, ins.memory_index_scale().trailing_zeros() as u8);
        self.c.get(L_REG + key.0 as u32).get(L_REG + key.1 as u32);
        if key.2 != 0 {
            self.c.i32c(key.2 as i32).shl();
        }
        self.c.add().set(L_AB);
        if !self.safe2.contains(&key) && !nocheck() {
            self.c.get(L_AB).u32c(GUEST_LO + SAFE_MARGIN as u32).sub().u32c(GUEST_HI - GUEST_LO - 2 * SAFE_MARGIN as u32).gt_u().if_();
            self.depth += 1;
            self.bail();
            self.depth -= 1;
            self.c.end();
            if self.safe2.len() >= 4 {
                self.safe2.remove(0);
            }
            self.safe2.push(key);
        }
        self.c.get(L_AB).i32c(disp).add().set(t);
        if disp > 0 {
            self.folded = Some((t, disp as u32));
        }
        let _ = size;
        Some(t)
    }

    /// On the way into loop `x`: the checks [`safe_in`] moved out of it, leaving for the
    /// interpreter at the loop's first block when one fails (as that block's own check would).
    fn loop_checks(&mut self, x: usize) {
        let Some(&(m, at)) = self.hoist.get(&x) else { return };
        let saved = (self.flags, self.fc, self.mmx, self.xmm, self.dirty, self.block_index, self.ctx);
        // (the state as the block starts)
        self.flags = Flags::Entry;
        self.fc = self.fc_in.get(&x).copied().unwrap_or_default();
        self.mmx = (0, 0);
        self.xmm = (0, 0);
        self.dirty = self.dirty_in.get(x).copied().unwrap_or(self.wregs);
        self.block_index = x as u32;
        self.ctx = self.ctxs[x];
        for r in (0..8).filter(|r| m & (1 << r) != 0) {
            self.c.get(L_REG + r).u32c(GUEST_LO + SAFE_MARGIN as u32).sub().u32c(GUEST_HI - GUEST_LO - 2 * SAFE_MARGIN as u32).gt_u().if_();
            self.depth += 1;
            self.c.u32c(at);
            self.exit_with(RET_INTERP);
            self.depth -= 1;
            self.c.end();
        }
        (self.flags, self.fc, self.mmx, self.xmm, self.dirty, self.block_index, self.ctx) = saved;
    }

    /// Whether [reg + disp, + size) is inside the guest range by what is known of `reg`,
    /// checking `reg` itself (once) when nothing is known yet.
    fn near_safe(&mut self, reg: usize, disp: i32, size: u32) -> bool {
        if nocheck() {
            return true;
        }
        if disp.unsigned_abs() >= SAFE_MARGIN as u32 / 2 {
            return false;
        }
        let off = match self.safe[reg] {
            Some(o) => o,
            None => {
                let l = L_REG + reg as u32;
                self.c.get(l).u32c(GUEST_LO + SAFE_MARGIN as u32).sub().u32c(GUEST_HI - GUEST_LO - 2 * SAFE_MARGIN as u32).gt_u().if_();
                self.depth += 1;
                self.bail();
                self.depth -= 1;
                self.c.end();
                self.safe[reg] = Some(0);
                0
            }
        };
        let lo = off + disp;
        lo > -SAFE_MARGIN && lo + (size as i32) < SAFE_MARGIN
    }

    /// Leaves the block unless [local `t`, + local `n`) is inside the guest range.
    fn check_len(&mut self, t: u32, n: u32) {
        let o = self.temp();
        self.c.get(t).u32c(GUEST_LO).sub().set(o);
        self.c.get(o).u32c(GUEST_HI - GUEST_LO).gt_u();
        self.c.get(n).u32c(GUEST_HI - GUEST_LO).get(o).sub().gt_u().or().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
    }

    /// Leaves the block unless [addr, addr + size) is inside the guest range.
    fn check(&mut self, t: u32, size: u32) {
        if nocheck() {
            return;
        }
        self.c.get(t).u32c(GUEST_LO).sub().u32c(GUEST_HI - GUEST_LO - size).gt_u().if_();
        self.depth += 1;
        self.bail();
        self.depth -= 1;
        self.c.end();
    }

    fn load(&mut self, bits: u32) {
        self.load_at(bits, 0);
    }

    fn load_at(&mut self, bits: u32, off: u32) {
        match bits {
            8 => self.c.load8u(off),
            16 => self.c.load16u(off),
            _ => self.c.load32(off),
        };
    }

    fn store(&mut self, bits: u32) {
        self.store_at(bits, 0);
    }

    fn store_at(&mut self, bits: u32, off: u32) {
        match bits {
            8 => self.c.store8(off),
            16 => self.c.store16(off),
            _ => self.c.store32(off),
        };
    }

    /// Pushes the address `a` of the memory operand, or its base when the displacement was
    /// folded (see `folded`); returns the offset to access it with.
    fn mem_at(&mut self, a: u32) -> u32 {
        match self.folded {
            Some((t, d)) if t == a => {
                self.c.get(L_AB);
                d
            }
            _ => {
                self.c.get(a);
                0
            }
        }
    }

    // ---- operands

    /// Pushes operand `i`. `addr` is the checked address if it is a memory operand.
    fn read_op(&mut self, i: u32, addr: Option<u32>) {
        let ins = self.ins;
        match ins.op_kind(i) {
            OpKind::Register => self.get_reg(ins.op_register(i)),
            OpKind::Memory => {
                if let Some(k) = self.slot_of(i) {
                    self.c.get(L_SLOT + k);
                    return;
                }
                let off = self.mem_at(addr.unwrap());
                self.load_at(op_bits(ins, i), off);
            }
            _ => {
                let v = ins.immediate(i) as u32 & mask(op_bits(ins, 0).max(op_bits(ins, i)));
                self.c.u32c(v);
            }
        }
    }

    /// Writes operand `i` from local `v`.
    fn write_op(&mut self, i: u32, addr: Option<u32>, v: u32) {
        let ins = self.ins;
        match ins.op_kind(i) {
            OpKind::Register => self.set_reg_from(ins.op_register(i), v),
            _ => {
                let off = self.mem_at(addr.unwrap());
                self.c.get(v);
                self.store_at(op_bits(ins, i), off);
                if let Some(k) = self.slot_of(i) {
                    self.c.get(v).set(L_SLOT + k);
                } else if self.slots.is_some() {
                    self.slots_reload_if(addr.unwrap(), op_bits(ins, i) / 8);
                }
                if self.smc {
                    let off = self.mem_at(addr.unwrap());
                    let e = self.temp();
                    self.c.u32c(off).add().set(e);
                    self.smc_check(e);
                    self.temps -= 1;
                }
            }
        }
    }

    /// The kept stack slot operand `i` is, if any.
    fn slot_of(&self, i: u32) -> Option<u32> {
        let (base, list) = self.slots.as_ref()?;
        slot_operand(self.ins, i, *base).and_then(|d| list.iter().position(|&x| x == d)).map(|k| k as u32)
    }

    /// Loads the kept slots (when they are inside the guest range; otherwise every access to
    /// them leaves the block anyway).
    fn slots_load(&mut self) {
        let Some((base, list)) = self.slots.clone() else { return };
        let (lo, hi) = (*list.iter().min().unwrap(), *list.iter().max().unwrap() + 4);
        let b = L_REG + reg_index(base).unwrap().0;
        self.c.get(b).i32c(lo).add().u32c(GUEST_LO).sub().u32c(GUEST_HI - GUEST_LO - (hi - lo) as u32).le_u().if_();
        for (k, &d) in list.iter().enumerate() {
            self.c.get(b).i32c(d).add().load32(0).set(L_SLOT + k as u32);
        }
        self.c.end();
    }

    /// After a store of `size` bytes at the address in local `a`: reloads the kept slots if
    /// it touched them.
    fn slots_reload_if(&mut self, a: u32, size: u32) {
        let Some((base, list)) = self.slots.clone() else { return };
        let (lo, hi) = (*list.iter().min().unwrap(), *list.iter().max().unwrap() + 4);
        let b = L_REG + reg_index(base).unwrap().0;
        // a + size > base + lo && a < base + hi
        self.c.get(a).get(b).i32c(lo - size as i32 + 1).add().sub().u32c((hi - lo) as u32 + size - 1).lt_u().if_();
        self.depth += 1;
        self.slots_load();
        self.depth -= 1;
        self.c.end();
    }

    /// The checked address of the instruction's memory operand, if it has one.
    /// The checked address of the memory operand, if any, for an access of `size` bytes.
    fn mem_addr_sized(&mut self, size: u32) -> Option<u32> {
        let ins = self.ins;
        (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory).then(|| self.addr(size))
    }

    /// Pushes the address part of a v128 store to operand `i` (the value follows); returns
    /// the offset for xmm_dst_store.
    fn xmm_dst_prep(&mut self, i: u32, a: Option<u32>) -> u32 {
        if self.ins.op_kind(i) == OpKind::Memory {
            self.mem_at(a.unwrap())
        } else {
            0
        }
    }

    fn xmm_dst_store(&mut self, i: u32, off: u32) {
        if self.ins.op_kind(i) == OpKind::Memory {
            self.c.v128store(off);
        } else {
            self.xmm_set(self.ins.op_register(i));
        }
    }

    /// Pushes XMM register or m128 operand `i` as a v128.
    fn xmm_read(&mut self, i: u32, a: Option<u32>) {
        if self.ins.op_kind(i) == OpKind::Memory {
            let off = self.mem_at(a.unwrap());
            self.c.v128load(off);
        } else {
            let l = self.xmm_get(self.ins.op_register(i));
            self.c.get(l);
        }
    }

    fn mem_addr(&mut self) -> Option<u32> {
        let ins = self.ins;
        (0..ins.op_count()).find(|&i| ins.op_kind(i) == OpKind::Memory).map(|i| self.addr(op_bits(ins, i) / 8))
    }

    // ---- flags

    fn flags_live(&self, bits: u32) -> bool {
        self.live_after & bits != 0
    }

    /// Records a lazy flag operation (if anything reads its flags).
    fn set_lazy(&mut self, kind: u32, bits: u32, a: Option<u32>, b: Option<u32>, r: u32, c: Option<u32>) {
        if !self.flags_live(STATUS_BITS) {
            return;
        }
        let op = lazy::op(kind, bits);
        self.c.u32c(op).set(L_OP);
        if let Some(a) = a {
            self.c.get(a).set(L_FA);
        }
        if let Some(b) = b {
            self.c.get(b).set(L_FB);
        }
        self.c.get(r).set(L_FR);
        if let Some(c) = c {
            self.c.get(c).set(L_FC);
        }
        self.flags = Flags::Known(op);
    }

    /// Pushes EFLAGS (exact status flags).
    fn eflags(&mut self) {
        match self.flags {
            Flags::Entry => {
                lazy_to_cpu(&mut self.c);
                self.c.i32c(-1).set(L_OP);
                self.c.end();
                // exact in the Cpu already (after COMISS, a host call, ...): no call
                self.c.get(L_CPU).load32(Off::lazy()).eqz().if_i32();
                self.c.get(L_CPU).load32(Off::eflags()).else_().get(L_CPU).call(F_EFLAGS).end();
            }
            Flags::Known(op) => {
                self.c.u32c(op).get(L_FA).get(L_FB).get(L_FR).get(L_FC).call(F_STATUS);
            }
        }
    }

    /// Pushes condition `cc` (0/1).
    fn cond(&mut self, cc: u32) {
        let neg = cc & 1 != 0;
        if let Flags::Known(op) = self.flags {
            if self.cond_known(op, cc) {
                if neg {
                    // (eqz: TurboFan folds it into the comparison or the branch, an xor stays)
                    self.c.eqz();
                }
                return;
            }
        } else if let Some(op) = self.predict(cc) {
            // flags from before the block: most likely those a block leading here left (a
            // second jcc after a cmp), checked
            self.c.get(L_OP).u32c(op).eq().if_i32();
            self.cond_known(op, cc);
            self.c.else_();
            self.cond_entry(cc);
            self.c.end();
            if neg {
                self.c.eqz();
            }
            return;
        }
        if self.flags == Flags::Entry {
            self.cond_entry(cc);
        } else {
            self.cond_eflags(cc);
        }
        if neg {
            self.c.eqz();
        }
    }

    /// Condition `cc` (no negation) from flags not known statically (C.1 of
    /// docs/v8-wasm-shape.md): ZF, SF and PF come from the lazy result alone, whatever the
    /// operation (every kind's are those of its result at its size), so those need EFLAGS only
    /// when the flags are in the Cpu.
    fn cond_entry(&mut self, cc: u32) {
        let k = cc >> 1;
        if !matches!(k, 2 | 4 | 5) {
            return self.cond_eflags(cc);
        }
        self.c.get(L_OP).i32c(-1).eq().if_i32();
        self.cond_eflags(cc & !1);
        self.c.else_();
        if k == 5 {
            // PF: the low byte has an even number of ones
            self.c.get(L_FR).i32c(0xff).and().op(0x69).i32c(1).and().eqz();
        } else {
            // the result's bits at the operation's size at the top: << 32 - (8 << size)
            self.c.get(L_FR).i32c(32).i32c(8).get(L_OP).i32c(3).and().shl().sub().shl();
            if k == 2 {
                self.c.eqz();
            } else {
                self.c.i32c(31).shr_u();
            }
        }
        self.c.end();
    }

    /// An operation the blocks leading here (functions) left their flags from, for which
    /// condition `cc` needs no EFLAGS.
    fn predict(&mut self, cc: u32) -> Option<u32> {
        let k = self.block_index as usize;
        let ops: Vec<u32> = self.preds.get(k)?.iter().filter_map(|&p| self.flags_out[p as usize]).collect();
        ops.into_iter().find(|&op| {
            let (b, l) = (self.c.b.len(), self.c.locs.len());
            let ok = self.cond_known(op, cc);
            self.c.b.truncate(b);
            self.c.locs.truncate(l);
            ok
        })
    }

    /// Pushes condition `cc` without its negation (bit 0) from the lazy operation `op` in
    /// the locals; false (nothing pushed) if that needs EFLAGS.
    fn cond_known(&mut self, op: u32, cc: u32) -> bool {
        {
            let bits = 8 << (op & 3);
            let m = mask(bits);
            let kind = op >> 2;
            let sext = |c: &mut Code, l: u32| {
                c.get(l);
                match bits {
                    8 => {
                        c.extend8_s();
                    }
                    16 => {
                        c.extend16_s();
                    }
                    _ => {}
                }
            };
            let zero = |c: &mut Code| {
                c.get(L_FR);
                if bits < 32 {
                    c.u32c(m).and();
                }
            };
            let sign = |c: &mut Code| {
                c.get(L_FR).i32c(bits as i32 - 1).shr_u().i32c(1).and();
            };
            let done = match (kind, cc >> 1) {
                (lazy::SUB, 1) => {
                    self.c.get(L_FA);
                    and_mask(&mut self.c, m);
                    self.c.get(L_FB);
                    and_mask(&mut self.c, m);
                    self.c.lt_u();
                    true
                }
                (lazy::SUB, 3) => {
                    self.c.get(L_FA);
                    and_mask(&mut self.c, m);
                    self.c.get(L_FB);
                    and_mask(&mut self.c, m);
                    self.c.le_u();
                    true
                }
                (lazy::SUB, 6) => {
                    sext(&mut self.c, L_FA);
                    sext(&mut self.c, L_FB);
                    self.c.lt_s();
                    true
                }
                (lazy::SUB, 7) => {
                    sext(&mut self.c, L_FA);
                    sext(&mut self.c, L_FB);
                    self.c.le_s();
                    true
                }
                (lazy::ADD, 1) => {
                    zero(&mut self.c);
                    self.c.get(L_FA);
                    and_mask(&mut self.c, m);
                    self.c.lt_u();
                    true
                }
                (lazy::LOGIC, 0) | (lazy::LOGIC, 1) => {
                    self.c.i32c(0);
                    true
                }
                (lazy::LOGIC, 3) => {
                    zero(&mut self.c);
                    self.c.eqz();
                    true
                }
                // (OF is clear: SF != OF is r < 0, ZF | SF != OF is r <= 0)
                (lazy::LOGIC, 6) => {
                    sext(&mut self.c, L_FR);
                    self.c.i32c(0).lt_s();
                    true
                }
                (lazy::LOGIC, 7) => {
                    sext(&mut self.c, L_FR);
                    self.c.i32c(0).le_s();
                    true
                }
                // CF of a 32-bit shift by 1..31: the last bit shifted out
                // SBB (a 64-bit comparison's high half): the exact difference a - b - carry
                // as i64, unsigned for B/BE, signed for L/LE
                (lazy::SBB, 1 | 3 | 6 | 7) if bits == 32 => {
                    let ext = if matches!(cc >> 1, 1 | 3) { 0xad } else { 0xac }; // i64.extend_i32_u/s
                    self.c.get(L_FA).op(ext).get(L_FB).op(ext).op(0x7d).get(L_FC).op(0xad).op(0x7d).i64c(0);
                    self.c.op(if matches!(cc >> 1, 1 | 6) { 0x53 } else { 0x57 }); // i64.lt_s / le_s
                    true
                }
                (lazy::ADC, 1) if bits == 32 => {
                    self.c.get(L_FA).op(0xad).get(L_FB).op(0xad).op(0x7c).get(L_FC).op(0xad).op(0x7c).i64c(1 << 32).op(0x5a);
                    true
                }
                (lazy::SHR, 1) if bits == 32 => {
                    self.c.get(L_FA).get(L_FB).i32c(1).sub().shr_u().i32c(1).and();
                    true
                }
                (lazy::SAR, 1) if bits == 32 => {
                    self.c.get(L_FA).get(L_FB).i32c(1).sub().shr_s().i32c(1).and();
                    true
                }
                (lazy::SHL, 1) if bits == 32 => {
                    self.c.get(L_FA).i32c(32).get(L_FB).sub().shr_u().i32c(1).and();
                    true
                }
                (lazy::INC | lazy::DEC, 1) => {
                    self.c.get(L_FC).i32c(1).and();
                    true
                }
                (lazy::MUL, 1) => {
                    self.c.get(L_FC).i32c(0).ne();
                    true
                }
                (lazy::ADD | lazy::SUB | lazy::LOGIC | lazy::INC | lazy::DEC | lazy::ADC | lazy::SBB | lazy::MUL | lazy::SHL | lazy::SHR | lazy::SAR, 2) => {
                    zero(&mut self.c);
                    self.c.eqz();
                    true
                }
                (lazy::ADD | lazy::SUB | lazy::LOGIC | lazy::INC | lazy::DEC | lazy::ADC | lazy::SBB | lazy::MUL | lazy::SHL | lazy::SHR | lazy::SAR, 4) => {
                    sign(&mut self.c);
                    true
                }
                // OF: the sign of (a ^ r) & (b ^ r) (ADD), (a ^ b) & (a ^ r) (SUB); INC
                // overflows to the smallest value, DEC from it
                (lazy::ADD | lazy::SUB | lazy::INC | lazy::DEC | lazy::MUL, 0 | 6 | 7) => {
                    let sh = bits as i32 - 1;
                    match kind {
                        lazy::ADD => {
                            self.c.get(L_FA).get(L_FR).xor().get(L_FB).get(L_FR).xor().and().i32c(sh).shr_u().i32c(1).and();
                        }
                        lazy::SUB => {
                            self.c.get(L_FA).get(L_FB).xor().get(L_FA).get(L_FR).xor().and().i32c(sh).shr_u().i32c(1).and();
                        }
                        lazy::INC => {
                            self.c.get(L_FR);
                            and_mask(&mut self.c, m);
                            self.c.u32c(1 << sh).eq();
                        }
                        lazy::DEC => {
                            self.c.get(L_FA);
                            and_mask(&mut self.c, m);
                            self.c.u32c(1 << sh).eq();
                        }
                        _ => {
                            self.c.get(L_FC).i32c(0).ne();
                        }
                    }
                    // L: SF != OF, LE: that or ZF
                    if cc >> 1 != 0 {
                        sign(&mut self.c);
                        self.c.xor();
                    }
                    if cc >> 1 == 7 {
                        zero(&mut self.c);
                        self.c.eqz().or();
                    }
                    true
                }
                (lazy::ADD, 3) => {
                    zero(&mut self.c);
                    self.c.eqz();
                    zero(&mut self.c);
                    self.c.get(L_FA);
                    and_mask(&mut self.c, m);
                    self.c.lt_u().or();
                    true
                }
                // PF: an even number of ones in the low byte
                (lazy::ADD | lazy::SUB | lazy::LOGIC | lazy::INC | lazy::DEC | lazy::ADC | lazy::SBB | lazy::MUL | lazy::SHL | lazy::SHR | lazy::SAR, 5) => {
                    self.c.get(L_FR).i32c(0xff).and().op(0x69).i32c(1).and().eqz(); // i32.popcnt
                    true
                }
                _ => false,
            };
            done
        }
    }

    /// Pushes condition `cc` without its negation from EFLAGS.
    fn cond_eflags(&mut self, cc: u32) {
        if LAYOUT.load(core::sync::atomic::Ordering::Relaxed) {
            let k = match self.flags {
                Flags::Known(op) => alloc::format!("CONDFB_k{}_b{}_cc{}", op >> 2, 8 << (op & 3), cc),
                Flags::Entry => alloc::format!("CONDFB_entry_cc{cc}"),
            };
            self.sizes.entry(k).or_insert((0, 0)).0 += 1;
        }
        let t = self.temp();
        self.eflags();
        self.c.set(t);
        let bit = |c: &mut Code, n: i32| {
            c.get(t).i32c(n).shr_u().i32c(1).and();
        };
        match cc >> 1 {
            0 => bit(&mut self.c, 11),
            1 => bit(&mut self.c, 0),
            2 => bit(&mut self.c, 6),
            3 => {
                self.c.get(t).u32c(0x41).and().i32c(0).ne();
            }
            4 => bit(&mut self.c, 7),
            5 => bit(&mut self.c, 2),
            6 => {
                self.c.get(t).i32c(7).shr_u().get(t).i32c(11).shr_u().xor().i32c(1).and();
            }
            _ => {
                self.c.get(t).i32c(7).shr_u().get(t).i32c(11).shr_u().xor().get(t).i32c(6).shr_u().or().i32c(1).and();
            }
        }
    }

    // ---- stack

    /// Pushes the 32-bit value in local `v`.
    fn push32(&mut self, v: u32) {
        let t = self.temp();
        self.c.get(L_REG + 4).i32c(4).sub().set(t);
        if !self.near_safe(4, -4, 4) {
            self.check(t, 4);
        }
        self.c.get(t).get(v).store32(0);
        self.c.get(t).set(L_REG + 4);
        self.safe[4] = self.safe[4].map(|o| o - 4);
    }

    /// Pops 32 bits into a new temporary.
    fn pop32(&mut self) -> u32 {
        let t = self.temp();
        let sp = L_REG + 4;
        if !self.near_safe(4, 0, 4) {
            self.check(sp, 4);
        }
        self.c.get(sp).load32(0).set(t);
        self.c.get(sp).i32c(4).add().set(sp);
        self.safe[4] = self.safe[4].map(|o| o + 4);
        t
    }

    // ---- the interpreter, for one instruction

    /// Runs the current instruction through the `step` import. With `ends`, leaves the
    /// block afterwards at whatever EIP the instruction produced.
    fn fallback(&mut self, ends: bool) {
        self.fell_back = true;
        self.fc_flush();
        self.fc = FCache::default();
        self.mmx = (0, 0);
        self.xmm = (0, 0);
        self.spill(self.eip());
        self.c.get(L_CPU).call(F_STEP).set(L_ST);
        self.reload();
        self.c.i32c(-1).set(L_OP);
        self.flags = Flags::Entry;
        self.c.get(L_ST).if_();
        self.depth += 1;
        self.c.get(L_CPU).load32(Off::eip());
        self.c.set(L_NEXT).br(self.depth);
        self.depth -= 1;
        self.c.end();
        if ends {
            self.c.get(L_CPU).load32(Off::eip());
            self.jump_dyn();
        }
    }

    /// The lazy flags into the Cpu if they are live after this instruction (L_OP -1 after).
    fn flags_to_cpu(&mut self) {
        if !self.flags_live(STATUS_BITS) {
            return;
        }
        match self.flags {
            Flags::Known(op) => {
                self.c.u32c(op).set(L_OP);
                lazy_store(&mut self.c);
            }
            Flags::Entry => {
                lazy_to_cpu(&mut self.c);
                self.c.end();
            }
        }
        self.c.i32c(-1).set(L_OP);
        self.flags = Flags::Entry;
    }

    /// Writes registers, EIP and the lazy flags back to the Cpu.
    fn spill(&mut self, eip: u32) {
        self.store_regs();
        self.c.get(L_CPU).u32c(eip).store32(Off::eip());
        match self.flags {
            Flags::Known(op) => {
                self.c.u32c(op).set(L_OP);
                lazy_store(&mut self.c);
            }
            Flags::Entry => {
                lazy_to_cpu(&mut self.c);
                self.c.end();
            }
        }
    }

    fn reload(&mut self) {
        self.safe = [None; 8];
        self.safe2.clear();
        self.load_regs();
    }

    /// The registers that can differ from the Cpu's into it.
    fn store_regs(&mut self) {
        for i in (0..8).filter(|i| self.wregs & (1 << i) != 0) {
            self.c.get(L_CPU).get(L_REG + i).store32(Off::reg(i as usize));
        }
    }

    /// The registers written since they were loaded into the Cpu (a call's or RET's).
    fn store_dirty(&mut self) {
        for i in (0..8).filter(|i| self.wregs & self.dirty & (1 << i) != 0) {
            self.c.get(L_CPU).get(L_REG + i).store32(Off::reg(i as usize));
        }
        self.dirty = 0;
    }

    /// [`Self::load_regs`] but ESP from local `esp` (a function's call returned it).
    fn load_regs_esp(&mut self, esp: u32) {
        self.dirty = 0;
        for i in (0..8).filter(|i| self.regs & (1 << i) != 0) {
            if i == 4 {
                self.c.get(esp).set(L_REG + 4);
            } else {
                self.c.get(L_CPU).load32(Off::reg(i as usize)).set(L_REG + i);
            }
        }
    }

    /// The registers the code names from the Cpu.
    fn load_regs(&mut self) {
        self.dirty = 0;
        for i in (0..8).filter(|i| self.regs & (1 << i) != 0) {
            self.c.get(L_CPU).load32(Off::reg(i as usize)).set(L_REG + i);
        }
    }

    // ---- instructions

    /// Emits one instruction; returns false if it ended the block.
    fn emit(&mut self, last: bool) -> bool {
        use Mnemonic::*;
        let ins = self.ins;
        self.temps = 0;
        self.folded = None;
        if ins.flow_control() == FlowControl::Next && PROBES.iter().any(|p| p.load(core::sync::atomic::Ordering::Relaxed) == ins.ip32()) {
            self.fallback(false);
            return true;
        }
        // locked read-modify-writes (xchg with memory is one) go to the interpreter, which
        // makes them atomic
        let locked = (ins.has_lock_prefix() || ins.mnemonic() == Xchg) && (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory);
        let simple = !locked && (0..ins.op_count()).all(|i| simple_operand(ins, i));
        let next = ins.next_ip32();
        // registers this instruction writes lose what was known about them (push/pop keep
        // ESP's track themselves)
        let mut written = 0u32;
        for u in iced_x86::InstructionInfoFactory::new().info(ins).used_registers() {
            use iced_x86::OpAccess::*;
            if matches!(u.access(), Write | CondWrite | ReadWrite | ReadCondWrite) && u.register().is_gpr() {
                written |= 1 << u.register().full_register32().number();
            }
        }
        self.dirty |= written as u8;
        if matches!(ins.mnemonic(), Push | Pop) {
            written &= !(1 << 4);
            if ins.mnemonic() == Pop && ins.op_kind(0) == OpKind::Register && ins.op_register(0).full_register32() == Register::ESP {
                written |= 1 << 4;
            }
        }
        if fc_handles(ins) {
            self.fc_begin();
        } else if fpu_neutral(ins) {
        } else if touches_fpu(ins) && !packed::supported(ins) {
            // (the packed ops use the MMX cache and leave the x87 state alone)
            self.fc_flush();
            self.fc = FCache::default();
        }
        if self.xmm.0 != 0 && !xmm_cached(ins) && (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_xmm()) {
            self.xmm_flush();
            self.xmm = (0, 0);
        }
        let r = self.emit_ins(last, next, simple);
        for i in 0..8 {
            if written & (1 << i) != 0 {
                self.safe[i] = None;
            }
        }
        self.safe2.retain(|&(b, x, _)| written & (1 << b | 1 << x) == 0);
        // ESP is taken to point into a stack at every block start (as on real hardware,
        // running off the stack needs a broken program), so an instruction that sets it to
        // something new has it checked here, before the next block relies on that
        if written & (1 << 4) != 0 && r {
            let imm = ins.op_count() == 2 && matches!(ins.op_kind(1), OpKind::Immediate8to32 | OpKind::Immediate32);
            let drift = match ins.mnemonic() {
                Call | Ret | Pushad | Popad | Pushfd | Popfd | Enter => true,
                Add | Sub | And => imm,
                _ => false,
            };
            if drift {
                self.safe[4] = Some(0);
            } else {
                self.check_esp(next);
            }
        }
        r
    }

    /// Leaves the block for the interpreter at `eip` (the state is that before `eip`) unless
    /// ESP is in the guest range with SAFE_MARGIN to spare.
    fn check_esp(&mut self, eip: u32) {
        self.c.get(L_REG + 4).u32c(GUEST_LO + SAFE_MARGIN as u32).sub().u32c(GUEST_HI - GUEST_LO - 2 * SAFE_MARGIN as u32).gt_u().if_();
        self.depth += 1;
        self.c.u32c(eip);
        self.exit_with(RET_INTERP);
        self.depth -= 1;
        self.c.end();
        self.safe[4] = Some(0);
    }

    fn emit_ins(&mut self, last: bool, next: u32, simple: bool) -> bool {
        use Mnemonic::*;
        let ins = self.ins;
        match ins.mnemonic() {
            Nop | Pause | Wait | Endbr32 | Prefetchnta | Prefetcht0 | Prefetcht1 | Prefetcht2 | Prefetchw | Lfence | Sfence => {}
            // (x86 orders other accesses itself, TSO; wasm on ARM64 doesn't: MFENCE, which code
            // uses for store-load ordering, becomes a full fence there)
            Mfence => {
                self.c.op(0xfe).op(0x03).op(0x00); // atomic.fence
            }

            // ---- string ops (DS:ESI without a segment override, ES:EDI)
            Movsb | Movsw | Movsd | Stosb | Stosw | Stosd | Lodsb | Lodsw | Lodsd
                if is_string(ins)
                    && !(self.smc && !matches!(ins.mnemonic(), Lodsb | Lodsw | Lodsd))
                    && ins.segment_prefix() == Register::None
                    && !ins.has_repne_prefix()
                    && !(ins.has_rep_prefix() && matches!(ins.mnemonic(), Lodsb | Lodsw | Lodsd)) =>
            {
                let mn = ins.mnemonic();
                let size: u32 = match mn {
                    Movsb | Stosb | Lodsb => 1,
                    Movsw | Stosw | Lodsw => 2,
                    _ => 4,
                };
                let (esi, edi, ecx) = (L_REG + 6, L_REG + 7, L_REG + 1);
                let movs = matches!(mn, Movsb | Movsw | Movsd);
                let stos = matches!(mn, Stosb | Stosw | Stosd);
                if ins.has_rep_prefix() {
                    // the whole run at once; with DF set it ends at EDI (ESI), so the range starts
                    // n - size below
                    let (n, df) = (self.temp(), self.temp());
                    self.c.get(ecx).i32c(size.trailing_zeros() as i32).shl().set(n);
                    self.c.get(L_CPU).load32(Off::eflags()).i32c(0x400).and().set(df);
                    self.c.get(ecx).u32c(1 << 28).gt_u();
                    if movs {
                        // a memmove only if the destination is not inside the source ahead of the
                        // copy (above it forward, below it backward); bytes forward are fine
                        // anyway (below)
                        self.c.get(esi).get(edi).sub().get(edi).get(esi).sub().get(df).select();
                        self.c.get(n).lt_u().get(edi).get(esi).ne().and();
                        if size == 1 {
                            self.c.get(df).and();
                        }
                        self.c.or();
                    }
                    self.c.if_();
                    self.depth += 1;
                    self.bail();
                    self.depth -= 1;
                    self.c.end();
                    // back: n - size with DF, else 0 (in df's local)
                    let (back, d) = (df, self.temp());
                    self.c.get(n).i32c(size as i32).sub().i32c(0).get(df).select().set(back);
                    self.c.get(edi).get(back).sub().set(d);
                    if movs {
                        let sv = self.temp();
                        self.c.get(esi).get(back).sub().set(sv);
                        self.check_len(d, n);
                        self.check_len(sv, n);
                        if size == 1 {
                            // forward onto itself (an LZ copy repeating the last `dist` bytes):
                            // `dist` bytes at a time, each from bytes already written
                            let (dist, k) = (self.temp(), self.temp());
                            self.c.get(d).get(sv).sub().set(dist);
                            self.c.get(df).eqz().get(dist).i32c(1).sub().get(n).i32c(1).sub().lt_u().and().if_();
                            self.c.block().loop_();
                            self.c.get(n).eqz().br_if(1);
                            self.c.get(n).get(dist).get(n).get(dist).lt_u().select().set(k);
                            self.c.get(d).get(sv).get(k).memory_copy();
                            self.c.get(d).get(k).add().set(d);
                            self.c.get(sv).get(k).add().set(sv);
                            self.c.get(n).get(k).sub().set(n);
                            self.c.br(0).end().end();
                            self.c.else_();
                            self.c.get(d).get(sv).get(n).memory_copy();
                            self.c.end();
                            self.c.get(ecx).set(n);
                        } else {
                            self.c.get(d).get(sv).get(n).memory_copy();
                        }
                    } else if size == 1 {
                        self.check_len(d, n);
                        self.c.get(d).get(L_REG).get(n).memory_fill();
                    } else {
                        // n is a multiple of the size: store until the end
                        self.check_len(d, n);
                        let (i, e) = (self.temp(), self.temp());
                        self.c.get(d).tee(i).get(n).add().set(e);
                        self.c.get(n).if_().loop_();
                        self.c.get(i).get(L_REG);
                        self.store(size * 8);
                        self.c.get(i).i32c(size as i32).add().tee(i).get(e).ne().br_if(0);
                        self.c.end().end();
                    }
                    // the step: -n with DF, else n
                    self.c.i32c(0).get(n).sub().get(n);
                    self.c.get(L_CPU).load32(Off::eflags()).i32c(0x400).and().select().set(n);
                    self.c.get(edi).get(n).add().set(edi);
                    if movs {
                        self.c.get(esi).get(n).add().set(esi);
                    }
                    self.c.i32c(0).set(ecx);
                } else {
                    // step: +size, or -size with DF
                    let st = self.temp();
                    self.c.i32c(size as i32).get(L_CPU).load32(Off::eflags()).i32c(9).shr_u().i32c(2).and();
                    self.c.i32c(size as i32).mul().sub().set(st);
                    let v = self.temp();
                    if movs || !stos {
                        self.c.get(esi).set(v);
                        self.check(v, size);
                        self.c.get(v);
                        self.load(size * 8);
                        self.c.set(v);
                        self.c.get(esi).get(st).add().set(esi);
                    } else {
                        self.c.get(L_REG).set(v);
                    }
                    if movs || stos {
                        let a = self.temp();
                        self.c.get(edi).set(a);
                        self.check(a, size);
                        self.c.get(a).get(v);
                        self.store(size * 8);
                        self.c.get(edi).get(st).add().set(edi);
                    } else {
                        self.set_reg_from(ins.op_register(0), v);
                    }
                }
            }
            // locked read-modify-writes of an aligned dword: wasm atomics (unaligned: the interpreter)
            Add | Sub | And | Or | Xor | Inc | Dec | Xadd | Xchg | Cmpxchg if locked32(ins) => {
                let mn = ins.mnemonic();
                let m = if ins.op_kind(0) == OpKind::Memory { 0 } else { 1 };
                let a = self.addr(4);
                self.c.get(a).i32c(3).and().if_();
                self.depth += 1;
                self.bail();
                self.depth -= 1;
                self.c.end();
                let (b, old, r) = (self.temp(), self.temp(), self.temp());
                match mn {
                    Inc | Dec => {
                        self.c.i32c(1);
                    }
                    _ => self.read_op(1 - m, None),
                }
                self.c.set(b);
                if mn == Cmpxchg {
                    self.c.get(a).get(L_REG).get(b).atomic32(0x48).set(old);
                    // ZF from EAX - old; EAX gets the old value (the same when they matched)
                    self.c.get(L_REG).get(old).sub().set(r);
                    self.c.get(L_REG).set(b);
                    self.c.get(old).set(L_REG);
                    self.set_lazy(lazy::SUB, 32, Some(b), Some(old), r, None);
                    return self.after(last, next);
                }
                self.c.get(a).get(b).atomic32(match mn {
                    Add | Inc | Xadd => 0x1e,
                    Sub | Dec => 0x25,
                    And => 0x2c,
                    Or => 0x33,
                    Xor => 0x3a,
                    _ => 0x41,
                });
                self.c.set(old);
                if mn == Xchg || mn == Xadd {
                    self.set_reg_from(ins.op_register(1 - m), old);
                }
                if mn == Xchg {
                    return self.after(last, next);
                }
                let carry = if matches!(mn, Inc | Dec) && self.flags_live(RflagsBits::CF) {
                    let c = self.temp();
                    self.cond(2);
                    self.c.set(c);
                    Some(c)
                } else {
                    None
                };
                self.c.get(old).get(b);
                let kind = match mn {
                    Add | Xadd => { self.c.add(); lazy::ADD }
                    Inc => { self.c.add(); lazy::INC }
                    Sub => { self.c.sub(); lazy::SUB }
                    Dec => { self.c.sub(); lazy::DEC }
                    And => { self.c.and(); lazy::LOGIC }
                    Or => { self.c.or(); lazy::LOGIC }
                    _ => { self.c.xor(); lazy::LOGIC }
                };
                self.c.set(r);
                let fb = if matches!(mn, Inc | Dec) { None } else { Some(b) };
                self.set_lazy(kind, 32, Some(old), fb, r, carry);
            }
            Mul | Imul if simple && ins.op_count() == 1 && op_bits(ins, 0) == 32 => {
                let addr = self.mem_addr();
                let (r, o) = (self.temp(), self.temp());
                let signed = ins.mnemonic() == Imul;
                self.c.get(L_REG);
                if signed { self.c.i64_extend_s(); } else { self.c.i64_extend_u(); }
                self.read_op(0, addr);
                if signed { self.c.i64_extend_s(); } else { self.c.i64_extend_u(); }
                self.c.i64_mul().set(L_W);
                self.c.get(L_W).wrap().set(r);
                self.c.get(L_W).i64c(32).i64_shr_u().wrap().set(L_REG + 2);
                self.c.get(r).set(L_REG);
                if self.flags_live(STATUS_BITS) {
                    if signed {
                        self.c.get(L_W).get(r).i64_extend_s().op(0x52 /* i64.ne */).set(o);
                    } else {
                        self.c.get(L_REG + 2).i32c(0).ne().set(o);
                    }
                    self.set_lazy(lazy::MUL, 32, None, None, r, Some(o));
                }
            }

            Mov | Movzx if simple => {
                let a = self.mem_addr();
                let v = self.temp();
                self.read_op(1, a);
                self.c.set(v);
                self.write_op(0, a, v);
            }
            Movsx if simple => {
                let a = self.mem_addr();
                let v = self.temp();
                self.read_op(1, a);
                if op_bits(ins, 1) == 8 {
                    self.c.extend8_s();
                } else {
                    self.c.extend16_s();
                }
                self.c.set(v);
                self.write_op(0, a, v);
            }
            Lea if op_bits(ins, 0) == 32
                && [ins.memory_base(), ins.memory_index()].iter().all(|&r| r == Register::None || r.is_gpr32()) =>
            {
                let v = self.temp();
                self.c.u32c(ins.memory_displacement32());
                if ins.memory_base() != Register::None {
                    self.c.get(L_REG + ins.memory_base().number() as u32).add();
                }
                if ins.memory_index() != Register::None {
                    self.c.get(L_REG + ins.memory_index().number() as u32);
                    if ins.memory_index_scale() > 1 {
                        self.c.i32c(ins.memory_index_scale().trailing_zeros() as i32).shl();
                    }
                    self.c.add();
                }
                self.c.set(v);
                self.write_op(0, None, v);
            }
            Xchg if simple => {
                let a = self.mem_addr();
                let (x, y) = (self.temp(), self.temp());
                self.read_op(0, a);
                self.c.set(x);
                self.read_op(1, a);
                self.c.set(y);
                self.write_op(0, a, y);
                self.write_op(1, a, x);
            }
            Bswap if op_bits(ins, 0) == 32 => {
                let r = L_REG + ins.op_register(0).number() as u32;
                let v = self.temp();
                self.c.get(r).set(v);
                // ((v >> 24) | (v << 24)) | ((v >> 8) & 0xff00) | ((v << 8) & 0xff0000)
                self.c.get(v).i32c(24).shr_u().get(v).i32c(24).shl().or();
                self.c.get(v).i32c(8).shr_u().u32c(0xff00).and().or();
                self.c.get(v).i32c(8).shl().u32c(0xff_0000).and().or().set(r);
            }

            Add | Sub | Cmp | And | Or | Xor | Test | Adc | Sbb if simple => {
                let bits = op_bits(ins, 0);
                let m = mask(bits);
                let addr = self.mem_addr();
                let (a, b, r) = (self.temp(), self.temp(), self.temp());
                let carry = if matches!(ins.mnemonic(), Adc | Sbb) {
                    let c = self.temp();
                    self.cond(2);
                    self.c.set(c);
                    Some(c)
                } else {
                    None
                };
                self.read_op(0, addr);
                self.c.set(a);
                self.read_op(1, addr);
                and_mask(&mut self.c, m);
                self.c.set(b);
                self.c.get(a).get(b);
                let kind = match ins.mnemonic() {
                    Add => {
                        self.c.add();
                        lazy::ADD
                    }
                    Adc => {
                        self.c.add().get(carry.unwrap()).add();
                        lazy::ADC
                    }
                    Sub | Cmp => {
                        self.c.sub();
                        lazy::SUB
                    }
                    Sbb => {
                        self.c.sub().get(carry.unwrap()).sub();
                        lazy::SBB
                    }
                    And | Test => {
                        self.c.and();
                        lazy::LOGIC
                    }
                    Or => {
                        self.c.or();
                        lazy::LOGIC
                    }
                    _ => {
                        self.c.xor();
                        lazy::LOGIC
                    }
                };
                if bits < 32 {
                    self.c.u32c(m).and();
                }
                self.c.set(r);
                if !matches!(ins.mnemonic(), Cmp | Test) {
                    self.write_op(0, addr, r);
                }
                self.set_lazy(kind, bits, Some(a), Some(b), r, carry);
            }
            Inc | Dec if simple => {
                let bits = op_bits(ins, 0);
                let addr = self.mem_addr();
                let (a, r) = (self.temp(), self.temp());
                let carry = if self.flags_live(RflagsBits::CF) {
                    let c = self.temp();
                    self.cond(2);
                    self.c.set(c);
                    Some(c)
                } else {
                    None
                };
                self.read_op(0, addr);
                self.c.tee(a).i32c(1);
                if ins.mnemonic() == Inc {
                    self.c.add();
                } else {
                    self.c.sub();
                }
                if bits < 32 {
                    self.c.u32c(mask(bits)).and();
                }
                self.c.set(r);
                self.write_op(0, addr, r);
                let kind = if ins.mnemonic() == Inc { lazy::INC } else { lazy::DEC };
                self.set_lazy(kind, bits, Some(a), None, r, carry);
            }
            Neg | Not if simple => {
                let bits = op_bits(ins, 0);
                let addr = self.mem_addr();
                let (a, r, z) = (self.temp(), self.temp(), self.temp());
                self.read_op(0, addr);
                self.c.set(a);
                if ins.mnemonic() == Neg {
                    self.c.i32c(0).get(a).sub();
                } else {
                    self.c.get(a).i32c(-1).xor();
                }
                if bits < 32 {
                    self.c.u32c(mask(bits)).and();
                }
                self.c.set(r);
                self.write_op(0, addr, r);
                if ins.mnemonic() == Neg {
                    self.c.i32c(0).set(z);
                    self.set_lazy(lazy::SUB, bits, Some(z), Some(a), r, None);
                }
            }
            // (their flags go through the interpreter when anything reads them)
            Tzcnt | Lzcnt | Popcnt | Bsf | Bsr if simple && op_bits(ins, 0) == 32 && !self.flags_live(STATUS_BITS) => {
                let addr = self.mem_addr();
                let (v, r) = (self.temp(), self.temp());
                self.read_op(1, addr);
                self.c.set(v);
                match ins.mnemonic() {
                    Tzcnt | Bsf => {
                        self.c.get(v).op(0x68);
                    }
                    Lzcnt => {
                        self.c.get(v).op(0x67);
                    }
                    Bsr => {
                        self.c.i32c(31).get(v).op(0x67).sub();
                    }
                    _ => {
                        self.c.get(v).op(0x69);
                    }
                }
                if matches!(ins.mnemonic(), Bsf | Bsr) {
                    // (a zero source leaves the destination as it was)
                    self.read_op(0, addr);
                    self.c.get(v).select();
                }
                self.c.set(r);
                self.write_op(0, addr, r);
            }
            // BT: CF is bit n of the value, as SHR by n + 1 has it (the other flags are undefined
            // or unchanged: through the interpreter when anything reads them)
            Bt if simple && (ins.op_kind(0) == OpKind::Register || ins.op_kind(1) != OpKind::Register) && !self.flags_live(STATUS_BITS & !RflagsBits::CF) => {
                let bits = op_bits(ins, 0);
                let addr = self.mem_addr();
                let (a, n, r) = (self.temp(), self.temp(), self.temp());
                self.read_op(0, addr);
                self.c.set(a);
                if ins.op_kind(1) == OpKind::Register {
                    self.read_op(1, addr);
                } else {
                    self.c.u32c(ins.immediate(1) as u32);
                }
                self.c.i32c(bits as i32 - 1).and().i32c(1).add().set(n);
                self.c.get(a).get(n).shr_u().set(r);
                self.set_lazy(lazy::SHR, bits, Some(a), Some(n), r, None);
            }
            Shl | Sal | Shr | Sar | Rol | Ror if simple && ins.op_count() == 2 => {
                let bits = op_bits(ins, 0);
                let imm = ins.op_kind(1) != OpKind::Register;
                let count = if imm { ins.immediate(1) as u32 & 31 } else { 0 };
                let rot = matches!(ins.mnemonic(), Rol | Ror);
                // flags that depend on a count only known at run time (or rotates, which keep
                // the other flags) go through the interpreter when anyone reads them
                if (imm && count == 0) && !rot {
                    // no-op, flags unchanged
                } else if rot && self.flags_live(STATUS_BITS) {
                    self.fallback(false);
                } else {
                    let addr = self.mem_addr();
                    let (a, n, r) = (self.temp(), self.temp(), self.temp());
                    self.read_op(0, addr);
                    self.c.set(a);
                    if imm {
                        self.c.u32c(count).set(n);
                    } else {
                        self.c.get(L_REG + 1).i32c(31).and().set(n);
                    }
                    let m = mask(bits);
                    match ins.mnemonic() {
                        Shl | Sal => {
                            self.c.get(a).get(n).shl();
                            if bits < 32 {
                                self.c.u32c(m).and();
                            }
                        }
                        Shr => {
                            self.c.get(a).get(n).shr_u();
                        }
                        Sar => {
                            self.c.get(a);
                            match bits {
                                8 => {
                                    self.c.extend8_s();
                                }
                                16 => {
                                    self.c.extend16_s();
                                }
                                _ => {}
                            }
                            self.c.get(n).shr_s();
                            if bits < 32 {
                                self.c.u32c(m).and();
                            }
                        }
                        // 8/16 bits: the operand repeated over 32 bits rotates the same (the result
                        // in the top bits for ROL, the bottom for ROR); the count mod the width
                        Rol | Ror if bits < 32 => {
                            self.c.get(a).u32c(m).and().u32c(if bits == 8 { 0x0101_0101 } else { 0x0001_0001 }).mul();
                            self.c.get(n).u32c(bits - 1).and();
                            if ins.mnemonic() == Rol {
                                self.c.rotl().u32c(32 - bits).shr_u();
                            } else {
                                self.c.rotr().u32c(m).and();
                            }
                        }
                        Rol => {
                            self.c.get(a).get(n).rotl();
                        }
                        _ => {
                            self.c.get(a).get(n).rotr();
                        }
                    }
                    self.c.set(r);
                    if !imm {
                        // a zero count leaves the operand alone (and a 8/16-bit SHL/SHR by more
                        // than the width is handled by the masks above)
                    }
                    self.write_op(0, addr, r);
                    if !rot {
                        let kind = match ins.mnemonic() {
                            Shl | Sal => lazy::SHL,
                            Shr => lazy::SHR,
                            _ => lazy::SAR,
                        };
                        if imm {
                            self.set_lazy(kind, bits, Some(a), Some(n), r, None);
                        } else if self.flags_live(STATUS_BITS) {
                            // a zero count in CL leaves the flags as they were
                            self.c.get(n).if_();
                            self.set_lazy(kind, bits, Some(a), Some(n), r, None);
                            self.c.end();
                            self.flags = Flags::Entry;
                        }
                    }
                }
            }
            // double shifts (64-bit shifts in compiled code): through an i64, flags when
            // nobody reads them
            Shld | Shrd if simple && op_bits(ins, 0) == 32 && !self.flags_live(STATUS_BITS) => {
                let addr = self.mem_addr();
                let (d, r) = (self.temp(), self.temp());
                self.read_op(0, addr);
                self.c.set(d);
                let shrd = ins.mnemonic() == Shrd;
                // (the high half, then the low one)
                if shrd {
                    self.read_op(1, addr);
                    self.c.i64_extend_u().i64c(32).op(0x86 /* i64.shl */);
                    self.c.get(d).i64_extend_u().op(0x84 /* i64.or */);
                } else {
                    self.c.get(d).i64_extend_u().i64c(32).op(0x86);
                    self.read_op(1, addr);
                    self.c.i64_extend_u().op(0x84);
                }
                if ins.op_kind(2) == OpKind::Register {
                    self.c.get(L_REG + 1).i32c(31).and().i64_extend_u();
                } else {
                    self.c.i64c((ins.immediate(2) & 31) as i64);
                }
                if shrd {
                    self.c.op(0x88 /* i64.shr_u */).wrap();
                } else {
                    self.c.op(0x86).i64c(32).op(0x88).wrap();
                }
                self.c.set(r);
                self.write_op(0, addr, r);
            }
            // the 64-bit halves of an XMM register (it lives in the Cpu)
            Movlps | Movhps | Movlpd | Movhpd if mem_ok(ins) && (ins.op_kind(0) == OpKind::Memory || ins.op_kind(1) == OpKind::Memory) => {
                let high = matches!(ins.mnemonic(), Movhps | Movhpd);
                let a = self.addr(8);
                if ins.op_kind(0) == OpKind::Memory {
                    let x = Off::xmm(ins.op_register(1)) + if high { 8 } else { 0 };
                    let o = self.mem_at(a);
                    self.c.get(L_CPU).load64(x).store64(o);
                } else {
                    let x = Off::xmm(ins.op_register(0)) + if high { 8 } else { 0 };
                    self.c.get(L_CPU);
                    let o = self.mem_at(a);
                    self.c.load64(o).store64(x);
                }
            }
            Imul if simple && ins.op_count() >= 2 && op_bits(ins, 0) == 32 => {
                let addr = self.mem_addr();
                let (r, o) = (self.temp(), self.temp());
                let (x, y) = if ins.op_count() == 3 { (1, 2) } else { (0, 1) };
                if !self.flags_live(STATUS_BITS) {
                    self.read_op(x, addr);
                    self.read_op(y, addr);
                    self.c.mul().set(r);
                    self.write_op(0, addr, r);
                    return self.after(last, next);
                }
                self.read_op(x, addr);
                self.c.i64_extend_s();
                self.read_op(y, addr);
                self.c.i64_extend_s().i64_mul().set(L_W);
                self.c.get(L_W).wrap().set(r);
                self.write_op(0, addr, r);
                {
                    // overflow: the 64-bit product differs from the sign extension of its low half
                    self.c.get(L_W).get(r).i64_extend_s().op(0x52 /* i64.ne */).set(o);
                    self.set_lazy(lazy::MUL, 32, None, None, r, Some(o));
                }
            }
            Cdq => {
                self.c.get(L_REG).i32c(31).shr_s().set(L_REG + 2);
            }
            Cwde => {
                self.c.get(L_REG).extend16_s().set(L_REG);
            }
            Setb | Setae | Sete | Setne | Setbe | Seta | Sets | Setns | Setp | Setnp | Setl | Setge | Setle | Setg | Seto | Setno if simple => {
                let addr = self.mem_addr();
                let v = self.temp();
                self.cond(cc_index(ins.condition_code()));
                if self.fuse_zx {
                    // and the MOVZX of it that follows (see emit_block)
                    self.c.set(L_REG + ins.op_register(0).number() as u32);
                    return true;
                }
                self.c.set(v);
                self.write_op(0, addr, v);
            }
            Cmovb | Cmovae | Cmove | Cmovne | Cmovbe | Cmova | Cmovs | Cmovns | Cmovp | Cmovnp | Cmovl | Cmovge | Cmovle | Cmovg | Cmovo | Cmovno
                if simple && op_bits(ins, 0) == 32 =>
            {
                let addr = self.mem_addr();
                let v = self.temp();
                let dst = L_REG + ins.op_register(0).number() as u32;
                self.read_op(1, addr);
                self.c.get(dst);
                self.cond(cc_index(ins.condition_code()));
                self.c.select().set(v);
                self.c.get(v).set(dst);
            }
            Push if simple && (op_bits(ins, 0) == 32 || ins.op_kind(0) == OpKind::Immediate8to32 || ins.op_kind(0) == OpKind::Immediate32) => {
                let addr = self.mem_addr();
                let v = self.temp();
                self.read_op(0, addr);
                self.c.set(v);
                self.push32(v);
            }
            Pop if simple && op_bits(ins, 0) == 32 && ins.op_kind(0) == OpKind::Register => {
                let v = self.pop32();
                self.set_reg_from(ins.op_register(0), v);
            }
            Leave => {
                let sp = L_REG + 4;
                self.c.get(L_REG + 5).set(sp);
                self.safe[4] = self.safe[5];
                let v = self.pop32();
                self.c.get(v).set(L_REG + 5);
            }


            // ---- x87 (through the block's register cache, FCache)
            Fld | Fldz | Fld1 | Fild if fc_handles(ins) => {
                // the value goes on the wasm stack before the push
                let mut copy = None;
                if ins.mnemonic() == Fild {
                    // (the exact integer goes in L_W)
                    let size = ins.memory_size().size() as u32;
                    let a = self.addr(size);
                    let o = self.mem_at(a);
                    match size {
                        2 => self.c.load16s(o).i64_extend_s(),
                        4 => self.c.load32(o).i64_extend_s(),
                        _ => self.c.load64(o),
                    };
                    self.c.set(L_W);
                    self.c.get(L_W).op(0xb9 /* f64.convert_i64_s */);
                } else if ins.mnemonic() == Fld && ins.op_kind(0) == OpKind::Memory {
                    let a = self.addr(ins.memory_size().size() as u32);
                    self.fload_mem(a);
                } else if ins.mnemonic() == Fld {
                    let i = st_index(ins.op_register(0));
                    let l = self.fc_get(i);
                    copy = Some(self.fc_slot(i));
                    self.c.get(l);
                } else {
                    self.c.f64c(if ins.mnemonic() == Fldz { 0.0 } else { 1.0 });
                }
                self.fc.delta -= 1;
                let r = self.fc_slot(0);
                self.c.set(L_FREG + r);
                self.fc_wrote(r);
                match (ins.mnemonic(), copy) {
                    (Fild, _) => {
                        // exact: the integer is kept beside the f64
                        self.fc_runtime_tags(r);
                        self.fc_phys_addr(r);
                        self.c.get(L_W).store64(Off::fints());
                        let t = self.fc_phys(r);
                        self.fbit(Off::fexact(), t, true);
                        self.fbit(Off::fempty(), t, false);
                    }
                    (_, Some(src)) if self.fc.valid & (1 << src) == 0 => {
                        // a register this block did not write: its exact integer comes along
                        self.fc_runtime_tags(r);
                        let (pd, ps) = (self.fc_phys(r), self.fc_phys(src));
                        self.freg_at(pd, 3);
                        self.freg_at(ps, 3);
                        self.c.load64(Off::fints()).store64(Off::fints());
                        self.fbit(Off::fempty(), pd, false);
                        self.fbit(Off::fexact(), pd, false);
                        self.c.get(L_CPU).get(L_CPU).load8u(Off::fexact());
                        self.c.get(L_CPU).load8u(Off::fexact()).get(ps).shr_u().i32c(1).and().get(pd).shl().or();
                        self.c.store8(Off::fexact());
                    }
                    _ => {}
                }
            }
            Fst | Fstp if fc_handles(ins) => {
                if ins.op_kind(0) == OpKind::Memory {
                    let a = self.addr(ins.memory_size().size() as u32);
                    let v = self.fc_get(0);
                    let o = self.mem_at(a);
                    self.c.get(v);
                    if ins.memory_size() == iced_x86::MemorySize::Float32 {
                        self.c.op(0xb6 /* f32.demote_f64 */).f32store(o);
                    } else {
                        self.c.f64store(o);
                    }
                } else {
                    let i = st_index(ins.op_register(0));
                    let v = self.fc_get(0);
                    let r = self.fc_slot(i);
                    self.c.get(v).set(L_FREG + r);
                    self.fc_wrote(r);
                }
                if ins.mnemonic() == Fstp {
                    self.fc_pop();
                }
            }
            Fist | Fistp if fc_handles(ins) => {
                let size = ins.memory_size().size() as u32;
                let a = self.addr(size);
                let v = self.fc_get(0);
                let r0 = self.fc_slot(0);
                // rounded by the control word's RC: 0 nearest even, 1 down, 2 up, 3 toward zero
                let rc = self.temp();
                self.c.get(L_CPU).load16u(Off::fcw()).i32c(10).shr_u().set(rc);
                self.c.get(v).op(0x9d).get(v).op(0x9b).get(rc).i32c(1).and().select();
                self.c.get(v).op(0x9c).get(v).op(0x9e).get(rc).i32c(1).and().select();
                self.c.get(rc).i32c(2).and().select().set(L_F0);
                // out of range (or NaN): the integer indefinite
                let o = self.mem_at(a);
                let (lo, hi) = match size {
                    2 => (-32768.0, 32767.0),
                    4 => (-2147483648.0, 2147483647.0),
                    _ => (-9.223372036854775808e18, 9.223372036854775808e18),
                };
                if size == 8 {
                    self.c.get(L_F0).op(0xfc).op(0x06 /* i64.trunc_sat_f64_s */).i64c(i64::MIN);
                    self.c.get(L_F0).f64c(lo).op(0x66).get(L_F0).f64c(hi).op(0x63 /* f64.lt */).and().select();
                    if (self.fc.valid | self.fc.empty) & (1 << r0) == 0 {
                        // FILD m64 -> FISTP m64 stays exact
                        let p = self.fc_phys(r0);
                        self.c.set(L_W);
                        self.freg_at(p, 3);
                        self.c.load64(Off::fints()).get(L_W);
                        self.c.get(L_CPU).load8u(Off::fexact()).get(p).shr_u().i32c(1).and().select();
                    }
                    self.c.store64(o);
                } else {
                    self.c.get(L_F0).op(0xfc).op(0x02 /* i32.trunc_sat_f64_s */).i32c(if size == 2 { -32768 } else { i32::MIN });
                    self.c.get(L_F0).f64c(lo).op(0x66).get(L_F0).f64c(hi).op(0x65).and().select();
                    if size == 2 {
                        self.c.store16(o);
                    } else {
                        self.c.store32(o);
                    }
                }
                if ins.mnemonic() == Fistp {
                    self.fc_pop();
                }
            }
            Fldcw if fpu_neutral(ins) => {
                let a = self.addr(2);
                self.c.get(L_CPU).get(a).load16u(0).store16(Off::fcw());
            }
            Fnstcw | Fstcw if fpu_neutral(ins) => {
                let a = self.addr(2);
                self.c.get(a).get(L_CPU).load16u(Off::fcw()).store16(0);
            }
            Emms => {
                self.c.get(L_CPU).i32c(0xff).store8(Off::fempty());
            }
            Cld | Std => {
                self.c.get(L_CPU).get(L_CPU).load32(Off::eflags());
                if ins.mnemonic() == Cld {
                    self.c.i32c(!0x400).and();
                } else {
                    self.c.i32c(0x400).or();
                }
                self.c.store32(Off::eflags());
            }
            Fxch if fc_handles(ins) => {
                let i = st_index(ins.op_register(1)).max(st_index(ins.op_register(0)));
                let (a, b) = (self.fc_get(0), self.fc_get(i));
                let (ra, rb) = (self.fc_slot(0), self.fc_slot(i));
                self.c.get(a).set(L_F0).get(b).set(a).get(L_F0).set(b);
                let known = self.fc.valid | self.fc.empty;
                if known & (1 << ra) == 0 || known & (1 << rb) == 0 {
                    // tags and exact integers in the Cpu move along (the cached ones are
                    // written over when the block leaves)
                    let (pa, pb) = (self.fc_phys(ra), self.fc_phys(rb));
                    self.freg_at(pa, 3);
                    self.c.load64(Off::fints()).set(L_W);
                    self.freg_at(pa, 3);
                    self.freg_at(pb, 3);
                    self.c.load64(Off::fints()).store64(Off::fints());
                    self.freg_at(pb, 3);
                    self.c.get(L_W).store64(Off::fints());
                    self.fswap_bits(Off::fexact(), pa, pb);
                    self.fswap_bits(Off::fempty(), pa, pb);
                }
                let fc = &mut self.fc;
                for m in [&mut fc.valid, &mut fc.empty] {
                    let (x, y) = ((*m >> ra) & 1, (*m >> rb) & 1);
                    *m = *m & !(1 << ra | 1 << rb) | x << rb | y << ra;
                }
                fc.dirty |= 1 << ra | 1 << rb;
            }
            Fadd | Faddp | Fsub | Fsubp | Fsubr | Fsubrp | Fmul | Fmulp | Fdiv | Fdivp | Fdivr | Fdivrp if fc_handles(ins) => {
                let mn = ins.mnemonic();
                // source -> L_F1, d = the destination's slot
                let d = if ins.op_count() == 1 {
                    let a = self.addr(ins.memory_size().size() as u32);
                    self.fload_mem(a);
                    self.c.set(L_F1);
                    self.fc_get(0);
                    self.fc_slot(0)
                } else {
                    let l = self.fc_get(st_index(ins.op_register(1)));
                    self.c.get(l).set(L_F1);
                    self.fc_get(st_index(ins.op_register(0)));
                    self.fc_slot(st_index(ins.op_register(0)))
                };
                self.c.get(L_FREG + d);
                if matches!(mn, Fsubr | Fsubrp | Fdivr | Fdivrp) {
                    self.c.set(L_F0).get(L_F1).get(L_F0);
                } else {
                    self.c.get(L_F1);
                }
                self.c.op(match mn {
                    Fadd | Faddp => 0xa0,
                    Fsub | Fsubp | Fsubr | Fsubrp => 0xa1,
                    Fmul | Fmulp => 0xa2,
                    _ => 0xa3,
                });
                self.c.set(L_FREG + d);
                self.fc_wrote(d);
                if matches!(mn, Faddp | Fsubp | Fsubrp | Fmulp | Fdivp | Fdivrp) {
                    self.fc_pop();
                }
            }
            Div | Idiv if op_bits(ins, 0) == 32 && simple => {
                let addr = self.mem_addr();
                let d = self.temp();
                self.read_op(0, addr);
                self.c.set(d);
                let (eax, edx) = (L_REG, L_REG + 2);
                if ins.mnemonic() == Div {
                    // #DE (divide by zero, quotient too big): the interpreter raises it
                    self.c.get(d).eqz().get(edx).get(d).ge_u().or().if_();
                    self.depth += 1;
                    self.bail();
                    self.depth -= 1;
                    self.c.end();
                    // EDX is usually 0 (zeroed just before): a 32-bit division, much cheaper
                    self.c.get(edx).eqz().if_();
                    self.c.get(eax).get(d).op(0x70 /* i32.rem_u */).get(eax).get(d).op(0x6e /* i32.div_u */).set(eax).set(edx);
                    self.c.else_();
                    self.c.get(edx).i64_extend_u().i64c(32).op(0x86).get(eax).i64_extend_u().op(0x84).set(L_W);
                    self.c.get(L_W).get(d).i64_extend_u().op(0x82 /* i64.rem_u */).wrap();
                    self.c.get(L_W).get(d).i64_extend_u().op(0x80 /* i64.div_u */).wrap().set(eax);
                    self.c.set(edx);
                    self.c.end();
                } else {
                    // d == 0, d == -1 (MIN / -1 traps in wasm) or a quotient outside i32
                    let r = self.temp();
                    self.c.get(d).eqz().get(d).i32c(-1).eq().or().if_();
                    self.depth += 1;
                    self.bail();
                    self.depth -= 1;
                    self.c.end();
                    // EDX:EAX usually is EAX sign-extended (CDQ): a 32-bit division
                    self.c.get(edx).get(eax).i32c(31).shr_s().eq().if_();
                    self.c.get(eax).get(d).op(0x6f /* i32.rem_s */).get(eax).get(d).op(0x6d /* i32.div_s */).set(eax).set(edx);
                    self.c.else_();
                    self.depth += 1;
                    // n = edx:eax
                    self.c.get(edx).i64_extend_u().i64c(32).op(0x86).get(eax).i64_extend_u().op(0x84).set(L_W);
                    self.c.get(L_W).get(d).i64_extend_s().op(0x81 /* i64.rem_s */).wrap().set(r);
                    self.c.get(L_W).get(d).i64_extend_s().op(0x7f /* i64.div_s */).set(L_W);
                    self.c.get(L_W).get(L_W).wrap().i64_extend_s().op(0x52 /* i64.ne */).if_();
                    self.depth += 1;
                    self.bail();
                    self.depth -= 1;
                    self.c.end();
                    self.c.get(L_W).wrap().set(eax);
                    self.c.get(r).set(edx);
                    self.depth -= 1;
                    self.c.end();
                }
            }

            // ---- SSE: 128-bit moves and bitwise logic on v128
            _ if xmm_cached(ins) && !is_logic(ins) => {
                let a = self.mem_addr_sized(16);
                let off = self.xmm_dst_prep(0, a);
                self.xmm_read(1, a);
                self.xmm_dst_store(0, off);
            }
            Pxor | Xorps | Xorpd if ins.op_kind(1) == OpKind::Register && ins.op_register(0) == ins.op_register(1) && ins.op_register(0).is_xmm() => {
                self.c.simd(12).b.extend_from_slice(&[0; 16]); // v128.const 0
                self.xmm_set(ins.op_register(0));
            }
            _ if xmm_cached(ins) => {
                let a = self.mem_addr_sized(16);
                let d = ins.op_register(0);
                if matches!(ins.mnemonic(), Pandn | Andnps | Andnpd) {
                    // ~dst & src = andnot(src, dst)
                    self.xmm_read(1, a);
                    let l = self.xmm_get(d);
                    self.c.get(l).simd(79);
                } else {
                    let l = self.xmm_get(d);
                    self.c.get(l);
                    self.xmm_read(1, a);
                    self.c.simd(match ins.mnemonic() {
                        Pxor | Xorps | Xorpd => 81,
                        Por | Orps | Orpd => 80,
                        _ => 78,
                    });
                }
                self.xmm_set(d);
            }

            // ---- control flow (always ends the block)
            Jmp if ins.op_kind(0) == OpKind::NearBranch32 => {
                self.goto(ins.near_branch32());
                return false;
            }
            Jo | Jno | Jb | Jae | Je | Jne | Jbe | Ja | Js | Jns | Jp | Jnp | Jl | Jge | Jle | Jg if ins.op_kind(0) == OpKind::NearBranch32 => {
                self.cond(cc_index(ins.condition_code()));
                self.c.if_();
                self.depth += 1;
                self.goto(ins.near_branch32());
                self.depth -= 1;
                self.c.end();
                self.goto(next);
                return false;
            }
            Call if ins.op_kind(0) == OpKind::NearBranch32 => {
                let ret = self.temp();
                self.c.u32c(next).set(ret);
                self.push32(ret);
                if let Some(&(entry, _)) = self.inlined.get(&(self.block_index as usize)) {
                    self.goto_block(entry, ins.near_branch32());
                    return false;
                }
                if self.fnm {
                    let t = self.temp();
                    match self.thunks.get(&ins.near_branch32()) {
                        // an import thunk's: to where its slot points
                        Some(&slot) if (GUEST_LO..GUEST_HI).contains(&slot) => self.c.u32c(slot).load32(0).set(t),
                        _ => self.c.u32c(ins.near_branch32()).set(t),
                    };
                    self.call_fn(t, next);
                    return false;
                }
                let f = ins.near_branch32();
                if self.block_of(f).is_none() && CALLS.load(core::sync::atomic::Ordering::Relaxed) {
                    let t = self.temp();
                    self.c.u32c(f).set(t);
                    self.call_out(t, next);
                    return false;
                }
                self.ras_push(next);
                self.goto(f);
                return false;
            }
            Call if simple && op_bits(ins, 0) == 32 => {
                let addr = self.mem_addr();
                let (t, ret) = (self.temp(), self.temp());
                self.read_op(0, addr);
                self.c.set(t);
                self.c.u32c(next).set(ret);
                self.push32(ret);
                if let Some(&(entry, Some(f))) = self.inlined.get(&(self.block_index as usize)) {
                    self.c.get(t).u32c(f).eq().if_();
                    self.depth += 1;
                    self.goto_block(entry, f);
                    self.depth -= 1;
                    self.c.end();
                }
                if self.fnm {
                    self.call_fn(t, next);
                    return false;
                }
                if CALLS.load(core::sync::atomic::Ordering::Relaxed) && INDIRECT_CALLS.load(core::sync::atomic::Ordering::Relaxed) {
                    self.call_out(t, next);
                    return false;
                }
                self.ras_push(next);
                self.c.get(t);
                self.jump_dyn();
                return false;
            }
            Jmp if self.structured && self.jnode.contains_key(&(self.block_index as usize)) => {
                // to the table's dispatch node: the index in L_BLK, the target in L_NEXT
                let d = self.jnode[&(self.block_index as usize)];
                if ins.op0_kind() == OpKind::Register {
                    self.get_reg(ins.op0_register());
                    self.c.set(L_NEXT);
                } else {
                    let addr = self.mem_addr();
                    self.read_op(0, addr);
                    self.c.set(L_NEXT);
                    self.get_reg(ins.memory_index());
                    self.c.set(L_BLK);
                }
                self.goto_block(d, TARGET_NEXT);
                return false;
            }
            Jmp if simple && op_bits(ins, 0) == 32 => {
                let addr = self.mem_addr();
                self.read_op(0, addr);
                let t = self.temp();
                self.c.set(t);
                if let Some(&(entry, Some(f))) = self.inlined.get(&(self.block_index as usize)) {
                    // an inlined callee's tail jump to the function it wraps
                    self.c.get(t).u32c(f).eq().if_();
                    self.depth += 1;
                    self.goto_block(entry, f);
                    self.depth -= 1;
                    self.c.end();
                }
                if self.fnm && !(ins.op0_kind() == OpKind::Memory && ins.memory_index() != Register::None) {
                    // a tail jump (an import thunk's `jmp [iat]`): a target without a function
                    // gets one (see emit_fn_lookup); where it goes is sampled (see call_fn)
                    self.c.i32c(1).set(L_ST);
                    let (h, e) = (self.temp(), self.temp());
                    let site = ins.ip32();
                    ring_sample(&mut self.c, h, e, t, site);
                }
                self.c.get(t);
                self.jump_dyn();
                return false;
            }
            Ret => {
                let t = self.pop32();
                if ins.op_count() == 1 {
                    let n = ins.immediate16() as i32;
                    self.c.get(L_REG + 4).i32c(n).add().set(L_REG + 4);
                }
                if self.ctx != 0 {
                    // an inlined callee returning to its call
                    let r = self.ret_to[self.ctx as usize];
                    self.c.get(t).u32c(r).eq().if_();
                    self.depth += 1;
                    let ctx = core::mem::replace(&mut self.ctx, 0);
                    self.goto(r);
                    self.ctx = ctx;
                    self.depth -= 1;
                    self.c.end();
                }
                if self.fnm {
                    // the state into the Cpu, back to the wasm caller
                    self.flags_to_cpu();
                    self.mmx_flush();
                    self.xmm_flush();
                    self.x87_flush_out();
                    bump(&mut self.c, 5);
                    self.store_dirty();
                    self.c.get(L_CPU).get(L_BUD).store32(Off::budget());
                    self.c.get(L_CPU).get(t).store32(Off::eip());
                    self.c.i32c(RET_FN).get(L_REG + 4).ret();
                    return false;
                }
                // most returns go back to the latest call in the region (through the dispatch,
                // so the x87/MMX cache goes back to the Cpu first like any jump's)
                self.fc_flush();
                self.c.get(t).get(L_RA1).eq().if_();
                self.depth += 1;
                self.c.get(L_BUD).i32c(1).sub().tee(L_BUD).i32c(0).le_s().if_();
                self.depth += 1;
                self.c.get(t);
                self.exit_with(RET_CONTINUE);
                self.depth -= 1;
                self.c.end();
                bump(&mut self.c, 11);
                self.c.get(L_RK1).set(L_BLK);
                self.c.br(self.depth - 1);
                self.depth -= 1;
                self.c.end();
                for &r in self.returns {
                    self.c.get(t).u32c(r).eq().if_();
                    self.depth += 1;
                    self.goto(r);
                    self.depth -= 1;
                    self.c.end();
                }
                if CALLS.load(core::sync::atomic::Ordering::Relaxed) {
                    // back to the wasm caller (a CALL's call_out, or the dispatcher)
                    self.mmx_flush();
                    self.xmm_flush();
                    self.x87_flush_out();
                    bump(&mut self.c, 5);
                    self.c.i32c(RET_VALUES).get(t);
                    push_state(&mut self.c);
                    self.c.ret();
                    return false;
                }
                self.c.get(t);
                self.jump_dyn();
                return false;
            }

            _ if packed::supported(ins) => self.packed(),
            _ => {
                let ends = ins.flow_control() != FlowControl::Next;
                self.fallback(ends);
                if ends {
                    return false;
                }
            }
        }
        self.after(last, next)
    }

    /// End of an instruction that does not branch: the block continues, or at its last
    /// instruction falls through to the next address.
    fn after(&mut self, last: bool, next: u32) -> bool {
        if last {
            self.goto(next);
        }
        true
    }

    /// Jumps to a static target: the block inside the region (polling the budget and signals
    /// on backward jumps), or out to the dispatcher. Blocks are in address order, guest loops
    /// are wasm loops: a backward jump goes to the loop label of the enclosing loop it heads,
    /// a forward one to the label just before the block when one is open. Anything else goes
    /// through the dispatch at the top (`L_BLK`), and into loops through their side dispatch.
    fn goto(&mut self, target: u32) {
        let Some(k) = self.block_of(target) else {
            // maybe another region's
            self.c.u32c(target);
            self.jump_dyn();
            return;
        };
        self.goto_block(k, target);
    }

    /// The block at `target` in the current context, or else the region's own.
    fn block_of(&self, target: u32) -> Option<usize> {
        let k = self.targets.get(&(self.ctx, target)).or_else(|| self.targets.get(&(0, target)));
        k.map(|&k| k as usize)
    }

    /// Pushes jump target `target`: TARGET_NEXT is the one in L_NEXT (a jump table's).
    fn push_target(&mut self, target: u32) {
        if target == TARGET_NEXT {
            self.c.get(L_NEXT);
        } else {
            self.c.u32c(target);
        }
    }

    /// [`Self::goto_block`] in a structured layout: to the open label, else out.
    fn goto_structured(&mut self, k: usize, target: u32) {
        if let Some(p) = self.back[k] {
            // (pending signals are noticed when the budget runs out)
            self.c.get(L_BUD).i32c(1).sub().tee(L_BUD).i32c(0).le_s();
            self.c.if_();
            self.depth += 1;
            self.push_target(target);
            self.exit_with(RET_CONTINUE);
            self.depth -= 1;
            self.c.end();
            self.c.br(self.depth - p);
        } else if let Some(p) = self.fwd[k] {
            self.c.br(self.depth - p);
        } else {
            // not a way the layout has (irreducible): leave the function
            self.push_target(target);
            self.jump_dyn();
        }
    }

    /// Jumps to block `k` (at address `target`).
    fn goto_block(&mut self, k: usize, target: u32) {
        if self.structured {
            self.mmx_flush();
            self.xmm_flush();
            // (the cache stays for the code after a conditional jump)
            let fc = self.fc;
            let labeled = self.back[k].is_some() || self.fwd[k].is_some();
            if labeled && self.fc_fn.is_some_and(|(root, nb)| k != root && k < nb) {
                self.fc_to(k);
            } else {
                self.x87_flush();
                self.fc = FCache::default();
            }
            self.goto_structured(k, target);
            self.fc = fc;
            return;
        }
        // around a single-block loop the MMX locals stay (see emit_level)
        let again = k as u32 == self.block_index && self.back[k].is_some();
        let keep = self.mm_loop != 0 && again;
        let xkeep = self.xm_loop != 0 && again;
        let backward = k as u32 <= self.block_index;
        // how: (L_BLK value, label); the top dispatch for none
        let route = if let Some(p) = self.back[k] {
            // (L_BLK is -1 inside loops except right after a side dispatch)
            Some((Some(-1), p))
        } else if let (false, Some(p)) = (backward, self.fwd[k]) {
            // a plain entry to a loop's head: no side dispatch
            Some((self.head[k].then_some(-1), p))
        } else if let Some(p) = self.loop_entry(k).filter(|_| !backward) {
            // into the middle of a loop (often the condition at its end): its side dispatch
            Some((Some(k as i32), p))
        } else {
            None
        };
        if !keep {
            self.mmx_flush();
        }
        if !xkeep {
            self.xmm_flush();
        }
        let fc = self.fc;
        if route.is_some() && self.fc_range.is_some_and(|(h, e)| (h..=e).contains(&k)) {
            self.fc_to(k);
        } else {
            self.x87_flush();
            self.fc = FCache::default();
        }
        if backward {
            // (pending signals are noticed when the budget runs out; the exits also let a loop
            // that V8 has optimized meanwhile continue in the optimized code: no OSR in wasm)
            self.c.get(L_BUD).i32c(1).sub().tee(L_BUD).i32c(0).le_s();
            self.c.if_();
            self.depth += 1;
            self.c.u32c(target);
            self.exit_with(RET_CONTINUE);
            self.depth -= 1;
            self.c.end();
        }
        self.fc = fc;
        if keep {
            // (an interpreted instruction dropped them: the Cpu has them)
            for i in 0..8 {
                if self.mm_loop & !self.mmx.0 & (1 << i) != 0 {
                    self.c.get(L_CPU).v128load64(Off::mm(Register::MM0 + i)).set(L_MMX + i);
                }
            }
        }
        if xkeep {
            for i in 0..8 {
                if self.xm_loop & !self.xmm.0 & (1 << i) != 0 {
                    self.c.get(L_CPU).v128load(Off::xmm(Register::XMM0 + i)).set(L_XMM + i);
                }
            }
        }
        match route {
            Some((None, p)) => {
                self.c.br(self.depth - p);
            }
            Some((Some(b), p)) => {
                if b >= 0 {
                    bump(&mut self.c, 13);
                }
                self.c.i32c(b).set(L_BLK).br(self.depth - p);
            }
            None => {
                if LAYOUT.load(core::sync::atomic::Ordering::Relaxed) {
                    use core::fmt::Write;
                    let _ = writeln!(self.notes, "TOP {} -> {k}{}", self.block_index, if backward { " back" } else { "" });
                }
                bump(&mut self.c, 12);
                self.c.i32c(k as i32).set(L_BLK).br(self.depth - 1);
            }
        }
    }

    /// Changes the x87 cache to the one block `k` of the loop starts with (see `fc_range`).
    /// Those count their slots from TOP (delta 0): L_FTOP moves by delta and the locals with
    /// it, and that is all when the rest is the same; otherwise it goes through the Cpu.
    fn fc_to(&mut self, k: usize) {
        let cur = self.fc;
        let d = cur.delta.rem_euclid(8) as u32;
        let rot = |m: u8| m.rotate_right(d);
        let canon = FCache { on: cur.on, delta: 0, have: rot(cur.have), dirty: rot(cur.dirty), valid: rot(cur.valid), empty: rot(cur.empty) };
        if cur.on && self.fc_out.get(&k).is_none() {
            self.fc_out.insert(k, canon);
        }
        let to = self.fc_in.get(&k).copied().unwrap_or_default();
        if !cur.on && !to.on {
            return;
        }
        if cur.on && canon == to {
            if d != 0 {
                // (the Cpu's TOP is L_FTOP while the delta is 0: see x87_flush)
                self.c.get(L_FTOP).i32c(d as i32).add().i32c(7).and().set(L_FTOP);
                self.c.get(L_CPU).get(L_FTOP).store32(Off::ftop());
                let slots: Vec<u32> = (0..8).filter(|r| cur.have & (1 << r) != 0).collect();
                for &r in &slots {
                    self.c.get(L_FREG + r);
                }
                for &r in slots.iter().rev() {
                    self.c.set(L_FREG + (r + 8 - d) % 8);
                }
            }
        } else {
            self.x87_flush();
            if to.on {
                if cur.on {
                    if d != 0 {
                        self.c.get(L_FTOP).i32c(d as i32).add().i32c(7).and().set(L_FTOP);
                    }
                } else {
                    self.c.get(L_CPU).load32(Off::ftop()).set(L_FTOP);
                }
                self.fc_load(to.have);
            }
        }
        self.fc = to;
    }

    /// Loads the slots in `have` from the Cpu (L_FTOP set).
    fn fc_load(&mut self, have: u8) {
        for r in 0..8 {
            if have & (1 << r) != 0 {
                self.fc_phys_addr(r);
                self.c.f64load(Off::fregs()).set(L_FREG + r);
            }
        }
    }

    /// The open label before a loop holding block k.
    fn loop_entry(&self, k: usize) -> Option<u32> {
        self.loops.range(..k).rev().filter(|&(_, &e)| e >= k).find_map(|(&h, _)| self.fwd[h])
    }

    /// CALL to the address in local `t` (the return address `next` is pushed) as a wasm call
    /// of the target's region; its RET comes back here with the state as results
    /// (RET_VALUES). Continues at `next` when that is where it returned, else wherever it went.
    /// Without a translation, or nested too deep, a tail call as before.
    fn call_out(&mut self, t: u32, next: u32) {
        self.mmx_flush();
        self.xmm_flush();
        self.x87_flush();
        self.fc = FCache::default();
        let hash = offset_of!(Cpu, jit_hash) as u32;
        let maskf = offset_of!(Cpu, jit_mask) as u32;
        let (h, e) = (self.temp(), self.temp());
        self.c.block(); // found 0
        self.c.block(); // slow 0, found 1
        self.depth += 2;
        self.c.get(L_CPU).load32(Off::calls()).u32c(MAX_DEPTH).ge_u().br_if(0);
        self.c.get(t).u32c(0x9e37_79b1).mul().i32c(15).shr_u().get(L_CPU).load32(maskf).and().set(h);
        self.c.loop_(); // probe 0, slow 1, found 2
        self.depth += 1;
        self.c.get(L_CPU).load32(hash).get(h).i32c(3).shl().add().tee(e).load32(0).tee(L_NEXT);
        self.c.get(t).eq().br_if(2);
        self.c.get(L_NEXT).eqz().br_if(1);
        self.c.get(h).i32c(1).add().get(L_CPU).load32(maskf).and().set(h).br(0);
        self.depth -= 1;
        self.c.end(); // probe
        self.depth -= 1;
        self.c.end(); // slow: the dispatcher's way
        bump(&mut self.c, 1);
        self.c.get(t);
        self.jump_dyn();
        self.depth -= 1;
        self.c.end(); // found: entry at e + 4
        bump(&mut self.c, 0);
        // into the ring like a jump between regions (a hot callee gets merged in, see emit_lookup)
        let ring = offset_of!(Cpu, jit_ring) as u32;
        self.c.get(L_CPU).load32(ring).tee(h).get(h).load32(0).tee(L_NEXT).i32c(1).add().store32(0);
        self.c.get(h).get(L_NEXT).i32c(RING_SIZE as i32 - 1).and().i32c(3).shl().add().tee(h);
        self.c.get(L_ARG).u32c((1 << ENTRY_SHIFT) - 1).and().store32(4);
        self.c.get(h).get(t).store32(8);
        self.c.get(L_CPU).get(L_CPU).load32(Off::calls()).i32c(1).add().store32(Off::calls());
        self.c.get(L_CPU).get(e).load32(4).tee(h);
        push_state(&mut self.c);
        self.c.get(h).u32c((1 << ENTRY_SHIFT) - 1).and().call_indirect(2, 0);
        for l in (L_REG..N_PARAMS).rev() {
            self.c.set(l);
        }
        self.c.set(L_NEXT).set(L_ST);
        self.c.get(L_CPU).get(L_CPU).load32(Off::calls()).i32c(1).sub().store32(Off::calls());
        // left another way (the state is in the Cpu): pass it on as it is
        self.c.get(L_ST).i32c(RET_VALUES).ne().if_();
        bump(&mut self.c, 4);
        self.c.get(L_ST);
        for _ in 1..N_PARAMS {
            self.c.i32c(0);
        }
        self.c.ret().end();
        self.c.get(L_NEXT).u32c(next).eq().if_();
        self.depth += 1;
        bump(&mut self.c, 2);
        self.goto(next);
        self.depth -= 1;
        self.c.end();
        bump(&mut self.c, 3);
        self.c.get(L_NEXT);
        self.jump_dyn();
    }

    /// A function's CALL of the address in local `t` (the return address `next` pushed): a
    /// wasm call of the target's function (the state through the Cpu), continuing at `next`
    /// when it returned there. Without a function there, nested too deep or out of budget,
    /// the dispatcher's way (leaving for `t`).
    fn call_fn(&mut self, t: u32, next: u32) {
        self.mmx_flush();
        self.xmm_flush();
        self.x87_flush();
        self.fc = FCache::default();
        // (the MMX/XMM locals are the Cpu's now: the callee may change the registers, and
        // flushing them again after its return would undo that, as a vectorcall's results)
        self.mmx = (0, 0);
        self.xmm = (0, 0);
        let (h, v) = (self.temp(), self.temp());
        let site = self.ins.ip32();
        if self.ins.flow_control() == FlowControl::IndirectCall {
            // where it goes, sampled by the host (a hint to inline it next time)
            ring_sample(&mut self.c, h, v, t, site);
        }
        self.flags_to_cpu();
        self.store_dirty();
        self.c.block(); // slow 0
        self.depth += 1;
        self.c.get(L_BUD).i32c(1).sub().tee(L_BUD).i32c(0).le_s();
        // (a function's L_ARG is its call depth)
        self.c.get(L_ARG).u32c(MAX_DEPTH).ge_u().or().br_if(0);
        // the call cache: this site's slot (by its address) holds (target, hash table entry)
        // of the latest function it called; a miss asks the host (which fills it)
        let slot = (site.wrapping_mul(0x9e37_79b1) >> 20) & (CALL_CACHE - 1);
        self.c.get(L_CPU).load32(Off::call_cache()).u32c(slot * 8).add().tee(h).load32(0).get(t).eq();
        self.c.if_i32().get(h).load32(4).else_().get(L_CPU).get(h).get(t).call(F_CALLMISS).end().tee(v).eqz().br_if(0);
        bump(&mut self.c, 0);
        self.c.get(L_CPU).get(L_BUD).store32(Off::budget());
        self.c.get(L_CPU).get(L_ARG).i32c(1).add();
        self.c.get(L_REG + 4);
        self.c.get(v).u32c((1 << ENTRY_SHIFT) - 1).and().i32c(1).add().call_indirect(T_FN, 0).set(v).set(h);
        // left another way (the state is in the Cpu): pass it on
        self.c.get(h).i32c(RET_FN).ne().if_().get(h).i32c(0).ret().end();
        self.c.get(L_CPU).load32(Off::eip()).u32c(next).ne().if_();
        bump(&mut self.c, 4);
        self.c.i32c(RET_CONTINUE).i32c(0).ret().end();
        self.load_regs_esp(v);
        self.c.get(L_CPU).load32(Off::budget()).set(L_BUD);
        self.c.i32c(-1).set(L_OP);
        self.goto(next);
        self.depth -= 1;
        self.c.end(); // slow
        bump(&mut self.c, 1);
        // (callmiss put (FN_TAG, target) in the ring: the host makes the callee a function)
        self.c.get(t);
        self.exit_with(RET_CONTINUE);
        self.temps -= 2;
    }

    /// Notes the return address of a call for RET, if it is one of the region's blocks.
    fn ras_push(&mut self, ret: u32) {
        let Some(k) = self.block_of(ret) else { return };
        self.c.u32c(ret).set(L_RA1).i32c(k as i32).set(L_RK1);
    }

    /// Jumps to the address on the stack (looked up by the region's shared code).
    fn jump_dyn(&mut self) {
        self.fc_flush();
        self.c.set(L_NEXT);
        if self.fnm && self.ctx != 0 {
            self.call_rest();
        }
        self.c.br(self.depth - 2);
    }

    /// Leaving an inlined callee (to L_NEXT: a block of it that was left out, or where its
    /// tail jump goes): a wasm call of the function there, going on at the call's return
    /// address when it returns there, as if the callee had been called (else the function
    /// would be left with the callee's frame on the guest stack, and its RET would return to
    /// this function's caller, which then has to leave too). Without a function there, on
    /// the way out as before.
    fn call_rest(&mut self) {
        let r = self.ret_to[self.ctx as usize];
        let (h, v) = (self.temp(), self.temp());
        let hash = offset_of!(Cpu, jit_hash) as u32;
        let maskf = offset_of!(Cpu, jit_mask) as u32;
        self.c.block(); // slow 0
        self.depth += 1;
        self.c.get(L_BUD).i32c(1).sub().tee(L_BUD).i32c(0).le_s();
        self.c.get(L_ARG).u32c(MAX_DEPTH).ge_u().or().br_if(0);
        // the hash table entry of L_NEXT (as emit_fn_lookup), a function's
        self.c.get(L_NEXT).u32c(0x9e37_79b1).mul().i32c(15).shr_u().get(L_CPU).load32(maskf).and().set(h);
        self.c.block(); // found 0, slow 1
        self.c.loop_(); // probe 0, found 1, slow 2
        self.c.get(L_CPU).load32(hash).get(h).i32c(3).shl().add().tee(v).load32(0).get(L_NEXT).eq().if_();
        self.c.get(v).load32(4).tee(v).u32c(FN_TAG).and().eqz().br_if(3).br(2);
        self.c.end();
        self.c.get(v).load32(0).eqz().br_if(2);
        self.c.get(h).i32c(1).add().get(L_CPU).load32(maskf).and().set(h).br(0);
        self.c.end();
        self.c.end();
        // the state into the Cpu (all of it: this can be in the middle of a block)
        for i in (0..8).filter(|i| self.wregs & (1 << i) != 0) {
            self.c.get(L_CPU).get(L_REG + i).store32(Off::reg(i as usize));
        }
        lazy_to_cpu(&mut self.c);
        self.c.end();
        bump(&mut self.c, 2);
        self.c.get(L_CPU).get(L_BUD).store32(Off::budget());
        self.c.get(L_CPU).get(L_ARG).i32c(1).add();
        self.c.get(v).i32c(ENTRY_SHIFT as i32).shr_u().u32c(0x7ff).and().i32c(16).shl().or();
        self.c.get(L_REG + 4);
        self.c.get(v).u32c((1 << ENTRY_SHIFT) - 1).and().i32c(1).add().call_indirect(T_FN, 0).set(v).set(h);
        // left another way (the state is in the Cpu): pass it on
        self.c.get(h).i32c(RET_FN).ne().if_().get(h).i32c(0).ret().end();
        self.c.get(L_CPU).load32(Off::eip()).u32c(r).ne().if_();
        bump(&mut self.c, 4);
        self.c.i32c(RET_CONTINUE).i32c(0).ret().end();
        let (dirty, fc, flags, mmx, xmm) = (self.dirty, self.fc, self.flags, self.mmx, self.xmm);
        self.load_regs_esp(v);
        self.c.get(L_CPU).load32(Off::budget()).set(L_BUD);
        self.c.i32c(-1).set(L_OP);
        self.fc = FCache::default();
        self.flags = Flags::Entry;
        // (the MMX/XMM locals were flushed on the way here, and are stale after the call)
        (self.mmx, self.xmm) = ((0, 0), (0, 0));
        let ctx = core::mem::replace(&mut self.ctx, 0);
        self.goto(r);
        self.ctx = ctx;
        (self.dirty, self.fc, self.flags, self.mmx, self.xmm) = (dirty, fc, flags, mmx, xmm);
        self.depth -= 1;
        self.c.end(); // slow
        self.temps -= 2;
    }
}

/// A jump table's dispatch node (`structure`): `br_table` on the index (L_BLK) to a case per
/// entry that goes on to the entry's block if the loaded target (L_NEXT) is that entry, else
/// (the table changed, or an index out of range) jumps there the dynamic way.
fn emit_dispatch(g: &mut Gen<'_>, t: &structure::Table, node: usize) {
    g.ctx = t.ctx;
    g.block_index = node as u32;
    g.flags = Flags::Entry;
    let m = t.entries.len();
    g.c.block(); // miss
    g.depth += 1;
    let miss = g.depth;
    for _ in 0..m {
        g.c.block();
    }
    g.depth += m as u32;
    if let Some((mul, shift, slots)) = &t.hash {
        g.c.get(L_NEXT).u32c(*mul).mul().i32c(*shift as i32).shr_u().op(0x0e);
        wasm::uleb(&mut g.c.b, slots.len() as u64);
        for &i in slots {
            wasm::uleb(&mut g.c.b, i as u64);
        }
    } else {
        g.c.get(L_BLK).op(0x0e);
        wasm::uleb(&mut g.c.b, m as u64);
        for i in 0..m {
            wasm::uleb(&mut g.c.b, i as u64);
        }
    }
    wasm::uleb(&mut g.c.b, m as u64);
    for &a in &t.entries {
        g.c.end();
        g.depth -= 1;
        g.c.get(L_NEXT).u32c(a).eq().if_();
        g.depth += 1;
        g.goto(a);
        g.depth -= 1;
        g.c.end();
        g.c.br(g.depth - miss);
    }
    g.c.end(); // miss
    g.depth -= 1;
    g.c.get(L_NEXT);
    g.jump_dyn();
}

/// A function's first node with more entries than its root (`entries[0]`): `br_table` on
/// L_BLK (0 the root, i the i-th other) to the block.
fn emit_entries<'a>(g: &mut Gen<'a>, blocks: &'a [(u32, Vec<Instruction>)], entries: &[usize], node: usize) {
    g.ctx = 0;
    g.block_index = node as u32;
    g.flags = Flags::Entry;
    g.fc = FCache::default();
    g.dirty = 0;
    g.mm_loop = 0;
    g.mmx = (0, 0);
    g.xm_loop = 0;
    g.xmm = (0, 0);
    let m = entries.len();
    for _ in 0..m {
        g.c.block();
    }
    g.depth += m as u32;
    g.c.get(L_BLK).op(0x0e);
    wasm::uleb(&mut g.c.b, m as u64);
    for i in 0..m {
        wasm::uleb(&mut g.c.b, i as u64);
    }
    wasm::uleb(&mut g.c.b, 0);
    for &k in entries {
        g.c.end();
        g.depth -= 1;
        g.goto_block(k, blocks[k].0);
    }
}

/// A function's adapter to the region calling convention (what the dispatcher and regions'
/// jumps call): the state into the Cpu, the function (the next table entry: the module may be
/// merged with others, see `saved`), and its RET as RET_CONTINUE (on at EIP).
fn fn_adapter() -> Func {
    let mut c = Code::default();
    for i in 0..8 {
        c.get(L_CPU).get(L_REG + i).store32(Off::reg(i as usize));
    }
    c.get(L_CPU).get(L_BUD).store32(Off::budget());
    lazy_to_cpu(&mut c);
    c.end();
    // (the entry in the depth's high half)
    c.get(L_CPU).get(L_CPU).load32(Off::calls()).get(L_ARG).i32c(ENTRY_SHIFT as i32).shr_u().u32c(0x7ff).and().i32c(16).shl().or();
    c.get(L_REG + 4);
    c.get(L_ARG).u32c((1 << ENTRY_SHIFT) - 1).and().i32c(1).add().call_indirect(T_FN, 0).set(L_REG + 4).tee(L_ARG).i32c(RET_CONTINUE).get(L_ARG).i32c(RET_FN).ne().select();
    for _ in 1..N_PARAMS {
        c.i32c(0);
    }
    Func { ty: 2, locals: Vec::new(), code: c }
}

/// Stores the lazy operation in the locals to the Cpu (L_OP holds it).
fn lazy_store(c: &mut Code) {
    let lo = Off::lazy();
    c.get(L_CPU).get(L_OP).store32(lo);
    c.get(L_CPU).get(L_FA).store32(lo + 4);
    c.get(L_CPU).get(L_FB).store32(lo + 8);
    c.get(L_CPU).get(L_FR).store32(lo + 12);
    c.get(L_CPU).get(L_FC).store32(lo + 16);
}

/// Opens `if L_OP != -1` and stores the lazy operation; the caller closes the `if`.
fn lazy_to_cpu(c: &mut Code) {
    c.get(L_OP).i32c(-1).ne().if_();
    lazy_store(c);
}

/// Writes registers, EIP (L_NEXT), the budget and lazy flags back to the Cpu.
fn writeback(c: &mut Code, wregs: u8) {
    for i in (0..8).filter(|i| wregs & (1 << i) != 0) {
        c.get(L_CPU).get(L_REG + i).store32(Off::reg(i as usize));
    }
    c.get(L_CPU).get(L_NEXT).store32(Off::eip());
    c.get(L_CPU).get(L_BUD).store32(Off::budget());
    lazy_to_cpu(c);
    c.end();
}

/// Code after the blocks (inside the top loop): finds L_NEXT in the hash table and continues
/// there if the block is in this function, else leaves with RET_CONTINUE.
fn emit_lookup(c: &mut Code) {
    let hash = offset_of!(Cpu, jit_hash) as u32;
    let maskf = offset_of!(Cpu, jit_mask) as u32;
    let (h, e, v) = (L_T, L_T + 1, L_T + 2);
    // depth here: top 0, exit 1
    // pending signals are noticed when the budget runs out (the dispatcher checks them)
    bump(c, 6);
    c.get(L_BUD).i32c(1).sub().tee(L_BUD).i32c(0).le_s();
    c.if_().i32c(RET_CONTINUE).set(L_ST).br(2).end();
    // address 0 is the key of an empty slot: never a block (a call through a null pointer
    // goes to the dispatcher, whose interpreter faults)
    c.get(L_NEXT).eqz().if_().i32c(RET_CONTINUE).set(L_ST).br(2).end();
    c.get(L_NEXT).u32c(0x9e37_79b1).mul().i32c(15).shr_u().get(L_CPU).load32(maskf).and().set(h);
    c.block(); // miss: miss 0, top 1, exit 2
    c.loop_(); // probe 0, miss 1, top 2, exit 3
    c.get(L_CPU).load32(hash).get(h).i32c(3).shl().add().tee(e).load32(0).set(v);
    c.get(v).get(L_NEXT).eq().if_(); // if 0, probe 1, miss 2, top 3
    c.get(e).load32(4).tee(v).u32c((1 << ENTRY_SHIFT) - 1).and();
    c.get(L_ARG).u32c((1 << ENTRY_SHIFT) - 1).and().ne().if_();
    // another region's block: hand over directly (a tail call with the state as arguments,
    // no dispatcher round trip). The jump goes into the ring the host reads to merge regions
    // that call each other a lot: ring[0] counts, then RING_SIZE (from, to) pairs.
    let ring = offset_of!(Cpu, jit_ring) as u32;
    bump(c, 7);
    c.get(L_CPU).load32(ring).tee(h).get(h).load32(0).tee(e).i32c(1).add().store32(0);
    c.get(h).get(e).i32c(RING_SIZE as i32 - 1).and().i32c(3).shl().add().tee(h);
    c.get(L_ARG).u32c((1 << ENTRY_SHIFT) - 1).and().store32(4);
    c.get(h).get(L_NEXT).store32(8);
    c.get(L_CPU).get(v);
    push_state(c);
    c.get(v).u32c((1 << ENTRY_SHIFT) - 1).and();
    c.op(0x13).op(2).op(0); // return_call_indirect table 0
    c.end();
    bump(c, 8);
    c.get(v).i32c(ENTRY_SHIFT as i32).shr_u().set(L_BLK).br(3);
    c.end();
    c.get(v).eqz().br_if(1);
    c.get(h).i32c(1).add().get(L_CPU).load32(maskf).and().set(h).br(0);
    c.end();
    c.end();
    bump(c, 9);
    c.i32c(RET_CONTINUE).set(L_ST).br(1);
}

/// [`emit_lookup`] of a function: a function's entry is a tail call of it (`jmp` to another
/// function), anything else leaves for the dispatcher.
fn emit_fn_lookup(c: &mut Code, entry: u32, wregs: u8) {
    let hash = offset_of!(Cpu, jit_hash) as u32;
    let maskf = offset_of!(Cpu, jit_mask) as u32;
    let (h, e, v) = (L_T, L_T + 1, L_T + 2);
    // depth here: top 0, exit 1
    bump(c, 6);
    c.get(L_BUD).i32c(1).sub().tee(L_BUD).i32c(0).le_s();
    c.if_().i32c(RET_CONTINUE).set(L_ST).br(2).end();
    c.get(L_NEXT).eqz().if_().i32c(RET_CONTINUE).set(L_ST).br(2).end();
    c.get(L_NEXT).u32c(0x9e37_79b1).mul().i32c(15).shr_u().get(L_CPU).load32(maskf).and().set(h);
    c.block(); // miss: miss 0, top 1, exit 2
    c.loop_(); // probe 0, miss 1, top 2, exit 3
    c.get(L_CPU).load32(hash).get(h).i32c(3).shl().add().tee(e).load32(0).set(v);
    c.get(v).get(L_NEXT).eq().if_();
    c.get(e).load32(4).tee(v).u32c(FN_TAG).and().eqz().br_if(2);
    bump(c, 7);
    writeback(c, wregs);
    c.get(L_CPU).get(L_ARG).get(v).i32c(ENTRY_SHIFT as i32).shr_u().u32c(0x7ff).and().i32c(16).shl().or();
    c.get(L_REG + 4);
    c.get(v).u32c((1 << ENTRY_SHIFT) - 1).and().i32c(1).add();
    c.op(0x13).op(T_FN as u8).op(0); // return_call_indirect, table 0
    c.end();
    c.get(v).eqz().br_if(1);
    c.get(h).i32c(1).add().get(L_CPU).load32(maskf).and().set(h).br(0);
    c.end();
    c.end();
    bump(c, 9);
    // leaving the function: into the ring as (entry, target), for the host to build the
    // function again with the blocks that ran since
    // (FN_TAG for a tail jump: the target becomes a function)
    ring_write(c, |c| {
        c.u32c(FN_TAG).u32c(entry).get(L_ST).select();
    });
    c.i32c(RET_CONTINUE).set(L_ST).br(1);
}

/// Writes (from, L_NEXT) to the ring (`from` pushes the first; uses L_T and L_T + 1).
fn ring_write(c: &mut Code, from: impl FnOnce(&mut Code)) {
    ring_write_with(c, L_T, L_T + 1, L_NEXT, from);
}

/// Where the indirect call or jump at `site` goes (to local `to`), into the ring one time in
/// eight (by a hash of the budget: any stride of it): the host's hints (see call_fn).
fn ring_sample(c: &mut Code, h: u32, e: u32, to: u32, site: u32) {
    c.get(L_BUD).u32c(0x9e37_79b1).mul().i32c(29).shr_u().eqz().if_();
    ring_write_with(c, h, e, to, |c| {
        c.u32c(FN_TAG | site);
    });
    c.end();
}

/// [`ring_write`] with temporaries `h`, `e` and the target in local `to`.
fn ring_write_with(c: &mut Code, h: u32, e: u32, to: u32, from: impl FnOnce(&mut Code)) {
    let ring = offset_of!(Cpu, jit_ring) as u32;
    c.get(L_CPU).load32(ring).tee(h).get(h).load32(0).tee(e).i32c(1).add().store32(0);
    c.get(h).get(e).i32c(RING_SIZE as i32 - 1).and().i32c(3).shl().add().tee(h);
    from(c);
    c.store32(4);
    c.get(h).get(to).store32(8);
}

/// Decodes the block at `eip` and flag liveness for it.
fn decode<M: Memory>(mem: &mut M, eip: u32) -> Vec<Instruction> {
    let mut out = Vec::new();
    let mut ip = eip;
    while out.len() < MAX_INS {
        let mut buf = [0u8; 15];
        let n = mem.fetch(ip, &mut buf);
        let mut dec = Decoder::with_ip(32, &buf[..n], ip as u64, DecoderOptions::NONE);
        let ins = dec.decode();
        if ins.is_invalid() {
            break;
        }
        ip = ins.next_ip32();
        let fc = ins.flow_control();
        out.push(ins);
        if fc != FlowControl::Next {
            break;
        }
    }
    out
}

/// C library functions done in wasm (by [`Gen::hle`]) where the guest has them: the start of
/// their code in Wine's PE C runtimes (msvcrt, ucrtbase and the msvcr*), "??" for an absolute
/// address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hle {
    /// memmove(dst, src, n) (also what memcpy jumps to)
    Move,
    /// memset(dst, c, n)
    Set,
}

const HLE: &[(Hle, &str)] = &[
    (Hle::Move, "5589e557565383ec208b45088b750c8945ec8b45108945f0a1????????85c00f85db0000008b45f085c00f84bf000000"),
    (Hle::Set, "55ba0101010189e5565383e4f083ec100fb6450c8b4d108b5d0869f001010101f7e201f283f90f767789442408895424"),
];

fn hle_at<M: Memory>(mem: &mut M, a: u32) -> Option<Hle> {
    let mut buf = [0u8; 48];
    if mem.fetch(a, &mut buf) < buf.len() {
        return None;
    }
    HLE.iter().find(|(_, sig)| {
        sig.as_bytes().chunks(2).zip(buf).all(|(h, b)| h == b"??" || u8::from_str_radix(core::str::from_utf8(h).unwrap(), 16) == Ok(b))
    }).map(|&(h, _)| h)
}

/// A RET at `a`.
fn ret_at(a: u32) -> Instruction {
    Decoder::with_ip(32, &[0xc3], a as u64, DecoderOptions::NONE).decode()
}

/// Guest loops: (first block, last block) ranges, from the backward jumps between blocks in
/// address order. Overlapping ranges are widened so they nest.
/// The blocks of a small function at `f` (all in the region, not following its calls), if it
/// returns somewhere; an indirect tail jump may go on to `hint`'s blocks (then in `tail`).
fn callee(blocks: &[(u32, Vec<Instruction>)], targets: &BTreeMap<u32, u32>, f: u32, hint: Option<u32>, fnm: bool) -> Option<(Vec<usize>, Option<usize>)> {
    let mut body = Vec::new();
    let mut queue = alloc::vec![f];
    let (mut size, mut ret, mut tail, mut split) = (0, false, None, usize::MAX);
    while let Some(a) = queue.pop() {
        // (a block of a hot path's callee that never ran is left out: jumps there leave)
        let Some(&k) = targets.get(&a) else {
            if fnm && a != f {
                continue;
            }
            return None;
        };
        if body.contains(&(k as usize)) {
            continue;
        }
        if tail.is_some() && hint == Some(a) && split == usize::MAX {
            split = body.len();
        }
        body.push(k as usize);
        let list = &blocks[k as usize].1;
        size += list.len();
        if size > MAX_INLINE {
            return None;
        }
        let last = list.last().unwrap();
        ret |= last.mnemonic() == Mnemonic::Ret;
        if last.mnemonic() == Mnemonic::Jmp && last.op0_kind() != OpKind::NearBranch32 && tail.is_none() {
            if let Some(h) = hint.filter(|&h| h != f && targets.contains_key(&h)) {
                tail = Some(k as usize);
                queue.insert(0, h); // (last: after the wrapper's own blocks)
            }
        }
        for s in successors(last) {
            if !(last.flow_control() == FlowControl::Call && s == last.near_branch32()) {
                queue.push(s);
            }
        }
    }
    // in address order, but the function the tail jump goes to after the wrapper (a forward jump)
    let (mut own, mut then) = (body[..split.min(body.len())].to_vec(), body[split.min(body.len())..].to_vec());
    own.sort_unstable();
    then.sort_unstable();
    own.extend(then);
    let body = own;
    ret.then_some((body, tail))
}

/// The blocks of the function at `f` that ran (`ran`), if those are few enough to inline
/// (MAX_INLINE instructions, no calls, a RET among them): the hot path of a bigger function
/// (Wine's heap helpers check and log in blocks that never run). A jump to a block left out
/// leaves the function there.
fn hot_callee<M: Memory>(mem: &mut M, f: u32, trap: (u32, u32), ran: &dyn Fn(u32) -> bool) -> Option<Vec<u32>> {
    let (mut seen, mut queue, mut size, mut ret) = (Vec::new(), alloc::vec![f], 0, false);
    while let Some(a) = queue.pop() {
        if seen.contains(&a) {
            continue;
        }
        if !(GUEST_LO..GUEST_HI).contains(&a) || (trap.0..trap.1).contains(&a) || !ran(a) {
            if a == f {
                return None;
            }
            continue;
        }
        seen.push(a);
        let list = decode(mem, a);
        size += list.len();
        let last = list.last()?;
        // (with calls inside it was slower: Wine's heap 210 -> 350 ns per HeapAlloc + HeapFree)
        // (with calls inside it was slower: Wine's heap 210 -> 350 ns per HeapAlloc + HeapFree)
        if size > MAX_INLINE || matches!(last.flow_control(), FlowControl::Call | FlowControl::IndirectCall | FlowControl::IndirectBranch) {
            return None;
        }
        ret |= last.mnemonic() == Mnemonic::Ret;
        queue.extend(successors(last));
    }
    ret.then_some(seen)
}


/// The blocks of the function at `f` if it is small enough to inline (MAX_INLINE instructions
/// over the blocks its jumps reach, no calls, ending in RET).
fn small_callee<M: Memory>(mem: &mut M, f: u32, trap: (u32, u32), hints: &dyn Fn(u32) -> Option<u32>) -> Option<Vec<u32>> {
    let (mut seen, mut queue, mut size, mut ret) = (Vec::new(), alloc::vec![f], 0, false);
    while let Some(a) = queue.pop() {
        if seen.contains(&a) {
            continue;
        }
        if !(GUEST_LO..GUEST_HI).contains(&a) || (trap.0..trap.1).contains(&a) {
            return None;
        }
        seen.push(a);
        let list = decode(mem, a);
        size += list.len();
        let last = list.last()?;
        if size > MAX_INLINE || matches!(last.flow_control(), FlowControl::Call | FlowControl::IndirectCall) {
            return None;
        }
        if last.flow_control() == FlowControl::IndirectBranch {
            // a wrapper's tail jump: to where it mostly goes
            queue.push(hints(last.ip32())?);
            continue;
        }
        ret |= last.mnemonic() == Mnemonic::Ret;
        queue.extend(successors(last));
    }
    ret.then_some(seen)
}

/// A loop the code around enters only in the middle (a jump to the condition at its end) starts
/// there instead: then just the way in from the top dispatch goes through its side dispatch,
/// not every turn of it (the dispatch at a loop's head is checked each time round).
fn rotate_loops(blocks: &mut [(u32, Vec<Instruction>)]) {
    let targets: BTreeMap<(u32, u32), u32> = blocks.iter().enumerate().map(|(i, b)| ((0, b.0), i as u32)).collect();
    let loops = find_loops(blocks, &alloc::vec![0; blocks.len()], &targets, &[]);
    for (&h, &e) in &loops {
        if e == h || loops.range(h + 1..=e).next().is_some() {
            continue;
        }
        let mut entry = None;
        let mut one = true;
        for (_, (_, list)) in blocks.iter().enumerate().filter(|(i, _)| !(h..=e).contains(i)) {
            let last = list.last().unwrap();
            for s in successors(last) {
                if last.flow_control() == FlowControl::Call && s == last.near_branch32() {
                    continue;
                }
                if let Some(j) = targets.get(&(0, s)).map(|&j| j as usize).filter(|j| (h..=e).contains(j)) {
                    one &= entry.is_none_or(|x| x == j);
                    entry = Some(j);
                }
            }
        }
        if let Some(m) = entry.filter(|&m| one && m > h) {
            blocks[h..=e].rotate_left(m - h);
        }
    }
}

/// A block inside a loop's range that only blocks before the loop jump to, and that jumps out
/// of the loop itself, goes before the loop: inside, each way into it is a jump into the middle
/// of the loop (through its dispatch), and V8 then keeps little in registers. (A compiler's
/// `for` loop whose test sits at the bottom, entered by a jump over a block only an enclosing
/// loop enters at the top, has its body in the middle of that enclosing loop's range.)
fn hoist_entries(blocks: &mut [(u32, Vec<Instruction>)]) {
    for _ in 0..8 {
        let targets: BTreeMap<(u32, u32), u32> = blocks.iter().enumerate().map(|(i, b)| ((0, b.0), i as u32)).collect();
        let loops = find_loops(blocks, &alloc::vec![0; blocks.len()], &targets, &[]);
        let succ = |i: usize| -> Vec<usize> {
            let last = blocks[i].1.last().unwrap();
            successors(last)
                .into_iter()
                .filter(|&s| !(last.flow_control() == FlowControl::Call && s == last.near_branch32()))
                .filter_map(|s| targets.get(&(0, s)).map(|&j| j as usize))
                .collect()
        };
        let mut preds: Vec<Vec<usize>> = alloc::vec![Vec::new(); blocks.len()];
        for i in 0..blocks.len() {
            for j in succ(i) {
                preds[j].push(i);
            }
        }
        let found = loops.iter().find_map(|(&h, &e)| {
            (h + 1..=e).find(|&m| {
                // in no loop nested in this one
                !loops.iter().any(|(&h2, &e2)| h2 > h && (h2..=e2).contains(&m))
                    && !preds[m].is_empty()
                    && preds[m].iter().all(|&p| p < h)
                    && succ(m).iter().any(|j| !(h..=e).contains(j))
            }).map(|m| (h, m))
        });
        let Some((h, m)) = found else { return };
        blocks[h..=m].rotate_right(1);
    }
}

/// Loops (head -> last block) from the backward jumps; an inlined callee's RET jumps to its
/// call's return address (`ret_to`).
fn find_loops(blocks: &[(u32, Vec<Instruction>)], ctxs: &[u32], targets: &BTreeMap<(u32, u32), u32>, ret_to: &[u32]) -> BTreeMap<usize, usize> {
    let mut ends: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, (_, list)) in blocks.iter().enumerate() {
        let ctx = ctxs[i];
        let last = list.last().unwrap();
        let mut next = successors(last);
        if ctx != 0 && last.mnemonic() == Mnemonic::Ret {
            next.push(ret_to[ctx as usize]);
        }
        for s in next {
            // a call to an earlier function is no loop (else everything between is one)
            if last.flow_control() == FlowControl::Call && s == last.near_branch32() {
                continue;
            }
            match targets.get(&(ctx, s)).or_else(|| targets.get(&(0, s))) {
                Some(&j) if j as usize <= i => {
                    let e = ends.entry(j as usize).or_insert(i);
                    *e = (*e).max(i);
                }
                _ => {}
            }
        }
    }
    let mut loops = BTreeMap::new();
    let mut open: Vec<usize> = Vec::new();
    for (&h, &e) in &ends {
        while open.last().is_some_and(|o| loops[o] < h) {
            open.pop();
        }
        for o in &open {
            let end = loops.get_mut(o).unwrap();
            *end = e.max(*end);
        }
        loops.insert(h, e);
        open.push(h);
    }
    loops
}

#[derive(Clone, Copy)]
enum Item {
    Block(usize),
    /// first and last block
    Loop(usize, usize),
}

impl Item {
    fn range(self) -> (usize, usize) {
        match self {
            Item::Block(i) => (i, i),
            Item::Loop(h, e) => (h, e),
        }
    }
}

/// The blocks a..=b at one nesting level: single blocks and the outermost loops.
fn level(a: usize, b: usize, loops: &BTreeMap<usize, usize>) -> Vec<Item> {
    let mut items = Vec::new();
    let mut i = a;
    while i <= b {
        if let Some(&e) = loops.get(&i) {
            items.push(Item::Loop(i, e));
            i = e + 1;
        } else {
            items.push(Item::Block(i));
            i += 1;
        }
    }
    items
}

/// One nesting level: a wasm block ending before each item (the targets of forward jumps),
/// the dispatch on L_BLK, then the items. The top level always dispatches; a loop level (its
/// range in `side`) only when entered from the top dispatch (L_BLK != -1; every other way into
/// a loop label sets it to -1), passing entries into nested loops on.
fn emit_level<'a>(
    g: &mut Gen<'a>,
    blocks: &'a [(u32, Vec<Instruction>)],
    loops: &BTreeMap<usize, usize>,
    items: &[Item],
    side: Option<(usize, usize)>,
    count: &mut usize,
) {
    let r = items.len();
    let mut label = alloc::vec![0; r];
    for t in (0..r).rev() {
        g.c.block();
        g.depth += 1;
        label[t] = g.depth;
        g.fwd[items[t].range().0] = Some(g.depth);
    }
    let (lo, hi) = side.unwrap_or((0, blocks.len() - 1));
    if side.is_some() {
        g.c.get(L_BLK).i32c(-1).ne().if_();
        g.depth += 1;
        g.c.get(L_BLK).i32c(lo as i32).sub();
    } else {
        g.c.get(L_BLK);
    }
    g.c.op(0x0e);
    wasm::uleb(&mut g.c.b, (hi - lo + 1) as u64);
    let mut t = 0;
    for m in lo..=hi {
        while items[t].range().1 < m {
            t += 1;
        }
        wasm::uleb(&mut g.c.b, (g.depth - label[t]) as u64);
    }
    wasm::uleb(&mut g.c.b, (g.depth - label[0]) as u64);
    if side.is_some() {
        g.c.end();
        g.depth -= 1;
    }
    for (t, &item) in items.iter().enumerate() {
        g.c.end();
        g.depth -= 1;
        match item {
            Item::Block(k) => {
                g.fwd[k] = None;
                emit_block(g, blocks, k, count);
            }
            Item::Loop(h, e) => {
                g.fwd[h] = None;
                // a loop with x87 code: emitted to see the caches its jumps come with, as many
                // times as these change (each block starting with the cache it was jumped to with
                // the time before), then for real with those
                if g.fc_range.is_none() && e - h < 48 && (h..=e).any(|k| blocks[k].1.iter().any(fc_handles)) {
                    let b0 = g.c.b.len();
                    let mut fin = BTreeMap::new();
                    g.fc_range = Some((h, e));
                    for _ in 0..8 {
                        g.fc_in = fin.clone();
                        g.fc_out.clear();
                        emit_loop(g, blocks, loops, h, e, &mut 0);
                        g.c.truncate(b0);
                        let out: BTreeMap<usize, FCache> = g.fc_out.iter().filter(|f| f.1.on).map(|(&k, &f)| (k, f)).collect();
                        if out == fin {
                            break;
                        }
                        fin = out;
                    }
                    if LAYOUT.load(core::sync::atomic::Ordering::Relaxed) {
                        use core::fmt::Write;
                        for (k, f) in &fin {
                            let _ = writeln!(g.notes, "FC {h}..{e} {k}: d{} have {:02x} dirty {:02x} valid {:02x} empty {:02x}", f.delta, f.have, f.dirty, f.valid, f.empty);
                        }
                    }
                    if !fin.is_empty() {
                        // ways in: L_BLK -1 (plain, to h) or the block (the side dispatch)
                        let mut groups: Vec<(u8, Vec<usize>)> = Vec::new();
                        for (&k, f) in &fin {
                            match groups.iter_mut().find(|g| g.0 == f.have) {
                                Some(g) => g.1.push(k),
                                None => groups.push((f.have, alloc::vec![k])),
                            }
                        }
                        for (have, ks) in groups {
                            for (i, &k) in ks.iter().enumerate() {
                                g.c.get(L_BLK).i32c(k as i32).eq();
                                if k == h {
                                    g.c.get(L_BLK).i32c(-1).eq().or();
                                }
                                if i > 0 {
                                    g.c.or();
                                }
                            }
                            g.c.if_();
                            g.c.get(L_CPU).load32(Off::ftop()).set(L_FTOP);
                            g.fc_load(have);
                            g.c.end();
                        }
                        g.fc_in = fin;
                        emit_loop(g, blocks, loops, h, e, count);
                    }
                    g.fc_range = None;
                    g.fc_in.clear();
                    if g.c.b.len() != b0 {
                        continue;
                    }
                }
                let outer = g.slots.is_none();
                if outer {
                    g.slots = slot_loop(&blocks[h..=e]);
                    g.slots_load();
                }
                let m = if h == e { mm_names(&blocks[h].1) } else { 0 };
                for i in 0..8 {
                    if m & (1 << i) != 0 {
                        g.c.get(L_CPU).v128load64(Off::mm(Register::MM0 + i)).set(L_MMX + i);
                    }
                }
                g.mm_pre = (m != 0).then_some((h, m));
                let x = if h == e { xmm_names(&blocks[h].1) } else { 0 };
                for i in 0..8 {
                    if x & (1 << i) != 0 {
                        g.c.get(L_CPU).v128load(Off::xmm(Register::XMM0 + i)).set(L_XMM + i);
                    }
                }
                g.xm_pre = (x != 0).then_some((h, x));
                emit_loop(g, blocks, loops, h, e, count);
                g.mm_pre = None;
                g.xm_pre = None;
                if outer {
                    g.slots = None;
                }
            }
        }
        debug_assert_eq!(g.depth + 1, label[t]);
    }
}

/// The displacement of operand `i` if it is a 32-bit [base + disp] slot.
fn slot_operand(ins: &Instruction, i: u32, base: Register) -> Option<i32> {
    (ins.op_kind(i) == OpKind::Memory
        && ins.memory_base() == base
        && ins.memory_index() == Register::None
        && matches!(ins.memory_segment(), Register::SS | Register::DS)
        && ins.memory_size().size() == 4)
        .then(|| ins.memory_displacement32() as i32)
}

/// The stack slots a loop keeps in locals (Gen::slots): the most used 32-bit [EBP + d] (or
/// [ESP + d]) of a loop of plain integer instructions that doesn't write that register (so
/// every store goes through Gen::write_op, and nothing outside the loop's code runs).
fn slot_loop(blocks: &[(u32, Vec<Instruction>)]) -> Option<(Register, Vec<i32>)> {
    use Mnemonic::*;
    let all = || blocks.iter().flat_map(|b| b.1.iter());
    for ins in all() {
        let plain = matches!(
            ins.mnemonic(),
            Mov | Movzx | Movsx | Add | Sub | Adc | Sbb | And | Or | Xor | Cmp | Test | Inc | Dec | Neg | Not | Lea | Shl | Shr | Sar | Rol | Ror | Imul | Mul | Div | Idiv | Cdq | Nop
        ) || alloc::format!("{:?}", ins.mnemonic()).starts_with("Set")
            || alloc::format!("{:?}", ins.mnemonic()).starts_with("Cmov")
            || matches!(ins.flow_control(), FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch) && ins.op0_kind() == OpKind::NearBranch32;
        if !plain || ins.has_lock_prefix() || ins.has_rep_prefix() || ins.has_repne_prefix() {
            return None;
        }
    }
    let writes = |r: Register| all().any(|ins| ins.op_count() > 0 && ins.op0_kind() == OpKind::Register && ins.op0_register().full_register32() == r && !matches!(ins.mnemonic(), Cmp | Test) && ins.flow_control() == FlowControl::Next);
    let base = [Register::EBP, Register::ESP].into_iter().find(|&r| !writes(r))?;
    let mut uses: BTreeMap<i32, u32> = BTreeMap::new();
    for ins in all() {
        for i in 0..ins.op_count() {
            if let Some(d) = slot_operand(ins, i, base) {
                *uses.entry(d).or_default() += 1;
            }
        }
    }
    let mut list: Vec<(i32, u32)> = uses.into_iter().filter(|u| u.1 >= 2).collect();
    list.sort_by_key(|u| core::cmp::Reverse(u.1));
    list.truncate(SLOTS_MAX.load(core::sync::atomic::Ordering::Relaxed) as usize);
    (!list.is_empty()).then(|| (base, list.into_iter().map(|u| u.0).collect()))
}

fn emit_loop<'a>(g: &mut Gen<'a>, blocks: &'a [(u32, Vec<Instruction>)], loops: &BTreeMap<usize, usize>, h: usize, e: usize, count: &mut usize) {
    g.c.loop_();
    g.depth += 1;
    g.back[h] = Some(g.depth);
    let mut inner = alloc::vec![Item::Block(h)];
    if h < e {
        inner.extend(level(h + 1, e, loops));
    }
    emit_level(g, blocks, loops, &inner, Some((h, e)), count);
    g.back[h] = None;
    g.c.end();
    g.depth -= 1;
}

/// The XMM registers a block names, or 0 if it names them in anything but what the XMM cache
/// takes (`xmm_cached`).
fn xmm_names(list: &[Instruction]) -> u8 {
    let mut m = 0u8;
    for ins in list {
        for o in 0..ins.op_count() {
            if ins.op_kind(o) == OpKind::Register && ins.op_register(o).is_xmm() {
                if !xmm_cached(ins) {
                    return 0;
                }
                m |= 1 << (ins.op_register(o) as u32 - Register::XMM0 as u32);
            }
        }
    }
    m
}

/// The MMX registers a block names, or 0 if it also has x87 instructions (which share them).
fn mm_names(list: &[Instruction]) -> u8 {
    let mut m = 0u8;
    for ins in list {
        use iced_x86::CpuidFeature::*;
        if ins.cpuid_features().iter().any(|f| matches!(f, FPU | FPU287 | FPU387 | FXSR)) || ins.mnemonic() == Mnemonic::Emms {
            return 0;
        }
        for o in 0..ins.op_count() {
            if ins.op_kind(o) == OpKind::Register && ins.op_register(o).is_mm() {
                m |= 1 << (ins.op_register(o) as u32 - Register::MM0 as u32);
            }
        }
    }
    m
}

/// What `ins` leaves in [`Gen::small`].
fn small_after(small: &mut [Option<u32>; 8], ins: &Instruction) {
    let mut info = iced_x86::InstructionInfoFactory::new();
    for u in info.info(ins).used_registers() {
        use iced_x86::OpAccess::*;
        if matches!(u.access(), Write | CondWrite | ReadWrite | ReadCondWrite) && u.register().is_gpr() {
            small[u.register().full_register32().number()] = Option::None;
        }
    }
    if ins.op_count() == 2 && ins.op_kind(0) == OpKind::Register && ins.op_register(0).is_gpr32() {
        let r = ins.op_register(0).number();
        let m = match ins.mnemonic() {
            Mnemonic::And if matches!(ins.op_kind(1), OpKind::Immediate32 | OpKind::Immediate8to32) && (ins.immediate32() as i32) >= 0 => Some(ins.immediate32()),
            Mnemonic::Movzx if op_bits(ins, 1) == 8 => Some(0xff),
            Mnemonic::Movzx if op_bits(ins, 1) == 16 => Some(0xffff),
            _ => None,
        };
        if m.is_some() {
            small[r] = m;
        }
    }
}

fn emit_block<'a>(g: &mut Gen<'a>, blocks: &'a [(u32, Vec<Instruction>)], k: usize, count: &mut usize) {
    let list = &blocks[k].1;
    g.block_index = k as u32;
    g.ctx = g.ctxs[k];
    g.safe = [None; 8];
    g.safe2.clear();
    g.safe[4] = Some(0); // see Gen::emit
    g.flags = Flags::Entry;
    g.smc = WATCH.get().is_some_and(|w| w(blocks[k].0));
    g.dirty = g.dirty_in.get(k).copied().unwrap_or(g.wregs);
    g.small = [None; 8];
    if let Some(&m) = g.safe_in.get(k) {
        for r in (0..8).filter(|r| m & (1 << r) != 0) {
            g.safe[r] = Some(0);
        }
    }
    g.fc = g.fc_in.get(&k).copied().unwrap_or_default();
    bump(&mut g.c, 14);
    if let Some(&at) = g.prof.get(k) {
        g.c.i32c(at as i32).i32c(at as i32).load32(0).i32c(1).add().store32(0);
    }
    g.mm_loop = match g.mm_pre {
        Some((h, m)) if h == k => m,
        _ => 0,
    };
    g.mmx = (g.mm_loop, g.mm_loop);
    g.xm_loop = match g.xm_pre {
        Some((h, m)) if h == k => m,
        _ => 0,
    };
    g.xmm = (g.xm_loop, g.xm_loop);
    let (live, live_in) = liveness(list, g.live_out[k]);
    if live_in == 0 {
        // dead here: tell V8 so (the lazy state is not kept in registers through the block)
        g.c.i32c(-1).set(L_OP);
        for l in L_FA..=L_FC {
            g.c.i32c(0).set(l);
        }
    }
    if let Some(&h) = g.hle.get(&blocks[k].0) {
        g.ins = &list[0];
        g.hle(h);
    }
    let m = list.len();
    let mut skip = false;
    for (i, ins) in list.iter().enumerate() {
        if i > 0 {
            small_after(&mut g.small, &list[i - 1]);
        }
        if core::mem::take(&mut skip) {
            continue;
        }
        g.fuse_zx = i + 2 < m && set_movzx(ins, &list[i + 1]);
        skip = g.fuse_zx;
        g.ins = ins;
        g.live_after = live[i];
        *count += 1;
        let b0 = g.c.b.len();
        g.fell_back = false;
        let go = g.emit(i + 1 == m);
        if LAYOUT.load(core::sync::atomic::Ordering::Relaxed) {
            let k = alloc::format!("{:?}{}", ins.mnemonic(), if g.fell_back { "*" } else { "" });
            let e = g.sizes.entry(k).or_insert((0, 0));
            e.0 += 1;
            e.1 += (g.c.b.len() - b0) as u32;
        }
        if !go {
            break;
        }
    }
    if let Some(o) = g.flags_out.get_mut(k) {
        *o = match g.flags {
            Flags::Known(op) => Some(op),
            Flags::Entry => None,
        };
    }
}

/// What [`Gen::x87_flush`] does, for a block leaving with its x87 cache (`info`: dirty slots,
/// valid << 8, empty << 16, TOP change << 24; `top`: L_FTOP; `v`: the slots' locals).
pub fn x87_out(f: &mut crate::Fpu, top: u32, info: u32, v: [f64; 8]) {
    let (dirty, valid, empty, delta) = (info as u8, (info >> 8) as u8, (info >> 16) as u8, (info >> 24) & 7);
    for r in 0..8 {
        if dirty & (1 << r) != 0 {
            f.regs[((top + r) & 7) as usize] = v[r as usize];
        }
    }
    if valid | empty != 0 {
        let rot = |m: u8| ((m as u32 * 0x101) << (top & 31) >> 8) as u8;
        f.empty = f.empty & !rot(valid) | rot(empty);
        f.exact &= !rot(valid | empty);
    }
    if delta != 0 {
        f.top = (top + delta) & 7;
    }
}

/// SETcc of a low byte register followed by MOVZX of it into its own 32-bit register.
fn set_movzx(a: &Instruction, b: &Instruction) -> bool {
    a.condition_code() != ConditionCode::None
        && a.op_count() == 1
        && a.flow_control() == FlowControl::Next
        && a.op0_kind() == OpKind::Register
        && matches!(a.op0_register(), Register::AL | Register::CL | Register::DL | Register::BL)
        && b.mnemonic() == Mnemonic::Movzx
        && b.op0_kind() == OpKind::Register
        && b.op1_kind() == OpKind::Register
        && b.op1_register() == a.op0_register()
        && b.op0_register().is_gpr32()
        && b.op0_register().number() == a.op0_register().number()
}

fn successors(last: &Instruction) -> Vec<u32> {
    let mut next = Vec::new();
    let direct = matches!(last.op0_kind(), OpKind::NearBranch32);
    match last.flow_control() {
        FlowControl::ConditionalBranch | FlowControl::Call if direct => {
            next.push(last.near_branch32());
            next.push(last.next_ip32());
        }
        FlowControl::UnconditionalBranch if direct => next.push(last.near_branch32()),
        FlowControl::Next | FlowControl::IndirectCall | FlowControl::Call | FlowControl::ConditionalBranch => {
            next.push(last.next_ip32())
        }
        _ => {}
    }
    next
}

/// Liveness of the status flags after each instruction, given those live at the end; and
/// those live at the start.
fn liveness(list: &[Instruction], out: u32) -> (Vec<u32>, u32) {
    let mut live = alloc::vec![0u32; list.len()];
    let mut l = out;
    for (i, ins) in list.iter().enumerate().rev() {
        live[i] = l;
        let written = ins.rflags_modified() & STATUS_BITS;
        let mut read = ins.rflags_read() & STATUS_BITS;
        let by_cl = (0..ins.op_count()).any(|o| ins.op_kind(o) == OpKind::Register && ins.op_register(o) == Register::CL);
        // a shift by 0 or a REP with ECX 0 keeps them: those live after stay live
        let keeps = by_cl || ins.has_rep_prefix() || ins.has_repne_prefix();
        if ins.mnemonic() == Mnemonic::Inc || ins.mnemonic() == Mnemonic::Dec {
            read |= RflagsBits::CF; // the lazy INC/DEC carries CF along
        }
        if ins.condition_code() != ConditionCode::None {
            read |= cc_reads(cc_index(ins.condition_code()));
        }
        l = if keeps { l | read } else { (l & !written) | read };
    }
    (live, l)
}

/// Status flags live at the start and at the end of each block, over the region's jumps
/// (everything is live where it is left or jumps dynamically).
fn block_liveness(g: &Gen, blocks: &[(u32, Vec<Instruction>)], tables: &[structure::Table]) -> (Vec<u32>, Vec<u32>) {
    let n = blocks.len();
    let succ: Vec<Option<Vec<usize>>> = (0..n)
        .map(|k| {
            let last = blocks[k].1.last().unwrap();
            let ctx = g.ctxs[k];
            let find = |a: u32| g.targets.get(&(ctx, a)).or_else(|| g.targets.get(&(0, a))).map(|&b| b as usize);
            // a function's call (not inlined) goes on at the return address with the flags
            // the callee left, or those from before when it kept them (some hand-written code
            // returns a result in ZF): the callee stores them at its RET, which can return to
            // any caller (everything live there)
            if g.fnm && matches!(last.flow_control(), FlowControl::Call | FlowControl::IndirectCall) && !g.inlined.contains_key(&k) {
                return find(last.next_ip32()).map(|b| alloc::vec![b]);
            }
            // a jump table's dispatch: to its entries (compilers keep no flags across one)
            if let Some(&d) = g.jnode.get(&k) {
                let t = &tables[d - blocks.len()];
                return t.entries.iter().map(|&a| g.targets.get(&(t.ctx, a)).or_else(|| g.targets.get(&(0, a))).map(|&b| b as usize)).collect();
            }
            let direct = last.op0_kind() == OpKind::NearBranch32 || last.flow_control() == FlowControl::Next;
            if !direct {
                return None;
            }
            match last.flow_control() {
                FlowControl::Call => match g.inlined.get(&k) {
                    Some(&(entry, _)) => Some(alloc::vec![entry]),
                    None => find(last.near_branch32()).map(|b| alloc::vec![b]),
                },
                FlowControl::Next | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch => {
                    successors(last).into_iter().map(find).collect()
                }
                _ => None,
            }
        })
        .collect();
    let mut live_in = alloc::vec![0u32; n];
    let mut live_out = alloc::vec![0u32; n];
    loop {
        let mut changed = false;
        for k in (0..n).rev() {
            let out = match &succ[k] {
                Some(s) => s.iter().fold(0, |a, &b| a | live_in[b]),
                None => STATUS_BITS,
            };
            live_out[k] = out;
            let l = liveness(&blocks[k].1, out).1;
            if l != live_in[k] {
                live_in[k] = l;
                changed = true;
            }
        }
        if !changed {
            return (live_in, live_out);
        }
    }
}

/// A function's registers that can be dirty (written since loaded from the Cpu) at each
/// block's start: its entry loads them all (none), a block passes on what came in and what it
/// writes, and a call made by [`Gen::call_fn`] none (it loads them after) but into a callee
/// inlined behind its guard.
fn dirty_in(g: &Gen, blocks: &[(u32, Vec<Instruction>)], l: &structure::Layout) -> Vec<u8> {
    let n = blocks.len();
    let total = l.succ.len();
    let mut info = iced_x86::InstructionInfoFactory::new();
    let writes: Vec<u8> = (0..n)
        .map(|k| {
            if g.hle.contains_key(&blocks[k].0) {
                return g.wregs;
            }
            let mut w = 0u8;
            for ins in &blocks[k].1 {
                for u in info.info(ins).used_registers() {
                    use iced_x86::OpAccess::*;
                    if matches!(u.access(), Write | CondWrite | ReadWrite | ReadCondWrite) && u.register().is_gpr() {
                        w |= 1 << u.register().full_register32().number();
                    }
                }
            }
            w & g.wregs
        })
        .collect();
    // blocks ending in a call_fn (and the entry of a callee inlined there)
    let calls: Vec<Option<Option<usize>>> = (0..n)
        .map(|k| {
            let last = blocks[k].1.last().unwrap();
            if last.mnemonic() != Mnemonic::Call {
                return None;
            }
            let inl = g.inlined.get(&k);
            if last.op0_kind() == OpKind::NearBranch32 {
                return if inl.is_some() { None } else { Some(None) };
            }
            let ok = op_bits(last, 0) == 32 && (0..last.op_count()).all(|i| simple_operand(last, i)) && !last.has_lock_prefix();
            ok.then(|| inl.map(|&(e, _)| e))
        })
        .collect();
    let mut din = alloc::vec![0u8; total];
    loop {
        let mut changed = false;
        for v in 0..total {
            let out = if v < n { din[v] | writes[v] } else { din[v] };
            for &w in &l.succ[v] {
                let o = match calls.get(v) {
                    Some(Some(e)) if *e != Some(w) => 0,
                    _ => out,
                };
                if din[w] | o != din[w] {
                    din[w] |= o;
                    changed = true;
                }
            }
        }
        if !changed {
            return din;
        }
    }
}

/// A function's registers (but ESP, see Gen::emit) known to be in the guest range with
/// SAFE_MARGIN to spare at each block's start: checked by an access [r + small] since last
/// written, on every way there. None at the entries, nor after a call (a callee could change
/// any).
///
/// Loops (D.1 of docs/v8-wasm-shape.md; V8 has no LICM): a register the loop's first block
/// uses as a base before writing it, that nothing in the loop writes (no calls in it), is
/// checked once on the way in (`hoist`), and is safe in the whole loop.
fn safe_in(g: &Gen, blocks: &[(u32, Vec<Instruction>)], l: &structure::Layout) -> (Vec<u8>, BTreeMap<usize, (u8, u32)>) {
    if nocheck() {
        return (Vec::new(), BTreeMap::new());
    }
    let n = blocks.len();
    let total = l.succ.len();
    let mut info = iced_x86::InstructionInfoFactory::new();
    // per block: (registers it leaves checked, registers it writes without checking after)
    let tr: Vec<(u8, u8)> = (0..n)
        .map(|k| {
            if g.hle.contains_key(&blocks[k].0) {
                return (0, 0xff);
            }
            let (mut gen, mut kill) = (0u8, 0u8);
            for ins in &blocks[k].1 {
                let base = ins.memory_base();
                let checks = (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory)
                    && !matches!(ins.mnemonic(), Mnemonic::Lea | Mnemonic::Nop | Mnemonic::Xchg)
                    && !ins.has_lock_prefix()
                    && base.is_gpr32()
                    && base != Register::ESP
                    && ins.memory_index() == Register::None
                    && !matches!(ins.memory_segment(), Register::FS | Register::GS)
                    && (ins.memory_displacement32() as i32).unsigned_abs() < SAFE_MARGIN as u32 / 2;
                if checks {
                    gen |= 1 << base.number();
                    kill &= !(1 << base.number());
                }
                for u in info.info(ins).used_registers() {
                    use iced_x86::OpAccess::*;
                    if matches!(u.access(), Write | CondWrite | ReadWrite | ReadCondWrite) && u.register().is_gpr() {
                        let b = 1u8 << u.register().full_register32().number();
                        gen &= !b;
                        kill |= b;
                    }
                }
            }
            (gen & !(1 << 4), kill)
        })
        .collect();
    let check_base = |ins: &Instruction| {
        let base = ins.memory_base();
        (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory)
            && !matches!(ins.mnemonic(), Mnemonic::Lea | Mnemonic::Nop | Mnemonic::Xchg)
            && !ins.has_lock_prefix()
            && base.is_gpr32()
            && base != Register::ESP
            && ins.memory_index() == Register::None
            && !matches!(ins.memory_segment(), Register::FS | Register::GS)
            && (ins.memory_displacement32() as i32).unsigned_abs() < SAFE_MARGIN as u32 / 2
    };
    let mut hoist: BTreeMap<usize, (u8, Vec<bool>)> = BTreeMap::new();
    if HOIST.load(core::sync::atomic::Ordering::Relaxed) {
        for (h, body) in l.loop_bodies() {
            if h >= n {
                continue;
            }
            // read as a base in the first block before it writes them
            let (mut cand, mut wrote) = (0u8, 0u8);
            for ins in &blocks[h].1 {
                if check_base(ins) && wrote & (1 << ins.memory_base().number()) == 0 {
                    cand |= 1 << ins.memory_base().number();
                }
                for u in info.info(ins).used_registers() {
                    use iced_x86::OpAccess::*;
                    if matches!(u.access(), Write | CondWrite | ReadWrite | ReadCondWrite) && u.register().is_gpr() {
                        wrote |= 1 << u.register().full_register32().number();
                    }
                }
            }
            cand &= !(1 << 4);
            for v in (0..n).filter(|&v| body[v]) {
                let last = blocks[v].1.last().unwrap();
                let call = matches!(last.flow_control(), FlowControl::Call | FlowControl::IndirectCall) && !g.inlined.contains_key(&v);
                let leaves_callee = g.ctxs[v] != 0 && l.succ[v].iter().any(|&w| w < n && g.ctxs[w] == 0);
                if call || leaves_callee || g.hle.contains_key(&blocks[v].0) {
                    cand = 0;
                }
                cand &= !tr[v].1;
            }
            if cand != 0 {
                hoist.insert(h, (cand, body));
            }
        }
    }
    let mut sin = alloc::vec![0xffu8; total];
    // (entries: the function's first node, else the root)
    sin[l.entry()] = 0;
    loop {
        let mut changed = false;
        for &v in l.order_iter() {
            let out = if v < n { (sin[v] & !tr[v].1) | tr[v].0 } else { sin[v] };
            let call = v < n && matches!(blocks[v].1.last().unwrap().flow_control(), FlowControl::Call | FlowControl::IndirectCall);
            let inl = g.inlined.get(&v).map(|x| x.0);
            for &w in &l.succ[v] {
                // (out of an inlined callee: maybe by call_rest, which loads them again)
                let mut o = if (call && inl != Some(w)) || (v < n && w < n && g.ctxs[v] != 0 && g.ctxs[w] == 0) { 0 } else { out };
                // (into a loop: through its hoisted checks)
                if let Some((m, body)) = hoist.get(&w) {
                    if !body[v] {
                        o |= m;
                    }
                }
                if sin[w] & o != sin[w] {
                    sin[w] &= o;
                    changed = true;
                }
            }
        }
        if !changed {
            return (sin, hoist.into_iter().map(|(h, (m, _))| (h, (m, blocks[h].0))).collect());
        }
    }
}

/// Hoisting range checks out of loops (VNVM_JIT_HOIST=0: off; see [`safe_in`]).
pub static HOIST: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Adds a region starting at `roots` to `module` (one function, exported as "f<n>"),
/// taking at most `max_blocks` blocks whose start `accept` agrees to. None if not even the
/// entry block can be translated.
pub fn translate_region<M: Memory>(module: &mut Module, mem: &mut M, roots: &[u32], max_blocks: usize, accept: impl FnMut(u32) -> bool) -> Option<Region> {
    translate(module, mem, roots, max_blocks, accept, None, &|_| None, &|_| false)
}

/// Adds the function at `entry` (see [`FN`]) to `module`: its adapter for the region calling
/// convention ("f<n>", entered by the dispatcher and by regions' jumps), then the function
/// ("f<n+1>"), entered at any of `starts` (table index | block << ENTRY_SHIFT | FN_TAG).
/// Small callees are inlined whether they ran or not (if outside `trap`).
/// `hints` gives the target an indirect call or jump at an address mostly goes to (a small one
/// is inlined behind a check).
pub fn translate_function<M: Memory>(module: &mut Module, mem: &mut M, entry: u32, max_blocks: usize, trap: (u32, u32), hints: &dyn Fn(u32) -> Option<u32>, ran: &dyn Fn(u32) -> bool, accept: impl FnMut(u32) -> bool) -> Option<Region> {
    translate(module, mem, &[entry], max_blocks, accept, Some(trap), hints, ran)
}

fn translate<M: Memory>(
    module: &mut Module,
    mem: &mut M,
    roots: &[u32],
    max_blocks: usize,
    mut accept: impl FnMut(u32) -> bool,
    fn_trap: Option<(u32, u32)>,
    hints: &dyn Fn(u32) -> Option<u32>,
    ran: &dyn Fn(u32) -> bool,
) -> Option<Region> {
    let fnm = fn_trap.is_some();
    // blocks of small callees to inline (functions)
    let mut forced: Vec<u32> = Vec::new();
    let mut blocks: Vec<(u32, Vec<Instruction>)> = Vec::new();
    let mut targets = BTreeMap::new();
    let mut queue: VecDeque<u32> = roots.iter().copied().collect();
    let mut total = 0;
    let mut hle = BTreeMap::new();
    let mut thunks = BTreeMap::new();
    while let Some(a) = queue.pop_front() {
        let max_ins = if fnm { FN_MAX_INS.load(core::sync::atomic::Ordering::Relaxed) } else { MAX_REGION_INS };
        if blocks.len() >= max_blocks.min(MAX_BLOCKS) || total >= max_ins {
            break;
        }
        if targets.contains_key(&a) || !(GUEST_LO..GUEST_HI).contains(&a) || !(forced.contains(&a) || accept(a)) || DENY.get().is_some_and(|d| d(a)) {
            continue;
        }
        let list = match hle_at(mem, a) {
            Some(h) => {
                hle.insert(a, h);
                alloc::vec![ret_at(a)]
            }
            None => decode(mem, a),
        };
        let Some(last) = list.last() else { continue };
        if fn_on() && matches!(last.flow_control(), FlowControl::Call | FlowControl::IndirectCall) {
            queue.push_back(last.next_ip32());
            if fnm && last.op0_kind() == OpKind::NearBranch32 {
                let f = last.near_branch32();
                if let Some(j) = decode(mem, f).first().filter(|j| j.mnemonic() == Mnemonic::Jmp && j.op0_kind() == OpKind::Memory && j.memory_base() == Register::None && j.memory_index() == Register::None && j.memory_segment() == Register::DS) {
                    thunks.insert(f, j.memory_displacement32());
                }
            }
            // a small callee comes along to be inlined (see `inline` below)
            let f = if last.op0_kind() == OpKind::NearBranch32 { Some(last.near_branch32()) } else { hints(last.ip32()) };
            if let (Some(trap), Some(f)) = (fn_trap, f) {
                if let Some(b) = small_callee(mem, f, trap, hints).or_else(|| hot_callee(mem, f, trap, ran)) {
                    // (all of them: a wrapper's tail jump has no static successor)
                    queue.extend(b.iter().copied());
                    forced.extend(b);
                }
            }
        } else {
            queue.extend(successors(last));
        }
        // `jmp [index*4 + table]` (switch statements, interpreters' opcode dispatch): the
        // table's targets that ran join the region, so the jump stays inside it
        let table = if fnm { structure::block_table(&list).map(|t| t.0) } else { structure::jump_table(last) };
        if let Some(t) = table {
            let near = last.ip32();
            for i in 0..1024u32 {
                let mut b = [0u8; 4];
                if mem.fetch(t.wrapping_add(4 * i), &mut b) < 4 {
                    break;
                }
                let v = u32::from_le_bytes(b);
                if v.abs_diff(near) > 0x10_0000 {
                    break;
                }
                queue.push_back(v);
            }
        }
        total += list.len();
        targets.insert(a, blocks.len() as u32);
        blocks.push((a, list));
    }
    if blocks.is_empty() {
        return None;
    }
    let code = blocks
        .iter()
        .map(|(a, list)| {
            let last = list.last().unwrap();
            // a block cut short ends at bytes that did not decode
            let end = last.next_ip32() + if last.flow_control() == FlowControl::Next { 15 } else { 0 };
            (*a, end.wrapping_sub(*a))
        })
        .collect();
    // address order: guest loops become contiguous ranges
    blocks.sort_unstable_by_key(|b| b.0);
    rotate_loops(&mut blocks);
    hoist_entries(&mut blocks);
    for (i, b) in blocks.iter().enumerate() {
        targets.insert(b.0, i as u32);
    }
    // small callees get a copy after each call (an indirect one checking for the target the
    // region was merged for), so the call and return are plain jumps inside a loop
    // (a wrapper's tail jump goes to one of the other roots: the first that fits)
    // where an indirect call or jump goes: for functions the hints, for regions the roots
    let cands: Vec<u32> = if fnm { blocks.iter().filter_map(|b| hints(b.1.last().unwrap().ip32())).collect() } else { roots.to_vec() };
    let inline = |f: u32| {
        cands.iter().filter(|&&h| h != f).find_map(|&h| callee(&blocks, &targets, f, Some(h), fnm).filter(|b| b.1.is_some()).map(|b| (b, Some(h)))).or_else(|| callee(&blocks, &targets, f, None, fnm).map(|b| (b, None)))
    };
    let mut order = Vec::new();
    let mut ctxs = Vec::new();
    let mut ret_to = alloc::vec![0];
    let mut inlined = BTreeMap::new();
    let mut extra = 0;
    for i in 0..blocks.len() {
        let call = order.len();
        order.push(i);
        ctxs.push(0);
        let last = blocks[i].1.last().unwrap();
        let found = match last.flow_control() {
            FlowControl::Call if last.op0_kind() == OpKind::NearBranch32 => {
                let f = last.near_branch32();
                inline(f).map(|(b, h)| (f, None, b, h))
            }
            // the target of an indirect call: one of the roots merged in
            // (a wrapper that tail-jumps to the latest root first: qsort calls its comparison
            // through one, and the latest root is then what the wrapper jumps to)
            FlowControl::IndirectCall if fnm => hints(last.ip32()).and_then(|f| inline(f).map(|(b, h)| (f, Some(f), b, h))),
            FlowControl::IndirectCall => {
                let found: Vec<_> = roots.iter().rev().filter_map(|&f| inline(f).map(|(b, h)| (f, Some(f), b, h))).collect();
                let pick = found.iter().position(|c| c.2 .1.is_some()).unwrap_or(0);
                found.into_iter().nth(pick)
            }
            _ => None,
        };
        let Some((f, guard, (body, tail), hint)) = found else { continue };
        let size: usize = body.iter().map(|&b| blocks[b].1.len()).sum();
        if extra + size > MAX_INLINE_TOTAL {
            continue;
        }
        extra += size;
        let entry = order.len() + body.iter().position(|&b| blocks[b].0 == f).unwrap();
        inlined.insert(call, (entry, guard));
        if let (Some(t), Some(h)) = (tail, hint) {
            let at = |b| order.len() + body.iter().position(|&x| x == b).unwrap();
            inlined.insert(at(t), (at(targets[&h] as usize), Some(h)));
        }
        for b in body {
            order.push(b);
            ctxs.push(ret_to.len() as u32);
        }
        ret_to.push(last.next_ip32());
    }
    let mut starts = Vec::new();
    let mut targets = BTreeMap::new();
    for (k, &b) in order.iter().enumerate() {
        targets.insert((ctxs[k], blocks[b].0), k as u32);
        if ctxs[k] == 0 && (!fnm || blocks[b].0 == roots[0]) {
            starts.push((blocks[b].0, k as u32));
        }
    }
    let blocks: Vec<(u32, Vec<Instruction>)> = order.iter().map(|&b| blocks[b].clone()).collect();
    if module.types.is_empty() {
        module.types.push(FuncType { params: alloc::vec![I32], results: alloc::vec![I32] });
        module.types.push(FuncType { params: alloc::vec![I32; 5], results: alloc::vec![I32] });
        module.types.push(FuncType { params: alloc::vec![I32; N_PARAMS as usize], results: alloc::vec![I32; N_PARAMS as usize] });
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
    // (in every module: saved bundles merge regions and functions, which needs the same
    // types and imports)
    if module.types.len() == T_FN as usize {
        // T_FN: (cpu, depth | entry << 16, esp) -> (status, esp): ESP in V8's registers both ways
        module.types.push(FuncType { params: alloc::vec![I32; 3], results: alloc::vec![I32; 2] });
        module.types.push(FuncType { params: alloc::vec![I32; 3], results: alloc::vec![I32] });
        module.imports.push(Import::Func("callmiss", T_CALLMISS));
    }

    let n = blocks.len() as u32;
    let mut returns: Vec<u32> = blocks
        .iter()
        .zip(&ctxs)
        .filter(|(_, &c)| c == 0)
        .filter_map(|((_, l), _)| l.last())
        .filter(|l| l.flow_control() == FlowControl::Call || l.flow_control() == FlowControl::IndirectCall)
        .map(|l| l.next_ip32())
        .filter(|&a| targets.contains_key(&(0, a)))
        .collect();
    returns.sort_unstable();
    returns.dedup();
    if returns.len() > 4 {
        returns.clear();
    }
    let loops = find_loops(&blocks, &ctxs, &targets, &ret_to);
    let mut head = alloc::vec![false; n as usize];
    for &h in loops.keys() {
        head[h] = true;
    }
    // a function's jump tables: one dispatch node each
    // (by the target if one of its jumps has the index no more)
    let mut tables: Vec<structure::Table> = Vec::new();
    let mut jtab = BTreeMap::new();
    let mut jnode = BTreeMap::new();
    if fnm {
        for (k, (_, list)) in blocks.iter().enumerate() {
            let last = list.last().unwrap();
            if let Some((t, by_index)) = structure::block_table(list) {
                let d = *jtab.entry((ctxs[k], t)).or_insert_with(|| {
                    tables.push(structure::Table { ctx: ctxs[k], entries: structure::table_entries(mem, t, last.ip32()), hash: None });
                    n as usize + tables.len() - 1
                });
                jnode.insert(k, d);
                let tab = &mut tables[d - n as usize];
                if !by_index && tab.hash.is_none() {
                    tab.entries.sort_unstable();
                    tab.entries.dedup();
                    tab.hash = Some(structure::perfect_hash(&tab.entries));
                }
            }
        }
    }
    let nodes = n as usize + tables.len() + 1; // (+ a function's node for more entries)
    let (mut regs, mut wregs) = (0xffu8, 0xffu8);
    if fnm {
        // (an HLE'd C function returns in EAX)
        (regs, wregs) = if hle.is_empty() { (1 << 4, 1 << 4) } else { (0x11, 0x11) };
        let mut info = iced_x86::InstructionInfoFactory::new();
        for ins in blocks.iter().flat_map(|b| b.1.iter()) {
            for u in info.info(ins).used_registers() {
                use iced_x86::OpAccess::*;
                let r = u.register();
                if r.is_gpr() {
                    let b = 1u8 << r.full_register32().number();
                    regs |= b;
                    if matches!(u.access(), Write | CondWrite | ReadWrite | ReadCondWrite) {
                        wregs |= b;
                    }
                }
            }
        }
    }
    let mut g = Gen {
        c: Code::default(),
        fnm,
        structured: fnm,
        jnode,
        regs,
        wregs,
        depth: 0,
        flags: Flags::Entry,
        targets: &targets,
        block_index: 0,
        ctx: 0,
        ctxs: &ctxs,
        ret_to: &ret_to,
        inlined: &inlined,
        live_out: Vec::new(),
        loops: &loops,
        fwd: alloc::vec![None; nodes],
        back: alloc::vec![None; nodes],
        head,
        returns: &returns,
        ins: &blocks[0].1[0],
        live_after: 0,
        dirty: 0xff,
        dirty_in: Vec::new(),
        small: [None; 8],
        safe_in: Vec::new(),
        hoist: BTreeMap::new(),
        preds: Vec::new(),
        flags_out: Vec::new(),
        smc: false,
        temps: 0,
        safe: [None; 8],
        safe2: Vec::new(),
        fc: FCache::default(),
        mmx: (0, 0),
        xmm: (0, 0),
        folded: None,
        mm_pre: None,
        mm_loop: 0,
        xm_pre: None,
        xm_loop: 0,
        slots: None,
        fc_range: None,
        fc_fn: None,
        fc_in: BTreeMap::new(),
        fc_out: BTreeMap::new(),
        notes: String::new(),
        fuse_zx: false,
        fell_back: false,
        sizes: Default::default(),
        hle,
        thunks,
        prof: Vec::new(),
    };
    let mut profile = Vec::new();
    if PROFILE.load(core::sync::atomic::Ordering::Relaxed) && cfg!(target_pointer_width = "32") {
        let counts: &'static mut [u32] = alloc::boxed::Box::leak(alloc::vec![0u32; n as usize].into_boxed_slice());
        for (k, (a, list)) in blocks.iter().enumerate() {
            let at = &counts[k] as *const u32 as usize as u32;
            g.prof.push(at);
            profile.push((*a, at, list.iter().map(profile_key).collect()));
        }
    }
    g.live_out = block_liveness(&g, &blocks, &tables).1;
    // functions: the layout, entered at the root and (when the dispatcher comes back to the
    // middle, as at a return address after the call left, or a loop's head after the budget
    // ran out) at blocks outside loops but the outermost's heads
    let mut extra = Vec::new();
    let fl = fnm.then(|| {
        let entry = starts[0].1 as usize;
        let l0 = structure::layout(&blocks, &ctxs, &targets, &inlined, &ret_to, &tables, &g.jnode, entry, &[]);
        let find = |a: u32| targets.get(&(0, a)).map(|&b| b as usize);
        let rets: Vec<usize> = (0..blocks.len())
            .filter(|&k| ctxs[k] == 0 && !inlined.contains_key(&k) && matches!(blocks[k].1.last().unwrap().flow_control(), FlowControl::Call | FlowControl::IndirectCall))
            .filter_map(|k| find(blocks[k].1.last().unwrap().next_ip32()))
            .collect();
        if !FN_ENTRIES.load(core::sync::atomic::Ordering::Relaxed) {
            return l0;
        }
        extra = l0.entries(blocks.len(), |k| k != entry && k < MAX_ENTRY && ctxs[k] == 0 && (rets.contains(&k) || l0.header(k)));
        if extra.is_empty() {
            return l0;
        }
        let l = structure::layout(&blocks, &ctxs, &targets, &inlined, &ret_to, &tables, &g.jnode, entry, &extra);
        if l.irreducible() > l0.irreducible() {
            extra.clear();
            return l0;
        }
        l
    });
    for &k in &extra {
        starts.push((blocks[k].0, k as u32));
    }
    bump(&mut g.c, 10);
    if fnm {
        // the state from the Cpu (ESP from the caller); status flags dead (a call's target)
        g.load_regs_esp(L_ESPIN);
        g.c.get(L_CPU).load32(Off::budget()).set(L_BUD);
        g.c.i32c(-1).set(L_OP);
        if !extra.is_empty() {
            // (the entry: 0 the root, else one past an `extra`)
            g.c.get(L_ARG).i32c(16).shr_u().set(L_BLK);
            g.c.get(L_ARG).u32c(0xffff).and().set(L_ARG);
        }
    } else {
        g.c.get(L_ARG).i32c(ENTRY_SHIFT as i32).shr_u().set(L_BLK);
    }
    g.c.block(); // exit
    if fnm {
        g.c.block(); // (where regions have the top loop)
    } else {
        g.c.loop_(); // top
    }
    g.c.block(); // dyn: returns and indirect jumps
    g.depth = 2;
    let mut count = 0;
    if fnm {
        let entry = starts[0].1 as usize;
        let l = fl.unwrap();
        g.dirty_in = dirty_in(&g, &blocks, &l);
        (g.safe_in, g.hoist) = safe_in(&g, &blocks, &l);
        g.preds = alloc::vec![Vec::new(); blocks.len()];
        for (v, ws) in l.succ.iter().enumerate().take(blocks.len()) {
            for &w in ws.iter().filter(|&&w| w < blocks.len()) {
                g.preds[w].push(v as u32);
            }
        }
        g.flags_out = alloc::vec![None; blocks.len()];
        if LAYOUT.load(core::sync::atomic::Ordering::Relaxed) {
            use core::fmt::Write;
            let _ = writeln!(g.notes, "IRREDUCIBLE {}", l.irreducible());
            let _ = writeln!(g.notes, "ENTRIES {:x?}", extra.iter().map(|&k| (k, blocks[k].0)).collect::<Vec<_>>());
        }
        let nb = n as usize;
        fn body<'a>(g: &mut Gen<'a>, l: &structure::Layout, blocks: &'a [(u32, Vec<Instruction>)], tables: &[structure::Table], entries: &[usize], count: &mut usize) {
            let nb = blocks.len();
            structure::emit(g, l, |g, k| {
                if k < nb {
                    emit_block(g, blocks, k, count);
                } else if k < nb + tables.len() {
                    emit_dispatch(g, &tables[k - nb], k);
                } else {
                    emit_entries(g, blocks, entries, k);
                }
            })
        }
        let entries: Vec<usize> = core::iter::once(entry).chain(extra.iter().copied()).collect();
        // with x87 code: emitted to see the caches its jumps come with (as in emit_level's
        // loops), as many times as these change, then for real with those
        if blocks.iter().any(|b| b.1.iter().any(fc_handles)) {
            let b0 = g.c.b.len();
            let mut fin = BTreeMap::new();
            g.fc_fn = Some((entry, nb));
            for _ in 0..6 {
                g.fc_in = fin.clone();
                g.fc_out.clear();
                body(&mut g, &l, &blocks, &tables, &entries, &mut 0);
                g.c.truncate(b0);
                let out: BTreeMap<usize, FCache> = g.fc_out.iter().filter(|f| f.1.on).map(|(&k, &f)| (k, f)).collect();
                if out == fin {
                    break;
                }
                fin = out;
            }
            g.fc_in = fin;
        }
        body(&mut g, &l, &blocks, &tables, &entries, &mut count);
        g.fc_fn = None;
        g.fc_in.clear();
    } else {
        let items = level(0, n as usize - 1, &loops);
        emit_level(&mut g, &blocks, &loops, &items, None, &mut count);
    }
    g.c.end(); // dyn
    if fnm {
        emit_fn_lookup(&mut g.c, roots[0], g.wregs);
    } else {
        emit_lookup(&mut g.c);
    }
    g.c.end(); // top
    g.c.end(); // exit
    writeback(&mut g.c, g.wregs);
    g.c.get(L_ST);
    if fnm {
        g.c.get(L_REG + 4);
    } else {
        for _ in 1..N_PARAMS {
            g.c.i32c(0);
        }
    }

    let mut locals = alloc::vec![I32; if fnm { N_PARAMS as usize - 2 } else { 0 } + N_LOCALS_I32 as usize];
    locals.extend_from_slice(&[I64, F64, F64, V128, V128, I32]);
    locals.extend_from_slice(&[F64; 8]);
    locals.extend_from_slice(&[V128; 16]);
    locals.push(I32); // L_AB
    locals.extend_from_slice(&[I32; N_SLOTS as usize]);
    if fnm {
        assert_eq!(2 + locals.len() as u32, L_ESPIN);
        locals.push(I32); // L_ESPIN
        let (locals, body) = wasm::compact_locals_param(2, L_ESPIN, &locals, core::mem::take(&mut g.c));
        let at = module.funcs.len() as u32;
        module.funcs.push(fn_adapter());
        module.funcs.push(Func { ty: T_FN, locals, code: body });
        module.name_at = at + 1;
    } else {
        let (locals, body) = wasm::compact_locals(N_PARAMS, &locals, core::mem::take(&mut g.c));
        module.funcs.push(Func { ty: 2, locals, code: body });
    }
    let mut layout = String::new();
    if LAYOUT.load(core::sync::atomic::Ordering::Relaxed) {
        use core::fmt::Write;
        for (k, (a, list)) in blocks.iter().enumerate() {
            let _ = writeln!(layout, "{k} {a:08x}-{:08x} ctx {} {}", list.last().unwrap().ip32(), ctxs[k], loops.get(&k).map_or(String::new(), |e| alloc::format!("loop ..{e}")));
        }
        for (a, h) in &g.hle {
            let _ = writeln!(layout, "HLE {a:08x} {h:?}");
        }
        layout.push_str(&g.notes);
        for (k, v) in &g.sizes {
            let _ = writeln!(layout, "SZ {k} {} {}", v.0, v.1);
        }
        let _ = writeln!(layout, "SZTOTAL {}", module.funcs.last().unwrap().code.b.len());
    }
    let sites = if fnm {
        blocks.iter().map(|b| b.1.last().unwrap()).filter(|l| matches!(l.flow_control(), FlowControl::IndirectCall | FlowControl::IndirectBranch)).map(|l| l.ip32()).collect()
    } else {
        Vec::new()
    };
    Some(Region { starts, instructions: count, code, layout, profile, sites })
}
