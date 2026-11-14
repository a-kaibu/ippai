//! The list box (ListBox) as user32 has it: items with text (LBS_HASSTRINGS, or any list box
//! not owner-drawn) and item data, kept sorted with LBS_SORT; one selection, or several with
//! LBS_MULTIPLESEL / LBS_EXTENDEDSEL (the caret and anchor apart); fixed or per-item heights
//! (owner-drawn lists ask the owner with WM_MEASUREITEM, draw with WM_DRAWITEM and hear of
//! removed items with WM_DELETEITEM); the top index scrolled with WM_VSCROLL, the wheel and the
//! keys; clicks and keys select and, with LBS_NOTIFY, tell the parent LBN_SELCHANGE /
//! LBN_DBLCLK / LBN_SETFOCUS / LBN_KILLFOCUS (changes made by messages tell it nothing).
//! Not done: multiple columns (laid out as one), LBS_NODATA, LB_DIR, drawing a scroll bar.

use super::*;

const LB_ERR: u32 = u32::MAX;

const LBS_NOTIFY: u32 = 0x1;
const LBS_SORT: u32 = 0x2;
const LBS_MULTIPLESEL: u32 = 0x8;
const LBS_OWNERDRAWFIXED: u32 = 0x10;
const LBS_OWNERDRAWVARIABLE: u32 = 0x20;
const LBS_HASSTRINGS: u32 = 0x40;
const LBS_EXTENDEDSEL: u32 = 0x800;
const LBS_NOSEL: u32 = 0x4000;

#[derive(Clone, Default)]
struct Item {
    text: Vec<u16>,
    data: u32,
    sel: bool,
    /// LBS_OWNERDRAWVARIABLE: its own height
    height: i32,
}

struct List {
    items: Vec<Item>,
    /// the selection of a single-selection list
    sel: i32,
    caret: i32,
    anchor: i32,
    top: usize,
    item_h: i32,
    font: u32,
    redraw: bool,
    locale: u32,
    measured: bool,
    /// a button went down on an item (LBN_SELCHANGE when it comes up or focus goes)
    captured: bool,
}

impl Default for List {
    fn default() -> List {
        List { items: Vec::new(), sel: -1, caret: -1, anchor: -1, top: 0, item_h: 0, font: 0, redraw: true, locale: 0x411, measured: false, captured: false }
    }
}

static LISTS: Mutex<Option<HashMap<u32, List>>> = Mutex::new(None);

fn list<R>(h: u32, f: impl FnOnce(&mut List) -> R) -> R {
    f(LISTS.lock().unwrap().get_or_insert_with(HashMap::default).entry(h).or_default())
}

fn style(h: u32) -> u32 {
    with(|s| s.wins.get(&h).map_or(0, |w| w.style))
}
fn owner_drawn(st: u32) -> bool {
    st & (LBS_OWNERDRAWFIXED | LBS_OWNERDRAWVARIABLE) != 0
}
fn has_strings(st: u32) -> bool {
    !owner_drawn(st) || st & LBS_HASSTRINGS != 0
}
fn multi(st: u32) -> bool {
    st & (LBS_MULTIPLESEL | LBS_EXTENDEDSEL) != 0
}
fn id_parent(h: u32) -> (u32, u32) {
    with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.id, w.parent)))
}
fn client(h: u32) -> (i32, i32) {
    with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.client.w(), w.client.h())))
}

/// LBN_* to the parent when the list has LBS_NOTIFY (LBN_SELCHANGE and LBN_DBLCLK; the focus
/// notifications go always)
fn notify(c: &mut Ctx, h: u32, code: u32) {
    if matches!(code, 1 | 2) && style(h) & LBS_NOTIFY == 0 {
        return;
    }
    let (id, parent) = id_parent(h);
    if parent != 0 {
        send(c, parent, 0x111, code << 16 | (id & 0xffff), h);
    }
}

/// The font's tmHeight (the system font's without WM_SETFONT)
fn font_height(c: &mut Ctx, h: u32, font: u32) -> i32 {
    let dc = gcall(c, "user32", "GetDC", &[h]);
    let f = if font != 0 { font } else { gcall(c, "gdi32", "GetStockObject", &[13]) };
    let old = gcall(c, "gdi32", "SelectObject", &[dc, f]);
    let tm = mem::heap_alloc(64, true);
    let ok = gcall(c, "gdi32", "GetTextMetricsW", &[dc, tm]) != 0;
    let th = text_height(ok, r32(tm) as i32);
    mem::heap_free(tm);
    gcall(c, "gdi32", "SelectObject", &[dc, old]);
    gcall(c, "user32", "ReleaseDC", &[h, dc]);
    th
}

/// An item's height from the font's text metrics: with no font file to measure (no C:\Windows\
/// Fonts, as host/tests on sake-grow run) GetTextMetrics fails, and a height of 1 put a click
/// at an item's top + 1 on the next item; then the 8-point dialog font's 13 pixels
fn text_height(ok: bool, height: i32) -> i32 {
    if ok && height > 0 {
        height
    } else {
        13
    }
}

