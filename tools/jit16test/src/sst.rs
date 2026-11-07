//! koji's 16-bit translation against SingleStepTests (real-mode tests recorded on real chips:
//! 80386EX, 80286, 8086, 8088, NEC V20; tests/sst/fetch.sh): each test whose instruction the
//! translator does itself (`ok16`) runs as a translated region (a block of its own, then the
//! interpreter for the HLT that ends the 386's and 286's tests, an INT or a jump's target), and
//! registers, flags and memory must be the chip's. crates/koji/tests/singlestep.rs checks the
//! interpreter on the same suites (tests/sst/moo.rs: the format, the tests not run); a test the
//! interpreter gets wrong too (a chip's quirk, counted in expected-<cpu>.tsv) is not run here,
//! so this checks the wasm the translator makes, real hardware the reference.
//!
//!   cargo run --release -- sst [cpu (all suites)] [file filter]
//!   SST_N=<n> tests per file (default all), SST_SHOW=<n> failures shown per file,
//!   SST_ROOT=<dir> (default target/sst)
//!
//! The guest's linear 0 sits at G in the wasm memory (the harness's Cpu, dispatch table and ring
//! are below): segment bases are selector * 16 + G. Instructions that load a segment register
//! are interpreter-only (not ok16), so no base is ever made without G.
use crate::*;
use iced_x86::Instruction;
use std::io::Read;

#[path = "../../../tests/sst/moo.rs"]
mod moo;
use moo::*;

const G: u32 = 0x20_0000;
const SPAN: u32 = 0x12_0000;

/// Real-mode interrupt delivery through the vector table (as singlestep.rs), G added
fn interrupt(cpu: &mut Cpu, mem: &mut Mem, n: u8, ret: u32) -> Result<(), Fault> {
    use koji::Memory;
    let flags = cpu.flags();
    let ip = mem.read16(G + n as u32 * 4)?;
    let cs = mem.read16(G + n as u32 * 4 + 2)?;
    let mut sp = cpu.regs[koji::ESP] as u16;
    let ss = cpu.sbase[2];
    for v in [flags as u16, cpu.sreg[1], ret as u16] {
        sp = sp.wrapping_sub(2);
        mem.write16(ss.wrapping_add(sp as u32), v)?;
    }
    cpu.regs[koji::ESP] = cpu.regs[koji::ESP] & 0xffff_0000 | sp as u32;
    cpu.eflags = flags & !0x300;
    cpu.sreg[1] = cs;
    cpu.sbase[1] = ((cs as u32) << 4) + G;
    cpu.eip = ip as u32;
    Ok(())
}

/// Where the interrupt an exit asks for returns to (None: no interrupt): the 808x's and V20's
/// exceptions (divide error, BOUND) are interrupts after the instruction, later chips' return
/// to the faulting one; an INT n returns past it
fn deliver(kind: Kind, e: Exit, c: &Cpu, next: u32) -> Option<(u8, u32)> {
    match e {
        Exit::Interrupt(n) if !kind.ends_with_hlt() => Some((n, next)),
        Exit::DivideError if !kind.ends_with_hlt() => Some((0, next)),
        Exit::Interrupt(n) => Some((n, c.eip)),
        Exit::DivideError => Some((0, c.eip)),
        Exit::InvalidOpcode => Some((6, c.eip)),
        _ => None,
    }
}

/// What an exit does (as singlestep.rs's loop): Ok(false) to stop. `pushed`: where an
/// interrupt put FLAGS.
fn exit(kind: Kind, c: &mut Cpu, mem: &mut Mem, e: Exit, next: u32, pushed: &mut Option<u32>) -> Result<bool, String> {
    if e == Exit::Halt && kind.ends_with_hlt() {
        c.eip = c.eip.wrapping_add(1);
        return Ok(false);
    }
    // (the 808x: the HLT run() put after the instruction)
    if e == Exit::Halt && c.eip == next {
        return Ok(false);
    }
    if let Some((n, ip)) = deliver(kind, e, c, next) {
        let at = c.sbase[2] - G + (c.regs[koji::ESP].wrapping_sub(2) & 0xffff);
        interrupt(c, mem, n, ip).map_err(|f| format!("fault {f:?} delivering int {n}"))?;
        *pushed = Some(at);
        return Ok(kind.ends_with_hlt());
    }
    match e {
        // (the 808x suites end at the next instruction)
        Exit::Continue => Ok(kind.ends_with_hlt()),
        e => Err(format!("exit {e:?}")),
    }
}

