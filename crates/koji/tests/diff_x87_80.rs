//! The precise x87 mode (koji::fpu::X87_80, #52) against the host's x87: random sequences
//! on 80-bit values of every exponent range (beyond the double's, denormals, near-ties, zeros,
//! infinities, NaNs) and m64/m32/integer operands in a MAP_32BIT data page, at a random
//! precision control (24/53/64 bits) and rounding mode, run natively (x87 encodings are the
//! same in 64-bit mode) and in the interpreter. Every result byte must be the CPU's: the
//! registers left stored as m80, FST m64/m32, FIST, the condition codes and FCOMI's flags. (A
//! test binary of its own: the mode is global.)
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use iced_x86::code_asm::*;
use koji::{step, Cpu, Exit, Fault, Memory};

const PAGE: usize = 4096;
const OUT: u32 = 0x200; // output area: 8 doubles + i64 + i32 + status + eflags

struct TwoRegions {
    code: Vec<u8>,
    data_base: u32,
    data: Vec<u8>,
}

impl Memory for TwoRegions {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if (0x1000..0x1000 + self.code.len() as u32).contains(&a) {
            return Ok(self.code[(a - 0x1000) as usize]);
        }
        let o = a.wrapping_sub(self.data_base) as usize;
        self.data.get(o).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        let o = a.wrapping_sub(self.data_base) as usize;
        match self.data.get_mut(o) {
            Some(b) => { *b = v; Ok(()) }
            None => Err(Fault { addr: a, write: true }),
        }
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
}

fn gen(rng: &mut Rng) -> Result<Vec<u8>, IcedError> {
    let mut a = CodeAssembler::new(32)?;
    a.fninit()?;
    a.xor(eax, eax)?; // same EFLAGS and EAX on both sides
    a.fldcw(word_ptr(ebx + 0x1f0))?;
    let st = [st0, st1, st2, st3, st4, st5, st6, st7];
    let mut depth = 0u32;
    for k in 0..3 {
        a.fld(tword_ptr(ebx + 0x100 + 16 * k))?;
        depth += 1;
    }
    for _ in 0..1 + rng.below(6) {
        if depth < 2 {
            a.fld(tword_ptr(ebx + 0x100 + 16 * rng.below(8) as i32))?;
            depth += 1;
        }
        let i = 1 + rng.below(depth as u64 - 1) as usize;
        let m80 = tword_ptr(ebx + 0x100 + 16 * rng.below(8) as i32);
        let m64 = qword_ptr(ebx + 8 * rng.below(8) as i32);
        let m32 = dword_ptr(ebx + 0x80 + 4 * rng.below(4) as i32);
        let i32m = dword_ptr(ebx + 0x40 + 4 * rng.below(4) as i32);
        let i16m = word_ptr(ebx + 0x60 + 2 * rng.below(4) as i32);
        match rng.below(30) {
            0 => a.fadd_2(st0, st[i])?,
            1 => a.fsub_2(st[i], st0)?,
            2 => a.fsubr_2(st0, st[i])?,
            3 => a.fmul_2(st[i], st0)?,
            4 => a.fdiv_2(st0, st[i])?,
            5 => a.fdivr_2(st[i], st0)?,
            6 if depth > 1 => { a.faddp(st[i], st0)?; depth -= 1; }
            7 if depth > 1 => { a.fsubp(st[i], st0)?; depth -= 1; }
            8 if depth > 1 => { a.fdivrp(st[i], st0)?; depth -= 1; }
            9 => a.fadd(m64)?,
            10 => a.fmul(m32)?,
            11 => a.fisub(i32m)?,
            12 => a.fidivr(i16m)?,
            13 => a.fchs()?,
            14 => a.fabs()?,
            15 => { a.fabs()?; a.fsqrt()?; }
            16 => a.fxch(st0, st[i])?,
            17 if depth < 7 => { a.fld(st[i])?; depth += 1; }
            18 if depth < 7 => { a.fld(m80)?; depth += 1; }
            19 if depth < 7 => { a.fild(i32m)?; depth += 1; }
            20 if depth < 7 => { a.fild(qword_ptr(ebx + 0x70))?; depth += 1; }
            21 => a.frndint()?,
            22 => a.fcom_2(st0, st[i])?,
            23 => a.fucomi(st0, st[i])?,
            24 => a.fst(qword_ptr(ebx + OUT as i32 + 0x60))?,
            25 => a.fst(dword_ptr(ebx + OUT as i32 + 0x68))?,
            26 => a.fdiv(m64)?,
            27 => a.fsubr(m32)?,
            28 if depth < 7 => { a.fldpi()?; depth += 1; }
            _ => a.fist(dword_ptr(ebx + OUT as i32 + 0x48))?,
        }
    }
    a.fistp(qword_ptr(ebx + OUT as i32 + 0x40))?;
    depth -= 1;
    for k in 0..depth {
        a.fstp(tword_ptr(ebx + OUT as i32 + 0x80 + 16 * k as i32))?;
    }
    a.fnstsw(ax)?;
    a.mov(dword_ptr(ebx + OUT as i32 + 0x50), eax)?;
    a.pushfd()?;
    a.pop(dword_ptr(ebx + OUT as i32 + 0x54))?;
    a.assemble(0x1000)
}

