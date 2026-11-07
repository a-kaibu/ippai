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
    7, 8, 9, 14, 15, 16, 19, 20, 23, 24, 25, 26, 27, 36, 37, 38, 40, 52, 53, 54, 55, 56, 57, 58,
    59, 60, 128, 129, 130, 131, 132, 133, 134, 135, 168, 171, 174, 175, 176, 185, 186, 187, 188,
    189, 190, 191, 192, 193, 194, 195, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209,
];
const VERTEX_RS: &[u32] = &[
    9, 22, 28, 29, 34, 35, 36, 37, 38, 48, 136, 137, 139, 140, 141, 142, 143, 145, 146, 147, 148,
    151, 152, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 166, 167, 170, 172, 173, 178, 179,
    180, 181, 182, 183, 184,
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
        let mut m = Mask {
            textures: !0,
            streams: !0,
            freq: !0,
            npatch: true,
            ..Default::default()
        };
        for &r in PIXEL_RS.iter().chain(VERTEX_RS) {
            m.rs[r as usize / 64] |= 1 << (r % 64);
        }
        for t in m.tss.iter_mut() {
            *t = PIXEL_TSS
                .iter()
                .chain(VERTEX_TSS)
                .fold(0, |a, &s| a | 1 << s);
        }
        for s in m.ss.iter_mut() {
            *s = ALL_SS.iter().fold(0, |a, &x| a | 1 << x);
        }
        (
            m.indices, m.decl, m.vs, m.ps, m.viewport, m.scissor, m.material,
        ) = (true, true, true, true, true, true, true);
        m.vsf = vec![!0; st.vsf.len().div_ceil(64)];
        m.psf = vec![!0; st.psf.len().div_ceil(64)];
        (m.vsi, m.psi, m.vsb, m.psb) = (!0, !0, !0, !0);
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
                for r in range(a(1), a(3)).take(256) {
                    set_bit(
                        if m == 94 {
                            &mut self.vsf
                        } else {
                            &mut self.psf
                        },
                        r as usize,
                    );
                }
            }
            96 | 111 | 98 | 113 => {
                for r in range(a(1), a(3)).filter(|&r| r < 16) {
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
    matches!(
        m,
        44 | 46
            | 47
            | 49
            | 51
            | 53
            | 55
            | 57
            | 65
            | 67
            | 69
            | 75
            | 79
            | 87
            | 89
            | 92
            | 94
            | 96
            | 98
            | 100
            | 102
            | 104
            | 107
            | 109
            | 111
            | 113
    )
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
        for (mask, d, sv) in [
            (m.vsi, &mut dst.vsi, &src.vsi),
            (m.psi, &mut dst.psi, &src.psi),
        ] {
            if mask >> s & 1 != 0 {
                d[s] = sv[s];
            }
        }
    }
    dst.vsb = dst.vsb & !(m.vsb as u32) | src.vsb & m.vsb as u32;
    dst.psb = dst.psb & !(m.psb as u32) | src.psb & m.psb as u32;
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
    for (bits, d, s) in [
        (&m.vsf, &mut dst.vsf, &src.vsf),
        (&m.psf, &mut dst.psf, &src.psf),
    ] {
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
            dst.lights.insert(*l, x.clone());
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
