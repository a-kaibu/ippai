//! Property sheets and wizards (comctl32 PropertySheet, CreatePropertySheetPage,
//! DestroyPropertySheetPage), as Wine's comctl32 propsheet.c lays them out and drives them.
//!
//! The sheet is a dialog box (DialogBoxIndirectParam, or CreateDialogIndirectParam for
//! PSH_MODELESS) whose procedure is ours: at WM_INITDIALOG it makes a tab control with a tab
//! for each page (or, for a wizard, Back / Next / Finish and an etched line), OK / Cancel /
//! Apply (and Help with PSH_HASHELP), creates the start page and sizes itself to the largest
//! page. Pages are the program's dialog templates made child dialogs (WS_CHILD | DS_CONTROL),
//! created the first time they are shown, with their PROPSHEETPAGE copy (the HPROPSHEETPAGE)
//! as the WM_INITDIALOG lParam. They hear PSN_ notifications (WM_NOTIFY, a PSHNOTIFY from the
//! sheet) and answer in DWL_MSGRESULT:
//!
//! - switching pages: PSN_KILLACTIVE to the old page (non-zero keeps it), then PSN_SETACTIVE
//!   to the new one: 0 takes it, -1 skips to the next in the same direction, another value is
//!   the resource id of the page to go to instead (`resolve`, tested below);
//! - OK / Apply: PSN_KILLACTIVE, then PSN_APPLY to every created page (PSNRET_INVALID shows
//!   that page and stops, PSNRET_INVALID_NOCHANGEPAGE stops); OK ends the sheet with 1 (or
//!   what PSM_RESTARTWINDOWS / PSM_REBOOTSYSTEM asked for);
//! - Cancel: PSN_QUERYCANCEL (non-zero keeps the sheet), PSN_RESET to every created page, 0;
//! - a wizard's Back / Next: PSN_KILLACTIVE, PSN_WIZBACK / PSN_WIZNEXT (-1 stays, 0 the page
//!   before / after, else a resource id), Finish: PSN_WIZFINISH (non-zero keeps the sheet).
//!
//! The PSM_ messages the pages send are answered too (set the selection by index, handle or
//! id, add / insert / remove pages, changed / unchanged, wizard buttons, press a button,
//! titles, Finish text, the tab control, the current page, index <-> page / window / id,
//! query siblings, restart / reboot, the result). Not done: Wizard97 headers and watermarks,
//! Aero wizards, page icons, context help.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::mem::{self, r16, r32, rp, w16, w32};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

// PROPSHEETHEADER.dwFlags
const PSH_PROPTITLE: u32 = 0x1;
const PSH_PROPSHEETPAGE: u32 = 0x8;
const PSH_WIZARD: u32 = 0x20;
const PSH_USEPSTARTPAGE: u32 = 0x40;
const PSH_NOAPPLYNOW: u32 = 0x80;
const PSH_USECALLBACK: u32 = 0x100;
const PSH_HASHELP: u32 = 0x200;
const PSH_MODELESS: u32 = 0x400;
const PSH_WIZARD97: u32 = 0x0100_0000 | 0x2000;
const PSH_AEROWIZARD: u32 = 0x4000;
const PSH_WIZARD_LITE: u32 = 0x0040_0000;
// PROPSHEETPAGE.dwFlags
const PSP_DLGINDIRECT: u32 = 0x1;
const PSP_USETITLE: u32 = 0x8;
const PSP_USECALLBACK: u32 = 0x80;
// callbacks
const PSCB_INITIALIZED: u32 = 1;
const PSCB_PRECREATE: u32 = 2;
const PSPCB_ADDREF: u32 = 0;
const PSPCB_RELEASE: u32 = 1;
const PSPCB_CREATE: u32 = 2;
// notifications
const PSN_SETACTIVE: i32 = -200;
const PSN_KILLACTIVE: i32 = -201;
const PSN_APPLY: i32 = -202;
const PSN_RESET: i32 = -203;
const PSN_HELP: i32 = -205;
const PSN_WIZBACK: i32 = -206;
const PSN_WIZNEXT: i32 = -207;
const PSN_WIZFINISH: i32 = -208;
const PSN_QUERYCANCEL: i32 = -209;
const TCN_SELCHANGE: i32 = -551;
const TCN_SELCHANGING: i32 = -552;
const PSNRET_INVALID: u32 = 1;
const PSNRET_INVALID_NOCHANGEPAGE: u32 = 2;
// PSM_SETWIZBUTTONS
const PSWIZB_BACK: u32 = 1;
const PSWIZB_NEXT: u32 = 2;
const PSWIZB_FINISH: u32 = 4;
const PSWIZB_DISABLEDFINISH: u32 = 8;
// control ids
const IDOK: u32 = 1;
const IDCANCEL: u32 = 2;
const IDHELP: u32 = 9;
const IDC_TABCONTROL: u32 = 0x3020;
const IDC_APPLY: u32 = 0x3021;
const IDC_BACK: u32 = 0x3023;
const IDC_NEXT: u32 = 0x3024;
const IDC_FINISH: u32 = 0x3025;
const IDC_LINE: u32 = 0x3026;

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_DISABLED: u32 = 0x0800_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
/// WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | DS_MODALFRAME | WS_VISIBLE | WS_DISABLED
const PAGE_DROP: u32 = 0x8000_0000 | 0x00c0_0000 | 0x0008_0000 | 0x0004_0000 | 0x80 | WS_VISIBLE | WS_DISABLED;
/// WS_CHILD | WS_TABSTOP | DS_CONTROL | DS_3DLOOK
const PAGE_ADD: u32 = WS_CHILD | WS_TABSTOP | 0x400 | 0x4;
const WM_NOTIFY: u32 = 0x4e;

/// HPROPSHEETPAGE: our copy of the PROPSHEETPAGE (whose address it is) and whether it came
/// from the W function.
static PAGES: Mutex<Option<HashMap<u32, bool>>> = Mutex::new(None);

fn page_wide(h: u32) -> Option<bool> {
    PAGES.lock().unwrap().get_or_insert_with(HashMap::new).get(&h).copied()
}

#[derive(Default)]
struct Page {
    /// the HPROPSHEETPAGE
    psp: u32,
    hwnd: u32,
}

#[derive(Default)]
struct Sheet {
    hwnd: u32,
    flags: u32,

    inst: u32,
    parent: u32,
    caption: Vec<u16>,
    callback: u32,
    start: usize,
    pages: Vec<Page>,
    active: Option<usize>,
    tab: u32,
    /// where a page goes in the sheet's client area, and its size
    area: (i32, i32, i32, i32),
    result: u32,
    ended: bool,
}

static SHEETS: Mutex<Option<HashMap<u32, Sheet>>> = Mutex::new(None);
static NEXT_KEY: Mutex<u32> = Mutex::new(1);

