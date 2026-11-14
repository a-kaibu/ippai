//! Properties of the specification itself (exhaustive where the domain is finite, property-based
//! where it is not): what must hold whatever Windows' measured details turn out to be. A model
//! change that breaks one is wrong whatever the answers say; cargo-mutants on src/model counts on
//! these together with the Windows answers to catch every mutant.
use proptest::prelude::*;
use wingfx::case::{parse_cases, parse_output, tokens};
use wingfx::model::d3d9::{all_states, Group};
use wingfx::model::gdi::{rop3, uses_d, uses_p, uses_s, Brush, Gdi, Surf};
use wingfx::model::{self, run_case};

/// the documented truth table, bit by bit, for all 256 operations and all 8 operand
/// combinations: a complete proof for the bitwise function (rop3 treats bits independently,
/// checked below on random words)
#[test]
fn rop3_is_the_truth_table() {
    for idx in 0..=255u8 {
        for k in 0..8u32 {
            let bit = |m: u32| if k & m != 0 { 1 } else { 0 };
            assert_eq!(rop3(idx, bit(4), bit(2), bit(1)) & 1, (idx as u32) >> k & 1, "op {idx:#x} operands {k:03b}");
        }
    }
    // the named operations are what windows.h says they compute
    let (p, s, d) = (0xf0f0_f0f0u32, 0xcccc_ccccu32, 0xaaaa_aaaau32);
    for (code, f) in [(0xcc, s), (0xee, s | d), (0x88, s & d), (0x66, s ^ d), (0x44, s & !d), (0x33, !s), (0x11, !(s | d)), (0xc0, p & s), (0xbb, !s | d), (0xf0, p), (0xfb, p | !s | d), (0x5a, p ^ d), (0x55, !d), (0x00, 0), (0xff, !0)] {
        assert_eq!(rop3(code, p, s, d), f, "{code:#x}");
    }
}

#[test]
fn operand_use_is_what_the_truth_table_reads() {
    for idx in 0..=255u8 {
        let flips = |which: u32| (0..8u32).any(|k| {
            let v = |m: u32| (k & m != 0) as u32;
            let (p, s, d) = (v(4), v(2), v(1));
            let (p2, s2, d2) = (p ^ (which >> 2 & 1), s ^ (which >> 1 & 1), d ^ (which & 1));
            rop3(idx, p, s, d) & 1 != rop3(idx, p2, s2, d2) & 1
        });
        assert_eq!(uses_p(idx), flips(4), "{idx:#x} P");
        assert_eq!(uses_s(idx), flips(2), "{idx:#x} S");
        assert_eq!(uses_d(idx), flips(1), "{idx:#x} D");
    }
}

proptest! {
    #[test]
    fn rop3_is_bitwise(idx: u8, p: u32, s: u32, d: u32, bit in 0u32..32) {
        let k = (p >> bit & 1) << 2 | (s >> bit & 1) << 1 | (d >> bit & 1);
        prop_assert_eq!(rop3(idx, p, s, d) >> bit & 1, (idx as u32) >> k & 1);
    }
}

const BPPS: &[(u32, bool)] = &[(1, false), (4, false), (8, false), (16, false), (16, true), (24, false), (32, false)];

fn surf(fmt: (u32, bool), w: i32, h: i32, seed: u32) -> Surf {
    let mut s = Surf::new(w, h, fmt.0, fmt.1, true, wingfx::model::gdi::default_palette(fmt.0));
    let mut r = wingfx::rng::XorShift32::new(seed);
    for row in s.rows.iter_mut() {
        for b in row.iter_mut() {
            *b = r.next() as u8;
        }
    }
    s
}

fn gdi_with(d: Surf, s: Surf, brush: Brush, org: (i32, i32), clip: Option<(i32, i32, i32, i32)>) -> Gdi {
    let mut g = Gdi::new();
    for (i, x) in [d, s].into_iter().enumerate() {
        g.slots[i] = Some(wingfx::model::gdi::Slot { s: x, text: 0, bk: 0xffffff, bkmode: 2, org, clip, brush: brush.clone(), mode: 1 });
    }
    g
}

