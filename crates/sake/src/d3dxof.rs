//! d3dxof.dll: DirectXFileCreate and the IDirectXFile objects over xof.rs (the .x file parser).
//! Programs read .x files with it directly (Pygmalion 体験版's motion files, before Direct3D 9's
//! D3DX mesh loaders existed); D3DRM's standard templates come in through RegisterTemplates.
//!
//! References, as Windows counts them (Wine's d3dxof tests): an enumerator holds a reference on
//! every data object it handed out and a data object on every child it handed out, so
//! GetNextDataObject gives a top-level object at 2 and GetNextObject a child at 3; neither holds
//! the IDirectXFile (its templates are shared, not its count). Each enumerator has its own objects
//! and each object its own data (GetData's pointer stays the same for it, a DWORD >= 2 before it).

use crate::api::{implement, Ctx, Handler};
use crate::mem;
use crate::xof::{self, Child, Guid, Templates, XFile};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_NOTIMPL: u32 = 0x8000_4001;

const fn iid(n: u8) -> Guid {
    [0x40 + n, 0xab, 0x82, 0x3d, 0xda, 0x62, 0xcf, 0x11, 0xab, 0x39, 0x00, 0x20, 0xaf, 0x71, 0xe4, 0x33]
}
const IID_UNKNOWN: Guid = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const IID_FILE: Guid = iid(0);
const IID_ENUM: Guid = iid(1);
const IID_OBJECT: Guid = iid(3);
const IID_DATA: Guid = iid(4);
const IID_REFERENCE: Guid = iid(5);

type Shared = Arc<Mutex<(XFile, Arc<Mutex<Templates>>)>>;

enum Kind {
    File(Arc<Mutex<Templates>>),
    /// the file, the objects it holds
    Enum(Shared, Vec<u32>),
    /// the file, node, next child, children it holds, its data in guest memory
    Data { f: Shared, node: usize, next: usize, kids: Vec<u32>, buf: Option<u32>, owner: u32 },
    Ref { f: Shared, name: Option<Vec<u8>>, guid: Option<Guid> },
}

struct Obj {
    refs: u32,
    kind: Kind,
}

static OBJS: Mutex<Option<HashMap<u32, Obj>>> = Mutex::new(None);

fn objs<R>(f: impl FnOnce(&mut HashMap<u32, Obj>) -> R) -> R {
    f(OBJS.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn guid_at(p: u32) -> Guid {
    mem::slice(p, 16).try_into().unwrap()
}

fn new_obj(kind: Kind, refs: u32) -> u32 {
    let vt = match kind {
        Kind::File(_) => vt_file(),
        Kind::Enum(..) => vt_enum(),
        Kind::Data { .. } => vt_data(),
        Kind::Ref { .. } => vt_ref(),
    };
    let o = mem::heap_alloc(2 * mem::ps(), true);
    mem::wp(o, vt);
    objs(|m| m.insert(o, Obj { refs, kind }));
    o
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, riid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return 0x8000_4003;
    }
    let want = if riid == 0 { [0xff; 16] } else { guid_at(riid) };
    let ok = objs(|m| {
        let Some(o) = m.get_mut(&this) else { return false };
        let ok = want == IID_UNKNOWN
            || match o.kind {
                Kind::File(_) => want == IID_FILE,
                Kind::Enum(..) => want == IID_ENUM,
                Kind::Data { .. } => want == IID_DATA || want == IID_OBJECT,
                Kind::Ref { .. } => want == IID_REFERENCE || want == IID_OBJECT,
            };
        if ok {
            o.refs += 1;
        }
        ok
    });
    mem::putp(out, if ok { this } else { 0 });
    if ok { 0 } else { E_NOINTERFACE }
}

fn add_ref(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    objs(|m| m.get_mut(&this).map_or(0, |o| {
        o.refs += 1;
        o.refs
    }))
}

fn release(c: &mut Ctx) -> u32 {
    release_obj(c.a(0))
}

/// Release; an object at 0 lets go of what it holds. (Its memory stays: a stale pointer still
/// finds a vtable.)
pub fn release_obj(this: u32) -> u32 {
    let mut todo = vec![this];
    let mut first = None;
    while let Some(p) = todo.pop() {
        let left = objs(|m| {
            let o = m.get_mut(&p)?;
            o.refs = o.refs.saturating_sub(1);
            let n = o.refs;
            if n == 0 {
                match m.remove(&p).map(|o| o.kind) {
                    Some(Kind::Enum(_, held)) | Some(Kind::Data { kids: held, .. }) => todo.extend(held),
                    _ => {}
                }
            }
            Some(n)
        });
        first.get_or_insert(left.unwrap_or(0));
    }
    first.unwrap_or(0)
}

fn shared_of(this: u32) -> Option<Shared> {
    objs(|m| match &m.get(&this)?.kind {
        Kind::Enum(f, _) | Kind::Data { f, .. } | Kind::Ref { f, .. } => Some(f.clone()),
        Kind::File(_) => None,
    })
}

// ---- DirectXFileCreate, IDirectXFile ----

#[allow(non_snake_case)]
fn DirectXFileCreate(c: &mut Ctx) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return xof::BADVALUE;
    }
    mem::putp(out, new_obj(Kind::File(Arc::new(Mutex::new(Templates::default()))), 1));
    0
}

