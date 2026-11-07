//! The interpreter against SingleStepTests (https://github.com/SingleStepTests): single-instruction
//! tests recorded on real chips, with the full register and memory state before and after.
//! 80386EX real mode covers every opcode with every operand/address size prefix combination
//! (66/67), random segment overrides, LOCK and REP prefixes, BCD, shifts by any count, and the
//! exceptions a real-mode instruction raises (#DE, INT n, #UD); the 80286, 8086, 8088 and NEC
//! V20 suites test the 16-bit instruction set again on other silicon (other edge cases, other
//! random states). They check the 16/32-bit ALU semantics, flags and addressing against
//! hardware instead of against our reading of the manuals.
//!
//! koji runs Win16 code in 16-bit protected mode; real mode is the same execution model with
//! segment base = selector * 16 ([`koji::LDT_REAL`]). What koji leaves to its embedder (sake)
//! the harness does like real-mode hardware: an INT n or exception pushes FLAGS, CS, IP and jumps
//! through the vector table at 0. What the older chips do that IA-32 doesn't (tests/sst/moo.rs,
//! `not_modeled`: the 808x's opcode aliases, unmasked shift counts, PUSH SP...) is not run.
//!
//! tests/sst/fetch.sh downloads the suites (pinned commits) to target/sst/<cpu>; a suite that
//! isn't there is skipped. `tests/sst/expected-<cpu>.tsv` holds the number of passing tests per
//! file: a file whose count drops fails the test, one that rises prints a note (SST_UPDATE=1
//! rewrites the files). SST_CPU=<cpu> picks a suite, SST_FILTER=<substring> files,
//! SST_SHOW=<n> prints the first n failures of each file, SST_ROOT=<dir> points at another copy.
use flate2::read::GzDecoder;
use koji::{step, Cpu, Exit, Fault, Memory};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

#[path = "../../../tests/sst/moo.rs"]
mod moo;
use moo::*;

const RAM: usize = 16 << 20;

/// The memory: written addresses kept (to undo), linear addresses wrapped at the chip's lines
struct Ram(Vec<u8>, Vec<u32>, u32);
impl Memory for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        let a = a & self.2;
        self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        let a = a & self.2;
        match self.0.get_mut(a as usize) {
            Some(b) => {
                *b = v;
                self.1.push(a);
                Ok(())
            }
            None => Err(Fault { addr: a, write: true }),
        }
    }
}

/// Real-mode interrupt delivery: push FLAGS, CS, IP (the IP given), clear IF and TF, jump
/// through the vector table.
fn interrupt(cpu: &mut Cpu, mem: &mut Ram, n: u8, ret: u32) -> Result<u32, Fault> {
    let flags = cpu.flags();
    // (the vector is read first: the pushes may land on it when SS:SP is near 0)
    let ip = mem.read16(n as u32 * 4)?;
    let cs = mem.read16(n as u32 * 4 + 2)?;
    let mut sp = cpu.regs[koji::ESP] as u16;
    let ss = cpu.sbase[2];
    let at = ss.wrapping_add(sp.wrapping_sub(2) as u32) & mem.2;
    for v in [flags as u16, cpu.sreg[1], ret as u16] {
        sp = sp.wrapping_sub(2);
        mem.write16(ss.wrapping_add(sp as u32), v)?;
    }
    cpu.regs[koji::ESP] = cpu.regs[koji::ESP] & 0xffff_0000 | sp as u32;
    cpu.eflags = flags & !0x300;
    cpu.sreg[1] = cs;
    cpu.sbase[1] = (cs as u32) << 4;
    cpu.eip = ip as u32;
    Ok(at)
}