fn sheet<R>(key: u32, f: impl FnOnce(&mut Sheet) -> R) -> Option<R> {
    SHEETS.lock().unwrap().get_or_insert_with(HashMap::new).get_mut(&key).map(f)
}

fn key_of(hwnd: u32) -> Option<u32> {
    let g = SHEETS.lock().unwrap();
    g.as_ref()?.iter().find(|(_, s)| s.hwnd == hwnd).map(|(k, _)| *k)
}

fn g(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let f = crate::api::export(dll, name);
    c.call(f, args)
}

/// A guest UTF-16 copy of `s` with its terminator (freed by the caller).
fn guest_w(s: &[u16]) -> u32 {
    let p = mem::heap_alloc(2 * s.len() as u32 + 2, true);
    for (i, &u) in s.iter().enumerate() {
        w16(p + 2 * i as u32, u);
    }
    p
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// A string argument that may be a resource id (IS_INTRESOURCE): its text.
fn res_str(c: &mut Ctx, inst: u32, p: u32, wide_: bool) -> Vec<u16> {
    if p == 0 {
        return Vec::new();
    }
    if p < 0x10000 {
        let buf = mem::heap_alloc(1024, true);
        let n = g(c, "user32", "LoadStringW", &[inst, p, buf, 512]);
        let s = (0..n).map(|i| r16(buf + 2 * i)).collect();
        mem::heap_free(buf);
        return s;
    }
    if wide_ {
        mem::wstrn(p, TEXT_MAX)
    } else {
        crate::text::a2w(&mem::cstrn(p, 2 * TEXT_MAX))
    }
}

// PROPSHEETPAGE fields: dwSize, dwFlags, then pointer-sized ones
fn psp_field(psp: u32, i: u32) -> u32 {
    rp(psp + 8 + i * mem::ps())
}
fn psp_inst(psp: u32) -> u32 {
    psp_field(psp, 0)
}
fn psp_template(psp: u32) -> u32 {
    psp_field(psp, 1)
}
fn psp_title(psp: u32) -> u32 {
    psp_field(psp, 3)
}
fn psp_dlgproc(psp: u32) -> u32 {
    psp_field(psp, 4)
}
fn psp_callback(psp: u32) -> u32 {
    psp_field(psp, 6)
}

/// The smallest PROPSHEETPAGE we keep: up to pszHeaderSubTitle.
fn psp_min() -> u32 {
    8 + 10 * mem::ps()
}

fn create_page(c: &mut Ctx, src: u32, wide_: bool) -> u32 {
    if src == 0 {
        return 0;
    }
    let size = r32(src);
    if size < 8 + 9 * mem::ps() - 4 || size > 0x1000 {
        // (smaller than PROPSHEETPAGE_V1_SIZE)
        return 0;
    }
    let p = mem::heap_alloc(size.max(psp_min()), true);
    for i in 0..size {
        mem::w8(p + i, mem::r8(src + i));
    }
    PAGES.lock().unwrap().get_or_insert_with(HashMap::new).insert(p, wide_);
    if r32(p + 4) & PSP_USECALLBACK != 0 && psp_callback(p) != 0 {
        let cb = psp_callback(p);
        c.call(cb, &[0, PSPCB_ADDREF, p]);
    }
    p
}

fn destroy_page(c: &mut Ctx, h: u32) -> bool {
    if page_wide(h).is_none() {
        return false;
    }
    if r32(h + 4) & PSP_USECALLBACK != 0 && psp_callback(h) != 0 {
        let cb = psp_callback(h);
        c.call(cb, &[0, PSPCB_RELEASE, h]);
    }
    PAGES.lock().unwrap().get_or_insert_with(HashMap::new).remove(&h);
    mem::heap_free(h);
    true
}

fn CreatePropertySheetPageA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    create_page(c, p, false)
}
fn CreatePropertySheetPageW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    create_page(c, p, true)
}
fn DestroyPropertySheetPage(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    destroy_page(c, h) as u32
}

/// The page's dialog template (DLGTEMPLATE or DLGTEMPLATEEX).
fn page_template(c: &mut Ctx, psp: u32) -> u32 {
    let t = psp_template(psp);
    if r32(psp + 4) & PSP_DLGINDIRECT != 0 {
        return t;
    }
    let inst = psp_inst(psp);
    let wide_ = page_wide(psp).unwrap_or(true);
    let r = g(c, "kernel32", if wide_ { "FindResourceW" } else { "FindResourceA" }, &[inst, t, 5]);
    if r == 0 {
        return 0;
    }
    let h = g(c, "kernel32", "LoadResource", &[inst, r]);
    g(c, "kernel32", "LockResource", &[h])
}

fn is_ex(t: u32) -> bool {
    r16(t) == 1 && r16(t + 2) == 0xffff
}
/// A template's style's offset
fn style_at(t: u32) -> u32 {
    if is_ex(t) {
        t + 12
    } else {
        t
    }
}
/// A template's size in dialog units
fn template_size(t: u32) -> (i32, i32) {
    let o = if is_ex(t) { t + 22 } else { t + 14 };
    (r16(o) as i16 as i32, r16(o + 2) as i16 as i32)
}
/// A template's caption (after the menu and class)
fn template_title(t: u32) -> Vec<u16> {
    let mut p = if is_ex(t) { t + 26 } else { t + 18 };
    for _ in 0..2 {
        p = match r16(p) {
            0 => p + 2,
            0xffff => p + 4,
            _ => {
                // (bounded: a damaged template's name runs to the first zero word, wherever)
                let mut k = 0;
                while r16(p) != 0 && k < TEXT_MAX {
                    p += 2;
                    k += 1;
                }
                p + 2
            }
        };
    }
    if r16(p) == 0xffff {
        return Vec::new();
    }
    mem::wstrn(p, TEXT_MAX)
}

/// The longest caption or title read from a template or a page (a window caption is far
/// shorter; a damaged resource would otherwise be copied up to the next zero word)
const TEXT_MAX: u32 = 0x8000;

fn page_title(c: &mut Ctx, psp: u32) -> Vec<u16> {
    if r32(psp + 4) & PSP_USETITLE != 0 {
        let inst = psp_inst(psp);
        let wide_ = page_wide(psp).unwrap_or(true);
        return res_str(c, inst, psp_title(psp), wide_);
    }
    let t = page_template(c, psp);
    if t == 0 {
        Vec::new()
    } else {
        template_title(t)
    }
}

fn page_res_id(c: &mut Ctx, psp: u32) -> u32 {
    let _ = c;
    if r32(psp + 4) & PSP_DLGINDIRECT != 0 {
        0
    } else {
        psp_template(psp)
    }
}

fn proc_addr() -> u32 {
    static P: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("propsheet_proc", sheet_proc, 16))
}

