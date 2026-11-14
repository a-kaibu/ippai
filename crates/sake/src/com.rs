//! COM objects from the generated vtable tables (com_methods.rs, from Microsoft's Win32 metadata:
//! tools/win32md): an object names its interface and the methods it implements; every other slot is
//! a stub that logs "Iface::Method" once and fails (E_NOTIMPL, 0.0 for float results, the hidden
//! result pointer for structure results). QueryInterface, AddRef and Release come for free:
//! QueryInterface answers the interface and every interface it derives from.

use crate::api::{host_fn, Ctx, Handler};
use crate::mem;
use std::collections::HashMap;
use std::sync::Mutex;

#[path = "com_methods.rs"]
pub(crate) mod methods;
pub use methods::Guid;

pub const E_NOTIMPL: u32 = 0x8000_4001;
pub const E_NOINTERFACE: u32 = 0x8000_4002;

struct Obj {
    iface: &'static str,
    refs: u32,
    /// what goes with the object at its last Release (its module's state), before its memory
    free: Option<fn(u32)>,
    /// a singleton the module hands out again (never freed)
    kept: bool,
}

/// Blocks of released objects, freed QUARANTINE releases later: a guest that calls through a
/// pointer it has just released still finds a vtable (whose methods find no object).
const QUARANTINE: usize = 256;
static FREED: Mutex<std::collections::VecDeque<u32>> = Mutex::new(std::collections::VecDeque::new());

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

pub fn warn_once(msg: &str) {
    static SEEN: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
    if SEEN.lock().unwrap().get_or_insert_with(Default::default).insert(msg.to_string()) {
        eprintln!("{msg}");
    }
}

/// A stub method's trap tag (crate::api::set_tag): STUB_TAG | its kind, STUB_WARNED once said
/// (a title that calls one every frame paid a lock, a format and a hash for each: review C-2).
const STUB_TAG: u32 = 1 << 31;
const STUB_WARNED: u32 = 1 << 30;

fn stub(c: &mut Ctx) -> u32 {
    let (tag, _) = crate::api::tag_of(c.trap);
    let kind = if tag & STUB_WARNED != 0 {
        tag as u8
    } else {
        let (iface, name, kind) = STUBS.lock().unwrap().as_ref().and_then(|m| m.get(&c.trap).copied()).unwrap_or(("?", "?", 0));
        warn_once(&format!("sake: com {iface}::{name} not implemented"));
        crate::api::set_tag(c.trap, STUB_TAG | STUB_WARNED | kind as u32, 0);
        kind
    };
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
    release_obj(c.a(0))
}

/// Release on `obj` (for objects whose own Release frees more): the references left. At the
/// last one the object's `on_free` runs and its block goes (after the quarantine).
pub fn release_obj(this: u32) -> u32 {
    let gone = objs(|m| match m.get_mut(&this) {
        Some(o) if o.refs > 1 || o.kept => {
            o.refs = o.refs.saturating_sub(1);
            Err(o.refs)
        }
        Some(_) => Ok(m.remove(&this).and_then(|o| o.free)),
        None => Err(0),
    });
    match gone {
        Err(n) => n,
        Ok(free) => {
            // (outside the table's lock: freeing an object releases what it held)
            if let Some(f) = free {
                f(this);
            }
            let old = {
                let mut q = FREED.lock().unwrap();
                q.push_back(this);
                if q.len() > QUARANTINE { q.pop_front() } else { None }
            };
            if let Some(p) = old {
                mem::heap_free(p);
            }
            0
        }
    }
}

/// `f` runs when `obj`'s last reference goes (its module's state for it, what it holds).
pub fn on_free(obj: u32, f: fn(u32)) {
    objs(|m| {
        if let Some(o) = m.get_mut(&obj) {
            o.free = Some(f);
        }
    });
}

/// `obj` is a singleton handed out again: its Release never frees it.
pub fn keep(obj: u32) {
    objs(|m| {
        if let Some(o) = m.get_mut(&obj) {
            o.kept = true;
        }
    });
}

