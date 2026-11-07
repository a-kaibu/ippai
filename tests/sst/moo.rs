//! SingleStepTests (https://github.com/SingleStepTests): the suites, their MOO files, and which
//! tests koji answers as the chip did. Shared by crates/koji/tests/singlestep.rs (the
//! interpreter) and tools/jit16test (`sst`: the 16-bit JIT). tests/sst/fetch.sh downloads them.
//!
//! MOO (Machine Opcode Operation, the suites' README): chunks of (4-byte id, u32 length,
//! payload). A TEST holds NAME, BYTS (the instruction), INIT and FINA (registers: RG32 with
//! RM32 masks of undefined bits on the 386, REGS of 16-bit registers on the others; RAM as
//! (address, byte) pairs), EXCP (an exception: number, where its FLAGS went) and HASH.
#![allow(dead_code)]
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register, RflagsBits};

/// Which chip recorded a suite: what of it koji, a 386-and-later CPU, can be held to
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 80386EX real mode (32-bit operands and addresses with 66/67)
    I386,
    /// 80286 real mode: 16-bit only, 24 address lines
    I286,
    /// 8088/8086 (NMOS): no invalid opcodes (every byte does something), 20 address lines,
    /// shift counts unmasked, PUSH SP pushes the new SP, a divide error returns past the
    /// instruction
    I86,
    /// NEC V20 (native mode): an 8088 with the 186's instructions and its own extensions
    V20,
}

pub struct Suite {
    pub cpu: &'static str,
    /// the directory of MOO.gz files under target/sst/<cpu>/
    pub dir: &'static str,
    pub kind: Kind,
}

pub const SUITES: [Suite; 5] = [
    Suite { cpu: "80386", dir: "v1_ex_real_mode", kind: Kind::I386 },
    Suite { cpu: "80286", dir: "v1_real_mode", kind: Kind::I286 },
    Suite { cpu: "8086", dir: "v1_binary", kind: Kind::I86 },
    Suite { cpu: "8088", dir: "v2_binary", kind: Kind::I86 },
    Suite { cpu: "V20", dir: "v1_native_binary", kind: Kind::V20 },
];

impl Kind {
    /// The address lines: linear addresses wrap there
    pub fn addr_mask(self) -> u32 {
        match self {
            Kind::I386 => u32::MAX,
            Kind::I286 => 0xff_ffff,
            _ => 0xf_ffff,
        }
    }
    /// 16-bit registers only
    pub fn regs16(self) -> bool {
        self != Kind::I386
    }
    /// The test ends with a HLT the suite put after the instruction (386, 286); the 808x
    /// suites end at the next instruction (NOPs follow)
    pub fn ends_with_hlt(self) -> bool {
        matches!(self, Kind::I386 | Kind::I286)
    }
}

