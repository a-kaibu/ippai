//! Differential test of protected-mode (32-bit) code against the host CPU, run in its real
//! 32-bit mode: Linux's compatibility code segment (selector 0x23) entered with a far call.
//! diff_native.rs runs register-form instructions in 64-bit mode, where the encodings it uses
//! mean the same; this covers what only 32-bit mode has or that needs memory: the one-byte
//! INC/DEC (0x40-0x4F), PUSHA/POPA, BCD (DAA/DAS/AAA/AAS/AAM/AAD), 32-bit stack operations
//! (push/pop esp, enter/leave, push/pop [mem]), memory operands in every 32-bit addressing form
//! (incl. absolute disp32 and [ebp+..]), bit strings in memory, CMPXCHG8B, XADD/CMPXCHG to memory
//! and the string instructions with REP. Each instruction runs from the same registers and
//! memory (one fixed mapping below 4GB at the same address on both sides) in the interpreter
//! and natively; registers, status flags (less the undefined ones) and DF and the data
//! region must agree.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};
use koji::flags::*;
use koji::{step, Cpu, Exit, Fault, Memory};

const BASE: u32 = 0x3000_0000;
const SIZE: usize = 0x10000;
/// the 32-bit harness starts here (the 64-bit one at BASE)
const CODE32: u32 = BASE + 0x100;
/// m16:32 far pointer to CODE32
const FAR: u32 = BASE + 0x1f00;
/// regs[8] @0, flags @32, 32-bit esp of the harness @36, 64-bit rsp @40
const STATE: u32 = BASE + 0x2000;
/// the harness's own 32-bit stack (above the far-call frame)
const STACK32: u32 = BASE + 0x7f00;
const DATA: u32 = BASE + 0x8000;
const DATA_LEN: usize = 0x8000;
const FL: u32 = STATE + 32;
const SAVE32: u32 = STATE + 36;
const SAVE64: u32 = STATE + 40;
const USER32_CS: u16 = 0x23;
const USER_DS: u8 = 0x2b;

struct Host;
impl Memory for Host {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if a < BASE || a as usize >= BASE as usize + SIZE {
            return Err(Fault { addr: a, write: false });
        }
        Ok(unsafe { *(a as usize as *const u8) })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if a < DATA || a as usize >= DATA as usize + DATA_LEN {
            return Err(Fault { addr: a, write: true });
        }
        unsafe { *(a as usize as *mut u8) = v };
        Ok(())
    }
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
    fn value(&mut self) -> u32 {
        const EDGE: [u32; 14] = [0, 1, 2, 9, 0x0f, 0x7f, 0x80, 0x99, 0xff, 0x7fff, 0x8000, 0xffff, 0x7fff_ffff, 0x8000_0000];
        match self.below(5) {
            0 => EDGE[self.below(EDGE.len() as u64) as usize],
            1 => 0xffff_ffff - self.below(3) as u32,
            2 => self.below(64) as u32,
            _ => self.next() as u32,
        }
    }
}

/// Code at BASE (64-bit, called from Rust) and CODE32 (32-bit, entered by the far call).
fn harness(insn: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let abs = |c: &mut Vec<u8>, a: u32| c.extend(a.to_le_bytes());
    let mut h = vec![0x53, 0x55, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]; // push rbx rbp r12-r15
    h.extend([0x48, 0x89, 0x24, 0x25]); // mov [SAVE64], rsp
    abs(&mut h, SAVE64);
    h.extend([0x48, 0xc7, 0xc4]); // mov rsp, STACK32
    abs(&mut h, STACK32);
    h.extend([0xff, 0x1c, 0x25]); // call far [FAR]
    abs(&mut h, FAR);
    h.extend([0x48, 0x8b, 0x24, 0x25]); // mov rsp, [SAVE64]
    abs(&mut h, SAVE64);
    h.extend([0x41, 0x5f, 0x41, 0x5e, 0x41, 0x5d, 0x41, 0x5c, 0x5d, 0x5b, 0xc3]);

    // 32-bit: data segments first (Linux leaves DS/ES null in 64-bit processes)
    let mut c = vec![0xb8, USER_DS, 0, 0, 0, 0x8e, 0xd8, 0x8e, 0xc0]; // mov eax, 0x2b; mov ds, eax; mov es, eax
    c.push(0x89); c.push(0x25); abs(&mut c, SAVE32); // mov [SAVE32], esp
    c.extend([0xff, 0x35]); abs(&mut c, FL); // push dword [FL]
    c.push(0x9d); // popfd
    for r in [0u8, 1, 2, 3, 5, 6, 7, 4] {
        c.extend([0x8b, 0x05 | r << 3]); // mov r32, [STATE + 4r]
        abs(&mut c, STATE + 4 * r as u32);
    }
    c.extend_from_slice(insn);
    for r in 0..8u8 {
        c.extend([0x89, 0x05 | r << 3]); // mov [STATE + 4r], r32
        abs(&mut c, STATE + 4 * r as u32);
    }
    c.extend([0x8b, 0x25]); abs(&mut c, SAVE32); // mov esp, [SAVE32]
    c.push(0x9c); // pushfd
    c.extend([0x8f, 0x05]); abs(&mut c, FL); // pop dword [FL]
    c.extend([0xfc, 0xcb]); // cld; retf
    (h, c)
}