fn arb_fmt() -> impl Strategy<Value = (u32, bool)> {
    prop::sample::select(BPPS)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// a color format's values survive the trip through a color (the palette formats: each
    /// table entry's color maps back to an entry of that color)
    #[test]
    fn colors_round_trip(fmt in arb_fmt(), v: u32) {
        let s = Surf::new(1, 1, fmt.0, fmt.1, true, wingfx::model::gdi::default_palette(fmt.0));
        let v = v & s.mask();
        if fmt.0 > 8 {
            let back = s.from_rgb(s.to_rgb(v));
            let m = if fmt.0 == 16 { 0x7fff | if fmt.1 { 0x8000 } else { 0 } } else { 0xffffff };
            prop_assert_eq!(back, v & m);
        } else {
            prop_assert_eq!(s.to_rgb(s.from_rgb(s.to_rgb(v))), s.to_rgb(v));
        }
    }

    /// BitBlt with an operation that ignores the source is PatBlt
    #[test]
    fn bitblt_without_source_is_patblt(fmt in arb_fmt(), sfmt in arb_fmt(), seed: u32, idx: u8, x in -3i32..10, y in -3i32..8, w in 0i32..12, h in 0i32..9, c in 0u32..0x1000000) {
        // (the 16 operations of P and D alone: bit p<<2|s<<1|d is bit p<<1|d of a 4-bit table)
        let t = idx & 15;
        let idx = (0..8u8).fold(0u8, |a, k| a | (t >> ((k >> 2 & 1) << 1 | (k & 1)) & 1) << k);
        prop_assert!(!uses_s(idx));
        let mut a = gdi_with(surf(fmt, 9, 6, seed), surf(sfmt, 5, 5, seed ^ 1), Brush::Solid(c), (0, 0), None);
        let mut b = a_clone(&a);
        a.blt(0, x, y, w, h, Some((1, 1, 1)), (idx as u32) << 16, false).unwrap();
        b.blt(0, x, y, w, h, None, (idx as u32) << 16, true).unwrap();
        prop_assert_eq!(&a.slots[0].as_ref().unwrap().s, &b.slots[0].as_ref().unwrap().s);
    }

    /// clipping only keeps pixels outside the clip rectangle as they were
    #[test]
    fn clipping_is_masking(fmt in arb_fmt(), seed: u32, idx: u8, x in -3i32..10, y in -3i32..8, w in 0i32..12, h in 0i32..9, cx in -2i32..10, cy in -2i32..7, cw in 0i32..12, ch in 0i32..9) {
        let clip = (cx, cy, cx + cw, cy + ch);
        let d = surf(fmt, 9, 6, seed);
        let mut clipped = gdi_with(d.clone(), surf(fmt, 9, 6, !seed), Brush::Solid(0x123456), (0, 0), Some(clip));
        let mut free = gdi_with(d.clone(), surf(fmt, 9, 6, !seed), Brush::Solid(0x123456), (0, 0), None);
        clipped.blt(0, x, y, w, h, Some((1, 2, 1)), (idx as u32) << 16, false).unwrap();
        free.blt(0, x, y, w, h, Some((1, 2, 1)), (idx as u32) << 16, false).unwrap();
        let (c, f) = (&clipped.slots[0].as_ref().unwrap().s, &free.slots[0].as_ref().unwrap().s);
        for py in 0..6 {
            for px in 0..9 {
                let inside = px >= clip.0 && px < clip.2 && py >= clip.1 && py < clip.3;
                prop_assert_eq!(c.get(px, py), if inside { f.get(px, py) } else { d.get(px, py) });
            }
        }
    }

    /// one blit is two blits of its halves (source and destination apart)
    #[test]
    fn blits_split(fmt in arb_fmt(), sfmt in arb_fmt(), seed: u32, idx: u8, x in -2i32..8, y in -2i32..6, w in 0i32..12, h in 0i32..8, cut in 0i32..12, sx in -2i32..6, sy in -2i32..6) {
        prop_assume!((fmt.0 == 1) == (sfmt.0 == 1));
        let cut = cut.min(w);
        let mk = || gdi_with(surf(fmt, 9, 6, seed), surf(sfmt, 10, 8, seed ^ 7), Brush::Pattern(surf((32, false), 3, 2, seed ^ 9)), (1, -1), None);
        let (mut one, mut two) = (mk(), mk());
        let rop = (idx as u32) << 16;
        one.blt(0, x, y, w, h, Some((1, sx, sy)), rop, false).unwrap();
        two.blt(0, x, y, cut, h, Some((1, sx, sy)), rop, false).unwrap();
        two.blt(0, x + cut, y, w - cut, h, Some((1, sx + cut, sy)), rop, false).unwrap();
        prop_assert_eq!(&one.slots[0].as_ref().unwrap().s, &two.slots[0].as_ref().unwrap().s);
    }

    /// DSTINVERT twice changes nothing; SRCCOPY between equal formats copies the values
    #[test]
    fn involution_and_copy(fmt in arb_fmt(), seed: u32, x in -3i32..9, y in -3i32..6, w in 0i32..12, h in 0i32..9) {
        let d = surf(fmt, 9, 6, seed);
        let mut g = gdi_with(d.clone(), surf(fmt, 9, 6, !seed), Brush::Solid(0), (0, 0), None);
        g.blt(0, x, y, w, h, None, 0x550009, true).unwrap();
        g.blt(0, x, y, w, h, None, 0x550009, true).unwrap();
        prop_assert_eq!(&g.slots[0].as_ref().unwrap().s, &d);
        g.blt(0, 0, 0, 9, 6, Some((1, 0, 0)), 0xcc0020, false).unwrap();
        let s = &g.slots[1].as_ref().unwrap().s;
        for py in 0..6 {
            for px in 0..9 {
                prop_assert_eq!(g.slots[0].as_ref().unwrap().s.get(px, py), s.get(px, py));
            }
        }
    }

    /// moving the brush origin by the pattern's size draws the same
    #[test]
    fn brush_origin_is_modular(fmt in arb_fmt(), seed: u32, ox in -9i32..9, oy in -9i32..9, pw in 1i32..9, ph in 1i32..9, idx: u8) {
        prop_assume!(fmt.0 != 1);
        let pat = surf((24, false), pw, ph, seed ^ 3);
        let mut a = gdi_with(surf(fmt, 9, 6, seed), surf(fmt, 9, 6, seed), Brush::Pattern(pat.clone()), (ox, oy), None);
        let mut b = gdi_with(surf(fmt, 9, 6, seed), surf(fmt, 9, 6, seed), Brush::Pattern(pat), (ox + pw, oy - 2 * ph), None);
        let rop = if uses_s(idx) { 0xf00021 } else { (idx as u32) << 16 };
        a.blt(0, -1, -1, 11, 8, None, rop, true).unwrap();
        b.blt(0, -1, -1, 11, 8, None, rop, true).unwrap();
        prop_assert_eq!(&a.slots[0].as_ref().unwrap().s, &b.slots[0].as_ref().unwrap().s);
    }

    /// whatever text comes in, parsing and the models never panic
    #[test]
    fn never_panics(text in "(case [a-z]{1,3}\n|end\n|(dib|fill|poke|dump|brush|patblt|bitblt|text|bk|org|clip|noclip|device|rs|tss|ss|sb)( (-?[0-9]{1,4}|0x[0-9a-f]{1,8}|#[0-9a-f]{6}|solid|dib|hatch|null|get|set|begin|end|create|apply|capture|release|td|bu|565)){0,9}\n){0,40}") {
        let cases = parse_cases(&text);
        for c in &cases {
            let _ = run_case(c);
        }
        let out = model::run("x", &cases);
        prop_assert_eq!(parse_output(&out).cases.len(), cases.len());
        for l in text.lines() {
            let _ = tokens(l);
        }
    }
}

