//! combase's Windows Runtime basics (api-ms-win-core-winrt-*): HSTRINGs and activation.
//! Unity 2021+ initializes the Windows Runtime at startup and asks for a few runtime classes
//! (gamepads, UI settings); none is registered here, which Unity takes as an older Windows.
//!
//! An HSTRING (NULL is the empty string) points at Windows' header, which C++/WinRT programs
//! also build themselves (its fast-pass and heap strings): flags (1: a reference string living
//! in the caller's HSTRING_HEADER), length in UTF-16 units, two padding words, the characters'
//! address (pointer-sized, +16), and for strings made on the heap a reference count after it,
//! then the characters and a terminating 0.
#![allow(non_snake_case)]
use crate::api::{implement, Ctx};
use crate::mem::{self, ps, r32, rp, w32, wp};

const S_OK: u32 = 0;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_POINTER: u32 = 0x8000_4003;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;
const REF: u32 = 1;
/// the characters' address
const PTR: u32 = 16;
/// a heap string's reference count
fn count(h: u32) -> u32 {
    h + PTR + ps()
}

/// The longest string plus one (the header and the characters' bytes fit in 31 bits)
const MAX_LEN: u32 = 0x3fff_fff0;

/// A new HSTRING holding `s` (empty: NULL)
fn create(s: &[u16]) -> Option<u32> {
    if s.is_empty() {
        return Some(0);
    }
    let n = u32::try_from(s.len()).ok().filter(|&n| n < MAX_LEN)?;
    let at = PTR + ps() + 4;
    let h = mem::heap_alloc(at + 2 * (n + 1), true);
    if h == 0 {
        return None;
    }
    w32(h + 4, n);
    wp(h + PTR, h + at);
    w32(count(h), 1);
    let b: Vec<u8> = s.iter().chain(&[0]).flat_map(|c| c.to_le_bytes()).collect();
    mem::write(h + at, &b);
    Some(h)
}

fn len(h: u32) -> u32 {
    if h == 0 { 0 } else { r32(h + 4) }
}

fn chars(h: u32) -> Vec<u16> {
    if h == 0 { Vec::new() } else { mem::wstrn(rp(h + PTR), r32(h + 4).min(MAX_LEN)) }
}

/// WindowsCreateString(src, length, &string)
fn WindowsCreateString(c: &mut Ctx) -> u32 {
    let (src, n, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_INVALIDARG;
    }
    if src == 0 && n != 0 {
        return E_POINTER;
    }
    // (the length is checked before the characters are read: a huge one is refused, not copied)
    if n >= MAX_LEN {
        return E_OUTOFMEMORY;
    }
    match create(&if n == 0 { Vec::new() } else { mem::wstrn(src, n) }) {
        Some(h) => {
            mem::putp(out, h);
            S_OK
        }
        None => E_OUTOFMEMORY,
    }
}

/// WindowsCreateStringReference(src, length, &header, &string): the string stays the caller's
/// (it has to end in a 0 at `length`)
fn WindowsCreateStringReference(c: &mut Ctx) -> u32 {
    let (src, n, hdr, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 || hdr == 0 {
        return E_INVALIDARG;
    }
    if n == 0 {
        mem::putp(out, 0);
        return S_OK;
    }
    if src == 0 {
        return E_POINTER;
    }
    if mem::r16(src + 2 * n) != 0 {
        return E_INVALIDARG;
    }
    w32(hdr, REF);
    w32(hdr + 4, n);
    w32(hdr + 8, 0);
    w32(hdr + 12, 0);
    wp(hdr + PTR, src);
    mem::putp(out, hdr);
    S_OK
}

fn WindowsDeleteString(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h != 0 && r32(h) & REF == 0 && mem::atomic_rmw32(count(h), |v| v.saturating_sub(1)) == 1 {
        mem::heap_free(h);
    }
    S_OK
}

/// WindowsDuplicateString(string, &copy): one more reference, or a copy of a reference string
fn WindowsDuplicateString(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_INVALIDARG;
    }
    let d = if h == 0 {
        0
    } else if r32(h) & REF != 0 {
        match create(&chars(h)) {
            Some(d) => d,
            None => return E_OUTOFMEMORY,
        }
    } else {
        mem::atomic_rmw32(count(h), |v| v + 1);
        h
    };
    mem::putp(out, d);
    S_OK
}

