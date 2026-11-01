//! Runs real functions from vnbox's x86_64 ntdll.dll (Wine 11.20) on the interpreter in long mode
//! (relocated below 4GB, Win64 calling convention).
//! Skipped when the DLL is not present (it lives in the vnvm image build tree).

use vnx86::{Cpu, Exit, Fault, Memory, ESP};

const LOAD_AT: u32 = 0x1000_0000;

const NTDLL: &str = "/home/user/img/rootfs/opt/wine-vnbox/lib/wine/x86_64-windows/ntdll.dll";
const RETURN_TRAP: u32 = 0x0000_f000;
const STACK_TOP: u32 = 0x0010_0000;
const TEB: u32 = 0x0020_0000;
const HEAP: u32 = 0x0030_0000;

/// 4 GiB address space backed by lazily allocated 4 KiB pages.
struct Sparse {
    pages: Vec<Option<Box<[u8; 4096]>>>,
}

impl Sparse {
    fn new() -> Self {
        Sparse { pages: (0..1 << 20).map(|_| None).collect() }
    }
    fn map(&mut self, addr: u32, len: u32) {
        for p in addr >> 12..=(addr + len.max(1) - 1) >> 12 {
            self.pages[p as usize].get_or_insert_with(|| Box::new([0; 4096]));
        }
    }
    fn put(&mut self, addr: u32, data: &[u8]) {
        self.map(addr, data.len() as u32);
        for (i, b) in data.iter().enumerate() {
            self.write8(addr + i as u32, *b).unwrap();
        }
    }
    fn get(&mut self, addr: u32, len: usize) -> Vec<u8> {
        (0..len).map(|i| self.read8(addr + i as u32).unwrap()).collect()
    }
    fn cstr(&mut self, addr: u32) -> String {
        let mut v = Vec::new();
        let mut a = addr;
        while let Ok(b) = self.read8(a) {
            if b == 0 {
                break;
            }
            v.push(b);
            a += 1;
        }
        String::from_utf8_lossy(&v).into_owned()
    }
}

impl Memory for Sparse {
    #[inline]
    fn read8(&mut self, addr: u32) -> Result<u8, Fault> {
        match &self.pages[(addr >> 12) as usize] {
            Some(p) => Ok(p[(addr & 0xfff) as usize]),
            None => Err(Fault { addr, write: false }),
        }
    }
    #[inline]
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), Fault> {
        match &mut self.pages[(addr >> 12) as usize] {
            Some(p) => {
                p[(addr & 0xfff) as usize] = v;
                Ok(())
            }
            None => Err(Fault { addr, write: true }),
        }
    }
}

