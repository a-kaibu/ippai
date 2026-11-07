//! WMI (wbemprox): a locator whose namespaces answer a query with one object of its class,
//! with plausible values for the properties games ask for (the processor, video card, OS,
//! memory) and VT_NULL for the rest. デカくてエッチな俺の姉 reads Win32_Processor without
//! checking that it got anything.

use crate::api::{Ctx, Handler};
use crate::mem;

const WBEM_S_FALSE: u32 = 1;
const WBEM_E_NOT_SUPPORTED: u32 = 0x8004_100c;

/// One object per interface (no state: the reference count does not matter).
fn singleton(name: &str, methods: &[(Handler, u32)], cell: &std::sync::OnceLock<u32>) -> u32 {
    *cell.get_or_init(|| {
        let o = mem::heap_alloc(8, true);
        mem::wp(o, crate::api::vtable(name, methods));
        o
    })
}

fn qi(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), c.a(0));
    0
}
fn one(_: &mut Ctx) -> u32 {
    1
}
fn not_supported(_: &mut Ctx) -> u32 {
    WBEM_E_NOT_SUPPORTED
}

/// CoCreateInstance(CLSID_WbemLocator)
pub fn locator() -> u32 {
    static L: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    singleton(
        "IWbemLocator",
        &[(qi, 12), (one, 4), (one, 4), (connect_server, 36)],
        &L,
    )
}

/// ConnectServer(this, resource, user, password, locale, flags, authority, ctx, &services)
fn connect_server(c: &mut Ctx) -> u32 {
    mem::putp(c.a(8), services());
    0
}

fn services() -> u32 {
    static S: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // (stack bytes of each method, `this` included)
    let sizes = [
        12, 4, 4, 24, 8, 12, 24, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 24, 24, 24,
        24, 32, 28,
    ];
    let mut m: Vec<(Handler, u32)> = sizes
        .iter()
        .map(|&b| (not_supported as Handler, b))
        .collect();
    m[0].0 = qi;
    m[1].0 = one;
    m[2].0 = one;
    m[12].0 = enum_at4; // CreateClassEnum(this, superclass, flags, ctx, &enum)
    m[18].0 = enum_at4; // CreateInstanceEnum(this, class, flags, ctx, &enum)
    m[20].0 = enum_at5; // ExecQuery(this, language, query, flags, ctx, &enum)
    singleton("IWbemServices", &m, &S)
}

fn enum_at4(c: &mut Ctx) -> u32 {
    let class = String::from_utf16_lossy(&mem::wstr(c.a(1)));
    mem::putp(c.a(4), new_enum(&class));
    0
}
fn enum_at5(c: &mut Ctx) -> u32 {
    let q = String::from_utf16_lossy(&mem::wstr(c.a(2)));
    let lower = q.to_ascii_lowercase();
    let class = lower
        .find(" from ")
        .map(|i| {
            q[i + 6..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    if crate::env_on!("SAKE_TRACE_COM") {
        eprintln!("sake: WMI query {q}");
    }
    mem::putp(c.a(5), new_enum(&class));
    0
}

const CLASSES: &[&str] = &[
    "win32_processor",
    "win32_videocontroller",
    "win32_operatingsystem",
    "win32_computersystem",
    "win32_physicalmemory",
];

/// An enumerator of one object of `class` (unknown classes: none). Layout: vtable, class
/// index + 1 (0: none), position.
fn new_enum(class: &str) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        crate::api::vtable(
            "IEnumWbemClassObject",
            &[
                (qi, 12),
                (one, 4),
                (one, 4),
                (reset, 4),
                (next, 20),
                (not_supported, 12),
                (clone, 8),
                (skip, 12),
            ],
        )
    });
    let o = mem::heap_alloc(12, true);
    mem::wp(o, vt);
    let k = CLASSES
        .iter()
        .position(|&n| n == class.to_ascii_lowercase())
        .map_or(0, |i| i as u32 + 1);
    mem::w32(o + 4, k);
    o
}
fn reset(c: &mut Ctx) -> u32 {
    mem::w32(c.a(0) + 8, 0);
    0
}
/// Next(this, timeout, count, objects, &returned)
fn next(c: &mut Ctx) -> u32 {
    let (this, count, out, ret) = (c.a(0), c.a(2), c.a(3), c.a(4));
    let k = mem::r32(this + 4);
    if k == 0 || mem::r32(this + 8) != 0 || count == 0 {
        mem::put32(ret, 0);
        return WBEM_S_FALSE;
    }
    mem::w32(this + 8, 1);
    mem::putp(out, object(k - 1));
    mem::put32(ret, 1);
    if count == 1 {
        0
    } else {
        WBEM_S_FALSE
    }
}
fn clone(c: &mut Ctx) -> u32 {
    let e = new_enum("");
    mem::w32(e + 4, mem::r32(c.a(0) + 4));
    mem::putp(c.a(1), e);
    0
}
fn skip(c: &mut Ctx) -> u32 {
    mem::w32(c.a(0) + 8, 1);
    WBEM_S_FALSE
}