/// WindowsGetStringRawBuffer(string, &length): the characters (an empty string's: "")
fn WindowsGetStringRawBuffer(c: &mut Ctx) -> u32 {
    static EMPTY: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let (h, n) = (c.a(0), c.a(1));
    if n != 0 {
        w32(n, len(h));
    }
    if h == 0 { *EMPTY.get_or_init(|| mem::heap_alloc(4, true)) } else { rp(h + PTR) }
}

fn WindowsGetStringLen(c: &mut Ctx) -> u32 {
    len(c.a(0))
}

fn WindowsIsStringEmpty(c: &mut Ctx) -> u32 {
    (len(c.a(0)) == 0) as u32
}

/// Ordinal (code unit by code unit) order of two strings: -1, 0 or 1
pub fn ordinal_cmp(a: &[u16], b: &[u16]) -> i32 {
    a.cmp(b) as i32
}

/// WindowsCompareStringOrdinal(a, b, &result)
fn WindowsCompareStringOrdinal(c: &mut Ctx) -> u32 {
    let (a, b, r) = (c.a(0), c.a(1), c.a(2));
    if r == 0 {
        return E_INVALIDARG;
    }
    w32(r, ordinal_cmp(&chars(a), &chars(b)) as u32);
    S_OK
}

fn class_name(h: u32) -> String {
    String::from_utf16_lossy(&chars(h))
}

/// RoGetActivationFactory(class, iid, &factory) / RoActivateInstance(class, &instance): only
/// Windows.UI.ViewManagement.UISettings (AdvHD x64 reads the theme's colors) and
/// AccessibilitySettings (Unity 6's App UI plugin asks for high contrast); C++/WinRT throws, and
/// the program terminates, when one is missing
fn RoGetActivationFactory(c: &mut Ctx) -> u32 {
    if let (Some(k), true) = (ui::Class::named(&class_name(c.a(0))), c.a(2) != 0) {
        let f = ui::factory(k);
        return ui::qi_into(f, c.a(1), c.a(2));
    }
    if crate::env_on!("SAKE_TRACE_COM") {
        eprintln!("sake: RoGetActivationFactory {}: not registered", class_name(c.a(0)));
    }
    if c.a(2) != 0 {
        mem::putp(c.a(2), 0);
    }
    REGDB_E_CLASSNOTREG
}
fn RoActivateInstance(c: &mut Ctx) -> u32 {
    if let (Some(k), true) = (ui::Class::named(&class_name(c.a(0))), c.a(1) != 0) {
        let o = ui::object(k);
        mem::putp(c.a(1), o);
        return if o == 0 { E_OUTOFMEMORY } else { S_OK };
    }
    if crate::env_on!("SAKE_TRACE_COM") {
        eprintln!("sake: RoActivateInstance {}: not registered", class_name(c.a(0)));
    }
    if c.a(1) != 0 {
        mem::putp(c.a(1), 0);
    }
    REGDB_E_CLASSNOTREG
}

const UI_SETTINGS: &str = "Windows.UI.ViewManagement.UISettings";
const ACCESSIBILITY: &str = "Windows.UI.ViewManagement.AccessibilitySettings";

/// Windows.UI.ViewManagement.UISettings: the light theme, default metrics, 100% text, no change
/// events ever raised; AccessibilitySettings: no high contrast. One block per object: an interface's vtable pointer at word k (the
/// interface pointer is block + k * pointer size), then the reference count.
mod ui {
    use super::*;
    use crate::api::{host_fn, Handler};
    use std::sync::OnceLock;

    const E_NOINTERFACE: u32 = 0x8000_4002;