fn u16_at(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

struct Image {
    base: u32,
    exports: std::collections::HashMap<String, u32>,
}

/// Maps a PE32+ image at LOAD_AT and applies its base relocations.
fn load(mem: &mut Sparse, file: &[u8]) -> Image {
    let pe = u32_at(file, 0x3c) as usize;
    assert_eq!(&file[pe..pe + 4], b"PE\0\0");
    let nsec = u16_at(file, pe + 6) as usize;
    let opt = pe + 24;
    assert_eq!(u16_at(file, opt), 0x20b, "not PE32+");
    let pref = u32_at(file, opt + 24) as u64 | (u32_at(file, opt + 28) as u64) << 32;
    let base = LOAD_AT;
    let image_size = u32_at(file, opt + 56);
    let headers = u32_at(file, opt + 60) as usize;
    mem.map(base, image_size);
    mem.put(base, &file[..headers]);
    let sec = opt + u16_at(file, pe + 20) as usize;
    for i in 0..nsec {
        let s = sec + i * 40;
        let va = u32_at(file, s + 12);
        let raw_size = u32_at(file, s + 16) as usize;
        let raw = u32_at(file, s + 20) as usize;
        if raw_size > 0 {
            mem.put(base + va, &file[raw..raw + raw_size]);
        }
    }
    // data directories start at opt + 112 in PE32+
    let delta = (base as u64).wrapping_sub(pref);
    let (rel_rva, rel_size) = (u32_at(file, opt + 112 + 5 * 8), u32_at(file, opt + 112 + 5 * 8 + 4));
    let mut off = 0;
    while off < rel_size {
        let page = mem.read32(base + rel_rva + off).unwrap();
        let size = mem.read32(base + rel_rva + off + 4).unwrap();
        if size == 0 { break; }
        for k in (8..size).step_by(2) {
            let e = mem.read16(base + rel_rva + off + k).unwrap();
            match e >> 12 {
                10 => {
                    let a = base + page + (e & 0xfff) as u32;
                    let v = mem.read64(a).unwrap();
                    mem.write64(a, v.wrapping_add(delta)).unwrap();
                }
                0 => {}
                t => panic!("relocation type {t}"),
            }
        }
        off += size;
    }
    let exp_rva = u32_at(file, opt + 112);
    let mut exports = std::collections::HashMap::new();
    let d = mem.get(base + exp_rva, 40);
    let (nfun, nnames) = (u32_at(&d, 20), u32_at(&d, 24));
    let (funcs, names, ords) = (u32_at(&d, 28), u32_at(&d, 32), u32_at(&d, 36));
    for i in 0..nnames {
        let name_rva = mem.read32(base + names + i * 4).unwrap();
        let ord = mem.read16(base + ords + i * 2).unwrap() as u32;
        assert!(ord < nfun);
        let rva = mem.read32(base + funcs + ord * 4).unwrap();
        let name = mem.cstr(base + name_rva);
        exports.insert(name, base + rva);
    }
    Image { base, exports }
}

struct Machine {
    mem: Sparse,
    cpu: Cpu,
    ntdll: Image,
    heap: u32,
    steps: u64,
}

impl Machine {
    fn new() -> Option<Self> {
        let file = std::fs::read(NTDLL).ok()?;
        let mut mem = Sparse::new();
        let ntdll = load(&mut mem, &file);
        mem.map(STACK_TOP - 0x10000, 0x10000);
        mem.map(TEB, 0x2000);
        mem.write64(TEB + 0x30, TEB as u64).unwrap(); // NtTib.Self
        mem.map(HEAP, 0x10000);
        mem.map(RETURN_TRAP, 1);
        let mut cpu = Cpu::default();
        cpu.mode64 = 1;
        cpu.gs_base = TEB;
        cpu.trap_lo = RETURN_TRAP;
        cpu.trap_hi = RETURN_TRAP + 1;
        Some(Machine { mem, cpu, ntdll, heap: HEAP, steps: 0 })
    }

    fn alloc(&mut self, data: &[u8]) -> u64 {
        let a = self.heap;
        self.mem.put(a, data);
        self.heap = (a + data.len() as u32 + 15) & !15;
        a as u64
    }

    /// Win64 call: the first four integer arguments in RCX, RDX, R8, R9 (and XMM0-3 for
    /// doubles, `fargs`), the rest on the stack above 32 bytes of shadow space.
    fn call(&mut self, func: &str, args: &[u64], fargs: &[f64]) -> u64 {
        let entry = *self.ntdll.exports.get(func).unwrap_or_else(|| panic!("no export {func}"));
        self.call_addr(entry, args, fargs)
    }

    fn call_addr(&mut self, entry: u32, args: &[u64], fargs: &[f64]) -> u64 {
        let mut sp = STACK_TOP - 0x100;
        let stack_args = if args.len() > 4 { &args[4..] } else { &[] };
        sp -= 8 * stack_args.len() as u32;
        sp &= !15;
        for (i, a) in stack_args.iter().enumerate() {
            self.mem.write64(sp + 32 + 8 * i as u32, *a).unwrap();
        }
        sp -= 8;
        self.mem.write64(sp, RETURN_TRAP as u64).unwrap();
        for (i, r) in [1usize, 2, 8, 9].iter().enumerate() {
            self.cpu.set_gpr(*r, args.get(i).copied().unwrap_or(0));
        }
        for (i, f) in fargs.iter().enumerate() {
            self.cpu.set_xmm(i, f.to_bits() as u128);
        }
        self.cpu.set_gpr(ESP, sp as u64);
        self.cpu.eip = entry;
        loop {
            self.steps += 1;
            match vnx86::step(&mut self.cpu, &mut self.mem) {
                Exit::Continue => {}
                Exit::Trap(RETURN_TRAP) => return self.cpu.gpr(0),
                e => panic!("{func:x}: {e:?} at rip {:#x} (ntdll+{:#x})", self.cpu.eip, self.cpu.eip.wrapping_sub(self.ntdll.base), func = entry),
            }
        }
    }
}

#[test]
fn x64_ntdll_functions_run_on_the_interpreter() {
    let Some(mut m) = Machine::new() else {
        eprintln!("skipping: {NTDLL} not found");
        return;
    };
    let t0 = std::time::Instant::now();

    let text = m.alloc(b"The quick brown fox jumps over the lazy dog");
    assert_eq!(m.call("RtlComputeCrc32", &[0, text, 43], &[]) as u32, 0x414fa339);
    assert_eq!(m.call("strlen", &[text], &[]), 43);

    let buf = m.alloc(&[0; 256]);
    let fmt = m.alloc(b"%d %x %-5s| %08X %I64d %p\0");
    let s = m.alloc(b"abc\0");
    let n = m.call("sprintf", &[buf, fmt, -42i64 as u64, 255, s, 0xbeef, 0x1234_89ab_cdef, 0xdead_beef_cafe], &[]);
    let out = m.mem.cstr(buf as u32);
    assert_eq!(out, "-42 ff abc  | 0000BEEF 20016857337327 0000DEADBEEFCAFE");
    assert_eq!(n as usize, out.len());

    let input = m.alloc(b"123 0x7f word\0");
    let sfmt = m.alloc(b"%d %x %15s\0");
    let iv = m.alloc(&[0; 8]);
    let sv = m.alloc(&[0; 16]);
    assert_eq!(m.call("sscanf", &[input, sfmt, iv, iv + 4, sv], &[]), 3);
    assert_eq!(m.mem.read32(iv as u32).unwrap(), 123);
    assert_eq!(m.mem.read32(iv as u32 + 4).unwrap(), 0x7f);
    assert_eq!(m.mem.cstr(sv as u32), "word");

    // qsort of 64-bit values with a guest comparator:
    // mov rax,[rcx]; cmp rax,[rdx]; setg al; setl cl; sub al,cl; movsx eax,al; ret
    let cmp = m.alloc(&[0x48, 0x8b, 0x01, 0x48, 0x3b, 0x02, 0x0f, 0x9f, 0xc0, 0x0f, 0x9c, 0xc1, 0x28, 0xc8, 0x0f, 0xbe, 0xc0, 0xc3]);
    let vals: [i64; 8] = [5, -3, 1 << 40, 0, 42, 7, -(1 << 50), 8];
    let arr = m.alloc(&vals.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    m.call("qsort", &[arr, 8, 8, cmp], &[]);
    let mut sorted = vals;
    sorted.sort();
    let got: Vec<i64> = (0..8).map(|i| m.mem.read64(arr as u32 + i * 8).unwrap() as i64).collect();
    assert_eq!(got, sorted);

    // libm on SSE2: doubles in and out in XMM0
    m.call("sqrt", &[], &[2.0]);
    assert_eq!(f64::from_bits(m.cpu.xmm(0) as u64), 2.0f64.sqrt());
    m.call("pow", &[], &[1.5, 10.0]);
    assert!((f64::from_bits(m.cpu.xmm(0) as u64) - 1.5f64.powi(10)).abs() < 1e-9);
    m.call("sin", &[], &[0.5]);
    assert!((f64::from_bits(m.cpu.xmm(0) as u64) - 0.5f64.sin()).abs() < 1e-15);

    // string/memory routines (SSE2 paths, rep movs)
    let big: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
    let src = m.alloc(&big);
    let dst = m.alloc(&[0; 1000]);
    m.call("memcpy", &[dst, src, 1000], &[]);
    assert_eq!(m.mem.get(dst as u32, 1000), big);
    assert_eq!(m.call("memcmp", &[dst, src, 1000], &[]) as i32, 0);
    let h = m.alloc(b"hello world\0");
    let w = m.alloc(b"world\0");
    assert_eq!(m.call("strstr", &[h, w], &[]), h + 6);

    let dt = t0.elapsed();
    eprintln!("{} instructions in {:?} ({:.1} M/s)", m.steps, dt, m.steps as f64 / dt.as_secs_f64() / 1e6);
}
