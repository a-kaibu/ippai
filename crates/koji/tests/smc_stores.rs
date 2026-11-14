//! Stores in a block whose code may be rewritten (koji::jit::set_watch: kura's W|X pages of the
//! main executable, where packers and protectors patch code): every kind of store the JIT
//! makes is followed by the check that leaves for the host when the page has translated code
//! (Cpu::jit_smc). Only write_op's and the packed `stored` ones had it; PUSH, FST/FSTP/FIST,
//! FNSTCW, MOVLPS/MOVHPS, MOVSS, MOVQ/MOVNTQ and the cached 128-bit moves stored without
//! (review b02). WATCH is a static: this test is alone in its binary.

use koji::jit::{self, wasm};
use koji::{Fault, Memory};

struct Code(Vec<u8>);
impl Memory for Code {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        a.checked_sub(0x10_0000).and_then(|i| self.0.get(i as usize).copied()).ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, _: u8) -> Result<(), Fault> {
        Err(Fault { addr: a, write: true })
    }
}

/// The JIT code of `bytes` then RET, as a region: how many smc exits (stores of an address
/// into Cpu::jit_smc) it has, or None if nothing was translated.
fn smc_exits(bytes: &[u8]) -> Option<usize> {
    let mut v = bytes.to_vec();
    v.push(0xc3);
    let mut m = wasm::Module::default();
    jit::translate_region(&mut m, &mut Code(v), &[0x10_0000], 16, |_| true)?;
    let b = m.finish();
    let mut c = wasm::Code::default();
    c.store32(std::mem::offset_of!(koji::Cpu, jit_smc) as u32);
    Some(b.windows(c.b.len()).filter(|w| *w == &c.b[..]).count())
}

#[test]
fn every_store_is_checked_in_watched_code() {
    jit::set_watch(|_| true);
    let cases: &[(&str, &[u8])] = &[
        ("mov [ebx], eax", &[0x89, 0x03]),
        ("push eax", &[0x50]),
        ("push imm32", &[0x68, 0x78, 0x56, 0x34, 0x12]),
        ("push dword [ebx]", &[0xff, 0x33]),
        ("fld1; fstp dword [ebx]", &[0xd9, 0xe8, 0xd9, 0x1b]),
        ("fld1; fst qword [ebx]", &[0xd9, 0xe8, 0xdd, 0x13]),
        ("fld1; fistp dword [ebx]", &[0xd9, 0xe8, 0xdb, 0x1b]),
        ("fnstcw [ebx]", &[0xd9, 0x3b]),
        ("movlps [ebx], xmm0", &[0x0f, 0x13, 0x03]),
        ("movhps [ebx], xmm0", &[0x0f, 0x17, 0x03]),
        ("movss [ebx], xmm0", &[0xf3, 0x0f, 0x11, 0x03]),
        ("movq [ebx], mm0", &[0x0f, 0x7f, 0x03]),
        ("movntq [ebx], mm0", &[0x0f, 0xe7, 0x03]),
        ("movaps xmm0, [ecx]; movaps [ebx], xmm0", &[0x0f, 0x28, 0x01, 0x0f, 0x29, 0x03]),
        ("movups [ebx], xmm1", &[0x0f, 0x11, 0x0b]),
        ("movdqa [ebx], xmm0", &[0x66, 0x0f, 0x7f, 0x03]),
        ("movsd [ebx], xmm0", &[0xf2, 0x0f, 0x11, 0x03]),
        ("add [ebx+ecx*4+8], eax", &[0x01, 0x44, 0x8b, 0x08]),
        ("mov fs:[0], eax", &[0x64, 0xa3, 0, 0, 0, 0]),
    ];
    for (name, bytes) in cases {
        let n = smc_exits(bytes).unwrap_or_else(|| panic!("{name}: not translated"));
        assert!(n >= 1, "{name}: a store without the self-modifying code check");
    }
    // (and none where nothing is stored)
    assert_eq!(smc_exits(&[0x8b, 0x03]), Some(0), "mov eax, [ebx]");
}
