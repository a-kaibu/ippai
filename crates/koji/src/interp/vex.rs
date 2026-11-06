//! VEX-encoded SSE (the 128-bit AVX/AVX2 forms that /arch:AVX2 code uses everywhere) and the
//! scalar/128-bit FMA3 instructions.
//!
//! A VEX instruction runs as its legacy form: `VEX_Vpaddw_xmm_xmm_xmmm128` is
//! `Paddw_xmm_xmmm128` (found by name, once). When the VEX form has the extra source register
//! (VEX.vvvv: `dst = src1 op src2`), the legacy form runs on a copy of src1 in the scratch
//! register XMM16 (`Cpu::vex_tmp`) and the result goes to dst. 256-bit forms aren't supported
//! (CPUID doesn't report AVX, so only code built for AVX unconditionally uses VEX at all).

use super::X;
use crate::{Exit, Memory};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};
use iced_x86::{Code, OpKind, Register};

macro_rules! tryf {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(f) => return Exit::Fault(f),
        }
    };
}

/// Per VEX `Code`: its legacy `Code` (INVALID: none), whether VEX.vvvv is the extra source,
/// and the FMA kind (see [`fma_kind`]).
struct Tables {
    legacy: Vec<Code>,
    nds: Vec<bool>,
    fma: Vec<u8>,
}

static TABLES: AtomicPtr<Tables> = AtomicPtr::new(core::ptr::null_mut());

fn tables() -> &'static Tables {
    let p = TABLES.load(Ordering::Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    let t = Box::into_raw(Box::new(build()));
    match TABLES.compare_exchange(core::ptr::null_mut(), t, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => unsafe { &*t },
        Err(other) => {
            drop(unsafe { Box::from_raw(t) });
            unsafe { &*other }
        }
    }
}

fn build() -> Tables {
    let n = Code::values().len();
    let mut by_name: BTreeMap<String, Code> = BTreeMap::new();
    for c in Code::values() {
        let s = format!("{c:?}");
        if !s.starts_with("VEX_") && !s.starts_with("EVEX_") && !s.starts_with("XOP_") && !s.starts_with("MVEX_") && !s.starts_with("D3NOW_") {
            by_name.insert(s, c);
        }
    }
    let mut t = Tables { legacy: vec![Code::INVALID; n], nds: vec![false; n], fma: vec![0; n] };
    for c in Code::values() {
        let s = format!("{c:?}");
        let Some(rest) = s.strip_prefix("VEX_V") else { continue };
        if rest.contains("ymm") {
            continue;
        }
        // (the 0F11 register form of vmovss/vmovsd is the 0F10 one with its operands swapped
        // in the encoding only)
        let toks: Vec<&str> = rest.split('_').filter(|t| !t.starts_with("0F")).collect();
        let mut mn = String::from(toks[0]);
        mn[..1].make_ascii_uppercase();
        let ops: Vec<&str> = toks[1..].to_vec();
        if let Some(k) = fma_kind(toks[0]) {
            t.fma[c as usize] = k;
            continue;
        }
        if ops.len() >= 2 {
            let mut d = ops.clone();
            d.remove(1);
            if let Some(l) = find(&by_name, &mn, &d) {
                t.legacy[c as usize] = l;
                t.nds[c as usize] = true;
                continue;
            }
        }
        if let Some(l) = find(&by_name, &mn, &ops) {
            t.legacy[c as usize] = l;
        }
    }
    t
}

/// The legacy code named `mn_ops`, or with a register operand that may also be memory there
/// (VEX `Vmovss_xmm_xmm_xmm` is `Movss_xmm_xmmm32`, `Vmovsd_m64_xmm` is `Movsd_xmmm64_xmm`).
fn find(by_name: &BTreeMap<String, Code>, mn: &str, ops: &[&str]) -> Option<Code> {
    let name = |ops: &[String]| -> String {
        let mut s = String::from(mn);
        for o in ops {
            s.push('_');
            s.push_str(o);
        }
        s
    };
    let ops: Vec<String> = ops.iter().map(|s| String::from(*s)).collect();
    if let Some(c) = by_name.get(&name(&ops)) {
        return Some(*c);
    }
    for i in 0..ops.len() {
        let alts: &[&str] = match ops[i].as_str() {
            "xmm" => &["xmmm32", "xmmm64", "xmmm128"],
            "m32" => &["xmmm32"],
            "m64" => &["xmmm64"],
            "m128" => &["xmmm128"],
            _ => &[],
        };
        for a in alts {
            let mut o = ops.clone();
            o[i] = String::from(*a);
            if let Some(c) = by_name.get(&name(&o)) {
                return Some(*c);
            }
        }
    }
    None
}

