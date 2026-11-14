//! RDTSC reads the embedder's clock (koji::set_tsc_source): EDX:EAX its 64 bits, never going
//! back. A constant one froze 十六夜の花嫁 (its game clock is TSC ticks over ticks per ms, the
//! rate measured over a second of timeGetTime: 0 ticks, so the default rate and the same time
//! forever, and its scheduler never ran the title).

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use koji::{step, Cpu, Exit, Fault, Memory};
use proptest::prelude::*;

struct Flat(Vec<u8>);
impl Memory for Flat {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        match self.0.get_mut(a as usize) {
            Some(b) => {
                *b = v;
                Ok(())
            }
            None => Err(Fault { addr: a, write: true }),
        }
    }
}

const CODE: u32 = 0x1000;

static CLOCK: AtomicU64 = AtomicU64::new(0);
fn clock() -> u64 {
    CLOCK.load(Relaxed)
}

fn rdtsc(cpu: &mut Cpu, mode64: bool) -> u64 {
    let mut m = Flat(vec![0; 0x2000]);
    m.0[CODE as usize..CODE as usize + 2].copy_from_slice(&[0x0f, 0x31]);
    cpu.eip = CODE;
    cpu.mode64 = mode64 as u32;
    cpu.eflags = 2;
    // (the high halves RDTSC clears in long mode)
    cpu.set_gpr(0, u64::MAX);
    cpu.set_gpr(2, u64::MAX);
    assert_eq!(step(cpu, &mut m), Exit::Continue);
    assert_eq!(cpu.eip, CODE + 2);
    let (a, d) = (cpu.gpr(0), cpu.gpr(2));
    if mode64 {
        assert!(a >> 32 == 0 && d >> 32 == 0, "RAX/RDX upper halves cleared");
    }
    (d as u32 as u64) << 32 | a as u32 as u64
}

// (one test: the clock is the process's)
#[test]
fn rdtsc_reads_the_embedders_clock() {
    let mut cpu = Cpu::default();
    // none set: Cpu::tsc
    cpu.tsc = 0x1234_5678_9abc_def0;
    assert_eq!(rdtsc(&mut cpu, false), 0x1234_5678_9abc_def0);
    koji::set_tsc_source(clock);
    for v in [0, 1, 0xffff_ffff, 0x1_0000_0000, 0x7654_3210_fedc_ba98, u64::MAX] {
        CLOCK.store(v, Relaxed);
        assert_eq!(rdtsc(&mut cpu, false), v);
        assert_eq!(rdtsc(&mut cpu, true), v);
    }
    // the clock moves on between two reads: so does RDTSC, whatever Cpu::tsc holds
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(256));
    runner
        .run(&(any::<u64>(), proptest::collection::vec(0u64..1 << 40, 1..20), any::<bool>()), |(start, steps, m64)| {
            let mut c = Cpu { tsc: 7, ..Cpu::default() };
            let mut t = start >> 1;
            CLOCK.store(t, Relaxed);
            let mut last = rdtsc(&mut c, m64);
            prop_assert_eq!(last, t);
            for s in steps {
                t = t.saturating_add(s);
                CLOCK.store(t, Relaxed);
                let now = rdtsc(&mut c, m64);
                prop_assert!(now >= last);
                prop_assert_eq!(now, t);
                last = now;
            }
            Ok(())
        })
        .unwrap();
}
