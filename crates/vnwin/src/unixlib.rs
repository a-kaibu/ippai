//! The Unix side of Wine's own PE DLLs that vnwin loads from the prefix's system32. Such a DLL
//! asks ntdll for `__wine_unix_call_dispatcher` and for its handle (NtQueryVirtualMemory with
//! MemoryWineUnixFuncs), then calls `dispatcher(handle, code, params)`. Here the handle is the
//! DLL's base and the backends are Rust: dwrite.dll's font backend (Wine's dlls/dwrite/freetype.c,
//! done with ttf-parser outlines and ab_glyph_rasterizer).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ab_glyph_rasterizer::{point, Point, Rasterizer};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::api::{host_fn, Ctx};
use crate::mem::{self, r32, w32, w8};

const STATUS_UNSUCCESSFUL: u32 = 0xc000_0001;
const STATUS_NOT_IMPLEMENTED: u32 = 0xc000_0002;
const STATUS_DLL_NOT_FOUND: u32 = 0xc000_0135;

/// The guest cell holding the dispatcher (ntdll's `__wine_unix_call_dispatcher` variable).
pub fn dispatcher_cell() -> u32 {
    static CELL: OnceLock<u32> = OnceLock::new();
    *CELL.get_or_init(|| {
        let cell = mem::heap_alloc(4, true);
        w32(cell, host_fn("__wine_unix_call_dispatcher", dispatch, 16));
        cell
    })
}

/// NtQueryVirtualMemory(MemoryWineUnixFuncs) for the module at `base`: its handle, if we have
/// a backend for it.
pub fn handle_for(base: u32) -> Result<u64, u32> {
    let name = crate::pe::with(base, |m| m.name.clone()).unwrap_or_default();
    match name.as_str() {
        "dwrite.dll" => Ok(base as u64),
        _ => Err(STATUS_DLL_NOT_FOUND),
    }
}

/// dispatcher(UINT64 handle, UINT code, void *params)
fn dispatch(c: &mut Ctx) -> u32 {
    let (base, code, p) = (c.a(0), c.a(2), c.a(3));
    match crate::pe::with(base, |m| m.name.clone()).unwrap_or_default().as_str() {
        "dwrite.dll" => dwrite(code, p),
        _ => STATUS_NOT_IMPLEMENTED,
    }
}

// --- dwrite.dll ---

static FONTS: Mutex<Option<HashMap<u32, (Vec<u8>, u32)>>> = Mutex::new(None);
static NEXT_FONT: Mutex<u32> = Mutex::new(1);

fn f32_at(a: u32) -> f32 {
    f32::from_bits(r32(a))
}

fn with_face<R>(object: u32, f: impl FnOnce(&Face) -> R) -> Option<R> {
    let g = FONTS.lock().unwrap();
    let (data, index) = g.as_ref()?.get(&object)?;
    let face = Face::parse(data, *index).ok()?;
    Some(f(&face))
}

#[derive(Clone, Copy)]
enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Default)]
struct Path(Vec<Seg>);