/// Where the instruction sits in the 32-bit harness.
fn insn_at() -> u32 {
    CODE32 + 9 + 6 + 6 + 1 + 8 * 6
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
    assert_eq!(p as usize, BASE as usize, "mmap at {:#x}", BASE);
    let mut far = CODE32.to_le_bytes().to_vec();
    far.extend(USER32_CS.to_le_bytes());
    unsafe { core::ptr::copy_nonoverlapping(far.as_ptr(), FAR as usize as *mut u8, far.len()) };
    g
}

#[derive(Clone, Copy)]
struct State {
    r: [u32; 8],
    flags: u32,
}

unsafe fn put_state(st: &State) {
    let p = STATE as usize as *mut u32;
    for i in 0..8 { *p.add(i) = st.r[i]; }
    *p.add(8) = st.flags;
}

unsafe fn get_state() -> State {
    let p = STATE as usize as *const u32;
    let mut st = State { r: [0; 8], flags: *p.add(8) };
    for i in 0..8 { st.r[i] = *p.add(i); }
    st
}

/// Flags the SDM leaves undefined for this instruction (and whether to skip the comparison).
fn undefined(ins: &Instruction, cpu: &Cpu) -> (u32, bool) {
    use Mnemonic::*;
    let bits = match ins.op0_kind() {
        OpKind::Register => ins.op0_register().size() as u32 * 8,
        OpKind::Memory => ins.memory_size().size() as u32 * 8,
        _ => 32,
    };
    let count = || -> u32 {
        let n = ins.op_count();
        (if n >= 2 && ins.op_kind(n - 1) == OpKind::Immediate8 {
            ins.immediate8() as u32
        } else if n >= 2 && ins.op_register(n - 1) == Register::CL {
            cpu.regs[1] & 0xff
        } else {
            1
        }) & 0x1f
    };
    match ins.mnemonic() {
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
        Daa | Das => (OF, false),
        Aaa | Aas => (OF | SF | ZF | PF, false),
        Aam | Aad => (OF | AF | CF, false),
        _ => (0, false),
    }
}

