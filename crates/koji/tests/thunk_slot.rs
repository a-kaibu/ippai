//! Function mode (koji::jit::FN, a static: this test is alone in its binary): a call through an
//! import thunk (`call thunk` ... `thunk: jmp [slot]`) reads the slot directly only when all
//! four bytes of it are inside the guest's range; a slot in the last three bytes below 2 GB
//! read sake's heap (review b02). Otherwise the call goes to the thunk.

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

fn module_for(slot: u32) -> Vec<u8> {
    // call thunk; ret ... thunk (+0x10): jmp [slot]
    let mut v = vec![0xe8, 0x0b, 0, 0, 0, 0xc3];
    v.resize(0x10, 0xcc);
    v.extend_from_slice(&[0xff, 0x25]);
    v.extend_from_slice(&slot.to_le_bytes());
    let mut m = wasm::Module::default();
    jit::translate_function(&mut m, &mut Code(v), 0x10_0000, jit::MAX_BLOCKS, 100, (0, 0), &|_| None, &|_| true, |a| a == 0x10_0000).unwrap();
    m.finish()
}

#[test]
fn thunk_slot_inside_the_guest() {
    jit::FN.store(true, std::sync::atomic::Ordering::Relaxed);
    let konst = |v: u32| {
        let mut c = wasm::Code::default();
        c.u32c(v);
        c.b
    };
    for (slot, direct) in [(0x7fff_fffc, true), (0x7fff_fffd, false), (0x7fff_ffff, false), (0x0040_1000, true), (0x9234_5678, false), (0x0000_fff4, false)] {
        let b = module_for(slot);
        let k = konst(slot);
        assert_eq!(b.windows(k.len()).any(|w| w == &k[..]), direct, "slot {slot:#x}");
    }
}
