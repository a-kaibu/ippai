//! The 256-bit forms of AVX, AVX2 and FMA3 (VEX.256), and the AVX2 gathers.
//!
//! A YMM register is its XMM register (the low half) and `Cpu::ymmh` (the upper half). Most
//! 256-bit instructions work on the two 128-bit lanes independently, as their VEX.128 form on
//! each: they run as that form twice ([`X::split`]), the second time with the upper halves
//! swapped into the XMM registers and a 256-bit memory operand 16 bytes on. The ones that
//! cross lanes (broadcasts, permutes, inserts and extracts, widening and narrowing
//! conversions, mask extraction, PTEST) are done here directly.

use super::X;
use crate::{Exit, Fault, Memory};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};
use iced_x86::{Code, Instruction, Mnemonic, OpKind, Register};

macro_rules! tryf {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(f) => return Exit::Fault(f),
        }
    };
}

/// Per VEX.256 `Code`: its VEX.128 `Code` (INVALID: none), found by name
/// (`VEX_Vaddps_ymm_ymm_ymmm256` -> `VEX_Vaddps_xmm_xmm_xmmm128`).
static HALF: AtomicPtr<Vec<Code>> = AtomicPtr::new(core::ptr::null_mut());

fn half() -> &'static [Code] {
    let p = HALF.load(Ordering::Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    let mut by_name: BTreeMap<String, Code> = BTreeMap::new();
    for c in Code::values() {
        let s = format!("{c:?}");
        if s.starts_with("VEX_") && !s.contains("ymm") {
            by_name.insert(s, c);
        }
    }
    let mut t = vec![Code::INVALID; Code::values().len()];
    for c in Code::values() {
        let s = format!("{c:?}");
        if s.starts_with("VEX_") && s.contains("ymm") {
            let x = s.replace("ymmm256", "xmmm128").replace("ymm", "xmm").replace("m256", "m128");
            // (VMOVDDUP's 128-bit form reads 64 bits)
            if let Some(&h) = by_name.get(&x).or_else(|| by_name.get(&x.replace("xmmm128", "xmmm64"))) {
                t[c as usize] = h;
            }
        }
    }
    let t = Box::into_raw(Box::new(t));
    match HALF.compare_exchange(core::ptr::null_mut(), t, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => unsafe { &*t },
        Err(other) => {
            drop(unsafe { Box::from_raw(t) });
            unsafe { &*other }
        }
    }
}

/// Register number of an XMM or YMM register.
#[inline]
fn vi(r: Register) -> usize {
    if r.is_ymm() {
        r as usize - Register::YMM0 as usize
    } else {
        r as usize - Register::XMM0 as usize
    }
}

#[inline]
fn lane(v: &[u128; 2], bits: u32, i: u32) -> u64 {
    let per = 128 / bits;
    let w = v[(i / per) as usize] >> (bits * (i % per));
    if bits == 64 { w as u64 } else { w as u64 & ((1u64 << bits) - 1) }
}

#[inline]
fn set_lane(v: &mut [u128; 2], bits: u32, i: u32, x: u64) {
    let per = 128 / bits;
    let m: u128 = if bits == 64 { u64::MAX as u128 } else { (1u128 << bits) - 1 };
    let s = bits * (i % per);
    let h = &mut v[(i / per) as usize];
    *h = (*h & !(m << s)) | ((x as u128 & m) << s);
}

/// `v`'s low `bits` repeated over 128 bits.
fn bcast(v: u128, bits: u32) -> u128 {
    if bits == 128 {
        return v;
    }
    let m: u128 = if bits == 64 { u64::MAX as u128 } else { (1u128 << bits) - 1 };
    (0..128 / bits).fold(0, |r, i| r | (v & m) << (bits * i))
}

impl<'a, M: Memory> X<'a, M> {
    /// Operand `i` as 256 bits (an XMM register or a smaller memory operand: the low half).
    fn read_y(&mut self, i: u32) -> Result<[u128; 2], Fault> {
        match self.ins.op_kind(i) {
            OpKind::Register if self.ins.op_register(i).is_ymm() => Ok(self.cpu.ymm(vi(self.ins.op_register(i)))),
            OpKind::Memory if self.ins.memory_size().size() == 32 => {
                let a = self.addr();
                let r = |x: &mut Self, a: u32| -> Result<u128, Fault> { Ok(x.mem.read64(a)? as u128 | (x.mem.read64(a.wrapping_add(8))? as u128) << 64) };
                Ok([r(self, a)?, r(self, a.wrapping_add(16))?])
            }
            _ => Ok([self.read_v(i)?, 0]),
        }
    }