/// The sheet's template: a popup with a caption, a system menu and no controls (they are
/// made at WM_INITDIALOG).
fn sheet_template(modeless: bool) -> u32 {
    let t = mem::heap_alloc(32, true);
    // WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN | DS_MODALFRAME | DS_3DLOOK | DS_CONTEXTHELP
    let style = 0x8000_0000u32 | 0x00c0_0000 | 0x0008_0000 | 0x0200_0000 | 0x80 | 0x4 | 0x2000 | if modeless { WS_VISIBLE } else { 0 };
    w32(t, style);
    w32(t + 4, 0x0001_0000); // WS_EX_CONTROLPARENT
    w16(t + 14, 212);
    w16(t + 16, 188);
    t
}

fn property_sheet(c: &mut Ctx, h: u32, wide_: bool) -> u32 {
    if h == 0 {
        return u32::MAX;
    }
    let ps = mem::ps();
    let flags = r32(h + 4);
    let parent = rp(h + 8);
    let inst = rp(h + 8 + ps);
    let caption_p = rp(h + 8 + 3 * ps);
    let n = r32(h + 8 + 4 * ps);
    let start = rp(h + 8 + 5 * ps);
    let arr = rp(h + 8 + 6 * ps);
    let callback = if flags & PSH_USECALLBACK != 0 { rp(h + 8 + 7 * ps) } else { 0 };
    let mut pages = Vec::new();
    let mut p = arr;
    for i in 0..n.min(100) {
        let hp = if flags & PSH_PROPSHEETPAGE != 0 {
            let sz = r32(p);
            let hp = create_page(c, p, wide_);
            p += sz;
            hp
        } else {
            rp(arr + i * ps)
        };
        if hp != 0 && page_wide(hp).is_some() {
            pages.push(Page { psp: hp, hwnd: 0 });
        }
    }
    if pages.is_empty() {
        return u32::MAX;
    }
    let mut start_idx = if flags & PSH_USEPSTARTPAGE != 0 { 0 } else { start as usize };
    if flags & PSH_USEPSTARTPAGE != 0 {
        let want = res_str(c, inst, start, wide_);
        for (i, pg) in pages.iter().enumerate() {
            let psp = pg.psp;
            if page_title(c, psp) == want {
                start_idx = i;
                break;
            }
        }
    }
    if start_idx >= pages.len() {
        start_idx = 0;
    }
    let mut caption = res_str(c, inst, caption_p, wide_);
    if flags & PSH_PROPTITLE != 0 && !is_wizard(flags) {
        caption = prop_title(&caption);
    }
    let key = {
        let mut k = NEXT_KEY.lock().unwrap();
        *k += 1;
        *k
    };
    SHEETS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, Sheet { flags, inst, parent, caption, callback, start: start_idx, pages, ..Sheet::default() });
    let modeless = flags & PSH_MODELESS != 0;
    let t = sheet_template(modeless);
    if callback != 0 {
        c.call(callback, &[0, PSCB_PRECREATE, t]);
    }
    let r = if modeless {
        g(c, "user32", "CreateDialogIndirectParamW", &[inst, t, parent, proc_addr(), key])
    } else {
        g(c, "user32", "DialogBoxIndirectParamW", &[inst, t, parent, proc_addr(), key])
    };
    mem::heap_free(t);
    // a modeless sheet whose window never came up gets no WM_NCDESTROY to free it (a second
    // end_sheet after one is a no-op)
    if !modeless || r == 0 {
        end_sheet(c, key);
    }
    r
}

/// The sheet is gone: its pages are destroyed (as PropertySheet does with them).
fn end_sheet(c: &mut Ctx, key: u32) {
    let s = SHEETS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&key);
    if let Some(s) = s {
        for p in s.pages {
            destroy_page(c, p.psp);
        }
    }
}

fn is_wizard(flags: u32) -> bool {
    flags & (PSH_WIZARD | PSH_WIZARD97 | PSH_AEROWIZARD | PSH_WIZARD_LITE) != 0
}

/// PSH_PROPTITLE's caption, as Japanese Windows writes it.
fn prop_title(s: &[u16]) -> Vec<u16> {
    let mut v = s.to_vec();
    v.extend(wide("のプロパティ"));
    v
}

fn PropertySheetA(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    property_sheet(c, h, false)
}
fn PropertySheetW(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    property_sheet(c, h, true)
}

/// Sends a PSN_ notification (a PSHNOTIFY from the sheet) to `page`: its DWL_MSGRESULT.
fn notify(c: &mut Ctx, sheet_hwnd: u32, page: u32, code: i32, lparam: u32) -> u32 {
    if page == 0 {
        return 0;
    }
    let ps = mem::ps();
    let n = mem::heap_alloc(4 * ps, true);
    mem::wp(n, sheet_hwnd);
    mem::wp(n + ps, 0);
    w32(n + 2 * ps, code as u32);
    mem::wp(n + 3 * ps, lparam);
    g(c, "user32", "SetWindowLongW", &[page, 0, 0]);
    let r = g(c, "user32", "SendMessageW", &[page, WM_NOTIFY, 0, n]);
    mem::heap_free(n);
    r
}

/// Where SetCurSel ends: from `index`, asking each page in turn (`ask`: its PSN_SETACTIVE
/// answer) until one takes it (0); -1 moves on by `step` (stopping at the ends), another
/// value is the resource id of the page to go to (`find`; an unknown one stays). Windows
/// would loop forever on pages that send each other back and forth; this stops after a
/// round of every page.
fn resolve(mut index: usize, step: isize, n: usize, mut ask: impl FnMut(usize) -> i32, find: impl Fn(i32) -> Option<usize>) -> usize {
    if n == 0 {
        return 0;
    }
    index = index.min(n - 1);
    for _ in 0..4 * n + 4 {
        match ask(index) {
            0 => return index,
            -1 => {
                let next = index as isize + if step == 0 { 1 } else { step };
                if next < 0 {
                    return 0;
                }
                if next as usize >= n {
                    return n - 1;
                }
                index = next as usize;
            }
            id => match find(id) {
                Some(i) => index = i,
                None => return index,
            },
        }
    }
    index
}

