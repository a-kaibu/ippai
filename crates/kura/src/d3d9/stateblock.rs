//! Direct3D 9 state blocks (formal/tla/StateBlocks.tla, winref probe d3d9sb): a block holds a
//! part of the device state, its mask: everything (D3DSBT_ALL), the pixel or the vertex states
//! (D3DSBT_PIXELSTATE / VERTEXSTATE, the lists the Direct3D 9 docs give for each), or what was
//! set between BeginStateBlock and EndStateBlock. Capture takes the masked states from the
//! device, Apply puts them back; the rest of the device state is left alone. While recording,
//! the Set* calls go to the recording, not to the device. The lists are Windows' (the
//! winref probe's answers): D3DSBT_ALL holds the defined states only (the pixel ones, the
//! vertex ones, and render state 40, D3D8's EDGEANTIALIAS, a pixel state still), a vertex
//! block no sampler state (D3DSAMP_DMAPOFFSET is ALL's alone), a pixel block those of every
//! sampler (the displacement map and vertex texture samplers too).

use super::device::{State, SAMPLERS};
use std::collections::BTreeSet;

/// (render states)
const PIXEL_RS: &[u32] = &[
    7, 8, 9, 14, 15, 16, 19, 20, 23, 24, 25, 26, 27, 36, 37, 38, 40, 52, 53, 54, 55, 56, 57, 58, 59, 60, 128, 129, 130,
    131, 132, 133, 134, 135, 168, 171, 174, 175, 176, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 198,
    199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209,
];
const VERTEX_RS: &[u32] = &[
    9, 22, 28, 29, 34, 35, 36, 37, 38, 48, 136, 137, 139, 140, 141, 142, 143, 145, 146, 147, 148, 151, 152, 154,
    155, 156, 157, 158, 159, 160, 161, 162, 163, 166, 167, 170, 172, 173, 178, 179, 180, 181, 182, 183, 184,
];
/// (texture stage states)
const PIXEL_TSS: &[u32] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 22, 23, 24, 26, 27, 28];
const VERTEX_TSS: &[u32] = &[11, 24];
/// (sampler states)
const PIXEL_SS: &[u32] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
const VERTEX_SS: &[u32] = &[];
const ALL_SS: &[u32] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];

/// Which parts of the device state a block holds.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Mask {
    pub rs: [u64; 4],
    pub tss: [u64; 8],
    pub ss: [u16; SAMPLERS],
    pub textures: u32,
    pub streams: u16,
    pub freq: u16,
    pub indices: bool,
    /// the vertex declaration and the FVF (each sets the other)
    pub decl: bool,
    pub vs: bool,
    pub ps: bool,
    pub vsf: Vec<u64>,
    pub psf: Vec<u64>,
    pub vsi: u16,
    pub psi: u16,
    pub vsb: u16,
    pub psb: u16,
    /// vertex integer and boolean constants past the first 16 (software vertex processing's
    /// 2048), register r at bit r - 16
    pub vsi_more: Vec<u64>,
    pub vsb_more: Vec<u64>,
    pub transforms: BTreeSet<u32>,
    /// every transform (D3DSBT_ALL: those never set too)
    pub every_transform: bool,
    pub viewport: bool,
    pub scissor: bool,
    pub material: bool,
    /// lights (their parameters and whether they are on)
    pub lights: BTreeSet<u32>,
    pub clip: u8,
    pub npatch: bool,
}

fn bit(b: &[u64], i: usize) -> bool {
    b.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 != 0)
}
fn set_bit(b: &mut Vec<u64>, i: usize) {
    if b.len() <= i / 64 {
        b.resize(i / 64 + 1, 0);
    }
    b[i / 64] |= 1 << (i % 64);
}

