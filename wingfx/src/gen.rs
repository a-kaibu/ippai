//! Case generators: the case files are made here, from fixed seeds (cases/<suite>.txt is what
//! `wingfx gen <suite>` prints; a test keeps them equal). Coverage comes from structure (every
//! raster operation on every pixel format, every state, every state block type) and from many
//! random cases around the edges (clipping, bitmap borders, overlap, odd sizes, both row orders).

use crate::model::d3d11::Kind;
use crate::model::d3d9::{all_states, all_states8, RENDER_STATES};
use crate::model::gdi::default_palette;
use crate::rng::XorShift32;
use std::fmt::Write;

pub const SUITES: &[&str] = &["gdi", "stretch", "pixel", "d3d8", "d3d9", "d3d11", "wic", "ddraw"];

pub fn suite(name: &str) -> Option<String> {
    match name {
        "gdi" => Some(gdi()),
        "stretch" => Some(stretch()),
        "pixel" => Some(pixel()),
        "d3d8" => Some(d3d8()),
        "d3d9" => Some(d3d9()),
        "d3d11" => Some(d3d11()),
        "wic" => Some(wic()),
        "ddraw" => Some(ddraw()),
        _ => None,
    }
}

/// the raster operation codes windows.h names
pub const NAMED_ROPS: &[(u32, &str)] = &[
    (0x00CC0020, "SRCCOPY"),
    (0x00EE0086, "SRCPAINT"),
    (0x008800C6, "SRCAND"),
    (0x00660046, "SRCINVERT"),
    (0x00440328, "SRCERASE"),
    (0x00330008, "NOTSRCCOPY"),
    (0x001100A6, "NOTSRCERASE"),
    (0x00C000CA, "MERGECOPY"),
    (0x00BB0226, "MERGEPAINT"),
    (0x00F00021, "PATCOPY"),
    (0x00FB0A09, "PATPAINT"),
    (0x005A0049, "PATINVERT"),
    (0x00550009, "DSTINVERT"),
    (0x00000042, "BLACKNESS"),
    (0x00FF0062, "WHITENESS"),
];

/// formats as `dib` arguments: (bpp, extra)
const FORMATS: &[(u32, &str)] = &[(1, ""), (4, ""), (8, ""), (16, ""), (16, " 565"), (24, ""), (32, "")];

fn fmt_name(f: &(u32, &str)) -> String {
    format!("{}{}", f.0, f.1.replace(' ', ""))
}

fn rcolor(r: &mut XorShift32, pal: &[u32]) -> u32 {
    // (exact palette colors and the extremes are where conversions round differently)
    match r.range(0, 5) {
        0 => *r.pick(&[0, 0xffffff, 0xff0000, 0x00ff00, 0x0000ff, 0x808080, 0x7f7f7f]),
        1 if !pal.is_empty() => *r.pick(pal),
        _ => r.next() & 0xffffff,
    }
}

fn dib_line(r: &mut XorShift32, slot: u32, w: i64, h: i64, f: &(u32, &str), custom_pal: bool) -> (String, Vec<u32>) {
    let order = if r.chance(3) { " bu" } else { "" };
    let mut s = format!("dib {slot} {w} {h} {}{order}{}", f.0, f.1);
    let mut pal = default_palette(f.0);
    if custom_pal && !pal.is_empty() {
        let n = pal.len().min(16);
        for e in pal.iter_mut().take(n) {
            *e = r.next() & 0xffffff;
            write!(s, " #{e:06x}").unwrap();
        }
    }
    (s, pal)
}

fn rop(r: &mut XorShift32) -> u32 {
    if r.chance(2) {
        r.pick(NAMED_ROPS).0
    } else {
        // any of the 256 operations, with an arbitrary low word (Windows reads bits 16..23)
        (r.next() & 0xff) << 16 | (r.next() & 0xffff)
    }
}