/// The IWbemClassObject of class `k` (vtable, class index)
fn object(k: u32) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        let sizes = [
            12, 4, 4, 8, 24, 20, 8, 20, 8, 24, 4, 12, 8, 12, 12, 12, 12, 12, 8, 20, 20, 8, 8, 20,
            4, 12, 12,
        ];
        let mut m: Vec<(Handler, u32)> = sizes
            .iter()
            .map(|&b| (not_supported as Handler, b))
            .collect();
        m[0].0 = qi;
        m[1].0 = one;
        m[2].0 = one;
        m[4].0 = get;
        crate::api::vtable("IWbemClassObject", &m)
    });
    let o = mem::heap_alloc(8, true);
    mem::wp(o, vt);
    mem::w32(o + 4, k);
    o
}

enum Val {
    S(&'static str),
    I(i32),
    B(bool),
}

fn value(class: &str, prop: &str) -> Option<Val> {
    use Val::*;
    Some(match (class, prop.to_ascii_lowercase().as_str()) {
        ("win32_processor", "name") => S("Intel(R) Core(TM) i5-8400 CPU @ 2.80GHz"),
        ("win32_processor", "manufacturer") => S("GenuineIntel"),
        ("win32_processor", "numberofcores") => I(4),
        ("win32_processor", "numberoflogicalprocessors") => I(4),
        ("win32_processor", "maxclockspeed" | "currentclockspeed") => I(2800),
        ("win32_processor", "architecture") => I(0),
        ("win32_processor", "processorid") => S("BFEBFBFF000906EA"),
        ("win32_videocontroller", "name" | "caption" | "description" | "videoprocessor") => {
            S("WebGPU Display Adapter")
        }
        ("win32_videocontroller", "adapterram") => I(0x4000_0000),
        ("win32_videocontroller", "driverversion") => S("31.0.101.1"),
        // (the ids DXGI's adapter reports, kura/src/d3d11: Unity matches VEN_/DEV_ against them
        // and reads the string without checking for VT_NULL)
        ("win32_videocontroller", "pnpdeviceid") => {
            S("PCI\\VEN_10DE&DEV_1C82&SUBSYS_00000000&REV_A1\\4&1A2B3C4D&0&0008")
        }
        ("win32_videocontroller", "currenthorizontalresolution") => I(1920),
        ("win32_videocontroller", "currentverticalresolution") => I(1080),
        ("win32_videocontroller", "currentbitsperpixel") => I(32),
        ("win32_videocontroller", "currentrefreshrate") => I(60),
        ("win32_operatingsystem", "caption" | "name") => S("Microsoft Windows 10 Pro"),
        ("win32_operatingsystem", "version") => S("10.0.19045"),
        ("win32_operatingsystem", "buildnumber") => S("19045"),
        ("win32_operatingsystem", "osarchitecture") => S("64-bit"),
        // (uint64 properties are strings in a VARIANT)
        ("win32_operatingsystem", "totalvisiblememorysize" | "freephysicalmemory") => S("8388608"),
        ("win32_computersystem", "totalphysicalmemory") => S("8589934592"),
        ("win32_computersystem", "numberofprocessors") => I(1),
        ("win32_computersystem", "numberoflogicalprocessors") => I(4),
        ("win32_physicalmemory", "capacity") => S("8589934592"),
        ("win32_physicalmemory", "speed") => I(2666),
        (_, "__class") => return None,
        (_, "isenabled" | "status") => B(true),
        _ => return None,
    })
}

/// Get(this, name, flags, VARIANT* val, CIMTYPE* type, long* flavor)
fn get(c: &mut Ctx) -> u32 {
    let class = CLASSES[mem::r32(c.a(0) + 4) as usize];
    let prop = String::from_utf16_lossy(&mem::wstr(c.a(1)));
    let (v, ty) = (c.a(3), c.a(4));
    let got = if prop.eq_ignore_ascii_case("__class") {
        Some(Val::S(class))
    } else {
        value(class, &prop)
    };
    if crate::env_on!("SAKE_TRACE_COM") {
        eprintln!("sake: WMI {class}.{prop}");
    }
    // VARIANT: vt (u16), 3 reserved u16, the value
    if v != 0 {
        mem::fill(v, 16, 0);
        match got {
            Some(Val::S(s)) => {
                mem::w16(v, 8); // VT_BSTR
                mem::w32(
                    v + 8,
                    crate::misc::bstr_alloc(&s.encode_utf16().collect::<Vec<_>>()),
                );
            }
            Some(Val::I(i)) => {
                mem::w16(v, 3); // VT_I4
                mem::w32(v + 8, i as u32);
            }
            Some(Val::B(b)) => {
                mem::w16(v, 11); // VT_BOOL
                mem::w16(v + 8, if b { 0xffff } else { 0 });
            }
            None => mem::w16(v, 1), // VT_NULL
        }
    }
    if ty != 0 {
        // CIM_STRING 8, CIM_SINT32 3, CIM_BOOLEAN 11
        mem::put32(
            ty,
            match got {
                Some(Val::S(_)) => 8,
                Some(Val::I(_)) => 3,
                Some(Val::B(_)) => 11,
                None => 8,
            },
        );
    }
    if c.a(5) != 0 {
        mem::put32(c.a(5), 0);
    }
    0
}
