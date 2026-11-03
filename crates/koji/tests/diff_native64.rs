//! Differential test of long mode (x86-64) against the host CPU: random instructions with REX
//! prefixes (64-bit operands, r8-r15, xmm8-15), register and memory forms (incl. RIP-relative
//! and the stack), run natively and in the interpreter at the same addresses.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use koji::flags::*;
use koji::{step, Cpu, Exit, Fault, Memory};

/// One fixed mapping below 4GB: harness code, state, data the operands point into.
const BASE: u64 = 0x2000_0000;
const SIZE: usize = 0x20000;
const STATE: u64 = BASE + 0x2000;
const DATA: u64 = BASE + 0x8000;
const DATA_LEN: usize = 0x8000;
const SAVE_RSP: u64 = STATE + 0x400;

/// State layout at STATE: r[16] @0, flags @128, xmm[16] @136, mm[8] @392
const R: u64 = 0;
const FL: u64 = 128;
const XMM: u64 = 136;
const MM: u64 = 392;

struct Host;
impl Memory for Host {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if (a as u64) < BASE || a as u64 >= BASE + SIZE as u64 {
            return Err(Fault { addr: a, write: false });
        }
        Ok(unsafe { *(a as usize as *const u8) })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if (a as u64) < BASE + 0x8000 || a as u64 >= BASE + SIZE as u64 {
            return Err(Fault { addr: a, write: true });
        }
        unsafe { *(a as usize as *mut u8) = v };
        Ok(())
    }
}

fn abs32(c: &mut Vec<u8>, a: u64) {
    c.extend((a as u32).to_le_bytes());
}

/// mov r64, [abs] (load) or mov [abs], r64
fn mov_abs(c: &mut Vec<u8>, r: u8, a: u64, store: bool) {
    c.extend([0x48 | (r >> 3) << 2, if store { 0x89 } else { 0x8b }, 0x04 | (r & 7) << 3, 0x25]);
    abs32(c, a);
}

fn harness(insn: &[u8]) -> (Vec<u8>, usize) {
    let mut c = vec![0x53, 0x55, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]; // push rbx rbp r12-r15
    c.extend([0xff, 0x34, 0x25]); // push qword [FL]; popfq
    abs32(&mut c, STATE + FL);
    c.push(0x9d);
    for n in 0..16u8 {
        // movdqu xmmN, [XMM + 16N]
        c.push(0xf3);
        if n >= 8 { c.push(0x44); }
        c.extend([0x0f, 0x6f, 0x04 | (n & 7) << 3, 0x25]);
        abs32(&mut c, STATE + XMM + 16 * n as u64);
    }
    for n in 0..8u8 {
        c.extend([0x0f, 0x6f, 0x04 | n << 3, 0x25]);
        abs32(&mut c, STATE + MM + 8 * n as u64);
    }
    mov_abs(&mut c, 4, SAVE_RSP, true);
    for r in 0..16u8 {
        mov_abs(&mut c, r, STATE + R + 8 * r as u64, false);
    }
    let at = c.len();
    c.extend_from_slice(insn);
    mov_abs(&mut c, 4, STATE + R + 32, true);
    mov_abs(&mut c, 4, SAVE_RSP, false);
    for r in 0..16u8 {
        if r != 4 { mov_abs(&mut c, r, STATE + R + 8 * r as u64, true); }
    }
    c.extend([0x9c, 0x8f, 0x04, 0x25]); // pushfq; pop qword [FL]
    abs32(&mut c, STATE + FL);
    for n in 0..16u8 {
        c.push(0xf3);
        if n >= 8 { c.push(0x44); }
        c.extend([0x0f, 0x7f, 0x04 | (n & 7) << 3, 0x25]);
        abs32(&mut c, STATE + XMM + 16 * n as u64);
    }
    for n in 0..8u8 {
        c.extend([0x0f, 0x7f, 0x04 | n << 3, 0x25]);
        abs32(&mut c, STATE + MM + 8 * n as u64);
    }
    c.extend([0x0f, 0x77, 0xfc]); // emms; cld
    c.extend([0x41, 0x5f, 0x41, 0x5e, 0x41, 0x5d, 0x41, 0x5c, 0x5d, 0x5b, 0xc3]);
    (c, at)
}

