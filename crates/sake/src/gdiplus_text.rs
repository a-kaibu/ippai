//! GDI+ text and simple drawing (gdiplus.dll): Graphics over a GDI+ bitmap (or a window/DC,
//! for measuring only), fonts from a LOGFONT, solid brushes and pens, string formats with
//! measurable character ranges, rectangle regions; DrawString, MeasureString,
//! MeasureCharacterRanges, FillRectangle, DrawLine. Glyphs come from gdi.rs's fonts
//! (gdi::text_font), grayscale antialiased, composited source-over into the bitmap.
//!
//! Layout follows GDI+'s default (non-typographic) string format: lines of the font's line
//! spacing, a leading and trailing padding of 1/6 em, wrapping at the layout width unless
//! StringFormatFlagsNoWrap, '\n' breaking lines.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx, Handler};
use crate::gdi::{LogFont, TextFont};
use crate::mem::{self, put32, r32, w32};
use crate::HashMap;
use std::sync::Mutex;

const OK: u32 = 0;
const INVALID_PARAMETER: u32 = 2;

enum Obj {
    /// target bitmap (0: a window or DC, measuring only), page unit and scale
    Graphics { img: u32, unit: u32, scale: f32 },
    Font { lf: LogFont, em: f32 },
    Brush { argb: u32 },
    Pen { argb: u32, width: f32 },
    Format { flags: u32, align: u32, ranges: Vec<(i32, i32)> },
    /// a rectangle (x, y, w, h) in world units; empty when w or h is 0
    Region { r: [f32; 4] },
}

static OBJS: Mutex<Option<HashMap<u32, Obj>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Obj>) -> R) -> R {
    f(OBJS.lock().unwrap().get_or_insert_with(HashMap::default))
}
fn new_obj(o: Obj, out: u32) -> u32 {
    if out == 0 {
        return INVALID_PARAMETER;
    }
    let h = mem::heap_alloc(16, true);
    with(|m| m.insert(h, o));
    mem::putp(out, h);
    OK
}
fn delete(c: &mut Ctx) -> u32 {
    if with(|m| m.remove(&c.a(0))).is_some() {
        mem::heap_free(c.a(0));
    }
    OK
}
fn ok(_: &mut Ctx) -> u32 {
    OK
}
fn fl(v: u32) -> f32 {
    f32::from_bits(v)
}

// ---- graphics ----

