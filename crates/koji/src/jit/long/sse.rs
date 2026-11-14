//! SSE and the VEX 128-bit forms (AVX/AVX2 code built for /arch:AVX2) in long mode, as wasm
//! SIMD. The XMM registers stay in the Cpu: each instruction loads its operands from there and
//! stores its result. A VEX instruction is translated as its legacy form (the interpreter's
//! table, interp::vex): with VEX.vvvv as the first source, `dst = src1 op src2` reads src1
//! where the legacy form reads its destination; then the destination's upper YMM half
//! (`Cpu::ymmh`) is cleared, as VEX.128 does. VEX.256 forms that compute each 128-bit lane
//! alike are their VEX.128 form twice: the lower lanes, then the upper ones with each register's
//! halves swapped (and memory 16 bytes on); the rest go to the interpreter.

use super::{Flags, Gen, I64Ops, I32_WRAP, I64_EXT_U, L_A, L_CPU, L_V0, L_V1, STATUS};
use crate::jit::packed::{binop, farith, shift, unpack};
use alloc::vec::Vec;
use crate::Cpu;
use core::mem::offset_of;
use iced_x86::{EncodingKind, Instruction, Mnemonic, OpKind, Register};

/// The Cpu offset of XMM register `r`.
fn xoff(r: Register) -> u32 {
    if r.is_mm() {
        return (offset_of!(Cpu, mm) + 8 * (r as usize - Register::MM0 as usize)) as u32;
    }
    let i = r as usize - Register::XMM0 as usize;
    if i < 8 {
        (offset_of!(Cpu, xmm) + 16 * i) as u32
    } else {
        (offset_of!(Cpu, xmm8) + 16 * (i - 8)) as u32
    }
}

fn is_x(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_xmm()
}

fn is_mm(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_mm()
}

/// An instruction with an MMX register: the forms translated (an MMX register is the low half
/// of a v128, as in jit/packed.rs: lane-wise operations give the same low half).
fn mmx(ins: &Instruction) -> bool {
    use Mnemonic::*;
    let m = ins.mnemonic();
    let n = ins.op_count();
    let full = || n == 2 && is_mm(ins, 0) && (is_mm(ins, 1) || mem(ins, 1)) && size_ok(ins, 8);
    match m {
        Movd | Movq => {
            let size = if m == Movd { 4 } else { 8 };
            n == 2
                && size_ok(ins, size)
                && ((is_mm(ins, 0) && (gpr(ins, 1) || mem(ins, 1) || (m == Movq && is_mm(ins, 1))))
                    || ((gpr(ins, 0) || mem(ins, 0)) && is_mm(ins, 1)))
        }
        Movntq => n == 2 && mem(ins, 0) && is_mm(ins, 1) && size_ok(ins, 8),
        Pandn | Pmulhw | Pmulhuw | Pmuludq | Packsswb | Packuswb | Packssdw | Psadbw => full(),
        _ if super::super::packed::binop(m).is_some() => full(),
        _ if super::super::packed::unpack(m).is_some_and(|(e, _)| e < 8) && matches!(m, Punpcklbw | Punpcklwd | Punpckldq | Punpckhbw | Punpckhwd | Punpckhdq) => full(),
        _ if super::super::packed::shift(m).is_some() => {
            n == 2 && is_mm(ins, 0) && (ins.op_kind(1) == OpKind::Immediate8 || ((is_mm(ins, 1) || mem(ins, 1)) && size_ok(ins, 8)))
        }
        _ => false,
    }
}

fn gpr(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Register && (ins.op_register(i).is_gpr32() || ins.op_register(i).is_gpr64())
}

fn mem(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Memory
}

/// The instruction as translated (the legacy form of a VEX one) and its first source register
/// (the destination register of a legacy one; None when operand 0 is memory or a GPR).
pub(super) fn plan(ins: &Instruction) -> Option<(Instruction, Option<Register>)> {
    if (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && (ins.op_register(i).is_ymm() || ins.op_register(i).is_zmm())) {
        return None;
    }
    let (li, src1) = if ins.encoding() == EncodingKind::VEX {
        if ins.mnemonic() == Mnemonic::Vzeroupper {
            return Some((*ins, None));
        }
        let (lc, nds) = crate::interp::vex::legacy_of(ins.code())?;
        // VCMPxx predicates 8..31 have no legacy form: the interpreter runs them
        if matches!(ins.mnemonic(), Mnemonic::Vcmpps | Mnemonic::Vcmppd | Mnemonic::Vcmpss | Mnemonic::Vcmpsd) && ins.immediate8() & 0x18 != 0 {
            return None;
        }
        let mut li = *ins;
        li.set_code(lc);
        if nds {
            if !is_x(ins, 0) || !is_x(ins, 1) {
                return None;
            }
            for j in 1..li.op_count() {
                let k = ins.op_kind(j + 1);
                li.set_op_kind(j, k);
                if k == OpKind::Register {
                    li.set_op_register(j, ins.op_register(j + 1));
                }
            }
            (li, Some(ins.op_register(1)))
        } else {
            (li, is_x(ins, 0).then(|| ins.op_register(0)))
        }
    } else if ins.encoding() == EncodingKind::Legacy {
        if ins.mnemonic() == Mnemonic::Emms {
            return Some((*ins, None));
        }
        if (0..ins.op_count()).any(|i| is_mm(ins, i)) {
            return mmx(ins).then(|| (*ins, is_mm(ins, 0).then(|| ins.op_register(0))));
        }
        (*ins, is_x(ins, 0).then(|| ins.op_register(0)))
    } else {
        return None;
    };
    supported(&li).then_some((li, src1))
}

/// A VEX.256 instruction whose lanes [`per_lane`] may compute alike: its VEX.128 form (the
/// same operands, ymm registers as xmm).
pub(super) fn lanes(ins: &Instruction) -> Option<Instruction> {
    if ins.encoding() != EncodingKind::VEX || !(0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_ymm()) {
        return None;
    }
    let mut x = *ins;
    x.set_code(crate::interp::vex::xmm_form(ins.code())?);
    for i in 0..ins.op_count() {
        if ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_ymm() {
            x.set_op_register(i, Register::try_from(Register::XMM0 as usize + ins.op_register(i).number()).ok()?);
        }
    }
    Some(x)
}