/// Creates page `i`'s window if it has none: its template made a child dialog of the sheet.
fn ensure_page(c: &mut Ctx, key: u32, i: usize) -> u32 {
    let Some((hwnd, psp, sheet_hwnd, area)) = sheet(key, |s| s.pages.get(i).map(|p| (p.hwnd, p.psp, s.hwnd, s.area))).flatten() else { return 0 };
    if hwnd != 0 {
        return hwnd;
    }
    if r32(psp + 4) & PSP_USECALLBACK != 0 && psp_callback(psp) != 0 {
        let cb = psp_callback(psp);
        if c.call(cb, &[0, PSPCB_CREATE, psp]) == 0 {
            return 0;
        }
    }
    let t = page_template(c, psp);
    if t == 0 {
        return 0;
    }
    // (the template made a child for the call, then put back as it was)
    let at = style_at(t);
    let old = r32(at);
    w32(at, old & !PAGE_DROP | PAGE_ADD);
    let wide_ = page_wide(psp).unwrap_or(true);
    let h = g(c, "user32", if wide_ { "CreateDialogIndirectParamW" } else { "CreateDialogIndirectParamA" }, &[psp_inst(psp), t, sheet_hwnd, psp_dlgproc(psp), psp]);
    w32(at, old);
    if h == 0 {
        return 0;
    }
    g(c, "user32", "ShowWindow", &[h, 0]);
    let (x, y, w, hh) = area;
    if w > 0 {
        g(c, "user32", "SetWindowPos", &[h, 0, x as u32, y as u32, w as u32, hh as u32, 0x14]);
    }
    sheet(key, |s| {
        if let Some(p) = s.pages.get_mut(i) {
            p.hwnd = h;
        }
    });
    h
}

/// PSN_KILLACTIVE to the active page: whether it lets go.
fn can_leave(c: &mut Ctx, key: u32) -> bool {
    let Some((sh, page)) = sheet(key, |s| (s.hwnd, s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd))) else { return false };
    notify(c, sh, page, PSN_KILLACTIVE, 0) == 0
}

/// Shows page `index` (or the one its PSN_SETACTIVE sends to), stepping by `step` past pages
/// that skip themselves.
fn set_cur_sel(c: &mut Ctx, key: u32, index: usize, step: isize) {
    let Some((n, sh)) = sheet(key, |s| (s.pages.len(), s.hwnd)) else { return };
    if n == 0 {
        return;
    }
    let mut ids = Vec::new();
    for i in 0..n {
        let psp = sheet(key, |s| s.pages[i].psp).unwrap_or(0);
        ids.push(page_res_id(c, psp));
    }
    let to = resolve(
        index,
        step,
        n,
        |i| {
            let h = ensure_page(c, key, i);
            notify(c, sh, h, PSN_SETACTIVE, 0) as i32
        },
        |id| ids.iter().position(|&r| r == id as u32 && r != 0),
    );
    show_page(c, key, to);
}

fn show_page(c: &mut Ctx, key: u32, i: usize) {
    let h = ensure_page(c, key, i);
    let Some((old, tab)) = sheet(key, |s| (s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd), s.tab)) else { return };
    if old != 0 && old != h {
        g(c, "user32", "ShowWindow", &[old, 0]);
    }
    if h != 0 {
        g(c, "user32", "ShowWindow", &[h, 5]);
    }
    if tab != 0 {
        g(c, "user32", "SendMessageW", &[tab, 0x130c, i as u32, 0]);
    }
    sheet(key, |s| s.active = Some(i));
}

fn item(c: &mut Ctx, sh: u32, id: u32) -> u32 {
    g(c, "user32", "GetDlgItem", &[sh, id])
}

fn create_control(c: &mut Ctx, sh: u32, inst: u32, class: &str, text: &[u16], style: u32, id: u32, font: u32) -> u32 {
    let cp = guest_w(&wide(class));
    let tp = guest_w(text);
    let h = g(c, "user32", "CreateWindowExW", &[0, cp, tp, WS_CHILD | style, 0, 0, 10, 10, sh, id, inst, 0]);
    mem::heap_free(cp);
    mem::heap_free(tp);
    if font != 0 && h != 0 {
        g(c, "user32", "SendMessageW", &[h, 0x30, font, 0]);
    }
    h
}

fn insert_tab(c: &mut Ctx, tab: u32, at: usize, text: &[u16]) {
    if tab == 0 {
        return;
    }
    let ps = mem::ps();
    let tp = guest_w(text);
    // TCITEMW: mask, state, stateMask, pszText, cchTextMax, iImage, lParam
    let it = mem::heap_alloc(16 + 4 * ps, true);
    w32(it, 1);
    mem::wp(it + 12, tp);
    g(c, "user32", "SendMessageW", &[tab, 0x133e, at as u32, it]);
    mem::heap_free(it);
    mem::heap_free(tp);
}

fn dlu(c: &mut Ctx, sh: u32, x: i32, y: i32) -> (i32, i32) {
    let r = mem::heap_alloc(16, true);
    w32(r + 8, x as u32);
    w32(r + 12, y as u32);
    g(c, "user32", "MapDialogRect", &[sh, r]);
    let v = (r32(r + 8) as i32, r32(r + 12) as i32);
    mem::heap_free(r);
    v
}

fn move_to(c: &mut Ctx, h: u32, x: i32, y: i32, w: i32, hh: i32) {
    if h != 0 {
        g(c, "user32", "SetWindowPos", &[h, 0, x as u32, y as u32, w as u32, hh as u32, 0x14]);
    }
}

/// Lays the sheet out: (page area, client size) from the largest page (in pixels), the
/// tab control's TCM_ADJUSTRECT margins (l, t, r, b: what it adds around a display area), the
/// button size and the margin. Pure, tested below.
fn layout(page: (i32, i32), tab_margins: Option<(i32, i32, i32, i32)>, button: (i32, i32), margin: (i32, i32), buttons: i32) -> ((i32, i32, i32, i32), (i32, i32), i32) {
    let (mx, my) = margin;
    let (pw, ph) = page;
    match tab_margins {
        Some((l, t, r, b)) => {
            let (tw, th) = (pw + l + r, ph + t + b);
            let by = my + th + my;
            let w = (mx + tw + mx).max(mx + buttons * (button.0 + mx / 2) + mx / 2);
            ((mx + l, my + t, pw, ph), (w, by + button.1 + my), by)
        }
        None => {
            // the page, an etched line and the buttons under it
            let ly = my + ph + my;
            let by = ly + 2 + my;
            let w = (mx + pw + mx).max(mx + buttons * (button.0 + mx / 2) + mx / 2);
            ((mx, my, pw, ph), (w, by + button.1 + my), by)
        }
    }
}

