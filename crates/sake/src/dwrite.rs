//! DirectWrite: the factory, text formats and text layouts, with sake's fonts (gdi.rs's
//! TextFont: the same faces and glyphs GDI draws). Layout is simple: lines break at newlines
//! and, when wrapping, at the character that would pass the width; text is aligned per line
//! and the block per paragraph alignment. d2d.rs draws what `layout` returns.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::com;
use crate::gdi::{text_font, TextFont};
use crate::mem::{self, w32};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone)]
pub struct Format {
    pub family: String,
    pub size: f32,
    pub weight: i32,
    /// DWRITE_TEXT_ALIGNMENT: 0 leading, 1 trailing, 2 center, 3 justified
    pub align: u32,
    /// DWRITE_PARAGRAPH_ALIGNMENT: 0 near, 1 far, 2 center
    pub para: u32,
    /// DWRITE_WORD_WRAPPING: 0 wrap, 1 no wrap...
    pub wrap: u32,
    /// DWRITE_LINE_SPACING_METHOD 1 (uniform): line height, baseline
    pub spacing: Option<(f32, f32)>,
    /// IDWriteTextLayout1::SetCharacterSpacing: leading, trailing, minimum advance
    pub char_spacing: (f32, f32, f32),
}

pub struct Layout {
    pub fmt: Format,
    pub text: Vec<u16>,
    pub max_w: f32,
    pub max_h: f32,
}

static FORMATS: Mutex<Option<HashMap<u32, Format>>> = Mutex::new(None);
static LAYOUTS: Mutex<Option<HashMap<u32, Layout>>> = Mutex::new(None);

