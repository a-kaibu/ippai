//! The WebBrowser control (CLSID_WebBrowser, "Shell.Explorer.2", and CLSID_WebBrowser_V1) as
//! programs embed it: an ActiveX control that hosts like IE's (IOleObject with in-place
//! activation into its own child window "Shell Embedding", IOleInPlaceObject /
//! IOleInPlaceActiveObject, IOleControl, IPersistStreamInit, IViewObject2, connection points for
//! DWebBrowserEvents2 / DWebBrowserEvents / IPropertyNotifySink) and IWebBrowser2 to drive it.
//! It shows no pages: Navigate takes the address (LocationURL / LocationName give it back) and
//! the control is at once READYSTATE_COMPLETE, not busy, with no document. Delphi's TWebBrowser
//! (TOleControl) needs the control to exist at all: without it CoCreateInstance failed with
//! REGDB_E_CLASSNOTREG and Kurovadis 体験版 quit on "OLE error 80040154" (#216).
//! Not done: drawing pages, the navigation events, IDispatch by name (the dual interface's
//! vtable is what programs call).
#![allow(non_snake_case)]

use crate::api::{Ctx, Handler};
use crate::mem::{self, r16, r32, w16, w32};
use crate::HashMap;
use std::sync::{Mutex, OnceLock};

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_FAIL: u32 = 0x8000_4005;
const E_INVALIDARG: u32 = 0x8007_0057;
const DISP_E_MEMBERNOTFOUND: u32 = 0x8002_0003;
const DISP_E_UNKNOWNNAME: u32 = 0x8002_0006;
const TYPE_E_ELEMENTNOTFOUND: u32 = 0x8002_802b;
const CONNECT_E_NOCONNECTION: u32 = 0x8004_0200;
const CONNECT_E_ADVISELIMIT: u32 = 0x8004_0201;
const OLEOBJ_S_INVALIDVERB: u32 = 0x0004_0180;

/// OLEMISC_SETCLIENTSITEFIRST | ACTIVATEWHENVISIBLE | INSIDEOUT | CANTLINKINSIDE |
/// RECOMPOSEONRESIZE, as IE's control reports
pub const MISC_STATUS: u32 = 0x20000 | 0x100 | 0x80 | 0x10 | 0x1;

/// A GUID's 16 bytes from its text form ("8856F961-340A-11D0-A96B-00C04FD705A2").
pub fn guid(s: &str) -> [u8; 16] {
    let h: Vec<u8> = s.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
    let byte = |i: usize| u8::from_str_radix(std::str::from_utf8(&h[2 * i..2 * i + 2]).unwrap(), 16).unwrap();
    let mut g = [0u8; 16];
    // Data1..Data3 little-endian, Data4 as written
    for (o, (at, n)) in [(0usize, (0usize, 4usize)), (4, (4, 2)), (6, (6, 2))] {
        for k in 0..n {
            g[o + k] = byte(at + n - 1 - k);
        }
    }
    for k in 8..16 {
        g[k] = byte(k);
    }
    g
}

pub const CLSID_WEBBROWSER: &str = "8856F961-340A-11D0-A96B-00C04FD705A2";
pub const CLSID_WEBBROWSER_V1: &str = "EAB22AC3-30C1-11CF-A7EB-0000C05BAE0B";

/// The interfaces, in the object's order, and the IIDs each answers
const IFACES: &[&[&str]] = &[
    // IWebBrowser2 (dual: IDispatch, IWebBrowser, IWebBrowserApp), and IUnknown
    &["00000000-0000-0000-C000-000000000046", "00020400-0000-0000-C000-000000000046", "EAB22AC1-30C1-11CF-A7EB-0000C05BAE0B", "0002DF05-0000-0000-C000-000000000046", "D30C1661-CDAF-11D0-8A3E-00C04FC9E26E"],
    // IOleObject
    &["00000112-0000-0000-C000-000000000046"],
    // IOleInPlaceObject, IOleWindow
    &["00000113-0000-0000-C000-000000000046", "00000114-0000-0000-C000-000000000046"],
    // IOleInPlaceActiveObject
    &["00000117-0000-0000-C000-000000000046"],
    // IOleControl
    &["B196B288-BAB4-101A-B69C-00AA00341D07"],
    // IPersistStreamInit, IPersist
    &["7FD52380-4E07-101B-AE2D-08002B2EC713", "0000010C-0000-0000-C000-000000000046"],
    // IConnectionPointContainer
    &["B196B284-BAB4-101A-B69C-00AA00341D07"],
    // IViewObject2, IViewObject
    &["00000127-0000-0000-C000-000000000046", "0000010D-0000-0000-C000-000000000046"],
];

