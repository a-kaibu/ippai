//! A game's own libvorbis (linked into its exe or a DLL, in x86) decodes its audio packets in
//! Rust instead (crates/vorbis): the packet decoding step, mapping0_inverse (floor curves,
//! residues, inverse coupling, IMDCT: most of Vorbis's cost, and x87 code that is slow
//! through the JIT). The rest of libvorbis (headers, windows, overlap-add, PCM out) stays the
//! game's, so its structures stay right for whatever else the game does with them.
//!
//! Finding it: libvorbis is built anew in every game (no two of four CatSystem2 exes had the
//! same bytes), so it is found through data every build has:
//!   - floor 1's dB table (1 KB of floats) is used by floor1_inverse2 only, and that function
//!     is entry 6 of floor 1's function table (vorbis_func_floor, 7 pointers);
//!   - _floor_P (the floor tables by type) points at that table; mapping0_inverse uses
//!     _floor_P and is entry 4 of mapping 0's table (vorbis_func_mapping, 5 pointers);
//!   - vorbis_synthesis_headerin compares a packet with the string "vorbis".
//! The functions found are patched to jump here, and every call is checked: headerin's packet
//! must be a Vorbis header (its bytes are kept, per vorbis_info), mapping0_inverse's block
//! must belong to a stream whose headers were seen, with the mode and block size they give.
//! Anything else goes on into the game's own code (a trampoline), as do streams this decoder
//! does not take (floor 0).
//!
//! Old libvorbis (1.0.x) also windows the block in mapping0_inverse; which a build does is
//! learnt on its first audible packets by running its own code alongside (see Shape).
//!
//! SAKE_NO_VORBIS: leave the game's decoder alone. SAKE_VORBIS_CHECK: run both, keep the
//! game's samples, and print how far apart they are (every 500 packets).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::api::{self, Ctx};
use crate::mem::{self, r32, w32};
use koji::iced_x86::{Decoder, DecoderOptions, FlowControl, Register};
use vorbis::decode::{self, State};
use vorbis::{Bits, Ident, Setup, Window};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Arguments in registers: whole-program optimization moves a direct callee's first
    /// argument to ecx (vi; vc and op on the stack); a __fastcall build (KiriKiri's
    /// wuvorbis.dll) has vi in ecx, vc in edx, op on the stack.
    Headerin { ecx: bool, edx: bool },
    /// __fastcall: vb in ecx, the look in edx
    Mapping { fastcall: bool },
}

/// trap address -> (what was patched, its trampoline)
static HOOKS: Mutex<Option<HashMap<u32, (Kind, u32)>>> = Mutex::new(None);

struct Stream {
    dec: Option<Arc<Mutex<(Arc<Setup>, State, Window)>>>,
    /// the codec_setup's first bytes when its first block came: its sizes, counts and first
    /// mode's address. A codec_setup freed and allocated again for a stream whose headers did
    /// not come through headerin (KiriKiri keeps decoders' vorbis_info) must not pass for it.
    seen: Option<[u8; 36]>,
    /// when its headers came (the oldest go first past MAX_STREAMS)
    born: u64,
}

/// streams kept (a game opens a stream per voice line and frees it, unseen by us)
const MAX_STREAMS: usize = 64;

/// vorbis_info's codec_setup -> its stream
static STREAMS: Mutex<Option<HashMap<u32, Stream>>> = Mutex::new(None);
/// (ident, setup) headers -> the setup read from them (a game's voices share theirs)
static SETUPS: Mutex<Option<HashMap<(Vec<u8>, Vec<u8>), Arc<Setup>>>> = Mutex::new(None);

/// [a, a + n) is committed guest memory.
fn readable(a: u32, n: u32) -> bool {
    if a < 0x1_0000 {
        return false;
    }
    let Some(end) = a.checked_add(n) else { return false };
    let mut p = a;
    loop {
        let Some((_, _, start, len, state, _)) = mem::query(p) else { return false };
        if state != 0x1000 {
            return false;
        }
        let run_end = start.saturating_add(len);
        if run_end >= end {
            return true;
        }
        p = run_end;
    }
}

struct Image<'a> {
    base: u32,
    img: &'a [u8],
    /// [start, end) offsets of code and of data sections
    code: Vec<(usize, usize)>,
    data: Vec<(usize, usize)>,
}

