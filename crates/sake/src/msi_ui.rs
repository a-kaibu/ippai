//! The internal UI's dialogs: the Dialog, Control, ControlEvent, ControlCondition,
//! EventMapping, TextStyle, CheckBox, RadioButton, ComboBox and ListBox tables shown as
//! windows (at INSTALLUILEVEL_FULL; at REDUCED only the modeless ones, at BASIC and below
//! none, as /qb shows no wizard).
//!
//! A modal dialog is a dialog box whose controls are made from its Control rows (installer
//! units, 1/12 of the dialog font's height: 4/3 pixel here); clicking a control publishes its
//! ControlEvent rows in order (EndDialog, NewDialog, SpawnDialog, DoAction, SetTargetPath,
//! [Property], AddLocal, Remove, AddSource, SetInstallLevel, Reset...), then ControlCondition
//! hides, shows, enables, disables or makes default controls and the controls showing
//! properties are refreshed. A modeless dialog (the progress dialog) stays up while the
//! sequence runs: ActionText, ActionData and SetProgress messages reach the controls
//! EventMapping subscribes. Bitmaps (Binary rows), lines and progress bars are painted by the
//! dialog itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use msidb::record::{Field, Record};

use crate::api::Ctx;
use crate::mem::{self, r32, w16, w32};
use crate::msi_pkg::{cond_true, query, Package};

const ERROR_SUCCESS: u32 = 0;
const ERROR_INSTALL_USEREXIT: u32 = 1602;
const ERROR_INSTALL_FAILURE: u32 = 1603;

/// msidbDialogAttributes
const DLG_VISIBLE: i32 = 1;
const DLG_MODAL: i32 = 2;
/// msidbControlAttributes
const CTL_VISIBLE: i32 = 1;
const CTL_ENABLED: i32 = 2;
const CTL_SUNKEN: i32 = 4;
const CTL_INDIRECT: i32 = 8;
const CTL_RIGHT: i32 = 64;
const CTL_TRANSPARENT: i32 = 0x10000;
const CTL_NOPREFIX: i32 = 0x20000;
const CTL_MULTILINE: i32 = 0x10000;
const CTL_PASSWORD: i32 = 0x200000;
const CTL_HASBORDER: i32 = 0x1000000;

/// EndDialog codes: Return, Exit, Retry, Ignore; NewDialog.
const END_RETURN: u32 = 1;
const END_EXIT: u32 = 2;
const END_RETRY: u32 = 3;
const END_IGNORE: u32 = 4;
const END_NEW: u32 = 100;

/// The first control id (Control_Cancel takes IDCANCEL so that Esc presses it).
const FIRST_ID: u32 = 100;

#[derive(Clone, Default)]
struct Ctl {
    name: String,
    kind: String,
    hwnd: u32,
    id: u32,
    rect: (i32, i32, i32, i32),
    attrs: i32,
    property: String,
    text: String,
    /// (radio buttons: each button's value; check box: the value when checked)
    value: String,
    /// a radio button of the group `group` (the group's control index)
    group: Option<usize>,
    hidden: bool,
}

