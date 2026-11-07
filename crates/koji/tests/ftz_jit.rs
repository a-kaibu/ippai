//! MXCSR.FTZ / DAZ and the JIT (#70): translated SSE arithmetic has no flush, so a program
//! loading either (LDMXCSR, FXRSTOR) turns koji::jit::MX_EXACT on, whose guards send denormal
//! operands and results that may be tiny to the interpreter; without it no SSE is translated
//! in those modes. One test: the mode is the process's.

use koji::{jit, step, Cpu, Exit, Fault, Memory};

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

const CODE: u32 = 0x10000;
const LDMXCSR: &[u8] = &[0x0f, 0xae, 0x15, 0x00, 0x20, 0x00, 0x00]; // ldmxcsr [0x2000]
const FXRSTOR: &[u8] = &[0x0f, 0xae, 0x0d, 0x00, 0x20, 0x00, 0x00]; // fxrstor [0x2000]

fn run(code: &[u8], mxcsr_at: usize, mx: u32) -> Cpu {
    let mut m = Flat(vec![0; 0x20000]);
    m.0[CODE as usize..CODE as usize + code.len()].copy_from_slice(code);
    m.0[0x2027] = 0; // (FXRSTOR's image: x87 control word etc. zero is fine)
    m.0[0x2000 + mxcsr_at..0x2004 + mxcsr_at].copy_from_slice(&mx.to_le_bytes());
    if mxcsr_at != 0 {
        m.0[0x201c..0x2020].copy_from_slice(&0xffffu32.to_le_bytes()); // MXCSR_MASK
    }
    let mut cpu = Cpu::default();
    cpu.eip = CODE;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    cpu.mxcsr = 0x1f80;
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    cpu
}

#[test]
fn ftz_and_daz_take_the_checked_path() {
    assert!(!jit::mx_exact());
    for mx in [0x9f80, 0x1fc0, 0x9fc0] {
        assert!(!jit::sse_ok(mx), "{mx:#x}: no unchecked translation");
    }
    assert!(jit::sse_ok(0x1f80));
    // rounding and exception masks alone don't switch
    assert_eq!(run(LDMXCSR, 0, 0x7f80).mxcsr, 0x7f80);
    assert_eq!(run(LDMXCSR, 0, 0x1f00).mxcsr, 0x1f00);
    assert!(!jit::mx_exact() && !jit::mx_switched());

    assert_eq!(run(LDMXCSR, 0, 0x9f80).mxcsr, 0x9f80);
    assert!(jit::mx_exact() && jit::mx_switched() && !jit::mx_switched(), "FTZ switches, once");
    for mx in [0x9f80, 0x1fc0, 0x9fc0] {
        assert!(jit::sse_ok(mx), "{mx:#x}: translated with the guards");
    }

    jit::MX_EXACT.store(false, core::sync::atomic::Ordering::Relaxed);
    assert_eq!(run(FXRSTOR, 24, 0x1fc0).mxcsr, 0x1fc0);
    assert!(jit::mx_exact() && jit::mx_switched(), "DAZ from FXRSTOR switches");
}