    fn snapshot(&self) -> [[u128; 2]; 17] {
        core::array::from_fn(|i| self.cpu.ymm(i))
    }

    fn restore(&mut self, s: &[[u128; 2]; 17]) {
        for (i, v) in s.iter().enumerate() {
            self.cpu.set_ymm(i, *v);
        }
    }

    fn swap_halves(&mut self) {
        for i in 0..17 {
            let x = self.cpu.xmm(i);
            self.cpu.set_xmm(i, self.cpu.ymmh[i]);
            self.cpu.ymmh[i] = x;
        }
    }

    /// Runs `li` (a 128-bit form), keeping every upper half as it was (VEX.128 clears its
    /// destination's).
    fn run_keep_h(&mut self, li: &Instruction) -> Exit {
        let h = self.cpu.ymmh;
        let e = X { cpu: &mut *self.cpu, mem: &mut *self.mem, ins: li, lock: None, daz: None, tiny: 0 }.exec();
        self.cpu.ymmh = h;
        e
    }

    /// Legacy SSE `code` (an xmm, xmm/m form) on `v` alone, in the scratch register.
    fn unary(&mut self, code: Code, v: u128) -> Result<u128, Exit> {
        let mut li = *self.ins;
        li.set_code(code);
        li.set_op0_kind(OpKind::Register);
        li.set_op0_register(Register::XMM16);
        li.set_op1_kind(OpKind::Register);
        li.set_op1_register(Register::XMM16);
        self.cpu.vex_tmp = v;
        let e = self.run_keep_h(&li);
        if e != Exit::Continue {
            return Err(e);
        }
        Ok(self.cpu.vex_tmp)
    }

