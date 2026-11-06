//! The interpreter on any bytes from any register state: never a panic (an instruction it
//! does not know is #UD, a bad access a fault), whatever a game's code or data holds.
#![no_main]

use koji::{Cpu, Exit, Fault, Memory};
use libfuzzer_sys::fuzz_target;

const LO: u32 = 0x10_0000;

struct Window(Vec<u8>);
impl Memory for Window {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a.wrapping_sub(LO) as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        match self.0.get_mut(a.wrapping_sub(LO) as usize) {
            Some(b) => {
                *b = v;
                Ok(())
            }
            None => Err(Fault { addr: a, write: true }),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 34 {
        return;
    }
    let (state, code) = data.split_at(34);
    let mut cpu = Cpu::default();
    for i in 0..8 {
        let v = u32::from_le_bytes(state[4 * i..4 * i + 4].try_into().unwrap());
        cpu.regs[i] = if state[32] >> i & 1 != 0 { LO + (v & 0xfffc) } else { v };
    }
    cpu.eflags = 0x202 | (state[33] as u32 & 0xd5) | (state[33] as u32 & 8) << 8;
    // (16-bit and 64-bit mode too: sake runs Win16 and x64 programs on the same core)
    match state[32] >> 6 {
        1 => cpu.mode64 = 1,
        2 => {
            cpu.mode16 = 1;
            cpu.sbase = [LO; 6];
        }
        _ => {}
    }
    let mut mem = Window(vec![0; 0x1_0000]);
    let n = code.len().min(0x8000);
    mem.0[..n].copy_from_slice(&code[..n]);
    cpu.eip = if cpu.mode16 != 0 { 0 } else { LO };
    cpu.regs[4] = if cpu.mode16 != 0 { 0xfff0 } else { LO + 0xfff0 };
    for _ in 0..2000 {
        if koji::step(&mut cpu, &mut mem) != Exit::Continue {
            break;
        }
    }
});