pub fn gdi() -> String {
    let mut s = String::from("# wingfx suite gdi: PatBlt and BitBlt into DIB sections (SPEC.md). Made by `wingfx gen gdi`.\n");
    // every ternary operation on every format, with a pattern brush so that P varies by pixel:
    // one 1x1 BitBlt per operation, its own pixel of a 16x16 picture
    for f in FORMATS {
        for brush in ["solid", "pattern"] {
            let mut r = XorShift32::new(0x5eed_0000 + f.0 * 7 + brush.len() as u32);
            writeln!(s, "case rop3-{}-{brush}", fmt_name(f)).unwrap();
            writeln!(s, "dib 0 16 16 {}{}", f.0, f.1).unwrap();
            writeln!(s, "fill 0 {}", r.next()).unwrap();
            writeln!(s, "dib 1 16 16 {}{}", f.0, f.1).unwrap();
            writeln!(s, "fill 1 {}", r.next()).unwrap();
            if brush == "solid" {
                writeln!(s, "brush 0 solid #{:06x}", r.next() & 0xffffff).unwrap();
            } else {
                writeln!(s, "dib 2 8 8 32").unwrap();
                writeln!(s, "fill 2 {}", r.next()).unwrap();
                writeln!(s, "brush 0 dib 2").unwrap();
            }
            for i in 0..256u32 {
                let (x, y) = (i % 16, i / 16);
                writeln!(s, "bitblt 0 {x} {y} 1 1 1 {x} {y} {:#x}", i << 16).unwrap();
            }
            writeln!(s, "dump 0\nend").unwrap();
        }
    }
    // named operations over whole pictures between every pair of formats
    let mut r = XorShift32::new(0x1234_5678);
    for d in FORMATS {
        for sf in FORMATS {
            if (d.0 == 1) != (sf.0 == 1) {
                continue; // (monochrome conversions: their own cases below)
            }
            writeln!(s, "case convert-{}-from-{}", fmt_name(d), fmt_name(sf)).unwrap();
            let (l, _) = dib_line(&mut r, 0, 7, 5, d, false);
            writeln!(s, "{l}\nfill 0 {}", r.next()).unwrap();
            let (l, _) = dib_line(&mut r, 1, 7, 5, sf, false);
            writeln!(s, "{l}\nfill 1 {}", r.next()).unwrap();
            writeln!(s, "bitblt 0 0 0 7 5 1 0 0 0xcc0020\ndump 0\nend").unwrap();
        }
    }
    // monochrome to and from color (the documented text/background color rules): unmodeled
    // until Windows' answers say how DIB sections with color tables follow them
    for (i, f) in FORMATS.iter().filter(|f| f.0 != 1).enumerate() {
        for dir in ["to", "from"] {
            writeln!(s, "case mono-{dir}-{}", fmt_name(f)).unwrap();
            let (a, b) = if dir == "to" { ((1, ""), *f) } else { (*f, (1, "")) };
            let (l, _) = dib_line(&mut r, 0, 9, 4, &a, i % 2 == 1);
            writeln!(s, "{l}\nfill 0 {}", r.next()).unwrap();
            let (l, pal) = dib_line(&mut r, 1, 9, 4, &b, false);
            writeln!(s, "{l}\nfill 1 {}", r.next()).unwrap();
            writeln!(s, "text 0 #{:06x}\nbk 0 #{:06x}", rcolor(&mut r, &[]), rcolor(&mut r, &[])).unwrap();
            writeln!(s, "text 1 #{:06x}\nbk 1 #{:06x}", rcolor(&mut r, &pal), rcolor(&mut r, &pal)).unwrap();
            // (a pixel equal to the background color, to see the color -> mono rule)
            if b.0 >= 8 {
                writeln!(s, "poke 1 2 1 {}", "ff".repeat(b.0 as usize / 8)).unwrap();
            }
            writeln!(s, "bitblt 0 0 0 9 4 1 0 0 0xcc0020\ndump 0\nend").unwrap();
        }
    }
    // random: formats, sizes, row orders, palettes, brushes, origins, clipping, borders, overlap
    let mut r = XorShift32::new(0xc0ff_ee00);
    for i in 0..400 {
        writeln!(s, "case random-{i}").unwrap();
        let df = *r.pick(FORMATS);
        let (w, h) = (r.range(1, 13), r.range(1, 7));
        let custom = r.chance(4);
        let (l, dpal) = dib_line(&mut r, 0, w, h, &df, custom);
        writeln!(s, "{l}\nfill 0 {}", r.next()).unwrap();
        let same = r.chance(5);
        let (sw, sh) = (r.range(1, 16), r.range(1, 9));
        if !same {
            // (a monochrome source with a color destination half as often as the rest)
            let sf = if r.chance(2) { df } else { *r.pick(FORMATS) };
            let custom = r.chance(4);
            let (l, _) = dib_line(&mut r, 1, sw, sh, &sf, custom);
            writeln!(s, "{l}\nfill 1 {}", r.next()).unwrap();
        }
        let src = if same { 0 } else { 1 };
        let (sw, sh) = if same { (w, h) } else { (sw, sh) };
        match r.range(0, 9) {
            0..=4 => writeln!(s, "brush 0 solid #{:06x}", rcolor(&mut r, &dpal)).unwrap(),
            5..=7 => {
                let pf = *r.pick(&[(32, ""), (24, ""), (8, ""), (16, ""), (1, "")]);
                let (pw, ph) = (r.range(1, 9), r.range(1, 9));
                let (l, _) = dib_line(&mut r, 2, pw, ph, &pf, false);
                writeln!(s, "{l}\nfill 2 {}\nbrush 0 dib 2", r.next()).unwrap();
            }
            8 => writeln!(s, "brush 0 hatch {} #{:06x}", r.range(0, 5), rcolor(&mut r, &dpal)).unwrap(),
            _ => writeln!(s, "brush 0 null").unwrap(),
        }
        if r.chance(3) {
            writeln!(s, "org 0 {} {}", r.range(-9, 9), r.range(-9, 9)).unwrap();
        }
        if r.chance(3) {
            writeln!(s, "text 0 #{:06x}\nbk 0 #{:06x}", rcolor(&mut r, &dpal), rcolor(&mut r, &dpal)).unwrap();
        }
        if r.chance(4) {
            let (x0, y0) = (r.range(-2, w), r.range(-2, h));
            writeln!(s, "clip 0 {x0} {y0} {} {}", x0 + r.range(0, w + 2), y0 + r.range(0, h + 2)).unwrap();
        }
        for _ in 0..r.range(1, 3) {
            let (x, y) = (r.range(-3, w + 1), r.range(-3, h + 1));
            let (bw, bh) = if r.chance(25) { (r.range(-4, -1), r.range(1, h)) } else { (r.range(0, w + 3), r.range(0, h + 3)) };
            let mut op = rop(&mut r);
            if r.chance(40) {
                op |= *r.pick(&[0x4000_0000u32, 0x8000_0000]);
            }
            if r.chance(5) && !crate::model::gdi::uses_s((op >> 16) as u8) {
                writeln!(s, "patblt 0 {x} {y} {bw} {bh} {op:#x}").unwrap();
            } else {
                writeln!(s, "bitblt 0 {x} {y} {bw} {bh} {src} {} {} {op:#x}", r.range(-3, sw), r.range(-3, sh)).unwrap();
            }
        }
        writeln!(s, "dump 0\nend").unwrap();
    }
    // what PatBlt does with an operation that needs a source, and the high flag bits
    for (i, op) in [0x00CC0020u32, 0x00660046, 0x40F00021, 0x80F00021, 0x405A0049].iter().enumerate() {
        writeln!(s, "case edge-patblt-{i}\ndib 0 6 3 32\nfill 0 {}\nbrush 0 solid #12ab34\npatblt 0 1 0 4 3 {op:#x}\ndump 0\nend", 77 + i).unwrap();
    }
    // IntersectClipRect with a rectangle not ordered; CreateDIBSection of a bit count GDI lacks
    writeln!(s, "case edge-clip-unordered\ndib 0 6 4 32\nfill 0 5\nclip 0 5 3 1 1\nbrush 0 solid #00ff00\npatblt 0 0 0 6 4 0xf00021\ndump 0\nend").unwrap();
    writeln!(s, "case edge-dib-2bpp\ndib 0 4 2 2\nend").unwrap();
    // negative extents (mirroring or normalizing?)
    for (i, (w, h)) in [(-3i32, 2i32), (3, -2), (-3, -2)].iter().enumerate() {
        writeln!(s, "case edge-negative-{i}\ndib 0 8 4 32\nfill 0 {}\ndib 1 8 4 32\nfill 1 {}\nbitblt 0 4 2 {w} {h} 1 4 2 0xcc0020\ndump 0\npatblt 0 4 2 {w} {h} 0x550009\ndump 0\nend", 90 + i, 95 + i).unwrap();
    }
    s
}