impl Image<'_> {
    fn new(base: u32, size: u32) -> Option<Image<'static>> {
        Image::of(base, unsafe { std::slice::from_raw_parts(base as usize as *const u8, size as usize) })
    }

    /// A mapped image's sections, from its headers.
    fn of(base: u32, img: &[u8]) -> Option<Image<'_>> {
        let rd = |o: usize| img.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let pe = rd(0x3c)? as usize;
        let nsec = u16::from_le_bytes(img.get(pe + 6..pe + 8)?.try_into().ok()?) as usize;
        let opt = u16::from_le_bytes(img.get(pe + 20..pe + 22)?.try_into().ok()?) as usize;
        // the resources' section (data directory 2): bytes, not pointers
        let rsrc = rd(pe + 24 + 112)? as usize;
        let (mut code, mut data) = (Vec::new(), Vec::new());
        for i in 0..nsec {
            let s = pe + 24 + opt + 40 * i;
            let (vsize, va, raw, ch) = (rd(s + 8)? as usize, rd(s + 12)? as usize, rd(s + 16)? as usize, rd(s + 36)?);
            let end = (va + vsize.max(raw)).min(img.len());
            if va >= end {
                continue;
            }
            if ch & 0x2000_0020 != 0 {
                code.push((va, end));
            } else if ch & 0x0200_0000 == 0 && !(va..end).contains(&rsrc) {
                // (not discardable ones: .reloc, a MinGW build's DWARF sections; nor resources)
                data.push((va, end));
            }
        }
        Some(Image { base, img, code, data })
    }

    fn in_code(&self, a: u32) -> bool {
        let o = a.wrapping_sub(self.base) as usize;
        self.code.iter().any(|&(s, e)| (s..e).contains(&o))
    }

    /// The 4 bytes at `o` (0 past the image's end: a damaged or packed module, not an abort).
    fn u32_at(&self, o: usize) -> u32 {
        self.img.get(o..o.saturating_add(4)).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    /// Addresses in `sections` holding the 4 bytes of `v` (any alignment).
    fn refs(&self, v: u32, sections: &[(usize, usize)]) -> Vec<u32> {
        let k = v.to_le_bytes();
        let mut out = Vec::new();
        for &(s, e) in sections {
            let mut o = s;
            while o + 4 <= e {
                match find_at(&self.img[o..e], &k) {
                    Some(p) => {
                        out.push(self.base + (o + p) as u32);
                        o += p + 1;
                    }
                    None => break,
                }
            }
        }
        out
    }

    fn find(&self, bytes: &[u8], sections: &[(usize, usize)]) -> Vec<u32> {
        let mut out = Vec::new();
        for &(s, e) in sections {
            let mut o = s;
            while o + bytes.len() <= e {
                match find_at(&self.img[o..e], bytes) {
                    Some(p) => {
                        out.push(self.base + (o + p) as u32);
                        o += p + 1;
                    }
                    None => break,
                }
            }
        }
        out
    }
}

/// Where `n` first is in `h`: windows().position with a scan for the first byte before each
/// compare (a 30-60 MB exe is scanned for the 1 KB dB table at every module load)
pub(crate) fn find_at(h: &[u8], n: &[u8]) -> Option<usize> {
    let (&first, _) = n.split_first()?;
    let last = h.len().checked_sub(n.len())?;
    let mut i = 0;
    while i <= last {
        let p = h[i..=last].iter().position(|&b| b == first)?;
        if &h[i + p..i + p + n.len()] == n {
            return Some(i + p);
        }
        i += p + 1;
    }
    None
}

/// `locate` on a module image mapped at `base` (fuzz/fuzz_targets/vorbis_hook.rs).
#[doc(hidden)]
pub fn locate_in(base: u32, img: &[u8]) -> Option<(Vec<u32>, u32)> {
    Image::of(base, img).and_then(|m| locate(&m))
}

