use iced_x86::code_asm::*;
use vnx86::{Cpu, Fault, Memory};
struct Flat(Vec<u8>);
impl Memory for Flat {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> { self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false }) }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> { match self.0.get_mut(a as usize) { Some(b) => { *b = v; Ok(()) } None => Err(Fault { addr: a, write: true }) } }
}
fn main() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.mov(eax, dword_ptr(esi + ecx * 4 + 8)).unwrap();
    a.movzx(edx, byte_ptr(edi)).unwrap();
    a.xor(eax, edx).unwrap();
    a.shr(eax, 8).unwrap();
    a.add(ebx, eax).unwrap();
    a.lea(edi, ptr(edi + 1)).unwrap();
    a.push(eax).unwrap();
    a.pop(edx).unwrap();
    a.inc(ecx).unwrap();
    a.and(ecx, 255).unwrap();
    a.jmp(top).unwrap();
    let code = a.assemble(0x1000).unwrap();
    let mut mem = Flat(vec![0; 1 << 20]);
    mem.0[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    for cached in [false, true] {
        let mut cpu = Cpu::default();
        cpu.eip = 0x1000; cpu.regs[4] = 0x80000; cpu.regs[6] = 0x20000; cpu.regs[7] = 0x30000;
        let mut c = vnx86::DecodeCache::new();
        let t = std::time::Instant::now();
        for _ in 0..20_000_000 { cpu.regs[7] &= 0x3ffff; let _ = if cached { vnx86::step_cached(&mut cpu, &mut mem, Some(&mut c)) } else { vnx86::step_flow(&mut cpu, &mut mem) }; }
        println!("cached {cached}: {:.1} ns/instr", t.elapsed().as_nanos() as f64 / 2e7);
    }
}