pub fn stretch() -> String {
    let mut s = String::from("# wingfx suite stretch: StretchBlt in each stretch mode (SPEC.md). Made by `wingfx gen stretch`.\n");
    let mut r = XorShift32::new(0x57e7_c400);
    // every enlargement and reduction of a row and of a column, 1..12 to 1..12, COLORONCOLOR
    for (dir, (a, b)) in [("x", (12, 1)), ("y", (1, 12))].iter().enumerate().map(|(i, d)| (i, d.1)) {
        let axis = if dir == 0 { "x" } else { "y" };
        writeln!(s, "case scale-{axis}\ndib 1 {a} {b} 32\nfill 1 {}\nmode 0 3", r.next()).unwrap();
        for dn in 1..=12 {
            writeln!(s, "dib 0 {a} {b} 32\nfill 0 7").unwrap();
            for sn in 1..=12 {
                let (dw, dh, sw, sh) = if dir == 0 { (dn, 1, sn, 1) } else { (1, dn, 1, sn) };
                writeln!(s, "stretchblt 0 0 0 {dw} {dh} 1 0 0 {sw} {sh} 0xcc0020").unwrap();
                writeln!(s, "dump 0").unwrap();
            }
        }
        writeln!(s, "end").unwrap();
    }
    for i in 0..250 {
        writeln!(s, "case random-{i}").unwrap();
        let df = *r.pick(FORMATS);
        let sf = if r.chance(2) { df } else { *r.pick(FORMATS) };
        let (w, h, sw, sh) = (r.range(1, 12), r.range(1, 8), r.range(1, 12), r.range(1, 8));
        let (l, _) = dib_line(&mut r, 0, w, h, &df, false);
        writeln!(s, "{l}\nfill 0 {}", r.next()).unwrap();
        let (l, _) = dib_line(&mut r, 1, sw, sh, &sf, false);
        writeln!(s, "{l}\nfill 1 {}", r.next()).unwrap();
        writeln!(s, "mode 0 {}", r.range(1, 4)).unwrap();
        if r.chance(3) {
            writeln!(s, "brush 0 solid #{:06x}", r.next() & 0xffffff).unwrap();
        }
        let ext = |r: &mut XorShift32, n: i64| if r.chance(12) { -r.range(1, n) } else { r.range(1, n + 2) };
        let (dw, dh) = (ext(&mut r, w), ext(&mut r, h));
        let (qw, qh) = (ext(&mut r, sw), ext(&mut r, sh));
        let op = if r.chance(3) { rop(&mut r) } else { 0xcc0020 };
        writeln!(s, "stretchblt 0 {} {} {dw} {dh} 1 {} {} {qw} {qh} {op:#x}\ndump 0\nend", r.range(-1, w - 1), r.range(-1, h - 1), r.range(0, sw - 1), r.range(0, sh - 1)).unwrap();
    }
    s
}

pub fn pixel() -> String {
    let mut s = String::from("# wingfx suite pixel: SetPixel, GetPixel, GetDIBits and SetDIBits between formats (SPEC.md). Made by `wingfx gen pixel`.\n");
    let mut r = XorShift32::new(0x9123_4567);
    for f in FORMATS {
        writeln!(s, "case pixel-{}", fmt_name(f)).unwrap();
        let (l, pal) = dib_line(&mut r, 0, 6, 3, f, false);
        writeln!(s, "{l}\nfill 0 {}", r.next()).unwrap();
        for y in 0..3 {
            for x in 0..6 {
                writeln!(s, "getpixel 0 {x} {y}").unwrap();
            }
        }
        for _ in 0..24 {
            writeln!(s, "setpixel 0 {} {} #{:06x}", r.range(-1, 6), r.range(-1, 3), rcolor(&mut r, &pal)).unwrap();
        }
        writeln!(s, "clip 0 1 1 4 2\ngetpixel 0 0 0\ngetpixel 0 2 1\nsetpixel 0 0 0 #ffffff\nsetpixel 0 3 1 #000000\ndump 0\nend").unwrap();
    }
    for d in FORMATS {
        for o in FORMATS {
            let ob = format!("{}{}", o.0, o.1);
            writeln!(s, "case dibits-{}-{}", fmt_name(d), fmt_name(o)).unwrap();
            let (l, _) = dib_line(&mut r, 0, 7, 3, d, false);
            writeln!(s, "{l}\nfill 0 {}\ngetdibits 0 {ob}\nsetdibits 0 {ob} {}\ndump 0\nend", r.next(), r.next()).unwrap();
        }
    }
    s
}