fn formats<R>(f: impl FnOnce(&mut HashMap<u32, Format>) -> R) -> R {
    f(FORMATS.lock().unwrap().get_or_insert_with(HashMap::new))
}
fn layouts<R>(f: impl FnOnce(&mut HashMap<u32, Layout>) -> R) -> R {
    f(LAYOUTS.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn fl(c: &Ctx, i: u32) -> f32 {
    c.f(i)
}

fn wstr(p: u32, n: Option<u32>) -> Vec<u16> {
    let mut v = Vec::new();
    if p == 0 {
        return v;
    }
    let mut i = 0;
    loop {
        if n.is_some_and(|n| i >= n) {
            break;
        }
        let ch = mem::r16(p + 2 * i);
        if n.is_none() && ch == 0 {
            break;
        }
        v.push(ch);
        i += 1;
    }
    v
}

/// The format of a text format or text layout object.
pub fn format_of(obj: u32) -> Option<Format> {
    formats(|m| m.get(&obj).cloned()).or_else(|| layouts(|m| m.get(&obj).map(|l| l.fmt.clone())))
}

fn set_format(obj: u32, f: impl Fn(&mut Format)) {
    let done = formats(|m| m.get_mut(&obj).map(&f).is_some());
    if !done {
        layouts(|m| {
            if let Some(l) = m.get_mut(&obj) {
                f(&mut l.fmt);
            }
        });
    }
}

// ---- factory ----

const FACTORY: &str = "IDWriteFactory7";

fn factory_methods() -> Vec<(&'static str, crate::api::Handler)> {
    vec![
        ("CreateTextFormat", CreateTextFormat),
        ("CreateTextLayout", CreateTextLayout),
        (
            "CreateGdiCompatibleTextLayout",
            CreateGdiCompatibleTextLayout,
        ),
        ("CreateRenderingParams", CreateRenderingParams),
        ("CreateMonitorRenderingParams", CreateRenderingParams),
        ("CreateCustomRenderingParams", CreateCustomRenderingParams),
        ("GetSystemFontCollection", GetSystemFontCollection),
        ("GetSystemFontFallback", GetSystemFontFallback),
        ("CreateNumberSubstitution", CreateNumberSubstitution),
        ("TranslateColorGlyphRun", TranslateColorGlyphRun),
    ]
}

static SHARED: Mutex<u32> = Mutex::new(0);

/// No color glyphs in the built-in faces: DWRITE_E_NOCOLOR, the enumerator NULL (its out
/// parameter is the last argument of every overload; IDWriteFactory4's and 8's take the
/// baseline origin as a D2D1_POINT_2F, one argument on x64, two dwords on x86).
fn TranslateColorGlyphRun(c: &mut Ctx) -> u32 {
    let n = com::method_dwords(c.trap).unwrap_or(9);
    let last = if crate::mem::x64() && n > 9 {
        n - 2
    } else {
        n - 1
    };
    mem::putp(c.a(last), 0);
    0x8898_500c
}
fn DWriteCreateFactory(c: &mut Ctx) -> u32 {
    let (ty, out) = (c.a(0), c.a(2));
    let f = if ty == 0 {
        let mut g = SHARED.lock().unwrap();
        if *g == 0 {
            *g = com::object(FACTORY, &factory_methods(), 8);
        }
        *g
    } else {
        com::object(FACTORY, &factory_methods(), 8)
    };
    mem::putp(out, f);
    0
}

fn CreateTextFormat(c: &mut Ctx) -> u32 {
    // (this, family, collection, weight, style, stretch, size, locale, out)
    let family = String::from_utf16_lossy(&wstr(c.a(1), None));
    let fmt = Format {
        family,
        size: fl(c, 6),
        weight: c.a(3) as i32,
        align: 0,
        para: 0,
        wrap: 0,
        spacing: None,
        char_spacing: (0.0, 0.0, 0.0),
    };
    let o = com::object("IDWriteTextFormat3", &format_methods(), 8);
    formats(|m| m.insert(o, fmt));
    mem::putp(c.a(8), o);
    0
}

fn format_methods() -> Vec<(&'static str, crate::api::Handler)> {
    vec![
        ("SetTextAlignment", SetTextAlignment),
        ("SetParagraphAlignment", SetParagraphAlignment),
        ("SetWordWrapping", SetWordWrapping),
        ("SetLineSpacing", SetLineSpacing),
        ("SetReadingDirection", ok),
        ("SetFlowDirection", ok),
        ("SetIncrementalTabStop", ok),
        ("SetTrimming", ok),
        ("GetTextAlignment", GetTextAlignment),
        ("GetParagraphAlignment", GetParagraphAlignment),
        ("GetWordWrapping", GetWordWrapping),
        ("GetReadingDirection", zero),
        ("GetFlowDirection", zero),
        ("GetFontWeight", GetFontWeight),
        ("GetFontStyle", zero),
        ("GetFontStretch", five),
        ("GetFontSize", GetFontSize),
        ("GetFontFamilyNameLength", GetFontFamilyNameLength),
        ("GetFontFamilyName", GetFontFamilyName),
        ("GetLocaleNameLength", zero),
        ("GetLocaleName", GetLocaleName),
    ]
}

fn ok(_: &mut Ctx) -> u32 {
    0
}
fn zero(_: &mut Ctx) -> u32 {
    0
}
fn five(_: &mut Ctx) -> u32 {
    5
}

fn SetTextAlignment(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    set_format(c.a(0), |f| f.align = v);
    0
}
fn SetParagraphAlignment(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    set_format(c.a(0), |f| f.para = v);
    0
}
fn SetWordWrapping(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    set_format(c.a(0), |f| f.wrap = v);
    0
}
fn SetLineSpacing(c: &mut Ctx) -> u32 {
    // (this, method, spacing, baseline)
    let (m, s, b) = (c.a(1), fl(c, 2), fl(c, 3));
    set_format(c.a(0), |f| {
        f.spacing = if m == 1 { Some((s, b)) } else { None }
    });
    0
}
fn GetTextAlignment(c: &mut Ctx) -> u32 {
    format_of(c.a(0)).map_or(0, |f| f.align)
}
fn GetParagraphAlignment(c: &mut Ctx) -> u32 {
    format_of(c.a(0)).map_or(0, |f| f.para)
}
fn GetWordWrapping(c: &mut Ctx) -> u32 {
    format_of(c.a(0)).map_or(0, |f| f.wrap)
}
fn GetFontWeight(c: &mut Ctx) -> u32 {
    format_of(c.a(0)).map_or(400, |f| f.weight as u32)
}
fn GetFontSize(c: &mut Ctx) -> u32 {
    c.st0 = Some(format_of(c.a(0)).map_or(0.0, |f| f.size) as f64);
    0
}
fn GetFontFamilyNameLength(c: &mut Ctx) -> u32 {
    format_of(c.a(0)).map_or(0, |f| f.family.encode_utf16().count() as u32)
}
fn GetFontFamilyName(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(1), c.a(2));
    let name: Vec<u16> =
        format_of(c.a(0)).map_or(Vec::new(), |f| f.family.encode_utf16().collect());
    if name.len() as u32 >= n {
        return 0x8007_007a;
    }
    for (i, ch) in name.iter().chain([0u16].iter()).enumerate() {
        mem::w16(p + 2 * i as u32, *ch);
    }
    0
}
fn GetLocaleName(c: &mut Ctx) -> u32 {
    if c.a(2) > 0 {
        mem::w16(c.a(1), 0);
    }
    0
}

// ---- layouts ----

fn layout_methods() -> Vec<(&'static str, crate::api::Handler)> {
    let mut v = format_methods();
    v.extend_from_slice(&[
        ("SetMaxWidth", SetMaxWidth),
        ("SetMaxHeight", SetMaxHeight),
        ("GetMaxWidth", GetMaxWidth),
        ("GetMaxHeight", GetMaxHeight),
        ("GetMetrics", GetMetrics),
        ("SetFontSize", SetFontSize),
        ("SetFontWeight", SetFontWeight),
        ("SetFontFamilyName", SetFontFamilyName),
        ("SetUnderline", ok),
        ("SetStrikethrough", ok),
        ("SetDrawingEffect", ok),
        ("SetLocaleName", ok),
        ("SetFontStyle", ok),
        ("SetFontStretch", ok),
        ("SetTypography", ok),
        ("SetInlineObject", ok),
        ("SetFontCollection", ok),
        ("SetCharacterSpacing", SetCharacterSpacing),
        ("GetLineMetrics", GetLineMetrics),
        ("GetClusterMetrics", GetClusterMetrics),
        ("Draw", Draw),
    ]);
    v
}

fn new_layout(text: Vec<u16>, fmt: u32, w: f32, h: f32, out: u32) -> u32 {
    let Some(fmt) = format_of(fmt) else {
        mem::putp(out, 0);
        return 0x8007_0057;
    };
    let o = com::object("IDWriteTextLayout4", &layout_methods(), 8);
    layouts(|m| {
        m.insert(
            o,
            Layout {
                fmt,
                text,
                max_w: w,
                max_h: h,
            },
        )
    });
    mem::putp(out, o);
    0
}

fn CreateTextLayout(c: &mut Ctx) -> u32 {
    // (this, string, length, format, max width, max height, out)
    new_layout(
        wstr(c.a(1), Some(c.a(2))),
        c.a(3),
        fl(c, 4),
        fl(c, 5),
        c.a(6),
    )
}
fn CreateGdiCompatibleTextLayout(c: &mut Ctx) -> u32 {
    // (this, string, length, format, width, height, pixels per dip, transform, natural, out)
    new_layout(
        wstr(c.a(1), Some(c.a(2))),
        c.a(3),
        fl(c, 4),
        fl(c, 5),
        c.a(9),
    )
}

fn SetMaxWidth(c: &mut Ctx) -> u32 {
    let v = fl(c, 1);
    layouts(|m| m.get_mut(&c.a(0)).map(|l| l.max_w = v));
    0
}
fn SetMaxHeight(c: &mut Ctx) -> u32 {
    let v = fl(c, 1);
    layouts(|m| m.get_mut(&c.a(0)).map(|l| l.max_h = v));
    0
}
fn GetMaxWidth(c: &mut Ctx) -> u32 {
    c.st0 = Some(layouts(|m| m.get(&c.a(0)).map_or(0.0, |l| l.max_w)) as f64);
    0
}
fn GetMaxHeight(c: &mut Ctx) -> u32 {
    c.st0 = Some(layouts(|m| m.get(&c.a(0)).map_or(0.0, |l| l.max_h)) as f64);
    0
}
// (the range-based setters apply to the whole layout)
fn SetFontSize(c: &mut Ctx) -> u32 {
    let v = fl(c, 1);
    layouts(|m| m.get_mut(&c.a(0)).map(|l| l.fmt.size = v));
    0
}
fn SetFontWeight(c: &mut Ctx) -> u32 {
    let v = c.a(1) as i32;
    layouts(|m| m.get_mut(&c.a(0)).map(|l| l.fmt.weight = v));
    0
}
fn SetFontFamilyName(c: &mut Ctx) -> u32 {
    let v = String::from_utf16_lossy(&wstr(c.a(1), None));
    layouts(|m| m.get_mut(&c.a(0)).map(|l| l.fmt.family = v));
    0
}

fn GetMetrics(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    let Some((text, fmt, w, h)) = layouts(|m| {
        m.get(&this)
            .map(|l| (l.text.clone(), l.fmt.clone(), l.max_w, l.max_h))
    }) else {
        return 0x8007_0057;
    };
    let r = layout(&text, &fmt, w, h);
    let (mut x0, mut x1, mut x1ws) = (f32::MAX, 0f32, 0f32);
    for l in &r.lines {
        x0 = x0.min(l.x);
        x1 = x1.max(l.x + l.width);
        x1ws = x1ws.max(l.x + l.width_ws);
    }
    if r.lines.is_empty() {
        x0 = 0.0;
    }
    let top = r.lines.first().map_or(0.0, |l| l.top);
    let vals = [
        x0,
        top,
        (x1 - x0).max(0.0),
        (x1ws - x0).max(0.0),
        r.height,
        w,
        h,
    ];
    for (i, v) in vals.iter().enumerate() {
        w32(p + 4 * i as u32, v.to_bits());
    }
    w32(p + 28, 0);
    w32(p + 32, r.lines.len() as u32);
    0
}

fn SetCharacterSpacing(c: &mut Ctx) -> u32 {
    // (this, leading, trailing, minimum advance, range): the whole text (ranges are not kept)
    let (lead, trail, min) = (fl(c, 1), fl(c, 2), fl(c, 3));
    if min < 0.0 {
        return E_INVALIDARG;
    }
    layouts(|m| {
        m.get_mut(&c.a(0))
            .map(|l| l.fmt.char_spacing = (lead, trail, min))
    });
    0
}

/// UTF-16 code units of a character
fn units(ch: u32) -> u32 {
    if ch > 0xffff {
        2
    } else {
        1
    }
}

/// DWRITE_LINE_METRICS of a laid-out text: (length, trailing white space length, newline
/// length, height, baseline) per line; lengths in code units, the newline counted as trailing
/// white space too.
pub fn line_metrics(chars: &[u32], laid: &Laid, line_h: f32) -> Vec<(u32, u32, u32, f32, f32)> {
    let u = |a: usize, b: usize| chars[a..b].iter().map(|&ch| units(ch)).sum::<u32>();
    laid.lines
        .iter()
        .map(|l| {
            let mut ws = l.to;
            while ws > l.from && is_space(chars[ws - 1] as u16) {
                ws -= 1;
            }
            let nl = u(l.to, l.to + l.nl);
            (
                u(l.from, l.to) + nl,
                u(ws, l.to) + nl,
                nl,
                line_h,
                laid.base,
            )
        })
        .collect()
}

fn line_height(fmt: &Format, laid: &Laid) -> f32 {
    if let Some((h, _)) = fmt.spacing {
        return h;
    }
    let (a, d, g) = laid
        .font
        .as_ref()
        .map_or((fmt.size * 0.86, fmt.size * 0.14, 0.0), |f| f.metrics());
    a + d + g
}

fn GetLineMetrics(c: &mut Ctx) -> u32 {
    // (this, out DWRITE_LINE_METRICS[], max count, out count)
    let (this, out, max, count) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((text, fmt, w, h)) = layout_of(this) else {
        return E_INVALIDARG;
    };
    let laid = layout(&text, &fmt, w, h);
    let m = line_metrics(&chars(&text), &laid, line_height(&fmt, &laid));
    w32(count, m.len() as u32);
    if (max as usize) < m.len() {
        return E_NOT_SUFFICIENT_BUFFER;
    }
    for (i, (len, ws, nl, lh, base)) in m.iter().enumerate() {
        let p = out + 24 * i as u32;
        for (k, v) in [*len, *ws, *nl, lh.to_bits(), base.to_bits(), 0]
            .iter()
            .enumerate()
        {
            w32(p + 4 * k as u32, *v);
        }
    }
    0
}

fn GetClusterMetrics(c: &mut Ctx) -> u32 {
    // (this, out DWRITE_CLUSTER_METRICS[], max count, out count): a cluster per character
    let (this, out, max, count) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((text, fmt, w, h)) = layout_of(this) else {
        return E_INVALIDARG;
    };
    let laid = layout(&text, &fmt, w, h);
    let ch = chars(&text);
    w32(count, ch.len() as u32);
    if (max as usize) < ch.len() {
        return E_NOT_SUFFICIENT_BUFFER;
    }
    for (i, &k) in ch.iter().enumerate() {
        let p = out + 8 * i as u32;
        w32(p, laid.adv[i].to_bits());
        mem::w16(p + 4, units(k) as u16);
        // canWrapLineAfter 1, isWhitespace 2, isNewline 4
        let nl = k == 10 || k == 13;
        let space = is_space(k as u16) || nl;
        // (a break goes after white space, never before it; between CJK characters; after the
        // last cluster)
        let wrap_after = space
            || ch
                .get(i + 1)
                .is_none_or(|&n| !is_space(n as u16) && (k > 0x2e7f || n > 0x2e7f));
        mem::w16(
            p + 6,
            wrap_after as u16 | (space as u16) << 1 | (nl as u16) << 2,
        );
    }
    0
}

/// The IDWriteFontFace of built-in face `i` (one object per face, kept).
fn face_obj(i: usize) -> u32 {
    static FACES: Mutex<Option<HashMap<usize, u32>>> = Mutex::new(None);
    *FACES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .entry(i)
        .or_insert_with(|| face(i as u32))
}

/// The glyph runs of a laid-out text: per line with characters, (face, glyph indices,
/// advances, first character, baseline origin relative to the layout's).
pub struct Run {
    pub face: usize,
    pub glyphs: Vec<u16>,
    pub adv: Vec<f32>,
    pub from: usize,
    pub to: usize,
    pub x: f32,
    pub y: f32,
}
pub fn runs(text: &[u16], laid: &Laid) -> Vec<Run> {
    let Some(font) = &laid.font else {
        return Vec::new();
    };
    let ch = chars(text);
    let mut v = Vec::new();
    for l in &laid.lines {
        // (trailing white space stays in the run, as DirectWrite draws it: blank glyphs)
        if l.to == l.from {
            continue;
        }
        let glyphs = ch[l.from..l.to]
            .iter()
            .map(|&k| font.glyph_index(k))
            .collect();
        v.push(Run {
            face: font.face(),
            glyphs,
            adv: laid.adv[l.from..l.to].to_vec(),
            from: l.from,
            to: l.to,
            x: l.x,
            y: l.baseline,
        });
    }
    v
}

fn Draw(c: &mut Ctx) -> u32 {
    // (this, client context, IDWriteTextRenderer, origin x, origin y): the renderer's
    // DrawGlyphRun for each run
    let (this, ctx, rend) = (c.a(0), c.a(1), c.a(2));
    let (ox, oy) = (fl(c, 3), fl(c, 4));
    let Some((text, fmt, w, h)) = layout_of(this) else {
        return E_INVALIDARG;
    };
    let laid = layout(&text, &fmt, w, h);
    let ch = chars(&text);
    // code unit offsets of characters
    let mut at = vec![0u32; ch.len() + 1];
    for (i, &k) in ch.iter().enumerate() {
        at[i + 1] = at[i] + units(k);
    }
    let draw_glyph_run = mem::rp(mem::rp(rend) + 6 * mem::ps());
    let ps = mem::ps();
    for r in runs(&text, &laid) {
        let n = r.glyphs.len() as u32;
        let nu = at[r.to] - at[r.from];
        // DWRITE_GLYPH_RUN, DWRITE_GLYPH_RUN_DESCRIPTION, then the arrays
        let (run_sz, desc_sz) = (4 * ps + 16, 4 * ps + 8);
        let size = run_sz + desc_sz + 2 * n + 4 * n + 8 * n + 2 * nu + 2 * nu + 16 + 32;
        let p = mem::heap_alloc(size, true);
        let desc = p + run_sz;
        let idx = (desc + desc_sz + 7) & !7;
        let adv = (idx + 2 * n + 3) & !3;
        let off = adv + 4 * n;
        let s = off + 8 * n;
        let map = s + 2 * nu + 2;
        let loc = (map + 2 * nu + 3) & !3;
        for (k, g) in r.glyphs.iter().enumerate() {
            mem::w16(idx + 2 * k as u32, *g);
            w32(adv + 4 * k as u32, r.adv[k].to_bits());
        }
        for (k, u) in text[at[r.from] as usize..at[r.to] as usize]
            .iter()
            .enumerate()
        {
            mem::w16(s + 2 * k as u32, *u);
        }
        let mut u = 0;
        for (k, &cp) in ch[r.from..r.to].iter().enumerate() {
            for _ in 0..units(cp) {
                mem::w16(map + 2 * u, k as u16);
                u += 1;
            }
        }
        for (k, b) in "ja-jp\0".encode_utf16().enumerate() {
            mem::w16(loc + 2 * k as u32, b);
        }
        let face = face_obj(r.face);
        mem::wp(p, face);
        w32(p + ps, fmt.size.to_bits());
        w32(p + ps + 4, n);
        mem::wp(p + ps + 8, idx);
        mem::wp(p + 2 * ps + 8, adv);
        mem::wp(p + 3 * ps + 8, off);
        // (sideways 0, bidi level 0: zeroed)
        mem::wp(desc, loc);
        mem::wp(desc + ps, s);
        w32(desc + 2 * ps, nu);
        mem::wp(desc + 3 * ps, map);
        w32(desc + 4 * ps, at[r.from]);
        // DrawGlyphRun(this, context, baseline x, baseline y, measuring mode, run, description, effect)
        let (bx, by) = (ox + r.x, oy + r.y);
        c.call_f(
            draw_glyph_run,
            &[rend, ctx, bx.to_bits(), by.to_bits(), 0, p, desc, 0],
            &[2, 3],
        );
        mem::heap_free(p);
    }
    0
}

// ---- rendering params, collections ----

fn CreateRenderingParams(c: &mut Ctx) -> u32 {
    // (CreateRenderingParams(this, out), CreateMonitorRenderingParams(this, monitor, out))
    let out = if is_monitor_call(c) { c.a(2) } else { c.a(1) };
    mem::putp(out, com::object("IDWriteRenderingParams3", &[], 8));
    0
}
fn is_monitor_call(c: &Ctx) -> bool {
    com::method_name(c.trap).is_some_and(|n| n == "CreateMonitorRenderingParams")
}
fn CreateCustomRenderingParams(c: &mut Ctx) -> u32 {
    // (this, gamma, contrast, [grayscale contrast,] cleartype level, geometry, mode, [grid fit,] out):
    // the out pointer is the last argument
    let n = com::method_dwords(c.trap).unwrap_or(8);
    mem::putp(c.a(n - 1), com::object("IDWriteRenderingParams3", &[], 8));
    0
}
fn GetSystemFontCollection(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), collection());
    0
}