fn init_sheet(c: &mut Ctx, sh: u32, key: u32) {
    let Some((flags, inst, caption, n, start, callback)) = sheet(key, |s| {
        s.hwnd = sh;
        (s.flags, s.inst, s.caption.clone(), s.pages.len(), s.start, s.callback)
    }) else {
        return;
    };
    let wiz = is_wizard(flags);
    let tp = guest_w(&caption);
    g(c, "user32", "SetWindowTextW", &[sh, tp]);
    mem::heap_free(tp);
    // the largest page, and the font of the first
    let (mut cx, mut cy) = (0, 0);
    let mut font = 0;
    for i in 0..n {
        let psp = sheet(key, |s| s.pages[i].psp).unwrap_or(0);
        let t = page_template(c, psp);
        if t != 0 {
            let (w, h) = template_size(t);
            cx = cx.max(w);
            cy = cy.max(h);
        }
    }
    let (pw, ph) = dlu(c, sh, cx, cy);
    let inst_btn = inst;
    let tab = if wiz {
        0
    } else {
        let t = create_control(c, sh, inst_btn, "SysTabControl32", &[], WS_VISIBLE | WS_TABSTOP | WS_GROUP | WS_CLIPSIBLINGS, IDC_TABCONTROL, 0);
        for i in 0..n {
            let psp = sheet(key, |s| s.pages[i].psp).unwrap_or(0);
            let title = page_title(c, psp);
            insert_tab(c, t, i, &title);
        }
        t
    };
    sheet(key, |s| s.tab = tab);
    // (fonts: the first page's, once it exists; until then the dialog's)
    let margins = if wiz {
        None
    } else {
        // TCM_ADJUSTRECT(TRUE) on an empty rect: what the tabs and border add
        let r = mem::heap_alloc(16, true);
        g(c, "user32", "SendMessageW", &[tab, 0x1328, 1, r]);
        let m = (-(r32(r) as i32), -(r32(r + 4) as i32), r32(r + 8) as i32, r32(r + 12) as i32);
        mem::heap_free(r);
        Some(m)
    };
    let button = dlu(c, sh, 50, 14);
    let margin = dlu(c, sh, 7, 7);
    let mut ids: Vec<(u32, Vec<u16>, u32)> = Vec::new();
    if wiz {
        ids.push((IDC_BACK, wide("< 戻る(&B)"), WS_VISIBLE | WS_TABSTOP | WS_GROUP));
        ids.push((IDC_NEXT, wide("次へ(&N) >"), WS_VISIBLE | WS_TABSTOP | 1));
        ids.push((IDC_FINISH, wide("完了"), WS_TABSTOP));
        ids.push((IDCANCEL, wide("キャンセル"), WS_VISIBLE | WS_TABSTOP));
    } else {
        ids.push((IDOK, wide("OK"), WS_VISIBLE | WS_TABSTOP | WS_GROUP | 1));
        ids.push((IDCANCEL, wide("キャンセル"), WS_VISIBLE | WS_TABSTOP));
        if flags & PSH_NOAPPLYNOW == 0 {
            ids.push((IDC_APPLY, wide("適用(&A)"), WS_VISIBLE | WS_TABSTOP | WS_DISABLED));
        }
    }
    if flags & PSH_HASHELP != 0 {
        ids.push((IDHELP, wide("ヘルプ"), WS_VISIBLE | WS_TABSTOP));
    }
    // (Finish shares Next's place)
    let slots = ids.iter().filter(|b| b.0 != IDC_FINISH).count() as i32;
    let (area, (w, h), by) = layout((pw, ph), margins, button, margin, slots);
    sheet(key, |s| s.area = area);
    let mut btns = Vec::new();
    for (id, text, style) in ids {
        let b = create_control(c, sh, inst_btn, "Button", &text, style, id, font);
        btns.push((id, b));
    }
    // right-aligned, Next and Finish in one place
    let mut x = w - margin.0;
    let gap = margin.0 / 2;
    for (id, b) in btns.iter().rev() {
        if *id == IDC_FINISH {
            continue;
        }
        x -= button.0;
        move_to(c, *b, x, by, button.0, button.1);
        if *id == IDC_NEXT {
            let f = item(c, sh, IDC_FINISH);
            move_to(c, f, x, by, button.0, button.1);
        }
        // (Back sits against Next)
        x -= if *id == IDC_NEXT { 0 } else { gap };
    }
    if wiz {
        let line = create_control(c, sh, inst_btn, "Static", &[], WS_VISIBLE | 0x10, IDC_LINE, 0);
        move_to(c, line, margin.0, by - margin.1 - 2, w - 2 * margin.0, 2);
        // (Back starts disabled until PSM_SETWIZBUTTONS says otherwise? Windows enables both)
    } else {
        let (l, t, r, b) = margins.unwrap_or_default();
        move_to(c, tab, area.0 - l, area.1 - t, area.2 + l + r, area.3 + t + b);
    }
    // the window around that client area, centred on the parent (or the screen)
    let style = g(c, "user32", "GetWindowLongW", &[sh, (-16i32) as u32]);
    let ex = g(c, "user32", "GetWindowLongW", &[sh, (-20i32) as u32]);
    let r = mem::heap_alloc(16, true);
    w32(r + 8, w as u32);
    w32(r + 12, h as u32);
    g(c, "user32", "AdjustWindowRectEx", &[r, style, 0, ex]);
    let (ww, wh) = (r32(r + 8) as i32 - r32(r) as i32, r32(r + 12) as i32 - r32(r + 4) as i32);
    let parent = sheet(key, |s| s.parent).unwrap_or(0);
    let (ox, oy, ow, oh) = if parent != 0 && g(c, "user32", "GetWindowRect", &[parent, r]) != 0 {
        (r32(r) as i32, r32(r + 4) as i32, r32(r + 8) as i32 - r32(r) as i32, r32(r + 12) as i32 - r32(r + 4) as i32)
    } else {
        (0, 0, g(c, "user32", "GetSystemMetrics", &[0]) as i32, g(c, "user32", "GetSystemMetrics", &[1]) as i32)
    };
    mem::heap_free(r);
    g(c, "user32", "SetWindowPos", &[sh, 0, (ox + (ow - ww) / 2).max(0) as u32, (oy + (oh - wh) / 2).max(0) as u32, ww as u32, wh as u32, 0x14]);
    // the start page (and its font for our controls)
    set_cur_sel(c, key, start, 1);
    let first = sheet(key, |s| s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd)).unwrap_or(0);
    if first != 0 {
        font = g(c, "user32", "SendMessageW", &[first, 0x31, 0, 0]);
    }
    if font != 0 {
        for (_, b) in &btns {
            g(c, "user32", "SendMessageW", &[*b, 0x30, font, 0]);
        }
        if tab != 0 {
            g(c, "user32", "SendMessageW", &[tab, 0x30, font, 0]);
        }
    }
    if callback != 0 {
        c.call(callback, &[sh, PSCB_INITIALIZED, 0]);
    }
}

fn end(c: &mut Ctx, key: u32, result: u32) {
    let Some((sh, modeless, r)) = sheet(key, |s| {
        s.ended = true;
        if s.result == 0 {
            s.result = result;
        }
        (s.hwnd, s.flags & PSH_MODELESS != 0, s.result)
    }) else {
        return;
    };
    let r = if result == 0 { 0 } else { r };
    sheet(key, |s| s.result = r);
    if !modeless {
        g(c, "user32", "EndDialog", &[sh, r]);
    }
}

