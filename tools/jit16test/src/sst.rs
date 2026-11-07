//! koji's 16-bit translation against SingleStepTests/80386 (real-mode tests recorded on a real
//! 386EX, tests/sst/fetch.sh): each test whose instruction the translator does itself (`ok16`)
//! runs as a translated region (a block of its own, then the interpreter for the HLT that ends
//! the test, an INT or a jump's target), and registers, flags and memory must be the 386's.
//! crates/koji/tests/singlestep.rs checks the interpreter on the same suite; this checks the
//! wasm the translator makes, real hardware the reference.
//!
//!   cargo run --release -- sst [dir (target/sst/v1_ex_real_mode)] [file filter]
//!   SST_N=<n> tests per file (default all), SST_SHOW=<n> failures shown per file
//!
//! The guest's linear 0 sits at G in the wasm memory (the harness's Cpu, dispatch table and ring
//! are below): segment bases are selector * 16 + G. Instructions that load a segment register
//! are interpreter-only (not ok16), so no base is ever made without G.
use crate::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind, RflagsBits};
use std::io::Read;

const G: u32 = 0x20_0000;
const SPAN: u32 = 0x12_0000;

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn chunks(mut b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while b.len() >= 8 {
        let id: [u8; 4] = b[..4].try_into().unwrap();
        let n = (u32le(b, 4) as usize).min(b.len() - 8);
        out.push((id, &b[8..8 + n]));
        b = &b[8 + n..];
    }
    out
}

#[derive(Default, Clone)]
struct State {
    /// RG32 order: cr0 cr3 eax ebx ecx edx esi edi ebp esp cs ds es fs gs ss eip eflags dr6 dr7
    regs: [Option<u32>; 20],
    ram: Vec<(u32, u8)>,
}

struct Test {
    bytes: Vec<u8>,
    init: State,
    fina: State,
    mask: [u32; 20],
    hash: String,
    excp: Option<(u8, u32)>,
}

const NAMES: [&str; 20] = [
    "cr0", "cr3", "eax", "ebx", "ecx", "edx", "esi", "edi", "ebp", "esp", "cs", "ds", "es", "fs", "gs", "ss", "eip", "eflags", "dr6", "dr7",
];

fn regs32(p: &[u8], into: &mut [Option<u32>; 20]) {
    let m = u32le(p, 0);
    let mut o = 4;
    for (i, r) in into.iter_mut().enumerate() {
        if m & 1 << i != 0 {
            *r = Some(u32le(p, o));
            o += 4;
        }
    }
}

fn masks32(p: &[u8], into: &mut [u32; 20]) {
    let mut v = [None; 20];
    regs32(p, &mut v);
    for (m, x) in into.iter_mut().zip(v) {
        if let Some(x) = x {
            *m = x;
        }
    }
}

fn state(p: &[u8]) -> (State, Option<[u32; 20]>) {
    let mut s = State::default();
    let mut mask = None;
    for (id, c) in chunks(p) {
        match &id {
            b"RG32" => regs32(c, &mut s.regs),
            b"RM32" => {
                let mut m = [u32::MAX; 20];
                masks32(c, &mut m);
                mask = Some(m);
            }
            b"RAM " => {
                let n = u32le(c, 0) as usize;
                s.ram = (0..n).map(|k| (u32le(c, 4 + 5 * k), c[8 + 5 * k])).collect();
            }
            _ => {}
        }
    }
    (s, mask)
}

