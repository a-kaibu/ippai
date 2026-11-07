//! The single-step trap (TF) as x86 has it and Windows reports it (tests/winref seh probe):
//! the trap comes after an instruction that *began* with TF set, with EIP past it; an
//! instruction that faults or interrupts reports that instead; POPF setting TF traps only after
//! the next instruction, POPF clearing it still traps after itself; a run with TF set changes
//! nothing else (registers, flags, memory) than a run without.

use koji::{flags::TF, step, Cpu, Exit, Fault, Memory};

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

fn setup(code: &[u8]) -> (Cpu, Flat) {
    let mut m = Flat(vec![0; 0x10000]);
    m.0[CODE as usize..CODE as usize + code.len()].copy_from_slice(code);
    let mut cpu = Cpu::default();
    cpu.eip = CODE;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    // (no trap range in the way)
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    (cpu, m)
}

#[test]
fn popf_setting_tf_traps_after_the_next_instruction() {
    // pushfd; or dword [esp], 0x100; popfd; nop; nop
    let (mut cpu, mut m) = setup(&[0x9c, 0x81, 0x0c, 0x24, 0x00, 0x01, 0x00, 0x00, 0x9d, 0x90, 0x90]);
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue, "POPF began with TF clear");
    assert_eq!(cpu.eip, CODE + 9);
    assert_eq!(step(&mut cpu, &mut m), Exit::SingleStep);
    assert_eq!(cpu.eip, CODE + 10, "EIP past the instruction that trapped");
}

#[test]
fn popf_clearing_tf_still_traps_after_itself() {
    // push 2; popfd
    let (mut cpu, mut m) = setup(&[0x6a, 0x02, 0x9d, 0x90]);
    cpu.eflags |= TF;
    assert_eq!(step(&mut cpu, &mut m), Exit::SingleStep);
    assert_eq!(step(&mut cpu, &mut m), Exit::SingleStep);
    assert_eq!(cpu.eflags & TF, 0);
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
}

#[test]
fn faults_and_interrupts_win_over_the_trap() {
    // mov eax, [0xfffff0]; int3; ud2; xor ecx, ecx; div ecx
    let (mut cpu, mut m) = setup(&[0xa1, 0xf0, 0xff, 0xff, 0x00, 0xcc, 0x0f, 0x0b, 0x31, 0xc9, 0xf7, 0xf1]);
    cpu.eflags |= TF;
    assert!(matches!(step(&mut cpu, &mut m), Exit::Fault(_)));
    cpu.eip = CODE + 5;
    assert_eq!(step(&mut cpu, &mut m), Exit::Interrupt(3));
    assert_eq!(step(&mut cpu, &mut m), Exit::InvalidOpcode);
    cpu.eip = CODE + 8;
    assert_eq!(step(&mut cpu, &mut m), Exit::SingleStep);
    assert_eq!(step(&mut cpu, &mut m), Exit::DivideError);
}

#[test]
fn not_in_16_bit_mode() {
    let (mut cpu, mut m) = setup(&[0x90]);
    cpu.mode16 = 1;
    cpu.eflags |= TF;
    cpu.eip = CODE;
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
}

/// Random register and memory instructions: with TF each one traps, and the end state is the
/// state without TF (but TF itself).
#[test]
fn tf_changes_nothing_else() {
    let mut seed = 0x9e37_79b9u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..500 {
        let mut code = Vec::new();
        for _ in 0..20 {
            let r = rnd();
            let (a, b) = ((r >> 8) % 8, (r >> 12) % 8);
            if a == 4 || b == 4 {
                continue; // (ESP stays)
            }
            match r % 6 {
                0 => code.extend([0x01, 0xc0 | (b << 3 | a) as u8]),              // add a, b
                1 => code.extend([0x29, 0xc0 | (b << 3 | a) as u8]),              // sub a, b
                2 => code.extend([0x0f, 0xaf, 0xc0 | (a << 3 | b) as u8]),        // imul a, b
                3 => code.extend([0xb8 + a as u8, r as u8, (r >> 16) as u8, 0, 0]), // mov a, imm
                4 => code.extend([0x89, 0x05 | (a << 3) as u8, 0x00, 0x40, 0, 0]), // mov [0x4000], a
                _ => code.extend([0x13, 0xc0 | (a << 3 | b) as u8]),              // adc a, b
            }
        }
        let n = code.len() as u32;
        let (mut plain, mut m1) = setup(&code);
        let (mut traced, mut m2) = setup(&code);
        traced.eflags |= TF;
        for i in 0..8 {
            if i != 4 {
                plain.regs[i] = rnd();
                traced.regs[i] = plain.regs[i];
            }
        }
        while plain.eip < CODE + n {
            assert_eq!(step(&mut plain, &mut m1), Exit::Continue);
        }
        while traced.eip < CODE + n {
            assert_eq!(step(&mut traced, &mut m2), Exit::SingleStep);
        }
        assert_eq!(plain.regs, traced.regs);
        assert_eq!(plain.flags() | TF, traced.flags());
        assert!(m1.0 == m2.0);
    }
}