/// Legacy forms whose VEX.256 form computes each 128-bit lane as the 128-bit form does, from
/// the same lane of its operands (packs, unpacks, shuffles and byte shifts are per lane in
/// AVX2; not shifts by an XMM count, which is one for both).
fn per_lane(li: &Instruction) -> bool {
    use Mnemonic::*;
    let m = li.mnemonic();
    match m {
        Movdqu | Movdqa | Movups | Movaps | Movupd | Movapd | Lddqu => true,
        Pandn | Andps | Andpd | Orps | Orpd | Xorps | Xorpd | Andnps | Andnpd | Pmulhw | Pmulhuw | Pmuludq | Packsswb | Packuswb
        | Packssdw | Packusdw | Pshufb | Psadbw | Minps | Maxps | Minpd | Maxpd | Pminsb | Pmaxsb | Pminud | Pmaxud | Pminsd
        | Pmaxsd | Pminuw | Pmaxuw | Pmulld | Pcmpeqq | Pcmpgtq | Pabsb | Pabsw | Pabsd | Cvtdq2ps | Cvttps2dq | Cvtps2dq => true,
        _ if binop(m).is_some() || unpack(m).is_some() => true,
        _ if farith(m).is_some_and(|f| f.1) => true,
        _ if shift(m).is_some() => li.op_kind(1) == OpKind::Immediate8,
        Pshufd | Pshuflw | Pshufhw | Shufps | Palignr | Pblendw | Blendps | Blendpd | Pslldq | Psrldq | Movsldup | Movshdup => true,
        _ => false,
    }
}

/// Memory operands of the size the form names, at most one.
fn size_ok(ins: &Instruction, size: usize) -> bool {
    (0..ins.op_count()).all(|i| !mem(ins, i) || ins.memory_size().size() == size)
}

fn supported(ins: &Instruction) -> bool {
    use Mnemonic::*;
    let m = ins.mnemonic();
    let n = ins.op_count();
    // xmm <- xmm/m128 (and two-operand forms with an immediate)
    let full = || n >= 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, 16);
    let imm = |k: u32| n == k + 1 && ins.op_kind(k) == OpKind::Immediate8;
    match m {
        Movdqu | Movdqa | Movups | Movaps | Movupd | Movapd | Lddqu => {
            n == 2 && size_ok(ins, 16) && ((is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1))) || (mem(ins, 0) && is_x(ins, 1)))
        }
        Movd | Movq => {
            n == 2
                && ((is_x(ins, 0) && (gpr(ins, 1) || mem(ins, 1) || (m == Movq && is_x(ins, 1))))
                    || ((gpr(ins, 0) || mem(ins, 0)) && is_x(ins, 1)))
        }
        Movss | Movsd => {
            n == 2 && ((is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1))) || (mem(ins, 0) && is_x(ins, 1)))
                && size_ok(ins, if m == Movss { 4 } else { 8 })
        }
        Pmovmskb | Movmskps | Movmskpd => n == 2 && gpr(ins, 0) && is_x(ins, 1),
        Pandn | Andps | Andpd | Orps | Orpd | Xorps | Xorpd | Andnps | Andnpd | Pmulhw | Pmulhuw | Pmuludq | Packsswb | Packuswb
        | Packssdw | Packusdw | Pshufb | Psadbw | Minps | Maxps | Minpd | Maxpd | Pminsb | Pmaxsb | Pminud | Pmaxud | Pminsd
        | Pmaxsd | Pminuw | Pmaxuw | Pmulld | Pcmpeqq | Pcmpgtq | Pabsb | Pabsw | Pabsd | Cvtdq2ps | Cvttps2dq | Cvtps2dq => {
            n == 2 && full()
        }
        _ if binop(m).is_some() || unpack(m).is_some() => n == 2 && full(),
        _ if farith(m).is_some_and(|f| f.1) => n == 2 && full(),
        _ if farith(m).is_some() => {
            let bits = farith(m).unwrap().0;
            n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, bits as usize / 8)
        }
        _ if shift(m).is_some() => n == 2 && is_x(ins, 0) && (ins.op_kind(1) == OpKind::Immediate8 || ((is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, 16))),
        Pshufd | Pshuflw | Pshufhw | Shufps | Palignr | Pblendw | Blendps | Blendpd => full() && imm(2),
        Pslldq | Psrldq => n == 2 && is_x(ins, 0) && ins.op_kind(1) == OpKind::Immediate8,
        Pinsrb | Pinsrw | Pinsrd | Pinsrq => {
            let size = match m { Pinsrb => 1, Pinsrw => 2, Pinsrd => 4, _ => 8 };
            n == 3 && is_x(ins, 0) && (gpr(ins, 1) || mem(ins, 1)) && size_ok(ins, size) && imm(2)
        }
        Pextrw | Pextrd | Pextrq | Pextrb => n == 3 && (gpr(ins, 0) || mem(ins, 0)) && is_x(ins, 1) && imm(2),
        Cvtps2pd | Cvtdq2pd => n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, 8),
        Cvtpd2ps => n == 2 && full(),
        Cvtss2sd | Cvtsd2ss => n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, if m == Cvtss2sd { 4 } else { 8 }),
        Cvtsi2ss | Cvtsi2sd => n == 2 && is_x(ins, 0) && (gpr(ins, 1) || mem(ins, 1)),
        Cvttss2si | Cvttsd2si | Cvtss2si | Cvtsd2si => {
            n == 2 && gpr(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, if matches!(m, Cvttss2si | Cvtss2si) { 4 } else { 8 })
        }
        Comiss | Ucomiss | Comisd | Ucomisd => {
            n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, if matches!(m, Comiss | Ucomiss) { 4 } else { 8 })
        }
        Pmovzxbw | Pmovzxwd | Pmovzxdq | Pmovsxbw | Pmovsxwd | Pmovsxdq => {
            n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, 8)
        }
        Pmovzxbd | Pmovzxwq | Pmovsxbd | Pmovsxwq => n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, 4),
        Movsldup | Movshdup => n == 2 && full(),
        Movddup => n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, 8),
        Minss | Maxss | Minsd | Maxsd => {
            n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, if matches!(m, Minss | Maxss) { 4 } else { 8 })
        }
        // (the mask is XMM0, implicit in the legacy form)
        Blendvps | Blendvpd | Pblendvb => {
            n == 2 && is_x(ins, 0) && (is_x(ins, 1) || mem(ins, 1)) && size_ok(ins, 16)
        }
        Movlps | Movhps | Movlpd | Movhpd => n == 2 && size_ok(ins, 8) && ((is_x(ins, 0) && mem(ins, 1)) || (mem(ins, 0) && is_x(ins, 1))),
        Movhlps | Movlhps => n == 2 && is_x(ins, 0) && is_x(ins, 1),
        Vzeroupper => true,
        _ => false,
    }
}

