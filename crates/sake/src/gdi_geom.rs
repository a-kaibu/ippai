//! Pixel geometry of GDI drawing, in device coordinates: line pixels, pen nibs, polygon and
//! ellipse spans, and Y-X banded regions. Pure functions; gdi.rs draws with them. The rules
//! follow what tests/winref measured on Windows (gdi probe).

use crate::user::Rect;

/// A run of pixels: row y, x0..x1 (x1 exclusive).
pub type Span = (i32, i32, i32);

/// ceil(n / d) for d > 0.
fn ceil_div(n: i64, d: i64) -> i64 {
    -((-n).div_euclid(d))
}

/// The pixels of a line from a to b: one per step along the major axis; on the minor axis the
/// nearest pixel, ties to the smaller coordinate. `last` includes b.
pub fn line_pts(a: (i32, i32), b: (i32, i32), last: bool) -> Vec<(i32, i32)> {
    line_pts_in(a, b, last, None).1
}

/// line_pts, only the steps whose major coordinate lies in `win` (the clip, grown by a pen's
/// nib): (the steps before them, those pixels, all the steps). A line to a far point (LineTo
/// x = 0x3fffffff) asked for gigabytes of pixels before any were clipped; the pixels kept are
/// the very ones line_pts gives at those steps (a dash pattern goes on counting the others).
pub fn line_pts_in(a: (i32, i32), b: (i32, i32), last: bool, win: Option<Rect>) -> (i64, Vec<(i32, i32)>, i64) {
    let (dx, dy) = (b.0 as i64 - a.0 as i64, b.1 as i64 - a.1 as i64);
    let n = dx.abs().max(dy.abs());
    let steps = if last { n + 1 } else { n };
    let xmajor = dx.abs() >= dy.abs();
    let (k0, k1) = match win {
        None => (0, steps),
        Some(w) => {
            // the major coordinate a + k * sign within lo..hi
            let (from, sign, lo, hi) = if xmajor { (a.0 as i64, dx.signum(), w.l as i64, w.r as i64) } else { (a.1 as i64, dy.signum(), w.t as i64, w.b as i64) };
            let (k0, k1) = match sign {
                0 => if (lo..hi).contains(&from) { (0, steps) } else { (0, 0) },
                1 => (lo - from, hi - from),
                _ => (from - hi + 1, from - lo + 1),
            };
            (k0.clamp(0, steps), k1.clamp(0, steps))
        }
    };
    let mut v = Vec::with_capacity((k1 - k0).max(0) as usize);
    for k in k0..k1 {
        let minor = |d: i64| a_minor(k, d, n);
        if xmajor {
            v.push(((a.0 as i64 + k * dx.signum()) as i32, (a.1 as i64 + minor(dy)) as i32));
        } else {
            v.push(((a.0 as i64 + minor(dx)) as i32, (a.1 as i64 + k * dy.signum()) as i32));
        }
    }
    (k0.min(k1), v, steps)
}

fn a_minor(k: i64, d: i64, n: i64) -> i64 {
    if n == 0 {
        return 0;
    }
    // round(k * d / n), halves down: ceil(k * d / n - 1/2) (in i128: k * d of a line across
    // the whole coordinate range is past i64)
    let (num, den) = (2 * k as i128 * d as i128 - n as i128, 2 * n as i128);
    -((-num).div_euclid(den)) as i64
}

/// The offsets a wide pen covers around each point of its path (a disc of diameter w).
pub fn nib(w: i32) -> Vec<(i32, i32)> {
    if w <= 1 {
        return vec![(0, 0)];
    }
    // diameter w: the pixel centres within (w - 1) / 2 of the point (3 -> a plus)
    let r2 = (w - 1) * (w - 1);
    let h = w / 2;
    let mut v = Vec::new();
    for y in -h..=h {
        for x in -h..=h {
            if 4 * (x * x + y * y) <= r2 {
                v.push((x, y));
            }
        }
    }
    v
}