#[derive(Default)]
struct Dlg {
    pkg: Option<Arc<Mutex<Package>>>,
    name: String,
    hwnd: u32,
    ctls: Vec<Ctl>,
    /// NewDialog's target
    next: Option<String>,
    /// painted by the dialog: (rect, HBITMAP or 0 for a line / progress bar, kind)
    paints: Vec<((i32, i32, i32, i32), u32, &'static str)>,
    /// progress: (done, total)
    progress: (u64, u64),
    /// EventMapping: (event, control index, attribute)
    subs: Vec<(String, usize, String)>,
    /// (programmatic changes: their notifications are not the user's)
    busy: bool,
    fonts: HashMap<String, u32>,
}

static DLGS: Mutex<Option<HashMap<u32, Dlg>>> = Mutex::new(None);
static NEXT_KEY: Mutex<u32> = Mutex::new(1);
/// The modeless dialog up (its key).
static MODELESS: Mutex<u32> = Mutex::new(0);

fn with<R>(key: u32, f: impl FnOnce(&mut Dlg) -> R) -> Option<R> {
    let mut g = DLGS.lock().unwrap();
    g.get_or_insert_with(HashMap::new).get_mut(&key).map(f)
}

fn key_of(hwnd: u32) -> Option<u32> {
    let g = DLGS.lock().unwrap();
    g.as_ref()?.iter().find(|(_, d)| d.hwnd == hwnd).map(|(k, _)| *k)
}

fn g(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let f = crate::api::export(dll, name);
    c.call(f, args)
}

/// A guest UTF-16 copy of `s` (freed by the caller).
fn wstr(s: &str) -> u32 {
    let v: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let p = mem::heap_alloc(2 * v.len() as u32, true);
    for (i, &u) in v.iter().enumerate() {
        w16(p + 2 * i as u32, u);
    }
    p
}

fn set_text(c: &mut Ctx, h: u32, s: &str) {
    let p = wstr(s);
    g(c, "user32", "SetWindowTextW", &[h, p]);
    mem::heap_free(p);
}

fn get_text(c: &mut Ctx, h: u32) -> String {
    let n = g(c, "user32", "GetWindowTextLengthW", &[h]);
    let p = mem::heap_alloc(2 * n + 2, true);
    g(c, "user32", "GetWindowTextW", &[h, p, n + 1]);
    let s = crate::k32::arg_str(p, true);
    mem::heap_free(p);
    s
}

fn send_msg(c: &mut Ctx, h: u32, m: u32, wp: u32, lp: u32) -> u32 {
    g(c, "user32", "SendMessageW", &[h, m, wp, lp])
}

/// Installer units to pixels.
fn px(u: i32) -> i32 {
    (u * 4 + 1) / 3
}

fn q(s: &str) -> String {
    s.replace('\'', "")
}

/// A control's text: its "{\Style}" / "{&Style}" prefixes (the style of the last) and the
/// text formatted.
fn styled(pk: &Package, raw: &str) -> (Option<String>, String) {
    let mut s = raw;
    let mut style = None;
    while s.starts_with("{\\") || s.starts_with("{&") {
        let Some(end) = s.find('}') else { break };
        style = Some(s[2..end].to_string());
        s = &s[end + 1..];
    }
    (style, pk.format(s))
}

/// A TextStyle's font (made once per dialog): face, points, bold/italic/underline/strike.
fn font(c: &mut Ctx, key: u32, pk: &Package, style: &str) -> u32 {
    if let Some(f) = with(key, |d| d.fonts.get(style).copied()).flatten() {
        return f;
    }
    let row = query(&pk.db, &format!("SELECT `FaceName`, `Size`, `StyleBits` FROM `TextStyle` WHERE `TextStyle` = '{}'", q(style))).into_iter().next();
    let (face, size, bits) = match &row {
        Some(r) => (r.get(1).text(), r.get(2).int().max(1), match r.get(3) {
            Field::Int(v) if *v != msidb::NULL_INT => *v,
            _ => 0,
        }),
        None => ("Tahoma".to_string(), 8, 0),
    };
    let fp = wstr(&face);
    let h = -((size * 96 + 36) / 72);
    let f = g(c, "gdi32", "CreateFontW", &[h as u32, 0, 0, 0, if bits & 1 != 0 { 700 } else { 400 }, (bits & 2 != 0) as u32, (bits & 4 != 0) as u32, (bits & 8 != 0) as u32, 1, 0, 0, 0, 0, fp]);
    mem::heap_free(fp);
    with(key, |d| d.fonts.insert(style.to_string(), f));
    f
}

/// Plain text of RTF (ScrollableText): groups of tables and destinations dropped, \par and
/// \line as line breaks, \'hh as bytes of the document's code page, \uN as characters.
pub fn rtf_text(rtf: &str) -> String {
    let b = rtf.as_bytes();
    let mut out: Vec<u16> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut cp = 1252u32;
    // per group: skipped
    let mut skip = vec![false];
    let mut uc_skip = 0usize;
    let mut i = 0;
    let flush = |bytes: &mut Vec<u8>, out: &mut Vec<u16>, cp: u32| {
        if !bytes.is_empty() {
            out.extend(crate::text::mb2w(cp, bytes));
            bytes.clear();
        }
    };
    while i < b.len() {
        let ch = b[i];
        let skipping = *skip.last().unwrap();
        match ch {
            b'{' => {
                skip.push(skipping);
                i += 1;
            }
            b'}' => {
                if skip.len() > 1 {
                    skip.pop();
                }
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&n) = b.get(i) else { break };
                if n == b'\'' {
                    let h = std::str::from_utf8(b.get(i + 1..i + 3).unwrap_or(b"")).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                    i += 3;
                    if let (Some(h), false) = (h, skipping) {
                        if uc_skip > 0 {
                            uc_skip -= 1;
                        } else {
                            bytes.push(h);
                        }
                    }
                    continue;
                }
                if !n.is_ascii_alphabetic() {
                    // \\ \{ \} and other symbols; \* marks a destination to skip
                    if n == b'*' {
                        *skip.last_mut().unwrap() = true;
                    } else if !skipping && matches!(n, b'\\' | b'{' | b'}') {
                        bytes.push(n);
                    } else if !skipping && n == b'~' {
                        bytes.push(b' ');
                    }
                    i += 1;
                    continue;
                }
                let start = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = &rtf[start..i];
                let nstart = i;
                if i < b.len() && (b[i] == b'-' || b[i].is_ascii_digit()) {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let num: Option<i32> = rtf[nstart..i].parse().ok();
                if i < b.len() && b[i] == b' ' {
                    i += 1;
                }
                match word {
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "header" | "footer" | "listtable" | "listoverridetable" | "rsidtbl" | "generator" | "xmlnstbl" | "themedata" | "datastore" | "latentstyles" => {
                        *skip.last_mut().unwrap() = true;
                    }
                    "ansicpg" => cp = num.unwrap_or(1252) as u32,
                    "par" | "line" | "sect" | "page" if !skipping => {
                        flush(&mut bytes, &mut out, cp);
                        out.extend([13, 10]);
                    }
                    "tab" if !skipping => bytes.push(b'\t'),
                    "u" if !skipping => {
                        flush(&mut bytes, &mut out, cp);
                        out.push(num.unwrap_or(0) as i16 as u16);
                        uc_skip = 1;
                    }
                    _ => {}
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                if !skipping {
                    if uc_skip > 0 {
                        uc_skip -= 1;
                    } else {
                        bytes.push(ch);
                    }
                }
                i += 1;
            }
        }
    }
    flush(&mut bytes, &mut out, cp);
    String::from_utf16_lossy(&out).trim_end().to_string()
}

/// A Binary row as an HBITMAP (BMP, JPEG, PNG; an icon's largest image).
fn binary_bitmap(pk: &Package, name: &str) -> u32 {
    let rows = query(&pk.db, &format!("SELECT `Data` FROM `Binary` WHERE `Name` = '{}'", q(name)));
    let Some(r) = rows.first() else { return 0 };
    let Field::Stream(data) = r.get(1) else { return 0 };
    let Some((w, h, px)) = crate::picture::decode_bgra(data) else { return 0 };
    crate::gdi::dib_section_bgra(w, h, &px)
}

/// The value a control shows for its property.
fn prop_value(pk: &Package, ctl: &Ctl) -> String {
    let mut p = ctl.property.clone();
    if ctl.attrs & CTL_INDIRECT != 0 {
        p = pk.prop(&p);
    }
    pk.prop(&p)
}

/// Builds the dialog's state and its controls (WM_INITDIALOG).
fn init(c: &mut Ctx, key: u32, hwnd: u32) {
    let Some((pkg, name)) = with(key, |d| {
        d.hwnd = hwnd;
        (d.pkg.clone().unwrap(), d.name.clone())
    }) else { return };
    let (dlg_row, rows, default_style) = {
        let pk = pkg.lock().unwrap();
        let d = query(&pk.db, &format!("SELECT `HCentering`, `VCentering`, `Width`, `Height`, `Attributes`, `Title`, `Control_First`, `Control_Default`, `Control_Cancel` FROM `Dialog` WHERE `Dialog` = '{}'", q(&name))).into_iter().next();
        let rows = query(&pk.db, &format!("SELECT `Control`, `Type`, `X`, `Y`, `Width`, `Height`, `Attributes`, `Property`, `Text` FROM `Control` WHERE `Dialog_` = '{}'", q(&name)));
        (d, rows, pk.prop("DefaultUIFont"))
    };
    let Some(d) = dlg_row else { return };
    let (cw, ch) = (px(d.get(3).int()), px(d.get(4).int()));
    let (default, cancel) = (d.get(8).text(), d.get(9).text());
    let title = pkg.lock().unwrap().format(&d.get(6).text());
    set_text(c, hwnd, &title);
    // the size and place: the client area Width x Height, centred by H/VCentering (percent)
    let rc = mem::heap_alloc(16, true);
    w32(rc + 8, cw as u32);
    w32(rc + 12, ch as u32);
    let style = g(c, "user32", "GetWindowLongW", &[hwnd, (-16i32) as u32]);
    g(c, "user32", "AdjustWindowRectEx", &[rc, style, 0, 0]);
    let (ww, wh) = (r32(rc + 8) as i32 - r32(rc) as i32, r32(rc + 12) as i32 - r32(rc + 4) as i32);
    mem::heap_free(rc);
    let (sw, sh) = (g(c, "user32", "GetSystemMetrics", &[0]) as i32, g(c, "user32", "GetSystemMetrics", &[1]) as i32);
    let pct = |v: i32| v.clamp(0, 100);
    let (x, y) = ((sw - ww) * pct(d.get(1).int()) / 100, (sh - wh) * pct(d.get(2).int()) / 100);
    g(c, "user32", "SetWindowPos", &[hwnd, 0, x.max(0) as u32, y.max(0) as u32, ww as u32, wh as u32, 0x14]);
    let dfont = {
        let pk = pkg.lock().unwrap();
        font(c, key, &pk, &default_style)
    };
    let mut ctls = Vec::new();
    let mut paints = Vec::new();
    let mut next_id = FIRST_ID;
    for r in rows {
        let (cname, kind) = (r.get(1).text(), r.get(2).text());
        let rect = (px(r.get(3).int()), px(r.get(4).int()), px(r.get(5).int()), px(r.get(6).int()));
        let attrs = match r.get(7) {
            Field::Int(v) if *v != msidb::NULL_INT => *v,
            _ => 0,
        };
        let (property, text) = (r.get(8).text(), r.get(9).text());
        let id = if cname == cancel {
            2
        } else {
            next_id += 1;
            next_id - 1
        };
        let base = Ctl { name: cname.clone(), kind: kind.clone(), id, rect, attrs, property: property.clone(), text: text.clone(), ..Ctl::default() };
        let (cls, st, label): (&str, u32, String) = {
            let pk = pkg.lock().unwrap();
            let (_, t) = styled(&pk, &text);
            match kind.as_str() {
                "Text" => ("Static", if attrs & CTL_RIGHT != 0 { 2 } else { 0 } | if attrs & CTL_NOPREFIX != 0 { 0x80 } else { 0 } | if attrs & CTL_SUNKEN != 0 { 0x1000 } else { 0 }, if attrs & CTL_INDIRECT != 0 || text.is_empty() && !property.is_empty() { prop_value(&pk, &base) } else { t }),
                "PushButton" => ("Button", if cname == default { 1 } else { 0 } | 0x0001_0000, t),
                "CheckBox" => ("Button", 3 | 0x0001_0000, t),
                "GroupBox" => ("Button", 7, t),
                "Edit" | "PathEdit" | "MaskedEdit" => ("Edit", 0x80 | 0x0080_0000 | 0x0001_0000 | if attrs & CTL_MULTILINE != 0 && kind == "Edit" { 4 | 0x1000 } else { 0 } | if attrs & CTL_PASSWORD != 0 { 0x20 } else { 0 }, prop_value(&pk, &base)),
                "ScrollableText" => ("Edit", 4 | 0x800 | 0x0020_0000 | 0x0080_0000 | 0x0001_0000, rtf_text(&text)),
                "ComboBox" => ("ComboBox", 3 | 0x0020_0000 | 0x0001_0000, String::new()),
                "ListBox" => ("ListBox", 1 | 0x0080_0000 | 0x0020_0000 | 0x0001_0000, String::new()),
                "ProgressBar" | "Line" | "Bitmap" | "Icon" => ("", 0, String::new()),
                "RadioButtonGroup" => (if attrs & CTL_HASBORDER != 0 { "Button" } else { "" }, 7, String::new()),
                // (DirectoryCombo, DirectoryList, SelectionTree, VolumeCostList, ListView,
                // Billboard: their property's value as text)
                _ => ("Static", 0, prop_value(&pk, &base)),
            }
        };
        let mut ctl = base;
        match kind.as_str() {
            "Bitmap" | "Icon" => {
                let pk = pkg.lock().unwrap();
                let bm = binary_bitmap(&pk, &pk.format(&text));
                paints.push((rect, bm, "bitmap"));
            }
            "Line" => paints.push((rect, 0, "line")),
            "ProgressBar" => paints.push((rect, 0, "progress")),
            _ => {}
        }
        if !cls.is_empty() {
            let visible = if attrs & CTL_VISIBLE != 0 { 0x1000_0000 } else { 0 };
            let disabled = if attrs & CTL_ENABLED == 0 && kind != "Text" && kind != "Line" { 0x0800_0000 } else { 0 };
            let cp = wstr(cls);
            let tp = wstr(&label);
            let h = g(c, "user32", "CreateWindowExW", &[if attrs & CTL_TRANSPARENT != 0 { 0x20 } else { 0 }, cp, tp, 0x4000_0000 | visible | disabled | st, rect.0 as u32, rect.1 as u32, rect.2 as u32, rect.3 as u32, hwnd, id, 0, 0]);
            mem::heap_free(cp);
            mem::heap_free(tp);
            let style_font = {
                let pk = pkg.lock().unwrap();
                styled(&pk, &text).0.map(|s| font(c, key, &pk, &s))
            };
            send_msg(c, h, 0x30, style_font.unwrap_or(dfont), 0);
            ctl.hwnd = h;
        }
        ctl.hidden = attrs & CTL_VISIBLE == 0;
        ctls.push(ctl);
    }
    // radio buttons, check boxes, lists: their tables
    let n = ctls.len();
    for i in 0..n {
        let ctl = ctls[i].clone();
        let pk = pkg.lock().unwrap();
        match ctl.kind.as_str() {
            "CheckBox" => {
                let v = query(&pk.db, &format!("SELECT `Value` FROM `CheckBox` WHERE `Property` = '{}'", q(&ctl.property))).first().map(|r| r.get(1).text()).unwrap_or_else(|| "1".into());
                ctls[i].value = v;
                let on = !pk.prop(&ctl.property).is_empty();
                drop(pk);
                send_msg(c, ctl.hwnd, 0xf1, on as u32, 0);
            }
            "RadioButtonGroup" => {
                let rows = query(&pk.db, &format!("SELECT `Value`, `X`, `Y`, `Width`, `Height`, `Text` FROM `RadioButton` WHERE `Property` = '{}' ORDER BY `Order`", q(&ctl.property)));
                let cur = pk.prop(&ctl.property);
                let items: Vec<(String, (i32, i32, i32, i32), String)> = rows.iter().map(|r| (r.get(1).text(), (ctl.rect.0 + px(r.get(2).int()), ctl.rect.1 + px(r.get(3).int()), px(r.get(4).int()), px(r.get(5).int())), styled(&pk, &r.get(6).text()).1)).collect();
                drop(pk);
                for (k, (value, rect, label)) in items.into_iter().enumerate() {
                    let id = next_id;
                    next_id += 1;
                    let cp = wstr("Button");
                    let tp = wstr(&label);
                    let visible = if ctl.attrs & CTL_VISIBLE != 0 { 0x1000_0000 } else { 0 };
                    let h = g(c, "user32", "CreateWindowExW", &[0, cp, tp, 0x4000_0000 | visible | 9 | if k == 0 { 0x0002_0000 | 0x0001_0000 } else { 0 }, rect.0 as u32, rect.1 as u32, rect.2 as u32, rect.3 as u32, hwnd, id, 0, 0]);
                    mem::heap_free(cp);
                    mem::heap_free(tp);
                    send_msg(c, h, 0x30, dfont, 0);
                    send_msg(c, h, 0xf1, (value == cur) as u32, 0);
                    ctls.push(Ctl { name: format!("{}#{k}", ctl.name), kind: "RadioButton".into(), hwnd: h, id, rect, attrs: ctl.attrs, property: ctl.property.clone(), value, group: Some(i), ..Ctl::default() });
                }
            }
            "ComboBox" | "ListBox" => {
                let rows = query(&pk.db, &format!("SELECT `Value`, `Text` FROM `{}` WHERE `Property` = '{}' ORDER BY `Order`", ctl.kind, q(&ctl.property)));
                let cur = pk.prop(&ctl.property);
                let items: Vec<(String, String)> = rows.iter().map(|r| (r.get(1).text(), { let t = r.get(2).text(); if t.is_empty() { r.get(1).text() } else { t } })).collect();
                drop(pk);
                let (add, sel) = if ctl.kind == "ComboBox" { (0x143, 0x14e) } else { (0x180, 0x186) };
                for (k, (v, t)) in items.iter().enumerate() {
                    let tp = wstr(t);
                    send_msg(c, ctl.hwnd, add, 0, tp);
                    mem::heap_free(tp);
                    if *v == cur {
                        send_msg(c, ctl.hwnd, sel, k as u32, 0);
                    }
                }
                ctls[i].value = items.iter().map(|x| x.0.clone()).collect::<Vec<_>>().join("\u{1}");
            }
            _ => {}
        }
    }
    let subs: Vec<(String, usize, String)> = {
        let pk = pkg.lock().unwrap();
        query(&pk.db, &format!("SELECT `Control_`, `Event`, `Attribute` FROM `EventMapping` WHERE `Dialog_` = '{}'", q(&name)))
            .iter()
            .filter_map(|r| Some((r.get(2).text(), ctls.iter().position(|x| x.name == r.get(1).text())?, r.get(3).text())))
            .collect()
    };
    with(key, |d| {
        d.ctls = ctls;
        d.paints = paints;
        d.subs = subs;
    });
    refresh(c, key);
    // the first control has the focus
    let first = d.get(7).text();
    if let Some(h) = with(key, |d| d.ctls.iter().find(|x| x.name == first).map(|x| x.hwnd)).flatten() {
        if h != 0 {
            g(c, "user32", "SetFocus", &[h]);
        }
    }
}

/// ControlCondition (Hide, Show, Disable, Enable, Default) and the controls showing
/// properties, after the properties may have changed.
fn refresh(c: &mut Ctx, key: u32) {
    let Some((pkg, name, ctls)) = with(key, |d| {
        d.busy = true;
        (d.pkg.clone().unwrap(), d.name.clone(), d.ctls.clone())
    }) else { return };
    let conds: Vec<(String, String)> = {
        let pk = pkg.lock().unwrap();
        query(&pk.db, &format!("SELECT `Control_`, `Action`, `Condition` FROM `ControlCondition` WHERE `Dialog_` = '{}'", q(&name)))
            .iter()
            .filter(|r| cond_true(&pk, &r.get(3).text()))
            .map(|r| (r.get(1).text(), r.get(2).text()))
            .collect()
    };
    for (cn, action) in conds {
        let hs: Vec<(usize, u32)> = ctls.iter().enumerate().filter(|(_, x)| x.name == cn || x.group.is_some_and(|gi| ctls[gi].name == cn)).map(|(i, x)| (i, x.hwnd)).collect();
        for (i, h) in hs {
            match action.as_str() {
                "Hide" | "Show" => {
                    let show = action == "Show";
                    if h != 0 {
                        g(c, "user32", "ShowWindow", &[h, if show { 5 } else { 0 }]);
                    }
                    with(key, |d| d.ctls[i].hidden = !show);
                }
                "Disable" | "Enable" if h != 0 => {
                    g(c, "user32", "EnableWindow", &[h, (action == "Enable") as u32]);
                }
                "Default" if h != 0 => {
                    send_msg(c, h, 0xf4, 1, 1);
                }
                _ => {}
            }
        }
    }
    for x in &ctls {
        if x.hwnd == 0 {
            continue;
        }
        let pk = pkg.lock().unwrap();
        let v = match x.kind.as_str() {
            "Text" if x.attrs & CTL_INDIRECT != 0 || x.text.is_empty() && !x.property.is_empty() => Some(prop_value(&pk, x)),
            "Text" if x.text.contains('[') => Some(styled(&pk, &x.text).1),
            "PathEdit" => Some(prop_value(&pk, x)),
            _ => None,
        };
        drop(pk);
        if let Some(v) = v {
            if get_text(c, x.hwnd) != v {
                set_text(c, x.hwnd, &v);
            }
        }
    }
    with(key, |d| d.busy = false);
    let h = with(key, |d| d.hwnd).unwrap_or(0);
    if h != 0 {
        g(c, "user32", "InvalidateRect", &[h, 0, 1]);
    }
}

/// A control's ControlEvent rows, in order; then the conditions again.
fn publish(c: &mut Ctx, key: u32, ctl: &str) {
    let Some((pkg, name, hwnd)) = with(key, |d| (d.pkg.clone().unwrap(), d.name.clone(), d.hwnd)) else { return };
    let events: Vec<(String, String)> = {
        let pk = pkg.lock().unwrap();
        query(&pk.db, &format!("SELECT `Event`, `Argument`, `Condition` FROM `ControlEvent` WHERE `Dialog_` = '{}' AND `Control_` = '{}' ORDER BY `Ordering`", q(&name), q(ctl)))
            .iter()
            .filter(|r| {
                let cnd = r.get(3).text();
                cnd.is_empty() || cond_true(&pk, &cnd)
            })
            .map(|r| (r.get(1).text(), r.get(2).text()))
            .collect()
    };
    let mut end: Option<u32> = None;
    for (ev, arg) in events {
        if crate::env_on!("SAKE_TRACE_MSI") {
            eprintln!("msi: {name}.{ctl}: {ev} {arg}");
        }
        let farg = pkg.lock().unwrap().format(&arg);
        match ev.as_str() {
            "EndDialog" => {
                end = Some(match arg.as_str() {
                    "Exit" => END_EXIT,
                    "Retry" => END_RETRY,
                    "Ignore" => END_IGNORE,
                    _ => END_RETURN,
                });
            }
            "NewDialog" => {
                with(key, |d| d.next = Some(farg.clone()));
                end = Some(END_NEW);
            }
            "SpawnDialog" | "SpawnWaitDialog" | "SelectionBrowse" | "DirectoryListNew" if ev != "DirectoryListNew" => {
                let r = run_modal(c, &pkg, &farg, hwnd);
                if r == ERROR_INSTALL_USEREXIT {
                    end = Some(END_EXIT);
                }
            }
            "DoAction" => {
                let r = crate::msi_install::do_action(c, &pkg, &farg, None);
                if r == ERROR_INSTALL_USEREXIT {
                    end = Some(END_EXIT);
                }
            }
            "SetTargetPath" | "CheckTargetPath" | "CheckExistingTargetPath" => {
                if ev == "SetTargetPath" {
                    let mut pk = pkg.lock().unwrap();
                    let v = pk.prop(&farg);
                    if !v.is_empty() && pk.costed {
                        pk.set_target(&farg, &v);
                    }
                }
            }
            "SetInstallLevel" => {
                let mut pk = pkg.lock().unwrap();
                pk.set_prop("INSTALLLEVEL", &farg);
                if pk.costed {
                    pk.plan_states();
                }
            }
            "AddLocal" | "AddSource" | "Remove" | "Reinstall" => {
                let st = match ev.as_str() {
                    "AddLocal" => crate::msi_pkg::INSTALLSTATE_LOCAL,
                    "AddSource" => crate::msi_pkg::INSTALLSTATE_SOURCE,
                    "Remove" => crate::msi_pkg::INSTALLSTATE_ABSENT,
                    _ => crate::msi_pkg::INSTALLSTATE_LOCAL,
                };
                let mut pk = pkg.lock().unwrap();
                if pk.cost_started {
                    let names: Vec<String> = if farg == "ALL" { pk.features.iter().map(|f| f.name.clone()).collect() } else { vec![farg.clone()] };
                    for n in names {
                        pk.set_feature_state(&n, st, 0);
                    }
                }
            }
            "Reset" => {}
            e if e.starts_with('[') && e.ends_with(']') => {
                let prop = pkg.lock().unwrap().format(&e[1..e.len() - 1]);
                let v = if arg == "{}" { String::new() } else { farg };
                pkg.lock().unwrap().set_prop(&prop, &v);
            }
            _ => {}
        }
    }
    if let Some(code) = end {
        g(c, "user32", "EndDialog", &[hwnd, code]);
        return;
    }
    refresh(c, key);
}

/// A control's input taken into its property.
fn take_input(c: &mut Ctx, key: u32, i: usize, code: u32) {
    let Some((pkg, ctl, busy, all)) = with(key, |d| (d.pkg.clone().unwrap(), d.ctls[i].clone(), d.busy, d.ctls.clone())) else { return };
    if busy || ctl.property.is_empty() {
        return;
    }
    let v = match ctl.kind.as_str() {
        "Edit" | "PathEdit" | "MaskedEdit" if code == 0x300 => Some(get_text(c, ctl.hwnd)),
        "CheckBox" if code == 0 => Some(if send_msg(c, ctl.hwnd, 0xf0, 0, 0) == 1 { ctl.value.clone() } else { String::new() }),
        "RadioButton" if code == 0 => Some(ctl.value.clone()),
        "ComboBox" | "ListBox" if code == 1 => {
            let sel = send_msg(c, ctl.hwnd, if ctl.kind == "ComboBox" { 0x147 } else { 0x188 }, 0, 0);
            ctl.value.split('\u{1}').nth(sel as usize).map(str::to_string)
        }
        _ => None,
    };
    let Some(v) = v else { return };
    let mut prop = ctl.property.clone();
    {
        let mut pk = pkg.lock().unwrap();
        if ctl.attrs & CTL_INDIRECT != 0 {
            prop = pk.prop(&prop);
        }
        pk.set_prop(&prop, &v);
    }
    // (a radio button: the group's events are its)
    let pubname = ctl.group.map_or(ctl.name.clone(), |gi| all[gi].name.clone());
    if matches!(ctl.kind.as_str(), "CheckBox" | "RadioButton" | "ComboBox" | "ListBox") {
        publish(c, key, &pubname);
    } else {
        refresh(c, key);
    }
}

fn paint(c: &mut Ctx, key: u32, hwnd: u32) {
    let Some((paints, progress)) = with(key, |d| (d.paints.clone(), d.progress)) else { return };
    let ps = mem::heap_alloc(64, true);
    let dc = g(c, "user32", "BeginPaint", &[hwnd, ps]);
    let rc = mem::heap_alloc(16, true);
    g(c, "user32", "GetClientRect", &[hwnd, rc]);
    let brush = crate::gdi::solid_brush(0xf0f0f0);
    g(c, "user32", "FillRect", &[dc, rc, brush]);
    let fill = |c: &mut Ctx, (x, y, w, h): (i32, i32, i32, i32), color: u32| {
        w32(rc, x as u32);
        w32(rc + 4, y as u32);
        w32(rc + 8, (x + w) as u32);
        w32(rc + 12, (y + h) as u32);
        g(c, "user32", "FillRect", &[dc, rc, crate::gdi::solid_brush(color)]);
    };
    for (r, bm, kind) in paints {
        match kind {
            "bitmap" if bm != 0 => {
                let mdc = g(c, "gdi32", "CreateCompatibleDC", &[dc]);
                let old = g(c, "gdi32", "SelectObject", &[mdc, bm]);
                let info = mem::heap_alloc(32, true);
                g(c, "gdi32", "GetObjectW", &[bm, 24, info]);
                let (bw, bh) = (r32(info + 4), r32(info + 8));
                mem::heap_free(info);
                g(c, "gdi32", "SetStretchBltMode", &[dc, 4]);
                g(c, "gdi32", "StretchBlt", &[dc, r.0 as u32, r.1 as u32, r.2 as u32, r.3 as u32, mdc, 0, 0, bw, bh, 0x00cc_0020]);
                g(c, "gdi32", "SelectObject", &[mdc, old]);
                g(c, "gdi32", "DeleteDC", &[mdc]);
            }
            "line" => {
                fill(c, (r.0, r.1, r.2, 1), 0xa0a0a0);
                fill(c, (r.0, r.1 + 1, r.2, 1), 0xffffff);
            }
            "progress" => {
                fill(c, r, 0xbcbcbc);
                fill(c, (r.0 + 1, r.1 + 1, r.2 - 2, r.3 - 2), 0xe6e6e6);
                let (done, total) = progress;
                let w = if total == 0 { 0 } else { ((r.2 - 2) as u64 * done.min(total) / total) as i32 };
                fill(c, (r.0 + 1, r.1 + 1, w, r.3 - 2), 0x25b006);
            }
            _ => {}
        }
    }
    mem::heap_free(rc);
    g(c, "user32", "EndPaint", &[hwnd, ps]);
    mem::heap_free(ps);
}

/// The dialogs' procedure.
fn dlg_proc(c: &mut Ctx) -> u32 {
    let (hwnd, msg, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    match msg {
        0x110 => {
            init(c, lp, hwnd);
            0
        }
        0x111 => {
            let Some(key) = key_of(hwnd) else { return 0 };
            let (id, code) = (wp & 0xffff, wp >> 16);
            let Some((i, ctl)) = with(key, |d| d.ctls.iter().position(|x| x.id == id).map(|i| (i, d.ctls[i].clone()))).flatten() else { return 0 };
            match ctl.kind.as_str() {
                "PushButton" if code == 0 => publish(c, key, &ctl.name),
                _ => take_input(c, key, i, code),
            }
            1
        }
        0xf => {
            let Some(key) = key_of(hwnd) else { return 0 };
            paint(c, key, hwnd);
            1
        }
        // transparent statics over the bitmaps
        0x138 => {
            let Some(key) = key_of(hwnd) else { return 0 };
            let transparent = with(key, |d| d.ctls.iter().any(|x| x.hwnd == lp && x.attrs & CTL_TRANSPARENT != 0)).unwrap_or(false);
            if transparent {
                g(c, "gdi32", "SetBkMode", &[wp, 1]);
                return g(c, "gdi32", "GetStockObject", &[5]);
            }
            0
        }
        // the close box: the cancel button's events
        0x10 => {
            let Some(key) = key_of(hwnd) else { return 0 };
            if let Some(n) = with(key, |d| d.ctls.iter().find(|x| x.id == 2).map(|x| x.name.clone())).flatten() {
                publish(c, key, &n);
            }
            1
        }
        _ => 0,
    }
}

fn proc_addr() -> u32 {
    static P: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("msi_dlg_proc", dlg_proc, 16))
}

/// The dialog's template: a popup with a caption and no controls (they are made at
/// WM_INITDIALOG), visible at once when modeless.
fn template(modeless: bool) -> u32 {
    let t = mem::heap_alloc(32, true);
    let style = 0x8000_0000u32 | 0x00c0_0000 | 0x0008_0000 | 0x80 | if modeless { 0x1000_0000 } else { 0 };
    w32(t, style);
    w16(t + 10, 0);
    w16(t + 14, 200);
    w16(t + 16, 150);
    t
}

fn new_dlg(pkg: &Arc<Mutex<Package>>, name: &str) -> u32 {
    let mut k = NEXT_KEY.lock().unwrap();
    let key = *k;
    *k += 1;
    DLGS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, Dlg { pkg: Some(pkg.clone()), name: name.to_string(), ..Dlg::default() });
    key
}

fn drop_dlg(c: &mut Ctx, key: u32) {
    let d = DLGS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&key);
    if let Some(d) = d {
        for f in d.fonts.values().chain(d.paints.iter().map(|p| &p.1)).filter(|&&h| h != 0) {
            g(c, "gdi32", "DeleteObject", &[*f]);
        }
    }
}