/// Runs `insn` natively and in the interpreter from the same state and memory; Err with a
/// description on a mismatch, Ok(false) if skipped.
fn compare(insn: &[u8], st: &State, data: &[u8]) -> Result<bool, String> {
    let (h, c) = harness(insn);
    unsafe {
        core::ptr::copy_nonoverlapping(h.as_ptr(), BASE as usize as *mut u8, h.len());
        core::ptr::copy_nonoverlapping(c.as_ptr(), CODE32 as usize as *mut u8, c.len());
        core::ptr::copy_nonoverlapping(data.as_ptr(), DATA as usize as *mut u8, data.len());
    }
    let at = insn_at();
    let ins = Decoder::with_ip(32, insn, at as u64, DecoderOptions::NONE).decode();
    assert_eq!(&c[(at - CODE32) as usize..][..insn.len()], insn);
    let mut cpu = Cpu::default();
    cpu.regs = st.r;
    cpu.eflags = st.flags;
    cpu.eip = at;
    let exit = step(&mut cpu, &mut Host);
    if exit == Exit::DivideError {
        return Ok(false); // the host would raise SIGFPE
    }
    if exit != Exit::Continue {
        return Err(format!("{:02x?} {:?}: exit {:?}", insn, ins.mnemonic(), exit));
    }
    if cpu.eip != at + insn.len() as u32 {
        return Err(format!("{:02x?} {:?}: eip {:#x}", insn, ins.mnemonic(), cpu.eip));
    }
    let data_ptr = DATA as usize as *mut u8;
    let got_mem = unsafe { core::slice::from_raw_parts(data_ptr, data.len()).to_vec() };
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), data_ptr, data.len());
        put_state(st);
        let f: extern "C" fn() = core::mem::transmute(BASE as usize);
        f();
    }
    let want = unsafe { get_state() };
    let want_mem = unsafe { core::slice::from_raw_parts(data_ptr, data.len()).to_vec() };
    let mut before = Cpu::default();
    before.regs = st.r;
    let (undef, skip) = undefined(&ins, &before);
    if skip { return Ok(false); }
    let mut diff = Vec::new();
    let mask = (STATUS | DF) & !undef;
    let (wf, gf) = (want.flags & mask, cpu.flags() & mask);
    if wf != gf { diff.push(format!("flags in {:03x} want {:03x} got {:03x}", st.flags & mask, wf, gf)); }
    for i in 0..8 {
        if want.r[i] != cpu.regs[i] {
            diff.push(format!("r{} in {:08x} want {:08x} got {:08x}", i, st.r[i], want.r[i], cpu.regs[i]));
        }
    }
    if want_mem != got_mem {
        let k = (0..data.len()).find(|&k| want_mem[k] != got_mem[k]).unwrap();
        diff.push(format!("memory at {:#x}: want {:02x?} got {:02x?}", DATA as usize + k,
            &want_mem[k..(k + 8).min(data.len())], &got_mem[k..(k + 8).min(data.len())]));
    }
    if diff.is_empty() { Ok(true) } else {
        Err(format!("{:02x?} {:?} (in {:08x?})\n  {}", insn, ins.mnemonic(), st.r, diff.join("\n  ")))
    }
}

/// A register other than ESP.
fn reg(rng: &mut Rng) -> u8 {
    loop {
        let r = rng.below(8) as u8;
        if r != 4 { return r; }
    }
}

/// How the ModRM operand is formed.
enum Rm {
    /// mod 3
    Reg,
    /// any memory form, pointed into DATA
    Mem,
}

/// Appends a ModRM (with SIB and displacement) for `reg_field`, setting the registers it uses so
/// that a memory operand lands at `target`.
fn modrm(rng: &mut Rng, v: &mut Vec<u8>, st: &mut State, reg_field: u8, rm: Rm, target: u32) -> Option<u8> {
    match rm {
        Rm::Reg => {
            let r = rng.below(8) as u8;
            v.push(0xc0 | reg_field << 3 | r);
            None
        }
        Rm::Mem => match rng.below(6) {
            0 => {
                // [disp32]
                v.push(reg_field << 3 | 5);
                v.extend(target.to_le_bytes());
                None
            }
            1 | 2 => {
                // [base + disp8] / [base + disp32] (EBP included: an SS access, flat)
                let b = reg(rng);
                let d8 = rng.below(2) == 0;
                let disp = if d8 { rng.below(256) as i8 as i32 } else { rng.next() as i32 >> 8 };
                v.push(if d8 { 0x40 } else { 0x80 } | reg_field << 3 | b);
                if d8 { v.push(disp as u8) } else { v.extend(disp.to_le_bytes()) }
                st.r[b as usize] = target.wrapping_sub(disp as u32);
                Some(b)
            }
            3 => {
                // [base] (not EBP, whose mod 0 form is disp32)
                let b = loop { let b = reg(rng); if b != 5 { break b; } };
                v.push(reg_field << 3 | b);
                st.r[b as usize] = target;
                Some(b)
            }
            4 => {
                // [esp + disp8] through a SIB
                let disp = rng.below(64) as u32;
                v.extend([0x44 | reg_field << 3, 0x24, disp as u8]);
                st.r[4] = target.wrapping_sub(disp);
                Some(4)
            }
            _ => {
                // [base + index*scale + disp8] or [index*scale + disp32]
                let i = reg(rng);
                let s = rng.below(4) as u8;
                let iv = rng.below(32) as u32;
                st.r[i as usize] = iv;
                if rng.below(3) == 0 {
                    v.extend([0x04 | reg_field << 3, s << 6 | i << 3 | 5]);
                    v.extend(target.wrapping_sub(iv << s).to_le_bytes());
                } else {
                    let b = loop { let b = reg(rng); if b != i { break b; } };
                    let disp = rng.below(256) as i8 as i32;
                    v.extend([0x44 | reg_field << 3, s << 6 | i << 3 | b, disp as u8]);
                    st.r[b as usize] = target.wrapping_sub(iv << s).wrapping_sub(disp as u32);
                }
                Some(i)
            }
        },
    }
}

