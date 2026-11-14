//! koji's dispatcher (jit/dispatch.rs) against `dispatch::slot` with a hash table far larger
//! than the tests' 2048 entries: kura doubles the table without limit, and up to 2^17 entries
//! every slot function agrees, so only a big table shows the generated probe and the Rust
//! `slot` (kura's insert) part ways. Every key inserted by `slot` must be found by the wasm
//! dispatcher; a key not in the table must come back as RET_INTERP. Also checks that the keys
//! spread over the whole table (the old `>> 15` put them all in the first 131,072 slots).
use koji::Cpu;
use wasmtime::*;

const CPU_B: u32 = 0x1000_0000;
const HASH_B: u32 = 0x1001_0000;

pub fn run(engine: &Engine, dispatch: &Module, memory: &SharedMemory, base: usize) -> usize {
    let mut fails = 0;
    for (log, keys) in [(18u32, 100_000u32), (20, 400_000)] {
        let n = 1u32 << log;
        let mask = n - 1;
        assert!(memory.data().len() >= (HASH_B + 8 * n) as usize);
        let hash = (base + HASH_B as usize) as *mut [u32; 2];
        unsafe { std::ptr::write_bytes(hash, 0, n as usize) };
        // keys as kura has them: block starts, clustered in a few code ranges
        let key = |i: u32| match i % 3 {
            0 => 0x40_1000 + i * 7,
            1 => 0x1000_0000 + i * 13,
            _ => 0x7700_0000u32.wrapping_add(i.wrapping_mul(0x9e5)),
        };
        let mut probes = 0u64;
        let mut high = 0u32;
        let mut inserted = Vec::new();
        for i in 0..keys {
            let k = key(i);
            let mut h = koji::jit::dispatch::slot(k, mask);
            if h >= 1 << 17 {
                high += 1;
            }
            loop {
                let e = unsafe { &mut *hash.add(h as usize) };
                if e[0] == 0 {
                    *e = [k, (i & 0x7ff) << koji::jit::ENTRY_SHIFT];
                    inserted.push((k, i & 0x7ff));
                    break;
                }
                if e[0] == k {
                    break;
                }
                probes += 1;
                h = (h + 1) & mask;
            }
        }
        let avg = probes as f64 / inserted.len() as f64;
        // (load <= 40 %: linear probing averages well under one extra probe)
        if avg > 2.0 {
            fails += 1;
            println!("bigtable 2^{log}: {avg:.1} extra probes per insert");
        }
        if (high as f64) < keys as f64 * 0.4 {
            fails += 1;
            println!("bigtable 2^{log}: only {high} of {keys} keys start past slot 131072");
        }
        // the dispatcher, with a block function that reports which entry it was entered with
        let mut store = Store::new(engine, ());
        let mut linker = Linker::new(engine);
        linker.define(&store, "env", "memory", memory.clone()).unwrap();
        let table = Table::new(&mut store, TableType::new(RefType::FUNCREF, 1, None), Ref::Func(None)).unwrap();
        let ty = FuncType::new(engine, (0..koji::jit::N_PARAMS).map(|_| ValType::I32), [ValType::I32]);
        let block = Func::new(&mut store, ty, |_, args, out| {
            out[0] = Val::I32((args[1].unwrap_i32() as u32 >> koji::jit::ENTRY_SHIFT) as i32 + 2);
            Ok(())
        });
        table.set(&mut store, 0, Ref::Func(Some(block))).unwrap();
        linker.define(&store, "env", "table", table).unwrap();
        let inst = match linker.instantiate(&mut store, dispatch) {
            Ok(i) => i,
            Err(e) => {
                println!("bigtable: dispatcher does not link: {e:?}");
                return fails + 1;
            }
        };
        let f = inst.get_typed_func::<i32, i32>(&mut store, "f0").unwrap();
        let mut enter = |eip: u32| -> i32 {
            let c = Cpu { eip, jit_hash: HASH_B, jit_mask: mask, jit_budget: 1 << 20, ..Cpu::default() };
            unsafe { std::ptr::write_unaligned((base + CPU_B as usize) as *mut Cpu, c) };
            f.call(&mut store, CPU_B as i32).unwrap()
        };
        let mut missed = 0;
        for &(k, v) in inserted.iter().step_by(7) {
            if enter(k) != v as i32 + 2 {
                missed += 1;
            }
        }
        if missed > 0 {
            fails += 1;
            println!("bigtable 2^{log}: the dispatcher missed {missed} keys that slot() placed");
        }
        // (a key not in the table: odd addresses past every range above)
        if enter(0xffff_fff1) != koji::jit::RET_INTERP {
            fails += 1;
            println!("bigtable 2^{log}: an absent key was found");
        }
        unsafe { std::ptr::write_bytes(hash, 0, n as usize) };
    }
    fails
}
