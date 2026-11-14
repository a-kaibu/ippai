//! vcomp*.dll (OpenMP for MSVC programs): a team of one thread. Parallel regions run on the
//! calling thread and every work-sharing loop gives it all the iterations.

use crate::api::{implement, Ctx};
use crate::mem::{r32, w32};

const DLLS: &[&str] = &["vcomp", "vcomp90", "vcomp100", "vcomp110", "vcomp120", "vcomp140"];

/// _vcomp_fork(ifval, nargs, wrapper, ...): wrapper(args...) once, here.
fn _vcomp_fork(c: &mut Ctx) -> u32 {
    let (n, f) = (c.a(1), c.a(2));
    let args: Vec<u32> = (0..n).map(|i| c.a(3 + i)).collect();
    c.call(f, &args);
    0
}
/// (first, last, step, increment, &begin, &end)
fn _vcomp_for_static_simple_init(c: &mut Ctx) -> u32 {
    w32(c.a(4), c.a(0));
    w32(c.a(5), c.a(1));
    0
}
/// (first, last, step, chunksize, &loops, &begin, &end, &next, &lastchunk)
fn _vcomp_for_static_init(c: &mut Ctx) -> u32 {
    let (first, last, step) = (c.a(0) as i32, c.a(1) as i32, c.a(2) as i32);
    let (loops, begin, end, next, lastchunk) = (c.a(4), c.a(5), c.a(6), c.a(7), c.a(8));
    // (a loop that runs no iteration: none)
    let none = first != last && (step == 0 || (step > 0) != (last > first));
    w32(loops, if none { 0 } else { 1 });
    if begin != 0 {
        w32(begin, first as u32);
        w32(lastchunk, first as u32);
    }
    w32(end, last as u32);
    w32(next, 0);
    0
}
// The loop and sections state is the calling thread's: every guest thread that runs a
// parallel region is a team of one here, and two of them sharing one state (a process-wide
// Mutex, as before) ran each other's ranges or none.
thread_local! {
    static DYN: std::cell::Cell<Option<(u32, u32)>> = const { std::cell::Cell::new(None) };
    static SECTIONS: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}
/// (flags, first, last, step, chunksize): the next _vcomp_for_dynamic_next gets it all. The
/// step is the size of one (VCOMP_DYNAMIC_FLAGS_INCREMENT 0x40 in flags: up, else down), as
/// Wine's vcomp reads it; any nonzero step is a loop.
pub(crate) fn dynamic_range(first: u32, last: u32, step: i32) -> Option<(u32, u32)> {
    (step != 0).then_some((first, last))
}
fn _vcomp_for_dynamic_init(c: &mut Ctx) -> u32 {
    DYN.with(|d| d.set(dynamic_range(c.a(1), c.a(2), c.a(3) as i32)));
    0
}
fn _vcomp_for_dynamic_next(c: &mut Ctx) -> u32 {
    match DYN.with(|d| d.take()) {
        Some((b, e)) => {
            w32(c.a(0), b);
            w32(c.a(1), e);
            1
        }
        None => 0,
    }
}
fn one(_: &mut Ctx) -> u32 {
    1
}
fn zero(_: &mut Ctx) -> u32 {
    0
}
/// _vcomp_sections_init(n): the one thread takes the sections in order
fn _vcomp_sections_init(c: &mut Ctx) -> u32 {
    SECTIONS.with(|s| s.set((0, c.a(0))));
    0
}
/// The next section of (taken, count), and the state after it (u32::MAX: none left).
pub(crate) fn next_section((taken, n): (u32, u32)) -> (u32, (u32, u32)) {
    if taken < n { (taken, (taken + 1, n)) } else { (u32::MAX, (taken, n)) }
}
fn _vcomp_sections_next(_: &mut Ctx) -> u32 {
    SECTIONS.with(|s| {
        let (r, next) = next_section(s.get());
        s.set(next);
        r
    })
}
/// _vcomp_atomic_add_i4(dest, v) and friends: atomic, since other guest threads may run
/// regions on the same variable at the same time
fn atomic_at<'a>(p: u32) -> Option<&'a std::sync::atomic::AtomicU32> {
    (p != 0 && p % 4 == 0 && p as u64 + 4 <= crate::mem::GUEST_TOP as u64).then(|| unsafe { &*(p as usize as *const std::sync::atomic::AtomicU32) })
}
fn add_i4(c: &mut Ctx) -> u32 {
    match atomic_at(c.a(0)) {
        Some(a) => {
            a.fetch_add(c.a(1), std::sync::atomic::Ordering::SeqCst);
        }
        None => w32(c.a(0), r32(c.a(0)).wrapping_add(c.a(1))),
    }
    0
}
fn sub_i4(c: &mut Ctx) -> u32 {
    match atomic_at(c.a(0)) {
        Some(a) => {
            a.fetch_sub(c.a(1), std::sync::atomic::Ordering::SeqCst);
        }
        None => w32(c.a(0), r32(c.a(0)).wrapping_sub(c.a(1))),
    }
    0
}

