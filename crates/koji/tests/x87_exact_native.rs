//! FILD'ed integers added, subtracted and multiplied on the x87 stack, against the host CPU:
//! an int64 beyond 2^53 is exact in the 64-bit register, so the hardware rounds the exact
//! result once (to 53 bits at the precision MSVC programs run with, by the rounding control);
//! rounding the operand to a double first and the result again differs (winref
//! host_intops "filds" caught it). Every rounding control, 53- and 64-bit precision.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use core::arch::asm;
use koji::{step, Cpu, Exit, Fault, Memory};

struct Flat(Vec<u8>);
impl Memory for Flat {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        match self.0.get_mut(a as usize) {
            Some(b) => {
                *b = v;
                Ok(())
            }
            None => Err(Fault { addr: a, write: true }),
        }
    }
}

/// op: 0 faddp, 1 fsubp, 2 fsubrp, 3 fmulp (ST(1) op ST(0), popped); returns the double stored.
fn host(cw: u16, q: i64, w: i32, op: u32) -> u64 {
    let mut d = 0u64;
    let mut old = 0u16;
    unsafe {
        match op {
            0 => asm!("fnstcw [{o}]", "fldcw [{c}]", "fild qword ptr [{q}]", "fild dword ptr [{w}]", "faddp", "fstp qword ptr [{d}]", "fldcw [{o}]",
                      o = in(reg) &mut old, c = in(reg) &cw, q = in(reg) &q, w = in(reg) &w, d = in(reg) &mut d),
            1 => asm!("fnstcw [{o}]", "fldcw [{c}]", "fild qword ptr [{q}]", "fild dword ptr [{w}]", "fsubp", "fstp qword ptr [{d}]", "fldcw [{o}]",
                      o = in(reg) &mut old, c = in(reg) &cw, q = in(reg) &q, w = in(reg) &w, d = in(reg) &mut d),
            2 => asm!("fnstcw [{o}]", "fldcw [{c}]", "fild qword ptr [{q}]", "fild dword ptr [{w}]", "fsubrp", "fstp qword ptr [{d}]", "fldcw [{o}]",
                      o = in(reg) &mut old, c = in(reg) &cw, q = in(reg) &q, w = in(reg) &w, d = in(reg) &mut d),
            _ => asm!("fnstcw [{o}]", "fldcw [{c}]", "fild qword ptr [{q}]", "fild dword ptr [{w}]", "fmulp", "fstp qword ptr [{d}]", "fldcw [{o}]",
                      o = in(reg) &mut old, c = in(reg) &cw, q = in(reg) &q, w = in(reg) &w, d = in(reg) &mut d),
        }
    }
    d
}

fn koji(cw: u16, q: i64, w: i32, op: u32) -> u64 {
    let mut m = Flat(vec![0; 0x10000]);
    m.0[0x2000..0x2002].copy_from_slice(&cw.to_le_bytes());
    m.0[0x2008..0x2010].copy_from_slice(&q.to_le_bytes());
    m.0[0x2010..0x2014].copy_from_slice(&w.to_le_bytes());
    // (DE E9: ST(1) - ST(0), what the operandless "fsubp" above assembles to)
    let arith: [u8; 2] = match op {
        0 => [0xde, 0xc1],
        1 => [0xde, 0xe9],
        2 => [0xde, 0xe1],
        _ => [0xde, 0xc9],
    };
    let mut code = vec![0xd9, 0x2d, 0x00, 0x20, 0, 0, 0xdf, 0x2d, 0x08, 0x20, 0, 0, 0xdb, 0x05, 0x10, 0x20, 0, 0];
    code.extend(arith);
    code.extend([0xdd, 0x1d, 0x18, 0x20, 0, 0]);
    m.0[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    let mut cpu = Cpu::default();
    cpu.eip = 0x1000;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    while cpu.eip < 0x1000 + code.len() as u32 {
        assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    }
    u64::from_le_bytes(m.0[0x2018..0x2020].try_into().unwrap())
}

#[test]
fn exact_integers_round_once() {
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for i in 0..200_000 {
        let r = rnd();
        let q = (rnd() as i64) >> (r % 16);
        let w = match r >> 60 {
            0 => 1,
            1 => -1,
            2 => i32::MIN,
            _ => (rnd() >> (r >> 8 & 31)) as i32,
        };
        let op = (r >> 16) as u32 % 4;
        let rc = (r >> 20) as u16 % 4;
        // 53-bit precision mostly (MSVC's), 64-bit sometimes (the result double rounds on
        // hardware there too: to 64 bits, then by FSTP to 53)
        let pc = if i % 4 == 0 { 3 } else { 2 };
        let cw = 0x7f | pc << 8 | rc << 10;
        if pc == 3 {
            continue; // (koji keeps 53-bit registers: only the exact result stays exact)
        }
        assert_eq!(koji(cw, q, w, op), host(cw, q, w, op), "cw {cw:#x} q {q} w {w} op {op}");
    }
}