/// Runs one test; None when it matched, else what differed
fn run(t: &Test, mem: &mut Ram, kind: Kind) -> Option<String> {
    let r = |i: usize| t.init.regs[i].unwrap_or(0);
    let mut cpu = Cpu { mode16: 1, ldt: koji::LDT_REAL, ..Cpu::default() };
    for (k, i) in [2, 4, 5, 3, 9, 8, 6, 7].into_iter().enumerate() {
        cpu.regs[k] = r(i); // eax ecx edx ebx esp ebp esi edi
    }
    cpu.eip = r(16);
    cpu.eflags = r(17) & FLAGS | 2;
    for (k, i) in [12, 10, 15, 11, 13, 14].into_iter().enumerate() {
        cpu.sreg[k] = r(i) as u16; // es cs ss ds fs gs
        cpu.sbase[k] = (r(i) & 0xffff) << 4;
    }
    for &(a, v) in &t.init.ram {
        mem.0[a as usize] = v;
    }
    mem.1.clear();
    let mut result = None;
    // (where an interrupt this harness delivered put FLAGS, for the suites without EXCP)
    let mut pushed = None;
    let start = cpu.eip;
    // (PUSHF: the image of FLAGS, its reserved bits the chip's own (the 808x's top four set))
    if t.bytes.iter().find(|b| !matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0xf0 | 0xf2 | 0xf3)) == Some(&0x9c) && kind.regs16() {
        pushed = Some((cpu.sbase[2] + (cpu.regs[koji::ESP].wrapping_sub(2) & 0xffff)) & mem.2);
    }
    for _ in 0..100_000 {
        let e = step(&mut cpu, mem);
        let deliver = match e {
            Exit::Continue => None,
            Exit::Halt if kind.ends_with_hlt() => {
                cpu.eip = cpu.eip.wrapping_add(1); // (an HLT at FFFF leaves IP at 10000)
                break;
            }
            // the 808x's and V20's exceptions (divide error, BOUND) are interrupts after the
            // instruction; later chips' return to the faulting one
            Exit::Interrupt(n) if !kind.ends_with_hlt() => Some((n, start.wrapping_add(t.bytes.len() as u32) & 0xffff)),
            Exit::DivideError if !kind.ends_with_hlt() => Some((0, start.wrapping_add(t.bytes.len() as u32) & 0xffff)),
            Exit::Interrupt(n) => Some((n, cpu.eip)), // (past the INT)
            Exit::DivideError => Some((0, cpu.eip)),
            Exit::InvalidOpcode => Some((6, cpu.eip)),
            e => {
                result = Some(format!("exit {e:?}"));
                break;
            }
        };
        if let Some((n, ip)) = deliver {
            match interrupt(&mut cpu, mem, n, ip) {
                Ok(at) => pushed = Some(at),
                Err(f) => {
                    result = Some(format!("fault {f:?} delivering int {n}"));
                    break;
                }
            }
        }
        // the 808x suites end at the next instruction (koji steps a REP string instruction
        // whole; an interrupt: at the handler)
        if !kind.ends_with_hlt() {
            break;
        }
    }
    let mut diffs = Vec::new();
    let undef = undefined_flags(&decode(&t.bytes), r(4));
    let flags_at = t.excp.map(|e| e.1).or(pushed);
    if let Some(r) = result {
        diffs.push(r);
    } else {
        let got = |i: usize| -> u32 {
            match i {
                2..=9 => cpu.regs[[0, 3, 1, 2, 6, 7, 5, 4][i - 2]],
                10 => cpu.sreg[1] as u32,
                11 => cpu.sreg[3] as u32,
                12 => cpu.sreg[0] as u32,
                13 => cpu.sreg[4] as u32,
                14 => cpu.sreg[5] as u32,
                15 => cpu.sreg[2] as u32,
                16 => cpu.eip,
                17 => cpu.flags(),
                _ => 0,
            }
        };
        for i in 2..18 {
            let want = t.fina.regs[i].unwrap_or(r(i));
            let mut m = t.mask[i];
            if i == 17 {
                m &= FLAGS & !undef;
            }
            if (10..=15).contains(&i) || kind.regs16() {
                m &= 0xffff;
            }
            if (got(i) ^ want) & m != 0 {
                diffs.push(format!("{} want {want:08x} got {:08x}", NAMES[i], got(i)));
            }
        }
        for &(a, v) in &t.fina.ram {
            let g = mem.0[a as usize];
            // (FLAGS an exception pushed: the defined ones)
            let m = match flags_at {
                Some(f) if a == f => (FLAGS & !undef) as u8,
                Some(f) if a == f + 1 => ((FLAGS & !undef) >> 8) as u8,
                _ => 0xff,
            };
            if (g ^ v) & m != 0 {
                diffs.push(format!("[{a:06x}] want {v:02x} got {g:02x}"));
            }
        }
    }
    // (undo this test's writes)
    for &(a, _) in &t.init.ram {
        mem.0[a as usize] = 0;
    }
    for &a in &mem.1 {
        mem.0[a as usize] = 0;
    }
    (!diffs.is_empty()).then(|| diffs.join(", "))
}

fn root() -> PathBuf {
    std::env::var_os("SST_ROOT").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/sst"))
}