fn templates_of(this: u32) -> Option<Arc<Mutex<Templates>>> {
    objs(|m| match &m.get(&this)?.kind {
        Kind::File(t) => Some(t.clone()),
        _ => None,
    })
}

/// RegisterTemplates(data, size)
fn register_templates(c: &mut Ctx) -> u32 {
    let (this, data, size) = (c.a(0), c.a(1), c.a(2));
    let Some(t) = templates_of(this) else { return xof::BADOBJECT };
    if data == 0 || size == 0 {
        return xof::BADVALUE;
    }
    let bytes = mem::slice(data, size).to_vec();
    let r = xof::register(&bytes, &mut t.lock().unwrap());
    r.err().unwrap_or(0)
}

/// The bytes CreateEnumObject reads: DXFILELOAD_FROMFILE (a path), FROMRESOURCE
/// (DXFILELOADRESOURCE: module, name, type), FROMMEMORY (DXFILELOADMEMORY: pointer, size). Only
/// the low 4 bits of the options count.
fn source(src: u32, opts: u32) -> Result<Vec<u8>, u32> {
    if src == 0 {
        return Err(xof::BADVALUE);
    }
    let ps = mem::ps();
    match opts & 0xf {
        0 => crate::d3dcompiler::read_file(&crate::k32::arg_str(src, false)).ok_or(xof::FILENOTFOUND),
        1 => {
            let (module, name, ty) = (mem::rp(src), mem::rp(src + ps), mem::rp(src + 2 * ps));
            let m = if module == 0 { crate::k32::module_handle(0, false) } else { module };
            let e = crate::pe::find_resource(m, &crate::pe::ResId::from_arg(ty, false), &crate::pe::ResId::from_arg(name, false)).ok_or(xof::RESOURCENOTFOUND)?;
            Ok(mem::slice(m + mem::r32(e), mem::r32(e + 4)).to_vec())
        }
        2 => {
            let (p, n) = (mem::rp(src), mem::r32(src + ps));
            if p == 0 {
                return Err(xof::BADVALUE);
            }
            Ok(mem::slice(p, n).to_vec())
        }
        _ => Err(xof::NOTDONEYET),
    }
}