/// The functions to patch in a module: (headerin candidates, mapping0_inverse).
fn locate(m: &Image) -> Option<(Vec<u32>, u32)> {
    let table: Vec<u8> = vorbis::floor1_table::INVERSE_DB.iter().flat_map(|f| f.to_le_bytes()).collect();
    // (a game may hold two copies: libvorbis's and another decoder's)
    let lut_refs: Vec<u32> = [m.find(&table, &m.data), m.find(&table, &m.code)].concat().into_iter().flat_map(|l| m.refs(l, &m.code)).collect();
    // (a build may compute the table instead: KiriKiri's wuvorbis.dll. Then a module that has
    // the string is searched through the registry's tables, below)
    if lut_refs.is_empty() && m.find(b"vorbis\0", &m.data).is_empty() {
        return None;
    }
    // every call target: functions
    let mut calls = Vec::new();
    for &(s, e) in &m.code {
        for o in s..e.saturating_sub(5) {
            if m.img[o] == 0xe8 {
                let t = (m.base + o as u32 + 5).wrapping_add(m.u32_at(o + 1));
                if m.in_code(t) {
                    calls.push(t);
                }
            }
        }
    }
    calls.sort_unstable();
    calls.dedup();
    // every aligned dword of data pointing into code: function table entries
    let mut ptrs: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(s, e) in &m.data {
        for o in (s..e.saturating_sub(3)).step_by(4) {
            let v = m.u32_at(o);
            if m.in_code(v) {
                ptrs.entry(v).or_default().push(m.base + o as u32);
            }
        }
    }
    let mut starts: Vec<u32> = ptrs.keys().copied().collect();
    starts.sort_unstable();
    // a function's start, not a switch case's (tables point into functions too): after
    // padding or a return, or 16-aligned, or a frame's set-up (push ebp; mov ebp, esp: Borland
    // packs functions with constants between them)
    let is_start = |f: u32| {
        let o = (f - m.base) as usize;
        f % 16 == 0 || matches!(m.img.get(o.wrapping_sub(1)), Some(0xcc | 0x90 | 0xc3)) || m.img.get(o..o + 3) == Some(&[0x55, 0x8b, 0xec][..])
    };
    // the functions that may hold `a`: starts a little before it (the tables below sort out
    // which)
    let funcs_of = |a: u32| -> Vec<u32> {
        let i = starts.partition_point(|&s| s <= a);
        starts[..i].iter().rev().take_while(|&&f| a - f < 0x2000).copied().filter(|&f| is_start(f)).collect()
    };
    // a function's code: up to the next function (a call target, or a start in a table)
    let body = |f: u32| -> (usize, usize) {
        let i = starts.partition_point(|&s| s <= f);
        let j = calls.partition_point(|&s| s <= f);
        let next_t = starts[i..].iter().copied().find(|&s| is_start(s)).unwrap_or(u32::MAX);
        let end = next_t.min(calls.get(j).copied().unwrap_or(u32::MAX)).min(f + 0x2000);
        ((f - m.base) as usize, ((end - m.base) as usize).min(m.img.len()))
    };
    let uses = |f: u32, v: u32| -> bool {
        let (s, e) = body(f);
        m.img[s..e].windows(4).any(|w| w == v.to_le_bytes())
    };
    // a table of `n` functions; `null`: a bit for each entry that may be 0 (an encoder's
    // functions in a build without it)
    let table_or_null = |at: u32, n: usize, null: u32| -> Option<Vec<u32>> {
        let o = at.checked_sub(m.base)? as usize;
        (0..n).map(|k| m.img.get(o + 4 * k..o + 4 * k + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).filter(|&v| (m.in_code(v) && is_start(v)) || (v == 0 && null >> k & 1 != 0))).collect()
    };
    let table_at = |at: u32, n: usize| table_or_null(at, n, 0);
    // floor 1's table: entry 6 is the function using the dB table
    let mut floor1 = Vec::new();
    for &r in &lut_refs {
        for f in funcs_of(r) {
            for &loc in ptrs.get(&f).map_or(&[][..], |v| v.as_slice()) {
                if table_at(loc - 24, 7).is_some_and(|t| t[6] == f) {
                    floor1.push(loc - 24);
                }
            }
        }
    }
    floor1.sort_unstable();
    floor1.dedup();
    // a residue table (vorbis_func_residue): unpack, look, free_info, free_look and inverse
    // (entries 1-4, 7) always there, the encoder's (pack, class, forward) may be null
    let residue_table = |t: u32| -> bool {
        let Some(o) = t.checked_sub(m.base).map(|o| o as usize).filter(|&o| o + 32 <= m.img.len()) else { return false };
        (0..8).all(|k| {
            let v = m.u32_at(o + 4 * k);
            (m.in_code(v) && is_start(v)) || (v == 0 && matches!(k, 0 | 5 | 6))
        })
    };
    // _residue_P: three pointers to residue tables
    let mut residue_p = Vec::new();
    for &(s, e) in &m.data {
        for o in (s..e.saturating_sub(11)).step_by(4) {
            let at = m.base + o as u32;
            if (0..3).all(|k| {
                let t = m.u32_at(o + 4 * k);
                !m.in_code(t) && residue_table(t)
            }) {
                residue_p.push(at);
            }
        }
    }
    // no dB table: _floor_P is just before _residue_P (registry.c), its entry 1 a floor table
    if lut_refs.is_empty() {
        for &r in &residue_p {
            let t = m.u32_at((r - 4 - m.base) as usize);
            if !m.in_code(t) && table_or_null(t, 7, 1).is_some() && table_or_null(m.u32_at((r - 8 - m.base) as usize), 7, 1).is_some() {
                floor1.push(t);
            }
        }
        if floor1.is_empty() {
            return None;
        }
    }
    // _floor_P points at floor 1's table (entry 1); mapping0_inverse is entry 4 of mapping 0's
    // table and uses both _floor_P and _residue_P
    let (mut mapping, mut old_mapping) = (Vec::new(), Vec::new());
    for &fb in &floor1 {
        for p in m.refs(fb, &m.data) {
            let floor_p = p - 4;
            for u in [m.refs(floor_p, &m.code), m.refs(p, &m.code)].concat() {
                for f in funcs_of(u) {
                    if !(uses(f, floor_p) || uses(f, p)) || !residue_p.iter().any(|&r| uses(f, r)) {
                        continue;
                    }
                    for &loc in ptrs.get(&f).map_or(&[][..], |v| v.as_slice()) {
                        // (pack and forward null without the encoder)
                        if table_or_null(loc - 16, 5, 0b1001).is_some_and(|t| t[4] == f) {
                            mapping.push(f);
                        }
                        // libvorbis 1.0 betas: an 8-entry table (pack, unpack, look, copy_info,
                        // free_info, free_look, forward, inverse) whose look uses them
                        // (only when there is no 1.0-and-later table)
                        if let Some(t) = table_at(loc - 8, 8).filter(|t| t[2] == f) {
                            old_mapping.push(t[7]);
                        }
                    }
                }
            }
        }
    }
    if mapping.is_empty() {
        mapping = old_mapping;
    }
    mapping.sort_unstable();
    mapping.dedup();
    // a build with the encoder: mapping0_forward (entry 3, just before inverse) passes too
    // (a beta table's entry need not be a key of `ptrs`, nor have 4 bytes after it in the image:
    // a HashMap index or a read past the image aborted the load)
    let next_is_candidate = |f: &u32| {
        ptrs.get(f).is_some_and(|locs| {
            locs.iter().any(|&loc| {
                let x = loc.wrapping_add(4).wrapping_sub(m.base) as usize;
                m.img.get(x..x.saturating_add(4)).is_some_and(|b| b.len() == 4 && mapping.contains(&m.u32_at(x)))
            })
        })
    };
    let forward: Vec<u32> = mapping.iter().copied().filter(next_is_candidate).collect();
    mapping.retain(|f| !forward.contains(f));
    if mapping.len() != 1 {
        if !mapping.is_empty() || !floor1.is_empty() {
            eprintln!("sake: vorbis: libvorbis at {:#x}? {} floor tables, mapping0_inverse candidates {mapping:x?}: left alone", m.base, floor1.len());
        }
        return None;
    }
    // headerin: called directly (no table), so its start is the nearest call target before
    // the string (or newer compilers' inlined memcmp: cmp dword [r], 'vorb')
    let mut refs: Vec<u32> = m.find(b"vorbis\0", &m.data).into_iter().flat_map(|s| m.refs(s, &m.code)).collect();
    refs.extend(m.refs(u32::from_le_bytes(*b"vorb"), &m.code));
    let mut headerin = Vec::new();
    for r in refs {
        let i = calls.partition_point(|&c| c <= r);
        if i > 0 && r - calls[i - 1] < 0x400 {
            headerin.push(calls[i - 1]);
        }
    }
    headerin.sort_unstable();
    headerin.dedup();
    if headerin.is_empty() {
        return None;
    }
    Some((headerin, mapping[0]))
}