impl OutlineBuilder for Path {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Seg::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Seg::Line(x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.push(Seg::Quad(x1, y1, x, y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.push(Seg::Cubic(x1, y1, x2, y2, x, y));
    }
    fn close(&mut self) {
        self.0.push(Seg::Close);
    }
}

impl Path {
    /// The glyph's outline in pixels (y up) at `emsize`, through `m` ([xx xy; yx yy]).
    fn glyph(face: &Face, glyph: u32, emsize: f32, m: [f32; 4]) -> Path {
        let mut p = Path::default();
        face.outline_glyph(GlyphId(glyph as u16), &mut p);
        let s = emsize / face.units_per_em() as f32;
        let t = |x: f32, y: f32| {
            let (x, y) = (x * s, y * s);
            (m[0] * x + m[1] * y, m[2] * x + m[3] * y)
        };
        for seg in p.0.iter_mut() {
            *seg = match *seg {
                Seg::Move(x, y) => {
                    let (x, y) = t(x, y);
                    Seg::Move(x, y)
                }
                Seg::Line(x, y) => {
                    let (x, y) = t(x, y);
                    Seg::Line(x, y)
                }
                Seg::Quad(a, b, x, y) => {
                    let ((a, b), (x, y)) = (t(a, b), t(x, y));
                    Seg::Quad(a, b, x, y)
                }
                Seg::Cubic(a, b, c, d, x, y) => {
                    let ((a, b), (c, d), (x, y)) = (t(a, b), t(c, d), t(x, y));
                    Seg::Cubic(a, b, c, d, x, y)
                }
                Seg::Close => Seg::Close,
            }
        }
        p
    }

    fn points(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.0.iter().flat_map(|s| match *s {
            Seg::Move(x, y) | Seg::Line(x, y) => vec![(x, y)],
            Seg::Quad(a, b, x, y) => vec![(a, b), (x, y)],
            Seg::Cubic(a, b, c, d, x, y) => vec![(a, b), (c, d), (x, y)],
            Seg::Close => vec![],
        })
    }

    /// FT_GLYPH_BBOX_PIXELS of the control box, y flipped: (left, top, right, bottom)
    fn bbox(&self) -> [i32; 4] {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in self.points() {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        if x0 > x1 {
            return [0; 4];
        }
        [x0.floor() as i32, -(y1.ceil() as i32), x1.ceil() as i32, -(y0.floor() as i32)]
    }

    /// Coverage over the box `b` (left, top, right, bottom; y down)
    fn raster(&self, b: [i32; 4]) -> (usize, usize, Vec<f32>) {
        let (w, h) = ((b[2] - b[0]).max(0) as usize, (b[3] - b[1]).max(0) as usize);
        let mut r = Rasterizer::new(w, h);
        let pt = |x: f32, y: f32| point(x - b[0] as f32, -y - b[1] as f32);
        let (mut start, mut cur): (Point, Point) = (point(0.0, 0.0), point(0.0, 0.0));
        for s in &self.0 {
            match *s {
                Seg::Move(x, y) => {
                    if cur != start {
                        r.draw_line(cur, start);
                    }
                    start = pt(x, y);
                    cur = start;
                }
                Seg::Line(x, y) => {
                    let p = pt(x, y);
                    r.draw_line(cur, p);
                    cur = p;
                }
                Seg::Quad(a, bb, x, y) => {
                    let p = pt(x, y);
                    r.draw_quad(cur, pt(a, bb), p);
                    cur = p;
                }
                Seg::Cubic(a, bb, c, d, x, y) => {
                    let p = pt(x, y);
                    r.draw_cubic(cur, pt(a, bb), pt(c, d), p);
                    cur = p;
                }
                Seg::Close => {
                    if cur != start {
                        r.draw_line(cur, start);
                    }
                    cur = start;
                }
            }
        }
        if cur != start {
            r.draw_line(cur, start);
        }
        let mut out = vec![0f32; w * h];
        r.for_each_pixel(|i, v| out[i] = v.min(1.0));
        (w, h, out)
    }
}

/// FreeType's glyph transform for simulations (oblique) and a MATRIX_2X2, as [xx xy; yx yy]
fn transform(simulations: u32, m: [f32; 4]) -> [f32; 4] {
    let user = [m[0], -m[2], -m[1], m[3]];
    if simulations & 2 != 0 {
        // user * oblique [1 1/3; 0 1]
        [user[0], user[0] / 3.0 + user[1], user[2], user[2] / 3.0 + user[3]]
    } else {
        user
    }
}

fn dwrite(code: u32, p: u32) -> u32 {
    match code {
        0 | 1 => 0, // process_attach/detach
        2 => {
            // create_font_object(data, size, index, *object)
            let (data, size, index, out) = (r32(p), r32(p + 8), r32(p + 16), r32(p + 20));
            let bytes = mem::slice(data, size).to_vec();
            if Face::parse(&bytes, index).is_err() {
                return STATUS_UNSUCCESSFUL;
            }
            let id = {
                let mut n = NEXT_FONT.lock().unwrap();
                *n += 1;
                *n
            };
            FONTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, (bytes, index));
            w32(out, id);
            w32(out + 4, 0);
            0
        }
        3 => {
            FONTS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&r32(p));
            0
        }
        4 => get_glyph_outline(p),
        5 => {
            let n = with_face(r32(p), |f| f.number_of_glyphs() as u32).unwrap_or(0);
            w32(r32(p + 8), n);
            0
        }
        6 => {
            // get_glyph_advance(object, glyph, mode, emsize, *advance, *has_contours)
            let (glyph, mode, emsize) = (r32(p + 8), r32(p + 12), f32_at(p + 16));
            let r = with_face(r32(p), |f| {
                let adv = f.glyph_hor_advance(GlyphId(glyph as u16)).unwrap_or(0) as f32 * emsize / f.units_per_em() as f32;
                let adv = if mode == 0 { adv.floor() } else { adv.round() };
                (adv as i32, f.outline_glyph(GlyphId(glyph as u16), &mut Path::default()).is_some())
            });
            let (adv, contours) = r.unwrap_or((0, false));
            w32(r32(p + 20), adv as u32);
            w32(r32(p + 24), contours as u32);
            if r.is_some() { 0 } else { STATUS_UNSUCCESSFUL }
        }
        7 => {
            // get_glyph_bbox(object, simulations, glyph, emsize, m, *bbox)
            let (sim, glyph, emsize) = (r32(p + 8), r32(p + 12), f32_at(p + 16));
            let m = transform(sim, [f32_at(p + 20), f32_at(p + 24), f32_at(p + 28), f32_at(p + 32)]);
            let b = with_face(r32(p), |f| Path::glyph(f, glyph, emsize, m).bbox()).unwrap_or([0; 4]);
            let out = r32(p + 36);
            for (i, v) in b.iter().enumerate() {
                w32(out + 4 * i as u32, *v as u32);
            }
            0
        }
        8 => {
            // get_glyph_bitmap(object, simulations, glyph, mode, emsize, m, bbox, pitch, bitmap, *is_1bpp)
            let (sim, glyph, mode, emsize) = (r32(p + 8), r32(p + 12), r32(p + 16), f32_at(p + 20));
            let m = transform(sim, [f32_at(p + 24), f32_at(p + 28), f32_at(p + 32), f32_at(p + 36)]);
            let b = [0, 1, 2, 3].map(|i| r32(p + 40 + 4 * i) as i32);
            let (pitch, bitmap, is_1bpp) = (r32(p + 56), r32(p + 60), r32(p + 64));
            let aliased = mode == 1;
            w32(is_1bpp, aliased as u32);
            let Some((w, h, cov)) = with_face(r32(p), |f| Path::glyph(f, glyph, emsize, m).raster(b)) else {
                return STATUS_UNSUCCESSFUL;
            };
            for y in 0..h {
                let row = bitmap + y as u32 * pitch;
                for x in 0..w {
                    let v = cov[y * w + x];
                    if aliased {
                        if v >= 0.5 {
                            let a = row + (x as u32 >> 3);
                            w8(a, mem::r8(a) | 0x80 >> (x & 7));
                        }
                    } else if v > 0.0 {
                        w8(row + x as u32, (v * 255.0 + 0.5) as u8);
                    }
                }
            }
            0
        }
        9 => {
            // get_design_glyph_metrics(object, simulations, glyph, upem, ascent, *metrics)
            let (sim, glyph, upem, ascent, out) = (r32(p + 8), r32(p + 12), r32(p + 16), r32(p + 20) as i32, r32(p + 24));
            let r = with_face(r32(p), |f| {
                let g = GlyphId(glyph as u16);
                let bb = f.glyph_bounding_box(g);
                let (x0, y1, w, h) = bb.map_or((0, 0, 0, 0), |b| (b.x_min as i32, b.y_max as i32, b.width() as i32, b.height() as i32));
                let adv = f.glyph_hor_advance(g).unwrap_or(0) as i32;
                let vadv = f.glyph_ver_advance(g).map_or(f.ascender() as i32 - f.descender() as i32, |v| v as i32);
                (x0, y1, w, h, adv, vadv, bb.is_some())
            });
            let Some((x0, y1, w, h, mut adv, vadv, contours)) = r else { return STATUS_UNSUCCESSFUL };
            if sim & 1 != 0 && contours && adv != 0 {
                adv += (upem as i32 + 49) / 50;
            }
            let tsb = ascent - y1;
            for (i, v) in [x0, adv, adv - x0 - w, tsb, vadv, vadv - h - tsb, ascent].iter().enumerate() {
                w32(out + 4 * i as u32, *v as u32);
            }
            0
        }
        _ => STATUS_NOT_IMPLEMENTED,
    }
}

/// get_glyph_outline(object, simulations, glyph, emsize, *outline): D2D figures (begin with a
/// point, lines, cubic Béziers, end) into the caller's arrays, or their sizes when it has none.
fn get_glyph_outline(p: u32) -> u32 {
    let (sim, glyph, emsize, o) = (r32(p + 8), r32(p + 12), f32_at(p + 16), r32(p + 20));
    // flip Y; oblique before the flip
    let m = if sim & 2 != 0 { [1.0, 1.0 / 3.0, 0.0, -1.0] } else { [1.0, 0.0, 0.0, -1.0] };
    let Some(path) = with_face(r32(p), |f| Path::glyph(f, glyph, emsize, m)) else { return STATUS_UNSUCCESSFUL };
    let (mut tags, mut pts): (Vec<u8>, Vec<(f32, f32)>) = (Vec::new(), Vec::new());
    let (mut started, mut cur) = (false, (0.0, 0.0));
    let begin = |tags: &mut Vec<u8>, pts: &mut Vec<(f32, f32)>, started: &mut bool, at: (f32, f32)| {
        if !*started {
            tags.push(0);
            pts.push(at);
            *started = true;
        }
    };
    let mut start = (0.0, 0.0);
    for s in &path.0 {
        match *s {
            Seg::Move(x, y) => {
                if started {
                    tags.push(1);
                    started = false;
                }
                cur = (x, y);
                start = cur;
            }
            Seg::Line(x, y) => {
                if !started && (x, y) == cur {
                    continue;
                }
                begin(&mut tags, &mut pts, &mut started, cur);
                pts.push((x, y));
                tags.push(2);
                cur = (x, y);
            }
            Seg::Quad(a, b, x, y) => {
                begin(&mut tags, &mut pts, &mut started, cur);
                let c1 = (cur.0 + 2.0 / 3.0 * (a - cur.0), cur.1 + 2.0 / 3.0 * (b - cur.1));
                let c2 = (x + 2.0 / 3.0 * (a - x), y + 2.0 / 3.0 * (b - y));
                pts.extend([c1, c2, (x, y)]);
                tags.push(3);
                cur = (x, y);
            }
            Seg::Cubic(a, b, c, d, x, y) => {
                begin(&mut tags, &mut pts, &mut started, cur);
                pts.extend([(a, b), (c, d), (x, y)]);
                tags.push(3);
                cur = (x, y);
            }
            Seg::Close => {
                // FreeType's contours close themselves: a closing line only when not back at the start
                if started && cur != start {
                    pts.push(start);
                    tags.push(2);
                }
                cur = start;
            }
        }
    }
    if started {
        tags.push(1);
    }
    let (tv, tc, ts) = (r32(o), r32(o + 4), r32(o + 8));
    let (pv, pc, ps) = (r32(o + 12), r32(o + 16), r32(o + 20));
    if pv == 0 {
        w32(o + 4, tags.len() as u32);
        w32(o + 16, pts.len() as u32);
        return 0;
    }
    if tc + tags.len() as u32 > ts || pc + pts.len() as u32 > ps {
        return STATUS_UNSUCCESSFUL;
    }
    for (i, t) in tags.iter().enumerate() {
        w8(tv + tc + i as u32, *t);
    }
    for (i, (x, y)) in pts.iter().enumerate() {
        let a = pv + 8 * (pc + i as u32);
        w32(a, x.to_bits());
        w32(a + 4, y.to_bits());
    }
    w32(o + 4, tc + tags.len() as u32);
    w32(o + 16, pc + pts.len() as u32);
    0
}
