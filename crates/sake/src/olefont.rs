//! OLE font objects backed by the ordinary GDI font implementation.
#![allow(non_snake_case)]
use crate::api::{self, Ctx, Handler};
use crate::mem::{self, ps, r16, r32, r64, rp, w16, w32, w64, wp};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
const POINTER: u32 = 0x80004003;
const INVALID: u32 = 0x80070057;
const NOINTERFACE: u32 = 0x80004002;
#[derive(Clone, PartialEq)]
struct Properties {
    name: Vec<u16>,
    size: i64,
    weight: i16,
    charset: i16,
    italic: bool,
    underline: bool,
    strike: bool,
}
impl Default for Properties {
    fn default() -> Self {
        Self {
            name: "MS Sans Serif".encode_utf16().collect(),
            size: 82500,
            weight: 400,
            charset: 0,
            italic: false,
            underline: false,
            strike: false,
        }
    }
}
struct Font {
    props: Properties,
    refs: u32,
    current: u32,
    handles: HashMap<u32, u32>,
    ratio: (i32, i32),
    hdc: u32,
}
static FONTS: Mutex<Option<HashMap<u32, Font>>> = Mutex::new(None);
fn with<R>(this: u32, f: impl FnOnce(&mut Font) -> R) -> Option<R> {
    let base = rp(this + ps());
    FONTS.lock().unwrap().as_mut()?.get_mut(&base).map(f)
}
fn is_iid(p: u32, id: u32, tail: &[u8]) -> bool {
    p != 0 && r32(p) == id && mem::slice(p + 4, 12) == tail
}
const OLETAIL: [u8; 12] = [
    0x74, 0xa8, 0x1a, 0x10, 0x8b, 0xba, 0, 0xaa, 0, 0x30, 0x0c, 0xab,
];
const COMTAIL: [u8; 12] = [0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
fn iid_offset(p: u32) -> Option<u32> {
    if is_iid(p, 0, &COMTAIL) || is_iid(p, 0xbef6e002, &OLETAIL) {
        Some(0)
    } else if is_iid(p, 0x20400, &COMTAIL) || is_iid(p, 0xbef6e003, &OLETAIL) {
        Some(2 * ps())
    } else {
        None
    }
}
fn query(c: &mut Ctx) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return POINTER;
    }
    wp(out, 0);
    let Some(offset) = iid_offset(iid) else {
        return NOINTERFACE;
    };
    let base = rp(this + ps());
    if with(this, |f| f.refs += 1).is_none() {
        return INVALID;
    }
    wp(out, base + offset);
    0
}
fn add(c: &mut Ctx) -> u32 {
    with(c.a(0), |f| {
        f.refs += 1;
        f.refs
    })
    .unwrap_or(0)
}
fn release(c: &mut Ctx) -> u32 {
    let base = rp(c.a(0) + ps());
    let removed = {
        let mut guard = FONTS.lock().unwrap();
        let Some(map) = guard.as_mut() else { return 0 };
        let Some(f) = map.get_mut(&base) else {
            return 0;
        };
        f.refs = f.refs.saturating_sub(1);
        if f.refs != 0 {
            return f.refs;
        }
        map.remove(&base)
    };
    if let Some(f) = removed {
        for h in f.handles.keys() {
            c.call(api::export("gdi32", "DeleteObject"), &[*h]);
        }
        mem::heap_free(base);
    }
    0
}
fn read(c: &mut Ctx, id: u32) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return POINTER;
    }
    let Some(p) = with(c.a(0), |f| f.props.clone()) else {
        return INVALID;
    };
    match id {
        0 => wp(out, crate::misc::bstr_alloc(&p.name)),
        2 => w64(out, p.size as u64),
        3 => w32(out, (p.weight >= 550) as u32),
        4 => w32(out, p.italic as u32),
        5 => w32(out, p.underline as u32),
        6 => w32(out, p.strike as u32),
        7 => w16(out, p.weight as u16),
        8 => w16(out, p.charset as u16),
        _ => return INVALID,
    }
    0
}
fn change(this: u32, id: u32, value: u64) -> u32 {
    if id == 0 && value == 0 {
        return INVALID;
    }
    if id == 2 && (value as i64) < 0 {
        return INVALID;
    }
    let name = if id == 0 {
        Some(mem::wstr(value as u32))
    } else {
        None
    };
    with(this, |f| {
        match id {
            0 => f.props.name = name.unwrap(),
            2 => f.props.size = value as i64,
            3 => f.props.weight = if value != 0 { 700 } else { 400 },
            4 => f.props.italic = value != 0,
            5 => f.props.underline = value != 0,
            6 => f.props.strike = value != 0,
            7 => f.props.weight = value as i16,
            8 => f.props.charset = value as i16,
            _ => return INVALID,
        }
        f.current = 0;
        0
    })
    .unwrap_or(INVALID)
}
macro_rules! property {
    ($get:ident,$put:ident,$id:expr) => {
        fn $get(c: &mut Ctx) -> u32 {
            read(c, $id)
        }
        fn $put(c: &mut Ctx) -> u32 {
            let v = c.a(1);
            change(c.a(0), $id, v as u64)
        }
    };
}
property!(get_name, put_name, 0);
property!(get_bold, put_bold, 3);
property!(get_italic, put_italic, 4);
property!(get_underline, put_underline, 5);
property!(get_strike, put_strike, 6);
property!(get_weight, put_weight, 7);
property!(get_charset, put_charset, 8);
fn get_size(c: &mut Ctx) -> u32 {
    read(c, 2)
}
fn put_size(c: &mut Ctx) -> u32 {
    let value = if mem::x64() {
        c.a64(1)
    } else {
        c.a(1) as u64 | ((c.a(2) as u64) << 32)
    };
    change(c.a(0), 2, value)
}
fn hfont(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return POINTER;
    }
    let Some((p, h, ratio)) = with(this, |f| (f.props.clone(), f.current, f.ratio)) else {
        return INVALID;
    };
    if h != 0 {
        wp(out, h);
        return 0;
    }
    let buffer = mem::heap_alloc(92, true);
    if buffer == 0 {
        return 0x8007000e;
    }
    // CY is points * 10000; a point is 2540 / 72 HIMETRIC units.
    let height = -((p.size as i128 * 2540 * ratio.0 as i128 + 360000 * ratio.1 as i128)
        / (720000 * ratio.1 as i128)) as i32;
    w32(buffer, height as u32);
    w32(buffer + 16, p.weight as u32);
    mem::w8(buffer + 20, p.italic as u8);
    mem::w8(buffer + 21, p.underline as u8);
    mem::w8(buffer + 22, p.strike as u8);
    mem::w8(buffer + 23, p.charset as u8);
    for (i, ch) in p.name.iter().take(31).enumerate() {
        w16(buffer + 28 + 2 * i as u32, *ch)
    }
    let h = c.call(api::export("gdi32", "CreateFontIndirectW"), &[buffer]);
    mem::heap_free(buffer);
    if h == 0 {
        return 0x8007000e;
    }
    with(this, |f| {
        f.current = h;
        f.handles.insert(h, 0);
    });
    wp(out, h);
    0
}
fn clone_font(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return POINTER;
    }
    let Some((p, r)) = with(c.a(0), |f| (f.props.clone(), f.ratio)) else {
        return INVALID;
    };
    wp(out, make(p, r));
    0
}
fn equal(c: &mut Ctx) -> u32 {
    let other = c.a(1);
    let Some(a) = with(c.a(0), |f| f.props.clone()) else {
        return INVALID;
    };
    if other == 0 {
        return POINTER;
    }
    let Some(b) = with(other, |f| f.props.clone()) else {
        return NOINTERFACE;
    };
    if a == b { 0 } else { 1 }
}
fn ratio(c: &mut Ctx) -> u32 {
    let (a, b) = (c.i(1), c.i(2));
    if a <= 0 || b <= 0 {
        return INVALID;
    }
    with(c.a(0), |f| {
        f.ratio = (a, b);
        f.current = 0;
        0
    })
    .unwrap_or(INVALID)
}
fn metrics(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return POINTER;
    }
    let buffer = mem::heap_alloc(ps(), true);
    if buffer == 0 {
        return 0x8007000e;
    }
    // Call get_hFont with its ordinary ABI through the vtable, then measure in GDI.
    let this = c.a(0);
    let hr = c.call(rp(rp(this) + 19 * ps()), &[this, buffer]);
    let h = rp(buffer);
    mem::heap_free(buffer);
    if hr != 0 {
        return hr;
    }
    let dc = c.call(api::export("gdi32", "CreateCompatibleDC"), &[0]);
    let old = c.call(api::export("gdi32", "SelectObject"), &[dc, h]);
    let ok = c.call(api::export("gdi32", "GetTextMetricsW"), &[dc, out]);
    c.call(api::export("gdi32", "SelectObject"), &[dc, old]);
    c.call(api::export("gdi32", "DeleteDC"), &[dc]);
    if ok != 0 { 0 } else { INVALID }
}
fn add_hfont(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    with(c.a(0), |f| {
        let Some(n) = f.handles.get_mut(&h) else {
            return INVALID;
        };
        *n += 1;
        0
    })
    .unwrap_or(INVALID)
}
fn release_hfont(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    with(c.a(0), |f| {
        let Some(n) = f.handles.get_mut(&h) else {
            return INVALID;
        };
        if *n == 0 {
            return INVALID;
        }
        *n -= 1;
        0
    })
    .unwrap_or(INVALID)
}
fn set_hdc(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    with(c.a(0), |f| {
        f.hdc = h;
        0
    })
    .unwrap_or(INVALID)
}
fn type_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return POINTER;
    }
    w32(c.a(1), 0);
    0
}
fn type_info(c: &mut Ctx) -> u32 {
    if c.a(3) == 0 {
        return POINTER;
    }
    wp(c.a(3), 0);
    0x8002000b
}
fn ids(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 || c.a(5) == 0 {
        return POINTER;
    }
    let mut hr = 0;
    for i in 0..c.a(3) {
        let name = String::from_utf16_lossy(&mem::wstr(rp(c.a(2) + i * ps()))).to_lowercase();
        let id = match name.as_str() {
            "name" => 0,
            "size" => 2,
            "bold" => 3,
            "italic" => 4,
            "underline" => 5,
            "strikethrough" => 6,
            "weight" => 7,
            "charset" => 8,
            _ => {
                hr = 0x80020006;
                u32::MAX
            }
        };
        w32(c.a(5) + 4 * i, id)
    }
    hr
}
fn invoke(c: &mut Ctx) -> u32 {
    let (this, id, flags, params, result) = (c.a(0), c.a(1), c.a(4), c.a(5), c.a(6));
    if !matches!(id, 0 | 2..=8) {
        return 0x80020003;
    }
    if params == 0 {
        return POINTER;
    }
    let argc = r32(params + 2 * ps());
    if flags & 2 != 0 {
        if argc != 0 {
            return 0x8002000e;
        }
        if result == 0 {
            return POINTER;
        }
        mem::write(result, &vec![0; if mem::x64() { 24 } else { 16 }]);
        let Some(p) = with(this, |f| f.props.clone()) else {
            return INVALID;
        };
        match id {
            0 => {
                w16(result, 8);
                wp(result + 8, crate::misc::bstr_alloc(&p.name))
            }
            2 => {
                w16(result, 6);
                w64(result + 8, p.size as u64)
            }
            7 | 8 => {
                w16(result, 2);
                w16(
                    result + 8,
                    if id == 7 { p.weight } else { p.charset } as u16,
                )
            }
            _ => {
                w16(result, 11);
                let b = match id {
                    3 => p.weight >= 550,
                    4 => p.italic,
                    5 => p.underline,
                    _ => p.strike,
                };
                w16(result + 8, if b { 0xffff } else { 0 })
            }
        }
        return 0;
    }
    if flags & 12 != 0 {
        if argc != 1 {
            return 0x8002000e;
        }
        let v = rp(params);
        if v == 0 {
            return POINTER;
        }
        let expected = match id {
            0 => 8,
            2 => 6,
            7 | 8 => 2,
            _ => 11,
        };
        if r16(v) != expected {
            return 0x80020005;
        }
        let value = match expected {
            8 => rp(v + 8) as u64,
            6 => r64(v + 8),
            _ => r16(v + 8) as u64,
        };
        return change(this, id, value);
    }
    0x80020003
}
fn make(props: Properties, scale: (i32, i32)) -> u32 {
    static VT: OnceLock<(u32, u32)> = OnceLock::new();
    let (vt, disp) = *VT.get_or_init(|| {
        let m: [(Handler, u32); 27] = [
            (query, 12),
            (add, 4),
            (release, 4),
            (get_name, 8),
            (put_name, 8),
            (get_size, 8),
            (put_size, 12),
            (get_bold, 8),
            (put_bold, 8),
            (get_italic, 8),
            (put_italic, 8),
            (get_underline, 8),
            (put_underline, 8),
            (get_strike, 8),
            (put_strike, 8),
            (get_weight, 8),
            (put_weight, 8),
            (get_charset, 8),
            (put_charset, 8),
            (hfont, 8),
            (clone_font, 8),
            (equal, 8),
            (ratio, 12),
            (metrics, 8),
            (add_hfont, 8),
            (release_hfont, 8),
            (set_hdc, 8),
        ];
        (
            api::vtable("IFont", &m),
            api::vtable(
                "IFontDisp",
                &[
                    (query, 12),
                    (add, 4),
                    (release, 4),
                    (type_count, 8),
                    (type_info, 16),
                    (ids, 24),
                    (invoke, 36),
                ],
            ),
        )
    });
    let base = mem::heap_alloc(4 * ps(), true);
    if base == 0 {
        return 0;
    }
    wp(base, vt);
    wp(base + ps(), base);
    wp(base + 2 * ps(), disp);
    wp(base + 3 * ps(), base);
    FONTS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(
            base,
            Font {
                props,
                refs: 1,
                current: 0,
                handles: HashMap::new(),
                ratio: scale,
                hdc: 0,
            },
        );
    base
}
fn OleCreateFontIndirect(c: &mut Ctx) -> u32 {
    let (desc, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 || iid == 0 {
        return POINTER;
    }
    wp(out, 0);
    let Some(offset) = iid_offset(iid) else {
        return NOINTERFACE;
    };
    let mut p = Properties::default();
    if desc != 0 {
        let name_at = if mem::x64() { 8 } else { 4 };
        let size_at = if mem::x64() { 16 } else { 8 };
        if r32(desc) < size_at + 24 {
            return INVALID;
        }
        let name = rp(desc + name_at);
        if name != 0 {
            p.name = mem::wstr(name)
        }
        p.size = r64(desc + size_at) as i64;
        p.weight = r16(desc + size_at + 8) as i16;
        p.charset = r16(desc + size_at + 10) as i16;
        p.italic = r32(desc + size_at + 12) != 0;
        p.underline = r32(desc + size_at + 16) != 0;
        p.strike = r32(desc + size_at + 20) != 0;
        if p.size < 0 {
            return INVALID;
        }
    }
    let base = make(p, (96, 2540));
    if base == 0 {
        return 0x8007000e;
    }
    wp(out, base + offset);
    0
}
pub fn init() {
    api::implement("oleaut32", "OleCreateFontIndirect", OleCreateFontIndirect);
    api::implement("olepro32", "OleCreateFontIndirect", OleCreateFontIndirect);
}