impl Mask {
    pub fn all(st: &State) -> Mask {
        let mut m = Mask { textures: !0, streams: !0, freq: !0, npatch: true, ..Default::default() };
        for &r in PIXEL_RS.iter().chain(VERTEX_RS) {
            m.rs[r as usize / 64] |= 1 << (r % 64);
        }
        for t in m.tss.iter_mut() {
            *t = PIXEL_TSS.iter().chain(VERTEX_TSS).fold(0, |a, &s| a | 1 << s);
        }
        for s in m.ss.iter_mut() {
            *s = ALL_SS.iter().fold(0, |a, &x| a | 1 << x);
        }
        (m.indices, m.decl, m.vs, m.ps, m.viewport, m.scissor, m.material) = (true, true, true, true, true, true, true);
        m.vsf = vec![!0; st.vsf.len().div_ceil(64)];
        m.psf = vec![!0; st.psf.len().div_ceil(64)];
        (m.vsi, m.psi, m.vsb, m.psb) = (!0, !0, !0, !0);
        (m.vsi_more, m.vsb_more) = (vec![!0; st.vsi_more.len().div_ceil(64)], vec![!0; st.vsb_more.len().div_ceil(64)]);
        m.every_transform = true;
        m.lights = st.lights.keys().copied().collect();
        m.clip = !0;
        m
    }
    pub fn pixel(st: &State) -> Mask {
        let mut m = Mask::default();
        for &r in PIXEL_RS {
            m.rs[r as usize / 64] |= 1 << (r % 64);
        }
        for t in m.tss.iter_mut() {
            *t = PIXEL_TSS.iter().fold(0, |a, &s| a | 1 << s);
        }
        for s in m.ss.iter_mut() {
            *s = PIXEL_SS.iter().fold(0, |a, &x| a | 1 << x);
        }
        m.ps = true;
        m.psf = vec![!0; st.psf.len().div_ceil(64)];
        (m.psi, m.psb) = (!0, !0);
        m
    }
    pub fn vertex(st: &State) -> Mask {
        let mut m = Mask::default();
        for &r in VERTEX_RS {
            m.rs[r as usize / 64] |= 1 << (r % 64);
        }
        for t in m.tss.iter_mut() {
            *t = VERTEX_TSS.iter().fold(0, |a, &s| a | 1 << s);
        }
        for s in m.ss.iter_mut() {
            *s = VERTEX_SS.iter().fold(0, |a, &x| a | 1 << x);
        }
        (m.vs, m.decl, m.freq, m.npatch) = (true, true, !0, true);
        m.vsf = vec![!0; st.vsf.len().div_ceil(64)];
        (m.vsi, m.vsb) = (!0, !0);
        (m.vsi_more, m.vsb_more) = (vec![!0; st.vsi_more.len().div_ceil(64)], vec![!0; st.vsb_more.len().div_ceil(64)]);
        m.lights = st.lights.keys().copied().collect();
        m
    }

    /// What a Set* call (device method `m` with arguments `a`) changes, added while recording.
    pub fn record(&mut self, m: u32, a: &dyn Fn(usize) -> u32) {
        let range = |start: u32, n: u32| start..start.saturating_add(n);
        match m {
            44 | 46 => {
                self.transforms.insert(a(1));
            }
            47 => self.viewport = true,
            49 => self.material = true,
            51 | 53 => {
                self.lights.insert(a(1));
            }
            55 if a(1) < 6 => self.clip |= 1 << a(1),
            57 if a(1) < 256 => self.rs[a(1) as usize / 64] |= 1 << (a(1) % 64),
            65 => {
                if let Some(s) = super::device::sampler_slot(a(1)) {
                    self.textures |= 1 << s;
                }
            }
            67 if a(1) < 8 && a(2) < 33 => self.tss[a(1) as usize] |= 1 << a(2),
            69 if a(2) < 14 => {
                if let Some(s) = super::device::sampler_slot(a(1)) {
                    self.ss[s] |= 1 << a(2);
                }
            }
            79 => self.npatch = true,
            75 => self.scissor = true,
            87 | 89 => self.decl = true,
            92 => self.vs = true,
            107 => self.ps = true,
            94 | 109 => {
                // (software vertex processing has 8192 float registers; a range past them is refused)
                for r in range(a(1), a(3)).take(8192) {
                    set_bit(if m == 94 { &mut self.vsf } else { &mut self.psf }, r as usize);
                }
            }
            96 | 111 | 98 | 113 => {
                for r in range(a(1), a(3)).take(2048) {
                    if r >= 16 {
                        match m {
                            96 => set_bit(&mut self.vsi_more, r as usize - 16),
                            98 => set_bit(&mut self.vsb_more, r as usize - 16),
                            _ => {}
                        }
                        continue;
                    }
                    *match m {
                        96 => &mut self.vsi,
                        111 => &mut self.psi,
                        98 => &mut self.vsb,
                        _ => &mut self.psb,
                    } |= 1 << r;
                }
            }
            100 if a(1) < 16 => self.streams |= 1 << a(1),
            102 if a(1) < 16 => self.freq |= 1 << a(1),
            104 => self.indices = true,
            _ => {}
        }
    }
}