/// "fmadd213sd" -> kind | order << 3 | double << 5 | packed << 6 | 0x80
/// (kind: 0 madd, 1 msub, 2 nmadd, 3 nmsub, 4 maddsub, 5 msubadd; order: 0 132, 1 213, 2 231)
fn fma_kind(mn: &str) -> Option<u8> {
    let kinds = [("fmaddsub", 4), ("fmsubadd", 5), ("fnmadd", 2), ("fnmsub", 3), ("fmadd", 0), ("fmsub", 1)];
    let (kind, rest) = kinds.iter().find_map(|(p, k)| mn.strip_prefix(p).map(|r| (*k, r)))?;
    let order = match rest.get(..3)? {
        "132" => 0,
        "213" => 1,
        "231" => 2,
        _ => return None,
    };
    let ty = match rest.get(3..)? {
        "ss" => 0,
        "sd" => 1 << 5,
        "ps" => 1 << 6,
        "pd" => 3 << 5,
        _ => return None,
    };
    Some(0x80 | kind | order << 3 | ty)
}

impl<'a, M: Memory> X<'a, M> {
    pub(super) fn vex(&mut self) -> Exit {
        use iced_x86::Mnemonic::*;
        let ins = self.ins;
        match ins.mnemonic() {
            Vzeroupper => return self.done(),
            Vzeroall => {
                for i in 0..16 {
                    self.cpu.set_xmm(i, 0);
                }
                return self.done();
            }
            _ => {}
        }
        if vex_only(ins.mnemonic()) {
            return self.vex_only();
        }
        let t = tables();
        let code = ins.code() as usize;
        if t.fma[code] != 0 {
            return self.fma(t.fma[code]);
        }
        let lc = t.legacy[code];
        if lc == Code::INVALID {
            return Exit::InvalidOpcode;
        }
        let mut li = *ins;
        li.set_code(lc);
        if !t.nds[code] {
            let mut x = X { cpu: &mut *self.cpu, mem: &mut *self.mem, ins: &li, lock: None };
            return x.exec();
        }
        let (dst, src1) = (ins.op0_register(), ins.op1_register());
        if !dst.is_xmm() || !src1.is_xmm() {
            return Exit::InvalidOpcode;
        }
        self.cpu.vex_tmp = self.cpu.xmm(src1 as usize - Register::XMM0 as usize);
        li.set_op_kind(0, OpKind::Register);
        li.set_op0_register(Register::XMM16);
        for j in 1..li.op_count() {
            let k = ins.op_kind(j + 1);
            li.set_op_kind(j, k);
            if k == OpKind::Register {
                li.set_op_register(j, ins.op_register(j + 1));
            }
        }
        let exit = X { cpu: &mut *self.cpu, mem: &mut *self.mem, ins: &li, lock: None }.exec();
        if exit == Exit::Continue {
            self.cpu.set_xmm(dst as usize - Register::XMM0 as usize, self.cpu.vex_tmp);
        }
        exit
    }

