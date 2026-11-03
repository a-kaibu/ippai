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
    f32::from_bits(c.a(i))
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
        ("CreateGdiCompatibleTextLayout", CreateGdiCompatibleTextLayout),
        ("CreateRenderingParams", CreateRenderingParams),
        ("CreateMonitorRenderingParams", CreateRenderingParams),
        ("CreateCustomRenderingParams", CreateCustomRenderingParams),
        ("GetSystemFontCollection", GetSystemFontCollection),
    ]
}

static SHARED: Mutex<u32> = Mutex::new(0);

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
    let fmt = Format { family, size: fl(c, 6), weight: c.a(3) as i32, align: 0, para: 0, wrap: 0, spacing: None };
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
    set_format(c.a(0), |f| f.spacing = if m == 1 { Some((s, b)) } else { None });
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
    let name: Vec<u16> = format_of(c.a(0)).map_or(Vec::new(), |f| f.family.encode_utf16().collect());
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
    ]);
    v
}

fn new_layout(text: Vec<u16>, fmt: u32, w: f32, h: f32, out: u32) -> u32 {
    let Some(fmt) = format_of(fmt) else {
        mem::putp(out, 0);
        return 0x8007_0057;
    };
    let o = com::object("IDWriteTextLayout4", &layout_methods(), 8);
    layouts(|m| m.insert(o, Layout { fmt, text, max_w: w, max_h: h }));
    mem::putp(out, o);
    0
}

fn CreateTextLayout(c: &mut Ctx) -> u32 {
    // (this, string, length, format, max width, max height, out)
    new_layout(wstr(c.a(1), Some(c.a(2))), c.a(3), fl(c, 4), fl(c, 5), c.a(6))
}
fn CreateGdiCompatibleTextLayout(c: &mut Ctx) -> u32 {
    // (this, string, length, format, width, height, pixels per dip, transform, natural, out)
    new_layout(wstr(c.a(1), Some(c.a(2))), c.a(3), fl(c, 4), fl(c, 5), c.a(9))
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
    let Some((text, fmt, w, h)) = layouts(|m| m.get(&this).map(|l| (l.text.clone(), l.fmt.clone(), l.max_w, l.max_h))) else { return 0x8007_0057 };
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
    let vals = [x0, top, (x1 - x0).max(0.0), (x1ws - x0).max(0.0), r.height, w, h];
    for (i, v) in vals.iter().enumerate() {
        w32(p + 4 * i as u32, v.to_bits());
    }
    w32(p + 28, 0);
    w32(p + 32, r.lines.len() as u32);
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
    mem::putp(c.a(1), com::object("IDWriteFontCollection", &[], 8));
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
}

pub struct Laid {
    pub font: Option<TextFont>,
    pub lines: Vec<Line>,
    /// each character's pen x within its line
    pub pen: Vec<f32>,
    pub height: f32,
}

fn is_space(ch: u16) -> bool {
    ch == b' ' as u16 || ch == 0x3000 || ch == b'\t' as u16
}

/// Lays `text` out in a `w` x `h` box (0 width: no wrapping).
pub fn layout(text: &[u16], fmt: &Format, w: f32, h: f32) -> Laid {
    let font = text_font(&fmt.family, fmt.size, fmt.weight);
    let (asc, desc, gap) = font.as_ref().map_or((fmt.size * 0.86, fmt.size * 0.14, 0.0), |f| f.metrics());
    let (line_h, base) = fmt.spacing.unwrap_or((asc + desc + gap, asc));
    let chars: Vec<u32> = char::decode_utf16(text.iter().copied()).map(|r| r.map_or(0xfffd, |c| c as u32)).collect();
    // (a surrogate pair is one char: map back to code units is not needed by the drawers)
    let adv: Vec<f32> = chars.iter().map(|&ch| if ch == 10 || ch == 13 { 0.0 } else { font.as_ref().map_or(fmt.size, |f| f.advance(ch)) }).collect();
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
                break end + if ch == 13 && chars.get(end + 1) == Some(&10) { 2 } else { 1 };
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
        let width = if trim > start { pen[trim - 1] + adv[trim - 1] } else { 0.0 };
        lines.push(Line { from: start, to: end, x: 0.0, top: 0.0, width, width_ws: x, baseline: 0.0 });
        if next >= n {
            // (text ending in a newline has an empty last line)
            if next > end {
                lines.push(Line { from: n, to: n, x: 0.0, top: 0.0, width: 0.0, width_ws: 0.0, baseline: 0.0 });
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
    Laid { font, lines, pen, height }
}

/// The characters of `text` as layout() counts them.
pub fn chars(text: &[u16]) -> Vec<u32> {
    char::decode_utf16(text.iter().copied()).map(|r| r.map_or(0xfffd, |c| c as u32)).collect()
}

/// A text layout object's text, format and box.
pub fn layout_of(obj: u32) -> Option<(Vec<u16>, Format, f32, f32)> {
    layouts(|m| m.get(&obj).map(|l| (l.text.clone(), l.fmt.clone(), l.max_w, l.max_h)))
}

pub fn init() {
    implement("dwrite", "DWriteCreateFactory", DWriteCreateFactory);
}
