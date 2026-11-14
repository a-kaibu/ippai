//! Case generators: the case files are made here, from fixed seeds (cases/<suite>.txt is what
//! `wingfx gen <suite>` prints; a test keeps them equal). Coverage comes from structure (every
//! raster operation on every pixel format, every state, every state block type) and from many
//! random cases around the edges (clipping, bitmap borders, overlap, odd sizes, both row orders).

use crate::model::d3d9::{all_states, RENDER_STATES};
use crate::model::gdi::default_palette;
use crate::rng::XorShift32;
use std::fmt::Write;

pub const SUITES: &[&str] = &["gdi", "stretch", "pixel", "d3d9"];

pub fn suite(name: &str) -> Option<String> {
    match name {
        "gdi" => Some(gdi()),
        "stretch" => Some(stretch()),
        "pixel" => Some(pixel()),
        "d3d9" => Some(d3d9()),
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
    let mut s = String::from("# gpu\n# wingfx suite d3d9: Direct3D 9 device state and state blocks (SPEC.md). Made by `wingfx gen d3d9`.\n");
    // every state a new device reports
    writeln!(s, "case defaults\ndevice swvp").unwrap();
    for i in 0..256 {
        writeln!(s, "rs get {i}").unwrap();
    }
    for st in 0..8 {
        for t in 0..34 {
            writeln!(s, "tss get {st} {t}").unwrap();
        }
    }
    for sm in (0..16).chain(256..=260) {
        for t in 0..14 {
            writeln!(s, "ss get {sm} {t}").unwrap();
        }
    }
    writeln!(s, "end").unwrap();
    let states = all_states();
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
    writeln!(s, "case set-each\ndevice swvp").unwrap();
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
        writeln!(s, "case create-{ty}\ndevice swvp\nsb create 0 {ty}").unwrap();
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
    let mut r = XorShift32::new(0xd3d9_0009);
    for i in 0..150 {
        writeln!(s, "case random-{i}\ndevice swvp").unwrap();
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
    let mut probe = |name: &str, body: &str| writeln!(s, "case edge-{name}\ndevice swvp\n{body}\nend").unwrap();
    probe("set-legacy-rs", "rs set 0 5\nrs get 0\nrs set 42 6\nrs get 42\nrs set 165 7\nrs get 165\nrs set 1 8\nrs get 1");
    probe("set-legacy-tss", "tss set 0 0 5\ntss get 0 0\ntss set 1 12 6\ntss get 1 12\ntss set 2 33 7\ntss get 2 33");
    probe("set-legacy-ss", "ss set 0 0 5\nss get 0 0");
    probe("end-without-begin", "sb end 0\nsb apply 0");
    probe("begin-twice", "sb begin\nsb begin\nrs set 7 0\nsb end 0\nrs get 7");
    probe("calls-while-recording", "sb create 1 1\nsb begin\nrs set 7 0\nsb apply 1\nsb capture 1\nsb create 2 2\nsb end 0\nrs get 7");
    probe("type-0", "sb create 0 0\nrs set 7 0\nsb apply 0\nrs get 7");
    probe("type-4", "sb create 0 4\nrs set 7 0\nsb apply 0\nrs get 7");
    let _ = RENDER_STATES;
    s
}