fn parse(data: &[u8]) -> Vec<Test> {
    let mut file_mask = [u32::MAX; 20];
    let mut tests = Vec::new();
    for (id, c) in chunks(data) {
        match &id {
            b"RM32" => masks32(c, &mut file_mask),
            b"TEST" => {
                let mut t = Test {
                    bytes: Vec::new(),
                    init: State::default(),
                    fina: State::default(),
                    mask: file_mask,
                    hash: String::new(),
                    excp: None,
                };
                for (id, c) in chunks(&c[4..]) {
                    match &id {
                        b"BYTS" => t.bytes = c[4..4 + u32le(c, 0) as usize].to_vec(),
                        b"INIT" => t.init = state(c).0,
                        b"FINA" => {
                            let (s, m) = state(c);
                            t.fina = s;
                            if let Some(m) = m {
                                for (a, b) in t.mask.iter_mut().zip(m) {
                                    *a &= b;
                                }
                            }
                        }
                        b"EXCP" => t.excp = Some((c[0], u32le(c, 1))),
                        b"HASH" => t.hash = c.iter().map(|b| format!("{b:02x}")).collect(),
                        _ => {}
                    }
                }
                tests.push(t);
            }
            _ => {}
        }
    }
    tests
}

/// Flags compared (as singlestep.rs): CF PF AF ZF SF TF DF OF
const FLAGS: u32 = 0x0dd5;

/// EFLAGS bits the instruction leaves undefined (as singlestep.rs)
fn undefined_flags(ins: &iced_x86::Instruction, ecx: u32) -> u32 {
    let u = ins.rflags_undefined();
    let mut f = [
        (RflagsBits::OF, 0x800),
        (RflagsBits::SF, 0x80),
        (RflagsBits::ZF, 0x40),
        (RflagsBits::AF, 0x10),
        (RflagsBits::CF, 1),
        (RflagsBits::PF, 4),
    ]
    .iter()
    .filter(|(r, _)| u & r != 0)
    .map(|(_, b)| b)
    .sum();
    if matches!(ins.mnemonic(), Mnemonic::Shl | Mnemonic::Sal | Mnemonic::Shr | Mnemonic::Sar) && ins.op_count() == 2 && ins.op1_kind() == OpKind::Register {
        let bits = ins.memory_size().size() as u32 * 8;
        let bits = if bits == 0 { ins.op0_register().size() as u32 * 8 } else { bits };
        if ecx & 31 >= bits {
            f |= 0x801;
        }
    }
    f
}

/// A SIB without an index but with a scale (the 386 scales the base; undefined): not run
fn invalid_sib(ins: &iced_x86::Instruction, bytes: &[u8]) -> bool {
    if ins.memory_index() != Register::None || !(0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory) {
        return false;
    }
    let code = &bytes[..ins.len()];
    let Some(p) = code.iter().position(|b| !matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3)) else {
        return false;
    };
    let a32 = code[..p].contains(&0x67);
    let op = if code[p] == 0x0f { p + 2 } else { p + 1 };
    match (a32, code.get(op), code.get(op + 1)) {
        (true, Some(&m), Some(&sib)) => m >> 6 != 3 && m & 7 == 4 && (sib >> 3) & 7 == 4 && sib >> 6 != 0,
        _ => false,
    }
}

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

/// What a test's exit leaves to do (as singlestep.rs's loop): false to stop
fn after_exit(c: &mut Cpu, mem: &mut Mem, e: Exit) -> Result<bool, String> {
    match e {
        Exit::Continue => Ok(true),
        Exit::Halt => {
            c.eip = c.eip.wrapping_add(1);
            Ok(false)
        }
        Exit::Interrupt(n) => {
            let ip = c.eip;
            interrupt(c, mem, n, ip).map_err(|f| format!("fault {f:?} delivering int {n}"))?;
            Ok(true)
        }
        e @ (Exit::DivideError | Exit::InvalidOpcode) => {
            let n = if e == Exit::DivideError { 0 } else { 6 };
            let ip = c.eip;
            interrupt(c, mem, n, ip).map_err(|f| format!("fault {f:?} delivering #{n}"))?;
            Ok(true)
        }
        e => Err(format!("exit {e:?}")),
    }
}