/// The outgoing interfaces it has connection points for: DWebBrowserEvents2, DWebBrowserEvents,
/// IPropertyNotifySink
const EVENTS: [&str; 3] = ["34A715A0-6587-11D0-924A-0020AFC7AC4D", "EAB22AC2-30C1-11CF-A7EB-0000C05BAE0B", "9BFBBC02-EFF1-101A-84ED-00AA00341D07"];

const IID_IUNKNOWN: &str = "00000000-0000-0000-C000-000000000046";
const IID_ICONNECTIONPOINT: &str = "B196B286-BAB4-101A-B69C-00AA00341D07";
const IID_IOLEINPLACESITE: &str = "00000119-0000-0000-C000-000000000046";

/// Which interface of the object an IID asks for
pub fn which(iid: &[u8]) -> Option<usize> {
    IFACES.iter().position(|ids| ids.iter().any(|s| guid(s) == iid))
}

/// Which connection point an IID asks for
pub fn event_index(iid: &[u8]) -> Option<usize> {
    EVENTS.iter().position(|s| guid(s) == iid)
}

/// The IWebBrowser2 slots whose value is a property: (the get's slot, VARIANT_BOOL (2 bytes)
/// or long), the put the slot after it where there is one
const PROPS: &[(usize, bool)] = &[
    (21, false), // Left
    (23, false), // Top
    (25, false), // Width
    (27, false), // Height
    (40, true),  // Visible
    (42, true),  // StatusBar
    (46, false), // ToolBar (int)
    (48, true),  // MenuBar
    (50, true),  // FullScreen
    (57, true),  // Offline
    (59, true),  // Silent
    (61, true),  // RegisterAsBrowser
    (63, true),  // RegisterAsDropTarget
    (65, true),  // TheaterMode
    (67, true),  // AddressBar
    (69, true),  // Resizable
];

#[derive(Default)]
struct Browser {
    refs: u32,
    site: u32,
    hwnd: u32,
    url: Vec<u16>,
    navigated: bool,
    props: HashMap<usize, u32>,
    /// (HIMETRIC)
    extent: (i32, i32),
    /// the connection point objects, made on first ask
    points: [u32; 3],
}

struct Point {
    owner: u32,
    which: usize,
    sinks: Vec<(u32, u32)>,
    next: u32,
}

static BROWSERS: Mutex<Option<HashMap<u32, Browser>>> = Mutex::new(None);
static POINTS: Mutex<Option<HashMap<u32, Point>>> = Mutex::new(None);

fn browsers<R>(f: impl FnOnce(&mut HashMap<u32, Browser>) -> R) -> R {
    f(BROWSERS.lock().unwrap().get_or_insert_with(HashMap::default))
}
fn points<R>(f: impl FnOnce(&mut HashMap<u32, Point>) -> R) -> R {
    f(POINTS.lock().unwrap().get_or_insert_with(HashMap::default))
}

fn base_of(this: u32) -> u32 {
    let ps = mem::ps();
    browsers(|m| (0..IFACES.len() as u32).map(|i| this.wrapping_sub(ps * i)).find(|b| m.contains_key(b))).unwrap_or(this)
}
fn with<R>(this: u32, f: impl FnOnce(&mut Browser) -> R) -> Option<R> {
    let b = base_of(this);
    browsers(|m| m.get_mut(&b).map(f))
}

/// A GUID in guest memory (for the IIDs sake asks objects of the program for)
fn guest_guid(s: &'static str) -> u32 {
    static AT: Mutex<Option<HashMap<&'static str, u32>>> = Mutex::new(None);
    *AT.lock().unwrap().get_or_insert_with(HashMap::default).entry(s).or_insert_with(|| {
        let p = mem::heap_alloc(16, false);
        mem::write(p, &guid(s));
        p
    })
}

/// Calls method `slot` of the program's object `obj`
fn vcall(c: &mut Ctx, obj: u32, slot: u32, args: &[u32]) -> u32 {
    let f = mem::rp(mem::rp(obj) + mem::ps() * slot);
    let mut a = vec![obj];
    a.extend_from_slice(args);
    c.call(f, &a)
}

fn ok(_: &mut Ctx) -> u32 {
    S_OK
}
fn s_false(_: &mut Ctx) -> u32 {
    S_FALSE
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let b = base_of(this);
    match which(mem::slice(iid, 16)) {
        Some(i) if with(b, |s| s.refs += 1).is_some() => {
            mem::putp(out, b + mem::ps() * i as u32);
            S_OK
        }
        _ => {
            mem::putp(out, 0);
            E_NOINTERFACE
        }
    }
}
fn add_ref(c: &mut Ctx) -> u32 {
    with(c.a(0), |s| {
        s.refs += 1;
        s.refs
    })
    .unwrap_or(1)
}
fn release(c: &mut Ctx) -> u32 {
    let b = base_of(c.a(0));
    let gone = browsers(|m| match m.get_mut(&b) {
        Some(s) if s.refs > 1 => {
            s.refs -= 1;
            None
        }
        Some(_) => m.remove(&b),
        None => None,
    });
    match gone {
        Some(s) => {
            if s.hwnd != 0 {
                gcall(c, "user32", "DestroyWindow", &[s.hwnd]);
            }
            if s.site != 0 {
                vcall(c, s.site, 2, &[]);
            }
            0
        }
        None => with(b, |s| s.refs).unwrap_or(0),
    }
}