/// One test (as singlestep.rs): with `jit` the instruction in a region, the rest stepped;
/// without, the interpreter alone. None when it matched the chip; `Err` when the region was
/// refused (not counted).
fn run(engine: &Engine, memory: &SharedMemory, base: usize, t: &Test, ins: &Instruction, kind: Kind, jit: bool) -> Result<Option<String>, ()> {
    let m = base as *mut u8;
    unsafe { std::ptr::write_bytes(m.add(G as usize), 0, SPAN as usize) };
    for &(a, v) in &t.init.ram {
        unsafe { *m.add((G + a) as usize) = v };
    }
    let r = |i: usize| t.init.regs[i].unwrap_or(0);
    let mut c = Cpu {
        mode16: 1,
        ldt: koji::LDT_REAL,
        ..Cpu::default()
    };
    for (k, i) in [2, 4, 5, 3, 9, 8, 6, 7].into_iter().enumerate() {
        c.regs[k] = r(i);
    }
    c.eip = r(16);
    c.eflags = r(17) & FLAGS | 2;
    for (k, i) in [12, 10, 15, 11, 13, 14].into_iter().enumerate() {
        c.sreg[k] = r(i) as u16;
        c.sbase[k] = ((r(i) & 0xffff) << 4) + G;
    }
    let start = c.eip;
    let next = start.wrapping_add(t.bytes.len() as u32) & 0xffff;
    // (where FLAGS went: an interrupt this harness delivered, a PUSHF on the 16-bit chips,
    // whose reserved bits are the chip's own)
    let mut pushed = None;
    if t.bytes.iter().find(|b| !matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0xf0 | 0xf2 | 0xf3)) == Some(&0x9c) && kind.regs16() {
        pushed = Some(c.sbase[2] - G + (c.regs[koji::ESP].wrapping_sub(2) & 0xffff));
    }
    // (the 808x suites have NOPs after the instruction, which a block would run on: a HLT
    // there ends it, put back before the memory is compared)
    let stop = (!kind.ends_with_hlt()).then(|| c.sbase[1] + next);
    let kept = stop.map(|a| unsafe { std::ptr::replace(m.add(a as usize), 0xf4) });
    let mut mem = Mem(m);
    let mut fail = None;
    // the translated instruction first (a region of its own), then the interpreter: to the
    // HLT on the 386 and 286; on the 808x only what the region left to it
    let mut go = true;
    if jit {
        unsafe { put_cpu(m, &c) };
        let mut j = Jit::new(engine, memory, base, true);
        *LAST_EXIT.lock().unwrap() = None;
        let status = match j.once(base) {
            Ok(s) => s,
            Err(e) if e == "refused" => return Err(()),
            Err(e) => return Ok(Some(e)),
        };
        c = unsafe { get_cpu(m) };
        match status {
            // (a region left by an instruction the interpreter stepped: what that exit does)
            2 => {
                let e = LAST_EXIT.lock().unwrap().take().unwrap();
                match exit(kind, &mut c, &mut mem, e, next, &mut pushed) {
                    Ok(g) => go = g,
                    Err(e) => fail = Some(e),
                }
            }
            // (the 386's and 286's tests run on to their HLT; on the 808x the interpreter runs
            // the instruction when the region left it to it, or finishes a REP string
            // instruction the region broke off)
            _ => go = kind.ends_with_hlt() || (c.eip == start && (status == 1 || ins.is_string_instruction() && c.regs[koji::ECX] & 0xffff != 0)),
        }
    }
    if fail.is_none() && go {
        for _ in 0..100_000 {
            let e = koji::step(&mut c, &mut mem);
            match exit(kind, &mut c, &mut mem, e, next, &mut pushed) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    fail = Some(e);
                    break;
                }
            }
        }
    }
    if let (Some(a), Some(b)) = (stop, kept) {
        unsafe { *m.add(a as usize) = b };
    }
    if let Some(f) = fail {
        return Ok(Some(f));
    }
    c.materialize();
    let undef = undefined_flags(ins, r(4));
    let got = |i: usize| -> u32 {
        match i {
            2..=9 => c.regs[[0, 3, 1, 2, 6, 7, 5, 4][i - 2]],
            10 => c.sreg[1] as u32,
            11 => c.sreg[3] as u32,
            12 => c.sreg[0] as u32,
            13 => c.sreg[4] as u32,
            14 => c.sreg[5] as u32,
            15 => c.sreg[2] as u32,
            16 => c.eip,
            17 => c.flags(),
            _ => 0,
        }
    };
    let mut diffs = Vec::new();
    for i in 2..18 {
        let want = t.fina.regs[i].unwrap_or(r(i));
        let mut mk = t.mask[i];
        if i == 17 {
            mk &= FLAGS & !undef;
        }
        if (10..=15).contains(&i) || kind.regs16() {
            mk &= 0xffff;
        }
        if (got(i) ^ want) & mk != 0 {
            diffs.push(format!("{} want {want:08x} got {:08x}", NAMES[i], got(i)));
        }
    }
    let flags_at = t.excp.map(|e| e.1).or(pushed);
    for &(a, v) in &t.fina.ram {
        let g = unsafe { *m.add((G + a) as usize) };
        let mk = match flags_at {
            Some(f) if a == f => (FLAGS & !undef) as u8,
            Some(f) if a == f + 1 => ((FLAGS & !undef) >> 8) as u8,
            _ => 0xff,
        };
        if (g ^ v) & mk != 0 {
            diffs.push(format!("[{a:06x}] want {v:02x} got {g:02x}"));
        }
    }
    Ok((!diffs.is_empty()).then(|| diffs.join(", ")))
}