fn random_state(rng: &mut Rng) -> State {
    let mut st = State { r: [0; 8], flags: 0x202 | (rng.next() as u32 & STATUS) };
    for r in st.r.iter_mut() { *r = rng.value(); }
    st.r[4] = DATA + 0x6000 + (rng.below(64) << 2) as u32;
    st
}

/// The registers the memory operand of these (complete) instruction bytes uses.
fn addr_regs(v: &[u8]) -> Vec<u8> {
    let ins = Decoder::new(32, v, DecoderOptions::NONE).decode();
    [ins.memory_base(), ins.memory_index()].iter().filter(|r| **r != Register::None)
        .map(|r| r.number() as u8).collect()
}

/// One random instruction (bytes) and the state it runs from; None when the draw is not usable.
fn gen(rng: &mut Rng, st: &mut State, data: &[u8]) -> Option<Vec<u8>> {
    let load = |a: u32, n: usize| -> u64 {
        let o = (a - DATA) as usize;
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(&data[o..o + n]);
        u64::from_le_bytes(b)
    };
    let mut v = Vec::new();
    let op16 = rng.below(6) == 0;
    if op16 { v.push(0x66); }
    let wide = if op16 { 2 } else { 4 };
    let target = DATA + 0x100 + rng.below(0x5000) as u32;
    let rm = |rng: &mut Rng| if rng.below(3) == 0 { Rm::Reg } else { Rm::Mem };
    let imm = |rng: &mut Rng, v: &mut Vec<u8>, n: usize| v.extend_from_slice(&rng.value().to_le_bytes()[..n]);
    match rng.below(30) {
        0..=3 => {
            // ALU r/m, r and r, r/m
            let op = (rng.below(8) as u8) << 3 | rng.below(4) as u8;
            v.push(op);
            let r = rng.below(8) as u8;
            let k = rm(rng);
            modrm(rng, &mut v, st, r, k, target);
        }
        4 => {
            let op = [0x84u8, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b][rng.below(8) as usize];
            v.push(op);
            let r = rng.below(8) as u8;
            let k = rm(rng);
            modrm(rng, &mut v, st, r, k, target);
        }
        5 => {
            let op = [0x80u8, 0x81, 0x83, 0xc6, 0xc7][rng.below(5) as usize];
            v.push(op);
            let e = if op >= 0xc6 { 0 } else { rng.below(8) as u8 };
            let k = rm(rng);
            modrm(rng, &mut v, st, e, k, target);
            imm(rng, &mut v, if op == 0x81 || op == 0xc7 { wide } else { 1 });
        }
        6 | 7 => {
            let op = [0xc0u8, 0xc1, 0xd0, 0xd1, 0xd2, 0xd3][rng.below(6) as usize];
            let e = [0u8, 1, 2, 3, 4, 5, 7][rng.below(7) as usize];
            v.push(op);
            let k = rm(rng);
            modrm(rng, &mut v, st, e, k, target);
            if op < 0xd0 { v.push(if rng.below(2) == 0 { rng.below(8) } else { rng.below(256) } as u8); }
        }
        8 => {
            let op = [0xf6u8, 0xf7][rng.below(2) as usize];
            let e = [0u8, 2, 3, 4, 5, 6, 7][rng.below(7) as usize];
            v.push(op);
            let k = rm(rng);
            modrm(rng, &mut v, st, e, k, target);
            if e == 0 { imm(rng, &mut v, if op == 0xf6 { 1 } else { wide }); }
        }
        9 => {
            // inc/dec r/m, push [mem], pop [mem]
            match rng.below(4) {
                0 | 1 => {
                    let op = [0xfeu8, 0xff][rng.below(2) as usize];
                    v.push(op);
                    let e = rng.below(2) as u8;
                    let k = rm(rng);
                    modrm(rng, &mut v, st, e, k, target);
                }
                2 => { v.push(0xff); modrm(rng, &mut v, st, 6, Rm::Mem, target); }
                _ => {
                    v.push(0x8f);
                    // pop [esp+..] computes the address after the pop: keep it simple
                    if modrm(rng, &mut v, st, 0, Rm::Mem, target) == Some(4) { return None; }
                }
            }
        }
        10 => {
            // one-byte inc/dec, push/pop r32 (incl. ESP), xchg eax
            let op = [0x40u8, 0x48, 0x50, 0x58, 0x90][rng.below(5) as usize] + rng.below(8) as u8;
            if op == 0x90 { return None; }
            v.push(op);
        }
        11 => {
            v.retain(|&b| b != 0x66);
            match rng.below(5) {
                0 => v.push(0x60),
                1 => v.push(0x61),
                2 => { v.push(0x6a); v.push(rng.next() as u8); }
                3 => { v.push(0x68); imm(rng, &mut v, 4); }
                _ => v.push(0x9c),
            }
        }
        12 => {
            // BCD
            v.retain(|&b| b != 0x66);
            match rng.below(6) {
                0 => v.push(0x27),
                1 => v.push(0x2f),
                2 => v.push(0x37),
                3 => v.push(0x3f),
                4 => { v.push(0xd4); v.push(if rng.below(2) == 0 { 10 } else { 1 + rng.below(255) as u8 }); }
                _ => { v.push(0xd5); v.push(if rng.below(2) == 0 { 10 } else { rng.next() as u8 }); }
            }
            if rng.below(2) == 0 { st.r[0] = (st.r[0] & !0xffff) | rng.below(0x200) as u32; }
            if rng.below(2) == 0 { st.flags ^= AF; }
        }
        13 => {
            v.extend([0x0f, 0xaf]);
            let r = reg(rng);
            let k = rm(rng);
            modrm(rng, &mut v, st, r, k, target);
        }
        14 => {
            let op = [0x69u8, 0x6b][rng.below(2) as usize];
            v.push(op);
            let r = reg(rng);
            let k = rm(rng);
            modrm(rng, &mut v, st, r, k, target);
            imm(rng, &mut v, if op == 0x69 { wide } else { 1 });
        }
        15 => {
            // bt/bts/btr/btc with a register bit offset: addresses outside the operand in memory
            v.extend([0x0f, [0xa3u8, 0xab, 0xb3, 0xbb][rng.below(4) as usize]]);
            let r = reg(rng);
            let k = rm(rng);
            let used = modrm(rng, &mut v, st, r, k, target);
            if used.is_some_and(|u| u == r) { return None; }
            if addr_regs(&v).contains(&r) { return None; }
            st.r[r as usize] = (rng.below(1024) as u32).wrapping_sub(512);
        }
        16 => {
            v.extend([0x0f, 0xba]);
            let e = 4 + rng.below(4) as u8;
            let k = rm(rng);
            modrm(rng, &mut v, st, e, k, target);
            v.push(rng.next() as u8);
        }
        17 => {
            v.extend([0x0f, [0xbcu8, 0xbd][rng.below(2) as usize]]);
            let r = reg(rng);
            let k = rm(rng);
            modrm(rng, &mut v, st, r, k, target);
        }
        18 => {
            let op = [0xa4u8, 0xa5, 0xac, 0xad][rng.below(4) as usize];
            v.extend([0x0f, op]);
            let r = reg(rng);
            let k = rm(rng);
            modrm(rng, &mut v, st, r, k, target);
            if op & 1 == 0 { v.push(rng.below(40) as u8); }
        }
        19 => {
            // cmpxchg, xadd (also with LOCK)
            if rng.below(3) == 0 { v.insert(0, 0xf0); }
            let op = [0xb0u8, 0xb1, 0xc0, 0xc1][rng.below(4) as usize];
            v.extend([0x0f, op]);
            let r = reg(rng);
            if v[0] == 0xf0 {
                modrm(rng, &mut v, st, r, Rm::Mem, target);
            } else {
                let k = rm(rng);
                modrm(rng, &mut v, st, r, k, target);
            }
            if rng.below(2) == 0 && !addr_regs(&v).contains(&0) { st.r[0] = load(target, 4) as u32; }
        }
        20 => {
            let r = reg(rng);
            let k = rm(rng);
            if rng.below(2) == 0 {
                v.extend([0x0f, 0x40 + rng.below(16) as u8]);
                modrm(rng, &mut v, st, r, k, target);
            } else {
                v.retain(|&b| b != 0x66);
                v.extend([0x0f, 0x90 + rng.below(16) as u8]);
                modrm(rng, &mut v, st, 0, k, target);
            }
        }
        21 => {
            let op = [0xb6u8, 0xb7, 0xbe, 0xbf][rng.below(4) as usize];
            v.extend([0x0f, op]);
            let r = reg(rng);
            let k = rm(rng);
            modrm(rng, &mut v, st, r, k, target);
        }
        22 => {
            // cmpxchg8b (half the time with edx:eax equal to memory)
            v.retain(|&b| b != 0x66);
            if rng.below(2) == 0 { v.push(0xf0); }
            v.extend([0x0f, 0xc7]);
            let t = target & !7;
            modrm(rng, &mut v, st, 1, Rm::Mem, t);
            if addr_regs(&v).iter().any(|&u| u == 0 || u == 2) { return None; }
            if rng.below(2) == 0 {
                let m = load(t, 8);
                st.r[0] = m as u32;
                st.r[2] = (m >> 32) as u32;
            }
        }
        23 => {
            // lea in every addressing form
            v.retain(|&b| b != 0x66);
            v.push(0x8d);
            let r = reg(rng);
            let t = rng.next() as u32;
            modrm(rng, &mut v, st, r, Rm::Mem, t);
        }
        24 => v.push([0x98u8, 0x99, 0x9e, 0x9f, 0xf5, 0xf8, 0xf9, 0xfc, 0xfd][rng.below(9) as usize]),
        25 => {
            v.retain(|&b| b != 0x66);
            v.extend([0x0f, 0xc8 + rng.below(8) as u8]);
        }
        26 => {
            // enter (nesting 0-3) / leave
            v.retain(|&b| b != 0x66);
            st.r[5] = DATA + 0x6800 + (rng.below(32) << 2) as u32;
            if rng.below(2) == 0 {
                v.push(0xc9);
            } else {
                v.push(0xc8);
                v.extend((rng.below(64) as u16 * 4).to_le_bytes());
                v.push(rng.below(4) as u8);
            }
        }
        _ => {
            // string instructions with and without REP, both directions
            let p = [None, Some(0xf3u8), Some(0xf2)][rng.below(3) as usize];
            let op = [0xa4u8, 0xa5, 0xaa, 0xab, 0xac, 0xad, 0xa6, 0xa7, 0xae, 0xaf][rng.below(10) as usize];
            if op & 1 == 0 { v.retain(|&b| b != 0x66); }
            if let Some(p) = p { v.push(p); }
            v.push(op);
            st.r[6] = DATA + 0x1000 + rng.below(0x800) as u32;
            st.r[7] = DATA + 0x2000 + rng.below(0x800) as u32;
            st.r[1] = rng.below(40) as u32;
            if rng.below(3) == 0 { st.flags |= DF } else { st.flags &= !DF }
            if rng.below(2) == 0 { st.r[0] = 0x4141_4141; }
        }
    }
    let ins = Decoder::new(32, &v, DecoderOptions::NONE).decode();
    if ins.is_invalid() || ins.len() != v.len() { return None; }
    Some(v)
}

