//! Translates every region of saved bundles again (their index has the roots, block starts and
//! guest code), to measure the code generator on real code offline:
//!   cargo run --release -p koji --example corpus -- out.wasm a.idx [b.idx ...]
//! writes all regions as one module (one function each, like a saved bundle) and prints the
//! wasm bytes per mnemonic (most first) and in all. tests/compile-cost.mjs then times V8's
//! compilers on it.
use std::collections::{BTreeMap, HashSet};
use koji::{Fault, Memory};

/// The guest code of the index's entries, by address.
struct Sparse(BTreeMap<u32, Vec<u8>>);

impl Memory for Sparse {
    fn read8(&mut self, addr: u32) -> Result<u8, Fault> {
        let (a, v) = self.0.range(..=addr).next_back().ok_or(Fault { addr, write: false })?;
        v.get((addr - a) as usize).copied().ok_or(Fault { addr, write: false })
    }
    fn write8(&mut self, addr: u32, _v: u8) -> Result<(), Fault> {
        Err(Fault { addr, write: true })
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut entries = Vec::new();
    for f in &args[2..] {
        entries.extend(koji::jit::saved::decode_all(&std::fs::read(f).unwrap()).unwrap());
    }
    koji::jit::LAYOUT.store(true, core::sync::atomic::Ordering::Relaxed);
    let mut sizes: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut mods, mut names, mut ins) = (Vec::new(), Vec::new(), 0);
    // VNVM_CORPUS_ROOT=<hex>: only the regions whose first root is that address
    let only = std::env::var("VNVM_CORPUS_ROOT").ok().and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    for e in entries.iter().filter(|e| only.is_none_or(|a| e.roots[0] == a)) {
        let mut mem = Sparse(BTreeMap::new());
        let mut at = 0;
        for &(a, n) in &e.code {
            mem.0.insert(a, e.text[at..at + n as usize].to_vec());
            at += n as usize;
        }
        let ok: HashSet<u32> = e.starts.iter().map(|s| s.0).collect();
        let mut m = koji::jit::wasm::Module::default();
        let Some(r) = koji::jit::translate_region(&mut m, &mut mem, &e.roots, 1024, |a| ok.contains(&a)) else { continue };
        ins += r.instructions;
        for l in r.layout.lines() {
            let p: Vec<&str> = l.split(' ').collect();
            if p.len() == 4 && p[0] == "SZ" {
                let s = sizes.entry(p[1].to_string()).or_default();
                s.0 += p[2].parse::<u64>().unwrap();
                s.1 += p[3].parse::<u64>().unwrap();
            }
        }
        mods.push(m.finish());
        names.push(e.name());
    }
    let refs: Vec<&[u8]> = mods.iter().map(|m| &m[..]).collect();
    let all = koji::jit::wasm::merge(&refs, &names).unwrap();
    std::fs::write(&args[1], &all).unwrap();
    let mut v: Vec<_> = sizes.into_iter().collect();
    v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    let total: u64 = v.iter().map(|x| x.1 .1).sum();
    for (k, (n, b)) in v.iter().take(40) {
        println!("{k:>14} {n:>8} {b:>9} {:>5.1}/ins {:>5.1}%", *b as f64 / *n as f64, 100.0 * *b as f64 / total as f64);
    }
    println!("regions {} instructions {ins} wasm {} (instructions' bytes {total}, {:.1} per instruction)", mods.len(), all.len(), total as f64 / ins as f64);
}