/// OK (`ok`) or Apply: whether the pages took it.
fn apply(c: &mut Ctx, key: u32, ok: bool) -> bool {
    if !can_leave(c, key) {
        return false;
    }
    let Some((sh, pages)) = sheet(key, |s| (s.hwnd, s.pages.iter().map(|p| p.hwnd).collect::<Vec<_>>())) else { return false };
    for (i, h) in pages.iter().enumerate() {
        match notify(c, sh, *h, PSN_APPLY, ok as u32) {
            PSNRET_INVALID => {
                show_page(c, key, i);
                return false;
            }
            PSNRET_INVALID_NOCHANGEPAGE => return false,
            _ => {}
        }
    }
    if !ok {
        let a = item(c, sh, IDC_APPLY);
        g(c, "user32", "EnableWindow", &[a, 0]);
        // (the active page hears PSN_SETACTIVE again)
        let cur = sheet(key, |s| s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd)).unwrap_or(0);
        notify(c, sh, cur, PSN_SETACTIVE, 0);
    }
    true
}

fn cancel(c: &mut Ctx, key: u32) {
    let Some((sh, cur, pages)) = sheet(key, |s| (s.hwnd, s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd), s.pages.iter().map(|p| p.hwnd).collect::<Vec<_>>())) else { return };
    if notify(c, sh, cur, PSN_QUERYCANCEL, 0) != 0 {
        return;
    }
    for h in pages {
        notify(c, sh, h, PSN_RESET, 0);
    }
    sheet(key, |s| s.result = 0);
    end(c, key, 0);
}

fn wiz_step(c: &mut Ctx, key: u32, forward: bool) {
    let Some((sh, active, n)) = sheet(key, |s| (s.hwnd, s.active, s.pages.len())) else { return };
    let Some(a) = active else { return };
    let cur = sheet(key, |s| s.pages[a].hwnd).unwrap_or(0);
    if notify(c, sh, cur, PSN_KILLACTIVE, 0) != 0 {
        return;
    }
    let r = notify(c, sh, cur, if forward { PSN_WIZNEXT } else { PSN_WIZBACK }, 0) as i32;
    let to = match r {
        -1 => return,
        0 => {
            if forward {
                a + 1
            } else if a == 0 {
                return;
            } else {
                a - 1
            }
        }
        id => match find_id(c, key, id as u32) {
            Some(i) => i,
            None => return,
        },
    };
    if to >= n {
        return;
    }
    set_cur_sel(c, key, to, if forward { 1 } else { -1 });
}

fn find_id(c: &mut Ctx, key: u32, id: u32) -> Option<usize> {
    let n = sheet(key, |s| s.pages.len())?;
    (0..n).find(|&i| {
        let psp = sheet(key, |s| s.pages[i].psp).unwrap_or(0);
        page_res_id(c, psp) == id && id != 0
    })
}

fn wiz_finish(c: &mut Ctx, key: u32) {
    let Some((sh, cur)) = sheet(key, |s| (s.hwnd, s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd))) else { return };
    if notify(c, sh, cur, PSN_WIZFINISH, 0) != 0 {
        return;
    }
    end(c, key, 1);
}

fn set_wiz_buttons(c: &mut Ctx, sh: u32, f: u32) {
    let back = item(c, sh, IDC_BACK);
    let next = item(c, sh, IDC_NEXT);
    let fin = item(c, sh, IDC_FINISH);
    g(c, "user32", "EnableWindow", &[back, (f & PSWIZB_BACK != 0) as u32]);
    let finish = f & (PSWIZB_FINISH | PSWIZB_DISABLEDFINISH) != 0;
    g(c, "user32", "ShowWindow", &[next, if finish { 0 } else { 5 }]);
    g(c, "user32", "ShowWindow", &[fin, if finish { 5 } else { 0 }]);
    g(c, "user32", "EnableWindow", &[next, (f & PSWIZB_NEXT != 0) as u32]);
    g(c, "user32", "EnableWindow", &[fin, (f & PSWIZB_FINISH != 0) as u32]);
    // (DM_SETDEFID: the button that is up)
    g(c, "user32", "SendMessageW", &[sh, 0x401, if finish { IDC_FINISH } else { IDC_NEXT }, 0]);
}

fn index_of_page(key: u32, hpage: u32) -> Option<usize> {
    sheet(key, |s| s.pages.iter().position(|p| p.psp == hpage)).flatten()
}

fn add_page(c: &mut Ctx, key: u32, at: usize, hpage: u32) -> bool {
    if page_wide(hpage).is_none() {
        return false;
    }
    let Some(tab) = sheet(key, |s| {
        let at = at.min(s.pages.len());
        s.pages.insert(at, Page { psp: hpage, hwnd: 0 });
        if let Some(a) = s.active {
            if a >= at {
                s.active = Some(a + 1);
            }
        }
        s.tab
    }) else {
        return false;
    };
    let title = page_title(c, hpage);
    insert_tab(c, tab, at, &title);
    true
}

fn remove_page(c: &mut Ctx, key: u32, i: usize) {
    let Some((h, psp, tab, active, n)) = sheet(key, |s| s.pages.get(i).map(|p| (p.hwnd, p.psp, s.tab, s.active, s.pages.len()))).flatten() else { return };
    if active == Some(i) && n > 1 {
        // (the page after it, or before the last)
        let to = if i + 1 < n { i + 1 } else { i - 1 };
        show_page(c, key, to);
    }
    if h != 0 {
        g(c, "user32", "DestroyWindow", &[h]);
    }
    if tab != 0 {
        g(c, "user32", "SendMessageW", &[tab, 0x1308, i as u32, 0]);
    }
    sheet(key, |s| {
        s.pages.remove(i);
        s.active = match s.active {
            Some(a) if a == i => None,
            Some(a) if a > i => Some(a - 1),
            a => a,
        };
    });
    destroy_page(c, psp);
}