#[derive(Clone, Copy, Default)]
struct State {
    r: [u64; 16],
    flags: u64,
    xmm: [u128; 16],
    mm: [u64; 8],
}

unsafe fn put_state(s: &State) {
    let p = STATE as usize as *mut u8;
    for i in 0..16 { (p.add(8 * i) as *mut u64).write_unaligned(s.r[i]); }
    (p.add(FL as usize) as *mut u64).write_unaligned(s.flags);
    for i in 0..16 { (p.add(XMM as usize + 16 * i) as *mut u128).write_unaligned(s.xmm[i]); }
    for i in 0..8 { (p.add(MM as usize + 8 * i) as *mut u64).write_unaligned(s.mm[i]); }
}

unsafe fn get_state() -> State {
    let p = STATE as usize as *const u8;
    let mut s = State::default();
    for i in 0..16 { s.r[i] = (p.add(8 * i) as *const u64).read_unaligned(); }
    s.flags = (p.add(FL as usize) as *const u64).read_unaligned();
    for i in 0..16 { s.xmm[i] = (p.add(XMM as usize + 16 * i) as *const u128).read_unaligned(); }
    for i in 0..8 { s.mm[i] = (p.add(MM as usize + 8 * i) as *const u64).read_unaligned(); }
    s
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn value(&mut self) -> u64 {
        const EDGE: [u64; 14] = [0, 1, 2, 0x7f, 0x80, 0xff, 0x8000, 0xffff, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff,
            0x7fff_ffff_ffff_ffff, 0x8000_0000_0000_0000, u64::MAX];
        match self.below(5) {
            0 => EDGE[self.below(EDGE.len() as u64) as usize],
            1 => self.below(64),
            2 => self.next() as u32 as u64,
            _ => self.next(),
        }
    }
}

/// An instruction being generated: prefixes, REX bits, opcode, ModRM operands.
struct Gen {
    p66: bool,
    w: bool,
    rex: bool,
    op: Vec<u8>,
    /// (reg field, rm field) as full register numbers; rm None = memory operand
    reg: u8,
    rm: Option<u8>,
    imm: Vec<u8>,
    /// base/index registers of a memory operand that must point into DATA
    mem: Option<(u8, Option<(u8, u8)>, i32)>,
    rip: bool,
    /// a complete encoding without ModRM
    raw: Option<Vec<u8>>,
}

impl Gen {
    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();
        if self.p66 { v.push(0x66); }
        let mut rex = 0x40 | (self.w as u8) << 3 | (self.reg >> 3) << 2;
        let mut modrm_rest = Vec::new();
        let modrm;
        match (self.rm, self.mem) {
            (Some(rm), _) => {
                rex |= rm >> 3;
                modrm = 0xc0 | (self.reg & 7) << 3 | (rm & 7);
            }
            (None, _) if self.rip => {
                modrm = 0x05 | (self.reg & 7) << 3;
                modrm_rest.extend([0, 0, 0, 0]); // displacement fixed up later
            }
            (None, Some((base, index, disp))) => {
                rex |= base >> 3;
                match index {
                    Some((idx, scale)) => {
                        rex |= (idx >> 3) << 1;
                        modrm = 0x84 | (self.reg & 7) << 3;
                        modrm_rest.push(scale << 6 | (idx & 7) << 3 | (base & 7));
                    }
                    None if base & 7 == 4 => {
                        modrm = 0x84 | (self.reg & 7) << 3;
                        modrm_rest.push(0x24);
                    }
                    None => modrm = 0x80 | (self.reg & 7) << 3 | (base & 7),
                }
                modrm_rest.extend(disp.to_le_bytes());
            }
            _ => unreachable!(),
        }
        if self.rex || rex != 0x40 { v.push(rex); }
        v.extend_from_slice(&self.op);
        v.push(modrm);
        v.extend(modrm_rest);
        v.extend_from_slice(&self.imm);
        v
    }
}