/// Spans inside polygons (each implicitly closed): a pixel is in when its corner point (x, y)
/// is, left and top edges in, right and bottom out. ALTERNATE or WINDING.
pub fn poly_spans(polys: &[Vec<(i32, i32)>], winding: bool) -> Vec<Span> {
    poly_spans_rows(polys, winding, ALL_ROWS)
}

/// Every row.
pub const ALL_ROWS: (i32, i32) = (i32::MIN, i32::MAX);

/// poly_spans of the rows `rows.0..rows.1` only (the clip's: a polygon 2^27 rows tall walked
/// every one of them, times its edges, to fill the few on the bitmap).
pub fn poly_spans_rows(polys: &[Vec<(i32, i32)>], winding: bool, rows: (i32, i32)) -> Vec<Span> {
    // GDI coordinates are 28-bit (+-2^27); a polygon beyond them makes an empty region (as
    // on Windows, Wine's gdi32 clipping test) instead of 2^31 rows of edge table
    const LIM: i32 = 1 << 27;
    if polys.iter().flatten().any(|&(x, y)| !(-LIM..LIM).contains(&x) || !(-LIM..LIM).contains(&y)) {
        return Vec::new();
    }
    let mut edges = Vec::new();
    for p in polys {
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            if a.1 != b.1 {
                edges.push((a, b));
            }
        }
    }
    let (Some(y0), Some(y1)) = (edges.iter().map(|e| e.0 .1.min(e.1 .1)).min(), edges.iter().map(|e| e.0 .1.max(e.1 .1)).max()) else {
        return Vec::new();
    };
    let (y0, y1) = (y0.max(rows.0), y1.min(rows.1));
    let mut out = Vec::new();
    let mut xs: Vec<(i64, i64, i32)> = Vec::new();
    for y in y0..y1 {
        xs.clear();
        for &(a, b) in &edges {
            let (top, bot, dir) = if a.1 < b.1 { (a, b, 1) } else { (b, a, -1) };
            if y < top.1 || y >= bot.1 {
                continue;
            }
            // x = top.x + (y - top.y) * (bot.x - top.x) / (bot.y - top.y), as a fraction
            let num = top.0 as i64 * (bot.1 - top.1) as i64 + (y - top.1) as i64 * (bot.0 - top.0) as i64;
            xs.push((num, (bot.1 - top.1) as i64, dir));
        }
        xs.sort_by(|p, q| (p.0 as i128 * q.1 as i128).cmp(&(q.0 as i128 * p.1 as i128)));
        let mut wind = 0;
        for i in 0..xs.len() {
            let inside_before = if winding { wind != 0 } else { wind & 1 != 0 };
            wind += if winding { xs[i].2 } else { 1 };
            let inside = if winding { wind != 0 } else { wind & 1 != 0 };
            if !inside_before && inside {
                let l = ceil_div(xs[i].0, xs[i].1) as i32;
                // find where it ends
                let mut w2 = wind;
                for j in i + 1..xs.len() {
                    w2 += if winding { xs[j].2 } else { 1 };
                    let ins = if winding { w2 != 0 } else { w2 & 1 != 0 };
                    if !ins {
                        let r = ceil_div(xs[j].0, xs[j].1) as i32;
                        if r > l {
                            out.push((y, l, r));
                        }
                        break;
                    }
                }
            }
        }
    }
    out
}