pub fn d3d9() -> String {
    state_suite(false)
}

pub fn d3d8() -> String {
    state_suite(true)
}

/// Direct3D 9's or 8's device state and state blocks
fn state_suite(v8: bool) -> String {
    let (suite, api, dev, stage_types) = if v8 { ("d3d8", "Direct3D 8", "device8", 30) } else { ("d3d9", "Direct3D 9", "device", 34) };
    let mut s = format!("# gpu\n# wingfx suite {suite}: {api} device state and state blocks (SPEC.md). Made by `wingfx gen {suite}`.\n");
    // every state a new device reports, on each kind of device
    for (name, mode) in [("defaults", "swvp"), ("defaults-hwvp", "hwvp"), ("defaults-mixed", "mixed")] {
        if name != "defaults" && !v8 {
            continue;
        }
        writeln!(s, "case {name}\n{dev} {mode}").unwrap();
        for i in 0..256 {
            writeln!(s, "rs get {i}").unwrap();
        }
        for st in 0..8 {
            for t in 0..stage_types {
                writeln!(s, "tss get {st} {t}").unwrap();
            }
        }
        if !v8 {
            for sm in (0..16).chain(256..=260) {
                for t in 0..14 {
                    writeln!(s, "ss get {sm} {t}").unwrap();
                }
            }
        }
        writeln!(s, "end").unwrap();
    }
    let states = if v8 { all_states8(true) } else { all_states() };
    let device = |s: &mut String, name: &str| writeln!(s, "case {name}\n{dev} swvp").unwrap();
    let get = |s: &mut String, k: &crate::model::d3d9::Key| {
        use crate::model::d3d9::Key::*;
        match k {
            Rs(i) => writeln!(s, "rs get {i}"),
            Tss(a, b) => writeln!(s, "tss get {a} {b}"),
            Ss(a, b) => writeln!(s, "ss get {a} {b}"),
        }
        .unwrap()
    };
    let set = |s: &mut String, k: &crate::model::d3d9::Key, v: u32| {
        use crate::model::d3d9::Key::*;
        match k {
            Rs(i) => writeln!(s, "rs set {i} {v:#x}"),
            Tss(a, b) => writeln!(s, "tss set {a} {b} {v:#x}"),
            Ss(a, b) => writeln!(s, "ss set {a} {b} {v:#x}"),
        }
        .unwrap()
    };
    // every documented state set to a value of its own and read back
    device(&mut s, "set-each");
    for (i, (k, ..)) in states.iter().enumerate() {
        set(&mut s, k, 0x1000 + i as u32);
    }
    for (k, ..) in &states {
        get(&mut s, k);
    }
    writeln!(s, "end").unwrap();
    // each state block type: everything changed after the block was made, the block applied,
    // everything read
    for ty in 1..=3 {
        device(&mut s, &format!("create-{ty}"));
        writeln!(s, "sb create 0 {ty}").unwrap();
        for (i, (k, ..)) in states.iter().enumerate() {
            set(&mut s, k, 0x2000 + i as u32);
        }
        writeln!(s, "sb apply 0").unwrap();
        for (k, ..) in &states {
            get(&mut s, k);
        }
        writeln!(s, "end").unwrap();
    }
    // random sequences of sets, gets and state block calls
    let mut r = XorShift32::new(if v8 { 0xd3d8_0008 } else { 0xd3d9_0009 });
    for i in 0..150 {
        device(&mut s, &format!("random-{i}"));
        let mut touched = Vec::new();
        let mut recording = false;
        for _ in 0..r.range(5, 40) {
            let k = r.pick(&states).0;
            match r.range(0, 19) {
                0..=8 => {
                    let v = match r.range(0, 3) {
                        0 => r.range(0, 9) as u32,
                        1 => *r.pick(&[0u32, 1, 0xffff_ffff, 0x3f80_0000, 0x8000_0000]),
                        _ => r.next(),
                    };
                    set(&mut s, &k, v);
                    touched.push(k);
                }
                9..=12 => get(&mut s, &k),
                13 if !recording => {
                    writeln!(s, "sb begin").unwrap();
                    recording = true;
                }
                14 if recording => {
                    writeln!(s, "sb end {}", r.range(0, 3)).unwrap();
                    recording = false;
                }
                15 if !recording => writeln!(s, "sb create {} {}", r.range(0, 3), r.range(1, 3)).unwrap(),
                16 | 17 if !recording => writeln!(s, "sb apply {}", r.range(0, 3)).unwrap(),
                18 if !recording => writeln!(s, "sb capture {}", r.range(0, 3)).unwrap(),
                19 if !recording => writeln!(s, "sb release {}", r.range(0, 3)).unwrap(),
                _ => {}
            }
        }
        if recording {
            writeln!(s, "sb end {}", r.range(0, 3)).unwrap();
        }
        // apply each block once more and read what the case touched
        for b in 0..3 {
            if r.chance(2) {
                writeln!(s, "sb apply {b}").unwrap();
            }
        }
        touched.sort();
        touched.dedup();
        for k in &touched {
            get(&mut s, k);
        }
        writeln!(s, "end").unwrap();
    }
    // what the documentation leaves open (unmodeled until Windows' answers come)
    let mut probe = |name: &str, body: &str| writeln!(s, "case edge-{name}\n{dev} swvp\n{body}\nend").unwrap();
    probe("set-legacy-rs", "rs set 0 5\nrs get 0\nrs set 42 6\nrs get 42\nrs set 165 7\nrs get 165\nrs set 1 8\nrs get 1");
    if v8 {
        probe("set-legacy-tss", "tss set 0 0 5\ntss get 0 0\ntss set 1 12 6\ntss get 1 12\ntss set 2 29 7\ntss get 2 29");
    } else {
        probe("set-legacy-tss", "tss set 0 0 5\ntss get 0 0\ntss set 1 12 6\ntss get 1 12\ntss set 2 33 7\ntss get 2 33");
        probe("set-legacy-ss", "ss set 0 0 5\nss get 0 0");
    }
    probe("end-without-begin", "sb end 0\nsb apply 0");
    probe("begin-twice", "sb begin\nsb begin\nrs set 7 0\nsb end 0\nrs get 7");
    probe("calls-while-recording", "sb create 1 1\nsb begin\nrs set 7 0\nsb apply 1\nsb capture 1\nsb create 2 2\nsb end 0\nrs get 7");
    probe("type-0", "sb create 0 0\nrs set 7 0\nsb apply 0\nrs get 7");
    probe("type-4", "sb create 0 4\nrs set 7 0\nsb apply 0\nrs get 7");
    let _ = RENDER_STATES;
    s
}

