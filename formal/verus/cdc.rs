// Verus (https://github.com/verus-lang/verus): unbounded proofs, for every input length, of the
// FastCDC cutter of crates/masu/src/cdc.rs (Cutter::next, the same steps on the same state;
// the gear table and the masks are parameters here). What Kani and proptest check for
// bounded inputs, proved for all:
//
// * a cut is never before `min` bytes of the chunk nor after `max` (every chunk but a file's
//   last is min..=max long, and the last one at most max);
// * without a cut, the chunk so far stays shorter than `max` (so the next bytes cut by then);
// * the cut is inside the bytes given (at least one of them, at most all).
//
//   verus formal/verus/cdc.rs        (formal/verus.sh; CI: formal.yml job "verus")
use vstd::prelude::*;

verus! {

pub struct Cutter {
    pub min: u64,
    pub avg: u64,
    pub max: u64,
    pub small: u64,
    pub large: u64,
    /// bytes of the current chunk so far
    pub len: u64,
    pub h: u64,
}

/// Params::with_avg's sizes (min avg/4, max avg*4, avg at least 64 KiB and at most 2^40 here)
pub open spec fn well_formed(c: &Cutter) -> bool {
    &&& 0 < c.min <= c.avg <= c.max
    &&& c.min < c.max
    &&& c.max <= 0x100_0000_0000
    &&& c.len < c.max
}

/// Where in `buf` the current chunk ends, if it does (cdc.rs Cutter::next).
pub fn next(c: &mut Cutter, buf: &[u8], gear: &[u64; 256]) -> (r: Option<usize>)
    requires
        well_formed(old(c)),
    ensures
        well_formed(final(c)),
        match r {
            Some(i) => {
                &&& 1 <= i <= buf.len()
                &&& final(c).len == 0
                &&& old(c).min <= old(c).len + i <= old(c).max
            },
            None => final(c).len == old(c).len + buf.len(),
        },
{
    let mut i: usize = 0;
    // (no cut before min: the hash is not even kept there)
    if c.len < c.min {
        let need = c.min - c.len;
        let skip: usize = if (need as u128) < (buf.len() as u128) { need as usize } else { buf.len() };
        c.len = c.len + skip as u64;
        i = skip;
        if c.len < c.min {
            return None;
        }
    }
    let mut h = c.h;
    while i < buf.len()
        invariant
            0 < c.min <= c.avg <= c.max,
            c.min < c.max,
            c.max <= 0x100_0000_0000,
            c.min == old(c).min,
            c.avg == old(c).avg,
            c.max == old(c).max,
            c.min <= c.len < c.max,
            i <= buf.len(),
            c.len == old(c).len + i,
        decreases buf.len() - i,
    {
        h = (h << 1u64).wrapping_add(gear[buf[i] as usize]);
        c.len = c.len + 1;
        i = i + 1;
        let m = if c.len < c.avg { c.small } else { c.large };
        if h & m == 0 || c.len >= c.max {
            c.len = 0;
            c.h = 0;
            return Some(i);
        }
    }
    c.h = h;
    None
}

} // verus!

fn main() {}