pub fn init() {
    for d in DLLS {
        for (name, f) in [
            ("_vcomp_fork", _vcomp_fork as crate::api::Handler),
            ("_vcomp_for_static_simple_init", _vcomp_for_static_simple_init),
            ("_vcomp_for_static_init", _vcomp_for_static_init),
            ("_vcomp_for_static_end", zero),
            ("_vcomp_for_dynamic_init", _vcomp_for_dynamic_init),
            ("_vcomp_for_dynamic_next", _vcomp_for_dynamic_next),
            ("_vcomp_sections_init", _vcomp_sections_init),
            ("_vcomp_sections_next", _vcomp_sections_next),
            ("_vcomp_barrier", zero),
            ("_vcomp_set_num_threads", zero),
            ("_vcomp_single_begin", one),
            ("_vcomp_single_end", zero),
            ("_vcomp_master_begin", one),
            ("_vcomp_master_end", zero),
            ("_vcomp_enter_critsect", zero),
            ("_vcomp_leave_critsect", zero),
            ("_vcomp_flush", zero),
            ("_vcomp_atomic_add_i4", add_i4),
            ("_vcomp_atomic_sub_i4", sub_i4),
            ("_vcomp_get_thread_num", zero),
            ("omp_get_thread_num", zero),
            ("omp_get_num_threads", one),
            ("omp_get_max_threads", one),
            ("omp_get_num_procs", one),
            ("omp_set_num_threads", zero),
            ("omp_in_parallel", zero),
            ("omp_get_dynamic", zero),
            ("omp_set_dynamic", zero),
            ("omp_get_nested", zero),
            ("omp_set_nested", zero),
            ("omp_init_lock", zero),
            ("omp_destroy_lock", zero),
            ("omp_set_lock", zero),
            ("omp_unset_lock", zero),
            ("omp_test_lock", one),
            ("omp_init_nest_lock", zero),
            ("omp_destroy_nest_lock", zero),
            ("omp_set_nest_lock", zero),
            ("omp_unset_nest_lock", zero),
        ] {
            implement(d, name, f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two guest threads, each its own team, run dynamic loops and sections at the same time:
    /// each gets exactly its own ranges (they overwrote each other through one global state).
    #[test]
    fn loops_and_sections_are_per_thread() {
        let threads: Vec<_> = (0..8u32)
            .map(|t| {
                std::thread::spawn(move || {
                    for round in 0..2000u32 {
                        let (first, last) = (t * 1000 + round, t * 1000 + round + 7);
                        DYN.with(|d| d.set(dynamic_range(first, last, 1)));
                        std::thread::yield_now();
                        assert_eq!(DYN.with(|d| d.take()), Some((first, last)));
                        assert_eq!(DYN.with(|d| d.take()), None);
                        SECTIONS.with(|s| s.set((0, t + 1)));
                        let mut got = vec![];
                        loop {
                            let (r, next) = next_section(SECTIONS.with(|s| s.get()));
                            SECTIONS.with(|s| s.set(next));
                            if r == u32::MAX {
                                break;
                            }
                            got.push(r);
                            std::thread::yield_now();
                        }
                        assert_eq!(got, (0..t + 1).collect::<Vec<_>>());
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }

    proptest::proptest! {
        /// Sections: 0..n in order, once each, then u32::MAX for ever.
        #[test]
        fn sections_in_order(n in 0u32..200, extra in 0u32..5) {
            let mut st = (0, n);
            for i in 0..n {
                let (r, next) = next_section(st);
                proptest::prop_assert_eq!(r, i);
                st = next;
            }
            for _ in 0..=extra {
                let (r, next) = next_section(st);
                proptest::prop_assert_eq!(r, u32::MAX);
                st = next;
            }
        }

        /// A dynamic loop with any nonzero step (up or down) hands out its whole range once.
        #[test]
        fn dynamic_loops_any_step(first: u32, last: u32, step: i32) {
            proptest::prop_assert_eq!(dynamic_range(first, last, step), (step != 0).then_some((first, last)));
        }
    }
}