/// the descriptions the d3d11 cases start from (those of masu's d3d11desc probe: valid, rt0 and
/// depth enabled, stencil disabled)
pub fn d3d11_base(k: Kind) -> Vec<u32> {
    match k {
        Kind::Blend => {
            let mut d = vec![0u32; 66];
            d[2..10].copy_from_slice(&[1, 5, 6, 1, 2, 1, 1, 0xf]);
            d
        }
        Kind::Depth => vec![1, 1, 2, 0, 0xffff, 1, 1, 1, 8, 1, 1, 1, 8],
        Kind::Raster => vec![3, 3, 0, 0, 0, 0, 1, 0, 0, 0],
        Kind::Sampler => vec![0x15, 1, 1, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0x7f7f_ffff],
    }
}

/// float values around what the descriptions check (no -0.0: see D3D11.DEDUP.FLOAT)
pub fn d3d11_floats() -> Vec<u32> {
    let up = |v: f32| v.to_bits() + 1;
    vec![
        0,
        1.0f32.to_bits(),
        (-1.0f32).to_bits(),
        0.5f32.to_bits(),
        15.99f32.to_bits(),
        up(15.99),
        16.0f32.to_bits(),
        (-16.0f32).to_bits(),
        up(-16.0),
        (-15.99f32).to_bits(),
        f32::MAX.to_bits(),
        f32::MIN.to_bits(),
        f32::INFINITY.to_bits(),
        f32::NEG_INFINITY.to_bits(),
        0x7fc0_0000,
        0xffc0_0000,
        0x7f80_0001,
        1,
        1000.0f32.to_bits(),
    ]
}

/// the values each word of a description is tried with
fn d3d11_pool(k: Kind, i: usize) -> Vec<u32> {
    let enums = || (0..=21).chain([0xffff_ffff, 0x8000_0000]).collect::<Vec<u32>>();
    let bools = || vec![0, 1, 2, 0xffff_ffff];
    match (k, i) {
        (Kind::Blend, 0 | 1) => bools(),
        (Kind::Blend, i) if (i - 2) % 8 == 0 => bools(),
        (Kind::Blend, i) if (i - 2) % 8 == 7 => (0..=0x10).chain([0x1f, 0xff, 0x100, 0x10f, 0xffff_ffff]).collect(),
        (Kind::Blend, _) => enums(),
        (Kind::Depth, 0 | 3) => bools(),
        (Kind::Depth, 4) => vec![0, 0xff, 0xff00, 0xffff, 0x0f3c, 0x1_ffff, 0xffff_ffff],
        (Kind::Depth, _) => (0..=10).chain([0xffff_ffff]).collect(),
        (Kind::Raster, 0 | 1) => (0..=5).chain([0xffff_ffff]).collect(),
        (Kind::Raster, 3) => vec![0, 1, (-5i32) as u32, i32::MAX as u32, i32::MIN as u32],
        (Kind::Raster, 4 | 5) => d3d11_floats(),
        (Kind::Raster, _) => bools(),
        (Kind::Sampler, 0) => vec![0, 1, 4, 5, 0x10, 0x11, 0x14, 0x15, 0x55, 0x80, 0x95, 0xd5, 0x115, 0x155, 0x195, 0x1d5, 2, 0x54, 0x200],
        (Kind::Sampler, 1..=3) => (0..=7).chain([0xffff_ffff]).collect(),
        (Kind::Sampler, 5) => (0..=18).chain([0xffff_ffff]).collect(),
        (Kind::Sampler, 6) => (0..=10).chain([0xffff_ffff]).collect(),
        (Kind::Sampler, _) => d3d11_floats(),
    }
}

fn d3d11_line(s: &mut String, k: Kind, d: &[u32], check: bool) {
    let n = d.iter().rposition(|w| *w != 0).map_or(0, |i| i + 1);
    let words: Vec<String> = d[..n].iter().map(|w| if *w < 10 { w.to_string() } else { format!("{w:#x}") }).collect();
    writeln!(s, "{}{} {}", k.name(), if check { " check" } else { "" }, words.join(" ")).unwrap();
}