/// The dialog procedure: Some(DWL_MSGRESULT) for what it handles.
fn handle(c: &mut Ctx, sh: u32, m: u32, wp: u32, lp: u32) -> Option<u32> {
    if m == 0x110 {
        init_sheet(c, sh, lp);
        return Some(1);
    }
    let key = key_of(sh)?;
    let page_by = |key: u32, wp: u32, lp: u32| if lp != 0 { index_of_page(key, lp) } else { Some(wp as usize) };
    match m {
        0x111 => {
            let id = wp & 0xffff;
            match id {
                IDOK => {
                    if apply(c, key, true) {
                        end(c, key, 1);
                    }
                }
                IDCANCEL => cancel(c, key),
                IDC_APPLY => {
                    apply(c, key, false);
                }
                IDC_BACK => wiz_step(c, key, false),
                IDC_NEXT => wiz_step(c, key, true),
                IDC_FINISH => wiz_finish(c, key),
                IDHELP => {
                    let cur = sheet(key, |s| s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd)).unwrap_or(0);
                    notify(c, sh, cur, PSN_HELP, 0);
                }
                _ => return None,
            }
            Some(0)
        }
        WM_NOTIFY => {
            let ps = mem::ps();
            let (from_id, code) = (rp(lp + ps), r32(lp + 2 * ps) as i32);
            if from_id != IDC_TABCONTROL {
                return None;
            }
            match code {
                TCN_SELCHANGING => Some(!can_leave(c, key) as u32),
                TCN_SELCHANGE => {
                    let tab = sheet(key, |s| s.tab).unwrap_or(0);
                    let i = g(c, "user32", "SendMessageW", &[tab, 0x130b, 0, 0]) as usize;
                    set_cur_sel(c, key, i, 0);
                    Some(0)
                }
                _ => None,
            }
        }
        // PSM_SETCURSEL
        0x465 => {
            let i = page_by(key, wp, lp)?;
            if sheet(key, |s| i < s.pages.len()) != Some(true) {
                return Some(0);
            }
            if !can_leave(c, key) {
                return Some(0);
            }
            set_cur_sel(c, key, i, 0);
            Some(1)
        }
        // PSM_REMOVEPAGE
        0x466 => {
            if let Some(i) = page_by(key, wp, lp) {
                remove_page(c, key, i);
            }
            Some(0)
        }
        // PSM_ADDPAGE
        0x467 => {
            let n = sheet(key, |s| s.pages.len()).unwrap_or(0);
            Some(add_page(c, key, n, lp) as u32)
        }
        // PSM_CHANGED / PSM_UNCHANGED
        0x468 | 0x46d => {
            let a = item(c, sh, IDC_APPLY);
            g(c, "user32", "EnableWindow", &[a, (m == 0x468) as u32]);
            Some(0)
        }
        // PSM_RESTARTWINDOWS / PSM_REBOOTSYSTEM
        0x469 | 0x46a => {
            sheet(key, |s| s.result = if m == 0x469 { 2 } else { 3 });
            Some(0)
        }
        // PSM_CANCELTOCLOSE: OK says Close, Cancel is off
        0x46b => {
            let ok = item(c, sh, IDOK);
            let t = guest_w(&wide("閉じる"));
            g(c, "user32", "SetWindowTextW", &[ok, t]);
            mem::heap_free(t);
            let cl = item(c, sh, IDCANCEL);
            g(c, "user32", "EnableWindow", &[cl, 0]);
            Some(0)
        }
        // PSM_QUERYSIBLINGS: each page until one answers
        0x46c => {
            let pages = sheet(key, |s| s.pages.iter().map(|p| p.hwnd).collect::<Vec<_>>()).unwrap_or_default();
            for h in pages.into_iter().filter(|&h| h != 0) {
                let r = g(c, "user32", "SendMessageW", &[h, 0x46c, wp, lp]);
                if r != 0 {
                    return Some(r);
                }
            }
            Some(0)
        }
        // PSM_APPLY
        0x46e => Some(apply(c, key, false) as u32),
        // PSM_SETTITLEA / W
        0x46f | 0x478 => {
            let inst = sheet(key, |s| s.inst).unwrap_or(0);
            let mut t = res_str(c, inst, lp, m == 0x478);
            if wp & PSH_PROPTITLE != 0 {
                t = prop_title(&t);
            }
            let p = guest_w(&t);
            g(c, "user32", "SetWindowTextW", &[sh, p]);
            mem::heap_free(p);
            Some(0)
        }
        // PSM_SETWIZBUTTONS
        0x470 => {
            set_wiz_buttons(c, sh, lp);
            Some(0)
        }
        // PSM_PRESSBUTTON
        0x471 => {
            let id = [IDC_BACK, IDC_NEXT, IDC_FINISH, IDOK, IDC_APPLY, IDCANCEL, IDHELP].get(wp as usize).copied()?;
            let b = item(c, sh, id);
            g(c, "user32", "PostMessageW", &[sh, 0x111, id, b]);
            Some(0)
        }
        // PSM_SETCURSELID
        0x472 => {
            let i = find_id(c, key, lp)?;
            if !can_leave(c, key) {
                return Some(0);
            }
            set_cur_sel(c, key, i, 0);
            Some(1)
        }
        // PSM_SETFINISHTEXTA / W: Finish with that text, no Back and Next
        0x473 | 0x479 => {
            let inst = sheet(key, |s| s.inst).unwrap_or(0);
            let t = res_str(c, inst, lp, m == 0x479);
            let fin = item(c, sh, IDC_FINISH);
            let p = guest_w(&t);
            g(c, "user32", "SetWindowTextW", &[fin, p]);
            mem::heap_free(p);
            for id in [IDC_BACK, IDC_NEXT] {
                let b = item(c, sh, id);
                g(c, "user32", "ShowWindow", &[b, 0]);
            }
            g(c, "user32", "ShowWindow", &[fin, 5]);
            g(c, "user32", "EnableWindow", &[fin, 1]);
            g(c, "user32", "SendMessageW", &[sh, 0x401, IDC_FINISH, 0]);
            Some(0)
        }
        // PSM_GETTABCONTROL
        0x474 => sheet(key, |s| s.tab),
        // PSM_ISDIALOGMESSAGE
        0x475 => Some(g(c, "user32", "IsDialogMessageW", &[sh, lp])),
        // PSM_GETCURRENTPAGEHWND (none once the sheet has ended)
        0x476 => sheet(key, |s| if s.ended { 0 } else { s.active.and_then(|a| s.pages.get(a)).map_or(0, |p| p.hwnd) }),
        // PSM_INSERTPAGE: after the page `wp` names (an index, or an HPROPSHEETPAGE)
        0x477 => {
            let at = if wp < 0x10000 { wp as usize } else { index_of_page(key, wp).map_or(0, |i| i + 1) };
            Some(add_page(c, key, at, lp) as u32)
        }
        // PSM_HWNDTOINDEX / INDEXTOHWND / PAGETOINDEX / INDEXTOPAGE / IDTOINDEX / INDEXTOID
        0x481 => sheet(key, |s| s.pages.iter().position(|p| p.hwnd == wp && wp != 0).map_or(u32::MAX, |i| i as u32)),
        0x482 => sheet(key, |s| s.pages.get(wp as usize).map_or(0, |p| p.hwnd)),
        0x483 => Some(index_of_page(key, lp).map_or(u32::MAX, |i| i as u32)),
        0x484 => sheet(key, |s| s.pages.get(wp as usize).map_or(0, |p| p.psp)),
        0x485 => Some(find_id(c, key, lp).map_or(u32::MAX, |i| i as u32)),
        0x486 => {
            let psp = sheet(key, |s| s.pages.get(wp as usize).map_or(0, |p| p.psp)).unwrap_or(0);
            Some(if psp == 0 { 0 } else { page_res_id(c, psp) })
        }
        // PSM_GETRESULT (-1 while the sheet is up)
        0x487 => sheet(key, |s| if s.ended { s.result } else { u32::MAX }),
        // WM_NCDESTROY: a modeless sheet's state goes with it
        0x82 => {
            if sheet(key, |s| s.flags & PSH_MODELESS != 0) == Some(true) {
                end_sheet(c, key);
            }
            None
        }
        _ => None,
    }
}