    /// FMA3: a (dst), b (vvvv), c (xmm/mem), rounded once (libm's fma).
    fn fma(&mut self, k: u8) -> Exit {
        let ins = self.ins;
        if !ins.op0_register().is_xmm() {
            return Exit::InvalidOpcode;
        }
        let a = self.cpu.xmm(xi(ins.op0_register()));
        let b = self.cpu.xmm(xi(ins.op1_register()));
        let c = tryf!(self.read_v(2));
        let (x, y, z) = match (k >> 3) & 3 {
            0 => (a, c, b),
            1 => (b, a, c),
            _ => (b, c, a),
        };
        let kind = k & 7;
        let double = k & 0x20 != 0;
        let packed = k & 0x40 != 0;
        let lane = if double { 64 } else { 32 };
        let n = if packed { 128 / lane } else { 1 };
        let mut r = a; // scalar: the rest of dst stays
        for i in 0..n {
            // (maddsub: even lanes subtract; msubadd: odd lanes)
            let sub = match kind {
                1 | 3 => true,
                4 => i % 2 == 0,
                5 => i % 2 == 1,
                _ => false,
            };
            let neg = matches!(kind, 2 | 3);
            let bits = if double {
                let g = |v: u128| f64::from_bits((v >> (64 * i)) as u64);
                let (p, q, s) = (g(x), g(y), g(z));
                let v = if p.is_nan() || q.is_nan() || s.is_nan() {
                    [g(a), g(b), g(c)].into_iter().find(|v| v.is_nan()).unwrap().to_bits() | 1 << 51
                } else {
                    libm::fma(if neg { -p } else { p }, q, if sub { -s } else { s }).to_bits()
                };
                v as u128
            } else {
                let g = |v: u128| f32::from_bits((v >> (32 * i)) as u32);
                let (p, q, s) = (g(x), g(y), g(z));
                let v = if p.is_nan() || q.is_nan() || s.is_nan() {
                    [g(a), g(b), g(c)].into_iter().find(|v| v.is_nan()).unwrap().to_bits() | 1 << 22
                } else {
                    libm::fmaf(if neg { -p } else { p }, q, if sub { -s } else { s }).to_bits()
                };
                v as u128
            };
            let m: u128 = if double { u64::MAX as u128 } else { u32::MAX as u128 };
            r = (r & !(m << (lane * i))) | bits << (lane * i);
        }
        self.cpu.set_xmm(xi(ins.op0_register()), r);
        self.done()
    }