pub fn d3d11() -> String {
    let mut s = String::from("# gpu\n# wingfx suite d3d11: Direct3D 11 state objects (SPEC.md). Made by `wingfx gen d3d11`.\n");
    // one word of a description through every value of interest, the others as `base`; at most
    // 60 lines a case (an executor case holds 64 objects)
    let sweep = |s: &mut String, name: &str, k: Kind, base: &[u32], at: usize, values: &[u32]| {
        for (c, chunk) in values.chunks(60).enumerate() {
            writeln!(s, "case {name}-{c}\ndevice11").unwrap();
            for v in chunk {
                let mut d = base.to_vec();
                d[at] = *v;
                d3d11_line(s, k, &d, false);
            }
            writeln!(s, "end").unwrap();
        }
    };
    // blend: each word of render target 0, 3 and 7, blending on and off, targets shared and not
    let blend = d3d11_base(Kind::Blend);
    for indep in [0, 1] {
        for (enable, rt) in [(1, 0), (0, 0), (1, 3), (1, 7)] {
            let mut base = blend.clone();
            base[1] = indep;
            base[2 + rt * 8..10 + rt * 8].copy_from_slice(&[enable, 5, 6, 1, 2, 1, 1, 0xf]);
            for f in 0..8 {
                let at = 2 + rt * 8 + f;
                sweep(&mut s, &format!("blend-i{indep}-rt{rt}-e{enable}-w{f}"), Kind::Blend, &base, at, &d3d11_pool(Kind::Blend, at));
            }
        }
        let mut base = blend.clone();
        base[1] = indep;
        sweep(&mut s, &format!("blend-i{indep}-a2c"), Kind::Blend, &base, 0, &d3d11_pool(Kind::Blend, 0));
        sweep(&mut s, &format!("blend-i{indep}-indep"), Kind::Blend, &base, 1, &d3d11_pool(Kind::Blend, 1));
    }
    // depth stencil: each word, depth and stencil each on and off
    for depth in [0, 1] {
        for stencil in [0, 1] {
            let mut base = d3d11_base(Kind::Depth);
            base[0] = depth;
            base[3] = stencil;
            for at in 0..13 {
                sweep(&mut s, &format!("depth-d{depth}-s{stencil}-w{at}"), Kind::Depth, &base, at, &d3d11_pool(Kind::Depth, at));
            }
        }
    }
    // rasterizer: each word
    let base = d3d11_base(Kind::Raster);
    for at in 0..10 {
        sweep(&mut s, &format!("raster-w{at}"), Kind::Raster, &base, at, &d3d11_pool(Kind::Raster, at));
    }
    // sampler: every filter value below 0x400, and some above
    let base = d3d11_base(Kind::Sampler);
    let filters: Vec<u32> = (0..0x400).chain([0x1000, 0x8000_0000, 0xffff_ffff]).collect();
    sweep(&mut s, "sampler-filter", Kind::Sampler, &base, 0, &filters);
    // each other word under a plain, an anisotropic, a comparison and a maximum filter, with
    // and without a BORDER address
    for filter in [0x15, 0x55, 0x95, 0x195] {
        for addr in [1, 4] {
            let mut base = d3d11_base(Kind::Sampler);
            base[0] = filter;
            base[3] = addr;
            for at in 1..13 {
                sweep(&mut s, &format!("sampler-f{filter:x}-a{addr}-w{at}"), Kind::Sampler, &base, at, &d3d11_pool(Kind::Sampler, at));
            }
        }
    }
    // random descriptions near the valid ones, repeats of earlier ones (the same object?) and
    // validation only (ppState NULL)
    let mut r = XorShift32::new(0xd3d1_1011);
    for i in 0..300 {
        writeln!(s, "case random-{i}\ndevice11").unwrap();
        let mut made: Vec<(Kind, Vec<u32>)> = Vec::new();
        for _ in 0..r.range(5, 40) {
            let (k, mut d) = if !made.is_empty() && r.chance(4) {
                r.pick(&made).clone()
            } else {
                let k = *r.pick(&Kind::ALL);
                (k, d3d11_base(k))
            };
            for _ in 0..r.range(0, 4) {
                let at = r.range(0, k.words() as i64 - 1) as usize;
                d[at] = *r.pick(&d3d11_pool(k, at));
            }
            d3d11_line(&mut s, k, &d, r.chance(10));
            made.push((k, d));
        }
        writeln!(s, "end").unwrap();
    }
    // the calls around the descriptions
    writeln!(s, "case nodev").unwrap();
    for k in Kind::ALL {
        d3d11_line(&mut s, k, &d3d11_base(k), false);
    }
    writeln!(s, "end\ncase nodesc\ndevice11").unwrap();
    for k in Kind::ALL {
        writeln!(s, "{} nodesc", k.name()).unwrap();
    }
    writeln!(s, "end\ncase check\ndevice11").unwrap();
    for k in Kind::ALL {
        let mut bad = d3d11_base(k);
        bad[if k == Kind::Blend { 3 } else { 0 }] = 0;
        d3d11_line(&mut s, k, &d3d11_base(k), true);
        d3d11_line(&mut s, k, &bad, true);
        d3d11_line(&mut s, k, &d3d11_base(k), false);
        d3d11_line(&mut s, k, &d3d11_base(k), true);
    }
    writeln!(s, "end").unwrap();
    // what the documentation leaves open: 0.0 and -0.0
    for (k, at) in [(Kind::Raster, 4), (Kind::Raster, 5), (Kind::Sampler, 4), (Kind::Sampler, 11)] {
        writeln!(s, "case edge-negzero-{}-w{at}\ndevice11", k.name()).unwrap();
        let mut d = d3d11_base(k);
        d3d11_line(&mut s, k, &d, false);
        d[at] = 0x8000_0000;
        d3d11_line(&mut s, k, &d, false);
        writeln!(s, "end").unwrap();
    }
    s
}