fn gcall(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let f = crate::api::export(dll, name);
    c.call(f, args)
}

fn bstr(chars: &[u16]) -> u32 {
    crate::misc::bstr_alloc(chars)
}

/// A BSTR's characters (by its length prefix; NULL is empty)
fn bstr_text(p: u32) -> Vec<u16> {
    if p == 0 {
        return Vec::new();
    }
    let n = r32(p - 4) / 2;
    (0..n).map(|i| r16(p + 2 * i)).collect()
}

/// The VARIANT at `p` as text (VT_BSTR, or VT_BSTR | VT_BYREF), else empty
fn variant_text(p: u32) -> Vec<u16> {
    if p == 0 {
        return Vec::new();
    }
    match r16(p) {
        8 => bstr_text(mem::rp(p + 8)),
        0x4008 => bstr_text(mem::rp(mem::rp(p + 8))),
        _ => Vec::new(),
    }
}

// ---- IDispatch ----

fn GetTypeInfoCount(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 0);
    S_OK
}
fn GetTypeInfo(c: &mut Ctx) -> u32 {
    mem::putp(c.a(3), 0);
    TYPE_E_ELEMENTNOTFOUND
}
fn GetIDsOfNames(c: &mut Ctx) -> u32 {
    // (every name unknown: DISPID_UNKNOWN for each)
    let (n, out) = (c.a(3), c.a(5));
    for i in 0..n.min(0x1000) {
        if out != 0 {
            w32(out + 4 * i, u32::MAX);
        }
    }
    DISP_E_UNKNOWNNAME
}
fn Invoke(_: &mut Ctx) -> u32 {
    DISP_E_MEMBERNOTFOUND
}

// ---- IWebBrowser / IWebBrowserApp / IWebBrowser2 ----

fn navigate_to(this: u32, url: Vec<u16>) -> u32 {
    with(this, |s| {
        s.url = url;
        s.navigated = true;
    });
    S_OK
}
fn Navigate(c: &mut Ctx) -> u32 {
    navigate_to(c.a(0), bstr_text(c.a(1)))
}
fn Navigate2(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_INVALIDARG;
    }
    navigate_to(c.a(0), variant_text(c.a(1)))
}
fn Refresh2(_: &mut Ctx) -> u32 {
    S_OK
}
/// get_Application: the control itself (IDispatch)
fn get_self(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let b = base_of(this);
    with(b, |s| s.refs += 1);
    mem::wp(out, b);
    S_OK
}
/// get_Parent, get_Container, get_Document: none
fn get_null(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::wp(c.a(1), 0);
    S_OK
}
fn put_bool(p: u32, v: bool) {
    w16(p, if v { 0xffff } else { 0 });
}
fn get_TopLevelContainer(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put_bool(c.a(1), false);
    S_OK
}
fn get_Busy(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put_bool(c.a(1), false);
    S_OK
}
fn get_Type(c: &mut Ctx) -> u32 {
    get_null(c)
}
fn get_LocationURL(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let url = with(this, |s| s.url.clone()).unwrap_or_default();
    mem::wp(out, bstr(&url));
    S_OK
}
fn get_Name(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let name: Vec<u16> = "Microsoft Web Browser Control".encode_utf16().collect();
    mem::wp(c.a(1), bstr(&name));
    S_OK
}
fn get_ReadyState(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    // READYSTATE_COMPLETE once it has gone anywhere, READYSTATE_UNINITIALIZED before
    let done = with(c.a(0), |s| s.navigated).unwrap_or(false);
    w32(c.a(1), if done { 4 } else { 0 });
    S_OK
}
fn GetProperty(c: &mut Ctx) -> u32 {
    // (no properties put: VT_EMPTY)
    if c.a(2) != 0 {
        mem::fill(c.a(2), 16, 0);
    }
    S_OK
}
fn QueryStatusWB(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    w32(c.a(2), 0);
    S_OK
}