/// Closes the modeless dialog that is up, if any.
pub fn close_modeless(c: &mut Ctx) {
    let key = std::mem::take(&mut *MODELESS.lock().unwrap());
    if key == 0 {
        return;
    }
    if let Some(h) = with(key, |d| d.hwnd) {
        if h != 0 {
            g(c, "user32", "DestroyWindow", &[h]);
        }
    }
    drop_dlg(c, key);
}

/// A modal dialog run to its end: ERROR_SUCCESS for Return (and Retry / Ignore, as the
/// sequence goes on), ERROR_INSTALL_USEREXIT for Exit; NewDialog runs the next in its place.
fn run_modal(c: &mut Ctx, pkg: &Arc<Mutex<Package>>, name: &str, parent: u32) -> u32 {
    let mut name = name.to_string();
    loop {
        let exists = !query(&pkg.lock().unwrap().db, &format!("SELECT `Dialog` FROM `Dialog` WHERE `Dialog` = '{}'", q(&name))).is_empty();
        if !exists {
            eprintln!("msi: no dialog {name}");
            return ERROR_INSTALL_FAILURE;
        }
        let key = new_dlg(pkg, &name);
        let t = template(false);
        let r = g(c, "user32", "DialogBoxIndirectParamW", &[0, t, parent, proc_addr(), key]);
        mem::heap_free(t);
        let next = with(key, |d| d.next.take()).flatten();
        drop_dlg(c, key);
        match r {
            END_NEW => match next {
                Some(n) => name = n,
                None => return ERROR_SUCCESS,
            },
            END_EXIT => return ERROR_INSTALL_USEREXIT,
            u32::MAX => return ERROR_INSTALL_FAILURE,
            _ => return ERROR_SUCCESS,
        }
    }
}

