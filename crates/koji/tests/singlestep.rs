//! The interpreter against SingleStepTests/80386 (https://github.com/SingleStepTests/80386):
//! real-mode tests recorded on a real 386EX, one instruction each, with the full register and
//! memory state before and after. They cover every opcode with every operand/address size
//! prefix combination (66/67), random segment overrides, LOCK and REP prefixes, BCD, shifts by
//! any count, and the exceptions a real-mode instruction raises (#DE, INT n, #UD), so they check
//! the 16/32-bit ALU semantics, flags and addressing against hardware instead of against our
//! reading of the manuals.
//!
//! koji runs Win16 code in 16-bit protected mode; real mode is the same execution model with
//! segment base = selector * 16 ([`koji::LDT_REAL`]). What koji leaves to its embedder (sake)
//! the harness does like real-mode hardware: an INT n or exception pushes FLAGS, CS, IP and jumps
//! through the vector table at 0.
//!
//! Each test file `<opcode>.MOO.gz` (MOO format, see the suite's README) holds 500-5000
//! tests. tests/sst/fetch.sh downloads the suite (pinned commit) to target/sst; without it this
//! test does nothing. `tests/sst/expected.tsv` holds the number of passing tests per file: a
//! file whose count drops fails the test, one that rises prints a note (SST_UPDATE=1 rewrites
//! the file). SST_FILTER=<substring> picks files, SST_SHOW=<n> prints the first n failures of
//! each file, SST_FILES=<dir> points at another copy of the suite.
use flate2::read::GzDecoder;
use koji::{step, Cpu, Exit, Fault, Memory};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const RAM: usize = 16 << 20;

struct Ram(Vec<u8>, Vec<u32>);
impl Memory for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
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

#[derive(Default, Clone)]
struct State {
    /// RG32 order: cr0 cr3 eax ebx ecx edx esi edi ebp esp cs ds es fs gs ss eip eflags dr6 dr7
    regs: [Option<u32>; 20],
    ram: Vec<(u32, u8)>,
}

struct Test {
    name: String,
    bytes: Vec<u8>,
    init: State,
    fina: State,
    /// RM32 masks (undefined bits) of this test, after the file's
    mask: [u32; 20],
    hash: String,
    /// EXCP: the exception or interrupt the instruction raised, and where its FLAGS went
    excp: Option<(u8, u32)>,
}