/// Patches `f` to jump to our handler; the first whole instructions (5 bytes or more) go into
/// a trampoline that goes on at the rest. None if they cannot move (a branch among them).
fn hook(f: u32, kind: Kind) -> Option<()> {
    let code = unsafe { std::slice::from_raw_parts(f as usize as *const u8, 32) };
    let mut d = Decoder::with_ip(32, code, f as u64, DecoderOptions::NONE);
    let mut len = 0usize;
    while len < 5 {
        let ins = d.decode();
        if ins.is_invalid() || ins.flow_control() != FlowControl::Next || ins.is_ip_rel_memory_operand() {
            return None;
        }
        len += ins.len();
    }
    let mut tr = code[..len].to_vec();
    let t = api::stub(&vec![0; len + 5]);
    tr.push(0xe9);
    tr.extend((f + len as u32).wrapping_sub(t + len as u32 + 5).to_le_bytes());
    mem::write(t, &tr);
    let name = format!("vorbis_{}_{f:x}", if matches!(kind, Kind::Mapping { .. }) { "mapping0_inverse" } else { "headerin" });
    let h = api::host_fn(&name, handler, 0);
    HOOKS.lock().unwrap().get_or_insert_with(HashMap::new).insert(h, (kind, t));
    let mut jmp = vec![0xe9];
    jmp.extend(h.wrapping_sub(f + 5).to_le_bytes());
    mem::write(f, &jmp);
    Some(())
}

/// Takes over the libvorbis of a module just loaded at `base` (`size` bytes), if it has one.
pub fn patch(base: u32, size: u32) {
    if std::env::var_os("SAKE_NO_VORBIS").is_some() || mem::x64() {
        return;
    }
    let Some(m) = Image::new(base, size) else { return };
    let Some((headerin, mapping)) = locate(&m) else { return };
    let code = |f: u32| unsafe { std::slice::from_raw_parts(f as usize as *const u8, 96) };
    let fastcall = reads(code(mapping), Register::ECX) && reads(code(mapping), Register::EDX);
    if hook(mapping, Kind::Mapping { fastcall }).is_none() {
        eprintln!("sake: vorbis: mapping0_inverse at {mapping:#x} could not be patched");
        return;
    }
    let n = headerin.iter().filter(|&&f| hook(f, Kind::Headerin { ecx: reads(code(f), Register::ECX), edx: reads(code(f), Register::EDX) }).is_some()).count();
    eprintln!("sake: vorbis: decoding natively (mapping0_inverse {mapping:#x}, {n} header readers {headerin:x?})");
}

/// Whether a function (its code) reads `reg` before it sets it (in its straight first part): an
/// argument comes in it. (push ecx makes room on the stack, it does not count.)
fn reads(code: &[u8], reg: Register) -> bool {
    use koji::iced_x86::{InstructionInfoFactory, Mnemonic, OpAccess};
    let mut d = Decoder::new(32, code, DecoderOptions::NONE);
    let mut info = InstructionInfoFactory::new();
    for _ in 0..24 {
        let ins = d.decode();
        if ins.is_invalid() {
            return false;
        }
        if ins.mnemonic() == Mnemonic::Push && ins.op0_register() == Register::ECX {
            continue;
        }
        for r in info.info(&ins).used_registers() {
            if r.register().full_register32() != reg {
                continue;
            }
            match r.access() {
                OpAccess::Read | OpAccess::CondRead | OpAccess::ReadWrite | OpAccess::ReadCondWrite => return true,
                OpAccess::Write | OpAccess::CondWrite => return false,
                _ => {}
            }
        }
        if ins.flow_control() != FlowControl::Next {
            return false;
        }
    }
    false
}

fn on_to(c: &mut Ctx, t: u32) -> u32 {
    c.cpu.eip = t;
    c.jumped = true;
    0
}

