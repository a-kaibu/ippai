//! ENTER with a nesting level whose frame pointer cannot be read (fuzz/regressions/x86_jit64/
//! enter-faults-after-push): the fault names the frame pointer's address, the registers and
//! RIP are as before the instruction, and the RBP it pushed first stays written below RSP (as
//! on the hardware, and as the JIT leaves it).

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

const CODE: u32 = 0x1000;

#[test]
fn enter_32_faults_after_its_push() {
    let mut m = Flat(vec![0; 0x10000]);
    // enter 0x10, 3
    m.0[CODE as usize..CODE as usize + 4].copy_from_slice(&[0xc8, 0x10, 0x00, 0x03]);
    let mut cpu = Cpu::default();
    cpu.eip = CODE;
    cpu.eflags = 2;
    cpu.regs[4] = 0x8000;
    cpu.regs[5] = 0x2_0004; // [ebp-4] past the memory
    let before = cpu.clone();
    assert_eq!(step(&mut cpu, &mut m), Exit::Fault(Fault { addr: 0x2_0000, write: false }));
    assert_eq!((cpu.regs, cpu.eip, cpu.eflags), (before.regs, before.eip, before.eflags));
    assert_eq!(m.0[0x7ffc..0x8000], 0x2_0004u32.to_le_bytes(), "EBP pushed");
}

#[test]
fn enter_64_faults_after_its_push() {
    // the fuzz input's: enter 0x656d, 0x2e with RBP all ones
    let mut m = Flat(vec![0; 0x10000]);
    m.0[CODE as usize..CODE as usize + 4].copy_from_slice(&[0xc8, 0x6d, 0x65, 0x2e]);
    let mut cpu = Cpu { mode64: 1, eip: CODE, ..Cpu::default() };
    cpu.eflags = 2;
    cpu.set_gpr(4, 0x8000);
    cpu.set_gpr(5, u64::MAX);
    let before = cpu.clone();
    // (RBP - 8 above 4 GB: the 32-bit space's last bytes, never guest memory)
    assert_eq!(step(&mut cpu, &mut m), Exit::Fault(Fault { addr: 0xffff_fff7, write: false }));
    assert_eq!((cpu.regs, cpu.hi, cpu.r8, cpu.eip, cpu.eflags), (before.regs, before.hi, before.r8, before.eip, before.eflags));
    assert_eq!(m.0[0x7ff8..0x8000], [0xff; 8], "RBP pushed");
}