/// Spans of the ellipse in the box l..r, t..b (exclusive): the pixels whose centres are inside.
pub fn ellipse_spans(l: i32, t: i32, r: i32, b: i32) -> Vec<Span> {
    ellipse_spans_rows(l, t, r, b, ALL_ROWS)
}
/// ellipse_spans of the rows `rows.0..rows.1` only.
pub fn ellipse_spans_rows(l: i32, t: i32, r: i32, b: i32, rows: (i32, i32)) -> Vec<Span> {
    let (w, h) = ((r as f64 - l as f64), (b as f64 - t as f64));
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let (cx, cy) = (l as f64 + w / 2.0, t as f64 + h / 2.0);
    let (a, bb) = (w / 2.0, h / 2.0);
    let mut out = Vec::new();
    for y in t.max(rows.0)..b.min(rows.1) {
        let dy = (y as f64 + 0.5 - cy) / bb;
        let q = 1.0 - dy * dy;
        if q < 0.0 {
            continue;
        }
        let hw = a * q.sqrt();
        // centres x + 0.5 within cx +- hw
        let x0 = (cx - hw - 0.5).ceil() as i32;
        let x1 = (cx + hw - 0.5).floor() as i32 + 1;
        if x1 > x0 {
            out.push((y, x0.max(l), x1.min(r)));
        }
    }
    out
}

/// Spans of a rectangle with corners rounded by ellipses of ew x eh.
pub fn round_rect_spans(l: i32, t: i32, r: i32, b: i32, ew: i32, eh: i32) -> Vec<Span> {
    round_rect_spans_rows(l, t, r, b, ew, eh, ALL_ROWS)
}
/// round_rect_spans of the rows `rows.0..rows.1` only.
pub fn round_rect_spans_rows(l: i32, t: i32, r: i32, b: i32, ew: i32, eh: i32, rows: (i32, i32)) -> Vec<Span> {
    let (ew, eh) = (ew.saturating_abs().min(r.saturating_sub(l)), eh.saturating_abs().min(b.saturating_sub(t)));
    let (y0, y1) = (t.max(rows.0), b.min(rows.1));
    if ew < 2 || eh < 2 {
        return (y0..y1).filter(|_| r > l).map(|y| (y, l, r)).collect();
    }
    // (the corner ellipse's row of each corner row on its own: each looked its row up in all
    // of them, and all of them were made for a corner of any height)
    let row = |ey: i32| ellipse_spans_rows(0, 0, ew, eh, (ey, ey + 1)).first().copied();
    let mut out = Vec::new();
    for y in y0..y1 {
        // corner rows use the ellipse's half rows
        let ey = if y - t < eh / 2 { Some(y - t) } else if b - 1 - y < eh / 2 { Some(eh - (b - y)) } else { None };
        match ey.and_then(row) {
            Some((_, x0, x1)) => {
                let (il, ir) = (x0, ew - x1);
                if r - ir > l + il {
                    out.push((y, l + il, r - ir));
                }
            }
            None if ey.is_some() => {}
            None => out.push((y, l, r)),
        }
    }
    out
}

/// Spans -> a set lookup: does (x, y) lie in them? (spans sorted by y)
pub fn spans_has(s: &[Span], x: i32, y: i32) -> bool {
    s.iter().any(|&(sy, x0, x1)| sy == y && x >= x0 && x < x1)
}

/// The border pixels of an area (a pixel with a 4-neighbour outside) and the rest.
pub fn outline(s: &[Span]) -> (Vec<(i32, i32)>, Vec<Span>) {
    outline_in(s, None)
}