fn handler(c: &mut Ctx) -> u32 {
    let Some((kind, t)) = HOOKS.lock().unwrap().as_ref().and_then(|h| h.get(&c.trap).copied()) else {
        // (cannot happen: every trap is registered with its entry)
        eprintln!("sake: vorbis hook without an entry at trap {:#x}", c.trap);
        return 0;
    };
    match kind {
        Kind::Headerin { ecx, edx } => {
            match (ecx, edx) {
                (true, true) => headerin(c.cpu.regs[1], c.a(0)),
                (true, false) => headerin(c.cpu.regs[1], c.a(1)),
                _ => headerin(c.a(0), c.a(2)),
            }
            on_to(c, t)
        }
        Kind::Mapping { fastcall } => match mapping(c, t, fastcall) {
            Some(r) => r,
            None => on_to(c, t),
        },
    }
}

/// vorbis_synthesis_headerin(vi, vc, op): keeps the identification and setup headers.
fn headerin(vi: u32, op: u32) {
    if !readable(op, 8) || !readable(vi, VI_SETUP[1] + 4) {
        return;
    }
    let (p, n) = (r32(op), r32(op + 4));
    if !(7..1 << 24).contains(&n) || !readable(p, n) {
        return;
    }
    let pkt = mem::slice(p, n);
    if &pkt[1..7] != b"vorbis" {
        return;
    }
    match pkt[0] {
        1 => {
            let mut g = IDENTS.lock().unwrap();
            let m = g.get_or_insert_with(HashMap::new);
            if m.len() >= MAX_STREAMS {
                m.clear();
            }
            m.insert(vi, pkt.to_vec());
        }
        5 => {
            let Some(ident) = IDENTS.lock().unwrap().as_mut().and_then(|m| m.remove(&vi)) else { return };
            let key = (ident, pkt.to_vec());
            let setup = {
                let mut c = SETUPS.lock().unwrap();
                let c = c.get_or_insert_with(HashMap::new);
                match c.get(&key) {
                    Some(s) => Some(s.clone()),
                    None => {
                        let r = Ident::read(&key.0).and_then(|i| Setup::read(i, &key.1));
                        match r {
                            Ok(s) => {
                                let s = Arc::new(s);
                                if c.len() > 64 {
                                    c.clear();
                                }
                                c.insert(key, s.clone());
                                Some(s)
                            }
                            Err(e) => {
                                eprintln!("sake: vorbis: a stream left to the game's decoder ({e:?})");
                                None
                            }
                        }
                    }
                }
            };
            let Some(setup) = setup else { return };
            // the codec_setup (its blocksizes are in from the identification header)
            let Some(cs) = codec_setup(vi, &setup) else {
                eprintln!("sake: vorbis: no codec_setup found for vorbis_info {vi:#x}: a stream left to the game's decoder");
                return;
            };
            let st = State::new(&setup);
            let win = Window::new(setup.ident.blocksize);
            let dec = Arc::new(Mutex::new((setup, st, win)));
            static BORN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let born = BORN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut g = STREAMS.lock().unwrap();
            let m = g.get_or_insert_with(HashMap::new);
            if m.len() >= MAX_STREAMS {
                // those whose codec_setup is gone or another's, then the oldest
                m.retain(|&k, s| s.seen.is_none_or(|seen| readable(k, 36) && mem::slice(k, 36) == seen));
                while m.len() >= MAX_STREAMS {
                    let oldest = *m.iter().min_by_key(|(_, s)| s.born).unwrap().0;
                    m.remove(&oldest);
                }
            }
            m.insert(cs, Stream { dec: Some(dec), seen: None, born });
        }
        _ => {}
    }
}

/// vorbis_block's fields (codec.h, 32-bit)
const VB_PCM: u32 = 0;
const VB_OPB: u32 = 4; // endbyte, endbit, buffer, ptr, storage
const VB_LW: u32 = 24;
const VB_W: u32 = 28;
const VB_NW: u32 = 32;
const VB_PCMEND: u32 = 36;
const VB_MODE: u32 = 40;
const VB_VD: u32 = 64;
/// vorbis_dsp_state's vi
const VD_VI: u32 = 4;
/// where vorbis_info's codec_setup may be: what streams are known by (KiriKiri's wuvorbis.dll
/// decodes with a copy of the vorbis_info its headers went into; the copy shares the
/// codec_setup). Its vorbis_info has a field more.
const VI_SETUP: [u32; 2] = [28, 32];

/// identification headers, by the vorbis_info they went into, until its setup header comes
static IDENTS: Mutex<Option<HashMap<u32, Vec<u8>>>> = Mutex::new(None);

/// The codec_setup of a vorbis_info whose identification header is in: its blocksizes are
/// that header's.
fn codec_setup(vi: u32, setup: &Setup) -> Option<u32> {
    VI_SETUP.iter().map(|&o| r32(vi + o)).find(|&cs| readable(cs, 8) && [r32(cs) as usize, r32(cs + 4) as usize] == setup.ident.blocksize)
}

/// What a build's mapping0_inverse leaves in vb->pcm: libvorbis 1.0.x windows the block there,
/// later versions leave that to vorbis_synthesis_blockin. Told apart on a build's first
/// packets by running its own code too.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Shape {
    /// not known yet (packets so far that matched neither)
    Unknown(u8),
    Raw,
    Windowed,
    /// in 16-bit units, not windowed, floor lines drawn in fixed point (KiriKiri's
    /// wuvorbis.dll)
    Scaled16,
    /// matches neither: the build is left alone
    Off,
}
/// trampoline (one per build) -> its shape
static SHAPES: Mutex<Option<HashMap<u32, Shape>>> = Mutex::new(None);