fn a_clone(g: &Gdi) -> Gdi {
    let mut n = Gdi::new();
    n.slots = g.slots.clone();
    n
}

/// the state block types: pixel and vertex states together are every state but the ALL-only
/// ones, the two only meet in the states documented in both lists
#[test]
fn state_block_groups_partition() {
    let s = all_states();
    let all = s.len();
    let pixel = s.iter().filter(|x| matches!(x.2, Group::Pixel | Group::Both)).count();
    let vertex = s.iter().filter(|x| matches!(x.2, Group::Vertex | Group::Both)).count();
    let both = s.iter().filter(|x| x.2 == Group::Both).count();
    let only_all = s.iter().filter(|x| x.2 == Group::All).count();
    let nowhere = s.iter().filter(|x| x.2 == Group::Nowhere).count();
    assert_eq!(pixel + vertex - both + only_all + nowhere, all);
    // SHADEMODE, FOGSTART, FOGEND, FOGDENSITY; TEXCOORDINDEX, TEXTURETRANSFORMFLAGS of 8 stages
    assert_eq!(both, 4 + 16);
    // D3DTSS_CONSTANT of 8 stages
    assert_eq!(nowhere, 8);
    // DMAPOFFSET of 21 samplers
    assert_eq!(only_all, 21);
}

/// D3D9 state machine: a D3DSBT_ALL block made at any point and applied after any sets brings
/// back every state; capture right after apply changes nothing
#[test]
fn state_blocks_restore() {
    use wingfx::case::Case;
    let mut r = wingfx::rng::XorShift32::new(5);
    for _ in 0..30 {
        let text = {
            let mut t = String::from("case x\ndevice\n");
            let st = all_states();
            for _ in 0..r.range(0, 20) {
                if let wingfx::model::d3d9::Key::Rs(i) = r.pick(&st).0 {
                    t += &format!("rs set {i} {}\n", r.next() % 1000);
                }
            }
            t += "sb create 0 1\n";
            for _ in 0..r.range(0, 40) {
                if let wingfx::model::d3d9::Key::Rs(i) = r.pick(&st).0 {
                    t += &format!("rs set {i} {}\n", r.next() % 1000);
                }
            }
            t
        };
        let get_all: String = (0..256).map(|i| format!("rs get {i}\n")).collect();
        let before = parse_cases(&format!("{}{get_all}", text.split("sb create 0 1").next().unwrap()));
        let after = parse_cases(&format!("{text}sb apply 0\nsb capture 0\nsb apply 0\n{get_all}"));
        let a: Case = before[0].clone();
        let b: Case = after[0].clone();
        let (x, y) = (run_case(&a).unwrap(), run_case(&b).unwrap());
        assert_eq!(x[1..], y[1..]);
    }
}