/// WM_MEASUREITEM to the owner: MEASUREITEMSTRUCT { CtlType ODT_LISTBOX, CtlID, itemID,
/// itemWidth, itemHeight, itemData }; the height it gives
fn measure_item(c: &mut Ctx, h: u32, index: i32, data: u32, height: i32) -> i32 {
    let (id, parent) = id_parent(h);
    if parent == 0 {
        return height;
    }
    let ps = mem::ps();
    let m = mem::heap_alloc(8 + 4 * 4 + ps, true);
    w32(m, 2);
    w32(m + 4, id);
    w32(m + 8, index as u32);
    w32(m + 12, client(h).0 as u32);
    w32(m + 16, height as u32);
    mem::wp(m + if ps == 8 { 24 } else { 20 }, data);
    send(c, parent, 0x2c, id, m);
    let r = r32(m + 16) as i32;
    mem::heap_free(m);
    r
}

/// The item heights from the font (and the owner for an owner-drawn list), once
fn ensure_measured(c: &mut Ctx, h: u32) {
    if list(h, |l| l.measured) {
        return;
    }
    let st = style(h);
    let font = list(h, |l| l.font);
    let mut ih = font_height(c, h, font);
    if st & LBS_OWNERDRAWFIXED != 0 {
        ih = measure_item(c, h, 0, 0, ih);
    }
    list(h, |l| {
        l.item_h = ih.max(1);
        l.measured = true;
    });
}

fn height_of(l: &List, st: u32, i: usize) -> i32 {
    if st & LBS_OWNERDRAWVARIABLE != 0 {
        l.items.get(i).map_or(l.item_h, |it| it.height)
    } else {
        l.item_h
    }
}

/// An item's rectangle in client coordinates (from the top index; items above it negative)
fn item_rect(l: &List, st: u32, w: i32, i: usize) -> Rect {
    let mut y = 0;
    if i >= l.top {
        for k in l.top..i {
            y += height_of(l, st, k);
        }
    } else {
        for k in i..l.top {
            y -= height_of(l, st, k);
        }
    }
    Rect { l: 0, t: y, r: w, b: y + height_of(l, st, i) }
}

/// The item at client height y, and whether it is really there (else the nearest one)
fn item_at(l: &List, st: u32, y: i32) -> (i32, bool) {
    if l.items.is_empty() {
        return (0, false);
    }
    if y < 0 {
        return (l.top as i32, false);
    }
    let mut t = 0;
    for i in l.top..l.items.len() {
        let hh = height_of(l, st, i);
        if y < t + hh {
            return (i as i32, true);
        }
        t += hh;
    }
    (l.items.len() as i32 - 1, false)
}

/// How many whole items fit from the top index
fn page(l: &List, st: u32, ch: i32) -> usize {
    let mut t = 0;
    let mut n = 0;
    for i in l.top..l.items.len() {
        t += height_of(l, st, i);
        if t > ch {
            break;
        }
        n += 1;
    }
    n.max(1)
}

/// The top index that shows item i
fn scroll_to(l: &mut List, st: u32, ch: i32, i: usize) {
    if i < l.top {
        l.top = i;
        return;
    }
    // (the heights from top to i summed once, then the top's dropped one at a time: not a
    // new sum per step, quadratic in the distance)
    let mut t: i64 = (l.top..=i).map(|k| height_of(l, st, k) as i64).sum();
    while l.top < i && t > ch as i64 {
        t -= height_of(l, st, l.top) as i64;
        l.top += 1;
    }
}

/// The button up (or the focus gone) after a click on an item: the capture released and
/// LBN_SELCHANGE (again, though the click changed nothing)
fn button_up(c: &mut Ctx, h: u32) {
    if !list(h, |l| std::mem::take(&mut l.captured)) {
        return;
    }
    if gcall(c, "user32", "GetCapture", &[]) == h {
        gcall(c, "user32", "ReleaseCapture", &[]);
    }
    if list(h, |l| !l.items.is_empty()) {
        notify(c, h, 1);
    }
}

fn changed(h: u32) {
    if list(h, |l| l.redraw) {
        invalidate(h, None, true);
    }
}

/// Why an item goes, for WM_DELETEITEM: Windows (tests/winref host_listbox) tells the parent
/// of every LB_DELETESTRING, even of a plain item without data; LB_RESETCONTENT of a list that
/// isn't owner-drawn tells nothing, even of an item with data; destroying the list tells of
/// owner-drawn items and items with data
#[derive(Clone, Copy, PartialEq, Debug)]
enum Gone {
    Deleted,
    Reset,
    Destroyed,
}

/// Whether the parent hears WM_DELETEITEM for an item that goes this way
fn tells_delete(gone: Gone, owner_drawn: bool, data: u32) -> bool {
    match gone {
        Gone::Deleted => true,
        Gone::Reset => owner_drawn,
        Gone::Destroyed => owner_drawn || data != 0,
    }
}