pub fn wic() -> String {
    use crate::model::wic::{bits, FORMATS};
    let mut s = String::from("# wingfx suite wic: WIC pixel format conversion (SPEC.md). Made by `wingfx gen wic`.\n");
    let names: Vec<&str> = FORMATS.iter().map(|f| f.0).collect();
    // every pair, an 8x4 picture of random bytes
    let mut seed = 1u32;
    for from in &names {
        for to in &names {
            writeln!(s, "case pair-{from}-{to}\nconvert {from} 8 4 {seed} {to}\nend").unwrap();
            seed += 1;
        }
    }
    // every byte value of each format to 32bppBGRA and back (byte i = i mod 256)
    for (f, from) in names.iter().enumerate() {
        let w = (256 * 8 / bits(f)).clamp(1, 256);
        writeln!(s, "case ramp-{from}\nconvert {from} {w} 1 ramp bgra\nconvert bgra 64 1 ramp {from}\nend").unwrap();
    }
    // straight and premultiplied alpha over many (color, alpha) pairs
    for (i, f) in ["pbgra", "prgba", "bgra"].iter().enumerate() {
        for k in 0..4 {
            writeln!(s, "case alpha-{f}-{k}\nconvert {f} 256 64 {} bgra\nconvert bgra 256 64 {} {f}\nend", 7000 + i * 10 + k, 7100 + i * 10 + k).unwrap();
        }
    }
    // odd sizes: rows that end inside a byte, one-pixel pictures
    let mut r = XorShift32::new(0x0057_1c00);
    for i in 0..300 {
        let (from, to) = (*r.pick(&names), *r.pick(&names));
        let (w, h) = (r.range(1, 13), r.range(1, 4));
        writeln!(s, "case odd-{i}\nconvert {from} {w} {h} {} {to}\nend", r.next()).unwrap();
    }
    s
}