    /// VEX-only 128-bit instructions (AVX/AVX2, see [`vex_only`]).
    fn vex_only(&mut self) -> Exit {
        use iced_x86::Mnemonic::*;
        let ins = self.ins;
        let mn = ins.mnemonic();
        if (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_ymm()) {
            return Exit::InvalidOpcode;
        }
        let n = ins.op_count();
        match mn {
            Vtestps | Vtestpd => {
                let lane = if mn == Vtestps { 32 } else { 64 };
                let a = self.cpu.xmm(xi(ins.op0_register()));
                let b = tryf!(self.read_v(1));
                let sign = (0..128 / lane).fold(0u128, |m, i| m | 1u128 << (lane * i + lane - 1));
                let mut f = self.cpu.eflags & !crate::flags::STATUS;
                if a & b & sign == 0 {
                    f |= crate::flags::ZF;
                }
                if !a & b & sign == 0 {
                    f |= crate::flags::CF;
                }
                self.cpu.eflags = f;
                self.done()
            }
            Vmaskmovps | Vmaskmovpd | Vpmaskmovd | Vpmaskmovq => {
                let lane: u32 = if matches!(mn, Vmaskmovps | Vpmaskmovd) { 32 } else { 64 };
                let p = self.addr();
                if ins.op_kind(0) == OpKind::Memory {
                    // store: (m, mask, src)
                    let mask = self.cpu.xmm(xi(ins.op1_register()));
                    let v = self.cpu.xmm(xi(ins.op2_register()));
                    for i in 0..128 / lane {
                        if (mask >> (lane * i + lane - 1)) & 1 != 0 {
                            let q = p.wrapping_add(i * lane / 8);
                            tryf!(if lane == 32 { self.mem.write32(q, (v >> (32 * i)) as u32) } else { self.mem.write64(q, (v >> (64 * i)) as u64) });
                        }
                    }
                } else {
                    let mask = self.cpu.xmm(xi(ins.op1_register()));
                    let mut r = 0u128;
                    for i in 0..128 / lane {
                        if (mask >> (lane * i + lane - 1)) & 1 != 0 {
                            let q = p.wrapping_add(i * lane / 8);
                            let v = tryf!(if lane == 32 { self.mem.read32(q).map(|v| v as u64) } else { self.mem.read64(q) });
                            r |= (v as u128) << (lane * i);
                        }
                    }
                    self.cpu.set_xmm(xi(ins.op0_register()), r);
                }
                self.done()
            }
            _ => {
                // dst = f(src1, src2 [, imm/reg]) on whole registers
                let a = tryf!(self.read_v(1));
                let b = if n > 2 { tryf!(self.read_v(2)) } else { 0 };
                let imm = ins.immediate8() as u32;
                let lanes = |lane: u32, f: &dyn Fn(u32, u64, u64) -> u64| -> u128 {
                    let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
                    (0..128 / lane).fold(0u128, |r, i| {
                        r | ((f(i, ((a >> (lane * i)) & m) as u64, ((b >> (lane * i)) & m) as u64) as u128) & m) << (lane * i)
                    })
                };
                let bcast = |lane: u32| -> u128 {
                    let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
                    (0..128 / lane).fold(0u128, |r, i| r | (a & m) << (lane * i))
                };
                let r = match mn {
                    Vpblendd => lanes(32, &|i, x, y| if imm >> i & 1 != 0 { y } else { x }),
                    Vpblendvb | Vblendvps | Vblendvpd => {
                        let mask = self.cpu.xmm(xi(ins.op3_register()));
                        let lane = match mn { Vpblendvb => 8, Vblendvps => 32, _ => 64 };
                        lanes(lane, &|i, x, y| if (mask >> (lane * i + lane - 1)) & 1 != 0 { y } else { x })
                    }
                    Vpbroadcastb => bcast(8),
                    Vpbroadcastw => bcast(16),
                    Vpbroadcastd | Vbroadcastss => bcast(32),
                    Vpbroadcastq => bcast(64),
                    Vpsllvd => lanes(32, &|_, x, y| if y < 32 { x << y } else { 0 }),
                    Vpsllvq => lanes(64, &|_, x, y| if y < 64 { x << y } else { 0 }),
                    Vpsrlvd => lanes(32, &|_, x, y| if y < 32 { x >> y } else { 0 }),
                    Vpsrlvq => lanes(64, &|_, x, y| if y < 64 { x >> y } else { 0 }),
                    Vpsravd => lanes(32, &|_, x, y| ((x as u32 as i32) >> y.min(31)) as u32 as u64),
                    Vpermilps if n == 3 && ins.op_kind(2) == OpKind::Immediate8 => {
                        lanes(32, &|i, _, _| (a >> (32 * ((imm >> (2 * i)) & 3))) as u32 as u64)
                    }
                    Vpermilps => lanes(32, &|_, _, y| (a >> (32 * (y & 3))) as u32 as u64),
                    Vpermilpd if n == 3 && ins.op_kind(2) == OpKind::Immediate8 => {
                        lanes(64, &|i, _, _| (a >> (64 * ((imm >> i) & 1))) as u64)
                    }
                    _ => lanes(64, &|_, _, y| (a >> (64 * ((y >> 1) & 1))) as u64),
                };
                self.cpu.set_xmm(xi(ins.op0_register()), r);
                self.done()
            }
        }
    }
}

fn vex_only(mn: iced_x86::Mnemonic) -> bool {
    use iced_x86::Mnemonic::*;
    matches!(
        mn,
        Vpblendd | Vpbroadcastb | Vpbroadcastw | Vpbroadcastd | Vpbroadcastq | Vbroadcastss | Vpsllvd | Vpsllvq | Vpsrlvd
            | Vpsrlvq | Vpsravd | Vpermilps | Vpermilpd | Vtestps | Vtestpd | Vmaskmovps | Vmaskmovpd | Vpmaskmovd
            | Vpmaskmovq | Vpblendvb | Vblendvps | Vblendvpd
    )
}

#[inline]
fn xi(r: Register) -> usize {
    r as usize - Register::XMM0 as usize
}

/// The legacy form of a VEX 128-bit instruction that runs as one (see the module docs), and
/// whether VEX.vvvv is its extra first source (`dst = src1 op src2`); None for the FMA and
/// VEX-only ones and for 256-bit forms.
pub(crate) fn legacy_of(code: Code) -> Option<(Code, bool)> {
    let t = tables();
    let c = code as usize;
    (t.fma[c] == 0 && t.legacy[c] != Code::INVALID && !vex_only(code.mnemonic())).then(|| (t.legacy[c], t.nds[c]))
}