/// WM_DELETEITEM (see tells_delete): DELETEITEMSTRUCT { CtlType, CtlID, itemID, hwndItem,
/// itemData }
fn delete_item(c: &mut Ctx, h: u32, index: usize, data: u32, gone: Gone) {
    if !tells_delete(gone, owner_drawn(style(h)), data) {
        return;
    }
    let (id, parent) = id_parent(h);
    if parent == 0 {
        return;
    }
    let ps = mem::ps();
    let d = mem::heap_alloc(12 + 4 + 2 * ps, true);
    w32(d, 2);
    w32(d + 4, id);
    w32(d + 8, index as u32);
    let at = if ps == 8 { 16 } else { 12 };
    mem::wp(d + at, h);
    mem::wp(d + at + ps, data);
    send(c, parent, 0x2d, id, d);
    mem::heap_free(d);
}

fn text_arg(st: u32, lp: u32, wide: bool) -> (Vec<u16>, u32) {
    if has_strings(st) {
        let t = if lp == 0 { Vec::new() } else { crate::k32::arg_str(lp, wide).encode_utf16().collect() };
        (t, 0)
    } else {
        (Vec::new(), lp)
    }
}

/// Case-insensitive search from after `from` (-1: from the start) round the list: prefix or
/// whole text; a list without strings compares the item data
fn find(l: &List, st: u32, from: u32, key: &[u16], data: u32, exact: bool) -> u32 {
    let n = l.items.len();
    if n == 0 {
        return LB_ERR;
    }
    let start = if (from as i32) >= 0 && (from as usize) < n { from as usize + 1 } else { 0 };
    // (the key folded once; each item's text folded as it is compared, no copy per item)
    let fold = |v: &[u16]| char::decode_utf16(v.iter().copied()).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).flat_map(char::to_lowercase).collect::<Vec<char>>();
    let want = fold(key);
    (0..n)
        .map(|k| (start + k) % n)
        .find(|&i| {
            if !has_strings(st) {
                return l.items[i].data == data;
            }
            let mut t = char::decode_utf16(l.items[i].text.iter().copied()).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).flat_map(char::to_lowercase);
            let prefix = want.iter().all(|&w| t.next() == Some(w));
            prefix && (!exact || t.next().is_none())
        })
        .map_or(LB_ERR, |i| i as u32)
}

/// Where a new item goes: LBS_SORT keeps the texts in order (case ignored)
fn insert_at(l: &List, st: u32, at: Option<usize>, t: &[u16]) -> usize {
    if st & LBS_SORT != 0 && has_strings(st) {
        // (the texts are in order: a binary search for the first one after `t`)
        return l.items.partition_point(|it| crate::collate::compare(t, &it.text, crate::collate::NORM_IGNORECASE) != std::cmp::Ordering::Less);
    }
    match at {
        Some(i) if i <= l.items.len() => i,
        _ => l.items.len(),
    }
}

/// Selects by the user (a click or a key): the item becomes the caret (and, alone, the
/// selection); for a key LBN_SELCHANGE when the selection changed (a click tells once, when
/// the button comes up: Windows sends one LBN_SELCHANGE per click, changed or not)
fn user_select(c: &mut Ctx, h: u32, i: i32, shift: bool, ctrl: bool, toggle: bool, click: bool) {
    let st = style(h);
    if st & LBS_NOSEL != 0 {
        return;
    }
    let ch = client(h).1;
    let moved = list(h, |l| {
        if i < 0 || i as usize >= l.items.len() {
            return false;
        }
        let before: Vec<bool> = l.items.iter().map(|it| it.sel).collect();
        let old_sel = l.sel;
        if st & LBS_MULTIPLESEL != 0 {
            if toggle {
                let it = &mut l.items[i as usize];
                it.sel = !it.sel;
            }
        } else if st & LBS_EXTENDEDSEL != 0 {
            if shift && l.anchor >= 0 {
                let (a, b) = (l.anchor.min(i), l.anchor.max(i));
                for (k, it) in l.items.iter_mut().enumerate() {
                    if !ctrl {
                        it.sel = false;
                    }
                    if (a..=b).contains(&(k as i32)) {
                        it.sel = true;
                    }
                }
            } else if ctrl {
                let it = &mut l.items[i as usize];
                it.sel = !it.sel;
                l.anchor = i;
            } else {
                for (k, it) in l.items.iter_mut().enumerate() {
                    it.sel = k as i32 == i;
                }
                l.anchor = i;
            }
        } else {
            l.sel = i;
        }
        l.caret = i;
        scroll_to(l, st, ch, i as usize);
        old_sel != l.sel || before.iter().zip(&l.items).any(|(b, it)| *b != it.sel)
    });
    invalidate(h, None, true);
    if moved && !click {
        notify(c, h, 1);
    }
}

/// The items a client `ch` high shows from the top index: (index, height, item)
fn visible(l: &List, st: u32, ch: i32) -> Vec<(usize, i32, Item)> {
    let mut y = 0;
    let mut v = Vec::new();
    for i in l.top..l.items.len() {
        if y >= ch {
            break;
        }
        let ih = height_of(l, st, i);
        v.push((i, ih, l.items[i].clone()));
        y += ih;
    }
    v
}