/// An 80-bit value: around 1, anywhere in the exponent range, a denormal, near a tie of each
/// precision, or a special.
fn f80(rng: &mut Rng) -> [u8; 10] {
    let mut m = rng.next();
    let r = rng.next();
    let mut e: u16 = match r % 9 {
        0 => (0x3fff + (r >> 8) % 40) as u16 - 20,
        1 => (0x3fff + (r >> 8) % 2400) as u16 - 1200,
        2 => ((r >> 8) % 0x7ffe + 1) as u16,
        3 => { m >>= (r >> 8) % 64; 0 }
        4 => { m = 0; 0 }
        5 => { m = 1 << 63; 0x7fff }
        6 => { m = 0xc000_0000_0000_0000 | m >> 2; 0x7fff }
        7 => { m = (m | 1 << 63) & !0x7ff | (r >> 20) & 0x400; (0x3fff + (r >> 30) % 100) as u16 - 50 }
        _ => { m = (m | 1 << 63) & !0xff_ffff_ffff | (r >> 20) & 0x80_0000_0000; (0x3fff + (r >> 30) % 100) as u16 - 50 }
    };
    if e != 0 && e != 0x7fff {
        m |= 1 << 63;
    }
    if r & 1 << 63 != 0 {
        e |= 0x8000;
    }
    let mut b = [0u8; 10];
    b[..8].copy_from_slice(&m.to_le_bytes());
    b[8..].copy_from_slice(&e.to_le_bytes());
    b
}

fn fill(rng: &mut Rng, d: &mut [u8]) {
    for k in 0..8 {
        // (doubles from bits: any exponent, NaNs too)
        let f = rng.next() >> (rng.below(3) * 2) ^ (rng.below(2) << 63);
        d[8 * k..8 * k + 8].copy_from_slice(&f.to_le_bytes());
        let x = f80(rng);
        d[0x100 + 16 * k..0x10a + 16 * k].copy_from_slice(&x);
    }
    for k in 0..4 {
        let v = [0i32, 1, -1, 7, -100, 1 << 20, i32::MIN, 3, rng.next() as i32][rng.below(9) as usize];
        d[0x40 + 4 * k..0x44 + 4 * k].copy_from_slice(&v.to_le_bytes());
        let w = [1i16, -3, 2, 1000, -32768, 5, rng.next() as i16][rng.below(7) as usize];
        d[0x60 + 2 * k..0x62 + 2 * k].copy_from_slice(&w.to_le_bytes());
        let f = (rng.next() as u32).rotate_left(rng.below(3) as u32);
        d[0x80 + 4 * k..0x84 + 4 * k].copy_from_slice(&f.to_le_bytes());
    }
    let big = (0x0123_4567_89ab_cdefu64 ^ rng.next()) as i64;
    d[0x70..0x78].copy_from_slice(&big.to_le_bytes());
    // exceptions masked; PC 24 / 53 / 64 (and the reserved 01), any RC
    let cw = 0x3f | [0u16, 2, 3, 1][rng.below(4) as usize] << 8 | (rng.below(4) as u16) << 10;
    d[0x1f0..0x1f2].copy_from_slice(&cw.to_le_bytes());
}

