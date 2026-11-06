//! COM objects from the generated vtable tables (com_methods.rs, from Wine's d2d1, dwrite and
//! wincodec IDL): an object names its interface and the methods it implements; every other slot is
//! a stub that logs "Iface::Method" once and fails (E_NOTIMPL, 0.0 for float results, the hidden
//! result pointer for structure results). QueryInterface, AddRef and Release come for free:
//! QueryInterface answers the interface and every interface it derives from.

use crate::api::{host_fn, Ctx, Handler};
use crate::mem;
use std::collections::HashMap;
use std::sync::Mutex;

#[path = "com_methods.rs"]
mod methods;
pub use methods::Guid;

pub const E_NOTIMPL: u32 = 0x8000_4001;
pub const E_NOINTERFACE: u32 = 0x8000_4002;

struct Obj {
    iface: &'static str,
    refs: u32,
}

static OBJS: Mutex<Option<HashMap<u32, Obj>>> = Mutex::new(None);
/// trap address -> (interface, method, result kind)
static STUBS: Mutex<Option<HashMap<u32, (&'static str, &'static str, u8)>>> = Mutex::new(None);
/// trap address -> (method, stack argument dwords), for handlers shared by several methods
static NAMES: Mutex<Option<HashMap<u32, (&'static str, u8)>>> = Mutex::new(None);

/// The name of the method whose trap `trap` is.
pub fn method_name(trap: u32) -> Option<&'static str> {
    NAMES.lock().unwrap().as_ref()?.get(&trap).map(|m| m.0)
}
/// Its argument count (dwords, with `this` and a hidden result pointer).
pub fn method_dwords(trap: u32) -> Option<u32> {
    NAMES.lock().unwrap().as_ref()?.get(&trap).map(|m| m.1 as u32)
}
static VTBLS: Mutex<Option<HashMap<&'static str, u32>>> = Mutex::new(None);

fn objs<R>(f: impl FnOnce(&mut HashMap<u32, Obj>) -> R) -> R {
    f(OBJS.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn table(iface: &str) -> &'static (&'static str, &'static [(&'static str, u8, u8)], &'static [&'static str]) {
    methods::IFACES.iter().find(|t| t.0 == iface).unwrap_or_else(|| panic!("com: no interface {iface}"))
}

pub fn iid(iface: &str) -> Option<Guid> {
    methods::IIDS.iter().find(|t| t.0 == iface).map(|t| t.1)
}

pub fn read_guid(p: u32) -> Guid {
    let mut b = [0u8; 8];
    for (i, v) in b.iter_mut().enumerate() {
        *v = mem::r8(p + 8 + i as u32);
    }
    (mem::r32(p), mem::r16(p + 4), mem::r16(p + 6), b)
}

fn fmt_guid(g: Guid) -> String {
    format!("{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}", g.0, g.1, g.2, g.3[0], g.3[1], g.3[2..].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn warn_once(msg: &str) {
    static SEEN: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
    if SEEN.lock().unwrap().get_or_insert_with(Default::default).insert(msg.to_string()) {
        eprintln!("{msg}");
    }
}

fn stub(c: &mut Ctx) -> u32 {
    let (iface, name, kind) = STUBS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).copied()).unwrap_or(("?", "?", 0));
    warn_once(&format!("sake: com {iface}::{name} not implemented"));
    match kind {
        1 => {
            c.st0 = Some(0.0);
            0
        }
        2 => c.a(1),
        _ => E_NOTIMPL,
    }
}

fn query_interface(c: &mut Ctx) -> u32 {
    let (this, riid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return 0x8000_4003;
    }
    let want = read_guid(riid);
    let found = objs(|m| {
        let o = m.get_mut(&this)?;
        let hit = table(o.iface).2.iter().any(|n| iid(n) == Some(want)).then_some(this);
        if hit.is_some() {
            o.refs += 1;
        }
        hit
    });
    match found {
        Some(p) => {
            mem::putp(out, p);
            0
        }
        None => {
            let iface = objs(|m| m.get(&this).map_or("?", |o| o.iface));
            warn_once(&format!("sake: com {iface}::QueryInterface {{{}}}: no", fmt_guid(want)));
            mem::putp(out, 0);
            E_NOINTERFACE
        }
    }
}

fn add_ref(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    objs(|m| m.get_mut(&this).map_or(1, |o| {
        o.refs += 1;
        o.refs
    }))
}

fn release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    objs(|m| match m.get_mut(&this) {
        Some(o) if o.refs > 1 => {
            o.refs -= 1;
            o.refs
        }
        Some(_) => {
            // (the memory is left: a guest holding a stale pointer still finds a vtable)
            m.remove(&this);
            0
        }
        None => 0,
    })
}

/// The vtable of `iface` with the methods `impls` names; built once per interface.
pub fn vtable(iface: &'static str, impls: &[(&str, Handler)]) -> u32 {
    if let Some(&v) = VTBLS.lock().unwrap().get_or_insert_with(HashMap::new).get(iface) {
        return v;
    }
    let t = table(iface);
    let k = mem::ps();
    let v = mem::heap_alloc(k * t.1.len() as u32, true);
    for (i, &(name, dwords, kind)) in t.1.iter().enumerate() {
        let f: Handler = match impls.iter().find(|m| m.0 == name) {
            Some(m) => m.1,
            None if i == 0 => query_interface,
            None if i == 1 => add_ref,
            None if i == 2 => release,
            None => stub,
        };
        let a = host_fn(&format!("{iface}::{i}:{name}"), f, 4 * dwords as u32);
        if i > 2 && !impls.iter().any(|m| m.0 == name) {
            STUBS.lock().unwrap().get_or_insert_with(HashMap::new).insert(a, (iface, name, kind));
        }
        NAMES.lock().unwrap().get_or_insert_with(HashMap::new).insert(a, (name, dwords));
        mem::wp(v + k * i as u32, a);
    }
    VTBLS.lock().unwrap().get_or_insert_with(HashMap::new).insert(iface, v);
    v
}

/// A new object (refcount 1) of `size` bytes, the vtable pointer first.
pub fn object(iface: &'static str, impls: &[(&str, Handler)], size: u32) -> u32 {
    let vt = vtable(iface, impls);
    let o = mem::heap_alloc(size.max(mem::ps()), true);
    mem::wp(o, vt);
    objs(|m| m.insert(o, Obj { iface, refs: 1 }));
    o
}

/// AddRef from the host (an object handed out again).
pub fn add_ref_obj(obj: u32) {
    objs(|m| {
        if let Some(o) = m.get_mut(&obj) {
            o.refs += 1;
        }
    });
}

pub fn is(obj: u32, iface: &str) -> bool {
    objs(|m| m.get(&obj).is_some_and(|o| o.iface == iface))
}

// ---- WIC ----

const CLSID_WIC_FACTORY: u32 = 0xcaca_f262;
const CLSID_WIC_FACTORY2: u32 = 0x317d_06e8;

/// CoCreateInstance of the WIC imaging factory.
pub fn create_instance(clsid: u32) -> Option<u32> {
    match mem::r32(clsid) {
        CLSID_WIC_FACTORY | CLSID_WIC_FACTORY2 => Some(object("IWICImagingFactory2", &[], 8)),
        _ => None,
    }
}