    type Guid = (u32, u16, u16, [u8; 8]);
    const fn g(a: u32, b: u16, c: u16, d: [u8; 8]) -> Guid {
        (a, b, c, d)
    }
    const IUNKNOWN: Guid = g(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    const IINSPECTABLE: Guid = g(0xaf86e2e0, 0xb12d, 0x4c6a, [0x9c, 0x5a, 0xd7, 0xaa, 0x65, 0x10, 0x1e, 0x90]);
    const IAGILE: Guid = g(0x94ea2b94, 0xe9cc, 0x49e0, [0xc0, 0xff, 0xee, 0x64, 0xca, 0x8f, 0x5b, 0x90]);
    const IFACTORY: Guid = g(0x35, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
    /// IUISettings, IUISettings2..5 (slots 0..4 of a settings object)
    const SETTINGS: [Guid; 5] = [
        g(0x85361600, 0x1c63, 0x4627, [0xbc, 0xb1, 0x3a, 0x89, 0xe0, 0xbc, 0x9c, 0x55]),
        g(0xbad82401, 0x2721, 0x44f9, [0xbb, 0x91, 0x2b, 0xb2, 0x28, 0xbe, 0x44, 0x2f]),
        g(0x03021be4, 0x5254, 0x4781, [0x81, 0x94, 0x51, 0x68, 0xf7, 0xd0, 0x6d, 0x7b]),
        g(0x52bb3002, 0x919b, 0x4d6b, [0x9b, 0x78, 0x8d, 0xd6, 0x6f, 0xf4, 0xb9, 0x3b]),
        g(0x5349d588, 0x0cb5, 0x5f05, [0xbd, 0x34, 0x70, 0x6b, 0x32, 0x31, 0xf0, 0xbd]),
    ];

    /// IAccessibilitySettings
    const ACCESS: Guid = g(0xfe0e8147, 0xc4c0, 0x4562, [0xb9, 0x62, 0x13, 0x27, 0xb5, 0x2a, 0xd5, 0xb9]);

    /// The runtime classes there are
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum Class {
        Settings,
        Access,
    }
    impl Class {
        pub fn named(s: &str) -> Option<Class> {
            match s {
                UI_SETTINGS => Some(Class::Settings),
                ACCESSIBILITY => Some(Class::Access),
                _ => None,
            }
        }
        pub(super) fn name(self) -> &'static str {
            match self {
                Class::Settings => UI_SETTINGS,
                Class::Access => ACCESSIBILITY,
            }
        }
    }
    /// What an object is: a class's factory or an instance of it
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Kind {
        Factory(Class),
        Object(Class),
    }
    fn kind(block: u32) -> Kind {
        let v = rp(block);
        if v == factory_vtable(Class::Settings) {
            Kind::Factory(Class::Settings)
        } else if v == factory_vtable(Class::Access) {
            Kind::Factory(Class::Access)
        } else if v == access_vtable() {
            Kind::Object(Class::Access)
        } else {
            Kind::Object(Class::Settings)
        }
    }
    /// An object's interfaces after IInspectable's, by slot
    fn ifaces(k: Kind) -> &'static [Guid] {
        match k {
            Kind::Factory(_) => &[IFACTORY],
            Kind::Object(Class::Settings) => &SETTINGS,
            Kind::Object(Class::Access) => &[ACCESS],
        }
    }
    fn count(block: u32) -> u32 {
        ifaces(kind(block)).len() as u32
    }
    /// (block, slot) of an interface pointer
    fn split(this: u32) -> (u32, u32) {
        // a vtable's first word after the IInspectable methods tells nothing; the slot is kept
        // in the vtable's word -1 instead
        let k = r32(rp(this) - 4);
        (this - k * ps(), k)
    }
    fn refs(block: u32) -> u32 {
        block + count(block) * ps()
    }