/// Whether device method `m` is a Set* call a recording takes.
pub fn records(m: u32) -> bool {
    matches!(m, 44 | 46 | 47 | 49 | 51 | 53 | 55 | 57 | 65 | 67 | 69 | 75 | 79 | 87 | 89 | 92 | 94 | 96 | 98 | 100 | 102 | 104 | 107 | 109 | 111 | 113)
}

/// `dst`'s masked states = `src`'s (the bindings: the caller counts them).
pub fn copy_masked(dst: &mut State, src: &State, m: &Mask) {
    // (D3DSBT_ALL blocks are captured and applied hundreds of times a frame: Siglus around
    // every draw; whole arrays at once there)
    for (w, &bits) in m.rs.iter().enumerate() {
        let mut b = bits;
        while b != 0 {
            let i = w * 64 + b.trailing_zeros() as usize;
            b &= b - 1;
            if let (Some(d), Some(s)) = (dst.rs.get_mut(i), src.rs.get(i)) {
                *d = *s;
            }
        }
    }
    for s in 0..8 {
        for t in 0..33 {
            if m.tss[s] >> t & 1 != 0 {
                dst.tss[s][t] = src.tss[s][t];
            }
        }
    }
    for s in 0..SAMPLERS {
        for t in 0..14 {
            if m.ss[s] >> t & 1 != 0 {
                dst.ss[s][t] = src.ss[s][t];
            }
        }
        if m.textures >> s & 1 != 0 {
            dst.textures[s] = src.textures[s];
        }
    }
    for s in 0..16 {
        if m.streams >> s & 1 != 0 {
            dst.streams[s] = src.streams[s];
        }
        if m.freq >> s & 1 != 0 {
            dst.freq[s] = src.freq[s];
        }
        for (mask, d, sv) in [(m.vsi, &mut dst.vsi, &src.vsi), (m.psi, &mut dst.psi, &src.psi)] {
            if mask >> s & 1 != 0 {
                d[s] = sv[s];
            }
        }
    }
    dst.vsb = dst.vsb & !(m.vsb as u32) | src.vsb & m.vsb as u32;
    dst.psb = dst.psb & !(m.psb as u32) | src.psb & m.psb as u32;
    for (i, d) in dst.vsi_more.iter_mut().enumerate().take(src.vsi_more.len()) {
        if bit(&m.vsi_more, i) {
            *d = src.vsi_more[i];
        }
    }
    for (i, d) in dst.vsb_more.iter_mut().enumerate().take(src.vsb_more.len()) {
        if bit(&m.vsb_more, i) {
            *d = src.vsb_more[i];
        }
    }
    if m.indices {
        dst.indices = src.indices;
    }
    if m.decl {
        (dst.decl, dst.fvf) = (src.decl, src.fvf);
    }
    if m.vs {
        dst.vs = src.vs;
    }
    if m.ps {
        dst.ps = src.ps;
    }
    for (bits, d, s) in [(&m.vsf, &mut dst.vsf, &src.vsf), (&m.psf, &mut dst.psf, &src.psf)] {
        if bits.len() * 64 >= s.len() && bits.iter().all(|&w| w == !0) && d.len() == s.len() {
            d.copy_from_slice(s);
            continue;
        }
        for i in 0..d.len().min(s.len()) {
            if bit(bits, i) {
                d[i] = s[i];
            }
        }
    }
    if m.every_transform {
        dst.transforms.clone_from(&src.transforms);
    }
    for t in &m.transforms {
        match src.transforms.get(t) {
            Some(x) => {
                dst.transforms.insert(*t, *x);
            }
            None => {
                dst.transforms.remove(t);
            }
        }
    }
    if m.viewport {
        dst.viewport = src.viewport;
    }
    if m.scissor {
        dst.scissor = src.scissor;
    }
    if m.material {
        dst.material = src.material;
    }
    for l in &m.lights {
        if let Some(x) = src.lights.get(l) {
            // (into the light's own bytes when it has them: no allocation per light per Apply)
            match dst.lights.get_mut(l) {
                Some(d) => {
                    d.0.clone_from(&x.0);
                    d.1 = x.1;
                }
                None => {
                    dst.lights.insert(*l, x.clone());
                }
            }
        }
    }
    for i in 0..6 {
        if m.clip >> i & 1 != 0 {
            dst.clip[i] = src.clip[i];
        }
    }
    if m.npatch {
        dst.npatch = src.npatch;
    }
}