/// A register that is not RSP (in a byte instruction with REX, 4..7 are SPL..DIL; 4 is SPL)
fn reg(rng: &mut Rng) -> u8 {
    loop {
        let r = rng.below(16) as u8;
        if r != 4 { return r; }
    }
}

fn gen(rng: &mut Rng) -> (Gen, bool) {
    let mut g = Gen { p66: false, w: rng.below(3) != 0, rex: rng.below(4) == 0, op: vec![], reg: reg(rng), rm: Some(reg(rng)),
        imm: vec![], mem: None, rip: false, raw: None };
    if !g.w && rng.below(6) == 0 { g.p66 = true; }
    let wide = if g.p66 && !g.w { 2 } else { 4 };
    let mut byte = false;
    let imm = |rng: &mut Rng, n: usize| rng.value().to_le_bytes()[..n].to_vec();
    let ext = |g: &mut Gen, e: u8| g.reg = e;
    match rng.below(20) {
        0 | 1 => {
            let base = [0x00u8, 0x08, 0x10, 0x18, 0x20, 0x28, 0x30, 0x38][rng.below(8) as usize];
            let op = base + rng.below(4) as u8;
            byte = op & 1 == 0;
            g.op = vec![op];
        }
        2 => {
            let op = [0x84u8, 0x85, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x63][rng.below(8) as usize];
            byte = matches!(op, 0x84 | 0x88 | 0x8a);
            g.op = vec![op];
        }
        3 => {
            let op = [0x80u8, 0x81, 0x83][rng.below(3) as usize];
            byte = op == 0x80;
            g.op = vec![op];
            ext(&mut g, rng.below(8) as u8);
            g.imm = imm(rng, if op == 0x81 { wide } else { 1 });
        }
        4 | 5 => {
            let op = [0xc0u8, 0xc1, 0xd0, 0xd1, 0xd2, 0xd3][rng.below(6) as usize];
            byte = op & 1 == 0;
            g.op = vec![op];
            ext(&mut g, [0u8, 1, 2, 3, 4, 5, 7][rng.below(7) as usize]);
            if op < 0xd0 {
                g.imm = vec![if rng.below(2) == 0 { rng.below(8) } else { rng.below(256) } as u8];
            }
        }
        6 => {
            let op = [0xf6u8, 0xf7][rng.below(2) as usize];
            byte = op == 0xf6;
            g.op = vec![op];
            let e = [0u8, 2, 3, 4, 5, 6, 7][rng.below(7) as usize];
            ext(&mut g, e);
            if e == 0 { g.imm = imm(rng, if byte { 1 } else { wide }); }
        }
        7 => {
            let op = [0xfeu8, 0xff][rng.below(2) as usize];
            byte = op == 0xfe;
            g.op = vec![op];
            ext(&mut g, rng.below(2) as u8);
        }
        8 => match rng.below(3) {
            0 => g.op = vec![0x0f, 0xaf],
            1 => { g.op = vec![0x69]; g.imm = imm(rng, wide); }
            _ => { g.op = vec![0x6b]; g.imm = imm(rng, 1); }
        },
        9 => {
            if rng.below(2) == 0 || g.rm.is_none() {
                g.op = vec![0x0f, [0xa3u8, 0xab, 0xb3, 0xbb][rng.below(4) as usize]];
            } else {
                g.op = vec![0x0f, 0xba];
                ext(&mut g, 4 + rng.below(4) as u8);
                g.imm = imm(rng, 1);
            }
        }
        10 => g.op = vec![0x0f, [0xbcu8, 0xbd][rng.below(2) as usize]],
        11 => {
            let op = [0xa4u8, 0xa5, 0xac, 0xad][rng.below(4) as usize];
            g.op = vec![0x0f, op];
            if op & 1 == 0 { g.imm = vec![rng.below(64) as u8]; }
        }
        12 => {
            let op = [0xb0u8, 0xb1, 0xc0, 0xc1][rng.below(4) as usize];
            byte = op & 1 == 0;
            g.op = vec![0x0f, op];
        }
        13 => g.op = vec![0x0f, 0x40 + rng.below(16) as u8],
        14 => {
            g.op = vec![0x0f, 0x90 + rng.below(16) as u8];
            byte = true;
            ext(&mut g, 0);
        }
        15 => {
            let op = [0xb6u8, 0xb7, 0xbe, 0xbf][rng.below(4) as usize];
            g.op = vec![0x0f, op];
            byte = false;
            // the source of b6/be is a byte register
            if op & 1 == 0 && g.rm.is_some_and(|r| r == 4) { g.rm = Some(0); }
        }
        16 => {
            // no ModRM: cbw/cwde/cdqe, cwd/cdq/cqo, sahf/lahf, cmc/clc/stc; bswap; mov r64, imm64
            let mut v = Vec::new();
            if g.p66 { v.push(0x66); }
            match rng.below(3) {
                0 => {
                    if g.w { v.push(0x48); }
                    v.push([0x98u8, 0x99, 0x9e, 0x9f, 0xf5, 0xf8, 0xf9][rng.below(7) as usize]);
                }
                1 => {
                    let r = reg(rng);
                    v.retain(|&b| b != 0x66);
                    v.extend([0x40 | (g.w as u8) << 3 | r >> 3, 0x0f, 0xc8 + (r & 7)]);
                }
                _ => {
                    let r = reg(rng);
                    v.retain(|&b| b != 0x66);
                    v.extend([0x48 | r >> 3, 0xb8 + (r & 7)]);
                    v.extend(rng.value().to_le_bytes());
                }
            }
            g.raw = Some(v);
            return (g, false);
        }
        17 => {
            // LEA
            g.op = vec![0x8d];
            g.p66 = false;
            g.rm = None;
        }
        _ => {
            let op = [0x10u8, 0x11, 0x18, 0x19][rng.below(4) as usize];
            byte = op & 1 == 0;
            g.op = vec![op];
        }
    }
    // memory forms for about a third (not for LEA's register form, which does not exist)
    if g.rm.is_none() || rng.below(3) == 0 {
        g.rm = None;
        if rng.below(4) == 0 {
            g.rip = true;
        } else {
            let base = reg(rng);
            let index = if rng.below(2) == 0 {
                let i = loop { let i = reg(rng); if i != base { break i; } };
                Some((i, rng.below(4) as u8))
            } else { None };
            g.mem = Some((base, index, rng.below(64) as i32 - 32));
        }
    }
    (g, byte)
}