pub fn ddraw() -> String {
    let mut s = String::from("# wingfx suite ddraw: DirectDraw 7 blits between system-memory surfaces (SPEC.md). Made by `wingfx gen ddraw`.\n");
    let fmts = ["8", "555", "565", "32"];
    let bytes = |f: &str| match f {
        "8" => 1,
        "32" => 4,
        _ => 2,
    };
    let mut r = XorShift32::new(0x00dd_7a00);
    // a key value: the pixel at (x, y) of a surface filled from `seed` (so that some match)
    let key = |f: &str, w: i64, seed: u32, x: i64, y: i64| -> u32 {
        let mut g = XorShift32::new(seed);
        let b = bytes(f);
        let mut v = 0;
        for _ in 0..((y * w + x) as u32 + 1) {
            v = 0;
            for k in 0..b {
                v |= (g.next() & 0xff) << (8 * k);
            }
        }
        v
    };
    for f in fmts {
        // copy, fill and the rectangle checks, one by one
        writeln!(s, "case basic-{f}\ndd\nddsurf 0 12 9 {f}\nddsurf 1 7 5 {f}\nddfill 0 11\nddfill 1 12").unwrap();
        for l in [
            "ddblt 0 0 0 7 5 1 all", "ddblt 0 all 0 all", "ddblt 0 5 4 12 9 1 all", "ddblt 0 2 1 5 3 1 3 2 6 4", "ddblt 0 6 4 13 9 1 all", "ddblt 0 -1 0 6 5 1 all",
            "ddblt 0 3 3 3 5 1 all", "ddblt 0 0 0 2 2 1 6 4 8 6", "ddblt 0 0 0 7 5 1 0 0 7 5 mirrorlr", "ddblt 0 1 2 8 7 1 all mirrorud", "ddblt 0 2 2 9 7 1 all mirrorlr mirrorud",
        ] {
            writeln!(s, "{l}").unwrap();
        }
        let m: u64 = if f == "32" { 0xffff_ffff } else { (1u64 << (8 * bytes(f))) - 1 };
        writeln!(s, "ddcolor 0 1 1 4 3 {:#x}\nddcolor 0 all {:#x}\nddcolor 0 10 0 13 2 5\nddblt 0 4 4 11 9 1 all\ndddump 0\nend", 0x1234_5678 & m, 0x9abc_def0u64 & m).unwrap();
        // the color keys: each kind, with a key that occurs in the source
        let k = key(f, 7, 12, 2, 1);
        let kd = key(f, 12, 11, 3, 3);
        for (name, setup, l) in [
            ("keysrc", format!("ddkey 1 src {k:#x}"), "ddblt 0 2 2 9 7 1 all keysrc"),
            ("keydest", format!("ddkey 0 dest {kd:#x}"), "ddblt 0 0 0 7 5 1 all keydest"),
            ("keysrcov", String::new(), &*format!("ddblt 0 3 1 10 6 1 all keysrcov {k:#x}")),
            ("keydestov", String::new(), &*format!("ddblt 0 0 2 7 7 1 all keydestov {kd:#x}")),
            ("keyboth", format!("ddkey 1 src {k:#x}\nddkey 0 dest {kd:#x}"), "ddblt 0 0 0 7 5 1 all keysrc keydest"),
            ("fast", String::new(), "ddfast 0 3 2 1 all\nddfast 0 8 6 1 all\nddfast 0 0 0 1 1 1 5 4"),
            ("fastsrckey", format!("ddkey 1 src {k:#x}"), "ddfast 0 1 1 1 all srckey\nddfast 0 5 4 1 0 0 7 5 srckey"),
            ("fastdestkey", format!("ddkey 0 dest {kd:#x}"), "ddfast 0 1 1 1 all destkey"),
            ("keynone", format!("ddkey 1 src {k:#x}\nddkey 1 src none\nddkey 1 src {k:#x}"), "ddblt 0 2 2 9 7 1 all keysrc"),
        ] {
            writeln!(s, "case {name}-{f}\ndd\nddsurf 0 12 9 {f}\nddsurf 1 7 5 {f}\nddfill 0 11\nddfill 1 12\n{setup}\n{l}\ndddump 0\nend").unwrap();
        }
        // within one surface, every direction of overlap
        writeln!(s, "case overlap-{f}\ndd\nddsurf 0 16 12 {f}\nddfill 0 21").unwrap();
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (2, 3), (-3, -2), (3, -1), (-2, 2)] {
            let (x, y) = (4 + dx, 4 + dy);
            writeln!(s, "ddblt 0 {x} {y} {} {} 0 4 4 12 8\nddfast 0 {} {} 0 5 5 11 9", x + 8, y + 4, 5 - dx, 5 - dy).unwrap();
        }
        writeln!(s, "dddump 0\nend").unwrap();
    }
    // random blits: rectangles at and past the edges, keys from the source, mirroring
    for i in 0..300 {
        let f = *r.pick(&fmts);
        let (dw, dh, sw, sh) = (r.range(1, 20), r.range(1, 14), r.range(1, 14), r.range(1, 10));
        let (ds, ss) = (r.next() | 1, r.next() | 1);
        writeln!(s, "case random-{i}\ndd\nddsurf 0 {dw} {dh} {f}\nddsurf 1 {sw} {sh} {f}\nddfill 0 {ds}\nddfill 1 {ss}").unwrap();
        let k = key(f, sw, ss, r.range(0, sw - 1), r.range(0, sh - 1));
        let kd = key(f, dw, ds, r.range(0, dw - 1), r.range(0, dh - 1));
        if r.chance(2) {
            writeln!(s, "ddkey 1 src {k:#x}").unwrap();
        }
        if r.chance(3) {
            writeln!(s, "ddkey 0 dest {kd:#x}").unwrap();
        }
        let src_key = s.lines().rev().take(3).any(|l| l.starts_with("ddkey 1 src"));
        let dest_key = s.lines().rev().take(3).any(|l| l.starts_with("ddkey 0 dest"));
        for _ in 0..r.range(1, 6) {
            let (w, h) = (r.range(0, sw.min(dw) + 1), r.range(0, sh.min(dh) + 1));
            let (sx, sy) = (r.range(-1, sw - w + 1), r.range(-1, sh - h + 1));
            let (dx, dy) = (r.range(-1, dw - w + 1), r.range(-1, dh - h + 1));
            let mut l = if r.chance(3) {
                format!("ddfast 0 {dx} {dy} 1 {sx} {sy} {} {}", sx + w, sy + h)
            } else {
                format!("ddblt 0 {dx} {dy} {} {} 1 {sx} {sy} {} {}", dx + w, dy + h, sx + w, sy + h)
            };
            let fast = l.starts_with("ddfast");
            match r.range(0, 7) {
                0 if src_key => l += if fast { " srckey" } else { " keysrc" },
                1 if dest_key => l += if fast { " destkey" } else { " keydest" },
                2 if !fast => l += &format!(" keysrcov {k:#x}"),
                3 if !fast => l += &format!(" keydestov {kd:#x}"),
                4 if !fast => l += *r.pick(&[" mirrorlr", " mirrorud", " mirrorlr mirrorud"]),
                _ => {}
            }
            writeln!(s, "{l}").unwrap();
        }
        writeln!(s, "dddump 0\nend").unwrap();
    }
    // what the documentation leaves open
    for (name, body) in [
        ("stretch", "ddsurf 0 12 9 32\nddsurf 1 5 3 32\nddfill 0 1\nddfill 1 2\nddblt 0 0 0 11 7 1 all\nddblt 0 0 0 2 2 1 all"),
        ("formats", "ddsurf 0 12 9 565\nddsurf 1 5 3 32\nddfill 0 1\nddfill 1 2\nddblt 0 0 0 5 3 1 all"),
        ("key-unset", "ddsurf 0 12 9 32\nddsurf 1 5 3 32\nddfill 0 1\nddfill 1 2\nddblt 0 0 0 5 3 1 all keysrc\nddfast 0 0 0 1 all srckey"),
        ("fill-highbits", "ddsurf 0 12 9 565\nddfill 0 1\nddcolor 0 all 0x12345"),
        ("fast-bothkeys", "ddsurf 0 12 9 32\nddsurf 1 5 3 32\nddfill 0 1\nddfill 1 2\nddkey 1 src 0\nddkey 0 dest 0\nddfast 0 0 0 1 all srckey destkey"),
        ("nosource", "ddsurf 0 12 9 32\nddfill 0 1\nddblt 0 all - all"),
    ] {
        writeln!(s, "case edge-{name}\ndd\n{body}\ndddump 0\nend").unwrap();
    }
    writeln!(s, "case nodev\nddsurf 0 4 4 32\nend").unwrap();
    s
}