/// Whether `copy_masked(dst, src, m)` would change `dst` (an Apply that changes nothing keeps
/// the draws prepared: Siglus applies a D3DSBT_ALL block around every draw). Floats compare
/// by value: a NaN counts as a change.
pub fn differs(dst: &State, src: &State, m: &Mask) -> bool {
    for (w, &bits) in m.rs.iter().enumerate() {
        let mut b = bits;
        while b != 0 {
            let i = w * 64 + b.trailing_zeros() as usize;
            b &= b - 1;
            if let (Some(d), Some(s)) = (dst.rs.get(i), src.rs.get(i)) {
                if d != s {
                    return true;
                }
            }
        }
    }
    for s in 0..8 {
        for t in 0..33 {
            if m.tss[s] >> t & 1 != 0 && dst.tss[s][t] != src.tss[s][t] {
                return true;
            }
        }
    }
    for s in 0..SAMPLERS {
        for t in 0..14 {
            if m.ss[s] >> t & 1 != 0 && dst.ss[s][t] != src.ss[s][t] {
                return true;
            }
        }
        if m.textures >> s & 1 != 0 && dst.textures[s] != src.textures[s] {
            return true;
        }
    }
    for s in 0..16 {
        if m.streams >> s & 1 != 0 && dst.streams[s] != src.streams[s]
            || m.freq >> s & 1 != 0 && dst.freq[s] != src.freq[s]
            || m.vsi >> s & 1 != 0 && dst.vsi[s] != src.vsi[s]
            || m.psi >> s & 1 != 0 && dst.psi[s] != src.psi[s]
        {
            return true;
        }
    }
    if (dst.vsb ^ src.vsb) & m.vsb as u32 != 0 || (dst.psb ^ src.psb) & m.psb as u32 != 0 {
        return true;
    }
    for (i, d) in dst.vsi_more.iter().enumerate().take(src.vsi_more.len()) {
        if bit(&m.vsi_more, i) && *d != src.vsi_more[i] {
            return true;
        }
    }
    for (i, d) in dst.vsb_more.iter().enumerate().take(src.vsb_more.len()) {
        if bit(&m.vsb_more, i) && *d != src.vsb_more[i] {
            return true;
        }
    }
    if m.indices && dst.indices != src.indices
        || m.decl && (dst.decl, dst.fvf) != (src.decl, src.fvf)
        || m.vs && dst.vs != src.vs
        || m.ps && dst.ps != src.ps
    {
        return true;
    }
    for (bits, d, s) in [(&m.vsf, &dst.vsf, &src.vsf), (&m.psf, &dst.psf, &src.psf)] {
        for i in 0..d.len().min(s.len()) {
            if bit(bits, i) && d[i] != s[i] {
                return true;
            }
        }
    }
    if m.every_transform && dst.transforms != src.transforms {
        return true;
    }
    for t in &m.transforms {
        if dst.transforms.get(t) != src.transforms.get(t) {
            return true;
        }
    }
    if m.viewport && dst.viewport != src.viewport || m.scissor && dst.scissor != src.scissor || m.material && dst.material != src.material {
        return true;
    }
    for l in &m.lights {
        if let Some(x) = src.lights.get(l) {
            if dst.lights.get(l) != Some(x) {
                return true;
            }
        }
    }
    for i in 0..6 {
        if m.clip >> i & 1 != 0 && dst.clip[i] != src.clip[i] {
            return true;
        }
    }
    m.npatch && dst.npatch != src.npatch
}