    fn qi(c: &mut Ctx) -> u32 {
        let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
        qi_into(split(this).0, iid, out)
    }
    /// QueryInterface on the object at `block` into `out`
    pub fn qi_into(block: u32, iid: u32, out: u32) -> u32 {
        let want = crate::com::read_guid(iid);
        let k = if [IUNKNOWN, IINSPECTABLE, IAGILE].contains(&want) {
            Some(0)
        } else {
            ifaces(kind(block)).iter().position(|s| *s == want).map(|k| k as u32)
        };
        let Some(k) = k else {
            if crate::env_on!("SAKE_TRACE_COM") {
                eprintln!("sake: {:?}: no interface {want:x?}", kind(block));
            }
            mem::putp(out, 0);
            return E_NOINTERFACE;
        };
        w32(refs(block), r32(refs(block)) + 1);
        mem::putp(out, block + k * ps());
        S_OK
    }
    fn add_ref(c: &mut Ctx) -> u32 {
        let b = split(c.a(0)).0;
        w32(refs(b), r32(refs(b)) + 1);
        r32(refs(b))
    }
    /// (an instance's block is freed at its last reference; a factory stays: one per class)
    fn release(c: &mut Ctx) -> u32 {
        let b = split(c.a(0)).0;
        let n = r32(refs(b)).saturating_sub(1);
        w32(refs(b), n);
        if n == 0 && matches!(kind(b), Kind::Object(_)) {
            mem::heap_free(b);
        }
        n
    }
    fn get_iids(c: &mut Ctx) -> u32 {
        w32(c.a(1), 0);
        mem::putp(c.a(2), 0);
        S_OK
    }
    fn class_name(c: &mut Ctx) -> u32 {
        let (Kind::Factory(k) | Kind::Object(k)) = kind(split(c.a(0)).0);
        let s: Vec<u16> = k.name().encode_utf16().collect();
        match create(&s) {
            Some(h) => {
                mem::putp(c.a(1), h);
                S_OK
            }
            None => E_OUTOFMEMORY,
        }
    }
    fn trust(c: &mut Ctx) -> u32 {
        w32(c.a(1), 0);
        S_OK
    }
    const INSPECTABLE: [(Handler, u32); 6] = [(qi, 12), (add_ref, 4), (release, 4), (get_iids, 12), (class_name, 8), (trust, 8)];

    /// A vtable of `methods` after IInspectable's, its slot `k` in the word before it.
    fn vtable(name: &str, k: u32, methods: &[(Handler, u32)]) -> u32 {
        let all: Vec<(Handler, u32)> = INSPECTABLE.iter().chain(methods).copied().collect();
        let t = mem::heap_alloc(8 + ps() * all.len() as u32, true) + 8;
        w32(t - 4, k);
        for (i, (f, bytes)) in all.iter().enumerate() {
            wp(t + ps() * i as u32, host_fn(&format!("winrt::{name}::{i}"), *f, *bytes));
        }
        t
    }

    /// IActivationFactory::ActivateInstance(&instance)
    fn activate(c: &mut Ctx) -> u32 {
        let Kind::Factory(k) = kind(split(c.a(0)).0) else { return E_NOINTERFACE };
        let o = object(k);
        mem::putp(c.a(1), o);
        if o == 0 { E_OUTOFMEMORY } else { S_OK }
    }
    fn factory_vtable(k: Class) -> u32 {
        static V: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
        *V[k as usize].get_or_init(|| vtable(&format!("{k:?}Factory"), 0, &[(activate, 8)]))
    }
    /// The class's factory (one per program)
    pub fn factory(k: Class) -> u32 {
        static F: [OnceLock<u32>; 2] = [OnceLock::new(), OnceLock::new()];
        *F[k as usize].get_or_init(|| {
            let b = mem::heap_alloc(ps() + 4, true);
            wp(b, factory_vtable(k));
            w32(b + ps(), 1);
            b
        })
    }
    /// A new instance (one reference), as its IInspectable
    pub fn object(k: Class) -> u32 {
        match k {
            Class::Settings => settings(),
            Class::Access => access(),
        }
    }