/// Flags the SDM leaves undefined for this instruction.
fn undefined(m: Mnemonic, ins: &iced_x86::Instruction, cpu: &Cpu, bits: u32) -> (u32, bool) {
    use Mnemonic::*;
    let cmask = if bits == 64 { 0x3f } else { 0x1f };
    let count = || -> u32 {
        let n = ins.op_count();
        (if n >= 2 && ins.op_kind(n - 1) == iced_x86::OpKind::Immediate8 {
            ins.immediate8() as u32
        } else if n >= 2 && ins.op_register(n - 1) == iced_x86::Register::CL {
            cpu.regs[1] & 0xff
        } else {
            1
        }) & cmask
    };
    match m {
        And | Or | Xor | Test => (AF, false),
        Shl | Sal | Shr | Sar => {
            let c = count();
            let mut u = AF;
            if c != 1 { u |= OF; }
            if c > bits { u |= CF; }
            (u, false)
        }
        Rol | Ror | Rcl | Rcr => (if count() != 1 { OF } else { 0 }, false),
        Shld | Shrd => {
            let c = count();
            if c > bits { return (STATUS, true); }
            (AF | if c != 1 { OF } else { 0 }, false)
        }
        Mul | Imul => (SF | ZF | AF | PF, false),
        Div | Idiv => (STATUS, false),
        Bt | Bts | Btr | Btc => (OF | SF | AF | PF, false),
        Bsf | Bsr => (CF | OF | SF | AF | PF, false),
        _ => (0, false),
    }
}