/// A block's own copy: the masked states of `st`, no objects outside the mask (so that the
/// block binds only what it holds).
pub fn masked_copy(st: &State, m: &Mask) -> State {
    let mut b = st.clone();
    for s in 0..SAMPLERS {
        if m.textures >> s & 1 == 0 {
            b.textures[s] = 0;
        }
    }
    for s in 0..16 {
        if m.streams >> s & 1 == 0 {
            b.streams[s] = (0, 0, 0);
        }
    }
    if !m.indices {
        b.indices = 0;
    }
    if !m.decl {
        b.decl = 0;
    }
    if !m.vs {
        b.vs = 0;
    }
    if !m.ps {
        b.ps = 0;
    }
    b
}

pub struct Block {
    pub st: State,
    pub mask: Mask,
}

/// A recording: the state the Set* calls change (a copy of the device's at BeginStateBlock,
/// binding what the device bound) and what they changed.
pub struct Recording {
    pub st: State,
    pub mask: Mask,
}

#[cfg(test)]
mod differs_tests {
    use super::super::device::default_state;
    use super::*;
    use proptest::prelude::*;

    /// Every field of two states, bit for bit (the reference `differs` is checked against)
    fn same(a: &State, b: &State) -> bool {
        let f4 = |x: &[[f32; 4]], y: &[[f32; 4]]| x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.map(f32::to_bits) == q.map(f32::to_bits));
        let mut ta: Vec<_> = a.transforms.iter().map(|(k, v)| (*k, v.map(f32::to_bits))).collect();
        let mut tb: Vec<_> = b.transforms.iter().map(|(k, v)| (*k, v.map(f32::to_bits))).collect();
        ta.sort_unstable();
        tb.sort_unstable();
        let mut la: Vec<_> = a.lights.iter().collect();
        let mut lb: Vec<_> = b.lights.iter().collect();
        la.sort_unstable_by_key(|x| *x.0);
        lb.sort_unstable_by_key(|x| *x.0);
        a.rs == b.rs && a.tss == b.tss && a.ss == b.ss && a.textures == b.textures && a.streams == b.streams && a.freq == b.freq
            && (a.indices, a.fvf, a.decl, a.vs, a.ps, a.vsb, a.psb, a.npatch) == (b.indices, b.fvf, b.decl, b.vs, b.ps, b.vsb, b.psb, b.npatch)
            && f4(&a.vsf, &b.vsf) && f4(&a.psf, &b.psf) && a.vsi == b.vsi && a.psi == b.psi && ta == tb
            && a.viewport == b.viewport && a.scissor == b.scissor && a.material.map(f32::to_bits) == b.material.map(f32::to_bits)
            && la == lb && f4(&a.clip, &b.clip) && a.vsi_more == b.vsi_more && a.vsb_more == b.vsb_more
    }

    /// A change of one field of `st` (field, index, value), the kinds Set* makes
    fn change(st: &mut State, (f, i, v): (u8, u16, u32)) {
        let i = i as usize;
        let x = (v % 5) as f32;
        match f % 16 {
            0 => st.rs[i % 256] = v % 4,
            1 => st.tss[i % 8][i % 33] = v % 4,
            2 => st.ss[i % SAMPLERS][i % 14] = v % 4,
            3 => st.textures[i % SAMPLERS] = v % 3,
            4 => st.streams[i % 16] = (v % 3, v % 2, 0),
            5 => st.freq[i % 16] = v % 3,
            6 => (st.indices, st.vs, st.ps) = (v % 2, v % 3, v % 2),
            7 => (st.decl, st.fvf) = (v % 2, v % 3),
            8 => {
                let n = st.vsf.len();
                st.vsf[i % n][i % 4] = x
            }
            9 => {
                let n = st.psf.len();
                st.psf[i % n][0] = x
            }
            10 => st.vsi[i % 16][0] = x as i32,
            11 => st.vsb ^= 1 << (i % 16),
            12 => drop(st.transforms.insert((i % 4) as u32 + 256 * (v % 2), [x; 16])),
            13 => drop(st.lights.insert((i % 4) as u32, (vec![v as u8; 104], v % 2 == 0))),
            14 => st.clip[i % 6][0] = x,
            _ => {
                if !st.vsi_more.is_empty() {
                    let (n, k) = (st.vsi_more.len(), st.vsb_more.len());
                    st.vsi_more[i % n][1] = x as i32;
                    st.vsb_more[i % k] = v % 2;
                }
                (st.viewport[0], st.scissor[1], st.material[2], st.npatch) = (v % 3, v as i32 % 3, x, v % 2);
            }
        }
    }

    proptest! {
        /// `differs` says whether `copy_masked` would change the state, for random states
        /// (hardware and software vertex processing) and every kind of mask: ALL, PIXEL,
        /// VERTEX and recorded ones.
        #[test]
        fn differs_is_whether_apply_changes(
            soft in any::<bool>(),
            base in prop::collection::vec((any::<u8>(), any::<u16>(), any::<u32>()), 0..20),
            dst_more in prop::collection::vec((any::<u8>(), any::<u16>(), any::<u32>()), 0..4),
            src_more in prop::collection::vec((any::<u8>(), any::<u16>(), any::<u32>()), 0..4),
            kind in 0u8..4,
            rec in prop::collection::vec((prop::sample::select(vec![44u32, 47, 49, 51, 53, 55, 57, 65, 67, 69, 75, 79, 87, 92, 94, 96, 98, 100, 102, 104, 107, 109, 111, 113]), 0u32..300, 1u32..40), 0..12),
        ) {
            let mut st = default_state();
            if soft {
                st.vsf.resize(8192, [0.0; 4]);
                st.vsi_more = vec![[0; 4]; 2048 - 16];
                st.vsb_more = vec![0; 2048 - 16];
            }
            for c in base {
                change(&mut st, c);
            }
            let (mut dst, mut src) = (st.clone(), st);
            for c in dst_more {
                change(&mut dst, c);
            }
            for c in src_more {
                change(&mut src, c);
            }
            let m = match kind {
                0 => Mask::all(&src),
                1 => Mask::pixel(&src),
                2 => Mask::vertex(&src),
                _ => {
                    let mut m = Mask::default();
                    for (meth, x, n) in rec {
                        m.record(meth, &|k| [0, x, x % 7, n][k.min(3)]);
                    }
                    m
                }
            };
            let mut after = dst.clone();
            copy_masked(&mut after, &src, &m);
            prop_assert_eq!(differs(&dst, &src, &m), !same(&after, &dst));
        }
    }
}