    fn out32(v: u32) -> impl Fn(&mut Ctx) -> u32 {
        move |c: &mut Ctx| {
            w32(c.a(1), v);
            S_OK
        }
    }
    fn hand(c: &mut Ctx) -> u32 {
        out32(1)(c) // RightHanded
    }
    fn size(c: &mut Ctx, w: f32) -> u32 {
        w32(c.a(1), w.to_bits());
        w32(c.a(1) + 4, w.to_bits());
        S_OK
    }
    fn cursor(c: &mut Ctx) -> u32 {
        size(c, 32.0)
    }
    fn scrollbar(c: &mut Ctx) -> u32 {
        size(c, 17.0)
    }
    fn duration(c: &mut Ctx) -> u32 {
        out32(5)(c)
    }
    fn yes(c: &mut Ctx) -> u32 {
        mem::write(c.a(1), &[1]);
        S_OK
    }
    fn no(c: &mut Ctx) -> u32 {
        mem::write(c.a(1), &[0]);
        S_OK
    }
    fn blink(c: &mut Ctx) -> u32 {
        out32(530)(c)
    }
    fn one(c: &mut Ctx) -> u32 {
        out32(1)(c)
    }
    fn dblclick(c: &mut Ctx) -> u32 {
        out32(500)(c)
    }
    fn hover(c: &mut Ctx) -> u32 {
        out32(400)(c)
    }
    /// Color { A, R, G, B }
    fn put_color(p: u32, (r, g, b): (u8, u8, u8)) -> u32 {
        mem::write(p, &[255, r, g, b]);
        S_OK
    }
    /// UIElementColor(type, &color): GetSysColor's, by UIElementType
    fn element_color(c: &mut Ctx) -> u32 {
        let rgb = match c.a(1) {
            // ActiveCaption, Background, ButtonFace, ButtonText, CaptionText, GrayText,
            // Highlight, HighlightText, Hotlight, InactiveCaption, InactiveCaptionText, Window,
            // WindowText
            0 => (0x99, 0xb4, 0xd1),
            1 => (0, 0, 0),
            2 => (0xf0, 0xf0, 0xf0),
            3 | 4 | 12 => (0, 0, 0),
            5 => (0x6d, 0x6d, 0x6d),
            6 => (0, 0x78, 0xd7),
            7 | 11 => (0xff, 0xff, 0xff),
            8 => (0, 0x66, 0xcc),
            9 => (0xbf, 0xcd, 0xdb),
            10 => (0, 0, 0),
            _ => (0xff, 0xff, 0xff),
        };
        put_color(c.a(2), rgb)
    }
    fn text_scale(c: &mut Ctx) -> u32 {
        crate::mem::w64(c.a(1), 1.0f64.to_bits());
        S_OK
    }
    /// add_*Changed(handler, &token): never raised
    fn add_event(c: &mut Ctx) -> u32 {
        crate::mem::w64(c.a(2), 1);
        S_OK
    }
    fn remove_event(_: &mut Ctx) -> u32 {
        S_OK
    }
    /// GetColorValue(UIColorType, &color): the light theme, Windows' default blue accent
    fn color_value(c: &mut Ctx) -> u32 {
        let rgb = match c.a(1) {
            0 => (0xff, 0xff, 0xff),
            1 => (0, 0, 0),
            2 => (0x00, 0x26, 0x42),
            3 => (0x00, 0x42, 0x75),
            4 => (0x00, 0x5a, 0x9e),
            5 => (0x00, 0x78, 0xd7),
            6 => (0x42, 0x9c, 0xe3),
            7 => (0x76, 0xb9, 0xed),
            8 => (0xa6, 0xd8, 0xff),
            _ => (0xff, 0xff, 0xff),
        };
        put_color(c.a(2), rgb)
    }

    fn vtables() -> &'static [u32; 5] {
        static V: OnceLock<[u32; 5]> = OnceLock::new();
        // (x86 argument bytes: this and the arguments; an event token is 8)
        let ev: [(Handler, u32); 2] = [(add_event, 12), (remove_event, 12)];
        V.get_or_init(|| {
            [
                vtable(
                    "IUISettings",
                    0,
                    &[
                        (hand, 8), (cursor, 8), (scrollbar, 8), (scrollbar, 8), (scrollbar, 8), (duration, 8), (yes, 8), (no, 8),
                        (blink, 8), (one, 8), (dblclick, 8), (hover, 8), (element_color, 12),
                    ],
                ),
                vtable("IUISettings2", 1, &[(text_scale, 8), ev[0], ev[1]]),
                vtable("IUISettings3", 2, &[(color_value, 12), ev[0], ev[1]]),
                vtable("IUISettings4", 3, &[(yes, 8), ev[0], ev[1]]),
                vtable("IUISettings5", 4, &[(yes, 8), ev[0], ev[1]]),
            ]
        })
    }
    /// HighContrastScheme(&name): none (the empty string)
    fn scheme(c: &mut Ctx) -> u32 {
        mem::putp(c.a(1), 0);
        S_OK
    }
    fn access_vtable() -> u32 {
        static V: OnceLock<u32> = OnceLock::new();
        *V.get_or_init(|| vtable("IAccessibilitySettings", 0, &[(no, 8), (scheme, 8), (add_event, 12), (remove_event, 12)]))
    }
    fn access() -> u32 {
        let b = mem::heap_alloc(ps() + 4, true);
        if b == 0 {
            return 0;
        }
        wp(b, access_vtable());
        w32(b + ps(), 1);
        b
    }
    /// A new settings object (one reference), as its IInspectable.
    fn settings() -> u32 {
        let v = vtables();
        let b = mem::heap_alloc(ps() * 5 + 4, true);
        if b == 0 {
            return 0;
        }
        for (k, t) in v.iter().enumerate() {
            wp(b + ps() * k as u32, *t);
        }
        w32(b + 5 * ps(), 1);
        b
    }
}