#[test]
fn random_protected_mode_ops_match_the_host_cpu() {
    let _m = map();
    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    let mut done = 0;
    let mut failures = Vec::new();
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for _ in 0..200_000 {
        let mut data = vec![0u8; DATA_LEN];
        for b in data.iter_mut() {
            *b = if rng.below(4) == 0 { 0x41 } else { rng.next() as u8 };
        }
        let mut st = random_state(&mut rng);
        let Some(insn) = gen(&mut rng, &mut st, &data) else { continue };
        match compare(&insn, &st, &data) {
            Ok(true) => {
                done += 1;
                let m = Decoder::new(32, &insn, DecoderOptions::NONE).decode().mnemonic();
                *seen.entry(format!("{m:?}")).or_default() += 1;
            }
            Ok(false) => {}
            Err(e) => if failures.len() < 20 { failures.push(e) },
        }
    }
    assert!(failures.is_empty(), "{} checked, mismatches:\n{}", done, failures.join("\n"));
    assert!(done > 150_000, "{}", done);
    // every kind the generator makes was compared
    for m in ["Inc", "Dec", "Pushad", "Popad", "Daa", "Das", "Aaa", "Aas", "Aam", "Aad", "Cmpxchg8b", "Enter", "Leave",
              "Movsd", "Cmpsb", "Scasd", "Bts", "Xadd", "Cmpxchg", "Shld", "Push", "Pop"] {
        assert!(seen.contains_key(m), "{m} never compared: {seen:?}");
    }
}
