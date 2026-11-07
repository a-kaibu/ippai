//! The JIT's MXCSR flag mode (koji::jit::MX_EXACT): translations without the flag checks until
//! the program first reads the flags (STMXCSR, FXSAVE in the interpreter), with them from then
//! on, never back; the switch reported once (the host then drops what it translated). One test:
//! the mode is the process's (tools/jit32test and jit64test run programs across the switch).

use core::sync::atomic::Ordering::Relaxed;
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

const CODE: u32 = 0x10000; // (jit::GUEST_LO)
const ADDSS_RET: &[u8] = &[0xf3, 0x0f, 0x58, 0xc1, 0xc3]; // addss xmm0, xmm1; ret
const STMXCSR: &[u8] = &[0x0f, 0xae, 0x1d, 0x00, 0x20, 0x00, 0x00]; // stmxcsr [0x2000]
const FXSAVE: &[u8] = &[0x0f, 0xae, 0x05, 0x00, 0x20, 0x00, 0x00]; // fxsave [0x2000]

fn setup(code: &[u8]) -> (Cpu, Flat) {
    let mut m = Flat(vec![0; 0x20000]);
    m.0[CODE as usize..CODE as usize + code.len()].copy_from_slice(code);
    let mut cpu = Cpu::default();
    cpu.eip = CODE;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    cpu.mxcsr = 0x1f80 | 0x21; // flags the program raised: precision, invalid
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    (cpu, m)
}

/// The module translated from `code` and whether it has the flag checks.
fn translate(code: &[u8]) -> (usize, bool) {
    let (_, mut m) = setup(code);
    let mut module = jit::wasm::Module::default();
    let r = jit::translate_region(&mut module, &mut m, &[CODE], 4, |_| true).expect("translated");
    (module.finish().len(), r.exact)
}

/// The instructions of `code` at CODE.
fn decode(code: &[u8]) -> Vec<iced_x86::Instruction> {
    iced_x86::Decoder::with_ip(32, code, CODE as u64, iced_x86::DecoderOptions::NONE).into_iter().collect()
}

const MOV_EAX: &[u8] = &[0xa1, 0x00, 0x20, 0x00, 0x00]; // mov eax, [0x2000]
const MOVZX_ECX: &[u8] = &[0x0f, 0xb7, 0x0d, 0x00, 0x20, 0x00, 0x00]; // movzx ecx, word [0x2000]

fn and(reg_eax: bool, imm: u32) -> Vec<u8> {
    let mut v = if reg_eax { vec![0x25] } else { vec![0x81, 0xe1] }; // and eax/ecx, imm32
    v.extend(imm.to_le_bytes());
    v
}

#[test]
fn flag_mode_switches_once_at_the_first_read() {
    assert!(!jit::mx_exact(), "off at the start");
    assert!(!jit::mx_switched());

    // reads of the control bits alone (MSVC's math functions) don't switch
    let ctl = |load: &[u8], eax: bool, imm: u32| jit::control_read(&decode(&[STMXCSR, load, &and(eax, imm)].concat())[0], &decode(&[STMXCSR, load, &and(eax, imm)].concat())[1..]);
    assert!(ctl(MOV_EAX, true, 0x7f80) && ctl(MOV_EAX, true, 0x1f80) && ctl(MOVZX_ECX, false, 0xffc0) && ctl(MOV_EAX, true, 0x6000));
    assert!(!ctl(MOV_EAX, true, 0x7fbf), "a flag bit kept");
    assert!(!ctl(MOV_EAX, true, 0x3f));
    assert!(!ctl(MOV_EAX, false, 0x7f80), "another register masked");
    assert!(!ctl(&[0xa1, 0x04, 0x20, 0x00, 0x00], true, 0x7f80), "another address loaded");
    let only = |v: Vec<u8>| jit::control_read(&decode(&v)[0], &decode(&v)[1..]);
    assert!(!only([STMXCSR, MOV_EAX].concat()), "not masked");
    assert!(!only([STMXCSR, &[0x90], MOV_EAX, &and(true, 0x7f80)].concat()), "not right after");
    let code = [STMXCSR, MOV_EAX, &and(true, 0x7f80)].concat();
    let (mut cpu, mut m) = setup(&code);
    for _ in 0..3 {
        assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    }
    assert_eq!(cpu.regs[0], 0x1f80);
    assert!(!jit::mx_exact() && !jit::mx_switched(), "no switch");
    let (fast, e) = translate(ADDSS_RET);
    assert!(!e);

    // the first read: the interpreter's STMXCSR stores the flags it has and switches
    let (mut cpu, mut m) = setup(STMXCSR);
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    assert_eq!(u32::from_le_bytes(m.0[0x2000..0x2004].try_into().unwrap()), 0x1fa1);
    assert!(jit::mx_exact());
    assert_eq!(jit::MX_AT.load(Relaxed), CODE, "where");
    assert!(jit::mx_switched(), "the switch is reported");
    assert!(!jit::mx_switched(), "once");
    let (exact, e) = translate(ADDSS_RET);
    assert!(e);
    assert!(exact > fast, "the checks are in the module ({exact} bytes, {fast} without)");

    // later reads don't switch again
    let (mut cpu, mut m) = setup(STMXCSR);
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    jit::mx_read(0);
    assert!(jit::mx_exact() && !jit::mx_switched());

    // FXSAVE reads them too (the image's MXCSR at +24)
    jit::MX_EXACT.store(false, Relaxed);
    let (mut cpu, mut m) = setup(FXSAVE);
    assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    assert_eq!(u32::from_le_bytes(m.0[0x2018..0x201c].try_into().unwrap()), 0x1fa1);
    assert!(jit::mx_exact() && jit::mx_switched() && !jit::mx_switched());
    assert_eq!(translate(ADDSS_RET), (exact, true));
}