fn expected_path(cpu: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../tests/sst/expected-{cpu}.tsv"))
}

/// One suite: (passing, run) per file; the files that got worse
fn suite(s: &Suite, filter: &str, show: usize) -> Option<(BTreeMap<String, (usize, usize)>, Vec<String>, BTreeMap<&'static str, usize>)> {
    let dir = suite_dir(&root(), s);
    let rd = std::fs::read_dir(&dir).ok()?;
    let revoked = revoked(&root(), s);
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.to_string_lossy().ends_with(".MOO.gz")).collect();
    files.sort();
    let expected = read_expected(s.cpu);
    let mut mem = Ram(vec![0; RAM], Vec::new(), s.kind.addr_mask());
    let mut now = BTreeMap::new();
    let mut skipped = BTreeMap::new();
    let mut worse = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().trim_end_matches(".MOO.gz").to_string();
        if !name.contains(filter) {
            continue;
        }
        let mut data = Vec::new();
        GzDecoder::new(std::fs::File::open(&f).unwrap()).read_to_end(&mut data).unwrap();
        let tests = parse(&data);
        let (mut ok, mut n, mut shown) = (0, 0, 0);
        for t in &tests {
            let why = skip(s.kind, t, &revoked);
            if let Some(w) = why {
                *skipped.entry(w).or_insert(0) += 1;
                continue;
            }
            n += 1;
            match run(t, &mut mem, s.kind) {
                None => ok += 1,
                Some(d) if shown < show => {
                    shown += 1;
                    eprintln!("{} {name} {} [{}] excp {:?}: {d}", s.cpu, t.name, t.bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(), t.excp);
                }
                Some(_) => {}
            }
        }
        if let Some(&(want, _)) = expected.get(&name) {
            if ok < want {
                worse.push(format!("{} {name}: {ok}/{n} pass, expected-{}.tsv says {want}", s.cpu, s.cpu));
            } else if ok > want {
                eprintln!("singlestep: {} {name} improved {want} -> {ok}/{n} (SST_UPDATE=1 records it)", s.cpu);
            }
        } else if ok < n {
            worse.push(format!("{} {name}: {ok}/{n} pass, not in expected-{}.tsv", s.cpu, s.cpu));
        }
        now.insert(name, (ok, n));
    }
    Some((now, worse, skipped))
}

fn read_expected(cpu: &str) -> BTreeMap<String, (usize, usize)> {
    std::fs::read_to_string(expected_path(cpu))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.to_string(), (f.next()?.parse().ok()?, f.next()?.parse().ok()?)))
        })
        .collect()
}

#[test]
fn singlestep() {
    let filter = std::env::var("SST_FILTER").unwrap_or_default();
    let only = std::env::var("SST_CPU").unwrap_or_default();
    let show: usize = std::env::var("SST_SHOW").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut worse = Vec::new();
    // (the suites in parallel: a few minutes each)
    let results: Vec<_> = std::thread::scope(|sc| {
        let hs: Vec<_> = SUITES.iter().filter(|s| only.is_empty() || s.cpu == only).map(|s| (s, sc.spawn(|| suite(s, &filter, show)))).collect();
        hs.into_iter().map(|(s, h)| (s, h.join().unwrap())).collect()
    });
    for (s, r) in results {
        let Some((now, w, skipped)) = r else {
            eprintln!("singlestep: no {} suite at {} (tests/sst/fetch.sh)", s.cpu, suite_dir(&root(), s).display());
            continue;
        };
        let (pass, total): (usize, usize) = now.values().fold((0, 0), |(a, b), (p, n)| (a + p, b + n));
        eprintln!("singlestep {}: {pass}/{total} tests pass in {} files; not run: {skipped:?}", s.cpu, now.len());
        worse.extend(w);
        if std::env::var_os("SST_UPDATE").is_some() {
            let mut all = read_expected(s.cpu);
            all.extend(now);
            let mut out = format!("# file\tpassing\ttests  ({} suite; crates/koji/tests/singlestep.rs; SST_UPDATE=1 rewrites)\n", s.cpu);
            for (k, (a, b)) in all {
                out += &format!("{k}\t{a}\t{b}\n");
            }
            std::fs::write(expected_path(s.cpu), out).unwrap();
        }
    }
    if std::env::var_os("SST_UPDATE").is_none() {
        assert!(worse.is_empty(), "fewer passing tests than expected:\n{}", worse.join("\n"));
    }
}