#[test]
fn precise_x87_matches_the_host_cpu() {
    koji::fpu::X87_80.store(true, std::sync::atomic::Ordering::Relaxed);
    let page = unsafe {
        libc::mmap(core::ptr::null_mut(), 2 * PAGE, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                   libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT, -1, 0)
    } as *mut u8;
    assert!(page as isize != -1);
    let code = page;
    let data = unsafe { page.add(PAGE) };
    let data_base = data as usize as u32;
    let mut rng = Rng(0x5eed_80b1_7ca1_0001);
    let n: usize = std::env::var("X87_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(40_000);
    let mut failures = Vec::new();
    let mut fails = 0;
    for _ in 0..n {
        let body = gen(&mut rng).unwrap();
        let mut d = vec![0u8; PAGE];
        fill(&mut rng, &mut d);
        let mut c = vec![0x53, 0x48, 0x89, 0xfb];
        c.extend_from_slice(&body);
        c.extend([0x5b, 0xc3]);
        unsafe {
            core::ptr::copy_nonoverlapping(c.as_ptr(), code, c.len());
            core::ptr::copy_nonoverlapping(d.as_ptr(), data, PAGE);
            let f: extern "C" fn(*mut u8) = core::mem::transmute(code);
            f(data);
        }
        let native = unsafe { core::slice::from_raw_parts(data, PAGE) }.to_vec();
        let mut mem = TwoRegions { code: body.clone(), data_base, data: d.clone() };
        let mut cpu = Cpu::default();
        cpu.eip = 0x1000;
        cpu.regs[3] = data_base;
        cpu.regs[4] = data_base + 0xf00;
        let end = 0x1000 + body.len() as u32;
        let mut k = 0;
        while cpu.eip != end {
            assert_eq!(step(&mut cpu, &mut mem), Exit::Continue, "at {:x}", cpu.eip);
            k += 1;
            assert!(k < 100);
        }
        let o = OUT as usize;
        let (mut want, mut got) = (native[o..o + 0x100].to_vec(), mem.data[o..o + 0x100].to_vec());
        // the status word: C0..C3 and TOP (no exception flags: koji keeps none); EFLAGS: ZF PF CF
        for v in [&mut want, &mut got] {
            let sw = u16::from_le_bytes([v[0x50], v[0x51]]) & 0x7d00;
            v[0x50..0x52].copy_from_slice(&sw.to_le_bytes());
            v[0x52] = 0;
            v[0x53] = 0;
            let fl = u32::from_le_bytes(v[0x54..0x58].try_into().unwrap()) & 0x45;
            v[0x54..0x58].copy_from_slice(&fl.to_le_bytes());
        }
        if want != got {
            fails += 1;
            if failures.len() < 10 {
                use iced_x86::{Decoder, DecoderOptions, Formatter, NasmFormatter};
                let mut s = String::new();
                let mut f = NasmFormatter::new();
                for ins in Decoder::with_ip(32, &body, 0x1000, DecoderOptions::NONE) {
                    f.format(&ins, &mut s);
                    s += "; ";
                }
                let diff: Vec<String> = (0..0x100).filter(|&i| want[i] != got[i]).map(|i| format!("+{i:#x}: {:02x} {:02x}", want[i], got[i])).collect();
                failures.push(format!("cw {:#x}: {s}\n  {}", u16::from_le_bytes([d[0x1f0], d[0x1f1]]), diff.join(" ")));
            }
        }
    }
    assert!(failures.is_empty(), "{fails} of {n} differ:\n{}", failures.join("\n"));
}
