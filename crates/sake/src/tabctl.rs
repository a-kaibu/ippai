//! The tab control (SysTabControl32) as comctl32 5.x has it (no visual styles): items with
//! text and lParam (TCM_SETITEMEXTRA bytes), the selection and focus, the layout of the tabs
//! from the font (a tab is its text plus the padding, at least six average characters; a row
//! is tmHeight + twice the vertical padding - 1 high, as Windows measures it; multi-line rows evened out and justified, the
//! selected row next to the display area), TCM_GETITEMRECT / TCM_ADJUSTRECT / TCM_HITTEST
//! from that, clicks and arrow keys asking the parent (TCN_SELCHANGING may refuse, then
//! TCN_SELCHANGE), and the classic look: raised soft edges around the tabs and the display.
//! Tabs at the bottom are laid out and hit-tested but drawn as at the top; vertical tabs, image
//! lists, owner-drawn tabs and the scroll arrows of a single line too narrow are not done.

use super::*;

struct Item {
    /// None: inserted without TCIF_TEXT
    text: Option<Vec<u16>>,
    /// lParam and what TCM_SETITEMEXTRA added
    extra: Vec<u8>,
    state: u32,
    image: i32,
}

struct Tab {
    items: Vec<Item>,
    sel: i32,
    focus: i32,
    font: u32,
    /// the padding the layout uses, and the one TCM_SETPADDING set: it takes effect at the next
    /// change that lays the tabs out again (`dirty`: items, text, font, size, item size, minimum
    /// width), not by itself
    pad: (i32, i32),
    pad_set: (i32, i32),
    dirty: bool,
    min_width: i32,
    /// TCM_SETITEMSIZE's width (TCS_FIXEDWIDTH) and height (0: from the font)
    width: i32,
    height: i32,
    extra: usize,
    ex_style: u32,
    images: u32,
    tooltips: u32,
}

impl Default for Tab {
    fn default() -> Tab {
        // (the width of a fixed-width tab before TCM_SETITEMSIZE is LOGPIXELSX)
        Tab { items: Vec::new(), sel: -1, focus: -1, font: 0, pad: (6, 3), pad_set: (6, 3), dirty: false, min_width: -1, width: 96, height: 0, extra: 4, ex_style: 0, images: 0, tooltips: 0 }
    }
}

static TABS: Mutex<Option<HashMap<u32, Tab>>> = Mutex::new(None);

fn tab<R>(h: u32, f: impl FnOnce(&mut Tab) -> R) -> R {
    f(TABS.lock().unwrap().get_or_insert_with(HashMap::default).entry(h).or_default())
}

const TCS_BOTTOM: u32 = 0x2;
const TCS_MULTILINE: u32 = 0x200;
const TCS_BUTTONS: u32 = 0x100;
const TCS_FLATBUTTONS: u32 = 0x8;
const TCS_FIXEDWIDTH: u32 = 0x400;
const TCS_RAGGEDRIGHT: u32 = 0x800;
const TCS_FOCUSNEVER: u32 = 0x8000;
const TCS_VERTICAL: u32 = 0x80;
const SELECTED: i32 = 2; // SELECTED_TAB_OFFSET
const BORDER: i32 = 2; // CONTROL_BORDER_SIZEX/Y, also DISPLAY_AREA_PADDINGX/Y

/// Where the tabs are: their rectangles in client coordinates, the row height, the rows.
pub(super) struct Layout {
    pub items: Vec<Rect>,
    pub tab_h: i32,
    pub rows: i32,
    /// tmAveCharWidth * 6 + both paddings (what TCM_SETMINTABWIDTH answers by default)
    pub default_min: i32,
}