/// CreateEnumObject(source, options, &enum)
fn create_enum_object(c: &mut Ctx) -> u32 {
    let (this, src, opts, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(t) = templates_of(this) else { return xof::BADOBJECT };
    if out == 0 {
        return xof::BADVALUE;
    }
    mem::putp(out, 0);
    let bytes = match source(src, opts) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let f = match XFile::open(&bytes, &mut t.lock().unwrap(), mem::ps() as usize) {
        Ok(f) => f,
        Err(e) => return e,
    };
    mem::putp(out, new_obj(Kind::Enum(Arc::new(Mutex::new((f, t))), Vec::new()), 1));
    0
}

fn create_save_object(c: &mut Ctx) -> u32 {
    if c.a(3) != 0 {
        mem::putp(c.a(3), 0);
    }
    crate::com::warn_once("sake: d3dxof IDirectXFile::CreateSaveObject not implemented");
    E_NOTIMPL
}

// ---- IDirectXFileEnumObject ----

/// A new data object for `node`, held by `holders` (the enumerator, and the parent for a child);
/// `owner`: the enumerator (0: none), which holds its children too.
fn data_obj(f: &Shared, node: usize, holders: &[u32], owner: u32, extra: u32) -> u32 {
    let o = new_obj(Kind::Data { f: f.clone(), node, next: 0, kids: Vec::new(), buf: None, owner }, holders.len() as u32 + extra);
    objs(|m| {
        for h in holders {
            match m.get_mut(h).map(|h| &mut h.kind) {
                Some(Kind::Enum(_, held)) | Some(Kind::Data { kids: held, .. }) => held.push(o),
                _ => {}
            }
        }
    });
    o
}

fn enum_of(this: u32) -> Option<Shared> {
    objs(|m| match &m.get(&this)?.kind {
        Kind::Enum(f, _) => Some(f.clone()),
        _ => None,
    })
}

/// GetNextDataObject(&data)
fn get_next_data_object(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let Some(f) = enum_of(this) else { return xof::BADOBJECT };
    if out == 0 {
        return xof::BADVALUE;
    }
    mem::putp(out, 0);
    let r = {
        let mut g = f.lock().unwrap();
        let (file, reg) = &mut *g;
        let mut reg = reg.lock().unwrap();
        file.next_top(&mut reg)
    };
    match r {
        Ok(Some(n)) => {
            mem::putp(out, data_obj(&f, n, &[this], this, 1));
            0
        }
        Ok(None) => xof::NOMOREOBJECTS,
        Err(e) => e,
    }
}

/// GetDataObjectById(guid, &data), GetDataObjectByName(name, &data)
fn by(c: &mut Ctx, name: bool) -> u32 {
    let (this, key, out) = (c.a(0), c.a(1), c.a(2));
    let Some(f) = enum_of(this) else { return xof::BADOBJECT };
    if out == 0 || key == 0 {
        return xof::BADVALUE;
    }
    mem::putp(out, 0);
    let n = mem::cstr(key);
    let g = guid_at(key);
    let hit = {
        let mut l = f.lock().unwrap();
        let (file, reg) = &mut *l;
        let mut reg = reg.lock().unwrap();
        if name { file.find(&mut reg, Some(&n), None) } else { file.find(&mut reg, None, Some(&g)) }
    };
    match hit {
        Some(k) => {
            mem::putp(out, data_obj(&f, k, &[this], this, 1));
            0
        }
        None => xof::NOTFOUND,
    }
}

// ---- IDirectXFileObject: GetName, GetId ----

/// (name, GUID) of a data object or a reference
fn ident(this: u32) -> Option<(Option<Vec<u8>>, Option<Guid>)> {
    let k = objs(|m| match &m.get(&this)?.kind {
        Kind::Data { f, node, .. } => Some(Err((f.clone(), *node))),
        Kind::Ref { name, guid, .. } => Some(Ok((name.clone(), *guid))),
        _ => None,
    })?;
    Some(match k {
        Ok(v) => v,
        Err((f, n)) => {
            let g = f.lock().unwrap();
            (g.0.nodes[n].name.clone(), g.0.nodes[n].guid)
        }
    })
}

/// GetName(buffer, &length): length in (bytes the buffer holds) and out (the name's with its NUL,
/// 0 for no name); a short buffer is BADVALUE.
fn get_name(c: &mut Ctx) -> u32 {
    let (this, buf, plen) = (c.a(0), c.a(1), c.a(2));
    let Some((name, _)) = ident(this) else { return xof::BADOBJECT };
    if plen == 0 {
        return xof::BADVALUE;
    }
    let need = name.as_ref().map_or(0, |n| n.len() as u32 + 1);
    if buf != 0 {
        let cap = mem::r32(plen);
        if need > cap {
            return xof::BADVALUE;
        }
        if cap > 0 {
            let n = name.unwrap_or_default();
            mem::write(buf, &n);
            mem::w8(buf + n.len() as u32, 0);
        }
    }
    mem::w32(plen, need);
    0
}

fn get_id(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let Some((_, g)) = ident(this) else { return xof::BADOBJECT };
    if out == 0 {
        return xof::BADVALUE;
    }
    mem::write(out, &g.unwrap_or([0; 16]));
    0
}

// ---- IDirectXFileData ----

/// The object's data in guest memory: allocated at the first GetData, with its strings.
fn data_buf(this: u32) -> Option<(u32, Vec<(Vec<u8>, usize, usize)>, usize)> {
    let (f, node, buf) = objs(|m| match &m.get(&this)?.kind {
        Kind::Data { f, node, buf, .. } => Some((f.clone(), *node, *buf)),
        _ => None,
    })?;
    let g = f.lock().unwrap();
    let n = &g.0.nodes[node];
    let p = match buf {
        Some(p) => p,
        None => {
            // (a header before it, as Windows' allocations: the DWORD just before is >= 2)
            let base = mem::heap_alloc(n.data.len() as u32 + 16, true);
            let p = base + 16;
            mem::w32(p - 4, 2);
            mem::write(p, &n.data);
            for (off, s) in &n.strs {
                let sp = mem::heap_alloc(s.len() as u32 + 1, true);
                mem::write(sp, s);
                mem::wp(p + *off as u32, sp);
            }
            objs(|m| {
                if let Some(Kind::Data { buf, .. }) = m.get_mut(&this).map(|o| &mut o.kind) {
                    *buf = Some(p);
                }
            });
            p
        }
    };
    Some((p, n.members.clone(), n.data.len()))
}

/// GetData(member or NULL, &size, &data)
fn get_data(c: &mut Ctx) -> u32 {
    let (this, member, psize, pdata) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if psize == 0 || pdata == 0 {
        return xof::BADVALUE;
    }
    let Some((p, members, len)) = data_buf(this) else { return xof::BADOBJECT };
    let (off, size) = if member == 0 {
        (0, len)
    } else {
        let want = mem::cstr(member);
        match members.iter().find(|m| m.0 == want) {
            Some(m) => (m.1, m.2),
            None => return xof::BADDATAREFERENCE,
        }
    };
    mem::w32(psize, size as u32);
    mem::putp(pdata, p + off as u32);
    0
}

/// A GUID's copy in guest memory that stays (GetType hands out a pointer).
fn guid_ptr(g: Guid) -> u32 {
    static PTRS: OnceLock<Mutex<HashMap<Guid, u32>>> = OnceLock::new();
    *PTRS.get_or_init(Default::default).lock().unwrap().entry(g).or_insert_with(|| {
        let p = mem::heap_alloc(16, false);
        mem::write(p, &g);
        p
    })
}

/// GetType(&const GUID*): its template's GUID.
fn get_type(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return xof::BADVALUE;
    }
    let t = objs(|m| match &m.get(&this)?.kind {
        Kind::Data { f, node, .. } => Some((f.clone(), *node)),
        _ => None,
    });
    let Some((f, n)) = t else { return xof::BADOBJECT };
    let g = f.lock().unwrap().0.nodes[n].tpl_guid;
    mem::putp(out, guid_ptr(g));
    0
}