/// The slot `c.trap` is in IWebBrowser2's vtable
fn browser_slot(c: &Ctx) -> Option<usize> {
    let v = vtables()[0];
    (0..BROWSER_SLOTS).find(|&i| mem::rp(v + mem::ps() * i as u32) == c.trap)
}
/// get_Left, get_Visible, ...: the value put, the window's for the size, else 0
fn prop_get(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let Some(slot) = browser_slot(c) else { return E_FAIL };
    let Some(&(_, short)) = PROPS.iter().find(|p| p.0 == slot) else { return E_FAIL };
    let v = with(this, |s| s.props.get(&slot).copied()).flatten().unwrap_or(0);
    if short {
        w16(out, v as u16);
    } else {
        w32(out, v);
    }
    S_OK
}
fn prop_put(c: &mut Ctx) -> u32 {
    let (this, v) = (c.a(0), c.a(1));
    let Some(slot) = browser_slot(c) else { return E_FAIL };
    let Some(&(get, short)) = PROPS.iter().find(|p| p.0 + 1 == slot) else { return E_FAIL };
    let v = if short { if v & 0xffff != 0 { 0xffff } else { 0 } } else { v };
    with(this, |s| s.props.insert(get, v));
    S_OK
}

// ---- IOleObject ----

fn SetClientSite(c: &mut Ctx) -> u32 {
    let (this, site) = (c.a(0), c.a(1));
    if site != 0 {
        vcall(c, site, 1, &[]);
    }
    let old = with(this, |s| std::mem::replace(&mut s.site, site)).unwrap_or(0);
    if old != 0 {
        vcall(c, old, 2, &[]);
    }
    S_OK
}
fn GetClientSite(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let site = with(this, |s| s.site).unwrap_or(0);
    if site != 0 {
        vcall(c, site, 1, &[]);
    }
    mem::wp(out, site);
    S_OK
}
fn Close(c: &mut Ctx) -> u32 {
    deactivate(c, c.a(0));
    S_OK
}
fn GetUserClassID(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::write(c.a(1), &guid(CLSID_WEBBROWSER));
    S_OK
}
fn SetExtent(c: &mut Ctx) -> u32 {
    let (this, size) = (c.a(0), c.a(2));
    if size == 0 {
        return E_INVALIDARG;
    }
    let e = (r32(size) as i32, r32(size + 4) as i32);
    with(this, |s| s.extent = e);
    S_OK
}
fn GetExtent(c: &mut Ctx) -> u32 {
    let (this, size) = (c.a(0), c.a(2));
    if size == 0 {
        return E_INVALIDARG;
    }
    let (w, h) = with(this, |s| s.extent).unwrap_or_default();
    w32(size, w as u32);
    w32(size + 4, h as u32);
    S_OK
}
/// IViewObject2::GetExtent(aspect, lindex, ptd, size)
fn ViewGetExtent(c: &mut Ctx) -> u32 {
    let (this, size) = (c.a(0), c.a(4));
    if size == 0 {
        return E_INVALIDARG;
    }
    let (w, h) = with(this, |s| s.extent).unwrap_or_default();
    w32(size, w as u32);
    w32(size + 4, h as u32);
    S_OK
}
fn Advise(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), 1);
    S_OK
}
fn GetMiscStatus(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    w32(c.a(2), MISC_STATUS);
    S_OK
}

/// "Shell Embedding", the control's window: DefWindowProc's (nothing drawn but the background)
fn window_class(c: &mut Ctx) -> u32 {
    static NAME: OnceLock<u32> = OnceLock::new();
    if let Some(&n) = NAME.get() {
        return n;
    }
    let name: Vec<u16> = "Shell Embedding\0".encode_utf16().collect();
    let np = mem::heap_alloc(name.len() as u32 * 2, false);
    for (i, ch) in name.iter().enumerate() {
        w16(np + 2 * i as u32, *ch);
    }
    let ps = mem::ps();
    // WNDCLASSW { style, lpfnWndProc, cbClsExtra, cbWndExtra, hInstance, hIcon, hCursor,
    // hbrBackground, lpszMenuName, lpszClassName }
    let wc = mem::heap_alloc(80, true);
    w32(wc, 0x8);
    let mut at = ps; // (the pointer after the style dword, aligned)
    mem::wp(wc + at, crate::api::export("user32", "DefWindowProcW"));
    at += ps + 8;
    // hInstance, hIcon, hCursor, hbrBackground (COLOR_WINDOW + 1), lpszMenuName
    mem::wp(wc + at + 3 * ps, 6);
    at += 5 * ps;
    mem::wp(wc + at, np);
    gcall(c, "user32", "RegisterClassW", &[wc]);
    *NAME.get_or_init(|| np)
}