/// tmHeight, tmAveCharWidth and each item's text width with the control's font.
fn measure(c: &mut Ctx, h: u32, font: u32, texts: &[Option<Vec<u16>>]) -> (i32, i32, Vec<i32>) {
    let dc = gcall(c, "user32", "GetDC", &[h]);
    let f = if font != 0 { font } else { gcall(c, "gdi32", "GetStockObject", &[13]) };
    let old = gcall(c, "gdi32", "SelectObject", &[dc, f]);
    let len = texts.iter().flatten().map(|t| t.len()).max().unwrap_or(0) as u32;
    let buf = mem::heap_alloc(64 + 2 * len + 2, true);
    gcall(c, "gdi32", "GetTextMetricsW", &[dc, buf]);
    let (tm_h, ave) = (r32(buf) as i32, r32(buf + 20) as i32);
    let widths = texts
        .iter()
        .map(|t| {
            let Some(t) = t else { return 0 };
            for (i, &ch) in t.iter().enumerate() {
                w16(buf + 8 + 2 * i as u32, ch);
            }
            w32(buf, 0);
            gcall(c, "gdi32", "GetTextExtentPoint32W", &[dc, buf + 8, t.len() as u32, buf]);
            r32(buf) as i32
        })
        .collect();
    mem::heap_free(buf);
    gcall(c, "gdi32", "SelectObject", &[dc, old]);
    gcall(c, "user32", "ReleaseDC", &[h, dc]);
    (tm_h, ave, widths)
}

pub(super) fn layout(c: &mut Ctx, h: u32) -> Layout {
    let (style, client) = with(|s| s.wins.get(&h).map_or((0, Rect::default()), |w| (w.style, Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() })));
    let (font, texts, pad, min_width, width, height, sel) = tab(h, |t| {
        if std::mem::take(&mut t.dirty) {
            t.pad = t.pad_set;
        }
        (t.font, t.items.iter().map(|i| i.text.clone()).collect::<Vec<_>>(), t.pad, t.min_width, t.width, t.height, t.sel)
    });
    let (tm_h, ave, text_w) = measure(c, h, font, &texts);
    let buttons = style & TCS_BUTTONS != 0;
    let multi = style & (TCS_MULTILINE | TCS_VERTICAL) != 0;
    let default_min = ave * 6 + pad.0 * 2;
    // (Windows: tmHeight + 5 with the default vertical padding 3, + 9 with 5, never below + 3:
    // padding 0..2 all give + 3, winref tabctl)
    let tab_h = if height != 0 { height } else if buttons { tm_h + 2 + 2 * pad.1 } else { tm_h + (2 * pad.1 - 1).max(3) };
    let n = texts.len();
    // (left, right, row) with every row starting at 0
    let mut cells: Vec<(i32, i32, i32)> = Vec::with_capacity(n);
    let (mut x, mut row) = (0, 0);
    for (i, t) in texts.iter().enumerate() {
        let w = if style & TCS_FIXEDWIDTH != 0 {
            width
        } else if t.is_none() {
            if min_width < 0 { default_min } else { min_width }
        } else {
            (text_w[i] + 2 * pad.0).max(if min_width < 0 { default_min } else { min_width })
        };
        let (mut l, mut r) = (x, x + w);
        if multi && r > client.r - BORDER - BORDER {
            r -= l;
            l = 0;
            row += 1;
        }
        cells.push((l, r, row));
        x = if buttons { r + 3 + if style & TCS_FLATBUTTONS != 0 { 8 } else { 0 } } else { r };
    }
    let rows = if n == 0 { 0 } else { row + 1 };
    if multi && style & TCS_RAGGEDRIGHT == 0 && rows > 1 {
        // the same number of tabs in each row (the first rows take the remainder), each row
        // stretched to the width
        let (per, rem) = (n as i32 / rows, n as i32 % rows);
        let (mut r_i, mut count, mut x) = (0, 0, 0);
        for cell in cells.iter_mut() {
            let w = cell.1 - cell.0;
            if count >= if r_i < rem { per + 1 } else { per } {
                r_i += 1;
                count = 0;
                x = 0;
            }
            *cell = (x, x + w, r_i);
            x = if buttons { x + w + 1 } else { x + w };
            count += 1;
        }
        let mut start = 0;
        while start < n {
            let mut end = start;
            while end < n && cells[end].2 == cells[start].2 {
                end += 1;
            }
            let k = (end - start) as i32;
            if k > 1 {
                let spare = client.r - 2 * SELECTED - cells[end - 1].1;
                let (each, rest) = (spare / k, spare % k);
                for (j, cell) in cells[start..end].iter_mut().enumerate() {
                    cell.0 += j as i32 * each;
                    cell.1 += (j as i32 + 1) * each;
                }
                cells[end - 1].1 += rest;
            } else {
                cells[start].0 = client.l;
                cells[start].1 = client.r - 4;
            }
            start = end;
        }
    }
    // the selected row goes next to the display area
    if rows > 1 && !buttons && sel >= 0 && (sel as usize) < n {
        let srow = cells[sel as usize].2;
        if srow != rows - 1 {
            for cell in cells.iter_mut() {
                if cell.2 == srow {
                    cell.2 = rows - 1;
                } else if cell.2 > srow {
                    cell.2 -= 1;
                }
            }
        }
    }
    let items = cells
        .iter()
        .map(|&(l, r, row)| {
            let (t, b) = if style & TCS_BOTTOM != 0 {
                let b = client.b - row * tab_h - if buttons { row * 3 } else { SELECTED };
                (b - tab_h, b)
            } else {
                let t = row * tab_h + if buttons { row * 3 } else { SELECTED };
                (t, t + tab_h)
            };
            Rect { l: l + SELECTED, t, r: r + SELECTED, b }
        })
        .collect();
    Layout { items, tab_h, rows, default_min }
}