/// Whether a segment of the test reaches past the 808x's 1 MB (its linear address wraps to 0
/// there, which this harness's flat memory doesn't; singlestep.rs's memory does)
fn past_lines(t: &Test, kind: Kind) -> bool {
    matches!(kind, Kind::I86 | Kind::V20) && [10, 11, 12, 15].iter().any(|&i| ((t.init.regs[i].unwrap_or(0) & 0xffff) << 4) + 0x10000 > 0x10_0000)
}

pub fn main(args: &[String]) {
    let only = args.first().cloned().unwrap_or_default();
    let filter = args.get(1).cloned().unwrap_or_default();
    let per_file: usize = std::env::var("SST_N").ok().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let show: usize = std::env::var("SST_SHOW").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
    let root = std::env::var_os("SST_ROOT").map(std::path::PathBuf::from).unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/sst").into());
    let mut config = Config::new();
    config.wasm_threads(true);
    let engine = Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, MemoryType::shared(koji::jit::wasm::memory_min_pages(), koji::jit::wasm::memory_min_pages())).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    LO.store(G, Relaxed);
    HI.store(G + SPAN, Relaxed);
    let (mut total, mut failed, mut refused, mut suites) = (0, 0, 0, 0);
    for s in SUITES.iter().filter(|s| only.is_empty() || s.cpu == only) {
        let Ok(rd) = std::fs::read_dir(suite_dir(&root, s)) else {
            println!("sst: no {} suite at {} (tests/sst/fetch.sh)", s.cpu, suite_dir(&root, s).display());
            continue;
        };
        suites += 1;
        let revoked = revoked(&root, s);
        let mut files: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.to_string_lossy().ends_with(".MOO.gz")).collect();
        files.sort();
        let (mut sn, mut sbad, mut interp) = (0, 0, 0);
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().trim_end_matches(".MOO.gz").to_string();
            if !name.contains(&filter) {
                continue;
            }
            let mut data = Vec::new();
            flate2::read::GzDecoder::new(std::fs::File::open(&f).unwrap()).read_to_end(&mut data).unwrap();
            let (mut n, mut bad, mut shown) = (0, 0, 0);
            for t in parse(&data) {
                if n >= per_file {
                    break;
                }
                let ins = decode(&t.bytes);
                // what the translator does itself, of the tests singlestep.rs runs
                if !koji::jit::ok16(&ins) || skip(s.kind, &t, &revoked).is_some() || past_lines(&t, s.kind) {
                    continue;
                }
                // (the interpreter differs from the chip too: its quirk, singlestep.rs's count)
                if !matches!(run(&engine, &memory, base, &t, &ins, s.kind, false), Ok(None)) {
                    interp += 1;
                    continue;
                }
                match run(&engine, &memory, base, &t, &ins, s.kind, true) {
                    Err(()) => refused += 1,
                    Ok(None) => n += 1,
                    Ok(Some(d)) => {
                        n += 1;
                        bad += 1;
                        if shown < show {
                            shown += 1;
                            let b: String = t.bytes.iter().take(ins.len()).map(|b| format!("{b:02x}")).collect();
                            println!("{} {name} {b} {ins}: {d}", s.cpu);
                        }
                    }
                }
            }
            if bad > 0 {
                println!("{} {name}: {} of {n} match", s.cpu, n - bad);
            }
            sn += n;
            sbad += bad;
        }
        println!("sst {}: {sn} tests through the translator, {sbad} differ ({interp} the interpreter gets wrong too: not run)", s.cpu);
        total += sn;
        failed += sbad;
    }
    println!("sst: {total} tests in {suites} suites through the translator, {failed} differ, {refused} refused");
    if failed > 0 {
        std::process::exit(1);
    }
}