const NAMES: [&str; 20] = ["cr0", "cr3", "eax", "ebx", "ecx", "edx", "esi", "edi", "ebp", "esp", "cs", "ds", "es", "fs", "gs", "ss", "eip", "eflags", "dr6", "dr7"];

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// The chunks of `b`: (id, payload)
fn chunks(mut b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while b.len() >= 8 {
        let id: [u8; 4] = b[..4].try_into().unwrap();
        let n = u32le(b, 4) as usize;
        let n = n.min(b.len() - 8);
        out.push((id, &b[8..8 + n]));
        b = &b[8 + n..];
    }
    out
}

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
    let top = chunks(data);
    let mut file_mask = [u32::MAX; 20];
    let mut tests = Vec::new();
    for (id, c) in top {
        match &id {
            b"RM32" => masks32(c, &mut file_mask),
            b"TEST" => {
                let mut t = Test { name: String::new(), bytes: Vec::new(), init: State::default(), fina: State::default(), mask: file_mask, hash: String::new(), excp: None };
                for (id, c) in chunks(&c[4..]) {
                    match &id {
                        b"NAME" => t.name = String::from_utf8_lossy(&c[4..4 + u32le(c, 0) as usize]).into_owned(),
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

/// Flags compared: CF PF AF ZF SF TF DF OF. Not IF, IOPL, NT: koji is a user-mode CPU (POPF
/// and IRET leave them alone, as Windows' user mode at IOPL 0 does), nor the reserved bits and
/// the upper half (RF, VM; no AC on a 386).
const FLAGS: u32 = 0x0dd5;

/// Real-mode interrupt delivery: push FLAGS, CS, IP (the IP given), clear IF and TF, jump
/// through the vector table.
fn interrupt(cpu: &mut Cpu, mem: &mut Ram, n: u8, ret: u32) -> Result<(), Fault> {
    let flags = cpu.flags();
    // (the vector is read first: the pushes may land on it when SS:SP is near 0)
    let ip = mem.read16(n as u32 * 4)?;
    let cs = mem.read16(n as u32 * 4 + 2)?;
    let mut sp = cpu.regs[koji::ESP] as u16;
    let ss = cpu.sbase[2];
    for v in [flags as u16, cpu.sreg[1], ret as u16] {
        sp = sp.wrapping_sub(2);
        mem.write16(ss.wrapping_add(sp as u32), v)?;
    }
    cpu.regs[koji::ESP] = cpu.regs[koji::ESP] & 0xffff_0000 | sp as u32;
    cpu.eflags = flags & !0x300;
    cpu.sreg[1] = cs;
    cpu.sbase[1] = (cs as u32) << 4;
    cpu.eip = ip as u32;
    Ok(())
}

/// EFLAGS bits the instruction leaves undefined (Intel's manuals; iced's tables). The 386
/// sets them in its own way, later CPUs differently, and the games run on later CPUs: not
/// compared.
/// A shift by CL of `count` (masked to 5 bits) at least the operand's width leaves CF and OF
/// undefined too (Intel: "CF undefined for SHL/SHR/SAR where count >= size").
fn undefined_flags(bytes: &[u8], ecx: u32) -> u32 {
    let ins = iced_x86::Decoder::new(16, bytes, iced_x86::DecoderOptions::NONE).decode();
    let u = ins.rflags_undefined();
    use iced_x86::RflagsBits as R;
    let mut f = [(R::OF, 0x800), (R::SF, 0x80), (R::ZF, 0x40), (R::AF, 0x10), (R::CF, 1), (R::PF, 4)].iter().filter(|(r, _)| u & r != 0).map(|(_, b)| b).sum();
    use iced_x86::Mnemonic::*;
    if matches!(ins.mnemonic(), Shl | Sal | Shr | Sar) && ins.op_count() == 2 && ins.op1_kind() == iced_x86::OpKind::Register {
        let bits = ins.memory_size().size() as u32 * 8;
        let bits = if bits == 0 { reg_bits(ins.op0_register()) } else { bits };
        if ecx & 31 >= bits {
            f |= 0x801;
        }
    }
    f
}

fn reg_bits(r: iced_x86::Register) -> u32 {
    r.size() as u32 * 8
}

/// Tests not run: I/O and privileged instructions (koji is a user-mode CPU: they fault), and
/// SHLD/SHRD by more than the operand's width (result and flags undefined).
fn not_modeled(bytes: &[u8], ecx: u32) -> bool {
    let ins = iced_x86::Decoder::new(16, bytes, iced_x86::DecoderOptions::NONE).decode();
    use iced_x86::Mnemonic::*;
    match ins.mnemonic() {
        In | Out | Insb | Insw | Insd | Outsb | Outsw | Outsd | Cli | Sti | Clts | Lmsw | Smsw | Lgdt | Lidt | Sgdt | Sidt | Lldt | Sldt | Ltr | Str | Hlt | Loadall => true,
        Shld | Shrd => {
            let bits = if ins.op0_kind() == iced_x86::OpKind::Register { reg_bits(ins.op0_register()) } else { ins.memory_size().size() as u32 * 8 };
            let n = if ins.op_count() == 3 && ins.op2_kind() == iced_x86::OpKind::Immediate8 { ins.immediate8() as u32 } else { ecx };
            n & 31 > bits
        }
        _ => false,
    }
}

/// Runs one test; None when it matched, else what differed.
/// A SIB byte without an index (index 100) but with a scale: the 386 scales the base, later
/// CPUs ignore the scale (the suite's README); undefined, not run.
fn invalid_sib(bytes: &[u8]) -> bool {
    let ins = iced_x86::Decoder::new(16, bytes, iced_x86::DecoderOptions::NONE).decode();
    if ins.memory_index() != iced_x86::Register::None || !(0..ins.op_count()).any(|i| ins.op_kind(i) == iced_x86::OpKind::Memory) {
        return false;
    }
    // (find the ModR/M after the prefixes and opcode: 32-bit addressing with rm 100 has a SIB)
    let code = &bytes[..ins.len()];
    let Some(p) = code.iter().position(|b| !matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3)) else { return false };
    let a32 = code[..p].contains(&0x67);
    let op = if code[p] == 0x0f { p + 2 } else { p + 1 };
    match (a32, code.get(op), code.get(op + 1)) {
        (true, Some(&m), Some(&sib)) => m >> 6 != 3 && m & 7 == 4 && (sib >> 3) & 7 == 4 && sib >> 6 != 0,
        _ => false,
    }
}

fn run(t: &Test, mem: &mut Ram) -> Option<String> {
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
    for _ in 0..100_000 {
        match step(&mut cpu, mem) {
            Exit::Continue => {}
            Exit::Halt => {
                cpu.eip = cpu.eip.wrapping_add(1); // (an HLT at FFFF leaves IP at 10000)
                break;
            }
            Exit::Interrupt(n) => {
                let ip = cpu.eip; // (past the INT)
                if let Err(f) = interrupt(&mut cpu, mem, n, ip) {
                    result = Some(format!("fault {f:?} delivering int {n}"));
                    break;
                }
            }
            e @ (Exit::DivideError | Exit::InvalidOpcode) => {
                let n = if e == Exit::DivideError { 0 } else { 6 };
                let ip = cpu.eip;
                if let Err(f) = interrupt(&mut cpu, mem, n, ip) {
                    result = Some(format!("fault {f:?} delivering #{n}"));
                    break;
                }
            }
            e => {
                result = Some(format!("exit {e:?}"));
                break;
            }
        }
    }
    let mut diffs = Vec::new();
    let undef = undefined_flags(&t.bytes, r(4));
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
            if i >= 10 && i <= 15 {
                m &= 0xffff;
            }
            if (got(i) ^ want) & m != 0 {
                diffs.push(format!("{} want {want:08x} got {:08x}", NAMES[i], got(i)));
            }
        }
        for &(a, v) in &t.fina.ram {
            let g = mem.0[a as usize];
            // (FLAGS an exception pushed: the defined ones)
            let m = match t.excp {
                Some((_, f)) if a == f => (FLAGS & !undef) as u8,
                Some((_, f)) if a == f + 1 => ((FLAGS & !undef) >> 8) as u8,
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

fn suite_dir() -> PathBuf {
    std::env::var_os("SST_FILES").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/sst/v1_ex_real_mode"))
}

fn expected_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/sst/expected.tsv")
}

#[test]
fn singlestep_386_real_mode() {
    let dir = suite_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("singlestep: no suite at {} (tests/sst/fetch.sh)", dir.display());
        return;
    };
    let revoked: Vec<String> = std::fs::read_to_string(dir.join("../revocation_list.txt")).unwrap_or_default().lines().filter(|l| !l.starts_with('#')).map(|l| l.trim().to_string()).collect();
    let filter = std::env::var("SST_FILTER").unwrap_or_default();
    let show: usize = std::env::var("SST_SHOW").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.to_string_lossy().ends_with(".MOO.gz")).collect();
    files.sort();
    let expected: BTreeMap<String, (usize, usize)> = std::fs::read_to_string(expected_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.to_string(), (f.next()?.parse().ok()?, f.next()?.parse().ok()?)))
        })
        .collect();
    let mut mem = Ram(vec![0; RAM], Vec::new());
    let mut now = BTreeMap::new();
    let (mut pass, mut total, mut worse) = (0, 0, Vec::new());
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().trim_end_matches(".MOO.gz").to_string();
        if !name.contains(&filter) {
            continue;
        }
        let mut data = Vec::new();
        GzDecoder::new(std::fs::File::open(&f).unwrap()).read_to_end(&mut data).unwrap();
        let tests = parse(&data);
        let mut ok = 0;
        let mut shown = 0;
        let mut n = 0;
        for t in &tests {
            // (#SS and #GP: real-mode segment limits, which koji does not have: Win16 code
            // runs with limits it never exceeds)
            if revoked.contains(&t.hash) || matches!(t.excp, Some((12 | 13, _))) || invalid_sib(&t.bytes) || not_modeled(&t.bytes, t.init.regs[4].unwrap_or(0)) {
                continue;
            }
            n += 1;
            match run(t, &mut mem) {
                None => ok += 1,
                Some(d) if shown < show => {
                    shown += 1;
                    eprintln!("{name} {} [{}] excp {:?}: {d}", t.name, t.bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(), t.excp);
                }
                Some(_) => {}
            }
        }
        pass += ok;
        total += n;
        if let Some(&(want, _)) = expected.get(&name) {
            if ok < want {
                worse.push(format!("{name}: {ok}/{n} pass, expected.tsv says {want}"));
            } else if ok > want {
                eprintln!("singlestep: {name} improved {want} -> {ok}/{n} (SST_UPDATE=1 records it)");
            }
        } else if ok < n {
            worse.push(format!("{name}: {ok}/{n} pass, not in expected.tsv"));
        }
        now.insert(name, (ok, n));
    }
    eprintln!("singlestep: {pass}/{total} tests pass in {} files", now.len());
    if std::env::var_os("SST_UPDATE").is_some() {
        let mut all = expected.clone();
        all.extend(now);
        let mut s = String::from("# file\tpassing\ttests  (crates/koji/tests/singlestep.rs; SST_UPDATE=1 rewrites)\n");
        for (k, (a, b)) in all {
            s += &format!("{k}\t{a}\t{b}\n");
        }
        std::fs::write(expected_path(), s).unwrap();
        return;
    }
    assert!(worse.is_empty(), "fewer passing tests than expected:\n{}", worse.join("\n"));
}