/// One test: the instruction in a region, the rest stepped; None when it matched. `Err` when
/// the region was refused (not counted).
fn run(engine: &Engine, memory: &SharedMemory, base: usize, t: &Test, ins: &iced_x86::Instruction) -> Result<Option<String>, ()> {
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
    unsafe { put_cpu(m, &c) };
    let mut jit = Jit::new(engine, memory, base, true);
    *LAST_EXIT.lock().unwrap() = None;
    let status = match jit.once(base) {
        Ok(s) => s,
        Err(e) if e == "refused" => return Err(()),
        Err(e) => return Ok(Some(e)),
    };
    let mut c = unsafe { get_cpu(m) };
    let mut mem = Mem(m);
    let mut fail = None;
    // a region left by an instruction the interpreter stepped: what that exit does
    let mut go = true;
    if status == 2 {
        let e = LAST_EXIT.lock().unwrap().take().unwrap();
        match after_exit(&mut c, &mut mem, e) {
            Ok(g) => go = g,
            Err(e) => fail = Some(e),
        }
    }
    if fail.is_none() && go {
        for _ in 0..100_000 {
            let e = koji::step(&mut c, &mut mem);
            match after_exit(&mut c, &mut mem, e) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    fail = Some(e);
                    break;
                }
            }
        }
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
        if (10..=15).contains(&i) {
            mk &= 0xffff;
        }
        if (got(i) ^ want) & mk != 0 {
            diffs.push(format!("{} want {want:08x} got {:08x}", NAMES[i], got(i)));
        }
    }
    for &(a, v) in &t.fina.ram {
        let g = unsafe { *m.add((G + a) as usize) };
        let mk = match t.excp {
            Some((_, f)) if a == f => (FLAGS & !undef) as u8,
            Some((_, f)) if a == f + 1 => ((FLAGS & !undef) >> 8) as u8,
            _ => 0xff,
        };
        if (g ^ v) & mk != 0 {
            diffs.push(format!("[{a:06x}] want {v:02x} got {g:02x}"));
        }
    }
    Ok((!diffs.is_empty()).then(|| diffs.join(", ")))
}

pub fn main(args: &[String]) {
    let dir = args.first().cloned().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/sst/v1_ex_real_mode").into());
    let filter = args.get(1).cloned().unwrap_or_default();
    let per_file: usize = std::env::var("SST_N").ok().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let show: usize = std::env::var("SST_SHOW").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
    let dir = std::path::PathBuf::from(dir);
    let revoked: Vec<String> = std::fs::read_to_string(dir.join("../revocation_list.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.trim().to_string())
        .collect();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("no suite (tests/sst/fetch.sh)")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".MOO.gz"))
        .collect();
    files.sort();
    let mut config = Config::new();
    config.wasm_threads(true);
    let engine = Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, MemoryType::shared(koji::jit::wasm::memory_min_pages(), koji::jit::wasm::memory_min_pages())).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    LO.store(G, Relaxed);
    HI.store(G + SPAN, Relaxed);
    let (mut total, mut failed, mut refused) = (0, 0, 0);
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
            let ins = Decoder::with_ip(16, &t.bytes, t.init.regs[16].unwrap_or(0) as u64, DecoderOptions::NONE).decode();
            // what the translator does itself, as singlestep.rs filters (revoked tests, #SS/#GP
            // of real-mode limits, the undefined SIB)
            if !koji::jit::ok16(&ins) || revoked.contains(&t.hash) || matches!(t.excp, Some((12 | 13, _))) || invalid_sib(&ins, &t.bytes) {
                continue;
            }
            match run(&engine, &memory, base, &t, &ins) {
                Err(()) => refused += 1,
                Ok(None) => n += 1,
                Ok(Some(d)) => {
                    n += 1;
                    bad += 1;
                    if shown < show {
                        shown += 1;
                        let b: String = t.bytes.iter().take(ins.len()).map(|b| format!("{b:02x}")).collect();
                        println!("{name} {b} {ins}: {d}");
                    }
                }
            }
        }
        if n > 0 {
            println!("{name}: {} of {n} match", n - bad);
        }
        total += n;
        failed += bad;
    }
    println!("sst: {total} tests through the translator, {failed} differ, {refused} refused");
    if failed > 0 {
        std::process::exit(1);
    }
}