    pub(super) fn vex256(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let mn = ins.mnemonic();
        let imm = ins.immediate8() as u32;
        let dst = || vi(ins.op0_register());
        match mn {
            Vbroadcastss | Vbroadcastsd | Vbroadcastf128 | Vbroadcasti128 | Vpbroadcastb | Vpbroadcastw | Vpbroadcastd | Vpbroadcastq => {
                let v = tryf!(self.read_y(1))[0];
                let bits = match mn {
                    Vpbroadcastb => 8,
                    Vpbroadcastw => 16,
                    Vbroadcastss | Vpbroadcastd => 32,
                    Vbroadcastsd | Vpbroadcastq => 64,
                    _ => 128,
                };
                let r = bcast(v, bits);
                self.cpu.set_ymm(dst(), [r, r]);
            }
            Vinsertf128 | Vinserti128 => {
                let mut a = tryf!(self.read_y(1));
                a[(imm & 1) as usize] = tryf!(self.read_y(2))[0];
                self.cpu.set_ymm(dst(), a);
            }
            Vextractf128 | Vextracti128 => {
                let v = tryf!(self.read_y(1))[(imm & 1) as usize];
                if ins.op0_kind() == OpKind::Register {
                    self.cpu.set_ymm(dst(), [v, 0]);
                } else {
                    tryf!(self.write_v(0, v));
                }
            }
            Vperm2f128 | Vperm2i128 => {
                let (a, b) = (tryf!(self.read_y(1)), tryf!(self.read_y(2)));
                let pick = |c: u32| if c & 8 != 0 { 0 } else { [a[0], a[1], b[0], b[1]][(c & 3) as usize] };
                self.cpu.set_ymm(dst(), [pick(imm), pick(imm >> 4)]);
            }
            Vpermd | Vpermps => {
                // (indices in the first source, the table in the second)
                let (idx, t) = (tryf!(self.read_y(1)), tryf!(self.read_y(2)));
                let mut r = [0; 2];
                for i in 0..8 {
                    set_lane(&mut r, 32, i, lane(&t, 32, lane(&idx, 32, i) as u32 & 7));
                }
                self.cpu.set_ymm(dst(), r);
            }
            Vpermq | Vpermpd => {
                let s = tryf!(self.read_y(1));
                let mut r = [0; 2];
                for i in 0..4 {
                    set_lane(&mut r, 64, i, lane(&s, 64, (imm >> (2 * i)) & 3));
                }
                self.cpu.set_ymm(dst(), r);
            }
            Vptest | Vtestps | Vtestpd => {
                let (a, b) = (tryf!(self.read_y(0)), tryf!(self.read_y(1)));
                let sign: u128 = match mn {
                    Vtestps => bcast(1 << 31, 32),
                    Vtestpd => bcast(1 << 63, 64),
                    _ => u128::MAX,
                };
                let z = (a[0] & b[0] | a[1] & b[1]) & sign == 0;
                let c = (!a[0] & b[0] | !a[1] & b[1]) & sign == 0;
                self.set_status(if z { crate::flags::ZF } else { 0 } | if c { crate::flags::CF } else { 0 });
            }
            Vmovmskps | Vmovmskpd | Vpmovmskb => {
                let s = tryf!(self.read_y(1));
                let bits = match mn {
                    Vmovmskps => 32,
                    Vmovmskpd => 64,
                    _ => 8,
                };
                let r = (0..256 / bits).fold(0u64, |r, i| r | (lane(&s, bits, i) >> (bits - 1)) << i);
                self.set_reg(ins.op0_register(), r);
            }
            Vpmovsxbw | Vpmovsxbd | Vpmovsxbq | Vpmovsxwd | Vpmovsxwq | Vpmovsxdq | Vpmovzxbw | Vpmovzxbd | Vpmovzxbq | Vpmovzxwd
            | Vpmovzxwq | Vpmovzxdq | Vcvtdq2pd | Vcvtps2pd => {
                // widening: each half from the next part of the source
                let (code, shift) = match mn {
                    Vpmovsxbw => (Code::Pmovsxbw_xmm_xmmm64, 64),
                    Vpmovsxbd => (Code::Pmovsxbd_xmm_xmmm32, 32),
                    Vpmovsxbq => (Code::Pmovsxbq_xmm_xmmm16, 16),
                    Vpmovsxwd => (Code::Pmovsxwd_xmm_xmmm64, 64),
                    Vpmovsxwq => (Code::Pmovsxwq_xmm_xmmm32, 32),
                    Vpmovsxdq => (Code::Pmovsxdq_xmm_xmmm64, 64),
                    Vpmovzxbw => (Code::Pmovzxbw_xmm_xmmm64, 64),
                    Vpmovzxbd => (Code::Pmovzxbd_xmm_xmmm32, 32),
                    Vpmovzxbq => (Code::Pmovzxbq_xmm_xmmm16, 16),
                    Vpmovzxwd => (Code::Pmovzxwd_xmm_xmmm64, 64),
                    Vpmovzxwq => (Code::Pmovzxwq_xmm_xmmm32, 32),
                    Vpmovzxdq => (Code::Pmovzxdq_xmm_xmmm64, 64),
                    Vcvtdq2pd => (Code::Cvtdq2pd_xmm_xmmm64, 64),
                    _ => (Code::Cvtps2pd_xmm_xmmm64, 64),
                };
                let s = tryf!(self.read_y(1))[0];
                let lo = match self.unary(code, s) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                let hi = match self.unary(code, s >> shift) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                self.cpu.set_ymm(dst(), [lo, hi]);
            }
            Vcvtpd2ps | Vcvtpd2dq | Vcvttpd2dq => {
                // narrowing: four doubles to four floats or dwords in the XMM register
                let code = match mn {
                    Vcvtpd2ps => Code::Cvtpd2ps_xmm_xmmm128,
                    Vcvtpd2dq => Code::Cvtpd2dq_xmm_xmmm128,
                    _ => Code::Cvttpd2dq_xmm_xmmm128,
                };
                let s = tryf!(self.read_y(1));
                let lo = match self.unary(code, s[0]) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                let hi = match self.unary(code, s[1]) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                self.cpu.set_ymm(dst(), [(lo as u64 as u128) | (hi as u64 as u128) << 64, 0]);
            }
            _ => return self.split(),
        }
        self.done()
    }