/// GetNextObject(&object): the next child, a data object or a reference.
fn get_next_object(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return xof::BADVALUE;
    }
    mem::putp(out, 0);
    let st = objs(|m| match &mut m.get_mut(&this)?.kind {
        Kind::Data { f, node, next, owner, .. } => {
            *next += 1;
            Some((f.clone(), *node, *next - 1, *owner))
        }
        _ => None,
    });
    let Some((f, node, i, owner)) = st else { return xof::BADOBJECT };
    let child = f.lock().unwrap().0.nodes[node].children.get(i).cloned();
    // (the enumerator that made this object holds its children too, while it lives)
    let owner = (owner != 0 && objs(|m| m.contains_key(&owner))).then_some(owner);
    let holders: Vec<u32> = std::iter::once(this).chain(owner).collect();
    match child {
        None => {
            objs(|m| {
                if let Some(Kind::Data { next, .. }) = m.get_mut(&this).map(|o| &mut o.kind) {
                    *next -= 1;
                }
            });
            xof::NOMOREOBJECTS
        }
        Some(Child::Obj(k)) => {
            mem::putp(out, data_obj(&f, k, &holders, owner.unwrap_or(0), 1));
            0
        }
        Some(Child::Ref { name, guid }) => {
            let o = new_obj(Kind::Ref { f: f.clone(), name, guid }, holders.len() as u32 + 1);
            objs(|m| {
                for h in &holders {
                    match m.get_mut(h).map(|h| &mut h.kind) {
                        Some(Kind::Enum(_, held)) | Some(Kind::Data { kids: held, .. }) => held.push(o),
                        _ => {}
                    }
                }
            });
            mem::putp(out, o);
            0
        }
    }
}