// ---- the system font collection: gdi.rs's built-in faces, a family of one font each ----

const E_INVALIDARG: u32 = 0x8007_0057;
const E_NOT_SUFFICIENT_BUFFER: u32 = 0x8007_007a;

/// An object whose value (a face index) follows the vtable pointer.
fn with_value(iface: &'static str, methods: &[(&'static str, crate::api::Handler)], v: u32) -> u32 {
    let o = com::object(iface, methods, 16);
    w32(o + mem::ps(), v);
    o
}
fn value(o: u32) -> u32 {
    mem::r32(o + mem::ps())
}
/// The built-in face index of an IDWriteFontFace of ours.
pub fn face_index(face: u32) -> usize {
    value(face) as usize
}

fn collection() -> u32 {
    with_value(
        "IDWriteFontCollection",
        &[
            ("GetFontFamilyCount", GetFontFamilyCount),
            ("GetFontFamily", GetFontFamily),
            ("FindFamilyName", FindFamilyName),
            ("GetFontFromFontFace", GetFontFromFontFace),
        ],
        0,
    )
}
fn family(i: u32) -> u32 {
    let m: [(&'static str, crate::api::Handler); 6] = [
        ("GetFontCollection", GetFontCollection),
        ("GetFontCount", one),
        ("GetFont", GetFont),
        ("GetFamilyNames", GetFamilyNames),
        ("GetFirstMatchingFont", GetFirstMatchingFont),
        ("GetMatchingFonts", GetMatchingFonts),
    ];
    with_value("IDWriteFontFamily", &m, i)
}
fn font(i: u32) -> u32 {
    let m: [(&'static str, crate::api::Handler); 11] = [
        ("GetFontFamily", FontGetFontFamily),
        ("GetWeight", weight),
        ("GetStretch", five),
        ("GetStyle", zero),
        ("IsSymbolFont", zero),
        ("GetFaceNames", GetFaceNames),
        ("GetInformationalStrings", GetInformationalStrings),
        ("GetSimulations", zero),
        ("HasCharacter", HasCharacter),
        ("CreateFontFace", CreateFontFace),
        ("GetMetrics", FontGetMetrics),
    ];
    with_value("IDWriteFont", &m, i)
}
fn face(i: u32) -> u32 {
    let m: [(&'static str, crate::api::Handler); 8] = [
        ("GetType", FaceGetType),
        ("GetFiles", GetFiles),
        ("GetIndex", FaceGetIndex),
        ("GetSimulations", zero),
        ("IsSymbolFont", zero),
        ("GetMetrics", FontGetMetrics),
        ("GetGlyphIndices", GetGlyphIndices),
        ("GetGlyphRunOutline", GetGlyphRunOutline),
    ];
    with_value("IDWriteFontFace", &m, i)
}

fn one(_: &mut Ctx) -> u32 {
    1
}
fn weight(_: &mut Ctx) -> u32 {
    400
}

/// The built-in face a family name (Japanese or English, any case) names.
fn find_family(name: &str) -> Option<usize> {
    (0..crate::gdi::sys_faces()).find(|&i| {
        crate::gdi::sys_face_names(i)
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name))
    })
}

fn GetFontFamilyCount(_: &mut Ctx) -> u32 {
    crate::gdi::sys_faces() as u32
}
fn GetFontFamily(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if i as usize >= crate::gdi::sys_faces() {
        mem::putp(out, 0);
        return E_INVALIDARG;
    }
    mem::putp(out, family(i));
    0
}
fn FindFamilyName(c: &mut Ctx) -> u32 {
    let name = String::from_utf16_lossy(&wstr(c.a(1), None));
    let i = find_family(&name);
    w32(c.a(2), i.map_or(u32::MAX, |i| i as u32));
    w32(c.a(3), i.is_some() as u32);
    0
}
fn GetFontFromFontFace(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), font(value(c.a(1))));
    0
}
fn GetFontCollection(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), collection());
    0
}
fn GetFont(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if i != 0 {
        mem::putp(out, 0);
        return E_INVALIDARG;
    }
    mem::putp(out, font(value(c.a(0))));
    0
}
fn GetFamilyNames(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), strings(family_names(value(c.a(0)) as usize)));
    0
}
/// (locale, name): the English name, then the Japanese one when the face has one
fn family_names(i: usize) -> Vec<(String, String)> {
    let n = crate::gdi::sys_face_names(i);
    // (a face without a Japanese name has its English one under ja-jp too, as MS UI Gothic)
    vec![
        ("en-us".to_string(), n[n.len() - 1].to_string()),
        ("ja-jp".to_string(), n[0].to_string()),
    ]
}
fn GetFirstMatchingFont(c: &mut Ctx) -> u32 {
    // (this, weight, stretch, style, out)
    mem::putp(c.a(4), font(value(c.a(0))));
    0
}
fn GetMatchingFonts(c: &mut Ctx) -> u32 {
    let m: [(&'static str, crate::api::Handler); 3] = [
        ("GetFontCollection", GetFontCollection),
        ("GetFontCount", one),
        ("GetFont", GetFont),
    ];
    mem::putp(c.a(4), with_value("IDWriteFontList", &m, value(c.a(0))));
    0
}
fn FontGetFontFamily(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), family(value(c.a(0))));
    0
}
fn GetFaceNames(c: &mut Ctx) -> u32 {
    mem::putp(
        c.a(1),
        strings(vec![
            ("en-us".to_string(), "Regular".to_string()),
            ("ja-jp".to_string(), "標準".to_string()),
        ]),
    );
    0
}
fn GetInformationalStrings(c: &mut Ctx) -> u32 {
    // (this, id, out strings, out exists)
    mem::putp(c.a(2), 0);
    w32(c.a(3), 0);
    0
}
fn HasCharacter(c: &mut Ctx) -> u32 {
    let has = crate::gdi::sys_face_has(value(c.a(0)) as usize, c.a(1));
    w32(c.a(2), has as u32);
    0
}
fn CreateFontFace(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), face(value(c.a(0))));
    0
}
/// DWRITE_FONT_METRICS in design units, from the face's tables.
fn FontGetMetrics(c: &mut Ctx) -> u32 {
    let (i, p) = (value(c.a(0)) as usize, c.a(1));
    let Some(m) = crate::gdi::sys_face_design_metrics(i) else {
        return 0;
    };
    for (k, v) in m.iter().enumerate() {
        mem::w16(p + 2 * k as u32, *v);
    }
    0
}
fn GetGlyphIndices(c: &mut Ctx) -> u32 {
    // (this, code points, count, out glyph indices)
    let (cp, n, out) = (c.a(1), c.a(2), c.a(3));
    let Some(f) = crate::gdi::face_font(value(c.a(0)) as usize, 16.0) else {
        return E_INVALIDARG;
    };
    for i in 0..n {
        mem::w16(out + 2 * i, f.glyph_index(mem::r32(cp + 4 * i)));
    }
    0
}
/// GetGlyphRunOutline(this, em size, glyph indices, advances, offsets, count, sideways, right
/// to left, geometry sink): into a Direct2D path geometry of ours, the glyphs themselves (with
/// their pen positions from the baseline origin)
fn GetGlyphRunOutline(c: &mut Ctx) -> u32 {
    let (face, em) = (value(c.a(0)) as usize, fl(c, 1));
    let (idx, adv, off, n, rtl, sink) = (c.a(2), c.a(3), c.a(4), c.a(5), c.a(7), c.a(8));
    let font = if adv == 0 {
        crate::gdi::face_font(face, em)
    } else {
        None
    };
    let mut pen = 0f32;
    let mut v = Vec::with_capacity(n as usize);
    for i in 0..n {
        let gi = mem::r16(idx + 2 * i);
        let a = if adv != 0 {
            f32::from_bits(mem::r32(adv + 4 * i))
        } else {
            font.as_ref().map_or(em, |f| f.index_advance(gi))
        };
        let (dx, dy) = if off != 0 {
            (
                f32::from_bits(mem::r32(off + 8 * i)),
                f32::from_bits(mem::r32(off + 8 * i + 4)),
            )
        } else {
            (0.0, 0.0)
        };
        let x = if rtl != 0 { -(pen + a) - dx } else { pen + dx };
        v.push((face, em, gi, x, -dy));
        pen += a;
    }
    if !crate::d2d::sink_glyphs(sink, v) {
        com::warn_once("sake: dwrite GetGlyphRunOutline into a sink of the game's own: no outline");
    }
    0
}
fn FaceGetType(c: &mut Ctx) -> u32 {
    // DWRITE_FONT_FACE_TYPE_TRUETYPE 1, _TRUETYPE_COLLECTION 2
    if crate::gdi::sys_face_file(value(c.a(0)) as usize)
        .0
        .ends_with(".ttc")
    {
        2
    } else {
        1
    }
}
fn FaceGetIndex(c: &mut Ctx) -> u32 {
    crate::gdi::sys_face_file(value(c.a(0)) as usize).1
}
fn GetFiles(c: &mut Ctx) -> u32 {
    // (this, in/out count, files)
    let (i, n, out) = (value(c.a(0)), c.a(1), c.a(2));
    if out == 0 {
        w32(n, 1);
        return 0;
    }
    if mem::r32(n) < 1 {
        return E_INVALIDARG;
    }
    let path: Vec<u16> = crate::gdi::sys_face_file(i as usize)
        .0
        .encode_utf16()
        .chain([0])
        .collect();
    let key = mem::heap_alloc(2 * path.len() as u32, false);
    for (k, ch) in path.iter().enumerate() {
        mem::w16(key + 2 * k as u32, *ch);
    }
    let m: [(&'static str, crate::api::Handler); 3] = [
        ("GetReferenceKey", GetReferenceKey),
        ("GetLoader", GetLoader),
        ("Analyze", Analyze),
    ];
    let f = with_value("IDWriteFontFile", &m, i);
    w32(f + mem::ps() + 4, key);
    mem::putp(out, f);
    w32(n, 1);
    0
}
fn GetReferenceKey(c: &mut Ctx) -> u32 {
    // (this, out key, out size): the key is the file's path
    let key = mem::r32(c.a(0) + mem::ps() + 4);
    mem::putp(c.a(1), key);
    w32(c.a(2), 2 * (wstr(key, None).len() as u32 + 1));
    0
}
fn GetLoader(c: &mut Ctx) -> u32 {
    static LOADER: Mutex<u32> = Mutex::new(0);
    let mut g = LOADER.lock().unwrap();
    if *g == 0 {
        let m: [(&'static str, crate::api::Handler); 3] = [
            ("GetFilePathLengthFromKey", GetFilePathLengthFromKey),
            ("GetFilePathFromKey", GetFilePathFromKey),
            ("GetLastWriteTimeFromKey", GetLastWriteTimeFromKey),
        ];
        *g = com::object("IDWriteLocalFontFileLoader", &m, 8);
    }
    com::add_ref_obj(*g);
    mem::putp(c.a(1), *g);
    0
}
fn Analyze(c: &mut Ctx) -> u32 {
    // (this, out supported, out file type, out face type, out faces)
    let i = value(c.a(0)) as usize;
    let file = crate::gdi::sys_face_file(i).0;
    let ttc = file.ends_with(".ttc");
    let faces = (0..crate::gdi::sys_faces())
        .filter(|&k| crate::gdi::sys_face_file(k).0 == file)
        .count() as u32;
    w32(c.a(1), 1);
    w32(c.a(2), if ttc { 3 } else { 2 });
    w32(c.a(3), if ttc { 2 } else { 1 });
    w32(c.a(4), faces);
    0
}
fn GetFilePathLengthFromKey(c: &mut Ctx) -> u32 {
    // (this, key, key size, out length without the terminator)
    w32(c.a(3), wstr(c.a(1), None).len() as u32);
    0
}
fn GetFilePathFromKey(c: &mut Ctx) -> u32 {
    // (this, key, key size, buffer, buffer size with the terminator)
    let (path, buf, n) = (wstr(c.a(1), None), c.a(3), c.a(4));
    if (n as usize) < path.len() + 1 {
        return E_NOT_SUFFICIENT_BUFFER;
    }
    mem::put_wstr(buf, n, &path);
    0
}
fn GetLastWriteTimeFromKey(c: &mut Ctx) -> u32 {
    mem::put64(c.a(3), 0);
    0
}

// ---- localized strings ----

static STRINGS: Mutex<Option<HashMap<u32, Vec<(String, String)>>>> = Mutex::new(None);

fn strings(v: Vec<(String, String)>) -> u32 {
    let m: [(&'static str, crate::api::Handler); 6] = [
        ("GetCount", StrGetCount),
        ("FindLocaleName", FindLocaleName),
        ("GetLocaleNameLength", GetLocaleNameLength),
        ("GetLocaleName", StrGetLocaleName),
        ("GetStringLength", GetStringLength),
        ("GetString", GetString),
    ];
    let o = com::object("IDWriteLocalizedStrings", &m, 8);
    STRINGS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(o, v);
    o
}
fn strings_of(o: u32) -> Vec<(String, String)> {
    STRINGS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&o).cloned())
        .unwrap_or_default()
}
fn StrGetCount(c: &mut Ctx) -> u32 {
    strings_of(c.a(0)).len() as u32
}
fn FindLocaleName(c: &mut Ctx) -> u32 {
    // (this, locale, out index, out exists)
    let want = String::from_utf16_lossy(&wstr(c.a(1), None));
    let i = strings_of(c.a(0))
        .iter()
        .position(|(l, _)| l.eq_ignore_ascii_case(&want));
    w32(c.a(2), i.map_or(u32::MAX, |i| i as u32));
    w32(c.a(3), i.is_some() as u32);
    0
}
/// (this, index, out length) / (this, index, buffer, size): the locale (`name` false) or the
/// string of entry `index`
fn str_length(c: &mut Ctx, name: bool) -> u32 {
    let v = strings_of(c.a(0));
    let Some(e) = v.get(c.a(1) as usize) else {
        w32(c.a(2), 0);
        return E_INVALIDARG;
    };
    w32(
        c.a(2),
        if name { &e.1 } else { &e.0 }.encode_utf16().count() as u32,
    );
    0
}
fn str_get(c: &mut Ctx, name: bool) -> u32 {
    let v = strings_of(c.a(0));
    let (buf, n) = (c.a(2), c.a(3));
    let Some(e) = v.get(c.a(1) as usize) else {
        mem::put_wstr(buf, n, &[]);
        return E_INVALIDARG;
    };
    let s: Vec<u16> = if name { &e.1 } else { &e.0 }.encode_utf16().collect();
    if (n as usize) < s.len() + 1 {
        mem::put_wstr(buf, n, &[]);
        return E_NOT_SUFFICIENT_BUFFER;
    }
    mem::put_wstr(buf, n, &s);
    0
}
fn GetLocaleNameLength(c: &mut Ctx) -> u32 {
    str_length(c, false)
}
fn StrGetLocaleName(c: &mut Ctx) -> u32 {
    str_get(c, false)
}
fn GetStringLength(c: &mut Ctx) -> u32 {
    str_length(c, true)
}
fn GetString(c: &mut Ctx) -> u32 {
    str_get(c, true)
}

// ---- font fallback ----

fn GetSystemFontFallback(c: &mut Ctx) -> u32 {
    mem::putp(
        c.a(1),
        com::object(
            "IDWriteFontFallback",
            &[("MapCharacters", MapCharacters)],
            8,
        ),
    );
    0
}
fn CreateNumberSubstitution(c: &mut Ctx) -> u32 {
    // (this, method, locale, ignore user override, out)
    mem::putp(c.a(4), com::object("IDWriteNumberSubstitution", &[], 8));
    0
}

/// The faces fallback tries after the requested family, in order (the UI face first, as
/// Japanese Windows falls back to its UI font).
const FALLBACK: [&str; 5] = [
    "MS UI Gothic",
    "MS PGothic",
    "MS Gothic",
    "MS PMincho",
    "MS Mincho",
];

/// MapCharacters' choice for the start of `text`: how many code units one face covers and
/// that face (None: a run no face has). A character goes to the requested face when it has it,
/// else to the first fallback face that does; white space and controls stay with the run.
fn map_chars(
    text: &[u16],
    requested: Option<usize>,
    has: &dyn Fn(usize, u32) -> bool,
) -> (usize, Option<usize>) {
    let order: Vec<usize> = requested
        .into_iter()
        .chain(FALLBACK.iter().filter_map(|n| find_family(n)))
        .collect();
    let pick = |ch: u32| order.iter().copied().find(|&i| has(i, ch));
    let neutral = |ch: u32| ch < 0x20 || ch == 0x20 || ch == 0x3000 || ch == 0xa0;
    let mut chars =
        char::decode_utf16(text.iter().copied()).map(|r| r.map_or(0xfffd, |c| c as u32));
    let Some(first) = chars.next() else {
        return (0, None);
    };
    let units = |ch: u32| if ch > 0xffff { 2 } else { 1 };
    let chosen = pick(first);
    let mut n = units(first);
    for ch in chars {
        let same = match chosen {
            Some(f) => (neutral(ch) && has(f, ch)) || pick(ch) == Some(f),
            None => pick(ch).is_none(),
        };
        if !same {
            break;
        }
        n += units(ch);
    }
    (n, chosen)
}

fn MapCharacters(c: &mut Ctx) -> u32 {
    // (this, source, position, length, collection, family, weight, style, stretch,
    //  out mapped length, out font, out scale)
    let (src, pos, len) = (c.a(1), c.a(2), c.a(3));
    let family = String::from_utf16_lossy(&wstr(c.a(5), None));
    let (out_len, out_font, out_scale) = (c.a(9), c.a(10), c.a(11));
    // the text, from the source's GetTextAtPosition(position, out text, out length)
    let get_text = mem::rp(mem::rp(src) + 3 * mem::ps());
    let tmp = mem::heap_alloc(16, true);
    let mut text = Vec::new();
    while (text.len() as u32) < len {
        c.call(get_text, &[src, pos + text.len() as u32, tmp, tmp + 8]);
        let (p, n) = (mem::rp(tmp), mem::r32(tmp + 8));
        if p == 0 || n == 0 {
            break;
        }
        text.extend(wstr(p, Some(n.min(len - text.len() as u32))));
    }
    mem::heap_free(tmp);
    let (n, f) = map_chars(&text, find_family(&family), &|i, ch| {
        crate::gdi::sys_face_has(i, ch)
    });
    w32(out_len, if text.is_empty() { len } else { n as u32 });
    mem::putp(out_font, f.map_or(0, |i| font(i as u32)));
    w32(out_scale, f.map_or(0.0f32, |_| 1.0).to_bits());
    0
}

// ---- layout ----

pub struct Line {
    /// the characters [from, to)
    pub from: usize,
    pub to: usize,
    /// left edge and top of the line, width without and with trailing white space
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub width_ws: f32,
    pub baseline: f32,
    /// the newline characters after `to` (0, 1 or 2 for CR LF)
    pub nl: usize,
}

pub struct Laid {
    pub font: Option<TextFont>,
    pub lines: Vec<Line>,
    /// each character's pen x within its line
    pub pen: Vec<f32>,
    /// each character's advance (0 for newlines)
    pub adv: Vec<f32>,
    pub height: f32,
    /// the baseline below a line's top
    pub base: f32,
}

fn is_space(ch: u16) -> bool {
    ch == b' ' as u16 || ch == 0x3000 || ch == b'\t' as u16
}

/// Lays `text` out in a `w` x `h` box (0 width: no wrapping).
pub fn layout(text: &[u16], fmt: &Format, w: f32, h: f32) -> Laid {
    layout_with(
        text,
        fmt,
        w,
        h,
        text_font(&fmt.family, fmt.size, fmt.weight),
    )
}
/// layout() in `font` (None: every advance the em size)
fn layout_with(text: &[u16], fmt: &Format, w: f32, h: f32, font: Option<TextFont>) -> Laid {
    let (asc, desc, gap) = font
        .as_ref()
        .map_or((fmt.size * 0.86, fmt.size * 0.14, 0.0), |f| f.metrics());
    let (line_h, base) = fmt.spacing.unwrap_or((asc + desc + gap, asc));
    let chars: Vec<u32> = char::decode_utf16(text.iter().copied())
        .map(|r| r.map_or(0xfffd, |c| c as u32))
        .collect();
    // (a surrogate pair is one char: map back to code units is not needed by the drawers)
    let (lead, trail, min_adv) = fmt.char_spacing;
    let adv: Vec<f32> = chars
        .iter()
        .map(|&ch| {
            if ch == 10 || ch == 13 {
                0.0
            } else {
                (lead + font.as_ref().map_or(fmt.size, |f| f.advance(ch)) + trail).max(min_adv)
            }
        })
        .collect();
    let wrap = fmt.wrap != 1 && w > 0.0;
    let n = chars.len();
    let mut lines = Vec::new();
    let mut pen = vec![0f32; n];
    let mut i = 0;
    loop {
        let start = i;
        let (mut x, mut end) = (0.0, i);
        let next = loop {
            if end == n {
                break n;
            }
            let ch = chars[end];
            if ch == 10 || ch == 13 {
                break end
                    + if ch == 13 && chars.get(end + 1) == Some(&10) {
                        2
                    } else {
                        1
                    };
            }
            if wrap && end > start && x + adv[end] > w + 0.01 && !is_space(ch as u16) {
                break end;
            }
            pen[end] = x;
            x += adv[end];
            end += 1;
        };
        let mut trim = end;
        while trim > start && is_space(chars[trim - 1] as u16) {
            trim -= 1;
        }
        let width = if trim > start {
            pen[trim - 1] + adv[trim - 1]
        } else {
            0.0
        };
        lines.push(Line {
            from: start,
            to: end,
            x: 0.0,
            top: 0.0,
            width,
            width_ws: x,
            baseline: 0.0,
            nl: next - end,
        });
        if next >= n {
            // (text ending in a newline has an empty last line)
            if next > end {
                lines.push(Line {
                    from: n,
                    to: n,
                    x: 0.0,
                    top: 0.0,
                    width: 0.0,
                    width_ws: 0.0,
                    baseline: 0.0,
                    nl: 0,
                });
            }
            break;
        }
        i = next;
    }
    let height = line_h * lines.len() as f32;
    let y0 = match fmt.para {
        1 => h - height,
        2 => (h - height) / 2.0,
        _ => 0.0,
    };
    for (k, l) in lines.iter_mut().enumerate() {
        l.x = match fmt.align {
            1 => w - l.width,
            2 => (w - l.width) / 2.0,
            _ => 0.0,
        };
        l.top = y0 + line_h * k as f32;
        l.baseline = l.top + base;
    }
    Laid {
        font,
        lines,
        pen,
        adv,
        height,
        base,
    }
}

/// The characters of `text` as layout() counts them.
pub fn chars(text: &[u16]) -> Vec<u32> {
    char::decode_utf16(text.iter().copied())
        .map(|r| r.map_or(0xfffd, |c| c as u32))
        .collect()
}

/// A text layout object's text, format and box.
pub fn layout_of(obj: u32) -> Option<(Vec<u16>, Format, f32, f32)> {
    layouts(|m| {
        m.get(&obj)
            .map(|l| (l.text.clone(), l.fmt.clone(), l.max_w, l.max_h))
    })
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    // (no font: every advance is the em size, 10)
    fn fmt(cs: (f32, f32, f32)) -> Format {
        Format {
            family: "no such face".into(),
            size: 10.0,
            weight: 400,
            align: 0,
            para: 0,
            wrap: 1,
            spacing: Some((12.0, 9.0)),
            char_spacing: cs,
        }
    }
    fn metrics(s: &str, w: f32, f: &Format) -> Vec<(u32, u32, u32, f32, f32)> {
        let t: Vec<u16> = s.encode_utf16().collect();
        let laid = layout_with(&t, f, w, 0.0, None);
        line_metrics(&chars(&t), &laid, line_height(f, &laid))
    }

    #[test]
    fn line_metrics_count_newlines_and_trailing_space_in_code_units() {
        let f = fmt((0.0, 0.0, 0.0));
        // (length, trailing white space, newline, height, baseline)
        assert_eq!(
            metrics("ab\ncd", 0.0, &f),
            vec![(3, 1, 1, 12.0, 9.0), (2, 0, 0, 12.0, 9.0)]
        );
        assert_eq!(
            metrics("a  \r\nb", 0.0, &f),
            vec![(5, 4, 2, 12.0, 9.0), (1, 0, 0, 12.0, 9.0)]
        );
        // a surrogate pair is two code units; text ending in a newline has an empty last line
        assert_eq!(
            metrics("\u{1f600}\n", 0.0, &f),
            vec![(3, 1, 1, 12.0, 9.0), (0, 0, 0, 12.0, 9.0)]
        );
        assert_eq!(metrics("", 0.0, &f), vec![(0, 0, 0, 12.0, 9.0)]);
    }

    #[test]
    fn wrapped_lines_have_no_newline() {
        let mut f = fmt((0.0, 0.0, 0.0));
        f.wrap = 0;
        assert_eq!(
            metrics("abcd", 20.0, &f),
            vec![(2, 0, 0, 12.0, 9.0), (2, 0, 0, 12.0, 9.0)]
        );
    }

    #[test]
    fn character_spacing_widens_each_advance() {
        let t: Vec<u16> = "ab".encode_utf16().collect();
        let laid = layout_with(&t, &fmt((1.0, 2.0, 0.0)), 0.0, 0.0, None);
        assert_eq!(laid.adv, vec![13.0, 13.0]);
        assert_eq!(laid.lines[0].width, 26.0);
        // the minimum advance wins over a smaller one; negative spacing narrows
        assert_eq!(
            layout_with(&t, &fmt((0.0, 0.0, 15.0)), 0.0, 0.0, None).adv,
            vec![15.0, 15.0]
        );
        assert_eq!(
            layout_with(&t, &fmt((-2.0, 0.0, 0.0)), 0.0, 0.0, None).adv,
            vec![8.0, 8.0]
        );
    }

    proptest::proptest! {
        /// The lines' lengths add up to the text's code units, trailing white space and the
        /// newline are parts of the line, and every line has the format's height.
        #[test]
        fn line_lengths_cover_the_text(s in proptest::collection::vec(proptest::prop_oneof![
            proptest::char::range('a', 'c'), proptest::strategy::Just(' '), proptest::strategy::Just('\n'),
            proptest::strategy::Just('\r'), proptest::strategy::Just('\u{1f600}')], 0..16), w in 0f32..60.0) {
            let text: String = s.into_iter().collect();
            let mut f = fmt((0.0, 0.0, 0.0));
            f.wrap = 0;
            let m = metrics(&text, w, &f);
            proptest::prop_assert_eq!(m.iter().map(|l| l.0).sum::<u32>(), text.encode_utf16().count() as u32);
            for l in &m {
                proptest::prop_assert!(l.2 <= l.1 && l.1 <= l.0);
                proptest::prop_assert_eq!((l.3, l.4), (12.0, 9.0));
            }
        }
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    // stand-in faces: 0 MS Gothic, 1 MS PGothic, 2 MS Mincho, 3 MS PMincho, 4 MS UI Gothic
    // (gdi.rs's order); `has` decides what each covers
    fn run(
        s: &str,
        req: Option<usize>,
        has: &dyn Fn(usize, u32) -> bool,
    ) -> (usize, Option<usize>) {
        map_chars(&s.encode_utf16().collect::<Vec<_>>(), req, has)
    }
    fn latin_only(i: usize, ch: u32) -> bool {
        i == 9 && ch < 0x80 || i != 9 && ch != 0x1f600
    }

    #[test]
    fn requested_face_keeps_what_it_has() {
        let all = |_: usize, ch: u32| ch != 0x1f600;
        assert_eq!(run("abcあいう", Some(0), &all), (6, Some(0)));
        // no face named: the UI face
        assert_eq!(run("あいう", None, &all), (3, Some(4)));
    }

    #[test]
    fn runs_split_where_the_face_changes() {
        // a requested face (9) with Latin only: the Latin run, then the Japanese one
        let has = |i: usize, ch: u32| latin_only(i, ch);
        assert_eq!(run("ab あい", Some(9), &has), (3, Some(9)));
        assert_eq!(run("あい ab", Some(9), &has), (3, Some(4)));
        // a space inside a Japanese run stays with it
        assert_eq!(run("あ い", Some(9), &has), (3, Some(4)));
    }

    #[test]
    fn characters_no_face_has() {
        let all = |_: usize, ch: u32| ch != 0x1f600;
        // a surrogate pair is two units; a run of them maps to no font
        assert_eq!(run("\u{1f600}\u{1f600}あ", Some(0), &all), (4, None));
        assert_eq!(run("あ\u{1f600}", Some(0), &all), (1, Some(0)));
        assert_eq!(run("", Some(0), &all), (0, None));
    }

    proptest::proptest! {
        /// The mapped length is never 0 for text, never passes the text, and every
        /// non-neutral character of the run is one the chosen face has (and none has, for a
        /// run without a face).
        #[test]
        fn mapped_runs_are_consistent(s in proptest::collection::vec(proptest::prop_oneof![
            proptest::char::range('a', 'e'), proptest::char::range('あ', 'お'), proptest::strategy::Just(' '), proptest::strategy::Just('\u{1f600}')], 0..12),
            req in proptest::option::of(0usize..10)) {
            let text: String = s.into_iter().collect();
            let u: Vec<u16> = text.encode_utf16().collect();
            let has = |i: usize, ch: u32| latin_only(i, ch);
            let (n, f) = map_chars(&u, req, &has);
            proptest::prop_assert!(n <= u.len());
            proptest::prop_assert_eq!(n == 0, u.is_empty());
            for ch in char::decode_utf16(u[..n].iter().copied()).map(|r| r.unwrap() as u32) {
                match f {
                    Some(f) => proptest::prop_assert!(has(f, ch)),
                    None => proptest::prop_assert!((0..10).all(|i| !has(i, ch))),
                }
            }
        }
    }
}

pub fn init() {
    implement("dwrite", "DWriteCreateFactory", DWriteCreateFactory);
}