static LOCK: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

fn map() -> std::sync::MutexGuard<'static, bool> {
    let mut g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if *g { return g; }
    *g = true;
    let p = unsafe {
        libc::mmap(BASE as usize as *mut _, SIZE, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                   libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE, -1, 0)
    };
    assert_eq!(p as u64, BASE, "mmap at {:#x}", BASE);
    g
}

/// Runs `insn` natively and in the interpreter from the same state and memory; Err with a
/// description on a mismatch, Ok(false) if skipped.
fn compare(insn: &[u8], st: &State, data: &[u8], fl_lanes: u32, bits: u32) -> Result<bool, String> {
    let (code, at) = harness(insn);
    let code_ptr = BASE as usize as *mut u8;
    let data_ptr = DATA as usize as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), code_ptr, code.len());
        core::ptr::copy_nonoverlapping(data.as_ptr(), data_ptr, data.len());
    }
    let ins = Decoder::with_ip(64, insn, BASE + at as u64, DecoderOptions::NONE).decode();
    // interpreter first
    let mut cpu = Cpu::default();
    cpu.mode64 = 1;
    for i in 0..16 { cpu.set_gpr(i, st.r[i]); }
    cpu.eflags = st.flags as u32;
    for i in 0..16 { cpu.set_xmm(i, st.xmm[i]); }
    cpu.mm = st.mm;
    cpu.eip = (BASE + at as u64) as u32;
    let exit = step(&mut cpu, &mut Host);
    if exit == Exit::DivideError {
        return Ok(false);
    }
    if exit != Exit::Continue {
        return Err(format!("{:02x?} {:?}: exit {:?}", insn, ins.mnemonic(), exit));
    }
    let got_mem = unsafe { core::slice::from_raw_parts(data_ptr, data.len()).to_vec() };
    // then the host
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), data_ptr, data.len());
        put_state(st);
        let f: extern "C" fn() = core::mem::transmute(code_ptr);
        f();
    }
    let want = unsafe { get_state() };
    let want_mem = unsafe { core::slice::from_raw_parts(data_ptr, data.len()).to_vec() };
    let mut before = Cpu::default();
    for i in 0..16 { before.set_gpr(i, st.r[i]); }
    let (undef, skip) = undefined(ins.mnemonic(), &ins, &before, bits);
    if skip { return Ok(false); }
    let mut diff = Vec::new();
    let (wf, gf) = (want.flags as u32 & STATUS & !undef, cpu.flags() & STATUS & !undef);
    if wf != gf { diff.push(format!("flags want {:03x} got {:03x}", wf, gf)); }
    for i in 0..16 {
        if want.r[i] != cpu.gpr(i) { diff.push(format!("r{} in {:x} want {:x} got {:x}", i, st.r[i], want.r[i], cpu.gpr(i))); }
        let (w, g) = (want.xmm[i], cpu.xmm(i));
        let same = w == g || (fl_lanes != 0 && (0..128 / fl_lanes).all(|k| {
            let m: u128 = if fl_lanes == 64 { u64::MAX as u128 } else { (1 << fl_lanes) - 1 };
            let (x, y) = ((w >> (k * fl_lanes)) & m, (g >> (k * fl_lanes)) & m);
            x == y || if fl_lanes == 32 { f32::from_bits(x as u32).is_nan() && f32::from_bits(y as u32).is_nan() }
                      else { f64::from_bits(x as u64).is_nan() && f64::from_bits(y as u64).is_nan() }
        }));
        if !same { diff.push(format!("xmm{} in {:032x} want {:032x} got {:032x}", i, st.xmm[i], w, g)); }
    }
    for i in 0..8 {
        if want.mm[i] != cpu.mm[i] { diff.push(format!("mm{} want {:x} got {:x}", i, want.mm[i], cpu.mm[i])); }
    }
    if want_mem != got_mem {
        let k = (0..data.len()).find(|&k| want_mem[k] != got_mem[k]).unwrap();
        diff.push(format!("memory at {:#x}: want {:02x?} got {:02x?}", DATA + k as u64,
            &want_mem[k..(k + 8).min(data.len())], &got_mem[k..(k + 8).min(data.len())]));
    }
    if diff.is_empty() { Ok(true) } else { Err(format!("{:02x?} {:?}\n  {}", insn, ins.mnemonic(), diff.join("\n  "))) }
}