fn sheet_proc(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    match handle(c, h, m, wp, lp) {
        Some(r) => {
            if m != 0x110 {
                g(c, "user32", "SetWindowLongW", &[h, 0, r]);
            }
            1
        }
        None => 0,
    }
}

pub fn init() {
    reg!("comctl32"; PropertySheetA, PropertySheetW, CreatePropertySheetPageA, CreatePropertySheetPageW, DestroyPropertySheetPage);
    implement("comctl32", "PropertySheet", PropertySheetA);
    implement("comctl32", "CreatePropertySheetPage", CreatePropertySheetPageA);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// review b09: a template's caption is read up to its terminator, and a damaged one (no
    /// terminator for 128 KiB) only up to TEXT_MAX
    #[test]
    fn template_captions_are_bounded() {
        let _g = crate::test_guest::enter();
        let t = mem::heap_alloc(0x30000, true);
        // DLGTEMPLATE (18 bytes), no menu, no class, the caption "Ab"
        for (i, u) in [0u16, 0, 0x41, 0x62, 0].iter().enumerate() {
            w16(t + 18 + 2 * i as u32, *u);
        }
        assert_eq!(template_title(t), vec![0x41, 0x62]);
        // a menu name with no end
        for i in 0..0x10000 {
            w16(t + 18 + 2 * i, 0x41);
        }
        let got = template_title(t);
        assert!(got.len() as u32 <= TEXT_MAX);
        mem::heap_free(t);
    }

    #[test]
    fn a_page_that_takes_it_stays() {
        assert_eq!(resolve(2, 1, 5, |_| 0, |_| None), 2);
    }

    #[test]
    fn skipping_pages_moves_on_and_stops_at_the_ends() {
        // pages 1 and 2 skip themselves
        assert_eq!(resolve(1, 1, 5, |i| if i == 1 || i == 2 { -1 } else { 0 }, |_| None), 3);
        assert_eq!(resolve(2, -1, 5, |i| if i == 1 || i == 2 { -1 } else { 0 }, |_| None), 0);
        // every page skips: the last one going forward, the first going back
        assert_eq!(resolve(0, 1, 4, |_| -1, |_| None), 3);
        assert_eq!(resolve(3, -1, 4, |_| -1, |_| None), 0);
    }

    #[test]
    fn a_resource_id_jumps() {
        // page 0 sends to the page with id 300 (index 3)
        let ids = [100, 200, 300, 400];
        assert_eq!(resolve(0, 1, 4, |i| if i == 0 { 400 } else { 0 }, |id| ids.iter().position(|&r| r == id)), 3);
        // an id no page has stays where it is
        assert_eq!(resolve(1, 1, 4, |i| if i == 1 { 999 } else { 0 }, |id| ids.iter().position(|&r| r == id)), 1);
    }

    #[test]
    fn pages_sending_each_other_back_and_forth_stop() {
        let ids = [10, 20];
        let mut asked = 0;
        let r = resolve(0, 1, 2, |i| {
            asked += 1;
            [20, 10][i]
        }, |id| ids.iter().position(|&r| r == id));
        assert!(r < 2);
        assert!(asked <= 4 * 2 + 4);
    }

    #[test]
    fn layouts() {
        // a sheet: the tab control around the page, the buttons under it
        let (area, size, by) = layout((300, 200), Some((4, 24, 4, 4)), (75, 23), (10, 10), 3);
        assert_eq!(area, (14, 34, 300, 200));
        assert_eq!(by, 10 + 228 + 10);
        assert_eq!(size, (328, by + 23 + 10));
        // a wizard: the page, a line, the buttons
        let (area, size, by) = layout((300, 200), None, (75, 23), (10, 10), 3);
        assert_eq!(area, (10, 10, 300, 200));
        assert_eq!(by, 10 + 200 + 10 + 2 + 10);
        assert_eq!(size.0, 320);
        // a small page still fits its buttons
        let (_, size, _) = layout((50, 50), Some((4, 24, 4, 4)), (75, 23), (10, 10), 4);
        assert!(size.0 >= 10 + 4 * (75 + 5));
    }

    proptest! {
        /// Wherever it starts and whatever the pages answer, SetCurSel ends on a page, asks a
        /// bounded number of times, and a page that takes it is where it ends.
        #[test]
        fn resolve_ends_on_a_page(n in 1usize..12, start in 0usize..16, fwd in any::<bool>(), answers in proptest::collection::vec(prop_oneof![Just(0i32), Just(-1), 1i32..20], 12)) {
            let ids: Vec<i32> = (0..n as i32).map(|i| i + 1).collect();
            let mut asked = Vec::new();
            let r = resolve(start, if fwd { 1 } else { -1 }, n, |i| { asked.push(i); answers[i] }, |id| ids.iter().position(|&x| x == id));
            prop_assert!(r < n);
            prop_assert!(asked.len() <= 4 * n + 4);
            prop_assert!(asked.iter().all(|&i| i < n));
            if answers[r] == 0 {
                // (a page that took it is the last one asked)
                prop_assert_eq!(asked.last().copied(), Some(r));
            }
        }

        /// Skipping only moves in its direction: with no ids, the pages asked go one way.
        #[test]
        fn skips_are_monotonic(n in 1usize..12, start in 0usize..12, fwd in any::<bool>(), skip in proptest::collection::vec(any::<bool>(), 12)) {
            let mut asked = Vec::new();
            let r = resolve(start, if fwd { 1 } else { -1 }, n, |i| { asked.push(i); if skip[i] { -1 } else { 0 } }, |_| None);
            for w in asked.windows(2) {
                prop_assert_eq!(w[1] as isize - w[0] as isize, if fwd { 1 } else { -1 });
            }
            let s = start.min(n - 1);
            // the first page from the start on that does not skip, else the end
            let want = if fwd { (s..n).find(|&i| !skip[i]).unwrap_or(n - 1) } else { (0..=s).rev().find(|&i| !skip[i]).unwrap_or(0) };
            prop_assert_eq!(r, want);
        }

        /// The page area always lies inside the client area, above the buttons.
        #[test]
        fn layout_contains_the_page(pw in 1i32..2000, ph in 1i32..2000, tab in any::<bool>(), m in 0i32..30, bw in 1i32..200, bh in 1i32..60, k in 1i32..6) {
            let margins = tab.then_some((2, 21, 4, 4));
            let ((x, y, w, h), (cw, ch), by) = layout((pw, ph), margins, (bw, bh), (m, m), k);
            prop_assert!(x >= 0 && y >= 0 && x + w <= cw && y + h <= by);
            prop_assert!(by + bh <= ch);
            prop_assert!(cw >= k * bw);
        }
    }
}