/// A dialog an action names (a sequence's or MsiDoAction's).
pub fn dialog(c: &mut Ctx, p: &Arc<Mutex<Package>>, name: &str) -> u32 {
    let level = p.lock().unwrap().ui_level & 0x1f;
    let attrs = query(&p.lock().unwrap().db, &format!("SELECT `Attributes` FROM `Dialog` WHERE `Dialog` = '{}'", q(name))).first().map(|r| r.get(1).int()).unwrap_or(DLG_VISIBLE | DLG_MODAL);
    let modal = attrs & DLG_MODAL != 0;
    let shown = match level {
        crate::msi_install::INSTALLUILEVEL_FULL => true,
        crate::msi_install::INSTALLUILEVEL_REDUCED => !modal,
        _ => false,
    };
    if crate::env_on!("SAKE_TRACE_MSI") {
        eprintln!("msi: dialog {name}{}", if shown { "" } else { " (not shown)" });
    }
    if !shown {
        return ERROR_SUCCESS;
    }
    close_modeless(c);
    if !modal {
        let key = new_dlg(p, name);
        let t = template(true);
        g(c, "user32", "CreateDialogIndirectParamW", &[0, t, 0, proc_addr(), key]);
        mem::heap_free(t);
        *MODELESS.lock().unwrap() = key;
        pump(c);
        return ERROR_SUCCESS;
    }
    run_modal(c, p, name, 0)
}