fn paint(c: &mut Ctx, h: u32) {
    ensure_measured(c, h);
    let st = style(h);
    let (w, ch) = client(h);
    let enabled = with(|s| s.wins.get(&h).is_none_or(|w| w.enabled));
    let focused = with(|s| s.focus == h);
    // only the items that show, with their heights (not the whole list, twice, per paint)
    let (shown, sel, caret, font) = list(h, |l| (visible(l, st, ch), l.sel, l.caret, l.font));
    let ps = mem::heap_alloc(64 + 16, true);
    let rc = ps + 64;
    let dc = gcall(c, "user32", "BeginPaint", &[h, ps]);
    if font != 0 {
        gcall(c, "gdi32", "SelectObject", &[dc, font]);
    }
    let (id, parent) = id_parent(h);
    // WM_CTLCOLORLISTBOX: the parent's brush (and colours set on the DC)
    let mut bg = if parent != 0 { send(c, parent, 0x134, dc, h) } else { 0 };
    if bg == 0 {
        bg = crate::gdi::solid_brush(0xffffff);
    }
    let mut y = 0;
    for (i, ih, item) in shown {
        let r = Rect { l: 0, t: y, r: w, b: y + ih };
        let selected = if multi(st) { item.sel } else { sel == i as i32 };
        if owner_drawn(st) && parent != 0 {
            // DRAWITEMSTRUCT { CtlType, CtlID, itemID, itemAction ODA_DRAWENTIRE, itemState,
            // hwndItem, hDC, rcItem, itemData }
            let pz = mem::ps();
            let d = mem::heap_alloc(64, true);
            w32(d, 2);
            w32(d + 4, id);
            w32(d + 8, i as u32);
            w32(d + 12, 1);
            let state = if selected { 1 } else { 0 } | if !enabled { 4 } else { 0 } | if focused && caret == i as i32 { 0x10 } else { 0 };
            w32(d + 16, state);
            let at = if pz == 8 { 24 } else { 20 };
            mem::wp(d + at, h);
            mem::wp(d + at + pz, dc);
            r.write(d + at + 2 * pz);
            mem::wp(d + at + 2 * pz + 16, item.data);
            send(c, parent, 0x2b, id, d);
            mem::heap_free(d);
        } else {
            let (fill, fg) = if selected { (0xd77800, 0xffffff) } else { (0, if enabled { 0 } else { 0x6d6d6d }) };
            if selected {
                crate::gdi::fill_rect_brush(dc, r, crate::gdi::solid_brush(fill));
            } else {
                crate::gdi::fill_rect_brush(dc, r, bg);
            }
            gcall(c, "gdi32", "SetBkMode", &[dc, 1]);
            gcall(c, "gdi32", "SetTextColor", &[dc, fg]);
            Rect { l: 2, ..r }.write(rc);
            // DT_SINGLELINE | DT_NOPREFIX (DT_EXPANDTABS with LBS_USETABSTOPS)
            let flags = 0x20 | 0x800 | if st & 0x80 != 0 { 0x40 } else { 0 };
            crate::gdi::draw_text(dc, &item.text, rc, flags);
            if focused && caret == i as i32 {
                r.write(rc);
                gcall(c, "user32", "DrawFocusRect", &[dc, rc]);
            }
        }
        y += ih;
    }
    if y < ch {
        crate::gdi::fill_rect_brush(dc, Rect { l: 0, t: y, r: w, b: ch }, bg);
    }
    gcall(c, "user32", "EndPaint", &[h, ps]);
    mem::heap_free(ps);
}