/// outline, from the spans (no per-pixel lookups: four hash lookups and a row scan per pixel
/// of every Rectangle drawn with a pen), only within `win` when given (a rectangle 2^30 wide
/// had every pixel of its top row listed). The spans must cover `win` grown by one pixel for
/// its border to be right there.
pub fn outline_in(s: &[Span], win: Option<Rect>) -> (Vec<(i32, i32)>, Vec<Span>) {
    let mut rows: std::collections::BTreeMap<i32, Vec<(i32, i32)>> = Default::default();
    for &(y, x0, x1) in s {
        if x1 > x0 {
            rows.entry(y).or_default().push((x0, x1));
        }
    }
    // each row's runs sorted and merged (touching runs are one: the pixels between are in)
    for v in rows.values_mut() {
        v.sort_unstable();
        let mut m: Vec<(i32, i32)> = Vec::with_capacity(v.len());
        for &(a, b) in v.iter() {
            match m.last_mut() {
                Some(l) if a <= l.1 => l.1 = l.1.max(b),
                _ => m.push((a, b)),
            }
        }
        *v = m;
    }
    let none: Vec<(i32, i32)> = Vec::new();
    let w = win.unwrap_or(Rect { l: i32::MIN, t: i32::MIN, r: i32::MAX, b: i32::MAX });
    let mut edge = Vec::new();
    let mut inner = Vec::new();
    for (&y, runs) in &rows {
        if y < w.t || y >= w.b {
            continue;
        }
        let up = y.checked_sub(1).and_then(|u| rows.get(&u)).unwrap_or(&none);
        let down = y.checked_add(1).and_then(|d| rows.get(&d)).unwrap_or(&none);
        for &(x0, x1) in runs {
            // inside: both horizontal neighbours in the run, and the pixels above and below in
            let mut ins: Vec<(i32, i32)> = Vec::new();
            let core = (x0.saturating_add(1), x1.saturating_sub(1));
            if core.1 > core.0 {
                for &(a, b) in up {
                    let (l, r) = (a.max(core.0), b.min(core.1));
                    if r <= l {
                        continue;
                    }
                    for &(c, d) in down {
                        let (l2, r2) = (c.max(l), d.min(r));
                        if r2 > l2 {
                            ins.push((l2, r2));
                        }
                    }
                }
            }
            ins.sort_unstable();
            // the run's pixels: the inside ones as spans, the rest as points (in the window)
            let (wl, wr) = (x0.max(w.l), x1.min(w.r));
            let mut x = wl;
            for &(a, b) in &ins {
                let (a, b) = (a.max(wl), b.min(wr));
                if b <= a {
                    continue;
                }
                edge.extend((x..a).map(|e| (e, y)));
                inner.push((y, a, b));
                x = b;
            }
            if wr > x {
                edge.extend((x..wr).map(|e| (e, y)));
            }
        }
    }
    (edge, inner)
}

#[cfg(test)]
/// outline as it was: per pixel, the reference for outline_in.
pub fn outline_old(s: &[Span]) -> (Vec<(i32, i32)>, Vec<Span>) {
    use std::collections::HashMap;
    let mut rows: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
    for &(y, x0, x1) in s {
        rows.entry(y).or_default().push((x0, x1));
    }
    let has = |x: i32, y: i32| rows.get(&y).is_some_and(|v| v.iter().any(|&(a, b)| x >= a && x < b));
    let mut edge = Vec::new();
    let mut inner = Vec::new();
    for &(y, x0, x1) in s {
        let mut run: Option<i32> = None;
        for x in x0..x1 {
            let e = !has(x - 1, y) || !has(x + 1, y) || !has(x, y - 1) || !has(x, y + 1);
            if e {
                edge.push((x, y));
                if let Some(a) = run.take() {
                    inner.push((y, a, x));
                }
            } else if run.is_none() {
                run = Some(x);
            }
        }
        if let Some(a) = run {
            inner.push((y, a, x1));
        }
    }
    (edge, inner)
}

/// The angle of (x, y) around (cx, cy), counter-clockwise from +x with y down, 0..2pi.
pub fn angle(cx: f64, cy: f64, x: f64, y: f64) -> f64 {
    let a = (cy - y).atan2(x - cx);
    if a < 0.0 {
        a + std::f64::consts::TAU
    } else {
        a
    }
}

/// Is angle a on the counter-clockwise arc from s to e?
pub fn on_arc(a: f64, s: f64, e: f64) -> bool {
    if s <= e {
        a >= s && a <= e
    } else {
        a >= s || a <= e
    }
}

