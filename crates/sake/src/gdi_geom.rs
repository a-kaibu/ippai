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
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let n = dx.abs().max(dy.abs());
    let mut v = Vec::with_capacity(n as usize + 1);
    let steps = if last { n + 1 } else { n };
    for k in 0..steps {
        let minor = |d: i32| a_minor(k as i64, d as i64, n as i64);
        if dx.abs() >= dy.abs() {
            v.push((a.0 + k * dx.signum(), a.1 + minor(dy)));
        } else {
            v.push((a.0 + minor(dx), a.1 + k * dy.signum()));
        }
    }
    v
}

fn a_minor(k: i64, d: i64, n: i64) -> i32 {
    if n == 0 {
        return 0;
    }
    // round(k * d / n), halves down: ceil(k * d / n - 1/2)
    ceil_div(2 * k * d - n, 2 * n) as i32
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
    // GDI coordinates are 28-bit (+-2^27); a polygon beyond them makes an empty region (as
    // on Windows, Wine's gdi32 clipping test) instead of 2^31 rows of edge table
    const LIM: i32 = 1 << 27;
    if polys
        .iter()
        .flatten()
        .any(|&(x, y)| !(-LIM..LIM).contains(&x) || !(-LIM..LIM).contains(&y))
    {
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
    let (Some(y0), Some(y1)) = (
        edges.iter().map(|e| e.0 .1.min(e.1 .1)).min(),
        edges.iter().map(|e| e.0 .1.max(e.1 .1)).max(),
    ) else {
        return Vec::new();
    };
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
            let num =
                top.0 as i64 * (bot.1 - top.1) as i64 + (y - top.1) as i64 * (bot.0 - top.0) as i64;
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
    let (w, h) = ((r - l) as f64, (b - t) as f64);
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let (cx, cy) = (l as f64 + w / 2.0, t as f64 + h / 2.0);
    let (a, bb) = (w / 2.0, h / 2.0);
    let mut out = Vec::new();
    for y in t..b {
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
    let (ew, eh) = (ew.abs().min(r - l), eh.abs().min(b - t));
    if ew < 2 || eh < 2 {
        return (t..b).filter(|_| r > l).map(|y| (y, l, r)).collect();
    }
    let e = ellipse_spans(0, 0, ew, eh);
    let mut out = Vec::new();
    for y in t..b {
        // corner rows use the ellipse's half rows
        let ey = if y - t < eh / 2 {
            Some(y - t)
        } else if b - 1 - y < eh / 2 {
            Some(eh - (b - y))
        } else {
            None
        };
        match ey.and_then(|ey| e.iter().find(|s| s.0 == ey)) {
            Some(&(_, x0, x1)) => {
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
    use std::collections::HashMap;
    let mut rows: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
    for &(y, x0, x1) in s {
        rows.entry(y).or_default().push((x0, x1));
    }
    let has = |x: i32, y: i32| {
        rows.get(&y)
            .is_some_and(|v| v.iter().any(|&(a, b)| x >= a && x < b))
    };
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
    let len = p
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) as usize)
        .sum::<usize>();
    let n = (len / 2).clamp(1, 512);
    let mut v = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let u = 1.0 - t;
        let f = |a: i32, b: i32, c: i32, d: i32| {
            u * u * u * a as f64
                + 3.0 * u * u * t * b as f64
                + 3.0 * u * t * t * c as f64
                + t * t * t * d as f64
        };
        let q = (
            f(p[0].0, p[1].0, p[2].0, p[3].0).round() as i32,
            f(p[0].1, p[1].1, p[2].1, p[3].1).round() as i32,
        );
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
    ivs_union(
        rects
            .iter()
            .filter(|r| r.t <= y0 && r.b >= y1)
            .map(|r| (r.l, r.r))
            .collect(),
    )
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
        out.extend(iv.iter().map(|&(l, r)| Rect {
            l,
            t: w[0],
            r,
            b: w[1],
        }));
        prev = if iv.is_empty() {
            None
        } else {
            Some((w[1], iv, start))
        };
    }
    out
}

fn edges_y(r: &[Rect]) -> Vec<i32> {
    let mut ys: Vec<i32> = r
        .iter()
        .filter(|r| !r.empty())
        .flat_map(|r| [r.t, r.b])
        .collect();
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
    banded(&edges_y(&all), |y0, y1| {
        ivs_op(&bands(&a, y0, y1), &bands(&b, y0, y1), f)
    })
}

/// Spans -> a banded region.
pub fn spans_region(s: &[Span]) -> Vec<Rect> {
    normalize(
        &s.iter()
            .map(|&(y, l, r)| Rect {
                l,
                t: y,
                r,
                b: y + 1,
            })
            .collect::<Vec<_>>(),
    )
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
        assert_eq!(
            line_pts((0, 0), (3, 0), false),
            vec![(0, 0), (1, 0), (2, 0)]
        );
    }

    #[test]
    fn regions_banded() {
        let a = vec![Rect {
            l: 0,
            t: 0,
            r: 10,
            b: 10,
        }];
        let b = vec![Rect {
            l: 5,
            t: 5,
            r: 15,
            b: 15,
        }];
        assert_eq!(combine(&a, &b, 2).len(), 3);
        assert_eq!(
            combine(&a, &b, 1),
            vec![Rect {
                l: 5,
                t: 5,
                r: 10,
                b: 10
            }]
        );
        assert_eq!(combine(&a, &b, 3).len(), 4);
        assert_eq!(normalize(&[a[0], a[0]]), a);
    }
}
