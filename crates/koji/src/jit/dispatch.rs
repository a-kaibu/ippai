//! The dispatcher module: `run(cpu) -> i32` looks `cpu.eip` up in the block hash table and
//! calls the block through the `env.table` funcref table, until a block returns non-zero, an
//! address has no block (returns [`super::RET_INTERP`]) or `cpu.jit_budget` runs out (returns 0).
//!
//! The hash table (owned by the embedder) is `cpu.jit_mask + 1` entries of (eip, entry), entry
//! being the region function's table index | the block's index << ENTRY_SHIFT (the function's
//! second argument);
//! eip 0 marks a free entry. Entries are found by linear probing from [`slot`].

use super::wasm::{Code, Func, FuncType, Import, Module, I32};
use super::Off;
use crate::Cpu;
use core::mem::offset_of;

/// First probe position of `eip` in a table of `mask + 1` entries.
///
/// Fibonacci hashing: the top log2(mask + 1) bits of the product, the shift taken from the
/// mask (`clz(mask)` is 32 - log2 of the table size; a shift of 32 is 0 in wasm and in
/// `wrapping_shr`, and `& mask` then makes it 0 for a table of one entry). It used to be
/// `>> 15` for every size, 17 bits: a table grown past 2^17 entries (more than 65,536 blocks;
/// kura doubles it without limit) still hashed every key into its first 131,072 slots and
/// linear probing walked one long cluster (review C-6). The generated probes (here,
/// [`probe_slot`]: jit/mod.rs, jit/long.rs) compute the same.
#[inline]
pub fn slot(eip: u32, mask: u32) -> u32 {
    eip.wrapping_mul(0x9e37_79b1).wrapping_shr(mask.leading_zeros()) & mask
}

/// Emits [`slot`]: the key (an i32) is on the stack, `mask` is a local holding the table's
/// mask; leaves the slot on the stack.
pub fn probe_slot(c: &mut Code, mask: u32) {
    c.u32c(0x9e37_79b1).mul().get(mask).clz().shr_u().get(mask).and();
}

pub fn module() -> alloc::vec::Vec<u8> {
    let mut m = Module::default();
    m.types.push(FuncType { params: alloc::vec![I32], results: alloc::vec![I32] });
    m.types.push(FuncType { params: alloc::vec![I32; super::N_PARAMS as usize], results: alloc::vec![I32] });
    m.imports.push(Import::Memory);
    m.imports.push(Import::Table("table"));
    let hash = offset_of!(Cpu, jit_hash) as u32;
    let maskf = offset_of!(Cpu, jit_mask) as u32;
    let (cpu, eip, h, e, st, mask) = (0, 1, 2, 3, 4, 5);
    let mut c = Code::default();
    c.get(cpu).load32(maskf).set(mask);
    c.loop_();
    {
        // budget
        c.get(cpu).get(cpu).load32(Off::budget()).i32c(1).sub().store32(Off::budget());
        c.get(cpu).load32(Off::budget()).i32c(0).le_s();
        c.if_().i32c(0).ret().end();
        c.get(cpu).load32(Off::eip()).set(eip);
        // (0 is the key of an empty slot: the interpreter faults there)
        c.get(eip).eqz().if_().i32c(super::RET_INTERP).ret().end();
        c.get(eip);
        probe_slot(&mut c, mask);
        c.set(h);
        c.block();
        c.loop_();
        {
            c.get(cpu).load32(hash).get(h).i32c(3).shl().add().tee(e).load32(0);
            c.get(eip).eq().br_if(1);
            c.get(e).load32(0).eqz().if_().i32c(super::RET_INTERP).ret().end();
            c.get(h).i32c(1).add().get(mask).and().set(h);
            c.br(0);
        }
        c.end();
        c.end();
        // (cpu, entry, registers, lazy flags in the Cpu, budget)
        c.get(cpu).get(e).load32(4).tee(e);
        for i in 0..8 {
            c.get(cpu).load32(Off::reg(i));
        }
        c.i32c(-1).i32c(0).i32c(0).i32c(0).i32c(0);
        c.get(cpu).load32(Off::budget());
        c.get(e).u32c((1 << super::ENTRY_SHIFT) - 1).and().call_indirect(1, 0);
        c.tee(st).if_().get(st).ret().end();
        c.br(0);
    }
    c.end();
    c.i32c(0);
    m.funcs.push(Func { ty: 0, locals: alloc::vec![I32; 5], code: c });
    m.finish()
}