/// The messages waiting for the modeless dialog (painting, its buttons).
fn pump(c: &mut Ctx) {
    let m = mem::heap_alloc(48, true);
    for _ in 0..64 {
        if g(c, "user32", "PeekMessageW", &[m, 0, 0, 0, 1]) == 0 {
            break;
        }
        g(c, "user32", "TranslateMessage", &[m]);
        g(c, "user32", "DispatchMessageW", &[m]);
    }
    mem::heap_free(m);
}

/// Install messages for the modeless dialog's subscribers (EventMapping): ActionText (an
/// action's description), ActionData, SetProgress. Returns whether a dialog took it.
pub fn message(c: &mut Ctx, kind: u32, rec: &Record) -> bool {
    let key = *MODELESS.lock().unwrap();
    if key == 0 {
        return false;
    }
    let Some((pkg, subs, ctls)) = with(key, |d| (d.pkg.clone().unwrap(), d.subs.clone(), d.ctls.clone())) else { return false };
    let text_of = |f: &Field| match f {
        Field::Null => String::new(),
        f => f.text(),
    };
    let set = |c: &mut Ctx, event: &str, v: &str| {
        for (_, i, _) in subs.iter().filter(|s| s.0 == event) {
            if ctls[*i].hwnd != 0 {
                set_text(c, ctls[*i].hwnd, v);
            }
        }
    };
    match kind >> 24 {
        // ACTIONSTART: [1] action, [2] description, [3] template
        8 => {
            set(c, "ActionText", &text_of(rec.get(2)));
            with(key, |d| d.next = Some(text_of(rec.get(3))));
        }
        // ACTIONDATA: the action's template filled with the record
        9 => {
            let tpl = with(key, |d| d.next.clone()).flatten().unwrap_or_default();
            let mut r = rec.clone();
            if !tpl.is_empty() {
                r.f[0] = Field::Str(tpl);
            }
            let v = pkg.lock().unwrap().format_record(&r);
            set(c, "ActionData", &v);
        }
        // PROGRESS: [1] 0 reset (total [2]), 2 increment ([2]), 1 action info, 3 add to total
        10 => {
            let (t, n) = (rec.get(1).int(), rec.get(2).int().max(0) as u64);
            with(key, |d| match t {
                0 => d.progress = (0, n),
                2 => d.progress.0 += n,
                3 => d.progress.1 += n,
                _ => {}
            });
            if subs.iter().any(|s| s.0 == "SetProgress") {
                if let Some(h) = with(key, |d| d.hwnd) {
                    g(c, "user32", "InvalidateRect", &[h, 0, 0]);
                }
            }
        }
        _ => {}
    }
    pump(c);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtf() {
        let r = r"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fnil Tahoma;}}{\colortbl ;\red0\green0\blue0;}{\*\generator Msftedit 5.41;}\viewkind4\uc1\pard\f0\fs16 License\par Line \b two\b0\tab x \{y\}\par\'80\par}";
        assert_eq!(rtf_text(r), "License\r\nLine two\tx {y}\r\n\u{20ac}");
        // (a Japanese document: Shift_JIS bytes)
        let r = r"{\rtf1\ansi\ansicpg932{\fonttbl{\f0 MS UI Gothic;}}\f0 \'82\'a0\'82\'a2\par}";
        assert_eq!(rtf_text(r), "あい");
    }

    #[test]
    fn units() {
        assert_eq!(px(0), 0);
        assert_eq!(px(370), 493);
        assert_eq!(px(3), 4);
    }
}