/// In-place activation (DoVerb INPLACEACTIVATE, SHOW, UIACTIVATE): the site is told, the
/// control's window made in the site's window at `rect`
fn activate(c: &mut Ctx, this: u32, site: u32, parent: u32, rect: u32) -> u32 {
    if with(this, |s| s.hwnd).unwrap_or(0) != 0 {
        return S_OK;
    }
    let site = if site != 0 { site } else { with(this, |s| s.site).unwrap_or(0) };
    if site == 0 {
        return E_INVALIDARG;
    }
    let out = mem::heap_alloc(8, true);
    let hr = vcall(c, site, 0, &[guest_guid(IID_IOLEINPLACESITE), out]);
    let ips = mem::rp(out);
    mem::heap_free(out);
    if hr != 0 || ips == 0 {
        return E_NOINTERFACE;
    }
    if vcall(c, ips, 5, &[]) != S_OK {
        // CanInPlaceActivate: no
        vcall(c, ips, 2, &[]);
        return E_FAIL;
    }
    vcall(c, ips, 6, &[]); // OnInPlaceActivate
    let mut parent = parent;
    if parent == 0 {
        let h = mem::heap_alloc(8, true);
        vcall(c, ips, 3, &[h]); // IOleWindow::GetWindow
        parent = mem::rp(h);
        mem::heap_free(h);
    }
    let (l, t, r, b) = if rect != 0 { (r32(rect) as i32, r32(rect + 4) as i32, r32(rect + 8) as i32, r32(rect + 12) as i32) } else { (0, 0, 0, 0) };
    let cls = window_class(c);
    // WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN
    let hwnd = gcall(c, "user32", "CreateWindowExW", &[0, cls, 0, 0x5600_0000, l as u32, t as u32, (r - l).max(0) as u32, (b - t).max(0) as u32, parent, 0, 0, 0]);
    vcall(c, ips, 2, &[]);
    with(this, |s| {
        s.hwnd = hwnd;
        s.props.insert(21, l as u32);
        s.props.insert(23, t as u32);
        s.props.insert(25, (r - l) as u32);
        s.props.insert(27, (b - t) as u32);
    });
    if hwnd == 0 {
        E_FAIL
    } else {
        S_OK
    }
}

fn deactivate(c: &mut Ctx, this: u32) {
    let (hwnd, site) = with(this, |s| (std::mem::take(&mut s.hwnd), s.site)).unwrap_or((0, 0));
    if hwnd == 0 {
        return;
    }
    gcall(c, "user32", "DestroyWindow", &[hwnd]);
    if site != 0 {
        let out = mem::heap_alloc(8, true);
        if vcall(c, site, 0, &[guest_guid(IID_IOLEINPLACESITE), out]) == 0 && mem::rp(out) != 0 {
            let ips = mem::rp(out);
            vcall(c, ips, 11, &[]); // OnInPlaceDeactivate
            vcall(c, ips, 2, &[]);
        }
        mem::heap_free(out);
    }
}

/// DoVerb(verb, msg, site, index, hwndParent, rect)
fn DoVerb(c: &mut Ctx) -> u32 {
    let (this, verb, site, parent, rect) = (c.a(0), c.a(1) as i32, c.a(3), c.a(5), c.a(6));
    match verb {
        // OLEIVERB_PRIMARY, SHOW, UIACTIVATE, INPLACEACTIVATE
        0 | -1 | -4 | -5 => activate(c, this, site, parent, rect),
        // OLEIVERB_HIDE
        -3 => {
            let h = with(this, |s| s.hwnd).unwrap_or(0);
            if h != 0 {
                gcall(c, "user32", "ShowWindow", &[h, 0]);
            }
            S_OK
        }
        // OPEN, DISCARDUNDOSTATE
        -2 | -6 => E_NOTIMPL,
        _ => OLEOBJ_S_INVALIDVERB,
    }
}

// ---- IOleInPlaceObject / IOleInPlaceActiveObject ----

fn GetWindow(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let h = with(this, |s| s.hwnd).unwrap_or(0);
    mem::wp(out, h);
    if h == 0 {
        E_FAIL
    } else {
        S_OK
    }
}
fn InPlaceDeactivate(c: &mut Ctx) -> u32 {
    deactivate(c, c.a(0));
    S_OK
}
fn SetObjectRects(c: &mut Ctx) -> u32 {
    let (this, pos) = (c.a(0), c.a(1));
    if pos == 0 {
        return E_INVALIDARG;
    }
    let (l, t, r, b) = (r32(pos) as i32, r32(pos + 4) as i32, r32(pos + 8) as i32, r32(pos + 12) as i32);
    let h = with(this, |s| {
        s.props.insert(21, l as u32);
        s.props.insert(23, t as u32);
        s.props.insert(25, (r - l) as u32);
        s.props.insert(27, (b - t) as u32);
        s.hwnd
    })
    .unwrap_or(0);
    if h != 0 {
        gcall(c, "user32", "MoveWindow", &[h, l as u32, t as u32, (r - l).max(0) as u32, (b - t).max(0) as u32, 1]);
    }
    S_OK
}