    /// A lane-wise 256-bit instruction as its VEX.128 form on each lane.
    fn split(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let h = half()[ins.code() as usize];
        if h == Code::INVALID {
            return Exit::InvalidOpcode;
        }
        let snap = self.snapshot();
        let mut li = *ins;
        li.set_code(h);
        // XMM operands of the 256-bit form (shift counts) are the same for both lanes
        let mut shared: [Option<usize>; 4] = [None; 4];
        for i in 0..ins.op_count() {
            if ins.op_kind(i) == OpKind::Register {
                let r = ins.op_register(i);
                if r.is_ymm() {
                    li.set_op_register(i, Register::XMM0 + vi(r) as u32);
                } else if r.is_xmm() {
                    shared[i as usize] = Some(vi(r));
                }
            }
        }
        let e = self.run_keep_h(&li);
        if e != Exit::Continue {
            self.restore(&snap);
            return e;
        }
        let mut hi = li;
        if (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory) && ins.memory_size().size() == 32 {
            hi.set_memory_displacement64(hi.memory_displacement64().wrapping_add(16));
        }
        // immediates with a part per lane
        let imm = ins.immediate8();
        let imm_hi = match ins.mnemonic() {
            Vblendps | Vpblendd => Some(imm >> 4),
            Vblendpd | Vshufpd => Some(imm >> 2),
            Vpermilpd if ins.op_count() == 3 && ins.op2_kind() == OpKind::Immediate8 => Some(imm >> 2),
            Vmpsadbw => Some(imm >> 3),
            _ => None,
        };
        if let Some(v) = imm_hi {
            hi.set_immediate8(v);
        }
        self.swap_halves();
        // a shared XMM operand goes in a register the instruction doesn't name (its number may
        // also be a YMM operand's, whose upper half is in that register now)
        let used: u32 = (0..ins.op_count()).filter(|&i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_vector_register()).fold(0, |m, i| m | 1 << vi(ins.op_register(i)));
        let mut free = (0..16).filter(|f| used & 1 << f == 0);
        let mut moved: [Option<(usize, u128)>; 4] = [None; 4];
        for (i, r) in shared.iter().enumerate() {
            if let Some(r) = r {
                let f = free.next().unwrap();
                moved[i] = Some((f, self.cpu.xmm(f)));
                self.cpu.set_xmm(f, snap[*r][0]);
                hi.set_op_register(i as u32, Register::XMM0 + f as u32);
            }
        }
        let e = self.run_keep_h(&hi);
        for (f, v) in moved.iter().flatten() {
            self.cpu.set_xmm(*f, *v);
        }
        self.swap_halves();
        if e != Exit::Continue {
            self.restore(&snap);
        }
        e
    }

    /// VGATHER*/VPGATHER*: each element whose mask element has its top bit set is loaded from
    /// base + index element * scale + displacement (and its mask element cleared, so a fault
    /// part way leaves what was done); then the mask is all clear and the destination's
    /// elements past the count are zero.
    pub(super) fn gather(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let mn = ins.mnemonic();
        let (d, mk, x) = (ins.op0_register(), ins.op2_register(), ins.memory_index());
        let qidx = matches!(mn, Vgatherqps | Vgatherqpd | Vpgatherqd | Vpgatherqq);
        let ibits = if qidx { 64 } else { 32 };
        let dbits = if matches!(mn, Vgatherdpd | Vgatherqpd | Vpgatherdq | Vpgatherqq) { 64 } else { 32 };
        let xbytes = if x.is_ymm() { 32 } else { 16 };
        let dbytes = if d.is_ymm() { 32 } else { 16 };
        let n = (xbytes * 8 / ibits).min(dbytes * 8 / dbits);
        let idx = self.cpu.ymm(vi(x));
        let mut dv = self.cpu.ymm(vi(d));
        let mut mv = self.cpu.ymm(vi(mk));
        let base = if ins.memory_base() == Register::None { 0 } else { self.get_reg(ins.memory_base()) };
        let disp = ins.memory_displacement64();
        let scale = ins.memory_index_scale() as u64;
        for i in 0..n {
            if lane(&mv, dbits, i) >> (dbits - 1) == 0 {
                continue;
            }
            let ix = lane(&idx, ibits, i);
            let ix = if ibits == 32 { ix as u32 as i32 as i64 as u64 } else { ix };
            let mut a = base.wrapping_add(disp).wrapping_add(ix.wrapping_mul(scale));
            if !self.m64() {
                a &= 0xffff_ffff;
            }
            let a = super::lin(a.wrapping_add(self.seg_base()), self.m64());
            let v = match if dbits == 32 { self.mem.read32(a).map(|v| v as u64) } else { self.mem.read64(a) } {
                Ok(v) => v,
                Err(f) => {
                    self.cpu.set_ymm(vi(d), dv);
                    self.cpu.set_ymm(vi(mk), mv);
                    return Exit::Fault(f);
                }
            };
            set_lane(&mut dv, dbits, i, v);
            set_lane(&mut mv, dbits, i, 0);
        }
        for i in n..256 / dbits {
            set_lane(&mut dv, dbits, i, 0);
        }
        if !d.is_ymm() {
            dv[1] = 0;
        }
        self.cpu.set_ymm(vi(d), dv);
        self.cpu.set_ymm(vi(mk), [0, 0]);
        self.done()
    }
}