fn notify(c: &mut Ctx, h: u32, code: i32) -> u32 {
    let (parent, id) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.parent, w.id)));
    if parent == 0 {
        return 0;
    }
    // NMHDR: hwndFrom, idFrom, code
    let p = mem::heap_alloc(24, true);
    let ps = if mem::x64() { 8 } else { 4 };
    mem::wp(p, h);
    mem::wp(p + ps, id);
    w32(p + 2 * ps, code as u32);
    let r = send(c, parent, 0x4e, id, p);
    mem::heap_free(p);
    r
}

const TCN_SELCHANGE: i32 = -551;
const TCN_SELCHANGING: i32 = -552;
const NM_CLICK: i32 = -2;
const NM_RCLICK: i32 = -5;

/// TCM_SETCURSEL: no notification; the previous selection, -1 past the items
fn set_cur_sel(h: u32, i: i32) -> u32 {
    let r = tab(h, |t| {
        if i >= t.items.len() as i32 {
            return None;
        }
        let prev = t.sel;
        if prev != i {
            let i = i.max(-1);
            t.sel = i;
            t.focus = i;
        }
        Some(prev)
    });
    match r {
        Some(prev) => {
            invalidate(h, None, true);
            prev as u32
        }
        None => -1i32 as u32,
    }
}

/// TCM_SETCURFOCUS (and the arrow keys): without TCS_BUTTONS the focused tab is selected, the
/// parent told (a refusal of TCN_SELCHANGING still selects it, as Windows does)
fn set_cur_focus(c: &mut Ctx, h: u32, i: i32) {
    let style = with(|s| s.wins.get(&h).map_or(0, |w| w.style));
    let n = tab(h, |t| t.items.len() as i32);
    if i < 0 {
        if style & TCS_BUTTONS == 0 {
            let had = tab(h, |t| {
                t.focus = -1;
                std::mem::replace(&mut t.sel, -1) != -1
            });
            if had {
                notify(c, h, TCN_SELCHANGE);
                invalidate(h, None, true);
            }
        }
    } else if i < n {
        if style & TCS_BUTTONS != 0 {
            let changed = tab(h, |t| std::mem::replace(&mut t.focus, i) != i);
            if changed {
                invalidate(h, None, true);
                notify(c, h, -554); // TCN_FOCUSCHANGE
            }
        } else {
            let changed = tab(h, |t| {
                t.focus = i;
                t.sel != i
            });
            if changed {
                let refused = notify(c, h, TCN_SELCHANGING) != 0;
                tab(h, |t| t.sel = i);
                if !refused {
                    notify(c, h, TCN_SELCHANGE);
                }
                invalidate(h, None, true);
            }
        }
    }
}

/// TCITEM's pszText, cchTextMax, iImage and lParam offsets
fn item_offsets() -> (u32, u32, u32, u32) {
    if mem::x64() { (16, 24, 28, 32) } else { (12, 16, 20, 24) }
}