/// IDirectXFileDataReference::Resolve(&data): the object it names, anywhere in the file.
fn resolve(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return xof::BADVALUE;
    }
    mem::putp(out, 0);
    let Some((name, guid)) = ident(this) else { return xof::BADOBJECT };
    let Some(f) = shared_of(this) else { return xof::BADOBJECT };
    let hit = {
        let mut l = f.lock().unwrap();
        let (file, reg) = &mut *l;
        let mut reg = reg.lock().unwrap();
        file.find(&mut reg, name.as_deref(), guid.as_ref())
    };
    match hit {
        Some(k) => {
            mem::putp(out, data_obj(&f, k, &[], 0, 1));
            0
        }
        None => xof::NOTFOUND,
    }
}

fn not_impl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}

fn vt_file() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[(qi, 12), (add_ref, 4), (release, 4), (create_enum_object, 16), (create_save_object, 16), (register_templates, 12)];
        crate::api::vtable("IDirectXFile", m)
    })
}
fn vt_enum() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[(qi, 12), (add_ref, 4), (release, 4), (get_next_data_object, 8), (|c| by(c, false), 12), (|c| by(c, true), 12)];
        crate::api::vtable("IDirectXFileEnumObject", m)
    })
}
fn vt_data() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[
            (qi, 12),
            (add_ref, 4),
            (release, 4),
            (get_name, 12),
            (get_id, 8),
            (get_data, 16),
            (get_type, 8),
            (get_next_object, 8),
            (not_impl, 8),  // AddDataObject(data)
            (not_impl, 12), // AddDataReference(name, guid)
            (not_impl, 24), // AddBinaryObject(name, guid, mime, data, size)
        ];
        crate::api::vtable("IDirectXFileData", m)
    })
}
fn vt_ref() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[(qi, 12), (add_ref, 4), (release, 4), (get_name, 12), (get_id, 8), (resolve, 8)];
        crate::api::vtable("IDirectXFileDataReference", m)
    })
}

pub fn init() {
    implement("d3dxof", "DirectXFileCreate", DirectXFileCreate);
}
