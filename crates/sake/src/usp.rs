//! Uniscribe (usp10), the simple way: one item per run, one glyph per character, advances from
//! the font. Enough for D3DXCreateFont (statically linked into Siglus), which lays text out
//! with these.

#![allow(non_snake_case)]

use std::sync::OnceLock;

use crate::api::{implement, Ctx};
use crate::gdi;
use crate::mem::{self, put32, r16, r32, w16, w32, w8};

const E_PENDING: u32 = 0x8000_000a;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;

fn wchars(p: u32, n: u32) -> Vec<u16> {
    // (a negative count: nothing; no text is longer than the guest's address space)
    let n = if (n as i32) < 0 { 0 } else { n.min(1 << 28) };
    (0..n).map(|i| r16(p + 2 * i)).collect()
}

/// The font for a call: the DC's (remembered in the SCRIPT_CACHE, a block holding the HFONT),
/// or the cached one when there is no DC.
fn cache_font(dc: u32, psc: u32) -> Option<u32> {
    if psc == 0 {
        return (dc != 0).then_some(0);
    }
    if dc != 0 {
        let f = gdi::dc_font_handle(dc);
        let mut blk = r32(psc);
        if blk == 0 {
            blk = mem::heap_alloc(4, true);
            w32(psc, blk);
        }
        w32(blk, f);
        return Some(f);
    }
    let blk = r32(psc);
    (blk != 0).then(|| r32(blk))
}

fn ScriptFreeCache(c: &mut Ctx) -> u32 {
    let psc = c.a(0);
    if psc != 0 && r32(psc) != 0 {
        mem::heap_free(r32(psc));
        w32(psc, 0);
    }
    0
}

/// SCRIPT_ITEMs: the whole text as one item, then the terminator.
fn ScriptItemize(c: &mut Ctx) -> u32 {
    let (n, max, items, count) = (c.a(1), c.a(2), c.a(5), c.a(6));
    if n == 0 || max < 2 || items == 0 {
        return E_INVALIDARG;
    }
    mem::fill(items, 16, 0);
    w32(items + 8, n);
    put32(count, 1);
    0
}

fn ScriptShape(c: &mut Ctx) -> u32 {
    let (dc, psc, s, n, max, out, clust, vis, count) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(6), c.a(7), c.a(8), c.a(9));
    if n > max {
        return E_OUTOFMEMORY;
    }
    let Some(f) = cache_font(dc, psc) else { return E_PENDING };
    let Some(g) = gdi::font_glyphs(dc, f, &wchars(s, n)) else { return E_PENDING };
    for (i, gi) in g.iter().enumerate() {
        let i = i as u32;
        w16(out + 2 * i, *gi);
        put16(clust, i, i as u16);
        put16(vis, i, 1 << 4); // fClusterStart
    }
    put32(count, n);
    0
}
fn put16(base: u32, i: u32, v: u16) {
    if base != 0 {
        w16(base + 2 * i, v);
    }
}

fn ScriptPlace(c: &mut Ctx) -> u32 {
    let (dc, psc, glyphs, n, adv, goff, abc) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(6), c.a(7), c.a(8));
    if (n as i32) <= 0 || n > 1 << 20 {
        return E_INVALIDARG;
    }
    let Some(f) = cache_font(dc, psc) else { return E_PENDING };
    let Some(w) = gdi::font_abc(dc, f, &wchars(glyphs, n)) else { return E_PENDING };
    let (mut ta, mut tb, mut tc) = (0, 0, 0);
    for (i, (a, b, cc)) in w.iter().enumerate() {
        let i = i as u32;
        put32(adv + 4 * i, (a + b + cc) as u32);
        if goff != 0 {
            w32(goff + 8 * i, 0);
            w32(goff + 8 * i + 4, 0);
        }
        if i == 0 {
            ta = *a;
        }
        tb += a + b + cc;
        tc = *cc;
    }
    if abc != 0 {
        w32(abc, ta as u32);
        w32(abc + 4, (tb - ta - tc) as u32);
        w32(abc + 8, tc as u32);
    }
    0
}

fn ScriptGetFontProperties(c: &mut Ctx) -> u32 {
    let (dc, psc, p) = (c.a(0), c.a(1), c.a(2));
    let Some(f) = cache_font(dc, psc) else { return E_PENDING };
    if p == 0 || r32(p) < 16 {
        return E_INVALIDARG;
    }
    let blank = gdi::font_glyphs(dc, f, &[0x20]).map_or(3, |g| g[0]);
    w16(p + 4, blank);
    w16(p + 6, 0);
    w16(p + 8, 0);
    w16(p + 10, 0xffff);
    w32(p + 12, 0);
    0
}

/// One script (the default) with empty SCRIPT_PROPERTIES.
fn ScriptGetProperties(c: &mut Ctx) -> u32 {
    static TABLE: OnceLock<u32> = OnceLock::new();
    let t = *TABLE.get_or_init(|| {
        let props = mem::heap_alloc(8, true);
        let t = mem::heap_alloc(4, true);
        w32(t, props);
        t
    });
    put32(c.a(0), t);
    put32(c.a(1), 1);
    0
}