impl Gen<'_> {
    /// Translates an instruction [`plan`] accepts; false (nothing emitted) if it can't.
    pub(super) fn sse(&mut self) -> bool {
        if super::avx::supported(&self.ins) {
            return self.avx();
        }
        if let Some(x) = lanes(&self.ins) {
            return self.sse256(&x);
        }
        let Some((li, src1)) = plan(&self.ins) else { return false };
        let ymmh = |r: usize| (offset_of!(Cpu, ymmh) + 16 * r) as u32;
        if li.mnemonic() == Mnemonic::Vzeroupper {
            for r in 0..16 {
                self.c.get(L_CPU).i64c(0).store64(ymmh(r));
                self.c.get(L_CPU).i64c(0).store64(ymmh(r) + 8);
            }
            return true;
        }
        // the memory operand's address first (it may leave for the interpreter)
        let size = (0..li.op_count()).find(|&i| mem(&li, i)).map(|_| li.memory_size().size() as u32);
        if let Some(s) = size {
            if !self.addr(s.max(1)) {
                return false;
            }
        }
        let saved = self.ins;
        self.ins = li;
        self.mx_guard(&li, src1, size, false);
        self.emit_sse(src1);
        self.ins = saved;
        // VEX.128 clears the upper half of the YMM register it writes
        use Mnemonic::*;
        if saved.encoding() == EncodingKind::VEX
            && is_x(&saved, 0)
            && !matches!(saved.mnemonic(), Vcomiss | Vcomisd | Vucomiss | Vucomisd | Vptest | Vtestps | Vtestpd | Vmaskmovdqu)
        {
            let r = ymmh(saved.op0_register() as usize - Register::XMM0 as usize);
            self.c.get(L_CPU).i64c(0).store64(r);
            self.c.get(L_CPU).i64c(0).store64(r + 8);
        }
        true
    }

    /// The MXCSR flags: the interpreter sets them (mxguard): leaves for it when the operands
    /// of `li` could raise one. `hi`: the operands are a VEX.256 instruction's upper lanes
    /// (swapped in: nothing known about them).
    fn mx_guard(&mut self, li: &Instruction, src1: Option<Register>, size: Option<u32>, hi: bool) {
        if let Some(k) = crate::jit::mxguard::kind(li) {
            use crate::jit::mxguard;
            // (an XMM register checked for the same range earlier in the block and not written
            // since: not again)
            let reg = |i: u32| {
                let r = if i == 0 { src1 } else { (li.op_kind(1) == OpKind::Register).then(|| li.op_register(1)) };
                r.filter(|r| r.is_xmm()).map(|r| r as usize - Register::XMM0 as usize)
            };
            let seen = |g: &Self, i: u32| !hi && reg(i).is_some_and(|r| mxguard::key(k, i).is_some_and(|w| g.mx_seen[r].contains(&Some(w))));
            let (a, b) = (k.3 && !seen(self, 0), !seen(self, 1));
            if k.0 != mxguard::G::Inexact {
                if a {
                    self.vfirst(src1);
                    self.c.set(L_V0);
                }
                if b {
                    self.vsrc(1, size.unwrap_or(16));
                    self.c.set(L_V1);
                }
            }
            let ops = mxguard::operands(&mut self.c, k, a.then_some(L_V0), b.then_some(L_V1), Some(super::L_V2));
            for i in (0..2).filter(|_| !hi) {
                if let (true, Some(r), Some(w)) = (i == 1 || k.3, reg(i), mxguard::key(k, i)) {
                    if !self.mx_seen[r].contains(&Some(w)) {
                        self.mx_seen[r] = [self.mx_seen[r][1], Some(w)];
                    }
                }
            }
            // (PE once seen set stays set in this block's straight-line code)
            let pe = mxguard::rounds(k.0) && self.mx_checked != Some(self.depth);
            if pe {
                mxguard::mode(&mut self.c, L_CPU, offset_of!(Cpu, mxcsr) as u32);
                if ops {
                    self.c.or();
                }
                self.mx_checked = Some(self.depth);
            }
            if ops || pe {
                self.c.if_();
                self.depth += 1;
                self.bail();
                self.depth -= 1;
                self.c.end();
            }
        }
    }

    /// A VEX.256 instruction as its VEX.128 form `x` on each lane: both lanes' MXCSR guards
    /// before either is written, then the lower lanes, then the upper ones (`swap_hi`).
    fn sse256(&mut self, x: &Instruction) -> bool {
        let Some((li, src1)) = plan(x) else { return false };
        if !per_lane(&li) {
            return false;
        }
        let size = (0..li.op_count()).find(|&i| mem(&li, i)).map(|_| li.memory_size().size() as u32);
        if size.is_some() && !self.addr(32) {
            return false;
        }
        // (the upper lanes of VBLENDPS / VBLENDPD take the immediate's upper bits)
        let mut hi = li;
        match li.mnemonic() {
            Mnemonic::Blendps => hi.set_immediate8(li.immediate8() >> 4),
            Mnemonic::Blendpd => hi.set_immediate8(li.immediate8() >> 2),
            _ => {}
        }
        let mut regs: Vec<Register> = src1.into_iter().chain((0..li.op_count()).filter(|&i| is_x(&li, i)).map(|i| li.op_register(i))).collect();
        regs.sort_unstable_by_key(|r| *r as u32);
        regs.dedup();
        let saved = self.ins;
        // (the address kept apart: CVTPS2DQ's code takes L_A for the rounding mode)
        let at = size.map(|_| {
            let t = self.temp();
            self.c.get(L_A).x(I64_EXT_U).set(t);
            t
        });
        let upper = |g: &mut Self, on: bool| {
            g.swap_hi(&regs);
            if let Some(t) = at {
                g.c.get(t).x(I32_WRAP);
                if on {
                    g.c.i32c(16).add();
                }
                g.c.set(L_A);
            }
        };
        if crate::jit::mxguard::kind(&li).is_some() {
            self.ins = li;
            self.mx_guard(&li, src1, size, false);
            upper(self, true);
            self.ins = hi;
            // (leaving from here: the halves back first)
            self.hi_swapped = regs.clone();
            self.mx_guard(&hi, src1, size, true);
            self.hi_swapped.clear();
            upper(self, false);
        }
        self.ins = li;
        self.emit_sse(src1);
        upper(self, true);
        self.ins = hi;
        self.emit_sse(src1);
        upper(self, false);
        self.ins = saved;
        true
    }

    /// Swaps the lower and upper halves of YMM registers `regs`.
    pub(super) fn swap_hi(&mut self, regs: &[Register]) {
        for &r in regs {
            let (x, h) = (xoff(r), (offset_of!(Cpu, ymmh) + 16 * (r as usize - Register::XMM0 as usize)) as u32);
            self.c.get(L_CPU).v128load(x).set(super::L_V2);
            self.c.get(L_CPU).get(L_CPU).v128load(h).v128store(x);
            self.c.get(L_CPU).get(super::L_V2).v128store(h);
        }
    }

    /// Pushes XMM register `r`.
    fn xget(&mut self, r: Register) {
        if r.is_mm() {
            self.c.get(L_CPU).v128load64(xoff(r));
        } else {
            self.c.get(L_CPU).v128load(xoff(r));
        }
    }

    /// Pushes operand `i` as a v128 (memory: its `size` bytes, zeros above).
    fn vsrc(&mut self, i: u32, size: u32) {
        if self.ins.op_kind(i) == OpKind::Register {
            let r = self.ins.op_register(i);
            self.xget(r);
            return;
        }
        self.c.get(L_A);
        match size {
            4 => self.c.v128load32(0),
            8 => self.c.v128load64(0),
            _ => self.c.v128load(0),
        };
    }

    /// Pushes the first source (`src1`: the destination register of a legacy form).
    fn vfirst(&mut self, src1: Option<Register>) {
        self.xget(src1.unwrap());
    }

    /// Stores the v128 in L_V0 to the destination register.
    fn vput(&mut self) {
        let d = self.ins.op_register(0);
        if d.is_mm() {
            self.c.get(L_CPU).get(L_V0).lane(0x1d, 0).store64(xoff(d));
        } else {
            self.c.get(L_CPU).get(L_V0).v128store(xoff(d));
        }
    }

    /// Pushes the i64 of a GPR or memory operand `i` (`bits` wide, zero-extended).
    fn gsrc(&mut self, i: u32, bits: u32) {
        if self.ins.op_kind(i) == OpKind::Memory {
            self.load(bits);
        } else {
            self.get_reg(self.ins.op_register(i));
        }
    }

    /// Writes the i64 on the stack to GPR/memory operand `i` (`bits` wide; a 32-bit register
    /// zero-extended).
    fn gdst(&mut self, i: u32, bits: u32) {
        let t = self.temp();
        self.c.set(t);
        if self.ins.op_kind(i) == OpKind::Memory {
            self.store(bits, t);
        } else {
            self.set_reg(self.ins.op_register(i), t);
        }
    }

    fn emit_sse(&mut self, src1: Option<Register>) {
        use Mnemonic::*;
        let ins = self.ins;
        let m = ins.mnemonic();
        match m {
            Movdqu | Movdqa | Movups | Movaps | Movupd | Movapd | Lddqu => {
                if mem(&ins, 0) {
                    self.c.get(L_A);
                    self.xget(ins.op_register(1));
                    self.c.v128store(0);
                } else {
                    self.vsrc(1, 16);
                    self.c.set(L_V0);
                    self.vput();
                }
            }
            Emms => {
                let empty = (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, empty)) as u32;
                self.c.get(L_CPU).i32c(0xff).store8(empty);
            }
            Movntq => {
                self.c.get(L_A).get(L_CPU).load64(xoff(ins.op_register(1))).store64(0);
            }
            Movd | Movq => {
                let bits = if m == Movd { 32 } else { 64 };
                if is_x(&ins, 0) || is_mm(&ins, 0) {
                    // zero-extended into the register
                    let d = ins.op_register(0);
                    if is_x(&ins, 1) || is_mm(&ins, 1) {
                        self.xget(ins.op_register(1));
                        self.c.lane(0x1d, 0); // i64x2.extract_lane 0
                    } else {
                        self.gsrc(1, bits);
                        if bits == 32 {
                            self.c.i64c(0xffff_ffff).x(super::I64_AND);
                        }
                    }
                    let t = self.temp();
                    self.c.set(t);
                    self.c.get(L_CPU).get(t).store64(xoff(d));
                    if !d.is_mm() {
                        self.c.get(L_CPU).i64c(0).store64(xoff(d) + 8);
                    }
                } else {
                    // out of the register's low bits
                    self.c.get(L_CPU).load64(xoff(ins.op_register(1)));
                    self.gdst(0, bits);
                }
            }
            Movss | Movsd => {
                let bytes = if m == Movss { 4 } else { 8 };
                if mem(&ins, 0) {
                    let r = ins.op_register(1);
                    self.c.get(L_A).get(L_CPU);
                    if bytes == 4 {
                        self.c.load32(xoff(r)).store32(0);
                    } else {
                        self.c.load64(xoff(r)).store64(0);
                    }
                } else if mem(&ins, 1) {
                    self.vsrc(1, bytes);
                    self.c.set(L_V0);
                    self.vput();
                } else {
                    // the low element replaced
                    self.vfirst(src1);
                    self.xget(ins.op_register(1));
                    if bytes == 4 {
                        self.c.lane(0x1b, 0).lane(0x1c, 0); // i32x4 extract, replace
                    } else {
                        self.c.lane(0x1d, 0).lane(0x1e, 0); // i64x2 extract, replace
                    }
                    self.c.set(L_V0);
                    self.vput();
                }
            }
            Pmovmskb | Movmskps | Movmskpd => {
                self.xget(ins.op_register(1));
                let op = match m { Pmovmskb => 0x64, Movmskps => 0xa4, _ => 0xc4 };
                self.c.simd(op).x(I64_EXT_U);
                self.gdst(0, 32);
            }
            Pinsrb | Pinsrw | Pinsrd | Pinsrq => {
                let (bits, op, lanes) = match m { Pinsrb => (8, 0x17, 16), Pinsrw => (16, 0x1a, 8), Pinsrd => (32, 0x1c, 4), _ => (64, 0x1e, 2) };
                let lane = (ins.immediate8() as u32 % lanes) as u8;
                self.vfirst(src1);
                self.gsrc(1, bits);
                if bits < 64 {
                    self.c.x(I32_WRAP);
                }
                self.c.lane(op, lane);
                self.c.set(L_V0);
                self.vput();
            }
            Pextrb | Pextrw | Pextrd | Pextrq => {
                let (bits, op, lanes) = match m { Pextrb => (8, 0x16, 16), Pextrw => (16, 0x19, 8), Pextrd => (32, 0x1b, 4), _ => (64, 0x1d, 2) };
                let lane = (ins.immediate8() as u32 % lanes) as u8;
                self.xget(ins.op_register(1));
                self.c.lane(op, lane); // (the unsigned extract for bytes and words)
                if bits < 64 {
                    self.c.x(I64_EXT_U);
                }
                // (to a register: zero-extended, a 32-bit write)
                let wbits = if mem(&ins, 0) { bits } else if bits == 64 { 64 } else { 32 };
                self.gdst(0, wbits);
            }
            _ if farith(m).is_some_and(|f| !f.1) => {
                // scalar: element 0 from the operation, the rest from the first source
                let (bits, _, _, op) = farith(m).unwrap();
                let (extract, replace) = if bits == 32 { (0x1f, 0x20) } else { (0x21, 0x22) };
                self.vfirst(src1);
                if !matches!(m, Sqrtss | Sqrtsd) {
                    self.vfirst(src1);
                    self.c.lane(extract, 0);
                }
                self.vsrc(1, bits / 8);
                self.c.lane(extract, 0).op(op as u8).lane(replace, 0);
                self.c.set(L_V0);
                self.vput();
            }
            Cvtss2sd | Cvtsd2ss | Cvtsi2ss | Cvtsi2sd => {
                self.vfirst(src1);
                match m {
                    Cvtss2sd => {
                        self.vsrc(1, 4);
                        self.c.lane(0x1f, 0).op(0xbb).lane(0x22, 0); // f64.promote_f32
                    }
                    Cvtsd2ss => {
                        self.vsrc(1, 8);
                        self.c.lane(0x21, 0).op(0xb6).lane(0x20, 0); // f32.demote_f64
                    }
                    _ => {
                        let bits = if mem(&ins, 1) { ins.memory_size().size() as u32 * 8 } else if ins.op_register(1).is_gpr64() { 64 } else { 32 };
                        self.gsrc(1, bits);
                        let f = m == Cvtsi2ss;
                        if bits == 32 {
                            self.c.x(I32_WRAP).op(if f { 0xb2 } else { 0xb7 }); // f32/f64.convert_i32_s
                        } else {
                            self.c.op(if f { 0xb4 } else { 0xb9 }); // f32/f64.convert_i64_s
                        }
                        self.c.lane(if f { 0x20 } else { 0x22 }, 0);
                    }
                }
                self.c.set(L_V0);
                self.vput();
            }
            Cvttss2si | Cvttsd2si | Cvtss2si | Cvtsd2si => self.to_int(),
            Comiss | Ucomiss | Comisd | Ucomisd => self.comi(src1),
            Movlps | Movhps | Movlpd | Movhpd => {
                // one quadword of the register (low: L, high: H) from or to memory
                let high = matches!(m, Movhps | Movhpd);
                if mem(&ins, 0) {
                    let r = ins.op_register(1);
                    self.c.get(L_A).get(L_CPU).load64(xoff(r) + if high { 8 } else { 0 }).store64(0);
                } else {
                    self.vfirst(src1);
                    self.c.get(L_A).load64(0);
                    self.c.lane(0x1e, high as u8); // i64x2.replace_lane
                    self.c.set(L_V0);
                    self.vput();
                }
            }
            _ => self.packed_op(src1),
        }
    }

    /// CVT(T)SS2SI / CVT(T)SD2SI: element 0 to a 32/64-bit integer, rounded by MXCSR.RC
    /// (truncated); out of range and NaN give the "integer indefinite" (0x80000000...).
    fn to_int(&mut self) {
        use Mnemonic::*;
        let ins = self.ins;
        let m = ins.mnemonic();
        let single = matches!(m, Cvttss2si | Cvtss2si);
        let wide = ins.op_register(0).is_gpr64();
        self.vsrc(1, if single { 4 } else { 8 });
        if single {
            self.c.lane(0x1f, 0).op(0xbb); // f64.promote_f32
        } else {
            self.c.lane(0x21, 0);
        }
        let f = self.temp();
        // (an i64 temp holds the f64's bits)
        self.c.op(0xbd).set(f); // i64.reinterpret_f64
        if matches!(m, Cvtss2si | Cvtsd2si) {
            // nearest, floor, ceil, trunc by RC
            let rc = L_A;
            self.c.get(L_CPU).load32(offset_of!(Cpu, mxcsr) as u32).i32c(13).shr_u().i32c(3).and().set(rc);
            // select(kept, this, rc != k): the rounding RC names
            self.c.get(f).op(0xbf).op(0x9e); // f64.reinterpret_i64, f64.nearest
            for (k, op) in [(1, 0x9c), (2, 0x9b), (3, 0x9d)] {
                self.c.get(f).op(0xbf).op(op); // floor, ceil, trunc
                self.c.get(rc).i32c(k).ne().select();
            }
            self.c.op(0xbd).set(f);
        }
        // in range: lo < x < hi (false for NaN) -> trunc, else indefinite
        let (lo, hi, ind): (f64, f64, i64) = if wide {
            (-9223372036854777856.0, 9223372036854775808.0, i64::MIN)
        } else {
            (-2147483649.0, 2147483648.0, 0x8000_0000)
        };
        if wide {
            self.c.get(f).op(0xbf).op(0xfc).op(0x06); // i64.trunc_sat_f64_s
        } else {
            self.c.get(f).op(0xbf).op(0xfc).op(0x02).x(I64_EXT_U); // i32.trunc_sat_f64_s
        }
        self.c.i64c(ind);
        self.c.get(f).op(0xbf).f64c(lo).op(0x64); // f64.gt
        self.c.get(f).op(0xbf).f64c(hi).op(0x63).and().select(); // f64.lt
        if !wide {
            self.c.i64c(0xffff_ffff).x(super::I64_AND);
        }
        self.gdst(0, if wide { 64 } else { 32 });
    }

    /// COMISS / UCOMISS / COMISD / UCOMISD: ZF PF CF from the compare (unordered 111, less 001,
    /// equal 100, greater 000), OF SF AF cleared.
    fn comi(&mut self, src1: Option<Register>) {
        let ins = self.ins;
        let single = matches!(ins.mnemonic(), Mnemonic::Comiss | Mnemonic::Ucomiss);
        let (eq, lt, ne) = if single { (0x41, 0x43, 0x42) } else { (0x47, 0x49, 0x48) };
        self.vfirst(src1);
        self.c.set(L_V0);
        self.vsrc(1, if single { 4 } else { 8 });
        self.c.set(L_V1);
        let (x, y) = (L_V0, L_V1);
        let lazy = offset_of!(Cpu, lazy) as u32;
        let ef = offset_of!(Cpu, eflags) as u32;
        self.c.get(L_CPU);
        self.c.get(L_CPU).load32(ef).u32c(!STATUS).and();
        self.c.get(x).get(y).simd(lt).lane(0x1b, 0).i32c(1).and();
        self.c.get(x).get(y).simd(eq).lane(0x1b, 0).i32c(0x40).and().or();
        self.c.get(x).get(x).simd(ne).get(y).get(y).simd(ne).simd(0x50).lane(0x1b, 0).i32c(0x45).and().or();
        self.c.or().store32(ef);
        self.c.get(L_CPU).i32c(0).store32(lazy);
        self.flags = Flags::Cpu;
    }

    /// The whole-register operations: L_V0 = f(first source, operand 1), to the destination.
    fn packed_op(&mut self, src1: Option<Register>) {
        use Mnemonic::*;
        let ins = self.ins;
        let m = ins.mnemonic();
        let (a, b) = (L_V0, L_V1);
        // the operands into L_V0 (first source) and L_V1 (operand 1) where they are vectors
        let src_size = match m {
            Cvtps2pd | Cvtdq2pd | Pmovzxbw | Pmovzxwd | Pmovzxdq | Pmovsxbw | Pmovsxwd | Pmovsxdq => 8,
            Pmovzxbd | Pmovzxwq | Pmovsxbd | Pmovsxwq | Minss | Maxss => 4,
            Movddup | Minsd | Maxsd => 8,
            _ if is_mm(&ins, 0) => 8,
            _ => 16,
        };
        if src1.is_some() {
            self.vfirst(src1);
            self.c.set(a);
        }
        if ins.op_count() >= 2 && ins.op_kind(1) != OpKind::Immediate8 {
            self.vsrc(1, src_size);
            self.c.set(b);
        }
        if let Some(op) = binop(m) {
            self.c.get(a).get(b).simd(op);
        } else if let Some((_, _, op, _)) = farith(m) {
            if !matches!(m, Sqrtps | Sqrtpd) {
                self.c.get(a);
            }
            self.c.get(b).simd(op);
        } else if let Some((e, high)) = unpack(m) {
            // (MMX: the high half of 8 bytes)
            let start = if !high { 0 } else if is_mm(&ins, 0) { 4 } else { 8 };
            let mut l = [0u8; 16];
            for k in 0..16 / (2 * e) {
                for t in 0..e {
                    let at = start + k * e + t;
                    l[2 * k * e + t] = at as u8;
                    l[2 * k * e + e + t] = 16 + at as u8;
                }
            }
            self.c.get(a).get(b).shuffle(l);
        } else if let Some((bits, op, arith)) = shift(m) {
            self.c.get(a);
            if ins.op_kind(1) == OpKind::Immediate8 {
                let n = ins.immediate8() as u32;
                if n < bits {
                    self.c.i32c(n as i32).simd(op);
                } else if arith {
                    self.c.i32c(bits as i32 - 1).simd(op);
                } else {
                    self.c.drop_().v128zero();
                }
            } else {
                // the count: the source's low 64 bits; past the lane width, zeros or the sign
                let w = self.temp();
                self.c.get(b).lane(0x1d, 0).set(w);
                if arith {
                    self.c.get(w).x(I32_WRAP).i32c(bits as i32 - 1);
                    self.c.get(w).i64c(bits as i64).x(super::I64_LT_U);
                    self.c.select().simd(op);
                } else {
                    self.c.get(w).x(I32_WRAP).simd(op);
                    self.c.i32c(0).get(w).i64c(bits as i64).x(super::I64_LT_U).sub().simd(0x0f).simd(0x4e);
                }
            }
        } else {
            match m {
                Andps | Andpd => {
                    self.c.get(a).get(b).simd(0x4e);
                }
                Orps | Orpd => {
                    self.c.get(a).get(b).simd(0x50);
                }
                Xorps | Xorpd => {
                    self.c.get(a).get(b).simd(0x51);
                }
                // ~dst & src = andnot(src, dst)
                Pandn | Andnps | Andnpd => {
                    self.c.get(b).get(a).simd(0x4f);
                }
                Minps | Maxps | Minpd | Maxpd => {
                    // the first operand where it is less (greater), else the second
                    let cmp = match m { Minps => 0x43, Maxps => 0x44, Minpd => 0x49, _ => 0x4a };
                    self.c.get(a).get(b).get(a).get(b).simd(cmp).simd(0x52);
                }
                Minss | Maxss | Minsd | Maxsd => {
                    // element 0 as MINPS/MAXPS picks it (the second operand when unordered or
                    // equal), the rest from the first source
                    let (cmp, replace, extract) = match m {
                        Minss => (0x43, 0x1c, 0x1b),
                        Maxss => (0x44, 0x1c, 0x1b),
                        Minsd => (0x49, 0x1e, 0x1d),
                        _ => (0x4a, 0x1e, 0x1d),
                    };
                    self.c.get(a);
                    self.c.get(a).get(b).get(a).get(b).simd(cmp).simd(0x52);
                    self.c.lane(extract, 0).lane(replace, 0);
                }
                Movsldup | Movshdup | Movddup => {
                    let pick: [u8; 4] = match m { Movsldup => [0, 0, 2, 2], Movshdup => [1, 1, 3, 3], _ => [0, 1, 0, 1] };
                    let l: [u8; 16] = core::array::from_fn(|k| pick[k / 4] * 4 + (k % 4) as u8);
                    self.c.get(b).get(b).shuffle(l);
                }
                Movhlps | Movlhps => {
                    // MOVHLPS: the source's high quadword to the low one; MOVLHPS: its low one
                    // to the high one
                    let l: [u8; 16] = core::array::from_fn(|k| {
                        (if m == Movhlps { if k < 8 { 16 + 8 + k } else { k } } else if k < 8 { k } else { 16 + k - 8 }) as u8
                    });
                    self.c.get(a).get(b).shuffle(l);
                }
                Blendvps | Blendvpd | Pblendvb => {
                    // the second operand where XMM0's element has its top bit set
                    let (shr, n) = match m { Blendvps => (0xac, 31), Blendvpd => (0xcc, 63), _ => (0x6c, 7) };
                    self.c.get(b).get(a);
                    self.c.get(L_CPU).v128load(xoff(Register::XMM0)).i32c(n).simd(shr);
                    self.c.simd(0x52);
                }
                Pminsb => {
                    self.c.get(a).get(b).simd(0x76);
                }
                Pmaxsb => {
                    self.c.get(a).get(b).simd(0x78);
                }
                Pminuw => {
                    self.c.get(a).get(b).simd(0x97);
                }
                Pmaxuw => {
                    self.c.get(a).get(b).simd(0x99);
                }
                Pminsd => {
                    self.c.get(a).get(b).simd(0xb6);
                }
                Pminud => {
                    self.c.get(a).get(b).simd(0xb7);
                }
                Pmaxsd => {
                    self.c.get(a).get(b).simd(0xb8);
                }
                Pmaxud => {
                    self.c.get(a).get(b).simd(0xb9);
                }
                Pmulld => {
                    self.c.get(a).get(b).simd(0xb5);
                }
                Pcmpeqq => {
                    self.c.get(a).get(b).simd(0xd6);
                }
                Pcmpgtq => {
                    self.c.get(a).get(b).simd(0xd9);
                }
                Pabsb => {
                    self.c.get(b).simd(0x60);
                }
                Pabsw => {
                    self.c.get(b).simd(0x80);
                }
                Pabsd => {
                    self.c.get(b).simd(0xa0);
                }
                Pmuludq => {
                    let l: [u8; 16] = core::array::from_fn(|k| (if k < 4 { k } else if k < 8 { k + 4 } else { k }) as u8);
                    self.c.get(a).get(a).shuffle(l);
                    self.c.get(b).get(b).shuffle(l);
                    self.c.simd(0xde); // i64x2.extmul_low_i32x4_u
                }
                Pmulhw | Pmulhuw => {
                    let (lo, hi, shr, narrow) = if m == Pmulhw { (0xbc, 0xbd, 0xac, 0x85) } else { (0xbe, 0xbf, 0xad, 0x86) };
                    self.c.get(a).get(b).simd(lo).i32c(16).simd(shr);
                    self.c.get(a).get(b).simd(hi).i32c(16).simd(shr);
                    self.c.simd(narrow);
                }
                Packsswb | Packuswb | Packssdw | Packusdw => {
                    let op = match m { Packsswb => 0x65, Packuswb => 0x66, Packssdw => 0x85, _ => 0x86 };
                    self.c.get(a).get(b).simd(op);
                    if is_mm(&ins, 0) {
                        // (MMX: the first operand's 4 bytes, then the second's)
                        let l: [u8; 16] = core::array::from_fn(|k| (if k < 4 { k } else if k < 8 { k + 4 } else { k }) as u8);
                        self.c.set(a).get(a).get(a).shuffle(l);
                    }
                }
                Pshufb => {
                    // swizzle gives 0 for indexes past 15: the high bit kept, the rest cut to 4 bits
                    self.c.get(a).get(b).i32c(0x8f).simd(0x0f).simd(0x4e).simd(0x0e);
                }
                Psadbw => {
                    // |a - b| per byte (max - min), summed per 8 bytes into the low word of each
                    // quadword
                    self.c.get(a).get(b).simd(0x79).get(a).get(b).simd(0x77).simd(0x71); // max_u - min_u
                    self.c.simd(0x7d); // i16x8.extadd_pairwise_i8x16_u
                    self.c.simd(0x7f); // i32x4.extadd_pairwise_i16x8_u
                    // the two dwords of each quadword added: low + (high << 0) via i64x2 ops
                    self.c.set(a);
                    self.c.get(a).i32c(32).simd(0xcd); // i64x2.shr_u 32
                    self.c.get(a).i64c(0xffff_ffff).simd(0x12).simd(0x4e); // and splat(0xffffffff)
                    self.c.simd(0xce); // i64x2.add
                }
                Shufps => {
                    let imm = ins.immediate8();
                    let pick = [imm & 3, (imm >> 2) & 3, 4 + ((imm >> 4) & 3), 4 + (imm >> 6)];
                    let l: [u8; 16] = core::array::from_fn(|k| pick[k / 4] * 4 + (k % 4) as u8);
                    self.c.get(a).get(b).shuffle(l);
                }
                Pshufd | Pshuflw | Pshufhw => {
                    let (e, at) = match m { Pshufd => (4, 0), Pshufhw => (2, 4), _ => (2, 0) };
                    let imm = ins.immediate8();
                    let mut l: [u8; 16] = core::array::from_fn(|k| k as u8);
                    for k in 0..4 {
                        let j = at + ((imm >> (2 * k)) as usize & 3);
                        for t in 0..e {
                            l[(at + k) * e + t] = (j * e + t) as u8;
                        }
                    }
                    self.c.get(b).get(b).shuffle(l);
                }
                Pblendw | Blendps | Blendpd => {
                    let e = match m { Pblendw => 2, Blendps => 4, _ => 8 };
                    let imm = ins.immediate8() as usize;
                    let l: [u8; 16] = core::array::from_fn(|k| (if imm >> (k / e) & 1 != 0 { 16 + k } else { k }) as u8);
                    self.c.get(a).get(b).shuffle(l);
                }
                Palignr => {
                    // (b:a)'s bytes from imm: b's bytes are 0..15, a's 16..31, past them zeros
                    let n = ins.immediate8() as usize;
                    if n < 16 {
                        let l: [u8; 16] = core::array::from_fn(|k| (k + n) as u8);
                        self.c.get(b).get(a).shuffle(l);
                    } else if n < 32 {
                        let l: [u8; 16] = core::array::from_fn(|k| (k + n - 16).min(16) as u8);
                        self.c.get(a).v128zero().shuffle(l);
                    } else {
                        self.c.v128zero();
                    }
                }
                Pslldq | Psrldq => {
                    let n = (ins.immediate8() as usize).min(16);
                    let l: [u8; 16] = core::array::from_fn(|k| {
                        (if m == Pslldq { if k >= n { k - n } else { 16 } } else if k + n < 16 { k + n } else { 16 }) as u8
                    });
                    self.c.get(a).v128zero().shuffle(l);
                }
                Cvtdq2ps => {
                    self.c.get(b).simd(0xfa);
                }
                Cvtps2pd => {
                    self.c.get(b).simd(0x5f);
                }
                Cvtdq2pd => {
                    self.c.get(b).simd(0xfe);
                }
                Cvtpd2ps => {
                    self.c.get(b).simd(0x5e);
                }
                Cvttps2dq | Cvtps2dq => {
                    self.c.get(b);
                    if m == Cvtps2dq {
                        // rounded by MXCSR.RC: nearest, floor, ceil, trunc
                        let rc = L_A;
                        self.c.get(L_CPU).load32(offset_of!(Cpu, mxcsr) as u32).i32c(13).shr_u().i32c(3).and().set(rc);
                        self.c.simd(0x6a); // f32x4.nearest
                        for (k, op) in [(1, 0x68), (2, 0x67), (3, 0x69)] {
                            self.c.get(b).simd(op);
                            self.c.i32c(0).get(rc).i32c(k).ne().sub().simd(0x11);
                            self.c.simd(0x52);
                        }
                    }
                    self.c.set(a);
                    // out of range or NaN: 0x80000000 (wasm saturates)
                    self.c.get(a).simd(0xf8); // i32x4.trunc_sat_f32x4_s
                    self.c.i32c(i32::MIN).simd(0x11);
                    self.c.get(a).i32c((-2147483648.0f32).to_bits() as i32).simd(0x11).simd(0x46); // f32x4.ge
                    self.c.get(a).i32c(2147483648.0f32.to_bits() as i32).simd(0x11).simd(0x43); // f32x4.lt
                    self.c.simd(0x4e).simd(0x52);
                }
                Pmovzxbw | Pmovzxwd | Pmovzxdq | Pmovsxbw | Pmovsxwd | Pmovsxdq | Pmovzxbd | Pmovzxwq | Pmovsxbd | Pmovsxwq => {
                    // (i16x8 / i32x4 / i64x2 .extend_low_* _s or _u)
                    let steps: &[u32] = match m {
                        Pmovsxbw => &[0x87],
                        Pmovzxbw => &[0x89],
                        Pmovsxwd => &[0xa7],
                        Pmovzxwd => &[0xa9],
                        Pmovsxdq => &[0xc7],
                        Pmovzxdq => &[0xc9],
                        Pmovsxbd => &[0x87, 0xa7],
                        Pmovzxbd => &[0x89, 0xa9],
                        Pmovsxwq => &[0xa7, 0xc7],
                        _ => &[0xa9, 0xc9],
                    };
                    self.c.get(b);
                    for &s in steps {
                        self.c.simd(s);
                    }
                }
                _ => unreachable!("{m:?}"),
            }
        }
        self.c.set(L_V0);
        self.vput();
    }
}