// ---- IOleControl ----

fn GetControlInfo(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return E_POINTER;
    }
    // CONTROLINFO { cb, hAccel, cAccel, dwFlags }: no mnemonics
    let ps = mem::ps();
    mem::wp(p + ps, 0);
    w16(p + 2 * ps, 0);
    w32(p + 2 * ps + 4, 0);
    S_OK
}

// ---- IPersistStreamInit ----

fn GetClassID(c: &mut Ctx) -> u32 {
    GetUserClassID(c)
}

// ---- IConnectionPointContainer / IConnectionPoint ----

fn FindConnectionPoint(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 || iid == 0 {
        return E_POINTER;
    }
    let Some(k) = event_index(mem::slice(iid, 16)) else {
        mem::wp(out, 0);
        return CONNECT_E_NOCONNECTION;
    };
    let b = base_of(this);
    let have = with(b, |s| s.points[k]).unwrap_or(0);
    let p = if have != 0 {
        have
    } else {
        let p = mem::heap_alloc(mem::ps(), true);
        mem::wp(p, point_vtable());
        points(|m| m.insert(p, Point { owner: b, which: k, sinks: Vec::new(), next: 1 }));
        with(b, |s| s.points[k] = p);
        p
    };
    with(b, |s| s.refs += 1);
    mem::wp(out, p);
    S_OK
}

fn point_qi(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = mem::slice(iid, 16);
    if g == guid(IID_IUNKNOWN) || g == guid(IID_ICONNECTIONPOINT) {
        let owner = points(|m| m.get(&this).map(|p| p.owner)).unwrap_or(0);
        with(owner, |s| s.refs += 1);
        mem::wp(out, this);
        S_OK
    } else {
        mem::wp(out, 0);
        E_NOINTERFACE
    }
}
/// A connection point lives as long as its control: its references are the control's
fn point_add_ref(c: &mut Ctx) -> u32 {
    let owner = points(|m| m.get(&c.a(0)).map(|p| p.owner)).unwrap_or(0);
    with(owner, |s| {
        s.refs += 1;
        s.refs
    })
    .unwrap_or(1)
}
fn point_release(c: &mut Ctx) -> u32 {
    let owner = points(|m| m.get(&c.a(0)).map(|p| p.owner)).unwrap_or(0);
    if owner == 0 {
        return 0;
    }
    vcall(c, owner, 2, &[])
}
fn GetConnectionInterface(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let Some(k) = points(|m| m.get(&this).map(|p| p.which)) else { return E_FAIL };
    mem::write(out, &guid(EVENTS[k]));
    S_OK
}
fn GetConnectionPointContainer(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let owner = points(|m| m.get(&this).map(|p| p.owner)).unwrap_or(0);
    with(owner, |s| s.refs += 1);
    mem::wp(out, owner + 6 * mem::ps());
    S_OK
}
fn PointAdvise(c: &mut Ctx) -> u32 {
    let (this, sink, cookie) = (c.a(0), c.a(1), c.a(2));
    if sink == 0 || cookie == 0 {
        return E_POINTER;
    }
    if !points(|m| m.contains_key(&this)) {
        return E_FAIL;
    }
    // (the sink as the outgoing interface would be asked for; IUnknown's AddRef either way)
    vcall(c, sink, 1, &[]);
    let k = points(|m| {
        let p = m.get_mut(&this)?;
        let k = p.next;
        p.next += 1;
        p.sinks.push((k, sink));
        Some(k)
    });
    match k {
        Some(k) => {
            w32(cookie, k);
            S_OK
        }
        None => CONNECT_E_ADVISELIMIT,
    }
}
fn Unadvise(c: &mut Ctx) -> u32 {
    let (this, cookie) = (c.a(0), c.a(1));
    let sink = points(|m| {
        let p = m.get_mut(&this)?;
        let i = p.sinks.iter().position(|s| s.0 == cookie)?;
        Some(p.sinks.remove(i).1)
    });
    match sink {
        Some(s) => {
            vcall(c, s, 2, &[]);
            S_OK
        }
        None => CONNECT_E_NOCONNECTION,
    }
}

// ---- vtables ----

/// IWebBrowser2's slot count: IUnknown 3, IDispatch 4, IWebBrowser 25, IWebBrowserApp 20,
/// IWebBrowser2 19
const BROWSER_SLOTS: usize = 71;

