//! The x87 status word's exception flags (#70) from FLD m32 / m64 (an SNaN is IE, a denormal
//! DE), then FABS, FCHS or FRNDINT (a fraction rounded off is PE, C1 when rounded away from
//! zero), in koji's interpreter against the host CPU, in every rounding mode: the special
//! values (denormals, NaNs of both kinds, infinities, halves) and random bits.
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

/// (name, the op's bytes: empty for the load alone)
const OPS: [(&str, &[u8]); 4] = [("fld", &[]), ("fabs", &[0xd9, 0xe1]), ("fchs", &[0xd9, 0xe0]), ("frndint", &[0xd9, 0xfc])];

/// fninit; fldcw [0x2010]; fld dword/qword [0x2000]; op; fnstsw [0x2020]
fn koji_sw(op: &[u8], v: u64, single: bool, cw: u16) -> u16 {
    let mut code = vec![0xdb, 0xe3, 0xd9, 0x2d, 0x10, 0x20, 0, 0, if single { 0xd9 } else { 0xdd }, 0x05, 0, 0x20, 0, 0];
    code.extend_from_slice(op);
    code.extend_from_slice(&[0xdd, 0x3d, 0x20, 0x20, 0, 0]);
    let mut m = Flat(vec![0; 0x10000]);
    m.0[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    m.0[0x2000..0x2008].copy_from_slice(&v.to_le_bytes());
    m.0[0x2010..0x2012].copy_from_slice(&cw.to_le_bytes());
    let mut cpu = Cpu::default();
    cpu.eip = 0x1000;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    while cpu.eip < 0x1000 + code.len() as u32 {
        assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    }
    u16::from_le_bytes([m.0[0x2020], m.0[0x2021]])
}

macro_rules! host1 {
    ($load:literal, $ins:literal, $v:expr, $cw:expr) => {{
        let v: u64 = $v;
        let cw: u16 = $cw;
        let mut sw: u16 = 0;
        unsafe {
            asm!("fninit", "fldcw [{c}]", concat!($load, " [{v}]"), $ins, "fnstsw [{s}]", "fninit",
                 c = in(reg) &cw, v = in(reg) &v, s = in(reg) &mut sw, out("st(0)") _);
        }
        sw
    }};
}

fn host_sw(name: &str, v: u64, single: bool, cw: u16) -> u16 {
    match (name, single) {
        ("fld", true) => host1!("fld dword ptr", "nop", v, cw),
        ("fld", false) => host1!("fld qword ptr", "nop", v, cw),
        ("fabs", true) => host1!("fld dword ptr", "fabs", v, cw),
        ("fabs", false) => host1!("fld qword ptr", "fabs", v, cw),
        ("fchs", true) => host1!("fld dword ptr", "fchs", v, cw),
        ("fchs", false) => host1!("fld qword ptr", "fchs", v, cw),
        ("frndint", true) => host1!("fld dword ptr", "frndint", v, cw),
        _ => host1!("fld qword ptr", "frndint", v, cw),
    }
}

#[test]
fn load_and_round_flags_match_the_host() {
    let specials64: &[u64] = &[
        0, 1 << 63, 1, (1 << 63) | 1, (1 << 52) - 1, 1 << 52, 0x3fe0 << 48, 0xbfe0 << 48, 0x3ff8 << 48, 0x4004 << 48,
        0x3fef_ffff_ffff_ffff, 0x3ff0_0000_0000_0001, 0x4330 << 48, 0x7ff0 << 48, 0xfff0 << 48, 0x7ff0_0000_0000_0001,
        0x7ff4 << 48, 0x7ff8 << 48, 0xfff8_0000_0000_0001, 0xc004_0000_0000_0001,
    ];
    let specials32: &[u64] = &[
        0, 0x8000_0000, 1, 0x8000_0001, 0x7f_ffff, 0x80_0000, 0x3f00_0000, 0xbfc0_0000, 0x3f7f_ffff, 0x4b00_0000, 0x7f80_0000,
        0x7f80_0001, 0x7fa0_0000, 0x7fc0_0000, 0xffc0_0001,
    ];
    let mut r = 0x1234_5678_9abc_def1u64;
    let mut bad = Vec::new();
    let mut n = 0;
    for cw in [0x027f, 0x067f, 0x0a7f, 0x0e7f, 0x037f] {
        for single in [false, true] {
            let mut vals: Vec<u64> = if single { specials32.to_vec() } else { specials64.to_vec() };
            for _ in 0..300 {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                // near the integers, the denormals and the NaNs
                let x = match r % 3 {
                    0 => r >> 12 & if single { 0x8fff_ffff } else { 0x800f_ffff_ffff_ffff },
                    1 => (r & if single { 0x803f_ffff } else { 0x800f_ffff_ffff_ffff }) | if single { 0x3f00_0000 + ((r >> 40 & 15) << 23) } else { (0x3fe + (r >> 56 & 31)) << 52 },
                    _ => r | if single { 0x7f80_0000 } else { 0x7ff0 << 48 },
                };
                vals.push(if single { x & 0xffff_ffff } else { x });
            }
            for &v in &vals {
                for (name, op) in OPS {
                    let mask = if name == "frndint" { 0x3f | 0x200 } else { 0x3f };
                    let (want, got) = (host_sw(name, v, single, cw) & mask, koji_sw(op, v, single, cw) & mask);
                    n += 1;
                    if want != got && bad.len() < 20 {
                        bad.push(format!("{name} {} cw {cw:04x} {v:#x}: koji {got:04x}, cpu {want:04x}", if single { "m32" } else { "m64" }));
                    }
                }
            }
        }
    }
    assert!(n > 10_000);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