fn GdipCreateFromHDC(c: &mut Ctx) -> u32 {
    new_obj(Obj::Graphics { img: 0, unit: 1, scale: 1.0 }, c.a(1))
}
fn GdipCreateFromHWND(c: &mut Ctx) -> u32 {
    new_obj(Obj::Graphics { img: 0, unit: 1, scale: 1.0 }, c.a(1))
}
fn GdipGetImageGraphicsContext(c: &mut Ctx) -> u32 {
    if !crate::gdiplus::is_image(c.a(0)) {
        return INVALID_PARAMETER;
    }
    new_obj(Obj::Graphics { img: c.a(0), unit: 1, scale: 1.0 }, c.a(1))
}
fn GdipSetPageUnit(c: &mut Ctx) -> u32 {
    let u = c.a(1);
    with(|m| match m.get_mut(&c.a(0)) {
        Some(Obj::Graphics { unit, .. }) => {
            *unit = u;
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
fn GdipSetPageScale(c: &mut Ctx) -> u32 {
    let s = fl(c.a(1));
    with(|m| match m.get_mut(&c.a(0)) {
        Some(Obj::Graphics { scale, .. }) if s > 0.0 => {
            *scale = s;
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
/// Pixels per world unit: the page unit (World/Display/Pixel 1, Point 96/72, Inch 96,
/// Document 96/300, Millimeter 96/25.4) times the page scale.
fn px_per_unit(unit: u32, scale: f32) -> f32 {
    let u = match unit {
        3 => 96.0 / 72.0,
        4 => 96.0,
        5 => 96.0 / 300.0,
        6 => 96.0 / 25.4,
        _ => 1.0,
    };
    u * scale
}
fn graphics(g: u32) -> Option<(u32, f32)> {
    with(|m| match m.get(&g) {
        Some(Obj::Graphics { img, unit, scale }) => Some((*img, px_per_unit(*unit, *scale))),
        _ => None,
    })
}

// ---- fonts, brushes, pens, formats ----

/// GdipCreateFontFromLogfont{A,W}(hdc, LOGFONT*, &font): its em in pixels (a negative
/// lfHeight is the em, a positive one the cell height)
fn font_from_logfont(c: &mut Ctx, wide: bool) -> u32 {
    let lf = crate::gdi::read_logfont(c.a(1), wide);
    let h = if lf.height == 0 { 16 } else { lf.height };
    let em = if h < 0 {
        -h as f32
    } else {
        crate::gdi::text_font(&lf.face, h as f32, lf.weight).map_or(h as f32, |f| {
            let (a, d, _) = f.metrics();
            h as f32 * h as f32 / (a + d).max(1.0)
        })
    };
    new_obj(Obj::Font { lf, em }, c.a(2))
}
fn GdipCreateFontFromLogfontA(c: &mut Ctx) -> u32 {
    font_from_logfont(c, false)
}
fn GdipCreateFontFromLogfontW(c: &mut Ctx) -> u32 {
    font_from_logfont(c, true)
}
fn font(f: u32) -> Option<(LogFont, f32)> {
    with(|m| match m.get(&f) {
        Some(Obj::Font { lf, em }) => Some((lf.clone(), *em)),
        _ => None,
    })
}
fn text_font(f: u32) -> Option<(TextFont, f32)> {
    let (lf, em) = font(f)?;
    Some((crate::gdi::text_font(&lf.face, em, lf.weight.max(400))?, em))
}
fn get_logfont(c: &mut Ctx, wide: bool) -> u32 {
    let Some((mut lf, em)) = font(c.a(0)) else { return INVALID_PARAMETER };
    lf.height = -(em.round() as i32);
    crate::gdi::write_logfont(c.a(2), &lf, wide);
    OK
}
fn GdipGetLogFontA(c: &mut Ctx) -> u32 {
    get_logfont(c, false)
}
fn GdipGetLogFontW(c: &mut Ctx) -> u32 {
    get_logfont(c, true)
}
/// (the font's size in its unit: UnitPixel)
fn GdipGetFontSize(c: &mut Ctx) -> u32 {
    let Some((_, em)) = font(c.a(0)) else { return INVALID_PARAMETER };
    put32(c.a(1), em.to_bits());
    OK
}
fn GdipGetFontUnit(c: &mut Ctx) -> u32 {
    if font(c.a(0)).is_none() {
        return INVALID_PARAMETER;
    }
    put32(c.a(1), 2);
    OK
}
/// GdipGetFontHeight(font, graphics, &height): the line spacing in the graphics' units
fn GdipGetFontHeight(c: &mut Ctx) -> u32 {
    let Some((tf, _)) = text_font(c.a(0)) else { return INVALID_PARAMETER };
    let k = graphics(c.a(1)).map_or(1.0, |g| g.1);
    put32(c.a(2), (line_height(&tf) / k).to_bits());
    OK
}
fn line_height(tf: &TextFont) -> f32 {
    let (a, d, g) = tf.metrics();
    a + d + g
}

fn GdipCreateSolidFill(c: &mut Ctx) -> u32 {
    new_obj(Obj::Brush { argb: c.a(0) }, c.a(1))
}
fn GdipCloneBrush(c: &mut Ctx) -> u32 {
    let Some(argb) = with(|m| match m.get(&c.a(0)) {
        Some(Obj::Brush { argb }) => Some(*argb),
        _ => None,
    }) else {
        return INVALID_PARAMETER;
    };
    new_obj(Obj::Brush { argb }, c.a(1))
}
/// GdipCreatePen1(argb, width, unit, &pen)
fn GdipCreatePen1(c: &mut Ctx) -> u32 {
    new_obj(Obj::Pen { argb: c.a(0), width: fl(c.a(1)) }, c.a(3))
}
fn color_of(b: u32) -> Option<u32> {
    with(|m| match m.get(&b) {
        Some(Obj::Brush { argb }) | Some(Obj::Pen { argb, .. }) => Some(*argb),
        _ => None,
    })
}

/// GdipCreateStringFormat(flags, language, &format)
fn GdipCreateStringFormat(c: &mut Ctx) -> u32 {
    new_obj(Obj::Format { flags: c.a(0), align: 0, ranges: Vec::new() }, c.a(2))
}
fn GdipSetStringFormatAlign(c: &mut Ctx) -> u32 {
    let a = c.a(1);
    with(|m| match m.get_mut(&c.a(0)) {
        Some(Obj::Format { align, .. }) => {
            *align = a;
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
/// (count, CharacterRange { First, Length }[])
fn GdipSetStringFormatMeasurableCharacterRanges(c: &mut Ctx) -> u32 {
    let (n, p) = (c.a(1), c.a(2));
    let v: Vec<(i32, i32)> = (0..n.min(32)).map(|i| (r32(p + 8 * i) as i32, r32(p + 8 * i + 4) as i32)).collect();
    with(|m| match m.get_mut(&c.a(0)) {
        Some(Obj::Format { ranges, .. }) => {
            *ranges = v;
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
fn format(f: u32) -> (u32, u32, Vec<(i32, i32)>) {
    with(|m| match m.get(&f) {
        Some(Obj::Format { flags, align, ranges }) => (*flags, *align, ranges.clone()),
        _ => (0, 0, Vec::new()),
    })
}

// ---- regions ----

fn GdipCreateRegion(c: &mut Ctx) -> u32 {
    new_obj(Obj::Region { r: [-4194304.0, -4194304.0, 8388608.0, 8388608.0] }, c.a(0))
}
/// GdipCreateRegionRect(RectF*, &region) / GdipCreateRegionRectI(Rect*, &region)
fn GdipCreateRegionRect(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let r = [0, 4, 8, 12].map(|o| fl(r32(p + o)));
    new_obj(Obj::Region { r }, c.a(1))
}
fn GdipCreateRegionRectI(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let r = [0, 4, 8, 12].map(|o| r32(p + o) as i32 as f32);
    new_obj(Obj::Region { r }, c.a(1))
}
fn region(g: u32) -> Option<[f32; 4]> {
    with(|m| match m.get(&g) {
        Some(Obj::Region { r }) => Some(*r),
        _ => None,
    })
}
/// GdipIsEmptyRegion(region, graphics, &BOOL)
fn GdipIsEmptyRegion(c: &mut Ctx) -> u32 {
    let Some(r) = region(c.a(0)) else { return INVALID_PARAMETER };
    put32(c.a(2), (r[2] <= 0.0 || r[3] <= 0.0) as u32);
    OK
}
/// GdipGetRegionBounds(region, graphics, RectF*) / ..I(.., Rect*)
fn GdipGetRegionBounds(c: &mut Ctx) -> u32 {
    let Some(r) = region(c.a(0)) else { return INVALID_PARAMETER };
    for (i, v) in r.iter().enumerate() {
        w32(c.a(2) + 4 * i as u32, v.to_bits());
    }
    OK
}
fn GdipGetRegionBoundsI(c: &mut Ctx) -> u32 {
    let Some(r) = region(c.a(0)) else { return INVALID_PARAMETER };
    // (the integer bounds cover the rectangle)
    let (x0, y0) = (r[0].floor(), r[1].floor());
    let (x1, y1) = ((r[0] + r[2]).ceil(), (r[1] + r[3]).ceil());
    for (i, v) in [x0, y0, x1 - x0, y1 - y0].iter().enumerate() {
        w32(c.a(2) + 4 * i as u32, *v as i32 as u32);
    }
    OK
}

// ---- layout ----

/// One laid out character: its index in the string and its cell (x, y, advance) in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Cell {
    pub i: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// Lays `s` out in the layout rectangle (pixels; a width of 0: unbounded) with `adv` giving
/// each character's advance: the default format's 1/6 em padding before each line, lines of
/// `line` pixels, '\n' (and "\r\n") breaking, words wrapping at the width unless `no_wrap`;
/// `align` 1 centers each line, 2 right-aligns it.
pub(crate) fn layout(s: &[u16], rect: [f32; 4], em: f32, line: f32, no_wrap: bool, align: u32, adv: impl Fn(u16) -> f32) -> Vec<Cell> {
    let pad = em / 6.0;
    let width = rect[2];
    let mut lines: Vec<Vec<(usize, f32)>> = vec![Vec::new()];
    let mut x = 0.0f32;
    let mut i = 0;
    while i < s.len() {
        let ch = s[i];
        if ch == b'\n' as u16 || ch == b'\r' as u16 {
            if ch == b'\r' as u16 && s.get(i + 1) == Some(&(b'\n' as u16)) {
                i += 1;
            }
            lines.push(Vec::new());
            x = 0.0;
            i += 1;
            continue;
        }
        let a = adv(ch);
        if !no_wrap && width > 0.0 && x > 0.0 && pad * 2.0 + x + a > width {
            // (wrap at the last space of the line when there is one, else before this character)
            let cur = lines.last_mut().unwrap();
            let cut = cur.iter().rposition(|&(k, _)| s[k] == b' ' as u16).map(|p| p + 1).filter(|&p| p < cur.len());
            let moved: Vec<(usize, f32)> = match cut {
                Some(p) => cur.split_off(p),
                None => Vec::new(),
            };
            let mut nx = 0.0;
            let mut next = Vec::new();
            for (k, w) in moved {
                next.push((k, w));
                nx += w;
            }
            lines.push(next);
            x = nx;
        }
        lines.last_mut().unwrap().push((i, a));
        x += a;
        i += 1;
    }
    let mut out = Vec::new();
    for (n, l) in lines.iter().enumerate() {
        let total: f32 = l.iter().map(|c| c.1).sum();
        let x0 = match align {
            1 if width > 0.0 => rect[0] + (width - total) / 2.0,
            2 if width > 0.0 => rect[0] + width - pad - total,
            _ => rect[0] + pad,
        };
        let mut x = x0;
        for &(k, w) in l {
            out.push(Cell { i: k, x, y: rect[1] + n as f32 * line, w });
            x += w;
        }
    }
    out
}

/// The string argument: `len` -1 means zero-terminated.
fn string(p: u32, len: u32) -> Vec<u16> {
    if len as i32 == -1 { mem::wstr(p) } else { (0..len).map(|i| mem::r16(p + 2 * i)).collect() }
}
fn rectf(p: u32) -> [f32; 4] {
    if p == 0 { [0.0; 4] } else { [0, 4, 8, 12].map(|o| fl(r32(p + o))) }
}

/// Lays out (graphics, string, length, font, layout RectF*, format): the cells in pixels, the
/// font, the line height in pixels and pixels per world unit.
fn lay(g: u32, sp: u32, len: u32, f: u32, rp: u32, fmt: u32) -> Option<(Vec<Cell>, Vec<u16>, TextFont, f32, f32, f32)> {
    let (_, k) = graphics(g)?;
    let (tf, em) = text_font(f)?;
    let s = string(sp, len);
    let (flags, align, _) = format(fmt);
    let r = rectf(rp).map(|v| v * k);
    let lh = line_height(&tf);
    let cells = layout(&s, r, em, lh, flags & 0x1000 != 0, align, |ch| tf.advance(ch as u32));
    Some((cells, s, tf, em, lh, k))
}

/// GdipMeasureString(graphics, string, length, font, layout, format, &bounds, &fitted, &lines)
fn GdipMeasureString(c: &mut Ctx) -> u32 {
    let Some((cells, s, _, em, lh, k)) = lay(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5)) else { return INVALID_PARAMETER };
    let r = rectf(c.a(4)).map(|v| v * k);
    let pad = em / 6.0;
    let right = cells.iter().map(|c| c.x + c.w).fold(r[0], f32::max) + pad;
    let lines = cells.iter().map(|c| ((c.y - r[1]) / lh).round() as u32 + 1).max().unwrap_or(1);
    let b = [r[0], r[1], right - r[0], lines as f32 * lh].map(|v| v / k);
    for (i, v) in b.iter().enumerate() {
        w32(c.a(6) + 4 * i as u32, v.to_bits());
    }
    if c.a(7) != 0 {
        put32(c.a(7), s.len() as u32);
    }
    if c.a(8) != 0 {
        put32(c.a(8), lines);
    }
    OK
}

/// GdipMeasureCharacterRanges(graphics, string, length, font, layout, format, count, regions):
/// each of the format's ranges as the box of its characters' cells
fn GdipMeasureCharacterRanges(c: &mut Ctx) -> u32 {
    let Some((cells, _, _, _, lh, k)) = lay(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5)) else { return INVALID_PARAMETER };
    let (_, _, ranges) = format(c.a(5));
    let (n, regs) = (c.a(6), c.a(7));
    for (j, &(first, count)) in ranges.iter().enumerate().take(n as usize) {
        let inr: Vec<&Cell> = cells.iter().filter(|c| c.i as i32 >= first && (c.i as i32) < first + count).collect();
        let r = if inr.is_empty() {
            [0.0; 4]
        } else {
            let x0 = inr.iter().map(|c| c.x).fold(f32::MAX, f32::min);
            let y0 = inr.iter().map(|c| c.y).fold(f32::MAX, f32::min);
            let x1 = inr.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max);
            let y1 = inr.iter().map(|c| c.y + lh).fold(f32::MIN, f32::max);
            [x0, y0, x1 - x0, y1 - y0].map(|v| v / k)
        };
        let reg = r32(regs + 4 * j as u32);
        with(|m| {
            if let Some(Obj::Region { r: rr }) = m.get_mut(&reg) {
                *rr = r;
            }
        });
    }
    OK
}

/// GdipDrawString(graphics, string, length, font, layout, format, brush)
fn GdipDrawString(c: &mut Ctx) -> u32 {
    let Some((cells, s, tf, _, _, _)) = lay(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5)) else { return INVALID_PARAMETER };
    let Some(argb) = color_of(c.a(6)) else { return INVALID_PARAMETER };
    let Some((img, _)) = graphics(c.a(0)) else { return INVALID_PARAMETER };
    if img == 0 {
        return OK;
    }
    let (asc, _, _) = tf.metrics();
    crate::gdiplus::with_pixels(img, |w, h, px| {
        for cell in &cells {
            let base_x = cell.x.round() as i32;
            let base_y = (cell.y + asc).round() as i32;
            tf.glyph(s[cell.i] as u32, |gx, top, gw, gh, cov| {
                for yy in 0..gh {
                    for xx in 0..gw {
                        let (x, y) = (base_x + gx + xx, base_y - top + yy);
                        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                            continue;
                        }
                        let a = cov[(yy * gw + xx) as usize];
                        if a != 0 {
                            blend(px, ((y as u32 * w + x as u32) * 4) as usize, argb, a);
                        }
                    }
                }
            });
        }
    });
    OK
}

/// Source-over of `argb` at coverage `cov` onto a non-premultiplied BGRA pixel.
pub(crate) fn blend(px: &mut [u8], i: usize, argb: u32, cov: u8) {
    let sa = ((argb >> 24) as u32 * cov as u32 + 127) / 255;
    if sa == 0 {
        return;
    }
    let src = [argb as u8, (argb >> 8) as u8, (argb >> 16) as u8];
    let da = px[i + 3] as u32;
    // out alpha = sa + da (1 - sa), in 0..255
    let oa = sa + (da * (255 - sa) + 127) / 255;
    for k in 0..3 {
        let d = px[i + k] as u32;
        let v = (src[k] as u32 * sa * 255 + d * da * (255 - sa)) / (oa * 255).max(1);
        px[i + k] = v.min(255) as u8;
    }
    px[i + 3] = oa.min(255) as u8;
}

/// GdipFillRectangle(graphics, brush, x, y, w, h) (REAL) / ..I (INT)
fn fill(c: &mut Ctx, r: [f32; 4]) -> u32 {
    let Some((img, k)) = graphics(c.a(0)) else { return INVALID_PARAMETER };
    let Some(argb) = color_of(c.a(1)) else { return INVALID_PARAMETER };
    if img == 0 {
        return OK;
    }
    let r = r.map(|v| v * k);
    crate::gdiplus::with_pixels(img, |w, h, px| {
        let x0 = r[0].round().clamp(0.0, w as f32) as u32;
        let y0 = r[1].round().clamp(0.0, h as f32) as u32;
        let x1 = (r[0] + r[2]).round().clamp(0.0, w as f32) as u32;
        let y1 = (r[1] + r[3]).round().clamp(0.0, h as f32) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                blend(px, ((y * w + x) * 4) as usize, argb, 255);
            }
        }
    });
    OK
}
fn GdipFillRectangle(c: &mut Ctx) -> u32 {
    let r = [2, 3, 4, 5].map(|i| fl(c.a(i)));
    fill(c, r)
}
fn GdipFillRectangleI(c: &mut Ctx) -> u32 {
    let r = [2, 3, 4, 5].map(|i| c.a(i) as i32 as f32);
    fill(c, r)
}
/// GdipDrawLineI(graphics, pen, x1, y1, x2, y2): a line of the pen's width (at least 1 pixel)
fn GdipDrawLineI(c: &mut Ctx) -> u32 {
    let Some((img, k)) = graphics(c.a(0)) else { return INVALID_PARAMETER };
    let Some((argb, width)) = with(|m| match m.get(&c.a(1)) {
        Some(Obj::Pen { argb, width }) => Some((*argb, *width)),
        _ => None,
    }) else {
        return INVALID_PARAMETER;
    };
    if img == 0 {
        return OK;
    }
    let [x1, y1, x2, y2] = [2, 3, 4, 5].map(|i| c.a(i) as i32 as f32 * k);
    let half = (width * k).max(1.0) / 2.0;
    crate::gdiplus::with_pixels(img, |w, h, px| {
        let n = (x2 - x1).abs().max((y2 - y1).abs()).ceil().max(1.0) as i32;
        let mut done = std::collections::HashSet::new();
        for t in 0..=n {
            let (cx, cy) = (x1 + (x2 - x1) * t as f32 / n as f32, y1 + (y2 - y1) * t as f32 / n as f32);
            for y in (cy - half).round() as i32..(cy + half).round().max((cy - half).round() + 1.0) as i32 {
                for x in (cx - half).round() as i32..(cx + half).round().max((cx - half).round() + 1.0) as i32 {
                    if x >= 0 && y >= 0 && x < w as i32 && y < h as i32 && done.insert((x, y)) {
                        blend(px, ((y as u32 * w + x as u32) * 4) as usize, argb, 255);
                    }
                }
            }
        }
    });
    OK
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("GdipCreateFromHDC", GdipCreateFromHDC),
        ("GdipCreateFromHWND", GdipCreateFromHWND),
        ("GdipGetImageGraphicsContext", GdipGetImageGraphicsContext),
        ("GdipDeleteGraphics", delete),
        ("GdipFlush", ok),
        ("GdipSetPageUnit", GdipSetPageUnit),
        ("GdipSetPageScale", GdipSetPageScale),
        ("GdipSetTextRenderingHint", ok),
        ("GdipSetSmoothingMode", ok),
        ("GdipSetCompositingQuality", ok),
        ("GdipSetInterpolationMode", ok),
        ("GdipCreateFontFromLogfontA", GdipCreateFontFromLogfontA),
        ("GdipCreateFontFromLogfontW", GdipCreateFontFromLogfontW),
        ("GdipGetLogFontA", GdipGetLogFontA),
        ("GdipGetLogFontW", GdipGetLogFontW),
        ("GdipGetFontSize", GdipGetFontSize),
        ("GdipGetFontUnit", GdipGetFontUnit),
        ("GdipGetFontHeight", GdipGetFontHeight),
        ("GdipDeleteFont", delete),
        ("GdipCreateSolidFill", GdipCreateSolidFill),
        ("GdipCloneBrush", GdipCloneBrush),
        ("GdipDeleteBrush", delete),
        ("GdipCreatePen1", GdipCreatePen1),
        ("GdipDeletePen", delete),
        ("GdipCreateStringFormat", GdipCreateStringFormat),
        ("GdipDeleteStringFormat", delete),
        ("GdipSetStringFormatAlign", GdipSetStringFormatAlign),
        ("GdipSetStringFormatMeasurableCharacterRanges", GdipSetStringFormatMeasurableCharacterRanges),
        ("GdipCreateRegion", GdipCreateRegion),
        ("GdipCreateRegionRect", GdipCreateRegionRect),
        ("GdipCreateRegionRectI", GdipCreateRegionRectI),
        ("GdipIsEmptyRegion", GdipIsEmptyRegion),
        ("GdipGetRegionBounds", GdipGetRegionBounds),
        ("GdipGetRegionBoundsI", GdipGetRegionBoundsI),
        ("GdipDeleteRegion", delete),
        ("GdipMeasureString", GdipMeasureString),
        ("GdipMeasureCharacterRanges", GdipMeasureCharacterRanges),
        ("GdipDrawString", GdipDrawString),
        ("GdipFillRectangle", GdipFillRectangle),
        ("GdipFillRectangleI", GdipFillRectangleI),
        ("GdipDrawLineI", GdipDrawLineI),
    ];
    for (n, f) in fns {
        implement("gdiplus", n, *f);
    }
    let _ = put32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ws(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn layout_single_line_padding() {
        let c = layout(&ws("ab"), [10.0, 20.0, 0.0, 0.0], 12.0, 15.0, false, 0, |_| 7.0);
        assert_eq!(c, vec![Cell { i: 0, x: 12.0, y: 20.0, w: 7.0 }, Cell { i: 1, x: 19.0, y: 20.0, w: 7.0 }]);
    }

    #[test]
    fn layout_breaks_and_wraps() {
        // "\r\n" is one break; "aa bb" wraps after the space at width 20 (pad 1 each side)
        let c = layout(&ws("x\r\ny"), [0.0, 0.0, 0.0, 0.0], 6.0, 10.0, false, 0, |_| 5.0);
        assert_eq!(c.iter().map(|c| (c.i, c.y)).collect::<Vec<_>>(), vec![(0, 0.0), (3, 10.0)]);
        let c = layout(&ws("aa bb"), [0.0, 0.0, 20.0, 0.0], 6.0, 10.0, false, 0, |_| 5.0);
        assert_eq!(c.iter().map(|c| (c.i, c.y)).collect::<Vec<_>>(), vec![(0, 0.0), (1, 0.0), (2, 0.0), (3, 10.0), (4, 10.0)]);
        // NoWrap keeps one line
        let c = layout(&ws("aa bb"), [0.0, 0.0, 30.0, 0.0], 6.0, 10.0, true, 0, |_| 5.0);
        assert!(c.iter().all(|c| c.y == 0.0));
    }

    proptest! {
        /// every character is laid out once, in order, and lines never overflow the width
        /// unless a single character is wider than it
        #[test]
        fn layout_covers_all(s in "[a-c \\n]{0,40}", width in 0.0f32..80.0, adv in 1.0f32..9.0) {
            let w: Vec<u16> = ws(&s);
            let cells = layout(&w, [0.0, 0.0, width, 0.0], 6.0, 10.0, false, 0, |_| adv);
            let printable: Vec<usize> = (0..w.len()).filter(|&i| w[i] != b'\n' as u16).collect();
            prop_assert_eq!(cells.iter().map(|c| c.i).collect::<Vec<_>>(), printable);
            if width > 2.0 + adv {
                for c in &cells {
                    prop_assert!(c.x + c.w <= width - 1.0 + 1e-3 || c.x <= 1.0 + 1e-3, "{:?} at width {}", c, width);
                }
            }
        }

        /// source-over: opaque paint replaces, zero coverage keeps, alpha stays in range
        #[test]
        fn blend_source_over(argb in any::<u32>(), dst in any::<[u8; 4]>(), cov in any::<u8>()) {
            let mut px = dst.to_vec();
            blend(&mut px, 0, argb, cov);
            if cov == 0 || argb >> 24 == 0 {
                prop_assert_eq!(px, dst.to_vec());
            } else if cov == 255 && argb >> 24 == 255 {
                prop_assert_eq!(px, vec![argb as u8, (argb >> 8) as u8, (argb >> 16) as u8, 255]);
            } else {
                prop_assert!(px[3] >= dst[3]);
            }
        }
    }
}