fn shape(tramp: u32) -> Shape {
    SHAPES.lock().unwrap().as_ref().and_then(|m| m.get(&tramp).copied()).unwrap_or(Shape::Unknown(0))
}

fn largest_diff(a: &[Vec<f32>], outs: &[u32]) -> (f32, f32) {
    let (mut worst, mut peak) = (0f32, 0f32);
    for (i, &p) in outs.iter().enumerate() {
        for (j, &ours) in a[i].iter().enumerate() {
            let theirs = f32::from_bits(r32(p + 4 * j as u32));
            worst = worst.max((ours - theirs).abs());
            peak = peak.max(theirs.abs());
        }
    }
    (worst, peak)
}

/// On a build whose shape is not known: runs its own code too and compares.
/// Runs the game's own mapping0_inverse.
fn original(c: &mut Ctx, tramp: u32, vb: u32, map: u32, fastcall: bool) -> u32 {
    if fastcall {
        c.cpu.regs[1] = vb;
        c.cpu.regs[2] = map;
        c.call(tramp, &[])
    } else {
        c.call(tramp, &[vb, map])
    }
}

fn learn(c: &mut Ctx, tramp: u32, vb: u32, map: u32, fastcall: bool, shapes: &[(Shape, Vec<Vec<f32>>)], outs: &[u32], misses: u8) -> u32 {
    let r = original(c, tramp, vb, map, fastcall);
    let peak = largest_diff(&shapes[0].1, outs).1;
    if peak < 1e-2 {
        // too quiet to tell
        return r;
    }
    let diffs: Vec<f32> = shapes.iter().map(|(_, b)| largest_diff(b, outs).0).collect();
    let s = match shapes.iter().zip(&diffs).find(|((_, _), &d)| d < 1e-4 * peak.max(1.0)) {
        Some(((s, _), _)) => *s,
        None if misses >= 3 => Shape::Off,
        None => Shape::Unknown(misses + 1),
    };
    match s {
        Shape::Unknown(_) => {}
        Shape::Off => eprintln!("sake: vorbis: decoder at {tramp:#x} left to the game (its samples match no shape: {diffs:?})"),
        _ => eprintln!("sake: vorbis: decoder at {tramp:#x} gives {s:?} blocks"),
    }
    SHAPES.lock().unwrap().get_or_insert_with(HashMap::new).insert(tramp, s);
    r
}

#[derive(Default)]
struct Check {
    packets: u64,
    bad: u64,
    worst: f32,
    peak: f32,
}
static CHECK: Mutex<Check> = Mutex::new(Check { packets: 0, bad: 0, worst: 0.0, peak: 0.0 });

/// A block left to the game's code; with SAKE_VORBIS_CHECK, says why (the first few times).
fn pass(why: &'static str) -> Option<u32> {
    if crate::env_on!("SAKE_VORBIS_CHECK") {
        static SEEN: Mutex<u32> = Mutex::new(0);
        let mut k = SEEN.lock().unwrap();
        *k += 1;
        if *k <= 8 {
            eprintln!("sake: vorbis: a block left to the game ({why})");
        }
    }
    None
}