fn random_state(rng: &mut Rng) -> State {
    let mut st = State::default();
    for i in 0..16 { st.r[i] = rng.value(); }
    st.r[4] = DATA + 0x4000;
    st.flags = 0x202 | (rng.next() & STATUS as u64);
    for x in st.xmm.iter_mut() { *x = (rng.next() as u128) << 64 | rng.next() as u128; }
    for x in st.mm.iter_mut() { *x = rng.next(); }
    st
}

/// Point the memory operand into DATA: base/index registers get values that land there, and a
/// RIP-relative displacement is chosen likewise.
fn place(g: &Gen, insn: &mut Vec<u8>, st: &mut State, rng: &mut Rng) {
    let at = BASE + harness(insn).1 as u64;
    let target = DATA + 0x100 + rng.below(0x3000) & !7;
    if g.rip {
        // the displacement sits right after the ModRM byte: find it from the encoding
        let modrm_at = insn.len() - g.imm.len() - 5;
        let disp = target as i64 - (at as i64 + insn.len() as i64);
        insn[modrm_at + 1..modrm_at + 5].copy_from_slice(&(disp as i32).to_le_bytes());
    } else if let Some((base, index, disp)) = g.mem {
        let mut b = target.wrapping_sub(disp as i64 as u64);
        if let Some((i, s)) = index {
            let iv = rng.below(16);
            st.r[i as usize] = iv;
            b = b.wrapping_sub(iv << s);
        }
        st.r[base as usize] = b;
    }
}

#[test]
fn random_long_mode_integer_ops_match_the_host_cpu() {
    let _m = map();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut done = 0;
    let mut failures = Vec::new();
    for _ in 0..150_000 {
        let (g, byte) = gen(&mut rng);
        let mut st = random_state(&mut rng);
        let insn = match &g.raw {
            Some(v) => v.clone(),
            None => {
                let mut insn = g.encode();
                if g.rm.is_none() {
                    // a bit string offset in a register addresses outside the operand: keep it small
                    if g.op.len() == 2 && matches!(g.op[1], 0xa3 | 0xab | 0xb3 | 0xbb) {
                        if g.mem.is_some_and(|(b, i, _)| b == g.reg || i.is_some_and(|i| i.0 == g.reg)) { continue; }
                        st.r[g.reg as usize] = rng.below(512).wrapping_sub(256);
                    }
                    place(&g, &mut insn, &mut st, &mut rng);
                }
                insn
            }
        };
        let ins = Decoder::new(64, &insn, DecoderOptions::NONE).decode();
        if ins.is_invalid() { continue; }
        let mut data = vec![0u8; DATA_LEN];
        for b in data.iter_mut() { *b = rng.next() as u8; }
        let bits = if byte { 8 } else if g.w { 64 } else if g.p66 { 16 } else { 32 };
        match compare(&insn, &st, &data, 0, bits) {
            Ok(true) => done += 1,
            Ok(false) => {}
            Err(e) => if failures.len() < 20 { failures.push(e) },
        }
    }
    assert!(failures.is_empty(), "{} checked, mismatches:\n{}", done, failures.join("\n"));
    assert!(done > 100_000, "{}", done);
}