/// SCRIPT_LOGATTR: every character a stop; breaks after spaces and around CJK characters.
fn ScriptBreak(c: &mut Ctx) -> u32 {
    let (s, n, out) = (c.a(0), c.a(1), c.a(3));
    let t = wchars(s, n);
    for i in 0..n as usize {
        let ch = t[i];
        let white = ch == 0x20 || ch == 0x3000 || ch == 9;
        let cjk = |c: u16| c >= 0x3000;
        let soft = i > 0 && (t[i - 1] == 0x20 || cjk(ch) || cjk(t[i - 1]));
        let mut v = 1 << 2; // fCharStop
        if soft {
            v |= 1 | 1 << 3;
        }
        if white {
            v |= 2;
        }
        w8(out + i as u32, v);
    }
    0
}

fn ScriptJustify(c: &mut Ctx) -> u32 {
    let (adv, n, out) = (c.a(1), c.a(2), c.a(5));
    mem::copy(out, adv, 4 * n);
    0
}

fn ScriptRecordDigitSubstitution(c: &mut Ctx) -> u32 {
    mem::fill(c.a(1), 8, 0);
    0
}
fn ScriptApplyDigitSubstitution(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        mem::fill(c.a(1), 4, 0);
    }
    if c.a(2) != 0 {
        mem::fill(c.a(2), 2, 0);
    }
    0
}

/// ScriptTextOut: the glyphs are drawn with ExtTextOutW's ETO_GLYPH_INDEX.
fn ScriptTextOut(c: &mut Ctx) -> u32 {
    let (dc, x, y, opts, rect, glyphs, n, adv) = (c.a(0), c.a(2), c.a(3), c.a(4), c.a(5), c.a(9), c.a(10), c.a(11));
    let f = crate::api::export("gdi32", "ExtTextOutW");
    c.call(f, &[dc, x, y, opts & 6 | 0x10, rect, glyphs, n, adv]);
    0
}

// SCRIPT_STRING_ANALYSIS: {dc, chars (copy), n, SIZE, advances}
fn ScriptStringAnalyse(c: &mut Ctx) -> u32 {
    let (dc, s, n, out) = (c.a(0), c.a(1), c.a(2), c.a(12));
    if n == 0 || (n as i32) < 0 || n > 1 << 28 || dc == 0 {
        return E_INVALIDARG;
    }
    let text = wchars(s, n);
    let Some(g) = gdi::font_glyphs(dc, 0, &text) else { return E_INVALIDARG };
    let w = gdi::font_abc(dc, 0, &g).unwrap_or_default();
    let blk = mem::heap_alloc(20 + 2 * n + 4 * n, true);
    let chars = blk + 20;
    let advs = chars + 2 * n;
    mem::copy(chars, s, 2 * n);
    let mut total = 0;
    for (i, (a, b, cc)) in w.iter().enumerate() {
        w32(advs + 4 * i as u32, (a + b + cc) as u32);
        total += a + b + cc;
    }
    w32(blk, dc);
    w32(blk + 4, n);
    w32(blk + 8, total as u32);
    // height: the font's line height
    let tmb = mem::heap_alloc(60, true);
    let f = crate::api::export("gdi32", "GetTextMetricsW");
    c.call(f, &[dc, tmb]);
    w32(blk + 12, r32(tmb));
    mem::heap_free(tmb);
    w32(blk + 16, advs);
    put32(out, blk);
    0
}
fn ScriptString_pSize(c: &mut Ctx) -> u32 {
    let blk = c.a(0);
    if blk == 0 { 0 } else { blk + 8 }
}
fn ScriptStringCPtoX(c: &mut Ctx) -> u32 {
    let (blk, cp, trailing, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if blk == 0 {
        return E_INVALIDARG;
    }
    let advs = r32(blk + 16);
    let end = (cp + (trailing != 0) as u32).min(r32(blk + 4));
    let x: i32 = (0..end).map(|i| r32(advs + 4 * i) as i32).sum();
    put32(out, x as u32);
    0
}
fn ScriptStringOut(c: &mut Ctx) -> u32 {
    let (blk, x, y, opts, rect) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if blk == 0 {
        return E_INVALIDARG;
    }
    let f = crate::api::export("gdi32", "ExtTextOutW");
    c.call(f, &[r32(blk), x, y, opts & 6, rect, blk + 20, r32(blk + 4), 0]);
    0
}
fn ScriptStringFree(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p != 0 && r32(p) != 0 {
        mem::heap_free(r32(p));
        w32(p, 0);
    }
    0
}

pub fn init() {
    for (n, f) in [
        ("ScriptFreeCache", ScriptFreeCache as crate::api::Handler),
        ("ScriptItemize", ScriptItemize),
        ("ScriptShape", ScriptShape),
        ("ScriptPlace", ScriptPlace),
        ("ScriptGetFontProperties", ScriptGetFontProperties),
        ("ScriptGetProperties", ScriptGetProperties),
        ("ScriptBreak", ScriptBreak),
        ("ScriptJustify", ScriptJustify),
        ("ScriptRecordDigitSubstitution", ScriptRecordDigitSubstitution),
        ("ScriptApplyDigitSubstitution", ScriptApplyDigitSubstitution),
        ("ScriptTextOut", ScriptTextOut),
        ("ScriptStringAnalyse", ScriptStringAnalyse),
        ("ScriptString_pSize", ScriptString_pSize),
        ("ScriptStringCPtoX", ScriptStringCPtoX),
        ("ScriptStringOut", ScriptStringOut),
        ("ScriptStringFree", ScriptStringFree),
    ] {
        implement("usp10", n, f);
        // (D3DX's font code looks them up in gdi32 too, where Windows also exports them)
        implement("gdi32", n, f);
    }
}