fn browser_methods() -> Vec<(Handler, u32)> {
    let g = prop_get as Handler;
    let p = prop_put as Handler;
    vec![
        (qi, 12),
        (add_ref, 4),
        (release, 4),
        // IDispatch
        (GetTypeInfoCount, 8),
        (GetTypeInfo, 16),
        (GetIDsOfNames, 24),
        (Invoke, 36),
        // IWebBrowser: GoBack, GoForward, GoHome, GoSearch, Navigate, Refresh, Refresh2, Stop
        (s_false, 4),
        (s_false, 4),
        (ok, 4),
        (ok, 4),
        (Navigate, 24),
        (ok, 4),
        (Refresh2, 8),
        (ok, 4),
        // get_Application, get_Parent, get_Container, get_Document, get_TopLevelContainer, get_Type
        (get_self, 8),
        (get_null, 8),
        (get_null, 8),
        (get_null, 8),
        (get_TopLevelContainer, 8),
        (get_Type, 8),
        // Left, Top, Width, Height (21..28)
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        // get_LocationName, get_LocationURL, get_Busy
        (get_LocationURL, 8),
        (get_LocationURL, 8),
        (get_Busy, 8),
        // IWebBrowserApp: Quit, ClientToWindow, PutProperty (BSTR, VARIANT), GetProperty
        (ok, 4),
        (ok, 12),
        (ok, 24),
        (GetProperty, 12),
        // get_Name, get_HWND, get_FullName, get_Path
        (get_Name, 8),
        (notimpl, 8),
        (notimpl, 8),
        (notimpl, 8),
        // Visible, StatusBar (40..43)
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        // StatusText
        (get_null, 8),
        (ok, 8),
        // ToolBar, MenuBar, FullScreen (46..51)
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        // IWebBrowser2: Navigate2, QueryStatusWB, ExecWB, ShowBrowserBar, get_ReadyState
        (Navigate2, 24),
        (QueryStatusWB, 12),
        (notimpl, 20),
        (notimpl, 16),
        (get_ReadyState, 8),
        // Offline, Silent, RegisterAsBrowser, RegisterAsDropTarget, TheaterMode, AddressBar,
        // Resizable (57..70)
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
        (g, 8),
        (p, 8),
    ]
}

fn vtables() -> [u32; 8] {
    static V: OnceLock<[u32; 8]> = OnceLock::new();
    *V.get_or_init(|| {
        let browser = crate::api::vtable("IWebBrowser2", &browser_methods());
        let unk: [(Handler, u32); 3] = [(qi, 12), (add_ref, 4), (release, 4)];
        let mk = |name: &str, rest: &[(Handler, u32)]| {
            let mut m = unk.to_vec();
            m.extend_from_slice(rest);
            crate::api::vtable(name, &m)
        };
        let ole = mk(
            "IOleObject",
            &[
                (SetClientSite, 8),
                (GetClientSite, 8),
                (ok, 12),     // SetHostNames
                (Close, 8),
                (notimpl, 12), // SetMoniker
                (notimpl, 16), // GetMoniker
                (notimpl, 16), // InitFromData
                (notimpl, 12), // GetClipboardData
                (DoVerb, 28),
                (notimpl, 8), // EnumVerbs
                (ok, 4),      // Update
                (ok, 4),      // IsUpToDate
                (GetUserClassID, 8),
                (notimpl, 12), // GetUserType
                (SetExtent, 12),
                (GetExtent, 12),
                (Advise, 12),
                (ok, 8),      // Unadvise
                (notimpl, 8), // EnumAdvise
                (GetMiscStatus, 12),
                (notimpl, 8), // SetColorScheme
            ],
        );
        let ipo = mk("IOleInPlaceObject", &[(GetWindow, 8), (notimpl, 8), (InPlaceDeactivate, 4), (ok, 4), (SetObjectRects, 12), (notimpl, 4)]);
        let ipao = mk("IOleInPlaceActiveObject", &[(GetWindow, 8), (notimpl, 8), (s_false, 8), (ok, 8), (ok, 8), (ok, 16), (ok, 8)]);
        let ctl = mk("IOleControl", &[(GetControlInfo, 8), (ok, 8), (ok, 8), (ok, 8)]);
        let psi = mk("IPersistStreamInit", &[(GetClassID, 8), (s_false, 4), (ok, 8), (ok, 12), (notimpl, 8), (ok, 4)]);
        let cpc = mk("IConnectionPointContainer", &[(notimpl, 8), (FindConnectionPoint, 12)]);
        let view = mk("IViewObject2", &[(ok, 44), (s_false, 28), (notimpl, 20), (notimpl, 8), (ok, 16), (notimpl, 16), (ViewGetExtent, 20)]);
        [browser, ole, ipo, ipao, ctl, psi, cpc, view]
    })
}

fn point_vtable() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        crate::api::vtable(
            "IConnectionPoint",
            &[(point_qi, 12), (point_add_ref, 4), (point_release, 4), (GetConnectionInterface, 8), (GetConnectionPointContainer, 8), (PointAdvise, 12), (Unadvise, 8), (notimpl, 8)],
        )
    })
}