pub fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// The chunks of `b`: (id, payload)
pub fn chunks(mut b: &[u8]) -> Vec<([u8; 4], &[u8])> {
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
pub struct State {
    /// RG32 order: cr0 cr3 eax ebx ecx edx esi edi ebp esp cs ds es fs gs ss eip eflags dr6 dr7
    pub regs: [Option<u32>; 20],
    pub ram: Vec<(u32, u8)>,
}

pub struct Test {
    pub name: String,
    pub bytes: Vec<u8>,
    pub init: State,
    pub fina: State,
    /// RM32 masks (undefined bits) of this test, after the file's
    pub mask: [u32; 20],
    pub hash: String,
    /// EXCP: the exception or interrupt the instruction raised, and where its FLAGS went
    pub excp: Option<(u8, u32)>,
}

pub const NAMES: [&str; 20] = ["cr0", "cr3", "eax", "ebx", "ecx", "edx", "esi", "edi", "ebp", "esp", "cs", "ds", "es", "fs", "gs", "ss", "eip", "eflags", "dr6", "dr7"];

/// REGS (16-bit suites): ax bx cx dx cs ss ds es sp bp si di ip flags, as RG32 indices
const REGS16: [usize; 14] = [2, 3, 4, 5, 10, 15, 11, 12, 9, 8, 6, 7, 16, 17];

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

fn regs16(p: &[u8], into: &mut [Option<u32>; 20]) {
    let m = u16::from_le_bytes([p[0], p[1]]);
    let mut o = 2;
    for (i, &k) in REGS16.iter().enumerate() {
        if m & 1 << i != 0 {
            into[k] = Some(u16::from_le_bytes([p[o], p[o + 1]]) as u32);
            o += 2;
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
            b"REGS" => regs16(c, &mut s.regs),
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

pub fn parse(data: &[u8]) -> Vec<Test> {
    let mut file_mask = [u32::MAX; 20];
    let mut tests = Vec::new();
    for (id, c) in chunks(data) {
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
/// and IRET leave them alone, as Windows' user mode at IOPL 0 does), nor the reserved bits
/// (the 808x's and 286's top four) and the upper half (RF, VM; no AC on a 386).
pub const FLAGS: u32 = 0x0dd5;

pub fn decode(bytes: &[u8]) -> Instruction {
    Decoder::new(16, bytes, DecoderOptions::NONE).decode()
}

/// EFLAGS bits the instruction leaves undefined (Intel's manuals; iced's tables). The chips
/// set them in their own ways, later CPUs differently, and the games run on later CPUs: not
/// compared. A shift by CL of at least the operand's width (count masked to 5 bits) leaves CF
/// and OF undefined too (Intel: "CF undefined for SHL/SHR/SAR where count >= size").
pub fn undefined_flags(ins: &Instruction, ecx: u32) -> u32 {
    let u = ins.rflags_undefined();
    let mut f = [(RflagsBits::OF, 0x800), (RflagsBits::SF, 0x80), (RflagsBits::ZF, 0x40), (RflagsBits::AF, 0x10), (RflagsBits::CF, 1), (RflagsBits::PF, 4)].iter().filter(|(r, _)| u & r != 0).map(|(_, b)| b).sum();
    if matches!(ins.mnemonic(), Mnemonic::Shl | Mnemonic::Sal | Mnemonic::Shr | Mnemonic::Sar) && ins.op_count() == 2 && ins.op1_kind() == OpKind::Register {
        let bits = ins.memory_size().size() as u32 * 8;
        let bits = if bits == 0 { ins.op0_register().size() as u32 * 8 } else { bits };
        if ecx & 31 >= bits {
            f |= 0x801;
        }
    }
    f
}

/// A SIB byte without an index (index 100) but with a scale: the 386 scales the base, later
/// CPUs ignore the scale (the suite's README); undefined, not run
pub fn invalid_sib(ins: &Instruction, bytes: &[u8]) -> bool {
    if ins.memory_index() != Register::None || !(0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory) {
        return false;
    }
    // (find the ModR/M after the prefixes and opcode: 32-bit addressing with rm 100 has a SIB)
    let code = &bytes[..ins.len().min(bytes.len())];
    let Some(p) = code.iter().position(|b| !matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3)) else { return false };
    let a32 = code[..p].contains(&0x67);
    let op = if code[p] == 0x0f { p + 2 } else { p + 1 };
    match (a32, code.get(op), code.get(op + 1)) {
        (true, Some(&m), Some(&sib)) => m >> 6 != 3 && m & 7 == 4 && (sib >> 3) & 7 == 4 && sib >> 6 != 0,
        _ => false,
    }
}

/// The opcode byte after the prefixes the chip knows (the 808x and 286: no 66/67/64/65), and
/// the ModR/M byte's reg field (when there is a next byte)
fn opcode(kind: Kind, bytes: &[u8]) -> Option<(usize, u8, Option<u8>)> {
    let pre: &[u8] = match kind {
        Kind::I386 => &[0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65, 0x66, 0x67, 0xf0, 0xf2, 0xf3],
        // (0xf1: the 808x's alias of LOCK; V20: 0x64/0x65 are REPNC/REPC)
        Kind::I86 => &[0x26, 0x2e, 0x36, 0x3e, 0xf0, 0xf1, 0xf2, 0xf3],
        Kind::V20 => &[0x26, 0x2e, 0x36, 0x3e, 0xf0, 0xf1, 0xf2, 0xf3, 0x64, 0x65],
        Kind::I286 => &[0x26, 0x2e, 0x36, 0x3e, 0xf0, 0xf2, 0xf3],
    };
    let p = bytes.iter().position(|b| !pre.contains(b))?;
    Some((p, bytes[p], bytes.get(p + 1).map(|m| (m >> 3) & 7)))
}

/// Whether LOCK is allowed on the instruction (the 386's rule: a read-modify-write of memory)
fn lockable(ins: &Instruction) -> bool {
    use Mnemonic::*;
    ins.op0_kind() == OpKind::Memory
        && matches!(ins.mnemonic(), Add | Adc | And | Btc | Btr | Bts | Cmpxchg | Dec | Inc | Neg | Not | Or | Sbb | Sub | Xor | Xadd | Xchg)
}

/// Why a test is not run (None: it is), for the chip's behaviour that is not the IA-32's koji
/// implements. `ecx`: the test's ECX (shift counts).
pub fn not_modeled(kind: Kind, bytes: &[u8], ecx: u32) -> Option<&'static str> {
    let ins = decode(bytes);
    use Mnemonic::*;
    // I/O and privileged instructions (koji is a user-mode CPU: they fault)
    if matches!(ins.mnemonic(), In | Out | Insb | Insw | Insd | Outsb | Outsw | Outsd | Cli | Sti | Clts | Lmsw | Smsw | Lgdt | Lidt | Sgdt | Sidt | Lldt | Sldt | Ltr | Str | Hlt | Loadall | Arpl | Lar | Lsl | Verr | Verw) {
        return Some("I/O or privileged");
    }
    // SHLD/SHRD by more than the operand's width: result and flags undefined
    if matches!(ins.mnemonic(), Shld | Shrd) {
        let bits = if ins.op0_kind() == OpKind::Register { ins.op0_register().size() as u32 * 8 } else { ins.memory_size().size() as u32 * 8 };
        let n = if ins.op_count() == 3 && ins.op2_kind() == OpKind::Immediate8 { ins.immediate8() as u32 } else { ecx };
        if n & 31 > bits {
            return Some("SHLD/SHRD past the width");
        }
    }
    let Some((p, op, reg)) = opcode(kind, bytes) else { return Some("prefixes only") };
    let prefixes = &bytes[..p];
    let rep = prefixes.iter().any(|b| matches!(b, 0xf2 | 0xf3));
    match kind {
        Kind::I386 => None,
        Kind::I286 => match op {
            // protected-mode system instructions, LOADALL; no 32-bit prefixes, FS/GS
            0x0f => Some("0F (286 system instructions)"),
            0x64..=0x67 => Some("no 66/67/64/65 on the 286"),
            // no coprocessor
            0xd8..=0xdf | 0x9b => Some("ESC without a coprocessor"),
            0xd6 | 0xf1 => Some("undocumented opcode"),
            // the 286's #UD forms koji (a 386) reads otherwise: MOV of FS/GS, LDS/LES of a register
            0x8c | 0x8e if reg.is_some_and(|r| r >= 4) => Some("8C/8E segment register 4-7"),
            0xc4 | 0xc5 if bytes.get(p + 1).is_some_and(|m| m >> 6 == 3) => Some("LES/LDS of a register"),
            // LOCK on an instruction that can't be locked: #UD on the 386, ignored by the 286
            _ if prefixes.contains(&0xf0) && !lockable(&ins) => Some("LOCK on a non-lockable instruction (286)"),
            _ => None,
        },
        Kind::I86 | Kind::V20 => {
            let v20 = kind == Kind::V20;
            let count = match op {
                0xd2 | 0xd3 => Some(ecx & 0xff),
                0xc0 | 0xc1 if v20 => bytes.get(ins.len().saturating_sub(1)).map(|&b| b as u32),
                _ => None,
            };
            match op {
                // 8086: POP CS; V20: its extended instructions
                0x0f => Some("0F (POP CS / V20 extensions)"),
                // 8086: aliases of the conditional jumps 70-7F; V20: the 186's (60-62, 68-6B
                // compared) and undefined/prefix bytes
                0x60..=0x6f if !v20 => Some("60-6F (aliases of 70-7F on the 808x)"),
                0x63..=0x67 | 0x6c..=0x6f if v20 => Some("V20 63-67, INS/OUTS"),
                // 808x: aliases of RET imm / RET / RETF imm / RETF
                0xc0 | 0xc1 | 0xc8 | 0xc9 if !v20 => Some("C0/C1/C8/C9 (aliases on the 808x)"),
                0xd6 => Some("SALC"),
                0xd8..=0xdf => Some("ESC (no coprocessor: EA only)"),
                0x9b => Some("WAIT"),
                // undefined ModR/M forms the 808x executes as something
                0x8c | 0x8e if reg.is_some_and(|r| r >= 4) => Some("8C/8E segment register 4-7"),
                0x8e if reg == Some(1) => Some("MOV CS"),
                0x8f if reg.is_some_and(|r| r != 0) => Some("8F reg != 0"),
                0xc6 | 0xc7 if reg.is_some_and(|r| r != 0) => Some("C6/C7 reg != 0"),
                0xfe if reg.is_some_and(|r| r >= 2) => Some("FE reg >= 2"),
                0xff if reg == Some(7) => Some("FF /7"),
                0xd0..=0xd3 | 0xc0 | 0xc1 if reg == Some(6) => Some("SETMO (shift group /6)"),
                0x8d | 0xc4 | 0xc5 if bytes.get(p + 1).is_some_and(|m| m >> 6 == 3) => Some("LEA/LES/LDS of a register"),
                // shift counts: the 808x and V20 don't mask them to 5 bits
                _ if count.is_some_and(|c| c >= 32) => Some("shift count >= 32 (unmasked on the 808x)"),
                // PUSH SP: the 808x pushes SP after the decrement
                0x54 => Some("PUSH SP (808x)"),
                0xff if reg == Some(6) && bytes.get(p + 1).is_some_and(|m| *m == 0xf4) => Some("PUSH SP (808x)"),
                // the 808x's REP prefix negates MUL/IDIV results
                0xf6 | 0xf7 if rep && reg.is_some_and(|r| r >= 4) => Some("REP MUL/DIV (808x)"),
                // V20: REPC/REPNC (repeat while carry / no carry)
                0xa4..=0xa7 | 0xaa..=0xaf if v20 && prefixes.iter().any(|b| matches!(b, 0x64 | 0x65)) => Some("REPC/REPNC (V20)"),
                // V20: AAM/AAD ignore the base (always 10)
                0xd4 | 0xd5 if v20 && bytes.get(p + 1) != Some(&0x0a) => Some("AAM/AAD base (V20)"),
                _ => None,
            }
        }
    }
}

/// Whether the 16-bit instruction (808x) accesses across the end of a 64 KB segment: the
/// 808x wraps the offset (the word at FFFF is FFFF and 0000; IP past FFFF goes on at 0000;
/// the 286 too for each word of a far pointer or BOUND pair at FFFE),
/// the 286 and 386 raise #GP/#SS there (not run either), and koji, whose Win16 code never
/// does it, goes on linearly. The instruction's bytes, its memory operand (from the
/// registers), the string instructions' ranges, the stack's words.
pub fn wraps(t: &Test) -> bool {
    let r = |i: usize| t.init.regs[i].unwrap_or(0) & 0xffff;
    let ins = decode(&t.bytes);
    if r(16) + t.bytes.len() as u32 > 0x10000 {
        return true;
    }
    let size = ins.memory_size().size() as u32;
    let reg = |x: Register| match x {
        Register::BX => r(3),
        Register::BP => r(8),
        Register::SI => r(6),
        Register::DI => r(7),
        _ => 0,
    };
    let string = ins.is_string_instruction();
    if (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory) && !string {
        let ea = (reg(ins.memory_base()) + reg(ins.memory_index()) + ins.memory_displacement32()) & 0xffff;
        if ea + size.max(1) > 0x10000 {
            return true;
        }
    }
    if string {
        let n = if ins.has_rep_prefix() || ins.has_repe_prefix() || ins.has_repne_prefix() { r(4) } else { 1 };
        let back = r(17) & 0x400 != 0;
        for k in 0..ins.op_count() {
            let x = match ins.op_kind(k) {
                OpKind::MemorySegSI => r(6),
                OpKind::MemoryESDI => r(7),
                _ => continue,
            };
            let span = n.max(1) * size;
            if if back { x + size > 0x10000 || x + size < span } else { x + span > 0x10000 } {
                return true;
            }
        }
    }
    // (the stack: an odd SP near either end makes a word straddle FFFF)
    let sp = r(9);
    let stack = ins.stack_pointer_increment() != 0 || matches!(ins.mnemonic(), Mnemonic::Int | Mnemonic::Int3 | Mnemonic::Into | Mnemonic::Iret);
    stack && sp & 1 != 0 && (sp < 0x20 || sp > 0xffe0)
}

/// Where a test's suite is: target/sst/<cpu>/<dir> (SST_DIR=<dir> elsewhere)
pub fn suite_dir(root: &std::path::Path, s: &Suite) -> std::path::PathBuf {
    root.join(s.cpu).join(s.dir)
}

/// The hashes of the suite's revoked tests (bad recordings)
pub fn revoked(root: &std::path::Path, s: &Suite) -> std::collections::HashSet<String> {
    std::fs::read_to_string(root.join(s.cpu).join("revocation_list.txt")).unwrap_or_default().lines().filter(|l| !l.starts_with('#')).map(|l| l.trim().to_lowercase()).collect()
}

/// Why a test is not run (None: it is): revoked, the real-mode segment limits (#SS/#GP:
/// koji has none; Win16 code runs within its limits), the 386's undefined SIB, a 64 KB
/// wrap on the 16-bit chips, and what [`not_modeled`] lists
pub fn skip(kind: Kind, t: &Test, revoked: &std::collections::HashSet<String>) -> Option<&'static str> {
    if revoked.contains(&t.hash) {
        Some("revoked")
    } else if matches!(t.excp, Some((12 | 13, _))) {
        Some("#SS/#GP (segment limits)")
    } else if invalid_sib(&decode(&t.bytes), &t.bytes) {
        Some("SIB scale without index")
    } else if kind != Kind::I386 && wraps(t) {
        Some("64 KB segment wrap")
    } else {
        not_modeled(kind, &t.bytes, t.init.regs[4].unwrap_or(0))
    }
}
