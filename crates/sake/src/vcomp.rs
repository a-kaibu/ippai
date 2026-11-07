//! vcomp*.dll (OpenMP for MSVC programs): a team of one thread. Parallel regions run on the
//! calling thread and every work-sharing loop gives it all the iterations.

use crate::api::{implement, Ctx};
use crate::mem::{r32, w32};

const DLLS: &[&str] = &[
    "vcomp", "vcomp90", "vcomp100", "vcomp110", "vcomp120", "vcomp140",
];

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
/// (flags, first, last, step, chunksize): the next _vcomp_for_dynamic_next gets it all
static DYN: std::sync::Mutex<Option<(u32, u32)>> = std::sync::Mutex::new(None);
fn _vcomp_for_dynamic_init(c: &mut Ctx) -> u32 {
    let step = c.a(3) as i32;
    *DYN.lock().unwrap() = (step > 0).then(|| (c.a(1), c.a(2)));
    0
}
fn _vcomp_for_dynamic_next(c: &mut Ctx) -> u32 {
    match DYN.lock().unwrap().take() {
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
static SECTIONS: std::sync::Mutex<(u32, u32)> = std::sync::Mutex::new((0, 0));
fn _vcomp_sections_init(c: &mut Ctx) -> u32 {
    *SECTIONS.lock().unwrap() = (0, c.a(0));
    0
}
fn _vcomp_sections_next(_: &mut Ctx) -> u32 {
    let mut s = SECTIONS.lock().unwrap();
    if s.0 < s.1 {
        s.0 += 1;
        s.0 - 1
    } else {
        u32::MAX
    }
}
/// _vcomp_atomic_add_i4(dest, v) and friends: one thread, plain arithmetic
fn add_i4(c: &mut Ctx) -> u32 {
    w32(c.a(0), r32(c.a(0)).wrapping_add(c.a(1)));
    0
}
fn sub_i4(c: &mut Ctx) -> u32 {
    w32(c.a(0), r32(c.a(0)).wrapping_sub(c.a(1)));
    0
}

pub fn init() {
    for d in DLLS {
        for (name, f) in [
            ("_vcomp_fork", _vcomp_fork as crate::api::Handler),
            (
                "_vcomp_for_static_simple_init",
                _vcomp_for_static_simple_init,
            ),
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