/// The references `obj` has (None: not a live object).
pub fn refs(obj: u32) -> Option<u32> {
    objs(|m| m.get(&obj).map(|o| o.refs))
}

/// Whether trap `a` is a COM method sake does not implement.
pub fn is_stub(a: u32) -> bool {
    STUBS.lock().unwrap().as_ref().is_some_and(|m| m.contains_key(&a))
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
        // ("name#slot": that slot only, where an interface has two methods of one name, as
        // ID2D1DeviceContext's CreateBitmap with D2D1_BITMAP_PROPERTIES1 at 57 beside 4's)
        let slot = format!("{name}#{i}");
        let found = impls.iter().find(|m| m.0 == slot).or_else(|| impls.iter().find(|m| m.0 == name));
        let f: Handler = match found {
            Some(m) => m.1,
            None if i == 0 => query_interface,
            None if i == 1 => add_ref,
            None if i == 2 => release,
            None => stub,
        };
        let a = host_fn(&format!("{iface}::{i}:{name}"), f, 4 * dwords as u32);
        if i > 2 && found.is_none() {
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
    objs(|m| m.insert(o, Obj { iface, refs: 1, free: None, kept: false }));
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

pub const CLSID_WIC_FACTORY: u32 = 0xcaca_f262;
pub const CLSID_WIC_FACTORY2: u32 = 0x317d_06e8;

/// CoCreateInstance of the WIC imaging factory.
pub fn create_instance(clsid: u32) -> Option<u32> {
    match mem::r32(clsid) {
        CLSID_WIC_FACTORY | CLSID_WIC_FACTORY2 => Some(crate::wic::factory()),
        0x4590_f811 => Some(crate::wmi::locator()), // CLSID_WbemLocator
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::methods::IFACES;

    fn slot(iface: &str, m: &str) -> (u8, u8) {
        let t = IFACES.iter().find(|t| t.0 == iface).unwrap();
        t.1.iter().find(|x| x.0 == m).map(|x| (x.1, x.2)).unwrap()
    }

    /// Integers wider or narrower than a dword come back in EAX (EDX:EAX), not through a hidden
    /// result pointer: the tables from Wine's IDL had these as structures, so their stubs popped
    /// one dword too many.
    #[test]
    fn integer_results_are_not_structures() {
        for (i, m) in [("IDWriteFontFace", "GetGlyphCount"), ("ID2D1Device", "GetMaximumTextureMemory"), ("ID2D1Device4", "GetMaximumColorGlyphCacheMemory"), ("IDWriteFontDownloadQueue", "GetGenerationCount"), ("IDWriteFontFaceReference", "GetFileSize"), ("IDWriteFontFaceReference", "GetLocalFileSize")] {
            assert_eq!(slot(i, m), (1, 0), "{i}::{m}");
        }
        // (a structure result: the hidden pointer counts)
        assert_eq!(slot("ID2D1RenderTarget", "GetSize"), (2, 2));
        assert_eq!(slot("ID2D1StrokeStyle", "GetMiterLimit"), (1, 1));
    }

    /// ID2D1DeviceContext (every version) has CreateBitmap twice: ID2D1RenderTarget's at 4
    /// (D2D1_BITMAP_PROPERTIES) and its own at 57 (D2D1_BITMAP_PROPERTIES1, whose options make a
    /// bitmap a target): d2d.rs implements "CreateBitmap#57" for the second alone.
    #[test]
    fn device_context_create_bitmap_slots() {
        for t in IFACES.iter().filter(|t| t.0.starts_with("ID2D1DeviceContext")) {
            let at: Vec<usize> = t.1.iter().enumerate().filter(|x| x.1 .0 == "CreateBitmap").map(|x| x.0).collect();
            assert_eq!(at, [4, 57], "{}", t.0);
        }
        for i in ["ID2D1RenderTarget", "ID2D1BitmapRenderTarget", "ID2D1HwndRenderTarget"] {
            let t = IFACES.iter().find(|t| t.0 == i).unwrap();
            assert_eq!(t.1.iter().filter(|x| x.0 == "CreateBitmap").count(), 1, "{i}");
        }
    }

    /// Overloads in one interface keep distinct names (the earlier get "_"s) and the vtable
    /// inherits every base method first.
    #[test]
    fn overloads_and_bases() {
        let t = IFACES.iter().find(|t| t.0 == "IDWriteFontSet").unwrap();
        let names: Vec<&str> = t.1.iter().map(|x| x.0).collect();
        assert_eq!(&names[..3], &["QueryInterface", "AddRef", "Release"]);
        assert_eq!(&names[7..10], &["GetPropertyValues__", "GetPropertyValues_", "GetPropertyValues"]);
        let s = IFACES.iter().find(|t| t.0 == "IWICStream").unwrap();
        assert_eq!(s.2, &["IWICStream", "IStream", "ISequentialStream", "IUnknown"]);
        assert_eq!(s.1.len(), 18);
    }
}

/// Object lifetimes (review B-1): the last Release runs the object's on_free once and frees its
/// block after the quarantine; kept singletons are never freed.
#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use crate::test_guest::enter;
    use proptest::prelude::*;

    static FREED_OBJS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    fn note(o: u32) {
        FREED_OBJS.lock().unwrap().push(o);
    }
    fn freed(o: u32) -> usize {
        FREED_OBJS.lock().unwrap().iter().filter(|&&x| x == o).count()
    }
    fn new() -> u32 {
        let o = object("ID2D1StrokeStyle1", &[], 8);
        on_free(o, note);
        o
    }

    #[test]
    fn last_release_frees_after_the_quarantine() {
        let _g = enter();
        FREED_OBJS.lock().unwrap().clear();
        let o = new();
        add_ref_obj(o);
        assert_eq!(release_obj(o), 1);
        assert_eq!(freed(o), 0);
        assert_eq!(release_obj(o), 0);
        assert_eq!(freed(o), 1);
        assert_eq!(refs(o), None);
        // (still a block with its vtable for a stale call; a Release more does nothing)
        assert!(mem::heap_owns(o));
        assert_eq!(release_obj(o), 0);
        assert_eq!(freed(o), 1);
        for _ in 0..QUARANTINE {
            let x = new();
            release_obj(x);
        }
        assert!(!mem::heap_owns(o));
    }

    #[test]
    fn kept_objects_stay() {
        let _g = enter();
        FREED_OBJS.lock().unwrap().clear();
        let o = new();
        keep(o);
        assert_eq!(release_obj(o), 0);
        assert_eq!(release_obj(o), 0);
        assert_eq!(refs(o), Some(0));
        assert_eq!(freed(o), 0);
        add_ref_obj(o);
        assert_eq!(refs(o), Some(1));
    }

    proptest! {
        /// Any AddRef/Release sequence over a few objects agrees with a count per object: on_free
        /// runs exactly when (and once) an object's count reaches 0, and the table forgets it.
        #[test]
        fn counts_as_a_model(ops in proptest::collection::vec((0usize..4, any::<bool>()), 0..60)) {
            let _g = enter();
            FREED_OBJS.lock().unwrap().clear();
            let objs: Vec<u32> = (0..4).map(|_| new()).collect();
            let mut model = [1u32; 4];
            for (i, up) in ops {
                let o = objs[i];
                if model[i] == 0 {
                    continue;
                }
                if up {
                    add_ref_obj(o);
                    model[i] += 1;
                } else {
                    model[i] -= 1;
                    prop_assert_eq!(release_obj(o), model[i]);
                }
                prop_assert_eq!(refs(o), (model[i] > 0).then_some(model[i]));
                prop_assert_eq!(freed(o), (model[i] == 0) as usize);
            }
            for (i, &o) in objs.iter().enumerate() {
                while model[i] > 0 {
                    model[i] -= 1;
                    release_obj(o);
                }
                prop_assert_eq!(freed(o), 1);
            }
        }
    }
}
