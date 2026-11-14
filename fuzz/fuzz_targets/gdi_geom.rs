//! sake::gdi_geom's clip-window paths (LineTo / Rectangle / Ellipse / RoundRect / Polygon to
//! far points draw only in the clip, review b06): any coordinates, windows and spans.
//! A windowed line is the whole line's steps in the window (when the whole line is small enough
//! to make), its counts add up; outline_in in a window is the whole outline cut to it; the
//! row-limited areas are the whole areas' rows; nothing panics and nothing costs more than the
//! window.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::gdi_geom::*;
use sake::user::Rect;

fn i(d: &[u8], k: usize) -> i32 {
    d.get(4 * k..4 * k + 4).map_or(0, |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fuzz_target!(|d: &[u8]| {
    let (a, b) = ((i(d, 0), i(d, 1)), (i(d, 2), i(d, 3)));
    let small = |v: i32| v.clamp(-200, 200);
    let win = Rect { l: small(i(d, 4)), t: small(i(d, 5)), r: small(i(d, 4)) + (i(d, 6) & 127), b: small(i(d, 5)) + (i(d, 7) & 127) };
    let last = d.first().is_some_and(|x| x & 1 != 0);
    let (skip, pts, steps) = line_pts_in(a, b, last, Some(win));
    assert!(skip >= 0 && skip + pts.len() as i64 <= steps);
    assert!(pts.len() <= 256, "{} points for a window of {:?}", pts.len(), win);
    // small lines: the window's points are the whole line's there, in order
    let (sa, sb) = ((small(a.0), small(a.1)), (small(b.0), small(b.1)));
    let all = line_pts(sa, sb, last);
    let (skip, got, steps) = line_pts_in(sa, sb, last, Some(win));
    assert_eq!(steps as usize, all.len());
    let xmajor = (sb.0 - sa.0).abs() >= (sb.1 - sa.1).abs();
    let inside = |p: &(i32, i32)| if xmajor { p.0 >= win.l && p.0 < win.r } else { p.1 >= win.t && p.1 < win.b };
    let want: Vec<_> = all.iter().copied().filter(inside).collect();
    assert_eq!(got, want);
    if let Some(f) = all.iter().position(inside) {
        assert_eq!(skip as usize, f);
    }
    // spans from the rest of the bytes: the windowed outline is the whole one cut
    let spans: Vec<Span> = d.get(32..).unwrap_or(&[]).chunks_exact(3).map(|c| ((c[0] as i8 / 8) as i32, (c[1] as i8 / 4) as i32, (c[1] as i8 / 4) as i32 + (c[2] & 31) as i32)).collect();
    let (we, wi) = outline_in(&spans, Some(win));
    let (ae, ai) = outline_in(&spans, None);
    let mut cut: Vec<_> = ae.into_iter().filter(|&(x, y)| win.has(x, y)).collect();
    let mut we = we;
    cut.sort();
    we.sort();
    assert_eq!(we, cut);
    let px = |v: &[Span]| {
        let mut p: Vec<(i32, i32)> = v.iter().flat_map(|&(y, a, b)| (a..b).map(move |x| (x, y))).collect();
        p.sort();
        p
    };
    assert_eq!(px(&wi), px(&ai).into_iter().filter(|&(x, y)| win.has(x, y)).collect::<Vec<_>>());
    // far areas cut to the window's rows: only those rows
    let rows = (win.t, win.b);
    let (l, t, r, bo) = (i(d, 8), i(d, 9), i(d, 10), i(d, 11));
    let e = ellipse_spans_rows(l, t, r, bo, rows);
    assert!(e.len() <= (rows.1 - rows.0).max(0) as usize && e.iter().all(|s| s.0 >= rows.0 && s.0 < rows.1));
    let rr = round_rect_spans_rows(l, t, r, bo, i(d, 12), i(d, 13), rows);
    assert!(rr.len() <= (rows.1 - rows.0).max(0) as usize);
    let poly: Vec<(i32, i32)> = (0..4).map(|k| (i(d, 14 + 2 * k), i(d, 15 + 2 * k))).collect();
    let ps = poly_spans_rows(&[poly], last, rows);
    assert!(ps.iter().all(|s| s.0 >= rows.0 && s.0 < rows.1));
});