pub fn init() {
    // (RoInitialize initializes COM on the thread as CoInitializeEx does: S_OK, then S_FALSE)
    implement("combase", "RoInitialize", crate::misc::com_enter);
    implement("combase", "RoUninitialize", crate::misc::com_leave);
    for (n, f) in [
        ("WindowsCreateString", WindowsCreateString as crate::api::Handler),
        ("WindowsCreateStringReference", WindowsCreateStringReference),
        ("WindowsDeleteString", WindowsDeleteString),
        ("WindowsDuplicateString", WindowsDuplicateString),
        ("WindowsGetStringRawBuffer", WindowsGetStringRawBuffer),
        ("WindowsGetStringLen", WindowsGetStringLen),
        ("WindowsIsStringEmpty", WindowsIsStringEmpty),
        ("WindowsCompareStringOrdinal", WindowsCompareStringOrdinal),
        ("RoGetActivationFactory", RoGetActivationFactory),
        ("RoActivateInstance", RoActivateInstance),
    ] {
        implement("combase", n, f);
    }
    // error origination (C++/WinRT reports each failed HRESULT): nothing kept; TRUE as "the
    // error was originated"
    for n in ["RoOriginateLanguageException", "RoOriginateError", "RoOriginateErrorW", "RoTransformError", "RoTransformErrorW"] {
        implement("combase", n, |_| 1);
    }
    for n in ["RoCaptureErrorContext", "RoClearError", "SetRestrictedErrorInfo", "RoSetErrorReportingFlags", "RoReportUnhandledError"] {
        implement("combase", n, |_| S_OK);
    }
    // GetRestrictedErrorInfo(&info): none (S_FALSE); RoGetErrorReportingFlags(&flags): 0
    implement("combase", "GetRestrictedErrorInfo", |c| {
        if c.a(0) != 0 {
            mem::putp(c.a(0), 0);
        }
        1
    });
    implement("combase", "RoGetErrorReportingFlags", |c| {
        if c.a(0) != 0 {
            w32(c.a(0), 0);
        }
        S_OK
    });
}

#[cfg(test)]
mod tests {
    use super::ordinal_cmp;
    use super::ui::Class;
    use proptest::prelude::*;

    #[test]
    fn classes_by_name() {
        assert_eq!(Class::named("Windows.UI.ViewManagement.UISettings"), Some(Class::Settings));
        assert_eq!(Class::named("Windows.UI.ViewManagement.AccessibilitySettings"), Some(Class::Access));
        // names are exact: no case folding, no prefixes
        assert_eq!(Class::named("windows.ui.viewmanagement.uisettings"), None);
        assert_eq!(Class::named("Windows.UI.ViewManagement.UISettings2"), None);
        assert_eq!(Class::named("Windows.Gaming.Input.Gamepad"), None);
        assert_eq!(Class::named(""), None);
        for k in [Class::Settings, Class::Access] {
            assert_eq!(Class::named(k.name()), Some(k));
        }
    }

    proptest! {
        /// only the two class names are registered
        #[test]
        fn unknown_names(s in "\\PC{0,60}") {
            let known = ["Windows.UI.ViewManagement.UISettings", "Windows.UI.ViewManagement.AccessibilitySettings"];
            prop_assert_eq!(Class::named(&s).is_some(), known.contains(&s.as_str()));
        }
    }

    #[test]
    fn ordinal_order() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(ordinal_cmp(&w("abc"), &w("abc")), 0);
        assert_eq!(ordinal_cmp(&w("ab"), &w("abc")), -1);
        assert_eq!(ordinal_cmp(&w("b"), &w("abc")), 1);
        // by code unit, not by culture: "B" (0x42) before "a" (0x61)
        assert_eq!(ordinal_cmp(&w("B"), &w("a")), -1);
        assert_eq!(ordinal_cmp(&[], &w("a")), -1);
    }
}