fn read_item(lp: u32, wide: bool, item: &mut Item, extra: usize, set: bool) {
    let (o_text, _, o_image, o_param) = item_offsets();
    let mask = r32(lp);
    if mask & 1 != 0 {
        let p = mem::rp(lp + o_text);
        // (LPSTR_TEXTCALLBACK, -1, is kept as no text)
        item.text = if p == 0 || p == u32::MAX && !mem::x64() {
            if set { Some(Vec::new()) } else { None }
        } else if wide {
            Some(wstr(p))
        } else {
            Some(a2w(&cstr(p)))
        };
    }
    if mask & 2 != 0 {
        item.image = r32(lp + o_image) as i32;
    }
    if mask & 8 != 0 {
        item.extra = (0..extra as u32).map(|i| mem::r8(lp + o_param + i)).collect();
    }
    if set && mask & 0x10 != 0 {
        let (state, smask) = (r32(lp + 4), r32(lp + 8));
        item.state = item.state & !smask | state & smask;
    }
}

fn paint(c: &mut Ctx, h: u32, given_dc: u32) {
    let (style, w, hh) = with(|s| s.wins.get(&h).map_or((0, 0, 0), |w| (w.style, w.client.w(), w.client.h())));
    let lay = layout(c, h);
    let (font, sel, texts, focus) = tab(h, |t| (t.font, t.sel, t.items.iter().map(|i| i.text.clone().unwrap_or_default()).collect::<Vec<_>>(), t.focus));
    let ps = mem::heap_alloc(64 + 16, true);
    let rc = ps + 64;
    let dc = if given_dc != 0 { given_dc } else { gcall(c, "user32", "BeginPaint", &[h, ps]) };
    let f = if font != 0 { font } else { gcall(c, "gdi32", "GetStockObject", &[13]) };
    let old = gcall(c, "gdi32", "SelectObject", &[dc, f]);
    let old_mode = gcall(c, "gdi32", "SetBkMode", &[dc, 1]);
    let old_color = gcall(c, "gdi32", "SetTextColor", &[dc, sys_color(18)]);
    let face = |r: Rect| crate::gdi::fill_rect_brush(dc, r, crate::gdi::solid_brush(sys_color(15)));
    let buttons = style & TCS_BUTTONS != 0;
    let draw_item = |c: &mut Ctx, i: usize| {
        let r0 = lay.items[i];
        if r0.l >= w || r0.r <= 0 {
            return;
        }
        let text;
        if buttons {
            let mut r = r0;
            draw_edge(dc, &mut r, if i as i32 == sel { 0xa } else { 5 }, 0x100f);
            text = Rect { l: r0.l + 2, t: r0.t + 2, r: r0.r - 2, b: r0.b - 2 };
        } else {
            let selected = i as i32 == sel;
            // a selected tab is 2 pixels wider on each side and 2 higher, and covers the
            // display area's top edge; the bottom row's tabs reach down to it
            let mut r = if selected { Rect { l: r0.l - SELECTED, t: r0.t - SELECTED, r: r0.r + SELECTED, b: r0.b - 1 } } else { r0 };
            let mut fill = r;
            if selected {
                if r.l == 0 {
                    fill.l += BORDER;
                }
                if r.r == w {
                    fill.r -= BORDER;
                }
            }
            if r0.b == lay.items.iter().map(|x| x.b).max().unwrap_or(0) && style & TCS_BOTTOM == 0 {
                fill.b += 3;
                r.b += 2;
            }
            face(fill);
            draw_edge(dc, &mut r, 5, 0x1000 | 1 | 2 | 4);
            // the rounded corners: the face colour over the outer corner pixels
            face(Rect { l: r.l, t: r.t, r: r.l + 1, b: r.t + 1 });
            face(Rect { l: r.r - 1, t: r.t, r: r.r, b: r.t + 1 });
            text = if selected { Rect { b: r.b + 1, ..r } } else { Rect { l: r.l + 2, t: r.t, r: r.r - 2, b: r.b - 2 } };
        }
        let s = &texts[i];
        if !s.is_empty() {
            text.write(rc);
            crate::gdi::draw_text(dc, s, rc, 0x1 | 0x4 | 0x20);
        }
        let _ = (c, focus);
    };
    if !buttons {
        for i in 0..lay.items.len() {
            if i as i32 != sel {
                draw_item(c, i);
            }
        }
        // the display area's raised border, then the selected tab over its top
        let top = if lay.items.is_empty() { 0 } else { lay.tab_h * lay.rows + BORDER };
        let mut b = if style & TCS_BOTTOM != 0 { Rect { l: 0, t: 0, r: w, b: hh - top } } else { Rect { l: 0, t: top, r: w, b: hh } };
        draw_edge(dc, &mut b, 5, 0x1000 | 0xf);
        if sel >= 0 && (sel as usize) < lay.items.len() {
            draw_item(c, sel as usize);
        }
    } else {
        for i in 0..lay.items.len() {
            draw_item(c, i);
        }
    }
    gcall(c, "gdi32", "SetTextColor", &[dc, old_color]);
    gcall(c, "gdi32", "SetBkMode", &[dc, old_mode]);
    gcall(c, "gdi32", "SelectObject", &[dc, old]);
    if given_dc == 0 {
        gcall(c, "user32", "EndPaint", &[h, ps]);
    }
    mem::heap_free(ps);
}