/// Points of a cubic Bezier, flattened.
pub fn bezier(p: [(i32, i32); 4]) -> Vec<(i32, i32)> {
    let len = p.windows(2).map(|w| ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) as usize).sum::<usize>();
    let n = (len / 2).clamp(1, 512);
    let mut v = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let u = 1.0 - t;
        let f = |a: i32, b: i32, c: i32, d: i32| u * u * u * a as f64 + 3.0 * u * u * t * b as f64 + 3.0 * u * t * t * c as f64 + t * t * t * d as f64;
        let q = (f(p[0].0, p[1].0, p[2].0, p[3].0).round() as i32, f(p[0].1, p[1].1, p[2].1, p[3].1).round() as i32);
        if v.last() != Some(&q) {
            v.push(q);
        }
    }
    v
}

// --- regions: Y-X banded rectangle lists (Windows' own form)

fn ivs_union(mut v: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
    v.sort();
    let mut out: Vec<(i32, i32)> = Vec::new();
    for (a, b) in v {
        if a >= b {
            continue;
        }
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn ivs_op(a: &[(i32, i32)], b: &[(i32, i32)], f: fn(bool, bool) -> bool) -> Vec<(i32, i32)> {
    let mut xs: Vec<i32> = a.iter().chain(b).flat_map(|&(l, r)| [l, r]).collect();
    xs.sort();
    xs.dedup();
    let inn = |v: &[(i32, i32)], x: i32| v.iter().any(|&(l, r)| x >= l && x < r);
    let mut out: Vec<(i32, i32)> = Vec::new();
    for w in xs.windows(2) {
        if f(inn(a, w[0]), inn(b, w[0])) {
            match out.last_mut() {
                Some(l) if l.1 == w[0] => l.1 = w[1],
                _ => out.push((w[0], w[1])),
            }
        }
    }
    out
}

fn bands(rects: &[Rect], y0: i32, y1: i32) -> Vec<(i32, i32)> {
    ivs_union(rects.iter().filter(|r| r.t <= y0 && r.b >= y1).map(|r| (r.l, r.r)).collect())
}

fn banded(ys: &[i32], mut row: impl FnMut(i32, i32) -> Vec<(i32, i32)>) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    let mut prev: Option<(i32, Vec<(i32, i32)>, usize)> = None;
    for w in ys.windows(2) {
        let iv = row(w[0], w[1]);
        // coalesce with the band above when it is the same and touches
        if let Some((pb, piv, start)) = &prev {
            if *pb == w[0] && *piv == iv && !iv.is_empty() {
                for r in &mut out[*start..] {
                    r.b = w[1];
                }
                prev = Some((w[1], iv, *start));
                continue;
            }
        }
        let start = out.len();
        out.extend(iv.iter().map(|&(l, r)| Rect { l, t: w[0], r, b: w[1] }));
        prev = if iv.is_empty() { None } else { Some((w[1], iv, start)) };
    }
    out
}

fn edges_y(r: &[Rect]) -> Vec<i32> {
    let mut ys: Vec<i32> = r.iter().filter(|r| !r.empty()).flat_map(|r| [r.t, r.b]).collect();
    ys.sort();
    ys.dedup();
    ys
}

/// Any rectangles -> the banded form.
pub fn normalize(r: &[Rect]) -> Vec<Rect> {
    let r: Vec<Rect> = r.iter().copied().filter(|r| !r.empty()).collect();
    banded(&edges_y(&r), |a, b| bands(&r, a, b))
}

/// RGN_AND 1, OR 2, XOR 3, DIFF 4 of two regions, banded.
pub fn combine(a: &[Rect], b: &[Rect], mode: u32) -> Vec<Rect> {
    let f: fn(bool, bool) -> bool = match mode {
        1 => |p, q| p && q,
        2 => |p, q| p || q,
        3 => |p, q| p != q,
        4 => |p, q| p && !q,
        _ => |p, _| p,
    };
    let mut all = a.to_vec();
    all.extend_from_slice(b);
    let a: Vec<Rect> = a.iter().copied().filter(|r| !r.empty()).collect();
    let b: Vec<Rect> = b.iter().copied().filter(|r| !r.empty()).collect();
    banded(&edges_y(&all), |y0, y1| ivs_op(&bands(&a, y0, y1), &bands(&b, y0, y1), f))
}

/// Spans -> a banded region.
pub fn spans_region(s: &[Span]) -> Vec<Rect> {
    normalize(&s.iter().map(|&(y, l, r)| Rect { l, t: y, r, b: y + 1 }).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_tie_down() {
        // (32,32)-(50,0): x 45.5 at y 8 -> 45
        assert!(line_pts((32, 32), (50, 0), false).contains(&(45, 8)));
        assert!(line_pts((32, 32), (13, 0), false).contains(&(22, 16)));
        assert!(line_pts((32, 32), (0, 45), false).contains(&(16, 38)));
        assert_eq!(line_pts((0, 0), (3, 0), false), vec![(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn regions_banded() {
        let a = vec![Rect { l: 0, t: 0, r: 10, b: 10 }];
        let b = vec![Rect { l: 5, t: 5, r: 15, b: 15 }];
        assert_eq!(combine(&a, &b, 2).len(), 3);
        assert_eq!(combine(&a, &b, 1), vec![Rect { l: 5, t: 5, r: 10, b: 10 }]);
        assert_eq!(combine(&a, &b, 3).len(), 4);
        assert_eq!(normalize(&[a[0], a[0]]), a);
    }

    /// line_pts as it was (i32 steps), the reference.
    fn old_line_pts(a: (i32, i32), b: (i32, i32), last: bool) -> Vec<(i32, i32)> {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let n = dx.abs().max(dy.abs());
        let steps = if last { n + 1 } else { n };
        (0..steps)
            .map(|k| {
                let minor = |d: i32| a_minor(k as i64, d as i64, n as i64) as i32;
                if dx.abs() >= dy.abs() { (a.0 + k * dx.signum(), a.1 + minor(dy)) } else { (a.0 + minor(dx), a.1 + k * dy.signum()) }
            })
            .collect()
    }

    fn pixels(v: &[Span]) -> std::collections::BTreeSet<(i32, i32)> {
        v.iter().flat_map(|&(y, a, b)| (a..b).map(move |x| (x, y))).collect()
    }

    proptest::proptest! {
        /// The steps a window keeps are line_pts' own at those steps, in order, the skipped
        /// ones counted (a dash pattern goes on as if all were drawn)
        #[test]
        fn windowed_lines_are_the_whole_lines_cut(a in (-300i32..300, -300i32..300), b in (-300i32..300, -300i32..300), last in proptest::bool::ANY,
                                                   w in (-200i32..200, -200i32..200, 0i32..150, 0i32..150)) {
            let all = old_line_pts(a, b, last);
            proptest::prop_assert_eq!(line_pts(a, b, last), all.clone());
            let win = Rect { l: w.0, t: w.1, r: w.0 + w.2, b: w.1 + w.3 };
            let (skip, got, steps) = line_pts_in(a, b, last, Some(win));
            proptest::prop_assert_eq!(steps as usize, all.len());
            let xmajor = (b.0 - a.0).abs() >= (b.1 - a.1).abs();
            let inside = |p: &(i32, i32)| if xmajor { p.0 >= win.l && p.0 < win.r } else { p.1 >= win.t && p.1 < win.b };
            let want: Vec<_> = all.iter().copied().filter(inside).collect();
            proptest::prop_assert_eq!(&got, &want);
            if let Some(first) = all.iter().position(inside) {
                proptest::prop_assert_eq!(skip as usize, first);
            }
        }

        /// outline_in is the old per-pixel outline, and within a window that part of it, for
        /// any spans (overlapping, touching, several a row)
        #[test]
        fn outlines_from_spans_are_the_per_pixel_ones(s in proptest::collection::vec((-8i32..8, -10i32..10, 0i32..12), 0..40), w in (-12i32..12, -12i32..12, 0i32..20, 0i32..20)) {
            let s: Vec<Span> = s.into_iter().map(|(y, a, n)| (y, a, a + n)).collect();
            let (oe, oi) = outline_old(&s);
            let (ne, ni) = outline_in(&s, None);
            let set = |v: &[(i32, i32)]| v.iter().copied().collect::<std::collections::BTreeSet<_>>();
            proptest::prop_assert_eq!(set(&ne), set(&oe));
            proptest::prop_assert_eq!(ne.len(), set(&ne).len(), "an edge pixel twice");
            proptest::prop_assert_eq!(pixels(&ni), pixels(&oi));
            proptest::prop_assert_eq!(ni.iter().map(|x| x.2 - x.1).sum::<i32>() as usize, pixels(&ni).len(), "inner spans overlap");
            let win = Rect { l: w.0, t: w.1, r: w.0 + w.2, b: w.1 + w.3 };
            let (we, wi) = outline_in(&s, Some(win));
            proptest::prop_assert_eq!(set(&we), set(&oe).into_iter().filter(|&(x, y)| win.has(x, y)).collect::<std::collections::BTreeSet<_>>());
            proptest::prop_assert_eq!(pixels(&wi), pixels(&oi).into_iter().filter(|&(x, y)| win.has(x, y)).collect::<std::collections::BTreeSet<_>>());
        }

        /// The row-limited areas are the whole ones' rows
        #[test]
        fn row_limited_areas_are_the_whole_ones_rows(l in -40i32..40, t in -40i32..40, w in 0i32..60, h in 0i32..60, ew in -30i32..30, eh in -30i32..30, rows in (-50i32..50, 0i32..60),
                                                     poly in proptest::collection::vec((-50i32..50, -50i32..50), 0..7), winding in proptest::bool::ANY) {
            let rows = (rows.0, rows.0 + rows.1);
            let cut = |v: Vec<Span>| v.into_iter().filter(|s| s.0 >= rows.0 && s.0 < rows.1).collect::<Vec<_>>();
            proptest::prop_assert_eq!(ellipse_spans_rows(l, t, l + w, t + h, rows), cut(ellipse_spans(l, t, l + w, t + h)));
            proptest::prop_assert_eq!(round_rect_spans_rows(l, t, l + w, t + h, ew, eh, rows), cut(round_rect_spans(l, t, l + w, t + h, ew, eh)));
            proptest::prop_assert_eq!(poly_spans_rows(&[poly.clone()], winding, rows), cut(poly_spans(&[poly], winding)));
        }
    }

    /// Far points (as LineTo to 0x3fffffff, which Windows clips cheaply) cost the window, not
    /// the line; a corner of any height does not make all its rows
    #[test]
    fn far_shapes_cost_their_window() {
        let win = Rect { l: 0, t: 0, r: 64, b: 64 };
        let t0 = std::time::Instant::now();
        let (skip, pts, steps) = line_pts_in((0, 10), (0x3fff_ffff, 20), true, Some(win));
        assert_eq!((skip, pts.len(), steps), (0, 64, 0x4000_0000));
        assert!(pts.iter().all(|p| p.1 == 10));
        let (_, pts, _) = line_pts_in((i32::MIN, i32::MIN), (i32::MAX, i32::MAX), false, Some(win));
        assert_eq!(pts, (0..64).map(|k| (k, k)).collect::<Vec<_>>());
        let r = round_rect_spans_rows(-(1 << 30), -(1 << 30), 1 << 30, 1 << 30, 1 << 29, 1 << 29, (0, 64));
        assert_eq!(r.len(), 64);
        let (e, i) = outline_in(&[(0, -(1 << 30), 1 << 30), (1, -(1 << 30), 1 << 30), (2, -(1 << 30), 1 << 30)], Some(win));
        assert_eq!(e.len(), 128);
        assert_eq!(i, vec![(1, 0, 64)]);
        assert!(t0.elapsed() < std::time::Duration::from_secs(2));
    }
}