/// CoCreateInstance(CLSID_WebBrowser): None for another class, else the HRESULT
pub fn create_instance(clsid: u32, iid: u32, out: u32) -> Option<u32> {
    let cls = mem::slice(clsid, 16);
    if cls != guid(CLSID_WEBBROWSER) && cls != guid(CLSID_WEBBROWSER_V1) {
        return None;
    }
    let Some(i) = which(mem::slice(iid, 16)) else {
        mem::putp(out, 0);
        return Some(E_NOINTERFACE);
    };
    let ps = mem::ps();
    let o = mem::heap_alloc(ps * IFACES.len() as u32, true);
    for (k, v) in vtables().iter().enumerate() {
        mem::wp(o + ps * k as u32, *v);
    }
    let mut b = Browser { refs: 1, ..Browser::default() };
    // (IE's control starts at about 7.5 x 5 cm, HIMETRIC)
    b.extent = (7620, 5080);
    b.props.insert(40, 0xffff); // Visible
    b.props.insert(63, 0xffff); // RegisterAsDropTarget
    b.props.insert(69, 0xffff); // Resizable
    browsers(|m| m.insert(o, b));
    mem::putp(out, o + ps * i as u32);
    Some(S_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn guids_from_text() {
        assert_eq!(guid(CLSID_WEBBROWSER), [0x61, 0xf9, 0x56, 0x88, 0x0a, 0x34, 0xd0, 0x11, 0xa9, 0x6b, 0x00, 0xc0, 0x4f, 0xd7, 0x05, 0xa2]);
        assert_eq!(guid("00000112-0000-0000-C000-000000000046"), [0x12, 0x01, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    }

    /// Each IID the control answers is in one interface only, IUnknown is the first's, and the
    /// interfaces Delphi's TOleControl asks for are there.
    #[test]
    fn interfaces() {
        let mut seen = std::collections::HashSet::new();
        for (i, ids) in IFACES.iter().enumerate() {
            for s in *ids {
                assert!(seen.insert(guid(s)), "{s} twice");
                assert_eq!(which(&guid(s)), Some(i));
            }
        }
        assert_eq!(which(&guid(IID_IUNKNOWN)), Some(0));
        for s in ["00000112-0000-0000-C000-000000000046", "7FD52380-4E07-101B-AE2D-08002B2EC713", "B196B288-BAB4-101A-B69C-00AA00341D07", "00020400-0000-0000-C000-000000000046", "D30C1661-CDAF-11D0-8A3E-00C04FC9E26E", "00000113-0000-0000-C000-000000000046", "00000117-0000-0000-C000-000000000046"] {
            assert!(which(&guid(s)).is_some(), "{s}");
        }
        assert_eq!(which(&guid(IID_ICONNECTIONPOINT)), None);
        assert_eq!(which(&guid(IID_IOLEINPLACESITE)), None);
        assert_eq!(event_index(&guid("34A715A0-6587-11D0-924A-0020AFC7AC4D")), Some(0));
    }

    /// IWebBrowser2's vtable has its 71 slots, the property gets where PROPS has them and each
    /// put right after its get.
    #[test]
    fn browser_vtable_layout() {
        let m = browser_methods();
        assert_eq!(m.len(), BROWSER_SLOTS);
        for &(get, _) in PROPS {
            assert!(m[get].0 as usize == prop_get as Handler as usize, "get at {get}");
            assert!(m[get + 1].0 as usize == prop_put as Handler as usize, "put at {}", get + 1);
            assert_eq!((m[get].1, m[get + 1].1), (8, 8));
        }
        assert_eq!(m.iter().filter(|x| x.0 as usize == prop_get as Handler as usize).count(), PROPS.len());
        assert!(m[11].0 as usize == Navigate as Handler as usize && m[11].1 == 24);
        assert!(m[52].0 as usize == Navigate2 as Handler as usize && m[52].1 == 24);
        assert!(m[56].0 as usize == get_ReadyState as Handler as usize);
    }

    proptest! {
        /// Any GUID's text form gives back its bytes (Data1..3 little-endian).
        #[test]
        fn guid_text_round_trip(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) {
            let s = format!("{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{}", d4[0], d4[1], d4[2..].iter().map(|b| format!("{b:02X}")).collect::<String>());
            let g = guid(&s);
            prop_assert_eq!(&g[0..4], &d1.to_le_bytes());
            prop_assert_eq!(&g[4..6], &d2.to_le_bytes());
            prop_assert_eq!(&g[6..8], &d3.to_le_bytes());
            prop_assert_eq!(&g[8..], &d4);
        }
    }
}