fn hit_test(c: &mut Ctx, h: u32, x: i32, y: i32) -> i32 {
    let lay = layout(c, h);
    lay.items.iter().position(|r| x >= r.l && x < r.r && y >= r.t && y < r.b).map_or(-1, |i| i as i32)
}

pub(super) fn proc_fn(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let wide = msg_wide();
    let style = with(|s| s.wins.get(&h).map_or(0, |w| w.style));
    let n = tab(h, |t| t.items.len() as u32);
    // TCM_SETITEM, TCM_INSERTITEM, TCM_DELETEITEM, TCM_DELETEALLITEMS lay the tabs out again
    if matches!(m, 0x1306 | 0x133d | 0x1307 | 0x133e | 0x1308 | 0x1309) {
        tab(h, |t| t.dirty = true);
    }
    match m {
        // TCM_GETIMAGELIST / TCM_SETIMAGELIST
        0x1302 => tab(h, |t| t.images),
        0x1303 => {
            let old = tab(h, |t| std::mem::replace(&mut t.images, lp));
            invalidate(h, None, true);
            old
        }
        0x1304 => n,
        // TCM_GETITEMA/W
        0x1305 | 0x133c => {
            if wp >= n || lp == 0 {
                return 0;
            }
            let (o_text, o_max, o_image, o_param) = item_offsets();
            let mask = r32(lp);
            let (text, extra, state, image) = tab(h, |t| {
                let i = &t.items[wp as usize];
                (i.text.clone().unwrap_or_default(), i.extra.clone(), i.state, i.image)
            });
            if mask & 8 != 0 {
                mem::write(lp + o_param, &extra);
            }
            if mask & 2 != 0 {
                w32(lp + o_image, image as u32);
            }
            if mask & 0x10 != 0 {
                w32(lp + 4, state & r32(lp + 8));
            }
            if mask & 1 != 0 {
                let (p, max) = (mem::rp(lp + o_text), r32(lp + o_max));
                if m == 0x133c { mem::put_wstr(p, max, &text) } else { mem::put_cstr(p, max, &w2a(&text)) };
            }
            1
        }
        // TCM_SETITEMA/W
        0x1306 | 0x133d => {
            if wp >= n || lp == 0 {
                return 0;
            }
            tab(h, |t| {
                let extra = t.extra;
                read_item(lp, m == 0x133d, &mut t.items[wp as usize], extra, true)
            });
            invalidate(h, None, true);
            1
        }
        // TCM_INSERTITEMA/W: at min(index, count); the first one is selected
        0x1307 | 0x133e => {
            if (wp as i32) < 0 || lp == 0 {
                return -1i32 as u32;
            }
            let i = wp.min(n);
            let focus = tab(h, |t| {
                let mut it = Item { text: None, extra: vec![0; t.extra], state: 0, image: -1 };
                read_item(lp, m == 0x133e, &mut it, t.extra, false);
                if r32(lp) & 2 == 0 {
                    it.image = -1;
                }
                t.items.insert(i as usize, it);
                if t.items.len() == 1 {
                    t.sel = 0;
                } else if i as i32 <= t.sel {
                    t.sel += 1;
                }
                t.focus
            });
            invalidate(h, None, true);
            if focus == -1 {
                set_cur_focus(c, h, i as i32);
            }
            i
        }
        // TCM_DELETEITEM: a deleted selection leaves none
        0x1308 => {
            if wp >= n {
                return 0;
            }
            tab(h, |t| {
                t.items.remove(wp as usize);
                let i = wp as i32;
                if t.items.is_empty() || i == t.sel {
                    t.sel = -1;
                } else if i < t.sel {
                    t.sel -= 1;
                }
            });
            invalidate(h, None, true);
            1
        }
        0x1309 => {
            tab(h, |t| {
                t.items.clear();
                t.sel = -1;
            });
            invalidate(h, None, true);
            1
        }
        // TCM_GETITEMRECT: TRUE for any item, also one past the control's right; a missing one
        // is (0, 0, 0, tab height), FALSE
        0x130a => {
            let lay = layout(c, h);
            match lay.items.get(wp as usize) {
                Some(r) => {
                    r.write(lp);
                    1
                }
                None => {
                    Rect { l: 0, t: 0, r: 0, b: lay.tab_h }.write(lp);
                    0
                }
            }
        }
        0x130b => tab(h, |t| t.sel as u32),
        0x130c => set_cur_sel(h, wp as i32),
        // TCM_HITTEST
        0x130d => {
            if lp == 0 {
                return -1i32 as u32;
            }
            let (x, y) = (r32(lp) as i32, r32(lp + 4) as i32);
            let i = hit_test(c, h, x, y);
            // TCHT_ONITEMLABEL between the horizontal paddings (the padding's own last pixel is
            // still the item; the whole height counts), TCHT_ONITEM on them, TCHT_NOWHERE
            // (winref tabctl)
            let flags = if i < 0 {
                1
            } else {
                let r = layout(c, h).items.swap_remove(i as usize);
                let (px, _) = tab(h, |t| t.pad);
                if x > r.l + px && x < r.r - px && y >= r.t && y < r.b { 4 } else { 6 }
            };
            w32(lp + 8, flags);
            i as u32
        }
        // TCM_SETITEMEXTRA: only while there are no items
        0x130e => {
            if n != 0 || (wp as i32) < 0 {
                return 0;
            }
            tab(h, |t| t.extra = (wp as usize).max(4));
            1
        }
        // TCM_ADJUSTRECT: display area <-> window rectangle
        0x1328 => {
            if lp == 0 {
                return -1i32 as u32;
            }
            let lay = layout(c, h);
            let mut r = Rect::read(lp);
            let tabs = lay.tab_h * lay.rows + if style & TCS_BUTTONS != 0 { 3 * (lay.rows - 1).max(0) } else { 0 };
            let d = 2 * BORDER;
            if wp != 0 {
                if style & TCS_BOTTOM != 0 { r.b += lay.tab_h * lay.rows } else { r.t -= tabs }
                r = Rect { l: r.l - d, t: r.t - d, r: r.r + d, b: r.b + d };
            } else {
                r = Rect { l: r.l + d, t: r.t + d, r: r.r - d, b: r.b - d };
                if style & TCS_BOTTOM != 0 { r.b -= lay.tab_h * lay.rows } else { r.t += tabs }
            }
            r.write(lp);
            0
        }
        // TCM_SETITEMSIZE: the old width and set height (0 while it comes from the font)
        0x1329 => {
            let (cx, cy) = ((lp & 0xffff) as i32, (lp >> 16) as i32);
            let old = tab(h, |t| {
                let old = (t.width as u32 & 0xffff) | (t.height as u32 & 0xffff) << 16;
                t.width = cx;
                t.height = cy;
                t.dirty = true;
                old
            });
            invalidate(h, None, true);
            old
        }
        0x132a => 0,
        // TCM_SETPADDING
        0x132b => {
            tab(h, |t| t.pad_set = ((lp & 0xffff) as i32, (lp >> 16) as i32));
            0
        }
        0x132c => layout(c, h).rows as u32,
        0x132d => tab(h, |t| t.tooltips),
        0x132e => {
            tab(h, |t| t.tooltips = wp);
            0
        }
        0x132f => tab(h, |t| t.focus as u32),
        0x1330 => {
            set_cur_focus(c, h, wp as i32);
            0
        }
        // TCM_SETMINTABWIDTH: the previous minimum, or the default one
        0x1331 => {
            let lay = layout(c, h);
            let prev = tab(h, |t| {
                t.dirty = true;
                std::mem::replace(&mut t.min_width, lp as i32)
            });
            invalidate(h, None, true);
            if prev < 0 { lay.default_min as u32 } else { prev as u32 }
        }
        // TCM_DESELECTALL (buttons only)
        0x1332 => {
            if style & TCS_BUTTONS != 0 {
                tab(h, |t| {
                    for (i, it) in t.items.iter_mut().enumerate() {
                        if wp == 0 || i as i32 != t.sel {
                            it.state &= !1;
                        }
                    }
                });
                invalidate(h, None, true);
            }
            0
        }
        // TCM_HIGHLIGHTITEM
        0x1333 => {
            let ok = tab(h, |t| match t.items.get_mut(wp as usize) {
                Some(it) => {
                    if lp & 0xffff != 0 { it.state |= 2 } else { it.state &= !2 }
                    true
                }
                None => false,
            });
            invalidate(h, None, true);
            ok as u32
        }
        // TCM_SETEXTENDEDSTYLE / TCM_GETEXTENDEDSTYLE
        0x1334 => tab(h, |t| {
            let old = t.ex_style;
            let mask = if wp == 0 { u32::MAX } else { wp };
            t.ex_style = t.ex_style & !mask | lp & mask;
            old
        }),
        0x1335 => tab(h, |t| t.ex_style),
        0xf => {
            paint(c, h, 0);
            0
        }
        // WM_PRINTCLIENT
        0x318 => {
            paint(c, h, wp);
            0
        }
        0x30 => {
            tab(h, |t| {
                t.font = wp;
                t.dirty = true;
            });
            if lp & 0xffff != 0 {
                invalidate(h, None, true);
            }
            0
        }
        0x31 => tab(h, |t| t.font),
        0x5 => {
            tab(h, |t| t.dirty = true);
            invalidate(h, None, true);
            def_proc(c, wide)
        }
        // WM_GETDLGCODE: DLGC_WANTARROWS | DLGC_WANTCHARS
        0x87 => 0x81,
        // WM_KEYDOWN: left/up and right/down move the focus
        0x100 => {
            let f = tab(h, |t| t.focus);
            let next = match wp {
                0x25 | 0x26 => f - 1,
                0x27 | 0x28 => f + 1,
                _ => return 0,
            };
            if next >= 0 && next < n as i32 {
                set_cur_focus(c, h, next);
            }
            0
        }
        0x201 | 0x203 => {
            if style & TCS_FOCUSNEVER == 0 {
                set_focus(c, h);
            }
            let (x, y) = ((lp & 0xffff) as i16 as i32, (lp >> 16) as i16 as i32);
            let i = hit_test(c, h, x, y);
            if i != -1 && i != tab(h, |t| t.sel) {
                if notify(c, h, TCN_SELCHANGING) != 0 {
                    return 0;
                }
                set_cur_sel(h, i);
                notify(c, h, TCN_SELCHANGE);
            }
            0
        }
        0x202 => {
            notify(c, h, NM_CLICK);
            0
        }
        0x205 => {
            notify(c, h, NM_RCLICK);
            0
        }
        0x7 | 0x8 => {
            invalidate(h, None, true);
            def_proc(c, wide)
        }
        0x2 => {
            TABS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
            def_proc(c, wide)
        }
        _ => def_proc(c, wide),
    }
}