#[cfg(test)]
mod tests {
    use super::slot;
    use proptest::prelude::*;

    proptest! {
        /// The slot is the top `log` bits of the product (Fibonacci hashing), for every size;
        /// at 2^17 entries the slot the 17-bit hash gave.
        #[test]
        fn top_bits_of_the_product(eip: u32, log in 1u32..=31) {
            let mask = (1u32 << log) - 1;
            prop_assert_eq!(slot(eip, mask), eip.wrapping_mul(0x9e37_79b1) >> (32 - log));
            if log == 17 {
                prop_assert_eq!(slot(eip, mask), (eip.wrapping_mul(0x9e37_79b1) >> 15) & mask);
            }
        }

        #[test]
        fn inside_the_table(eip: u32, log in 0u32..=32) {
            let mask = ((1u64 << log) - 1) as u32;
            prop_assert!(slot(eip, mask) <= mask);
        }
    }

    /// Review C-6: with more than 65,536 blocks kura grows the table past 2^17 entries; the
    /// keys must then use all of it, not the first 131,072 slots.
    #[test]
    fn big_tables_are_used_whole() {
        for log in [18u32, 20, 22] {
            let mask = (1u32 << log) - 1;
            let mut seen = alloc::vec![0u8; 1 << (log - 12)];
            let n = 1u32 << (log - 2);
            let mut high = 0;
            for i in 0..n {
                let s = slot(0x40_0000 + i * 5, mask);
                high += (s >= 1 << 17) as u32;
                seen[(s >> 12) as usize] = 1;
            }
            // every 4096-slot stripe of the table gets keys, and most keys start past 2^17
            assert!(seen.iter().all(|&b| b == 1), "2^{log}: stripes left empty");
            assert!(high as u64 * 8 >= n as u64 * 3, "2^{log}: {high} of {n} past 2^17");
        }
    }

    /// A table of 2^19 entries filled to 40 % with block starts: linear probing stays short
    /// (with the 17-bit hash every insert past 131,072 walked the one cluster).
    #[test]
    fn probes_stay_short_past_2_17() {
        let mask = (1u32 << 19) - 1;
        let mut t = alloc::vec![0u32; mask as usize + 1];
        let mut probes = 0u64;
        let n = 200_000u32;
        for i in 0..n {
            let k = 0x1000 + i * 3;
            let mut h = slot(k, mask);
            while t[h as usize] != 0 {
                probes += 1;
                h = (h + 1) & mask;
            }
            t[h as usize] = k;
        }
        assert!(probes < n as u64 * 2, "{probes} extra probes for {n} inserts");
    }
}

#[cfg(kani)]
mod proofs {
    use super::slot;

    /// For every key and every table size (1 to 2^32 entries) the slot is inside the table and
    /// is the top log2(size) bits of the product, the bits a larger table adds included.
    #[kani::proof]
    fn slot_in_table_and_top_bits() {
        let eip: u32 = kani::any();
        let log: u32 = kani::any_where(|&l| l <= 32);
        let mask = ((1u64 << log) - 1) as u32;
        let s = slot(eip, mask);
        assert!(s <= mask);
        let p = eip.wrapping_mul(0x9e37_79b1) as u64;
        assert_eq!(s as u64, if log == 0 { 0 } else { p >> (32 - log) });
    }
}