/// mapping0_inverse(vb, map): the block's samples (before windowing) into vb->pcm. None: the
/// game's own code is to do it.
fn mapping(c: &mut Ctx, tramp: u32, fastcall: bool) -> Option<u32> {
    let (vb, map) = if fastcall { (c.cpu.regs[1], c.cpu.regs[2]) } else { (c.a(0), c.a(1)) };
    let sh = shape(tramp);
    if sh == Shape::Off || !readable(vb, VB_VD + 4) {
        return pass("off or vb unreadable");
    }
    let vd = r32(vb + VB_VD);
    if !readable(vd, 8) {
        return pass("vd unreadable");
    }
    let vi = r32(vd + VD_VI);
    if !readable(vi, VI_SETUP[1] + 4) {
        return pass("vi unreadable");
    }
    let dec = {
        let mut g = STREAMS.lock().unwrap();
        let Some(m) = g.as_mut() else { return pass("no headers for its vorbis_info") };
        let Some(key) = VI_SETUP.iter().map(|&o| r32(vi + o)).find(|k| m.contains_key(k)) else {
            return pass("no headers for its vorbis_info");
        };
        if !readable(key, 36) {
            return pass("codec_setup unreadable");
        }
        let now: [u8; 36] = mem::slice(key, 36).try_into().unwrap();
        let s = m.get_mut(&key).unwrap();
        let Some(dec) = s.dec.clone() else {
            return pass("no headers for its vorbis_info");
        };
        match s.seen {
            Some(seen) if seen != now => {
                // another stream's now
                s.dec = None;
                return pass("codec_setup used again");
            }
            Some(_) => {}
            None => s.seen = Some(now),
        }
        dec
    };
    let mut g = dec.lock().unwrap();
    let (setup, st, win) = &mut *g;
    let ch = setup.ident.channels;
    let mode = r32(vb + VB_MODE) as usize;
    let w = r32(vb + VB_W) as usize;
    if mode >= setup.modes.len() || setup.modes[mode].blockflag as usize != w {
        return pass("mode/blocksize");
    }
    let n = setup.ident.blocksize[w];
    let (endbyte, endbit, buffer, storage) = (r32(vb + VB_OPB), r32(vb + VB_OPB + 4), r32(vb + VB_OPB + 8), r32(vb + VB_OPB + 16));
    if endbit > 7 || endbyte > storage || storage > 1 << 24 || !readable(buffer, storage) {
        return pass("bit reader");
    }
    let pcm = r32(vb + VB_PCM);
    if !readable(pcm, 4 * ch as u32) {
        return pass("pcm unreadable");
    }
    let outs: Vec<u32> = (0..ch as u32).map(|i| r32(pcm + 4 * i)).collect();
    if !outs.iter().all(|&p| readable(p, 4 * n as u32)) {
        return pass("pcm buffers unreadable");
    }
    let mut decode_as = |down: bool| {
        st.lines_round_down = down;
        let mut bufs = vec![vec![0f32; n]; ch];
        let mut b = Bits::at(mem::slice(buffer, storage), endbyte as usize * 8 + endbit as usize);
        let mut v: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
        decode::mapping_inverse(setup, st, &mut b, mode, &mut v);
        bufs
    };
    let mut bufs = decode_as(sh == Shape::Scaled16);
    let scaled16 = |mut b: Vec<Vec<f32>>| {
        b.iter_mut().flatten().for_each(|v| *v *= 32768.0);
        b
    };
    let (lw, nw) = (r32(vb + VB_LW) as usize, r32(vb + VB_NW) as usize);
    let window = |bufs: &mut Vec<Vec<f32>>| {
        for b in bufs.iter_mut() {
            win.apply(b, lw, w, nw);
        }
    };
    match sh {
        Shape::Unknown(misses) => {
            let mut windowed = bufs.clone();
            window(&mut windowed);
            let fixed = scaled16(decode_as(true));
            drop(g);
            let shapes = [(Shape::Raw, bufs), (Shape::Windowed, windowed), (Shape::Scaled16, fixed)];
            return Some(learn(c, tramp, vb, map, fastcall, &shapes, &outs, misses));
        }
        Shape::Windowed => window(&mut bufs),
        Shape::Scaled16 => bufs = scaled16(bufs),
        _ => {}
    }
    // (differences in the stream's units)
    let unit = if sh == Shape::Scaled16 { 32768.0 } else { 1.0 };
    drop(g);
    if crate::env_on!("SAKE_VORBIS_CHECK") {
        // the game's own result stays
        let r = original(c, tramp, vb, map, fastcall);
        let mut k = CHECK.lock().unwrap();
        let mut bad = None;
        for (i, &p) in outs.iter().enumerate() {
            for (j, &ours) in bufs[i].iter().enumerate() {
                let theirs = f32::from_bits(r32(p + 4 * j as u32));
                let d = (ours - theirs).abs() / unit;
                if d > 1e-3 && bad.is_none() {
                    bad = Some((i, j, ours, theirs));
                }
                k.worst = k.worst.max(d);
                k.peak = k.peak.max(theirs.abs() / unit);
            }
        }
        if let Some((i, j, ours, theirs)) = bad {
            k.bad += 1;
            if k.bad <= 8 {
                let pkt = mem::slice(buffer, storage);
                let hex: String = pkt.iter().take(48).map(|b| format!("{b:02x}")).collect();
                eprintln!("sake: vorbis check: packet {} differs (mode {mode}, n {n}, {} bytes, from bit {}): channel {i} sample {j}: ours {ours} vs {theirs}; {hex}", k.packets, storage, endbyte * 8 + endbit);
            }
        }
        k.packets += 1;
        if k.packets % 500 == 0 {
            eprintln!("sake: vorbis check: {} packets ({} differ by over 1e-3), largest difference {:e} (largest sample {:e})", k.packets, k.bad, k.worst, k.peak);
        }
        return Some(r);
    }
    for (i, &p) in outs.iter().enumerate() {
        let bytes: Vec<u8> = bufs[i].iter().flat_map(|v| v.to_le_bytes()).collect();
        mem::write(p, &bytes);
    }
    w32(vb + VB_PCMEND, n as u32);
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecx_argument() {
        // Rance 01's headerin: sub esp,24h; mov eax,[cookie]; xor eax,esp; mov [esp+20h],eax;
        // push ebx; mov ebx,[esp+30h]; push ebp; mov ebp,[esp+30h]; push esi; push edi; mov esi,ecx
        let lto = hex("83ec24a174c47800 33c4894424205 38b5c2430558b6c243056578bf1");
        assert!(reads(&lto, Register::ECX));
        // cdecl: push esi; mov esi,[esp+8]; mov ecx,[esp+10h]; test ecx,ecx
        assert!(!reads(&hex("568b7424088b4c241085c9"), Register::ECX));
        // never touched before a branch
        assert!(!reads(&hex("5633c0eb00"), Register::ECX));
        // push ecx (room for a local) is not a use: push ecx; mov ecx, [esp+8]
        assert!(!reads(&hex("518b4c2408"), Register::ECX));
        // wuvorbis.dll's mapping0_inverse (__fastcall): push ebp; mov ebp,esp; sub esp,38h;
        // push ebx; mov ebx,ecx; push esi; push edi; mov eax,[ebx+40h]; mov edi,edx
        let fc = hex("558bec83ec38538bd956578b43408bfa");
        assert!(reads(&fc, Register::ECX) && reads(&fc, Register::EDX));
        assert!(!reads(&lto, Register::EDX));
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    /// A PE file laid out as the loader maps it.
    fn map(file: &[u8]) -> (u32, Vec<u8>) {
        let rd = |o: usize| u32::from_le_bytes(file[o..o + 4].try_into().unwrap());
        let pe = rd(0x3c) as usize;
        let nsec = u16::from_le_bytes(file[pe + 6..pe + 8].try_into().unwrap()) as usize;
        let opt = u16::from_le_bytes(file[pe + 20..pe + 22].try_into().unwrap()) as usize;
        let (base, size, hdr) = (rd(pe + 24 + 28), rd(pe + 24 + 56) as usize, rd(pe + 24 + 60) as usize);
        let mut img = vec![0u8; size];
        img[..hdr].copy_from_slice(&file[..hdr]);
        for i in 0..nsec {
            let s = pe + 24 + opt + 40 * i;
            let (va, raw, ptr) = (rd(s + 12) as usize, rd(s + 16) as usize, rd(s + 20) as usize);
            let n = raw.min(file.len().saturating_sub(ptr)).min(size - va);
            img[va..va + n].copy_from_slice(&file[ptr..ptr + n]);
        }
        (base, img)
    }

    /// Where libvorbis is in real games' exes (not in the repository):
    ///   SAKE_VORBIS_EXES="a.exe:b.exe" cargo test -p sake vorbis_hook -- --ignored --nocapture
    #[test]
    #[ignore]
    fn locate_in_games() {
        let list = std::env::var("SAKE_VORBIS_EXES").unwrap_or_default();
        for path in list.split(':').filter(|p| !p.is_empty()) {
            let (base, img) = map(&std::fs::read(path).unwrap());
            let found = Image::of(base, &img).and_then(|m| locate(&m));
            eprintln!("{path}: {found:x?}");
        }
    }

    /// A mapped image: PE header, a code section at 0x1000 and a data section at 0x2000.
    pub(super) fn image(code: &[u8], data: &[u8]) -> Vec<u8> {
        let mut img = vec![0u8; 0x1000 + 0x1000 + data.len().max(0x1000)];
        img[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        img[0x80..0x84].copy_from_slice(b"PE\0\0");
        img[0x86..0x88].copy_from_slice(&2u16.to_le_bytes());
        img[0x94..0x96].copy_from_slice(&0xe0u16.to_le_bytes());
        for (i, (va, n, ch)) in [(0x1000u32, 0x1000u32, 0x6000_0020u32), (0x2000, data.len().max(0x1000) as u32, 0xc000_0040)].into_iter().enumerate() {
            let s = 0x80 + 24 + 0xe0 + 40 * i;
            img[s + 8..s + 12].copy_from_slice(&n.to_le_bytes());
            img[s + 12..s + 16].copy_from_slice(&va.to_le_bytes());
            img[s + 16..s + 20].copy_from_slice(&n.to_le_bytes());
            img[s + 36..s + 40].copy_from_slice(&ch.to_le_bytes());
        }
        let c = code.len().min(0x1000);
        img[0x1000..0x1000 + c].copy_from_slice(&code[..c]);
        img[0x2000..0x2000 + data.len()].copy_from_slice(data);
        img
    }

    proptest::proptest! {
        /// find_at is windows().position, on small alphabets (many near matches)
        #[test]
        fn find_at_is_position(h in proptest::collection::vec(0u8..3, 0..300), n in proptest::collection::vec(0u8..3, 0..6)) {
            let want = if n.is_empty() { None } else { h.windows(n.len()).position(|w| w == &n[..]) };
            proptest::prop_assert_eq!(super::find_at(&h, &n), want);
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 300, ..Default::default() })]
        /// Any code and data around libvorbis's dB table and its "vorbis" string, with function
        /// tables pointing anywhere (into code, past the image, at the image's last bytes): the
        /// search never panics (a HashMap index and a read past the image aborted a load), and
        /// what it finds is in the code section.
        #[test]
        fn locate_never_panics(code in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..0x1000),
                               ptrs in proptest::collection::vec((0usize..0x400, 0u32..0x2400), 0..200),
                               at in 0usize..0x200, string_at in 0usize..0x40) {
            let table: Vec<u8> = vorbis::floor1_table::INVERSE_DB.iter().flat_map(|f| f.to_le_bytes()).collect();
            let mut data = vec![0u8; 0x1000];
            let t = at * 4;
            if t + table.len() <= data.len() {
                data[t..t + table.len()].copy_from_slice(&table);
            }
            let s0 = 0xf00 + string_at;
            data[s0..s0 + 7].copy_from_slice(b"vorbis\0");
            for (k, (slot, target)) in ptrs.into_iter().enumerate() {
                let o = (slot * 4) % 0xffc;
                let v = if k % 7 == 0 { 0x40_0000 + 0x2ffe } else { 0x40_0000 + 0x1000 + target };
                data[o..o + 4].copy_from_slice(&v.to_le_bytes());
            }
            let img = image(&code, &data);
            if let Some((heads, inv)) = locate_in(0x40_0000, &img) {
                proptest::prop_assert!((0x40_1000..0x40_2000).contains(&inv));
                for h in heads {
                    proptest::prop_assert!((0x40_1000..0x40_2000).contains(&h));
                }
            }
        }
    }
}
