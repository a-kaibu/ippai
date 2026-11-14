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
const OUT_OF_MEMORY: u32 = 3;
const NOT_TRUETYPE_FONT: u32 = 16;

enum Obj {
    /// target bitmap, or the DC drawn into (both 0: a window, measuring only), page unit and
    /// scale, rendering origin (where hatches start)
    Graphics { img: u32, hdc: u32, unit: u32, scale: f32, origin: (i32, i32) },
    /// its LOGFONT (made from the family), em in pixels, family (gdiplus_font), size in its
    /// unit, FontStyle (bold 1, italic 2, underline 4, strikeout 8)
    Font { lf: LogFont, em: f32, family: u32, size: f32, unit: u32, style: u32 },
    Pen { argb: u32, width: f32 },
    Format(StringFormat),
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

fn new_graphics(img: u32, hdc: u32, out: u32) -> u32 {
    new_obj(Obj::Graphics { img, hdc, unit: 1, scale: 1.0, origin: (0, 0) }, out)
}
fn GdipCreateFromHDC(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return OUT_OF_MEMORY;
    }
    new_graphics(0, c.a(0), c.a(1))
}
fn GdipCreateFromHWND(c: &mut Ctx) -> u32 {
    new_graphics(0, 0, c.a(1))
}
/// GdipCreateBitmapFromGraphics(width, height, graphics, &bitmap): a 32bppPARGB bitmap
fn GdipCreateBitmapFromGraphics(c: &mut Ctx) -> u32 {
    if graphics(c.a(2)).is_none() || c.i(0) <= 0 || c.i(1) <= 0 {
        return INVALID_PARAMETER;
    }
    crate::gdiplus::new_bitmap(c.a(0), c.a(1), 0x000e_200b, c.a(3))
}
fn GdipGetRenderingOrigin(c: &mut Ctx) -> u32 {
    let (xp, yp) = (c.a(1), c.a(2));
    with(|m| match m.get(&c.a(0)) {
        Some(Obj::Graphics { origin, .. }) if xp != 0 && yp != 0 => {
            w32(xp, origin.0 as u32);
            w32(yp, origin.1 as u32);
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
fn GdipSetRenderingOrigin(c: &mut Ctx) -> u32 {
    let o = (c.i(1), c.i(2));
    with(|m| match m.get_mut(&c.a(0)) {
        Some(Obj::Graphics { origin, .. }) => {
            *origin = o;
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
fn GdipGetImageGraphicsContext(c: &mut Ctx) -> u32 {
    if !crate::gdiplus::is_image(c.a(0)) {
        return INVALID_PARAMETER;
    }
    new_graphics(c.a(0), 0, c.a(1))
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
    let s = c.f(1);
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
    target(g).map(|(img, _, k, _)| (img, k))
}
/// a graphics' bitmap, DC, pixels per unit and rendering origin
fn target(g: u32) -> Option<(u32, u32, f32, (i32, i32))> {
    with(|m| match m.get(&g) {
        Some(Obj::Graphics { img, hdc, unit, scale, origin }) => Some((*img, *hdc, px_per_unit(*unit, *scale), *origin)),
        _ => None,
    })
}

// ---- fonts, brushes, pens, formats ----

/// The LOGFONT GDI+ reports for a font: its family's face, the em as a negative height, the
/// style's weight and flags, the rest 0 (Wine's tests)
fn logfont_of(family: u32, em: f32, style: u32, charset: u8) -> LogFont {
    let name = crate::gdiplus_font::family(family).map_or_else(String::new, |f| f.name);
    LogFont {
        height: -(em.round() as i32),
        width: 0,
        esc: 0,
        orient: 0,
        weight: if style & 1 != 0 { 700 } else { 400 },
        italic: (style & 2 != 0) as u8,
        underline: (style & 4 != 0) as u8,
        strike: (style & 8 != 0) as u8,
        charset,
        out_prec: 0,
        clip_prec: 0,
        quality: 0,
        pitch: 0,
        face: name,
    }
}
/// The charset of a family's LOGFONT: SHIFTJIS for the built-in Japanese faces
fn charset_of(family: u32) -> u8 {
    match crate::gdiplus_font::family(family) {
        Some(f) if crate::gdi::find_face(&f.name).is_some() && crate::gdi::face_names(f.face).first().is_some_and(|n| !n.is_ascii()) => 128,
        _ => 0,
    }
}

/// GdipCreateFontFromLogfont{A,W}(hdc, LOGFONT*, &font): its em in pixels (a negative
/// lfHeight is the em, a positive one the cell height: the em that fits the family's ascent
/// and descent in it), unit World. A face GDI+ does not have is Arial, unless the LOGFONT
/// has neither a weight nor a charset (NotTrueTypeFont; Wine's tests)
fn font_from_logfont(c: &mut Ctx, wide: bool) -> u32 {
    let (lp, out) = (c.a(1), c.a(2));
    if c.a(0) == 0 || lp == 0 || out == 0 {
        return INVALID_PARAMETER;
    }
    let lf = crate::gdi::read_logfont(lp, wide);
    let known = !lf.face.is_empty() && crate::gdi::find_face(&lf.face).is_some() || crate::gdiplus_font::is_family_name(&lf.face);
    if !known && lf.weight == 0 && lf.charset == 0 {
        return NOT_TRUETYPE_FONT;
    }
    let family = crate::gdiplus_font::family_for(&lf.face);
    let Some(fam) = crate::gdiplus_font::family(family) else { return NOT_TRUETYPE_FONT };
    let h = if lf.height == 0 { 16 } else { lf.height };
    let em = em_for_height(h, crate::gdiplus_font::design_metrics(fam.face));
    let style = (lf.weight >= 600) as u32 | (lf.italic != 0) as u32 * 2 | (lf.underline != 0) as u32 * 4 | (lf.strike != 0) as u32 * 8;
    let charset = if lf.charset == 1 { charset_of(family) } else { lf.charset };
    let lf = logfont_of(family, em, style, charset);
    new_obj(Obj::Font { lf, em, family, size: em, unit: 0, style }, out)
}
fn GdipCreateFontFromLogfontA(c: &mut Ctx) -> u32 {
    font_from_logfont(c, false)
}
fn GdipCreateFontFromLogfontW(c: &mut Ctx) -> u32 {
    font_from_logfont(c, true)
}
/// GdipCreateFont(family, size, style, unit, &font): a positive size in a unit other than
/// UnitDisplay
fn GdipCreateFont(c: &mut Ctx) -> u32 {
    let (family, size, style, unit, out) = (c.a(0), c.f(1), c.a(2), c.a(3), c.a(4));
    if crate::gdiplus_font::family(family).is_none() || out == 0 || !(size > 0.0) || unit == 1 || unit > 6 {
        return INVALID_PARAMETER;
    }
    let em = size * px_per_unit(unit, 1.0);
    let lf = logfont_of(family, em, style, charset_of(family));
    new_obj(Obj::Font { lf, em, family, size, unit, style }, out)
}
fn GdipCloneFont(c: &mut Ctx) -> u32 {
    let o = with(|m| match m.get(&c.a(0)) {
        Some(Obj::Font { lf, em, family, size, unit, style }) => Some(Obj::Font { lf: lf.clone(), em: *em, family: *family, size: *size, unit: *unit, style: *style }),
        _ => None,
    });
    let Some(o) = o else { return INVALID_PARAMETER };
    new_obj(o, c.a(1))
}
fn GdipDeleteFont(c: &mut Ctx) -> u32 {
    if font(c.a(0)).is_none() {
        return INVALID_PARAMETER;
    }
    delete(c)
}
fn font(f: u32) -> Option<(LogFont, f32)> {
    with(|m| match m.get(&f) {
        Some(Obj::Font { lf, em, .. }) => Some((lf.clone(), *em)),
        _ => None,
    })
}
/// a font's family, size, unit and style
fn font_info(f: u32) -> Option<(u32, f32, u32, u32)> {
    with(|m| match m.get(&f) {
        Some(Obj::Font { family, size, unit, style, .. }) => Some((*family, *size, *unit, *style)),
        _ => None,
    })
}
fn text_font(f: u32) -> Option<(TextFont, f32)> {
    let (lf, em) = font(f)?;
    let face = crate::gdiplus_font::family(with(|m| match m.get(&f) {
        Some(Obj::Font { family, .. }) => *family,
        _ => 0,
    }))
    .map(|fam| crate::gdi::face_names(fam.face).pop().unwrap_or(lf.face.clone()))
    .unwrap_or(lf.face.clone());
    Some((crate::gdi::text_font(&face, em, lf.weight.max(400))?, em))
}
/// GdipGetLogFont{A,W}(font, graphics, &LOGFONT)
fn get_logfont(c: &mut Ctx, wide: bool) -> u32 {
    let Some((lf, _)) = font(c.a(0)) else { return INVALID_PARAMETER };
    if graphics(c.a(1)).is_none() || c.a(2) == 0 {
        return INVALID_PARAMETER;
    }
    crate::gdi::write_logfont(c.a(2), &lf, wide);
    OK
}
fn GdipGetLogFontA(c: &mut Ctx) -> u32 {
    get_logfont(c, false)
}
fn GdipGetLogFontW(c: &mut Ctx) -> u32 {
    get_logfont(c, true)
}
/// (the size in the font's own unit)
fn GdipGetFontSize(c: &mut Ctx) -> u32 {
    let (Some((_, size, _, _)), out) = (font_info(c.a(0)), c.a(1)) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    put32(out, size.to_bits());
    OK
}
fn GdipGetFontUnit(c: &mut Ctx) -> u32 {
    let (Some((_, _, unit, _)), out) = (font_info(c.a(0)), c.a(1)) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    put32(out, unit);
    OK
}
fn GdipGetFontStyle(c: &mut Ctx) -> u32 {
    let (Some((_, _, _, style)), out) = (font_info(c.a(0)), c.a(1)) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    put32(out, style);
    OK
}
/// GdipGetFamily(font, &family): the family it was made from, the same object
fn GdipGetFamily(c: &mut Ctx) -> u32 {
    let (Some((family, _, _, _)), out) = (font_info(c.a(0)), c.a(1)) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    mem::putp(out, family);
    OK
}
/// The line spacing of a font in pixels at `dpi`: its size in pixels there times the
/// family's line spacing over its em height
fn line_spacing_at(f: u32, dpi: f32) -> Option<f32> {
    let (family, size, unit, _) = font_info(f)?;
    let fam = crate::gdiplus_font::family(family)?;
    Some(spacing_px(size, unit, dpi, crate::gdiplus_font::design_metrics(fam.face)))
}
/// a font's line spacing in pixels at `dpi`: its size in pixels times line spacing / em
/// (pixel and world sizes ignore the dpi)
fn spacing_px(size: f32, unit: u32, dpi: f32, m: [u16; 4]) -> f32 {
    let px = size * px_per_unit(unit, 1.0) * if matches!(unit, 3..=6) { dpi / 96.0 } else { 1.0 };
    px * m[3] as f32 / m[0].max(1) as f32
}
/// the em size a positive LOGFONT height (a cell height) asks for: the height scaled by
/// em / (win ascent + win descent), rounded; a negative height is the em itself
fn em_for_height(h: i32, m: [u16; 4]) -> f32 {
    if h < 0 {
        -h as f32
    } else {
        (h as f32 * m[0] as f32 / (m[1] as f32 + m[2] as f32).max(1.0)).round()
    }
}
/// GdipGetFontHeight(font, graphics, &height): the line spacing in the graphics' units (no
/// graphics: pixels at 96 dpi)
fn GdipGetFontHeight(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    let Some(h) = line_spacing_at(c.a(0), 96.0).filter(|_| out != 0) else { return INVALID_PARAMETER };
    let k = graphics(c.a(1)).map_or(1.0, |g| g.1);
    put32(out, (h / k).to_bits());
    OK
}
/// GdipGetFontHeightGivenDPI(font, dpi, &height)
fn GdipGetFontHeightGivenDPI(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    let Some(h) = line_spacing_at(c.a(0), c.f(1)).filter(|_| out != 0) else { return INVALID_PARAMETER };
    put32(out, h.to_bits());
    OK
}
fn line_height(tf: &TextFont) -> f32 {
    let (a, d, g) = tf.metrics();
    a + d + g
}

/// GdipCreatePen1(argb, width, unit, &pen)
fn GdipCreatePen1(c: &mut Ctx) -> u32 {
    new_obj(Obj::Pen { argb: c.a(0), width: c.f(1) }, c.a(3))
}
/// a brush's (gdiplus_brush) or pen's color
fn color_of(b: u32) -> Option<u32> {
    crate::gdiplus_brush::main_color(b).or_else(|| {
        with(|m| match m.get(&b) {
            Some(Obj::Pen { argb, .. }) => Some(*argb),
            _ => None,
        })
    })
}

#[derive(Clone)]
struct StringFormat {
    flags: u32,
    align: u32,
    line_align: u32,
    trimming: u32,
    hotkey: u32,
    digit_language: u16,
    digit_method: u32,
    first_tab: f32,
    tabs: Vec<f32>,
    ranges: Vec<(i32, i32)>,
    generic: bool,
}
impl StringFormat {
    fn new(flags: u32) -> Self {
        Self { flags, align: 0, line_align: 0, trimming: 1, hotkey: 0,
            digit_language: 0, digit_method: 0, first_tab: 0.0,
            tabs: Vec::new(), ranges: Vec::new(), generic: false }
    }
}
fn edit_format(h: u32, f: impl FnOnce(&mut StringFormat) -> u32) -> u32 {
    with(|m| match m.get_mut(&h) {
        Some(Obj::Format(v)) => f(v),
        _ => INVALID_PARAMETER,
    })
}
fn GdipCreateStringFormat(c: &mut Ctx) -> u32 {
    new_obj(Obj::Format(StringFormat::new(c.a(0))), c.a(2))
}
fn GdipCloneStringFormat(c: &mut Ctx) -> u32 {
    let v = with(|m| match m.get(&c.a(0)) {
        Some(Obj::Format(v)) => Some(v.clone()), _ => None,
    });
    let Some(mut v) = v else { return INVALID_PARAMETER };
    v.generic = false;
    new_obj(Obj::Format(v), c.a(1))
}
fn GdipDeleteStringFormat(c: &mut Ctx) -> u32 {
    with(|m| {
        match m.get(&c.a(0)) {
            Some(Obj::Format(v)) if v.generic => return OK,
            Some(Obj::Format(_)) => (),
            _ => return INVALID_PARAMETER,
        }
        m.remove(&c.a(0));
        mem::heap_free(c.a(0));
        OK
    })
}
// Native generic formats are shared, mutable objects, including across Delete calls.
static GENERIC_FORMATS: Mutex<[u32; 2]> = Mutex::new([0; 2]);
fn generic_format(out: u32, typographic: bool) -> u32 {
    if out == 0 { return INVALID_PARAMETER; }
    let mut handles = GENERIC_FORMATS.lock().unwrap();
    let h = &mut handles[typographic as usize];
    if *h == 0 {
        let mut v = StringFormat::new(if typographic { 0x6004 } else { 0 });
        v.generic = true;
        if typographic { v.trimming = 0; }
        *h = mem::heap_alloc(16, true);
        with(|m| m.insert(*h, Obj::Format(v)));
    }
    mem::putp(out, *h);
    OK
}
fn GdipStringFormatGetGenericDefault(c: &mut Ctx) -> u32 { generic_format(c.a(0), false) }
fn GdipStringFormatGetGenericTypographic(c: &mut Ctx) -> u32 { generic_format(c.a(0), true) }
macro_rules! format_property {
    ($get:ident, $set:ident, $field:ident, $max:expr) => {
        fn $get(c: &mut Ctx) -> u32 {
            if c.a(1) == 0 { return INVALID_PARAMETER; }
            edit_format(c.a(0), |v| { w32(c.a(1), v.$field); OK })
        }
        fn $set(c: &mut Ctx) -> u32 {
            if c.a(1) > $max { return INVALID_PARAMETER; }
            edit_format(c.a(0), |v| { v.$field = c.a(1); OK })
        }
    }
}
format_property!(GdipGetStringFormatFlags, GdipSetStringFormatFlags, flags, u32::MAX);
format_property!(GdipGetStringFormatAlign, GdipSetStringFormatAlign, align, 2);
format_property!(GdipGetStringFormatLineAlign, GdipSetStringFormatLineAlign, line_align, 2);
format_property!(GdipGetStringFormatTrimming, GdipSetStringFormatTrimming, trimming, 5);
format_property!(GdipGetStringFormatHotkeyPrefix, GdipSetStringFormatHotkeyPrefix, hotkey, 2);
fn GdipGetStringFormatDigitSubstitution(c: &mut Ctx) -> u32 {
    edit_format(c.a(0), |v| {
        if c.a(1) != 0 { mem::w16(c.a(1), v.digit_language); }
        if c.a(2) != 0 { w32(c.a(2), v.digit_method); }
        OK
    })
}
fn GdipSetStringFormatDigitSubstitution(c: &mut Ctx) -> u32 {
    edit_format(c.a(0), |v| { v.digit_language = c.a(1) as u16; v.digit_method = c.a(2); OK })
}
fn GdipGetStringFormatMeasurableCharacterRangeCount(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 { return INVALID_PARAMETER; }
    edit_format(c.a(0), |v| { w32(c.a(1), v.ranges.len() as u32); OK })
}
fn GdipSetStringFormatMeasurableCharacterRanges(c: &mut Ctx) -> u32 {
    let (n, p) = (c.i(1), c.a(2));
    edit_format(c.a(0), |v| {
        if n < 0 || (n > 0 && p == 0) { return INVALID_PARAMETER; }
        if n > 32 { return 11; } // ValueOverflow; do not silently truncate.
        v.ranges = (0..n as u32).map(|i| (r32(p + 8*i) as i32, r32(p + 8*i + 4) as i32)).collect();
        OK
    })
}
fn GdipGetStringFormatTabStopCount(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 { return INVALID_PARAMETER; }
    edit_format(c.a(0), |v| { w32(c.a(1), v.tabs.len() as u32); OK })
}
fn GdipSetStringFormatTabStops(c: &mut Ctx) -> u32 {
    let (first, n, p) = (c.f(1), c.i(2), c.a(3));
    if p == 0 { return INVALID_PARAMETER; }
    edit_format(c.a(0), |v| {
        if n <= 0 { return OK; }
        // (a garbage count: OutOfMemory, not gigabytes of floats)
        if n > 1 << 16 { return 3; }
        if first < 0.0 { return 6; } // Native GDI+ returns NotImplemented.
        v.tabs = (0..n as u32).map(|i| fl(r32(p + 4*i))).collect();
        v.first_tab = first;
        OK
    })
}
fn GdipGetStringFormatTabStops(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 || c.a(3) == 0 { return INVALID_PARAMETER; }
    edit_format(c.a(0), |v| {
        w32(c.a(2), v.first_tab.to_bits());
        for (i, tab) in v.tabs.iter().take(c.i(1).max(0) as usize).enumerate() {
            w32(c.a(3) + 4*i as u32, tab.to_bits());
        }
        OK
    })
}
// Drawing needs only these scalars: do not clone ranges on every Draw/MeasureString.
fn format(f: u32) -> (u32, u32) {
    with(|m| match m.get(&f) {
        Some(Obj::Format(v)) => (v.flags, v.align),
        _ => (0, 0),
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
/// The longest string read (a garbage length read up to 8 GiB into a Vec).
const MAX_STRING: u32 = 1 << 20;
fn string(p: u32, len: u32) -> Vec<u16> {
    if len as i32 == -1 { mem::wstr(p) } else { (0..len.min(MAX_STRING)).map(|i| mem::r16(p + 2 * i)).collect() }
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
    let (flags, align) = format(fmt);
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
    let ranges = with(|m| match m.get(&c.a(5)) {
        Some(Obj::Format(v)) => v.ranges.clone(), _ => Vec::new(),
    });
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

/// GdipFillRectangle(graphics, brush, x, y, w, h) (REAL) / ..I (INT): each pixel the brush's
/// color there (gdiplus_brush), source-over, into the bitmap or the DC
fn fill(c: &mut Ctx, r: [f32; 4]) -> u32 {
    let Some((img, hdc, k, origin)) = target(c.a(0)) else { return INVALID_PARAMETER };
    let Some(paint) = crate::gdiplus_brush::painter(c.a(1)) else { return INVALID_PARAMETER };
    if img == 0 && hdc == 0 {
        return OK;
    }
    let r = r.map(|v| v * k);
    let (mut x0, mut y0) = (r[0].round() as i32, r[1].round() as i32);
    let (mut x1, mut y1) = ((r[0] + r[2]).round() as i32, (r[1] + r[3]).round() as i32);
    if img != 0 {
        let Some((w, h)) = crate::gdiplus::size_of(img) else { return INVALID_PARAMETER };
        (x0, y0, x1, y1) = (x0.clamp(0, w as i32), y0.clamp(0, h as i32), x1.clamp(0, w as i32), y1.clamp(0, h as i32));
    } else {
        // (the DC's clip first: "clear everything" with FillRectangle(0, 0, 10000, 10000)
        // sampled 8 bytes of colour per pixel of all of it, gigabytes at the old +-16384 clamp)
        let Some(b) = crate::gdi::clip_bounds(hdc) else { return INVALID_PARAMETER };
        (x0, y0, x1, y1) = (x0.max(b.l), y0.max(b.t), x1.min(b.r), y1.min(b.b));
    }
    if x1 <= x0 || y1 <= y0 {
        return OK;
    }
    let (fw, fh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    // (the colors first: no lock of the bitmaps or DCs is held while sampling the brush)
    let mut cols = Vec::with_capacity(fw * fh);
    for y in y0..y1 {
        for x in x0..x1 {
            cols.push(paint.at(x, y, origin));
        }
    }
    if img != 0 {
        crate::gdiplus::with_pixels(img, |w, _, px| {
            for y in y0..y1 {
                for x in x0..x1 {
                    if let Some(argb) = cols[(y - y0) as usize * fw + (x - x0) as usize] {
                        blend(px, ((y as u32 * w + x as u32) * 4) as usize, argb, 255);
                    }
                }
            }
        });
    } else {
        crate::gdi::blend_argb(hdc, x0, y0, fw as i32, fh as i32, &cols);
    }
    OK
}
fn GdipFillRectangle(c: &mut Ctx) -> u32 {
    let r = [2, 3, 4, 5].map(|i| c.f(i));
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
        // (a pen wider than the image covers it: a garbage width looped over its square)
        let half = half.min((w + h) as f32);
        let n = (x2 - x1).abs().max((y2 - y1).abs()).ceil().max(1.0) as i32;
        // only the steps whose nib can touch the image (a far end point walked billions of
        // steps outside it); the same steps as all of them would be there
        let Some((ta, tb)) = line_steps([x1, y1, x2, y2], n, half + 1.0, (w as f32, h as f32)) else { return };
        let mut done = std::collections::HashSet::new();
        for t in ta..=tb {
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

/// The steps `ta..=tb` of `0..=n` along the line `p` (x1, y1, x2, y2) at which it is within
/// `pad` of the `w` x `h` image (Liang-Barsky on the parameter), None: none of them.
fn line_steps(p: [f32; 4], n: i32, pad: f32, (w, h): (f32, f32)) -> Option<(i32, i32)> {
    let (dx, dy) = (p[2] - p[0], p[3] - p[1]);
    let (mut t0, mut t1) = (0f64, 1f64);
    for (q, d) in [(p[0] + pad, -dx), (w + pad - p[0], dx), (p[1] + pad, -dy), (h + pad - p[1], dy)] {
        let (q, d) = (q as f64, d as f64);
        if d == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let r = q / d;
        if d < 0.0 {
            t0 = t0.max(r);
        } else {
            t1 = t1.min(r);
        }
    }
    if !(t0 <= t1) {
        return None;
    }
    let ta = ((t0 * n as f64).floor() as i64 - 1).clamp(0, n as i64) as i32;
    let tb = ((t1 * n as f64).ceil() as i64 + 1).clamp(0, n as i64) as i32;
    Some((ta, tb))
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("GdipCreateFromHDC", GdipCreateFromHDC),
        ("GdipCreateFromHWND", GdipCreateFromHWND),
        ("GdipCreateBitmapFromGraphics", GdipCreateBitmapFromGraphics),
        ("GdipGetRenderingOrigin", GdipGetRenderingOrigin),
        ("GdipSetRenderingOrigin", GdipSetRenderingOrigin),
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
        ("GdipDeleteFont", GdipDeleteFont),
        ("GdipCreateFont", GdipCreateFont),
        ("GdipCloneFont", GdipCloneFont),
        ("GdipGetFontStyle", GdipGetFontStyle),
        ("GdipGetFamily", GdipGetFamily),
        ("GdipGetFontHeightGivenDPI", GdipGetFontHeightGivenDPI),
        ("GdipCreatePen1", GdipCreatePen1),
        ("GdipDeletePen", delete),
        ("GdipCloneStringFormat", GdipCloneStringFormat),
        ("GdipStringFormatGetGenericDefault", GdipStringFormatGetGenericDefault),
        ("GdipStringFormatGetGenericTypographic", GdipStringFormatGetGenericTypographic),
        ("GdipGetStringFormatAlign", GdipGetStringFormatAlign),
        ("GdipGetStringFormatFlags", GdipGetStringFormatFlags),
        ("GdipSetStringFormatFlags", GdipSetStringFormatFlags),
        ("GdipGetStringFormatLineAlign", GdipGetStringFormatLineAlign),
        ("GdipSetStringFormatLineAlign", GdipSetStringFormatLineAlign),
        ("GdipGetStringFormatTrimming", GdipGetStringFormatTrimming),
        ("GdipSetStringFormatTrimming", GdipSetStringFormatTrimming),
        ("GdipGetStringFormatHotkeyPrefix", GdipGetStringFormatHotkeyPrefix),
        ("GdipSetStringFormatHotkeyPrefix", GdipSetStringFormatHotkeyPrefix),
        ("GdipGetStringFormatDigitSubstitution", GdipGetStringFormatDigitSubstitution),
        ("GdipSetStringFormatDigitSubstitution", GdipSetStringFormatDigitSubstitution),
        ("GdipGetStringFormatMeasurableCharacterRangeCount", GdipGetStringFormatMeasurableCharacterRangeCount),
        ("GdipGetStringFormatTabStopCount", GdipGetStringFormatTabStopCount),
        ("GdipSetStringFormatTabStops", GdipSetStringFormatTabStops),
        ("GdipGetStringFormatTabStops", GdipGetStringFormatTabStops),
        ("GdipCreateStringFormat", GdipCreateStringFormat),
        ("GdipDeleteStringFormat", GdipDeleteStringFormat),
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

    proptest::proptest! {
        /// line_steps keeps every step of a line whose nib can touch the image, whatever the
        /// end points (brute force over all of them)
        #[test]
        fn line_steps_keep_every_step_near_the_image(p in proptest::array::uniform4(-3000i32..3000), w in 1u32..200, h in 1u32..200, half in 0.5f32..20.0) {
            let p = p.map(|v| v as f32);
            let n = (p[2] - p[0]).abs().max((p[3] - p[1]).abs()).ceil().max(1.0) as i32;
            let r = line_steps(p, n, half + 1.0, (w as f32, h as f32));
            for t in 0..=n {
                let (cx, cy) = (p[0] + (p[2] - p[0]) * t as f32 / n as f32, p[1] + (p[3] - p[1]) * t as f32 / n as f32);
                let near = (cx + half).round() as i32 >= 0 && (cy + half).round() as i32 >= 0 && (cx - half).round() as i32 <= w as i32 && (cy - half).round() as i32 <= h as i32;
                if near {
                    proptest::prop_assert!(r.is_some_and(|(a, b)| a <= t && t <= b), "step {} of {} near the image, kept {:?}", t, n, r);
                }
            }
        }
    }
    use proptest::prelude::*;

    // Tahoma's design metrics: em 2048, win ascent 2049, win descent 423, spacing 2472
    const TAHOMA: [u16; 4] = [2048, 2049, 423, 2472];

    #[test]
    fn font_height_units() {
        // a 30-unit Tahoma in pixel, point, inch, document and millimetre units at 96 dpi
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        for (unit, want) in [(2, 36.2109), (3, 48.2812), (4, 3476.25), (5, 11.5875), (6, 136.8602)] {
            let got = spacing_px(30.0, unit, 96.0, TAHOMA);
            assert!(close(got, want), "unit {unit}: {got} vs {want}");
        }
        // point sizes follow the dpi; pixel and world sizes don't
        assert!(close(spacing_px(30.0, 3, 192.0, TAHOMA), 96.5625));
        assert_eq!(spacing_px(30.0, 2, 192.0, TAHOMA), spacing_px(30.0, 2, 96.0, TAHOMA));
        assert_eq!(spacing_px(30.0, 0, 192.0, TAHOMA), spacing_px(30.0, 2, 96.0, TAHOMA));
    }

    #[test]
    fn logfont_heights() {
        assert_eq!(em_for_height(-16, TAHOMA), 16.0);
        // a cell height of 30 is an em of 30 * 2048 / 2472 = 24.85 -> 25
        assert_eq!(em_for_height(30, TAHOMA), 25.0);
        assert_eq!(em_for_height(0, TAHOMA), 0.0);
        assert_eq!(em_for_height(12, [0, 0, 0, 0]), 0.0);
    }

    proptest! {
        #[test]
        fn spacing_scales_linearly(size in 0.5f32..500.0, unit in 0u32..7, dpi in 24f32..600.0) {
            let one = spacing_px(1.0, unit, dpi, TAHOMA);
            let got = spacing_px(size, unit, dpi, TAHOMA);
            prop_assert!((got - one * size).abs() <= 1e-4 * got.max(1.0));
            prop_assert!(got > 0.0);
        }

        #[test]
        fn cell_height_em_never_exceeds_cell(h in 1i32..2000) {
            let em = em_for_height(h, TAHOMA);
            prop_assert!(em <= h as f32);
            prop_assert!((em - h as f32 * 2048.0 / 2472.0).abs() <= 0.5);
        }
    }

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

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod format_abi_tests {
    use super::*;
    use crate::test_guest::{call, enter};

    /// FillRectangle on an HDC far past its bitmap (the "clear everything" call): the bitmap
    /// filled, done in the DC's clip (it sampled 8 bytes of colour per pixel of the whole
    /// rectangle first: 8 GiB at the old clamp, an abort)
    #[test]
    fn huge_fills_on_a_dc_fill_its_bitmap() {
        let _guard = enter();
        crate::test_guest::guest_thread();
        crate::gdi::init();
        let dc = call(crate::gdi::CreateCompatibleDC, &[0]);
        let bmp = call(crate::gdi::CreateBitmap, &[8, 6, 1, 32, 0]);
        call(crate::gdi::SelectObject, &[dc, bmp]);
        let p = mem::heap_alloc(16, true);
        assert_eq!(call(GdipCreateFromHDC, &[dc, p]), OK);
        let g = r32(p);
        assert_eq!(call(crate::gdiplus_brush::GdipCreateSolidFill, &[0xff11_2233, p + 4]), OK);
        let b = r32(p + 4);
        assert_eq!(call(GdipFillRectangleI, &[g, b, (-(1i32 << 30)) as u32, (-(1i32 << 30)) as u32, i32::MAX as u32, i32::MAX as u32]), OK);
        for y in 0..6 {
            for x in 0..8 {
                assert_eq!(call(crate::gdi::GetPixel, &[dc, x, y]), 0x33_2211, "({x}, {y})");
            }
        }
        // (a rectangle off the bitmap: nothing)
        assert_eq!(call(crate::gdiplus_brush::GdipCreateSolidFill, &[0xff00_0000, p + 8]), OK);
        assert_eq!(call(GdipFillRectangleI, &[g, r32(p + 8), 100, 100, 1 << 20, 1 << 20]), OK);
        assert_eq!(call(crate::gdi::GetPixel, &[dc, 7, 5]), 0x33_2211);
        mem::heap_free(p);
    }

    #[test]
    fn string_format_clone_owns_tabs_and_ranges() {
        let _guard = enter();
        let p = mem::heap_alloc(128, true);
        assert_eq!(call(GdipCreateStringFormat, &[0xdeadbeef, 0x419, p]), OK);
        let h = r32(p);
        w32(p + 16, 10.0f32.to_bits());
        w32(p + 20, 20.0f32.to_bits());
        assert_eq!(call(GdipSetStringFormatTabStops, &[h, 5.0f32.to_bits(), 2, p + 16]), OK);
        assert_eq!(call(GdipSetStringFormatMeasurableCharacterRanges, &[h, 1, p + 16]), OK);
        assert_eq!(call(GdipCloneStringFormat, &[h, p + 4]), OK);
        let cloned = r32(p + 4);
        assert_ne!(h, cloned);
        assert_eq!(call(GdipSetStringFormatFlags, &[h, 0]), OK);
        assert_eq!(call(GdipSetStringFormatTabStops, &[h, 0, 1, p + 20]), OK);
        assert_eq!(call(GdipSetStringFormatMeasurableCharacterRanges, &[h, 0, 0]), OK);
        assert_eq!(call(GdipDeleteStringFormat, &[h]), OK);
        assert_eq!(call(GdipGetStringFormatFlags, &[cloned, p + 8]), OK);
        assert_eq!(r32(p + 8), 0xdeadbeef);
        assert_eq!(call(GdipGetStringFormatTabStops, &[cloned, 2, p + 8, p + 32]), OK);
        assert_eq!(fl(r32(p + 8)), 5.0);
        assert_eq!([fl(r32(p + 32)), fl(r32(p + 36))], [10.0, 20.0]);
        assert_eq!(call(GdipGetStringFormatMeasurableCharacterRangeCount, &[cloned, p + 8]), OK);
        assert_eq!(r32(p + 8), 1);
        assert_eq!(call(GdipDeleteStringFormat, &[cloned]), OK);
        mem::heap_free(p);
    }

    #[test]
    fn string_format_abi_validation_preserves_outputs_and_state() {
        let _guard = enter();
        let p = mem::heap_alloc(128, true);
        assert_eq!(call(GdipCreateStringFormat, &[0, 0x419, p]), OK);
        let h = r32(p);
        w32(p + 8, 0xaaaa_aaaa);
        assert_eq!(call(GdipGetStringFormatDigitSubstitution, &[h, p + 8, 0]), OK);
        assert_eq!(r32(p + 8), 0xaaaa_0000, "LANGID writes exactly two bytes");
        assert_eq!(call(GdipGetStringFormatDigitSubstitution, &[h, 0, 0]), OK);
        assert_eq!(call(GdipSetStringFormatMeasurableCharacterRanges, &[0, 1, 0]), INVALID_PARAMETER);
        assert_eq!(call(GdipSetStringFormatMeasurableCharacterRanges, &[h, 1, 0]), INVALID_PARAMETER);
        assert_eq!(call(GdipSetStringFormatMeasurableCharacterRanges, &[h, u32::MAX, p + 16]), INVALID_PARAMETER);
        assert_eq!(call(GdipSetStringFormatMeasurableCharacterRanges, &[h, 33, p + 16]), 11);
        assert_eq!(call(GdipSetStringFormatTabStops, &[h, 0, 0, 0]), INVALID_PARAMETER);
        w32(p + 16, 3.0f32.to_bits());
        assert_eq!(call(GdipSetStringFormatTabStops, &[h, 7.0f32.to_bits(), 1, p + 16]), OK);
        assert_eq!(call(GdipSetStringFormatTabStops, &[h, 0, 0, p + 16]), OK);
        assert_eq!(call(GdipSetStringFormatTabStops, &[h, 0, u32::MAX, p + 16]), OK);
        assert_eq!(call(GdipGetStringFormatTabStopCount, &[h, p + 8]), OK);
        assert_eq!(r32(p + 8), 1);
        assert_eq!(call(GdipSetStringFormatTabStops, &[h, (-1.0f32).to_bits(), 1, p + 16]), 6);
        assert_eq!(call(GdipGetStringFormatTabStops, &[h, 0, p + 8, p + 16]), OK);
        assert_eq!(fl(r32(p + 8)), 7.0);
        assert_eq!(fl(r32(p + 16)), 3.0);
        for (set, get, max) in [
            (GdipSetStringFormatAlign as Handler, GdipGetStringFormatAlign as Handler, 2),
            (GdipSetStringFormatLineAlign as Handler, GdipGetStringFormatLineAlign as Handler, 2),
            (GdipSetStringFormatTrimming as Handler, GdipGetStringFormatTrimming as Handler, 5),
            (GdipSetStringFormatHotkeyPrefix as Handler, GdipGetStringFormatHotkeyPrefix as Handler, 2),
        ] {
            assert_eq!(call(set, &[h, max]), OK);
            assert_eq!(call(set, &[h, max + 1]), INVALID_PARAMETER);
            assert_eq!(call(get, &[h, p + 8]), OK);
            assert_eq!(r32(p + 8), max);
        }
        assert_eq!(call(GdipSetStringFormatAlign, &[h, 3]), INVALID_PARAMETER);
        assert_eq!(call(GdipGetStringFormatAlign, &[h, 0]), INVALID_PARAMETER);
        assert_eq!(call(GdipDeleteStringFormat, &[h]), OK);
        mem::heap_free(p);
    }

    #[test]
    fn string_format_generic_lifetime_and_clone() {
        let _guard = enter();
        let p = mem::heap_alloc(32, true);
        for (get, flags, trimming) in [
            (GdipStringFormatGetGenericDefault as Handler, 0, 1),
            (GdipStringFormatGetGenericTypographic as Handler, 0x6004, 0),
        ] {
            assert_eq!(call(get, &[0]), INVALID_PARAMETER);
            assert_eq!(call(get, &[p]), OK);
            let h = r32(p);
            assert_eq!(call(GdipGetStringFormatFlags, &[h, p + 8]), OK);
            assert_eq!(r32(p + 8), flags);
            assert_eq!(call(GdipGetStringFormatTrimming, &[h, p + 8]), OK);
            assert_eq!(r32(p + 8), trimming);
            assert_eq!(call(GdipSetStringFormatFlags, &[h, 0x1000]), OK);
            assert_eq!(call(GdipDeleteStringFormat, &[h]), OK);
            assert_eq!(call(get, &[p + 4]), OK);
            assert_eq!(r32(p + 4), h);
            assert_eq!(call(GdipGetStringFormatFlags, &[h, p + 8]), OK);
            assert_eq!(r32(p + 8), 0x1000);
            assert_eq!(call(GdipCloneStringFormat, &[h, p + 4]), OK);
            let cloned = r32(p + 4);
            assert_eq!(call(GdipDeleteStringFormat, &[cloned]), OK);
            assert_eq!(call(GdipGetStringFormatFlags, &[cloned, p + 8]), INVALID_PARAMETER);
            assert_eq!(call(GdipSetStringFormatFlags, &[h, flags]), OK);
        }
        mem::heap_free(p);
    }
}