pub(super) fn proc_fn(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let wide = msg_wide();
    let st = style(h);
    match m {
        // WM_CREATE: the item height (an owner-drawn fixed list asks its owner)
        0x1 => {
            ensure_measured(c, h);
            0
        }
        // LB_ADDSTRING, LB_INSERTSTRING, LB_ADDFILE
        0x180 | 0x181 | 0x196 => {
            ensure_measured(c, h);
            let (text, data) = text_arg(st, lp, wide);
            let at = if m == 0x181 {
                if wp as i32 == -1 {
                    None
                } else if wp as usize > list(h, |l| l.items.len()) {
                    return LB_ERR;
                } else {
                    Some(wp as usize)
                }
            } else {
                None
            };
            let i = list(h, |l| {
                // (LB_INSERTSTRING does not sort)
                let i = if m == 0x181 { at.unwrap_or(l.items.len()) } else { insert_at(l, st, at, &text) };
                let item_h = l.item_h;
                l.items.insert(i, Item { text, data, sel: false, height: item_h });
                for v in [&mut l.sel, &mut l.caret, &mut l.anchor] {
                    if *v >= i as i32 {
                        *v += 1;
                    }
                }
                i
            });
            if st & LBS_OWNERDRAWVARIABLE != 0 {
                let ih = list(h, |l| l.item_h);
                let hh = measure_item(c, h, i as i32, data, ih);
                // (the parent may have deleted or reset items while measuring)
                list(h, |l| {
                    if let Some(it) = l.items.get_mut(i) {
                        it.height = hh;
                    }
                });
            }
            changed(h);
            i as u32
        }
        // LB_DELETESTRING
        0x182 => {
            let Some(item) = list(h, |l| {
                if wp as usize >= l.items.len() {
                    return None;
                }
                let it = l.items.remove(wp as usize);
                let i = wp as i32;
                for v in [&mut l.sel, &mut l.caret, &mut l.anchor] {
                    if *v == i {
                        *v = -1;
                    } else if *v > i {
                        *v -= 1;
                    }
                }
                l.top = l.top.min(l.items.len().saturating_sub(1));
                Some(it)
            }) else {
                return LB_ERR;
            };
            delete_item(c, h, wp as usize, item.data, Gone::Deleted);
            changed(h);
            list(h, |l| l.items.len() as u32)
        }
        // LB_RESETCONTENT
        0x184 => {
            let items = list(h, |l| {
                let v = std::mem::take(&mut l.items);
                l.sel = -1;
                l.caret = -1;
                l.anchor = -1;
                l.top = 0;
                v
            });
            // (the last first)
            for (i, it) in items.iter().enumerate().rev() {
                delete_item(c, h, i, it.data, Gone::Reset);
            }
            changed(h);
            0
        }
        // LB_SETSEL(select, index or -1): a multiple-selection list's
        0x185 => {
            if !multi(st) {
                return LB_ERR;
            }
            let r = list(h, |l| {
                if lp as i32 == -1 {
                    for it in &mut l.items {
                        it.sel = wp != 0;
                    }
                    return 0;
                }
                let Some(it) = l.items.get_mut(lp as usize) else { return LB_ERR };
                it.sel = wp != 0;
                if wp != 0 {
                    l.caret = lp as i32;
                    l.anchor = lp as i32;
                }
                0
            });
            changed(h);
            r
        }
        // LB_SETCURSEL: a single-selection list's (-1 clears it, and answers LB_ERR)
        0x186 => {
            if multi(st) {
                return LB_ERR;
            }
            let ch = client(h).1;
            let r = list(h, |l| {
                // (-1 clears; past the end answers LB_ERR and keeps the selection, as Windows)
                if wp as i32 == -1 {
                    l.sel = -1;
                    return LB_ERR;
                }
                if wp as usize >= l.items.len() {
                    return LB_ERR;
                }
                l.sel = wp as i32;
                l.caret = wp as i32;
                scroll_to(l, st, ch, wp as usize);
                wp
            });
            changed(h);
            r
        }
        // LB_GETSEL
        0x187 => list(h, |l| match l.items.get(wp as usize) {
            Some(it) => (if multi(st) { it.sel } else { l.sel == wp as i32 }) as u32,
            None => LB_ERR,
        }),
        // LB_GETCURSEL: the selection, or a multiple-selection list's caret
        0x188 => list(h, |l| {
            let v = if multi(st) { l.caret } else { l.sel };
            if v < 0 { LB_ERR } else { v as u32 }
        }),
        // LB_GETTEXT, LB_GETTEXTLEN (a list without strings: its item data, 4 bytes)
        0x189 | 0x18a => {
            let Some(it) = list(h, |l| l.items.get(wp as usize).cloned()) else { return LB_ERR };
            if !has_strings(st) {
                if m == 0x189 && lp != 0 {
                    mem::wp(lp, it.data);
                }
                return mem::ps();
            }
            if wide {
                if m == 0x189 && lp != 0 {
                    mem::put_wstr(lp, it.text.len() as u32 + 1, &it.text);
                }
                it.text.len() as u32
            } else {
                let a = crate::text::s2a(&String::from_utf16_lossy(&it.text));
                if m == 0x189 && lp != 0 {
                    mem::put_cstr(lp, a.len() as u32 + 1, &a);
                }
                a.len() as u32
            }
        }
        0x18b => list(h, |l| l.items.len() as u32),
        // LB_SELECTSTRING: a single-selection list selects the item found
        0x18c => {
            let (key, data) = text_arg(st, lp, wide);
            let ch = client(h).1;
            let r = list(h, |l| {
                let i = find(l, st, wp, &key, data, false);
                if i != LB_ERR {
                    if multi(st) {
                        l.items[i as usize].sel = true;
                    } else {
                        l.sel = i as i32;
                    }
                    l.caret = i as i32;
                    scroll_to(l, st, ch, i as usize);
                }
                i
            });
            changed(h);
            r
        }
        // LB_DIR
        0x18d => LB_ERR,
        0x18e => list(h, |l| l.top as u32),
        // LB_FINDSTRING, LB_FINDSTRINGEXACT
        0x18f | 0x1a2 => {
            let (key, data) = text_arg(st, lp, wide);
            list(h, |l| find(l, st, wp, &key, data, m == 0x1a2))
        }
        // LB_GETSELCOUNT, LB_GETSELITEMS
        0x190 => {
            if !multi(st) {
                return LB_ERR;
            }
            list(h, |l| l.items.iter().filter(|it| it.sel).count() as u32)
        }
        0x191 => {
            if !multi(st) {
                return LB_ERR;
            }
            let sel: Vec<u32> = list(h, |l| l.items.iter().enumerate().filter(|(_, it)| it.sel).map(|(i, _)| i as u32).collect());
            let n = sel.len().min(wp as usize);
            for (k, i) in sel[..n].iter().enumerate() {
                w32(lp + 4 * k as u32, *i);
            }
            n as u32
        }
        // LB_SETTABSTOPS
        0x192 => 1,
        // LB_GETHORIZONTALEXTENT, LB_SETHORIZONTALEXTENT, LB_SETCOLUMNWIDTH
        0x193 | 0x194 | 0x195 => 0,
        // LB_SETTOPINDEX
        0x197 => {
            let r = list(h, |l| {
                if wp as usize >= l.items.len() && !(l.items.is_empty() && wp == 0) {
                    return LB_ERR;
                }
                l.top = wp as usize;
                0
            });
            changed(h);
            r
        }
        // LB_GETITEMRECT
        0x198 => {
            ensure_measured(c, h);
            let w = client(h).0;
            list(h, |l| {
                if (wp as usize) >= l.items.len() && !(l.items.is_empty() && wp == 0) {
                    return LB_ERR;
                }
                item_rect(l, st, w, wp as usize).write(lp);
                1
            })
        }
        // LB_GETITEMDATA, LB_SETITEMDATA (-1: every item)
        0x199 => list(h, |l| l.items.get(wp as usize).map_or(LB_ERR, |it| it.data)),
        0x19a => list(h, |l| {
            if wp as i32 == -1 {
                for it in &mut l.items {
                    it.data = lp;
                }
                return 1;
            }
            match l.items.get_mut(wp as usize) {
                Some(it) => {
                    it.data = lp;
                    1
                }
                None => LB_ERR,
            }
        }),
        // LB_SELITEMRANGE(select, MAKELONG(first, last)), LB_SELITEMRANGEEX(first, last)
        0x19b | 0x183 => {
            if !multi(st) {
                return LB_ERR;
            }
            let (on, a, b) = if m == 0x19b { (wp != 0, lp & 0xffff, lp >> 16) } else if wp <= lp { (true, wp, lp) } else { (false, lp, wp) };
            list(h, |l| {
                for (k, it) in l.items.iter_mut().enumerate() {
                    if (a..=b).contains(&(k as u32)) {
                        it.sel = on;
                    }
                }
            });
            changed(h);
            0
        }
        // LB_SETANCHORINDEX, LB_GETANCHORINDEX, LB_SETCARETINDEX, LB_GETCARETINDEX
        0x19c => list(h, |l| {
            if wp as usize >= l.items.len() {
                return LB_ERR;
            }
            l.anchor = wp as i32;
            0
        }),
        0x19d => list(h, |l| l.anchor as u32),
        0x19e => {
            let ch = client(h).1;
            let r = list(h, |l| {
                if wp as usize >= l.items.len() {
                    return LB_ERR;
                }
                l.caret = wp as i32;
                if !multi(st) {
                    l.sel = wp as i32;
                }
                scroll_to(l, st, ch, wp as usize);
                0
            });
            changed(h);
            r
        }
        0x19f => list(h, |l| if l.caret < 0 { 0 } else { l.caret as u32 }),
        // LB_SETITEMHEIGHT(index, height), LB_GETITEMHEIGHT(index)
        0x1a0 => {
            ensure_measured(c, h);
            let hh = (lp & 0xffff) as i32;
            if hh > 255 {
                return LB_ERR;
            }
            let r = list(h, |l| {
                if st & LBS_OWNERDRAWVARIABLE != 0 {
                    match l.items.get_mut(wp as usize) {
                        Some(it) => it.height = hh,
                        None => return LB_ERR,
                    }
                } else {
                    l.item_h = hh;
                }
                0
            });
            changed(h);
            r
        }
        0x1a1 => {
            ensure_measured(c, h);
            list(h, |l| {
                if st & LBS_OWNERDRAWVARIABLE != 0 {
                    l.items.get(wp as usize).map_or(LB_ERR, |it| it.height as u32)
                } else {
                    l.item_h as u32
                }
            })
        }
        // LB_SETLOCALE, LB_GETLOCALE
        0x1a5 => list(h, |l| std::mem::replace(&mut l.locale, wp)),
        0x1a6 => list(h, |l| l.locale),
        // LB_SETCOUNT (LBS_NODATA only), LB_INITSTORAGE (the items there are)
        0x1a7 => LB_ERR,
        0x1a8 => list(h, |l| l.items.len() as u32 + wp),
        // LB_ITEMFROMPOINT: the item, and 1 << 16 outside the items
        0x1a9 => {
            ensure_measured(c, h);
            let (x, y) = ((lp & 0xffff) as i16 as i32, (lp >> 16) as i16 as i32);
            let (w, _) = client(h);
            list(h, |l| {
                let (i, inside) = item_at(l, st, y);
                (i as u32 & 0xffff) | if inside && x >= 0 && x < w { 0 } else { 1 << 16 }
            })
        }
        // LB_GETLISTBOXINFO: the items in a column
        0x1b2 => {
            ensure_measured(c, h);
            let ch = client(h).1;
            list(h, |l| page(l, st, ch) as u32)
        }
        0xf => {
            paint(c, h);
            0
        }
        0x14 => 1,
        0x30 => {
            list(h, |l| {
                l.font = wp;
                l.measured = false;
            });
            ensure_measured(c, h);
            if lp != 0 {
                invalidate(h, None, true);
            }
            0
        }
        0x31 => list(h, |l| l.font),
        // WM_SETREDRAW
        0xb => {
            list(h, |l| l.redraw = wp != 0);
            if wp != 0 {
                invalidate(h, None, true);
            }
            0
        }
        // WM_GETDLGCODE: DLGC_WANTARROWS | DLGC_WANTCHARS
        0x87 => 0x81,
        0x7 => {
            invalidate(h, None, true);
            notify(c, h, 4);
            0
        }
        0x8 => {
            button_up(c, h);
            invalidate(h, None, true);
            notify(c, h, 5);
            0
        }
        // clicks: select the item there (MK_SHIFT 4, MK_CONTROL 8)
        // (WM_LBTRACKPOINT to the owner first with LBS_NOTIFY; the capture is held till the
        // button comes up)
        0x201 => {
            ensure_measured(c, h);
            set_focus(c, h);
            let y = (lp >> 16) as i16 as i32;
            let (i, inside) = list(h, |l| item_at(l, st, y));
            if inside {
                let (_, parent) = id_parent(h);
                if st & LBS_NOTIFY != 0 && parent != 0 {
                    send(c, parent, 0x131, i as u32, lp);
                }
                list(h, |l| l.captured = true);
                gcall(c, "user32", "SetCapture", &[h]);
                user_select(c, h, i, wp & 4 != 0, wp & 8 != 0, true, true);
            }
            0
        }
        0x202 => {
            button_up(c, h);
            0
        }
        // a double click only tells the parent (the first click selected)
        0x203 => {
            if list(h, |l| !l.items.is_empty()) {
                notify(c, h, 2);
            }
            0
        }
        // keys: up, down, page up / down, home, end move the caret (and the selection); space
        // toggles in a multiple-selection list
        0x100 => {
            ensure_measured(c, h);
            let ch = client(h).1;
            let (n, cur, pg) = list(h, |l| (l.items.len() as i32, if multi(st) { l.caret } else { l.sel }, page(l, st, ch) as i32));
            if n == 0 {
                return 0;
            }
            let shift = super::key_down(0x10);
            let ctrl = super::key_down(0x11);
            let to = match wp {
                0x26 => (cur - 1).max(0),
                0x28 => (cur + 1).min(n - 1),
                0x21 => (cur - pg + 1).max(0),
                0x22 => (cur + pg - 1).min(n - 1),
                0x24 => 0,
                0x23 => n - 1,
                0x20 if st & LBS_MULTIPLESEL != 0 => {
                    user_select(c, h, cur.max(0), false, false, true, false);
                    return 0;
                }
                _ => return 0,
            };
            if st & LBS_MULTIPLESEL != 0 {
                list(h, |l| {
                    l.caret = to;
                    scroll_to(l, st, ch, to as usize);
                });
                invalidate(h, None, true);
            } else {
                user_select(c, h, to, shift, ctrl, false, false);
            }
            0
        }
        // WM_VSCROLL: SB_LINEUP / LINEDOWN / PAGEUP / PAGEDOWN / THUMBPOSITION / TOP / BOTTOM
        0x115 => {
            ensure_measured(c, h);
            let ch = client(h).1;
            list(h, |l| {
                let n = l.items.len();
                let pg = page(l, st, ch);
                let last = n.saturating_sub(pg);
                l.top = match wp & 0xffff {
                    0 => l.top.saturating_sub(1),
                    1 => (l.top + 1).min(last),
                    2 => l.top.saturating_sub(pg),
                    3 => (l.top + pg).min(last),
                    4 | 5 => ((wp >> 16) as usize).min(last),
                    6 => 0,
                    7 => last,
                    _ => l.top,
                };
            });
            invalidate(h, None, true);
            0
        }
        // the wheel: three lines a notch
        0x20a => {
            ensure_measured(c, h);
            let ch = client(h).1;
            let d = (wp >> 16) as i16 as i32 / 120 * 3;
            list(h, |l| {
                let last = l.items.len().saturating_sub(page(l, st, ch)) as i32;
                l.top = (l.top as i32 - d).clamp(0, last.max(0)) as usize;
            });
            invalidate(h, None, true);
            0
        }
        0x2 => {
            let items = list(h, |l| std::mem::take(&mut l.items));
            // (the last first)
            for (i, it) in items.iter().enumerate().rev() {
                delete_item(c, h, i, it.data, Gone::Destroyed);
            }
            LISTS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
            def_proc(c, wide)
        }
        // WM_NCDESTROY: what messages between WM_DESTROY and now made again goes too
        0x82 => {
            LISTS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
            def_proc(c, wide)
        }
        _ => def_proc(c, wide),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_is_as_high_as_its_font_or_13() {
        assert_eq!(text_height(true, 16), 16);
        assert_eq!(text_height(false, 0), 13);
        assert_eq!(text_height(true, 0), 13);
        assert_eq!(text_height(false, 16), 13);
    }

    // Windows' answers (tests/winref/golden/host_listbox-32): LB_DELETESTRING of a plain item
    // without data sent WM_DELETEITEM; LB_RESETCONTENT of a plain list with an item of data 42
    // sent none; destroying an owner-drawn list sent one per item
    #[test]
    fn delete_item_is_told_as_windows_tells_it() {
        for od in [false, true] {
            for data in [0, 1, 42, u32::MAX] {
                assert!(tells_delete(Gone::Deleted, od, data));
                assert_eq!(tells_delete(Gone::Reset, od, data), od, "reset od {od} data {data}");
                assert_eq!(tells_delete(Gone::Destroyed, od, data), od || data != 0);
            }
        }
    }

    proptest::proptest! {
        // a click at an item's top + 1 is on that item, whatever the font gave
        #[test]
        fn a_click_inside_an_items_rect_is_that_item(ok: bool, th in -5i32..40, n in 1usize..30, top in 0usize..30, pick in 0usize..30) {
            let ih = text_height(ok, th).max(1);
            // (a font of 1 pixel has no top + 1 inside its items)
            proptest::prop_assume!(ih >= 2);
            let l = List { items: (0..n).map(|_| Item { text: Vec::new(), data: 0, sel: false, height: ih }).collect(), item_h: ih, top: top.min(n - 1), ..List::default() };
            let i = l.top + pick % (n - l.top);
            let r = item_rect(&l, 0, 100, i);
            proptest::prop_assert_eq!(item_at(&l, 0, r.t + 1), (i as i32, true));
        }
    }

    fn item(t: &str, h: i32) -> Item {
        Item { text: t.encode_utf16().collect(), data: t.len() as u32, sel: false, height: h }
    }
    fn texts() -> impl proptest::strategy::Strategy<Value = Vec<String>> {
        proptest::collection::vec("[a-cA-CàÀ0-2 ]{0,4}", 0..24)
    }

    proptest::proptest! {
        /// The rows a paint draws are those the old whole-list walk drew
        #[test]
        fn visible_rows_are_the_walks(hs in proptest::collection::vec(-3i32..30, 0..40), top in 0usize..45, ch in -5i32..200, var: bool, ih in 1i32..20) {
            let st = if var { LBS_OWNERDRAWVARIABLE } else { 0 };
            let l = List { items: hs.iter().map(|&h| item("x", h)).collect(), top, item_h: ih, ..List::default() };
            let mut want = Vec::new();
            let mut y = 0;
            for i in top..l.items.len() {
                if y >= ch { break; }
                let h = height_of(&l, st, i);
                want.push((i, h));
                y += h;
            }
            let got: Vec<(usize, i32)> = visible(&l, st, ch).into_iter().map(|(i, h, _)| (i, h)).collect();
            proptest::prop_assert_eq!(got, want);
        }

        /// scroll_to's running sum lands on the top the per-step sums gave
        #[test]
        fn scroll_to_is_the_per_step_sum(hs in proptest::collection::vec(-3i32..30, 1..60), top in 0usize..60, i in 0usize..60, ch in -5i32..200, var: bool, ih in 1i32..20) {
            let st = if var { LBS_OWNERDRAWVARIABLE } else { 0 };
            let mut l = List { items: hs.iter().map(|&h| item("x", h)).collect(), top: top % hs.len(), item_h: ih, ..List::default() };
            let i = i % hs.len();
            let mut old = List { items: l.items.clone(), top: l.top, item_h: ih, ..List::default() };
            if i < old.top {
                old.top = i;
            } else {
                while old.top < i && (old.top..=i).map(|k| height_of(&old, st, k)).sum::<i32>() > ch {
                    old.top += 1;
                }
            }
            scroll_to(&mut l, st, ch, i);
            proptest::prop_assert_eq!(l.top, old.top);
        }

        /// LB_FINDSTRING(EXACT) as the lower-cased copies found them
        #[test]
        fn find_is_the_folded_compare(ts in texts(), key in "[a-cA-CàÀ0-2 ]{0,3}", from in -1i32..30, exact: bool) {
            let l = List { items: ts.iter().map(|t| item(t, 1)).collect(), ..List::default() };
            let k: Vec<u16> = key.encode_utf16().collect();
            let n = ts.len();
            let want = if n == 0 { LB_ERR } else {
                let start = if from >= 0 && (from as usize) < n { from as usize + 1 } else { 0 };
                let w = key.to_lowercase();
                (0..n).map(|j| (start + j) % n).find(|&j| {
                    let t = ts[j].to_lowercase();
                    if exact { t == w } else { t.starts_with(&w) }
                }).map_or(LB_ERR, |j| j as u32)
            };
            proptest::prop_assert_eq!(find(&l, LBS_HASSTRINGS, from as u32, &k, 0, exact), want);
        }

        /// LBS_SORT: adding one at a time through the binary search keeps the order the linear
        /// scan made
        #[test]
        fn sorted_inserts_are_the_scans(ts in texts()) {
            let st = LBS_SORT | LBS_HASSTRINGS;
            let mut l = List::default();
            let mut old: Vec<Vec<u16>> = Vec::new();
            for t in &ts {
                let w: Vec<u16> = t.encode_utf16().collect();
                let at = insert_at(&l, st, None, &w);
                let o = old.iter().position(|x| crate::collate::compare(&w, x, crate::collate::NORM_IGNORECASE) == std::cmp::Ordering::Less).unwrap_or(old.len());
                proptest::prop_assert_eq!(at, o);
                l.items.insert(at, Item { text: w.clone(), ..Item::default() });
                old.insert(o, w);
            }
        }
    }
}