#[test]
fn long_mode_stack_and_strings_match_the_host_cpu() {
    let _m = map();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut failures = Vec::new();
    // push/pop r64 (incl. r8-r15), push imm, push/pop [mem], pushfq, enter/leave,
    // movs/stos/lods/cmps/scas in all widths with and without REP
    let mut cases: Vec<Vec<u8>> = Vec::new();
    for r in 0..16u8 {
        if r == 4 { continue; }
        cases.push(if r >= 8 { vec![0x41, 0x50 + (r & 7)] } else { vec![0x50 + r] });
        cases.push(if r >= 8 { vec![0x41, 0x58 + (r & 7)] } else { vec![0x58 + r] });
    }
    cases.extend([vec![0x6a, 0x85], vec![0x68, 0x01, 0x80, 0, 0x80], vec![0x9c], vec![0x66, 0x50], vec![0xc9],
        vec![0xc8, 0x10, 0, 0], vec![0xc8, 0x20, 0, 3], vec![0xff, 0x34, 0x24], vec![0x8f, 0x44, 0x24, 0x08]]);
    for p in [vec![], vec![0xf3], vec![0xf2]] {
        for op in [0xa4u8, 0xa5, 0xaa, 0xab, 0xac, 0xad, 0xa6, 0xa7, 0xae, 0xaf] {
            for w in [None, Some(0x48u8), Some(0x66)] {
                if op & 1 == 0 && w.is_some() { continue; }
                let mut v = p.clone();
                if let Some(w) = w {
                    if w == 0x66 { v.insert(0, 0x66) } else { v.push(w) }
                }
                v.push(op);
                cases.push(v);
            }
        }
    }
    let mut done = 0;
    for _ in 0..30 {
        for insn in &cases {
            let mut st = random_state(&mut rng);
            st.r[4] = DATA + 0x4000 + (rng.below(64) << 3);
            st.r[5] = DATA + 0x5000 + (rng.below(64) << 3);
            st.r[6] = DATA + 0x1000 + rng.below(0x800);
            st.r[7] = DATA + 0x2000 + rng.below(0x800);
            st.r[1] = rng.below(40);
            if rng.below(3) == 0 { st.flags |= DF as u64; } else { st.flags &= !(DF as u64); }
            let mut data = vec![0u8; DATA_LEN];
            for b in data.iter_mut() { *b = if rng.below(4) == 0 { rng.next() as u8 } else { 0x41 }; }
            st.r[0] = if rng.below(2) == 0 { 0x4141_4141_4141_4141 } else { rng.next() };
            match compare(insn, &st, &data, 0, 64) {
                Ok(true) => done += 1,
                Ok(false) => {}
                Err(e) => if failures.len() < 20 { failures.push(e) },
            }
        }
    }
    assert!(failures.is_empty(), "{} checked, mismatches:\n{}", done, failures.join("\n"));
}

/// (prefix, opcode after 0F, imm8, float lane bits (0 = integer), operand classes, REX.W allowed)
struct SseOp(u8, u8, bool, u32, u8, u8, bool);

const SSE_OPS: &[SseOp] = &[
    SseOp(0x66, 0x60, false, 0, b'x', b'x', false), SseOp(0x66, 0x62, false, 0, b'x', b'x', false),
    SseOp(0x66, 0x6c, false, 0, b'x', b'x', false), SseOp(0x66, 0x6d, false, 0, b'x', b'x', false),
    SseOp(0x66, 0x6f, false, 0, b'x', b'x', false), SseOp(0x66, 0x70, true, 0, b'x', b'x', false),
    SseOp(0x66, 0x74, false, 0, b'x', b'x', false), SseOp(0x66, 0xd4, false, 0, b'x', b'x', false),
    SseOp(0x66, 0xd7, false, 0, b'g', b'x', true), SseOp(0x66, 0xdb, false, 0, b'x', b'x', false),
    SseOp(0x66, 0xef, false, 0, b'x', b'x', false), SseOp(0x66, 0xf4, false, 0, b'x', b'x', false),
    SseOp(0x66, 0xfe, false, 0, b'x', b'x', false), SseOp(0x66, 0xfb, false, 0, b'x', b'x', false),
    SseOp(0x66, 0x6e, false, 0, b'x', b'g', true), SseOp(0x66, 0x7e, false, 0, b'x', b'g', true),
    SseOp(0x00, 0x6e, false, 0, b'm', b'g', true), SseOp(0x00, 0x7e, false, 0, b'm', b'g', true),
    SseOp(0xf3, 0x7e, false, 0, b'x', b'x', false), SseOp(0x66, 0xd6, false, 0, b'x', b'x', false),
    SseOp(0x66, 0xc4, true, 0, b'x', b'g', false), SseOp(0x66, 0xc5, true, 0, b'g', b'x', true),
    SseOp(0x00, 0x28, false, 32, b'x', b'x', false), SseOp(0x00, 0x2e, false, 32, b'x', b'x', false),
    SseOp(0x00, 0x50, false, 32, b'g', b'x', true), SseOp(0x00, 0x58, false, 32, b'x', b'x', false),
    SseOp(0x00, 0x59, false, 32, b'x', b'x', false), SseOp(0x00, 0xc6, true, 32, b'x', b'x', false),
    SseOp(0xf3, 0x10, false, 32, b'x', b'x', false), SseOp(0xf3, 0x5a, false, 64, b'x', b'x', false),
    SseOp(0xf3, 0x2a, false, 32, b'x', b'g', true), SseOp(0xf3, 0x2c, false, 0, b'g', b'x', true),
    SseOp(0xf3, 0x2d, false, 0, b'g', b'x', true),
    SseOp(0xf2, 0x10, false, 64, b'x', b'x', false), SseOp(0xf2, 0x58, false, 64, b'x', b'x', false),
    SseOp(0xf2, 0x5e, false, 64, b'x', b'x', false), SseOp(0xf2, 0x51, false, 64, b'x', b'x', false),
    SseOp(0xf2, 0x2a, false, 64, b'x', b'g', true), SseOp(0xf2, 0x2c, false, 0, b'g', b'x', true),
    SseOp(0xf2, 0x2d, false, 0, b'g', b'x', true), SseOp(0x66, 0x2f, false, 64, b'x', b'x', false),
    SseOp(0x66, 0x57, false, 64, b'x', b'x', false), SseOp(0x66, 0x14, false, 64, b'x', b'x', false),
    SseOp(0x66, 0x5a, false, 32, b'x', b'x', false), SseOp(0x66, 0xe6, false, 0, b'x', b'x', false),
];

#[test]
fn random_long_mode_sse_ops_match_the_host_cpu() {
    let _m = map();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut failures = Vec::new();
    let mut done = 0;
    for _ in 0..60_000 {
        let op = &SSE_OPS[rng.below(SSE_OPS.len() as u64) as usize];
        let pick = |rng: &mut Rng, c: u8| match c { b'g' => reg(rng), b'm' => rng.below(8) as u8, _ => rng.below(16) as u8 };
        let (r, m) = (pick(&mut rng, op.4), pick(&mut rng, op.5));
        let w = op.6 && rng.below(2) == 0;
        let mut insn = Vec::new();
        if op.0 != 0 { insn.push(op.0); }
        let rex = 0x40 | (w as u8) << 3 | (r >> 3) << 2 | m >> 3;
        if rex != 0x40 || rng.below(4) == 0 { insn.push(rex); }
        insn.extend([0x0f, op.1, 0xc0 | (r & 7) << 3 | (m & 7)]);
        if op.2 {
            let imm = rng.next() as u8;
            insn.push(imm);
        }
        let ins = Decoder::new(64, &insn, DecoderOptions::NONE).decode();
        if ins.is_invalid() { continue; }
        let mut st = random_state(&mut rng);
        for x in st.xmm.iter_mut() {
            if rng.below(2) == 0 {
                let f: f64 = [0.0, -0.0, 1.0, -1.5, 0.5, 1e300, 2.5e9, -7.5e9, 9.3e18, -9.3e18, f64::NAN][rng.below(11) as usize];
                *x = (*x & !(u64::MAX as u128)) | f.to_bits() as u128;
            }
        }
        let data = vec![0u8; DATA_LEN];
        match compare(&insn, &st, &data, op.3, 32) {
            Ok(true) => done += 1,
            Ok(false) => {}
            Err(e) => if failures.len() < 20 { failures.push(e) },
        }
    }
    assert!(failures.is_empty(), "{} checked, mismatches:\n{}", done, failures.join("\n"));
}
